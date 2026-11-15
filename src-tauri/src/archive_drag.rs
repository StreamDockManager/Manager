use crate::editor_lifecycle::{self, EditorWork};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::InstalledPackageExport;
use stream_dock_manager_runtime::{updates::verify_file, Runtime};
use tauri::{AppHandle, Manager, WebviewWindow};
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const RECEIPT_LIFETIME: Duration = Duration::from_secs(900);

struct Archive {
    _directory: TempDir,
    _slot: OwnedSemaphorePermit,
    file: PathBuf,
    generation: u64,
    expires: Instant,
}

type Reviews = HashMap<String, (u64, Instant, Arc<InstalledPackageExport>)>;
pub struct Archives {
    slots: Arc<Semaphore>,
    archives: Mutex<HashMap<String, Arc<Archive>>>,
    reviews: Mutex<Reviews>,
    dragging: Arc<AtomicBool>,
}

impl Default for Archives {
    // Construct the default value
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(4)),
            archives: Mutex::default(),
            reviews: Mutex::default(),
            dragging: Arc::default(),
        }
    }
}

// Check whether this runtime version is supported
fn supported() -> bool {
    !crate::manager_shortcut::is_wayland()
}

// Stage for the current operation
fn stage(
    app: &AppHandle,
    generation: u64,
    filename: &str,
    write: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<String, String> {
    let state = app.state::<Archives>();
    let slot = state
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Too many ZIPs are ready. Close an export or wait for it to expire.")?;
    #[cfg(target_os = "linux")]
    let staging_root = PathBuf::from("/tmp");
    #[cfg(not(target_os = "linux"))]
    let staging_root = std::env::temp_dir();
    let directory = tempfile::Builder::new()
        .prefix("stream-dock-manager-export-")
        .tempdir_in(staging_root)
        .map_err(|error| error.to_string())?;
    let filename: String = filename
        .chars()
        .take(155)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file = directory.path().join(format!(
        "{}.zip",
        filename.trim_end_matches(".zip").trim_start_matches('.')
    ));
    write(&file)?;
    let id = uuid::Uuid::new_v4().to_string();
    let expires = Instant::now() + RECEIPT_LIFETIME;
    state.archives.lock().unwrap().insert(
        id.clone(),
        Arc::new(Archive {
            _directory: directory,
            _slot: slot,
            file,
            generation,
            expires,
        }),
    );
    let handle = app.clone();
    let expiry_id = id.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep_until(tokio::time::Instant::from_std(expires)).await;
        handle
            .state::<Archives>()
            .archives
            .lock()
            .unwrap()
            .remove(&expiry_id);
    });
    Ok(id)
}

impl Archives {
    // Retrieve the requested value
    fn get(&self, generation: u64, id: &str) -> Result<Arc<Archive>, String> {
        self.archives
            .lock()
            .unwrap()
            .get(id)
            .filter(|archive| archive.generation == generation && archive.expires > Instant::now())
            .cloned()
            .ok_or_else(|| "This ZIP has expired. Prepare the archive again.".into())
    }
}

// Copy verified
fn copy_verified(
    app: &AppHandle,
    generation: u64,
    source: &Path,
    hash: &str,
    size: u64,
) -> Result<String, String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != size || size > MAX_ARCHIVE {
        return Err("The source is no longer a regular ZIP of the expected size".into());
    }
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stream-dock-manager-export.zip");
    stage(app, generation, filename, |dest| {
        let mut input = File::open(source)
            .map_err(|error| error.to_string())?
            .take(size + 1);
        let mut output = File::create(dest).map_err(|error| error.to_string())?;
        io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        verify_file(dest, hash, size)
    })
}

// Resolve the export approved for a native drag operation
pub fn reviewed_export(
    app: &AppHandle,
    generation: u64,
    path: &Path,
    sha256: &str,
) -> Option<String> {
    if !supported() {
        return None;
    }
    let size = fs::symlink_metadata(path).ok()?.len();
    copy_verified(app, generation, path, sha256, size).ok()
}
// Handle the archive drag supported desktop command

#[tauri::command]
pub fn archive_drag_supported(_editor_work: EditorWork) -> bool {
    supported()
}
// Handle the prepare catalog archive drag desktop command

#[tauri::command]
pub async fn prepare_catalog_archive_drag(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    job: String,
) -> Result<String, String> {
    if !supported() {
        return Err("File dragging is unavailable on this desktop".into());
    }
    let generation = _editor_work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let selection = runtime
            .updates
            .downloaded_selection(&job)
            .ok_or("No verified catalog package is associated with this download")?;
        let package = selection.package();
        let (file, _source_lease) = runtime.updates.leased_completed_download(&job)?;
        copy_verified(&app, generation, &file, &package.sha256, package.size)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the review installed package export desktop command

#[tauri::command]
pub async fn review_installed_package_export(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
) -> Result<(String, Vec<String>), String> {
    let generation = _editor_work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let review = runtime
            .plugins
            .lock()
            .unwrap()
            .installed_package_export(&plugin_id)?;
        let files = review.files();
        let id = uuid::Uuid::new_v4().to_string();
        let state = app.state::<Archives>();
        let mut reviews = state.reviews.lock().unwrap();
        reviews.retain(|_, (owner, expires, _)| *owner == generation && *expires > Instant::now());
        if reviews.len() >= 4 {
            return Err("Close an existing plugin export review first".into());
        }
        reviews.insert(
            id.clone(),
            (
                generation,
                Instant::now() + RECEIPT_LIFETIME,
                Arc::new(review),
            ),
        );
        Ok((id, files))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the build installed package export desktop command

#[tauri::command]
pub async fn build_installed_package_export(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    review_id: String,
    selected: Vec<String>,
) -> Result<String, String> {
    let generation = _editor_work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let state = app.state::<Archives>();
        let review = state
            .reviews
            .lock()
            .unwrap()
            .get(&review_id)
            .filter(|(owner, expires, _)| *owner == generation && *expires > Instant::now())
            .map(|(_, _, review)| review.clone())
            .ok_or("The package review has expired")?;
        stage(
            &app,
            generation,
            &format!("{}.zip", review.id),
            |destination| {
                runtime
                    .plugins
                    .lock()
                    .unwrap()
                    .check_export_installation(&review)?;
                review.write(&selected, destination)?;
                runtime
                    .plugins
                    .lock()
                    .unwrap()
                    .check_export_installation(&review)
            },
        )
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the release archive export desktop command

#[tauri::command]
pub fn release_archive_export(
    _editor_work: EditorWork,
    app: AppHandle,
    id: String,
) -> Result<(), String> {
    let generation = _editor_work.generation()?;
    let state = app.state::<Archives>();
    let mut reviews = state.reviews.lock().unwrap();
    if reviews
        .get(&id)
        .is_some_and(|(owner, _, _)| *owner == generation)
    {
        reviews.remove(&id);
    }
    drop(reviews);
    let mut archives = state.archives.lock().unwrap();
    if archives
        .get(&id)
        .is_some_and(|archive| archive.generation == generation)
    {
        archives.remove(&id);
    }
    Ok(())
}
// Handle the save installed package export desktop command

#[tauri::command]
pub async fn save_installed_package_export(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    receipt: String,
    path: String,
) -> Result<(), String> {
    let archive = app
        .state::<Archives>()
        .get(_editor_work.generation()?, &receipt)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let path = Path::new(&path);
        let parent = path
            .parent()
            .ok_or("Choose a ZIP destination")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if runtime.owns_export_destination(&parent) || parent.starts_with(archive._directory.path())
        {
            return Err("Choose a destination outside managed application data".into());
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .is_none_or(|s| !s.eq_ignore_ascii_case("zip"))
        {
            return Err("Choose a .zip file".into());
        }
        let target = parent.join(path.file_name().ok_or("Choose a ZIP filename")?);
        let mut output = NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
        let mut input = File::open(&archive.file).map_err(|error| error.to_string())?;
        io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        output
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        output.persist(target).map_err(|error| error.to_string())?;
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(target_os = "windows")]
// Require local drag file
fn require_local_drag_file(file: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::{Storage::FileSystem::GetDriveTypeW, System::WindowsProgramming::DRIVE_FIXED},
    };

    let mut root = file;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let root: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    // The shell drag backend requires a local drive
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_FIXED {
        return Err("File dragging requires local storage. Use Save ZIP or Show in folder.".into());
    }
    Ok(())
}

struct ActiveDrag(Arc<AtomicBool>, AtomicBool);
impl ActiveDrag {
    // Finish for the current operation
    fn finish(&self) -> bool {
        let first = !self.1.swap(true, Ordering::AcqRel);
        if first {
            self.0.store(false, Ordering::Release);
        }
        first
    }
}
impl Drop for ActiveDrag {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(target_os = "linux")]
// Start x11 drag
fn start_x11_drag(
    window: &WebviewWindow,
    file: &Path,
    finished: impl Fn() + 'static,
) -> Result<(), String> {
    use gtk::{gdk, gdk_pixbuf::Pixbuf, glib, prelude::*};
    use std::{cell::RefCell, rc::Rc};

    let window = window.gtk_window().map_err(|error| error.to_string())?;
    let surface = window.window().ok_or("The editor window is unavailable")?;
    let pointer = surface
        .display()
        .default_seat()
        .and_then(|seat| seat.pointer())
        .ok_or("File dragging requires a pointer")?;
    if !surface
        .device_position(&pointer)
        .3
        .contains(gdk::ModifierType::BUTTON1_MASK)
    {
        return Ok(());
    }
    let uri = glib::filename_to_uri(file, None).map_err(|error| error.to_string())?;
    let targets = gtk::TargetList::new(&[gtk::TargetEntry::new(
        "text/uri-list",
        gtk::TargetFlags::empty(),
        0,
    )]);
    // GTK expects the initiating button number and requests file data before emitting drag-end
    let context = window
        .drag_begin_with_coordinates(&targets, gdk::DragAction::COPY, 1, None, -1, -1)
        .ok_or("Could not start file dragging. Use Save ZIP or Show in folder.")?;
    if let Ok(icon) = Pixbuf::from_read(&include_bytes!("../icons/32x32.png")[..]) {
        context.drag_set_icon_pixbuf(&icon, 0, 0);
    }

    let data_handler = window.connect_drag_data_get(move |_, _, data, _, _| {
        data.set_uris(&[&uri]);
    });
    // Suppress the snap-back animation so Escape releases the native drag immediately
    let failed_handler = window.connect_drag_failed(|_, _, _| glib::Propagation::Stop);
    let handlers = Rc::new(RefCell::new(None));
    let handlers_for_end = handlers.clone();
    let end_handler = window.connect_drag_end(move |window, _| {
        if let Some((data, failed, end)) = handlers_for_end.borrow_mut().take() {
            window.disconnect(data);
            window.disconnect(failed);
            window.disconnect(end);
        }
        finished();
    });
    handlers.replace(Some((data_handler, failed_handler, end_handler)));
    Ok(())
}
// Handle the start archive drag desktop command

#[tauri::command]
pub async fn start_archive_drag(
    _editor_work: EditorWork,
    window: WebviewWindow,
    receipt: String,
) -> Result<(), String> {
    if window.label() != "main" || !supported() {
        return Err("File dragging is unavailable on this desktop".into());
    }
    let app = window.app_handle().clone();
    let archive = app
        .state::<Archives>()
        .get(_editor_work.generation()?, &receipt)?;
    #[cfg(target_os = "windows")]
    require_local_drag_file(&archive.file)?;
    let dragging = app.state::<Archives>().dragging.clone();
    if dragging.swap(true, Ordering::AcqRel) {
        return Err("A file drag is already active".into());
    }
    let active = ActiveDrag(dragging, AtomicBool::new(false));
    editor_lifecycle::on_main_thread(&app, move |_| {
        let file = archive.file.clone();
        let finished = move || {
            if active.finish() {
                // Some receivers read the source just after the drop callback
                let archive = archive.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(archive);
                });
            }
        };
        #[cfg(target_os = "linux")]
        return start_x11_drag(&window, &file, finished);
        #[cfg(not(target_os = "linux"))]
        drag::start_drag(
            &window,
            drag::DragItem::Files(vec![file]),
            drag::Image::Raw(include_bytes!("../icons/32x32.png").to_vec()),
            move |_, _| finished(),
            Default::default(),
        )
        .map_err(|_| "Could not start file dragging. Use Save ZIP or Show in folder.".into())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify archive receipts expire and are scoped to the editor generation
    fn archive_receipts_expire_and_are_scoped_to_the_editor_generation() {
        let state = Archives::default();
        let expires = Instant::now() + RECEIPT_LIFETIME;
        for (id, expires) in [("valid", expires), ("stale", Instant::now())] {
            let directory = tempfile::tempdir().unwrap();
            let file = directory.path().join("fixture.zip");
            let archive = Archive {
                _directory: directory,
                _slot: state.slots.clone().try_acquire_owned().unwrap(),
                file,
                generation: 4,
                expires,
            };
            state
                .archives
                .lock()
                .unwrap()
                .insert(id.into(), Arc::new(archive));
        }
        assert!(state.get(4, "valid").is_ok());
        assert!(state.get(5, "valid").is_err());
        assert!(state.get(4, "stale").is_err());
        assert!(state.get(4, "unknown").is_err());
    }
}
