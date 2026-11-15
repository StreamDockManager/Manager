use crate::editor_lifecycle::{self, EditorWork};
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use stream_dock_manager_runtime::{media, video};
use tauri::{AppHandle, Manager, State};

const CHUNK: usize = 1024 * 1024;

struct Upload {
    _directory: tempfile::TempDir,
    path: PathBuf,
    file: File,
    size: u64,
    written: u64,
    generation: u64,
    format: &'static str,
    validated: bool,
}

type Pending = std::collections::HashMap<uuid::Uuid, Upload>;

#[derive(Default)]
pub struct MediaDrops(Arc<Mutex<Pending>>);

// Discard uploads
pub fn discard_uploads(app: &AppHandle) {
    if let Some(drops) = app.try_state::<MediaDrops>() {
        drops.0.lock().unwrap().clear();
    }
}
// Handle the append media drop desktop command

#[tauri::command]
pub async fn append_media_drop(
    work: EditorWork,
    state: State<'_, MediaDrops>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let header = |key| {
        request
            .headers()
            .get(key)
            .and_then(|value| value.to_str().ok())
    };
    let id = header("x-media-id")
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or("Invalid media upload")?;
    let size = header("x-media-size")
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("Invalid media size")?;
    let offset = header("x-media-offset")
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("Invalid media offset")?;
    let format = match header("x-media-format") {
        Some("png") => "png",
        Some("jpeg") => "jpeg",
        Some("svg") => "svg",
        Some("gif") => "gif",
        Some("mp4") => "mp4",
        _ => return Err("Unsupported dropped media format".into()),
    };
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Dropped media requires binary data".into());
    };
    let limit = match format {
        "mp4" => video::MAX_RECORDING,
        "svg" => 256 * 1024,
        _ => media::MAX_SOURCE as u64,
    };
    if size > limit
        || bytes.is_empty()
        || bytes.len() > CHUNK
        || offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > size)
    {
        return Err("Dropped media exceeds its size limit".into());
    }
    let bytes = bytes.clone();
    let pending = state.0.clone();
    let generation = work.generation()?;
    editor_lifecycle::spawn_blocking(&work, move || {
        let mut pending = pending.lock().unwrap();
        if offset == 0 {
            if pending.len() >= 2 || pending.contains_key(&id) {
                return Err("Finish the current media import first".into());
            }
            let directory = tempfile::Builder::new()
                .prefix("stream-dock-manager-media-drop-")
                .tempdir()
                .map_err(|error| error.to_string())?;
            let path = directory.path().join(format!("Dropped media.{format}"));
            let file = File::options()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            pending.insert(
                id,
                Upload {
                    _directory: directory,
                    path,
                    file,
                    size,
                    written: 0,
                    generation,
                    format,
                    validated: false,
                },
            );
        }
        let upload = pending
            .get_mut(&id)
            .filter(|upload| {
                upload.generation == generation
                    && upload.size == size
                    && upload.format == format
                    && upload.written == offset
                    && !upload.validated
            })
            .ok_or("Dropped media is out of sequence")?;
        upload
            .file
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        upload.written += bytes.len() as u64;
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the finish media drop desktop command

#[tauri::command]
pub async fn finish_media_drop(
    work: EditorWork,
    state: State<'_, MediaDrops>,
    id: String,
) -> Result<String, String> {
    let id = uuid::Uuid::parse_str(&id).map_err(|_| "Invalid media upload")?;
    let generation = work.generation()?;
    let pending = state.0.clone();
    editor_lifecycle::spawn_blocking(&work, move || {
        let mut pending = pending.lock().unwrap();
        let upload = pending
            .get_mut(&id)
            .filter(|upload| upload.generation == generation && upload.written == upload.size)
            .ok_or("Dropped media transfer is incomplete")?;
        let mut prefix = Vec::new();
        File::open(&upload.path)
            .map_err(|error| error.to_string())?
            .take(if upload.format == "svg" {
                256 * 1024
            } else {
                32
            })
            .read_to_end(&mut prefix)
            .map_err(|error| error.to_string())?;
        if media::detect(&prefix)? != upload.format {
            return Err("The file contents do not match the selected media format".into());
        }
        upload.validated = true;
        upload
            .path
            .to_str()
            .map(str::to_owned)
            .ok_or("Invalid media staging path".into())
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the discard media drop desktop command

#[tauri::command]
pub fn discard_media_drop(
    work: EditorWork,
    state: State<MediaDrops>,
    id: String,
) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(&id).map_err(|_| "Invalid media upload")?;
    let generation = work.generation()?;
    let mut pending = state.0.lock().unwrap();
    if pending
        .get(&id)
        .is_some_and(|upload| upload.generation == generation)
    {
        pending.remove(&id);
    }
    Ok(())
}
