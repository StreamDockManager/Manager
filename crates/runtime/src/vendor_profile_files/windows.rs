//! Windows confinement uses native, handle-relative opens. DOS drive mappings
//! are resolved once to a local volume device, never followed as package paths
//! UNC, network and substituted drives are deliberately unsupported
use std::{
    ffi::OsStr,
    fs::File,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, Prefix},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtOpenFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, UNICODE_STRING},
        Storage::FileSystem::{
            FileAttributeTagInfo, GetFileInformationByHandleEx, GetFileType, QueryDosDeviceW,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
            FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_TYPE_DISK, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

#[derive(Clone, Copy)]
enum Kind {
    Directory,
    Archive,
}

impl Kind {
    // Check access to the Windows resource
    fn access(self) -> u32 {
        FILE_READ_ATTRIBUTES.0
            | SYNCHRONIZE.0
            | match self {
                Self::Directory => FILE_TRAVERSE.0,
                Self::Archive => FILE_READ_DATA.0,
            }
    }

    // Select synchronous no-reparse-point open flags for the expected Windows file kind
    fn options(self) -> u32 {
        FILE_OPEN_REPARSE_POINT.0
            | FILE_SYNCHRONOUS_IO_NONALERT.0
            | match self {
                Self::Directory => FILE_DIRECTORY_FILE.0,
                Self::Archive => FILE_NON_DIRECTORY_FILE.0,
            }
    }
}

// Validate one Windows path component and reject traversal, separators, streams and NUL characters
fn checked_name(name: &OsStr) -> Result<Vec<u16>, String> {
    let wide: Vec<_> = name.encode_wide().collect();
    if wide.is_empty()
        || wide == [b'.' as u16]
        || wide == [b'.' as u16, b'.' as u16]
        || wide.iter().any(|&ch| matches!(ch, 0 | 47 | 58 | 92))
    {
        return Err("Bundled profile paths require ordinary names without streams".into());
    }
    unicode_byte_length(&wide)?;
    Ok(wide)
}

// Compute the encoded byte length of Unicode text
fn unicode_byte_length(wide: &[u16]) -> Result<u16, String> {
    wide.len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| "Bundled profile path component is too long".into())
}

// Require every relative path component to be an ordinary validated filename
fn normal_parts<'a>(parts: impl Iterator<Item = Component<'a>>) -> Result<Vec<Vec<u16>>, String> {
    parts
        .map(|part| match part {
            Component::Normal(name) => checked_name(name),
            _ => Err("Bundled profile path must stay inside its package".into()),
        })
        .collect()
}

// Split an absolute local-drive package path into its drive letter and safe components
fn root_parts(path: &Path) -> Result<(u8, Vec<Vec<u16>>), String> {
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => {
                drive
            }
            _ => return Err("Bundled profiles require a local disk package root".into()),
        },
        _ => return Err("Bundled profile package root must be absolute".into()),
    };
    if parts.next() != Some(Component::RootDir) {
        return Err("Bundled profile package root must be absolute".into());
    }
    Ok((drive, normal_parts(parts)?))
}

// Accept only a numbered local hard-disk volume target and append its root separator
fn volume_root(target: &[u16]) -> Result<Vec<u16>, String> {
    let prefix: Vec<_> = r"\Device\HarddiskVolume".encode_utf16().collect();
    let suffix = target
        .strip_prefix(prefix.as_slice())
        .filter(|suffix| !suffix.is_empty())
        .ok_or("Bundled profiles require a local volume, without drive aliases")?;
    if !suffix
        .iter()
        .all(|&ch| (b'0' as u16..=b'9' as u16).contains(&ch))
    {
        return Err("Bundled profiles require a local volume, without drive aliases".into());
    }
    let mut root = target.to_vec();
    root.push(b'\\' as u16);
    unicode_byte_length(&root)?;
    Ok(root)
}

// Resolve the current DOS drive mapping to a validated native local-volume root
fn drive_root(drive: u8) -> Result<Vec<u16>, String> {
    let drive_name = [u16::from(drive), b':' as u16, 0];
    let mut target = vec![0u16; 32768];
    // The input is NUL terminated and both buffers remain live for this call
    let length =
        unsafe { QueryDosDeviceW(PCWSTR(drive_name.as_ptr()), Some(&mut target)) } as usize;
    if length == 0 || length > target.len() {
        return Err("Could not resolve bundled profile package volume".into());
    }
    let end = target[..length]
        .iter()
        .position(|&ch| ch == 0)
        .ok_or("Invalid bundled profile package volume")?;
    // QueryDosDevice can also return older mappings. Only its first, current
    // target is used, and the mutable DOS alias is never opened afterward
    volume_root(&target[..end])
}

// Open the requested Windows resource
fn open(parent: Option<&OwnedHandle>, name: &mut [u16], kind: Kind) -> Result<OwnedHandle, String> {
    let length = unicode_byte_length(name)?;
    let object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: PWSTR(name.as_mut_ptr()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.map_or(HANDLE::default(), |parent| HANDLE(parent.as_raw_handle())),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut raw = HANDLE::default();
    let mut io = IO_STATUS_BLOCK::default();
    // All FFI inputs are live for the synchronous open. Descendant names are
    // validated single components; the sole absolute name is the volume root
    // No handles are inherited, and neither the leaf nor ancestors are followed
    // through reparse points. Successful native handles have exactly one owner
    let status = unsafe {
        NtOpenFile(
            &mut raw,
            kind.access(),
            &attributes,
            &mut io,
            (FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0,
            kind.options(),
        )
    };
    if status.is_err() || raw.is_invalid() {
        return Err("Could not open bundled profile path without reparse points".into());
    }
    // NtOpenFile with synchronous options returns an owned handle on success
    // Convert immediately so every subsequent validation error closes it
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    check_handle(&handle, kind)?;
    Ok(handle)
}

// Check handle
fn check_handle(handle: &OwnedHandle, kind: Kind) -> Result<(), String> {
    let raw = HANDLE(handle.as_raw_handle());
    // Both queries inspect the already-opened object, never its pathname
    if unsafe { GetFileType(raw) } != FILE_TYPE_DISK {
        return Err("Bundled profile path must be on a regular disk".into());
    }
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            raw,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(|_| "Could not inspect bundled profile path")?;
    let is_directory = info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || is_directory != matches!(kind, Kind::Directory)
    {
        return Err("Bundled profile path has an unexpected type or reparse point".into());
    }
    Ok(())
}

// Open the package directory through handle-relative components on its validated local volume
pub(super) fn directory(path: &Path) -> Result<OwnedHandle, String> {
    let (drive, parts) = root_parts(path)?;
    let mut current = open(None, &mut drive_root(drive)?, Kind::Directory)?;
    for mut name in parts {
        current = open(Some(&current), &mut name, Kind::Directory)?;
    }
    Ok(current)
}

// Read from directory
pub(super) fn read_from_directory(
    root: &OwnedHandle,
    relative: &Path,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut parts = normal_parts(relative.components())?;
    let (name, parents) = parts.split_last_mut().ok_or("Empty bundled profile path")?;
    let mut parent = None;
    for name in parents {
        parent = Some(open(
            Some(parent.as_ref().unwrap_or(root)),
            name,
            Kind::Directory,
        )?);
    }
    let handle = open(Some(parent.as_ref().unwrap_or(root)), name, Kind::Archive)?;
    let file = File::from(handle);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs::{self, OpenOptions},
        os::windows::{ffi::OsStringExt, fs::OpenOptionsExt},
    };
    use windows::Win32::{
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
        System::{
            Ioctl::FSCTL_SET_REPARSE_POINT, SystemServices::IO_REPARSE_TAG_MOUNT_POINT,
            IO::DeviceIoControl,
        },
    };

    // Read the requested Windows data
    fn read(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>, String> {
        read_from_directory(&directory(root)?, relative, limit)
    }

    // Junction creation does not require symbolic-link privilege or Developer
    // Mode. Fixtures stay inside a disposable local-volume directory. These
    // tests must execute on Windows; cross-compilation is not qualification
    fn junction(link: &Path, target: &Path) {
        fs::create_dir(link).unwrap();
        let target = target.canonicalize().unwrap();
        let mut substitute: Vec<_> = target.as_os_str().encode_wide().collect();
        assert!(substitute.starts_with(&[92, 92, 63, 92]));
        substitute[1] = b'?' as u16; // Win32 \\?\ becomes native \??\.
        let substitute_length = unicode_byte_length(&substitute).unwrap();
        substitute.extend([0, 0]); // Substitute terminator, empty print name
        let path_length = unicode_byte_length(&substitute).unwrap();
        let mut data = Vec::new();
        data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        data.extend((8 + path_length).to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend(0u16.to_le_bytes()); // SubstituteNameOffset
        data.extend(substitute_length.to_le_bytes());
        data.extend((substitute_length + 2).to_le_bytes()); // PrintNameOffset
        data.extend(0u16.to_le_bytes()); // PrintNameLength
        for ch in substitute {
            data.extend(ch.to_le_bytes());
        }
        let handle = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(link)
            .unwrap();
        let mut returned = 0;
        // The fixture's byte buffer encodes the documented mount-point reparse
        // header and UTF-16 path; DeviceIoControl consumes it synchronously
        unsafe {
            DeviceIoControl(
                HANDLE(handle.as_raw_handle()),
                FSCTL_SET_REPARSE_POINT,
                Some(data.as_ptr().cast()),
                data.len().try_into().unwrap(),
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .expect("Junction fixtures require a local filesystem with reparse support");
    }

    #[test]
    // Verify that rejects unsafe root names and drive mappings
    fn rejects_unsafe_root_names_and_drive_mappings() {
        for path in [
            "",
            ".",
            r"package",
            r"C:package",
            r"\package",
            r"C:\package\..\outside",
            r"\\server\share\package",
            r"\\?\UNC\server\share\package",
            r"\\.\C:\package",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\package",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\package",
            "C:\\package\0ignored",
            r"C:\package:stream",
        ] {
            assert!(root_parts(Path::new(path)).is_err(), "{path:?}");
        }
        for path in [r"C:\package", r"\\?\C:\package", r"c:\package", r"C:\"] {
            assert!(root_parts(Path::new(path)).is_ok(), "{path:?}");
        }
        for target in [
            "",
            r"\Device\HarddiskVolume",
            r"\Device\HarddiskVolume1\package",
            r"\Device\HarddiskVolume1\",
            r"\??\C:\package",
            r"\Device\LanmanRedirector\server\share",
            r"\Device\NamedPipe",
            r"\Device\HarddiskVolume1:stream",
        ] {
            assert!(volume_root(&target.encode_utf16().collect::<Vec<_>>()).is_err());
        }
        assert_eq!(
            volume_root(
                &r"\Device\HarddiskVolume12"
                    .encode_utf16()
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            r"\Device\HarddiskVolume12\"
                .encode_utf16()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    // Verify that validates native component lengths without lossy conversion
    fn validates_native_component_lengths_without_lossy_conversion() {
        for name in [
            "",
            ".",
            "..",
            "archive:stream",
            "file\0ignored",
            "a/b",
            r"a\b",
        ] {
            assert!(checked_name(OsStr::new(name)).is_err(), "{name:?}");
        }
        let unpaired_surrogate = OsString::from_wide(&[b'a' as u16, 0xd800]);
        assert_eq!(
            checked_name(&unpaired_surrogate).unwrap(),
            [b'a' as u16, 0xd800]
        );
        assert_eq!(unicode_byte_length(&vec![1; 32767]).unwrap(), 65534);
        assert!(unicode_byte_length(&vec![1; 32768]).is_err());
        assert!(checked_name(&OsString::from_wide(&vec![1; 32768])).is_err());
    }

    #[test]
    // Verify that requests synchronous read only non reparse handles
    fn requests_synchronous_read_only_non_reparse_handles() {
        for kind in [Kind::Directory, Kind::Archive] {
            assert_ne!(kind.options() & FILE_OPEN_REPARSE_POINT.0, 0);
            assert_ne!(kind.options() & FILE_SYNCHRONOUS_IO_NONALERT.0, 0);
            assert_ne!(kind.access() & SYNCHRONIZE.0, 0);
            assert_ne!(kind.access() & FILE_READ_ATTRIBUTES.0, 0);
        }
        assert_eq!(
            Kind::Directory.access(),
            FILE_READ_ATTRIBUTES.0 | FILE_TRAVERSE.0 | SYNCHRONIZE.0
        );
        assert_eq!(
            Kind::Archive.access(),
            FILE_READ_ATTRIBUTES.0 | FILE_READ_DATA.0 | SYNCHRONIZE.0
        );
        assert_ne!(Kind::Directory.options() & FILE_DIRECTORY_FILE.0, 0);
        assert_eq!(Kind::Directory.options() & FILE_NON_DIRECTORY_FILE.0, 0);
        assert_ne!(Kind::Archive.options() & FILE_NON_DIRECTORY_FILE.0, 0);
        assert_eq!(Kind::Archive.options() & FILE_DIRECTORY_FILE.0, 0);
    }

    #[test]
    // Verify that reads only bounded regular package files
    fn reads_only_bounded_regular_package_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("profiles")).unwrap();
        fs::write(root.join("profiles/archive"), b"profile").unwrap();
        assert_eq!(
            read(&root, Path::new("profiles/archive"), 7).unwrap(),
            b"profile"
        );
        assert!(read(&root, Path::new("profiles/archive"), 6).is_err());
        for path in [
            "",
            ".",
            "../outside",
            "/outside",
            "profiles",
            r"C:\outside",
            r"C:outside",
            "profiles/archive:stream",
            "profiles/../profiles/archive",
        ] {
            assert!(read(&root, Path::new(path), 1024).is_err(), "{path}");
        }
    }

    #[test]
    // Verify that error paths and success release opened handles
    fn error_paths_and_success_release_opened_handles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = root.join("archive");
        fs::write(&path, b"inside").unwrap();
        assert!(read(&root, Path::new("archive"), 5).is_err());
        // Exclusive opening fails if an error leaked a reader handle
        drop(
            OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap(),
        );
        assert_eq!(read(&root, Path::new("archive"), 6).unwrap(), b"inside");
        drop(
            OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap(),
        );
        assert!(read(&root, Path::new("missing/archive"), 1024).is_err());
        drop(
            OpenOptions::new()
                .read(true)
                .share_mode(0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
                .open(&root)
                .unwrap(),
        );
    }

    #[test]
    // Verify that rejects junctions at root ancestors parents and leaf
    fn rejects_junctions_at_root_ancestors_parents_and_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        let outside = root.join("outside");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("archive"), b"outside").unwrap();
        junction(&package.join("parent"), &outside);
        junction(&root.join("alias"), &package);
        assert!(read(&package, Path::new("parent/archive"), 1024).is_err());
        assert!(read(&package, Path::new("parent"), 1024).is_err());
        assert!(directory(&package.join("parent")).is_err());
        assert!(directory(&root.join("alias/parent")).is_err());
    }

    #[test]
    // Verify that pinned root survives replacement and rejects swapped parent
    fn pinned_root_survives_replacement_and_rejects_swapped_parent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        let outside = root.join("outside");
        fs::create_dir_all(package.join("profiles")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(package.join("profiles/archive"), b"inside").unwrap();
        fs::write(outside.join("archive"), b"outside").unwrap();
        let pinned = directory(&package).unwrap();
        let old = root.join("old-package");
        fs::rename(&package, &old).unwrap();
        junction(&package, &outside);
        assert_eq!(
            read_from_directory(&pinned, Path::new("profiles/archive"), 1024).unwrap(),
            b"inside"
        );
        assert!(read(&package, Path::new("archive"), 1024).is_err());
        fs::rename(old.join("profiles"), old.join("old-profiles")).unwrap();
        junction(&old.join("profiles"), &outside);
        assert!(read_from_directory(&pinned, Path::new("profiles/archive"), 1024).is_err());
    }

    #[test]
    // Verify that reads utf16 names without accepting nul or streams
    fn reads_utf16_names_without_accepting_nul_or_streams() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let name = OsString::from_wide(&[b'a' as u16, 0xd800]);
        fs::write(root.join(&name), b"inside").unwrap();
        assert_eq!(read(&root, Path::new(&name), 6).unwrap(), b"inside");
        let invalid = OsString::from_wide(&[b'a' as u16, 0, b'b' as u16]);
        assert!(read(&root, Path::new(&invalid), 1024).is_err());
        fs::write(root.join("archive"), b"inside").unwrap();
        fs::write(root.join("archive:outside"), b"outside").unwrap();
        assert!(read(&root, Path::new("archive:outside"), 1024).is_err());
    }

    #[test]
    #[ignore = "Requires Windows symbolic-link privilege or Developer Mode in a disposable local-volume fixture"]
    // Verify that rejects file symlinks and replacement after admission
    fn rejects_file_symlinks_and_replacement_after_admission() {
        use std::os::windows::fs::symlink_file;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let package = root.join("package");
        fs::create_dir(&package).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"outside").unwrap();
        fs::write(package.join("archive"), b"inside").unwrap();
        let pinned = directory(&package).unwrap();
        fs::remove_file(package.join("archive")).unwrap();
        symlink_file(&outside, package.join("archive")).unwrap();
        assert!(read_from_directory(&pinned, Path::new("archive"), 1024).is_err());
        assert!(read(&package, Path::new("archive"), 1024).is_err());
    }
}
