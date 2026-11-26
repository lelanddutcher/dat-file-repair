//! What the output drive can hold: file system type, largest file, free space.
//!
//! A repaired file is about as large as its `.DAT`, often tens of GB. Two drive limits stop
//! such a write late and confusingly, so they are checked before the write starts:
//!
//! - **FAT32** (and FAT16) cannot store a file of 4 GiB or more. Many SD cards and USB sticks
//!   are FAT32. The write fails at exactly 4 GiB with "File too large".
//! - **Free space.** The write fails when the drive is full.
//!
//! macOS and Linux use `statfs`/`statvfs`; Windows uses `GetVolumeInformationW` and
//! `GetDiskFreeSpaceExW`. Every query may fail (network shares often hide the file system
//! type); callers treat an unknown value as "no limit known".

use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Largest file FAT12/16/32 can hold: 4 GiB - 1 byte.
pub const FAT_MAX_FILE: u64 = (1 << 32) - 1;

/// Headroom kept free on the output drive beyond the file itself.
pub const SPACE_HEADROOM: u64 = 64 * 1024 * 1024;

/// Facts about the drive that holds a folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    /// File system name as the system reports it: `apfs`, `msdos`, `exfat`, `FAT32`, `NTFS`...
    pub fs_name: Option<String>,
    /// Largest single file the file system can store, when it has a known small limit.
    pub max_file_size: Option<u64>,
    /// Bytes available to this user.
    pub free_bytes: Option<u64>,
}

impl VolumeInfo {
    /// A plain name for messages: `FAT32` rather than `msdos`.
    pub fn fs_label(&self) -> String {
        match self.fs_name.as_deref() {
            Some(n) if is_fat(n) => "FAT32".into(),
            Some(n) => n.to_string(),
            None => "unknown".into(),
        }
    }
}

/// True for the FAT family names reported by macOS (`msdos`), Linux (`vfat`, `msdos`) and
/// Windows (`FAT`, `FAT32`).
pub fn is_fat(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "msdos" | "vfat" | "fat" | "fat12" | "fat16" | "fat32"
    )
}

/// Reads what the drive holding `dir` can store. Never fails: unknown values are `None`.
pub fn volume_info(dir: &Path) -> VolumeInfo {
    let mut info = sys::query(dir);
    if info.max_file_size.is_none() && info.fs_name.as_deref().is_some_and(is_fat) {
        info.max_file_size = Some(FAT_MAX_FILE);
    }
    info
}

/// Checks that a file of `size` bytes fits on the drive that holds `dir`.
pub fn check_fits(dir: &Path, size: u64) -> Result<()> {
    check_fits_with(dir, size, &volume_info(dir))
}

/// [`check_fits`] with the drive facts given (tests use made-up drives).
pub fn check_fits_with(dir: &Path, size: u64, info: &VolumeInfo) -> Result<()> {
    if let Some(limit) = info.max_file_size {
        if size > limit {
            return Err(Error::FileTooLargeForVolume {
                dir: dir.to_path_buf(),
                fs: info.fs_label(),
                size,
                limit,
            });
        }
    }
    if let Some(free) = info.free_bytes {
        let need = size.saturating_add(SPACE_HEADROOM);
        if free < need {
            return Err(Error::NotEnoughSpace {
                dir: dir.to_path_buf(),
                need: size,
                free,
            });
        }
    }
    Ok(())
}

/// The folder a file will be written into (`.` for a bare file name).
pub fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Checks that `dir` exists and accepts new files, by creating and deleting a small file.
/// Catches read-only cards and shares, missing folders and permission problems before a long
/// repair rather than after it.
pub fn check_writable(dir: &Path) -> Result<()> {
    let err = |source: io::Error| Error::OutputNotWritable {
        dir: dir.to_path_buf(),
        source,
    };
    if !dir.is_dir() {
        return Err(err(io::Error::new(
            io::ErrorKind::NotFound,
            "the folder does not exist",
        )));
    }
    let probe = dir.join(format!(".datfix-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        // A leftover probe from a crashed run: the folder is writable.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(err(e)),
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
mod sys {
    use super::VolumeInfo;
    use std::ffi::{CStr, CString};
    use std::path::Path;

    // Field widths differ between platforms (u32 on some), so the casts are kept.
    #[allow(clippy::unnecessary_cast)]
    pub fn query(dir: &Path) -> VolumeInfo {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
            return VolumeInfo::default();
        };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path and `st` is a properly sized out buffer.
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return VolumeInfo::default();
        }
        // SAFETY: the kernel NUL-terminates `f_fstypename`.
        let name = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        VolumeInfo {
            fs_name: Some(name),
            max_file_size: None,
            free_bytes: Some((st.f_bavail as u64).saturating_mul(st.f_bsize as u64)),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use super::VolumeInfo;
    use std::ffi::CString;
    use std::path::Path;

    // Field widths differ between platforms (u32 on some), so the casts are kept.
    #[allow(clippy::unnecessary_cast)]
    pub fn query(dir: &Path) -> VolumeInfo {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
            return VolumeInfo::default();
        };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid path and out buffer.
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return VolumeInfo::default();
        }
        // MSDOS_SUPER_MAGIC (vfat and msdos), EXFAT_SUPER_MAGIC.
        let name = match st.f_type as i64 {
            0x4d44 => Some("vfat".to_string()),
            0x2011_bab0 => Some("exfat".to_string()),
            _ => None,
        };
        VolumeInfo {
            fs_name: name,
            max_file_size: None,
            free_bytes: Some((st.f_bavail as u64).saturating_mul(st.f_bsize as u64)),
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::VolumeInfo;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetVolumeInformationW, GetVolumePathNameW,
    };

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub fn query(dir: &Path) -> VolumeInfo {
        let path = wide(dir);
        let mut root = vec![0u16; 32_768];
        // SAFETY: `path` is NUL-terminated; `root` is a writable buffer of the given length.
        let ok = unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        let mut info = VolumeInfo::default();
        if ok != 0 {
            let mut fs = vec![0u16; 64];
            // SAFETY: `root` is NUL-terminated by the call above; optional outputs are null.
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            };
            if ok != 0 {
                let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
                info.fs_name = Some(
                    std::ffi::OsString::from_wide(&fs[..end])
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        let mut free: u64 = 0;
        // SAFETY: `path` is NUL-terminated; the two unused outputs are null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            info.free_bytes = Some(free);
        }
        info
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "android",
    windows
)))]
mod sys {
    use super::VolumeInfo;
    pub fn query(_: &std::path::Path) -> VolumeInfo {
        VolumeInfo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat32_refuses_files_of_4_gib() {
        let fat = VolumeInfo {
            fs_name: Some("msdos".into()),
            max_file_size: Some(FAT_MAX_FILE),
            free_bytes: Some(1 << 40),
        };
        let dir = Path::new("/Volumes/CARD");
        assert!(check_fits_with(dir, FAT_MAX_FILE, &fat).is_ok());
        let err = check_fits_with(dir, 5_000_000_000, &fat).unwrap_err();
        assert!(matches!(err, Error::FileTooLargeForVolume { .. }));
        let m = err.user_message();
        assert_eq!(m.kind, "file_too_large_for_volume");
        assert!(m.message.contains("FAT32"), "{}", m.message);
        assert!(m.next_step.contains("exFAT"));
    }

    #[test]
    fn free_space_includes_headroom() {
        let small = VolumeInfo {
            fs_name: Some("apfs".into()),
            max_file_size: None,
            free_bytes: Some(1_000_000_000),
        };
        let dir = Path::new("/tmp");
        assert!(check_fits_with(dir, 900_000_000, &small).is_ok());
        let err = check_fits_with(dir, 990_000_000, &small).unwrap_err();
        assert_eq!(err.user_message().kind, "disk_full");
        // Unknown values never block a write.
        assert!(check_fits_with(dir, u64::MAX, &VolumeInfo::default()).is_ok());
    }

    #[test]
    fn this_machine_reports_its_temp_drive() {
        let info = volume_info(&std::env::temp_dir());
        if cfg!(any(target_os = "macos", windows)) {
            assert!(info.fs_name.is_some(), "{info:?}");
        }
        if cfg!(any(unix, windows)) {
            assert!(info.free_bytes.unwrap_or(0) > 0, "{info:?}");
        }
        assert!(is_fat("msdos") && is_fat("FAT32") && !is_fat("exfat"));
    }

    #[test]
    fn write_probe_finds_missing_and_read_only_folders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_writable(dir.path()).is_ok());
        let missing = dir.path().join("nope");
        let err = check_writable(&missing).unwrap_err();
        assert_eq!(err.user_message().kind, "output_not_writable");
        assert!(err.user_message().message.contains("does not exist"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = dir.path().join("ro");
            std::fs::create_dir(&ro).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
            // Root ignores permissions; only check when the probe really fails.
            if let Err(e) = check_writable(&ro) {
                let m = e.user_message();
                assert_eq!(m.kind, "output_not_writable");
                assert!(m.next_step.contains("another folder"));
            }
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}
