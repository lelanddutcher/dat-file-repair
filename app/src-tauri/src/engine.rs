//! The only module that talks to the repair engine (`datfix-core`).
//!
//! The engine is changing while the app is built. Every call into it lives here, and this module
//! turns the engine's types into small, app-shaped types that serialize to JSON for the web page.
//! When the engine's API changes, this file is the one to fix.
//!
//! Nothing here knows about Tauri. That keeps it testable with plain `cargo test`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use datfix_core::Error as CoreError;
use datfix_core::inspect::{InspectReport, inspect_with};
use datfix_core::layout::{self, DatLayout};
use datfix_core::mp4::reference::Reference;
use datfix_core::read_at::FileSource;
use datfix_core::repair::{
    self, Checked, Event, Phase, RefKind, ReferenceSource, RepairOptions, SearchOptions, Verdict,
    check_reference, choose_profile, search_references,
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
/// A temporary file nobody wrote to for this long belongs to a repair that was killed.
const STALE_AFTER: Duration = Duration::from_secs(60);

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
    /// For example "Canon EOS R5 Mark II", or a family guess when the model is unknown.
    pub camera: String,
    /// True when `camera` is the exact model read from the file.
    pub camera_known: bool,
    /// What the file holds, for example "video, 4 audio channels, timecode".
    pub contents: String,
    /// Complete video frames divided by the frame rate.
    pub estimated_seconds: Option<f64>,
    /// Frame rate as people write it ("119.88"), when known.
    pub frame_rate: Option<String>,
    pub video_frames: u64,
    /// True when the last group of samples was cut off mid-write.
    pub cut_off: bool,
}

/// Where the decoder settings for one `.DAT` come from.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SettingsSource {
    /// A healthy clip whose every track fits.
    Clip,
    /// A healthy clip's video; sound or data tracks are rebuilt.
    ClipVideoOnly,
    /// DATfix's built-in camera settings.
    Builtin,
}

/// One clip a search looked at.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateView {
    pub path: PathBuf,
    pub file_name: String,
    /// "fits", "videoOnly" or "skipped".
    pub verdict: &'static str,
    /// Why it was skipped, in plain words.
    pub reason: Option<String>,
    /// The technical reason.
    pub detail: Option<String>,
}

/// The settings chosen for one `.DAT`, or why none fit.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceMatch {
    pub ok: bool,
    pub source: SettingsSource,
    /// The clip, for `Clip` and `ClipVideoOnly` (and the clip that did not fit, when `!ok`).
    pub path: Option<PathBuf>,
    /// One plain sentence, for example "Using built-in settings for Canon EOS R5 Mark II, 4K
    /// 119.88p."
    pub summary: String,
    /// Why no clip was used, when the built-in settings were picked automatically.
    pub search_note: Option<String>,
    /// Tracks rebuilt instead of copied, one plain sentence each.
    pub rebuilt: Vec<String>,
    /// Why the clip does not fit, in plain words. Empty when `ok`.
    pub reasons: Vec<String>,
    /// Technical text for "Details".
    pub details: String,
    /// Clips checked by a search (0 for a single clip or the built-in settings).
    pub checked: usize,
    pub candidates: Vec<CandidateView>,
}

/// What a finished repair produced.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOutcome {
    pub output: PathBuf,
    /// Where the settings came from, one plain sentence.
    pub settings: String,
    pub rebuilt: Vec<String>,
    pub camera: Option<String>,
    pub duration_seconds: f64,
    pub video_frames: u64,
    pub audio_tracks: usize,
    pub bytes_written: u64,
    pub notes: Vec<String>,
}

/// A temporary file left by a repair that was killed.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialView {
    pub path: PathBuf,
    pub bytes: u64,
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
    /// A clip was checked and does not fit (folder search). `reason` is plain words.
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
    pub fn new(kind: &'static str, title: &str, message: String, next_step: &str) -> Self {
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
    Capabilities {
        reference_free: true,
    }
}

/// Reads the `.DAT`'s recovery index and summarizes it. Also returns the index so later steps
/// (reference checks, the repair itself) do not have to walk a 40 GB file again.
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
    let opts = RepairOptions {
        cancel: Some(cancel),
        ..Default::default()
    };
    let (report, index) =
        inspect_with(dat, &opts, &mut |ev| forward(ev, on_progress)).map_err(map_error)?;
    Ok((describe(dat, &report), Arc::new(index)))
}

/// Checks one clip the person chose against an indexed `.DAT`. The clip always wins: when it
/// does not fit, the result says why and nothing else is tried.
pub fn check_reference_file(
    dat: &Path,
    index: &DatIndex,
    clip: &Path,
) -> AppResult<ReferenceMatch> {
    let (src, lay) = open_with_layout(dat, index)?;
    let checked =
        Reference::open(clip).and_then(|r| check_reference(&src, index, &lay, r, RefKind::User));
    Ok(match checked {
        Ok(c) => matched(c, None, 0, Vec::new()),
        Err(e) => {
            let m = e.user_message();
            ReferenceMatch {
                ok: false,
                source: SettingsSource::Clip,
                path: Some(clip.to_path_buf()),
                summary: format!("{} doesn't match this file.", file_name(clip)),
                search_note: None,
                rebuilt: Vec::new(),
                reasons: if m.reasons.is_empty() {
                    vec![m.message]
                } else {
                    m.reasons
                },
                details: e.to_string(),
                checked: 1,
                candidates: Vec::new(),
            }
        }
    })
}

/// Picks DATfix's built-in settings for the `.DAT`.
pub fn check_builtin(dat: &Path, index: &DatIndex) -> AppResult<ReferenceMatch> {
    let (src, lay) = open_with_layout(dat, index)?;
    let c = choose_profile(&src, index, &lay, &mut |_| {}).map_err(map_error)?;
    Ok(matched(c, None, 0, Vec::new()))
}

/// Folders to search automatically for a reference: the `.DAT`'s own folder, or the whole card
/// when the `.DAT` sits on a camera card (a folder tree with `DCIM`, `XFVC` and so on).
pub fn auto_search_dirs(dat: &Path) -> (Vec<PathBuf>, bool) {
    let Some(parent) = dat.parent() else {
        return (Vec::new(), false);
    };
    // A camera card has a DCIM (or XFVC, PRIVATE, CONTENTS) folder at its top. A plain drive
    // root does not count: searching a whole external disk would take far too long.
    for dir in parent.ancestors().take(4) {
        if CARD_MARKERS.iter().any(|m| dir.join(m).is_dir()) {
            return (vec![dir.to_path_buf()], true);
        }
    }
    (vec![parent.to_path_buf()], false)
}

/// Searches folders for a clip that fits the `.DAT`. When none fits, falls back to the
/// built-in settings and says so.
pub fn find_reference(
    dat: &Path,
    index: &DatIndex,
    dirs: &[PathBuf],
    recursive: bool,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> AppResult<ReferenceMatch> {
    let (src, lay) = open_with_layout(dat, index)?;
    let opts = SearchOptions {
        depth: if recursive {
            MAX_SEARCH_DEPTH as u32
        } else {
            0
        },
        max_candidates: MAX_CANDIDATES,
        cancel: Some(cancel),
    };
    let found = search_references(&src, index, &lay, dirs, &opts, &mut |ev| {
        forward(ev, on_progress)
    })
    .map_err(map_error)?;
    let candidates: Vec<CandidateView> = found.candidates.iter().map(candidate_view).collect();
    let checked = found.checked();
    if let Some(c) = found.chosen {
        return Ok(matched(c, None, checked, candidates));
    }

    // Folder names only; the full path is in the tooltip of the clip list.
    let where_ = dirs
        .iter()
        .map(|d| format!("the folder “{}”", file_name(d)))
        .collect::<Vec<_>>()
        .join(", ");
    let note = if checked == 0 {
        format!("No clips were found in {where_}.")
    } else {
        format!(
            "None of the {checked} clip{} in {where_} matches this file.",
            if checked == 1 { "" } else { "s" }
        )
    };
    match choose_profile(&src, index, &lay, &mut |_| {}) {
        Ok(c) => Ok(matched(c, Some(note), checked, candidates)),
        Err(e) => {
            let mut err = map_error(e);
            err.message = format!("{note} {}", err.message);
            err.reasons = candidates
                .iter()
                .filter_map(|c| c.reason.as_ref().map(|r| format!("{}: {r}", c.file_name)))
                .take(8)
                .collect();
            err.details = format!(
                "{}\n{}",
                err.details,
                candidates
                    .iter()
                    .map(|c| format!(
                        "{}: {}",
                        c.path.display(),
                        c.detail.as_deref().unwrap_or(c.verdict)
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            Err(err)
        }
    }
}

/// Temporary files of killed repairs in the given folders.
pub fn find_partials(dirs: &[PathBuf]) -> Vec<PartialView> {
    let mut out: Vec<PartialView> = Vec::new();
    for d in dirs {
        for p in repair::find_stale_partials(d, STALE_AFTER) {
            if !out.iter().any(|o| o.path == p.path) {
                out.push(PartialView {
                    path: p.path,
                    bytes: p.bytes,
                });
            }
        }
    }
    out
}

/// Deletes temporary files found by [`find_partials`]. Returns the bytes freed.
pub fn remove_partials(paths: &[PathBuf]) -> AppResult<u64> {
    repair::remove_partials(paths).map_err(map_error)
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
/// `reference` is the clip to use, or `None` for the built-in settings. `index` is the index
/// read when the file was added, so a 40 GB file is not walked twice. `cancel` stops the job
/// within about one 8 MB block; the engine deletes its temporary file when it stops.
pub fn repair(
    dat: &Path,
    reference: Option<&Path>,
    output: &Path,
    index: Option<Arc<DatIndex>>,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(Progress),
) -> AppResult<RepairOutcome> {
    preflight(dat, output)?;
    let opts = RepairOptions {
        // Never replace an existing file. `suggest_output` picks a free name.
        overwrite: false,
        cancel: Some(cancel),
        index,
        // Skip unreadable parts of a failing card (the engine default).
        ..Default::default()
    };
    let source = match reference {
        Some(r) => ReferenceSource::File(r.to_path_buf()),
        None => ReferenceSource::Builtin,
    };
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
        settings: report.settings,
        rebuilt: report.rebuilt,
        camera: report.camera,
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

/// Opens the `.DAT` and works out its layout (camera, frame rate) from a cached index.
fn open_with_layout(dat: &Path, index: &DatIndex) -> AppResult<(FileSource, DatLayout)> {
    let src = open_dat(dat)?;
    let name = dat.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lay = layout::analyze(&src, index, name);
    Ok((src, lay))
}

/// Turns a chosen reference into the page's shape.
fn matched(
    c: Checked,
    search_note: Option<String>,
    checked: usize,
    candidates: Vec<CandidateView>,
) -> ReferenceMatch {
    let source = match c.kind {
        RefKind::User => SettingsSource::Clip,
        RefKind::Hybrid => SettingsSource::ClipVideoOnly,
        RefKind::Profile => SettingsSource::Builtin,
    };
    ReferenceMatch {
        ok: true,
        source,
        path: (source != SettingsSource::Builtin).then(|| c.reference.path.clone()),
        summary: c.summary,
        search_note,
        rebuilt: c.rebuilt,
        reasons: Vec::new(),
        details: c.notes.join("\n"),
        checked,
        candidates,
    }
}

fn candidate_view(c: &repair::Candidate) -> CandidateView {
    let (verdict, reason, detail) = match &c.verdict {
        Verdict::Fits => ("fits", None, None),
        Verdict::VideoOnly { rebuilt } => ("videoOnly", None, Some(rebuilt.join(" "))),
        Verdict::Skipped { reason } => (
            "skipped",
            Some(reason.user.clone()),
            Some(reason.detail.clone()),
        ),
    };
    CandidateView {
        path: c.path.clone(),
        file_name: file_name(&c.path),
        verdict,
        reason,
        detail,
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
        Event::ReferenceRejected {
            path, user_reason, ..
        } => Progress::Rejected {
            path: path.clone(),
            reason: user_reason.clone(),
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

/// Describes the camera and contents in plain words (see `docs/format/canon-dat-format.md`,
/// section 4, for the family guesses used when the model is unknown).
fn describe(dat: &Path, r: &InspectReport) -> DatSummary {
    let count = |k: &[u8; 4]| r.pattern.iter().filter(|f| f.0 == *k).count();
    let has = |k: &[u8; 4]| r.types.iter().any(|t| t.kind.0 == *k);
    let lpcm = count(b"lpcm");
    let guess = if has(b"tdmt") {
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

    DatSummary {
        path: dat.to_path_buf(),
        file_name: file_name(dat),
        size_bytes: r.file_size,
        camera: r.camera.clone().unwrap_or_else(|| guess.into()),
        camera_known: r.camera.is_some(),
        contents: parts.join(", "),
        estimated_seconds: r.estimated_seconds,
        frame_rate: r.estimated_fps.map(layout::fps_text),
        video_frames: r.video_frames,
        cut_off: r.truncation.is_some(),
    }
}

/// Maps an engine error to a friendly one. The words come from the engine
/// ([`CoreError::user_message`]), so the CLI and the app say the same thing; the technical
/// text goes to `details`.
pub fn map_error(e: CoreError) -> AppError {
    if matches!(e, CoreError::Cancelled) {
        return AppError::cancelled();
    }
    let m = e.user_message();
    AppError {
        kind: m.kind,
        title: m.title,
        message: m.message,
        next_step: m.next_step,
        reasons: m.reasons,
        details: e.to_string(),
    }
}

fn io_error(path: &Path, verb: &str, source: std::io::Error) -> AppError {
    map_error(CoreError::io(
        format!("cannot {verb} {}", path.display()),
        source,
    ))
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
        assert!(!err.details.is_empty());
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
