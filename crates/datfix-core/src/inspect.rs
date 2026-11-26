//! A quick summary of a `.DAT` without a reference clip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fourcc::FourCC;
use crate::layout::{self, DatLayout};
use crate::read_at::{FileSource, Tolerant};
use crate::repair::{Event, RepairOptions, classify_input, index_dat, not_a_dat};
use crate::repi::{DatIndex, Truncation};

/// Canon cameras record audio at 48 kHz. Used only for the duration estimate.
const ASSUMED_AUDIO_RATE: f64 = 48_000.0;

/// Totals for one `repi` sample type.
#[derive(Clone, Debug, Serialize)]
pub struct TypeSummary {
    pub kind: FourCC,
    pub groups: u64,
    /// Complete samples (incomplete tail samples excluded).
    pub samples: u64,
    pub bytes: u64,
    /// Size of every sample, when fixed.
    pub fixed_size: Option<u32>,
    /// Most common number of samples per group.
    pub typical_group: u32,
}

/// Result of [`inspect`].
#[derive(Clone, Debug, Serialize)]
pub struct InspectReport {
    pub path: PathBuf,
    pub file_size: u64,
    pub has_canon_header: bool,
    pub groups: u64,
    pub types: Vec<TypeSummary>,
    /// Types of the groups in the first interval, for example `tmcd tdmt lpcm lpcm lpcm lpcm vide`.
    pub pattern: Vec<FourCC>,
    pub gap_count: u64,
    pub gap_bytes: u64,
    /// The most common gap sizes as `(size, count)`.
    pub common_gaps: Vec<(u64, u64)>,
    pub rejected_headers: u64,
    pub trailing_bytes: u64,
    pub truncation: Option<Truncation>,
    /// Estimated duration: complete video frames divided by the frame rate. Before the layout
    /// is known ([`summarize`] alone), estimated from 48 kHz PCM audio instead.
    pub estimated_seconds: Option<f64>,
    /// Complete video frames in the index.
    pub video_frames: u64,
    /// The `ftyp` brand of this recording family (`XFVC` or `CAEP`), when known.
    pub brand: Option<String>,
    /// The camera as people know it (`Canon EOS R5 Mark II`), from the body code in the
    /// per-frame records.
    pub camera: Option<String>,
    /// The body code as hex (`006049`), when the records carry one.
    pub body_code: Option<String>,
    /// Estimated video frame rate from the same data.
    pub estimated_fps: Option<f64>,
    /// Body, codec, frame rate and family as read from the `.DAT`.
    pub layout: Option<DatLayout>,
}

/// Builds the summary from an index.
pub fn summarize(path: &Path, index: &DatIndex) -> InspectReport {
    let mut types: Vec<TypeSummary> = Vec::new();
    let mut group_sizes: BTreeMap<FourCC, BTreeMap<u32, u64>> = BTreeMap::new();
    for g in &index.groups {
        let s = match types.iter_mut().find(|t| t.kind == g.kind) {
            Some(s) => s,
            None => {
                types.push(TypeSummary {
                    kind: g.kind,
                    groups: 0,
                    samples: 0,
                    bytes: 0,
                    fixed_size: (g.fixed_size > 0).then_some(g.fixed_size),
                    typical_group: 0,
                });
                types.last_mut().unwrap()
            }
        };
        s.groups += 1;
        s.samples += g.complete as u64;
        s.bytes += g.len_of(g.complete);
        *group_sizes
            .entry(g.kind)
            .or_default()
            .entry(g.declared)
            .or_default() += 1;
    }
    for t in &mut types {
        if let Some(m) = group_sizes.get(&t.kind) {
            t.typical_group = m.iter().max_by_key(|(_, c)| **c).map_or(0, |(n, _)| *n);
        }
    }

    // The first interval: everything up to and including the first video group.
    let mut pattern = Vec::new();
    for g in &index.groups {
        pattern.push(g.kind);
        if g.kind.0 == *b"vide" {
            break;
        }
    }

    let mut gaps: BTreeMap<u64, u64> = BTreeMap::new();
    for g in &index.gaps {
        *gaps.entry(g.len).or_default() += 1;
    }
    let mut common_gaps: Vec<(u64, u64)> = gaps.into_iter().collect();
    common_gaps.sort_by_key(|&(size, count)| (std::cmp::Reverse(count), size));
    common_gaps.truncate(5);

    // Duration estimate from PCM bytes. 24-bit mono groups (R5 II) use 3-byte samples; groups
    // stored as one big sample are assumed to be 16-bit stereo (4 bytes per audio frame).
    let pcm = types.iter().find(|t| t.kind.0 == *b"lpcm");
    let video = types.iter().find(|t| t.kind.0 == *b"vide");
    let lpcm_per_interval = pattern.iter().filter(|k| k.0 == *b"lpcm").count().max(1) as f64;
    let frame_bytes = |p: &TypeSummary| match p.fixed_size {
        Some(s) if s <= 8 => s as f64,
        _ => 4.0,
    };
    let estimated_seconds =
        pcm.map(|p| p.bytes as f64 / frame_bytes(p) / lpcm_per_interval / ASSUMED_AUDIO_RATE);
    let estimated_fps = match (pcm, video) {
        (Some(p), Some(v)) if p.typical_group > 0 => {
            let group_bytes = p.typical_group as f64 * p.fixed_size.unwrap_or(0) as f64;
            let span = group_bytes / frame_bytes(p) / ASSUMED_AUDIO_RATE;
            (span > 0.0).then(|| v.typical_group as f64 / span)
        }
        _ => None,
    };
    let video_frames = video.map_or(0, |v| v.samples);

    InspectReport {
        path: path.to_path_buf(),
        file_size: index.file_size,
        has_canon_header: index.has_canon_header,
        groups: index.groups.len() as u64,
        types,
        pattern,
        gap_count: index.gaps.len() as u64,
        gap_bytes: index.gaps.iter().map(|g| g.len).sum(),
        common_gaps,
        rejected_headers: index.rejected_count,
        trailing_bytes: index.trailing_bytes,
        truncation: index.truncation.clone(),
        estimated_seconds,
        video_frames,
        brand: None,
        camera: None,
        body_code: None,
        estimated_fps,
        layout: None,
    }
}

/// Adds what the layout knows: camera, brand, frame rate, and the duration from the video
/// frame count.
pub fn apply_layout(report: &mut InspectReport, lay: DatLayout) {
    if let Some(r) = lay.frame_rate {
        report.estimated_fps = Some(r.fps());
        if report.video_frames > 0 {
            report.estimated_seconds = Some(report.video_frames as f64 / r.fps());
        }
    }
    report.brand = lay.brand().map(|b| b.to_string());
    report.camera = lay.camera();
    report.body_code = lay.body_code.map(|c| layout::hex(&c));
    report.layout = Some(lay);
}

/// Summarizes an index that was already read from `src`.
pub fn inspect_index<R: crate::read_at::ReadAt + ?Sized>(
    path: &Path,
    src: &R,
    index: &DatIndex,
) -> InspectReport {
    let mut report = summarize(path, index);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    apply_layout(&mut report, layout::analyze(src, index, name));
    report
}

/// Walks a `.DAT` and summarizes it. Also returns the index, so a later
/// [`crate::repair::repair`] can skip the walk ([`RepairOptions::index`]). `opts.cancel`
/// stops the walk.
pub fn inspect_with(
    path: &Path,
    opts: &RepairOptions,
    on_event: &mut dyn FnMut(&Event),
) -> Result<(InspectReport, DatIndex)> {
    let file = FileSource::open(path)
        .map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
    let src = Tolerant::new(file, !opts.stop_on_read_error);
    classify_input(path, &src)?;
    let index = index_dat(&src, opts, on_event).map_err(|e| not_a_dat(path, &src, e))?;
    if index.groups.is_empty() {
        return Err(Error::NotADat(format!(
            "{} has no Canon recovery index (repi)",
            path.display()
        )));
    }
    let report = inspect_index(path, &src, &index);
    Ok((report, index))
}

/// Walks a `.DAT` and summarizes it.
pub fn inspect(path: &Path, on_event: &mut dyn FnMut(&Event)) -> Result<InspectReport> {
    inspect_with(path, &RepairOptions::default(), on_event).map(|(r, _)| r)
}
