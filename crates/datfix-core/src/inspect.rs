//! A quick summary of a `.DAT` without a reference clip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fourcc::FourCC;
use crate::read_at::FileSource;
use crate::repair::{Event, RepairOptions, index_dat};
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
    /// Estimated duration from 48 kHz PCM audio, when the `.DAT` has indexed PCM. Assumes
    /// 16-bit stereo when the sample layout does not say otherwise.
    pub estimated_seconds: Option<f64>,
    /// Estimated video frame rate from the same data.
    pub estimated_fps: Option<f64>,
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
        rejected_headers: index.rejected_headers.len() as u64,
        trailing_bytes: index.trailing_bytes,
        truncation: index.truncation.clone(),
        estimated_seconds,
        estimated_fps,
    }
}

/// Walks a `.DAT` and summarizes it.
pub fn inspect(path: &Path, on_event: &mut dyn FnMut(&Event)) -> Result<InspectReport> {
    let src = FileSource::open(path)
        .map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
    let index = index_dat(&src, &RepairOptions::default(), on_event)?;
    Ok(summarize(path, &index))
}
