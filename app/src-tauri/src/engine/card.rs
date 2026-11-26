//! "Recover from a memory card": scan a card image (or the card itself) for recordings the file
//! system lost, copy them out, and make an image of a card.
//!
//! This is part of the engine layer: like `engine.rs`, it is the only place that calls
//! `datfix-core`'s `carve` module, and it turns the engine's findings into plain-words types for
//! the page. See `docs/format/card-carving.md` for how a scan works.
//!
//! Safety rules kept here:
//!
//! - The image or device is only ever opened with `ImageSource::open` (read-only) or read by
//!   `dd` (`if=` only). Nothing here opens it for writing.
//! - Every output is a new file. Names that exist get " (2)", " (3)" and so on.
//! - Nothing is saved onto the card that is being scanned or imaged.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use datfix_core::Error as CoreError;
use datfix_core::carve::{
    self, FinishedClip, Found, FsKind, ImageSource, Recovered, ScanEvent, ScanOptions, ScanPhase,
    ScanReport, StreamEnd,
};
use datfix_core::read_at::ReadAt;
use serde::Serialize;

use super::{AppError, AppResult, file_name, gb, map_error};

/// Finished clips copied from the card per recording, as references (same as the CLI).
const MAX_REFERENCES: usize = 3;
/// Folder names that mark a camera card.
const CARD_MARKERS: [&str; 4] = ["DCIM", "XFVC", "PRIVATE", "CONTENTS"];
/// Progress updates are sent at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// Imaging block size. A multiple of every sector size.
const IMAGE_BLOCK: usize = 4 << 20;

// ---------------------------------------------------------------------------------------------
// Types sent to the page
// ---------------------------------------------------------------------------------------------

/// Progress of a scan, a copy or an imaging job.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CardProgress {
    /// A new stage started. `label` is ready to show.
    #[serde(rename_all = "camelCase")]
    Stage { label: String },
    /// Bytes done in the current stage.
    #[serde(rename_all = "camelCase")]
    Step { done: u64, total: u64 },
    /// Something was found ("recording", "clip" or "fragment").
    #[serde(rename_all = "camelCase")]
    Found {
        what: &'static str,
        offset: u64,
        length: u64,
    },
}

/// A recording or headerless piece found on the card.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingView {
    /// Byte position on the card. Identifies the recording in later calls.
    pub offset: u64,
    pub size_bytes: u64,
    pub camera: Option<String>,
    pub duration_seconds: Option<f64>,
    pub video_frames: u64,
    /// "119.88" and so on.
    pub frame_rate: Option<String>,
    /// The name the card's file list gives it, when known.
    pub file_name: Option<String>,
    /// Folder and name on the card, when known.
    pub card_path: Option<String>,
    /// "deleted", "empty" (a 0-byte entry) or null.
    pub badge: Option<&'static str>,
    /// Number of separate places on the card it was joined from (1 = in one piece).
    pub pieces: usize,
    /// Why it may be incomplete, in plain words.
    pub may_be_incomplete: Option<String>,
    /// Part of it was overwritten by a later clip.
    pub overlaps_clip: bool,
    /// Finished clips on the card that may serve as references.
    pub references: usize,
    /// Technical text for "Details".
    pub details: String,
}

/// A finished camera clip found on the card.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipView {
    pub offset: u64,
    pub size_bytes: u64,
    pub camera: Option<String>,
    pub duration_seconds: Option<f64>,
    pub file_name: Option<String>,
    pub deleted: bool,
    /// False when the clip runs past the end of the image.
    pub complete: bool,
}

/// What a scan found.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardScanView {
    pub image: PathBuf,
    pub image_name: String,
    pub size_bytes: u64,
    pub is_device: bool,
    pub seconds: f64,
    /// For example "exFAT, 128 KB clusters, 12 files (3 deleted)". Null when none was found.
    pub file_system: Option<String>,
    pub recordings: Vec<RecordingView>,
    pub fragments: Vec<RecordingView>,
    pub clips: Vec<ClipView>,
    /// Plain notes for the person.
    pub notes: Vec<String>,
    /// Technical summary for "Details".
    pub details: String,
}

/// A recording copied off the card, ready for the normal repair flow.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedRecording {
    pub offset: u64,
    /// The new `.DAT` file, when the copy worked.
    pub dat: Option<PathBuf>,
    pub error: Option<AppError>,
}

/// Result of [`prepare_repair`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedCard {
    pub recordings: Vec<PreparedRecording>,
    /// Folder of finished clips copied from the card, used as references. Null when the card
    /// holds none that fit.
    pub clips_dir: Option<PathBuf>,
    pub clips: usize,
}

/// A memory card (or other removable disk) the computer can see.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardDevice {
    /// The card's name, for example "EOS_DIGITAL".
    pub name: String,
    pub mount_point: PathBuf,
    /// Whole-disk identifier, for example "disk4".
    pub device: String,
    /// Raw device path, for example "/dev/rdisk4".
    pub raw_device: PathBuf,
    pub size_bytes: u64,
    pub file_system: String,
    /// It has a DCIM, XFVC, PRIVATE or CONTENTS folder.
    pub looks_like_camera_card: bool,
    /// DATfix can read the whole device without an administrator password.
    pub readable: bool,
}

/// The engine's findings, kept by the app between the scan and "Repair all".
pub struct CardSession {
    pub image: PathBuf,
    src: ImageSource,
    report: ScanReport,
}

// ---------------------------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------------------------

/// Scans a card image or raw device. Blocks; call it from a background thread.
pub fn scan(
    image: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<(CardScanView, Arc<CardSession>)> {
    let src = open_image(image)?;
    if src.size() == 0 {
        return Err(AppError::new(
            "image_empty",
            "This file is empty",
            format!("{} holds no data.", image.display()),
            "Choose the card image itself (.img, .dmg or .dd), or make a new image of the card.",
        ));
    }
    let opts = ScanOptions {
        cancel: Some(cancel),
        stitch: true,
    };
    let mut forward = progress_forwarder(on_progress);
    // Test hook: DATFIX_DEV_SCAN_MBPS slows the scan down so progress and Cancel can be seen
    // and tested on a small image. Unset in normal use.
    let result = match dev_scan_rate() {
        Some(rate) => carve::scan(&Throttled::new(&src, rate), &opts, &mut forward),
        None => carve::scan(&src, &opts, &mut forward),
    };
    let mut report = result.map_err(|e| scan_error(e, "scan"))?;
    report.is_device = src.is_device;
    let view = describe(image, &report, &src);
    Ok((
        view,
        Arc::new(CardSession {
            image: image.to_path_buf(),
            src,
            report,
        }),
    ))
}

fn open_image(image: &Path) -> AppResult<ImageSource> {
    if image.is_dir() {
        return Err(AppError::new(
            "not_a_file",
            "This is a folder",
            format!("{} is a folder, not a card image.", image.display()),
            "Choose the image file of the card (.img, .dmg or .dd).",
        ));
    }
    ImageSource::open(image).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            let mut err = AppError::new(
                "card_needs_admin",
                "DATfix can't read the card directly",
                "Your computer only lets administrators read a whole memory card.".into(),
                "Choose “Make an image of the card” instead. It asks for your password once, \
                 makes a copy, and scans the copy.",
            );
            err.details = format!("cannot open {}: {e}", image.display());
            err
        } else {
            map_error(CoreError::io(format!("cannot open {}", image.display()), e))
        }
    })
}

/// Maps a scan or copy error; "stopped" gets words that fit a scan.
fn scan_error(e: CoreError, what: &str) -> AppError {
    let err = map_error(e);
    if err.is_cancelled() {
        let mut c = AppError::new(
            "cancelled",
            "Stopped",
            format!("The {what} was stopped. Nothing was changed on the card."),
            "Start again when you are ready.",
        );
        c.details = err.details;
        return c;
    }
    err
}

/// Turns engine scan events into page updates, sending byte progress at most every 100 ms.
fn progress_forwarder(on_progress: &mut dyn FnMut(CardProgress)) -> impl FnMut(&ScanEvent) + '_ {
    let mut last = Instant::now() - PROGRESS_EVERY;
    move |ev: &ScanEvent| match ev {
        ScanEvent::Phase { phase, .. } => on_progress(CardProgress::Stage {
            label: phase_label(*phase).into(),
        }),
        ScanEvent::Progress { done, total, .. } => {
            if done >= total || last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                on_progress(CardProgress::Step {
                    done: *done,
                    total: *total,
                });
            }
        }
        ScanEvent::Found {
            what,
            offset,
            length,
        } => on_progress(CardProgress::Found {
            what: match what {
                Found::Recording => "recording",
                Found::Clip => "clip",
                Found::Fragment => "fragment",
            },
            offset: *offset,
            length: *length,
        }),
        ScanEvent::Note { .. } => {}
    }
}

fn phase_label(p: ScanPhase) -> &'static str {
    match p {
        ScanPhase::ReadingFileSystem => "Reading the card's list of files",
        ScanPhase::Scanning => "Searching the whole card for recordings",
        ScanPhase::Following => "Putting the recordings together",
        ScanPhase::Extracting => "Copying from the card",
    }
}

fn describe(image: &Path, r: &ScanReport, src: &ImageSource) -> CardScanView {
    let recordings: Vec<RecordingView> = r
        .recordings
        .iter()
        .map(|rec| recording_view(r, rec))
        .collect();
    let fragments: Vec<RecordingView> = r
        .fragments
        .iter()
        .map(|rec| recording_view(r, rec))
        .collect();
    let clips = r.clips.iter().map(clip_view).collect();
    let file_system = r.volumes.first().map(|v| {
        format!(
            "{}, {} KB clusters, {} file{} ({} deleted)",
            match v.kind {
                FsKind::ExFat => "exFAT",
                FsKind::Fat32 => "FAT32",
            },
            v.cluster_size / 1024,
            v.files,
            if v.files == 1 { "" } else { "s" },
            v.deleted
        )
    });

    let mut notes = Vec::new();
    if r.volumes.is_empty() {
        if looks_compressed(src) {
            notes.push(
                "This looks like a compressed disk image. DATfix can only search uncompressed \
                 images. Make a new image as “DVD/CD master” or “read/write” in Disk Utility."
                    .to_string(),
            );
        } else {
            notes.push(
                "DATfix could not read the card's list of files, so it searched by content \
                 only. Recordings can still be found, but without their names."
                    .to_string(),
            );
        }
    }
    let mut details = vec![format!(
        "{}: {} bytes{}, scanned in {:.1} s, {} group headers",
        image.display(),
        r.size,
        if r.is_device { " (device)" } else { "" },
        r.seconds,
        r.headers_seen
    )];
    for v in &r.volumes {
        details.push(format!(
            "file system {:?} at byte {}, cluster {} bytes, heap at {}, {} files, {} deleted{}",
            v.kind,
            v.offset,
            v.cluster_size,
            v.heap_offset,
            v.files,
            v.deleted,
            if v.warnings.is_empty() {
                String::new()
            } else {
                format!("; {}", v.warnings.join("; "))
            }
        ));
    }
    for h in &r.dat_entries {
        details.push(format!(
            ".DAT entry {} ({} bytes{}) -> {}",
            h.path,
            h.size,
            if h.deleted { ", deleted" } else { "" },
            match (h.recording, h.data_offset) {
                (Some(o), _) => format!("recording at byte {o}"),
                (None, Some(o)) => format!("nothing found at byte {o}"),
                (None, None) => "no start cluster recorded".into(),
            }
        ));
    }
    details.extend(r.notes.iter().map(|n| format!("note: {n}")));

    CardScanView {
        image: image.to_path_buf(),
        image_name: file_name(image),
        size_bytes: r.size,
        is_device: r.is_device,
        seconds: r.seconds,
        file_system,
        recordings,
        fragments,
        clips,
        notes,
        details: details.join("\n"),
    }
}

fn recording_view(r: &ScanReport, rec: &Recovered) -> RecordingView {
    let badge = rec.file.as_ref().and_then(|f| {
        if f.deleted {
            Some("deleted")
        } else if f.size == 0 {
            Some("empty")
        } else {
            None
        }
    });
    let card_path = rec.file.as_ref().map(|f| f.path.clone());
    let end = match rec.end {
        StreamEnd::DatUnit => "ends at a 32 MiB unit, like the camera's .DAT",
        StreamEnd::LastGroup => "ends after the last complete group",
        StreamEnd::ImageEnd => "cut off by the end of the image",
        StreamEnd::FileSize => "ends at the size in the file entry",
    };
    let mut details = vec![
        format!(
            "byte {} on the card, {} bytes, {} groups, {} video frames",
            rec.offset, rec.length, rec.groups, rec.video_frames
        ),
        end.to_string(),
    ];
    if let Some(d) = &rec.recording {
        details.push(d.clone());
    }
    if rec.pieces.len() > 1 {
        details.push(format!(
            "joined from {} pieces: {}",
            rec.pieces.len(),
            rec.pieces
                .iter()
                .map(|p| format!("{}+{}", p.offset, p.length))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if rec.resyncs > 0 {
        details.push(format!(
            "{} group(s) found by a forward search",
            rec.resyncs
        ));
    }
    if !rec.overlaps_clips.is_empty() {
        details.push(format!(
            "overlaps finished clip(s) at byte {:?}",
            rec.overlaps_clips
        ));
    }
    if !rec.has_dat_header {
        details.push("headerless: no .DAT start leads here".into());
    }
    RecordingView {
        offset: rec.offset,
        size_bytes: rec.length,
        camera: rec.camera.clone(),
        duration_seconds: rec.duration_seconds,
        video_frames: rec.video_frames,
        frame_rate: rec.frame_rate.map(datfix_core::layout::fps_text),
        file_name: card_path
            .as_deref()
            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string()),
        card_path,
        badge,
        pieces: rec.pieces.len(),
        may_be_incomplete: rec.partial.clone(),
        overlaps_clip: !rec.overlaps_clips.is_empty(),
        references: carve::reference_candidates(r, rec, MAX_REFERENCES).len(),
        details: details.join("\n"),
    }
}

fn clip_view(c: &FinishedClip) -> ClipView {
    ClipView {
        offset: c.offset,
        size_bytes: c.length,
        camera: c.camera.clone(),
        duration_seconds: c.duration_seconds,
        file_name: c
            .file
            .as_ref()
            .map(|f| f.path.rsplit('/').next().unwrap_or(&f.path).to_string()),
        deleted: c.file.as_ref().is_some_and(|f| f.deleted),
        complete: c.complete,
    }
}

/// A UDIF disk image (`.dmg`) ends with a 512-byte "koly" trailer. A raw read/write image keeps
/// the disk's bytes as they are; a compressed one does not, and a scan finds nothing in it.
fn looks_compressed(src: &ImageSource) -> bool {
    let size = src.size();
    if size < 512 {
        return false;
    }
    let mut tail = [0u8; 4];
    src.read_exact_at(&mut tail, size - 512).is_ok() && &tail == b"koly"
}

// ---------------------------------------------------------------------------------------------
// Copying off the card
// ---------------------------------------------------------------------------------------------

/// Copies the chosen recordings, and the finished clips that can serve as their references,
/// into `out_dir`. The page then repairs the copies with the normal repair flow.
pub fn prepare_repair(
    session: &CardSession,
    offsets: &[u64],
    out_dir: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<PreparedCard> {
    check_output_dir(out_dir, &session.image)?;
    let r = &session.report;
    let recs: Vec<&Recovered> = offsets
        .iter()
        .filter_map(|o| r.recordings.iter().find(|x| x.offset == *o))
        .collect();
    let mut clips: Vec<&FinishedClip> = Vec::new();
    for rec in &recs {
        for c in carve::reference_candidates(r, rec, MAX_REFERENCES) {
            if !clips.iter().any(|x| x.offset == c.offset) {
                clips.push(c);
            }
        }
    }
    // The copies, the repaired videos (about as big again) and the reference clips.
    let need = recs.iter().map(|x| x.length * 2).sum::<u64>()
        + clips.iter().map(|c| c.length).sum::<u64>();
    check_space(out_dir, need)?;

    let opts = ScanOptions {
        cancel: Some(cancel.clone()),
        stitch: true,
    };
    let clips_dir = if clips.is_empty() {
        None
    } else {
        let dir = unique_path(out_dir, "Card clips", "");
        std::fs::create_dir(&dir).map_err(|e| write_error(&dir, e))?;
        Some(dir)
    };
    let n = recs.len();
    let mut out = Vec::new();
    for (i, rec) in recs.iter().enumerate() {
        on_progress(CardProgress::Stage {
            label: if n == 1 {
                "Copying the recording from the card".into()
            } else {
                format!("Copying recording {} of {n} from the card", i + 1)
            },
        });
        let path = recording_path(out_dir, rec);
        let mut forward = progress_forwarder(on_progress);
        match carve::extract(&session.src, rec, &path, &opts, &mut |ev| {
            if !matches!(ev, ScanEvent::Phase { .. }) {
                forward(ev)
            }
        }) {
            Ok(_) => out.push(PreparedRecording {
                offset: rec.offset,
                dat: Some(path),
                error: None,
            }),
            Err(e) => {
                let err = scan_error(e, "copy");
                if err.is_cancelled() {
                    return Err(err);
                }
                out.push(PreparedRecording {
                    offset: rec.offset,
                    dat: None,
                    error: Some(err),
                });
            }
        }
    }
    let mut saved_clips = 0;
    if let Some(dir) = &clips_dir {
        for (i, c) in clips.iter().enumerate() {
            on_progress(CardProgress::Stage {
                label: format!(
                    "Copying finished clip {} of {} to use as a reference",
                    i + 1,
                    clips.len()
                ),
            });
            let path = clip_path(dir, c);
            let mut forward = progress_forwarder(on_progress);
            match carve::extract_clip(&session.src, c, &path, &opts, &mut |ev| {
                if !matches!(ev, ScanEvent::Phase { .. }) {
                    forward(ev)
                }
            }) {
                Ok(_) => saved_clips += 1,
                // A clip that cannot be copied only means one reference less.
                Err(e) => {
                    let err = scan_error(e, "copy");
                    if err.is_cancelled() {
                        return Err(err);
                    }
                }
            }
        }
    }
    Ok(PreparedCard {
        recordings: out,
        clips_dir: clips_dir.filter(|_| saved_clips > 0),
        clips: saved_clips,
    })
}

/// Saves one fragment ("fragment") or finished clip ("clip") as a new file in `out_dir`.
pub fn save_item(
    session: &CardSession,
    kind: &str,
    offset: u64,
    out_dir: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<PathBuf> {
    check_output_dir(out_dir, &session.image)?;
    let r = &session.report;
    let opts = ScanOptions {
        cancel: Some(cancel),
        stitch: true,
    };
    let mut forward = progress_forwarder(on_progress);
    let not_found = || {
        AppError::new(
            "not_found",
            "That item is no longer in the list",
            format!("Nothing at byte {offset} in the last scan."),
            "Scan the card again.",
        )
    };
    let path = match kind {
        "clip" => {
            let c = r
                .clips
                .iter()
                .find(|c| c.offset == offset)
                .ok_or_else(not_found)?;
            check_space(out_dir, c.length)?;
            let path = clip_path(out_dir, c);
            carve::extract_clip(&session.src, c, &path, &opts, &mut forward)
                .map_err(|e| scan_error(e, "copy"))?;
            path
        }
        _ => {
            let rec = r
                .fragments
                .iter()
                .chain(&r.recordings)
                .find(|x| x.offset == offset)
                .ok_or_else(not_found)?;
            check_space(out_dir, rec.length)?;
            let path = if rec.has_dat_header {
                recording_path(out_dir, rec)
            } else {
                unique_path(out_dir, &format!("fragment_{}", rec.offset), ".DAT")
            };
            carve::extract(&session.src, rec, &path, &opts, &mut forward)
                .map_err(|e| scan_error(e, "copy"))?;
            path
        }
    };
    Ok(path)
}

/// `<card name>.DAT` when the card's file list names it, else `recovered_<offset>.DAT`.
fn recording_path(dir: &Path, rec: &Recovered) -> PathBuf {
    let stem = rec
        .file
        .as_ref()
        .and_then(|f| safe_stem(&f.path))
        .unwrap_or_else(|| format!("recovered_{}", rec.offset));
    unique_path(dir, &stem, ".DAT")
}

/// `<card name>.MP4` when known, else `clip_<offset>.MP4`.
fn clip_path(dir: &Path, c: &FinishedClip) -> PathBuf {
    let stem = c
        .file
        .as_ref()
        .and_then(|f| safe_stem(&f.path))
        .unwrap_or_else(|| format!("clip_{}", c.offset));
    unique_path(dir, &stem, ".MP4")
}

/// The file name from a card path, without extension, made safe for any file system.
fn safe_stem(card_path: &str) -> Option<String> {
    let name = card_path.rsplit('/').next()?;
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_ ()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    (!clean.is_empty()).then_some(clean)
}

/// `<dir>/<stem><ext>`, or `<stem> (2)<ext>` and so on when it exists.
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| {
            if n == 1 {
                dir.join(format!("{stem}{ext}"))
            } else {
                dir.join(format!("{stem} ({n}){ext}"))
            }
        })
        .find(|p| !p.exists() && !datfix_core::repair::temp_path(p).exists())
        .expect("an unused name exists")
}

fn write_error(path: &Path, e: std::io::Error) -> AppError {
    map_error(CoreError::io(format!("cannot write {}", path.display()), e))
}

/// The folder must exist and must not be on the card being read.
fn check_output_dir(out_dir: &Path, source: &Path) -> AppResult<()> {
    if !out_dir.is_dir() {
        return Err(AppError::new(
            "output_folder_missing",
            "The save folder is gone",
            format!("The folder {} does not exist.", out_dir.display()),
            "Choose another folder.",
        ));
    }
    if on_same_device(out_dir, source) {
        return Err(AppError::new(
            "output_on_card",
            "Don't save onto the card",
            "This folder is on the memory card you are recovering. Saving there could \
             overwrite the video you want back."
                .into(),
            "Choose a folder on your computer or another drive.",
        ));
    }
    Ok(())
}

fn check_space(dir: &Path, need: u64) -> AppResult<()> {
    if let Ok(free) = fs4::available_space(dir)
        && free < need + 64 * 1024 * 1024
    {
        return Err(AppError::new(
            "disk_full",
            "Not enough free space",
            format!(
                "This needs about {} but {} has only {} free.",
                gb(need),
                dir.display(),
                gb(free)
            ),
            "Free up space, or choose a folder on another drive.",
        ));
    }
    Ok(())
}

/// True when `dir` is on the card that `source` reads: `source` is a raw device and `dir` is on
/// one of its partitions, or `source` is an image file and `dir` is... never (an image file is
/// just a file; saving next to it is fine).
#[cfg(unix)]
fn on_same_device(dir: &Path, source: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(disk) = whole_disk_of_device(source) else {
        return false;
    };
    let Ok(dir_dev) = std::fs::metadata(dir).map(|m| m.dev()) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let part = name.strip_prefix(&disk).unwrap_or("x");
        (part.is_empty() || part.starts_with('s'))
            && std::fs::metadata(e.path()).is_ok_and(|m| m.rdev() == dir_dev)
    })
}

#[cfg(not(unix))]
fn on_same_device(_dir: &Path, _source: &Path) -> bool {
    false
}

/// "disk4" for `/dev/rdisk4`, `/dev/disk4` or `/dev/disk4s1`; null for a regular file.
#[cfg(unix)]
fn whole_disk_of_device(path: &Path) -> Option<String> {
    let name = path.strip_prefix("/dev").ok()?.to_str()?;
    let name = name.strip_prefix('r').unwrap_or(name);
    let rest = name.strip_prefix("disk")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| format!("disk{digits}"))
}

// ---------------------------------------------------------------------------------------------
// Cards and imaging (macOS)
// ---------------------------------------------------------------------------------------------

/// Memory cards and other removable disks that are mounted now. Camera cards first.
#[cfg(target_os = "macos")]
pub fn list_cards() -> Vec<CardDevice> {
    let boot = diskutil_info("/")
        .and_then(|v| v["ParentWholeDisk"].as_str().map(String::from))
        .unwrap_or_default();
    let Ok(entries) = std::fs::read_dir("/Volumes") else {
        return Vec::new();
    };
    let mut out: Vec<CardDevice> = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let mount = e.path();
        if file_name(&mount).starts_with('.') {
            continue;
        }
        let Some(info) = diskutil_info(&mount.to_string_lossy()) else {
            continue;
        };
        let Some(disk) = info["ParentWholeDisk"].as_str().map(String::from) else {
            continue;
        };
        if disk == boot || out.iter().any(|c| c.device == disk) {
            continue;
        }
        let flag = |k: &str| info[k].as_bool().unwrap_or(false);
        let looks_like_camera_card = CARD_MARKERS.iter().any(|m| mount.join(m).is_dir());
        // Only removable media (card readers) and anything holding camera folders. A big
        // external work disk is not offered.
        if !(looks_like_camera_card || flag("RemovableMedia")) {
            continue;
        }
        let whole = diskutil_info(&disk);
        let size_bytes = whole
            .as_ref()
            .and_then(|w| w["Size"].as_u64().or_else(|| w["TotalSize"].as_u64()))
            .unwrap_or(0);
        let raw_device = PathBuf::from(format!("/dev/r{disk}"));
        out.push(CardDevice {
            name: info["VolumeName"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| file_name(&mount)),
            mount_point: mount,
            readable: std::fs::File::open(&raw_device).is_ok(),
            raw_device,
            device: disk,
            size_bytes,
            file_system: info["FilesystemName"]
                .as_str()
                .or_else(|| info["FilesystemType"].as_str())
                .unwrap_or("")
                .to_string(),
            looks_like_camera_card,
        });
    }
    out.sort_by_key(|c| (!c.looks_like_camera_card, c.name.clone()));
    out
}

#[cfg(not(target_os = "macos"))]
pub fn list_cards() -> Vec<CardDevice> {
    Vec::new()
}

/// `diskutil info -plist <what>` as JSON (converted with `plutil`, so no plist crate is needed).
#[cfg(target_os = "macos")]
fn diskutil_info(what: &str) -> Option<serde_json::Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let plist = Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", what])
        .output()
        .ok()
        .filter(|o| o.status.success())?
        .stdout;
    let mut child = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&plist).ok()?;
    let out = child.wait_with_output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Makes a raw image of a whole card (`device` is "disk4" and so on) in `out_dir`. The card is
/// only read. When the device needs administrator rights, macOS asks for the password and `dd`
/// does the reading. Returns the new image file (read-only).
#[cfg(target_os = "macos")]
pub fn image_card(
    device: &str,
    out_dir: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<PathBuf> {
    let valid = device
        .strip_prefix("disk")
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    if !valid {
        return Err(AppError::new(
            "bad_device",
            "That card is not available",
            format!("{device} is not a disk name."),
            "Insert the card again and choose it from the list.",
        ));
    }
    let card = list_cards().into_iter().find(|c| c.device == device);
    let raw = PathBuf::from(format!("/dev/r{device}"));
    check_output_dir(out_dir, &raw)?;
    let size = card.as_ref().map_or(0, |c| c.size_bytes);
    check_space(out_dir, size)?;
    let stem = card
        .as_ref()
        .and_then(|c| safe_stem(&format!("{}.img", c.name)))
        .unwrap_or_else(|| "card".into());
    let output = unique_path(out_dir, &format!("{stem} card image"), ".img");
    on_progress(CardProgress::Stage {
        label: "Copying the whole card into an image file".into(),
    });
    match ImageSource::open(&raw) {
        Ok(src) => copy_device(&src, &output, &cancel, on_progress)?,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            image_with_admin(&raw, &output, size, &cancel, on_progress)?
        }
        Err(e) => {
            return Err(map_error(CoreError::io(
                format!("cannot open {}", raw.display()),
                e,
            )));
        }
    }
    // The image is evidence: keep it read-only.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o444));
    Ok(output)
}

#[cfg(not(target_os = "macos"))]
pub fn image_card(
    _device: &str,
    _out_dir: &Path,
    _cancel: Arc<AtomicBool>,
    _on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<PathBuf> {
    Err(AppError::new(
        "not_supported",
        "Not available on this computer yet",
        "DATfix can make card images on macOS only.".into(),
        "Make a raw image with another tool (see Other ways), then choose it here.",
    ))
}

/// Copies a readable device into a new file (hidden temporary name, renamed when complete).
#[cfg(target_os = "macos")]
fn copy_device(
    src: &ImageSource,
    output: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<()> {
    use std::io::Write;
    let total = src.size();
    let tmp = datfix_core::repair::temp_path(output);
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| write_error(&tmp, e))?;
    let result = (|| -> AppResult<()> {
        let mut buf = vec![0u8; IMAGE_BLOCK];
        let mut off = 0u64;
        let mut last = Instant::now() - PROGRESS_EVERY;
        while off < total {
            if cancel.load(Ordering::Relaxed) {
                return Err(scan_error(CoreError::Cancelled, "copy"));
            }
            let n = ((total - off) as usize).min(IMAGE_BLOCK);
            src.read_exact_at(&mut buf[..n], off).map_err(|e| {
                map_error(CoreError::io(
                    format!("cannot read the card at byte {off}"),
                    e,
                ))
            })?;
            file.write_all(&buf[..n])
                .map_err(|e| write_error(output, e))?;
            off += n as u64;
            if off == total || last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                on_progress(CardProgress::Step { done: off, total });
            }
        }
        file.sync_all().map_err(|e| write_error(output, e))?;
        Ok(())
    })();
    match result {
        Ok(()) => std::fs::rename(&tmp, output).map_err(|e| write_error(output, e)),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Reads the card with `dd` as administrator. macOS shows its standard password prompt.
/// Progress comes from the growing file's size. Cancel: the root shell watches for a stop file
/// and ends `dd` (a normal user cannot signal a root process).
#[cfg(target_os = "macos")]
fn image_with_admin(
    raw: &Path,
    output: &Path,
    total: u64,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(CardProgress),
) -> AppResult<()> {
    use std::process::{Command, Stdio};
    let tmp = datfix_core::repair::temp_path(output);
    let stop = tmp.with_extension("stop");
    let uid = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
        .unwrap_or_else(|| "0".into());
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    // `dd` opens `if=` read-only. `of=` is the new temporary file in the chosen folder.
    let script = format!(
        "/bin/dd if={raw} of={tmp} bs=4m 2>/dev/null & P=$!; \
         while /bin/kill -0 $P 2>/dev/null; do \
           if [ -e {stop} ]; then /bin/kill $P; fi; /bin/sleep 0.5; \
         done; wait $P; R=$?; /usr/sbin/chown {uid} {tmp}; exit $R",
        raw = q(raw),
        tmp = q(&tmp),
        stop = q(&stop),
    );
    let prompt = "DATfix needs your password to read the whole memory card. \
                  Nothing is written to the card.";
    let mut child = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with administrator privileges with prompt (item 2 of argv)",
            "-e",
            "end run",
            &script,
            prompt,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| map_error(CoreError::io("cannot start osascript", e)))?;
    let mut asked_stop = false;
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break s;
        }
        if cancel.load(Ordering::Relaxed) && !asked_stop {
            asked_stop = true;
            // Before the password is entered there is no root shell yet: end the prompt.
            let _ = std::fs::write(&stop, b"");
            if !tmp.exists() {
                let _ = child.kill();
            }
        }
        if let Ok(m) = std::fs::metadata(&tmp) {
            on_progress(CardProgress::Step {
                done: m.len().min(total),
                total,
            });
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut t = String::new();
            let _ = std::io::Read::read_to_string(&mut s, &mut t);
            t
        })
        .unwrap_or_default();
    let _ = std::fs::remove_file(&stop);
    let got = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let fail = |err: AppError| {
        let _ = std::fs::remove_file(&tmp);
        Err(err)
    };
    if asked_stop {
        return fail(scan_error(CoreError::Cancelled, "copy"));
    }
    if !status.success() {
        if stderr.contains("-128") {
            return fail(AppError::new(
                "admin_declined",
                "No password was entered",
                "DATfix needs an administrator password to read the whole card.".into(),
                "Try again, or make the image with Disk Utility (see Other ways).",
            ));
        }
        let mut err = AppError::new(
            "imaging_failed",
            "The card could not be copied",
            "Reading the card stopped with an error. The card may be failing.".into(),
            "Try another card reader, or make the image with Disk Utility (see Other ways).",
        );
        err.details = format!("dd via osascript: {status}; {}", stderr.trim());
        return fail(err);
    }
    if total > 0 && got != total {
        let mut err = AppError::new(
            "imaging_short",
            "The image is incomplete",
            format!(
                "The card holds {} but only {} were copied.",
                gb(total),
                gb(got)
            ),
            "Try again with another card reader.",
        );
        err.details = format!("expected {total} bytes, got {got}");
        return fail(err);
    }
    on_progress(CardProgress::Step { done: got, total });
    std::fs::rename(&tmp, output).map_err(|e| write_error(output, e))
}

#[cfg(target_os = "macos")]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------------------------
// Test hook: a slow source
// ---------------------------------------------------------------------------------------------

fn dev_scan_rate() -> Option<f64> {
    std::env::var("DATFIX_DEV_SCAN_MBPS")
        .ok()?
        .parse::<f64>()
        .ok()
        .filter(|r| *r > 0.0)
        .map(|r| r * 1e6)
}

/// Reads at most `rate` bytes per second (test hook only).
struct Throttled<'a> {
    inner: &'a ImageSource,
    rate: f64,
}

impl<'a> Throttled<'a> {
    fn new(inner: &'a ImageSource, rate: f64) -> Self {
        Throttled { inner, rate }
    }
}

impl ReadAt for Throttled<'_> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let n = self.inner.read_at(buf, offset)?;
        if n > 64 * 1024 {
            std::thread::sleep(Duration::from_secs_f64(n as f64 / self.rate));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(
            safe_stem("/XFVC/A_0001C809H241214_1422235Q_LAD04.DAT").as_deref(),
            Some("A_0001C809H241214_1422235Q_LAD04")
        );
        assert_eq!(safe_stem("/DCIM/_AD1234.DAT").as_deref(), Some("_AD1234"));
        assert_eq!(safe_stem("/a/.DAT").as_deref(), None);
        assert_eq!(safe_stem("/a/x:y.MP4").as_deref(), Some("x_y"));
        let dir = tempfile::tempdir().unwrap();
        let a = unique_path(dir.path(), "A", ".DAT");
        assert_eq!(a, dir.path().join("A.DAT"));
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "A", ".DAT"),
            dir.path().join("A (2).DAT")
        );
    }

    #[cfg(unix)]
    #[test]
    fn device_names() {
        assert_eq!(
            whole_disk_of_device(Path::new("/dev/rdisk4")).as_deref(),
            Some("disk4")
        );
        assert_eq!(
            whole_disk_of_device(Path::new("/dev/disk12s1")).as_deref(),
            Some("disk12")
        );
        assert_eq!(whole_disk_of_device(Path::new("/tmp/card.img")), None);
        let dir = tempfile::tempdir().unwrap();
        assert!(!on_same_device(dir.path(), Path::new("/tmp/card.img")));
    }

    #[test]
    fn scan_of_an_empty_file_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("card.img");
        std::fs::write(&img, b"").unwrap();
        let Err(e) = scan(&img, Arc::default(), &mut |_| {}) else {
            panic!("an empty image must fail");
        };
        assert_eq!(e.kind, "image_empty");
    }

    #[test]
    fn scan_of_a_blank_image_finds_nothing_and_explains() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("card.img");
        std::fs::write(&img, vec![0u8; 4 << 20]).unwrap();
        let mut stages = 0;
        let (view, _) = scan(&img, Arc::default(), &mut |p| {
            if matches!(p, CardProgress::Stage { .. }) {
                stages += 1;
            }
        })
        .unwrap();
        assert!(view.recordings.is_empty());
        assert!(stages >= 2);
        assert_eq!(view.notes.len(), 1);
        assert!(view.file_system.is_none());
    }
}
