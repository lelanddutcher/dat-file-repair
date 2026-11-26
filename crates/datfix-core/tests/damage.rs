//! Mid-file damage must not break audio/video sync.
//!
//! A damaged copy of a `.DAT` (a zeroed block in the middle) is repaired next to the clean
//! file. Every sample that survives must play at the same time in both outputs, and audio must
//! last as long as video.
//!
//! The synthetic test always runs. The real-media test is gated:
//!
//! ```sh
//! DATFIX_DAMAGE_DAT=/Volumes/15T/dat-file-repair/samples/r5m2_64MB.DAT \
//! DATFIX_DAMAGE_OUT=/Volumes/15T/dat-file-repair/out/synthetic \
//!   cargo test --release --test damage -- --nocapture
//! ```
//!
//! Optional: `DATFIX_DAMAGE_AT` (byte offset of the zeroed block, default: 29.5 MiB, which
//! removes the audio and video headers of the second interval of the R5 Mark II sample),
//! `DATFIX_DAMAGE_LEN` (default 1 MiB), `DATFIX_DAMAGE_REF` (reference clip; default: the
//! built-in profiles).

mod common;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use datfix_core::mp4::atom;
use datfix_core::mp4::reference::{Reference, TrackKind};
use datfix_core::mp4::tables;
use datfix_core::read_at::FileSource;
use datfix_core::repair::{self, ReferenceSource, RepairOptions, RepairReport};

/// For each track (by handler + order), a map from `.DAT` offset to (decode time in seconds,
/// samples in that chunk-run), plus the track's total duration.
struct Placement {
    /// `(track index, dat offset) -> decode time (s)`
    times: HashMap<(usize, u64), f64>,
    durations: Vec<(TrackKind, f64)>,
}

fn placement(out: &Path) -> Placement {
    let r = Reference::open(out).unwrap();
    let src = FileSource::open(out).unwrap();
    let mdat = atom::scan_top_level(&src)
        .unwrap()
        .into_iter()
        .find(|b| b.kind.0 == *b"mdat")
        .unwrap();
    // The output payload is the .DAT from its first group (offset 16) on.
    let to_dat = |o: u64| o - (mdat.offset + mdat.header_len) + 16;
    let mut times = HashMap::new();
    let mut durations = Vec::new();
    for (ti, t) in r.tracks.iter().enumerate() {
        let stbl = r.trak(t).find(&[b"mdia", b"minf", b"stbl"]).unwrap();
        let stts = tables::parse_stts(stbl.child(b"stts").unwrap().payload().unwrap()).unwrap();
        let mut deltas = stts
            .iter()
            .flat_map(|&(n, d)| std::iter::repeat_n(d, n as usize));
        let mut ticks = 0u64;
        for (off, _) in r.sample_locations(t).unwrap() {
            times.insert((ti, to_dat(off)), ticks as f64 / t.timescale as f64);
            ticks += deltas.next().unwrap() as u64;
        }
        durations.push((t.kind, ticks as f64 / t.timescale as f64));
    }
    Placement { times, durations }
}

/// Asserts that every sample of `damaged` sits at the same time as in `clean`, and that audio
/// lasts as long as video (within one frame). Returns samples compared after the damage.
fn assert_in_sync(clean: &Placement, damaged: &Placement, frame: f64, damage_end: u64) -> usize {
    let mut after = 0;
    for (&(ti, off), &t) in &damaged.times {
        // Inserted silence points past the copied payload; it has no clean counterpart.
        let Some(&c) = clean.times.get(&(ti, off)) else {
            continue;
        };
        assert!(
            (c - t).abs() < 1e-6,
            "track {ti} sample at .DAT byte {off}: {t:.6} s in the damaged repair, {c:.6} s in the clean one"
        );
        if off > damage_end {
            after += 1;
        }
    }
    let video = damaged
        .durations
        .iter()
        .find(|d| d.0 == TrackKind::Video)
        .unwrap()
        .1;
    for (kind, d) in &damaged.durations {
        assert!(
            (d - video).abs() <= frame + 1e-9,
            "{kind:?} lasts {d:.4} s, video {video:.4} s"
        );
    }
    after
}

#[test]
fn synthetic_damage_keeps_sync() {
    let dir = tempfile::tempdir().unwrap();
    let clean = common::dat(4, 2);
    // Zero the whole third interval (index 2): headers and samples are gone.
    let interval = (clean.len() - 16) / 4;
    let mut damaged = clean.clone();
    let start = 16 + 2 * interval;
    damaged[start..start + interval].fill(0);
    let reference = dir.path().join("ref.MP4");
    std::fs::write(&reference, common::reference(2, &common::dat(2, 2)[16..])).unwrap();

    let run = |bytes: &[u8], name: &str| -> (RepairReport, PathBuf) {
        let dat = dir.path().join(format!("{name}.DAT"));
        std::fs::write(&dat, bytes).unwrap();
        let out = dir.path().join(format!("{name}.MP4"));
        let r = repair::repair(
            &dat,
            &ReferenceSource::File(reference.clone()),
            &out,
            &RepairOptions::default(),
            &mut |_| {},
        )
        .unwrap();
        (r, out)
    };
    let (rc, oc) = run(&clean, "clean");
    let (rd, od) = run(&damaged, "damaged");
    assert_eq!(rc.video_frames, 24);
    assert_eq!(rd.video_frames, 18);
    assert_eq!(rd.gaps.len(), 1);
    assert_eq!(rd.gaps[0].lost_frames, 6);
    assert_eq!(rd.gaps[0].method, "timecode");
    // Still 0.96 s long: the frame before the gap holds, audio gets silence.
    assert!((rd.duration_seconds - 0.96).abs() < 1e-9);
    let after = assert_in_sync(
        &placement(&oc),
        &placement(&od),
        0.04,
        (start + interval) as u64,
    );
    assert!(after > 0, "no samples after the damage were compared");
}

/// Copies `src` to `dst`, zeroing `len` bytes at `at`. The source is only read.
fn damaged_copy(src: &Path, dst: &Path, at: u64, len: u64) {
    let mut input = std::fs::File::open(src).unwrap();
    let mut out = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .unwrap();
    std::io::copy(&mut input, &mut out).unwrap();
    out.seek(SeekFrom::Start(at)).unwrap();
    out.write_all(&vec![0u8; len as usize]).unwrap();
    out.sync_all().unwrap();
    // The source is unchanged.
    let mut check = vec![0u8; 16];
    input.seek(SeekFrom::Start(0)).unwrap();
    input.read_exact(&mut check).unwrap();
    assert_eq!(&check[4..8], b"mdat");
}

#[test]
fn real_damage_keeps_sync() {
    let Some(dat) = std::env::var_os("DATFIX_DAMAGE_DAT").map(PathBuf::from) else {
        eprintln!("DATFIX_DAMAGE_DAT not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let out_dir =
        std::env::var_os("DATFIX_DAMAGE_OUT").map_or(tmp.path().to_path_buf(), PathBuf::from);
    std::fs::create_dir_all(&out_dir).unwrap();
    let env_u64 = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let at = env_u64("DATFIX_DAMAGE_AT", 29 * 1024 * 1024 + 512 * 1024);
    let len = env_u64("DATFIX_DAMAGE_LEN", 1024 * 1024);
    let source = match std::env::var_os("DATFIX_DAMAGE_REF") {
        Some(r) => ReferenceSource::File(PathBuf::from(r)),
        None => ReferenceSource::Auto,
    };
    let stem = dat.file_stem().unwrap().to_string_lossy().to_string();
    let damaged = out_dir.join(format!("{stem}_zeroed_{at}_{len}.DAT"));
    damaged_copy(&dat, &damaged, at, len);

    let opts = RepairOptions {
        overwrite: true,
        ..Default::default()
    };
    let oc = out_dir.join(format!("{stem}_clean.MP4"));
    let od = out_dir.join(format!("{stem}_zeroed.MP4"));
    let rc = repair::repair(&dat, &source, &oc, &opts, &mut |_| {}).unwrap();
    let rd = repair::repair(&damaged, &source, &od, &opts, &mut |_| {}).unwrap();
    eprintln!(
        "clean: {} frames {:.4} s; damaged: {} frames {:.4} s, gaps {:?}",
        rc.video_frames, rc.duration_seconds, rd.video_frames, rd.duration_seconds, rd.gaps
    );
    for n in &rd.notes {
        eprintln!("  note: {n}");
    }
    let frame = 1.0 / (rc.video_frames as f64 / rc.duration_seconds);
    let after = assert_in_sync(&placement(&oc), &placement(&od), frame, at + len);
    eprintln!("{after} samples after the damage sit at the same time as in the clean repair");
    assert!(after > 0);
    assert!(!rd.gaps.is_empty(), "the damage was not detected");
}
