//! End-to-end repairs of synthetic `.DAT` files against synthetic references.

mod common;

use std::path::Path;

use datfix_core::Error;
use datfix_core::mp4::reference::{Reference, TrackKind};
use datfix_core::mp4::tables;
use datfix_core::repair::{self, Event, ReferenceSource, RepairOptions};

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn repair(dat: &Path, reference: &Path, out: &Path) -> datfix_core::Result<repair::RepairReport> {
    let mut events = Vec::new();
    let r = repair::repair(
        dat,
        &ReferenceSource::File(reference.to_path_buf()),
        out,
        &RepairOptions::default(),
        &mut |e: &Event| events.push(serde_json::to_string(e).unwrap()),
    );
    // Every event must serialize to one JSON object with an "event" tag.
    assert!(events.iter().all(|e| e.starts_with("{\"event\":")));
    r
}

#[test]
fn repairs_truncated_long_gop_dat() {
    let dir = tempfile::tempdir().unwrap();
    let mut dat = common::dat(3, 2);
    let full_len = dat.len();
    // Cut the last video group after 4 complete frames plus a few bytes of the fifth.
    let last_vide = full_len - (100 + 107 + 114 + 121 + 128 + 135);
    dat.truncate(last_vide + 100 + 107 + 114 + 121 + 10);
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &dat);
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 2)[16..]));
    let out = dir.path().join("out.MP4");

    let report = repair(&dat_path, &ref_path, &out).unwrap();
    assert_eq!(report.video_frames, 16);
    assert!(report.reordered);
    assert_eq!(report.keyframes, 1);
    assert_eq!(report.truncation.as_ref().unwrap().dropped_samples, 2);
    // 16 frames at 25 fps = 0.64 s = 30720 audio frames per channel.
    let audio: Vec<_> = report
        .tracks
        .iter()
        .filter(|t| t.handler.0 == *b"soun")
        .collect();
    assert_eq!(audio.len(), 2);
    assert!(
        audio
            .iter()
            .all(|t| t.samples == 30720 && t.trimmed_samples == 34560 - 30720)
    );

    // The output parses as an MP4 and its tables point at the original sample bytes.
    let parsed = Reference::open(&out).unwrap();
    assert_eq!(parsed.tracks.len(), 4);
    let video = parsed.video_track().unwrap();
    assert_eq!(video.sample_count, 16);
    let out_bytes = std::fs::read(&out).unwrap();
    let locs = parsed.sample_locations(video).unwrap();
    for (i, (off, size)) in locs.iter().enumerate() {
        let expected = common::frame(0, 0, 100 + 7 * (i % 6)).len();
        assert_eq!(*size as usize, expected, "frame {i} size");
        // First 4 bytes are the NAL length prefix.
        let prefix = u32::from_be_bytes(
            out_bytes[*off as usize..*off as usize + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(prefix as usize, expected - 4);
    }
    // Composition offsets: decode POC order 0 3 1 2 5 4 needs a one-frame shift.
    let stbl = parsed
        .trak(video)
        .find(&[b"mdia", b"minf", b"stbl"])
        .unwrap();
    let ctts = tables::parse_ctts(stbl.child(b"ctts").unwrap().payload().unwrap()).unwrap();
    assert_eq!(&ctts[..6], &[1000, 3000, 0, 0, 2000, 0]);
    assert_eq!(video.edit_media_time, Some(1000));
    // The tmcd track carries the timecode samples from the .DAT.
    let tmcd = parsed
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Timecode)
        .unwrap();
    let tc = parsed.sample_locations(tmcd).unwrap();
    assert_eq!(tc.len(), 16);
    assert_eq!(
        &out_bytes[tc[7].0 as usize..tc[7].0 as usize + 4],
        &7u32.to_be_bytes()
    );
}

#[test]
fn refuses_reference_with_wrong_audio_track_count() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 4));
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 2)[16..]));
    let out = dir.path().join("out.MP4");
    match repair(&dat_path, &ref_path, &out) {
        Err(Error::Incompatible { reasons, .. }) => {
            assert!(
                reasons.iter().any(|r| r.contains("4 PCM audio stream")),
                "{reasons:?}"
            );
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert!(!out.exists());
}

#[test]
fn refuses_reference_from_another_mode() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 2));
    // Reference mdat has one extra lpcm stream: a different recording mode.
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 3)[16..]));
    let out = dir.path().join("out.MP4");
    let err = repair(&dat_path, &ref_path, &out).unwrap_err();
    assert!(matches!(err, Error::Incompatible { .. }), "{err}");
}

#[test]
fn chooses_compatible_reference_from_folder_and_protects_output() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 2));
    let refs = dir.path().join("refs");
    std::fs::create_dir(&refs).unwrap();
    // The smaller file is incompatible (4 audio tracks); the bigger one fits.
    write(&refs.join("a_small.MP4"), &common::reference(4, &[]));
    write(
        &refs.join("b_big.mp4"),
        &common::reference(2, &common::dat(4, 2)[16..]),
    );
    write(&refs.join("notes.txt"), b"not a clip");
    let out = dir.path().join("out.MP4");

    let mut rejected = 0;
    let report = repair::repair(
        &dat_path,
        &ReferenceSource::Dir(refs.clone()),
        &out,
        &RepairOptions::default(),
        &mut |e: &Event| {
            if matches!(e, Event::ReferenceRejected { .. }) {
                rejected += 1;
            }
        },
    )
    .unwrap();
    assert_eq!(rejected, 1);
    assert!(report.reference.ends_with("b_big.mp4"));

    // A second run must not overwrite without permission.
    let again = repair::repair(
        &dat_path,
        &ReferenceSource::Dir(refs.clone()),
        &out,
        &RepairOptions::default(),
        &mut |_| {},
    );
    assert!(matches!(again, Err(Error::OutputExists(_))));
    let forced = RepairOptions {
        overwrite: true,
        ..Default::default()
    };
    repair::repair(
        &dat_path,
        &ReferenceSource::Dir(refs),
        &out,
        &forced,
        &mut |_| {},
    )
    .unwrap();
    // Writing over the input is always refused.
    let onto_input = repair::repair(
        &dat_path,
        &ReferenceSource::File(dir.path().join("refs/b_big.mp4")),
        &dat_path,
        &forced,
        &mut |_| {},
    );
    assert!(matches!(onto_input, Err(Error::OutputIsInput)));
    // No temporary files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("partial"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn rejects_file_without_index() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("random.DAT");
    write(&dat_path, &vec![7u8; 100_000]);
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &[]));
    let err = repair(&dat_path, &ref_path, &dir.path().join("o.MP4")).unwrap_err();
    assert!(matches!(err, Error::NotADat(_)));
}
