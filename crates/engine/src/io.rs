//! Sector-aligned buffers and direct (unbuffered, write-through) file output.
//!
//! On Windows files are opened with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH`,
//! which requires every write to be a whole number of sectors at a sector-aligned
//! offset from a sector-aligned buffer. We use 4096 as the alignment unit, which
//! satisfies both 512e and 4Kn drives. On macOS/Linux the same code path uses
//! `F_NOCACHE` / plain writes so the engine can be developed and tested anywhere.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub const ALIGN: usize = 4096;

/// A heap buffer aligned to [`ALIGN`] whose length is a multiple of [`ALIGN`].
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> Self {
        assert!(len > 0 && len.is_multiple_of(ALIGN), "AlignedBuf length must be a multiple of {ALIGN}");
        let layout = Layout::from_size_align(len, ALIGN).unwrap();
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        Self { ptr, len }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, Layout::from_size_align(self.len, ALIGN).unwrap()) }
    }
}

/// Round `n` up to the next multiple of [`ALIGN`].
pub fn align_up(n: usize) -> usize {
    n.div_ceil(ALIGN) * ALIGN
}

/// A file opened for direct positional writes.
pub struct DirectFile {
    file: File,
}

impl DirectFile {
    /// Create (truncating) a file for direct I/O.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.write(true).read(true).create(true).truncate(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH};
            opts.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
        }
        let file = opts.open(path)?;
        #[cfg(target_os = "macos")]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }
        Ok(Self { file })
    }

    /// Write a whole aligned buffer prefix (`len` must be a multiple of ALIGN) at `offset`.
    pub fn write_aligned(&mut self, offset: u64, buf: &AlignedBuf, len: usize) -> io::Result<()> {
        debug_assert!(len.is_multiple_of(ALIGN) && len <= buf.len());
        debug_assert!(offset.is_multiple_of(ALIGN as u64));
        write_all_at(&self.file, &buf.as_slice()[..len], offset)
    }

    /// Set the final length. Used once at close to trim the padding of the last block.
    pub fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        while !buf.is_empty() {
            match file.write_at(buf, offset) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            match file.seek_write(buf, offset) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buf_is_aligned() {
        let b = AlignedBuf::new(ALIGN * 3);
        assert_eq!(b.as_slice().as_ptr() as usize % ALIGN, 0);
        assert_eq!(b.len(), ALIGN * 3);
        assert!(b.as_slice().iter().all(|&x| x == 0));
    }

    #[test]
    fn direct_file_writes_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.bin");
        let mut f = DirectFile::create(&p).unwrap();
        let mut b = AlignedBuf::new(ALIGN * 2);
        b.as_mut_slice().fill(7);
        f.write_aligned(0, &b, ALIGN * 2).unwrap();
        f.write_aligned(ALIGN as u64 * 2, &b, ALIGN).unwrap();
        f.set_len(ALIGN as u64 * 2 + 100).unwrap();
        drop(f);
        let data = std::fs::read(&p).unwrap();
        assert_eq!(data.len(), ALIGN * 2 + 100);
        assert!(data.iter().all(|&x| x == 7));
    }
}
