//! Positional reads ("read N bytes at offset X") over files and in-memory buffers.
//!
//! The walker and the video analyzer jump around a file that may be 80 GB or more. Reading at
//! an absolute offset keeps that code simple: there is no shared cursor to keep in sync. The
//! same trait is implemented for byte slices so unit tests can use synthetic data in memory.

use std::fs::File;
use std::io;
use std::path::Path;

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
}
