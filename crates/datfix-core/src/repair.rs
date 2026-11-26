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
use crate::read_at::{FileSource, Overlay, ReadAt, Region, Tolerant};
use crate::repi::{self, DatIndex, Truncation};
use crate::timeline::{self, TimelineGap};
use crate::timing::{self, VideoTiming};
use crate::volume;

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
    /// Stop at the first read error. By default (`false`) unreadable parts of a failing card
    /// are skipped and treated as damage; the report lists them and the seconds lost.
    pub stop_on_read_error: bool,
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
    /// Regions of the `.DAT` that could not be read and were treated as damage.
    pub unreadable: Vec<Region>,
    pub unreadable_bytes: u64,
    /// Seconds of video lost to damaged or unreadable regions (the picture holds over them).
    pub lost_seconds: f64,
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
    // A file that starts with zeros may still hold a recording further in (a piece carved
    // from a card image). The index walk scans for it; an all-zero file is reported after.
    if let Some(detail) = playable_mp4(src) {
        return Err(Error::AlreadyPlayable {
            path: path.to_path_buf(),
            detail,
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

/// Recognizes a finished MP4/MOV (renamed to `.DAT`): `ftyp`, a `moov` with at least one
/// track, and an `mdat` with data. Returns a short technical description.
pub fn playable_mp4<R: ReadAt + ?Sized>(src: &R) -> Option<String> {
    let boxes = crate::mp4::atom::scan_top_level(src).ok()?;
    let ftyp = boxes.iter().find(|b| b.kind.0 == *b"ftyp")?;
    let moov = boxes.iter().find(|b| b.kind.0 == *b"moov")?;
    let mdat = boxes.iter().find(|b| b.kind.0 == *b"mdat")?;
    let len = src.size();
    // A moov that is cut short or an empty mdat means the file is not complete.
    if moov.offset + moov.size > len || mdat.size <= mdat.header_len || moov.size > 512 << 20 {
        return None;
    }
    let bytes = src.read_vec(moov.offset, moov.size as usize).ok()?;
    let atoms = crate::mp4::atom::parse_atoms(&bytes).ok()?;
    let tracks = atoms
        .first()?
        .children()
        .iter()
        .filter(|a| a.kind.0 == *b"trak")
        .count();
    if tracks == 0 {
        return None;
    }
    let mut brand = [0u8; 4];
    src.read_exact_at(&mut brand, ftyp.offset + 8).ok()?;
    Some(format!(
        "file type '{}', {tracks} track(s), {} bytes of media",
        String::from_utf8_lossy(&brand),
        mdat.size - mdat.header_len
    ))
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

/// Everything decided before the output is written: settings, sample tables, the new `moov`.
pub struct RepairPlan {
    pub reference: Reference,
    pub plans: Vec<TrackPlan>,
    pub timing: VideoTiming,
    pub kind: RefKind,
    pub label: String,
    pub synthesized: Vec<String>,
    pub rebuilt: Vec<String>,
    pub settings: String,
    pub profile: Option<String>,
    pub layout: DatLayout,
    pub gaps: Vec<TimelineGap>,
    pub trimmed: Vec<u64>,
    pub damaged: u64,
    pub notes: Vec<String>,
    pub created: Option<i64>,
    pub created_source: String,
    pub truncation: Option<Truncation>,
    /// `.DAT` bytes `[copy_start, copy_end)` become the `mdat` payload, then `silence` zeros.
    pub copy_start: u64,
    pub copy_end: u64,
    pub silence: u64,
    /// The serialized `moov` box.
    pub moov: Vec<u8>,
}

impl RepairPlan {
    /// Size of the output file in bytes.
    pub fn output_size(&self) -> u64 {
        self.reference.ftyp.len() as u64
            + self.moov.len() as u64
            + 16
            + (self.copy_end - self.copy_start)
            + self.silence
    }

    /// Where the payload starts in the output file.
    pub fn payload_offset(&self) -> u64 {
        self.reference.ftyp.len() as u64 + self.moov.len() as u64 + 16
    }

    /// Seconds of video lost to damaged or unreadable regions.
    pub fn lost_seconds(&self) -> f64 {
        let frames: i64 = self.gaps.iter().map(|g| g.lost_frames).sum();
        let vt = self.plans.iter().find(|p| p.repi_type.0 == *b"vide");
        vt.map_or(0.0, |p| {
            frames as f64 * self.reference.tracks[p.ref_track].sample_seconds()
        })
    }
}

/// Removes, or cuts short, every group whose bytes touch an unreadable region, so the
/// timeline treats the region as damage (frames lost, picture held, audio silent).
pub fn exclude_unreadable(index: &DatIndex, bad: &[Region]) -> DatIndex {
    let mut out = index.clone();
    if bad.is_empty() {
        return out;
    }
    let hit = |a: u64, b: u64| bad.iter().any(|r| r.overlaps(a, b));
    out.groups.retain_mut(|g| {
        if hit(g.header_offset, g.data_offset()) {
            return false;
        }
        if hit(g.data_offset(), g.complete_end()) {
            g.complete = g
                .samples(g.complete)
                .take_while(|&(o, s)| !hit(o, o + s as u64))
                .count() as u32;
        }
        true
    });
    out
}

/// Plans a repair of `src` (the `.DAT` bytes) without writing anything.
///
/// `dat` is the `.DAT`'s path, used for messages, its file name (creation time) and, with
/// [`ReferenceSource::Auto`], its folder. A [`ReferenceSource::File`] must be opened by the
/// caller and passed as `given`. `mtime` is the `.DAT`'s modification time (Unix seconds).
#[allow(clippy::too_many_arguments)]
pub fn plan_repair<R: ReadAt + ?Sized>(
    dat: &Path,
    src: &R,
    index: &DatIndex,
    reference: &ReferenceSource,
    given: Option<&Reference>,
    mtime: Option<i64>,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<RepairPlan> {
    let name = dat.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lay = layout::analyze(src, index, name);
    if !index
        .groups
        .iter()
        .any(|g| g.kind.0 == *b"vide" && g.complete > 0)
    {
        return Err(Error::NoFrames {
            path: dat.to_path_buf(),
            detail: if index.groups.iter().any(|g| g.kind.0 == *b"vide") {
                "the file ends inside its first video frame".into()
            } else {
                "the index lists no video samples".into()
            },
        });
    }

    on_event(&Event::Phase {
        phase: Phase::CheckingReference,
        message: format!("checking decoder settings for {}", lay.describe()),
    });
    let checked = match (given, reference) {
        (Some(r), _) => check_reference(src, index, &lay, r.clone(), RefKind::User)?,
        (None, ReferenceSource::Dir(d)) => match choose_from_dir(src, index, &lay, d, on_event) {
            Ok(c) => c,
            Err(dir_err @ Error::NoReference { .. }) => {
                match choose_profile(src, index, &lay, on_event) {
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
        (None, ReferenceSource::Builtin) => choose_profile(src, index, &lay, on_event)?,
        (None, ReferenceSource::Auto) => {
            let dirs: Vec<PathBuf> = dat.parent().map(Path::to_path_buf).into_iter().collect();
            let opts = SearchOptions {
                depth: 0,
                ..SearchOptions::default()
            };
            let found = search_references(src, index, &lay, &dirs, &opts, on_event)?;
            let (checked, chosen) = (found.checked(), found.chosen);
            match chosen {
                Some(c) => c,
                None => {
                    let mut c = choose_profile(src, index, &lay, on_event)?;
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
        (None, ReferenceSource::File(p)) => {
            check_reference(src, index, &lay, Reference::open(p)?, RefKind::User)?
        }
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
    if index.rejected_count > 0 {
        notes.push(format!(
            "{} damaged index header(s) were skipped; the samples they described are lost",
            index.rejected_count
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
    let tl = timeline::build(src, index, &lay);
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
    let (frames, damaged) = analyze_video(src, &vplan, codec, opts, on_event, &mut notes)?;
    let timing = timing::video_timing(&frames);

    let vt = &reference.tracks[vplan.ref_track];
    let duration = vplan.duration_units() as f64 * vt.sample_seconds();
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
        .unwrap_or(copy_start)
        .max(copy_start);
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
    let moov = moov.to_bytes();
    drop(outputs);
    Ok(RepairPlan {
        reference,
        plans: mapping.plans,
        timing,
        kind,
        label,
        synthesized,
        rebuilt,
        settings,
        profile,
        layout: lay,
        gaps: tl.gaps,
        trimmed,
        damaged,
        notes,
        created,
        created_source,
        truncation: index.truncation.clone(),
        copy_start,
        copy_end,
        silence,
        moov,
    })
}

/// Plans with every unreadable region known so far excluded. Reading during planning can
/// find more bad regions; plan again until none are new.
#[allow(clippy::too_many_arguments)]
fn plan_around_bad<R: ReadAt>(
    dat: &Path,
    src: &Tolerant<R>,
    index: &DatIndex,
    reference: &ReferenceSource,
    given: Option<&Reference>,
    mtime: Option<i64>,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<RepairPlan> {
    let mut tries = 0;
    loop {
        let bad = src.bad_regions();
        let index = exclude_unreadable(index, &bad);
        let plan = plan_repair(dat, src, &index, reference, given, mtime, opts, on_event)
            .map_err(|e| unreadable_error(src, e))?;
        tries += 1;
        if src.bad_regions().len() == bad.len() || tries >= 8 {
            return Ok(plan);
        }
    }
}

/// Turns the error of a read that failed because too much was unreadable into
/// [`Error::TooMuchUnreadable`].
fn unreadable_error<R: ReadAt>(src: &Tolerant<R>, e: Error) -> Error {
    if !src.exceeded() {
        return e;
    }
    let (last_offset, source) = src.last_error().expect("exceeded after an error");
    Error::TooMuchUnreadable {
        unreadable: src.bad_bytes(),
        total: src.size(),
        last_offset,
        source,
    }
}

/// Writes a planned repair to the temporary file `tmp`. Deletes it on failure.
fn write_plan<R: ReadAt + ?Sized>(
    tmp: &Path,
    plan: &RepairPlan,
    src: &R,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<u64> {
    let mut throttle = Throttle::new();
    let written = writer::write_file(
        tmp,
        &plan.reference.ftyp,
        &plan.moov,
        src,
        plan.copy_start,
        plan.copy_end,
        plan.silence,
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
    if written.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    written
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
    let file = FileSource::open(dat)
        .map_err(|e| Error::io(format!("cannot open {}", dat.display()), e))?;
    let mtime = file
        .file()
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let src = Tolerant::new(file, !opts.stop_on_read_error);
    classify_input(dat, &src).map_err(|e| unreadable_error(&src, e))?;
    let out_dir = volume::parent_dir(output);
    // Fail before a long index walk: a folder that can't be written, or a drive that can't
    // hold a file as large as the .DAT (the output is about as large).
    volume::check_writable(&out_dir)?;
    volume::check_fits(&out_dir, src.size().saturating_sub(16 << 20))?;
    // Open a single reference first so a typo fails before a long index walk.
    let given = match reference {
        ReferenceSource::File(p) => {
            if same_file(p, dat) {
                return Err(Error::NotAClip {
                    path: p.clone(),
                    what: "the broken .DAT itself".into(),
                });
            }
            Some(Reference::open(p)?)
        }
        _ => None,
    };
    let walked;
    let index: &DatIndex = match &opts.index {
        Some(i) if i.file_size == src.size() => i,
        _ => {
            walked = index_dat(&src, opts, on_event)
                .map_err(|e| unreadable_error(&src, not_a_dat(dat, &src, e)))?;
            &walked
        }
    };

    let mut plan = plan_around_bad(
        dat,
        &src,
        index,
        reference,
        given.as_ref(),
        mtime,
        opts,
        on_event,
    )?;
    volume::check_fits(&out_dir, plan.output_size())?;

    on_event(&Event::Phase {
        phase: Phase::Writing,
        message: format!("writing {}", output.display()),
    });
    let tmp = temp_path(output);
    let known_bad = src.bad_regions().len();
    let mut written =
        write_plan(&tmp, &plan, &src, opts, on_event).map_err(|e| unreadable_error(&src, e))?;

    // New unreadable regions turned up while copying: the tables still point at samples that
    // were written as zeros. Plan again around every bad region and write a second file,
    // reading the payload back from the first one instead of from the failing card.
    if src.bad_regions().len() != known_bad {
        let first = tmp.with_file_name(format!(
            "{}.pass1",
            tmp.file_name().map_or("".into(), |n| n.to_string_lossy())
        ));
        let cleanup = |e: Error| {
            let _ = std::fs::remove_file(&first);
            e
        };
        std::fs::rename(&tmp, &first)
            .map_err(|e| Error::io(format!("cannot rename {}", tmp.display()), e))?;
        let copy = FileSource::open(&first)
            .map_err(|e| cleanup(Error::io(format!("cannot read {}", first.display()), e)))?;
        let overlay = Overlay {
            base: &src,
            copy,
            start: plan.copy_start,
            end: plan.copy_end,
            copy_offset: plan.payload_offset(),
        };
        let index = exclude_unreadable(index, &src.bad_regions());
        on_event(&Event::Note {
            message: format!(
                "{} unreadable region(s) found while copying; rebuilding the tables around them",
                src.bad_regions().len() - known_bad
            ),
        });
        let plan2 = plan_repair(
            dat,
            &overlay,
            &index,
            reference,
            given.as_ref(),
            mtime,
            opts,
            on_event,
        )
        .map_err(cleanup)?;
        volume::check_fits(&out_dir, plan2.output_size()).map_err(cleanup)?;
        written = write_plan(&tmp, &plan2, &overlay, opts, on_event).map_err(cleanup)?;
        drop(overlay);
        let _ = std::fs::remove_file(&first);
        plan = plan2;
    }

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

    let unreadable = src.bad_regions();
    let unreadable_bytes: u64 = unreadable.iter().map(|r| r.len).sum();
    let lost_seconds = plan.lost_seconds();
    if unreadable_bytes > 0 {
        plan.notes.push(format!(
            "{unreadable_bytes} bytes in {} region(s) could not be read from the card; they are \
             treated as damage ({lost_seconds:.2} s of video lost in damaged or unreadable parts)",
            unreadable.len()
        ));
    }
    let plan = plan;
    let mut tracks: Vec<TrackReport> = plan
        .plans
        .iter()
        .zip(&plan.trimmed)
        .map(|(p, &trim)| {
            let rt = &plan.reference.tracks[p.ref_track];
            TrackReport {
                track_id: rt.track_id,
                handler: rt.handler,
                codec: rt.codec,
                source: p.repi_type,
                samples: p.sample_count,
                duration_seconds: (p.duration_units() * rt.sample_delta as u64) as f64
                    / rt.timescale as f64,
                trimmed_samples: trim,
            }
        })
        .collect();
    tracks.sort_by_key(|t| t.track_id);
    let duration_seconds = tracks
        .iter()
        .find(|t| t.source.0 == *b"vide")
        .map_or(0.0, |t| t.duration_seconds);
    let video_frames = plan
        .plans
        .iter()
        .find(|p| p.repi_type.0 == *b"vide")
        .map_or(0, |p| p.sample_count);
    Ok(RepairReport {
        input: dat.to_path_buf(),
        reference: PathBuf::from(plan.label),
        reference_kind: plan.kind,
        synthesized: plan.synthesized,
        rebuilt: plan.rebuilt,
        settings: plan.settings,
        profile: plan.profile,
        camera: plan.layout.camera(),
        recording: plan.layout.describe(),
        creation_time: plan.created,
        creation_time_source: plan.created_source,
        gaps: plan.gaps,
        output: output.to_path_buf(),
        bytes_written: written,
        duration_seconds,
        video_frames,
        keyframes: plan.timing.sync_samples.len() as u64,
        reordered: plan.timing.reordered,
        damaged_frames: plan.damaged,
        tracks,
        truncation: plan.truncation,
        unreadable,
        unreadable_bytes,
        lost_seconds,
        notes: plan.notes,
    })
}

/// True when two paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Turns "no index found" into a clearer error when the file is empty or all zeros.
pub(crate) fn not_a_dat<R: ReadAt + ?Sized>(dat: &Path, src: &R, e: Error) -> Error {
    if !matches!(e, Error::NotADat(_)) {
        return e;
    }
    if is_all_zero(src).unwrap_or(false) {
        return Error::EmptyDat {
            path: dat.to_path_buf(),
            detail: "only zero bytes".into(),
        };
    }
    let len = src.size();
    let mut head = [0u8; 16];
    let n = len.min(16) as usize;
    if src.read_exact_at(&mut head[..n], 0).is_ok()
        && head[..n.min(8)] == [0, 0, 0, 0, b'm', b'd', b'a', b't'][..n.min(8)]
        && len < 64 * 1024
    {
        return Error::NoFrames {
            path: dat.to_path_buf(),
            detail: format!("the file is only {len} bytes, shorter than one group of samples"),
        };
    }
    e
}

/// True when every byte of `src` is zero (reads in 8 MiB blocks; stops at the first non-zero).
fn is_all_zero<R: ReadAt + ?Sized>(src: &R) -> std::io::Result<bool> {
    let len = src.size();
    let mut buf = vec![0u8; (8 << 20).min(len as usize)];
    let mut pos = 0u64;
    while pos < len {
        let n = ((len - pos) as usize).min(buf.len());
        src.read_exact_at(&mut buf[..n], pos)?;
        if buf[..n].iter().any(|&b| b != 0) {
            return Ok(false);
        }
        pos += n as u64;
    }
    Ok(true)
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
