//! The repair pipeline: index the `.DAT`, match a reference, rebuild tables, write the MP4.
//!
//! The pipeline reports what it is doing through a callback that receives [`Event`] values.
//! The CLI prints them; a GUI can drive a progress bar from them. Events serialize to JSON.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::codec::{FrameAnalyzer, FrameInfo, VideoCodec};
use crate::error::{Error, Result};
use crate::layout::{self, DatLayout};
use crate::mapping::TrackPlan;
use crate::mp4::reference::{Reference, TrackKind};
use crate::mp4::tables::MP4_EPOCH_OFFSET;
use crate::mp4::writer::{self, TrackOutput, rescale};
use crate::read_at::{FileSource, ReadAt};
use crate::repi::{self, DatIndex, Truncation};
use crate::timeline::{self, TimelineGap};
use crate::timing::{self, VideoTiming};

pub use crate::select::{
    Candidate, Checked, RefKind, ReferenceSearch, SearchOptions, Verdict, check_reference,
    choose_from_dir, choose_profile, search_references,
};

/// Stage of the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Indexing,
    CheckingReference,
    AnalyzingVideo,
    Writing,
}

/// Something the pipeline wants the user (or a GUI) to know.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A new stage started.
    Phase { phase: Phase, message: String },
    /// Progress inside a stage. `done` and `total` share a unit (bytes or frames).
    Progress { phase: Phase, done: u64, total: u64 },
    /// A candidate reference was skipped (folder search). `reason` is technical,
    /// `user_reason` is one plain sentence.
    ReferenceRejected {
        path: PathBuf,
        reason: String,
        user_reason: String,
    },
    /// The reference that will be used.
    ReferenceChosen { path: PathBuf },
    /// A non-fatal observation.
    Note { message: String },
}

/// Where the decoder settings come from.
#[derive(Clone, Debug)]
pub enum ReferenceSource {
    /// One specific healthy MP4. It always wins: no fallback.
    File(PathBuf),
    /// A folder searched recursively for a compatible MP4. Falls back to the built-in
    /// profiles when nothing in it fits.
    Dir(PathBuf),
    /// No reference: pick a built-in profile from what the `.DAT` itself shows.
    Builtin,
    /// Search the `.DAT`'s own folder (not its subfolders) for a fitting clip, then fall back
    /// to the built-in profiles.
    Auto,
}

/// Options for [`repair`].
#[derive(Clone, Debug, Default)]
pub struct RepairOptions {
    /// Replace the output if it already exists.
    pub overwrite: bool,
    /// Set to `true` from another thread to stop the repair.
    pub cancel: Option<Arc<AtomicBool>>,
    /// An index already read from this `.DAT` (for example by [`crate::inspect`]). Skips the
    /// index walk, which takes about 40 s for a 38 GB file. Ignored when its file size does
    /// not match the file on disk.
    pub index: Option<Arc<DatIndex>>,
}

/// One output track in the report.
#[derive(Clone, Debug, Serialize)]
pub struct TrackReport {
    pub track_id: u32,
    pub handler: crate::FourCC,
    pub codec: crate::FourCC,
    pub source: crate::FourCC,
    pub samples: u64,
    pub duration_seconds: f64,
    /// Samples removed so the track does not run past the video.
    pub trimmed_samples: u64,
}

/// What a finished repair produced.
#[derive(Clone, Debug, Serialize)]
pub struct RepairReport {
    pub input: PathBuf,
    /// The clip path, or `built-in profile <id>`.
    pub reference: PathBuf,
    /// `user` (every track from the clip), `hybrid` (clip video + synthesized tracks) or
    /// `profile` (built-in decoder settings).
    pub reference_kind: RefKind,
    /// Tracks or boxes written from Canon's conventions instead of copied.
    pub synthesized: Vec<String>,
    /// What was rebuilt and why, in plain sentences.
    pub rebuilt: Vec<String>,
    /// Where the settings came from, as one plain sentence.
    pub settings: String,
    /// The built-in profile in plain words (`Canon EOS R5 Mark II, 4K 119.88p`), when used.
    pub profile: Option<String>,
    /// The camera as people know it, when the `.DAT` names it.
    pub camera: Option<String>,
    /// What the `.DAT` itself shows: body, codec, frame rate, family.
    pub recording: String,
    /// Recording start written into the file (UTC, seconds since 1970) and where it came from.
    pub creation_time: Option<i64>,
    pub creation_time_source: String,
    /// Damaged regions and how many frames each removed.
    pub gaps: Vec<TimelineGap>,
    pub output: PathBuf,
    pub bytes_written: u64,
    pub duration_seconds: f64,
    pub video_frames: u64,
    pub keyframes: u64,
    /// True for Long-GOP video with B-frames (a `ctts` table was written).
    pub reordered: bool,
    /// Frames whose NAL structure could not be read (kept, but they may show artifacts).
    pub damaged_frames: u64,
    pub tracks: Vec<TrackReport>,
    pub truncation: Option<Truncation>,
    pub notes: Vec<String>,
}

/// Rate-limits progress events to about 200 per stage.
struct Throttle {
    next: u64,
}

impl Throttle {
    fn new() -> Self {
        Throttle { next: 0 }
    }

    fn due(&mut self, done: u64, total: u64) -> bool {
        if done >= self.next || done >= total {
            self.next = done + (total / 200).max(1);
            true
        } else {
            false
        }
    }
}

fn cancelled(opts: &RepairOptions) -> bool {
    opts.cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Walks the `.DAT` and reports progress.
pub fn index_dat<R: ReadAt + ?Sized>(
    src: &R,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<DatIndex> {
    on_event(&Event::Phase {
        phase: Phase::Indexing,
        message: "reading the recovery index".into(),
    });
    let total = src.size();
    let mut throttle = Throttle::new();
    repi::walk(src, &mut |pos| {
        if throttle.due(pos, total) {
            on_event(&Event::Progress {
                phase: Phase::Indexing,
                done: pos,
                total,
            });
        }
        !cancelled(opts)
    })
}

/// Analyzes every video frame: keyframe flag and display order.
fn analyze_video<R: ReadAt + ?Sized>(
    src: &R,
    plan: &TrackPlan,
    codec: VideoCodec,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
    notes: &mut Vec<String>,
) -> Result<(Vec<FrameInfo>, u64)> {
    on_event(&Event::Phase {
        phase: Phase::AnalyzingVideo,
        message: format!("reading {} frame headers", plan.sample_count),
    });
    let total = plan.sample_count;
    let mut analyzer = FrameAnalyzer::new(codec);
    let mut frames = Vec::with_capacity(total as usize);
    let mut damaged = 0u64;
    let mut throttle = Throttle::new();
    for (i, (off, size)) in plan.sample_locations().enumerate() {
        let info = match analyzer.analyze(src, off, size) {
            Ok(f) => f,
            Err(e) => {
                damaged += 1;
                if damaged <= 5 {
                    notes.push(format!(
                        "frame {} (byte {off} of the .DAT) looks damaged: {e}",
                        i + 1
                    ));
                }
                // Keep the frame in decode order right after its predecessor.
                FrameInfo {
                    order: frames.last().map_or((0, 0), |f: &FrameInfo| f.order),
                    ..Default::default()
                }
            }
        };
        frames.push(info);
        let done = i as u64 + 1;
        if throttle.due(done, total) {
            on_event(&Event::Progress {
                phase: Phase::AnalyzingVideo,
                done,
                total,
            });
            if cancelled(opts) {
                return Err(Error::Cancelled);
            }
        }
    }
    if damaged > 5 {
        notes.push(format!("{damaged} frames in total look damaged"));
    }
    Ok((frames, damaged))
}

/// Trims every non-video track so it ends with the video, and pads PCM tracks that end early
/// with silence. Returns trimmed sample counts.
fn fit_to_video(
    reference: &Reference,
    plans: &mut [TrackPlan],
    notes: &mut Vec<String>,
) -> Vec<u64> {
    let vi = plans
        .iter()
        .position(|p| p.repi_type.0 == *b"vide")
        .expect("video plan");
    let vt = &reference.tracks[plans[vi].ref_track];
    let video_ticks = plans[vi].duration_units() * vt.sample_delta as u64;
    let mut trimmed = vec![0u64; plans.len()];
    for (i, p) in plans.iter_mut().enumerate() {
        if i == vi {
            continue;
        }
        let rt = &reference.tracks[p.ref_track];
        // Samples that fit in the video's duration, rounded to nearest.
        let fit = rescale(video_ticks, vt.timescale, rt.timescale) as f64 / rt.sample_delta as f64;
        let keep = fit.round() as u64;
        let have = p.duration_units();
        if keep < have {
            // Per-frame tracks with stretched samples: cut by duration, not by count.
            let mut units = 0u64;
            let mut n = 0u64;
            if p.steps.is_empty() {
                n = keep;
            } else {
                for &s in &p.steps {
                    if units + s as u64 > keep {
                        break;
                    }
                    units += s as u64;
                    n += 1;
                }
            }
            trimmed[i] = p.sample_count - n;
            p.truncate(n);
        } else if keep > have {
            let short = (keep - have) as f64 * rt.sample_seconds();
            if rt.kind == TrackKind::PcmAudio && p.steps.is_empty() {
                let added = timeline::pad_to(p, keep);
                if short > vt.sample_seconds() / 2.0 && added > 0 {
                    notes.push(format!(
                        "track {} ({}) ended {short:.3} s before the video; padded with silence",
                        rt.track_id, rt.codec
                    ));
                }
            } else if short > vt.sample_seconds() {
                notes.push(format!(
                    "track {} ({}) ends {short:.3} s before the video",
                    rt.track_id, p.repi_type
                ));
            }
        }
    }
    trimmed
}

/// Rejects inputs that are not Canon `.DAT` recordings, with a message that says what to do.
pub fn classify_input<R: ReadAt + ?Sized>(path: &Path, src: &R) -> Result<()> {
    let len = src.size();
    if len == 0 {
        return Err(Error::EmptyDat {
            path: path.to_path_buf(),
            detail: "0 bytes".into(),
        });
    }
    let head = src
        .read_vec(0, len.min(1 << 20) as usize)
        .map_err(|e| Error::io(format!("cannot read {}", path.display()), e))?;
    if head.iter().all(|&b| b == 0) {
        return Err(Error::EmptyDat {
            path: path.to_path_buf(),
            detail: "only zero bytes".into(),
        });
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"CDXA" {
        return Err(Error::NotCanon(
            "it is a Video CD track (RIFF/CDXA MPEG-1). Video CD .dat files are not broken; \
             open them in VLC or convert them with ffmpeg."
                .into(),
        ));
    }
    if head.len() >= 4 && head[0..3] == [0, 0, 1] && matches!(head[3], 0xba | 0xb3) {
        return Err(Error::NotCanon(
            "it is an MPEG program stream (as on Video CDs and some DVD recorders), not a Canon \
             recording. It is not broken; open it in VLC or rename it to .mpg."
                .into(),
        ));
    }
    let canon_header = head.len() >= 16
        && head[0..8] == [0, 0, 0, 0, b'm', b'd', b'a', b't']
        && &head[12..16] == b"wide";
    let has_repi = head.windows(4).any(|w| w == repi::MAGIC);
    if canon_header && !has_repi {
        return Err(Error::Unsupported(
            "this is an unfinished Canon recording, but from a body that does not write a \
             recovery index (repi), such as the EOS R or M50 Mark II. Rebuilding these needs \
             frame-boundary guessing, which datfix does not support yet. Keep the file \
             unchanged; a later version may repair it."
                .into(),
        ));
    }
    Ok(())
}

/// Picks the creation time: the Canon file name (local time) adjusted to UTC with the
/// offset implied by the file's modification time, else modification time minus duration.
fn creation_time(layout: &DatLayout, mtime: Option<i64>, duration: f64) -> (Option<i64>, String) {
    let end_estimate = mtime.map(|m| m - duration.round() as i64);
    if let Some(local) = layout.start.name_local {
        if let Some(utc) = end_estimate {
            let offset = local - utc;
            let rounded = (offset as f64 / 900.0).round() as i64 * 900;
            if rounded.abs() <= 14 * 3600 && (offset - rounded).abs() <= 600 {
                let sign = if rounded < 0 { '-' } else { '+' };
                return (
                    Some(local - rounded),
                    format!(
                        "file name (camera local time, UTC{sign}{:02}:{:02} inferred from the file \
                         time)",
                        rounded.abs() / 3600,
                        rounded.abs() % 3600 / 60
                    ),
                );
            }
        }
        return (
            Some(local),
            "file name (camera local time; time zone unknown, written as UTC)".into(),
        );
    }
    match end_estimate {
        Some(t) => (
            Some(t),
            "file modification time minus the recording length".into(),
        ),
        None => (None, "unknown".into()),
    }
}

/// Repairs `dat` into `output`.
pub fn repair(
    dat: &Path,
    reference: &ReferenceSource,
    output: &Path,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<RepairReport> {
    check_output(dat, output, opts)?;
    let src = FileSource::open(dat)
        .map_err(|e| Error::io(format!("cannot open {}", dat.display()), e))?;
    classify_input(dat, &src)?;
    // Open a single reference first so a typo fails before a long index walk.
    let given = match reference {
        ReferenceSource::File(p) => Some(Reference::open(p)?),
        _ => None,
    };
    let walked;
    let index: &DatIndex = match &opts.index {
        Some(i) if i.file_size == src.size() => i,
        _ => {
            walked = index_dat(&src, opts, on_event)?;
            &walked
        }
    };
    let name = dat.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lay = layout::analyze(&src, index, name);

    on_event(&Event::Phase {
        phase: Phase::CheckingReference,
        message: format!("checking decoder settings for {}", lay.describe()),
    });
    let checked = match (given, reference) {
        (Some(r), _) => check_reference(&src, index, &lay, r, RefKind::User)?,
        (None, ReferenceSource::Dir(d)) => match choose_from_dir(&src, index, &lay, d, on_event) {
            Ok(c) => c,
            Err(dir_err @ Error::NoReference { .. }) => {
                match choose_profile(&src, index, &lay, on_event) {
                    Ok(mut c) => {
                        c.notes.insert(
                            0,
                            format!(
                                "no clip in {} fits this .DAT; falling back to the built-in \
                                 profiles",
                                d.display()
                            ),
                        );
                        c
                    }
                    Err(_) => return Err(dir_err),
                }
            }
            Err(e) => return Err(e),
        },
        (None, ReferenceSource::Builtin) => choose_profile(&src, index, &lay, on_event)?,
        (None, ReferenceSource::Auto) => {
            let dirs: Vec<PathBuf> = dat.parent().map(Path::to_path_buf).into_iter().collect();
            let opts = SearchOptions {
                depth: 0,
                ..SearchOptions::default()
            };
            let found = search_references(&src, index, &lay, &dirs, &opts, on_event)?;
            let (checked, chosen) = (found.checked(), found.chosen);
            match chosen {
                Some(c) => c,
                None => {
                    let mut c = choose_profile(&src, index, &lay, on_event)?;
                    if checked > 0 {
                        c.notes.insert(
                            0,
                            format!(
                                "none of the {checked} clip(s) next to the .DAT fits; using the \
                                 built-in profiles"
                            ),
                        );
                    }
                    c
                }
            }
        }
        (None, ReferenceSource::File(_)) => unreachable!("opened above"),
    };
    let Checked {
        reference,
        mut mapping,
        codec,
        kind,
        label,
        synthesized,
        rebuilt,
        summary: settings,
        profile,
        notes: check_notes,
    } = checked;
    for n in check_notes.iter().chain(&mapping.notes) {
        on_event(&Event::Note { message: n.clone() });
    }

    let mut notes = check_notes;
    notes.append(&mut mapping.notes);
    if let Some(t) = &index.truncation {
        notes.push(format!(
            "the recording was cut off at byte {}; {} incomplete sample(s) were dropped",
            t.offset, t.dropped_samples
        ));
    }
    if !index.rejected_headers.is_empty() {
        notes.push(format!(
            "{} damaged index header(s) were skipped; the samples they described are lost",
            index.rejected_headers.len()
        ));
    }
    if lay.frame_rate.is_some() && !lay.frame_rate_from_audio {
        notes.push(
            "the frame rate was inferred from the index group size (no PCM audio to measure \
             against)"
                .into(),
        );
    }

    // Keep audio and video in sync across damaged regions.
    let tl = timeline::build(&src, index, &lay);
    notes.extend(timeline::apply(&tl, &reference, &mut mapping.plans));
    for g in &tl.gaps {
        if g.lost_frames > 0 {
            notes.push(format!(
                "{} damaged bytes before frame {}: {} frame(s) lost (measured by {}); the picture \
                 holds and audio is silent for that time",
                g.damaged_bytes, g.resumes_at_frame, g.lost_frames, g.method
            ));
        }
    }

    let trimmed = fit_to_video(&reference, &mut mapping.plans, &mut notes);

    let vplan = mapping.video().clone();
    let (frames, damaged) = analyze_video(&src, &vplan, codec, opts, on_event, &mut notes)?;
    let timing = timing::video_timing(&frames);

    let vt = &reference.tracks[vplan.ref_track];
    let duration = vplan.duration_units() as f64 * vt.sample_seconds();
    let mtime = src
        .file()
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let (created, created_source) = creation_time(&lay, mtime, duration);
    let time = created.map_or(0, |t| (t + MP4_EPOCH_OFFSET as i64).max(0) as u64);

    // The payload is copied from the first group to the end of the last sample used; inserted
    // silence points at a zero block appended after it.
    let copy_start = index.payload_start();
    let copy_end = mapping
        .plans
        .iter()
        .map(TrackPlan::end_offset)
        .max()
        .unwrap_or(copy_start);
    let silence = mapping
        .plans
        .iter()
        .map(TrackPlan::silence_bytes)
        .max()
        .unwrap_or(0);
    for p in &mut mapping.plans {
        for c in &mut p.chunks {
            if c.silence {
                c.offset = copy_end;
            }
        }
    }

    let outputs: Vec<TrackOutput> = mapping
        .plans
        .iter()
        .map(|p| TrackOutput {
            ref_track: &reference.tracks[p.ref_track],
            plan: p,
            video: (p.repi_type.0 == *b"vide").then_some(&timing),
        })
        .collect();

    // Build the moov twice: once to learn its size, then with the real chunk offsets.
    let bad_ref = |reason: String| Error::BadReference {
        path: reference.path.clone(),
        reason,
    };
    let (probe, _) = writer::build_moov(&reference, &outputs, 0, time).map_err(bad_ref)?;
    let payload_start = reference.ftyp.len() as u64 + probe.encoded_len() + 16;
    let delta = payload_start as i64 - copy_start as i64;
    let (moov, dropped) = writer::build_moov(&reference, &outputs, delta, time).map_err(bad_ref)?;
    debug_assert_eq!(moov.encoded_len(), probe.encoded_len());
    if !dropped.is_empty() {
        notes.push(format!(
            "reference boxes not carried over: {}",
            dropped.join(", ")
        ));
    }

    on_event(&Event::Phase {
        phase: Phase::Writing,
        message: format!("writing {}", output.display()),
    });
    let tmp = temp_path(output);
    let mut throttle = Throttle::new();
    let written = writer::write_file(
        &tmp,
        &reference.ftyp,
        &moov.to_bytes(),
        &src,
        copy_start,
        copy_end,
        silence,
        &mut |done, total| {
            if throttle.due(done, total) {
                on_event(&Event::Progress {
                    phase: Phase::Writing,
                    done,
                    total,
                });
            }
            !cancelled(opts)
        },
    );
    let written = match written {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if opts.overwrite && output.exists() {
        std::fs::remove_file(output)
            .map_err(|e| Error::io(format!("cannot replace {}", output.display()), e))?;
    }
    std::fs::rename(&tmp, output).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(
            format!("cannot rename the finished file to {}", output.display()),
            e,
        )
    })?;

    let mut tracks: Vec<TrackReport> = outputs
        .iter()
        .zip(&trimmed)
        .map(|(t, &trim)| TrackReport {
            track_id: t.ref_track.track_id,
            handler: t.ref_track.handler,
            codec: t.ref_track.codec,
            source: t.plan.repi_type,
            samples: t.plan.sample_count,
            duration_seconds: t.media_duration() as f64 / t.ref_track.timescale as f64,
            trimmed_samples: trim,
        })
        .collect();
    tracks.sort_by_key(|t| t.track_id);
    let duration_seconds = tracks
        .iter()
        .find(|t| t.source.0 == *b"vide")
        .map_or(0.0, |t| t.duration_seconds);
    Ok(RepairReport {
        input: dat.to_path_buf(),
        reference: PathBuf::from(label),
        reference_kind: kind,
        synthesized,
        rebuilt,
        settings,
        profile,
        camera: lay.camera(),
        recording: lay.describe(),
        creation_time: created,
        creation_time_source: created_source,
        gaps: tl.gaps,
        output: output.to_path_buf(),
        bytes_written: written,
        duration_seconds,
        video_frames: vplan.sample_count,
        keyframes: timing.sync_samples.len() as u64,
        reordered: timing.reordered,
        damaged_frames: damaged,
        tracks,
        truncation: index.truncation.clone(),
        notes,
    })
}

/// Refuses to write over the input, or over an existing file unless allowed.
fn check_output(input: &Path, output: &Path, opts: &RepairOptions) -> Result<()> {
    if output.exists() {
        let same = match (input.canonicalize(), output.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if same {
            return Err(Error::OutputIsInput);
        }
        if !opts.overwrite {
            return Err(Error::OutputExists(output.to_path_buf()));
        }
    }
    Ok(())
}

/// Suffix of the hidden temporary file a repair writes before it renames it into place.
pub const PARTIAL_SUFFIX: &str = ".datfix-partial";

/// Hidden temp file next to the output; renamed into place on success.
pub fn temp_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output.mp4");
    output.with_file_name(format!(".{name}{PARTIAL_SUFFIX}"))
}

/// A temporary file left by a repair that was killed (power loss, force quit).
#[derive(Clone, Debug, Serialize)]
pub struct PartialFile {
    pub path: PathBuf,
    pub bytes: u64,
    /// Seconds since the file was last written.
    pub idle_seconds: u64,
}

/// True for names that a repair's temporary file can have (`.<name>.datfix-partial`).
pub fn is_partial_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(PARTIAL_SUFFIX) && name.len() > PARTIAL_SUFFIX.len() + 1
}

/// Lists temporary files in `dir` (not its subfolders) that no repair has written to for at
/// least `idle`. A running repair writes its file continuously, so a file idle for a minute
/// belongs to a repair that was killed.
pub fn find_stale_partials(dir: &Path, idle: Duration) -> Vec<PartialFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut out: Vec<PartialFile> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(is_partial_name))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let idle_for = now
                .duration_since(meta.modified().ok()?)
                .unwrap_or_default();
            (idle_for >= idle).then(|| PartialFile {
                path: e.path(),
                bytes: meta.len(),
                idle_seconds: idle_for.as_secs(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Deletes temporary files found by [`find_stale_partials`]. Refuses any path whose name is
/// not a repair's temporary file name. Returns the bytes freed.
pub fn remove_partials(paths: &[PathBuf]) -> Result<u64> {
    let mut freed = 0;
    for p in paths {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !is_partial_name(name) {
            return Err(Error::io(
                format!("refusing to delete {}", p.display()),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not a datfix temporary file",
                ),
            ));
        }
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(p) {
            Ok(()) => freed += len,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(format!("cannot delete {}", p.display()), e)),
        }
    }
    Ok(freed)
}

/// Video timing for callers that want it without writing (tests, diagnostics).
pub fn compute_video_timing<R: ReadAt + ?Sized>(
    src: &R,
    locations: &[(u64, u32)],
    codec: VideoCodec,
) -> Result<VideoTiming, String> {
    let mut analyzer = FrameAnalyzer::new(codec);
    let frames = locations
        .iter()
        .map(|&(o, s)| analyzer.analyze(src, o, s))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(timing::video_timing(&frames))
}
