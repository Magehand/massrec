//! Destination disks: free space, volume identity and the sequential write test.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::io::{AlignedBuf, DirectFile, ALIGN};

pub const TEST_FILE_NAME: &str = ".masscap_disktest.tmp";

/// Free bytes available to this process on the volume holding `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path"))?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide = to_wide(path);
        let mut free_to_caller = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_to_caller, &mut total, &mut free) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free_to_caller)
    }
}

/// A stable identifier for the volume holding `path`.
pub fn volume_serial(path: &Path) -> io::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::metadata(path)?;
        Ok(format!("dev-{:x}", md.dev()))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
        let wide = to_wide(path);
        let mut root = [0u16; 261];
        if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut serial = 0u32;
        let ok = unsafe {
            GetVolumeInformationW(root.as_ptr(), std::ptr::null_mut(), 0, &mut serial, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF))
    }
}

#[cfg(windows)]
fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Result of a throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputResult {
    pub bytes: u64,
    pub seconds: f64,
    /// Sustained bytes per second over the whole run, including the final sync.
    pub bytes_per_second: u64,
    /// Slowest single block, as bytes/s. A hint of how spiky the disk is.
    pub worst_block_bps: u64,
}

/// Write `total_bytes` sequentially with direct I/O in `block_bytes` blocks and
/// measure the sustained rate. The temp file is removed afterwards.
/// `progress` receives bytes written so far.
pub fn throughput_test(dir: &Path, total_bytes: u64, block_bytes: usize, mut progress: impl FnMut(u64)) -> io::Result<ThroughputResult> {
    std::fs::create_dir_all(dir)?;
    let block = (block_bytes / ALIGN).max(1) * ALIGN;
    let path: PathBuf = dir.join(TEST_FILE_NAME);
    let result = (|| {
        let mut f = DirectFile::create(&path)?;
        let mut buf = AlignedBuf::new(block);
        // Non-zero, non-repeating-ish content so compressing/dedup'ing volumes cannot cheat.
        let mut x: u32 = 0x9E37_79B9;
        for b in buf.as_mut_slice().iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        let blocks = total_bytes.div_ceil(block as u64).max(1);
        let start = Instant::now();
        let mut worst = f64::INFINITY;
        for i in 0..blocks {
            let t0 = Instant::now();
            f.write_aligned(i * block as u64, &buf, block)?;
            let dt = t0.elapsed().as_secs_f64();
            if dt > 0.0 {
                worst = worst.min(block as f64 / dt);
            }
            progress((i + 1) * block as u64);
        }
        f.sync()?;
        let seconds = start.elapsed().as_secs_f64().max(1e-6);
        let bytes = blocks * block as u64;
        Ok(ThroughputResult {
            bytes,
            seconds,
            bytes_per_second: (bytes as f64 / seconds) as u64,
            worst_block_bps: if worst.is_finite() { worst as u64 } else { 0 },
        })
    })();
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_and_serial_work_on_tempdir() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).unwrap() > 0);
        assert!(!volume_serial(d.path()).unwrap().is_empty());
    }

    #[test]
    fn small_throughput_test_runs_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let mut last = 0;
        let r = throughput_test(d.path(), 8 * 1024 * 1024, 1024 * 1024, |b| last = b).unwrap();
        assert_eq!(r.bytes, 8 * 1024 * 1024);
        assert_eq!(last, r.bytes);
        assert!(r.bytes_per_second > 0);
        assert!(!d.path().join(TEST_FILE_NAME).exists());
    }
}
