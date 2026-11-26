//! Choosing decoder settings for a `.DAT`: a clip the user gave, a folder of clips, or the
//! built-in profile bank.
//!
//! Every candidate goes through the same two steps:
//!
//! 1. **Video gate** ([`video_gate`]). The candidate's video must fit the `.DAT`: same frame
//!    rate (measured from the `.DAT`'s own group cadence), same `repi` video layout, compatible
//!    GOP structure, and every probed frame must pass a full slice-header check against the
//!    candidate's parameter sets ([`crate::codec::verify_frame`]).
//! 2. **Track mapping.** If the candidate's other tracks fit as well, it is used as it is.
//!    Otherwise a hybrid is assembled ([`crate::hybrid`]): the candidate's video track plus
//!    synthesized audio, timecode and metadata tracks.
//!
//! A reference given by the user always wins over the profile bank.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use crate::codec::{self, FrameAnalyzer, VideoCodec, verify_frame};
use crate::error::{Error, Reason, Result};
use crate::hybrid;
use crate::layout::{DatLayout, fps_text, friendly_body};
use crate::mapping::{self, Mapping, TrackPlan};
use crate::mp4::reference::{Reference, TrackKind};
use crate::mp4::synth;
use crate::mp4::writer::{self, TrackOutput};
use crate::profile::{self, Profile};
use crate::read_at::ReadAt;
use crate::repair::Event;
use crate::repi::DatIndex;
use crate::timing;

/// Canon's `uuid` box in `moov` (holds `CNCV` firmware string and the `CNTH` thumbnail).
pub const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
/// Folder levels searched below `--reference-dir`.
pub const SEARCH_DEPTH: u32 = 3;
/// Candidates checked at most in a folder search.
const MAX_CANDIDATES: usize = 300;
/// Video groups whose frames are fully checked: the first two and one from the middle.
const GATE_GROUPS: usize = 2;
/// Frames written for an ffmpeg trial decode.
const FFMPEG_FRAMES: u64 = 90;

/// Where the decoder settings came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// A healthy clip whose every track fits the `.DAT`.
    User,
    /// A healthy clip's video track plus synthesized tracks.
    Hybrid,
    /// A built-in profile (possibly with synthesized tracks).
    Profile,
}

/// A reference that passed the checks.
pub struct Checked {
    pub reference: Reference,
    pub mapping: Mapping,
    pub codec: VideoCodec,
    pub kind: RefKind,
    /// Human name: the clip path or `built-in profile <id>`.
    pub label: String,
    /// Tracks that were synthesized instead of copied (technical names).
    pub synthesized: Vec<String>,
    /// What was rebuilt and why, in plain sentences.
    pub rebuilt: Vec<String>,
    /// Where the settings come from, as one plain sentence, for example
    /// `Using built-in settings for Canon EOS R5 Mark II, 4K 119.88p.`
    pub summary: String,
    /// The built-in profile in plain words, when one was used.
    pub profile: Option<String>,
    pub notes: Vec<String>,
}

/// Frames checked by the video gate: every frame of the first groups plus one middle group.
fn gate_frames(index: &DatIndex) -> Vec<(u64, u32)> {
    let video: Vec<_> = index
        .groups
        .iter()
        .filter(|g| g.kind.0 == *b"vide" && g.complete > 0)
        .collect();
    let mut picks: Vec<usize> = (0..video.len().min(GATE_GROUPS)).collect();
    if video.len() > GATE_GROUPS + 1 {
        picks.push(video.len() / 2);
    }
    picks
        .into_iter()
        .flat_map(|i| video[i].samples(video[i].complete).collect::<Vec<_>>())
        .collect()
}

/// Describes the video settings of a codec configuration.
pub fn describe_codec(codec: &VideoCodec) -> String {
    match codec {
        VideoCodec::Hevc(c) => format!("HEVC {}", c.sps[0].describe()),
        VideoCodec::Avc(c) => format!("H.264 {}x{}", c.sps[0].width, c.sps[0].height),
    }
}

const LAYOUT_NOT_FOLLOWED: &str =
    "Its settings fit, but DATfix could not follow how this .DAT is laid out.";

const NOT_LIKE_FRAMES: &str =
    "Its picture settings (resolution, colour or compression) don't fit the frames in this .DAT.";
const ALL_I_VS_IPB: &str = "It was recorded with ALL-I compression; this .DAT uses IPB (Long GOP).";

/// Checks that the candidate's video fits the `.DAT`. Returns the codec and notes, or the
/// reasons it does not fit.
pub fn video_gate<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    reference: &Reference,
) -> std::result::Result<(VideoCodec, Vec<String>), Vec<Reason>> {
    let mut notes = Vec::new();
    let no_video = || {
        vec![Reason::new(
            "The clip has no video.",
            "it has no video track",
        )]
    };
    let vt = reference.video_track().ok_or_else(no_video)?;
    let vf = vt.video.as_ref().ok_or_else(no_video)?;
    let codec = VideoCodec::from_config(vt.codec, vf.config_kind, &vf.config)
        .map_err(|e| vec![Reason::new("Its video settings can't be read.", e)])?;
    if let Some(c) = layout.codec {
        let same = (c.0 == *b"avc1") == matches!(codec, VideoCodec::Avc(_));
        if !same {
            let dat = if c.0 == *b"avc1" { "H.264" } else { "HEVC" };
            return Err(vec![Reason::new(
                format!(
                    "It was recorded as {} video; this .DAT holds {dat} video.",
                    codec.name()
                ),
                format!(
                    "codec differs: the .DAT holds {dat} frames, the clip is {}",
                    codec.name()
                ),
            )]);
        }
    }
    let ref_fps = vt.timescale as f64 / vt.sample_delta as f64;
    if let Some(rate) = layout.frame_rate {
        if !rate.matches(ref_fps) {
            return Err(vec![Reason::new(
                format!(
                    "It was recorded at {} frames per second, but this .DAT was recorded at {}.",
                    fps_text(ref_fps),
                    fps_text(rate.fps())
                ),
                format!(
                    "frame rate differs: the .DAT was recorded at {rate} ({} frames per 0.5 s \
                     group{}), the clip at {ref_fps:.3}p",
                    layout.frames_per_group,
                    if layout.frame_rate_from_audio {
                        ", measured against its audio"
                    } else {
                        ""
                    }
                ),
            )]);
        }
    }
    if let Some(shapes) = &reference.repi_shapes {
        if let Some(v) = shapes.iter().find(|s| s.kind.0 == *b"vide") {
            if v.typical_samples > 0
                && layout.frames_per_group > 0
                && v.typical_samples != layout.frames_per_group
            {
                return Err(vec![Reason::new(
                    "It was recorded in a different recording mode on the camera.",
                    format!(
                        "recording mode differs: the clip stores {} video frames per index \
                         group, the .DAT stores {}",
                        v.typical_samples, layout.frames_per_group
                    ),
                )]);
            }
            if v.align != 0 {
                let dat_align = layout.shape(b"vide").map_or(1, |s| s.align);
                if dat_align != v.align {
                    return Err(vec![Reason::new(
                        "It comes from a different camera model or recording format.",
                        format!(
                            "frame padding differs ({} vs {} bytes): another camera family",
                            v.align, dat_align
                        ),
                    )]);
                }
            }
        }
    }
    let ref_long = vt.has_stss || vt.has_ctts;
    let gop_reason = || {
        vec![Reason::new(
            ALL_I_VS_IPB,
            "the .DAT is Long GOP (it has non-key frames) but the clip is All-Intra",
        )]
    };
    if layout.long_gop == Some(true) && !ref_long {
        return Err(gop_reason());
    }

    // Every probed frame must pass the full header check.
    let frames = gate_frames(index);
    for (i, &(off, size)) in frames.iter().enumerate() {
        if let Err(e) = verify_frame(src, &codec, off, size) {
            // The first frames read fine with these settings, and this one is not even cut
            // into whole NAL units: the sample boundaries are wrong, not the settings.
            if i > 0 && !codec::nal_chain_fits(src, codec.length_size(), off, size) {
                return Err(vec![Reason::engine(
                    LAYOUT_NOT_FOLLOWED,
                    format!(
                        "frame {} (byte {off}, {size} bytes) is not cut at a frame boundary, \
                         while the {i} frame(s) before it read correctly with these settings: {e}",
                        i + 1
                    ),
                )]);
            }
            return Err(vec![Reason::new(
                NOT_LIKE_FRAMES,
                format!(
                    "the video frames do not fit the clip's settings ({}): frame at byte {off}: \
                     {e}",
                    describe_codec(&codec)
                ),
            )]);
        }
    }
    // POC parse of the first frames (the analyzer used for the tables).
    let mut analyzer = FrameAnalyzer::new(codec.clone());
    let mut non_key = false;
    for &(off, size) in frames.iter().take(60) {
        match analyzer.analyze(src, off, size) {
            Ok(f) => non_key |= !f.sync && !f.cra,
            Err(e) => {
                // Every one of these frames passed the full header check above, so the
                // timing parser is at fault, not the settings.
                return Err(vec![Reason::engine(
                    LAYOUT_NOT_FOLLOWED,
                    format!(
                        "the video frames cannot be read with the clip's {} settings: {e}",
                        codec.name()
                    ),
                )]);
            }
        }
    }
    if non_key && !ref_long {
        return Err(gop_reason());
    }
    if let (Some(body), Some(theirs)) = (
        layout.body,
        reference.moov.child(b"udta").and_then(synth::model_of),
    ) {
        if body != theirs {
            notes.push(format!(
                "the .DAT comes from a {}, the reference from a {}; their video settings match",
                friendly_body(body),
                friendly_body(&theirs)
            ));
        }
    }
    Ok((codec, notes))
}

/// Checks a candidate: video gate, then the full track mapping, then a hybrid.
pub fn check_reference<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    reference: Reference,
    kind: RefKind,
) -> Result<Checked> {
    let label = reference.path.display().to_string();
    let name = reference
        .path
        .file_name()
        .map_or_else(|| label.clone(), |n| n.to_string_lossy().into_owned());
    let subject = if kind == RefKind::Profile {
        "the built-in settings for this camera"
    } else {
        "your reference clip"
    };
    let incompatible = |path: &Path, reasons: Vec<Reason>| Error::Incompatible {
        path: path.to_path_buf(),
        reasons,
    };
    let rejected = |path: &Path, reasons: Vec<Reason>| match reasons.iter().find(|r| r.engine) {
        Some(r) => Error::LayoutNotSupported {
            detail: r.detail.clone(),
        },
        None => incompatible(path, reasons),
    };
    let (codec, mut notes) = match video_gate(src, index, layout, &reference) {
        Ok(x) => x,
        Err(reasons) => return Err(rejected(&reference.path, reasons)),
    };
    match mapping::map_tracks(index, &reference) {
        Ok(mapping) => Ok(Checked {
            reference,
            mapping,
            codec,
            kind,
            summary: format!("Using the settings from your clip {name}."),
            label,
            synthesized: Vec::new(),
            rebuilt: Vec::new(),
            profile: None,
            notes,
        }),
        Err(exact) => {
            let path = reference.path.clone();
            let h = hybrid::assemble(src, &reference, index, layout, subject).map_err(|e| {
                let mut detail = exact.clone();
                detail.push(e.detail.clone());
                rejected(
                    &path,
                    vec![Reason {
                        detail: detail.join("; "),
                        ..e
                    }],
                )
            })?;
            // The synthesized tracks follow the .DAT's own layout by construction; when they
            // still do not map, the engine is at fault.
            let mapping = mapping::map_tracks(index, &h.reference).map_err(|mut r| {
                r.splice(0..0, exact.iter().cloned());
                Error::LayoutNotSupported {
                    detail: format!("the rebuilt tracks do not fit the index: {}", r.join("; ")),
                }
            })?;
            let mut why = exact.clone();
            why.dedup();
            notes.push(format!(
                "the reference's video settings fit, but not all of its other tracks ({}). \
                 Its video track is used with synthesized {}{}",
                why.join("; "),
                if h.synthesized.is_empty() {
                    "file type".to_string()
                } else {
                    h.synthesized.join(", ")
                },
                if h.reused.is_empty() {
                    String::new()
                } else {
                    format!("; reused: {}", h.reused.join(", "))
                }
            ));
            Ok(Checked {
                reference: h.reference,
                mapping,
                codec,
                kind: if kind == RefKind::Profile {
                    RefKind::Profile
                } else {
                    RefKind::Hybrid
                },
                summary: format!("Using the video settings from your clip {name}."),
                label,
                synthesized: h.synthesized,
                rebuilt: h.rebuilt,
                profile: None,
                notes,
            })
        }
    }
}

const NOT_ORIGINAL: &str =
    "It's an edited or converted copy, not the original file from the camera.";

/// Opens a candidate clip and rejects files that cannot be camera originals.
pub fn open_candidate(path: &Path) -> std::result::Result<Reference, Reason> {
    let r = Reference::open(path).map_err(|e| match e {
        Error::NotAClip { what, .. } => Reason::new(
            format!("It is {what}."),
            format!("not a video clip: {what}"),
        ),
        other => {
            let detail = match other {
                Error::BadReference { reason, .. } => reason,
                other => other.to_string(),
            };
            Reason::new(
                "It can't be read as a video file (it may be damaged or not fully copied).",
                detail,
            )
        }
    })?;
    let brands: Vec<String> = r
        .ftyp
        .get(8..)
        .unwrap_or_default()
        .chunks_exact(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    let vt = r.video_track();
    if let Some(v) = vt {
        match &v.codec.0 {
            b"av01" => {
                return Err(Reason::new(
                    NOT_ORIGINAL,
                    "AV1 re-encode (not a camera original)",
                ));
            }
            b"hev1" => {
                return Err(Reason::new(
                    NOT_ORIGINAL,
                    "hev1 HEVC (a remux or re-encode; Canon writes hvc1 with settings in the \
                     header)",
                ));
            }
            b"hvc1" | b"avc1" => {}
            other => {
                return Err(Reason::new(
                    "Its video format isn't one that Canon cameras record.",
                    format!(
                        "video codec {} is not one Canon writes",
                        String::from_utf8_lossy(other)
                    ),
                ));
            }
        }
    }
    let canon_brand = brands.iter().any(|b| b == "CAEP" || b == "XFVC");
    let canon_uuid =
        r.moov.children().iter().any(|a| {
            a.kind.0 == *b"uuid" && a.payload().is_some_and(|p| p.starts_with(&CANON_UUID))
        });
    if !canon_brand && !canon_uuid {
        return Err(Reason::new(
            "It isn't an original file from a Canon camera (it looks exported or converted).",
            format!(
                "not a Canon camera original (file type {}, no Canon metadata box): probably \
                 exported or re-encoded",
                brands.join("/")
            ),
        ));
    }
    if vt.is_none() || r.tracks.iter().all(|t| t.kind != TrackKind::Video) {
        return Err(Reason::new("The clip has no video.", "no video track"));
    }
    Ok(r)
}

/// How a folder search runs.
#[derive(Clone, Copy, Debug)]
pub struct SearchOptions<'a> {
    /// Folder levels searched below each folder (0 = only the folder itself).
    pub depth: u32,
    /// Clips checked at most.
    pub max_candidates: usize,
    /// Set from another thread to stop the search with [`Error::Cancelled`].
    pub cancel: Option<&'a AtomicBool>,
}

impl Default for SearchOptions<'_> {
    fn default() -> Self {
        SearchOptions {
            depth: SEARCH_DEPTH,
            max_candidates: MAX_CANDIDATES,
            cancel: None,
        }
    }
}

/// What a search decided about one clip.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Every track fits.
    Fits,
    /// The video fits; the other tracks would be rebuilt (plain sentences).
    VideoOnly { rebuilt: Vec<String> },
    /// The clip cannot be used.
    Skipped { reason: Reason },
}

/// One checked clip.
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub path: PathBuf,
    pub verdict: Verdict,
}

/// Result of [`search_references`].
pub struct ReferenceSearch {
    /// `.MP4` files found in the folders (before the [`SearchOptions::max_candidates`] limit).
    pub found: usize,
    /// Every clip checked, in the order checked, with its verdict.
    pub candidates: Vec<Candidate>,
    /// The best clip: the first whose every track fits, else the first whose video fits.
    pub chosen: Option<Checked>,
}

impl ReferenceSearch {
    /// Clips checked.
    pub fn checked(&self) -> usize {
        self.candidates.len()
    }

    /// The skipped clips and why, for [`Error::NoReference`].
    pub fn skipped(&self) -> Vec<(PathBuf, Reason)> {
        self.candidates
            .iter()
            .filter_map(|c| match &c.verdict {
                Verdict::Skipped { reason } => Some((c.path.clone(), reason.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Searches folders for a clip that fits the `.DAT` and reports a verdict for every clip it
/// checked. Shallow clips first, then small ones (cheap to open, same settings). The search
/// stops at the first clip whose every track fits; a clip whose only video fits is kept as a
/// fallback while the search goes on.
pub fn search_references<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    dirs: &[PathBuf],
    opts: &SearchOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<ReferenceSearch> {
    let mut keyed: Vec<(usize, u64, PathBuf)> = Vec::new();
    for dir in dirs {
        let mut files = Vec::new();
        profile::collect_mp4s(dir, opts.depth, &mut files)
            .map_err(|e| Error::io(format!("cannot list {}", dir.display()), e))?;
        for p in files {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            // Our own outputs are never camera originals.
            if name.contains("_repaired") {
                continue;
            }
            let depth = p.strip_prefix(dir).map_or(0, |r| r.components().count());
            let size = std::fs::metadata(&p).map_or(u64::MAX, |m| m.len());
            keyed.push((depth, size, p));
        }
    }
    keyed.sort();
    keyed.dedup_by(|a, b| a.2 == b.2);
    let found = keyed.len();
    let mut candidates = Vec::new();
    let mut hybrid: Option<Checked> = None;
    let mut reject = |path: PathBuf, reason: Reason, candidates: &mut Vec<Candidate>| {
        on_event(&Event::ReferenceRejected {
            path: path.clone(),
            reason: reason.detail.clone(),
            user_reason: reason.user.clone(),
        });
        candidates.push(Candidate {
            path,
            verdict: Verdict::Skipped { reason },
        });
    };
    for (_, _, path) in keyed.into_iter().take(opts.max_candidates) {
        if opts.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let reference = match open_candidate(&path) {
            Ok(r) => r,
            Err(reason) => {
                reject(path, reason, &mut candidates);
                continue;
            }
        };
        match check_reference(src, index, layout, reference, RefKind::User) {
            Ok(c) if c.kind == RefKind::User => {
                candidates.push(Candidate {
                    path: path.clone(),
                    verdict: Verdict::Fits,
                });
                on_event(&Event::ReferenceChosen { path });
                return Ok(ReferenceSearch {
                    found,
                    candidates,
                    chosen: Some(c),
                });
            }
            Ok(c) => {
                candidates.push(Candidate {
                    path,
                    verdict: Verdict::VideoOnly {
                        rebuilt: c.rebuilt.clone(),
                    },
                });
                if hybrid.is_none() {
                    hybrid = Some(c);
                }
            }
            Err(e) => {
                let reason = match e {
                    Error::Incompatible { reasons, .. } => Reason::new(
                        reasons
                            .iter()
                            .map(|r| r.user.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        reasons
                            .iter()
                            .map(|r| r.detail.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                    other @ Error::LayoutNotSupported { .. } => {
                        Reason::engine(LAYOUT_NOT_FOLLOWED, other.to_string())
                    }
                    other => Reason::new(other.user_message().message, other.to_string()),
                };
                reject(path, reason, &mut candidates);
            }
        }
    }
    if let Some(c) = &hybrid {
        on_event(&Event::ReferenceChosen {
            path: c.reference.path.clone(),
        });
    }
    Ok(ReferenceSearch {
        found,
        candidates,
        chosen: hybrid,
    })
}

/// Searches `dir` (recursively, [`SEARCH_DEPTH`] levels) for a fitting clip. A clip whose
/// every track fits wins over a hybrid. Rejected candidates are reported with the reason.
pub fn choose_from_dir<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    dir: &Path,
    on_event: &mut dyn FnMut(&Event),
) -> Result<Checked> {
    let dirs = [dir.to_path_buf()];
    let search = search_references(
        src,
        index,
        layout,
        &dirs,
        &SearchOptions::default(),
        on_event,
    )?;
    let tried = search.skipped();
    search.chosen.ok_or_else(|| {
        layout_fault(tried.iter().map(|(_, r)| r)).unwrap_or_else(|| Error::NoReference {
            dir: dir.to_path_buf(),
            tried,
        })
    })
}

/// When any candidate failed through DATfix's own fault, the error that says so.
pub fn layout_fault<'a>(reasons: impl IntoIterator<Item = &'a Reason>) -> Option<Error> {
    reasons
        .into_iter()
        .find(|r| r.engine)
        .map(|r| Error::LayoutNotSupported {
            detail: r.detail.clone(),
        })
}

/// Key that is equal for two configurations that decode the same way (colour tags and other
/// VUI fields are ignored).
fn decode_key(codec: &VideoCodec) -> String {
    match codec {
        VideoCodec::Hevc(c) => format!("{:?}{:?}{}", c.sps, c.pps, c.length_size),
        VideoCodec::Avc(c) => format!("{:?}{:?}{}", c.sps, c.pps, c.length_size),
    }
}

/// Picks a built-in profile for the `.DAT`.
pub fn choose_profile<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    on_event: &mut dyn FnMut(&Event),
) -> Result<Checked> {
    choose_profile_from(src, index, layout, profile::bank(), on_event)
}

/// [`choose_profile`] over an explicit list (tests use their own bank).
pub fn choose_profile_from<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    bank: &[Profile],
    on_event: &mut dyn FnMut(&Event),
) -> Result<Checked> {
    let summary = layout.describe();
    let camera = layout.describe_plain();
    let mut tried: Vec<(String, String)> = Vec::new();
    let code = layout.body_code.map(|c| crate::layout::hex(&c));
    let mut candidates: Vec<&Profile> = Vec::new();
    for p in bank {
        let why = if code.is_some() && p.body_code.is_some() && p.body_code != code {
            Some(format!("other body ({})", p.body))
        } else if layout.codec.is_some_and(|c| c.to_string() != p.codec) {
            Some(format!("other codec ({})", p.codec))
        } else if layout
            .frame_rate
            .is_some_and(|r| !r.matches(p.frame_rate().fps()))
        {
            Some(format!("other frame rate ({})", p.frame_rate()))
        } else {
            None
        };
        match why {
            Some(w) => tried.push((p.id.clone(), w)),
            None => candidates.push(p),
        }
    }
    let mut passing: Vec<(&Profile, Checked)> = Vec::new();
    let mut engine_fault: Option<Error> = None;
    for p in candidates {
        let reference = match p.to_reference() {
            Ok(r) => r,
            Err(e) => {
                tried.push((p.id.clone(), e));
                continue;
            }
        };
        match check_reference(src, index, layout, reference, RefKind::Profile) {
            Ok(mut c) => {
                c.label = format!("built-in profile {}", p.id);
                passing.push((p, c));
            }
            Err(e) => {
                if matches!(e, Error::LayoutNotSupported { .. }) && engine_fault.is_none() {
                    engine_fault = Some(Error::LayoutNotSupported {
                        detail: format!("built-in profile {}: {e}", p.id),
                    });
                }
                let reason = match &e {
                    Error::Incompatible { reasons, .. } => reasons
                        .iter()
                        .map(|r| r.detail.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                    other => other.to_string(),
                };
                tried.push((p.id.clone(), reason));
            }
        }
    }
    if passing.is_empty() {
        if let Some(e) = engine_fault {
            return Err(e);
        }
        // Only list the near misses: profiles of this body (or all when the body is unknown).
        tried.sort();
        return Err(Error::NoProfile {
            summary,
            camera,
            tried,
        });
    }
    // Group decode-equivalent settings.
    let mut keys: Vec<String> = Vec::new();
    for (_, c) in &passing {
        let k = decode_key(&c.codec);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut notes = Vec::new();
    if keys.len() > 1 {
        // Let ffmpeg decide when it is installed: keep the settings that decode cleanly.
        let mut decodes = Vec::new();
        for k in &keys {
            let (_, c) = passing
                .iter()
                .find(|(_, c)| decode_key(&c.codec) == *k)
                .expect("key from passing");
            if let Some(Ok(())) = ffmpeg_check(src, c) {
                decodes.push(k.clone());
            }
        }
        if decodes.len() != 1 {
            let candidates = passing
                .iter()
                .map(|(p, _)| format!("{} ({})", p.id, p.describe()))
                .collect();
            return Err(Error::AmbiguousProfile {
                summary,
                camera,
                candidates,
            });
        }
        notes.push(format!(
            "{} profiles fit the frames; ffmpeg decoded only one of them cleanly",
            keys.len()
        ));
        keys = decodes;
    }
    let mut same: Vec<(&Profile, Checked)> = passing
        .into_iter()
        .filter(|(_, c)| decode_key(&c.codec) == keys[0])
        .collect();
    // Prefer no synthesized tracks, then the most common settings.
    same.sort_by_key(|(p, c)| (c.synthesized.len(), std::cmp::Reverse(p.sources)));
    if same.len() > 1 {
        notes.push(format!(
            "{} built-in profiles fit and decode the same way; they differ only in colour tagging \
             or similar header details. Using the most common one ({}); others: {}",
            same.len(),
            same[0].0.id,
            same[1..]
                .iter()
                .map(|(p, _)| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let (p, mut chosen) = same.swap_remove(0);
    let plain = p.plain();
    chosen.summary = format!("Using built-in settings for {plain}.");
    chosen.profile = Some(plain);
    chosen.notes.insert(
        0,
        format!(
            "no reference given; using built-in profile {} ({})",
            p.id,
            p.describe()
        ),
    );
    chosen.notes.extend(notes);
    on_event(&Event::ReferenceChosen {
        path: PathBuf::from(&chosen.label),
    });
    Ok(chosen)
}

/// Finds `ffmpeg` on `PATH`.
pub fn ffmpeg_path() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// Writes the first frames with the candidate's video settings to a temporary MP4 and asks
/// ffmpeg to decode them. `None` when ffmpeg is not installed.
pub fn ffmpeg_check<R: ReadAt + ?Sized>(
    src: &R,
    c: &Checked,
) -> Option<std::result::Result<(), String>> {
    let ffmpeg = ffmpeg_path()?;
    Some(ffmpeg_check_with(&ffmpeg, src, c))
}

fn ffmpeg_check_with<R: ReadAt + ?Sized>(
    ffmpeg: &Path,
    src: &R,
    c: &Checked,
) -> std::result::Result<(), String> {
    let mut plan: TrackPlan = c.mapping.video().clone();
    plan.truncate(FFMPEG_FRAMES.min(plan.sample_count));
    let locations: Vec<(u64, u32)> = plan.sample_locations().collect();
    let mut analyzer = FrameAnalyzer::new(c.codec.clone());
    let frames = locations
        .iter()
        .map(|&(o, s)| analyzer.analyze(src, o, s))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let timing = timing::video_timing(&frames);
    let vt = &c.reference.tracks[plan.ref_track];
    let outputs = [TrackOutput {
        ref_track: vt,
        plan: &plan,
        video: Some(&timing),
    }];
    let copy_start = plan.chunks.first().map_or(0, |ch| ch.offset);
    let copy_end = plan.end_offset();
    let (probe, _) = writer::build_moov(&c.reference, &outputs, 0, 0)?;
    let payload_start = c.reference.ftyp.len() as u64 + probe.encoded_len() + 16;
    let delta = payload_start as i64 - copy_start as i64;
    let (moov, _) = writer::build_moov(&c.reference, &outputs, delta, 0)?;
    let tmp = std::env::temp_dir().join(format!(
        "datfix-probe-{}-{}.mp4",
        std::process::id(),
        copy_start
    ));
    let _ = std::fs::remove_file(&tmp);
    writer::write_file(
        &tmp,
        &c.reference.ftyp,
        &moov.to_bytes(),
        src,
        copy_start,
        copy_end,
        0,
        &mut |_, _| true,
    )
    .map_err(|e| e.to_string())?;
    let out = Command::new(ffmpeg)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&tmp)
        .args(["-f", "null", "-"])
        .output();
    let _ = std::fs::remove_file(&tmp);
    let out = out.map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() && err.trim().is_empty() {
        Ok(())
    } else {
        Err(err.lines().next().unwrap_or("ffmpeg failed").to_string())
    }
}
