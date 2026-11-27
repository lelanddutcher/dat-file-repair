//! Checks the keyframe and composition-offset logic against a camera's own tables.
//!
//! Set `DATFIX_REF` to a healthy Long-GOP or intra clip. The test analyzes the clip's own video
//! samples with `datfix-core` and compares the result with the `stss` and `ctts` the camera
//! wrote. `DATFIX_REF_FRAMES` limits how many frames are read (default 3000).
//!
//! ```sh
//! DATFIX_REF=/path/to/clip.MP4 cargo test --release --test reference_timing -- --nocapture
//! ```

use std::path::PathBuf;

use datfix_core::codec::VideoCodec;
use datfix_core::mp4::reference::Reference;
use datfix_core::mp4::tables;
use datfix_core::read_at::FileSource;
use datfix_core::repair::compute_video_timing;

#[test]
fn timing_matches_camera_tables() {
    let Some(path) = std::env::var_os("DATFIX_REF").map(PathBuf::from) else {
        eprintln!("DATFIX_REF not set; skipping");
        return;
    };
    let limit: usize = std::env::var("DATFIX_REF_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    let reference = Reference::open(&path).unwrap();
    let vt = reference.video_track().unwrap();
    let vf = vt.video.as_ref().unwrap();
    let codec = VideoCodec::from_config(vt.codec, vf.config_kind, &vf.config).unwrap();
    let stbl = reference
        .trak(vt)
        .find(&[b"mdia", b"minf", b"stbl"])
        .unwrap();
    let camera_sync: Option<Vec<u32>> = stbl
        .child(b"stss")
        .map(|a| tables::parse_stss(a.payload().unwrap()).unwrap());
    let camera_ctts: Option<Vec<i64>> = stbl
        .child(b"ctts")
        .map(|a| tables::parse_ctts(a.payload().unwrap()).unwrap());

    // Stop just before a keyframe so the last GOP is complete in both tables.
    let mut locations = reference.sample_locations(vt).unwrap();
    let mut n = locations.len().min(limit);
    if let Some(sync) = &camera_sync {
        n = sync
            .iter()
            .map(|&s| s as usize - 1)
            .filter(|&s| s <= n)
            .max()
            .unwrap_or(n)
            .max(1);
    }
    locations.truncate(n);

    let src = FileSource::open(&path).unwrap();
    let timing = compute_video_timing(&src, &locations, codec).unwrap();
    eprintln!(
        "{}: {n} frames, {} keyframes, reordered={}, shift={} frames",
        path.display(),
        timing.sync_samples.len(),
        timing.reordered,
        timing.shift_frames
    );

    match &camera_sync {
        Some(sync) => {
            let expected: Vec<u32> = sync
                .iter()
                .copied()
                .filter(|&s| (s as usize) <= n)
                .collect();
            assert_eq!(
                timing.sync_samples, expected,
                "keyframes differ from the camera's stss"
            );
        }
        None => assert!(timing.all_sync, "camera says all frames are keyframes"),
    }
    match &camera_ctts {
        Some(ctts) => {
            let ours: Vec<i64> = timing
                .composition
                .iter()
                .map(|&c| c as i64 * vt.sample_delta as i64)
                .collect();
            let mismatches = ours.iter().zip(ctts).filter(|(a, b)| a != b).count();
            assert_eq!(mismatches, 0, "ctts differs in {mismatches} of {n} frames");
            assert_eq!(
                Some(timing.shift_frames as i64 * vt.sample_delta as i64),
                vt.edit_media_time,
                "edit list shift differs"
            );
        }
        None => assert!(
            !timing.reordered,
            "camera wrote no ctts but frames are reordered"
        ),
    }
}
