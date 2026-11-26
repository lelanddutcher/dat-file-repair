//! What a `.DAT` says about the recording mode, without any reference clip.
//!
//! The `repi` index, the per-frame records and the frames themselves reveal:
//!
//! - the **family** (spec section 4): which streams are indexed and how;
//! - the **frame rate**: one interval of groups is 0.5005 s, so the frames per video group
//!   (and the PCM bytes per interval) give the rate;
//! - the **body**: bytes 1–3 of every 128-byte per-frame record are a body code;
//! - the **codec** and **GOP** structure, from the NAL headers of the first frames;
//! - the **start time**, from the timecode counter and the Canon file name.
//!
//! This is enough to pick decoder settings from the built-in profile bank and to refuse a
//! reference recorded at another frame rate.

use serde::{Deserialize, Serialize};

use crate::fourcc::FourCC;
use crate::read_at::ReadAt;
use crate::repi::{DatIndex, Group, StreamShape};

/// Bytes in one per-frame record (spec section 6).
pub const RECORD_SIZE: u64 = 128;
/// Audio sample rate of every Canon body in the corpus.
pub const AUDIO_RATE: u32 = 48_000;
/// Offset of the 7-bit frame counter inside a per-frame record.
const RECORD_COUNTER: usize = 0x54;

/// Body codes from the per-frame records (spec section 6) and the names Canon writes in `modl`.
pub const BODIES: [([u8; 3], &str); 5] = [
    ([0x00, 0x00, 0x45], "Canon EOS R3"),
    ([0x00, 0x10, 0x42], "Canon EOS R5"),
    ([0x00, 0x30, 0x45], "Canon EOS R6"),
    ([0x00, 0x10, 0x48], "Canon EOS R6m2"),
    ([0x00, 0x60, 0x49], "Canon EOS R5m2"),
];

/// Name of a body code, when known.
pub fn body_name(code: [u8; 3]) -> Option<&'static str> {
    BODIES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// The recording layouts seen on Canon bodies (spec section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Family {
    /// `CAEP`, video only (R3 with audio off).
    A,
    /// `CAEP` with 16-bit stereo LPCM stored as one big sample per interval.
    B,
    /// `CAEP` with AAC.
    BPrime,
    /// `XFVC` (R5 Mark II, C80): timecode, metadata, four mono 24-bit PCM streams or AAC.
    C,
    /// Something else.
    Unknown,
}

impl Family {
    /// The `ftyp` major brand Canon uses for this family.
    pub fn brand(self) -> FourCC {
        match self {
            Family::C => FourCC::new(b"XFVC"),
            _ => FourCC::new(b"CAEP"),
        }
    }

    /// Short description for messages.
    pub fn describe(self) -> &'static str {
        match self {
            Family::A => "CAEP, video only",
            Family::B => "CAEP with 16-bit LPCM",
            Family::BPrime => "CAEP with AAC",
            Family::C => "XF-HEVC S (XFVC) with 24-bit LPCM",
            Family::Unknown => "unknown layout",
        }
    }
}

/// A frame rate as `timescale / frame_duration`, for example 120000/1001.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FrameRate {
    pub timescale: u32,
    pub frame_duration: u32,
}

impl FrameRate {
    pub fn fps(self) -> f64 {
        self.timescale as f64 / self.frame_duration as f64
    }

    /// Nominal integer rate (30 for 29.97).
    pub fn nominal(self) -> u32 {
        self.fps().round() as u32
    }

    /// True when two rates agree within 0.1 %.
    pub fn matches(self, fps: f64) -> bool {
        (self.fps() - fps).abs() <= self.fps() * 0.001
    }

    /// Picks the usual Canon rational for a measured rate (NTSC rates use x/1001).
    pub fn from_fps(fps: f64) -> FrameRate {
        let ntsc = fps * 1.001;
        if (ntsc - ntsc.round()).abs() < (fps - fps.round()).abs() {
            FrameRate {
                timescale: ntsc.round() as u32 * 1000,
                frame_duration: 1001,
            }
        } else {
            FrameRate {
                timescale: (fps * 1000.0).round() as u32,
                frame_duration: 1000,
            }
        }
    }
}

impl std::fmt::Display for FrameRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.3}p", self.fps())
    }
}

/// Recording start as read from the `.DAT` and its name.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StartTime {
    /// Date and time from a Canon file name such as `A_0002D054H241221_134042C0_LAD04`, as
    /// seconds since 1970 in the camera's local time.
    pub name_local: Option<i64>,
    /// Time of day from the first timecode sample, in seconds after local midnight.
    pub timecode_seconds: Option<f64>,
}

/// Everything [`analyze`] found.
#[derive(Clone, Debug, Serialize)]
pub struct DatLayout {
    pub family: Family,
    /// Frames in a full video group (one 0.5005 s interval).
    pub frames_per_group: u32,
    pub frame_rate: Option<FrameRate>,
    /// True when the rate was measured against the PCM audio; false when it was inferred from
    /// the group size alone (assuming an NTSC rate).
    pub frame_rate_from_audio: bool,
    #[serde(serialize_with = "ser_code")]
    pub body_code: Option<[u8; 3]>,
    pub body: Option<&'static str>,
    /// `hvc1` or `avc1`, from the first frame's NAL header.
    pub codec: Option<FourCC>,
    /// True when the first groups hold non-key frames.
    pub long_gop: Option<bool>,
    /// True when each video group is followed by n × 128-byte records.
    pub has_records: bool,
    /// PCM streams per interval and bytes per audio frame (3 = 24-bit mono, 4 = 16-bit stereo).
    pub pcm_streams: u32,
    pub pcm_frame_bytes: u32,
    pub shapes: Vec<StreamShape>,
    pub start: StartTime,
}

fn ser_code<S: serde::Serializer>(c: &Option<[u8; 3]>, s: S) -> Result<S::Ok, S::Error> {
    match c {
        Some(c) => s.serialize_str(&hex(c)),
        None => s.serialize_none(),
    }
}

/// Lower-case hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl DatLayout {
    pub fn shape(&self, kind: &[u8; 4]) -> Option<&StreamShape> {
        self.shapes.iter().find(|s| s.kind.0 == *kind)
    }

    /// One-line summary such as `Canon EOS R5m2, HEVC 119.880p Long GOP, XFVC with 24-bit LPCM`.
    pub fn describe(&self) -> String {
        let body = self.body.unwrap_or("unknown Canon body");
        let codec = match self.codec.map(|c| c.0) {
            Some(c) if c == *b"hvc1" => "HEVC",
            Some(c) if c == *b"avc1" => "H.264",
            _ => "unknown codec",
        };
        let rate = self
            .frame_rate
            .map_or("unknown frame rate".to_string(), |r| r.to_string());
        let gop = match self.long_gop {
            Some(true) => " Long GOP",
            Some(false) => " All-Intra",
            None => "",
        };
        format!("{body}, {codec} {rate}{gop}, {}", self.family.describe())
    }
}

/// Reads the per-frame record block after a video group, if it is there.
pub fn records_of<R: ReadAt + ?Sized>(src: &R, g: &Group) -> Option<Vec<u8>> {
    if g.complete != g.declared {
        return None;
    }
    let at = g.declared_end();
    let len = RECORD_SIZE * g.declared as u64;
    let block = src.read_vec(at, len as usize).ok()?;
    // Every record starts with 0x04 and the same body code.
    let ok = block
        .chunks_exact(RECORD_SIZE as usize)
        .all(|r| r[0] == 0x04 && r[1..4] == block[1..4]);
    ok.then_some(block)
}

/// 7-bit frame counter of the first record after a video group (counts frames mod 128).
pub fn record_counter<R: ReadAt + ?Sized>(src: &R, g: &Group) -> Option<u32> {
    let block = records_of(src, g)?;
    Some(block[RECORD_COUNTER] as u32 & 0x7f)
}

/// First timecode counter of a `tmcd` group (a frame count since local midnight).
pub fn tmcd_counter<R: ReadAt + ?Sized>(src: &R, g: &Group) -> Option<u32> {
    if g.kind.0 != *b"tmcd" || g.fixed_size != 4 || g.complete == 0 {
        return None;
    }
    let b = src.read_vec(g.data_offset(), 4).ok()?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Guesses the codec from the first NAL header of a frame.
fn guess_codec<R: ReadAt + ?Sized>(src: &R, g: &Group) -> Option<(FourCC, bool)> {
    // Look at the first few frames: codec from the first, GOP from all of them.
    let mut codec = None;
    let mut long = false;
    for (off, size) in g.samples(g.complete.min(g.declared)).take(64) {
        if size < 6 {
            continue;
        }
        let b = src.read_vec(off, 6).ok()?;
        let nal_len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        if nal_len == 0 || nal_len + 4 > size {
            continue;
        }
        let (h0, h1) = (b[4], b[5]);
        let hevc_type = (h0 >> 1) & 0x3f;
        let is_hevc =
            h0 & 0x81 == 0 && h1 == 1 && (hevc_type <= 21 || (32..=40).contains(&hevc_type));
        let avc_type = h0 & 0x1f;
        let is_avc = h0 & 0x80 == 0
            && (1..=12).contains(&avc_type)
            && !(matches!(avc_type, 6 | 9) && h0 & 0x60 != 0);
        let this = if is_hevc {
            FourCC::new(b"hvc1")
        } else if is_avc {
            FourCC::new(b"avc1")
        } else {
            continue;
        };
        codec.get_or_insert(this);
        // A frame whose first VCL NAL is not a random access point means Long GOP. Only the
        // first NAL is checked here; prefix SEI/AUD units make this a lower bound.
        if this.0 == *b"hvc1" && hevc_type < 16 {
            long = true;
        }
        if this.0 == *b"avc1" && avc_type == 1 {
            long = true;
        }
    }
    codec.map(|c| (c, long))
}

/// Parses `H241221_134042` (or `A241029_…`) out of a Canon file name: a letter, YYMMDD,
/// underscore, HHMMSS. Returns seconds since 1970 in local time.
pub fn time_from_name(name: &str) -> Option<i64> {
    let b = name.as_bytes();
    for i in 0..b.len().saturating_sub(14) {
        let w = &b[i..i + 14];
        if !w[0].is_ascii_uppercase() || w[7] != b'_' {
            continue;
        }
        let digits = |r: std::ops::Range<usize>| -> Option<i64> {
            w[r].iter().try_fold(0i64, |a, &c| {
                c.is_ascii_digit().then(|| a * 10 + (c - b'0') as i64)
            })
        };
        let (Some(date), Some(time)) = (digits(1..7), digits(8..14)) else {
            continue;
        };
        let (y, mo, d) = (2000 + date / 10000, date / 100 % 100, date % 100);
        let (h, mi, s) = (time / 10000, time / 100 % 100, time % 100);
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
            continue;
        }
        return Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s);
    }
    None
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Analyzes the layout of a walked `.DAT`. `name` is the file name (for the start time).
pub fn analyze<R: ReadAt + ?Sized>(src: &R, index: &DatIndex, name: &str) -> DatLayout {
    let shapes = index.shapes();
    let has = |k: &[u8; 4]| shapes.iter().any(|s| s.kind.0 == *k);
    let pcm = shapes.iter().find(|s| s.kind.0 == *b"lpcm");
    let pcm_streams = pcm.map_or(0, |p| p.streams);
    let pcm_frame_bytes = match pcm {
        Some(p) if p.fixed_size > 0 && p.fixed_size <= 8 => p.fixed_size,
        Some(_) => 4, // one big sample of 16-bit stereo frames
        None => 0,
    };
    let family = if has(b"tdmt") || (pcm_frame_bytes == 3 && pcm_streams == 4) {
        Family::C
    } else if has(b"aac ") {
        Family::BPrime
    } else if pcm.is_some() {
        Family::B
    } else if has(b"vide") && shapes.len() == 1 {
        Family::A
    } else {
        Family::Unknown
    };
    let video = shapes.iter().find(|s| s.kind.0 == *b"vide");
    let frames_per_group = video.map_or(0, |v| v.typical_samples);

    // Frame rate: frames per group against the audio of one interval, else 0.5005 s.
    let mut frame_rate_from_audio = false;
    let fps = match pcm {
        Some(p) if frames_per_group > 0 && p.typical_samples > 0 => {
            let bytes = p.typical_samples as f64 * p.fixed_size.max(1) as f64;
            let span = bytes / pcm_frame_bytes as f64 / AUDIO_RATE as f64;
            frame_rate_from_audio = true;
            Some(frames_per_group as f64 / span)
        }
        _ if frames_per_group > 0 => Some(frames_per_group as f64 / 0.5005),
        _ => None,
    };
    let frame_rate = fps.map(FrameRate::from_fps);

    let first_video = index
        .groups
        .iter()
        .find(|g| g.kind.0 == *b"vide" && g.complete > 0);
    let full_video = index
        .groups
        .iter()
        .find(|g| g.kind.0 == *b"vide" && g.complete == g.declared);
    let records = full_video.and_then(|g| records_of(src, g));
    let body_code = records.as_ref().map(|r| [r[1], r[2], r[3]]);
    let (codec, long_gop) = match first_video.and_then(|g| guess_codec(src, g)) {
        Some((c, l)) => (Some(c), Some(l)),
        None => (None, None),
    };

    let timecode_seconds = index
        .groups
        .iter()
        .find_map(|g| tmcd_counter(src, g))
        .zip(frame_rate)
        .map(|(c, r)| c as f64 / r.fps());
    DatLayout {
        family,
        frames_per_group,
        frame_rate,
        frame_rate_from_audio,
        body_code,
        body: body_code.and_then(body_name),
        codec,
        long_gop,
        has_records: records.is_some(),
        pcm_streams,
        pcm_frame_bytes,
        shapes,
        start: StartTime {
            name_local: time_from_name(name),
            timecode_seconds,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canon_names() {
        let t = time_from_name("A_0002D054H241221_134042C0_LAD04.DAT").unwrap();
        // 2024-12-21 13:40:42
        assert_eq!(t, 1_734_788_442);
        assert_eq!(time_from_name("LAD33743.DAT"), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn frame_rates() {
        let r = FrameRate::from_fps(60.0 / 0.5005);
        assert_eq!((r.timescale, r.frame_duration), (120_000, 1001));
        let r = FrameRate::from_fps(15.0 / 0.5005);
        assert_eq!((r.timescale, r.frame_duration), (30_000, 1001));
        let r = FrameRate::from_fps(25.0);
        assert_eq!((r.timescale, r.frame_duration), (25_000, 1000));
        assert!(r.matches(25.01));
        assert!(!r.matches(25.1));
    }
}
