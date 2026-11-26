//! `datfix`: repair interrupted Canon `.DAT` recordings into playable `.MP4` files.
//!
//! This binary is a thin wrapper around `datfix-core`. It parses arguments, prints progress,
//! and prints results either as friendly text or as newline-delimited JSON (`--json`), one
//! object per line, for GUIs and scripts.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod scan;

use clap::{Parser, Subcommand};
use datfix_core::inspect::{self, InspectReport};
use datfix_core::profile;
use datfix_core::repair::{
    self, Event, Phase, RefKind, ReferenceSource, RepairOptions, RepairReport, Rotation,
};

/// Parses `--rotate`: 0, 90, 180 or 270 (also -90).
fn parse_rotation(s: &str) -> Result<Rotation, String> {
    s.trim()
        .trim_end_matches('\u{b0}')
        .parse::<i32>()
        .ok()
        .and_then(Rotation::from_degrees)
        .ok_or_else(|| format!("'{s}' is not 0, 90, 180 or 270"))
}

#[derive(Parser)]
#[command(
    name = "datfix",
    version,
    about = "Repair interrupted Canon .DAT recordings into playable MP4 files"
)]
struct Cli {
    /// Print newline-delimited JSON events instead of text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize a .DAT: streams, sample counts, duration estimate, truncation.
    Inspect {
        /// The .DAT file.
        dat: PathBuf,
    },
    /// Rebuild a playable MP4 from a .DAT. Without --reference or --reference-dir, the
    /// clips in the .DAT's own folder are tried first, then the built-in profiles.
    Repair {
        /// The .DAT file. It is only read, never changed.
        dat: PathBuf,
        /// A healthy MP4 recorded on the same camera. Always wins over the built-in profiles.
        /// If only its video matches, its video track is used and the other tracks are
        /// synthesized.
        #[arg(long, conflicts_with_all = ["reference_dir", "builtin"])]
        reference: Option<PathBuf>,
        /// A folder searched (3 levels deep) for a compatible camera-original MP4. Re-encodes
        /// are skipped. Falls back to the built-in profiles when nothing fits.
        #[arg(long, conflicts_with = "builtin")]
        reference_dir: Option<PathBuf>,
        /// Use the built-in profiles only; do not look for clips next to the .DAT.
        #[arg(long)]
        builtin: bool,
        /// Output file. Default: <DAT name>_repaired.MP4 in the current folder.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Replace the output if it already exists.
        #[arg(long)]
        force: bool,
        /// Stop at the first read error. By default unreadable parts of a failing card are
        /// skipped and treated as damage, and the report says how many seconds were lost.
        #[arg(long)]
        stop_on_read_error: bool,
        /// Copy the reference clip's own metadata into the output (its thumbnail, camera and
        /// lens serial numbers, firmware, owner and dates). By default it is left out, because
        /// it describes the reference clip, not this recording.
        #[arg(long)]
        keep_reference_metadata: bool,
        /// Turn the picture clockwise for display: 0, 90, 180 or 270 degrees. Use it for
        /// vertical recordings. Default: the reference clip's orientation (upright for the
        /// built-in settings).
        #[arg(long, value_name = "DEGREES", value_parser = parse_rotation)]
        rotate: Option<Rotation>,
    },
    /// Find recordings on a card image or device: 0-byte, deleted or missing .DAT files whose
    /// data is still on the card. Reads only; image the card first (see `docs/format/card-carving.md`).
    Scan(scan::ScanArgs),
    /// Find hidden temporary files left by repairs that were killed, and delete them with --yes.
    Clean {
        /// The folder the repaired files were saved in. Default: the current folder.
        dir: Option<PathBuf>,
        /// Delete the files found (otherwise only list them).
        #[arg(long)]
        yes: bool,
    },
    /// List the built-in decoder profiles, or rebuild them from camera originals.
    Profiles {
        #[command(subcommand)]
        action: Option<ProfilesAction>,
    },
}

#[derive(Subcommand)]
enum ProfilesAction {
    /// List the built-in profiles (the default).
    List,
    /// Build a profile bank from healthy camera MP4s (searched recursively). Keeps only codec
    /// settings and track templates; no media, thumbnails or location data.
    Harvest {
        /// Files or folders of camera-original MP4s.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where to write the bank JSON.
        #[arg(short, long)]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match cli.command {
        Command::Inspect { dat } => run_inspect(&dat, json),
        Command::Profiles { action } => match action.unwrap_or(ProfilesAction::List) {
            ProfilesAction::List => run_profiles_list(json),
            ProfilesAction::Harvest { paths, out } => run_harvest(&paths, &out, json),
        },
        Command::Scan(args) => scan::run(&args, json),
        Command::Clean { dir, yes } => {
            run_clean(&dir.unwrap_or_else(|| PathBuf::from(".")), yes, json)
        }
        Command::Repair {
            dat,
            reference,
            reference_dir,
            builtin,
            output,
            force,
            stop_on_read_error,
            keep_reference_metadata,
            rotate,
        } => {
            let source = match (reference, reference_dir) {
                (Some(r), _) => ReferenceSource::File(r),
                (None, Some(d)) => ReferenceSource::Dir(d),
                (None, None) if builtin => ReferenceSource::Builtin,
                (None, None) => ReferenceSource::Auto,
            };
            let output = output.unwrap_or_else(|| default_output(&dat));
            let opts = RepairOptions {
                overwrite: force,
                stop_on_read_error,
                keep_reference_metadata,
                rotation: rotate,
                ..Default::default()
            };
            run_repair(&dat, &source, &output, &opts, json)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let m = e.user_message();
            if json {
                emit_json(&serde_json::json!({
                    "event": "error", "kind": m.kind, "title": m.title, "message": m.message,
                    "next_step": m.next_step, "reasons": m.reasons, "detail": e.to_string(),
                }));
            } else {
                clear_progress_line();
                eprintln!("error: {}", m.title);
                eprintln!("  {}", m.message);
                for r in &m.reasons {
                    eprintln!("  - {r}");
                }
                eprintln!("  What to do: {}", m.next_step);
                let detail = e.to_string();
                if detail.trim() != m.message.trim() {
                    eprintln!();
                    eprintln!("details:");
                    for line in detail.lines() {
                        eprintln!("  {line}");
                    }
                }
            }
            ExitCode::FAILURE
        }
    }
}

fn run_profiles_list(json: bool) -> datfix_core::Result<()> {
    let bank = profile::bank();
    if json {
        let list: Vec<_> = bank
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id, "body": p.body, "body_code": p.body_code, "family": p.family,
                    "codec": p.codec, "width": p.width, "height": p.height,
                    "fps": p.frame_rate().fps(), "chroma": p.chroma, "bit_depth": p.bit_depth,
                    "long_gop": p.long_gop, "audio": p.audio, "sources": p.sources,
                })
            })
            .collect();
        emit_json(&serde_json::json!({ "event": "result", "profiles": list }));
        return Ok(());
    }
    println!(
        "{} built-in profiles (decoder settings only, no media):",
        bank.len()
    );
    for p in bank {
        println!("  {}", p.id);
        println!(
            "      {}  [body code {}, family {:?}, from {} clip(s)]",
            p.describe(),
            p.body_code.as_deref().unwrap_or("?"),
            p.family,
            p.sources
        );
    }
    Ok(())
}

fn run_harvest(paths: &[PathBuf], out: &Path, json: bool) -> datfix_core::Result<()> {
    let (profiles, rejected) = profile::harvest_paths(paths, &mut |path, r| {
        if json {
            return;
        }
        match r {
            Ok(id) => eprintln!("  {} -> {id}", path.display()),
            Err(e) => eprintln!("  skipped {}: {e}", path.display()),
        }
    })?;
    let bank = profile::Bank {
        version: 1,
        profiles,
    };
    let text = serde_json::to_string_pretty(&bank).expect("bank serializes");
    std::fs::write(out, text + "\n")
        .map_err(|e| datfix_core::Error::io(format!("cannot write {}", out.display()), e))?;
    if json {
        emit_json(&serde_json::json!({
            "event": "result", "profiles": bank.profiles.len(), "rejected": rejected.len()
        }));
    } else {
        println!(
            "wrote {} profile(s) to {} ({} file(s) skipped)",
            bank.profiles.len(),
            out.display(),
            rejected.len()
        );
    }
    Ok(())
}

/// `<stem>_repaired.MP4` in the current directory. Never next to the input, which may be on a
/// read-only card or share.
fn default_output(dat: &Path) -> PathBuf {
    let stem = dat
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!("{stem}_repaired.MP4"))
}

fn emit_json(value: &impl serde::Serialize) {
    let mut out = std::io::stdout().lock();
    // A closed pipe is not worth crashing over.
    let _ = serde_json::to_writer(&mut out, value);
    let _ = writeln!(out);
}

/// Returns an event printer for the chosen output mode.
fn event_printer(json: bool) -> impl FnMut(&Event) {
    let tty = std::io::stderr().is_terminal();
    move |ev: &Event| {
        if json {
            emit_json(ev);
            return;
        }
        match ev {
            Event::Phase { message, .. } => {
                clear_progress_line();
                eprintln!("{message}...");
            }
            Event::Progress { phase, done, total } if tty && *total > 0 => {
                let pct = *done as f64 * 100.0 / *total as f64;
                let unit = match phase {
                    Phase::AnalyzingVideo => format!("{done}/{total} frames"),
                    _ => format!("{:.1}/{:.1} GB", *done as f64 / 1e9, *total as f64 / 1e9),
                };
                eprint!("\r  {pct:5.1}%  {unit}   ");
                let _ = std::io::stderr().flush();
            }
            Event::Progress { .. } => {}
            Event::ReferenceRejected {
                path, user_reason, ..
            } => {
                clear_progress_line();
                eprintln!("  skipped {}: {user_reason}", path.display());
            }
            Event::ReferenceChosen { path } => {
                clear_progress_line();
                eprintln!("  using {}", path.display());
            }
            Event::Note { .. } => {} // notes are printed with the result
        }
    }
}

fn clear_progress_line() {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
    }
}

fn run_inspect(dat: &Path, json: bool) -> datfix_core::Result<()> {
    let report = inspect::inspect(dat, &mut event_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "result", "inspect": report }));
    } else {
        clear_progress_line();
        print_inspect(&report);
    }
    Ok(())
}

fn print_inspect(r: &InspectReport) {
    println!("{}", r.path.display());
    println!(
        "  size: {} bytes ({:.2} GB)",
        r.file_size,
        r.file_size as f64 / 1e9
    );
    println!(
        "  Canon .DAT header: {}",
        if r.has_canon_header {
            "yes (unfinished mdat)"
        } else {
            "no"
        }
    );
    if let Some(c) = &r.camera {
        println!("  camera: {c}");
    }
    if let Some(b) = &r.brand {
        println!("  brand: {b}");
    }
    if let Some(l) = &r.layout {
        println!("  recording: {}", l.describe());
    }
    if let Some(code) = &r.body_code {
        println!("  body code: {code}");
    }
    println!("  repi groups: {}", r.groups);
    let pattern: Vec<String> = r.pattern.iter().map(|k| k.to_string()).collect();
    println!("  first interval: {}", pattern.join(" "));
    for t in &r.types {
        let size = t
            .fixed_size
            .map_or("variable".to_string(), |s| format!("{s} B each"));
        println!(
            "  {:<5} groups {:>6}  samples {:>11}  bytes {:>14}  {:>6}/group  size {size}",
            t.kind, t.groups, t.samples, t.bytes, t.typical_group
        );
    }
    if r.gap_count > 0 {
        let common: Vec<String> = r
            .common_gaps
            .iter()
            .map(|(s, c)| format!("{s} B x{c}"))
            .collect();
        println!(
            "  unindexed bytes between groups: {} gaps, {} bytes (most common: {})",
            r.gap_count,
            r.gap_bytes,
            common.join(", ")
        );
    }
    if r.rejected_headers > 0 {
        println!("  damaged headers skipped: {}", r.rejected_headers);
    }
    match (r.estimated_seconds, r.estimated_fps) {
        (Some(s), Some(f)) => println!(
            "  estimated duration: {} ({} frames at {f:.3} fps)",
            hms(s),
            r.video_frames
        ),
        (Some(s), None) => println!("  estimated duration: {}", hms(s)),
        _ => println!(
            "  estimated duration: unknown ({} frames, frame rate unknown)",
            r.video_frames
        ),
    }
    match &r.truncation {
        Some(t) => println!(
            "  truncated: yes, at byte {} ({} incomplete {} sample(s), {} bytes missing)",
            t.offset,
            t.dropped_samples,
            t.kind.map_or("header".to_string(), |k| k.to_string()),
            t.missing_bytes
        ),
        None => println!("  truncated: no"),
    }
}

fn run_repair(
    dat: &Path,
    source: &ReferenceSource,
    output: &Path,
    opts: &RepairOptions,
    json: bool,
) -> datfix_core::Result<()> {
    let out_dir = match output.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let stale = repair::find_stale_partials(&out_dir, Duration::from_secs(60));
    if !stale.is_empty() && !json {
        let bytes: u64 = stale.iter().map(|p| p.bytes).sum();
        eprintln!(
            "note: {} unfinished file(s) from an earlier repair that was stopped ({:.2} GB) are in \
             {}. Delete them with: datfix clean \"{}\" --yes",
            stale.len(),
            bytes as f64 / 1e9,
            out_dir.display(),
            out_dir.display()
        );
    }
    let report = repair::repair(dat, source, output, opts, &mut event_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "result", "repair": report }));
    } else {
        clear_progress_line();
        print_repair(&report);
    }
    Ok(())
}

fn run_clean(dir: &Path, yes: bool, json: bool) -> datfix_core::Result<()> {
    let found = repair::find_stale_partials(dir, Duration::from_secs(60));
    let bytes: u64 = found.iter().map(|p| p.bytes).sum();
    let freed = if yes {
        let paths: Vec<PathBuf> = found.iter().map(|p| p.path.clone()).collect();
        Some(repair::remove_partials(&paths)?)
    } else {
        None
    };
    if json {
        emit_json(&serde_json::json!({
            "event": "result", "partials": found, "bytes": bytes, "freed": freed,
        }));
        return Ok(());
    }
    if found.is_empty() {
        println!("No unfinished repair files in {}.", dir.display());
        return Ok(());
    }
    for p in &found {
        println!(
            "  {}  {:.2} GB, untouched for {} min",
            p.path.display(),
            p.bytes as f64 / 1e9,
            p.idle_seconds / 60
        );
    }
    match freed {
        Some(n) => println!(
            "Deleted {} file(s), freed {:.2} GB.",
            found.len(),
            n as f64 / 1e9
        ),
        None => println!(
            "{} unfinished file(s), {:.2} GB. Run again with --yes to delete them.",
            found.len(),
            bytes as f64 / 1e9
        ),
    }
    Ok(())
}

fn print_repair(r: &RepairReport) {
    println!("Repaired: {}", r.output.display());
    println!("  {}", r.settings);
    for line in &r.rebuilt {
        println!("  {line}");
    }
    if let Some(c) = &r.camera {
        println!("  camera: {c}");
    }
    println!("  recording: {}", r.recording);
    let kind = match r.reference_kind {
        RefKind::User => "reference clip",
        RefKind::Hybrid => "reference clip's video + synthesized tracks",
        RefKind::Profile => "built-in profile",
    };
    println!("  decoder settings: {} ({kind})", r.reference.display());
    if !r.synthesized.is_empty() {
        println!("  synthesized: {}", r.synthesized.join(", "));
    }
    if let Some(t) = r.creation_time {
        println!(
            "  recorded: {} UTC, from {}",
            utc(t),
            r.creation_time_source
        );
    }
    if let Some(r) = r.rotation.filter(|r| r.degrees() != 0) {
        println!("  rotation: {r} clockwise");
    }
    println!(
        "  duration: {}  video frames: {}  keyframes: {}{}",
        hms(r.duration_seconds),
        r.video_frames,
        r.keyframes,
        if r.reordered { "  (Long GOP)" } else { "" }
    );
    for t in &r.tracks {
        let trim = if t.trimmed_samples > 0 {
            format!("  (trimmed {} to match video)", t.trimmed_samples)
        } else {
            String::new()
        };
        println!(
            "  track {} {} {:<4} from {:<4}: {} samples, {:.3} s{trim}",
            t.track_id, t.handler, t.codec, t.source, t.samples, t.duration_seconds
        );
    }
    println!("  size: {:.2} GB", r.bytes_written as f64 / 1e9);
    if r.unreadable_bytes > 0 {
        println!(
            "  unreadable: {:.1} MB in {} place(s) could not be read from the card; {:.2} s of \
             video lost (the picture holds, sound is silent there)",
            r.unreadable_bytes as f64 / 1e6,
            r.unreadable.len(),
            r.lost_seconds
        );
    } else if r.lost_seconds > 0.0 {
        println!("  damaged: {:.2} s of video lost", r.lost_seconds);
    }
    for n in &r.notes {
        println!("  note: {n}");
    }
}

/// Formats Unix seconds as `YYYY-MM-DD hh:mm:ss`.
fn utc(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Formats seconds as h:mm:ss.mmm.
fn hms(s: f64) -> String {
    let ms = (s * 1000.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
