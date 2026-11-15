//! Private NSIS command mode. Never accepts a user-selected deletion target
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
};

#[cfg(any(windows, test))]
use std::path::PathBuf;

const MODE: &str = "--uninstall-default-data";
const PRESERVED: &str =
    "Files outside the default configuration folder and OS credential stores are not removed.";

/// Arguments exclude argv[0]. Run this before Tauri, logging, or storage startup
pub(crate) fn dispatch(args: impl IntoIterator<Item = OsString>) -> Option<i32> {
    let args: Vec<_> = args.into_iter().collect();
    if !args.iter().any(|arg| {
        arg == MODE
            || arg
                .as_encoded_bytes()
                .starts_with(b"--uninstall-default-data=")
    }) {
        return None;
    }
    // In particular, never combine this mode with --config or activation arguments
    if args.len() != 2 || args[0] != MODE || args[1].is_empty() {
        return Some(1);
    }
    Some(run_reported(Path::new(&args[1])))
}

// Run reported
fn run_reported(report_path: &Path) -> i32 {
    if !report_path.is_absolute() {
        return 1;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep the new report exclusively owned until its final contents are durable
        options.share_mode(0).custom_flags(0x0020_0000); // OPEN_REPARSE_POINT
    }
    let Ok(mut report) = options.open(report_path) else {
        return 1;
    };
    if write_report(
        &mut report,
        "Default configuration cleanup did not complete. Data may remain; check the default configuration folder.",
    )
    .is_err()
    {
        return 1;
    }
    let result = cleanup_default(report_path);
    let (code, message) = match result {
        Ok(message) => (0, message),
        Err(message) => (1, message),
    };
    if write_report(&mut report, &message).is_err() {
        return 1;
    }
    code
}

/// NSIS Unicode FileReadUTF16LE expects UTF-16LE, without a BOM. Keep the first
/// line within its string buffer, retaining unusually long details below it.
fn write_report(file: &mut File, message: &str) -> std::io::Result<()> {
    let message: String = message
        .chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect();
    let units: Vec<u16> = message.encode_utf16().collect();
    let text = if units.len() > 800 {
        let prefix: String = message
            .chars()
            .scan(0, |size, character| {
                *size += character.len_utf16();
                (*size <= 750).then_some(character)
            })
            .collect();
        format!("{prefix}... (full detail follows)\r\n{message}\r\n")
    } else {
        format!("{message}\r\n")
    };
    let bytes: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()
}

#[cfg(not(windows))]
// Remove only the reviewed default configuration data
fn cleanup_default(_report_path: &Path) -> Result<String, String> {
    Err(format!(
        "Default configuration cleanup is available only on Windows. No configuration data was removed. {PRESERVED}"
    ))
}

#[cfg(windows)]
// Remove only the reviewed default configuration data
fn cleanup_default(report_path: &Path) -> Result<String, String> {
    // ProjectDirs resolves Windows' RoamingAppData known folder, not an arbitrary
    // --config argument or an APPDATA environment override.
    let root = stream_dock_manager_runtime::Runtime::default_root().map_err(|error| {
        format!("Default configuration could not be resolved: {error}. No data was removed.")
    })?;
    let scope = cleanup::Scope::new(&root).map_err(|error| error.to_string())?;
    // A report must survive cleanup. Canonicalize its existing parent only, never
    // the deletion root (which would hide a junction or symlink).
    let report_parent = report_path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .ok_or_else(|| {
            "The cleanup report directory could not be resolved. No data was removed.".to_string()
        })?;
    let root_parent = root.parent().unwrap();
    if report_parent.starts_with(root_parent)
        || root_parent
            .canonicalize()
            .is_ok_and(|parent| report_parent.starts_with(parent))
    {
        return Err(
            "The cleanup report must be outside the Manager data folder. No data was removed."
                .into(),
        );
    }
    match cleanup::remove(scope) {
        Ok(cleanup::Outcome::Absent) => Ok(format!(
            "No default Manager/CLI configuration folder was found at {}. {PRESERVED}",
            root.display()
        )),
        Ok(cleanup::Outcome::Removed) => Ok(format!(
            "Default Manager/CLI configuration removed from {}. {PRESERVED}",
            root.display()
        )),
        Err(error) => Err(format!("{error} {PRESERVED}")),
    }
}

#[cfg(any(windows, test))]
mod cleanup {
    use super::*;
    use std::{fmt, fs, io};

    pub(super) struct Scope {
        root: PathBuf,
        parent: PathBuf,
    }

    impl Scope {
        // Construct the value and initialize its state
        pub(super) fn new(root: &Path) -> Result<Self, Failure> {
            let invalid = || Failure::new(root, "The default configuration path is not recognized");
            if !root.is_absolute()
                || root.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::CurDir | std::path::Component::ParentDir
                    )
                })
            {
                return Err(invalid());
            }
            #[cfg(windows)]
            for component in root.components() {
                use std::path::{Component, Prefix};
                match component {
                    Component::Prefix(prefix)
                        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
                    {
                        // Network/device namespaces are deliberately not deletion targets
                        return Err(invalid());
                    }
                    Component::Normal(name) => {
                        let Some(name) = name.to_str() else {
                            return Err(invalid());
                        };
                        if name.ends_with([' ', '.'])
                            || name
                                .chars()
                                .any(|c| c.is_control() || ":<>\"|?*".contains(c))
                        {
                            return Err(invalid());
                        }
                    }
                    _ => {}
                }
            }
            let parent = root.parent().ok_or_else(invalid)?;
            let shared = parent.parent().ok_or_else(invalid)?;
            if root.file_name() != Some("config".as_ref())
                || parent.file_name() != Some("Stream Dock Manager".as_ref())
                || shared.file_name() != Some("streamdockmanager".as_ref())
                || shared.parent().is_none()
            {
                return Err(invalid());
            }
            Ok(Self {
                root: root.into(),
                parent: parent.into(),
            })
        }
    }

    #[derive(Debug)]
    pub(super) struct Failure {
        path: PathBuf,
        reason: String,
        partial: bool,
    }

    impl Failure {
        // Construct the value and initialize its state
        fn new(path: &Path, reason: impl ToString) -> Self {
            Self {
                path: path.into(),
                reason: reason.to_string(),
                partial: false,
            }
        }
    }

    impl fmt::Display for Failure {
        // Format the value for display
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let status = if self.partial {
                "Default configuration cleanup did not finish; some data may have been removed"
            } else {
                "Default configuration was not removed"
            };
            write!(
                formatter,
                "{status}. {}: {}.",
                self.path.display(),
                self.reason
            )
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Outcome {
        Absent,
        Removed,
    }

    struct Pin {
        path: PathBuf,
        file: File,
    }

    // Check whether the value is link
    fn is_link(metadata: &fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x0400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
        }
        #[cfg(not(windows))]
        {
            metadata.file_type().is_symlink()
        }
    }

    // Inspect file metadata before allowing removal
    fn checked_metadata(path: &Path) -> Result<fs::Metadata, Failure> {
        let metadata = fs::symlink_metadata(path).map_err(|error| Failure::new(path, error))?;
        if is_link(&metadata) {
            return Err(Failure::new(
                path,
                "Symlinks and reparse points are not removed",
            ));
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(Failure::new(
                path,
                "The entry is not a regular file or directory",
            ));
        }
        Ok(metadata)
    }

    // Pin
    fn pin(path: &Path) -> Result<Pin, Failure> {
        if !checked_metadata(path)?.is_dir() {
            return Err(Failure::new(path, "Expected a directory"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // No DELETE sharing pins the directory's name. No WRITE sharing also
            // prevents in-place junction/reparse tagging while the handle is held.
            options
                .share_mode(0x0000_0001) // FILE_SHARE_READ
                .custom_flags(0x0200_0000 | 0x0020_0000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        }
        let file = options
            .open(path)
            .map_err(|error| Failure::new(path, error))?;
        let metadata = file.metadata().map_err(|error| Failure::new(path, error))?;
        if is_link(&metadata) || !metadata.is_dir() {
            return Err(Failure::new(
                path,
                "Directory changed during cleanup preflight",
            ));
        }
        Ok(Pin {
            path: path.into(),
            file,
        })
    }

    // Check pin
    fn check_pin(pin: &Pin) -> Result<(), Failure> {
        let metadata = pin
            .file
            .metadata()
            .map_err(|error| Failure::new(&pin.path, error))?;
        if is_link(&metadata) || !metadata.is_dir() || !checked_metadata(&pin.path)?.is_dir() {
            return Err(Failure::new(&pin.path, "Directory changed during cleanup"));
        }
        Ok(())
    }

    /// Pin top-down, so none of a later open's ancestors can be renamed or retagged
    fn ancestry(root: &Path) -> Result<Option<Vec<Pin>>, Failure> {
        let paths: Vec<_> = root.ancestors().collect();
        let mut pins = Vec::new();
        for path in paths
            .into_iter()
            .rev()
            .filter(|path| !path.as_os_str().is_empty())
        {
            match fs::symlink_metadata(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(Failure::new(path, error)),
                Ok(_) => pins.push(pin(path)?),
            }
        }
        Ok(Some(pins))
    }

    struct Tree {
        directories: Vec<Pin>,
        files: Vec<PathBuf>,
    }

    // Inventory the reviewed configuration tree before removal
    fn snapshot(root: &Path) -> Result<Tree, Failure> {
        let mut tree = Tree {
            directories: Vec::new(),
            files: Vec::new(),
        };
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            for entry in
                fs::read_dir(&directory).map_err(|error| Failure::new(&directory, error))?
            {
                let path = entry
                    .map_err(|error| Failure::new(&directory, error))?
                    .path();
                let metadata = checked_metadata(&path)?;
                if metadata.is_dir() {
                    tree.directories.push(pin(&path)?);
                    pending.push(path);
                } else {
                    tree.files.push(path);
                }
            }
        }
        Ok(tree)
    }

    // Acquire exclusive ownership before removing configuration data
    fn own_writer(root: &Path) -> Result<(File, Pin, PathBuf), Failure> {
        let profiles = root.join("profiles");
        match fs::create_dir(&profiles) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Failure::new(&profiles, error)),
        }
        let profile_pin = pin(&profiles)?;
        let path = profiles.join("writer.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Existing Manager/CLI ownership opens request read/write access, so
            // even an owner that has not yet acquired its lock blocks this open.
            options
                .share_mode(0x0000_0004) // FILE_SHARE_DELETE only
                .custom_flags(0x0020_0000); // OPEN_REPARSE_POINT
        }
        let file = match options.create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !checked_metadata(&path)?.is_file() {
                    return Err(Failure::new(
                        &path,
                        "The ownership lock is not a regular file",
                    ));
                }
                options.create_new(false).open(&path).map_err(|error| {
                    Failure::new(
                        &path,
                        format!("Quit Manager and the standalone CLI, then retry ({error})"),
                    )
                })?
            }
            Err(error) => return Err(Failure::new(&path, error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| Failure::new(&path, error))?;
        if is_link(&metadata) || !metadata.is_file() {
            return Err(Failure::new(
                &path,
                "The ownership lock changed during preflight",
            ));
        }
        file.try_lock().map_err(|error| {
            Failure::new(
                &path,
                format!("Quit Manager and the standalone CLI, then retry ({error})"),
            )
        })?;
        Ok((file, profile_pin, path))
    }

    // Check whether the path component matches the reviewed target
    fn component_matches(name: &std::ffi::OsStr, expected: &str) -> bool {
        #[cfg(windows)]
        {
            name.to_string_lossy().eq_ignore_ascii_case(expected)
        }
        #[cfg(not(windows))]
        {
            name == expected
        }
    }

    // Check whether the profile path still matches the reviewed location
    fn same_profiles_path(root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let parts: Vec<_> = relative.components().collect();
        parts.len() == 1 && component_matches(parts[0].as_os_str(), "profiles")
    }

    // Check whether the writer lock path is unchanged
    fn same_lock_path(root: &Path, path: &Path) -> bool {
        path.parent()
            .is_some_and(|parent| same_profiles_path(root, parent))
            && path
                .file_name()
                .is_some_and(|name| component_matches(name, "writer.lock"))
    }

    // Check ancestry
    fn check_ancestry(path: &Path) -> Result<(), Failure> {
        for ancestor in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            if !checked_metadata(ancestor)?.is_dir() {
                return Err(Failure::new(ancestor, "A parent is no longer a directory"));
            }
        }
        Ok(())
    }

    // Remove for the current operation
    pub(super) fn remove(scope: Scope) -> Result<Outcome, Failure> {
        remove_with_hook(scope, || {})
    }

    // Remove with hook
    fn remove_with_hook(scope: Scope, before_delete: impl FnOnce()) -> Result<Outcome, Failure> {
        let Some(mut ancestors) = ancestry(&scope.root)? else {
            return Ok(Outcome::Absent);
        };
        // Preflight the entire tree before creating an absent ownership lock
        let preflight = snapshot(&scope.root)?;
        let (owner, profile_pin, lock_path) = own_writer(&scope.root)?;
        let mut tree = snapshot(&scope.root)?;
        drop(preflight);
        drop(profile_pin); // The final snapshot now pins this directory too.
        for pin in ancestors.iter().chain(&tree.directories) {
            check_pin(pin)?;
        }
        before_delete();
        let mut deleted = false;
        let removal = (|| {
            for pin in ancestors.iter().chain(&tree.directories) {
                check_pin(pin)?;
            }
            for path in &tree.files {
                if same_lock_path(&scope.root, path) {
                    continue;
                }
                check_ancestry(path)?;
                if !checked_metadata(path)?.is_file() {
                    return Err(Failure::new(path, "File changed during cleanup"));
                }
                fs::remove_file(path).map_err(|error| Failure::new(path, error))?;
                deleted = true;
            }
            // Snapshot deletion never recursively follows new entries. A changed or
            // newly populated directory is preserved and produces a failure.
            tree.directories
                .sort_by_key(|pin| std::cmp::Reverse(pin.path.components().count()));
            let mut profiles_pin = None;
            for directory in tree.directories {
                if same_profiles_path(&scope.root, &directory.path) {
                    profiles_pin = Some(directory);
                    continue;
                }
                check_pin(&directory)?;
                check_ancestry(&directory.path)?;
                let path = directory.path.clone();
                drop(directory);
                fs::remove_dir(&path).map_err(|error| Failure::new(&path, error))?;
                deleted = true;
            }
            // Keep the writer locked until every other data entry has been removed
            // Windows may defer this unlink until owner is closed. After that point,
            // use ONLY nonrecursive removals; a new writer's files cannot be erased.
            check_ancestry(&lock_path)?;
            if !checked_metadata(&lock_path)?.is_file() {
                return Err(Failure::new(
                    &lock_path,
                    "The ownership lock changed during cleanup",
                ));
            }
            fs::remove_file(&lock_path).map_err(|error| Failure::new(&lock_path, error))?;
            deleted = true;
            drop(owner);
            drop(profiles_pin);
            let profiles = lock_path.parent().unwrap();
            fs::remove_dir(profiles).map_err(|error| Failure::new(profiles, error))?;
            let root_pin = ancestors.pop().unwrap();
            check_pin(&root_pin)?;
            drop(root_pin);
            fs::remove_dir(&scope.root).map_err(|error| Failure::new(&scope.root, error))?;
            let parent_pin = ancestors.pop().unwrap();
            check_pin(&parent_pin)?;
            drop(parent_pin);
            match fs::remove_dir(&scope.parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(Failure::new(&scope.parent, error)),
            }
            Ok(Outcome::Removed)
        })();
        removal.map_err(|mut error| {
            error.partial = deleted;
            error
        })
    }

    #[cfg(test)]
    // Remove after preflight
    pub(super) fn remove_after_preflight(
        scope: Scope,
        hook: impl FnOnce(),
    ) -> Result<Outcome, Failure> {
        remove_with_hook(scope, hook)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        // Construct the value and initialize its state
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = std::env::temp_dir().canonicalize().unwrap();
            let directory = base.join(format!(
                "sdm-uninstall-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        // Read the root directory owned by this store
        fn root(&self) -> PathBuf {
            self.0.join("streamdockmanager/Stream Dock Manager/config")
        }
        // Create the configuration data used by the removal test
        fn populate(&self) -> PathBuf {
            let root = self.root();
            fs::create_dir_all(root.join("profiles/assets")).unwrap();
            fs::write(root.join("profiles/writer.lock"), b"").unwrap();
            fs::write(root.join("profiles/store.sqlite3"), b"fixture").unwrap();
            fs::write(root.join("profiles/assets/icon.png"), b"fixture").unwrap();
            fs::write(root.join("settings.json"), b"fixture").unwrap();
            root
        }
        // Remove for the current operation
        fn remove(&self) -> Result<cleanup::Outcome, cleanup::Failure> {
            cleanup::remove(cleanup::Scope::new(&self.root()).unwrap())
        }
    }
    impl Drop for Fixture {
        // Release resources held by this owner
        fn drop(&mut self) {
            // Tests own this entire disposable tree; never remove a user root
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    // Decode report
    fn decode_report(path: &Path) -> String {
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len() % 2, 0);
        String::from_utf16(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    // Verify ordinary arguments do not enter cleanup
    fn ordinary_arguments_do_not_enter_cleanup() {
        for args in [
            vec![],
            vec!["--config", "custom"],
            vec!["--autostart"],
            vec!["--", "custom"],
        ] {
            assert_eq!(dispatch(args.into_iter().map(OsString::from)), None);
        }
    }

    #[test]
    // Verify malformed cleanup never falls through to normal startup
    fn malformed_cleanup_never_falls_through_to_normal_startup() {
        for args in [
            vec![MODE],
            vec![MODE, ""],
            vec![MODE, "relative-report"],
            vec![MODE, "report", "--config", "custom"],
            vec!["--config", "custom", MODE, "report"],
            vec!["--uninstall-default-data=report"],
            vec!["--", MODE, "report"],
        ] {
            assert_eq!(dispatch(args.into_iter().map(OsString::from)), Some(1));
        }
    }

    #[test]
    // Verify report is create new and cannot clobber existing data
    fn report_is_create_new_and_cannot_clobber_existing_data() {
        let fixture = Fixture::new();
        let report = fixture.0.join("existing.txt");
        fs::write(&report, b"keep").unwrap();
        assert_eq!(dispatch([MODE.into(), report.into_os_string()]), Some(1));
        assert_eq!(fs::read(fixture.0.join("existing.txt")).unwrap(), b"keep");
    }

    #[test]
    // Verify report encoding and long path detail are preserved
    fn report_encoding_and_long_path_detail_are_preserved() {
        let fixture = Fixture::new();
        let path = fixture.0.join("report.txt");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let detail = format!(
            "Unable to remove C:\\{}\\config: access denied",
            "界".repeat(900)
        );
        write_report(&mut file, &detail).unwrap();
        let text = decode_report(&path);
        assert!(text.lines().next().unwrap().encode_utf16().count() <= 800);
        assert!(text.contains(&detail));
        write_report(&mut file, "Short report").unwrap();
        assert_eq!(decode_report(&path), "Short report\r\n");
    }

    #[cfg(not(windows))]
    #[test]
    // Verify non windows command fails closed
    fn non_windows_command_fails_closed() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let report = fixture.0.join("result.txt");
        assert_eq!(
            dispatch([MODE.into(), report.clone().into_os_string()]),
            Some(1)
        );
        assert!(root.join("profiles/store.sqlite3").is_file());
        assert!(decode_report(&report).contains("only on Windows"));
    }

    #[test]
    // Verify deletes only default config and empty application parent
    fn deletes_only_default_config_and_empty_application_parent() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let shared_sibling = fixture.0.join("streamdockmanager/another-product");
        let custom = fixture.0.join("custom-config");
        fs::create_dir_all(&shared_sibling).unwrap();
        fs::create_dir_all(&custom).unwrap();
        fs::write(shared_sibling.join("keep"), b"shared").unwrap();
        fs::write(custom.join("keep"), b"custom").unwrap();
        assert_eq!(fixture.remove().unwrap(), cleanup::Outcome::Removed);
        assert!(!root.exists());
        assert!(!root.parent().unwrap().exists());
        assert!(fixture.0.join("streamdockmanager").is_dir());
        assert_eq!(fs::read(shared_sibling.join("keep")).unwrap(), b"shared");
        assert_eq!(fs::read(custom.join("keep")).unwrap(), b"custom");
    }

    #[test]
    // Verify preserves nonempty application parent
    fn preserves_nonempty_application_parent() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let sibling = root.parent().unwrap().join("other-data");
        fs::write(&sibling, b"keep").unwrap();
        assert_eq!(fixture.remove().unwrap(), cleanup::Outcome::Removed);
        assert_eq!(fs::read(sibling).unwrap(), b"keep");
    }

    #[test]
    // Verify missing root is success without creating directories
    fn missing_root_is_success_without_creating_directories() {
        let fixture = Fixture::new();
        assert_eq!(fixture.remove().unwrap(), cleanup::Outcome::Absent);
        assert!(!fixture.0.join("streamdockmanager").exists());
    }

    #[test]
    // Verify empty root without profiles is removed
    fn empty_root_without_profiles_is_removed() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.root()).unwrap();
        assert_eq!(fixture.remove().unwrap(), cleanup::Outcome::Removed);
    }

    #[test]
    // Verify custom or malformed scope is rejected
    fn custom_or_malformed_scope_is_rejected() {
        let fixture = Fixture::new();
        for path in [
            fixture.0.clone(),
            fixture.0.join("custom"),
            fixture.root().join("../config"),
            PathBuf::from("streamdockmanager/Stream Dock Manager/config"),
        ] {
            assert!(cleanup::Scope::new(&path).is_err(), "{}", path.display());
        }
    }

    #[test]
    // Verify ownership lock contention preserves every data file
    fn ownership_lock_contention_preserves_every_data_file() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("profiles/writer.lock"))
            .unwrap();
        owner.try_lock().unwrap();
        let error = fixture.remove().unwrap_err().to_string();
        assert!(error.contains("not removed"));
        assert!(error.contains("writer.lock"));
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), b"fixture");
        assert_eq!(
            fs::read(root.join("profiles/store.sqlite3")).unwrap(),
            b"fixture"
        );
    }

    #[test]
    // Verify malformed profile directory fails before deletion
    fn malformed_profile_directory_fails_before_deletion() {
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("profiles"), b"not a directory").unwrap();
        fs::write(root.join("keep"), b"keep").unwrap();
        assert!(fixture.remove().is_err());
        assert_eq!(fs::read(root.join("keep")).unwrap(), b"keep");
    }

    #[test]
    // Verify new entries are not recursively removed and failure reports partial work
    fn new_entries_are_not_recursively_removed_and_failure_reports_partial_work() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let new_entry = root.join("profiles/assets/arrived-late");
        let error = cleanup::remove_after_preflight(cleanup::Scope::new(&root).unwrap(), || {
            fs::write(&new_entry, b"keep").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("some data may have been removed"));
        assert!(error.contains("assets"));
        assert_eq!(fs::read(new_entry).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    // Verify symlink root ancestry and tree are rejected before deletion
    fn symlink_root_ancestry_and_tree_are_rejected_before_deletion() {
        use std::os::unix::fs::symlink;
        for position in ["root", "ancestor", "tree"] {
            let fixture = Fixture::new();
            let root = fixture.populate();
            let outside = fixture.0.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep"), b"outside").unwrap();
            match position {
                "root" => {
                    fs::rename(&root, fixture.0.join("original")).unwrap();
                    symlink(&outside, &root).unwrap();
                }
                "ancestor" => {
                    let shared = fixture.0.join("streamdockmanager");
                    fs::rename(&shared, fixture.0.join("original")).unwrap();
                    symlink(fixture.0.join("original"), &shared).unwrap();
                }
                _ => symlink(&outside, root.join("linked-directory")).unwrap(),
            }
            assert!(fixture
                .remove()
                .unwrap_err()
                .to_string()
                .contains("not removed"));
            assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside");
            if position == "tree" {
                assert!(root.join("profiles/store.sqlite3").is_file());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    // Verify reparse like tree change is detected after preflight
    fn reparse_like_tree_change_is_detected_after_preflight() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let root = fixture.populate();
        let target = root.join("profiles/assets");
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"outside").unwrap();
        let error = cleanup::remove_after_preflight(cleanup::Scope::new(&root).unwrap(), || {
            fs::rename(&target, fixture.0.join("original-assets")).unwrap();
            symlink(&outside, &target).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("not removed"));
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside");
        assert!(root.join("profiles/store.sqlite3").is_file());
    }

    #[test]
    // Verify malformed lock and non directory root preserve data
    fn malformed_lock_and_non_directory_root_preserve_data() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let lock = root.join("profiles/writer.lock");
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        assert!(fixture
            .remove()
            .unwrap_err()
            .to_string()
            .contains("not removed"));
        assert!(root.join("settings.json").is_file());

        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(&root, b"keep malformed root").unwrap();
        assert!(fixture.remove().is_err());
        assert_eq!(fs::read(root).unwrap(), b"keep malformed root");
    }

    #[cfg(unix)]
    #[test]
    // Verify unreadable subtree fails before any file is deleted
    fn unreadable_subtree_fails_before_any_file_is_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let root = fixture.populate();
        let blocked = root.join("profiles/assets");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        // Privileged Unix test runners can bypass permissions; that is not evidence
        // of inaccessible-directory behavior. Windows has separate native tests.
        if fs::read_dir(&blocked).is_ok() {
            fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let result = fixture.remove();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.contains("not removed"));
        assert!(error.contains(&blocked.to_string_lossy().to_string()));
        assert!(root.join("profiles/store.sqlite3").is_file());
        assert!(root.join("settings.json").is_file());
    }

    #[cfg(windows)]
    #[test]
    // Verify windows case insensitive profiles and lock are supported
    fn windows_case_insensitive_profiles_and_lock_are_supported() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        fs::rename(
            root.join("profiles/writer.lock"),
            root.join("profiles/WRITER.LOCK"),
        )
        .unwrap();
        fs::rename(root.join("profiles"), root.join("PROFILES")).unwrap();
        assert_eq!(fixture.remove().unwrap(), cleanup::Outcome::Removed);
    }

    #[cfg(windows)]
    #[test]
    // Verify windows directory write handle blocks preflight
    fn windows_directory_write_handle_blocks_preflight() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let root = fixture.populate();
        let _writer = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x0200_0000)
            .open(root.join("profiles/assets"))
            .unwrap();
        assert!(fixture
            .remove()
            .unwrap_err()
            .to_string()
            .contains("not removed"));
        assert!(root.join("settings.json").is_file());
    }

    #[cfg(windows)]
    #[test]
    // Verify windows cleanup lock blocks new writers
    fn windows_cleanup_lock_blocks_new_writers() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let result = cleanup::remove_after_preflight(cleanup::Scope::new(&root).unwrap(), || {
            assert!(OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("profiles/writer.lock"))
                .is_err());
        });
        assert_eq!(result.unwrap(), cleanup::Outcome::Removed);
    }

    #[cfg(windows)]
    #[test]
    // Verify windows undeletable file reports its path and preserves it
    fn windows_undeletable_file_reports_its_path_and_preserves_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let root = fixture.populate();
        let blocked = root.join("profiles/store.sqlite3");
        let _reader = OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&blocked)
            .unwrap();
        let error = fixture.remove().unwrap_err().to_string();
        assert!(error.contains(&blocked.to_string_lossy().to_string()));
        assert_eq!(fs::read(&blocked).unwrap(), b"fixture");
        assert!(root.join("profiles/writer.lock").exists());
    }

    #[cfg(windows)]
    #[test]
    // Verify windows open writer without lock also blocks cleanup
    fn windows_open_writer_without_lock_also_blocks_cleanup() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let _owner = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("profiles/writer.lock"))
            .unwrap();
        assert!(fixture.remove().is_err());
        assert!(root.join("profiles/store.sqlite3").exists());
    }

    #[cfg(windows)]
    #[test]
    // Verify windows directory pins prevent root rename
    fn windows_directory_pins_prevent_root_rename() {
        let fixture = Fixture::new();
        let root = fixture.populate();
        let result = cleanup::remove_after_preflight(cleanup::Scope::new(&root).unwrap(), || {
            assert!(fs::rename(&root, fixture.0.join("redirected")).is_err());
            assert!(fs::rename(
                root.join("profiles/assets"),
                fixture.0.join("redirected-assets")
            )
            .is_err());
        });
        assert_eq!(result.unwrap(), cleanup::Outcome::Removed);
    }

    #[cfg(windows)]
    #[test]
    // Verify windows junction root ancestry and tree preserve targets
    fn windows_junction_root_ancestry_and_tree_preserve_targets() {
        for position in ["root", "ancestor", "tree"] {
            let fixture = Fixture::new();
            let root = fixture.populate();
            let outside = fixture.0.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep"), b"outside").unwrap();
            let (junction, destination) = match position {
                "root" => {
                    fs::rename(&root, fixture.0.join("original")).unwrap();
                    (root.clone(), outside.clone())
                }
                "ancestor" => {
                    let shared = fixture.0.join("streamdockmanager");
                    let original = fixture.0.join("original");
                    fs::rename(&shared, &original).unwrap();
                    (shared, original)
                }
                _ => (root.join("linked-directory"), outside.clone()),
            };
            let status = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&junction)
                .arg(&destination)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "Could not create disposable test junction"
            );
            assert!(fixture
                .remove()
                .unwrap_err()
                .to_string()
                .contains("not removed"));
            assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside");
            fs::remove_dir(junction).unwrap();
        }
    }
}
