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

#[cfg(all(debug_assertions, target_os = "macos"))]
mod native_watch {
    use std::{
        fs,
        io::{self, ErrorKind},
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::Duration,
    };

    const POLL_INTERVAL: Duration = Duration::from_millis(500);

    // Watch only the private launcher's own generation directory
    fn private_directory(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !path.is_absolute() || !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "native dev watch requires a private directory",
            ));
        }
        Ok(())
    }

    fn consume_reload(directory: &Path) -> io::Result<bool> {
        let path = directory.join("reload");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() != 0 {
            return Ok(false);
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn listen(
        directory: &Path,
        mut on_reload: impl FnMut(),
        mut wait: impl FnMut() -> bool,
    ) -> io::Result<()> {
        private_directory(directory)?;
        fs::File::create_new(directory.join("ready"))?;
        // Check before waiting so a request placed during native startup is not lost
        while private_directory(directory).is_ok() {
            if consume_reload(directory)? {
                on_reload();
            }
            if !wait() {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn start(app: &tauri::AppHandle) {
        if !tauri::is_dev() {
            return;
        }
        let Some(directory) =
            std::env::var_os("STREAM_DOCK_MANAGER_NATIVE_WATCH_DIR").map(PathBuf::from)
        else {
            return;
        };
        let app = app.clone();
        let listener = std::thread::Builder::new()
            .name("dev-native-watch".into())
            .spawn(move || {
                if listen(
                    &directory,
                    || crate::editor_lifecycle::request_native_watch_restart(&app),
                    || {
                        std::thread::sleep(POLL_INTERVAL);
                        true
                    },
                )
                .is_err()
                {
                    eprintln!("The native development reload listener could not use its markers");
                }
            });
        if listener.is_err() {
            eprintln!("The native development reload listener could not start");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn creates_ready_and_consumes_one_reload_queued_during_boot() {
            let directory = tempfile::tempdir().unwrap();
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let reload = directory.path().join("reload");
            fs::File::create_new(&reload).unwrap();
            let mut observed = 0;
            let mut ticks = 0;
            listen(
                directory.path(),
                || {
                    assert!(directory.path().join("ready").is_file());
                    assert!(!reload.exists());
                    observed += 1;
                },
                || {
                    ticks += 1;
                    ticks < 3
                },
            )
            .unwrap();
            assert_eq!(observed, 1);
        }

        #[test]
        fn ignores_symlink_and_nonempty_reload_and_rejects_shared_directory() {
            let directory = tempfile::tempdir().unwrap();
            let reload = directory.path().join("reload");
            let target = directory.path().join("target");
            fs::File::create_new(&target).unwrap();
            std::os::unix::fs::symlink(&target, &reload).unwrap();
            assert!(!consume_reload(directory.path()).unwrap());
            assert!(target.is_file());
            fs::remove_file(&reload).unwrap();
            fs::write(&reload, "not a reload marker").unwrap();
            assert!(!consume_reload(directory.path()).unwrap());
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                listen(directory.path(), || panic!("reload"), || false)
                    .unwrap_err()
                    .kind(),
                ErrorKind::PermissionDenied
            );
            assert!(!directory.path().join("ready").exists());
        }
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
pub(crate) fn start_native_watch(app: &tauri::AppHandle) {
    native_watch::start(app);
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
