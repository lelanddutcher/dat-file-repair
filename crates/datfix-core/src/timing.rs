//! Keyframe and display-order tables for the video track.
//!
//! Frames are stored in *decode order*. With B-frames, a frame may be decoded before an
//! earlier-displayed one. MP4 handles this with `ctts`: each sample gets a composition offset,
//! so `display time = decode time + offset`. Version 0 `ctts` offsets cannot be negative, so all
//! offsets are shifted up and the edit list (`elst`) starts playback that much later. Canon's own
//! files do the same (a one-frame shift at 119.88p).

use crate::codec::FrameInfo;

/// Video tables computed from per-frame facts.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoTiming {
    /// 1-based numbers of keyframes.
    pub sync_samples: Vec<u32>,
    /// True when every frame is a keyframe (intra-only video). `stss` is then omitted.
    pub all_sync: bool,
    /// Composition offset of each frame, in frames, after the shift. All zero for intra video.
    pub composition: Vec<u32>,
    /// True when display order differs from decode order anywhere.
    pub reordered: bool,
    /// How many frames every offset was shifted up by (the edit list skips this much).
    pub shift_frames: u32,
}

/// Computes keyframes and composition offsets for frames listed in decode order.
pub fn video_timing(frames: &[FrameInfo]) -> VideoTiming {
    let sync_samples: Vec<u32> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.sync)
        .map(|(i, _)| i as u32 + 1)
        .collect();

    // Display rank of each frame: sort decode indices by (sequence, poc), ties by decode index.
    let mut by_display: Vec<usize> = (0..frames.len()).collect();
    by_display.sort_by_key(|&i| (frames[i].order, i));
    let mut rank = vec![0i64; frames.len()];
    for (r, &i) in by_display.iter().enumerate() {
        rank[i] = r as i64;
    }

    // offset = display slot - decode slot. The most negative one decides the shift.
    let shift = (0..frames.len())
        .map(|i| i as i64 - rank[i])
        .max()
        .unwrap_or(0)
        .max(0);
    let composition: Vec<u32> = (0..frames.len())
        .map(|i| (rank[i] - i as i64 + shift) as u32)
        .collect();
    let reordered = (0..frames.len()).any(|i| rank[i] != i as i64);

    VideoTiming {
        all_sync: sync_samples.len() == frames.len(),
        sync_samples,
        composition,
        reordered,
        shift_frames: shift as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sync: bool, poc: i64) -> FrameInfo {
        FrameInfo {
            sync,
            order: (0, poc),
        }
    }

    #[test]
    fn intra_only() {
        let t = video_timing(&[f(true, 0), f(true, 1), f(true, 2)]);
        assert!(t.all_sync);
        assert!(!t.reordered);
        assert_eq!(t.shift_frames, 0);
        assert_eq!(t.composition, vec![0, 0, 0]);
    }

    #[test]
    fn b_frames_get_offsets() {
        // Decode order I0 P4 B2 B1 B3 (POC), display order 0 1 2 3 4.
        let t = video_timing(&[
            f(true, 0),
            f(false, 4),
            f(false, 2),
            f(false, 1),
            f(false, 3),
        ]);
        assert_eq!(t.sync_samples, vec![1]);
        assert!(t.reordered);
        // Ranks 0 4 2 1 3; rank - decode = 0 3 0 -2 -1; shift 2.
        assert_eq!(t.shift_frames, 2);
        assert_eq!(t.composition, vec![2, 5, 2, 0, 1]);
    }

    #[test]
    fn sequences_sort_after_each_other() {
        let frames = [
            FrameInfo {
                sync: true,
                order: (0, 0),
            },
            FrameInfo {
                sync: false,
                order: (0, 1),
            },
            FrameInfo {
                sync: true,
                order: (1, 0),
            },
        ];
        let t = video_timing(&frames);
        assert!(!t.reordered);
        assert_eq!(t.sync_samples, vec![1, 3]);
    }
}
