//! The only module that talks to the repair engine (`datfix-core`).
//!
//! The engine is changing while the app is built. Every call into it lives here, and this module
//! turns the engine's types into small, app-shaped types that serialize to JSON for the web page.
//! When the engine's API changes, this file is the one to fix.
//!
//! Nothing here knows about Tauri. That keeps it testable with plain `cargo test`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use datfix_core::Error as CoreError;
use datfix_core::inspect::{InspectReport, summarize};
use datfix_core::mp4::reference::Reference;
use datfix_core::read_at::FileSource;
use datfix_core::repair::{
    self, Event, Phase, ReferenceSource, RepairOptions, check_reference, index_dat,
};
// Re-exported so `commands.rs` can cache indexes without importing the engine itself.
pub use datfix_core::repi::DatIndex;
use serde::Serialize;

/// How many candidate clips a folder search checks before it gives up.
const MAX_CANDIDATES: usize = 60;
/// How deep a folder search goes below the folder the user picked.
const MAX_SEARCH_DEPTH: usize = 6;
/// Folder names that mark the top of a camera card.
const CARD_MARKERS: [&str; 4] = ["DCIM", "XFVC", "PRIVATE", "CONTENTS"];

// ---------------------------------------------------------------------------------------------
// Types sent to the web page. `rename_all = "camelCase"` matches JavaScript naming.
// ---------------------------------------------------------------------------------------------

/// What the engine can do. The page hides or disables buttons for missing features.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Repair with a built-in profile instead of a reference clip.
    pub reference_free: bool,
}

/// Plain-words summary of one `.DAT`, shown right after the user adds it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatSummary {
    pub path: PathBuf,
    pub file_name: String,
    pub size_bytes: u64,
    /// For example "Canon XF-HEVC S camera (EOS R5 Mark II or C80)".
    pub camera: String,
    /// What the file holds, for example "video, 4 audio channels, timecode".
    pub contents: String,
    /// Length that can be recovered, when the engine can estimate it before repair.
    pub estimated_seconds: Option<f64>,
    pub video_frames: u64,
    /// True when the last group of samples was cut off mid-write.
    pub cut_off: bool,
}

/// Result of checking one reference clip against one `.DAT`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceMatch {
    pub path: PathBuf,
    pub ok: bool,
    /// Why the clip does not fit, in plain words. Empty when `ok`.
    pub reasons: Vec<String>,
    /// How many clips were checked to find this one (1 for a single file).
    pub checked: usize,
}

/// What a finished repair produced.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOutcome {
    pub output: PathBuf,
    pub reference: PathBuf,
    pub duration_seconds: f64,
    pub video_frames: u64,
    pub audio_tracks: usize,
    pub bytes_written: u64,
    pub notes: Vec<String>,
}

/// A progress update, sent to the page while a long job runs.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Progress {
    /// A new stage started. `label` is ready to show.
    #[serde(rename_all = "camelCase")]
    Stage { stage: Stage, label: String },
    /// Progress inside the current stage. `unit` is "bytes" or "frames".
    #[serde(rename_all = "camelCase")]
    Step {
        stage: Stage,
        done: u64,
        total: u64,
        unit: &'static str,
    },
    /// A clip was checked and does not fit (folder search).
    #[serde(rename_all = "camelCase")]
    Rejected { path: PathBuf, reason: String },
    /// A non-fatal observation.
    #[serde(rename_all = "camelCase")]
    Note { message: String },
}

/// The stages of a repair, in order.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Indexing,
    CheckingReference,
    AnalyzingVideo,
    Writing,
}

/// An error the page can show as-is: a title, an explanation, and the next step.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    /// Stable code for the page, for example "empty_dat" or "cancelled".
    pub kind: &'static str,
    pub title: String,
    pub message: String,
    pub next_step: String,
    /// Specific reasons, for example why a reference does not match.
    pub reasons: Vec<String>,
    /// Full technical text for "Copy details".
    pub details: String,
}

impl AppError {
    fn new(kind: &'static str, title: &str, message: String, next_step: &str) -> Self {
        AppError {
            kind,
            title: title.into(),
            details: message.clone(),
            message,
            next_step: next_step.into(),
            reasons: Vec::new(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == "cancelled"
    }

    /// The error for a stopped job.
    pub fn cancelled() -> Self {
        AppError::new(
            "cancelled",
            "Stopped",
            "The repair was stopped. No partial file was left behind.".into(),
            "Press Repair to start again.",
        )
    }
}

pub type AppResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------------------------
// Engine calls
// ---------------------------------------------------------------------------------------------

/// Features of the engine this build links against.
pub fn capabilities() -> Capabilities {
    // The current engine always needs a reference clip. When the engine gains a built-in
    // profile bank, set this to true and fill in `ReferenceChoice::Builtin` in `repair`.
    Capabilities {
        reference_free: false,
    }
}

/// Reads the `.DAT`'s recovery index and summarizes it. Also returns the index so later steps
/// (reference checks) do not have to walk a 40 GB file again.
pub fn inspect(
    dat: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(Progress),
) -> AppResult<(DatSummary, Arc<DatIndex>)> {
    let meta = std::fs::metadata(dat).map_err(|e| io_error(dat, "read", e))?;
    if meta.is_dir() {
        return Err(AppError::new(
            "not_a_file",
            "This is a folder",
            format!("{} is a folder, not a .DAT file.", dat.display()),
            "Drop the .DAT file itself, or drop the whole card folder.",
        ));
    }
    if meta.len() == 0 {
        return Err(empty_dat(dat));
    }
    let src = open_dat(dat)?;
    let opts = RepairOptions {
        overwrite: false,
        cancel: Some(cancel),
    };
    let index = index_dat(&src, &opts, &mut |ev| forward(ev, on_progress)).map_err(map_error)?;
    let report = summarize(dat, &index);
    if index.groups.is_empty() {
        return Err(not_a_dat(dat, "no Canon recovery index (repi) was found"));
    }
    Ok((describe(dat, &report), Arc::new(index)))
}

/// Checks one reference clip against an indexed `.DAT`.
pub fn check_reference_file(
    dat: &Path,
    index: &DatIndex,
    clip: &Path,
) -> AppResult<ReferenceMatch> {
    let src = open_dat(dat)?;
    Ok(check_one(&src, index, clip))
}

/// Folders to search automatically for a reference: the `.DAT`'s own folder, or the whole card
/// when the `.DAT` sits on a camera card (a folder tree with `DCIM`, `XFVC` and so on).
pub fn auto_search_dirs(dat: &Path) -> (Vec<PathBuf>, bool) {
    let Some(parent) = dat.parent() else {
        return (Vec::new(), false);
    };
    for dir in parent.ancestors().take(4) {
        // A mounted card: `/Volumes/<name>` on macOS, a drive root such as `E:\` on Windows.
        let is_volume_root = dir.parent().is_some_and(|p| p == Path::new("/Volumes"))
            || (cfg!(windows) && dir.parent().is_none());
        let has_marker = CARD_MARKERS.iter().any(|m| dir.join(m).is_dir());
        if has_marker || is_volume_root {
            return (vec![dir.to_path_buf()], true);
        }
    }
    (vec![parent.to_path_buf()], false)
}

/// Searches folders for a clip that fits the `.DAT`. Clips in the `.DAT`'s own folder are
/// tried first, then the smallest clips first (they are quickest to read).
pub fn find_reference(
    dat: &Path,
    index: &DatIndex,
    dirs: &[PathBuf],
    recursive: bool,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> AppResult<ReferenceMatch> {
    let src = open_dat(dat)?;
    let home = dat.parent().map(Path::to_path_buf);
    let mut candidates = Vec::new();
    for d in dirs {
        collect_mp4s(
            d,
            if recursive { MAX_SEARCH_DEPTH } else { 0 },
            &mut candidates,
        );
    }
    candidates.sort_by_key(|(path, size)| (path.parent() != home.as_deref(), *size));
    candidates.dedup();

    let where_ = dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if candidates.is_empty() {
        return Err(AppError::new(
            "no_reference_found",
            "No healthy clips found",
            format!("There are no .MP4 clips in {where_}."),
            "Choose a clip you recorded on the same camera, or search another folder.",
        ));
    }

    let mut rejected = Vec::new();
    for (i, (clip, _)) in candidates.iter().take(MAX_CANDIDATES).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::cancelled());
        }
        let m = check_one(&src, index, clip);
        if m.ok {
            return Ok(ReferenceMatch {
                checked: i + 1,
                ..m
            });
        }
        let reason = m.reasons.join("; ");
        on_progress(Progress::Rejected {
            path: clip.clone(),
            reason: reason.clone(),
        });
        rejected.push(format!("{}: {reason}", file_name(clip)));
    }

    let mut err = AppError::new(
        "no_reference_found",
        "No matching clip found",
        format!(
            "None of the {} clips checked in {where_} was recorded with the same settings as this .DAT.",
            rejected.len()
        ),
        "Choose a clip recorded on the same camera with the same resolution, frame rate and audio settings.",
    );
    err.details = format!("{}\n{}", err.message, rejected.join("\n"));
    rejected.truncate(8);
    err.reasons = rejected;
    Err(err)
}

/// Picks an output path that does not exist yet: `<dir>/<stem>_repaired.mp4`, then
/// `<stem>_repaired (2).mp4` and so on. `dir` defaults to the `.DAT`'s folder. `taken` holds
/// paths already promised to other files in the same batch.
pub fn suggest_output(dat: &Path, dir: Option<&Path>, taken: &[PathBuf]) -> PathBuf {
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| dat.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let stem = dat
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    (1..)
        .map(|n| {
            let name = if n == 1 {
                format!("{stem}_repaired.mp4")
            } else {
                format!("{stem}_repaired ({n}).mp4")
            };
            dir.join(name)
        })
        .find(|p| !p.exists() && !taken.contains(p))
        .expect("an unused name exists")
}

/// Runs a full repair. Blocks until done; call it from a background thread.
///
/// `cancel` stops the job within about one 8 MB block. The engine deletes its temporary file
/// when it stops, so a cancelled repair leaves nothing behind.
pub fn repair(
    dat: &Path,
    reference: &Path,
    output: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(Progress),
) -> AppResult<RepairOutcome> {
    preflight(dat, output)?;
    let opts = RepairOptions {
        // Never replace an existing file. `suggest_output` picks a free name.
        overwrite: false,
        cancel: Some(cancel),
    };
    let source = ReferenceSource::File(reference.to_path_buf());
    let report = repair::repair(dat, &source, output, &opts, &mut |ev| {
        forward(ev, on_progress)
    })
    .map_err(map_error)?;
    let audio_tracks = report
        .tracks
        .iter()
        .filter(|t| t.handler.0 == *b"soun")
        .count();
    Ok(RepairOutcome {
        output: report.output,
        reference: report.reference,
        duration_seconds: report.duration_seconds,
        video_frames: report.video_frames,
        audio_tracks,
        bytes_written: report.bytes_written,
        notes: report.notes,
    })
}

/// Lists `.DAT` files and `.MP4` clips in dropped paths. Folders are searched (a whole card can
/// be dropped). Returns `(dats, mp4s)`.
pub fn expand_dropped(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dats = Vec::new();
    let mut mp4s = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, MAX_SEARCH_DEPTH, &mut |f| {
                sort_file(f, &mut dats, &mut mp4s)
            });
        } else {
            sort_file(p, &mut dats, &mut mp4s);
        }
    }
    dats.sort();
    dats.dedup();
    mp4s.sort();
    mp4s.dedup();
    (dats, mp4s)
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn open_dat(dat: &Path) -> AppResult<FileSource> {
    FileSource::open(dat).map_err(|e| io_error(dat, "read", e))
}

fn check_one(src: &FileSource, index: &DatIndex, clip: &Path) -> ReferenceMatch {
    let result = Reference::open(clip).and_then(|r| check_reference(src, index, r));
    let reasons = match result {
        Ok(_) => Vec::new(),
        Err(CoreError::Incompatible { reasons, .. }) => reasons,
        Err(CoreError::BadReference { reason, .. }) => vec![reason],
        Err(e) => vec![e.to_string()],
    };
    ReferenceMatch {
        path: clip.to_path_buf(),
        ok: reasons.is_empty(),
        reasons,
        checked: 1,
    }
}

/// Refuses outputs that would touch the `.DAT`, and checks for free space before a long write.
fn preflight(dat: &Path, output: &Path) -> AppResult<()> {
    let same_name = output == dat;
    let same_file = match (dat.canonicalize(), output.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same_name || same_file {
        return Err(map_error(CoreError::OutputIsInput));
    }
    let dir = output.parent().unwrap_or(Path::new("."));
    if !dir.is_dir() {
        return Err(AppError::new(
            "output_folder_missing",
            "The save folder is gone",
            format!("The folder {} does not exist.", dir.display()),
            "Choose another place to save the repaired video.",
        ));
    }
    let need = std::fs::metadata(dat).map(|m| m.len()).unwrap_or(0);
    // A few MB of headroom for the new header.
    if let Ok(free) = fs4::available_space(dir)
        && free < need + 64 * 1024 * 1024
    {
        return Err(AppError::new(
            "disk_full",
            "Not enough free space",
            format!(
                "The repaired video needs about {} but {} has only {} free.",
                gb(need),
                dir.display(),
                gb(free)
            ),
            "Free up space, or choose another drive to save to.",
        ));
    }
    Ok(())
}

/// Turns an engine event into an app progress update.
fn forward(ev: &Event, on_progress: &mut dyn FnMut(Progress)) {
    let p = match ev {
        Event::Phase { phase, .. } => Progress::Stage {
            stage: stage(*phase),
            label: stage_label(*phase).into(),
        },
        Event::Progress { phase, done, total } => Progress::Step {
            stage: stage(*phase),
            done: *done,
            total: *total,
            unit: if *phase == Phase::AnalyzingVideo {
                "frames"
            } else {
                "bytes"
            },
        },
        Event::ReferenceRejected { path, reason } => Progress::Rejected {
            path: path.clone(),
            reason: reason.clone(),
        },
        Event::ReferenceChosen { .. } => return,
        Event::Note { message } => Progress::Note {
            message: message.clone(),
        },
    };
    on_progress(p);
}

fn stage(p: Phase) -> Stage {
    match p {
        Phase::Indexing => Stage::Indexing,
        Phase::CheckingReference => Stage::CheckingReference,
        Phase::AnalyzingVideo => Stage::AnalyzingVideo,
        Phase::Writing => Stage::Writing,
    }
}

fn stage_label(p: Phase) -> &'static str {
    match p {
        Phase::Indexing => "Reading the recovery index",
        Phase::CheckingReference => "Checking the reference clip",
        Phase::AnalyzingVideo => "Reading the video frames",
        Phase::Writing => "Writing the repaired video",
    }
}

/// Describes the camera family and contents from the group pattern (see
/// `docs/format/canon-dat-format.md`, section 4).
fn describe(dat: &Path, r: &InspectReport) -> DatSummary {
    let count = |k: &[u8; 4]| r.pattern.iter().filter(|f| f.0 == *k).count();
    let has = |k: &[u8; 4]| r.types.iter().any(|t| t.kind.0 == *k);
    let lpcm = count(b"lpcm");
    let camera = if has(b"tdmt") {
        "Canon XF-HEVC S camera (such as EOS R5 Mark II or C80)"
    } else if has(b"aac ") {
        "Canon EOS camera with AAC audio (such as EOS R5, R6 or C70)"
    } else if lpcm > 0 {
        "Canon EOS camera with PCM audio (such as EOS R3, R5 or R6 Mark II)"
    } else if has(b"vide") {
        "Canon EOS camera, video without sound (such as EOS R3)"
    } else {
        "Canon camera (unknown model)"
    };

    let mut parts = Vec::new();
    if has(b"vide") {
        parts.push("video".to_string());
    }
    match lpcm {
        0 if has(b"aac ") => parts.push("sound".into()),
        0 => {}
        1 => parts.push("stereo sound".into()),
        n => parts.push(format!("{n} audio channels")),
    }
    if has(b"tmcd") {
        parts.push("timecode".into());
    }
    let video_frames = r
        .types
        .iter()
        .find(|t| t.kind.0 == *b"vide")
        .map_or(0, |t| t.samples);

    DatSummary {
        path: dat.to_path_buf(),
        file_name: file_name(dat),
        size_bytes: r.file_size,
        camera: camera.into(),
        contents: parts.join(", "),
        // Frames divided by frame rate is exact; the audio-based estimate runs a little long
        // because audio is recorded slightly ahead of the last complete video frame.
        estimated_seconds: match r.estimated_fps {
            Some(fps) if fps > 0.0 && video_frames > 0 => Some(video_frames as f64 / fps),
            _ => r.estimated_seconds,
        },
        video_frames,
        cut_off: r.truncation.is_some(),
    }
}

/// Maps an engine error to a friendly one.
///
/// Every engine variant is listed on purpose: when the engine adds one, this stops compiling
/// and points here.
pub fn map_error(e: CoreError) -> AppError {
    let details = e.to_string();
    let mut err = match e {
        CoreError::Io { context, source } => return io_error_ctx(&context, source),
        CoreError::NotADat(msg) => AppError::new(
            "not_a_dat",
            "This file cannot be repaired",
            format!("This does not look like a Canon recording: {msg}."),
            "Check that it is the .DAT file from a Canon camera card. Older Canon bodies (such as the EOS R or M50 Mark II) do not write the index DATfix needs.",
        ),
        CoreError::BadReference { path, reason } => AppError::new(
            "bad_reference",
            "The healthy clip cannot be read",
            format!("{} cannot be used: {reason}.", file_name(&path)),
            "Choose a clip that plays normally, recorded on the same camera.",
        ),
        CoreError::Incompatible { path, reasons } => {
            let mut e = AppError::new(
                "incompatible_reference",
                "The healthy clip does not match",
                format!(
                    "{} was recorded with different settings than this .DAT.",
                    file_name(&path)
                ),
                "Choose a clip recorded on the same camera with the same resolution, frame rate, codec and audio settings.",
            );
            e.reasons = reasons;
            e
        }
        CoreError::NoReference { dir, tried } => {
            let mut e = AppError::new(
                "no_reference_found",
                "No matching clip found",
                format!(
                    "No clip in {} was recorded with the same settings.",
                    dir.display()
                ),
                "Choose a clip recorded on the same camera with the same settings.",
            );
            e.reasons = tried
                .into_iter()
                .take(8)
                .map(|(p, r)| format!("{}: {r}", file_name(&p)))
                .collect();
            e
        }
        CoreError::OutputExists(path) => AppError::new(
            "output_exists",
            "A file with that name already exists",
            format!(
                "{} already exists. DATfix never replaces files.",
                path.display()
            ),
            "Choose another name or folder for the repaired video.",
        ),
        CoreError::OutputIsInput => AppError::new(
            "output_is_input",
            "That would overwrite the original",
            "The save location is the .DAT file itself. The original is never changed.".into(),
            "Choose another name for the repaired video.",
        ),
        CoreError::Cancelled => AppError::cancelled(),
    };
    err.details = details;
    err
}

fn io_error(path: &Path, verb: &str, source: std::io::Error) -> AppError {
    io_error_ctx(&format!("cannot {verb} {}", path.display()), source)
}

/// Explains common disk and card errors in plain words.
fn io_error_ctx(context: &str, source: std::io::Error) -> AppError {
    use std::io::ErrorKind as K;
    let details = format!("{context}: {source}");
    let (kind, title, message, next): (&'static str, &str, &str, &str) = match source.kind() {
        K::NotFound => (
            "not_found",
            "File not found",
            "A file is missing. The card or drive may have been removed.",
            "Reconnect the card or drive, then add the file again.",
        ),
        K::PermissionDenied | K::ReadOnlyFilesystem => (
            "permission_denied",
            "No permission",
            "DATfix is not allowed to read or write here. A locked card or a read-only folder causes this.",
            "Choose another folder to save to. On a Mac, check System Settings > Privacy & Security > Files and Folders.",
        ),
        K::StorageFull => (
            "disk_full",
            "The disk is full",
            "The drive ran out of space while writing. The partial file was removed.",
            "Free up space, or choose another drive to save to.",
        ),
        K::UnexpectedEof => (
            "read_error",
            "The file ended early",
            "The file is shorter than expected. It may still be copying.",
            "Wait until the copy is finished, then try again.",
        ),
        _ if source.raw_os_error() == Some(5) => (
            "read_error",
            "The card or disk reported a read error",
            "Part of the file could not be read from the card or disk.",
            "Copy the .DAT to your computer's internal drive first (a card recovery tool can help), then repair the copy.",
        ),
        _ => (
            "io",
            "A disk error stopped the repair",
            "The operating system reported an error while reading or writing.",
            "Check the card or drive is still connected and has free space, then try again.",
        ),
    };
    let mut e = AppError::new(kind, title, message.into(), next);
    e.details = details;
    e
}

fn empty_dat(dat: &Path) -> AppError {
    AppError::new(
        "empty_dat",
        "This .DAT file is empty (0 bytes)",
        format!(
            "{} contains no data. The camera saved the file's name, but the video itself was never recorded into it, so there is nothing for DATfix to rebuild.",
            file_name(dat)
        ),
        "The video may still be on the card. Stop using the card, make a full copy (an image) of it, and run a card recovery tool such as PhotoRec on the copy. Then repair the .DAT it finds.",
    )
}

fn not_a_dat(dat: &Path, why: &str) -> AppError {
    map_error(CoreError::NotADat(format!("{} has {why}", file_name(dat))))
}

/// Adds `.dat` files and `.mp4` clips; skips hidden files and our own outputs.
fn sort_file(p: &Path, dats: &mut Vec<PathBuf>, mp4s: &mut Vec<PathBuf>) {
    let name = file_name(p);
    if name.starts_with('.') {
        return;
    }
    match extension(p).as_str() {
        "dat" => dats.push(p.to_path_buf()),
        "mp4" if !name.contains("_repaired") => mp4s.push(p.to_path_buf()),
        _ => {}
    }
}

/// Collects `(path, size)` of usable clips under `dir`.
fn collect_mp4s(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    walk(dir, depth, &mut |f| {
        let name = file_name(f);
        if extension(f) == "mp4" && !name.starts_with('.') && !name.contains("_repaired") {
            let size = std::fs::metadata(f).map_or(u64::MAX, |m| m.len());
            out.push((f.to_path_buf(), size));
        }
    });
}

/// Calls `visit` for every file under `dir`, `depth` folders deep. Skips hidden folders.
fn walk(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if depth > 0 && !file_name(&p).starts_with('.') {
                walk(&p, depth - 1, visit);
            }
        } else {
            visit(&p);
        }
    }
}

fn extension(p: &Path) -> String {
    p.extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_output_never_reuses_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("A001.DAT");
        let first = suggest_output(&dat, None, &[]);
        assert_eq!(first, dir.path().join("A001_repaired.mp4"));
        std::fs::write(&first, b"x").unwrap();
        let second = suggest_output(&dat, None, &[]);
        assert_eq!(second, dir.path().join("A001_repaired (2).mp4"));
        let third = suggest_output(&dat, None, std::slice::from_ref(&second));
        assert_eq!(third, dir.path().join("A001_repaired (3).mp4"));
    }

    #[test]
    fn empty_dat_explains_card_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("EMPTY.DAT");
        std::fs::write(&dat, b"").unwrap();
        let err = inspect(&dat, Arc::default(), &mut |_| {}).unwrap_err();
        assert_eq!(err.kind, "empty_dat");
        assert!(err.next_step.contains("PhotoRec"));
    }

    #[test]
    fn expand_dropped_finds_dats_in_card_folders() {
        let dir = tempfile::tempdir().unwrap();
        let reel = dir.path().join("XFVC/REEL_0001");
        std::fs::create_dir_all(&reel).unwrap();
        std::fs::write(reel.join("A.DAT"), b"x").unwrap();
        std::fs::write(reel.join("B.MP4"), b"x").unwrap();
        std::fs::write(reel.join("._B.MP4"), b"x").unwrap();
        std::fs::write(reel.join("B_repaired.mp4"), b"x").unwrap();
        let (dats, mp4s) = expand_dropped(&[dir.path().to_path_buf()]);
        assert_eq!(dats, vec![reel.join("A.DAT")]);
        assert_eq!(mp4s, vec![reel.join("B.MP4")]);
        // A .DAT on a card is searched from the card's top folder, recursively.
        let (dirs, recursive) = auto_search_dirs(&reel.join("A.DAT"));
        assert_eq!(dirs, vec![dir.path().to_path_buf()]);
        assert!(recursive);
    }

    #[test]
    fn output_cannot_be_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("A.DAT");
        std::fs::write(&dat, b"x").unwrap();
        let err = preflight(&dat, &dat).unwrap_err();
        assert_eq!(err.kind, "output_is_input");
    }
}
