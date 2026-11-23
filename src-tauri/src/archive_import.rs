use crate::editor_lifecycle::{self, EditorWork};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use stream_dock_manager_runtime::archive_import::{
    identify, ArchiveImportKind, MAX_DROPPED_ARCHIVE,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tempfile::TempDir;

const MAX_PENDING: usize = 16;
const MAX_CHUNK: usize = 1024 * 1024;
const MAX_FOLDER_FILES: usize = 4096;
const MAX_FOLDER_DIRS: usize = 4096;
const MAX_FOLDER_DEPTH: usize = 64;
const MAX_COMPONENT: usize = 255;
const MAX_RELATIVE_PATH: usize = 4096;
const MAX_PLUGIN_FILE: u64 = 64 * 1024 * 1024;
const MAX_PLUGIN_SIZE: u64 = 256 * 1024 * 1024;
const INVALID_FOLDER: &str = "Invalid dropped plugin folder transfer.";
const UNSAFE_PATH: &str = "Dropped plugin folder contains an invalid path.";
const CONFLICT: &str = "Dropped plugin paths are duplicated or conflict.";
const OUT_OF_SEQUENCE: &str = "Dropped plugin folder transfer is out of sequence.";
const FOLDER_LIMIT: &str = "Dropped plugin folder exceeds package limits.";
const FOLDER_INCOMPLETE: &str = "Dropped plugin folder transfer is incomplete.";

struct Upload {
    file: File,
    path: PathBuf,
    _directory: TempDir,
    size: u64,
    received: u64,
}

enum DropUpload {
    Archive(Upload),
    PluginFolder(FolderUpload),
}

struct FolderChunk {
    start: bool,
    root: String,
    path: String,
    size: u64,
    offset: u64,
}

struct FolderFile {
    path: String,
    size: u64,
    received: u64,
    file: File,
}

struct FolderPath {
    spelling: String,
    directory: bool,
}

struct FolderUpload {
    editor: u64,
    current: Option<FolderFile>,
    _directory: TempDir,
    root: PathBuf,
    name: String,
    // Retain each occupied portable name, including intermediate directories.
    paths: HashMap<String, FolderPath>,
    files: usize,
    directories: usize,
    bytes: u64,
    sealed: bool,
}

fn portable_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > MAX_COMPONENT
        || matches!(name, "." | "..")
        || name.ends_with([' ', '.'])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return false;
    }

    // Windows treats these as aliases even with extensions. The superscript
    // digits are also recognized as device numbers.
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "AUX" | "CON" | "CONIN$" | "CONOUT$" | "NUL" | "PRN"
    ) {
        return false;
    }
    !["COM", "LPT"].into_iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

impl FolderChunk {
    fn validate(&self, length: usize) -> Result<(), String> {
        if !portable_name(&self.root)
            || !self.root.to_ascii_lowercase().ends_with(".sdplugin")
            || self.root.len() <= ".sdPlugin".len()
            || self.path.len() > MAX_RELATIVE_PATH
            || self.path.split('/').count() > MAX_FOLDER_DEPTH
            || !self.path.split('/').all(portable_name)
        {
            return Err(UNSAFE_PATH.into());
        }
        if length > MAX_CHUNK
            || self.size > MAX_PLUGIN_FILE
            || (length == 0 && (self.size != 0 || self.offset != 0))
            || self
                .offset
                .checked_add(length as u64)
                .is_none_or(|end| end > self.size)
        {
            return Err(FOLDER_LIMIT.into());
        }
        if self.start && self.offset != 0 {
            return Err(OUT_OF_SEQUENCE.into());
        }
        Ok(())
    }
}

impl FolderUpload {
    fn new(editor: u64, name: String) -> Result<Self, String> {
        let directory = tempfile::Builder::new()
            .prefix("stream-dock-manager-plugin-drop-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = directory.path().join(&name);
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        Ok(Self {
            editor,
            _directory: directory,
            root,
            name,
            paths: HashMap::new(),
            files: 0,
            directories: 0,
            bytes: 0,
            current: None,
            sealed: false,
        })
    }

    fn start_file(&mut self, name: &str, size: u64) -> Result<FolderFile, String> {
        let bytes = self.bytes.checked_add(size).ok_or(FOLDER_LIMIT)?;
        if self.files >= MAX_FOLDER_FILES || bytes > MAX_PLUGIN_SIZE {
            return Err(FOLDER_LIMIT.into());
        }
        let mut destination = self.root.clone();
        let mut relative = String::new();
        let mut parts = name.split('/').peekable();
        while let Some(part) = parts.next() {
            destination.push(part);
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            let key = relative.to_lowercase();
            let directory = parts.peek().is_some();
            if let Some(existing) = self.paths.get(&key) {
                if !directory || !existing.directory || existing.spelling != relative {
                    return Err(CONFLICT.into());
                }
                continue;
            }
            if !directory {
                let file = File::options()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::AlreadyExists {
                            CONFLICT.into()
                        } else {
                            error.to_string()
                        }
                    })?;
                self.paths.insert(
                    key,
                    FolderPath {
                        spelling: relative,
                        directory,
                    },
                );
                self.files += 1;
                self.bytes = bytes;
                return Ok(FolderFile {
                    path: name.into(),
                    size,
                    received: 0,
                    file,
                });
            }
            if self.directories >= MAX_FOLDER_DIRS {
                return Err(FOLDER_LIMIT.into());
            }
            // Never reuse an on-disk directory the portable inventory did not
            // create under this exact spelling. This also catches filesystem
            // aliases not covered by case folding (e.g. decomposed Unicode).
            fs::create_dir(&destination).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    CONFLICT.into()
                } else {
                    error.to_string()
                }
            })?;
            self.paths.insert(
                key,
                FolderPath {
                    spelling: relative.clone(),
                    directory,
                },
            );
            self.directories += 1;
        }
        Err(UNSAFE_PATH.into())
    }

    fn append(&mut self, chunk: FolderChunk, bytes: &[u8]) -> Result<(), String> {
        if self.sealed || self.name != chunk.root {
            return Err(OUT_OF_SEQUENCE.into());
        }
        if chunk.offset == 0 {
            if self.current.is_some() {
                return Err(OUT_OF_SEQUENCE.into());
            }
            self.current = Some(self.start_file(&chunk.path, chunk.size)?);
        }
        let current = self
            .current
            .as_mut()
            .filter(|current| {
                current.path == chunk.path
                    && current.size == chunk.size
                    && current.received == chunk.offset
            })
            .ok_or(OUT_OF_SEQUENCE)?;
        current.file.write_all(bytes).map_err(|e| e.to_string())?;
        current.received += bytes.len() as u64;
        if current.received == current.size {
            self.current.take();
        }
        Ok(())
    }

    fn check_complete(&self) -> Result<(), String> {
        if self.current.is_some() {
            return Err(FOLDER_INCOMPLETE.into());
        }
        if !self
            .paths
            .get("manifest.json")
            .is_some_and(|entry| !entry.directory && entry.spelling == "manifest.json")
        {
            return Err("A dropped plugin folder needs a root manifest.json.".into());
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(String, ArchiveImportKind), String> {
        self.check_complete()?;
        if !self.sealed {
            stream_dock_manager_plugin_host::prepare_dropped_folder_executables(&self.root)?;
            self.sealed = true;
        }
        Ok((
            self.root
                .to_str()
                .ok_or("The temporary plugin path is not UTF-8.")?
                .into(),
            ArchiveImportKind::Plugin,
        ))
    }
}

fn append_folder(
    slot: &mut Option<DropUpload>,
    editor: u64,
    incoming: Result<(FolderChunk, Vec<u8>), String>,
) -> Result<(), String> {
    // Taking ownership lets TempDir remove anything partially staged on any error.
    // An invalid folder request does not discard an unrelated ZIP or sealed review.
    let prior = match slot.as_ref() {
        Some(DropUpload::PluginFolder(folder)) if folder.editor == editor && !folder.sealed => {
            slot.take()
        }
        _ => None,
    };
    let (chunk, bytes) = incoming?;
    chunk.validate(bytes.len())?;
    let mut folder = if chunk.start {
        *slot = None;
        FolderUpload::new(editor, chunk.root.clone())?
    } else {
        let Some(DropUpload::PluginFolder(folder)) = prior else {
            return Err(OUT_OF_SEQUENCE.into());
        };
        folder
    };
    folder.append(chunk, &bytes)?;
    *slot = Some(DropUpload::PluginFolder(folder));
    Ok(())
}

#[derive(Default)]
struct Pending {
    paths: VecDeque<String>,
    warning: Option<&'static str>,
}

#[derive(Default)]
pub struct ArchiveDrops {
    pending: Mutex<Pending>,
    upload: Arc<Mutex<Option<DropUpload>>>,
    epoch: Arc<AtomicU64>,
}

impl ArchiveDrops {
    fn discard(&self) {
        let mut upload = self.upload.lock().unwrap();
        // Invalidate queued native workers before discarding the partial tree.
        self.epoch.fetch_add(1, Ordering::AcqRel);
        upload.take();
    }
}

// Receive for the current operation
pub fn receive(app: &AppHandle, paths: &[PathBuf]) {
    let state = app.state::<ArchiveDrops>();
    let mut pending = state.pending.lock().unwrap();
    for path in paths.iter().filter(|path| path.is_absolute()) {
        match path.to_str() {
            Some(path) if pending.paths.len() < MAX_PENDING => pending.paths.push_back(path.into()),
            Some(_) => {
                pending.warning =
                    Some("The archive import queue was full. Drop the remaining files again.");
                break;
            }
            None => pending.warning = Some("A dropped archive path is not UTF-8."),
        }
    }
    let has_pending = !pending.paths.is_empty() || pending.warning.is_some();
    drop(pending);
    if has_pending {
        editor_lifecycle::reveal_with_activation(app, None);
    }
}

// Notify for the current operation
pub fn notify(app: &AppHandle) {
    if app
        .state::<crate::commands::EditorReadiness>()
        .0
        .load(Ordering::SeqCst)
        && app.try_state::<ArchiveDrops>().is_some_and(|state| {
            let pending = state.pending.lock().unwrap();
            !pending.paths.is_empty() || pending.warning.is_some()
        })
    {
        let _ = app.emit_to("main", "archive-dropped", ());
    }
}

// Discard upload
pub fn discard_upload(app: &AppHandle) {
    if let Some(state) = app.try_state::<ArchiveDrops>() {
        state.discard();
    }
}
// Handle the next archive drop desktop command

#[tauri::command]
pub fn next_archive_drop(
    _editor_work: EditorWork,
    state: State<ArchiveDrops>,
) -> Result<Option<String>, String> {
    let mut pending = state.pending.lock().unwrap();
    if let Some(path) = pending.paths.pop_front() {
        return Ok(Some(path));
    }
    if let Some(warning) = pending.warning.take() {
        return Err(warning.into());
    }
    Ok(None)
}
// Handle the append archive drop desktop command

#[tauri::command]
pub async fn append_archive_drop(
    work: EditorWork,
    state: State<'_, ArchiveDrops>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Invalid archive transfer")
    };
    let size = header("x-archive-size")?;
    let offset = header("x-archive-offset")?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Archive transfers require binary data.".into());
    };
    if bytes.is_empty()
        || bytes.len() > MAX_CHUNK
        || size > MAX_DROPPED_ARCHIVE
        || offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > size)
    {
        return Err("Archive transfer exceeds size limits.".into());
    }
    let filename = if offset == 0 {
        let name = request
            .headers()
            .get("x-archive-name")
            .filter(|value| value.len() <= 2048)
            .and_then(|value| STANDARD.decode(value.as_bytes()).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or("Invalid archive filename.")?;
        let name = name.replace(['/', '\\', ':', '\0'], "_");
        if name.is_empty() || matches!(name.as_str(), "." | "..") {
            return Err("Invalid archive filename.".into());
        }
        Some(name)
    } else {
        None
    };
    let bytes = bytes.clone();
    let upload = state.upload.clone();
    let epoch = state.epoch.clone();
    let observed = epoch.load(Ordering::Acquire);
    editor_lifecycle::spawn_blocking(&work, move || {
        let mut upload = upload.lock().unwrap();
        if epoch.load(Ordering::Acquire) != observed {
            return Err("Archive transfer is out of sequence.".into());
        }
        if let Some(filename) = filename {
            let directory = tempfile::Builder::new()
                .prefix("stream-dock-manager-import-")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let path = directory.path().join(filename);
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            *upload = Some(DropUpload::Archive(Upload {
                _directory: directory,
                path,
                file,
                size,
                received: 0,
            }));
        }
        let current = upload
            .as_mut()
            .and_then(|upload| match upload {
                DropUpload::Archive(upload) if upload.size == size && upload.received == offset => {
                    Some(upload)
                }
                _ => None,
            })
            .ok_or("Archive transfer is out of sequence.")?;
        current.file.write_all(&bytes).map_err(|e| e.to_string())?;
        current.received += bytes.len() as u64;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
/// Receive one bounded chunk of a user-dropped package folder through binary IPC.
#[tauri::command]
pub async fn append_plugin_folder_drop(
    work: EditorWork,
    state: State<'_, ArchiveDrops>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    // EditorWork already validates the header at admission. Retain that exact
    // value, rather than sampling a possibly newer generation in the worker.
    let editor = request
        .headers()
        .get("x-stream-dock-manager-editor-generation")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(INVALID_FOLDER)?;
    let incoming = (|| {
        let headers = request.headers();
        let decoded = |name, max: usize| {
            headers
                .get(name)
                .filter(|value| value.len() <= max.div_ceil(3) * 4)
                .and_then(|value| STANDARD.decode(value.as_bytes()).ok())
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .ok_or(INVALID_FOLDER)
        };
        let decimal = |name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .filter(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(INVALID_FOLDER)
        };
        let start = match headers.get("x-plugin-start").map(|value| value.as_bytes()) {
            None => false,
            Some(b"1") => true,
            _ => return Err(INVALID_FOLDER.into()),
        };
        let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
            return Err(INVALID_FOLDER.into());
        };
        let chunk = FolderChunk {
            start,
            root: decoded("x-plugin-folder", MAX_COMPONENT)?,
            path: decoded("x-plugin-file", MAX_RELATIVE_PATH)?,
            size: decimal("x-plugin-size")?,
            offset: decimal("x-plugin-offset")?,
        };
        chunk.validate(bytes.len())?;
        Ok((chunk, bytes.clone()))
    })();
    let upload = state.upload.clone();
    let epoch = state.epoch.clone();
    let observed = epoch.load(Ordering::Acquire);
    let worker = work.worker();
    editor_lifecycle::blocking_result(&work, move || {
        let mut upload = upload.lock().unwrap();
        if epoch.load(Ordering::Acquire) != observed {
            return Err(OUT_OF_SEQUENCE.into());
        }
        {
            let _admission = worker
                .commit_for_generation(editor)
                .map_err(|_| OUT_OF_SEQUENCE)?;
        }
        append_folder(&mut upload, editor, incoming)?;
        if worker.commit_for_generation(editor).is_err() {
            upload.take();
            return Err(OUT_OF_SEQUENCE.into());
        }
        Ok(())
    })
    .await
}

// Handle the inspect archive drop desktop command

#[tauri::command]
pub async fn inspect_archive_drop(
    work: EditorWork,
    state: State<'_, ArchiveDrops>,
    path: Option<String>,
) -> Result<(String, ArchiveImportKind), String> {
    let editor = work.generation()?;
    let upload = state.upload.clone();
    let epoch = state.epoch.clone();
    let observed = epoch.load(Ordering::Acquire);
    let worker = work.worker();
    editor_lifecycle::spawn_blocking(&work, move || {
        if let Some(path) = path {
            let kind = identify(Path::new(&path))?;
            return Ok((path, kind));
        }
        let mut upload = upload.lock().unwrap();
        if epoch.load(Ordering::Acquire) != observed {
            return Err(FOLDER_INCOMPLETE.into());
        }
        if matches!(upload.as_ref(), Some(DropUpload::PluginFolder(folder)) if folder.editor == editor) {
            let Some(DropUpload::PluginFolder(mut folder)) = upload.take() else {
                unreachable!()
            };
            let review = folder.finish()?;
            let _admission = worker
                .commit_for_generation(editor)
                .map_err(|_| OUT_OF_SEQUENCE)?;
            *upload = Some(DropUpload::PluginFolder(folder));
            return Ok(review);
        }
        let complete = upload
            .as_ref()
            .and_then(|upload| match upload {
                DropUpload::Archive(upload) if upload.received == upload.size => Some(upload),
                _ => None,
            })
            .ok_or("Archive transfer is incomplete.")?;
        let path = &complete.path;
        let kind = identify(path)?;
        Ok((
            path.to_str()
                .ok_or("The temporary archive path is not UTF-8.")?
                .into(),
            kind,
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}
// Handle the discard archive drop desktop command

#[tauri::command]
pub fn discard_archive_drop(_editor_work: EditorWork, state: State<ArchiveDrops>) {
    state.discard();
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    const ROOT: &str = "org.fixture.sdPlugin";

    fn chunk(start: bool, path: &str, size: u64, offset: u64) -> FolderChunk {
        FolderChunk {
            start,
            root: ROOT.into(),
            path: path.into(),
            size,
            offset,
        }
    }

    fn append(
        slot: &mut Option<DropUpload>,
        editor: u64,
        metadata: FolderChunk,
        bytes: &[u8],
    ) -> Result<(), String> {
        append_folder(slot, editor, Ok((metadata, bytes.to_vec())))
    }

    fn staged(slot: &Option<DropUpload>) -> &FolderUpload {
        match slot {
            Some(DropUpload::PluginFolder(folder)) => folder,
            _ => panic!("no staged folder"),
        }
    }

    #[test]
    fn stages_exact_bytes_in_a_disposable_tree_and_reuses_exact_directories() {
        let mut slot = None;
        append(&mut slot, 7, chunk(true, "manifest.json", 2, 0), b"{").unwrap();
        assert_eq!(
            staged(&slot).check_complete().unwrap_err(),
            FOLDER_INCOMPLETE
        );
        append(&mut slot, 7, chunk(false, "manifest.json", 2, 1), b"}").unwrap();
        append(&mut slot, 7, chunk(false, "icons/café.svg", 0, 0), b"").unwrap();
        append(&mut slot, 7, chunk(false, "icons/second.svg", 3, 0), b"svg").unwrap();

        let folder = staged(&slot);
        folder.check_complete().unwrap();
        assert_eq!(folder.files, 3);
        assert_eq!(folder.directories, 1);
        assert_eq!(folder.bytes, 5);
        assert_eq!(fs::read(folder.root.join("manifest.json")).unwrap(), b"{}");
        assert_eq!(fs::read(folder.root.join("icons/café.svg")).unwrap(), b"");
        assert_eq!(
            fs::read(folder.root.join("icons/second.svg")).unwrap(),
            b"svg"
        );
        let staging = folder.root.clone();
        drop(slot);
        assert!(!staging.exists());
    }

    #[test]
    fn rejects_missing_root_manifest_and_conflicting_portable_paths() {
        for (first, second) in [
            ("icons/icon.svg", "icons/icon.svg"),
            ("icons/icon.svg", "Icons/other.svg"),
            ("icons/icon.svg", "icons/ICON.svg"),
            ("icons", "icons/icon.svg"),
            ("icons/icon.svg", "icons"),
        ] {
            let mut slot = None;
            append(&mut slot, 7, chunk(true, first, 0, 0), b"").unwrap();
            assert!(staged(&slot).check_complete().is_err());
            let root = staged(&slot).root.clone();
            assert_eq!(
                append(&mut slot, 7, chunk(false, second, 0, 0), b"").unwrap_err(),
                CONFLICT
            );
            assert!(slot.is_none());
            assert!(!root.exists());
        }
        let mut slot = None;
        append(&mut slot, 7, chunk(true, "Manifest.json", 0, 0), b"").unwrap();
        assert!(staged(&slot).check_complete().is_err());
    }

    #[test]
    fn fails_closed_on_invalid_chunk_or_sequence_without_affecting_other_editors() {
        for failure in [
            chunk(false, "manifest.json", 4, 2),
            chunk(false, "manifest.json", 3, 1),
            chunk(false, "other", 4, 1),
            chunk(false, "other", 4, 0),
            chunk(false, "../outside", 4, 0),
            FolderChunk {
                root: "other.sdPlugin".into(),
                ..chunk(false, "manifest.json", 4, 1)
            },
        ] {
            let mut slot = None;
            append(&mut slot, 7, chunk(true, "manifest.json", 4, 0), b"{").unwrap();
            let root = staged(&slot).root.clone();
            assert!(append(&mut slot, 6, chunk(false, "manifest.json", 4, 1), b"}").is_err());
            assert!(root.exists());
            assert!(append(&mut slot, 7, failure, b"}").is_err());
            assert!(slot.is_none());
            assert!(!root.exists());
        }
        let mut slot = None;
        append(&mut slot, 7, chunk(true, "manifest.json", 4, 0), b"{").unwrap();
        let root = staged(&slot).root.clone();
        assert!(append_folder(&mut slot, 7, Err(INVALID_FOLDER.into())).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn enforces_portable_path_byte_depth_and_upload_limits_before_writing() {
        for name in [
            "",
            ".",
            "..",
            "../x",
            "/x",
            "bin//x",
            "bin/x/",
            "bin\\x",
            "C:drive",
            "file:stream",
            "thing ",
            "thing.",
            "nul.txt",
            "COM1.cfg",
            "COM1 .cfg",
            "LPT³.txt",
            "CONIN$",
            "x/CONOUT$.txt",
            "\u{7f}",
            "x\u{9f}",
        ] {
            assert!(chunk(true, name, 0, 0).validate(0).is_err(), "{name:?}");
        }
        assert!(chunk(true, &"ü".repeat(127), 0, 0).validate(0).is_ok());
        assert!(chunk(true, &"ü".repeat(128), 0, 0).validate(0).is_err());
        assert!(chunk(true, &vec!["a"; 64].join("/"), 0, 0)
            .validate(0)
            .is_ok());
        assert!(chunk(true, &vec!["a"; 65].join("/"), 0, 0)
            .validate(0)
            .is_err());
        let near_path = format!("{}/{}", vec!["x".repeat(255); 16].join("/"), "x");
        assert_eq!(near_path.len(), MAX_RELATIVE_PATH + 1);
        assert!(chunk(true, &near_path, 0, 0).validate(0).is_err());
        for root in [
            ".sdPlugin",
            "parent/fixture.sdPlugin",
            "NUL.sdPlugin",
            "fixture",
        ] {
            assert!(FolderChunk {
                root: root.into(),
                ..chunk(true, "a", 0, 0)
            }
            .validate(0)
            .is_err());
        }
        assert!(chunk(true, "a", MAX_PLUGIN_FILE, 0)
            .validate(MAX_CHUNK)
            .is_ok());
        assert!(chunk(true, "a", MAX_PLUGIN_FILE + 1, 0)
            .validate(1)
            .is_err());
        assert!(chunk(true, "a", MAX_PLUGIN_FILE, 0)
            .validate(MAX_CHUNK + 1)
            .is_err());
        assert!(chunk(true, "a", 1, 0).validate(0).is_err());
        assert!(chunk(true, "a", 1, 1).validate(1).is_err());
        assert!(chunk(false, "a", MAX_PLUGIN_FILE, u64::MAX)
            .validate(1)
            .is_err());
        let mut folder = FolderUpload::new(1, ROOT.into()).unwrap();
        folder.bytes = MAX_PLUGIN_SIZE;
        assert_eq!(
            folder.start_file("oversize", 1).err().unwrap(),
            FOLDER_LIMIT
        );
        folder.files = MAX_FOLDER_FILES;
        assert_eq!(
            folder.start_file("file_limit", 0).err().unwrap(),
            FOLDER_LIMIT
        );
        folder.files = 0;
        folder.directories = MAX_FOLDER_DIRS;
        assert_eq!(
            folder.start_file("dir_limit/file", 0).err().unwrap(),
            FOLDER_LIMIT
        );
        assert_eq!(fs::read_dir(folder.root).unwrap().count(), 0);
    }

    #[test]
    fn editor_binary_ipc_stages_inspects_and_discards_only_a_complete_folder() {
        use serde_json::{json, Value};
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{self, MockRuntime};

        let app = test::mock_builder()
            .manage(ArchiveDrops::default())
            .manage(Arc::new(editor_lifecycle::EditorLifecycle::default()))
            .invoke_handler(tauri::generate_handler![
                append_plugin_folder_drop,
                inspect_archive_drop,
                discard_archive_drop,
            ])
            .build(crate::desktop_context())
            .unwrap();
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let plugin =
            tauri::WebviewWindowBuilder::new(&app, "plugin-folder-probe", Default::default())
                .build()
                .unwrap();
        let invoke = |window: &tauri::WebviewWindow<MockRuntime>,
                      command: &str,
                      body,
                      headers|
         -> Result<Value, Value> {
            test::get_ipc_response(
                window,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: CallbackFn(0),
                    error: CallbackFn(1),
                    url: if cfg!(target_os = "windows") {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body,
                    headers,
                    invoke_key: test::INVOKE_KEY.into(),
                },
            )
            .map(|response| response.deserialize().unwrap())
        };
        let generation = [("x-stream-dock-manager-editor-generation", "1")];
        let headers = || {
            generation
                .into_iter()
                .map(|(name, value)| (name.parse().unwrap(), value.parse().unwrap()))
                .collect::<tauri::http::HeaderMap>()
        };
        let stage =
            |window: &tauri::WebviewWindow<MockRuntime>, metadata: FolderChunk, bytes: &[u8]| {
                let mut headers = headers();
                for (name, value) in [
                    ("x-plugin-folder", STANDARD.encode(metadata.root)),
                    ("x-plugin-file", STANDARD.encode(metadata.path)),
                    ("x-plugin-size", metadata.size.to_string()),
                    ("x-plugin-offset", metadata.offset.to_string()),
                ] {
                    headers.insert(name, value.parse().unwrap());
                }
                if metadata.start {
                    headers.insert("x-plugin-start", "1".parse().unwrap());
                }
                invoke(
                    window,
                    "append_plugin_folder_drop",
                    InvokeBody::Raw(bytes.to_vec()),
                    headers,
                )
            };
        let inspect = || {
            invoke(
                &main,
                "inspect_archive_drop",
                InvokeBody::Json(json!({"path": null})),
                headers(),
            )
        };

        let denied = stage(&plugin, chunk(true, "manifest.json", 2, 0), b"{}").unwrap_err();
        assert!(denied.to_string().contains("not allowed"), "{denied}");

        stage(&main, chunk(true, "manifest.json", 2, 0), b"{").unwrap();
        assert_eq!(inspect().unwrap_err(), json!(FOLDER_INCOMPLETE));
        assert_eq!(
            stage(&main, chunk(false, "manifest.json", 2, 1), b"}").unwrap_err(),
            json!(OUT_OF_SEQUENCE)
        );

        stage(&main, chunk(true, "manifest.json", 2, 0), b"xx").unwrap();
        assert!(inspect().is_err());
        assert!(app.state::<ArchiveDrops>().upload.lock().unwrap().is_none());

        let manifest = serde_json::to_vec(&json!({
            "UUID": "org.fixture",
            "Name": "Folder fixture",
            "SDKVersion": 1,
            "Version": "1.0",
            "StreamDockManager": {"ManifestDialect": "vsd"},
            "CodePath": "index.html",
            "Actions": [{"UUID": "org.fixture.action", "Name": "Fixture"}]
        }))
        .unwrap();
        let offset = 9;
        stage(
            &main,
            chunk(true, "manifest.json", manifest.len() as u64, 0),
            &manifest[..offset],
        )
        .unwrap();
        stage(
            &main,
            chunk(false, "manifest.json", manifest.len() as u64, offset as u64),
            &manifest[offset..],
        )
        .unwrap();
        let index = b"<!doctype html>";
        stage(
            &main,
            chunk(false, "index.html", index.len() as u64, 0),
            index,
        )
        .unwrap();
        stage(&main, chunk(false, "icons/empty.txt", 0, 0), b"").unwrap();
        let result = inspect().unwrap();
        assert_eq!(result[1], json!("plugin"));
        let path = Path::new(result[0].as_str().unwrap());
        assert_eq!(path.file_name().unwrap(), ROOT);
        assert_eq!(fs::read(path.join("manifest.json")).unwrap(), manifest);
        assert_eq!(fs::read(path.join("icons/empty.txt")).unwrap(), b"");
        assert!(stage(&main, chunk(false, "late.txt", 0, 0), b"").is_err());
        assert!(path.is_dir());
        invoke(
            &main,
            "discard_archive_drop",
            InvokeBody::Json(json!({})),
            headers(),
        )
        .unwrap();
        assert!(!path.exists());
    }
}
