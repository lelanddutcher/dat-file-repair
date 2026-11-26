//! Card-level recovery: find Canon recordings in a raw card image or device.
//!
//! # Why
//!
//! When a Canon camera loses power, the recording's bytes are on the card, but the file
//! system may not show them: the `.DAT` entry can be 0 bytes, deleted, or missing (FAT32
//! cards can lose the entry altogether). This module finds the recordings by their content.
//!
//! # How
//!
//! 1. **Signature pass** ([`scan`]): one sequential read of the whole image with large
//!    aligned blocks. It records every valid `repi` group header, every sector-aligned
//!    Canon `.DAT` start (`00000000 'mdat' 00000008 'wide'`), and every sector-aligned
//!    `ftyp` box. This is the only full read; it runs at disk speed.
//! 2. **Finished clips**: each `ftyp` with a Canon brand (`CAEP`, `XFVC`) is parsed box by
//!    box to find its extent and its `mdat`.
//! 3. **Chains**: from each `.DAT` start, the group chain is followed header by header using
//!    the header list from step 1. When the chain breaks and the recording has timecode,
//!    a continuation elsewhere on the card is searched ([stitching](#stitching)).
//! 4. **End**: a `.DAT` is always a whole number of 32 MiB units long. The last unit that
//!    holds the last header found is the end of the recording.
//! 5. **Fragments**: chains that no `.DAT` start reaches and that are not inside a finished
//!    clip are reported as headerless fragments.
//!
//! # Stitching
//!
//! Camera recordings are usually contiguous. When the chain breaks inside the image, the
//! carver looks for a header elsewhere whose distance from the expected position is a whole
//! number of clusters and whose following `tmcd` group continues the timecode exactly. The
//! break point must be a cluster boundary counted from the `.DAT` start, and the samples of
//! the broken group must check out (NAL lengths, per-frame records, timecode) through the
//! stitched view. Anything ambiguous stops the recording there and reports it as partial.
//!
//! The source is only ever read. See `docs/format/card-carving.md`.

pub mod fs;
pub mod source;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fourcc::FourCC;
use crate::layout::{self, friendly_body};
use crate::read_at::ReadAt;
use crate::repi;

pub use fs::{DirEntry, FsKind, Volume};
pub use source::{ImageSource, StreamView};

/// The 16 bytes a Canon `.DAT` starts with.
pub const DAT_HEADER: [u8; 16] = [
    0, 0, 0, 0, b'm', b'd', b'a', b't', 0, 0, 0, 8, b'w', b'i', b'd', b'e',
];
/// Canon cameras grow a `.DAT` in 32 MiB units.
pub const DAT_UNIT: u64 = 32 << 20;
/// Files start on a sector boundary.
const SECTOR: u64 = 512;
/// Signature pass block size.
const BLOCK: usize = 8 << 20;
/// Bytes carried from one block to the next so no signature is split.
const OVERLAP: usize = 32;
/// Group types Canon writes.
const KINDS: [&[u8; 4]; 5] = [b"vide", b"tmcd", b"lpcm", b"aac ", b"tdmt"];
/// Size of one per-frame record after a video group (spec section 6).
const RECORD_SIZE: u64 = 128;
/// How far past the expected position a resync may look for the next header.
const RESYNC_WINDOW: u64 = 64 << 10;
/// Cluster sizes tried for the break point when the file system is unknown.
const UNIT_SIZES: [u64; 8] = [
    512 << 10,
    256 << 10,
    128 << 10,
    64 << 10,
    32 << 10,
    16 << 10,
    8 << 10,
    4 << 10,
];
/// Smallest continuation step assumed without a file system.
const MIN_CLUSTER: u64 = 4 << 10;
/// Groups followed from a candidate continuation to reach its first `tmcd` group.
const LOOKAHEAD_GROUPS: usize = 16;
/// Fragments shorter than this many groups are not reported.
const MIN_FRAGMENT_GROUPS: usize = 4;
/// Stitches per recording at most.
const MAX_STITCHES: usize = 64;

/// Stage of a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    ReadingFileSystem,
    Scanning,
    Following,
    Extracting,
}

/// Progress and findings, in the same style as [`crate::repair::Event`].
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ScanEvent {
    Phase {
        phase: ScanPhase,
        message: String,
    },
    /// `done` and `total` are bytes.
    Progress {
        phase: ScanPhase,
        done: u64,
        total: u64,
    },
    /// A recording, finished clip or fragment was found.
    Found {
        what: Found,
        offset: u64,
        length: u64,
    },
    Note {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Found {
    Recording,
    Clip,
    Fragment,
}

/// Options for [`scan`].
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Set from another thread to stop.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Try to join fragmented recordings (only with timecode proof).
    pub stitch: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            cancel: None,
            stitch: true,
        }
    }
}

/// One contiguous run of image bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Piece {
    pub offset: u64,
    pub length: u64,
}

/// How the end of a recovered stream was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEnd {
    /// The end of the 32 MiB unit that holds the last header (how the camera sizes a `.DAT`).
    DatUnit,
    /// The end of the last complete group (no unit boundary fits).
    LastGroup,
    /// The image ended first.
    ImageEnd,
    /// The size in the card's directory entry.
    FileSize,
}

/// A recording (or headerless fragment) found on the card.
#[derive(Clone, Debug, Serialize)]
pub struct Recovered {
    /// Image offset of the first byte: the `.DAT` header, or the first group of a fragment.
    pub offset: u64,
    /// Bytes of the recovered `.DAT` (a fragment gets a 16-byte header added).
    pub length: u64,
    pub pieces: Vec<Piece>,
    /// True when the `.DAT` header was found (false for fragments).
    pub has_dat_header: bool,
    pub groups: u64,
    pub video_frames: u64,
    pub duration_seconds: Option<f64>,
    pub frame_rate: Option<f64>,
    /// The camera as people know it, from the per-frame records.
    pub camera: Option<String>,
    /// Body, codec, frame rate and family.
    pub recording: Option<String>,
    /// `CAEP` or `XFVC`.
    pub brand: Option<String>,
    /// First timecode counter, when the recording has a `tmcd` stream.
    pub start_timecode: Option<u32>,
    pub end: StreamEnd,
    /// Fragments were joined.
    pub stitched: bool,
    /// Why the recording may be incomplete, in plain words.
    pub partial: Option<String>,
    /// Offsets of finished clips whose bytes overlap this recording.
    pub overlaps_clips: Vec<u64>,
    /// The file-system entry that points here, if any.
    pub file: Option<FileRef>,
    /// Headers reached by a forward search instead of at the exact expected place.
    pub resyncs: u64,
}

/// A file-system entry matched to a finding.
#[derive(Clone, Debug, Serialize)]
pub struct FileRef {
    pub path: String,
    pub size: u64,
    pub deleted: bool,
}

/// A finished Canon MP4 found on the card.
#[derive(Clone, Debug, Serialize)]
pub struct FinishedClip {
    pub offset: u64,
    pub length: u64,
    pub brand: String,
    /// Image range of the `mdat` payload.
    pub mdat: Option<Piece>,
    /// True when `ftyp`, `moov` and `mdat` are all inside the image.
    pub complete: bool,
    pub camera: Option<String>,
    pub duration_seconds: Option<f64>,
    pub file: Option<FileRef>,
}

/// A `.DAT` directory entry and what the scan found where it points.
#[derive(Clone, Debug, Serialize)]
pub struct DatHint {
    pub path: String,
    pub size: u64,
    pub deleted: bool,
    pub data_offset: Option<u64>,
    /// Offset of the recording found at that place.
    pub recording: Option<u64>,
}

/// A summary of one file system.
#[derive(Clone, Debug, Serialize)]
pub struct VolumeSummary {
    pub kind: FsKind,
    pub offset: u64,
    pub cluster_size: u64,
    pub heap_offset: u64,
    pub files: usize,
    pub deleted: usize,
    pub warnings: Vec<String>,
}

/// Everything a scan found.
#[derive(Clone, Debug, Serialize)]
pub struct ScanReport {
    pub size: u64,
    pub is_device: bool,
    pub volumes: Vec<VolumeSummary>,
    pub recordings: Vec<Recovered>,
    pub clips: Vec<FinishedClip>,
    pub fragments: Vec<Recovered>,
    pub dat_entries: Vec<DatHint>,
    pub headers_seen: u64,
    pub seconds: f64,
    pub notes: Vec<String>,
}

/// A valid `repi` header seen by the signature pass.
#[derive(Clone, Copy, Debug)]
struct Hit {
    offset: u64,
    kind: FourCC,
    fixed: u32,
    n: u32,
    header_len: u32,
}

#[derive(Default)]
struct Signatures {
    hits: Vec<Hit>,
    dat_starts: Vec<u64>,
    ftyps: Vec<u64>,
}

fn cancelled(opts: &ScanOptions) -> bool {
    opts.cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Parses a 20-byte group header. Same rules as [`repi`]'s walker, plus a known type.
fn parse_hit(b: &[u8], offset: u64) -> Option<Hit> {
    if b.len() < 20 || b[..4] != repi::MAGIC || b[7] != repi::FLAG {
        return None;
    }
    let header_len = u32::from_le_bytes([b[4], b[5], b[6], 0]);
    let kind = FourCC::from_slice(&b[8..12]);
    if !KINDS.iter().any(|k| **k == kind.0) {
        return None;
    }
    let fixed = u32::from_le_bytes(b[12..16].try_into().unwrap());
    let n = u32::from_le_bytes(b[16..20].try_into().unwrap());
    let expect = if fixed > 0 { 20 } else { 20 + 4 * n as u64 };
    let ok = (1..=repi::MAX_SAMPLES_PER_GROUP).contains(&n)
        && fixed <= repi::MAX_SAMPLE_SIZE
        && header_len as u64 == expect
        && fixed as u64 * n as u64 <= repi::MAX_GROUP_PAYLOAD;
    ok.then_some(Hit {
        offset,
        kind,
        fixed,
        n,
        header_len,
    })
}

/// Pass 1: one sequential read of the image. A reader thread keeps the disk busy while the
/// main thread searches the previous block.
fn signature_pass<R: ReadAt + Sync + ?Sized>(
    src: &R,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<Signatures> {
    let total = src.size();
    let finder_repi = memchr::memmem::Finder::new(b"repi");
    let finder_dat = memchr::memmem::Finder::new(&DAT_HEADER);
    let finder_ftyp = memchr::memmem::Finder::new(b"ftyp");
    let mut sig = Signatures::default();
    let stop = AtomicBool::new(false);
    let step = (total / 1000).max(BLOCK as u64);
    let mut next_report = 0u64;

    std::thread::scope(|s| -> Result<()> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<(u64, Vec<u8>)>>(2);
        let stop = &stop;
        s.spawn(move || {
            let mut off = 0u64;
            while off < total && !stop.load(Ordering::Relaxed) {
                let n = ((total - off) as usize).min(BLOCK);
                let mut buf = vec![0u8; n];
                let r = src.read_exact_at(&mut buf, off).map(|_| (off, buf));
                let failed = r.is_err();
                if tx.send(r).is_err() || failed {
                    break;
                }
                off += n as u64;
            }
        });
        let mut data: Vec<u8> = Vec::with_capacity(BLOCK + OVERLAP);
        for msg in rx {
            let (off, buf) = match msg {
                Ok(v) => v,
                Err(e) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(Error::io("cannot read the card image", e));
                }
            };
            if cancelled(opts) {
                stop.store(true, Ordering::Relaxed);
                return Err(Error::Cancelled);
            }
            let base = off - data.len() as u64;
            data.extend_from_slice(&buf);
            let end = off + buf.len() as u64;
            let last = end >= total;
            // Positions below `limit` are checked now; the rest wait for the next block.
            let limit = if last {
                data.len()
            } else {
                data.len() - OVERLAP
            };
            for p in finder_repi.find_iter(&data) {
                if p >= limit {
                    break;
                }
                if let Some(h) = data
                    .get(p..p + 20)
                    .and_then(|b| parse_hit(b, base + p as u64))
                {
                    sig.hits.push(h);
                }
            }
            for p in finder_dat.find_iter(&data) {
                if p >= limit {
                    break;
                }
                if (base + p as u64) % SECTOR == 0 {
                    sig.dat_starts.push(base + p as u64);
                }
            }
            for p in finder_ftyp.find_iter(&data) {
                if p >= limit {
                    break;
                }
                let at = (base + p as u64).wrapping_sub(4);
                if p >= 4 && at % SECTOR == 0 {
                    sig.ftyps.push(at);
                }
            }
            data.drain(..limit);
            if end >= next_report || last {
                next_report = end + step;
                on_event(&ScanEvent::Progress {
                    phase: ScanPhase::Scanning,
                    done: end,
                    total,
                });
            }
        }
        Ok(())
    })?;
    Ok(sig)
}

/// Where a group's samples end, from its header and size table.
struct Geom {
    header_end: u64,
    /// End of the samples packed back to back.
    packed: u64,
    /// End with every sample padded to 8 bytes.
    padded: u64,
    sizes: Vec<u32>,
    sizes_ok: bool,
}

fn geom<R: ReadAt + ?Sized>(src: &R, at: u64, h: &Hit) -> Geom {
    let header_end = at + h.header_len as u64;
    if h.fixed > 0 {
        let end = header_end + h.fixed as u64 * h.n as u64;
        return Geom {
            header_end,
            packed: end,
            padded: end,
            sizes: Vec::new(),
            sizes_ok: true,
        };
    }
    let sizes: Option<Vec<u32>> = src.read_vec(at + 20, 4 * h.n as usize).ok().map(|raw| {
        raw.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    });
    match sizes {
        Some(s) if s.iter().all(|&x| x > 0 && x <= repi::MAX_SAMPLE_SIZE) => {
            let packed = header_end + s.iter().map(|&x| x as u64).sum::<u64>();
            let padded = header_end + s.iter().map(|&x| (x as u64).div_ceil(8) * 8).sum::<u64>();
            Geom {
                header_end,
                packed,
                padded,
                sizes: s,
                sizes_ok: true,
            }
        }
        _ => Geom {
            header_end,
            packed: header_end,
            padded: header_end,
            sizes: Vec::new(),
            sizes_ok: false,
        },
    }
}

/// Expected end of group `h` (with records) for a given layout.
fn expected_end(g: &Geom, h: &Hit, pad8: bool, records: bool) -> u64 {
    let body = if pad8 { g.padded } else { g.packed };
    let rec = if records && h.kind.0 == *b"vide" {
        RECORD_SIZE * h.n as u64
    } else {
        0
    };
    body + rec
}

/// True when the 4-byte NAL length prefixes at `off` add up to exactly `size`.
fn nal_chain_fits<R: ReadAt + ?Sized>(src: &R, off: u64, size: u32) -> bool {
    let end = off + size as u64;
    let mut pos = off;
    let mut b = [0u8; 4];
    for _ in 0..4096 {
        if pos == end {
            return true;
        }
        if pos + 4 > end || src.read_exact_at(&mut b, pos).is_err() {
            return false;
        }
        let nal = u32::from_be_bytes(b) as u64;
        if nal == 0 {
            return false;
        }
        pos += 4 + nal;
    }
    false
}

/// Video layout learned while following a chain.
#[derive(Clone, Copy, Default)]
struct VideoMode {
    /// Samples padded to 8 bytes (families A/B/B′).
    pad8: Option<bool>,
    /// 128-byte records after each video group.
    records: Option<bool>,
}

/// Follows chains over the header list.
struct Follower<'a, R: ReadAt + ?Sized> {
    src: &'a R,
    hits: &'a [Hit],
    claimed: Vec<bool>,
    /// Image ranges owned by finished clips' `mdat`s.
    clip_ranges: Vec<(u64, u64)>,
    cluster: Option<u64>,
}

/// State of one chain walk.
struct Walk {
    pieces: Vec<Piece>,
    /// Image offset where the current piece starts.
    piece_start: u64,
    /// Stream offset of `piece_start`.
    piece_stream: u64,
    /// Indexes of the groups, in order.
    groups: Vec<usize>,
    kinds: Vec<FourCC>,
    mode: VideoMode,
    /// Timecode the next `tmcd` group must start with.
    tc_next: Option<u32>,
    tc_first: Option<u32>,
    resyncs: u64,
    stitched: bool,
    partial: Option<String>,
}

impl Walk {
    fn stream_of(&self, abs: u64) -> u64 {
        self.piece_stream + (abs - self.piece_start)
    }
}

impl<'a, R: ReadAt + ?Sized> Follower<'a, R> {
    fn idx_at(&self, offset: u64) -> Option<usize> {
        self.hits.binary_search_by_key(&offset, |h| h.offset).ok()
    }

    fn in_clip(&self, off: u64) -> bool {
        self.clip_ranges.iter().any(|&(a, b)| off >= a && off < b)
    }

    fn tmcd_counter(&self, at: u64) -> Option<u32> {
        let b = self.src.read_vec(at + 20, 4).ok()?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Finds the header that follows group `i` in the same piece of the image.
    /// Returns `(index, exact)`; `exact` is false for a forward-search resync.
    fn next_of(&self, i: usize, kinds: &[FourCC], mode: VideoMode) -> Option<(usize, bool)> {
        let h = &self.hits[i];
        let g = geom(self.src, h.offset, h);
        if !g.sizes_ok {
            return None;
        }
        let n = h.n as u64;
        let is_vide = h.kind.0 == *b"vide";
        let start = self.hits.partition_point(|x| x.offset < g.packed);
        let ok_gap = |gap: u64| {
            if is_vide {
                let pad_ok = |p: u64| match mode.pad8 {
                    Some(true) => p <= 7 * n,
                    Some(false) => p == 0,
                    None => p <= 7 * n,
                };
                match mode.records {
                    Some(true) => gap >= RECORD_SIZE * n && pad_ok(gap - RECORD_SIZE * n),
                    Some(false) => pad_ok(gap),
                    None => {
                        pad_ok(gap) || (gap >= RECORD_SIZE * n && pad_ok(gap - RECORD_SIZE * n))
                    }
                }
            } else {
                gap == 0
            }
        };
        let mut first_any = None;
        for (j, x) in self.hits.iter().enumerate().skip(start) {
            let gap = x.offset - g.packed;
            if gap > RESYNC_WINDOW + 135 * n {
                break;
            }
            if ok_gap(gap) {
                return Some((j, true));
            }
            if first_any.is_none() && (kinds.is_empty() || kinds.contains(&x.kind)) {
                first_any = Some(j);
            }
        }
        first_any.map(|j| (j, false))
    }

    /// Learns the video padding and record layout from a video group and its successor.
    fn learn(&self, i: usize, next: usize, mode: &mut VideoMode) {
        let h = &self.hits[i];
        if h.kind.0 != *b"vide" || h.fixed > 0 {
            return;
        }
        let g = geom(self.src, h.offset, h);
        let gap = self.hits[next].offset - g.packed;
        let rec = RECORD_SIZE * h.n as u64;
        let (records, pads) = if gap >= rec {
            (true, gap - rec)
        } else {
            (false, gap)
        };
        if mode.records.is_none() {
            mode.records = Some(records);
        }
        if mode.pad8.is_none() {
            if pads > 0 {
                mode.pad8 = Some(true);
            } else if g.padded != g.packed {
                mode.pad8 = Some(false);
            }
        }
    }

    /// Follows contiguous groups from `start` and returns the first tmcd counter found.
    fn lookahead_tmcd(&self, start: usize, kinds: &[FourCC], mut mode: VideoMode) -> Option<u32> {
        let mut i = start;
        for _ in 0..LOOKAHEAD_GROUPS {
            let h = self.hits[i];
            if h.kind.0 == *b"tmcd" && h.fixed == 4 {
                return self.tmcd_counter(h.offset);
            }
            let (j, _) = self.next_of(i, kinds, mode)?;
            self.learn(i, j, &mut mode);
            i = j;
        }
        None
    }

    /// Walks a chain from hit `first`. `stream_base` is the stream offset of `first`'s
    /// header (16 for a `.DAT`, 16 for a fragment with a synthetic header).
    fn walk(&mut self, first: usize, piece_start: u64, stitch: bool) -> Walk {
        let mut w = Walk {
            pieces: Vec::new(),
            piece_start,
            piece_stream: 0,
            groups: Vec::new(),
            kinds: Vec::new(),
            mode: VideoMode::default(),
            tc_next: None,
            tc_first: None,
            resyncs: 0,
            stitched: false,
            partial: None,
        };
        let mut i = first;
        loop {
            self.claimed[i] = true;
            w.groups.push(i);
            let h = self.hits[i];
            if !w.kinds.contains(&h.kind) {
                w.kinds.push(h.kind);
            }
            if h.kind.0 == *b"tmcd" && h.fixed == 4 {
                let c = self.tmcd_counter(h.offset);
                if w.tc_first.is_none() {
                    w.tc_first = c;
                }
                w.tc_next = c.map(|c| c.wrapping_add(h.n));
            }
            let next = self
                .next_of(i, &w.kinds, w.mode)
                .filter(|&(j, _)| !self.claimed[j] && !self.in_clip(self.hits[j].offset))
                .filter(|&(j, _)| self.timecode_continues(&w, j));
            match next {
                Some((j, exact)) => {
                    self.learn(i, j, &mut w.mode);
                    if !exact {
                        w.resyncs += 1;
                    }
                    i = j;
                }
                None => {
                    if stitch && w.tc_next.is_some() && w.pieces.len() < MAX_STITCHES {
                        match self.try_stitch(&w, i) {
                            Ok((b, delta, j)) => {
                                let len = b - w.piece_start;
                                w.pieces.push(Piece {
                                    offset: w.piece_start,
                                    length: len,
                                });
                                w.piece_stream += len;
                                w.piece_start = (b as i128 + delta) as u64;
                                w.stitched = true;
                                i = j;
                                continue;
                            }
                            Err(why) => w.partial = why,
                        }
                    }
                    break;
                }
            }
        }
        w
    }

    /// True when the image does not end right after group `i`.
    fn chain_could_continue(&self, i: usize) -> bool {
        let h = &self.hits[i];
        let g = geom(self.src, h.offset, h);
        g.padded + RESYNC_WINDOW < self.src.size()
    }

    /// A `tmcd` group must continue the timecode exactly.
    fn timecode_continues(&self, w: &Walk, j: usize) -> bool {
        let h = &self.hits[j];
        if h.kind.0 != *b"tmcd" || h.fixed != 4 {
            return true;
        }
        match (w.tc_next, self.tmcd_counter(h.offset)) {
            (Some(want), Some(got)) => want == got,
            _ => true,
        }
    }

    /// Looks for the continuation of a chain that broke after hit `last`.
    ///
    /// Returns `(break point, continuation - break point, next hit)`, or a reason when no
    /// single continuation is proven (`None` when the image simply ends).
    fn try_stitch(
        &self,
        w: &Walk,
        last: usize,
    ) -> std::result::Result<(u64, i128, usize), Option<String>> {
        let h = self.hits[last];
        if !self.chain_could_continue(last) {
            return Err(None);
        }
        let g = geom(self.src, h.offset, &h);
        // The timecode the continuation's first tmcd group must carry.
        let Some(tc_want) = w.tc_next else {
            return Err(None);
        };
        // Possible expected positions of the next header.
        let layouts: Vec<(bool, bool)> = match (w.mode.pad8, w.mode.records) {
            (Some(p), Some(r)) => vec![(p, r)],
            (Some(p), None) => vec![(p, true), (p, false)],
            (None, Some(r)) => vec![(true, r), (false, r)],
            (None, None) => vec![(true, true), (false, true), (true, false), (false, false)],
        };
        let step = self.cluster.unwrap_or(MIN_CLUSTER);
        let mut found: Vec<(u64, i128, usize)> = Vec::new();
        for (pad8, records) in layouts {
            if !g.sizes_ok {
                break;
            }
            let expect = expected_end(&g, &h, pad8, records);
            for (j, x) in self.hits.iter().enumerate() {
                if self.claimed[j] || !w.kinds.contains(&x.kind) || self.in_clip(x.offset) {
                    continue;
                }
                let delta = x.offset as i128 - expect as i128;
                if delta == 0 || delta.rem_euclid(step as i128) != 0 {
                    continue;
                }
                let mut mode = w.mode;
                mode.pad8 = Some(pad8);
                mode.records = Some(records);
                if self.lookahead_tmcd(j, &w.kinds, mode) != Some(tc_want) {
                    continue;
                }
                if let Some(b) = self.break_point(w, &h, &g, expect, delta, pad8, records)? {
                    if !found.iter().any(|f| f.2 == j) {
                        found.push((b, delta, j));
                    }
                }
            }
        }
        // No continuation is the normal case: the recording simply ends here.
        match found.len() {
            0 => Err(None),
            1 => Ok(found[0]),
            n => Err(Some(format!(
                "{n} places on the card could continue this recording; none was chosen"
            ))),
        }
    }

    /// Picks the cluster boundary where the first piece ends.
    ///
    /// Candidates lie between the end of the broken group's header and the expected next
    /// header, on 32 MiB units counted from the stream start first, then on cluster
    /// boundaries. A candidate counts when every sample of the broken group checks out
    /// through the stitched view. Several passing candidates at the same level is ambiguous.
    #[allow(clippy::too_many_arguments)]
    fn break_point(
        &self,
        w: &Walk,
        h: &Hit,
        g: &Geom,
        expect: u64,
        delta: i128,
        pad8: bool,
        records: bool,
    ) -> std::result::Result<Option<u64>, Option<String>> {
        // Stream offset 0 is the `.DAT` start, so cluster boundaries are multiples of the
        // cluster size in stream offsets.
        let lo_s = w.stream_of(g.header_end.max(h.offset + 20));
        let hi_s = w.stream_of(expect);
        let mut levels: Vec<u64> = vec![DAT_UNIT];
        match self.cluster {
            Some(c) => levels.push(c),
            None => levels.extend(UNIT_SIZES),
        }
        for unit in levels {
            let first = lo_s.div_ceil(unit) * unit;
            let mut ok = Vec::new();
            let mut b_s = first.max(unit);
            let mut tried = 0;
            while b_s <= hi_s && tried < 4096 {
                tried += 1;
                let b = w.piece_start + (b_s - w.piece_stream);
                if self.stitched_group_ok(w, h, b, delta, pad8, records) {
                    ok.push(b);
                }
                b_s += unit;
            }
            match ok.len() {
                0 => continue,
                1 => return Ok(Some(ok[0])),
                _ => {
                    return Err(Some(
                        "the recording is fragmented, but the exact break point cannot be proven"
                            .into(),
                    ));
                }
            }
        }
        Ok(None)
    }

    /// Checks the broken group's samples through a view that jumps by `delta` at `b`.
    fn stitched_group_ok(
        &self,
        w: &Walk,
        h: &Hit,
        b: u64,
        delta: i128,
        pad8: bool,
        records: bool,
    ) -> bool {
        let cont = b as i128 + delta;
        if cont < 0 {
            return false;
        }
        let tail = self.src.size().saturating_sub(cont as u64);
        let view = StreamView::new(
            self.src,
            Vec::new(),
            vec![(w.piece_start, b - w.piece_start), (cont as u64, tail)],
        );
        let at = h.offset - w.piece_start;
        let Ok(hb) = view.read_vec(at, 20) else {
            return false;
        };
        if parse_hit(&hb, at).is_none() {
            return false;
        }
        let g = geom(&view, at, h);
        if !g.sizes_ok {
            return false;
        }
        match &h.kind.0 {
            b"vide" if h.fixed == 0 => {
                let mut off = g.header_end;
                for &s in &g.sizes {
                    if !nal_chain_fits(&view, off, s) {
                        return false;
                    }
                    off += if pad8 {
                        (s as u64).div_ceil(8) * 8
                    } else {
                        s as u64
                    };
                }
                if records {
                    let len = RECORD_SIZE * h.n as u64;
                    let Ok(block) = view.read_vec(off, len as usize) else {
                        return false;
                    };
                    let ok = block
                        .chunks_exact(RECORD_SIZE as usize)
                        .all(|r| r[0] == 0x04 && r[1..4] == block[1..4]);
                    if !ok {
                        return false;
                    }
                }
                true
            }
            b"tmcd" if h.fixed == 4 => {
                let Ok(raw) = view.read_vec(g.header_end, 4 * h.n as usize) else {
                    return false;
                };
                let c: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                c.windows(2).all(|p| p[1] == p[0].wrapping_add(1))
                    && w.tc_next.is_none_or(|t| t.wrapping_sub(h.n) == c[0])
            }
            _ => true,
        }
    }
}

/// Reads a finished clip's box layout at `at`. Returns `None` unless it is a Canon `ftyp`.
fn read_clip<R: ReadAt + ?Sized>(src: &R, at: u64) -> Option<FinishedClip> {
    let len = src.size();
    let head = src.read_vec(at, 8).ok()?;
    let ftyp_size = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
    if !(16..=256).contains(&ftyp_size) {
        return None;
    }
    let ftyp = src.read_vec(at, ftyp_size as usize).ok()?;
    let brands: Vec<FourCC> = std::iter::once(&ftyp[8..12])
        .chain(ftyp[16..].chunks_exact(4))
        .map(FourCC::from_slice)
        .collect();
    let brand = brands
        .iter()
        .find(|b| b.0 == *b"CAEP" || b.0 == *b"XFVC")?
        .to_string();
    let mut pos = at;
    let mut mdat = None;
    let mut moov = None;
    for _ in 0..64 {
        if pos + 8 > len {
            break;
        }
        let Ok(b) = src.read_vec(pos, 16.min((len - pos) as usize)) else {
            break;
        };
        let size32 = u32::from_be_bytes(b[..4].try_into().unwrap()) as u64;
        let kind = FourCC::from_slice(&b[4..8]);
        if !kind.is_printable() {
            break;
        }
        let (size, hdr) = match size32 {
            0 => (len - pos, 8),
            1 if b.len() >= 16 => (u64::from_be_bytes(b[8..16].try_into().unwrap()), 16),
            s => (s, 8),
        };
        if size < hdr {
            break;
        }
        match &kind.0 {
            b"mdat" => {
                mdat = Some(Piece {
                    offset: pos + hdr,
                    length: size - hdr,
                })
            }
            b"moov" => moov = Some((pos, size)),
            _ => {}
        }
        pos = pos.saturating_add(size);
        if mdat.is_some() && moov.is_some() {
            break;
        }
    }
    let complete = mdat.is_some() && moov.is_some() && pos <= len;
    let (camera, duration_seconds) = moov
        .filter(|&(p, s)| s <= 64 << 20 && p + s <= len)
        .and_then(|(p, s)| src.read_vec(p, s as usize).ok())
        .map(|m| (moov_model(&m), moov_duration(&m)))
        .unwrap_or((None, None));
    Some(FinishedClip {
        offset: at,
        length: pos.min(len) - at,
        brand,
        mdat,
        complete,
        camera,
        duration_seconds,
        file: None,
    })
}

fn moov_model(moov: &[u8]) -> Option<String> {
    let i = memchr::memmem::find(moov, b"modl")?;
    let start = i.checked_sub(4)?;
    let size = u32::from_be_bytes(moov.get(start..i)?.try_into().ok()?) as usize;
    let s = moov.get(i + 8..start + size)?;
    let s = s.split(|&c| c == 0).next()?;
    let s = String::from_utf8_lossy(s).trim().to_string();
    (!s.is_empty()).then(|| friendly_body(&s))
}

fn moov_duration(moov: &[u8]) -> Option<f64> {
    let i = memchr::memmem::find(moov, b"mvhd")? + 4;
    let v = *moov.get(i)?;
    let (ts, dur) = if v == 1 {
        (
            u32::from_be_bytes(moov.get(i + 20..i + 24)?.try_into().ok()?),
            u64::from_be_bytes(moov.get(i + 24..i + 32)?.try_into().ok()?),
        )
    } else {
        (
            u32::from_be_bytes(moov.get(i + 12..i + 16)?.try_into().ok()?),
            u32::from_be_bytes(moov.get(i + 16..i + 20)?.try_into().ok()?) as u64,
        )
    };
    (ts > 0).then(|| dur as f64 / ts as f64)
}

/// Builds a [`Recovered`] from a finished walk: chooses the end and reads the layout.
fn finish<R: ReadAt + ?Sized>(
    f: &Follower<'_, R>,
    mut w: Walk,
    offset: u64,
    has_dat_header: bool,
    known_size: Option<u64>,
) -> Recovered {
    let src = f.src;
    let size = src.size();
    let last = f.hits[*w.groups.last().unwrap()];
    let g = geom(src, last.offset, &last);
    let pad8 = w.mode.pad8.unwrap_or(false);
    let records = w.mode.records.unwrap_or(false);
    let full_end = if g.sizes_ok {
        expected_end(&g, &last, pad8, records)
    } else {
        g.header_end
    };
    // Stream offsets; a fragment's stream starts with a 16-byte synthetic header.
    let hdr_end_s = w.stream_of(last.offset + 20);
    let full_end_s = w.stream_of(full_end);
    let mut end = StreamEnd::LastGroup;
    let mut end_s = full_end_s;
    if has_dat_header {
        let unit_end = hdr_end_s.div_ceil(DAT_UNIT) * DAT_UNIT;
        if unit_end <= full_end_s + RESYNC_WINDOW {
            end_s = unit_end;
            end = StreamEnd::DatUnit;
        }
        // A live directory entry with a real size wins when it fits the last group.
        if let Some(k) = known_size.filter(|&k| k > hdr_end_s && k <= full_end_s + RESYNC_WINDOW) {
            end_s = k;
            end = StreamEnd::FileSize;
        }
    }
    // Without a unit or file size, drop video samples that do not check out: the bytes
    // after a break belong to something else.
    if end == StreamEnd::LastGroup && last.kind.0 == *b"vide" && g.sizes_ok {
        let mut off = g.header_end;
        for &s in &g.sizes {
            if off + s as u64 > size || !nal_chain_fits(src, off, s) {
                end_s = w.stream_of(off);
                break;
            }
            off += if pad8 {
                (s as u64).div_ceil(8) * 8
            } else {
                s as u64
            };
        }
    }
    let mut piece_len = end_s - w.piece_stream;
    if w.piece_start + piece_len > size {
        piece_len = size - w.piece_start;
        end = StreamEnd::ImageEnd;
    }
    w.pieces.push(Piece {
        offset: w.piece_start,
        length: piece_len,
    });
    let prefix = if has_dat_header {
        Vec::new()
    } else {
        DAT_HEADER.to_vec()
    };
    let pieces: Vec<(u64, u64)> = w.pieces.iter().map(|p| (p.offset, p.length)).collect();
    let view = StreamView::new(src, prefix, pieces);
    let length = view.size();
    let mut rec = Recovered {
        offset,
        length,
        pieces: w.pieces.clone(),
        has_dat_header,
        groups: w.groups.len() as u64,
        video_frames: 0,
        duration_seconds: None,
        frame_rate: None,
        camera: None,
        recording: None,
        brand: None,
        start_timecode: w.tc_first,
        end,
        stitched: w.stitched,
        partial: w.partial.take(),
        overlaps_clips: Vec::new(),
        file: None,
        resyncs: w.resyncs,
    };
    if let Ok(index) = repi::walk(&view, &mut |_| true) {
        let lay = layout::analyze(&view, &index, "");
        rec.groups = index.groups.len() as u64;
        rec.video_frames = index
            .groups
            .iter()
            .filter(|g| g.kind.0 == *b"vide")
            .map(|g| g.complete as u64)
            .sum();
        rec.frame_rate = lay.frame_rate.map(|r| r.fps());
        rec.duration_seconds = rec.frame_rate.map(|fps| rec.video_frames as f64 / fps);
        rec.camera = lay.camera();
        rec.brand = lay.brand().map(|b| b.to_string());
        rec.recording = Some(lay.describe());
    }
    rec
}

/// Scans a card image or device path. The path is opened read-only.
pub fn scan_path(
    path: &Path,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<(ImageSource, ScanReport)> {
    let src = ImageSource::open(path).map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
            ". Raw devices need administrator rights; the safe way is to make an image of the \
             card first (see docs/format/card-carving.md)"
        } else {
            ""
        };
        Error::io(format!("cannot open {}{hint}", path.display()), e)
    })?;
    if src.size() == 0 {
        return Err(Error::NotADat(format!(
            "{} is empty or its size cannot be read",
            path.display()
        )));
    }
    let mut report = scan(&src, opts, on_event)?;
    report.is_device = src.is_device;
    Ok((src, report))
}

/// Scans `src` for Canon recordings, finished clips and fragments.
pub fn scan<R: ReadAt + Sync + ?Sized>(
    src: &R,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<ScanReport> {
    let t0 = Instant::now();
    let size = src.size();
    let mut notes = Vec::new();

    on_event(&ScanEvent::Phase {
        phase: ScanPhase::ReadingFileSystem,
        message: "reading the card's file system".into(),
    });
    let volumes = fs::find_volumes(src);
    if volumes.is_empty() {
        notes.push(
            "no exFAT or FAT32 file system was found; recordings are found by content only".into(),
        );
    }

    on_event(&ScanEvent::Phase {
        phase: ScanPhase::Scanning,
        message: format!("scanning {:.1} GB for Canon recordings", size as f64 / 1e9),
    });
    let sig = signature_pass(src, opts, on_event)?;

    on_event(&ScanEvent::Phase {
        phase: ScanPhase::Following,
        message: format!(
            "following {} recording start(s), {} clip header(s), {} group header(s)",
            sig.dat_starts.len(),
            sig.ftyps.len(),
            sig.hits.len()
        ),
    });

    // Finished clips.
    let mut clips: Vec<FinishedClip> = sig
        .ftyps
        .iter()
        .filter_map(|&at| read_clip(src, at))
        .collect();
    // A file-system entry pointing at a finding names it.
    let file_at = |off: u64| -> Option<FileRef> {
        volumes
            .iter()
            .flat_map(|v| &v.entries)
            .filter(|e| !e.is_dir && e.data_offset == Some(off))
            .min_by_key(|e| e.deleted)
            .map(|e| FileRef {
                path: e.path.clone(),
                size: e.size,
                deleted: e.deleted,
            })
    };
    for c in &mut clips {
        c.file = file_at(c.offset);
        on_event(&ScanEvent::Found {
            what: Found::Clip,
            offset: c.offset,
            length: c.length,
        });
    }

    let mut f = Follower {
        src,
        hits: &sig.hits,
        claimed: vec![false; sig.hits.len()],
        clip_ranges: clips
            .iter()
            .filter_map(|c| c.mdat.map(|m| (m.offset, m.offset + m.length)))
            .collect(),
        cluster: volumes.first().map(|v| v.cluster_size),
    };

    let mut recordings = Vec::new();
    for &start in &sig.dat_starts {
        if cancelled(opts) {
            return Err(Error::Cancelled);
        }
        let Some(i) = f.idx_at(start + 16) else {
            continue;
        };
        if f.claimed[i] {
            continue;
        }
        let w = f.walk(i, start, opts.stitch);
        let file = file_at(start);
        let known = file
            .as_ref()
            .filter(|e| !e.deleted && e.size > 0)
            .map(|e| e.size);
        let mut rec = finish(&f, w, start, true, known);
        rec.file = file;
        on_event(&ScanEvent::Found {
            what: Found::Recording,
            offset: rec.offset,
            length: rec.length,
        });
        recordings.push(rec);
    }

    // Headerless fragments: chains no `.DAT` start reached, outside finished clips.
    let mut fragments = Vec::new();
    for i in 0..sig.hits.len() {
        if f.claimed[i] || f.in_clip(sig.hits[i].offset) {
            continue;
        }
        let at = sig.hits[i].offset;
        // A fragment's stream gets a 16-byte header, so its first group sits at 16.
        let mut w = f.walk(i, at, false);
        w.piece_stream = 16;
        if w.groups.len() < MIN_FRAGMENT_GROUPS {
            continue;
        }
        let rec = finish(&f, w, at, false, None);
        on_event(&ScanEvent::Found {
            what: Found::Fragment,
            offset: rec.offset,
            length: rec.length,
        });
        fragments.push(rec);
    }

    // Overlaps with finished clips.
    let clip_spans: Vec<(u64, u64, u64)> = clips
        .iter()
        .map(|c| (c.offset, c.offset + c.length, c.offset))
        .collect();
    for r in recordings.iter_mut().chain(fragments.iter_mut()) {
        for p in &r.pieces {
            for &(a, b, id) in &clip_spans {
                if p.offset < b && a < p.offset + p.length && !r.overlaps_clips.contains(&id) {
                    r.overlaps_clips.push(id);
                }
            }
        }
    }

    // `.DAT` directory entries and what is at their first cluster.
    let mut dat_entries = Vec::new();
    for v in &volumes {
        for e in v.entries.iter().filter(|e| e.is_dat()) {
            let found = e
                .data_offset
                .and_then(|o| recordings.iter().find(|r| r.offset == o))
                .map(|r| r.offset);
            dat_entries.push(DatHint {
                path: e.path.clone(),
                size: e.size,
                deleted: e.deleted,
                data_offset: e.data_offset,
                recording: found,
            });
        }
    }
    for h in &dat_entries {
        if h.recording.is_none() {
            notes.push(format!(
                "{}{} points at {}, but no recording starts there",
                h.path,
                if h.deleted { " (deleted)" } else { "" },
                h.data_offset
                    .map_or("no cluster".to_string(), |o| format!("byte {o}"))
            ));
        }
    }

    let volumes = volumes
        .iter()
        .map(|v| VolumeSummary {
            kind: v.kind,
            offset: v.offset,
            cluster_size: v.cluster_size,
            heap_offset: v.heap_offset,
            files: v.entries.iter().filter(|e| !e.is_dir).count(),
            deleted: v.entries.iter().filter(|e| e.deleted).count(),
            warnings: v.warnings.clone(),
        })
        .collect();
    Ok(ScanReport {
        size,
        is_device: false,
        volumes,
        recordings,
        clips,
        fragments,
        dat_entries,
        headers_seen: sig.hits.len() as u64,
        seconds: t0.elapsed().as_secs_f64(),
        notes,
    })
}

/// Copies a recovered stream into a new `.DAT` file. Never overwrites: the output must not
/// exist. Returns the bytes written.
pub fn extract<R: ReadAt + ?Sized>(
    src: &R,
    rec: &Recovered,
    output: &Path,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<u64> {
    let prefix = if rec.has_dat_header {
        Vec::new()
    } else {
        DAT_HEADER.to_vec()
    };
    let pieces: Vec<(u64, u64)> = rec.pieces.iter().map(|p| (p.offset, p.length)).collect();
    let view = StreamView::new(src, prefix, pieces);
    copy_out(&view, output, opts, on_event)
}

/// Copies a finished clip into a new `.MP4` file.
pub fn extract_clip<R: ReadAt + ?Sized>(
    src: &R,
    clip: &FinishedClip,
    output: &Path,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<u64> {
    let view = StreamView::new(src, Vec::new(), vec![(clip.offset, clip.length)]);
    copy_out(&view, output, opts, on_event)
}

fn copy_out<R: ReadAt + ?Sized>(
    view: &R,
    output: &Path,
    opts: &ScanOptions,
    on_event: &mut dyn FnMut(&ScanEvent),
) -> Result<u64> {
    let total = view.size();
    let tmp = crate::repair::temp_path(output);
    let werr = |e| Error::io(format!("cannot write {}", output.display()), e);
    if output.exists() {
        return Err(Error::OutputExists(output.to_path_buf()));
    }
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(werr)?;
    let result = (|| -> Result<()> {
        let mut buf = vec![0u8; BLOCK];
        let mut off = 0u64;
        let mut next = 0u64;
        while off < total {
            if cancelled(opts) {
                return Err(Error::Cancelled);
            }
            let n = ((total - off) as usize).min(BLOCK);
            view.read_exact_at(&mut buf[..n], off)
                .map_err(|e| Error::io("cannot read the card image", e))?;
            file.write_all(&buf[..n]).map_err(werr)?;
            off += n as u64;
            if off >= next || off == total {
                next = off + (total / 200).max(1);
                on_event(&ScanEvent::Progress {
                    phase: ScanPhase::Extracting,
                    done: off,
                    total,
                });
            }
        }
        file.sync_all().map_err(werr)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&tmp, output).map_err(werr)?;
            Ok(total)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Finished clips worth trying as references for `rec`: same brand, nearest on the card
/// first, complete, at most `max` of them.
pub fn reference_candidates<'a>(
    report: &'a ScanReport,
    rec: &Recovered,
    max: usize,
) -> Vec<&'a FinishedClip> {
    let mut v: Vec<&FinishedClip> = report
        .clips
        .iter()
        .filter(|c| c.complete)
        .filter(|c| rec.brand.as_ref().is_none_or(|b| *b == c.brand))
        .filter(|c| !rec.overlaps_clips.contains(&c.offset))
        .collect();
    v.sort_by_key(|c| (c.offset as i128 - rec.offset as i128).unsigned_abs());
    v.truncate(max);
    v
}

/// File name for a recovered stream: `recovered_<offset>.DAT`.
pub fn recovered_name(rec: &Recovered) -> PathBuf {
    PathBuf::from(format!("recovered_{}.DAT", rec.offset))
}

/// Groups the findings by camera for a summary line.
pub fn cameras(report: &ScanReport) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in &report.recordings {
        *m.entry(r.camera.clone().unwrap_or_else(|| "unknown".into()))
            .or_default() += 1;
    }
    m
}

#[cfg(test)]
mod tests;
