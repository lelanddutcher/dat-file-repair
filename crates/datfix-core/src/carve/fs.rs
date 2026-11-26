//! Minimal, read-only exFAT and FAT32 parsing for card images.
//!
//! The carver does not need the file system to find recordings: it finds them by their
//! content. The file system adds three things:
//!
//! 1. **Names.** A recovered stream that starts where a `.DAT` entry points gets its name.
//! 2. **Hints.** A deleted or 0-byte `.DAT` entry still holds its first cluster (exFAT keeps
//!    it in deleted entries; FAT32 keeps at least the low 16 bits). The report says whether
//!    data was found there.
//! 3. **Cluster geometry.** Cluster size and the start of the cluster heap make fragment
//!    stitching exact.
//!
//! Only the boot sector, the FAT and directory clusters are read. Partition tables: none
//! ("superfloppy"), MBR, and GPT.

use serde::Serialize;

use crate::read_at::ReadAt;

/// Limits that keep a damaged file system from making the walk loop or explode.
const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 200_000;
const MAX_DIR_BYTES: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FsKind {
    ExFat,
    Fat32,
}

/// One directory entry (live or deleted).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DirEntry {
    /// Path from the volume root, `/` separated. Deleted FAT32 short names start with `_`.
    pub path: String,
    pub size: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    /// Absolute image offset of the first cluster, when the cluster number is valid.
    pub data_offset: Option<u64>,
    /// When the file was created (the camera creates a `.DAT` when recording starts).
    pub created: Option<FsTime>,
    /// When the file was last written (about when recording stopped).
    pub modified: Option<FsTime>,
}

/// A FAT/exFAT timestamp: local time of the device that wrote it, and the time-zone offset
/// when the file system stores one (exFAT only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FsTime {
    /// Seconds since 1970, counting the device's local time as if it were UTC.
    pub local: i64,
    /// Seconds east of UTC, when recorded.
    pub utc_offset: Option<i32>,
}

impl FsTime {
    /// UTC seconds since 1970, when the offset is known.
    pub fn utc(&self) -> Option<i64> {
        self.utc_offset.map(|o| self.local - o as i64)
    }
}

/// Decodes a DOS date and time (FAT32 and exFAT share the bit layout) plus a count of 10 ms
/// units (0–199) that adds up to two seconds. Returns `None` for zero or invalid dates.
pub fn dos_time(date: u16, time: u16, centis: u8) -> Option<i64> {
    let (y, m, d) = (
        1980 + (date >> 9) as i64,
        ((date >> 5) & 0xf) as i64,
        (date & 0x1f) as i64,
    );
    let (h, mi, s2) = (
        (time >> 11) as i64,
        ((time >> 5) & 0x3f) as i64,
        (time & 0x1f) as i64,
    );
    if date == 0 || !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s2 > 29
    {
        return None;
    }
    let extra = if centis < 200 { centis as i64 / 100 } else { 0 };
    Some(crate::layout::days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s2 * 2 + extra)
}

/// exFAT time-zone byte: bit 7 says the offset is valid, bits 0–6 are a signed count of
/// 15-minute steps.
fn exfat_offset(b: u8) -> Option<i32> {
    if b & 0x80 == 0 {
        return None;
    }
    let steps = ((b << 1) as i8 >> 1) as i32;
    Some(steps * 15 * 60)
}

fn exfat_time(stamp: &[u8], centis: u8, zone: u8) -> Option<FsTime> {
    let v = u32::from_le_bytes(stamp.try_into().ok()?);
    Some(FsTime {
        local: dos_time((v >> 16) as u16, v as u16, centis)?,
        utc_offset: exfat_offset(zone),
    })
}

impl DirEntry {
    /// The file name without folders.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// True for `*.DAT` (any case). macOS `._` metadata files are not recordings.
    pub fn is_dat(&self) -> bool {
        !self.is_dir
            && !self.name().starts_with("._")
            && self.name().to_ascii_uppercase().ends_with(".DAT")
    }
}

/// A FAT-family volume found in the image.
#[derive(Clone, Debug, Serialize)]
pub struct Volume {
    pub kind: FsKind,
    /// Image offset of the volume's boot sector.
    pub offset: u64,
    pub cluster_size: u64,
    /// Image offset of cluster 2 (the first data cluster).
    pub heap_offset: u64,
    pub cluster_count: u32,
    pub entries: Vec<DirEntry>,
    /// Problems met while walking directories (the walk keeps going).
    pub warnings: Vec<String>,
    #[serde(skip)]
    fat_offset: u64,
}

impl Volume {
    /// Image offset of `cluster`, or `None` when it is outside the heap.
    pub fn cluster_offset(&self, cluster: u32) -> Option<u64> {
        (cluster >= 2 && cluster - 2 < self.cluster_count)
            .then(|| self.heap_offset + (cluster - 2) as u64 * self.cluster_size)
    }

    /// True when `offset` is a cluster boundary of this volume.
    pub fn is_cluster_boundary(&self, offset: u64) -> bool {
        offset >= self.heap_offset && (offset - self.heap_offset) % self.cluster_size == 0
    }

    fn fat_next<R: ReadAt + ?Sized>(&self, src: &R, cluster: u32) -> Option<u32> {
        let mut b = [0u8; 4];
        src.read_exact_at(&mut b, self.fat_offset + cluster as u64 * 4)
            .ok()?;
        let v = u32::from_le_bytes(b);
        let v = match self.kind {
            FsKind::Fat32 => v & 0x0fff_ffff,
            FsKind::ExFat => v,
        };
        (v >= 2 && v - 2 < self.cluster_count).then_some(v)
    }

    /// Reads a cluster chain (or `contiguous_len` bytes of consecutive clusters).
    fn read_chain<R: ReadAt + ?Sized>(
        &self,
        src: &R,
        first: u32,
        contiguous_len: Option<u64>,
    ) -> Option<Vec<u8>> {
        let start = self.cluster_offset(first)?;
        if let Some(len) = contiguous_len {
            return src.read_vec(start, len.min(MAX_DIR_BYTES) as usize).ok();
        }
        let mut out = Vec::new();
        let mut c = first;
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(c) || out.len() as u64 >= MAX_DIR_BYTES {
                break;
            }
            let at = self.cluster_offset(c)?;
            out.extend(src.read_vec(at, self.cluster_size as usize).ok()?);
            match self.fat_next(src, c) {
                Some(n) => c = n,
                None => break,
            }
        }
        Some(out)
    }
}

/// Finds FAT-family volumes: at offset 0, or in MBR/GPT partitions.
pub fn find_volumes<R: ReadAt + ?Sized>(src: &R) -> Vec<Volume> {
    let mut out = Vec::new();
    if let Some(v) = open_volume(src, 0) {
        out.push(v);
        return out;
    }
    for start in partition_starts(src) {
        if let Some(v) = open_volume(src, start) {
            out.push(v);
        }
    }
    out
}

/// Byte offsets of partitions listed in an MBR or a GPT.
fn partition_starts<R: ReadAt + ?Sized>(src: &R) -> Vec<u64> {
    let Ok(mbr) = src.read_vec(0, 512) else {
        return Vec::new();
    };
    if mbr[510..512] != [0x55, 0xaa] {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..4 {
        let e = &mbr[446 + 16 * i..446 + 16 * (i + 1)];
        let kind = e[4];
        let lba = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
        if kind == 0xee {
            out.extend(gpt_starts(src));
        } else if kind != 0 && lba > 0 {
            out.push(lba * 512);
        }
    }
    out
}

fn gpt_starts<R: ReadAt + ?Sized>(src: &R) -> Vec<u64> {
    // The GPT header is at LBA 1. Try 512- and 4096-byte sectors.
    for sector in [512u64, 4096] {
        let Ok(h) = src.read_vec(sector, 92) else {
            continue;
        };
        if &h[0..8] != b"EFI PART" {
            continue;
        }
        let table = u64::from_le_bytes(h[72..80].try_into().unwrap()) * sector;
        let count = u32::from_le_bytes(h[80..84].try_into().unwrap()).min(256) as u64;
        let size = u32::from_le_bytes(h[84..88].try_into().unwrap()) as u64;
        if !(128..=1024).contains(&size) {
            continue;
        }
        let Ok(t) = src.read_vec(table, (count * size) as usize) else {
            continue;
        };
        return t
            .chunks_exact(size as usize)
            .filter(|e| e[..16].iter().any(|&b| b != 0))
            .map(|e| u64::from_le_bytes(e[32..40].try_into().unwrap()) * sector)
            .collect();
    }
    Vec::new()
}

/// Reads the boot sector at `at` and walks the directory tree.
pub fn open_volume<R: ReadAt + ?Sized>(src: &R, at: u64) -> Option<Volume> {
    let b = src.read_vec(at, 512).ok()?;
    let mut v = if &b[3..11] == b"EXFAT   " {
        exfat_geometry(&b, at)?
    } else if &b[82..90] == b"FAT32   " && b[510..512] == [0x55, 0xaa] {
        fat32_geometry(&b, at)?
    } else {
        return None;
    };
    let root = match v.kind {
        FsKind::ExFat => u32::from_le_bytes(b[96..100].try_into().unwrap()),
        FsKind::Fat32 => u32::from_le_bytes(b[44..48].try_into().unwrap()),
    };
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    walk_dir(src, &v, root, None, "", 0, &mut entries, &mut warnings);
    v.entries = entries;
    v.warnings = warnings;
    Some(v)
}

fn exfat_geometry(b: &[u8], at: u64) -> Option<Volume> {
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64;
    let bps_shift = b[108];
    let spc_shift = b[109];
    if !(9..=12).contains(&bps_shift) || bps_shift as u32 + spc_shift as u32 > 25 {
        return None;
    }
    let bps = 1u64 << bps_shift;
    Some(Volume {
        kind: FsKind::ExFat,
        offset: at,
        cluster_size: bps << spc_shift,
        heap_offset: at + u32_at(88) * bps,
        cluster_count: u32_at(92) as u32,
        entries: Vec::new(),
        warnings: Vec::new(),
        fat_offset: at + u32_at(80) * bps,
    })
}

fn fat32_geometry(b: &[u8], at: u64) -> Option<Volume> {
    let bps = u16::from_le_bytes([b[11], b[12]]) as u64;
    let spc = b[13] as u64;
    let reserved = u16::from_le_bytes([b[14], b[15]]) as u64;
    let fats = b[16] as u64;
    let total = u32::from_le_bytes(b[32..36].try_into().unwrap()) as u64;
    let fat_size = u32::from_le_bytes(b[36..40].try_into().unwrap()) as u64;
    if ![512, 1024, 2048, 4096].contains(&bps) || spc == 0 || !spc.is_power_of_two() || fats == 0 {
        return None;
    }
    let data_start = reserved + fats * fat_size;
    let clusters = total.saturating_sub(data_start) / spc;
    Some(Volume {
        kind: FsKind::Fat32,
        offset: at,
        cluster_size: bps * spc,
        heap_offset: at + data_start * bps,
        cluster_count: clusters.min(u32::MAX as u64) as u32,
        entries: Vec::new(),
        warnings: Vec::new(),
        fat_offset: at + reserved * bps,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk_dir<R: ReadAt + ?Sized>(
    src: &R,
    v: &Volume,
    first: u32,
    contiguous_len: Option<u64>,
    prefix: &str,
    depth: usize,
    out: &mut Vec<DirEntry>,
    warnings: &mut Vec<String>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    let Some(data) = v.read_chain(src, first, contiguous_len) else {
        warnings.push(format!("cannot read directory {prefix}/ (cluster {first})"));
        return;
    };
    let found = match v.kind {
        FsKind::ExFat => parse_exfat_dir(&data),
        FsKind::Fat32 => parse_fat32_dir(&data),
    };
    for (mut e, contiguous) in found {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        e.path = format!("{prefix}/{}", e.path);
        e.data_offset = v.cluster_offset(e.first_cluster);
        let recurse = e.is_dir && !e.deleted && e.first_cluster >= 2;
        let (path, cl, size) = (e.path.clone(), e.first_cluster, e.size);
        out.push(e);
        if recurse {
            let len = contiguous.then_some(size);
            walk_dir(src, v, cl, len, &path, depth + 1, out, warnings);
        }
    }
}

/// Parses exFAT directory entry sets. Returns each file with its "no FAT chain" flag.
pub fn parse_exfat_dir(data: &[u8]) -> Vec<(DirEntry, bool)> {
    let mut out = Vec::new();
    let entries: Vec<&[u8]> = data.chunks_exact(32).collect();
    let mut i = 0;
    while i < entries.len() {
        let e = entries[i];
        let t = e[0];
        if t == 0 {
            break; // end of directory
        }
        // File entry: 0x85 in use, 0x05 deleted.
        if t & 0x7f != 0x05 {
            i += 1;
            continue;
        }
        let deleted = t & 0x80 == 0;
        let secondary = e[1] as usize;
        let attrs = u16::from_le_bytes([e[4], e[5]]);
        if !(2..=18).contains(&secondary) || i + secondary >= entries.len() {
            i += 1;
            continue;
        }
        let stream = entries[i + 1];
        if stream[0] & 0x7f != 0x40 {
            i += 1;
            continue;
        }
        let no_fat_chain = stream[1] & 0x02 != 0;
        let name_len = stream[3] as usize;
        let first_cluster = u32::from_le_bytes(stream[20..24].try_into().unwrap());
        let size = u64::from_le_bytes(stream[24..32].try_into().unwrap());
        let mut units: Vec<u16> = Vec::new();
        for n in &entries[i + 2..=i + secondary] {
            if n[0] & 0x7f != 0x41 {
                break;
            }
            units.extend(
                n[2..32]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]])),
            );
        }
        units.truncate(name_len);
        out.push((
            DirEntry {
                path: String::from_utf16_lossy(&units),
                size,
                first_cluster,
                deleted,
                is_dir: attrs & 0x10 != 0,
                data_offset: None,
                created: exfat_time(&e[8..12], e[20], e[22]),
                modified: exfat_time(&e[12..16], e[21], e[23]),
            },
            no_fat_chain,
        ));
        i += 1 + secondary;
    }
    out
}

/// Parses FAT32 directory entries, joining long names. FAT32 never marks chains contiguous.
pub fn parse_fat32_dir(data: &[u8]) -> Vec<(DirEntry, bool)> {
    let mut out = Vec::new();
    let mut lfn: Vec<Vec<u16>> = Vec::new();
    for e in data.chunks_exact(32) {
        if e[0] == 0 {
            break;
        }
        let deleted = e[0] == 0xe5;
        let attr = e[11];
        if attr == 0x0f {
            let mut part = Vec::with_capacity(13);
            for r in [1..11, 14..26, 28..32] {
                part.extend(
                    e[r].chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]])),
                );
            }
            lfn.push(part);
            continue;
        }
        let pieces = std::mem::take(&mut lfn);
        if attr & 0x08 != 0 || e[0] == b'.' {
            continue; // volume label, "." and ".."
        }
        let long: Vec<u16> = pieces
            .into_iter()
            .rev()
            .flatten()
            .take_while(|&u| u != 0 && u != 0xffff)
            .collect();
        let name = if long.is_empty() {
            short_name(e, deleted)
        } else {
            String::from_utf16_lossy(&long)
        };
        let hi = u16::from_le_bytes([e[20], e[21]]) as u32;
        let lo = u16::from_le_bytes([e[26], e[27]]) as u32;
        out.push((
            DirEntry {
                path: name,
                size: u32::from_le_bytes(e[28..32].try_into().unwrap()) as u64,
                first_cluster: (hi << 16) | lo,
                deleted,
                is_dir: attr & 0x10 != 0,
                data_offset: None,
                created: dos_time(le16(e, 16), le16(e, 14), e[13]).map(|local| FsTime {
                    local,
                    utc_offset: None,
                }),
                modified: dos_time(le16(e, 24), le16(e, 22), 0).map(|local| FsTime {
                    local,
                    utc_offset: None,
                }),
            },
            false,
        ));
    }
    out
}

fn le16(e: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([e[at], e[at + 1]])
}

fn short_name(e: &[u8], deleted: bool) -> String {
    let mut base: Vec<u8> = e[0..8].to_vec();
    if deleted {
        base[0] = b'_';
    }
    let base = String::from_utf8_lossy(&base).trim_end().to_string();
    let ext = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn dos_times() {
        // 2024-12-21 13:40:42 plus 1.99 s of 10 ms units.
        let date = (44 << 9) | (12 << 5) | 21;
        let time = (13 << 11) | (40 << 5) | 21;
        assert_eq!(dos_time(date, time, 0), Some(1_734_788_442));
        assert_eq!(dos_time(date, time, 199), Some(1_734_788_443));
        assert_eq!(dos_time(0, 0, 0), None);
        assert_eq!(dos_time(date, 24 << 11, 0), None);
        assert_eq!(exfat_offset(0x00), None);
        assert_eq!(exfat_offset(0x80 | 20), Some(5 * 3600));
        assert_eq!(exfat_offset(0x80 | 0x6c), Some(-5 * 3600)); // -20 steps
    }

    #[test]
    fn fat32_entry_times() {
        let mut e = [0u8; 32];
        e[0..11].copy_from_slice(b"LAD12345DAT");
        e[11] = 0x20;
        let date: u16 = (42 << 9) | (4 << 5) | 24;
        let time: u16 = (10 << 11) | (22 << 5) | 16;
        e[13] = 100;
        e[14..16].copy_from_slice(&time.to_le_bytes());
        e[16..18].copy_from_slice(&date.to_le_bytes());
        e[22..24].copy_from_slice(&(time + 4).to_le_bytes());
        e[24..26].copy_from_slice(&date.to_le_bytes());
        let (d, _) = &parse_fat32_dir(&e)[0];
        let local = crate::layout::days_from_civil(2022, 4, 24) * 86_400 + 37_352;
        assert_eq!(
            d.created,
            Some(FsTime {
                local: local + 1,
                utc_offset: None
            })
        );
        assert_eq!(d.modified.unwrap().local, local + 8);
        assert_eq!(d.created.unwrap().utc(), None);
    }

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// One exFAT file entry set (file + stream + names).
    pub(crate) fn exfat_set(
        name: &str,
        first_cluster: u32,
        size: u64,
        dir: bool,
        deleted: bool,
    ) -> Vec<u8> {
        let units = utf16(name);
        let name_entries = units.len().div_ceil(15);
        let mut v = vec![0u8; 32 * (2 + name_entries)];
        let del = |t: u8| if deleted { t & 0x7f } else { t };
        v[0] = del(0x85);
        v[1] = (1 + name_entries) as u8;
        v[4] = if dir { 0x10 } else { 0x20 };
        let s = &mut v[32..64];
        s[0] = del(0xc0);
        s[1] = 0x01 | if dir { 0x02 } else { 0 };
        s[3] = units.len() as u8;
        s[8..16].copy_from_slice(&size.to_le_bytes());
        s[20..24].copy_from_slice(&first_cluster.to_le_bytes());
        s[24..32].copy_from_slice(&size.to_le_bytes());
        for (k, chunk) in units.chunks(15).enumerate() {
            let n = &mut v[64 + 32 * k..96 + 32 * k];
            n[0] = del(0xc1);
            for (j, u) in chunk.iter().enumerate() {
                n[2 + 2 * j..4 + 2 * j].copy_from_slice(&u.to_le_bytes());
            }
        }
        v
    }

    /// A tiny exFAT volume: 512-byte sectors, 4 KiB clusters, FAT at sector 24, heap at 32.
    /// Root at cluster 2 holds `DCIM/` (cluster 3); `DCIM` holds the given entries.
    pub(crate) fn exfat_image(dcim_entries: &[Vec<u8>], at: u64, total: usize) -> Vec<u8> {
        let mut img = vec![0u8; total];
        let b = &mut img[at as usize..at as usize + 512];
        b[0..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
        b[3..11].copy_from_slice(b"EXFAT   ");
        b[80..84].copy_from_slice(&24u32.to_le_bytes());
        b[84..88].copy_from_slice(&8u32.to_le_bytes());
        b[88..92].copy_from_slice(&32u32.to_le_bytes());
        let clusters = ((total as u64 - at - 32 * 512) / 4096) as u32;
        b[92..96].copy_from_slice(&clusters.to_le_bytes());
        b[96..100].copy_from_slice(&2u32.to_le_bytes());
        b[108] = 9;
        b[109] = 3;
        b[510] = 0x55;
        b[511] = 0xaa;
        let fat = at as usize + 24 * 512;
        for c in [2usize, 3] {
            img[fat + 4 * c..fat + 4 * c + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        }
        let heap = at as usize + 32 * 512;
        let root = exfat_set("DCIM", 3, 4096, true, false);
        img[heap..heap + root.len()].copy_from_slice(&root);
        let mut off = heap + 4096;
        for e in dcim_entries {
            img[off..off + e.len()].copy_from_slice(e);
            off += e.len();
        }
        img
    }

    #[test]
    fn exfat_lists_live_deleted_and_empty_entries() {
        let entries = vec![
            exfat_set("A_0001C001_LAD04.MP4", 10, 50_000, false, false),
            exfat_set("A_0001C002_LAD04.DAT", 0, 0, false, false),
            exfat_set("LAD12345.DAT", 40, 67_108_864, false, true),
        ];
        let img = exfat_image(&entries, 0, 512 * 1024);
        let vols = find_volumes(img.as_slice());
        assert_eq!(vols.len(), 1);
        let v = &vols[0];
        assert_eq!(v.kind, FsKind::ExFat);
        assert_eq!(v.cluster_size, 4096);
        assert_eq!(v.heap_offset, 32 * 512);
        let names: Vec<&str> = v.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "/DCIM",
                "/DCIM/A_0001C001_LAD04.MP4",
                "/DCIM/A_0001C002_LAD04.DAT",
                "/DCIM/LAD12345.DAT"
            ]
        );
        let del = &v.entries[3];
        assert!(del.deleted && del.is_dat());
        assert_eq!(del.first_cluster, 40);
        assert_eq!(del.data_offset, Some(32 * 512 + 38 * 4096));
        let empty = &v.entries[2];
        assert_eq!((empty.size, empty.data_offset), (0, None));
    }

    #[test]
    fn exfat_inside_mbr_partition() {
        let mut img = exfat_image(&[exfat_set("X.DAT", 5, 1, false, true)], 8192, 256 * 1024);
        img[446 + 4] = 0x07;
        img[446 + 8..446 + 12].copy_from_slice(&16u32.to_le_bytes());
        img[510] = 0x55;
        img[511] = 0xaa;
        let vols = find_volumes(img.as_slice());
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].offset, 8192);
        assert_eq!(
            vols[0].entries[1].data_offset,
            Some(8192 + 32 * 512 + 3 * 4096)
        );
    }

    fn fat_short(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> Vec<u8> {
        let mut e = vec![0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e
    }

    fn fat_lfn(seq: u8, chars: &str) -> Vec<u8> {
        let mut u = utf16(chars);
        u.push(0);
        u.resize(13, 0xffff);
        let mut e = vec![0u8; 32];
        e[0] = seq;
        e[11] = 0x0f;
        let slots = (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2));
        for (s, c) in slots.zip(u) {
            e[s..s + 2].copy_from_slice(&c.to_le_bytes());
        }
        e
    }

    #[test]
    fn fat32_parses_long_short_and_deleted_names() {
        let mut d = Vec::new();
        d.extend(fat_short(b"CARD       ", 0x08, 0, 0)); // volume label
        d.extend(fat_lfn(0x41, "LAD00001.MP4"));
        d.extend(fat_short(b"LAD00001MP4", 0x20, 0x0001_0005, 1234));
        let mut l = fat_lfn(0x41, "LAD00002.DAT");
        l[0] = 0xe5;
        d.extend(l);
        let mut s = fat_short(b"LAD00002DAT", 0x20, 9, 0);
        s[0] = 0xe5;
        d.extend(s);
        let mut s = fat_short(b"OLD     DAT", 0x20, 7, 0);
        s[0] = 0xe5;
        d.extend(s);
        d.extend(vec![0u8; 32]);
        let got = parse_fat32_dir(&d);
        let names: Vec<&str> = got.iter().map(|(e, _)| e.path.as_str()).collect();
        assert_eq!(names, ["LAD00001.MP4", "LAD00002.DAT", "_LD.DAT"]);
        assert_eq!(got[0].0.first_cluster, 0x0001_0005);
        assert!(got[1].0.deleted && got[1].0.is_dat());
        assert_eq!(got[2].0.first_cluster, 7);
    }

    #[test]
    fn fat32_volume_geometry_and_root_walk() {
        // 512-byte sectors, 8 sectors per cluster, 32 reserved, 2 FATs of 16 sectors.
        let total = 512 * 1024;
        let mut img = vec![0u8; total];
        let b = &mut img[..512];
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 8;
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        b[16] = 2;
        b[32..36].copy_from_slice(&((total / 512) as u32).to_le_bytes());
        b[36..40].copy_from_slice(&16u32.to_le_bytes());
        b[44..48].copy_from_slice(&2u32.to_le_bytes());
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xaa;
        let fat = 32 * 512;
        img[fat + 8..fat + 12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
        let heap = (32 + 32) * 512;
        let e = fat_short(b"LAD00003DAT", 0x20, 4, 0);
        img[heap..heap + 32].copy_from_slice(&e);
        let vols = find_volumes(img.as_slice());
        assert_eq!(vols.len(), 1);
        let v = &vols[0];
        assert_eq!(
            (v.kind, v.cluster_size, v.heap_offset),
            (FsKind::Fat32, 4096, heap as u64)
        );
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].path, "/LAD00003.DAT");
        assert_eq!(v.entries[0].data_offset, Some(heap as u64 + 2 * 4096));
        assert!(v.is_cluster_boundary(heap as u64 + 4096));
        assert!(!v.is_cluster_boundary(heap as u64 + 512));
    }

    #[test]
    fn no_volume_in_random_data() {
        let img = vec![0x5au8; 64 * 1024];
        assert!(find_volumes(img.as_slice()).is_empty());
    }
}
