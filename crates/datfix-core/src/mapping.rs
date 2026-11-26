//! Matching `repi` sample types to the reference clip's tracks, and checking they fit.
//!
//! | `repi` type | Reference track |
//! |---|---|
//! | `vide` | the video track |
//! | `lpcm` | PCM sound tracks. The k-th `lpcm` group in a run feeds the k-th PCM track. |
//! | `aac ` | the compressed (`mp4a`) sound track |
//! | `tmcd` | the timecode track |
//! | `tdmt` | the Canon metadata track (`cndm`) |
//!
//! Other types are reported and skipped. Each `repi` group becomes one chunk of its track, so
//! the output's chunk layout matches what the camera would have written.

use std::collections::HashMap;

use serde::Serialize;

use crate::fourcc::FourCC;
use crate::mp4::reference::{RefTrack, Reference, TrackKind};
use crate::repi::{DatIndex, StreamShape};

/// A run of samples that sit back to back in the `.DAT` (one `repi` group).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Chunk {
    /// File offset in the `.DAT` of the first sample.
    pub offset: u64,
    pub samples: u32,
}

/// Everything needed to write one output track.
#[derive(Clone, Debug, Serialize)]
pub struct TrackPlan {
    /// Index into [`Reference::tracks`].
    pub ref_track: usize,
    pub repi_type: FourCC,
    pub chunks: Vec<Chunk>,
    /// Size of every sample, or 0 when `sizes` lists them.
    pub fixed_size: u32,
    #[serde(skip)]
    pub sizes: Vec<u32>,
    pub sample_count: u64,
}

impl TrackPlan {
    /// `(offset, size)` of every sample, in order.
    pub fn sample_locations(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        let mut index = 0usize;
        self.chunks.iter().flat_map(move |c| {
            let mut off = c.offset;
            (0..c.samples)
                .map(|_| {
                    let size = if self.fixed_size > 0 {
                        self.fixed_size
                    } else {
                        self.sizes[index]
                    };
                    index += 1;
                    let here = off;
                    off += size as u64;
                    (here, size)
                })
                .collect::<Vec<_>>()
        })
    }

    /// Keeps only the first `keep` samples.
    pub fn truncate(&mut self, keep: u64) {
        if keep >= self.sample_count {
            return;
        }
        let mut left = keep;
        let mut chunks = Vec::new();
        for c in &self.chunks {
            if left == 0 {
                break;
            }
            let n = (c.samples as u64).min(left) as u32;
            chunks.push(Chunk {
                offset: c.offset,
                samples: n,
            });
            left -= n as u64;
        }
        self.chunks = chunks;
        if self.fixed_size == 0 {
            self.sizes.truncate(keep as usize);
        }
        self.sample_count = keep;
    }

    /// File offset just after the last sample.
    pub fn end_offset(&self) -> u64 {
        let Some(last) = self.chunks.last() else {
            return 0;
        };
        let tail: u64 = if self.fixed_size > 0 {
            self.fixed_size as u64 * last.samples as u64
        } else {
            let n = self.sizes.len();
            self.sizes[n - last.samples as usize..]
                .iter()
                .map(|&s| s as u64)
                .sum()
        };
        last.offset + tail
    }
}

/// Result of mapping a `.DAT` onto a reference.
#[derive(Clone, Debug, Serialize)]
pub struct Mapping {
    pub plans: Vec<TrackPlan>,
    /// Non-fatal observations for the user.
    pub notes: Vec<String>,
}

impl Mapping {
    /// The video plan. Mapping always produces one.
    pub fn video(&self) -> &TrackPlan {
        self.plans
            .iter()
            .find(|p| p.repi_type.0 == *b"vide")
            .expect("mapping has video")
    }
}

/// Which kind of reference track a `repi` type feeds.
fn wanted_kind(t: FourCC) -> Option<TrackKind> {
    match &t.0 {
        b"vide" => Some(TrackKind::Video),
        b"lpcm" => Some(TrackKind::PcmAudio),
        b"aac " | b"mp4a" => Some(TrackKind::CompressedAudio),
        b"tmcd" => Some(TrackKind::Timecode),
        b"tdmt" => Some(TrackKind::Metadata),
        _ => None,
    }
}

/// Maps the `.DAT` streams onto the reference tracks and checks they fit.
///
/// Returns the reasons for refusal when the reference does not match.
pub fn map_tracks(index: &DatIndex, reference: &Reference) -> Result<Mapping, Vec<String>> {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    // Types in order of first appearance.
    let mut types: Vec<FourCC> = Vec::new();
    for g in &index.groups {
        if !types.contains(&g.kind) {
            types.push(g.kind);
        }
    }
    if !types.contains(&FourCC::new(b"vide")) {
        reasons.push("the .DAT has no video (vide) samples".to_string());
        return Err(reasons);
    }

    // For lpcm, the position of each group inside its run of consecutive lpcm groups.
    let mut run_pos = vec![0usize; index.groups.len()];
    let mut run_len_max = 0usize;
    let mut pos = 0usize;
    for (i, g) in index.groups.iter().enumerate() {
        if g.kind.0 == *b"lpcm" {
            run_pos[i] = pos;
            pos += 1;
            run_len_max = run_len_max.max(pos);
        } else {
            pos = 0;
        }
    }

    // Decide the target reference track(s) for each type.
    let tracks_of = |k: TrackKind| -> Vec<usize> {
        reference
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == k)
            .map(|(i, _)| i)
            .collect()
    };
    let mut targets: HashMap<FourCC, Vec<usize>> = HashMap::new();
    for &t in &types {
        let Some(kind) = wanted_kind(t) else {
            let n = index.groups.iter().filter(|g| g.kind == t).count();
            notes.push(format!("skipped {n} groups of unknown sample type '{t}'"));
            continue;
        };
        let mut candidates = tracks_of(kind);
        if kind == TrackKind::Metadata {
            // Prefer Canon's `cndm` metadata track when there are several.
            candidates.sort_by_key(|&i| reference.tracks[i].codec.0 != *b"cndm");
        }
        if candidates.is_empty() {
            let what = match kind {
                TrackKind::Video => "a video track",
                TrackKind::PcmAudio => "a PCM audio track",
                TrackKind::CompressedAudio => "an AAC audio track",
                TrackKind::Timecode => "a timecode track",
                _ => "a Canon metadata track",
            };
            if kind == TrackKind::Video
                || kind == TrackKind::PcmAudio
                || kind == TrackKind::CompressedAudio
            {
                reasons.push(format!(
                    "the .DAT has '{t}' samples but the reference has no {what}"
                ));
            } else {
                notes.push(format!(
                    "the reference has no {what}; '{t}' samples are left out"
                ));
            }
            continue;
        }
        let wanted = if t.0 == *b"lpcm" { run_len_max } else { 1 };
        if t.0 == *b"lpcm" && candidates.len() != wanted {
            reasons.push(format!(
                "the .DAT has {wanted} PCM audio stream(s) but the reference has {} PCM audio track(s)",
                candidates.len()
            ));
            continue;
        }
        candidates.truncate(wanted);
        targets.insert(t, candidates);
    }

    // Build one plan per reference track.
    let mut plans: Vec<TrackPlan> = Vec::new();
    let mut plan_of_track: HashMap<usize, usize> = HashMap::new();
    for (gi, g) in index.groups.iter().enumerate() {
        let Some(tgt) = targets.get(&g.kind) else {
            continue;
        };
        if g.complete == 0 {
            continue;
        }
        let ref_track = tgt[if g.kind.0 == *b"lpcm" { run_pos[gi] } else { 0 }];
        let pi = *plan_of_track.entry(ref_track).or_insert_with(|| {
            plans.push(TrackPlan {
                ref_track,
                repi_type: g.kind,
                chunks: Vec::new(),
                fixed_size: g.fixed_size,
                sizes: Vec::new(),
                sample_count: 0,
            });
            plans.len() - 1
        });
        let p = &mut plans[pi];
        if p.fixed_size != g.fixed_size {
            reasons.push(format!(
                "'{}' groups change sample size inside the .DAT ({} then {}); this is not supported",
                g.kind, p.fixed_size, g.fixed_size
            ));
            return Err(reasons);
        }
        if g.align <= 1 {
            // Packed samples: the whole group is one chunk.
            p.chunks.push(Chunk {
                offset: g.data_offset(),
                samples: g.complete,
            });
        } else {
            // Padded samples are not contiguous, so each one is its own chunk (as Canon does).
            p.chunks.extend(
                g.samples(g.complete)
                    .map(|(offset, _)| Chunk { offset, samples: 1 }),
            );
        }
        if g.fixed_size == 0 {
            p.sizes.extend_from_slice(&g.sizes[..g.complete as usize]);
        }
        p.sample_count += g.complete as u64;
    }

    // Some cameras store a whole PCM group as one big sample (for example 96096 bytes =
    // 24024 stereo 16-bit frames). MP4 stores one sample per audio frame, so split it.
    for p in &mut plans {
        let rt = &reference.tracks[p.ref_track];
        let frame = rt.fixed_sample_size;
        if rt.kind == TrackKind::PcmAudio
            && frame > 0
            && p.fixed_size > frame
            && p.fixed_size % frame == 0
        {
            let k = p.fixed_size / frame;
            p.chunks.iter_mut().for_each(|c| c.samples *= k);
            p.sample_count *= k as u64;
            p.fixed_size = frame;
        }
    }

    // Sample size must match the reference for fixed-size streams.
    for p in &plans {
        let rt = &reference.tracks[p.ref_track];
        if p.fixed_size > 0 && rt.fixed_sample_size > 0 && p.fixed_size != rt.fixed_sample_size {
            let hint = if rt.kind == TrackKind::PcmAudio {
                " (bit depth or channel count differ)"
            } else {
                ""
            };
            reasons.push(format!(
                "'{}' samples are {} bytes in the .DAT but {} bytes in reference track {} ({}){hint}",
                p.repi_type, p.fixed_size, rt.fixed_sample_size, rt.track_id, rt.codec
            ));
        }
        if rt.kind == TrackKind::Video && p.fixed_size > 0 {
            reasons.push("video samples have a fixed size, which Canon video never has".into());
        }
    }

    let shapes = index.shapes();
    check_group_durations(reference, &plans, &shapes, &mut reasons, &mut notes);
    compare_shapes(reference, &shapes, &mut reasons, &mut notes);

    for (i, t) in reference.tracks.iter().enumerate() {
        if !plans.iter().any(|p| p.ref_track == i) {
            notes.push(format!(
                "reference track {} ({} {}) has no data in the .DAT and is left out of the output",
                t.track_id, t.handler, t.codec
            ));
        }
    }

    if reasons.is_empty() {
        Ok(Mapping { plans, notes })
    } else {
        Err(reasons)
    }
}

/// Compares the `.DAT` stream layout with the `repi` layout inside the reference's `mdat`.
/// A different number of samples per group, or a stream only one side has, means the clips
/// were recorded in different modes.
fn compare_shapes(
    reference: &Reference,
    dat: &[StreamShape],
    reasons: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(theirs) = &reference.repi_shapes else {
        notes.push(
            "the reference has no Canon recovery index, so its recording mode could not be compared"
                .into(),
        );
        return;
    };
    for d in dat {
        let Some(r) = theirs.iter().find(|r| r.kind == d.kind) else {
            continue; // handled by the track mapping
        };
        if d.typical_samples > 0 && r.typical_samples > 0 && d.typical_samples != r.typical_samples
        {
            reasons.push(format!(
                "the reference stores {} '{}' samples per index group but the .DAT stores {}: \
                 the clips use a different frame rate or recording mode",
                r.typical_samples, r.kind, d.typical_samples
            ));
        }
    }
    let extra: Vec<String> = theirs
        .iter()
        .filter(|r| !dat.iter().any(|d| d.kind == r.kind))
        .map(|r| format!("'{}'", r.kind))
        .collect();
    if !extra.is_empty() {
        reasons.push(format!(
            "the reference also records {} stream(s) that the .DAT does not have: it was recorded \
             in a different mode (for example with audio or timecode on)",
            extra.join(", ")
        ));
    }
}

/// Checks that one typical group of every stream covers the same time span as one typical
/// video group. A mismatch means a different frame rate or audio rate than the reference.
fn check_group_durations(
    reference: &Reference,
    plans: &[TrackPlan],
    shapes: &[StreamShape],
    reasons: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    // Typical samples per group, in the reference's sample units (PCM groups may be split).
    let typical = |p: &TrackPlan| {
        let shape = shapes.iter().find(|s| s.kind == p.repi_type)?;
        let split = match (shape.fixed_size, p.fixed_size) {
            (a, b) if a > 0 && b > 0 => a / b,
            _ => 1,
        };
        Some(shape.typical_samples * split).filter(|&n| n > 0)
    };
    let Some(video_plan) = plans.iter().find(|p| p.repi_type.0 == *b"vide") else {
        return;
    };
    let vt: &RefTrack = &reference.tracks[video_plan.ref_track];
    let Some(vn) = typical(video_plan) else {
        return;
    };
    let video_span = vn as f64 * vt.sample_seconds();
    let frame = vt.sample_seconds();
    let fps = 1.0 / frame;

    let mut compared = false;
    for p in plans.iter().filter(|p| p.repi_type.0 != *b"vide") {
        let rt = &reference.tracks[p.ref_track];
        let Some(n) = typical(p) else {
            continue;
        };
        let span = n as f64 * rt.sample_seconds();
        compared = true;
        if (span - video_span).abs() > frame * 1.01 {
            let real_fps = vn as f64 / span;
            reasons.push(format!(
                "timing differs: in the .DAT, {vn} video frames span {span:.4} s of '{}' ({real_fps:.3} fps), \
                 but the reference video runs at {fps:.3} fps",
                p.repi_type
            ));
        }
    }
    // Without audio, only the reference's own repi layout (compare_shapes) or its chunking can
    // hint at the frame rate.
    if !compared
        && reference.repi_shapes.is_none()
        && vt.samples_per_chunk > 0
        && vt.samples_per_chunk != vn
    {
        notes.push(format!(
            "could not verify the frame rate: the .DAT has no indexed audio. Video groups hold {vn} \
             frames, the reference's chunks hold {}; check that both clips use the same frame rate",
            vt.samples_per_chunk
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cuts_chunks() {
        let mut p = TrackPlan {
            ref_track: 0,
            repi_type: FourCC::new(b"vide"),
            chunks: vec![
                Chunk {
                    offset: 100,
                    samples: 2,
                },
                Chunk {
                    offset: 500,
                    samples: 2,
                },
            ],
            fixed_size: 0,
            sizes: vec![10, 20, 30, 40],
            sample_count: 4,
        };
        assert_eq!(
            p.sample_locations().collect::<Vec<_>>(),
            vec![(100, 10), (110, 20), (500, 30), (530, 40)]
        );
        assert_eq!(p.end_offset(), 570);
        p.truncate(3);
        assert_eq!(p.chunks.len(), 2);
        assert_eq!(p.chunks[1].samples, 1);
        assert_eq!(p.sizes, vec![10, 20, 30]);
        assert_eq!(p.end_offset(), 530);
    }
}
