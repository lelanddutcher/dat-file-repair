//! Synthetic card images: recordings, clips, fragments and file-system hints.

use super::*;
use crate::repi::tests::group_bytes;

/// Deterministic noise that never contains a valid group header.
fn junk(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// A video sample of one NAL unit with a 4-byte length prefix.
fn nal_sample(size: u32, fill: u8) -> Vec<u8> {
    let mut v = (size - 4).to_be_bytes().to_vec();
    v.extend(std::iter::repeat_n(fill, size as usize - 4));
    v
}

/// A Canon-like stream: `tmcd ×F`, `lpcm ×1`, `vide ×F` (padded to 8) + F records, repeated.
/// With `timecode` false the stream is video only (family A). Stops once `min_len` is reached.
fn canon_stream(min_len: usize, tc0: u32, timecode: bool, frame: u32) -> Vec<u8> {
    const F: u32 = 4;
    let mut d = DAT_HEADER.to_vec();
    let mut tc = tc0;
    let mut k = 0u32;
    while d.len() < min_len {
        if timecode {
            let mut g = group_bytes(b"tmcd", 4, &[4; F as usize]);
            g.truncate(20);
            for i in 0..F {
                g.extend((tc + i).to_be_bytes());
            }
            d.extend(g);
            d.extend(group_bytes(b"lpcm", 4, &[4; 300]));
        }
        tc += F;
        let sizes: Vec<u32> = (0..F).map(|i| frame + 3 + 5 * ((k + i) % 7)).collect();
        let mut g = group_bytes(b"vide", 0, &sizes);
        g.truncate(20 + 4 * F as usize);
        for (i, &s) in sizes.iter().enumerate() {
            g.extend(nal_sample(s, 0x40 + i as u8));
            while (g.len() - (20 + 4 * F as usize)) % 8 != 0 {
                g.push(0xee);
            }
        }
        for i in 0..F {
            let mut r = vec![0u8; 128];
            r[0] = 0x04;
            r[1..4].copy_from_slice(&[0x00, 0x00, 0x45]);
            r[0x54] = (k + i) as u8 & 0x7f;
            g.extend(r);
        }
        d.extend(g);
        k += F;
    }
    d
}

fn run(img: &[u8]) -> ScanReport {
    scan(img, &ScanOptions::default(), &mut |_| {}).unwrap()
}

fn bytes_of(img: &[u8], r: &Recovered) -> Vec<u8> {
    let mut v = if r.has_dat_header {
        Vec::new()
    } else {
        DAT_HEADER.to_vec()
    };
    for p in &r.pieces {
        v.extend_from_slice(&img[p.offset as usize..(p.offset + p.length) as usize]);
    }
    v
}

#[test]
fn finds_interrupted_recording_cut_at_a_32mib_unit() {
    // The camera stopped mid-group: the .DAT is exactly one 32 MiB unit.
    let mut dat = canon_stream(DAT_UNIT as usize + 200_000, 3_000_000, true, 20_000);
    dat.truncate(DAT_UNIT as usize);
    let at = 3 << 20;
    let mut img = junk(at, 1);
    img.extend(&dat);
    img.extend(junk(2 << 20, 2));
    let r = run(&img);
    assert_eq!(r.recordings.len(), 1, "{:?}", r.notes);
    let rec = &r.recordings[0];
    assert_eq!(rec.offset, at as u64);
    assert_eq!(rec.length, DAT_UNIT);
    assert_eq!(rec.end, StreamEnd::DatUnit);
    assert!(!rec.stitched && rec.partial.is_none());
    assert_eq!(rec.start_timecode, Some(3_000_000));
    assert_eq!(bytes_of(&img, rec), dat);
    assert!(rec.video_frames > 1000);
    assert!(r.fragments.is_empty());
}

#[test]
fn stitches_a_fragmented_recording_with_timecode_proof() {
    let mut dat = canon_stream(DAT_UNIT as usize + 200_000, 100, true, 20_000);
    dat.truncate(DAT_UNIT as usize);
    let half = 16 << 20; // a 512 KiB cluster boundary counted from the .DAT start
    let at = 1 << 20;
    let mut img = junk(at, 3);
    img.extend(&dat[..half]);
    img.extend(junk(3 << 20, 4)); // another file's clusters
    let second = img.len();
    img.extend(&dat[half..]);
    img.extend(junk(1 << 20, 5));
    let r = run(&img);
    assert_eq!(r.recordings.len(), 1);
    let rec = &r.recordings[0];
    assert!(rec.stitched, "{rec:?}");
    assert_eq!(
        rec.pieces,
        vec![
            Piece {
                offset: at as u64,
                length: half as u64
            },
            Piece {
                offset: second as u64,
                length: (dat.len() - half) as u64
            }
        ]
    );
    assert_eq!(bytes_of(&img, rec), dat);
    assert!(r.fragments.is_empty());
}

#[test]
fn does_not_stitch_without_timecode() {
    // Video only: the break cannot be proven, so the recording stops and the rest is a
    // headerless fragment.
    let dat = canon_stream(3 << 20, 0, false, 20_000);
    let cut = 1 << 20;
    let mut img = junk(4096, 6);
    img.extend(&dat[..cut]);
    img.extend(junk(1 << 20, 7));
    img.extend(&dat[cut..]);
    img.extend(junk(4096, 8));
    let r = run(&img);
    assert_eq!(r.recordings.len(), 1);
    assert!(!r.recordings[0].stitched);
    let why = r.recordings[0].partial.as_deref().unwrap_or_default();
    assert!(why.contains("may continue at byte"), "{why}");
    // Frames after the cut are dropped. A frame that starts before the cut can only be checked
    // by its NAL length prefixes, so at most one frame of foreign bytes may remain.
    let kept = bytes_of(&img, &r.recordings[0]);
    assert!(kept.len() <= cut + 32 * 1024, "{}", kept.len());
    assert_eq!(kept[..cut], dat[..cut]);
    assert_eq!(r.fragments.len(), 1);
    let frag = &r.fragments[0];
    assert!(!frag.has_dat_header);
    // The fragment reads as a .DAT with a synthetic header.
    let bytes = bytes_of(&img, frag);
    assert_eq!(&bytes[..16], &DAT_HEADER);
    assert!(repi::walk(&bytes, &mut |_| true).is_ok());
}

#[test]
fn ignores_a_stale_recording_with_a_different_timecode() {
    // Recording A, then directly after it the start of an old recording B whose headers
    // would chain: the timecode jump ends A.
    let a = canon_stream(400_000, 10_000, true, 20_000);
    let mut b = canon_stream(400_000, 90_000, true, 20_000);
    b.drain(..16); // B's own .DAT header was overwritten
    let mut img = junk(8192, 9);
    img.extend(&a);
    img.extend(&b);
    img.extend(junk(8192, 10));
    let r = run(&img);
    assert_eq!(r.recordings.len(), 1);
    assert_eq!(r.recordings[0].length, a.len() as u64);
    assert_eq!(r.fragments.len(), 1);
    assert_eq!(r.fragments[0].start_timecode, Some(90_000));
}

/// A minimal finished Canon clip: `ftyp CAEP`, `moov` (mvhd + modl), `mdat` with groups.
fn finished_clip(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(24u32.to_be_bytes());
    v.extend(b"ftypmp42\0\0\0\0mp42CAEP");
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&2500u32.to_be_bytes());
    let mut modl = Vec::new();
    let name = b"Canon EOS R5m2\0";
    modl.extend(((8 + 6 + name.len()) as u32).to_be_bytes());
    modl.extend(b"modl\0\0\0\0\x15\xc7");
    modl.extend(name);
    let moov_len = 8 + 8 + mvhd.len() + modl.len();
    v.extend((moov_len as u32).to_be_bytes());
    v.extend(b"moov");
    v.extend(((8 + mvhd.len()) as u32).to_be_bytes());
    v.extend(b"mvhd");
    v.extend(mvhd);
    v.extend(modl);
    v.extend(1u32.to_be_bytes());
    v.extend(b"mdat");
    v.extend(((16 + payload.len()) as u64).to_be_bytes());
    v.extend(payload);
    v
}

#[test]
fn reports_finished_clips_and_does_not_carve_inside_them() {
    let body = canon_stream(300_000, 500, true, 20_000);
    let clip = finished_clip(&body[16..]);
    let mut img = junk(4096, 11);
    let clip_at = img.len() as u64;
    img.extend(&clip);
    while img.len() % 512 != 0 {
        img.push(0);
    }
    let dat = canon_stream(300_000, 900, true, 20_000);
    let dat_at = img.len() as u64;
    img.extend(&dat);
    img.extend(junk(4096, 12));
    let r = run(&img);
    assert_eq!(r.clips.len(), 1);
    let c = &r.clips[0];
    assert_eq!((c.offset, c.length), (clip_at, clip.len() as u64));
    assert!(c.complete);
    assert_eq!(c.brand, "CAEP");
    assert_eq!(c.camera.as_deref(), Some("Canon EOS R5 Mark II"));
    assert_eq!(c.duration_seconds, Some(2.5));
    assert!(r.fragments.is_empty(), "{:?}", r.fragments);
    assert_eq!(r.recordings.len(), 1);
    assert_eq!(r.recordings[0].offset, dat_at);
    assert!(r.recordings[0].overlaps_clips.is_empty());
    let refs = reference_candidates(&r, &r.recordings[0], 3);
    assert_eq!(refs.len(), 1);
}

#[test]
fn names_a_recording_from_a_deleted_exfat_entry() {
    use super::fs::tests::{exfat_image, exfat_set};
    // Cluster 40 of the tiny exFAT volume (heap at 16 KiB, 4 KiB clusters).
    let cluster = 40u32;
    let entries = vec![
        exfat_set("A_0001C001_LAD04.DAT", cluster, 1 << 20, false, true),
        exfat_set("A_0001C002_LAD04.DAT", 0, 0, false, false),
    ];
    let dat = canon_stream(200_000, 42, true, 20_000);
    let mut img = exfat_image(&entries, 0, 2 << 20);
    let at = 32 * 512 + (cluster as usize - 2) * 4096;
    img[at..at + dat.len()].copy_from_slice(&dat);
    let r = run(&img);
    assert_eq!(r.volumes.len(), 1);
    assert_eq!(r.recordings.len(), 1);
    let rec = &r.recordings[0];
    let f = rec.file.as_ref().unwrap();
    assert!(f.deleted);
    assert_eq!(f.path, "/DCIM/A_0001C001_LAD04.DAT");
    assert_eq!(r.dat_entries.len(), 2);
    assert_eq!(r.dat_entries[0].recording, Some(at as u64));
    assert_eq!(r.dat_entries[1].recording, None);
    assert_eq!(bytes_of(&img, rec), dat);
}

#[test]
fn extract_writes_a_new_file_and_never_overwrites() {
    let dat = canon_stream(100_000, 7, true, 5_000);
    let mut img = junk(4096, 13);
    img.extend(&dat);
    let r = run(&img);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join(recovered_name(&r.recordings[0]));
    let n = extract(
        &img,
        &r.recordings[0],
        &out,
        &ScanOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(n, dat.len() as u64);
    assert_eq!(std::fs::read(&out).unwrap(), dat);
    assert!(matches!(
        extract(
            &img,
            &r.recordings[0],
            &out,
            &ScanOptions::default(),
            &mut |_| {}
        ),
        Err(Error::OutputExists(_))
    ));
}

#[test]
fn signature_split_across_blocks_is_found() {
    // Put the .DAT start so that its header straddles the 8 MiB block boundary.
    let dat = canon_stream(100_000, 1, true, 5_000);
    let at = BLOCK - 512;
    let mut img = junk(at, 14);
    img.extend(&dat);
    let r = run(&img);
    assert_eq!(r.recordings.len(), 1);
    assert_eq!(r.recordings[0].offset, at as u64);
}

#[test]
fn cancel_stops_the_scan() {
    let img = junk(1 << 20, 15);
    let opts = ScanOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        scan(img.as_slice(), &opts, &mut |_| {}),
        Err(Error::Cancelled)
    ));
}

#[test]
fn two_equal_continuations_leave_the_recording_partial() {
    let mut dat = canon_stream(DAT_UNIT as usize + 200_000, 100, true, 20_000);
    dat.truncate(DAT_UNIT as usize);
    let half = 16 << 20;
    let mut img = junk(1 << 20, 16);
    img.extend(&dat[..half]);
    img.extend(junk(1 << 20, 17));
    img.extend(&dat[half..]);
    img.extend(junk(1 << 20, 18));
    img.extend(&dat[half..]); // a stale copy of the same bytes
    let r = run(&img);
    let rec = &r.recordings[0];
    assert!(!rec.stitched);
    assert!(
        rec.partial.as_deref().unwrap().contains("could continue"),
        "{rec:?}"
    );
    assert!((half as u64..half as u64 + 32 * 1024).contains(&rec.length));
}

#[test]
fn carved_recording_keeps_its_card_name_and_times() {
    use super::fs::tests::{exfat_image, exfat_set};
    let cluster = 40u32;
    let mut set = exfat_set("LAD12345.DAT", cluster, 0, false, true);
    // Created 2022-04-24 10:22:33.50 at UTC-04:00, modified 10:22:41 the same day.
    let date = (42u32 << 9) | (4 << 5) | 24;
    let created = (date << 16) | (10 << 11) | (22 << 5) | 16;
    let modified = (date << 16) | (10 << 11) | (22 << 5) | 20;
    set[8..12].copy_from_slice(&created.to_le_bytes());
    set[12..16].copy_from_slice(&modified.to_le_bytes());
    set[20] = 150; // +1.5 s
    set[22] = 0x80 | (-16i8 as u8 & 0x7f); // -16 x 15 min
    set[23] = set[22];
    let dat = canon_stream(200_000, 42, true, 20_000);
    let mut img = exfat_image(&[set], 0, 2 << 20);
    let at = 32 * 512 + (cluster as usize - 2) * 4096;
    img[at..at + dat.len()].copy_from_slice(&dat);
    let r = run(&img);
    let rec = &r.recordings[0];
    let f = rec.file.as_ref().unwrap();
    let local = crate::layout::days_from_civil(2022, 4, 24) * 86_400 + 10 * 3600 + 22 * 60 + 33;
    assert_eq!(
        f.created,
        Some(FsTime {
            local,
            utc_offset: Some(-4 * 3600)
        })
    );
    assert_eq!(f.created.unwrap().utc(), Some(local + 4 * 3600));
    assert_eq!(f.modified.unwrap().local, local + 7);
    assert_eq!(recovered_name(rec), PathBuf::from("LAD12345.DAT"));
    let hint = hint_for(rec);
    assert_eq!(hint.name.as_deref(), Some("LAD12345.DAT"));
    assert_eq!(hint.created_local, Some(local));
    assert_eq!(hint.utc_offset, Some(-4 * 3600));
    assert!(hint.ignore_file_time);
}

#[test]
fn recovered_name_without_a_usable_entry() {
    let mut img = vec![0u8; 1 << 20];
    let dat = canon_stream(200_000, 42, true, 20_000);
    img[4096..4096 + dat.len()].copy_from_slice(&dat);
    let r = run(&img);
    let rec = &r.recordings[0];
    assert_eq!(recovered_name(rec), PathBuf::from("recovered_4096.DAT"));
    let mut odd = rec.clone();
    odd.file = Some(FileRef {
        path: "/DCIM/../../etc/x.DAT".into(),
        size: 0,
        deleted: true,
        created: None,
        modified: None,
    });
    // Only the last part of the path is used; unusual characters fall back to the offset.
    assert_eq!(recovered_name(&odd), PathBuf::from("x.DAT"));
    odd.file.as_mut().unwrap().path = "/DCIM/a:b.DAT".into();
    assert_eq!(recovered_name(&odd), PathBuf::from("recovered_4096.DAT"));
    let hint = hint_for(rec);
    assert_eq!(hint.name, None);
    assert!(hint.ignore_file_time);
}

/// A 0-byte entry has no first cluster. A recording at a cluster no entry points at, and the
/// image with `entries` in `/DCIM`. The recording's timecode starts at `start` (seconds after
/// midnight).
fn image_with_unplaced_recording(entries: &[Vec<u8>], start: u32) -> (Vec<u8>, u64) {
    use super::fs::tests::exfat_image;
    // The synthetic stream's frame rate, learned from a scan of the stream alone.
    let probe = canon_stream(200_000, 0, true, 20_000);
    let fps = run(&probe).recordings[0].frame_rate.expect("frame rate");
    let tc0 = (start as f64 * fps).round() as u32;
    let dat = canon_stream(200_000, tc0, true, 20_000);
    let mut img = exfat_image(entries, 0, 2 << 20);
    let at = 32 * 512 + (40 - 2) * 4096;
    img[at..at + dat.len()].copy_from_slice(&dat);
    (img, at as u64)
}

#[test]
fn canon_name_times() {
    assert_eq!(
        name_time_of_day("/XFVC/CLIPS001/A_0001C809H241214_1422235Q_LAD04.DAT"),
        Some(14 * 3600 + 22 * 60 + 23)
    );
    assert_eq!(name_time_of_day("/DCIM/100EOSR5/LAD12345.DAT"), None);
    assert_eq!(name_time_of_day("H241214_996060"), None);
    assert_eq!(name_time_of_day("H241214_12345"), None);
}

#[test]
fn names_a_recording_from_a_zero_byte_entry_by_the_time_in_its_name() {
    use super::fs::tests::exfat_set;
    let name = "A_0002C054H220424_102233C0_LAD04.DAT";
    let entries = vec![
        exfat_set("A_0001C001_LAD04.MP4", 10, 50_000, false, false),
        exfat_set(name, 0, 0, false, false),
    ];
    let (img, at) = image_with_unplaced_recording(&entries, 10 * 3600 + 22 * 60 + 34);
    let r = run(&img);
    let rec = &r.recordings[0];
    assert_eq!(rec.offset, at);
    assert_eq!(rec.file_match, Some(FileMatch::StartTime));
    let f = rec.file.as_ref().unwrap();
    assert_eq!((f.path.as_str(), f.size), (&*format!("/DCIM/{name}"), 0));
    assert_eq!(recovered_name(rec), PathBuf::from(name));
    assert_eq!(hint_for(rec).name.as_deref(), Some(name));
    let h = r
        .dat_entries
        .iter()
        .find(|h| h.path.ends_with(name))
        .unwrap();
    assert_eq!(h.recording, Some(at));
}

#[test]
fn names_a_recording_from_a_zero_byte_entry_by_its_creation_time() {
    use super::fs::tests::exfat_set;
    let mut set = exfat_set("LAD12345.DAT", 0, 0, false, false);
    let date = (42u32 << 9) | (4 << 5) | 24;
    let created = (date << 16) | (10 << 11) | (22 << 5) | 16; // 10:22:32
    set[8..12].copy_from_slice(&created.to_le_bytes());
    set[12..16].copy_from_slice(&created.to_le_bytes());
    let (img, _) = image_with_unplaced_recording(&[set], 10 * 3600 + 22 * 60 + 33);
    let r = run(&img);
    let rec = &r.recordings[0];
    assert_eq!(rec.file_match, Some(FileMatch::StartTime));
    let hint = hint_for(rec);
    assert_eq!(hint.name.as_deref(), Some("LAD12345.DAT"));
    let local = crate::layout::days_from_civil(2022, 4, 24) * 86_400 + 10 * 3600 + 22 * 60 + 32;
    assert_eq!(hint.created_local, Some(local));
}

#[test]
fn does_not_name_by_time_when_the_match_is_ambiguous_or_off() {
    use super::fs::tests::exfat_set;
    let start = 10 * 3600 + 22 * 60 + 33;
    // Two entries with the same start time: neither names the recording.
    let entries = vec![
        exfat_set("A_0002C054H220424_102233C0_LAD04.DAT", 0, 0, false, false),
        exfat_set("B_0002C054H220424_102234C0_LAD04.DAT", 0, 0, false, false),
    ];
    let (img, _) = image_with_unplaced_recording(&entries, start);
    let r = run(&img);
    assert!(r.recordings[0].file.is_none());
    assert_eq!(
        recovered_name(&r.recordings[0]),
        PathBuf::from(format!("recovered_{}.DAT", r.recordings[0].offset))
    );
    // A name 5 s away does not match.
    let entries = vec![exfat_set(
        "A_0002C054H220424_102238C0_LAD04.DAT",
        0,
        0,
        false,
        false,
    )];
    let (img, _) = image_with_unplaced_recording(&entries, start);
    let r = run(&img);
    assert!(r.recordings[0].file.is_none());
    assert_eq!(r.recordings[0].file_match, None);
}

#[test]
fn first_cluster_match_is_marked() {
    use super::fs::tests::{exfat_image, exfat_set};
    let entries = vec![exfat_set("LAD1.DAT", 40, 0, false, true)];
    let dat = canon_stream(200_000, 42, true, 20_000);
    let mut img = exfat_image(&entries, 0, 2 << 20);
    let at = 32 * 512 + 38 * 4096;
    img[at..at + dat.len()].copy_from_slice(&dat);
    let r = run(&img);
    assert_eq!(r.recordings[0].file_match, Some(FileMatch::FirstCluster));
}
