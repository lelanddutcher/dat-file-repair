//! Walking the Canon `repi` recovery index inside a `.DAT`.
//!
//! # Layout
//!
//! A Canon `.DAT` starts with `00 00 00 00 'mdat'` (an `mdat` box whose size was never filled
//! in) and an 8-byte `wide` box. Then come *groups*. Each group is a `repi` header followed by
//! its samples, back to back. The header is little-endian:
//!
//! ```text
//! offset  size  field
//!      0     4  'repi'
//!      4     3  header_len   (u24) = 20 + (fixed_size == 0 ? 4 * n : 0)
//!      7     1  0x83         (flag)
//!      8     4  type         ('vide', 'lpcm', 'tmcd', 'tdmt', 'aac ', ...)
//!     12     4  fixed_size   (u32) size of every sample, or 0 when sizes vary
//!     16     4  n            (u32) number of samples
//!     20  4*n  sizes         (u32 each) only when fixed_size == 0
//! ```
//!
//! Groups may be separated by bytes that no header describes (padding, or data such as
//! unindexed audio). The walker finds the next group by scanning for `repi` and validating the
//! header strictly.
//!
//! # Damage handling
//!
//! - A header that fails validation is skipped and the scan continues after it.
//! - A group that runs past the end of the file keeps only its complete samples. The walk stops
//!   there: a camera that lost power writes nothing after its last group.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fourcc::FourCC;
use crate::read_at::ReadAt;

/// The four magic bytes of a group header.
pub const MAGIC: [u8; 4] = *b"repi";
/// The flag byte that follows the header length.
pub const FLAG: u8 = 0x83;
/// Size of the fixed part of a header.
pub const HEADER_MIN: u64 = 20;
/// Sanity limits used by header validation. They are far above anything a camera writes.
pub const MAX_SAMPLES_PER_GROUP: u32 = 8_000_000;
pub const MAX_SAMPLE_SIZE: u32 = 1 << 30;
pub const MAX_GROUP_PAYLOAD: u64 = 8 << 30;
/// Rejected header offsets kept in [`DatIndex::rejected_headers`]; the rest are only counted.
pub const MAX_REJECTED_LISTED: usize = 10_000;
/// Sample alignments tried for variable-size groups. 1 means "packed back to back".
const ALIGNMENTS: [u32; 5] = [1, 8, 4, 16, 32];
/// Samples checked per group when detecting alignment. Checking only a few is not enough: a
/// wrong alignment can fit by chance when the padding happens to agree for the first frames.
const ALIGN_CHECK_SAMPLES: u32 = 128;

/// Rounds `offset` up to a multiple of `align`.
fn align_up(offset: u64, align: u32) -> u64 {
    let a = align.max(1) as u64;
    offset.div_ceil(a) * a
}

/// One `repi` group: a header plus the samples it describes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    /// Sample type, for example `vide` or `lpcm`.
    pub kind: FourCC,
    /// File offset of the `repi` magic.
    pub header_offset: u64,
    /// Header length in bytes (20, or 20 + 4n).
    pub header_len: u32,
    /// Size of every sample, or 0 when `sizes` lists them.
    pub fixed_size: u32,
    /// Per-sample sizes when `fixed_size == 0`; empty otherwise.
    #[serde(skip)]
    pub sizes: Vec<u32>,
    /// Number of samples the header declares.
    pub declared: u32,
    /// Number of samples whose bytes are entirely inside the file.
    pub complete: u32,
    /// Each sample is padded to a multiple of this many bytes. 1 = packed.
    /// R5 Mark II video is packed; R3/R5/R6 II (`CAEP`) video is padded to 8 bytes.
    pub align: u32,
}

impl Group {
    /// File offset of the first sample.
    pub fn data_offset(&self) -> u64 {
        self.header_offset + self.header_len as u64
    }

    /// Size of sample `i`.
    pub fn sample_size(&self, i: usize) -> u32 {
        if self.fixed_size > 0 {
            self.fixed_size
        } else {
            self.sizes[i]
        }
    }

    /// Bytes of the first `count` samples, padding excluded.
    pub fn len_of(&self, count: u32) -> u64 {
        if self.fixed_size > 0 {
            self.fixed_size as u64 * count as u64
        } else {
            self.sizes[..count as usize].iter().map(|&s| s as u64).sum()
        }
    }

    /// Bytes one sample occupies in the file: its size rounded up to the alignment unit.
    fn stride(&self, size: u32) -> u64 {
        align_up(size as u64, self.align)
    }

    /// `(offset, size)` of the first `count` samples.
    ///
    /// Canon pads each sample to a multiple of `align` bytes. The rule is on the sample *size*,
    /// not on the file offset: sample `i + 1` starts `round_up(size_i, align)` bytes after
    /// sample `i` (spec section 5).
    pub fn samples(&self, count: u32) -> impl Iterator<Item = (u64, u32)> + '_ {
        let mut off = self.data_offset();
        (0..count as usize).map(move |i| {
            let size = self.sample_size(i);
            let here = off;
            off += self.stride(size);
            (here, size)
        })
    }

    /// File offset just after the `count`-th sample, its padding included.
    pub fn end_of(&self, count: u32) -> u64 {
        if self.align <= 1 {
            self.data_offset() + self.len_of(count)
        } else {
            self.data_offset()
                + (0..count as usize)
                    .map(|i| self.stride(self.sample_size(i)))
                    .sum::<u64>()
        }
    }

    /// File offset just after the last declared sample.
    pub fn declared_end(&self) -> u64 {
        self.end_of(self.declared)
    }

    /// File offset just after the last complete sample.
    pub fn complete_end(&self) -> u64 {
        self.end_of(self.complete)
    }
}

/// Bytes between two groups that no header describes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gap {
    pub offset: u64,
    pub len: u64,
}

/// How the end of the file cut the last group.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Truncation {
    /// Where the cut group (or header) starts.
    pub offset: u64,
    /// The cut group's type, when its header could be read.
    pub kind: Option<FourCC>,
    /// Samples the header declared but the file does not fully contain.
    pub dropped_samples: u32,
    /// Bytes missing to complete the group.
    pub missing_bytes: u64,
}

/// Everything the walker learned about a `.DAT`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DatIndex {
    pub file_size: u64,
    /// True when the file starts with a size-0 `mdat` (the Canon `.DAT` signature).
    pub has_canon_header: bool,
    pub groups: Vec<Group>,
    /// Unindexed bytes between groups. Normal for many cameras.
    pub gaps: Vec<Gap>,
    /// Places where `repi` appeared but the header failed validation (the first
    /// [`MAX_REJECTED_LISTED`]; `rejected_count` has the total).
    pub rejected_headers: Vec<u64>,
    pub rejected_count: u64,
    /// Bytes after the last group that hold no complete header.
    pub trailing_bytes: u64,
    pub truncation: Option<Truncation>,
}

impl DatIndex {
    /// Offset where the first group starts; the repaired `mdat` payload starts here.
    pub fn payload_start(&self) -> u64 {
        self.groups.first().map(|g| g.header_offset).unwrap_or(0)
    }

    /// The typical shape of each stream, in order of first appearance.
    pub fn shapes(&self) -> Vec<StreamShape> {
        let mut out: Vec<StreamShape> = Vec::new();
        let mut counts: Vec<std::collections::HashMap<u32, usize>> = Vec::new();
        let mut run = 0u32;
        let mut prev: Option<FourCC> = None;
        for g in &self.groups {
            // A run counts only once another type ends it; a video-only file has no runs.
            if prev != Some(g.kind) {
                if let Some(p) = prev.and_then(|p| out.iter().position(|s| s.kind == p)) {
                    out[p].streams = out[p].streams.max(run);
                }
                run = 0;
            }
            run += 1;
            prev = Some(g.kind);
            let i = match out.iter().position(|s| s.kind == g.kind) {
                Some(i) => i,
                None => {
                    out.push(StreamShape {
                        kind: g.kind,
                        typical_samples: 0,
                        fixed_size: g.fixed_size,
                        align: g.align,
                        streams: 1,
                    });
                    counts.push(Default::default());
                    out.len() - 1
                }
            };
            // Only complete groups say how many samples a group normally holds.
            if g.complete == g.declared {
                *counts[i].entry(g.declared).or_default() += 1;
            }
            out[i].align = out[i].align.max(g.align);
        }
        for (s, c) in out.iter_mut().zip(counts) {
            s.typical_samples = c
                .into_iter()
                .max_by_key(|&(n, k)| (k, n))
                .map_or(0, |(n, _)| n);
        }
        out
    }
}

/// How one stream is laid out: a fingerprint of the camera's recording mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamShape {
    pub kind: FourCC,
    /// Most common samples per group (0 when no group is complete).
    pub typical_samples: u32,
    /// Sample size, or 0 when sizes vary.
    pub fixed_size: u32,
    /// Sample alignment (1 = packed).
    pub align: u32,
    /// Groups of this type in a row, for example 4 for four mono PCM channels.
    pub streams: u32,
}

/// Why a candidate header was not accepted.
enum HeaderResult {
    Valid(Group),
    /// The header is cut by the end of the file.
    Truncated,
    Invalid,
}

/// Walks every group of a `.DAT`.
///
/// `progress` receives the current file offset after each group. It returns `false` to cancel.
pub fn walk<R: ReadAt + ?Sized>(
    src: &R,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<DatIndex> {
    walk_from(src, 0, usize::MAX, progress)
}

/// Walks groups starting at `start`, stopping after `max_groups` groups.
///
/// Used with a limit to sample the `repi` layout inside a healthy clip's `mdat`.
pub fn walk_from<R: ReadAt + ?Sized>(
    src: &R,
    start: u64,
    max_groups: usize,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<DatIndex> {
    walk_window(src, start, max_groups, u64::MAX, progress)
}

/// Like [`walk_from`], but gives up once `max_gap` bytes after the last group (or after
/// `start`) hold no `repi` header.
///
/// Used to sample the index inside a healthy clip: a clip from a body without `repi` would
/// otherwise be scanned to its end, which takes minutes for a large file on a network share.
pub fn walk_window<R: ReadAt + ?Sized>(
    src: &R,
    start: u64,
    max_groups: usize,
    max_gap: u64,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<DatIndex> {
    let len = src.size();
    let io = |e| Error::io("cannot read the .DAT", e);
    let mut index = DatIndex {
        file_size: len,
        ..Default::default()
    };
    if len >= 8 {
        let mut head = [0u8; 8];
        src.read_exact_at(&mut head, 0).map_err(io)?;
        index.has_canon_header = head == [0, 0, 0, 0, b'm', b'd', b'a', b't'];
    }

    // `last_end` is where the previous group ended; the scan starts there.
    let mut last_end = start;
    let mut scan_from = start;
    // Alignment seen per stream: (type, alignment, groups in a row that agreed).
    let mut aligns: Vec<(FourCC, u32, u32)> = Vec::new();
    while index.groups.len() < max_groups {
        let until = last_end.saturating_add(max_gap).min(len);
        let Some(at) = find_magic(src, scan_from, until).map_err(io)? else {
            index.trailing_bytes = len - last_end;
            break;
        };
        match read_header(src, at).map_err(io)? {
            HeaderResult::Valid(mut g) => {
                if g.fixed_size == 0 && g.declared >= 2 {
                    // Once full checks of two groups agree on a stream's alignment, later
                    // groups only confirm it on a few samples. A full check reads every NAL
                    // length of up to 128 frames; over a network share that cost ~15 ms per
                    // group.
                    let known = aligns
                        .iter()
                        .find(|(k, _, n)| *k == g.kind && *n >= 2)
                        .map(|&(_, a, _)| a);
                    g.align = match known {
                        Some(a) if confirm_alignment(src, &g, a).map_err(io)? => a,
                        _ => detect_alignment(src, &g).map_err(io)?,
                    };
                    match aligns.iter_mut().find(|(k, _, _)| *k == g.kind) {
                        Some(e) if e.1 == g.align => e.2 += 1,
                        Some(e) => *e = (g.kind, g.align, 1),
                        None => aligns.push((g.kind, g.align, 1)),
                    }
                }
                // The first gap (before the first group) is the file header, not a gap.
                if at > last_end && !index.groups.is_empty() {
                    index.gaps.push(Gap {
                        offset: last_end,
                        len: at - last_end,
                    });
                }
                let end = g.declared_end();
                if end > len {
                    // Keep only samples whose last byte is inside the file.
                    let complete = g
                        .samples(g.declared)
                        .take_while(|&(o, s)| o + s as u64 <= len)
                        .count() as u32;
                    g.complete = complete;
                    index.truncation = Some(Truncation {
                        offset: at,
                        kind: Some(g.kind),
                        dropped_samples: g.declared - complete,
                        missing_bytes: end - len,
                    });
                    index.groups.push(g);
                    break;
                }
                index.groups.push(g);
                last_end = end;
                scan_from = end;
                if !progress(end) {
                    return Err(Error::Cancelled);
                }
            }
            HeaderResult::Truncated => {
                index.truncation = Some(Truncation {
                    offset: at,
                    kind: None,
                    dropped_samples: 0,
                    missing_bytes: 0,
                });
                index.trailing_bytes = len - last_end;
                break;
            }
            HeaderResult::Invalid => {
                // Cap the list: a file full of near-miss headers must not grow it without bound.
                if index.rejected_headers.len() < MAX_REJECTED_LISTED {
                    index.rejected_headers.push(at);
                }
                index.rejected_count += 1;
                scan_from = at + 1;
            }
        }
    }
    progress(len);
    if index.groups.is_empty() {
        return Err(Error::NotADat(
            "no Canon recovery index (repi) was found in this file. It may not be a Canon \
             camera .DAT, or the start of the file is missing."
                .into(),
        ));
    }
    Ok(index)
}

/// Reads and validates the header at `at`.
fn read_header<R: ReadAt + ?Sized>(src: &R, at: u64) -> std::io::Result<HeaderResult> {
    let len = src.size();
    if at + HEADER_MIN > len {
        return Ok(HeaderResult::Truncated);
    }
    let mut h = [0u8; HEADER_MIN as usize];
    src.read_exact_at(&mut h, at)?;
    let header_len = u32::from_le_bytes([h[4], h[5], h[6], 0]);
    let flag = h[7];
    let kind = FourCC::from_slice(&h[8..12]);
    let fixed_size = u32::from_le_bytes(h[12..16].try_into().unwrap());
    let n = u32::from_le_bytes(h[16..20].try_into().unwrap());

    let expected_len = if fixed_size > 0 {
        HEADER_MIN
    } else {
        HEADER_MIN + 4 * n as u64
    };
    let plausible = h[..4] == MAGIC
        && flag == FLAG
        && kind.is_printable()
        && (1..=MAX_SAMPLES_PER_GROUP).contains(&n)
        && fixed_size <= MAX_SAMPLE_SIZE
        && header_len as u64 == expected_len;
    if !plausible {
        return Ok(HeaderResult::Invalid);
    }
    if at + expected_len > len {
        return Ok(HeaderResult::Truncated);
    }

    let sizes = if fixed_size == 0 {
        let raw = src.read_vec(at + HEADER_MIN, 4 * n as usize)?;
        let sizes: Vec<u32> = raw
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        if sizes.iter().any(|&s| s == 0 || s > MAX_SAMPLE_SIZE) {
            return Ok(HeaderResult::Invalid);
        }
        sizes
    } else {
        Vec::new()
    };
    let g = Group {
        kind,
        header_offset: at,
        header_len,
        fixed_size,
        sizes,
        declared: n,
        complete: n,
        align: 1,
    };
    if g.len_of(n) > MAX_GROUP_PAYLOAD {
        return Ok(HeaderResult::Invalid);
    }
    Ok(HeaderResult::Valid(g))
}

/// Works out how the samples of a variable-size group are laid out.
///
/// Canon video samples are length-prefixed NAL units (4-byte big-endian lengths). For each
/// candidate alignment, the first samples are checked: their NAL lengths must add up to
/// exactly the sample size. Sample 0 always starts right after the header, so at least two
/// samples must be checked to tell alignments apart. Falls back to packed (1).
fn detect_alignment<R: ReadAt + ?Sized>(src: &R, g: &Group) -> std::io::Result<u32> {
    let len = src.size();
    'candidates: for align in ALIGNMENTS {
        let mut off = g.data_offset();
        let mut checked = 0;
        for i in 0..g.declared.min(ALIGN_CHECK_SAMPLES) as usize {
            let size = g.sizes[i];
            if off + size as u64 > len {
                break;
            }
            if !nal_chain_fits(src, off, size)? {
                continue 'candidates;
            }
            checked += 1;
            off += align_up(size as u64, align);
        }
        if checked >= 2 {
            return Ok(align);
        }
    }
    Ok(1)
}

/// Checks a known alignment on a few samples: the first two, one in the middle and the last
/// two complete ones. A wrong alignment shifts every later sample, so the last ones catch it.
fn confirm_alignment<R: ReadAt + ?Sized>(src: &R, g: &Group, align: u32) -> std::io::Result<bool> {
    let len = src.size();
    let n = g.declared as usize;
    let mut picks = [0, 1, n / 2, n.saturating_sub(2), n - 1];
    picks.sort_unstable();
    let mut off = g.data_offset();
    let mut checked = 0;
    for (i, &size) in g.sizes.iter().enumerate() {
        if off + size as u64 > len {
            break;
        }
        if picks.contains(&i) {
            if !nal_chain_fits(src, off, size)? {
                return Ok(false);
            }
            checked += 1;
        }
        off += align_up(size as u64, align);
    }
    Ok(checked >= 2)
}

/// True when the 4-byte NAL length prefixes starting at `off` add up to exactly `size`.
fn nal_chain_fits<R: ReadAt + ?Sized>(src: &R, off: u64, size: u32) -> std::io::Result<bool> {
    let end = off + size as u64;
    let mut pos = off;
    let mut b = [0u8; 4];
    for _ in 0..4096 {
        if pos == end {
            return Ok(true);
        }
        if pos + 4 > end {
            return Ok(false);
        }
        src.read_exact_at(&mut b, pos)?;
        let nal = u32::from_be_bytes(b) as u64;
        if nal == 0 {
            return Ok(false);
        }
        pos += 4 + nal;
    }
    Ok(false)
}

/// Finds the next `repi` magic at or after `from` that starts before `until`.
///
/// The common case is that the magic sits exactly at `from`, so that is checked with a 4-byte
/// read first. Otherwise the file is scanned in blocks that grow from 64 KiB to 4 MiB.
fn find_magic<R: ReadAt + ?Sized>(src: &R, from: u64, until: u64) -> std::io::Result<Option<u64>> {
    // The magic may start up to 3 bytes before `until` and still end inside the window.
    let len = src.size().min(until.saturating_add(3));
    if from + 4 > len {
        return Ok(None);
    }
    let mut probe = [0u8; 4];
    src.read_exact_at(&mut probe, from)?;
    if probe == MAGIC {
        return Ok(Some(from));
    }
    let mut block = 64 * 1024usize;
    let mut pos = from + 1;
    let mut buf = Vec::new();
    while pos + 4 <= len {
        let n = ((len - pos) as usize).min(block);
        buf.resize(n, 0);
        src.read_exact_at(&mut buf, pos)?;
        if let Some(i) = buf.windows(4).position(|w| w == MAGIC) {
            return Ok(Some(pos + i as u64));
        }
        // Step back 3 bytes so a magic split across two blocks is still found.
        pos += (n as u64).saturating_sub(3).max(1);
        block = (block * 2).min(4 * 1024 * 1024);
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds one group with samples filled by a recognizable byte pattern.
    pub(crate) fn group_bytes(kind: &[u8; 4], fixed: u32, sizes: &[u32]) -> Vec<u8> {
        let n = sizes.len() as u32;
        let header_len = if fixed > 0 { 20 } else { 20 + 4 * n };
        let mut v = Vec::new();
        v.extend_from_slice(b"repi");
        v.extend_from_slice(&header_len.to_le_bytes()[..3]);
        v.push(FLAG);
        v.extend_from_slice(kind);
        v.extend_from_slice(&fixed.to_le_bytes());
        v.extend_from_slice(&n.to_le_bytes());
        if fixed == 0 {
            for s in sizes {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        for (i, &s) in sizes.iter().enumerate() {
            v.extend(std::iter::repeat_n((i % 200) as u8 + 1, s as usize));
        }
        v
    }

    /// The 16-byte Canon `.DAT` file header.
    pub(crate) fn dat_header() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0];
        v.extend_from_slice(b"mdat");
        v.extend_from_slice(&[0, 0, 0, 8]);
        v.extend_from_slice(b"wide");
        v
    }

    fn no_progress() -> impl FnMut(u64) -> bool {
        |_| true
    }

    #[test]
    fn walks_clean_stream() {
        let mut d = dat_header();
        d.extend(group_bytes(b"tmcd", 4, &[4; 3]));
        d.extend(group_bytes(b"vide", 0, &[100, 50, 70]));
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert!(idx.has_canon_header);
        assert_eq!(idx.groups.len(), 2);
        assert_eq!(idx.groups[0].kind, FourCC::new(b"tmcd"));
        assert_eq!(idx.groups[0].header_offset, 16);
        assert_eq!(idx.groups[1].sizes, vec![100, 50, 70]);
        assert_eq!(idx.groups[1].declared_end(), d.len() as u64);
        assert!(idx.gaps.is_empty());
        assert!(idx.truncation.is_none());
        assert_eq!(idx.payload_start(), 16);
    }

    #[test]
    fn records_padding_between_groups() {
        let mut d = dat_header();
        d.extend(group_bytes(b"vide", 0, &[10, 20]));
        let gap_at = d.len() as u64;
        d.extend(vec![0x55u8; 7680]);
        d.extend(group_bytes(b"tmcd", 4, &[4; 2]));
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert_eq!(idx.groups.len(), 2);
        assert_eq!(
            idx.gaps,
            vec![Gap {
                offset: gap_at,
                len: 7680
            }]
        );
    }

    #[test]
    fn keeps_complete_samples_of_truncated_group() {
        let mut d = dat_header();
        d.extend(group_bytes(b"lpcm", 3, &[3; 10]));
        d.extend(group_bytes(b"vide", 0, &[100, 100, 100, 100]));
        // Cut in the middle of the third video sample.
        d.truncate(d.len() - 150);
        let idx = walk(&d, &mut no_progress()).unwrap();
        let v = &idx.groups[1];
        assert_eq!(v.declared, 4);
        assert_eq!(v.complete, 2);
        let t = idx.truncation.unwrap();
        assert_eq!(t.dropped_samples, 2);
        assert_eq!(t.missing_bytes, 150);
        assert!(v.complete_end() <= d.len() as u64);
    }

    #[test]
    fn stops_at_truncated_header() {
        let mut d = dat_header();
        d.extend(group_bytes(b"vide", 0, &[10]));
        let full = group_bytes(b"vide", 0, &[10, 10, 10]);
        d.extend(&full[..24]); // magic + part of the size table
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert_eq!(idx.groups.len(), 1);
        let t = idx.truncation.unwrap();
        assert_eq!(t.kind, None);
        assert_eq!(idx.trailing_bytes, 24);
    }

    #[test]
    fn resyncs_after_damaged_header() {
        let mut d = dat_header();
        d.extend(group_bytes(b"vide", 0, &[10, 10]));
        let bad_at = d.len();
        d.extend(group_bytes(b"vide", 0, &[10, 10]));
        d[bad_at + 7] = 0x00; // corrupt the flag byte
        d.extend(group_bytes(b"vide", 0, &[30]));
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert_eq!(idx.groups.len(), 2);
        assert_eq!(idx.rejected_headers, vec![bad_at as u64]);
        // The damaged group's bytes become a gap.
        assert_eq!(idx.gaps.len(), 1);
        assert_eq!(idx.gaps[0].offset, bad_at as u64);
        assert_eq!(idx.groups[1].sizes, vec![30]);
    }

    #[test]
    fn rejects_inconsistent_header_length() {
        let mut d = dat_header();
        let mut g = group_bytes(b"vide", 0, &[10, 10]);
        g[4] = 99; // header_len no longer equals 20 + 4n
        d.extend(g);
        assert!(matches!(
            walk(&d, &mut no_progress()),
            Err(Error::NotADat(_))
        ));
    }

    #[test]
    fn finds_magic_across_block_boundary() {
        let mut d = vec![0u8; 64 * 1024 + 1];
        d.extend(group_bytes(b"vide", 0, &[5]));
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert_eq!(idx.groups[0].header_offset, 64 * 1024 + 1);
        assert!(!idx.has_canon_header);
    }

    /// A sample made of one NAL unit with a 4-byte length prefix.
    fn nal_sample(size: u32) -> Vec<u8> {
        let mut v = (size - 4).to_be_bytes().to_vec();
        v.extend(std::iter::repeat_n(0x40u8, size as usize - 4));
        v
    }

    #[test]
    fn detects_packed_and_aligned_samples() {
        let sizes = [29u32, 30, 45, 17];
        // Packed: samples back to back.
        let mut d = dat_header();
        let mut g = group_bytes(b"vide", 0, &sizes);
        g.truncate(20 + 16);
        for &s in &sizes {
            g.extend(nal_sample(s));
        }
        d.extend(&g);
        let idx = walk(&d, &mut no_progress()).unwrap();
        assert_eq!(idx.groups[0].align, 1);
        assert_eq!(idx.groups[0].declared_end(), d.len() as u64);

        // Padded: each sample occupies its size rounded up to 8 bytes. The data starts at an
        // offset that is 4 mod 8 (as in Canon family B), so sample starts are 4 mod 8 too.
        let mut d = dat_header();
        d.extend(&group_bytes(b"vide", 0, &sizes)[..36]);
        assert_eq!(d.len() % 8, 4);
        for &s in &sizes {
            d.extend(nal_sample(s));
            while d.len() % 8 != 4 {
                d.push(0xee);
            }
        }
        let idx = walk(&d, &mut no_progress()).unwrap();
        let g = &idx.groups[0];
        assert_eq!(g.align, 8);
        let offs: Vec<u64> = g.samples(4).map(|(o, _)| o).collect();
        assert_eq!(offs, vec![52, 52 + 32, 52 + 64, 52 + 112]);
        // The group ends after the last sample's padding.
        assert_eq!(g.declared_end(), d.len() as u64);
        assert!(idx.truncation.is_none());
    }

    /// A variable-size group whose samples are single NAL units, each padded to `align`.
    fn nal_group(d: &mut Vec<u8>, sizes: &[u32], align: usize) {
        let start = d.len();
        d.extend(&group_bytes(b"vide", 0, sizes)[..20 + 4 * sizes.len()]);
        let base = d.len();
        for &s in sizes {
            d.extend(nal_sample(s));
            while (d.len() - base) % align != 0 {
                d.push(0xee);
            }
        }
        assert!(d.len() > start);
    }

    #[test]
    fn known_alignment_is_confirmed_cheaply_and_rechecked_on_change() {
        let sizes = [29u32, 30, 45, 17, 33, 61, 12, 90];
        let mut d = dat_header();
        for _ in 0..4 {
            nal_group(&mut d, &sizes, 8);
        }
        // The camera switched to packed samples (another body or mode mid-file).
        nal_group(&mut d, &sizes, 1);
        let idx = walk(&d, &mut no_progress()).unwrap();
        let aligns: Vec<u32> = idx.groups.iter().map(|g| g.align).collect();
        assert_eq!(aligns, [8, 8, 8, 8, 1]);
        assert_eq!(idx.groups[4].declared_end(), d.len() as u64);
    }

    #[test]
    fn shapes_count_streams_per_interval() {
        let mut d = dat_header();
        for _ in 0..3 {
            d.extend(group_bytes(b"tmcd", 4, &[4; 6]));
            for _ in 0..4 {
                d.extend(group_bytes(b"lpcm", 3, &[3; 10]));
            }
            d.extend(group_bytes(b"vide", 0, &[10; 6]));
        }
        let shapes = walk(&d, &mut no_progress()).unwrap().shapes();
        let kinds: Vec<String> = shapes.iter().map(|s| s.kind.to_string()).collect();
        assert_eq!(kinds, ["tmcd", "lpcm", "vide"]);
        assert_eq!(shapes[1].streams, 4);
        assert_eq!(shapes[1].typical_samples, 10);
        assert_eq!(shapes[2].streams, 1);

        // Video only: consecutive groups are one stream, not many.
        let mut d = dat_header();
        for _ in 0..5 {
            d.extend(group_bytes(b"vide", 0, &[10; 15]));
        }
        let shapes = walk(&d, &mut no_progress()).unwrap().shapes();
        assert_eq!(shapes[0].streams, 1);
        assert_eq!(shapes[0].typical_samples, 15);
    }

    #[test]
    fn cancel_stops_walk() {
        let mut d = dat_header();
        d.extend(group_bytes(b"vide", 0, &[5]));
        d.extend(group_bytes(b"vide", 0, &[5]));
        assert!(matches!(walk(&d, &mut |_| false), Err(Error::Cancelled)));
    }
}
