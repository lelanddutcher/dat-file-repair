//! Positional reads ("read N bytes at offset X") over files and in-memory buffers.
//!
//! The walker and the video analyzer jump around a file that may be 80 GB or more. Reading at
//! an absolute offset keeps that code simple: there is no shared cursor to keep in sync. The
//! same trait is implemented for byte slices so unit tests can use synthetic data in memory.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Mutex;

/// A source of bytes that can be read at any offset.
pub trait ReadAt {
    /// Total length in bytes.
    fn size(&self) -> u64;

    /// Reads up to `buf.len()` bytes starting at `offset`. Returns how many bytes were read;
    /// zero means end of data.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Fills `buf` completely or fails with `UnexpectedEof`.
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads exactly `len` bytes at `offset` into a new vector.
    fn read_vec(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; len];
        self.read_exact_at(&mut v, offset)?;
        Ok(v)
    }
}

impl ReadAt for [u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if offset >= self.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let n = buf.len().min(self.len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
}

impl ReadAt for Vec<u8> {
    fn size(&self) -> u64 {
        self.as_slice().size()
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.as_slice().read_at(buf, offset)
    }
}

/// A file opened read-only, with its length cached.
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    /// Opens `path` for reading. The file is never opened with write access.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileSource { file, len })
    }

    /// The underlying file, for metadata queries.
    pub fn file(&self) -> &File {
        &self.file
    }
}

impl ReadAt for FileSource {
    fn size(&self) -> u64 {
        self.len
    }

    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        // `seek_read` moves the file cursor, but nothing in this crate relies on the cursor.
        std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
    }
}

/// Smallest region marked unreadable after a read error. Card sectors are 512 bytes, but a
/// failing card usually loses whole erase blocks, and every failed read can take seconds.
pub const BAD_BLOCK: u64 = 64 * 1024;
/// Largest region skipped at once after consecutive failures (the skip doubles each time).
const MAX_SKIP: u64 = 16 * 1024 * 1024;

/// True for errors that mean "this part of the medium cannot be read" (a bad sector), as
/// opposed to "the file or device is gone". Only these are skipped over.
pub fn is_media_error(e: &io::Error) -> bool {
    match e.raw_os_error() {
        // EIO.
        #[cfg(unix)]
        Some(5) => true,
        // ERROR_CRC, ERROR_SECTOR_NOT_FOUND, ERROR_READ_FAULT, ERROR_DEVICE_HARDWARE_ERROR,
        // ERROR_IO_DEVICE.
        #[cfg(windows)]
        Some(23 | 27 | 30 | 483 | 1117) => true,
        _ => false,
    }
}

/// A byte range `[offset, offset + len)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Region {
    pub offset: u64,
    pub len: u64,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.offset + self.len
    }
    /// True when `[start, end)` shares a byte with this region.
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        start < self.end() && self.offset < end
    }
}

#[derive(Default)]
struct BadState {
    /// Sorted, non-overlapping.
    regions: Vec<Region>,
    /// Length of the last skip, doubled while failures follow each other.
    run: u64,
    last_error: Option<(u64, io::ErrorKind, Option<i32>)>,
    exceeded: bool,
}

/// Wraps a source on a card or disk that may have unreadable sectors.
///
/// When `skip` is on, a read that fails with a media error ([`is_media_error`]) does not stop
/// the job: the failing block is recorded and read as zeros. Consecutive failures skip
/// growing distances (64 KiB up to 16 MiB), so a dead region costs a few timeouts, not one per
/// sector. Known bad regions are never read again. The caller asks for
/// [`Tolerant::bad_regions`] afterwards and treats them as damage.
///
/// When more than `limit` bytes are unreadable (the card was probably removed), reads fail
/// with the last error and [`Tolerant::exceeded`] turns true.
pub struct Tolerant<R> {
    inner: R,
    skip: bool,
    limit: u64,
    state: Mutex<BadState>,
}

impl<R: ReadAt> Tolerant<R> {
    /// `skip = false` passes every error through unchanged.
    pub fn new(inner: R, skip: bool) -> Self {
        let size = inner.size();
        Tolerant {
            inner,
            skip,
            limit: (size / 4).max(64 * 1024 * 1024),
            state: Mutex::new(BadState::default()),
        }
    }

    /// Overrides the unreadable-bytes limit (tests).
    pub fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    pub fn inner(&self) -> &R {
        &self.inner
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BadState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Regions read as zeros so far, sorted.
    pub fn bad_regions(&self) -> Vec<Region> {
        self.state().regions.clone()
    }

    /// Total unreadable bytes so far.
    pub fn bad_bytes(&self) -> u64 {
        self.state().regions.iter().map(|r| r.len).sum()
    }

    /// True once more than the limit was unreadable.
    pub fn exceeded(&self) -> bool {
        self.state().exceeded
    }

    /// The last read error: `(offset, error)`.
    pub fn last_error(&self) -> Option<(u64, io::Error)> {
        self.state().last_error.map(|(off, kind, raw)| {
            let e = match raw {
                Some(code) => io::Error::from_raw_os_error(code),
                None => io::Error::from(kind),
            };
            (off, e)
        })
    }

    /// End of the known bad region holding `offset`, or the start of the next one.
    fn lookup(&self, offset: u64) -> (Option<u64>, u64) {
        let st = self.state();
        let i = st.regions.partition_point(|r| r.end() <= offset);
        match st.regions.get(i) {
            Some(r) if r.offset <= offset => (Some(r.end()), u64::MAX),
            Some(r) => (None, r.offset),
            None => (None, u64::MAX),
        }
    }

    /// Records a failure at `offset` and returns the end of the region now marked bad.
    fn mark(&self, offset: u64, e: &io::Error) -> u64 {
        let size = self.inner.size();
        let mut st = self.state();
        let start = offset / BAD_BLOCK * BAD_BLOCK;
        let follows = st.regions.last().is_some_and(|r| r.end() == start);
        st.run = if follows {
            (st.run * 2).clamp(BAD_BLOCK, MAX_SKIP)
        } else {
            BAD_BLOCK
        };
        let end = (start + st.run).min(size);
        // Insert and merge.
        let i = st.regions.partition_point(|r| r.end() < start);
        let mut new = Region {
            offset: start,
            len: end - start,
        };
        let mut j = i;
        while j < st.regions.len() && st.regions[j].offset <= new.end() {
            let r = st.regions[j];
            let s = r.offset.min(new.offset);
            let e = r.end().max(new.end());
            new = Region {
                offset: s,
                len: e - s,
            };
            j += 1;
        }
        st.regions.splice(i..j, [new]);
        st.last_error = Some((offset, e.kind(), e.raw_os_error()));
        let total: u64 = st.regions.iter().map(|r| r.len).sum();
        if total > self.limit {
            st.exceeded = true;
        }
        end
    }
}

impl<R: ReadAt> ReadAt for Tolerant<R> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if !self.skip {
            return self.inner.read_at(buf, offset);
        }
        let size = self.size();
        if offset >= size || buf.is_empty() {
            return self.inner.read_at(buf, offset);
        }
        if self.exceeded() {
            let (_, e) = self.last_error().expect("exceeded after an error");
            return Err(e);
        }
        let want = (buf.len() as u64).min(size - offset);
        let (inside, next_bad) = self.lookup(offset);
        if let Some(end) = inside {
            let n = want.min(end - offset) as usize;
            buf[..n].fill(0);
            return Ok(n);
        }
        let want = want.min(next_bad - offset) as usize;
        match self.inner.read_at(&mut buf[..want], offset) {
            Ok(n) => Ok(n),
            Err(e) if is_media_error(&e) => {
                // Narrow a large failed read down to the first failing block: read the part
                // before the next block boundary on its own.
                let block_end = ((offset / BAD_BLOCK + 1) * BAD_BLOCK).min(size);
                if want as u64 > block_end - offset {
                    let first = (block_end - offset) as usize;
                    match self.inner.read_at(&mut buf[..first], offset) {
                        Ok(n) => return Ok(n),
                        Err(e2) if is_media_error(&e2) => {}
                        Err(e2) => return Err(e2),
                    }
                }
                let end = self.mark(offset, &e);
                if self.exceeded() {
                    return Err(e);
                }
                let n = (want as u64).min(end - offset) as usize;
                buf[..n].fill(0);
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }
}

impl<R: ReadAt + ?Sized> ReadAt for &R {
    fn size(&self) -> u64 {
        (**self).size()
    }
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        (**self).read_at(buf, offset)
    }
}

/// Reads `[start, end)` from a second file (where an earlier pass already copied it) and
/// everything else from `base`. The second pass of a repair over a failing card reads the
/// payload back from its own first output instead of from the card.
pub struct Overlay<'a, R: ?Sized> {
    pub base: &'a R,
    pub copy: FileSource,
    pub start: u64,
    pub end: u64,
    /// Offset of `start` inside `copy`.
    pub copy_offset: u64,
}

impl<R: ReadAt + ?Sized> ReadAt for Overlay<'_, R> {
    fn size(&self) -> u64 {
        self.base.size()
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if offset >= self.start && offset < self.end {
            let n = (buf.len() as u64).min(self.end - offset) as usize;
            self.copy
                .read_at(&mut buf[..n], offset - self.start + self.copy_offset)
        } else {
            let n = if offset < self.start {
                (buf.len() as u64).min(self.start - offset) as usize
            } else {
                buf.len()
            };
            self.base.read_at(&mut buf[..n], offset)
        }
    }
}

/// An in-memory source whose reads fail with a media error inside given ranges. Simulates a
/// failing card in tests.
pub struct FaultySource {
    pub data: Vec<u8>,
    pub bad: Vec<(u64, u64)>,
    /// Number of reads that hit a bad range (each would be a slow timeout on a real card).
    pub failures: std::sync::atomic::AtomicU64,
}

impl FaultySource {
    pub fn new(data: Vec<u8>, bad: Vec<(u64, u64)>) -> Self {
        FaultySource {
            data,
            bad,
            failures: Default::default(),
        }
    }

    /// The OS error code for "I/O error" on this platform.
    pub fn media_error() -> io::Error {
        #[cfg(windows)]
        return io::Error::from_raw_os_error(23);
        #[cfg(not(windows))]
        return io::Error::from_raw_os_error(5);
    }
}

impl ReadAt for FaultySource {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let end = offset + buf.len() as u64;
        if self.bad.iter().any(|&(a, b)| offset < b && a < end) {
            self.failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(Self::media_error());
        }
        self.data.read_at(buf, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_reads_stop_at_end() {
        let data = vec![1u8, 2, 3, 4];
        let mut buf = [0u8; 3];
        assert_eq!(data.read_at(&mut buf, 2).unwrap(), 2);
        assert_eq!(&buf[..2], &[3, 4]);
        assert_eq!(data.read_at(&mut buf, 9).unwrap(), 0);
        assert!(data.read_exact_at(&mut buf, 2).is_err());
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8 + 1).collect()
    }

    #[test]
    fn tolerant_reads_zeros_for_bad_blocks_and_records_them() {
        let data = pattern(1 << 20);
        let bad_at = 300_000u64;
        let src = Tolerant::new(
            FaultySource::new(data.clone(), vec![(bad_at, bad_at + 10)]),
            true,
        );
        let all = src.read_vec(0, data.len()).unwrap();
        let block = bad_at / BAD_BLOCK * BAD_BLOCK;
        assert_eq!(
            src.bad_regions(),
            vec![Region {
                offset: block,
                len: BAD_BLOCK
            }]
        );
        assert_eq!(&all[..block as usize], &data[..block as usize]);
        assert!(
            all[block as usize..(block + BAD_BLOCK) as usize]
                .iter()
                .all(|&b| b == 0)
        );
        assert_eq!(
            &all[(block + BAD_BLOCK) as usize..],
            &data[(block + BAD_BLOCK) as usize..]
        );
        // Known bad regions are not read again.
        let before = src
            .inner()
            .failures
            .load(std::sync::atomic::Ordering::Relaxed);
        src.read_vec(block, 100).unwrap();
        let after = src
            .inner()
            .failures
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(before, after);
    }

    #[test]
    fn tolerant_skips_growing_steps_over_a_dead_region() {
        let data = pattern(64 << 20);
        let src = Tolerant::new(FaultySource::new(data, vec![(1 << 20, 41 << 20)]), true);
        src.read_vec(0, 64 << 20).unwrap();
        let fails = src
            .inner()
            .failures
            .load(std::sync::atomic::Ordering::Relaxed);
        // 40 MiB dead: a few dozen failed reads, not 640 (one per 64 KiB block).
        assert!(fails < 40, "{fails} failed reads");
        let r = src.bad_regions();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].offset, 1 << 20);
        assert!(r[0].end() >= 41 << 20);
    }

    #[test]
    fn tolerant_gives_up_past_its_limit_and_passes_errors_when_off() {
        let data = pattern(4 << 20);
        let src = Tolerant::new(FaultySource::new(data.clone(), vec![(0, 4 << 20)]), true)
            .with_limit(1 << 20);
        assert!(src.read_vec(0, 4 << 20).is_err());
        assert!(src.exceeded());
        let off = Tolerant::new(FaultySource::new(data, vec![(10, 20)]), false);
        let e = off.read_vec(0, 100).unwrap_err();
        assert!(is_media_error(&e));
        assert!(off.bad_regions().is_empty());
    }
}
