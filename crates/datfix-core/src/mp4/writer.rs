//! Building the output `moov` and writing the repaired file.
//!
//! Output layout:
//!
//! ```text
//! ftyp   copied from the reference
//! moov   the reference's moov, with durations and sample tables rebuilt
//! mdat   64-bit header, then the .DAT bytes from the first repi group on, copied unchanged
//! ```
//!
//! Because the `.DAT` bytes are copied as they are, the `repi` headers and any padding stay
//! inside `mdat`. Players ignore them: the chunk offsets point only at sample data.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mapping::TrackPlan;
use crate::read_at::ReadAt;
use crate::timing::VideoTiming;

use super::atom::{Atom, Body};
use super::reference::{RefTrack, Reference};
use super::tables::{self, TimedHeader};

/// Size of the copy buffer. Large buffers keep throughput high on network and USB disks.
const COPY_BUFFER: usize = 8 * 1024 * 1024;

/// One track of the output.
pub struct TrackOutput<'a> {
    pub ref_track: &'a RefTrack,
    pub plan: &'a TrackPlan,
    /// Keyframes and composition offsets; video only.
    pub video: Option<&'a VideoTiming>,
}

impl TrackOutput<'_> {
    /// Duration in the track's media timescale.
    pub fn media_duration(&self) -> u64 {
        self.plan.duration_units() * self.ref_track.sample_delta as u64
    }
}

/// Converts a duration between timescales, rounding to nearest.
pub fn rescale(value: u64, from: u32, to: u32) -> u64 {
    ((value as u128 * to as u128 + from as u128 / 2) / from as u128) as u64
}

/// A display rotation for the video track, clockwise, as people say it ("turn the picture
/// 90 degrees to the right").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u32", try_from = "u32")]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    /// Degrees clockwise.
    pub fn degrees(self) -> u32 {
        match self {
            Rotation::R0 => 0,
            Rotation::R90 => 90,
            Rotation::R180 => 180,
            Rotation::R270 => 270,
        }
    }

    /// Parses 0, 90, 180 or 270 (degrees clockwise). `-90` is accepted as 270.
    pub fn from_degrees(degrees: i32) -> Option<Rotation> {
        match degrees.rem_euclid(360) {
            0 => Some(Rotation::R0),
            90 => Some(Rotation::R90),
            180 => Some(Rotation::R180),
            270 => Some(Rotation::R270),
            _ => None,
        }
    }

    /// The `tkhd` display matrix (nine big-endian 32-bit values, 16.16 / 2.30 fixed point)
    /// for a picture `width` x `height` pixels. Same values ffmpeg and iPhones write.
    pub fn matrix(self, width: u32, height: u32) -> [u8; 36] {
        const ONE: i32 = 0x10000;
        let (w, h) = ((width as i32) << 16, (height as i32) << 16);
        let (a, b, c, d, tx, ty) = match self {
            Rotation::R0 => (ONE, 0, 0, ONE, 0, 0),
            Rotation::R90 => (0, ONE, -ONE, 0, h, 0),
            Rotation::R180 => (-ONE, 0, 0, -ONE, w, h),
            Rotation::R270 => (0, -ONE, ONE, 0, 0, w),
        };
        let mut out = [0u8; 36];
        for (i, v) in [a, b, 0, c, d, 0, tx, ty, 0x4000_0000].iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Reads the rotation back from a display matrix. `None` for anything that is not a plain
    /// quarter turn (scaling, mirroring, shearing).
    pub fn from_matrix(m: &[u8]) -> Option<Rotation> {
        if m.len() < 36 {
            return None;
        }
        let v = |i: usize| i32::from_be_bytes([m[i * 4], m[i * 4 + 1], m[i * 4 + 2], m[i * 4 + 3]]);
        const ONE: i32 = 0x10000;
        match (v(0), v(1), v(3), v(4)) {
            (ONE, 0, 0, ONE) => Some(Rotation::R0),
            (0, ONE, n, 0) if n == -ONE => Some(Rotation::R90),
            (a, 0, 0, d) if a == -ONE && d == -ONE => Some(Rotation::R180),
            (0, n, ONE, 0) if n == -ONE => Some(Rotation::R270),
            _ => None,
        }
    }
}

impl From<Rotation> for u32 {
    fn from(r: Rotation) -> u32 {
        r.degrees()
    }
}

impl TryFrom<u32> for Rotation {
    type Error = String;
    fn try_from(d: u32) -> Result<Self, String> {
        i32::try_from(d)
            .ok()
            .and_then(Rotation::from_degrees)
            .ok_or_else(|| format!("rotation must be 0, 90, 180 or 270 degrees, not {d}"))
    }
}

impl std::fmt::Display for Rotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\u{b0}", self.degrees())
    }
}

/// Offset of the display matrix inside a `tkhd` [`TimedHeader::tail`].
const TKHD_MATRIX: std::ops::Range<usize> = 16..52;

/// The rotation stored in a `trak`'s `tkhd`, if it is a plain quarter turn.
pub fn trak_rotation(trak: &Atom) -> Option<Rotation> {
    let h = TimedHeader::parse(trak.child(b"tkhd")?.payload()?, 8).ok()?;
    Rotation::from_matrix(h.tail.get(TKHD_MATRIX)?)
}

/// How the output `moov` treats the reference's own metadata and orientation.
#[derive(Clone, Debug, Default)]
pub struct MoovOptions {
    /// Copy the reference's Canon `uuid` box (thumbnail, EXIF with serial numbers, lens and
    /// dates), `meta` and every `udta` child verbatim. Off by default: those describe the
    /// reference clip, not this recording.
    pub keep_reference_metadata: bool,
    /// Model string written into the rebuilt `udta` (`Canon EOS R5`). `None` keeps the
    /// reference's own `modl`, if it has one.
    pub model: Option<String>,
    /// Display rotation of the video track. `None` keeps the reference's matrix.
    pub rotation: Option<Rotation>,
}

/// What [`build_moov_with`] left out.
#[derive(Clone, Debug, Default)]
pub struct MoovSummary {
    /// Sample-table boxes of the reference that could not be rebuilt (`sdtp (track 1)`).
    pub dropped: Vec<String>,
    /// Reference metadata left out for privacy, in plain words.
    pub withheld: Vec<String>,
}

/// Plain description of a metadata box left out of the output.
fn describe_withheld(a: &Atom) -> String {
    match &a.kind.0 {
        b"uuid" => {
            let canon = a
                .payload()
                .is_some_and(|p| p.starts_with(&crate::select::CANON_UUID));
            if canon {
                "Canon metadata box (thumbnail picture, camera and lens serial numbers, firmware, owner and capture dates)"
                    .into()
            } else {
                "vendor metadata box (uuid)".into()
            }
        }
        b"meta" => "metadata box (meta)".into(),
        b"loci" | b"\xa9xyz" => "location (GPS)".into(),
        other => format!("user data '{}'", String::from_utf8_lossy(other)),
    }
}

/// Rebuilds a `udta` with the maker and model only. Every other child (location, rating,
/// owner, free space) is listed in `withheld`.
fn scrubbed_udta(
    udta: Option<&Atom>,
    model: Option<&str>,
    withheld: &mut Vec<String>,
) -> Option<Atom> {
    if let Some(u) = udta {
        let payload = match &u.body {
            Body::Leaf(p) => p.clone(),
            Body::Container(c) => {
                let mut v = Vec::new();
                c.iter().for_each(|a| a.write_to(&mut v));
                v
            }
        };
        for child in super::atom::parse_atoms(&payload).unwrap_or_default() {
            if !matches!(
                &child.kind.0,
                b"manu" | b"modl" | b"free" | b"skip" | b"urat"
            ) {
                withheld.push(describe_withheld(&child));
            }
        }
    }
    let model = model
        .map(str::to_string)
        .or_else(|| udta.and_then(super::synth::model_of))?;
    Some(super::synth::udta_model(&model))
}

/// Builds the new `moov`, copying the reference's metadata verbatim (the behaviour before
/// metadata scrubbing). See [`build_moov_with`].
pub fn build_moov(
    reference: &Reference,
    tracks: &[TrackOutput],
    offset_delta: i64,
    time: u64,
) -> Result<(Atom, Vec<String>), String> {
    let opts = MoovOptions {
        keep_reference_metadata: true,
        ..MoovOptions::default()
    };
    build_moov_with(reference, tracks, offset_delta, time, &opts).map(|(a, s)| (a, s.dropped))
}

/// Builds the new `moov`.
///
/// `offset_delta` is added to every `.DAT` offset to get the output file offset.
/// `time` (seconds since 1904) becomes the creation and modification time.
/// Returns the tree and what was left out.
pub fn build_moov_with(
    reference: &Reference,
    tracks: &[TrackOutput],
    offset_delta: i64,
    time: u64,
    opts: &MoovOptions,
) -> Result<(Atom, MoovSummary), String> {
    let movie_ts = reference.movie_timescale;
    let mut summary = MoovSummary::default();
    let mut children = Vec::new();
    let mut movie_duration = 0u64;
    let scrub = !opts.keep_reference_metadata;
    let mut udta_done = false;

    for (i, child) in reference.moov.children().iter().enumerate() {
        match &child.kind.0 {
            b"trak" => {
                let Some(t) = tracks.iter().find(|t| t.ref_track.moov_index == i) else {
                    continue;
                };
                let (trak, track_duration) =
                    build_trak(child, t, movie_ts, offset_delta, time, opts, &mut summary)?;
                movie_duration = movie_duration.max(track_duration);
                children.push(trak);
            }
            // Padding the camera reserved for its own header; not needed.
            b"free" | b"skip" => {}
            b"udta" if scrub => {
                udta_done = true;
                children.extend(scrubbed_udta(
                    Some(child),
                    opts.model.as_deref(),
                    &mut summary.withheld,
                ));
            }
            b"uuid" | b"meta" if scrub => summary.withheld.push(describe_withheld(child)),
            _ => children.push(child.clone()),
        }
    }
    if scrub && !udta_done {
        children.extend(scrubbed_udta(
            None,
            opts.model.as_deref(),
            &mut summary.withheld,
        ));
    }

    let mvhd = children
        .iter_mut()
        .find(|a| a.kind.0 == *b"mvhd")
        .ok_or("reference moov has no mvhd")?;
    let mut h = TimedHeader::parse(mvhd.payload().unwrap_or_default(), 4)?;
    h.duration = movie_duration;
    h.creation_time = time;
    h.modification_time = time;
    mvhd.body = Body::Leaf(h.to_payload());

    summary.dropped.sort();
    summary.dropped.dedup();
    summary.withheld.dedup();
    Ok((Atom::container(b"moov", children), summary))
}

/// Rebuilds one `trak`. Returns it and its duration in the movie timescale.
fn build_trak(
    src: &Atom,
    t: &TrackOutput,
    movie_ts: u32,
    offset_delta: i64,
    time: u64,
    opts: &MoovOptions,
    summary: &mut MoovSummary,
) -> Result<(Atom, u64), String> {
    let mut trak = src.clone();
    if !opts.keep_reference_metadata {
        // Per-track user data and metadata (names, locations) belong to the reference.
        trak.children_mut().retain(|a| {
            let keep = !matches!(&a.kind.0, b"udta" | b"meta" | b"uuid");
            if !keep {
                summary.withheld.push(describe_withheld(a));
            }
            keep
        });
    }
    let media_duration = t.media_duration();
    let track_duration = rescale(media_duration, t.ref_track.timescale, movie_ts);

    // tkhd: duration in the movie timescale.
    let tkhd = trak.find_mut(&[b"tkhd"]).ok_or("no tkhd")?;
    let mut h = TimedHeader::parse(tkhd.payload().unwrap_or_default(), 8)?;
    h.duration = track_duration;
    h.creation_time = time;
    h.modification_time = time;
    if let Some(r) = opts.rotation.filter(|_| t.video.is_some()) {
        if h.tail.len() >= 60 {
            let size = |at: usize| u32::from_be_bytes(h.tail[at..at + 4].try_into().unwrap()) >> 16;
            let m = r.matrix(size(52), size(56));
            h.tail[TKHD_MATRIX].copy_from_slice(&m);
        }
    }
    tkhd.body = Body::Leaf(h.to_payload());

    // Edit list: one edit covering the whole track. Video with B-frames starts `shift` frames
    // in, so the first displayed frame plays at time zero. The shift always comes from the
    // rebuilt video tables, never from the reference: a Long-GOP reference's one-frame shift on
    // an All-Intra .DAT would hide the first frame and drop the last.
    let media_time = match t.video {
        Some(v) => v.shift_frames as i64 * t.ref_track.sample_delta as i64,
        None => t.ref_track.edit_media_time.unwrap_or(0),
    };
    let needs_edit = trak.child(b"edts").is_some() || media_time != 0;
    trak.children_mut().retain(|a| a.kind.0 != *b"edts");
    if needs_edit {
        let edts = Atom::container(
            b"edts",
            vec![tables::build_elst(track_duration, media_time)],
        );
        // Keep the usual order: tkhd, edts, then the rest.
        let at = trak
            .children()
            .iter()
            .position(|a| a.kind.0 == *b"tkhd")
            .map_or(0, |p| p + 1);
        trak.children_mut().insert(at, edts);
    }

    // mdhd: duration in the media timescale.
    let mdhd = trak.find_mut(&[b"mdia", b"mdhd"]).ok_or("no mdhd")?;
    let mut h = TimedHeader::parse(mdhd.payload().unwrap_or_default(), 4)?;
    h.duration = media_duration;
    h.creation_time = time;
    h.modification_time = time;
    mdhd.body = Body::Leaf(h.to_payload());

    // Sample table.
    let stbl = trak
        .find_mut(&[b"mdia", b"minf", b"stbl"])
        .ok_or("no stbl")?;
    let stsd = stbl.child(b"stsd").cloned().ok_or("no stsd")?;
    for a in stbl.children() {
        if !matches!(
            &a.kind.0,
            b"stsd" | b"stts" | b"stss" | b"ctts" | b"stsc" | b"stsz" | b"stco" | b"co64"
        ) {
            summary
                .dropped
                .push(format!("{} (track {})", a.kind, t.ref_track.track_id));
        }
    }
    *stbl = Atom::container(b"stbl", build_stbl(stsd, t, offset_delta)?);
    Ok((trak, track_duration))
}

/// Builds the children of a new `stbl`.
fn build_stbl(stsd: Atom, t: &TrackOutput, offset_delta: i64) -> Result<Vec<Atom>, String> {
    let plan = t.plan;
    let count =
        u32::try_from(plan.sample_count).map_err(|_| "too many samples for one MP4 track")?;
    let delta = t.ref_track.sample_delta;
    let stts = if plan.steps.is_empty() {
        tables::build_stts_constant(plan.sample_count, delta)
    } else {
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &s in &plan.steps {
            let d = s.checked_mul(delta).ok_or("sample duration overflow")?;
            match runs.last_mut() {
                Some((n, last)) if *last == d && *n < u32::MAX => *n += 1,
                _ => runs.push((1, d)),
            }
        }
        tables::build_stts(&runs)
    };
    let mut out = vec![stsd, stts];
    if let Some(v) = t.video {
        if !v.all_sync {
            out.push(tables::build_stss(&v.sync_samples));
        }
        if v.reordered || t.ref_track.has_ctts {
            let delta = t.ref_track.sample_delta;
            let offsets: Vec<u32> = v.composition.iter().map(|&c| c * delta).collect();
            out.push(tables::build_ctts(&offsets));
        }
    }
    let per_chunk: Vec<u32> = plan.chunks.iter().map(|c| c.samples).collect();
    out.push(tables::build_stsc(&per_chunk));
    out.push(if plan.fixed_size > 0 {
        tables::build_stsz_fixed(plan.fixed_size, count)
    } else {
        tables::build_stsz(&plan.sizes)
    });
    let offsets = plan
        .chunks
        .iter()
        .map(|c| {
            u64::try_from(c.offset as i64 + offset_delta)
                .map_err(|_| "chunk offset before file start".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    out.push(tables::build_co64(&offsets));
    Ok(out)
}

/// Writes `ftyp + moov + mdat` to `path`, copying `src[copy_start..copy_end]` as the payload
/// and then `silence` zero bytes (inserted PCM silence points there).
///
/// `progress(done, total)` is called after each block and returns `false` to cancel.
#[allow(clippy::too_many_arguments)]
pub fn write_file<R: ReadAt + ?Sized>(
    path: &Path,
    ftyp: &[u8],
    moov: &[u8],
    src: &R,
    copy_start: u64,
    copy_end: u64,
    silence: u64,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<u64> {
    let ctx = |what: &str| format!("cannot {what} {}", path.display());
    // `create_new` refuses to clobber anything; the caller picked a fresh temp name.
    let mut f = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::io(ctx("create"), e))?;
    let payload = copy_end - copy_start + silence;
    let mut header = Vec::with_capacity(ftyp.len() + moov.len() + 16);
    header.extend_from_slice(ftyp);
    header.extend_from_slice(moov);
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(b"mdat");
    header.extend_from_slice(&(16 + payload).to_be_bytes());
    f.write_all(&header)
        .map_err(|e| Error::io(ctx("write"), e))?;

    let mut buf = vec![0u8; COPY_BUFFER];
    let mut pos = copy_start;
    while pos < copy_end {
        let n = ((copy_end - pos) as usize).min(buf.len());
        src.read_exact_at(&mut buf[..n], pos)
            .map_err(|e| Error::io(format!("cannot read the .DAT at byte {pos}"), e))?;
        f.write_all(&buf[..n])
            .map_err(|e| Error::io(ctx("write"), e))?;
        pos += n as u64;
        if !progress(pos - copy_start, payload) {
            return Err(Error::Cancelled);
        }
    }
    let mut left = silence;
    buf.iter_mut().for_each(|b| *b = 0);
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        f.write_all(&buf[..n])
            .map_err(|e| Error::io(ctx("write"), e))?;
        left -= n as u64;
    }
    f.sync_all().map_err(|e| Error::io(ctx("flush"), e))?;
    Ok(header.len() as u64 + payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_matrices_round_trip() {
        for d in [0, 90, 180, 270] {
            let r = Rotation::from_degrees(d).unwrap();
            assert_eq!(Rotation::from_matrix(&r.matrix(4096, 2160)), Some(r));
            assert_eq!(r.degrees(), d as u32);
        }
        assert_eq!(Rotation::from_degrees(-90), Some(Rotation::R270));
        assert_eq!(Rotation::from_degrees(45), None);
        // Same bytes ffmpeg writes for a 90 degree clockwise display rotation of 4096x2160.
        let m = Rotation::R90.matrix(4096, 2160);
        assert_eq!(&m[0..8], &[0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(&m[12..16], &(-0x10000i32).to_be_bytes());
        assert_eq!(&m[24..28], &(2160i32 << 16).to_be_bytes());
        assert_eq!(&m[32..36], &0x4000_0000u32.to_be_bytes());
        // A zoomed matrix is not a rotation.
        let mut z = Rotation::R0.matrix(1, 1);
        z[0..4].copy_from_slice(&0x20000i32.to_be_bytes());
        assert_eq!(Rotation::from_matrix(&z), None);
        let json = serde_json::to_string(&Rotation::R180).unwrap();
        assert_eq!(json, "180");
        assert!(serde_json::from_str::<Rotation>("45").is_err());
    }
}
