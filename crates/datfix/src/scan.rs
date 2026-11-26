//! `datfix scan`: find recordings on a card image or device, and optionally save or repair them.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use datfix_core::carve::{
    self, FinishedClip, ImageSource, Recovered, ScanEvent, ScanOptions, ScanPhase, ScanReport,
    StreamEnd,
};
use datfix_core::repair::{self, ReferenceSource, RepairOptions};

use crate::{clear_progress_line, emit_json, event_printer, hms, print_repair};

/// Finished clips tried as references for one recovered recording.
const MAX_REFERENCES: usize = 3;

#[derive(Args)]
pub struct ScanArgs {
    /// A card image (`dd` output, `.img`, raw `.dmg`) or a device such as /dev/rdisk4.
    /// It is only read. Imaging the card first is the safe way; see docs/format/card-carving.md.
    pub image: PathBuf,
    /// Save each recording found as <dir>/recovered_<offset>.DAT. Without it, only report.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Also repair each recording to <dir>/recovered_<offset>.MP4, using finished clips from
    /// the same card as references, then the built-in profiles.
    #[arg(long, requires = "out")]
    pub repair: bool,
    /// Also save the finished Canon clips found (including deleted ones) as
    /// <dir>/clips/clip_<offset>.MP4.
    #[arg(long, requires = "out")]
    pub clips: bool,
    /// Also save headerless fragments (pieces no .DAT start leads to) as fragment_<offset>.DAT.
    #[arg(long, requires = "out")]
    pub fragments: bool,
    /// Do not join fragmented recordings.
    #[arg(long)]
    pub no_stitch: bool,
}

fn scan_printer(json: bool) -> impl FnMut(&ScanEvent) {
    let tty = std::io::stderr().is_terminal();
    move |ev: &ScanEvent| {
        if json {
            emit_json(ev);
            return;
        }
        match ev {
            ScanEvent::Phase { message, .. } => {
                clear_progress_line();
                eprintln!("{message}...");
            }
            ScanEvent::Progress { phase, done, total } if tty && *total > 0 => {
                let verb = match phase {
                    ScanPhase::Extracting => "saved",
                    _ => "read",
                };
                eprint!(
                    "\r  {:5.1}%  {verb} {:.1}/{:.1} GB   ",
                    *done as f64 * 100.0 / *total as f64,
                    *done as f64 / 1e9,
                    *total as f64 / 1e9
                );
                let _ = std::io::stderr().flush();
            }
            _ => {}
        }
    }
}

pub fn run(args: &ScanArgs, json: bool) -> datfix_core::Result<()> {
    let opts = ScanOptions {
        stitch: !args.no_stitch,
        ..Default::default()
    };
    let (src, report) = carve::scan_path(&args.image, &opts, &mut scan_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "result", "scan": report }));
    } else {
        clear_progress_line();
        print_report(&args.image, &report);
    }
    let Some(dir) = &args.out else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)
        .map_err(|e| datfix_core::Error::io(format!("cannot create {}", dir.display()), e))?;
    let mut saved = Vec::new();
    let mut names = std::collections::HashSet::new();
    for rec in &report.recordings {
        // The original name, unless two recordings would share it.
        let mut name = carve::recovered_name(rec);
        if !names.insert(name.clone()) {
            name = PathBuf::from(format!("recovered_{}.DAT", rec.offset));
        }
        let path = dir.join(name);
        save(&src, rec, &path, &opts, json)?;
        saved.push((rec, path));
    }
    if args.fragments {
        for rec in &report.fragments {
            let path = dir.join(format!("fragment_{}.DAT", rec.offset));
            save(&src, rec, &path, &opts, json)?;
        }
    }
    if args.clips {
        let clips_dir = dir.join("clips");
        std::fs::create_dir_all(&clips_dir).map_err(|e| {
            datfix_core::Error::io(format!("cannot create {}", clips_dir.display()), e)
        })?;
        for clip in &report.clips {
            save_clip(&src, clip, &clips_dir.join(clip_name(clip)), &opts, json)?;
        }
    }
    if args.repair {
        let mut failed = 0;
        for (rec, dat) in &saved {
            if let Err(e) = repair_one(&src, &report, rec, dat, dir, &opts, json) {
                failed += 1;
                let m = e.user_message();
                if json {
                    emit_json(&serde_json::json!({
                        "event": "repair_failed", "offset": rec.offset, "title": m.title,
                        "message": m.message, "detail": e.to_string(),
                    }));
                } else {
                    clear_progress_line();
                    eprintln!("could not repair {}: {}", dat.display(), m.title);
                    eprintln!("  {}", m.message);
                }
            }
        }
        if failed > 0 && !json {
            eprintln!(
                "{failed} of {} recording(s) could not be repaired; the .DAT copies are kept.",
                saved.len()
            );
        }
    }
    Ok(())
}

fn clip_name(clip: &FinishedClip) -> String {
    format!("clip_{}.MP4", clip.offset)
}

fn save(
    src: &ImageSource,
    rec: &Recovered,
    path: &Path,
    opts: &ScanOptions,
    json: bool,
) -> datfix_core::Result<()> {
    if path.exists() {
        if !json {
            eprintln!("  {} already exists; kept as is", path.display());
        }
        return Ok(());
    }
    let n = carve::extract(src, rec, path, opts, &mut scan_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "saved", "path": path, "bytes": n }));
    } else {
        clear_progress_line();
        eprintln!("  saved {} ({:.2} GB)", path.display(), n as f64 / 1e9);
    }
    Ok(())
}

fn save_clip(
    src: &ImageSource,
    clip: &FinishedClip,
    path: &Path,
    opts: &ScanOptions,
    json: bool,
) -> datfix_core::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let n = carve::extract_clip(src, clip, path, opts, &mut scan_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "saved", "path": path, "bytes": n }));
    } else {
        clear_progress_line();
        eprintln!("  saved {} ({:.2} GB)", path.display(), n as f64 / 1e9);
    }
    Ok(())
}

/// Repairs one saved recording. References: finished clips from the card (same brand,
/// nearest first), copied into `<dir>/clips/`.
fn repair_one(
    src: &ImageSource,
    report: &ScanReport,
    rec: &Recovered,
    dat: &Path,
    dir: &Path,
    opts: &ScanOptions,
    json: bool,
) -> datfix_core::Result<()> {
    let refs = carve::reference_candidates(report, rec, MAX_REFERENCES);
    let ref_dir = dir.join("clips");
    let source = if refs.is_empty() {
        ReferenceSource::Builtin
    } else {
        std::fs::create_dir_all(&ref_dir).map_err(|e| {
            datfix_core::Error::io(format!("cannot create {}", ref_dir.display()), e)
        })?;
        for c in &refs {
            save_clip(src, c, &ref_dir.join(clip_name(c)), opts, json)?;
        }
        ReferenceSource::Dir(ref_dir)
    };
    let output = dat.with_extension("MP4");
    let report = repair::repair(
        dat,
        &source,
        &output,
        &RepairOptions {
            hint: Some(carve::hint_for(rec)),
            ..RepairOptions::default()
        },
        &mut event_printer(json),
    )?;
    if json {
        emit_json(&serde_json::json!({ "event": "result", "repair": report }));
    } else {
        clear_progress_line();
        print_repair(&report);
    }
    Ok(())
}

fn print_report(image: &Path, r: &ScanReport) {
    println!(
        "{}: {:.2} GB{}, scanned in {:.1} s ({:.0} MB/s), {} group headers",
        image.display(),
        r.size as f64 / 1e9,
        if r.is_device { " (device)" } else { "" },
        r.seconds,
        r.size as f64 / 1e6 / r.seconds.max(0.001),
        r.headers_seen
    );
    for v in &r.volumes {
        println!(
            "  file system: {:?} at byte {}, {} KiB clusters, {} file(s), {} deleted entr(ies)",
            v.kind,
            v.offset,
            v.cluster_size / 1024,
            v.files,
            v.deleted
        );
    }
    println!();
    if r.recordings.is_empty() {
        println!("No interrupted recordings found.");
    } else {
        println!("Interrupted recordings: {}", r.recordings.len());
        for rec in &r.recordings {
            print_recovered(rec, "recovered");
        }
    }
    if !r.fragments.is_empty() {
        println!();
        println!(
            "Headerless fragments (no .DAT start leads to them): {}",
            r.fragments.len()
        );
        for rec in &r.fragments {
            print_recovered(rec, "fragment");
        }
    }
    if !r.clips.is_empty() {
        println!();
        println!("Finished Canon clips: {}", r.clips.len());
        for c in &r.clips {
            let name = c.file.as_ref().map_or(String::new(), |f| {
                format!("  {}{}", f.path, if f.deleted { " (deleted)" } else { "" })
            });
            println!(
                "  byte {:>14}  {:>9.1} MB  {} {}{}{}{}",
                c.offset,
                c.length as f64 / 1e6,
                c.brand,
                c.camera.as_deref().unwrap_or("unknown camera"),
                c.duration_seconds
                    .map_or(String::new(), |d| format!(", {}", hms(d))),
                if c.complete { "" } else { ", incomplete" },
                name
            );
        }
    }
    if !r.dat_entries.is_empty() {
        println!();
        println!(".DAT entries in the file system:");
        for h in &r.dat_entries {
            println!(
                "  {}  {} bytes{}  -> {}",
                h.path,
                h.size,
                if h.deleted { ", deleted" } else { "" },
                match (h.recording, h.data_offset) {
                    (Some(o), _) => format!("recording at byte {o}"),
                    (None, Some(o)) => format!("nothing found at byte {o}"),
                    (None, None) => "no start cluster recorded".into(),
                }
            );
        }
    }
    for n in &r.notes {
        println!("note: {n}");
    }
}

fn print_recovered(rec: &Recovered, label: &str) {
    let name = rec.file.as_ref().map_or(String::new(), |f| {
        format!(
            " = {}{}",
            f.path,
            if f.deleted {
                " (deleted)"
            } else if f.size == 0 {
                " (0-byte entry)"
            } else {
                ""
            }
        )
    });
    println!(
        "  {label}_{}.DAT  {:.1} MB, {} frames{}{name}",
        rec.offset,
        rec.length as f64 / 1e6,
        rec.video_frames,
        rec.duration_seconds
            .map_or(String::new(), |d| format!(", {}", hms(d)))
    );
    if let Some(desc) = &rec.recording {
        println!("      {desc}");
    }
    let end = match rec.end {
        StreamEnd::DatUnit => "ends at a 32 MiB unit, like the camera's .DAT",
        StreamEnd::LastGroup => "ends after the last complete group",
        StreamEnd::ImageEnd => "cut off by the end of the image",
        StreamEnd::FileSize => "ends at the size in the file entry",
    };
    let pieces = if rec.pieces.len() > 1 {
        let list: Vec<String> = rec
            .pieces
            .iter()
            .map(|p| format!("{}+{}", p.offset, p.length))
            .collect();
        format!(
            "; joined from {} pieces ({})",
            rec.pieces.len(),
            list.join(", ")
        )
    } else {
        String::new()
    };
    println!("      {} groups; {end}{pieces}", rec.groups);
    if !rec.overlaps_clips.is_empty() {
        println!(
            "      overlaps finished clip(s) at byte {:?}: part of it was overwritten",
            rec.overlaps_clips
        );
    }
    if let Some(p) = &rec.partial {
        println!("      may be incomplete: {p}");
    }
}
