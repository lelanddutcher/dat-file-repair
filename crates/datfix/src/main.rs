//! `datfix`: repair interrupted Canon `.DAT` recordings into playable `.MP4` files.
//!
//! This binary is a thin wrapper around `datfix-core`. It parses arguments, prints progress,
//! and prints results either as friendly text or as newline-delimited JSON (`--json`), one
//! object per line, for GUIs and scripts.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use datfix_core::inspect::{self, InspectReport};
use datfix_core::repair::{self, Event, Phase, ReferenceSource, RepairOptions, RepairReport};

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
    /// Rebuild a playable MP4 from a .DAT and a healthy clip from the same camera.
    Repair {
        /// The .DAT file. It is only read, never changed.
        dat: PathBuf,
        /// A healthy MP4 recorded on the same camera with the same settings.
        #[arg(
            long,
            conflicts_with = "reference_dir",
            required_unless_present = "reference_dir"
        )]
        reference: Option<PathBuf>,
        /// A folder of healthy MP4s; the smallest compatible one is used.
        #[arg(long)]
        reference_dir: Option<PathBuf>,
        /// Output file. Default: <DAT name>_repaired.MP4 in the current folder.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Replace the output if it already exists.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match cli.command {
        Command::Inspect { dat } => run_inspect(&dat, json),
        Command::Repair {
            dat,
            reference,
            reference_dir,
            output,
            force,
        } => {
            let source = match (reference, reference_dir) {
                (Some(r), _) => ReferenceSource::File(r),
                (None, Some(d)) => ReferenceSource::Dir(d),
                (None, None) => unreachable!("clap requires one of them"),
            };
            let output = output.unwrap_or_else(|| default_output(&dat));
            run_repair(&dat, &source, &output, force, json)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json {
                emit_json(&serde_json::json!({ "event": "error", "message": e.to_string() }));
            } else {
                clear_progress_line();
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
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
            Event::ReferenceRejected { path, reason } => {
                clear_progress_line();
                eprintln!("  skipped {}: {reason}", path.display());
            }
            Event::ReferenceChosen { path } => {
                clear_progress_line();
                eprintln!("  using reference {}", path.display());
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
        (Some(s), Some(f)) => println!("  estimated duration: {} ({f:.3} fps)", hms(s)),
        (Some(s), None) => println!("  estimated duration: {}", hms(s)),
        _ => println!("  estimated duration: unknown (no indexed PCM audio; repair to find out)"),
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
    force: bool,
    json: bool,
) -> datfix_core::Result<()> {
    let opts = RepairOptions {
        overwrite: force,
        cancel: None,
    };
    let report = repair::repair(dat, source, output, &opts, &mut event_printer(json))?;
    if json {
        emit_json(&serde_json::json!({ "event": "result", "repair": report }));
    } else {
        clear_progress_line();
        print_repair(&report);
    }
    Ok(())
}

fn print_repair(r: &RepairReport) {
    println!("Repaired: {}", r.output.display());
    println!("  reference: {}", r.reference.display());
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
    for n in &r.notes {
        println!("  note: {n}");
    }
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
