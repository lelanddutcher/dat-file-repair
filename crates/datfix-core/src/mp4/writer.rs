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

/// Builds the new `moov`.
///
/// `offset_delta` is added to every `.DAT` offset to get the output file offset.
/// `time` (seconds since 1904) becomes the creation and modification time.
/// Returns the tree and the names of reference boxes that were dropped.
pub fn build_moov(
    reference: &Reference,
    tracks: &[TrackOutput],
    offset_delta: i64,
    time: u64,
) -> Result<(Atom, Vec<String>), String> {
    let movie_ts = reference.movie_timescale;
    let mut dropped = Vec::new();
    let mut children = Vec::new();
    let mut movie_duration = 0u64;

    for (i, child) in reference.moov.children().iter().enumerate() {
        match &child.kind.0 {
            b"trak" => {
                let Some(t) = tracks.iter().find(|t| t.ref_track.moov_index == i) else {
                    continue;
                };
                let (trak, track_duration) =
                    build_trak(child, t, movie_ts, offset_delta, time, &mut dropped)?;
                movie_duration = movie_duration.max(track_duration);
                children.push(trak);
            }
            // Padding the camera reserved for its own header; not needed.
            b"free" | b"skip" => {}
            _ => children.push(child.clone()),
        }
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

    dropped.sort();
    dropped.dedup();
    Ok((Atom::container(b"moov", children), dropped))
}

/// Rebuilds one `trak`. Returns it and its duration in the movie timescale.
fn build_trak(
    src: &Atom,
    t: &TrackOutput,
    movie_ts: u32,
    offset_delta: i64,
    time: u64,
    dropped: &mut Vec<String>,
) -> Result<(Atom, u64), String> {
    let mut trak = src.clone();
    let media_duration = t.media_duration();
    let track_duration = rescale(media_duration, t.ref_track.timescale, movie_ts);

    // tkhd: duration in the movie timescale.
    let tkhd = trak.find_mut(&[b"tkhd"]).ok_or("no tkhd")?;
    let mut h = TimedHeader::parse(tkhd.payload().unwrap_or_default(), 8)?;
    h.duration = track_duration;
    h.creation_time = time;
    h.modification_time = time;
    tkhd.body = Body::Leaf(h.to_payload());

    // Edit list: one edit covering the whole track. Video with B-frames starts `shift` frames
    // in, so the first displayed frame plays at time zero.
    let media_time = match t.video {
        Some(v) if v.shift_frames > 0 => v.shift_frames as i64 * t.ref_track.sample_delta as i64,
        _ => t.ref_track.edit_media_time.unwrap_or(0),
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
            dropped.push(format!("{} (track {})", a.kind, t.ref_track.track_id));
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
