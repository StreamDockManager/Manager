use std::path::Path;

// Require compaction space
pub(crate) fn require_compaction_space(path: &Path, database_bytes: u64) -> Result<(), String> {
    let required = database_bytes.saturating_mul(2).saturating_add(1024 * 1024);
    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)] // statvfs field widths differ across Unix targets.
    let available = {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "Invalid storage path")?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let stat = unsafe { stat.assume_init() };
        (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
    };
    #[cfg(windows)]
    let available = {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            // Declare the native Get Disk Free Space Ex W operation used by this adapter
            fn GetDiskFreeSpaceExW(
                path: *const u16,
                available: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }
        if unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        available
    };
    #[cfg(not(any(unix, windows)))]
    let available = {
        let _ = path;
        return Err("Compaction free-space checks are unavailable on this platform".into());
    };
    if available < required {
        return Err("Insufficient free disk space for database compaction".into());
    }
    Ok(())
}

/// Flush directory entries on platforms supporting directory fsync
pub fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// Replace the destination atomically using the platform file API
pub(crate) fn atomic_replace(from: &Path, to: &Path) -> Result<(), String> {
    replace_file(from, to)?;
    #[cfg(not(windows))]
    sync_directory(to.parent().ok_or("Replacement has no parent directory")?)?;
    Ok(())
}

/// OS replacement only. Callers own flushing files and syncing parent directories
#[cfg(not(windows))]
pub fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

#[cfg(windows)]
// Replace file
pub fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    #[link(name = "kernel32")]
    unsafe extern "system" {
        // Declare the native Move File Ex W operation used by this adapter
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

// Ensure directory
pub(crate) fn ensure_directory(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    // Verify replacement and failed source preserve destination
    fn replacement_and_failed_source_preserve_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();
        replace_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(replace_file(&source, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        std::fs::write(&source, b"durable").unwrap();
        atomic_replace(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"durable");
    }
}
