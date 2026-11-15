//! The development launcher owns Vite across native process restarts
use std::{io::Write, path::Path};

// Process the request through this subsystem
pub(crate) fn request() -> Result<bool, String> {
    request_at(
        tauri::is_dev(),
        std::env::var_os("STREAM_DOCK_MANAGER_DEV_RESTART_FILE")
            .as_deref()
            .map(Path::new),
    )
}

// Schedule the development restart at the supplied time
fn request_at(development: bool, path: Option<&Path>) -> Result<bool, String> {
    let Some(path) = path.filter(|_| development) else {
        return Ok(false);
    };
    let mut request = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    request
        .write_all(b"restart")
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify only launcher owned development requests use the handoff
    fn only_launcher_owned_development_requests_use_the_handoff() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("restart");
        assert!(!request_at(false, Some(&marker)).unwrap());
        assert!(!request_at(true, None).unwrap());
        assert!(!marker.exists());
        assert!(request_at(true, Some(&marker)).unwrap());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "restart");
        assert!(request_at(true, Some(&marker)).is_err());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "restart");
    }
}
