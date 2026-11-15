use crate::editor_lifecycle::{self, EditorWork};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    collections::VecDeque,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
};
use stream_dock_manager_runtime::archive_import::{
    identify, ArchiveImportKind, MAX_DROPPED_ARCHIVE,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tempfile::TempDir;

const MAX_PENDING: usize = 16;
const MAX_CHUNK: usize = 1024 * 1024;

struct Upload {
    file: File,
    path: PathBuf,
    _directory: TempDir,
    size: u64,
    received: u64,
}

#[derive(Default)]
struct Pending {
    paths: VecDeque<String>,
    warning: Option<&'static str>,
}

#[derive(Default)]
pub struct ArchiveDrops {
    pending: Mutex<Pending>,
    upload: Arc<Mutex<Option<Upload>>>,
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
        state.upload.lock().unwrap().take();
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
    editor_lifecycle::spawn_blocking(&work, move || {
        let mut upload = upload.lock().unwrap();
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
            *upload = Some(Upload {
                _directory: directory,
                path,
                file,
                size,
                received: 0,
            });
        }
        let current = upload
            .as_mut()
            .filter(|upload| upload.size == size && upload.received == offset)
            .ok_or("Archive transfer is out of sequence.")?;
        current.file.write_all(&bytes).map_err(|e| e.to_string())?;
        current.received += bytes.len() as u64;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
// Handle the inspect archive drop desktop command

#[tauri::command]
pub async fn inspect_archive_drop(
    work: EditorWork,
    state: State<'_, ArchiveDrops>,
    path: Option<String>,
) -> Result<(String, ArchiveImportKind), String> {
    let upload = state.upload.clone();
    editor_lifecycle::spawn_blocking(&work, move || {
        if let Some(path) = path {
            let kind = identify(Path::new(&path))?;
            return Ok((path, kind));
        }
        let upload = upload.lock().unwrap();
        let complete = upload
            .as_ref()
            .filter(|upload| upload.received == upload.size)
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
    state.upload.lock().unwrap().take();
}
