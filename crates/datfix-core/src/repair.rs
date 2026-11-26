//! The repair pipeline: index the `.DAT`, match a reference, rebuild tables, write the MP4.
//!
//! The pipeline reports what it is doing through a callback that receives [`Event`] values.
//! The CLI prints them; a GUI can drive a progress bar from them. Events serialize to JSON.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::codec::{FrameAnalyzer, FrameInfo, VideoCodec};
use crate::error::{Error, Result};
use crate::mapping::{self, Mapping, TrackPlan};
use crate::mp4::reference::Reference;
use crate::mp4::tables::MP4_EPOCH_OFFSET;
use crate::mp4::writer::{self, TrackOutput, rescale};
use crate::read_at::{FileSource, ReadAt};
use crate::repi::{self, DatIndex, Truncation};
use crate::timing::{self, VideoTiming};

/// Frames analyzed when probing whether a reference fits.
const PROBE_FRAMES: usize = 30;

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
    /// A candidate reference was skipped (folder search).
    ReferenceRejected { path: PathBuf, reason: String },
    /// The reference that will be used.
    ReferenceChosen { path: PathBuf },
    /// A non-fatal observation.
    Note { message: String },
}

/// Where the reference comes from.
#[derive(Clone, Debug)]
pub enum ReferenceSource {
    /// One specific healthy MP4.
    File(PathBuf),
    /// A folder to search for the smallest compatible MP4.
    Dir(PathBuf),
}

/// Options for [`repair`].
#[derive(Clone, Debug, Default)]
pub struct RepairOptions {
    /// Replace the output if it already exists.
    pub overwrite: bool,
    /// Set to `true` from another thread to stop the repair.
    pub cancel: Option<Arc<AtomicBool>>,
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
    pub reference: PathBuf,
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

/// A reference that passed the compatibility check.
pub struct Checked {
    pub reference: Reference,
    pub mapping: Mapping,
    pub codec: VideoCodec,
}

/// Checks whether `reference` fits the `.DAT`: track mapping, codec, and a trial parse of the
/// first frames with the reference's parameter sets.
pub fn check_reference<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    reference: Reference,
) -> Result<Checked> {
    let incompatible = |reference: &Reference, reasons: Vec<String>| Error::Incompatible {
        path: reference.path.clone(),
        reasons,
    };
    let mapping = match mapping::map_tracks(index, &reference) {
        Ok(m) => m,
        Err(reasons) => return Err(incompatible(&reference, reasons)),
    };
    let vt = &reference.tracks[mapping.video().ref_track];
    let vf = vt.video.as_ref().expect("video track has a video format");
    let codec = match VideoCodec::from_config(vt.codec, vf.config_kind, &vf.config) {
        Ok(c) => c,
        Err(e) => return Err(incompatible(&reference, vec![e])),
    };

    let mut analyzer = FrameAnalyzer::new(codec.clone());
    let mut probed = Vec::new();
    for (off, size) in mapping.video().sample_locations().take(PROBE_FRAMES) {
        match analyzer.analyze(src, off, size) {
            Ok(f) => probed.push(f),
            Err(e) => {
                let reason = format!(
                    "the video frames cannot be read with the reference's {} settings: {e}",
                    codec.name()
                );
                return Err(incompatible(&reference, vec![reason]));
            }
        }
    }
    if !vt.has_stss && !vt.has_ctts && probed.iter().any(|f| !f.sync && !f.cra) {
        let reason = "the .DAT is Long GOP (it has non-key frames) but the reference is All-Intra"
            .to_string();
        return Err(incompatible(&reference, vec![reason]));
    }
    Ok(Checked {
        reference,
        mapping,
        codec,
    })
}

/// Finds MP4 files in `dir` (not recursive) and returns the smallest compatible one.
pub fn choose_reference<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    dir: &Path,
    on_event: &mut dyn FnMut(&Event),
) -> Result<Checked> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::io(format!("cannot list {}", dir.display()), e))?;
    let mut candidates: Vec<(u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip macOS "._" resource-fork files and our own temporary outputs.
            !name.starts_with('.')
                && p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
        .collect();
    candidates.sort();

    let mut tried = Vec::new();
    for (_, path) in candidates {
        let result = Reference::open(&path).and_then(|r| check_reference(src, index, r));
        match result {
            Ok(c) => {
                on_event(&Event::ReferenceChosen { path: path.clone() });
                return Ok(c);
            }
            Err(e) => {
                let reason = match &e {
                    Error::Incompatible { reasons, .. } => reasons.join("; "),
                    other => other.to_string(),
                };
                on_event(&Event::ReferenceRejected {
                    path: path.clone(),
                    reason: reason.clone(),
                });
                tried.push((path, reason));
            }
        }
    }
    Err(Error::NoReference {
        dir: dir.to_path_buf(),
        tried,
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

/// Trims every non-video track so it ends with the video. Returns trimmed sample counts.
fn trim_to_video(
    reference: &Reference,
    plans: &mut [TrackPlan],
    notes: &mut Vec<String>,
) -> Vec<u64> {
    let vi = plans
        .iter()
        .position(|p| p.repi_type.0 == *b"vide")
        .expect("video plan");
    let vt = &reference.tracks[plans[vi].ref_track];
    let video_ticks = plans[vi].sample_count * vt.sample_delta as u64;
    let mut trimmed = vec![0u64; plans.len()];
    for (i, p) in plans.iter_mut().enumerate() {
        if i == vi {
            continue;
        }
        let rt = &reference.tracks[p.ref_track];
        // Samples that fit in the video's duration, rounded to nearest.
        let fit = rescale(video_ticks, vt.timescale, rt.timescale) as f64 / rt.sample_delta as f64;
        let keep = fit.round() as u64;
        if keep < p.sample_count {
            trimmed[i] = p.sample_count - keep;
            p.truncate(keep);
        } else if keep > p.sample_count {
            let short = (keep - p.sample_count) as f64 * rt.sample_seconds();
            if short > vt.sample_seconds() {
                notes.push(format!(
                    "track {} ({}) ends {short:.3} s before the video",
                    rt.track_id, p.repi_type
                ));
            }
        }
    }
    trimmed
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
    // Open a single reference first so a typo fails before a long index walk.
    let given = match reference {
        ReferenceSource::File(p) => Some(Reference::open(p)?),
        ReferenceSource::Dir(_) => None,
    };
    let index = index_dat(&src, opts, on_event)?;

    on_event(&Event::Phase {
        phase: Phase::CheckingReference,
        message: "checking the reference clip".into(),
    });
    let checked = match (given, reference) {
        (Some(r), _) => check_reference(&src, &index, r)?,
        (None, ReferenceSource::Dir(d)) => choose_reference(&src, &index, d, on_event)?,
        (None, ReferenceSource::File(_)) => unreachable!("opened above"),
    };
    let Checked {
        reference,
        mut mapping,
        codec,
    } = checked;
    for n in &mapping.notes {
        on_event(&Event::Note { message: n.clone() });
    }

    let mut notes = std::mem::take(&mut mapping.notes);
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

    let trimmed = trim_to_video(&reference, &mut mapping.plans, &mut notes);

    let vplan = mapping.video().clone();
    let (frames, damaged) = analyze_video(&src, &vplan, codec, opts, on_event, &mut notes)?;
    let timing = timing::video_timing(&frames);

    // Creation time: the .DAT's modification time, close to when recording stopped.
    let unix = src
        .file()
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let time = unix + MP4_EPOCH_OFFSET;

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
    let copy_start = index.payload_start();
    let copy_end = mapping
        .plans
        .iter()
        .map(TrackPlan::end_offset)
        .max()
        .unwrap_or(copy_start);
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
        reference: reference.path.clone(),
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

/// Hidden temp file next to the output; renamed into place on success.
fn temp_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output.mp4");
    output.with_file_name(format!(".{name}.datfix-partial"))
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
