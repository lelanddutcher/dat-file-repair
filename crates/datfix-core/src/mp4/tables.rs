//! Sample tables and header boxes: parsing from a reference and building for the output.
//!
//! The sample table (`stbl`) tells a player where every sample is and when to show it:
//!
//! | Box | Meaning |
//! |---|---|
//! | `stsd` | Codec settings (copied from the reference). |
//! | `stts` | Decode duration of each sample, run-length encoded. |
//! | `ctts` | Composition offset (display time minus decode time), for B-frames. |
//! | `stss` | Which samples are keyframes. Absent means "all of them". |
//! | `stsc` | How many samples each chunk holds, run-length encoded. |
//! | `stsz` | Size of each sample, or one size for all. |
//! | `co64` | Absolute file offset of each chunk (64-bit). |

use super::atom::Atom;
use super::bytes::{ByteReader, PutBe};

/// Seconds between 1904-01-01 (the MP4 epoch) and 1970-01-01 (the Unix epoch).
pub const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;

// ---------------------------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------------------------

/// Full-box header: one version byte and three flag bytes.
fn full_box_header(out: &mut Vec<u8>, version: u8, flags: u32) {
    out.put_u32((u32::from(version) << 24) | (flags & 0x00ff_ffff));
}

/// `stts` with one entry per run of equal durations.
pub fn build_stts(runs: &[(u32, u32)]) -> Atom {
    let mut p = Vec::with_capacity(8 + runs.len() * 8);
    full_box_header(&mut p, 0, 0);
    p.put_u32(runs.len() as u32);
    for &(count, delta) in runs {
        p.put_u32(count);
        p.put_u32(delta);
    }
    Atom::leaf(b"stts", p)
}

/// `stts` for `count` samples that all last `delta` ticks. Counts above `u32::MAX` are split.
pub fn build_stts_constant(count: u64, delta: u32) -> Atom {
    let mut runs = Vec::new();
    let mut left = count;
    while left > 0 {
        let n = left.min(u32::MAX as u64) as u32;
        runs.push((n, delta));
        left -= n as u64;
    }
    build_stts(&runs)
}

/// `stss` listing 1-based keyframe sample numbers.
pub fn build_stss(sync_samples: &[u32]) -> Atom {
    let mut p = Vec::with_capacity(8 + sync_samples.len() * 4);
    full_box_header(&mut p, 0, 0);
    p.put_u32(sync_samples.len() as u32);
    for &s in sync_samples {
        p.put_u32(s);
    }
    Atom::leaf(b"stss", p)
}

/// `ctts` (version 0, unsigned offsets), run-length encoding the per-sample offsets.
pub fn build_ctts(offsets: &[u32]) -> Atom {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &o in offsets {
        match runs.last_mut() {
            Some((count, off)) if *off == o => *count += 1,
            _ => runs.push((1, o)),
        }
    }
    let mut p = Vec::with_capacity(8 + runs.len() * 8);
    full_box_header(&mut p, 0, 0);
    p.put_u32(runs.len() as u32);
    for (count, off) in runs {
        p.put_u32(count);
        p.put_u32(off);
    }
    Atom::leaf(b"ctts", p)
}

/// `stsc` from the number of samples in each chunk.
pub fn build_stsc(samples_per_chunk: &[u32]) -> Atom {
    let mut entries: Vec<(u32, u32)> = Vec::new(); // (first_chunk, samples_per_chunk)
    for (i, &n) in samples_per_chunk.iter().enumerate() {
        if entries.last().map(|e| e.1) != Some(n) {
            entries.push((i as u32 + 1, n));
        }
    }
    let mut p = Vec::with_capacity(8 + entries.len() * 12);
    full_box_header(&mut p, 0, 0);
    p.put_u32(entries.len() as u32);
    for (first, n) in entries {
        p.put_u32(first);
        p.put_u32(n);
        p.put_u32(1); // sample description index: always the single stsd entry
    }
    Atom::leaf(b"stsc", p)
}

/// `stsz` where every sample has the same size.
pub fn build_stsz_fixed(size: u32, count: u32) -> Atom {
    let mut p = Vec::with_capacity(12);
    full_box_header(&mut p, 0, 0);
    p.put_u32(size);
    p.put_u32(count);
    Atom::leaf(b"stsz", p)
}

/// `stsz` with one entry per sample.
pub fn build_stsz(sizes: &[u32]) -> Atom {
    let mut p = Vec::with_capacity(12 + sizes.len() * 4);
    full_box_header(&mut p, 0, 0);
    p.put_u32(0);
    p.put_u32(sizes.len() as u32);
    for &s in sizes {
        p.put_u32(s);
    }
    Atom::leaf(b"stsz", p)
}

/// `co64`: 64-bit chunk offsets, so files larger than 4 GiB work.
pub fn build_co64(offsets: &[u64]) -> Atom {
    let mut p = Vec::with_capacity(8 + offsets.len() * 8);
    full_box_header(&mut p, 0, 0);
    p.put_u32(offsets.len() as u32);
    for &o in offsets {
        p.put_u64(o);
    }
    Atom::leaf(b"co64", p)
}

/// `elst` with a single edit: play `segment_duration` (movie timescale) starting at
/// `media_time` (media timescale) at normal speed.
pub fn build_elst(segment_duration: u64, media_time: i64) -> Atom {
    let v1 = segment_duration > u32::MAX as u64 || media_time > i32::MAX as i64;
    let mut p = Vec::new();
    full_box_header(&mut p, v1 as u8, 0);
    p.put_u32(1);
    if v1 {
        p.put_u64(segment_duration);
        p.put_u64(media_time as u64);
    } else {
        p.put_u32(segment_duration as u32);
        p.put_u32(media_time as i32 as u32);
    }
    p.put_u16(1); // media rate integer part
    p.put_u16(0); // media rate fraction
    Atom::leaf(b"elst", p)
}

// ---------------------------------------------------------------------------------------------
// Header boxes with durations: mvhd, tkhd, mdhd
// ---------------------------------------------------------------------------------------------

/// The shared layout of `mvhd`, `tkhd` and `mdhd`.
///
/// All three start with version/flags, creation time and modification time, then some
/// box-specific fields (`middle`), then a duration, then more fields (`tail`). Version 0 uses
/// 32-bit times and duration; version 1 uses 64-bit. Rewriting through this struct lets the
/// writer change times and duration and switch to version 1 when a value no longer fits.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedHeader {
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    /// mvhd/mdhd: the 4-byte timescale. tkhd: track id + 4 reserved bytes.
    pub middle: Vec<u8>,
    pub duration: u64,
    pub tail: Vec<u8>,
}

impl TimedHeader {
    /// Parses a payload. `middle_len` is 4 for `mvhd`/`mdhd` and 8 for `tkhd`.
    pub fn parse(payload: &[u8], middle_len: usize) -> Result<Self, String> {
        let mut r = ByteReader::new(payload);
        let vf = r.u32()?;
        let version = (vf >> 24) as u8;
        let (ctime, mtime) = if version == 1 {
            (r.u64()?, r.u64()?)
        } else {
            (r.u32()? as u64, r.u32()? as u64)
        };
        let middle = r.bytes(middle_len)?.to_vec();
        let duration = if version == 1 {
            r.u64()?
        } else {
            // 0xFFFFFFFF means "unknown" in version 0.
            match r.u32()? {
                u32::MAX => u64::MAX,
                d => d as u64,
            }
        };
        let tail = r.bytes(r.remaining())?.to_vec();
        Ok(TimedHeader {
            flags: vf & 0x00ff_ffff,
            creation_time: ctime,
            modification_time: mtime,
            middle,
            duration,
            tail,
        })
    }

    /// Serializes, picking version 0 when every value fits in 32 bits.
    pub fn to_payload(&self) -> Vec<u8> {
        let v1 = self.creation_time > u32::MAX as u64
            || self.modification_time > u32::MAX as u64
            || self.duration >= u32::MAX as u64;
        let mut p = Vec::with_capacity(36 + self.middle.len() + self.tail.len());
        full_box_header(&mut p, v1 as u8, self.flags);
        if v1 {
            p.put_u64(self.creation_time);
            p.put_u64(self.modification_time);
        } else {
            p.put_u32(self.creation_time as u32);
            p.put_u32(self.modification_time as u32);
        }
        p.extend_from_slice(&self.middle);
        if v1 {
            p.put_u64(self.duration);
        } else {
            p.put_u32(self.duration as u32);
        }
        p.extend_from_slice(&self.tail);
        p
    }

    /// The timescale stored in `middle` for `mvhd` and `mdhd`.
    pub fn timescale(&self) -> u32 {
        u32::from_be_bytes([
            self.middle[0],
            self.middle[1],
            self.middle[2],
            self.middle[3],
        ])
    }
}

// ---------------------------------------------------------------------------------------------
// Parsers used on the reference
// ---------------------------------------------------------------------------------------------

/// Entries of an `stts` box as `(count, delta)`.
pub fn parse_stts(payload: &[u8]) -> Result<Vec<(u32, u32)>, String> {
    let mut r = ByteReader::new(payload);
    r.skip(4)?;
    let n = r.u32()? as usize;
    (0..n).map(|_| Ok((r.u32()?, r.u32()?))).collect()
}

/// Entries of an `stsc` box as `(first_chunk, samples_per_chunk, description_index)`.
pub fn parse_stsc(payload: &[u8]) -> Result<Vec<(u32, u32, u32)>, String> {
    let mut r = ByteReader::new(payload);
    r.skip(4)?;
    let n = r.u32()? as usize;
    (0..n).map(|_| Ok((r.u32()?, r.u32()?, r.u32()?))).collect()
}

/// `stsz` as `(fixed_size, count, per-sample sizes)`. The list is empty when `fixed_size > 0`.
pub fn parse_stsz(payload: &[u8]) -> Result<(u32, u32, Vec<u32>), String> {
    let mut r = ByteReader::new(payload);
    r.skip(4)?;
    let fixed = r.u32()?;
    let count = r.u32()?;
    let sizes = if fixed == 0 {
        (0..count).map(|_| r.u32()).collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    Ok((fixed, count, sizes))
}

/// Chunk offsets from `stco` (32-bit) or `co64` (64-bit).
pub fn parse_chunk_offsets(atom: &Atom) -> Result<Vec<u64>, String> {
    let payload = atom.payload().ok_or("chunk offset box is not a leaf")?;
    let mut r = ByteReader::new(payload);
    r.skip(4)?;
    let n = r.u32()? as usize;
    if atom.kind.0 == *b"co64" {
        (0..n).map(|_| r.u64()).collect()
    } else {
        (0..n).map(|_| r.u32().map(u64::from)).collect()
    }
}

/// Per-sample composition offsets from a `ctts` box (version 1 offsets may be negative).
pub fn parse_ctts(payload: &[u8]) -> Result<Vec<i64>, String> {
    let mut r = ByteReader::new(payload);
    let version = r.u8()?;
    r.skip(3)?;
    let n = r.u32()? as usize;
    let mut out = Vec::new();
    for _ in 0..n {
        let count = r.u32()?;
        let raw = r.u32()?;
        let off = if version == 1 {
            raw as i32 as i64
        } else {
            raw as i64
        };
        out.extend(std::iter::repeat_n(off, count as usize));
    }
    Ok(out)
}

/// 1-based sample numbers from an `stss` box.
pub fn parse_stss(payload: &[u8]) -> Result<Vec<u32>, String> {
    let mut r = ByteReader::new(payload);
    r.skip(4)?;
    let n = r.u32()? as usize;
    (0..n).map(|_| r.u32()).collect()
}

/// Media time of the first edit in an `elst` box (`-1` means an empty edit).
pub fn parse_elst_first_media_time(payload: &[u8]) -> Result<Option<i64>, String> {
    let mut r = ByteReader::new(payload);
    let version = r.u8()?;
    r.skip(3)?;
    let n = r.u32()?;
    for _ in 0..n {
        let media_time = if version == 1 {
            r.skip(8)?;
            r.u64()? as i64
        } else {
            r.skip(4)?;
            r.u32()? as i32 as i64
        };
        r.skip(4)?;
        if media_time >= 0 {
            return Ok(Some(media_time));
        }
    }
    Ok(None)
}

/// Absolute `(offset, size)` of every sample described by a sample table.
pub fn sample_locations(stbl: &Atom) -> Result<Vec<(u64, u32)>, String> {
    let stsz = stbl
        .child(b"stsz")
        .and_then(Atom::payload)
        .ok_or("no stsz")?;
    let stsc = stbl
        .child(b"stsc")
        .and_then(Atom::payload)
        .ok_or("no stsc")?;
    let co = stbl
        .child(b"co64")
        .or_else(|| stbl.child(b"stco"))
        .ok_or("no stco/co64")?;
    let (fixed, count, sizes) = parse_stsz(stsz)?;
    let stsc = parse_stsc(stsc)?;
    let chunks = parse_chunk_offsets(co)?;
    let size_of = |i: usize| if fixed > 0 { fixed } else { sizes[i] };

    let mut out = Vec::with_capacity(count as usize);
    for (ci, &chunk_off) in chunks.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        // The last stsc entry whose first_chunk <= this chunk applies.
        let spc = stsc
            .iter()
            .rev()
            .find(|e| e.0 <= chunk_no)
            .map(|e| e.1)
            .ok_or("stsc does not cover the first chunk")?;
        let mut off = chunk_off;
        for _ in 0..spc {
            if out.len() >= count as usize {
                break;
            }
            let s = size_of(out.len());
            out.push((off, s));
            off += s as u64;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stsc_merges_runs() {
        let a = build_stsc(&[60, 60, 60, 12]);
        assert_eq!(
            parse_stsc(a.payload().unwrap()).unwrap(),
            vec![(1, 60, 1), (4, 12, 1)]
        );
    }

    #[test]
    fn ctts_round_trip() {
        let offs = [1001, 3003, 0, 0, 2002, 2002];
        let a = build_ctts(&offs);
        let back = parse_ctts(a.payload().unwrap()).unwrap();
        assert_eq!(back, offs.iter().map(|&o| o as i64).collect::<Vec<_>>());
    }

    #[test]
    fn timed_header_upgrades_to_version_1() {
        // mdhd v0: vf, ctime, mtime, timescale, duration, language+quality
        let mut p = vec![0, 0, 0, 0];
        p.extend_from_slice(&5u32.to_be_bytes());
        p.extend_from_slice(&6u32.to_be_bytes());
        p.extend_from_slice(&48000u32.to_be_bytes());
        p.extend_from_slice(&100u32.to_be_bytes());
        p.extend_from_slice(&[0x55, 0xc4, 0, 0]);
        let mut h = TimedHeader::parse(&p, 4).unwrap();
        assert_eq!(h.timescale(), 48000);
        assert_eq!(h.to_payload(), p);
        h.duration = 1 << 40;
        let v1 = h.to_payload();
        assert_eq!(v1[0], 1);
        assert_eq!(TimedHeader::parse(&v1, 4).unwrap(), h);
    }

    #[test]
    fn sample_locations_follow_chunks() {
        let stbl = Atom::container(
            b"stbl",
            vec![
                build_stsc(&[2, 1]),
                build_stsz(&[10, 20, 30]),
                build_co64(&[1000, 5000]),
            ],
        );
        assert_eq!(
            sample_locations(&stbl).unwrap(),
            vec![(1000, 10), (1010, 20), (5000, 30)]
        );
    }

    #[test]
    fn elst_round_trip() {
        let e = build_elst(29_681_652, 1001);
        assert_eq!(
            parse_elst_first_media_time(e.payload().unwrap()).unwrap(),
            Some(1001)
        );
        let e = build_elst(1 << 40, 0);
        assert_eq!(e.payload().unwrap()[0], 1);
        assert_eq!(
            parse_elst_first_media_time(e.payload().unwrap()).unwrap(),
            Some(0)
        );
    }
}
