//! Engine round 3: reference metadata privacy, rotation, layout errors, AAC synthesis and
//! recording hints, end to end on synthetic files.

mod common;

use std::path::{Path, PathBuf};

use datfix_core::Error;
use datfix_core::mp4::atom::{self, Atom};
use datfix_core::mp4::reference::{Reference, TrackKind};
use datfix_core::mp4::writer::{self, Rotation};
use datfix_core::repair::{
    self, DateSource, RecordingHint, ReferenceSource, RepairOptions, RepairReport,
};
use datfix_core::select::CANON_UUID;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn run(
    dat: &Path,
    reference: &Path,
    out: &Path,
    opts: &RepairOptions,
) -> datfix_core::Result<RepairReport> {
    repair::repair(
        dat,
        &ReferenceSource::File(reference.to_path_buf()),
        out,
        opts,
        &mut |_| {},
    )
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn string_box(kind: &[u8; 4], s: &str) -> Atom {
    let mut p = vec![0, 0, 0, 0, 0x15, 0xc7];
    p.extend_from_slice(s.as_bytes());
    Atom::leaf(kind, p)
}

/// A reference like Canon's: a `uuid` box with a thumbnail and serial number, a `udta` with
/// maker, model and a location, and a per-track `udta` on the video.
fn identifying_reference(dir: &Path) -> PathBuf {
    let bytes = common::reference(2, &common::dat(2, 2)[16..]);
    let mut top = atom::parse_atoms(&bytes).unwrap();
    let moov = top.iter_mut().find(|a| a.kind.0 == *b"moov").unwrap();
    let mut uuid = CANON_UUID.to_vec();
    uuid.extend(Atom::leaf(b"CNTH", b"\xff\xd8JPEG SERIAL-0063129 2021:12:11".to_vec()).to_bytes());
    let mut udta = Vec::new();
    for a in [
        string_box(b"manu", "Canon"),
        string_box(b"modl", "Canon EOS R5"),
        Atom::leaf(b"loci", b"\0\0\0\0GPS-27.9N-82.4W".to_vec()),
    ] {
        a.write_to(&mut udta);
    }
    let kids = moov.children_mut();
    kids.insert(0, Atom::leaf(b"uuid", uuid));
    kids.insert(1, Atom::leaf(b"udta", udta));
    let video = kids.iter_mut().find(|a| a.kind.0 == *b"trak").unwrap();
    video
        .children_mut()
        .push(Atom::leaf(b"udta", b"\0\0\0\x10name OWNER-NAME".to_vec()));
    let mut out = Vec::new();
    top.iter().for_each(|a| a.write_to(&mut out));
    let path = dir.join("ref.MP4");
    write(&path, &out);
    path
}

#[test]
fn reference_metadata_stays_out_unless_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let dat = dir.path().join("clip.DAT");
    write(&dat, &common::dat(3, 2));
    let reference = identifying_reference(dir.path());

    let out = dir.path().join("private.MP4");
    let report = run(&dat, &reference, &out, &RepairOptions::default()).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let header = &bytes[..bytes.len().min(64 * 1024)];
    for secret in [
        &b"SERIAL-0063129"[..],
        b"GPS-27.9N",
        b"OWNER-NAME",
        b"2021:12:11",
        b"loci",
    ] {
        assert!(
            !contains(header, secret),
            "{}",
            String::from_utf8_lossy(secret)
        );
    }
    assert!(!contains(header, &CANON_UUID));
    // Maker and model stay: players and NLEs show them, and they say nothing about the owner.
    assert!(contains(header, b"Canon EOS R5"));
    assert!(
        report
            .withheld_metadata
            .iter()
            .any(|w| w.contains("Canon metadata box"))
    );
    assert!(
        report
            .withheld_metadata
            .iter()
            .any(|w| w.contains("location"))
    );
    let parsed = Reference::open(&out).unwrap();
    assert_eq!(parsed.tracks.len(), 4);

    let kept = dir.path().join("kept.MP4");
    let opts = RepairOptions {
        keep_reference_metadata: true,
        ..Default::default()
    };
    let report = run(&dat, &reference, &kept, &opts).unwrap();
    let bytes = std::fs::read(&kept).unwrap();
    for secret in [&b"SERIAL-0063129"[..], b"GPS-27.9N", b"OWNER-NAME"] {
        assert!(contains(&bytes[..64 * 1024], secret));
    }
    assert!(report.withheld_metadata.is_empty());
}

#[test]
fn rotation_sets_the_video_matrix_only() {
    let dir = tempfile::tempdir().unwrap();
    let dat = dir.path().join("clip.DAT");
    write(&dat, &common::dat(3, 2));
    let reference = dir.path().join("ref.MP4");
    write(&reference, &common::reference(2, &common::dat(2, 2)[16..]));

    let out = dir.path().join("upright.MP4");
    let report = run(&dat, &reference, &out, &RepairOptions::default()).unwrap();
    // The test reference's matrix is all zeros: left as it is, so no quarter turn.
    assert_eq!(report.rotation, None);

    for (deg, r) in [
        (90, Rotation::R90),
        (180, Rotation::R180),
        (270, Rotation::R270),
    ] {
        let out = dir.path().join(format!("rot{deg}.MP4"));
        let opts = RepairOptions {
            rotation: Some(r),
            ..Default::default()
        };
        let report = run(&dat, &reference, &out, &opts).unwrap();
        assert_eq!(report.rotation, Some(r));
        let parsed = Reference::open(&out).unwrap();
        for t in &parsed.tracks {
            let got = writer::trak_rotation(parsed.trak(t));
            let want = (t.kind == TrackKind::Video).then_some(r);
            assert_eq!(got, want, "track {}", t.track_id);
        }
    }
}

/// Offset of the size table of the first `vide` group.
fn first_video_sizes(dat: &[u8]) -> usize {
    (16..dat.len() - 12)
        .find(|&i| &dat[i..i + 4] == b"repi" && &dat[i + 8..i + 12] == b"vide")
        .unwrap()
        + 20
}

#[test]
fn layout_faults_are_not_blamed_on_the_reference() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = common::dat(3, 2);
    // Move three bytes of frame 4 into frame 3 in the index: both stay inside the group, but
    // frame 3 no longer ends at a NAL boundary.
    let at = first_video_sizes(&bytes);
    let size =
        |b: &[u8], k: usize| u32::from_le_bytes(b[at + 4 * k..at + 4 * k + 4].try_into().unwrap());
    let (s2, s3) = (size(&bytes, 2), size(&bytes, 3));
    bytes[at + 8..at + 12].copy_from_slice(&(s2 + 3).to_le_bytes());
    bytes[at + 12..at + 16].copy_from_slice(&(s3 - 3).to_le_bytes());
    let dat = dir.path().join("clip.DAT");
    write(&dat, &bytes);
    let reference = dir.path().join("ref.MP4");
    write(&reference, &common::reference(2, &common::dat(2, 2)[16..]));

    let err = run(
        &dat,
        &reference,
        &dir.path().join("out.MP4"),
        &RepairOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::LayoutNotSupported { .. }), "{err}");
    let m = err.user_message();
    assert_eq!(m.kind, "layout_not_supported");
    assert!(m.next_step.contains("issues/new?template="));
    assert!(m.message.contains("will not help"));
    assert!(!m.next_step.to_lowercase().contains("choose a clip"));

    // A reference that really does not fit still says so.
    let other = dir.path().join("other.MP4");
    write(
        &other,
        &common::reference_at(2, &common::dat(2, 2)[16..], 500),
    );
    let err = run(
        &dat,
        &other,
        &dir.path().join("out2.MP4"),
        &RepairOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Incompatible { .. }), "{err}");
}

/// A family B′ `.DAT` (timecode, AAC, video) with `aac_first` as the first byte of every
/// AAC frame: 23 frames of 1024 samples per 0.5005 s interval of 6 frames.
fn aac_dat(intervals: usize, aac_first: u8) -> Vec<u8> {
    let mut d = vec![
        0, 0, 0, 0, b'm', b'd', b'a', b't', 0, 0, 0, 8, b'w', b'i', b'd', b'e',
    ];
    for i in 0..intervals {
        let tc: Vec<Vec<u8>> = (0..6)
            .map(|f| ((i * 6 + f) as u32).to_be_bytes().to_vec())
            .collect();
        d.extend(common::group(b"tmcd", 4, &tc));
        let aac: Vec<Vec<u8>> = (0..23)
            .map(|k| vec![aac_first, k as u8, 0x5a, 0xa5])
            .collect();
        d.extend(common::group(b"aac ", 0, &aac));
        let frames: Vec<Vec<u8>> = common::GOP
            .iter()
            .enumerate()
            .map(|(k, &(t, lsb))| {
                let t = if i > 0 && t == 19 { 1 } else { t };
                common::frame(t, (lsb + 6 * i as u32) % 256, 100 + 7 * k)
            })
            .collect();
        d.extend(common::group(b"vide", 0, &frames));
    }
    d
}

#[test]
fn hybrid_builds_aac_settings_from_the_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let dat = dir.path().join("clip.DAT");
    // 0x21: a channel pair element (stereo).
    write(&dat, &aac_dat(3, 0x21));
    let reference = dir.path().join("ref.MP4");
    // Without PCM the rate comes from the group size: 6 frames per 0.5005 s = 11.988 fps.
    write(
        &reference,
        &common::reference_at(0, &common::dat(2, 0)[16..], 2085),
    );
    let out = dir.path().join("out.MP4");
    let report = run(&dat, &reference, &out, &RepairOptions::default()).unwrap();
    assert!(
        report
            .synthesized
            .iter()
            .any(|s| s.contains("AAC-LC 48 kHz stereo")),
        "{:?}",
        report.synthesized
    );
    let parsed = Reference::open(&out).unwrap();
    let aac = parsed
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::CompressedAudio)
        .unwrap();
    assert_eq!(aac.codec.0, *b"mp4a");
    assert_eq!(aac.audio.as_ref().unwrap().channels, 2);
    assert_eq!((aac.timescale, aac.sample_delta), (48_000, 1024));

    // A first byte that is no channel element: the clear error stays.
    let bad = dir.path().join("bad.DAT");
    write(&bad, &aac_dat(3, 0xe0));
    let err = run(
        &bad,
        &reference,
        &dir.path().join("bad.MP4"),
        &RepairOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("AAC"), "{err}");
}

#[test]
fn hints_name_and_date_carved_recordings() {
    let dir = tempfile::tempdir().unwrap();
    let dat = dir.path().join("recovered_123456.DAT");
    write(&dat, &common::dat(3, 2));
    let reference = dir.path().join("ref.MP4");
    write(&reference, &common::reference(2, &common::dat(2, 2)[16..]));

    // A Canon name from the card wins; the copy's own file time (scan time) is ignored.
    let opts = RepairOptions {
        hint: Some(RecordingHint {
            name: Some("A_0002D054H241221_134042C0_LAD04.DAT".into()),
            utc_offset: Some(-5 * 3600),
            ignore_file_time: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(&dat, &reference, &dir.path().join("a.MP4"), &opts).unwrap();
    assert_eq!(r.date_source, DateSource::FileName);
    assert_eq!(r.creation_time, Some(1_734_788_442 + 5 * 3600));

    // No dated name: the directory entry's creation time.
    let opts = RepairOptions {
        hint: Some(RecordingHint {
            name: Some("LAD12345.DAT".into()),
            created_local: Some(1_650_000_000),
            ignore_file_time: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(&dat, &reference, &dir.path().join("b.MP4"), &opts).unwrap();
    assert_eq!(r.date_source, DateSource::CardEntry);
    assert_eq!(r.creation_time, Some(1_650_000_000));

    // Nothing but an ignored file time: unknown, never the scan time.
    let opts = RepairOptions {
        hint: Some(RecordingHint {
            ignore_file_time: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(&dat, &reference, &dir.path().join("c.MP4"), &opts).unwrap();
    assert_eq!(r.date_source, DateSource::Unknown);
    assert_eq!(r.creation_time, None);
}
