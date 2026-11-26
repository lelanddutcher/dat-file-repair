//! Repairs every `.DAT` under a folder, using healthy MP4s from the same folder as references.
//!
//! Gated on `DATFIX_CORPUS`. Outputs go to `DATFIX_OUT` (default: a temporary folder) and are
//! deleted after checking unless `DATFIX_KEEP=1`. With `DATFIX_FFPROBE=1`, each output is also
//! decoded by `ffprobe -count_frames` (slow for big files).
//!
//! ```sh
//! DATFIX_CORPUS=/Volumes/15T/dat-file-repair/corpus DATFIX_OUT=/Volumes/15T/dat-file-repair/out \
//!   cargo test --release --test corpus -- --nocapture
//! ```
//!
//! A `.DAT` with no compatible reference next to it falls back to the built-in profiles. When
//! neither fits it is reported as skipped, not failed: that is missing test data, not a bug.
//! Any other error fails the test.
//!
//! `DATFIX_MODE=auto` ignores the neighbouring clips and uses the built-in profiles only.
//! Each line reports where the decoder settings came from: `user` (a neighbouring clip),
//! `hybrid` (its video + synthesized tracks) or `profile`.

use std::path::{Path, PathBuf};
use std::process::Command;

use datfix_core::Error;
use datfix_core::mp4::reference::Reference;
use datfix_core::repair::{self, ReferenceSource, RepairOptions};

fn find_dats(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            find_dats(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dat")) {
            out.push(p);
        }
    }
}

fn ffprobe_frames(path: &Path) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[test]
fn repairs_every_dat_in_corpus() {
    let Some(root) = std::env::var_os("DATFIX_CORPUS").map(PathBuf::from) else {
        eprintln!("DATFIX_CORPUS not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = std::env::var_os("DATFIX_OUT").map_or(tmp.path().to_path_buf(), PathBuf::from);
    let keep = std::env::var("DATFIX_KEEP").is_ok_and(|v| v == "1");
    let probe = std::env::var("DATFIX_FFPROBE").is_ok_and(|v| v == "1");
    let auto = std::env::var("DATFIX_MODE").is_ok_and(|v| v == "auto");

    let mut dats = Vec::new();
    find_dats(&root, &mut dats);
    dats.sort();
    assert!(!dats.is_empty(), "no .DAT files under {}", root.display());

    let mut failures = Vec::new();
    let mut skipped = 0;
    for (i, dat) in dats.iter().enumerate() {
        let name = dat.file_stem().unwrap().to_string_lossy();
        let out = out_dir.join(format!("corpus_{i:02}_{name}.MP4"));
        let opts = RepairOptions {
            overwrite: true,
            ..Default::default()
        };
        let dir = dat.parent().unwrap().to_path_buf();
        let source = if auto {
            ReferenceSource::Auto
        } else {
            ReferenceSource::Dir(dir)
        };
        let started = std::time::Instant::now();
        match repair::repair(dat, &source, &out, &opts, &mut |_| {}) {
            Ok(report) => {
                // The output must parse and hold every frame the report claims.
                let parsed = Reference::open(&out).unwrap();
                let frames = parsed.video_track().unwrap().sample_count;
                assert_eq!(frames, report.video_frames, "{}", dat.display());
                let decoded = if probe { ffprobe_frames(&out) } else { None };
                eprintln!(
                    "OK    {name}: {:?} {}, {frames} frames, {:.1} s, {} tracks, {:.1} GB, damaged {}, ffprobe {:?}, {:.1} s",
                    report.reference_kind,
                    report.reference.display(),
                    report.duration_seconds,
                    report.tracks.len(),
                    report.bytes_written as f64 / 1e9,
                    report.damaged_frames,
                    decoded,
                    started.elapsed().as_secs_f64()
                );
                if let Some(d) = decoded {
                    if d != frames {
                        failures.push(format!("{name}: ffprobe decoded {d} of {frames} frames"));
                    }
                }
                if !keep {
                    let _ = std::fs::remove_file(&out);
                }
            }
            Err(e @ (Error::NoReference { .. } | Error::NoProfile { .. })) => {
                skipped += 1;
                let first = e
                    .to_string()
                    .lines()
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                eprintln!("SKIP  {name}: no compatible reference ({first})");
            }
            Err(e) => {
                eprintln!("FAIL  {name}: {e}");
                failures.push(format!("{name}: {e}"));
            }
        }
    }
    eprintln!(
        "{} .DAT files: {} repaired, {skipped} skipped, {} failed",
        dats.len(),
        dats.len() - skipped - failures.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
