//! Inputs and environments that real users hit: wrong files picked as the reference,
//! renamed or carved `.DAT` files, files too short to hold a frame, unusual paths, and
//! output folders that cannot take the file. Each case must end in a clear message (or a
//! working repair), never a panic.

mod common;

use std::path::{Path, PathBuf};

use datfix_core::Error;
use datfix_core::inspect;
use datfix_core::repair::{self, ReferenceSource, RepairOptions, RepairReport};

fn write(path: &Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

/// A synthetic `.DAT` (3 intervals, 2 PCM streams) and a matching reference in `dir`.
fn setup(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("clip.DAT");
    write(&dat, &common::dat(3, 2));
    let reference = dir.join("ref.MP4");
    write(&reference, &common::reference(2, &common::dat(2, 2)[16..]));
    (dat, reference)
}

fn run(dat: &Path, reference: &Path, out: &Path) -> datfix_core::Result<RepairReport> {
    repair::repair(
        dat,
        &ReferenceSource::File(reference.to_path_buf()),
        out,
        &RepairOptions::default(),
        &mut |_| {},
    )
}

fn kind(r: datfix_core::Result<RepairReport>) -> (&'static str, String, Error) {
    let Err(e) = r else {
        panic!("expected an error")
    };
    let m = e.user_message();
    (
        m.kind,
        format!("{} {} {}", m.title, m.message, m.next_step),
        e,
    )
}

// ---------------------------------------------------------------------------------------
// The reference is not a healthy clip
// ---------------------------------------------------------------------------------------

#[test]
fn reference_is_the_dat_itself() {
    let dir = tempfile::tempdir().unwrap();
    let (dat, _) = setup(dir.path());
    let (k, text, _) = kind(run(&dat, &dat, &dir.path().join("out.MP4")));
    assert_eq!(k, "not_a_clip");
    assert!(text.contains("the broken .DAT itself"), "{text}");
    // Another unfinished recording is recognized from its first bytes.
    let other = dir.path().join("other.DAT");
    std::fs::copy(&dat, &other).unwrap();
    let (k, text, _) = kind(run(&dat, &other, &dir.path().join("out.MP4")));
    assert_eq!(k, "not_a_clip");
    assert!(text.contains("unfinished recording"), "{text}");
}

#[test]
fn reference_is_a_photo_or_raw_file() {
    let dir = tempfile::tempdir().unwrap();
    let (dat, _) = setup(dir.path());
    let out = dir.path().join("out.MP4");

    let jpeg = dir.path().join("IMG_0001.JPG");
    write(
        &jpeg,
        &[
            0xff, 0xd8, 0xff, 0xe1, 0, 0x10, b'E', b'x', b'i', b'f', 0, 0,
        ],
    );
    let (k, text, _) = kind(run(&dat, &jpeg, &out));
    assert_eq!(k, "not_a_clip");
    assert!(text.contains("JPEG"), "{text}");

    // Canon CR3 photos and Cinema RAW Light (.CRM) clips are ISO files with brand `crx `.
    let mut crm = 24u32.to_be_bytes().to_vec();
    crm.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
    crm.extend_from_slice(&8u32.to_be_bytes());
    crm.extend_from_slice(b"moov");
    let crm_path = dir.path().join("A001C001.CRM");
    write(&crm_path, &crm);
    let (k, text, _) = kind(run(&dat, &crm_path, &out));
    assert_eq!(k, "not_a_clip");
    assert!(text.contains("Canon RAW"), "{text}");

    let empty = dir.path().join("zeros.MP4");
    write(&empty, &[0u8; 4096]);
    let (k, _, _) = kind(run(&dat, &empty, &out));
    assert_eq!(k, "not_a_clip");
    assert!(!out.exists());
}

#[test]
fn reference_cut_short_or_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let (dat, reference) = setup(dir.path());
    let out = dir.path().join("out.MP4");
    let full = std::fs::read(&reference).unwrap();

    // Cut inside the moov box (a copy that stopped early).
    let cut = dir.path().join("cut.MP4");
    write(&cut, &full[..200]);
    let (k, _, e) = kind(run(&dat, &cut, &out));
    assert_eq!(k, "bad_reference");
    assert!(e.to_string().contains("cut short"), "{e}");

    // Deterministic garbage of several sizes: errors only.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for len in [1usize, 7, 8, 16, 100, 4096, 70_000] {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let g = dir.path().join(format!("garbage{len}.MP4"));
        write(&g, &bytes);
        let (k, _, _) = kind(run(&dat, &g, &out));
        assert!(
            ["bad_reference", "not_a_clip"].contains(&k),
            "{len} bytes: {k}"
        );
    }
}

#[test]
fn quicktime_mov_reference_with_the_same_settings_works() {
    let dir = tempfile::tempdir().unwrap();
    let (dat, reference) = setup(dir.path());
    let mut mov = std::fs::read(&reference).unwrap();
    // Same tracks, QuickTime brand.
    mov[8..12].copy_from_slice(b"qt  ");
    let mov_path = dir.path().join("ref.MOV");
    write(&mov_path, &mov);
    let r = run(&dat, &mov_path, &dir.path().join("out.MP4")).unwrap();
    assert_eq!(r.video_frames, 18);
}

// ---------------------------------------------------------------------------------------
// The .DAT is unusual
// ---------------------------------------------------------------------------------------

#[test]
fn renamed_playable_mp4_says_it_already_plays() {
    let dir = tempfile::tempdir().unwrap();
    let (_, reference) = setup(dir.path());
    let renamed = dir.path().join("MVI_0001.DAT");
    std::fs::copy(&reference, &renamed).unwrap();
    let (k, text, e) = kind(run(&renamed, &reference, &dir.path().join("out.MP4")));
    assert_eq!(k, "already_playable");
    assert!(text.contains("already plays"), "{text}");
    assert!(text.contains(".MP4"), "{text}");
    assert!(e.to_string().contains("track(s)"), "{e}");
    // `inspect` says the same.
    let Err(e) = inspect::inspect(&renamed, &mut |_| {}) else {
        panic!("inspect must refuse a playable MP4")
    };
    assert_eq!(e.user_message().kind, "already_playable");
}

#[test]
fn leading_zeros_from_a_card_carve_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (_, reference) = setup(dir.path());
    // A piece carved from a card image: 3 MiB of zeros, then the groups (no mdat header).
    let mut carved = vec![0u8; 3 << 20];
    carved.extend(&common::dat(3, 2)[16..]);
    let dat = dir.path().join("f12345678.DAT");
    write(&dat, &carved);
    let r = run(&dat, &reference, &dir.path().join("out.MP4")).unwrap();
    assert_eq!(r.video_frames, 18);
    // The payload starts at the first group, not at the zeros.
    assert!(r.bytes_written < (1 << 20), "{}", r.bytes_written);
}

#[test]
fn files_too_short_for_one_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (_, reference) = setup(dir.path());
    let out = dir.path().join("out.MP4");
    let full = common::dat(1, 2);

    // 0 bytes and all zeros: nothing was saved.
    for (name, bytes) in [("empty.DAT", vec![]), ("zeros.DAT", vec![0u8; 3 << 20])] {
        let p = dir.path().join(name);
        write(&p, &bytes);
        let (k, text, _) = kind(run(&p, &reference, &out));
        assert_eq!(k, "empty_dat", "{name}");
        assert!(text.contains("PhotoRec"));
    }

    // Only the 16-byte header, then a header cut in its size table, then a cut inside the
    // first video frame: a Canon recording with no complete frame.
    let first_vide = full
        .windows(8)
        .position(|w| w[..4] == *b"repi" && w[7] == 0x83)
        .unwrap();
    let vide_at = full
        .windows(12)
        .position(|w| w[..4] == *b"repi" && &w[8..12] == b"vide")
        .unwrap();
    assert!(first_vide < vide_at);
    for (name, len) in [
        ("header.DAT", 16),
        ("cut_header.DAT", 16 + 10),
        ("cut_frame.DAT", vide_at + 20 + 4 * 6 + 50),
    ] {
        let p = dir.path().join(name);
        write(&p, &full[..len]);
        let (k, text, _) = kind(run(&p, &reference, &out));
        assert_eq!(k, "no_frames", "{name}: {text}");
        assert!(text.contains("first complete video frame"), "{text}");
    }
    assert!(!out.exists());
}

// ---------------------------------------------------------------------------------------
// Paths and output folders
// ---------------------------------------------------------------------------------------

#[test]
fn non_ascii_spaces_and_long_names() {
    let dir = tempfile::tempdir().unwrap();
    let (_, reference) = setup(dir.path());
    let folder = dir.path().join("Hochzeit Müller").join("Trauung – Kirche");
    let dat = folder.join("クリップ 01.DAT");
    write(&dat, &common::dat(3, 2));
    let out = folder.join("クリップ 01_repaired.MP4");
    let r = run(&dat, &reference, &out).unwrap();
    assert_eq!(r.video_frames, 18);
    assert!(out.exists());

    // An output name close to the 255-byte limit: the temporary name must still fit.
    let long = format!("{}.MP4", "Ä".repeat(124)); // 248 + 4 bytes
    assert!(long.len() <= 255);
    let out = folder.join(&long);
    let r = run(&dat, &reference, &out).unwrap();
    assert_eq!(r.output, out);
    assert!(out.exists());
    let temp = repair::temp_path(&out);
    let temp_name = temp.file_name().unwrap().to_string_lossy().into_owned();
    assert!(temp_name.len() <= 255, "{}", temp_name.len());
    assert!(repair::is_partial_name(&temp_name));
}

#[test]
fn output_folder_missing_or_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let (dat, reference) = setup(dir.path());
    let (k, text, _) = kind(run(
        &dat,
        &reference,
        &dir.path().join("no such folder/out.MP4"),
    ));
    assert_eq!(k, "output_not_writable");
    assert!(text.contains("another folder"), "{text}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = dir.path().join("card");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = run(&dat, &reference, &ro.join("out.MP4"));
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores folder permissions; the check only applies to normal users.
        if let Err(e) = r {
            assert_eq!(e.user_message().kind, "output_not_writable");
            assert!(e.user_message().next_step.contains("another folder"));
        }
    }
}

#[test]
#[cfg(unix)]
fn input_on_read_only_media_is_only_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (dat, reference) = setup(dir.path());
    let card = dir.path().join("CARD/DCIM/100EOSR5");
    std::fs::create_dir_all(&card).unwrap();
    let on_card = card.join("A001.DAT");
    std::fs::copy(&dat, &on_card).unwrap();
    std::fs::set_permissions(&on_card, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&card, std::fs::Permissions::from_mode(0o555)).unwrap();
    let before = std::fs::read(&on_card).unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let r = run(&on_card, &reference, &out_dir.join("A001.MP4"));
    std::fs::set_permissions(&card, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r.unwrap().video_frames, 18);
    assert_eq!(std::fs::read(&on_card).unwrap(), before);
}

#[test]
fn every_new_message_names_a_next_step() {
    let dir = PathBuf::from("/Volumes/CARD");
    let io = || std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem);
    let errors = [
        Error::NotAClip {
            path: dir.join("x.JPG"),
            what: "a photo (JPEG)".into(),
        },
        Error::AlreadyPlayable {
            path: dir.join("x.DAT"),
            detail: "d".into(),
        },
        Error::NoFrames {
            path: dir.join("x.DAT"),
            detail: "d".into(),
        },
        Error::FileTooLargeForVolume {
            dir: dir.clone(),
            fs: "FAT32".into(),
            size: 5 << 30,
            limit: (1 << 32) - 1,
        },
        Error::NotEnoughSpace {
            dir: dir.clone(),
            need: 5 << 30,
            free: 1 << 30,
        },
        Error::OutputNotWritable {
            dir: dir.clone(),
            source: io(),
        },
        Error::TooMuchUnreadable {
            unreadable: 1,
            total: 2,
            last_offset: 0,
            source: io(),
        },
    ];
    for e in errors {
        let m = e.user_message();
        assert!(!m.title.is_empty() && !m.message.is_empty() && !m.next_step.is_empty());
        assert!(!e.to_string().is_empty());
    }
    // A file-too-large error from the OS (FAT32 at 4 GiB) explains the limit.
    let m = datfix_core::error::io_message(&std::io::Error::from(std::io::ErrorKind::FileTooLarge));
    assert!(m.message.contains("FAT32"));
}
