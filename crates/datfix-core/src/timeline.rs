//! Keeping audio and video in sync across damaged regions.
//!
//! A Canon `.DAT` is a sequence of *intervals*, one every 0.5005 s. Each interval holds a
//! group per stream: timecode, metadata, audio, then video. When a region of the file is
//! damaged (zeroed blocks, a bad sector), the walker skips to the next valid `repi` header and
//! whole intervals can be lost. Without care, everything after the damage would play early:
//! the video frames and the audio would both close the gap, and by different amounts.
//!
//! This module gives every group a position on the timeline, in video frames from the start:
//!
//! - Undamaged intervals follow each other: position = previous position + previous frames.
//! - After damage, the position comes from the interval's own clock, best first:
//!   1. the `tmcd` counter (a 32-bit frame count), compared with the last good one;
//!   2. the 7-bit frame counter in the per-frame records (family A, no timecode), with the
//!      number of whole 128-frame wraps estimated from the damaged byte count;
//!   3. the damaged byte count divided by the average bytes per interval.
//!
//! Lost video frames become a longer duration for the frame before the gap (the picture holds),
//! lost audio becomes inserted silence, so every later sample keeps its place.

use serde::Serialize;

use crate::layout::{self, DatLayout};
use crate::mapping::{Chunk, TrackPlan};
use crate::mp4::reference::{Reference, TrackKind};
use crate::read_at::ReadAt;
use crate::repi::DatIndex;

/// One damaged region and how much time it removed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimelineGap {
    /// Position of the first frame after the gap, in frames from the start.
    pub resumes_at_frame: i64,
    /// Video frames missing in the gap.
    pub lost_frames: i64,
    /// Bytes between the last good group and the next one.
    pub damaged_bytes: u64,
    /// How the gap was measured: `timecode`, `frame counter` or `byte estimate`.
    pub method: &'static str,
}

/// Position of every group on the timeline.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    /// Start of the group's interval, in frames from the start of the recording.
    pub interval_start: Vec<i64>,
    /// Index of the interval each group belongs to.
    pub interval_of: Vec<usize>,
    pub gaps: Vec<TimelineGap>,
}

impl Timeline {
    pub fn has_gaps(&self) -> bool {
        self.gaps.iter().any(|g| g.lost_frames > 0)
    }
}

struct Interval {
    groups: Vec<usize>,
    /// Damaged bytes just before this interval (0 when it follows cleanly).
    damage_before: u64,
    frames: i64,
    tmcd: Option<u32>,
    record: Option<u32>,
    bytes: u64,
}

/// Builds the timeline of a walked `.DAT`.
pub fn build<R: ReadAt + ?Sized>(src: &R, index: &DatIndex, layout: &DatLayout) -> Timeline {
    let groups = &index.groups;
    let f = layout.frames_per_group.max(1) as i64;
    // Bytes the camera writes after a video group without indexing them.
    let records_after = |gi: usize| -> u64 {
        let g = &groups[gi];
        if g.kind.0 == *b"vide" && layout.has_records {
            layout::RECORD_SIZE * g.declared as u64
        } else {
            0
        }
    };

    // Split into intervals.
    let mut intervals: Vec<Interval> = Vec::new();
    for gi in 0..groups.len() {
        let g = &groups[gi];
        let damage = if gi == 0 {
            0
        } else {
            let prev = &groups[gi - 1];
            let expected = prev.declared_end() + records_after(gi - 1);
            let rejected = index
                .rejected_headers
                .iter()
                .any(|&r| r > prev.header_offset && r < g.header_offset);
            if g.header_offset != expected || rejected {
                g.header_offset.saturating_sub(prev.declared_end())
            } else {
                0
            }
        };
        let starts =
            gi == 0 || damage > 0 || g.kind.0 == *b"tmcd" || groups[gi - 1].kind.0 == *b"vide";
        if starts {
            intervals.push(Interval {
                groups: Vec::new(),
                damage_before: damage,
                frames: 0,
                tmcd: None,
                record: None,
                bytes: 0,
            });
        }
        let iv = intervals.last_mut().expect("an interval was started");
        iv.groups.push(gi);
        iv.bytes += g.end_of(g.complete) - g.header_offset + records_after(gi);
        if g.kind.0 == *b"vide" {
            iv.frames += g.complete as i64;
            if iv.record.is_none() {
                iv.record = layout::record_counter(src, g);
            }
        }
        if g.kind.0 == *b"tmcd" && iv.tmcd.is_none() {
            iv.tmcd = layout::tmcd_counter(src, g);
        }
    }

    // Average bytes per frame over clean, complete intervals (for byte estimates).
    let (mut b, mut n) = (0u64, 0i64);
    for iv in &intervals {
        if iv.damage_before == 0 && iv.frames == f {
            b += iv.bytes;
            n += iv.frames;
        }
    }
    let bytes_per_frame = if n > 0 { b as f64 / n as f64 } else { 0.0 };

    let mut start = vec![0i64; intervals.len()];
    let mut gaps = Vec::new();
    let mut last_tmcd: Option<(i64, u32)> = None;
    for i in 0..intervals.len() {
        let iv = &intervals[i];
        if i > 0 {
            let prev = &intervals[i - 1];
            // An interval whose video was lost still lasted one interval.
            let prev_len = if prev.frames > 0 { prev.frames } else { f };
            let expected = start[i - 1] + prev_len;
            start[i] = expected;
            if iv.damage_before > 0 {
                let est = if bytes_per_frame > 0.0 {
                    iv.damage_before as f64 / bytes_per_frame
                } else {
                    0.0
                };
                // Upper bound for a believable gap: generous, to absorb bitrate swings.
                let bound = (est * 4.0) as i64 + 4 * f;
                let mut method = "byte estimate";
                let mut lost = ((est / f as f64).round() as i64) * f;
                if let (Some(c), Some((p0, c0))) = (iv.tmcd, last_tmcd) {
                    let cand = p0 + (c as i64 - c0 as i64);
                    if cand >= expected && cand - expected <= bound {
                        lost = cand - expected;
                        method = "timecode";
                    }
                } else if let (Some(c), Some(pc)) = (iv.record, prev.record) {
                    if prev.frames > 0 {
                        let next = (pc as i64 + prev.frames) % 128;
                        let base = (c as i64 - next).rem_euclid(128);
                        let k = ((est - base as f64) / 128.0).round().max(0.0) as i64;
                        lost = base + 128 * k;
                        method = "frame counter";
                    }
                }
                start[i] = expected + lost.max(0);
                gaps.push(TimelineGap {
                    resumes_at_frame: start[i],
                    lost_frames: lost.max(0) + if prev.frames > 0 { 0 } else { f },
                    damaged_bytes: iv.damage_before,
                    method,
                });
            }
        }
        if let Some(c) = iv.tmcd {
            last_tmcd = Some((start[i], c));
        }
    }

    let mut interval_start = vec![0i64; groups.len()];
    let mut interval_of = vec![0usize; groups.len()];
    for (i, iv) in intervals.iter().enumerate() {
        for &gi in &iv.groups {
            interval_start[gi] = start[i];
            interval_of[gi] = i;
        }
    }
    Timeline {
        interval_start,
        interval_of,
        gaps,
    }
}

/// Positions (in frames) of each sample of a per-frame track (video, timecode, metadata).
fn frame_positions(plan: &TrackPlan, timeline: &Timeline) -> Vec<i64> {
    let mut out = Vec::with_capacity(plan.sample_count as usize);
    let mut last_group = usize::MAX;
    let mut j = 0i64;
    for c in &plan.chunks {
        if c.group != last_group {
            j = 0;
            last_group = c.group;
        }
        for _ in 0..c.samples {
            out.push(timeline.interval_start[c.group] + j);
            j += 1;
        }
    }
    out
}

/// Turns positions into per-sample durations (in frames).
fn steps_from(pos: &[i64]) -> Vec<u32> {
    let mut steps: Vec<u32> = pos
        .windows(2)
        .map(|w| (w[1] - w[0]).max(1) as u32)
        .collect();
    if !pos.is_empty() {
        steps.push(1);
    }
    steps
}

/// Applies the timeline to the plans: longer frames before gaps for per-frame tracks, and
/// inserted silence for PCM tracks. Returns notes for the user.
pub fn apply(timeline: &Timeline, reference: &Reference, plans: &mut [TrackPlan]) -> Vec<String> {
    let mut notes = Vec::new();
    if !timeline.has_gaps() {
        return notes;
    }
    let Some(vi) = plans.iter().position(|p| p.repi_type.0 == *b"vide") else {
        return notes;
    };
    let vt = &reference.tracks[plans[vi].ref_track];
    let frame_seconds = vt.sample_seconds();
    for p in plans.iter_mut() {
        let rt = &reference.tracks[p.ref_track];
        let per_frame = matches!(&p.repi_type.0, b"vide" | b"tmcd" | b"tdmt");
        if per_frame {
            let pos = frame_positions(p, timeline);
            let steps = steps_from(&pos);
            if steps.iter().any(|&s| s != 1) {
                p.steps = steps;
            }
            continue;
        }
        if rt.kind != TrackKind::PcmAudio || p.fixed_size == 0 {
            if rt.kind == TrackKind::CompressedAudio {
                notes.push(format!(
                    "track {} ({}) is compressed audio; no silence could be inserted for the lost \
                     section, so it drifts by the length of the gap after the damage",
                    rt.track_id, rt.codec
                ));
            }
            continue;
        }
        // Audio: each group's chunk should start at its interval's time.
        let rate = rt.timescale as f64 / rt.sample_delta as f64;
        let tolerance = (frame_seconds * rate / 2.0).max(1.0) as i64;
        let mut chunks: Vec<Chunk> = Vec::with_capacity(p.chunks.len() + 4);
        let mut cursor = 0i64;
        let mut inserted = 0i64;
        let mut last_interval = usize::MAX;
        for c in &p.chunks {
            let iv = timeline.interval_of[c.group];
            if iv != last_interval {
                last_interval = iv;
                let target =
                    (timeline.interval_start[c.group] as f64 * frame_seconds * rate).round() as i64;
                if target - cursor > tolerance {
                    let n = (target - cursor) as u32;
                    chunks.push(Chunk {
                        offset: 0,
                        samples: n,
                        group: c.group,
                        silence: true,
                    });
                    cursor += n as i64;
                    inserted += n as i64;
                }
            }
            chunks.push(c.clone());
            cursor += c.samples as i64;
        }
        if inserted > 0 {
            p.sample_count += inserted as u64;
            p.chunks = chunks;
            notes.push(format!(
                "track {} ({}): inserted {:.3} s of silence where the damaged section was",
                rt.track_id,
                rt.codec,
                inserted as f64 / rate
            ));
        }
    }
    notes
}

/// Appends silence so a PCM track lasts `samples` samples. Returns the samples added.
pub fn pad_to(plan: &mut TrackPlan, samples: u64) -> u64 {
    if plan.fixed_size == 0 || samples <= plan.sample_count {
        return 0;
    }
    let add = samples - plan.sample_count;
    let group = plan.chunks.last().map_or(0, |c| c.group);
    plan.chunks.push(Chunk {
        offset: 0,
        samples: add as u32,
        group,
        silence: true,
    });
    plan.sample_count = samples;
    add
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::repi::tests::{dat_header, group_bytes};

    /// Family A stream: `intervals` video groups of 15 frames, each followed by 15 records
    /// that carry the R3 body code and a 7-bit frame counter.
    fn family_a(intervals: usize) -> (Vec<u8>, Vec<(usize, usize)>) {
        let mut d = dat_header();
        let mut spans = Vec::new();
        for i in 0..intervals {
            let start = d.len();
            d.extend(group_bytes(b"vide", 0, &[100; 15]));
            for f in 0..15 {
                let mut rec = vec![0u8; 128];
                rec[0] = 4;
                rec[1..4].copy_from_slice(&[0, 0, 0x45]);
                rec[0x54] = ((i * 15 + f + 7) % 128) as u8;
                d.extend(rec);
            }
            spans.push((start, d.len()));
        }
        (d, spans)
    }

    fn gaps_after_zeroing(intervals: usize, lost: std::ops::Range<usize>) -> Vec<TimelineGap> {
        let (mut d, spans) = family_a(intervals);
        d[spans[lost.start].0..spans[lost.end - 1].1].fill(0);
        let index = crate::repi::walk(&d, &mut |_| true).unwrap();
        let lay = crate::layout::analyze(&d, &index, "");
        assert!(lay.has_records);
        assert_eq!(lay.body, Some("Canon EOS R3"));
        build(&d, &index, &lay).gaps
    }

    #[test]
    fn frame_counter_measures_gaps_in_family_a() {
        let gaps = gaps_after_zeroing(6, 2..3);
        assert_eq!(gaps.len(), 1);
        assert_eq!((gaps[0].lost_frames, gaps[0].method), (15, "frame counter"));
        assert_eq!(gaps[0].resumes_at_frame, 45);
        // 150 lost frames: the counter wraps at 128; the byte count picks the right wrap.
        let gaps = gaps_after_zeroing(14, 2..12);
        assert_eq!(
            (gaps[0].lost_frames, gaps[0].method),
            (150, "frame counter")
        );
    }

    #[test]
    fn steps_stretch_the_frame_before_a_gap() {
        assert_eq!(steps_from(&[0, 1, 2, 7, 8]), vec![1, 1, 5, 1, 1]);
        assert!(steps_from(&[]).is_empty());
    }
}
