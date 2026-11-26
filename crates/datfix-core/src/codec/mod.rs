//! Video codec helpers: find the slice header inside each frame and read what the sample
//! tables need (keyframe flag and display order).
//!
//! Each video sample in a Canon `.DAT` is one frame stored as length-prefixed NAL units
//! (usually a 4-byte big-endian length, then the NAL unit). Parameter sets are not repeated in
//! the stream; they come from the reference clip.

pub mod avc;
pub mod bits;
pub mod hevc;

use crate::fourcc::FourCC;
use crate::read_at::ReadAt;

/// How many bytes of a slice NAL unit to read for header parsing.
const SLICE_HEADER_BYTES: usize = 96;

/// Codec configuration taken from the reference's sample entry.
#[derive(Clone, Debug)]
pub enum VideoCodec {
    Hevc(hevc::HevcConfig),
    Avc(avc::AvcConfig),
}

impl VideoCodec {
    /// Builds the codec from a sample entry type and its `hvcC`/`avcC` payload.
    pub fn from_config(
        codec: FourCC,
        config_kind: Option<FourCC>,
        config: &[u8],
    ) -> Result<Self, String> {
        match (&codec.0, config_kind.map(|k| k.0)) {
            (b"hvc1" | b"hev1", Some(k)) if k == *b"hvcC" => {
                hevc::HevcConfig::from_hvcc(config).map(VideoCodec::Hevc)
            }
            (b"avc1" | b"avc3", Some(k)) if k == *b"avcC" => {
                avc::AvcConfig::from_avcc(config).map(VideoCodec::Avc)
            }
            _ => Err(format!(
                "video codec {codec} is not supported (HEVC and H.264 are)"
            )),
        }
    }

    /// Bytes in each NAL length prefix.
    pub fn length_size(&self) -> u8 {
        match self {
            VideoCodec::Hevc(c) => c.length_size,
            VideoCodec::Avc(c) => c.length_size,
        }
    }

    /// Short human name.
    pub fn name(&self) -> &'static str {
        match self {
            VideoCodec::Hevc(_) => "HEVC",
            VideoCodec::Avc(_) => "H.264",
        }
    }

    /// Picture size from the first SPS.
    pub fn picture_size(&self) -> (u32, u32) {
        match self {
            VideoCodec::Hevc(c) => (c.sps[0].width, c.sps[0].height),
            VideoCodec::Avc(c) => (c.sps[0].width, c.sps[0].height),
        }
    }
}

/// Bytes of each slice NAL read for a full header check (wavefront entry points need room).
const VERIFY_HEADER_BYTES: usize = 4096;

/// How well a frame fits a set of parameter sets (see [`verify_frame`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameFit {
    /// Slice segments whose complete header parsed and ended on the alignment bit.
    pub full_checks: u32,
    /// Slice segments in the frame.
    pub slices: u32,
}

/// Checks one frame against the codec settings more strictly than [`FrameAnalyzer::analyze`]:
/// every NAL length must fit, and every HEVC slice segment header must parse completely,
/// end on its alignment bit, and agree with the picture size (slice addresses, wavefront
/// entry points). Used to choose between candidate settings when no reference is given.
pub fn verify_frame<R: ReadAt + ?Sized>(
    src: &R,
    codec: &VideoCodec,
    offset: u64,
    size: u32,
) -> Result<FrameFit, String> {
    let ls = codec.length_size() as u64;
    let end = offset + size as u64;
    let mut pos = offset;
    let mut fit = FrameFit::default();
    let mut first_rows: Option<(u32, u32, bool)> = None;
    while pos < end {
        if pos + ls > end {
            return Err("frame ends inside a NAL length".into());
        }
        let mut head = [0u8; 4];
        src.read_exact_at(&mut head[..ls as usize], pos)
            .map_err(|e| e.to_string())?;
        let nal_len = head[..ls as usize]
            .iter()
            .fold(0u64, |a, &b| (a << 8) | b as u64);
        let nal_start = pos + ls;
        if nal_len < 2 || nal_start + nal_len > end {
            return Err(format!(
                "NAL unit length {nal_len} at byte {} does not fit the frame",
                pos - offset
            ));
        }
        let take = (nal_len as usize).min(VERIFY_HEADER_BYTES);
        let nal = src.read_vec(nal_start, take).map_err(|e| e.to_string())?;
        match codec {
            VideoCodec::Hevc(cfg) => {
                if nal[0] & 0x80 != 0 || nal[1] & 7 == 0 {
                    return Err("invalid HEVC NAL header".into());
                }
                if hevc::is_vcl(hevc::nal_type(nal[0])) {
                    fit.slices += 1;
                    if let Some(c) = hevc::check_slice(&nal, cfg)? {
                        if fit.slices == 1 && !c.first_in_picture {
                            return Err("frame does not start with its first slice".into());
                        }
                        let data = nal_len.saturating_sub(2 + c.header_bytes as u64);
                        if c.entry_bytes >= data.max(1) {
                            return Err(format!(
                                "entry points span {} bytes but the slice holds {data}",
                                c.entry_bytes
                            ));
                        }
                        if fit.slices == 1 {
                            first_rows = Some((c.entry_points, c.ctbs.1, c.wavefront && !c.tiles));
                        }
                        fit.full_checks += 1;
                    }
                }
            }
            VideoCodec::Avc(cfg) => {
                if nal[0] & 0x80 != 0 {
                    return Err("invalid H.264 NAL header".into());
                }
                if avc::is_vcl(avc::nal_type(nal[0])) {
                    fit.slices += 1;
                    avc::parse_slice(&nal, cfg)?;
                }
            }
        }
        pos = nal_start + nal_len;
    }
    if fit.slices == 0 {
        return Err("frame contains no slice".into());
    }
    // One wavefront slice covers every CTB row: entry points = rows - 1.
    if let Some((entry_points, rows, wpp)) = first_rows {
        if wpp && fit.slices == 1 && entry_points + 1 != rows {
            return Err(format!(
                "the frame has {} CTB rows but the settings describe {rows}",
                entry_points + 1
            ));
        }
    }
    Ok(fit)
}

/// Facts about one video frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameInfo {
    /// Clean random access point: HEVC IDR/BLA, H.264 IDR.
    pub sync: bool,
    /// HEVC CRA picture. It is a keyframe only when no RASL picture follows it (see timing).
    pub cra: bool,
    /// HEVC RASL picture: a leading picture that cannot be decoded when playback starts at the
    /// CRA before it.
    pub rasl: bool,
    /// `(sequence, poc)`: sorting by this gives display order.
    pub order: (u32, i64),
}

/// Reads frames one after another in decode order and tracks POC state between them.
pub struct FrameAnalyzer {
    codec: VideoCodec,
    hevc_poc: hevc::PocTracker,
    avc_poc: avc::PocTracker,
}

impl FrameAnalyzer {
    pub fn new(codec: VideoCodec) -> Self {
        FrameAnalyzer {
            codec,
            hevc_poc: hevc::PocTracker::default(),
            avc_poc: avc::PocTracker::default(),
        }
    }

    /// Analyzes the frame stored at `offset` with `size` bytes.
    ///
    /// Walks the NAL units of the frame, skips non-slice units (AUD, SEI, ...) and parses the
    /// first slice header. Fails when the NAL lengths do not add up to the sample size, which
    /// means the frame (or the `repi` size table) is damaged.
    pub fn analyze<R: ReadAt + ?Sized>(
        &mut self,
        src: &R,
        offset: u64,
        size: u32,
    ) -> Result<FrameInfo, String> {
        let ls = self.codec.length_size() as u64;
        let end = offset + size as u64;
        let mut pos = offset;
        while pos + ls < end {
            let mut head = [0u8; 6];
            let n = (ls as usize + 2).min((end - pos) as usize);
            src.read_exact_at(&mut head[..n], pos)
                .map_err(|e| e.to_string())?;
            let nal_len = head[..ls as usize]
                .iter()
                .fold(0u64, |a, &b| (a << 8) | b as u64);
            let nal_start = pos + ls;
            if nal_len == 0 || nal_start + nal_len > end {
                return Err(format!(
                    "NAL unit length {nal_len} at byte {} does not fit the frame",
                    pos - offset
                ));
            }
            let header0 = head[ls as usize];
            let is_slice = match self.codec {
                VideoCodec::Hevc(_) => hevc::is_vcl(hevc::nal_type(header0)),
                VideoCodec::Avc(_) => avc::is_vcl(avc::nal_type(header0)),
            };
            if is_slice {
                let take = (nal_len as usize).min(SLICE_HEADER_BYTES);
                let nal = src.read_vec(nal_start, take).map_err(|e| e.to_string())?;
                return self.parse_slice(&nal);
            }
            pos = nal_start + nal_len;
        }
        Err("frame contains no slice".into())
    }

    fn parse_slice(&mut self, nal: &[u8]) -> Result<FrameInfo, String> {
        match &self.codec {
            VideoCodec::Hevc(cfg) => {
                let s = hevc::parse_slice(nal, cfg)?;
                Ok(FrameInfo {
                    sync: (16..=20).contains(&s.nal_type),
                    cra: s.nal_type == 21,
                    rasl: s.nal_type == 8 || s.nal_type == 9,
                    order: self.hevc_poc.next(&s),
                })
            }
            VideoCodec::Avc(cfg) => {
                let s = avc::parse_slice(nal, cfg)?;
                Ok(FrameInfo {
                    sync: s.nal_type == 5,
                    order: self.avc_poc.next(&s)?,
                    ..Default::default()
                })
            }
        }
    }
}
