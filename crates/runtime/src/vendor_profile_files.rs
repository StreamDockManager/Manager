//! Confined, non-extracting reads of declared bundled profile archives
//! Every path component is opened relative to an already pinned directory
use std::path::Path;

// Read the requested vendor profile files data
pub(crate) fn read(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        let root = unix::directory(root)?;
        unix::read_from_directory(&root, relative, limit)
    }
    #[cfg(windows)]
    {
        let root = windows::directory(root)?;
        windows::read_from_directory(&root, relative, limit)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (root, relative, limit);
        Err("Secure bundled profile archive reading is unavailable on this platform".into())
    }
}

#[cfg(windows)]
mod windows;

#[cfg(unix)]
mod unix {
    use rustix::{
        fd::{AsFd, OwnedFd},
        fs::{open, openat, Mode, OFlags},
    };
    use std::{
        ffi::OsStr,
        fs::File,
        path::{Component, Path},
    };

    // Open directories without following links, blocking or leaking descriptors to child processes
    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    /// The installed package root is already absolute and canonical. Do not
    /// canonicalize here: doing so could follow an attacker-swapped symlink
    pub(super) fn directory(path: &Path) -> Result<OwnedFd, String> {
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err("Bundled profile package root must be absolute".into());
        }
        let mut current = open("/", directory_flags(), Mode::empty())
            .map_err(|_| "Could not open bundled profile package root")?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err("Invalid bundled profile package root".into());
            };
            current = openat(&current, name, directory_flags(), Mode::empty()).map_err(|_| {
                "Bundled profile package path must contain only directories, without symlinks"
            })?;
        }
        Ok(current)
    }

    // Read from directory
    pub(super) fn read_from_directory(
        root: &OwnedFd,
        relative: &Path,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        let parts: Vec<&OsStr> = relative
            .components()
            .map(|part| match part {
                Component::Normal(name) => Ok(name),
                _ => Err("Bundled profile path must stay inside its package"),
            })
            .collect::<Result<_, _>>()?;
        let (name, parents) = parts.split_last().ok_or("Empty bundled profile path")?;
        let mut parent = None;
        for name in parents {
            let current = parent.as_ref().map_or_else(|| root.as_fd(), OwnedFd::as_fd);
            parent = Some(
                openat(current, *name, directory_flags(), Mode::empty())
                    .map_err(|_| "Bundled profile parent must be a directory without symlinks")?,
            );
        }
        let current = parent.as_ref().map_or_else(|| root.as_fd(), OwnedFd::as_fd);
        let descriptor = openat(
            current,
            *name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "Could not open a regular bundled profile archive without symlinks")?;
        let file = File::from(descriptor);
        let metadata = file
            .metadata()
            .map_err(|_| "Could not inspect bundled profile archive")?;
        if !metadata.is_file() || metadata.len() > limit as u64 {
            return Err("Expected a bounded regular bundled profile archive".into());
        }
        stream_dock_manager_storage::bounded_io::read_from(
            file,
            limit,
            "Bundled profile archive exceeds its size limit",
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    // Verify that reads only bounded regular package files
    fn reads_only_bounded_regular_package_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("profiles")).unwrap();
        fs::write(root.join("profiles/test.streamDeckProfile"), b"profile").unwrap();
        assert_eq!(
            read(&root, Path::new("profiles/test.streamDeckProfile"), 7).unwrap(),
            b"profile"
        );
        assert!(read(&root, Path::new("profiles/test.streamDeckProfile"), 6).is_err());
        for path in ["", ".", "../outside", "/outside", "profiles"] {
            assert!(read(&root, Path::new(path), 1024).is_err(), "{path}");
        }
    }

    #[test]
    // Verify that rejects leaf and parent symlinks without reading the target
    fn rejects_leaf_and_parent_symlinks_without_reading_the_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        let outside = root.join("outside");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("test.streamDeckProfile"), b"outside").unwrap();
        symlink(outside.join("test.streamDeckProfile"), package.join("leaf")).unwrap();
        symlink(&outside, package.join("parent")).unwrap();
        assert!(read(&package, Path::new("leaf"), 1024).is_err());
        assert!(read(&package, Path::new("parent/test.streamDeckProfile"), 1024).is_err());
        assert!(read(
            &package.join("parent"),
            Path::new("test.streamDeckProfile"),
            1024
        )
        .is_err());
    }

    #[test]
    // Verify that pinned root cannot be redirected by directory replacement
    fn pinned_root_cannot_be_redirected_by_directory_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        let outside = root.join("outside");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(package.join("test.streamDeckProfile"), b"inside").unwrap();
        fs::write(outside.join("test.streamDeckProfile"), b"outside").unwrap();
        let pinned = unix::directory(&package).unwrap();
        fs::rename(&package, root.join("old-package")).unwrap();
        symlink(&outside, &package).unwrap();
        assert_eq!(
            unix::read_from_directory(&pinned, Path::new("test.streamDeckProfile"), 1024).unwrap(),
            b"inside"
        );
        assert!(read(&package, Path::new("test.streamDeckProfile"), 1024).is_err());
    }

    #[test]
    // Verify that replacement symlinks are rejected even after root admission
    fn replacement_symlinks_are_rejected_even_after_root_admission() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        let outside = root.join("outside");
        fs::create_dir_all(package.join("profiles")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(package.join("profiles/test.streamDeckProfile"), b"inside").unwrap();
        fs::write(outside.join("test.streamDeckProfile"), b"outside").unwrap();
        let pinned = unix::directory(&package).unwrap();
        fs::remove_file(package.join("profiles/test.streamDeckProfile")).unwrap();
        symlink(
            outside.join("test.streamDeckProfile"),
            package.join("profiles/test.streamDeckProfile"),
        )
        .unwrap();
        assert!(unix::read_from_directory(
            &pinned,
            Path::new("profiles/test.streamDeckProfile"),
            1024
        )
        .is_err());
        fs::rename(package.join("profiles"), package.join("old-profiles")).unwrap();
        symlink(&outside, package.join("profiles")).unwrap();
        assert!(unix::read_from_directory(
            &pinned,
            Path::new("profiles/test.streamDeckProfile"),
            1024
        )
        .is_err());
    }

    #[test]
    // Verify that rejects special files without waiting for a fifo writer
    fn rejects_special_files_without_waiting_for_a_fifo_writer() {
        use rustix::fs::Mode;
        use std::{os::unix::net::UnixListener, sync::mpsc, time::Duration};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        // macOS has no mkfifoat; the POSIX utility creates only this fixture path
        assert!(std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(root.join("pipe"))
            .status()
            .unwrap()
            .success());
        let _listener = UnixListener::bind(root.join("socket")).unwrap();
        assert!(read(&root, Path::new("socket"), 1024).is_err());
        let (send, receive) = mpsc::channel();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            let _ = send.send(read(&worker_root, Path::new("pipe"), 1024));
        });
        let result = receive.recv_timeout(Duration::from_secs(2));
        if result.is_err() {
            // Unblock a regressed blocking reader before failing the test
            let _writer = rustix::fs::open(
                root.join("pipe"),
                rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                Mode::empty(),
            );
        }
        assert!(result
            .expect("FIFO rejection must not wait for a writer")
            .is_err());
        worker.join().unwrap();
    }

    #[test]
    // Verify that rejects relative roots parent traversal and symlinked ancestors
    fn rejects_relative_roots_parent_traversal_and_symlinked_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("parent/package/profiles")).unwrap();
        fs::write(root.join("parent/package/profiles/archive"), b"inside").unwrap();
        let package = root.join("parent/package");
        assert!(read(Path::new("parent/package"), Path::new("archive"), 1024).is_err());
        assert!(read(
            &package.join(".."),
            Path::new("package/profiles/archive"),
            1024
        )
        .is_err());
        assert!(read(&package, Path::new("profiles/../profiles/archive"), 1024).is_err());
        symlink(root.join("parent"), root.join("alias")).unwrap();
        assert!(read(
            &root.join("alias/package"),
            Path::new("profiles/archive"),
            1024
        )
        .is_err());
    }

    #[test]
    // Verify that handles non UTF-8 names without accepting nul paths
    fn handles_non_utf8_names_without_accepting_nul_paths() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let name = OsStr::from_bytes(b"profile-\xff.streamDeckProfile");
        // Verify the fixture is writable before exercising a non-UTF-8 name
        fs::write(root.join("ordinary"), b"inside").unwrap();
        let created = fs::write(root.join(name), b"inside");
        #[cfg(target_os = "macos")]
        match created {
            Ok(()) => assert_eq!(read(&root, Path::new(name), 1024).unwrap(), b"inside"),
            // macOS filesystem/environment combinations may reject invalid UTF-8
            // names at creation. Such a name must still fail closed in the reader;
            // this branch does not claim that an unreadable file was round-tripped
            Err(_) => assert!(read(&root, Path::new(name), 1024).is_err()),
        }
        #[cfg(not(target_os = "macos"))]
        {
            created.unwrap();
            assert_eq!(read(&root, Path::new(name), 1024).unwrap(), b"inside");
        }
        assert!(read(
            &root,
            Path::new(OsStr::from_bytes(b"profile\0ignored")),
            1024
        )
        .is_err());
    }
}
