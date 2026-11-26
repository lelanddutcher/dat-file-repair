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
fn hybrid_replaces_audio_tracks_that_do_not_fit() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 4));
    // The clip has 2 PCM tracks; the .DAT has 4 PCM streams. The video fits.
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 2)[16..]));
    let out = dir.path().join("out.MP4");
    let report = repair(&dat_path, &ref_path, &out).unwrap();
    assert_eq!(report.reference_kind, repair::RefKind::Hybrid);
    assert!(
        report.synthesized.iter().any(|s| s.contains("4 x ipcm")),
        "{:?}",
        report.synthesized
    );
    let parsed = Reference::open(&out).unwrap();
    let pcm = parsed
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::PcmAudio)
        .count();
    assert_eq!(pcm, 4);
    assert!(
        report
            .tracks
            .iter()
            .all(|t| (t.duration_seconds - 0.48).abs() < 1e-9)
    );
}

#[test]
fn refuses_reference_with_other_frame_rate() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 2));
    // Same streams, but the clip's video runs at 50 fps (the .DAT's audio says 25 fps).
    let ref_path = dir.path().join("ref.MP4");
    write(
        &ref_path,
        &common::reference_at(2, &common::dat(2, 2)[16..], 500),
    );
    let out = dir.path().join("out.MP4");
    match repair(&dat_path, &ref_path, &out) {
        Err(Error::Incompatible { reasons, .. }) => {
            assert!(
                reasons.iter().any(|r| r.contains("frame rate differs")),
                "{reasons:?}"
            );
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert!(!out.exists());
}

#[test]
fn chooses_compatible_reference_from_folder_and_protects_output() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(2, 2));
    let refs = dir.path().join("refs");
    std::fs::create_dir(&refs).unwrap();
    // The smaller file only fits as a hybrid (4 audio tracks); the bigger one fits exactly
    // and wins. A third clip runs at another frame rate and is rejected.
    write(&refs.join("a_small.MP4"), &common::reference(4, &[]));
    // The exact match sits in a subfolder: the search is recursive.
    let sub = refs.join("day2");
    std::fs::create_dir(&sub).unwrap();
    write(
        &sub.join("b_big.mp4"),
        &common::reference(2, &common::dat(4, 2)[16..]),
    );
    write(
        &refs.join("c_fast.MP4"),
        &common::reference_at(2, &common::dat(2, 2)[16..], 500),
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
    assert_eq!(report.reference_kind, repair::RefKind::User);

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
        &ReferenceSource::File(dir.path().join("refs/day2/b_big.mp4")),
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

#[test]
fn explains_inputs_that_are_not_repairable() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("o.MP4");
    let run = |name: &str, bytes: &[u8]| -> Error {
        let p = dir.path().join(name);
        write(&p, bytes);
        repair::repair(
            &p,
            &ReferenceSource::Auto,
            &out,
            &RepairOptions::default(),
            &mut |_| {},
        )
        .unwrap_err()
    };

    // 0 bytes: the data never made it into the file.
    let e = run("empty.DAT", &[]);
    assert!(matches!(e, Error::EmptyDat { .. }), "{e}");
    let msg = e.to_string();
    assert!(
        msg.contains("the data isn't in this file") && msg.contains("PhotoRec"),
        "{msg}"
    );
    // Only zeros: same advice.
    let e = run("zeros.DAT", &vec![0u8; 4096]);
    assert!(matches!(e, Error::EmptyDat { .. }), "{e}");

    // Video CD .dat (RIFF/CDXA) and MPEG program streams are not Canon files.
    let mut vcd = b"RIFF\x00\x10\x00\x00CDXAfmt ".to_vec();
    vcd.resize(4096, 1);
    let e = run("AVSEQ01.DAT", &vcd);
    assert!(
        e.to_string().starts_with("this isn't a Canon camera file"),
        "{e}"
    );
    let mut ps = vec![0, 0, 1, 0xba];
    ps.resize(4096, 0x44);
    let e = run("MUSIC01.DAT", &ps);
    assert!(matches!(e, Error::NotCanon(_)), "{e}");

    // A Canon .DAT header without any repi index (EOS R, M50 Mark II).
    let mut eos_r = vec![
        0, 0, 0, 0, b'm', b'd', b'a', b't', 0, 0, 0, 8, b'w', b'i', b'd', b'e',
    ];
    eos_r.extend(std::iter::repeat_n(0x5a, 64 * 1024));
    let e = run("MVI_0001.DAT", &eos_r);
    assert!(matches!(e, Error::Unsupported(_)), "{e}");
    assert!(e.to_string().contains("EOS R"), "{e}");
    assert!(!out.exists());
}

#[test]
fn creation_time_comes_from_the_canon_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let dat_path = dir.path().join("A_0002D054H241221_134042C0_LAD04.DAT");
    write(&dat_path, &common::dat(2, 2));
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 2)[16..]));
    let out = dir.path().join("out.MP4");
    let report = repair(&dat_path, &ref_path, &out).unwrap();
    // 2024-12-21 13:40:42 local. The file's mtime is "now", so no time zone can be inferred
    // and the local time is written as is.
    assert_eq!(report.creation_time, Some(1_734_788_442));
    assert!(report.creation_time_source.contains("file name"));
    let parsed = Reference::open(&out).unwrap();
    let mvhd = parsed.moov.child(b"mvhd").unwrap().payload().unwrap();
    let h = tables::TimedHeader::parse(mvhd, 4).unwrap();
    assert_eq!(h.creation_time, 1_734_788_442 + tables::MP4_EPOCH_OFFSET);
}

#[test]
fn picks_a_harvested_profile_when_no_reference_is_given() {
    use datfix_core::read_at::FileSource;
    use datfix_core::{layout, profile, repi, select};

    let dir = tempfile::tempdir().unwrap();
    let ref_path = dir.path().join("ref.MP4");
    write(&ref_path, &common::reference(2, &common::dat(2, 2)[16..]));
    let p = profile::harvest(&ref_path).unwrap();
    assert_eq!(p.width, 64);
    // The profile holds settings only: no sample data from the clip's mdat.
    assert!(p.moov.len() / 2 < 4096);

    let dat_path = dir.path().join("clip.DAT");
    write(&dat_path, &common::dat(3, 2));
    // No built-in profile runs at 25 fps: a clear refusal, not a guess.
    let e = repair::repair(
        &dat_path,
        &ReferenceSource::Auto,
        &dir.path().join("o.MP4"),
        &RepairOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, Error::NoProfile { .. }), "{e}");

    // With the harvested profile in the bank, it is chosen.
    let src = FileSource::open(&dat_path).unwrap();
    let index = repi::walk(&src, &mut |_| true).unwrap();
    let lay = layout::analyze(&src, &index, "clip.DAT");
    let bank = vec![p.clone()];
    let c = select::choose_profile_from(&src, &index, &lay, &bank, &mut |_| {}).unwrap();
    assert_eq!(c.kind, repair::RefKind::Profile);
    assert!(c.label.contains(&p.id));

    // A profile at another frame rate is not even tried.
    let mut other = p;
    other.timescale = 50_000;
    let Err(e) = select::choose_profile_from(&src, &index, &lay, &[other], &mut |_| {}) else {
        panic!("a profile at another frame rate was accepted");
    };
    assert!(e.to_string().contains("other frame rate"), "{e}");
}
