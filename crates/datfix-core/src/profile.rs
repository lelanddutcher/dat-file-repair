//! The built-in profile bank: decoder settings harvested from healthy Canon clips.
//!
//! Most people with a broken `.DAT` do not have a healthy clip recorded in exactly the same
//! mode. A *profile* is the part of such a clip that a repair needs: the `ftyp` box and a
//! stripped `moov` that keeps only codec settings and track templates. It holds **no media**:
//! no samples, no thumbnail (Canon's `uuid` box is dropped), no location (`loci` is dropped),
//! no owner data. Sample tables are reduced to one placeholder entry.
//!
//! Profiles are keyed by body code, codec, resolution, frame rate and chroma/bit depth, plus
//! the `repi` layout of the clip, and are embedded in the library as JSON
//! (`profiles/bank.json`). `datfix profiles harvest` rebuilds that file from a folder of
//! camera originals.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::codec::VideoCodec;
use crate::error::{Error, Result};
use crate::layout::{self, Family, FrameRate};
use crate::mp4::atom::{self, Atom, Body};
use crate::mp4::reference::{Reference, TrackKind};
use crate::mp4::synth;
use crate::mp4::tables::{self, TimedHeader};
use crate::read_at::FileSource;
use crate::repi::{self, StreamShape};

/// The embedded bank.
const BANK_JSON: &str = include_str!("../profiles/bank.json");

/// One set of decoder settings and track templates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    /// Model string from the clip, for example `Canon EOS R5m2`.
    pub body: String,
    /// Body code from the per-frame records, hex (`006049`).
    pub body_code: Option<String>,
    pub family: Family,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub chroma: String,
    pub bit_depth: u32,
    pub timescale: u32,
    pub frame_duration: u32,
    pub long_gop: bool,
    /// Short audio description, for example `ipcm 24-bit x4`.
    pub audio: String,
    /// `repi` layout of the source clip.
    pub shapes: Vec<StreamShape>,
    /// `ftyp` box, hex.
    pub ftyp: String,
    /// Stripped `moov` box, hex.
    pub moov: String,
    /// How many healthy clips produced exactly these bytes.
    pub sources: u32,
}

/// The bank file layout.
#[derive(Serialize, Deserialize)]
pub struct Bank {
    pub version: u32,
    pub profiles: Vec<Profile>,
}

impl Profile {
    pub fn frame_rate(&self) -> FrameRate {
        FrameRate {
            timescale: self.timescale,
            frame_duration: self.frame_duration,
        }
    }

    /// One-line description for listings.
    pub fn describe(&self) -> String {
        format!(
            "{} {} {}x{} {} {} {}-bit {}, {}",
            self.body,
            if self.codec == "avc1" {
                "H.264"
            } else {
                "HEVC"
            },
            self.width,
            self.height,
            self.frame_rate(),
            self.chroma,
            self.bit_depth,
            if self.long_gop {
                "Long GOP"
            } else {
                "All-Intra"
            },
            self.audio
        )
    }

    /// Turns the profile into a reference that the repair can use like a healthy clip.
    pub fn to_reference(&self) -> Result<Reference, String> {
        let ftyp = from_hex(&self.ftyp)?;
        let moov_bytes = from_hex(&self.moov)?;
        let moov = atom::parse_atoms(&moov_bytes)?
            .pop()
            .ok_or("empty profile moov")?;
        let mut r = Reference::from_parts(
            &PathBuf::from(format!("built-in profile {}", self.id)),
            0,
            ftyp,
            moov,
        )?;
        r.repi_shapes = Some(self.shapes.clone());
        Ok(r)
    }

    /// Bytes of the video sample entry, used to tell distinct decoder settings apart.
    pub fn video_key(&self) -> Vec<u8> {
        self.to_reference()
            .ok()
            .and_then(|r| {
                let v = r.video_track()?.video.as_ref()?.config.clone();
                Some(v)
            })
            .unwrap_or_default()
    }
}

/// The embedded profiles.
pub fn bank() -> &'static [Profile] {
    static BANK: OnceLock<Vec<Profile>> = OnceLock::new();
    BANK.get_or_init(|| {
        serde_json::from_str::<Bank>(BANK_JSON)
            .map(|b| b.profiles)
            .unwrap_or_default()
    })
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Zeroes times and duration in an `mvhd`/`tkhd`/`mdhd` payload.
fn zero_times(atom: &mut Atom, middle: usize) -> Result<(), String> {
    let mut h = TimedHeader::parse(atom.payload().ok_or("not a leaf")?, middle)?;
    h.creation_time = 0;
    h.modification_time = 0;
    h.duration = 0;
    atom.body = Body::Leaf(h.to_payload());
    Ok(())
}

/// Keeps the codec settings of a sample table and replaces every table with a placeholder.
fn strip_stbl(stbl: &Atom) -> Result<Atom, String> {
    let mut out = vec![stbl.child(b"stsd").ok_or("no stsd")?.clone()];
    let stts = tables::parse_stts(
        stbl.child(b"stts")
            .and_then(Atom::payload)
            .ok_or("no stts")?,
    )?;
    let delta = stts.iter().max_by_key(|e| e.0).map_or(1, |e| e.1);
    out.push(tables::build_stts(&[(1, delta)]));
    if let Some(stss) = stbl.child(b"stss") {
        // An empty stss still says "not every frame is a keyframe" (Long GOP).
        let _ = stss;
        out.push(tables::build_stss(&[]));
    }
    if stbl.child(b"ctts").is_some() {
        out.push(tables::build_ctts(&[]));
    }
    let per_chunk = stbl
        .child(b"stsc")
        .and_then(Atom::payload)
        .map(tables::parse_stsc)
        .transpose()?
        .and_then(|v| v.first().map(|e| e.1))
        .unwrap_or(1);
    out.push(tables::build_stsc(&[per_chunk]));
    let (fixed, _, _) = tables::parse_stsz(
        stbl.child(b"stsz")
            .and_then(Atom::payload)
            .ok_or("no stsz")?,
    )?;
    out.push(tables::build_stsz_fixed(fixed, 0));
    out.push(tables::build_co64(&[]));
    Ok(Atom::container(b"stbl", out))
}

/// Strips a `trak` down to its template.
fn strip_trak(trak: &Atom) -> Result<Atom, String> {
    let mut children = Vec::new();
    for c in trak.children() {
        match &c.kind.0 {
            b"tkhd" => {
                // Orientation belongs to one clip, not to the recording mode: reset the matrix.
                let mut h = TimedHeader::parse(c.payload().ok_or("tkhd")?, 8)?;
                h.creation_time = 0;
                h.modification_time = 0;
                h.duration = 0;
                if h.tail.len() >= 52 {
                    let mut m = Vec::new();
                    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
                        m.extend_from_slice(&v.to_be_bytes());
                    }
                    h.tail[16..52].copy_from_slice(&m);
                }
                children.push(Atom::leaf(b"tkhd", h.to_payload()));
            }
            b"tref" => children.push(c.clone()),
            b"edts" => {
                let mt = c
                    .find(&[b"elst"])
                    .and_then(Atom::payload)
                    .map(tables::parse_elst_first_media_time)
                    .transpose()?
                    .flatten()
                    .unwrap_or(0);
                children.push(Atom::container(b"edts", vec![tables::build_elst(0, mt)]));
            }
            b"mdia" => {
                let mut mdia = Vec::new();
                for m in c.children() {
                    match &m.kind.0 {
                        b"mdhd" => {
                            let mut t = m.clone();
                            zero_times(&mut t, 4)?;
                            mdia.push(t);
                        }
                        b"hdlr" => mdia.push(m.clone()),
                        b"minf" => {
                            let mut minf = Vec::new();
                            for n in m.children() {
                                match &n.kind.0 {
                                    b"vmhd" | b"smhd" | b"nmhd" | b"gmhd" | b"hmhd" | b"dinf" => {
                                        minf.push(n.clone())
                                    }
                                    b"stbl" => minf.push(strip_stbl(n)?),
                                    _ => {}
                                }
                            }
                            mdia.push(Atom::container(b"minf", minf));
                        }
                        _ => {}
                    }
                }
                children.push(Atom::container(b"mdia", mdia));
            }
            _ => {}
        }
    }
    Ok(Atom::container(b"trak", children))
}

/// Strips a camera `moov` to codec settings and track templates.
pub fn strip_moov(moov: &Atom) -> Result<(Atom, Option<String>), String> {
    let mut children = Vec::new();
    let mut model = None;
    for c in moov.children() {
        match &c.kind.0 {
            b"mvhd" => {
                let mut t = c.clone();
                zero_times(&mut t, 4)?;
                children.push(t);
            }
            b"trak" => children.push(strip_trak(c)?),
            b"udta" => model = synth::model_of(c),
            _ => {} // uuid (thumbnail), free, meta: dropped
        }
    }
    if let Some(m) = &model {
        children.push(synth::udta_model(m));
    }
    Ok((Atom::container(b"moov", children), model))
}

/// Short body name for ids (`Canon EOS R5m2` -> `R5m2`).
fn short_body(body: &str) -> String {
    body.trim_start_matches("Canon ")
        .trim_start_matches("EOS ")
        .replace(' ', "")
}

/// Builds a profile from one healthy camera original.
pub fn harvest(path: &Path) -> Result<Profile, String> {
    let reference = Reference::open(path).map_err(|e| e.to_string())?;
    let brand_ok = reference
        .ftyp
        .windows(4)
        .any(|w| w == b"CAEP" || w == b"XFVC");
    if !brand_ok {
        return Err("not a Canon camera original (no CAEP/XFVC brand)".into());
    }
    let shapes = reference
        .repi_shapes
        .clone()
        .ok_or("its mdat has no Canon recovery index")?;
    let vt = reference.video_track().ok_or("no video track")?;
    let vf = vt.video.as_ref().ok_or("no video format")?;
    let codec = VideoCodec::from_config(vt.codec, vf.config_kind, &vf.config)?;
    let (width, height, chroma, bit_depth) = match &codec {
        VideoCodec::Hevc(c) => {
            let s = &c.sps[0];
            let chroma = ["4:0:0", "4:2:0", "4:2:2", "4:4:4"][s.chroma_format_idc.min(3) as usize];
            (s.width, s.height, chroma.to_string(), s.bit_depth)
        }
        VideoCodec::Avc(c) => (c.sps[0].width, c.sps[0].height, "4:2:0".to_string(), 8),
    };

    // Body code and family from the clip's own repi groups and records.
    let src = FileSource::open(path).map_err(|e| e.to_string())?;
    let boxes = atom::scan_top_level(&src).map_err(|e| e.to_string())?;
    let mdat = boxes
        .iter()
        .find(|b| b.kind.0 == *b"mdat")
        .ok_or("no mdat")?;
    let index = repi::walk_from(&src, mdat.offset + mdat.header_len, 48, &mut |_| true)
        .map_err(|e| e.to_string())?;
    let lay = layout::analyze(&src, &index, "");

    let (moov, model) = strip_moov(&reference.moov)?;
    let body = model.unwrap_or_else(|| lay.body.unwrap_or("Canon").to_string());
    let audio_tracks: Vec<_> = reference
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::PcmAudio | TrackKind::CompressedAudio))
        .collect();
    let audio = match audio_tracks.first() {
        None => "no audio".to_string(),
        Some(a) => {
            let f = a.audio.as_ref();
            format!(
                "{} {}-bit {}ch x{}",
                a.codec,
                f.map_or(0, |f| f.bits_per_sample),
                f.map_or(0, |f| f.channels),
                audio_tracks.len()
            )
        }
    };
    let long_gop = vt.has_stss || vt.has_ctts;
    let rate = FrameRate {
        timescale: vt.timescale,
        frame_duration: vt.sample_delta,
    };
    let id = format!(
        "{}-{}-{}x{}-{:.2}p-{}-{}bit-{}-{}",
        short_body(&body),
        if vt.codec.0 == *b"avc1" {
            "h264"
        } else {
            "hevc"
        },
        width,
        height,
        rate.fps(),
        chroma.replace(':', ""),
        bit_depth,
        if long_gop { "lgop" } else { "alli" },
        audio
            .split_whitespace()
            .next()
            .unwrap_or("none")
            .replace("no", "noaudio")
    );
    Ok(Profile {
        id,
        body,
        body_code: lay.body_code.map(|c| layout::hex(&c)),
        family: lay.family,
        codec: vt.codec.to_string(),
        width,
        height,
        chroma,
        bit_depth,
        timescale: rate.timescale,
        frame_duration: rate.frame_duration,
        long_gop,
        audio,
        shapes,
        ftyp: layout::hex(&reference.ftyp),
        moov: layout::hex(&moov.to_bytes()),
        sources: 1,
    })
}

/// Adds a profile to a list, merging exact duplicates (same bytes) and making ids unique.
pub fn merge(into: &mut Vec<Profile>, p: Profile) {
    if let Some(same) = into
        .iter_mut()
        .find(|q| q.ftyp == p.ftyp && q.moov == p.moov && q.shapes == p.shapes)
    {
        same.sources += p.sources;
        return;
    }
    let mut p = p;
    let base = p.id.clone();
    let mut n = 2;
    while into.iter().any(|q| q.id == p.id) {
        p.id = format!("{base}-v{n}");
        n += 1;
    }
    into.push(p);
}

/// Profiles from a harvest, and the files that could not be used with the reason.
pub type Harvest = (Vec<Profile>, Vec<(PathBuf, String)>);

/// Harvests every `.MP4` under `paths` (recursively). Returns the profiles and the files that
/// could not be used, with the reason.
pub fn harvest_paths(
    paths: &[PathBuf],
    on_file: &mut dyn FnMut(&Path, &std::result::Result<String, String>),
) -> Result<Harvest> {
    let mut files = Vec::new();
    for p in paths {
        collect_mp4s(p, 12, &mut files)
            .map_err(|e| Error::io(format!("cannot list {}", p.display()), e))?;
    }
    files.sort();
    let mut out = Vec::new();
    let mut rejected = Vec::new();
    for f in files {
        match harvest(&f) {
            Ok(p) => {
                on_file(&f, &Ok(p.id.clone()));
                merge(&mut out, p);
            }
            Err(e) => {
                on_file(&f, &Err(e.clone()));
                rejected.push((f, e));
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((out, rejected))
}

/// Lists `.mp4` files under `path`, skipping hidden files, up to `depth` folders deep.
pub fn collect_mp4s(path: &Path, depth: u32, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    for e in std::fs::read_dir(path)?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if depth > 0 {
                collect_mp4s(&p, depth - 1, out)?;
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
        {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_parses_and_turns_into_references() {
        for p in bank() {
            let r = p.to_reference().unwrap_or_else(|e| panic!("{}: {e}", p.id));
            let vt = r.video_track().unwrap();
            assert_eq!(vt.timescale, p.timescale, "{}", p.id);
            assert_eq!(vt.sample_delta, p.frame_duration, "{}", p.id);
            let vf = vt.video.as_ref().unwrap();
            VideoCodec::from_config(vt.codec, vf.config_kind, &vf.config).unwrap();
            // No media and no personal data: no uuid (thumbnail), no location.
            let moov = from_hex(&p.moov).unwrap();
            assert!(
                !moov
                    .windows(4)
                    .any(|w| w == b"uuid" || w == b"loci" || w == b"CNTH")
            );
            assert!(
                moov.len() < 16 * 1024,
                "{} moov is {} bytes",
                p.id,
                moov.len()
            );
        }
    }
}
