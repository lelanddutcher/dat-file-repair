//! Read-only access to a card image or a raw device.
//!
//! A card image is a plain file (`dd` output, `.img`, `.iso`, a raw `.dmg` made by Disk
//! Utility or `hdiutil create`). A raw device is `/dev/rdiskN` on macOS, `/dev/sdX` on Linux
//! or `\\.\PhysicalDriveN` on Windows. Devices need reads that start and end on a sector
//! boundary, and their length is not in the file metadata. [`ImageSource`] hides both.
//!
//! Nothing here ever opens a path for writing.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::read_at::ReadAt;

/// Read alignment used for devices. 4096 covers 512-byte and 4K-sector media.
pub const DEVICE_ALIGN: u64 = 4096;

/// A card image or device, opened read-only.
pub struct ImageSource {
    file: File,
    len: u64,
    /// Reads are widened to multiples of this (1 for regular files).
    align: u64,
    /// True when the path is a block or character device.
    pub is_device: bool,
}

/// What kind of path a user gave.
pub fn is_device_path(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if let Ok(m) = std::fs::metadata(path) {
            let t = m.file_type();
            return t.is_block_device() || t.is_char_device();
        }
        false
    }
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        s.starts_with(r"\\.\") || s.starts_with(r"\\?\GLOBALROOT")
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

impl ImageSource {
    /// Opens `path` for reading only.
    pub fn open(path: &Path) -> io::Result<Self> {
        let is_device = is_device_path(path);
        // `File::open` is read-only on every platform. On Windows it shares read and write
        // access, which a mounted volume needs.
        let file = File::open(path)?;
        let meta_len = file.metadata()?.len();
        let mut src = ImageSource {
            file,
            len: meta_len,
            align: if is_device { DEVICE_ALIGN } else { 1 },
            is_device,
        };
        if is_device || meta_len == 0 {
            src.len = src.probe_len()?;
            if src.len > 0 && !is_device {
                // A zero-length "file" that still returns data is a device in disguise.
                src.align = DEVICE_ALIGN;
                src.is_device = true;
            }
        }
        Ok(src)
    }

    /// Finds the device length by probing reads: the largest aligned offset that still
    /// returns data. About 2 × 40 small reads.
    fn probe_len(&self) -> io::Result<u64> {
        let a = DEVICE_ALIGN;
        let mut buf = vec![0u8; a as usize];
        let readable = |off: u64, buf: &mut [u8]| -> bool {
            matches!(self.raw_read(buf, off), Ok(n) if n > 0)
        };
        if !readable(0, &mut buf) {
            return Ok(0);
        }
        // Grow an upper bound, then binary search on block numbers.
        let mut hi_block: u64 = 1;
        while readable(hi_block * a, &mut buf) {
            hi_block *= 2;
            if hi_block > (1u64 << 40) {
                return Err(io::Error::other("device length probe did not terminate"));
            }
        }
        let mut lo_block = hi_block / 2; // readable
        while hi_block - lo_block > 1 {
            let mid = (lo_block + hi_block) / 2;
            if readable(mid * a, &mut buf) {
                lo_block = mid;
            } else {
                hi_block = mid;
            }
        }
        let n = self.raw_read(&mut buf, lo_block * a)?;
        Ok(lo_block * a + n as u64)
    }

    #[cfg(unix)]
    fn raw_read(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
    }

    #[cfg(windows)]
    fn raw_read(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
    }

    /// Reads `buf.len()` bytes at an offset that is already a multiple of the alignment.
    /// Used by the streaming scan, which reads large aligned blocks.
    pub fn read_block(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.align > 1 && (offset % self.align != 0 || buf.len() as u64 % self.align != 0) {
            return self.read_at(buf, offset);
        }
        let want = (self.len.saturating_sub(offset)).min(buf.len() as u64) as usize;
        let mut done = 0;
        while done < want {
            match self.raw_read(&mut buf[done..want], offset + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }
}

impl ReadAt for ImageSource {
    fn size(&self) -> u64 {
        self.len
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = (self.len - offset).min(buf.len() as u64) as usize;
        if self.align <= 1 {
            return self.raw_read(&mut buf[..want], offset);
        }
        let a = self.align;
        if offset % a == 0 && want as u64 % a == 0 {
            return self.read_block(&mut buf[..want], offset);
        }
        // Widen to aligned bounds, read, copy out the middle.
        let start = offset / a * a;
        let end = (offset + want as u64).div_ceil(a) * a;
        let mut tmp = vec![0u8; (end - start) as usize];
        let got = self.read_block(&mut tmp, start)?;
        let skip = (offset - start) as usize;
        if got <= skip {
            return Ok(0);
        }
        let n = (got - skip).min(want);
        buf[..n].copy_from_slice(&tmp[skip..skip + n]);
        Ok(n)
    }
}

/// A view of a stream that lives in one or more pieces of the image.
///
/// Offset 0 of the view is the start of the first piece. Used to read a recovered recording
/// as if it were one `.DAT` file, without copying it.
pub struct StreamView<'a, R: ReadAt + ?Sized> {
    src: &'a R,
    /// Bytes put in front of the pieces (a synthetic `.DAT` header for orphan chains).
    prefix: Vec<u8>,
    /// `(image offset, length)` of each piece, in stream order.
    pieces: Vec<(u64, u64)>,
    len: u64,
}

impl<'a, R: ReadAt + ?Sized> StreamView<'a, R> {
    pub fn new(src: &'a R, prefix: Vec<u8>, pieces: Vec<(u64, u64)>) -> Self {
        let len = prefix.len() as u64 + pieces.iter().map(|p| p.1).sum::<u64>();
        StreamView {
            src,
            prefix,
            pieces,
            len,
        }
    }
}

impl<R: ReadAt + ?Sized> ReadAt for StreamView<'_, R> {
    fn size(&self) -> u64 {
        self.len
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let plen = self.prefix.len() as u64;
        if offset < plen {
            let n = ((plen - offset) as usize).min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[offset as usize..offset as usize + n]);
            return Ok(n);
        }
        let mut rel = offset - plen;
        for &(at, len) in &self.pieces {
            if rel < len {
                let n = ((len - rel) as usize).min(buf.len());
                return self.src.read_at(&mut buf[..n], at + rel);
            }
            rel -= len;
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_maps_pieces_and_prefix() {
        let img: Vec<u8> = (0u8..100).collect();
        let v = StreamView::new(img.as_slice(), vec![0xaa, 0xbb], vec![(10, 5), (50, 3)]);
        assert_eq!(v.size(), 10);
        let all = v.read_vec(0, 10).unwrap();
        assert_eq!(all, vec![0xaa, 0xbb, 10, 11, 12, 13, 14, 50, 51, 52]);
        let mid = v.read_vec(5, 4).unwrap();
        assert_eq!(mid, vec![13, 14, 50, 51]);
    }

    #[test]
    fn regular_file_opens_with_its_length() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("img.bin");
        std::fs::write(&p, vec![7u8; 10_000]).unwrap();
        let s = ImageSource::open(&p).unwrap();
        assert_eq!(s.size(), 10_000);
        assert!(!s.is_device);
        let mut b = [0u8; 8];
        assert_eq!(s.read_at(&mut b, 9_996).unwrap(), 4);
    }
}
