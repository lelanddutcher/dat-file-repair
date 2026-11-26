//! Reading a healthy *reference* clip.
//!
//! A `.DAT` holds the media but not the codec settings. Those live in the `moov` box of a
//! finished MP4 from the same camera and mode. This module reads that `moov` and summarizes
//! each track: what it is, its codec, timescale, sample duration and sample size.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fourcc::FourCC;
use crate::read_at::{FileSource, ReadAt};
use crate::repi::{self, StreamShape};

use super::atom::{self, Atom};
use super::bytes::ByteReader;
use super::tables;

/// Largest `moov` we are willing to load into memory (a 10-hour clip needs far less).
const MAX_MOOV: u64 = 512 * 1024 * 1024;
/// Bytes of a clip's `mdat` searched for the first `repi` group.
const REPI_SAMPLE_WINDOW: u64 = 8 * 1024 * 1024;

/// Recognizes files that are certainly not a healthy video clip from their first bytes.
/// Returns what the file is, in plain words.
pub fn sniff_not_a_clip(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0, 0, 0, 0, b'm', b'd', b'a', b't']) {
        return Some("an unfinished recording (.DAT), not a clip that plays");
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("a photo (JPEG)");
    }
    if head.starts_with(b"\x89PNG") {
        return Some("a picture (PNG)");
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return Some("a photo (TIFF or RAW)");
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" {
        return Some("a RIFF file (WAV, AVI or similar), not an MP4");
    }
    if head.len() >= 4 && head[0..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        return Some("a Matroska or WebM video, not a camera MP4");
    }
    if head.iter().all(|&b| b == 0) && !head.is_empty() {
        return Some("empty (it starts with only zero bytes)");
    }
    None
}

/// Recognizes `ftyp` brands of files that are not video clips (photos, RAW video).
fn sniff_brand(ftyp: &[u8]) -> Option<&'static str> {
    let brand = ftyp.get(8..12)?;
    match brand {
        b"crx " => Some("a Canon RAW file (CR3 photo or Cinema RAW Light .CRM video)"),
        b"heic" | b"heix" | b"mif1" | b"msf1" | b"avif" => Some("a photo (HEIF/AVIF)"),
        _ => None,
    }
}

/// Sound sample entry types that carry uncompressed PCM.
const PCM_CODECS: [&[u8; 4]; 9] = [
    b"ipcm", b"fpcm", b"lpcm", b"sowt", b"twos", b"in24", b"in32", b"raw ", b"NONE",
];

/// What a track carries, as far as the repair is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    PcmAudio,
    CompressedAudio,
    Timecode,
    Metadata,
    Other,
}

/// Audio settings read from a sound sample entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AudioFormat {
    pub channels: u32,
    pub bits_per_sample: u32,
    pub sample_rate: u32,
}

/// Video settings read from a visual sample entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VideoFormat {
    pub width: u16,
    pub height: u16,
    /// `hvcC` or `avcC`.
    pub config_kind: Option<FourCC>,
    /// Payload of the `hvcC`/`avcC` box (holds VPS/SPS/PPS).
    #[serde(skip)]
    pub config: Vec<u8>,
}

/// One track of the reference.
#[derive(Clone, Debug, Serialize)]
pub struct RefTrack {
    /// Index of this `trak` among the children of `moov`.
    #[serde(skip)]
    pub moov_index: usize,
    pub track_id: u32,
    pub handler: FourCC,
    pub kind: TrackKind,
    /// Sample entry type, for example `hvc1`, `ipcm`, `tmcd`, `cndm`.
    pub codec: FourCC,
    pub timescale: u32,
    /// Duration of one sample in `timescale` ticks (the most common `stts` delta).
    pub sample_delta: u32,
    pub sample_count: u64,
    /// `stsz` sample size; 0 when sizes vary.
    pub fixed_sample_size: u32,
    pub mean_sample_size: f64,
    /// Samples in the first chunk (Canon writes one chunk per `repi` group).
    pub samples_per_chunk: u32,
    pub has_ctts: bool,
    pub has_stss: bool,
    /// Media time of the first edit, if the track has an edit list.
    pub edit_media_time: Option<i64>,
    pub audio: Option<AudioFormat>,
    pub video: Option<VideoFormat>,
}

impl RefTrack {
    /// Length of one sample in seconds.
    pub fn sample_seconds(&self) -> f64 {
        self.sample_delta as f64 / self.timescale as f64
    }
}

/// A parsed reference clip.
#[derive(Clone, Debug)]
pub struct Reference {
    pub path: PathBuf,
    pub file_size: u64,
    /// The complete `ftyp` box, copied into the output unchanged.
    pub ftyp: Vec<u8>,
    pub major_brand: FourCC,
    /// The `moov` box as a tree.
    pub moov: Atom,
    pub movie_timescale: u32,
    pub tracks: Vec<RefTrack>,
    /// Layout of the `repi` groups inside the reference's own `mdat`, when it has them. Canon
    /// cameras keep the index in finished files too, so this is a fingerprint of the
    /// recording mode that can be compared with the `.DAT`.
    pub repi_shapes: Option<Vec<StreamShape>>,
}

impl Reference {
    /// Opens and parses a reference MP4.
    pub fn open(path: &Path) -> Result<Reference> {
        let src = FileSource::open(path)
            .map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
        Self::from_source(path, &src)
    }

    /// Parses a reference from any byte source. `path` is only used for messages.
    ///
    /// Every size read from the file is checked against the file length before memory is
    /// allocated, so a damaged or hostile file fails with an error instead of exhausting memory.
    pub fn from_source<R: ReadAt + ?Sized>(path: &Path, src: &R) -> Result<Reference> {
        let bad = |reason: String| Error::BadReference {
            path: path.to_path_buf(),
            reason,
        };
        let io = |e| Error::io(format!("cannot read {}", path.display()), e);
        let mut head = [0u8; 64];
        let head_len = (src.size().min(64)) as usize;
        src.read_exact_at(&mut head[..head_len], 0).map_err(io)?;
        if let Some(what) = sniff_not_a_clip(&head[..head_len]) {
            return Err(Error::NotAClip {
                path: path.to_path_buf(),
                what: what.into(),
            });
        }
        let boxes = atom::scan_top_level(src).map_err(io)?;

        let find = |kind: &[u8; 4]| boxes.iter().find(|b| b.kind.0 == *kind);
        let ftyp_box =
            find(b"ftyp").ok_or_else(|| bad("it has no ftyp box, so it is not an MP4".into()))?;
        let moov_box = find(b"moov").ok_or_else(|| {
            bad("it has no moov box (it may itself be an unfinished recording)".into())
        })?;
        if moov_box.size > MAX_MOOV || ftyp_box.size > 4096 {
            return Err(bad("its header boxes are implausibly large".into()));
        }
        for b in [ftyp_box, moov_box] {
            if b.offset.saturating_add(b.size) > src.size() {
                return Err(bad(format!(
                    "the file ends inside its {} box (it is cut short or still copying)",
                    b.kind
                )));
            }
        }
        let read = |b: &atom::TopBox| src.read_vec(b.offset, b.size as usize).map_err(io);
        let ftyp = read(ftyp_box)?;
        if let Some(what) = sniff_brand(&ftyp) {
            return Err(Error::NotAClip {
                path: path.to_path_buf(),
                what: what.into(),
            });
        }
        let moov_bytes = read(moov_box)?;
        let mut parsed =
            atom::parse_atoms(&moov_bytes).map_err(|e| bad(format!("broken moov: {e}")))?;
        let moov = parsed.pop().ok_or_else(|| bad("empty moov".into()))?;
        let mut reference = Self::from_parts(path, src.size(), ftyp, moov).map_err(bad)?;
        if let Some(mdat) = find(b"mdat") {
            // Sample the first groups only; that is enough to see every stream. Give up after
            // a few MB without a header: bodies without `repi` have none at all.
            reference.repi_shapes = repi::walk_window(
                src,
                mdat.offset + mdat.header_len,
                32,
                REPI_SAMPLE_WINDOW,
                &mut |_| true,
            )
            .ok()
            .map(|index| index.shapes());
        }
        Ok(reference)
    }

    /// Builds a reference from an already-parsed `ftyp` box and `moov` tree.
    pub fn from_parts(
        path: &Path,
        file_size: u64,
        ftyp: Vec<u8>,
        moov: Atom,
    ) -> Result<Reference, String> {
        if ftyp.len() < 12 {
            return Err("ftyp box is too short".into());
        }
        let major_brand = FourCC::from_slice(&ftyp[8..12]);
        let mvhd = moov
            .child(b"mvhd")
            .and_then(Atom::payload)
            .ok_or("moov has no mvhd")?;
        let movie_timescale = tables::TimedHeader::parse(mvhd, 4)?.timescale();
        if movie_timescale == 0 {
            return Err("movie timescale is zero".into());
        }

        let mut tracks = Vec::new();
        for (i, trak) in moov.children().iter().enumerate() {
            if trak.kind.0 != *b"trak" {
                continue;
            }
            let t = parse_track(i, trak).map_err(|e| format!("track {}: {e}", tracks.len() + 1))?;
            tracks.push(t);
        }
        if tracks.is_empty() {
            return Err("moov has no tracks".into());
        }
        Ok(Reference {
            path: path.to_path_buf(),
            file_size,
            ftyp,
            major_brand,
            moov,
            movie_timescale,
            tracks,
            repi_shapes: None,
        })
    }

    /// The `trak` box for a track.
    pub fn trak(&self, track: &RefTrack) -> &Atom {
        &self.moov.children()[track.moov_index]
    }

    /// The first video track, if any.
    pub fn video_track(&self) -> Option<&RefTrack> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Video)
    }

    /// Absolute positions of every sample of a track inside the reference file.
    pub fn sample_locations(&self, track: &RefTrack) -> Result<Vec<(u64, u32)>, String> {
        let stbl = self
            .trak(track)
            .find(&[b"mdia", b"minf", b"stbl"])
            .ok_or("track has no sample table")?;
        tables::sample_locations(stbl)
    }
}

/// Reads the facts we need from one `trak`.
fn parse_track(moov_index: usize, trak: &Atom) -> Result<RefTrack, String> {
    let tkhd = trak
        .child(b"tkhd")
        .and_then(Atom::payload)
        .ok_or("no tkhd")?;
    let tk = tables::TimedHeader::parse(tkhd, 8)?;
    let track_id = u32::from_be_bytes([tk.middle[0], tk.middle[1], tk.middle[2], tk.middle[3]]);

    let mdia = trak.child(b"mdia").ok_or("no mdia")?;
    let mdhd = mdia
        .child(b"mdhd")
        .and_then(Atom::payload)
        .ok_or("no mdhd")?;
    let timescale = tables::TimedHeader::parse(mdhd, 4)?.timescale();
    if timescale == 0 {
        return Err("media timescale is zero".into());
    }
    let hdlr = mdia
        .child(b"hdlr")
        .and_then(Atom::payload)
        .ok_or("no hdlr")?;
    if hdlr.len() < 12 {
        return Err("hdlr too short".into());
    }
    let handler = FourCC::from_slice(&hdlr[8..12]);

    let stbl = mdia.find(&[b"minf", b"stbl"]).ok_or("no stbl")?;
    let stsd = stbl
        .child(b"stsd")
        .and_then(Atom::payload)
        .ok_or("no stsd")?;
    let (codec, entry) = first_sample_entry(stsd)?;

    let stts = tables::parse_stts(
        stbl.child(b"stts")
            .and_then(Atom::payload)
            .ok_or("no stts")?,
    )?;
    // The most common delta. Canon writes a single entry; other writers may end with a short one.
    let sample_delta = stts.iter().max_by_key(|e| e.0).map(|e| e.1).unwrap_or(0);
    if sample_delta == 0 {
        return Err("stts has no sample duration".into());
    }

    let (fixed_sample_size, count, sizes) = tables::parse_stsz(
        stbl.child(b"stsz")
            .and_then(Atom::payload)
            .ok_or("no stsz")?,
    )?;
    let mean_sample_size = if fixed_sample_size > 0 {
        fixed_sample_size as f64
    } else if sizes.is_empty() {
        0.0
    } else {
        sizes.iter().map(|&s| s as f64).sum::<f64>() / sizes.len() as f64
    };
    let samples_per_chunk = stbl
        .child(b"stsc")
        .and_then(Atom::payload)
        .map(tables::parse_stsc)
        .transpose()?
        .and_then(|v| v.first().map(|e| e.1))
        .unwrap_or(0);

    let edit_media_time = match trak.find(&[b"edts", b"elst"]).and_then(Atom::payload) {
        Some(p) => tables::parse_elst_first_media_time(p)?,
        None => None,
    };

    let is_pcm = PCM_CODECS.iter().any(|c| **c == codec.0);
    let kind = match &handler.0 {
        b"vide" => TrackKind::Video,
        b"soun" if is_pcm => TrackKind::PcmAudio,
        b"soun" => TrackKind::CompressedAudio,
        b"tmcd" => TrackKind::Timecode,
        _ if codec.0 == *b"tmcd" => TrackKind::Timecode,
        b"meta" | b"text" | b"sbtl" | b"hint" => TrackKind::Metadata,
        _ => TrackKind::Other,
    };

    let audio = if handler.0 == *b"soun" {
        Some(parse_audio_entry(codec, entry)?)
    } else {
        None
    };
    let video = if handler.0 == *b"vide" {
        Some(parse_video_entry(entry)?)
    } else {
        None
    };

    Ok(RefTrack {
        moov_index,
        track_id,
        handler,
        kind,
        codec,
        timescale,
        sample_delta,
        sample_count: count as u64,
        fixed_sample_size,
        mean_sample_size,
        samples_per_chunk,
        has_ctts: stbl.child(b"ctts").is_some(),
        has_stss: stbl.child(b"stss").is_some(),
        edit_media_time,
        audio,
        video,
    })
}

/// Returns the type and bytes (header included) of the first sample entry in `stsd`.
fn first_sample_entry(stsd: &[u8]) -> Result<(FourCC, &[u8]), String> {
    let mut r = ByteReader::new(stsd);
    r.skip(4)?; // version + flags
    let count = r.u32()?;
    if count == 0 {
        return Err("stsd has no sample entry".into());
    }
    let start = r.pos();
    let size = r.u32()? as usize;
    let kind = FourCC::from_slice(r.bytes(4)?);
    if size < 16 || start + size > stsd.len() {
        return Err(format!("sample entry {kind} has a bad size"));
    }
    Ok((kind, &stsd[start..start + size]))
}

/// Finds a child box inside a sample entry, starting `children_at` bytes into the entry, and
/// returns a copy of its payload.
fn entry_child(entry: &[u8], children_at: usize, kind: &[u8; 4]) -> Option<Vec<u8>> {
    let children = atom::parse_atoms(entry.get(children_at..)?).ok()?;
    let found = children.into_iter().find(|a| a.kind.0 == *kind)?;
    found.payload().map(<[u8]>::to_vec)
}

/// Reads channels, bit depth and rate from a sound sample entry.
fn parse_audio_entry(codec: FourCC, entry: &[u8]) -> Result<AudioFormat, String> {
    let mut r = ByteReader::new(entry);
    r.skip(16)?; // size, type, 6 reserved, data reference index
    let version = r.u16()?;
    r.skip(6)?; // revision, vendor
    let channels = r.u16()? as u32;
    let bits = r.u16()? as u32;
    r.skip(4)?; // compression id, packet size
    let rate = r.u32()? >> 16;
    let mut fmt = AudioFormat {
        channels,
        bits_per_sample: bits,
        sample_rate: rate,
    };
    let children_at = match version {
        1 => 52,
        2 => {
            // QuickTime sound description v2 stores the real values in an extension.
            r.skip(4)?; // size of struct only
            let rate = f64::from_bits(r.u64()?);
            fmt.channels = r.u32()?;
            r.skip(4)?;
            fmt.bits_per_sample = r.u32()?;
            fmt.sample_rate = rate.round() as u32;
            72
        }
        _ => 36,
    };
    // ISO `ipcm`/`fpcm` keep the true sample size in a `pcmC` box.
    if matches!(&codec.0, b"ipcm" | b"fpcm") {
        if let Some(pcmc) = entry_child(entry, children_at, b"pcmC") {
            if pcmc.len() >= 6 {
                fmt.bits_per_sample = pcmc[5] as u32;
            }
        }
    }
    Ok(fmt)
}

/// Reads size and decoder configuration from a visual sample entry.
fn parse_video_entry(entry: &[u8]) -> Result<VideoFormat, String> {
    let mut r = ByteReader::new(entry);
    r.skip(32)?;
    let width = r.u16()?;
    let height = r.u16()?;
    // A visual sample entry has 78 bytes of fields after its 8-byte header.
    let children_at = 86;
    let mut config_kind = None;
    let mut config = Vec::new();
    for kind in [b"hvcC", b"avcC"] {
        if let Some(c) = entry_child(entry, children_at, kind) {
            config_kind = Some(FourCC::new(kind));
            config = c;
            break;
        }
    }
    Ok(VideoFormat {
        width,
        height,
        config_kind,
        config,
    })
}
