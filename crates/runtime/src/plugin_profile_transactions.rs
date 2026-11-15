//! Bounded file helpers for staged external plugin-profile artifacts
//! Managed records now commit together in SQLite; no multi-file settings journal
use std::path::Path;
// Read the requested plugin profile transactions data
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err("Expected a bounded regular import file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.len() > limit as u64 {
        return Err("Expected a bounded regular import file".into());
    }
    // Pre-open metadata is not a race defense. The stream bound applies to the
    // opened handle too, including growth after either metadata observation
    stream_dock_manager_storage::bounded_io::read_from(
        file,
        limit,
        "Import file exceeds its size limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that regular file boundary and wrong type
    fn regular_file_boundary_and_wrong_type() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input");
        std::fs::write(&path, b"1234").unwrap();
        assert_eq!(read(&path, 4).unwrap(), b"1234");
        assert!(read(&path, 3).is_err());
        assert!(read(root.path(), 4).is_err());
    }
    #[cfg(unix)]
    #[test]
    // Verify that import rejects symlink even when target is small
    fn import_rejects_symlink_even_when_target_is_small() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input");
        let link = root.path().join("link");
        std::fs::write(&path, b"ok").unwrap();
        std::os::unix::fs::symlink(path, &link).unwrap();
        assert!(read(&link, 4).is_err());
    }
}
