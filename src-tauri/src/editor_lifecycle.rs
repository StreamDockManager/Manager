//! Main-window lifetime. Runtime, device output and auxiliary windows are independent
use std::sync::{atomic::Ordering, Arc, Mutex, MutexGuard};
use std::time::Duration;
use stream_dock_manager_runtime::desktop::EditorRestoreState;
use stream_dock_manager_runtime::Runtime;
use tauri::{Emitter, Manager};

#[cfg(any(target_os = "macos", test))]
mod dock;
#[cfg(target_os = "macos")]
mod macos;

const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
const GENERATION_HEADER: &str = "x-stream-dock-manager-editor-generation";
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
struct Placement {
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
    scale: f64,
}
impl Placement {
    // Capture the current editor window state
    fn capture(window: &tauri::WebviewWindow) -> Option<Self> {
        // Only normal bounds update the cache; maximized/fullscreen presentation is
        // recorded separately and never prevents closing the editor.
        if window.is_maximized().ok()? || window.is_fullscreen().ok()? {
            return None;
        }
        let size = window.inner_size().ok()?;
        if size.width == 0 || size.height == 0 || size.width > 32768 || size.height > 32768 {
            return None;
        }
        let scale = window.scale_factor().ok()?;
        if !scale.is_finite() || scale <= 0.0 || scale > 16.0 {
            return None;
        }
        Some(Self {
            position: window.outer_position().ok()?,
            size,
            scale,
        })
    }
    // Read the editor window bounds
    fn bounds(self, macos: bool) -> (tauri::Position, tauri::Size) {
        if macos {
            // Cocoa global coordinates are logical points; the new window initially
            // inherits the primary display's scale, not the saved display's scale.
            (
                self.position.to_logical::<f64>(self.scale).into(),
                self.size.to_logical::<f64>(self.scale).into(),
            )
        } else {
            // Windows converts Logical coordinates using the new window's current DPI
            // Keep the recorded desktop pixels and move before applying the saved size
            (self.position.into(), self.size.into())
        }
    }
    // Check whether saved window bounds remain visible on a display
    fn reachable(self, monitors: &[tauri::Monitor]) -> bool {
        let center = (
            i64::from(self.position.x) + i64::from(self.size.width) / 2,
            i64::from(self.position.y) + i64::from(self.size.height) / 2,
        );
        monitors.iter().any(|monitor| {
            let origin = monitor.position();
            let size = monitor.size();
            center.0 >= i64::from(origin.x)
                && center.1 >= i64::from(origin.y)
                && center.0 < i64::from(origin.x) + i64::from(size.width)
                && center.1 < i64::from(origin.y) + i64::from(size.height)
        })
    }
    #[cfg(target_os = "macos")]
    // Configure for the current operation
    fn configure(self, app: &tauri::AppHandle, config: &mut tauri::utils::config::WindowConfig) {
        let size = self.size.to_logical::<f64>(self.scale);
        config.width = size.width;
        config.height = size.height;
        if app
            .available_monitors()
            .is_ok_and(|monitors| self.reachable(&monitors))
        {
            let position = self.position.to_logical::<f64>(self.scale);
            config.x = Some(position.x);
            config.y = Some(position.y);
            config.center = false;
        } else {
            config.x = None;
            config.y = None;
            config.center = true;
        }
    }
    // Apply for the current operation
    fn apply(self, window: &tauri::WebviewWindow) {
        let (position, size) = self.bounds(cfg!(target_os = "macos"));
        if window
            .available_monitors()
            .is_ok_and(|monitors| self.reachable(&monitors))
        {
            let _ = window.set_position(position);
            let _ = window.set_size(size);
        } else {
            let _ = window.set_size(size);
            let _ = window.center();
        }
    }
}

#[derive(Debug)]
struct Owner {
    generation: u64,
    fenced: bool,
    present: bool,
    creating: bool,
    destroying: bool,
    pending_reveal: bool,
    ready: bool,
    ever_ready: bool,
    active: usize,
    placement: Option<Placement>,
    #[cfg(target_os = "macos")]
    pending_normal: Option<Placement>,
    maximized: bool,
    fullscreen: bool,
    pending_quit: bool,
    restart_recovery: Option<bool>,
    restart_native_watch: bool,
    pending_close: bool,
    next_action: u64,
    pending_action: Option<(u64, bool)>,
    confirmed_close: Option<u64>,
    draining: bool,
    quit_drain: Option<u64>,
    tray_available: bool,
}
impl Default for Owner {
    // Construct the default value
    fn default() -> Self {
        Self {
            generation: 1,
            fenced: false,
            present: true,
            creating: false,
            destroying: false,
            pending_reveal: false,
            ready: false,
            ever_ready: false,
            active: 0,
            placement: None,
            #[cfg(target_os = "macos")]
            pending_normal: None,
            maximized: false,
            fullscreen: false,
            pending_quit: false,
            restart_recovery: None,
            restart_native_watch: false,
            pending_close: false,
            next_action: 0,
            pending_action: None,
            confirmed_close: None,
            draining: false,
            quit_drain: None,
            tray_available: true,
        }
    }
}
impl Owner {
    // Admit for the current operation
    fn admit(&self, generation: u64, command: &str) -> Result<(), String> {
        if generation != self.generation || !self.present {
            return Err("This editor document has expired".into());
        }
        if self.fenced && !cleanup(command) {
            return Err("The editor is releasing its window; try again after reopening it".into());
        }
        Ok(())
    }
    // Reveal for the current operation
    fn reveal(&mut self) -> bool {
        if self.destroying || self.confirmed_close.is_some() {
            self.pending_reveal = true;
            return false;
        }
        if self.quit_drain.take().is_some() {
            self.fenced = false;
        }
        self.pending_action = None;
        self.pending_quit = false;
        self.pending_close = false;
        self.restart_recovery = None;
        self.restart_native_watch = false;
        true
    }
    // Check whether this operation can quit without flush
    fn can_quit_without_flush(&self) -> bool {
        !self.present || !self.ever_ready
    }
    // Check whether the current lifecycle can accept a close request
    fn accepts_close_request(&self) -> bool {
        self.present
            && !self.destroying
            && self.confirmed_close.is_none()
            && self.quit_drain.is_none()
            && self.pending_action.is_none()
    }
    // Begin action
    fn begin_action(&mut self, quit: bool) -> Option<u64> {
        // Repeated native close/quit requests share their existing decision
        if let Some((id, kind)) = self.pending_action {
            if kind == quit {
                return Some(id);
            }
        }
        self.next_action = self
            .next_action
            .checked_add(1)
            .filter(|n| *n <= MAX_SEQUENCE)?;
        self.pending_action = Some((self.next_action, quit));
        Some(self.next_action)
    }
    // Validate action
    fn validate_action(&self, generation: u64, action: u64, quit: bool) -> Result<(), String> {
        self.admit(generation, "finish_window_action")?;
        if self.pending_action != Some((action, quit)) {
            return Err("Window action was superseded".into());
        }
        Ok(())
    }
    // Apply the user’s editor-close confirmation
    fn confirm_close(&mut self, generation: u64, action: u64) -> Result<(), String> {
        self.validate_action(generation, action, false)?;
        if !self.tray_available {
            return Err("The system tray is unavailable. Keep the editor open or use Quit.".into());
        }
        if self.confirmed_close.is_some() {
            return Err("This window close has already been confirmed".into());
        }
        self.confirmed_close = Some(action);
        self.ready = false;
        Ok(())
    }
    // Start drain
    fn start_drain(&mut self, generation: u64, action: u64) -> Result<(), String> {
        self.validate_action(generation, action, false)?;
        if self.confirmed_close != Some(action) || self.draining {
            return Err("Window close is not awaiting completion".into());
        }
        self.draining = true;
        self.fenced = true;
        Ok(())
    }
    // Check whether this operation can commit
    fn can_commit(&self, generation: u64, action: u64) -> bool {
        self.generation == generation
            && self.present
            && !self.destroying
            && self.pending_action == Some((action, false))
            && self.confirmed_close == Some(action)
            && self.draining
            && self.fenced
            && self.active == 0
            && self.tray_available
    }
    // Finish action
    fn finish_action(&mut self, generation: u64, action: u64, quit: bool) -> Result<(), String> {
        self.validate_action(generation, action, quit)?;
        if !quit {
            return Err("Closing the editor requires confirmed teardown".into());
        }
        // Retain the frontend ticket while native quit cleanup runs so an exact
        // abort can still cancel this quit, including its pending restart intent.
        Ok(())
    }
    // Begin quit drain
    fn begin_quit_drain(&mut self) -> Result<Option<u64>, String> {
        if self.quit_drain.is_some() {
            return Ok(None);
        }
        let token = self
            .next_action
            .checked_add(1)
            .filter(|n| *n <= MAX_SEQUENCE)
            .ok_or("Cannot allocate a native quit cleanup ticket")?;
        self.next_action = token;
        self.quit_drain = Some(token);
        self.pending_quit = true;
        self.fenced = true;
        if self.pending_action.is_some_and(|(_, quit)| !quit) {
            self.pending_action = None;
        }
        Ok(Some(token))
    }
    // Check that the token still identifies the fenced pending quit drain
    fn current_quit_drain(&self, token: u64) -> bool {
        self.quit_drain == Some(token) && self.pending_quit && self.fenced
    }
    // Check whether this operation can commit quit
    fn can_commit_quit(&self, token: u64, blocked: bool) -> bool {
        // Passive workspace/surface subscriptions hold EditorWork until shutdown
        // They are not scratch cleanup and must not make every Quit time out
        self.current_quit_drain(token) && !blocked
    }
    // Cancel quit drain
    fn cancel_quit_drain(&mut self, token: u64) -> bool {
        if !self.current_quit_drain(token) {
            return false;
        }
        self.quit_drain = None;
        self.pending_action = None;
        self.pending_quit = false;
        self.pending_close = false;
        self.restart_recovery = None;
        self.restart_native_watch = false;
        self.fenced = false;
        true
    }
    // Cancel the pending lifecycle transition
    fn abort_action(&mut self, generation: u64, action: u64) -> Result<bool, String> {
        self.admit(generation, "abort_window_action")?;
        if self.pending_action.map(|(id, _)| id) != Some(action) {
            return Err("Window action was superseded".into());
        }
        let quit_draining = self.quit_drain.take().is_some();
        let was_confirmed = self.confirmed_close.take().is_some() || self.draining || quit_draining;
        self.pending_action = None;
        self.pending_quit = false;
        self.pending_close = false;
        self.pending_reveal = false;
        self.restart_recovery = None;
        self.restart_native_watch = false;
        self.draining = false;
        self.fenced = false;
        Ok(was_confirmed)
    }
    // Begin creation
    fn begin_creation(&mut self) -> Option<u64> {
        if self.creating
            || self.destroying
            || self.confirmed_close.is_some()
            || self.quit_drain.is_some()
        {
            return None;
        }
        self.generation = self
            .generation
            .checked_add(1)
            .filter(|n| *n <= MAX_SEQUENCE)?;
        self.creating = true;
        self.ready = false;
        self.ever_ready = false;
        self.present = true;
        #[cfg(target_os = "macos")]
        {
            self.pending_normal = if self.fullscreen {
                self.placement
            } else {
                None
            };
        }
        Some(self.generation)
    }
    #[cfg(target_os = "macos")]
    // Restore normal window state after leaving fullscreen
    fn normal_after_fullscreen(&mut self, fullscreen: Option<bool>) -> Option<Placement> {
        // Called after the native did-exit notification, never solely because
        // Tao's requested fullscreen bit changed during an animation
        if fullscreen != Some(false) {
            return None;
        }
        let placement = self.pending_normal.take()?;
        self.fullscreen = false;
        self.maximized = false;
        self.placement = Some(placement);
        Some(placement)
    }
}
// Clean up resources owned by this lifecycle
fn cleanup(command: &str) -> bool {
    matches!(
        command,
        "cancel_keyboard_macro_recording"
            | "cancel_keyboard_macro_test"
            | "keyboard_macro_test_status"
            | "stop_keyboard_macro_recording"
            | "cancel_vendor_profile_review"
            | "finish_keyboard_preview"
            | "release_archive_export"
            | "discard_media_drop"
            | "discard_archive_drop"
            | "vault_cancel_entry_import"
            | "save_draft"
            | "unsubscribe_workspace"
            | "unsubscribe_surfaces"
            | "unsubscribe_plugin_logs"
            | "release_media"
            | "finish_window_action"
            | "abort_window_action"
    )
}
// Recognize commands that participate in editor lifecycle coordination
fn lifecycle(command: &str) -> bool {
    matches!(
        command,
        "editor_restore_state"
            | "frontend_ready"
            | "confirm_window_close"
            | "finish_window_action"
            | "abort_window_action"
    )
}
#[derive(Default)]
pub struct EditorLifecycle {
    owner: Mutex<Owner>,
    idle: tokio::sync::Notify,
    #[cfg(target_os = "macos")]
    dock: dock::Controller,
}

// Do not route an alert into a closing or not-yet-subscribed editor
pub fn accepts_plugin_alerts(app: &tauri::AppHandle) -> bool {
    app.try_state::<Arc<EditorLifecycle>>()
        .and_then(|core| {
            core.owner
                .lock()
                .ok()
                .map(|state| state.ready && state.present && !state.fenced && !state.destroying)
        })
        .unwrap_or(false)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuitDrainOutcome {
    Ready,
    TimedOut,
    Superseded,
}
impl EditorLifecycle {
    // Start close drain
    fn start_close_drain(
        &self,
        runtime: &Runtime,
        generation: u64,
        action: u64,
    ) -> Result<(), String> {
        self.owner.lock().unwrap().start_drain(generation, action)?;
        // The current page cannot release handles lost by an earlier page reload
        // Admission is fenced first; signal every native reader without dropping its
        // worker permit or treating cancellation as an acknowledgement of retirement.
        runtime.retire_editor_readers();
        runtime.vault.cancel_editor_entry_import(generation);
        Ok(())
    }

    // Finish action
    pub fn finish_action(&self, generation: u64, action: u64, quit: bool) -> Result<(), String> {
        self.owner
            .lock()
            .unwrap()
            .finish_action(generation, action, quit)
    }
    // Wait for quit drain
    async fn wait_for_quit_drain(
        &self,
        token: u64,
        timeout: Duration,
        blocked: impl Fn() -> bool,
    ) -> QuitDrainOutcome {
        tokio::time::timeout(timeout, async {
            loop {
                let notified = self.idle.notified();
                {
                    let state = self.owner.lock().unwrap();
                    if !state.current_quit_drain(token) {
                        return QuitDrainOutcome::Superseded;
                    }
                    drop(state);
                    if !blocked() {
                        return QuitDrainOutcome::Ready;
                    }
                }
                tokio::select! {
                    _ = notified => {},
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {},
                }
            }
        })
        .await
        .unwrap_or(QuitDrainOutcome::TimedOut)
    }
    // Wait for drain
    async fn wait_for_drain(
        &self,
        generation: u64,
        action: u64,
        timeout: Duration,
        blocked: impl Fn() -> bool,
    ) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                let notified = self.idle.notified();
                {
                    let state = self.owner.lock().unwrap();
                    if state.generation != generation || state.confirmed_close != Some(action) {
                        return false;
                    }
                    if state.active == 0 {
                        drop(state);
                        if !blocked() {
                            return true;
                        }
                    }
                }
                // Cancellation replies may precede native restoration (for example
                // benchmark device restoration). Keep the fence while it settles.
                tokio::select! {
                    _ = notified => {},
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {},
                }
            }
        })
        .await
        .unwrap_or(false)
    }
}

/// Injected by Tauri, never deserialized from frontend arguments. The permit stays in the
/// command future or is moved into its detached/native worker until that worker retires.
pub struct EditorWork(Option<Arc<EditorLifecycle>>);

/// Keep the editor fence stable through a reviewed native commit
pub(crate) struct EditorCommit<'a> {
    _owner: MutexGuard<'a, Owner>,
}

impl EditorWork {
    // Ensure editable
    pub(crate) fn ensure_editable(&self) -> Result<(), String> {
        let core = self
            .0
            .as_ref()
            .ok_or("This command belongs to the Manager editor")?;
        let state = core.owner.lock().unwrap();
        if state.fenced || !state.present {
            return Err("The editor is closing; cancel the preview instead of applying".into());
        }
        Ok(())
    }
    // Return the current generation identifier
    pub(crate) fn generation(&self) -> Result<u64, String> {
        self.0
            .as_ref()
            .map(|core| core.owner.lock().unwrap().generation)
            .ok_or_else(|| "This command belongs to the Manager editor".into())
    }

    // Recheck only editor state here; native commit already holds its storage lock
    pub(crate) fn commit_for_generation(
        &self,
        generation: u64,
    ) -> Result<EditorCommit<'_>, String> {
        let owner = self
            .0
            .as_ref()
            .ok_or("vault_import_expired")?
            .owner
            .lock()
            .unwrap();
        if owner.generation != generation || !owner.present || owner.fenced {
            return Err("vault_import_expired".into());
        }
        Ok(EditorCommit { _owner: owner })
    }

    // Run the editor lifecycle worker
    pub(crate) fn worker(&self) -> Self {
        if let Some(core) = &self.0 {
            core.owner.lock().unwrap().active += 1;
        }
        Self(self.0.clone())
    }
}
impl Drop for EditorWork {
    // Release resources held by this owner
    fn drop(&mut self) {
        if let Some(core) = &self.0 {
            let mut state = core.owner.lock().unwrap();
            state.active -= 1;
            drop(state);
            core.idle.notify_one();
        }
    }
}
impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for EditorWork {
    // Translate the desktop command into a lifecycle request
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        if command.message.webview_ref().label() != "main" {
            return Ok(Self(None));
        }
        let app = command.message.webview_ref().app_handle();
        let core = app.state::<Arc<EditorLifecycle>>().inner().clone();
        let mut state = core.owner.lock().unwrap();
        if command.name == "editor_restore_state" {
            if !state.present {
                return Err("The editor document has expired".into());
            }
            return Ok(Self(None));
        }
        let generation = command
            .message
            .headers()
            .get(GENERATION_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value <= MAX_SEQUENCE)
            .ok_or_else(|| tauri::ipc::InvokeError::from("Missing editor document generation"))?;
        state
            .admit(generation, command.name)
            .map_err(tauri::ipc::InvokeError::from)?;
        if lifecycle(command.name) {
            return Ok(Self(None));
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or_else(|| tauri::ipc::InvokeError::from("Too many editor operations"))?;
        drop(state);
        Ok(Self(Some(core)))
    }
}

/// Injected runtime access with the same editor admission and worker lifetime as EditorWork
pub struct EditorRuntime {
    pub(crate) work: EditorWork,
    pub(crate) runtime: Arc<Runtime>,
}
impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for EditorRuntime {
    // Translate the desktop command into a lifecycle request
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let work = EditorWork::from_command(tauri::ipc::CommandItem { ..command })?;
        let runtime = tauri::State::<Arc<Runtime>>::from_command(tauri::ipc::CommandItem {
            key: "runtime",
            ..command
        })?
        .inner()
        .clone();
        Ok(Self { work, runtime })
    }
}

// Carry editor admission into blocking work until the task actually exits
pub fn spawn_blocking<F, T>(permit: &EditorWork, task: F) -> tauri::async_runtime::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = permit.worker();
    // Keep the worker permit alive inside the task so dropping its caller cannot release admission early
    tauri::async_runtime::spawn_blocking(move || {
        let _editor_work = permit;
        task()
    })
}

/// Preserve editor admission across blocking Result-returning work and flatten only
/// the task join error; the operation's own error remains unchanged.
pub async fn blocking_result<F, T>(permit: &EditorWork, task: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking(permit, task)
        .await
        .map_err(|e| e.to_string())?
}
// Handle the editor restore state desktop command

#[tauri::command]
pub fn editor_restore_state(app: tauri::AppHandle, _editor_work: EditorWork) -> EditorRestoreState {
    // Called once per JS document, before that document can subscribe. Pagehide IPC
    // is best-effort, so a reload must also retire the previous document's readers.
    let generation = app
        .state::<Arc<EditorLifecycle>>()
        .owner
        .lock()
        .unwrap()
        .generation;
    let runtime = app.state::<Arc<Runtime>>();
    runtime.retire_editor_readers();
    runtime.vault.cancel_editor_entry_import(generation);
    crate::archive_import::discard_upload(&app);
    EditorRestoreState {
        generation,
        restore: None,
    }
}

// Resume for the current operation
fn resume(app: &tauri::AppHandle, generation: u64) {
    let _ = app.emit_to(
        "main",
        "editor-resume",
        serde_json::json!({"generation":generation}),
    );
}
// Check whether the lifecycle operation was cancelled
fn cancelled(app: &tauri::AppHandle, generation: u64, action: u64) {
    let _ = app.emit_to(
        "main",
        "window-action-cancelled",
        serde_json::json!({"generation":generation,"action_id":action}),
    );
}
// Resolve whether the application should appear in the dock
fn dock_visibility(app: &tauri::AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    dock::request(app, visible);
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}
// Show error
fn show_error(app: &tauri::AppHandle, message: impl Into<String>) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    app.dialog()
        .message(message.into())
        .title("Stream Dock Manager")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

// Install native quit
pub fn install_native_quit(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    macos::install(app)?;
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

/// Initialize the configured lazy window only after the native tray is installed
pub fn start(app: &tauri::AppHandle, visible: bool, tray_available: bool) {
    {
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        state.present = false;
        state.generation = 0;
        state.tray_available = tray_available;
    }
    if visible || !tray_available {
        reveal(app);
    } else {
        dock_visibility(app, false);
    }
}

/// Cache normal geometry throughout the window's life, before maximize/fullscreen
pub fn track_placement(window: &tauri::WebviewWindow) {
    let placement = Placement::capture(window);
    let maximized = window.is_maximized().ok();
    let fullscreen = window.is_fullscreen().ok();
    let core = window.app_handle().state::<Arc<EditorLifecycle>>();
    let mut state = core.owner.lock().unwrap();
    if state.creating || state.destroying {
        return;
    }
    #[cfg(target_os = "macos")]
    if state.pending_normal.is_some() && fullscreen == Some(false) {
        // The requested bit clears before Cocoa finishes exit. Keep normal
        // bounds until the native did-exit notification restores them.
        state.fullscreen = false;
        state.maximized = false;
        return;
    }
    if let Some(placement) = placement {
        state.placement = Some(placement);
    }
    if let Some(maximized) = maximized {
        state.maximized = maximized;
    }
    if let Some(fullscreen) = fullscreen {
        state.fullscreen = fullscreen;
    }
}

#[cfg(target_os = "macos")]
// Restore normal after fullscreen
fn restore_normal_after_fullscreen(app: &tauri::AppHandle, generation: u64, origin: usize) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.ns_window().ok().map(|pointer| pointer as usize) != Some(origin)
        || window.is_fullscreen().ok() != Some(false)
    {
        return;
    }
    let core = app.state::<Arc<EditorLifecycle>>();
    let mut state = core.owner.lock().unwrap();
    if state.generation != generation || !state.present || state.creating || state.destroying {
        return;
    }
    let Some(normal) = state.normal_after_fullscreen(Some(false)) else {
        return;
    };
    drop(state);
    // Queue after Tao's own exit handlers. Zoom and frame writes retain their
    // native order, and no lifecycle lock is held during AppKit callbacks.
    let _ = window.unmaximize();
    normal.apply(&window);
}

/// Serialize ticket checks and native effects with reveal
pub async fn on_main_thread<T: Send + 'static>(
    app: &tauri::AppHandle,
    action: impl FnOnce(tauri::AppHandle) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(action(handle));
    })
    .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

// Complete quit
pub fn complete_quit(app: &tauri::AppHandle) {
    let core = app.state::<Arc<EditorLifecycle>>().inner().clone();
    let ticket = core.owner.lock().unwrap().begin_quit_drain();
    let token = match ticket {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(error) => {
            #[cfg(target_os = "macos")]
            macos::reply(false);
            show_error(app, error);
            return;
        }
    };
    app.state::<crate::commands::EditorReadiness>()
        .0
        .store(false, Ordering::SeqCst);
    // Current invocation path: Tauri 2.12 ipc::protocol -> Webview::on_message ->
    // run_invoke_handler -> tauri-macros 2.7 body_blocking calls these synchronous
    // scratch commands inline in Wry 0.57's native UI callback (WK scheme/script,
    // WebView2 resource/message, or WebKitGTK scheme/script callback). Revisit
    // this ordering if those commands become async or the invocation path changes.
    // Fence before cancellation and recheck on the same UI thread before exit
    // Cancellation signals never join a live worker
    stream_dock_manager_runtime::keyboard_macro::recorder::cancel_all();
    stream_dock_manager_runtime::keyboard_macro::test_session::cancel_all();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = core
            .wait_for_quit_drain(
                token,
                DRAIN_TIMEOUT,
                || stream_dock_manager_runtime::keyboard_macro::test_session::editor_retirement_blocked() || stream_dock_manager_runtime::keyboard_macro::recorder::editor_retirement_blocked(),
            )
            .await;
        if result == QuitDrainOutcome::Superseded {
            return;
        }
        let _ = on_main_thread(&app, move |app| {
            let core = app.state::<Arc<EditorLifecycle>>();
            let state = core.owner.lock().unwrap();
            if !state.current_quit_drain(token) {
                return Ok(());
            }
            // Recheck after dispatch, even after a successful worker-side sample
            // Never join or hold a native service lock while waiting for cleanup
            let blocked =
                stream_dock_manager_runtime::keyboard_macro::test_session::editor_retirement_blocked() || stream_dock_manager_runtime::keyboard_macro::recorder::editor_retirement_blocked();
            let ready = state.can_commit_quit(token, blocked);
            drop(state);
            if ready {
                complete_drained_quit(&app, token);
            } else {
                fail_quit_drain(
                    &app,
                    token,
                    "Quit is waiting for keyboard macro cleanup. Manager remains open. Try Quit again after cleanup finishes.".into(),
                );
            }
            Ok(())
        })
        .await;
    });
}

// Report a failed quit drain and preserve the running application
fn fail_quit_drain(app: &tauri::AppHandle, token: u64, error: String) {
    let core = app.state::<Arc<EditorLifecycle>>();
    let (generation, action, present, ready) = {
        let mut state = core.owner.lock().unwrap();
        let previous = (
            state.generation,
            state.pending_action,
            state.present,
            state.ready,
        );
        if !state.cancel_quit_drain(token) {
            return;
        }
        previous
    };
    core.idle.notify_one();
    app.state::<crate::commands::EditorReadiness>()
        .0
        .store(present && ready, Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    macos::reply(false);
    if let Some((action, _)) = action {
        cancelled(app, generation, action);
    }
    if present {
        show_current(app);
        resume(app, generation);
    }
    show_error(app, error);
}

/// Called only on the UI thread after this exact quit ticket has drained
fn complete_drained_quit(app: &tauri::AppHandle, token: u64) {
    let (restart, restart_native_watch) = {
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        (
            state.restart_recovery.take(),
            std::mem::take(&mut state.restart_native_watch),
        )
    };
    if let Some(recovery) = restart {
        if let Err(error) = app.state::<Arc<Runtime>>().request_restart(recovery) {
            fail_quit_drain(app, token, format!("Could not request restart: {error}"));
            return;
        }
    }
    let launcher_restart = if restart.is_some() || restart_native_watch {
        match crate::dev_restart::request() {
            Ok(false) if restart_native_watch => {
                fail_quit_drain(
                    app,
                    token,
                    "The native development launcher is unavailable".into(),
                );
                return;
            }
            Ok(requested) => requested,
            Err(error) => {
                fail_quit_drain(
                    app,
                    token,
                    format!("Could not request development restart: {error}"),
                );
                return;
            }
        }
    } else {
        false
    };
    crate::manager_shortcut::stop(app);
    #[cfg(target_os = "macos")]
    app.state::<Arc<EditorLifecycle>>().dock.cancel();
    app.state::<Arc<Runtime>>().shutdown();
    crate::manager_log::finish();
    if restart.is_some() && !launcher_restart {
        app.restart();
    }
    #[cfg(target_os = "macos")]
    if macos::reply(true) {
        return;
    }
    app.exit(0);
}
// Handle the confirm window close desktop command

#[tauri::command]
pub async fn confirm_window_close(
    app: tauri::AppHandle,
    generation: u64,
    action_id: u64,
    _editor_work: EditorWork,
) -> Result<(), String> {
    on_main_thread(&app, move |app| {
        app.state::<Arc<EditorLifecycle>>()
            .owner
            .lock()
            .unwrap()
            .confirm_close(generation, action_id)?;
        app.state::<crate::commands::EditorReadiness>()
            .0
            .store(false, Ordering::SeqCst);
        app.state::<Arc<Runtime>>().cancel_debug_sessions();
        Ok(())
    })
    .await
}
// Handle the abort window action desktop command

#[tauri::command]
pub async fn abort_window_action(
    app: tauri::AppHandle,
    generation: u64,
    action_id: u64,
    _editor_work: EditorWork,
) -> Result<(), String> {
    on_main_thread(&app, move |app| {
        let core = app.state::<Arc<EditorLifecycle>>();
        let restarting = core.owner.lock().unwrap().restart_recovery.is_some();
        let was_confirmed = core
            .owner
            .lock()
            .unwrap()
            .abort_action(generation, action_id)?;
        if restarting {
            stream_dock_manager_plugin_host::manager_diagnostics::recovery(
                stream_dock_manager_plugin_host::manager_diagnostics::RecoveryEvent::RestartCancelled,
                0,
            );
        }
        core.idle.notify_one();
        #[cfg(target_os = "macos")]
        {
            macos::cancel_action(generation, action_id);
            macos::reply(false);
        }
        if was_confirmed {
            show_current(&app);
            resume(&app, generation);
        }
        Ok(())
    })
    .await
}

// Show current
fn show_current(app: &tauri::AppHandle) {
    dock_visibility(app, true);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// All entry points serialize creation/reveal with destruction on the native UI thread
pub fn reveal(app: &tauri::AppHandle) {
    reveal_inner(app, None, false);
}

// Reveal with activation
pub fn reveal_with_activation(app: &tauri::AppHandle, token: Option<String>) {
    reveal_inner(app, token, true);
}

// Reveal inner
fn reveal_inner(app: &tauri::AppHandle, token: Option<String>, external: bool) {
    let app = app.clone();
    let dispatch = app.clone();
    let _ = dispatch.run_on_main_thread(move || {
        let core = app.state::<Arc<EditorLifecycle>>();
        let (previous, resumed_quit) = {
            let mut state = core.owner.lock().unwrap();
            if external && state.pending_quit { return; }
            #[cfg(target_os = "linux")]
            crate::manager_shortcut::remember_activation(&app, token);
            #[cfg(not(target_os = "linux"))]
            let _ = token;
            let previous = state.pending_action.map(|(action, _)| (state.generation, action));
            let resumed_quit = state.quit_drain.is_some().then_some((state.generation, state.present && state.ready));
            if !state.reveal() { return; }
            (previous, resumed_quit)
        };
        #[cfg(target_os = "macos")]
        macos::reply(false);
        if let Some((generation, action)) = previous { cancelled(&app, generation, action); }
        if let Some((generation, ready)) = resumed_quit {
            core.idle.notify_one();
            app.state::<crate::commands::EditorReadiness>().0.store(ready, Ordering::SeqCst);
            resume(&app, generation);
        }
        dock_visibility(&app, true);
        let window = if let Some(window) = app.get_webview_window("main") { window } else {
            let generation = {
                let mut state = core.owner.lock().unwrap();
                let Some(generation) = state.begin_creation() else { return; };
                generation
            };
            app.state::<crate::commands::EditorReadiness>().0.store(false, Ordering::SeqCst);
            let (placement, maximized, fullscreen) = {
                let state = core.owner.lock().unwrap();
                (state.placement, state.maximized, state.fullscreen)
            };
            let config = app.config().app.windows.iter().find(|window| window.label == "main").cloned();
            #[cfg(target_os = "macos")]
            let config = config.map(|mut config| {
                // Initial fullscreen uses Tao's supported retry while Cocoa is
                // leaving the old Space. A runtime request can fail during that
                // animation. Set normal bounds first, before native construction.
                if let Some(placement) = placement { placement.configure(&app, &mut config); }
                else { config.center = true; }
                config.fullscreen = fullscreen;
                // Cocoa ignores initial zoom on an initially hidden window
                // Non-fullscreen zoom is queued after construction, below
                config.maximized = false;
                config
            });
            let outcome = config.ok_or_else(|| "Main window configuration is missing".to_string())
                .and_then(|config| tauri::WebviewWindowBuilder::from_config(&app, &config)
                    .and_then(|builder| builder.build()).map_err(|error| error.to_string()));
            match outcome {
                Ok(window) => {
                    if let Some(runtime) = app.try_state::<Arc<Runtime>>() {
                        runtime.invalidate_resource_attribution();
                    }
                    #[cfg(target_os = "macos")]
                    if maximized && !fullscreen { let _ = window.maximize(); }
                    #[cfg(not(target_os = "macos"))]
                    {
                        if let Some(placement) = placement { placement.apply(&window); }
                        else { let _ = window.center(); }
                        if fullscreen { let _ = window.set_fullscreen(true); }
                        else if maximized { let _ = window.maximize(); }
                    }
                    core.owner.lock().unwrap().creating = false;
                    track_placement(&window);
                    crate::update_registration_display(&window);
                    window
                }
                Err(error) => {
                    let mut state = core.owner.lock().unwrap();
                    if state.generation == generation { state.present = false; state.creating = false; }
                    drop(state);
                    dock_visibility(&app, false);
                    crate::manager_log::failure("Could not create the editor window");
                    show_error(&app, format!("Could not reopen the Manager editor: {error}. Try Open Stream Dock Manager in the tray again."));
                    return;
                }
            }
        };
        if window.show().is_err() {
            crate::manager_log::warning("Could not show the editor window");
        }
        let _ = window.unminimize();
        #[cfg(target_os = "linux")]
        crate::manager_shortcut::activate(&app, &window);
        let _ = window.set_focus();
        let _ = window.emit("editor-visibility", true);
        crate::tray::deliver_editor_request(&app);
        crate::catalog_link::notify(&app);
        crate::archive_import::notify(&app);
    });
}

/// Tauri removed the old label, but its old window handlers still run after the
/// application callback. Keep reveal fenced until a later native UI turn so their
/// label-scoped Destroyed delivery/listener cleanup cannot reach the replacement.
pub fn destroyed(app: &tauri::AppHandle) {
    stream_dock_manager_runtime::keyboard_macro::recorder::cancel_owner("main");
    stream_dock_manager_runtime::keyboard_macro::test_session::cancel_owner("main");
    crate::manager_log::stage(crate::manager_log::Stage::EditorClosed);
    app.state::<Arc<Runtime>>().cancel_locate(None);
    app.state::<Arc<Runtime>>().cancel_hardware_preview(None);
    app.state::<Arc<Runtime>>()
        .cancel_plugin_palette_preview(None);
    let runtime = app.state::<Arc<Runtime>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.cancel_all_keyboard_previews());
    crate::archive_import::discard_upload(app);
    crate::media_drop::discard_uploads(app);
    let generation = {
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        state.present = false;
        state.destroying = true;
        state.ready = false;
        state.generation
    };
    app.state::<Arc<Runtime>>()
        .vault
        .cancel_editor_entry_import(generation);
    app.state::<crate::commands::EditorReadiness>()
        .0
        .store(false, Ordering::SeqCst);
    let app = app.clone();
    // run_on_main_thread executes inline when called by the UI thread. Dispatch
    // from the async worker to enqueue behind the remaining Destroyed handlers.
    tauri::async_runtime::spawn(async move {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || complete_destruction(&handle));
    });
}

// Complete destruction
fn complete_destruction(app: &tauri::AppHandle) {
    let core = app.state::<Arc<EditorLifecycle>>();
    let (reveal_requested, quit) = {
        let mut state = core.owner.lock().unwrap();
        state.present = false;
        state.destroying = false;
        state.ready = false;
        state.fenced = state.quit_drain.is_some();
        state.draining = false;
        state.confirmed_close = None;
        if state.quit_drain.is_none() {
            state.pending_action = None;
        }
        (
            std::mem::take(&mut state.pending_reveal),
            state.pending_quit,
        )
    };
    core.idle.notify_one();
    app.state::<crate::commands::EditorReadiness>()
        .0
        .store(false, Ordering::SeqCst);
    if quit {
        complete_quit(app);
    } else if reveal_requested {
        reveal(app);
    } else {
        dock_visibility(app, false);
    }
}

// Mark this editor generation ready and deliver deferred close or tray requests
pub fn ready(app: &tauri::AppHandle, generation: u64) -> Result<(), String> {
    let core = app.state::<Arc<EditorLifecycle>>();
    let (quit, close) = {
        let mut state = core.owner.lock().unwrap();
        state.admit(generation, "frontend_ready")?;
        if state.confirmed_close.is_some() {
            return Err("The editor is closing".into());
        }
        state.ready = true;
        state.ever_ready = true;
        (state.pending_quit, std::mem::take(&mut state.pending_close))
    };
    app.state::<crate::commands::EditorReadiness>()
        .0
        .store(true, Ordering::SeqCst);
    if quit {
        request_quit(app);
    } else if close {
        request_close(app);
    } else {
        crate::tray::deliver_editor_request(app);
        crate::catalog_link::notify(app);
        crate::archive_import::notify(app);
    }
    Ok(())
}

// Emit action
fn emit_action(app: &tauri::AppHandle, generation: u64, action_id: u64, quit: bool) {
    #[cfg(target_os = "macos")]
    if quit {
        macos::bind_action(generation, action_id);
    }
    let _ = app.emit_to(
        "main",
        "flush-drafts",
        serde_json::json!({"quit":quit,"generation":generation,"action_id":action_id}),
    );
}

// Request closure of the editor window
pub fn request_close(app: &tauri::AppHandle) {
    let app = app.clone();
    let dispatch = app.clone();
    let _ = dispatch.run_on_main_thread(move || {
        let core = app.state::<Arc<EditorLifecycle>>();
        {
            let state = core.owner.lock().unwrap();
            if !state.accepts_close_request() { return; }
            if !state.tray_available {
                drop(state);
                crate::manager_log::warning("Editor close requires an available tray");
                show_error(&app, "The system tray is unavailable. Keep the editor open or use Quit to stop the application.");
                return;
            }
        }
        // AppKit may reenter application callbacks; never hold the lifecycle lock
        #[cfg(target_os = "macos")]
        macos::reply(false);
        let mut state = core.owner.lock().unwrap();
        if !state.accepts_close_request() { return; }
        state.pending_quit = false;
        state.restart_native_watch = false;
        if state.ready {
            let generation = state.generation;
            let action = state.begin_action(false);
            drop(state);
            if let Some(action_id) = action { emit_action(&app, generation, action_id, false); }
        } else if !state.ever_ready {
            let generation = state.generation;
            let Some(action_id) = state.begin_action(false) else { return; };
            if state.confirm_close(generation, action_id).is_err() { return; }
            drop(state);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = finish_close(&handle, generation, action_id).await {
                    // An inert startup document has no frontend close coordinator to abort
                    let _ = on_main_thread(&handle, move |app| {
                        let _ = app.state::<Arc<EditorLifecycle>>().owner.lock().unwrap()
                            .abort_action(generation, action_id);
                        Ok(())
                    }).await;
                    show_error(&handle, error);
                }
            });
        } else {
            state.pending_close = true;
        }
    });
}

// Request an orderly application restart
pub fn request_restart(app: &tauri::AppHandle, recovery: bool) {
    stream_dock_manager_plugin_host::manager_diagnostics::recovery(
        if recovery {
            stream_dock_manager_plugin_host::manager_diagnostics::RecoveryEvent::RestartRecoveryRequested
        } else {
            stream_dock_manager_plugin_host::manager_diagnostics::RecoveryEvent::RestartNormalRequested
        },
        0,
    );
    let core = app.state::<Arc<EditorLifecycle>>();
    let superseded = {
        let mut state = core.owner.lock().unwrap();
        let previous = state.quit_drain.map(|token| {
            let previous = (
                state.generation,
                state.pending_action,
                state.present && state.ready,
            );
            state.cancel_quit_drain(token);
            previous
        });
        state.restart_recovery = Some(recovery);
        state.restart_native_watch = false;
        previous
    };
    if let Some((generation, action, ready)) = superseded {
        core.idle.notify_one();
        app.state::<crate::commands::EditorReadiness>()
            .0
            .store(ready, Ordering::SeqCst);
        if let Some((action, _)) = action {
            cancelled(app, generation, action);
        }
    }
    request_quit(app);
}

// Request an orderly application shutdown
pub fn request_quit(app: &tauri::AppHandle) {
    request_quit_with_native_watch(app, false);
}

#[cfg(all(debug_assertions, target_os = "macos"))]
pub(crate) fn request_native_watch_restart(app: &tauri::AppHandle) {
    request_quit_with_native_watch(app, true);
}

fn request_quit_with_native_watch(app: &tauri::AppHandle, native_watch: bool) {
    let app = app.clone();
    let dispatch = app.clone();
    let _ = dispatch.run_on_main_thread(move || {
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        // Do not convert a manual quit or an explicit recovery restart into a watch reload
        if native_watch && !state.pending_quit && state.restart_recovery.is_none() {
            state.restart_native_watch = true;
        }
        state.pending_quit = true;
        if state.confirmed_close.is_some() || state.destroying || state.quit_drain.is_some() {
            return;
        }
        if state.can_quit_without_flush() || app.get_webview_window("main").is_none() {
            drop(state);
            complete_quit(&app);
        } else if state.ready {
            if state.pending_action.is_some_and(|(_, quit)| quit) {
                return;
            }
            let previous = state.pending_action.map(|(id, _)| id);
            let generation = state.generation;
            let action = state.begin_action(true);
            drop(state);
            if let Some(previous) = previous {
                cancelled(&app, generation, previous);
            }
            if let Some(action_id) = action {
                emit_action(&app, generation, action_id, true);
            }
        }
    });
}

// Recover close
async fn recover_close(app: &tauri::AppHandle, generation: u64, action: u64) {
    let _ = on_main_thread(app, move |app| {
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        if state.generation != generation
            || state.pending_action != Some((action, false))
            || state.destroying
        {
            return Ok(());
        }
        state.present = true;
        state.confirmed_close = None;
        state.draining = false;
        state.fenced = false;
        state.pending_reveal = false;
        state.pending_quit = false;
        state.restart_native_watch = false;
        drop(state);
        core.idle.notify_one();
        #[cfg(target_os = "macos")]
        macos::reply(false);
        show_current(&app);
        resume(&app, generation);
        Ok(())
    })
    .await;
}

// Finish close
pub async fn finish_close(
    app: &tauri::AppHandle,
    generation: u64,
    action: u64,
) -> Result<(), String> {
    let core = app.state::<Arc<EditorLifecycle>>().inner().clone();
    on_main_thread(app, move |app| {
        app.state::<Arc<EditorLifecycle>>().start_close_drain(
            &app.state::<Arc<Runtime>>(),
            generation,
            action,
        )
    })
    .await?;
    let drained = core
        .wait_for_drain(generation, action, DRAIN_TIMEOUT, || {
            app.state::<Arc<Runtime>>().editor_retirement_blocked()
                || app.state::<crate::location::Requests>().pending()
        })
        .await;
    if !drained {
        let active = core.owner.lock().unwrap().active;
        let mut blockers = app.state::<Arc<Runtime>>().editor_retirement_blockers();
        if app.state::<crate::location::Requests>().pending() {
            blockers.push("location requests");
        }
        crate::manager_log::editor_close_blocked(active, &blockers);
        let mut pending = blockers
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        if active > 0 {
            pending.push(format!("native operations ({active})"));
        }
        let detail = if pending.is_empty() {
            String::new()
        } else {
            format!(" Still waiting for: {}.", pending.join(", "))
        };
        recover_close(app, generation, action).await;
        return Err(format!(
            "Editor operations have not finished.{detail} Try closing again once they complete."
        ));
    }
    let result = on_main_thread(app, move |app| {
        let window = app
            .get_webview_window("main")
            .ok_or("The editor window is unavailable")?;
        track_placement(&window);
        let core = app.state::<Arc<EditorLifecycle>>();
        let mut state = core.owner.lock().unwrap();
        if !state.can_commit(generation, action) {
            return Err("Window close was superseded".into());
        }
        if app.state::<Arc<Runtime>>().editor_retirement_blocked()
            || app.state::<crate::location::Requests>().pending()
        {
            return Err(
                "The editor still owns unfinished work. Finish or cancel it before closing.".into(),
            );
        }
        state.present = false;
        state.destroying = true;
        drop(state);
        let result = window.destroy().map_err(|error| error.to_string());
        if result.is_err() {
            crate::manager_log::failure("Could not destroy the editor window");
            let mut state = core.owner.lock().unwrap();
            state.present = true;
            state.destroying = false;
        }
        result
    })
    .await;
    if result.is_err() {
        recover_close(app, generation, action).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify pending quit cleanup times out without releasing admission or restart
    fn pending_quit_cleanup_times_out_without_releasing_admission_or_restart() {
        let core = EditorLifecycle::default();
        let token = {
            let mut state = core.owner.lock().unwrap();
            state.restart_recovery = Some(true);
            state.restart_native_watch = true;
            state.begin_quit_drain().unwrap().unwrap()
        };
        assert_eq!(
            tauri::async_runtime::block_on(core.wait_for_quit_drain(
                token,
                Duration::from_millis(1),
                || true,
            )),
            QuitDrainOutcome::TimedOut
        );
        let mut state = core.owner.lock().unwrap();
        assert!(state.current_quit_drain(token));
        assert!(!state.can_commit_quit(token, true));
        assert_eq!(state.restart_recovery, Some(true));
        for command in [
            "begin_keyboard_macro_test",
            "start_keyboard_macro_test",
            "apply_edits",
        ] {
            assert!(state.admit(1, command).is_err(), "{command}");
        }
        for command in ["cancel_keyboard_macro_test", "keyboard_macro_test_status"] {
            assert!(state.admit(1, command).is_ok(), "{command}");
        }
        assert!(state.cancel_quit_drain(token));
        assert!(state.admit(1, "apply_edits").is_ok());
        assert!(!state.pending_quit);
        assert_eq!(state.restart_recovery, None);
        assert!(!state.restart_native_watch);
    }

    #[test]
    // Verify completed quit cleanup requires current ticket and main thread recheck
    fn completed_quit_cleanup_requires_current_ticket_and_main_thread_recheck() {
        let core = EditorLifecycle::default();
        let token = {
            let mut state = core.owner.lock().unwrap();
            state.active = 2; // Passive subscriptions are released by normal shutdown.
            let token = state.begin_quit_drain().unwrap().unwrap();
            assert_eq!(state.begin_quit_drain().unwrap(), None);
            assert_eq!(state.next_action, token);
            token
        };
        let samples = std::cell::Cell::new(0);
        assert_eq!(
            tauri::async_runtime::block_on(core.wait_for_quit_drain(
                token,
                Duration::from_secs(1),
                || {
                    samples.set(samples.get() + 1);
                    samples.get() == 1
                },
            )),
            QuitDrainOutcome::Ready
        );
        assert!(samples.get() >= 2);
        let state = core.owner.lock().unwrap();
        assert!(state.can_commit_quit(token, false));
        assert!(!state.can_commit_quit(token, true));
        assert!(!state.can_commit_quit(token + 1, false));
    }

    #[test]
    // Verify superseded quit waiter cannot finish or cancel a new quit
    fn superseded_quit_waiter_cannot_finish_or_cancel_a_new_quit() {
        let core = EditorLifecycle::default();
        let (old, current) = {
            let mut state = core.owner.lock().unwrap();
            state.restart_recovery = Some(true);
            let old = state.begin_quit_drain().unwrap().unwrap();
            assert!(state.reveal());
            assert!(!state.fenced);
            assert_eq!(state.restart_recovery, None);
            state.restart_recovery = Some(false);
            let current = state.begin_quit_drain().unwrap().unwrap();
            assert!(current > old);
            assert!(!state.cancel_quit_drain(old));
            assert_eq!(state.restart_recovery, Some(false));
            (old, current)
        };
        assert_eq!(
            tauri::async_runtime::block_on(core.wait_for_quit_drain(
                old,
                Duration::from_secs(1),
                || false,
            )),
            QuitDrainOutcome::Superseded
        );
        let state = core.owner.lock().unwrap();
        assert!(!state.can_commit_quit(old, false));
        assert!(state.can_commit_quit(current, false));
    }

    #[test]
    // Verify confirmed quit retains exact abort ticket during native drain
    fn confirmed_quit_retains_exact_abort_ticket_during_native_drain() {
        let mut state = Owner::default();
        let action = state.begin_action(true).unwrap();
        state.finish_action(1, action, true).unwrap();
        let token = state.begin_quit_drain().unwrap().unwrap();
        assert_ne!(action, token);
        assert!(state.abort_action(1, token).is_err());
        assert!(state.current_quit_drain(token));
        assert!(state.abort_action(1, action).unwrap());
        assert!(!state.can_commit_quit(token, false));
        assert!(!state.fenced);
    }

    #[test]
    // Verify quit without editor or successful bootstrap still owns a drain ticket
    fn quit_without_editor_or_successful_bootstrap_still_owns_a_drain_ticket() {
        for present in [false, true] {
            let mut state = Owner {
                present,
                ..Owner::default()
            };
            assert!(state.can_quit_without_flush());
            let token = state.begin_quit_drain().unwrap().unwrap();
            assert!(!state.can_commit_quit(token, true));
            assert!(state.begin_creation().is_none());
            assert!(state.can_commit_quit(token, false));
            assert!(state.cancel_quit_drain(token));
        }
    }

    #[test]
    // Verify cancelled or superseded restarts do not leak into a later quit
    fn cancelled_or_superseded_restarts_do_not_leak_into_a_later_quit() {
        for recovery in [false, true] {
            let mut state = Owner {
                restart_recovery: Some(recovery),
                ..Default::default()
            };
            let action = state.begin_action(true).unwrap();
            assert!(state.abort_action(state.generation, action + 1).is_err());
            assert_eq!(state.restart_recovery, Some(recovery));
            state.abort_action(state.generation, action).unwrap();
            assert_eq!(state.restart_recovery, None);
            state.restart_recovery = Some(recovery);
            state.begin_action(true).unwrap();
            assert!(state.reveal());
            assert_eq!(state.restart_recovery, None);
        }
    }
    #[test]
    // Verify mixed scale restore uses desktop pixels on windows and points on macos
    fn mixed_scale_restore_uses_desktop_pixels_on_windows_and_points_on_macos() {
        let saved = Placement {
            position: tauri::PhysicalPosition::new(2000, 80),
            size: tauri::PhysicalSize::new(1440, 940),
            scale: 1.0,
        };
        let (position, size) = saved.bounds(false);
        assert_eq!(position.to_physical::<i32>(2.0), saved.position);
        assert_eq!(size.to_physical::<u32>(2.0), saved.size);
        let retina = Placement {
            position: tauri::PhysicalPosition::new(4000, 160),
            size: tauri::PhysicalSize::new(2880, 1880),
            scale: 2.0,
        };
        let (position, size) = retina.bounds(true);
        assert_eq!(
            position.to_logical::<f64>(1.0),
            tauri::LogicalPosition::new(2000.0, 80.0)
        );
        assert_eq!(
            size.to_logical::<f64>(1.0),
            tauri::LogicalSize::new(1440.0, 940.0)
        );
    }
    #[test]
    // Verify blocking result preserves values errors and releases admission
    fn blocking_result_preserves_values_errors_and_releases_admission() {
        let core = Arc::new(EditorLifecycle::default());
        core.owner.lock().unwrap().active = 1;
        let permit = EditorWork(Some(core.clone()));
        assert_eq!(
            tauri::async_runtime::block_on(blocking_result(&permit, || Ok(42))),
            Ok(42)
        );
        assert_eq!(
            tauri::async_runtime::block_on(blocking_result(&permit, || Err::<(), _>(
                "operation failed".into()
            ))),
            Err("operation failed".into())
        );
        let error =
            tauri::async_runtime::block_on(blocking_result(&permit, || -> Result<(), String> {
                panic!("worker panic")
            }))
            .unwrap_err();
        assert!(error.contains("panic"));
        assert_eq!(core.owner.lock().unwrap().active, 1);
        drop(permit);
        assert_eq!(core.owner.lock().unwrap().active, 0);
    }

    #[test]
    // Verify blocking worker survives abandoned await and releases only after completion
    fn blocking_worker_survives_abandoned_await_and_releases_only_after_completion() {
        let core = Arc::new(EditorLifecycle::default());
        core.owner.lock().unwrap().active = 1;
        let permit = EditorWork(Some(core.clone()));
        let (release, wait) = std::sync::mpsc::channel();
        let (started, selected) = std::sync::mpsc::channel();
        let worker = spawn_blocking(&permit, move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
        });
        selected.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(core.owner.lock().unwrap().active, 2);
        drop(permit);
        worker.abort();
        assert_eq!(core.owner.lock().unwrap().active, 1);
        release.send(()).unwrap();
        tauri::async_runtime::block_on(worker).unwrap();
        assert_eq!(core.owner.lock().unwrap().active, 0);
    }
    #[test]
    // Verify failed initial or recreated startup can quit but resume must flush
    fn failed_initial_or_recreated_startup_can_quit_but_resume_must_flush() {
        let mut state = Owner::default();
        assert!(state.can_quit_without_flush());
        state.ready = true;
        state.ever_ready = true;
        assert!(!state.can_quit_without_flush());
        state.ready = false;
        assert!(!state.can_quit_without_flush());
        state.present = false;
        assert!(state.can_quit_without_flush());
        state.begin_creation().unwrap();
        assert!(state.can_quit_without_flush());
    }
    #[test]
    // Verify owned async retirement outlives an abandoned IPC waiter
    fn owned_async_retirement_outlives_an_abandoned_ipc_waiter() {
        let core = Arc::new(EditorLifecycle::default());
        core.owner.lock().unwrap().active = 1;
        let permit = EditorWork(Some(core.clone()));
        let owned = permit.worker();
        tauri::async_runtime::block_on(async {
            let (release, wait) = tokio::sync::oneshot::channel();
            let (started, selected) = tokio::sync::oneshot::channel();
            let (retired, done) = tokio::sync::oneshot::channel();
            let operation = tauri::async_runtime::spawn(async move {
                let _editor_work = owned;
                started.send(()).unwrap();
                wait.await.unwrap();
                drop(_editor_work);
                retired.send(()).unwrap();
            });
            let ipc_waiter = tauri::async_runtime::spawn(operation);
            selected.await.unwrap();
            ipc_waiter.abort();
            let _ = ipc_waiter.await;
            drop(permit);
            assert_eq!(core.owner.lock().unwrap().active, 1);
            release.send(()).unwrap();
            done.await.unwrap();
            assert_eq!(core.owner.lock().unwrap().active, 0);
        });
    }
    #[test]
    // Verify actual command dispatch requires current document header
    fn actual_command_dispatch_requires_current_document_header() {
        let core = Arc::new(EditorLifecycle::default());
        let app = tauri::test::mock_builder()
            .manage(core.clone())
            .invoke_handler(tauri::generate_handler![crate::builtin_capabilities])
            .build(crate::desktop_context())
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |generation: Option<&str>| {
            let mut headers = tauri::http::HeaderMap::new();
            if let Some(generation) = generation {
                headers.insert(GENERATION_HEADER, generation.parse().unwrap());
            }
            tauri::test::get_ipc_response(
                &window,
                tauri::webview::InvokeRequest {
                    cmd: "builtin_capabilities".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(target_os = "windows") {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({})),
                    headers,
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
        };
        assert!(invoke(None).is_err());
        assert!(invoke(Some("0")).is_err());
        assert!(invoke(Some("1")).is_ok());
        assert_eq!(core.owner.lock().unwrap().active, 0);
        core.owner.lock().unwrap().fenced = true;
        assert!(invoke(Some("1")).is_err());
        core.owner.lock().unwrap().fenced = false;
        assert!(invoke(Some("1")).is_ok());
    }
    #[test]
    // Verify grouped runtime injection preserves admission and releases failed requests
    fn grouped_runtime_injection_preserves_admission_and_releases_failed_requests() {
        let core = Arc::new(EditorLifecycle::default());
        let app = tauri::test::mock_builder()
            .manage(core.clone())
            .invoke_handler(tauri::generate_handler![
                crate::commands::video_recording_frame
            ])
            .build(crate::desktop_context())
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |generation: Option<&str>, recording: serde_json::Value| {
            let mut headers = tauri::http::HeaderMap::new();
            if let Some(generation) = generation {
                headers.insert(GENERATION_HEADER, generation.parse().unwrap());
            }
            tauri::test::get_ipc_response(
                &window,
                tauri::webview::InvokeRequest {
                    cmd: "video_recording_frame".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(target_os = "windows") {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({
                        "recording": recording, "positionUs": 0
                    })),
                    headers,
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .expect_err("fixture request must fail")
        };
        let recording = serde_json::json!("invalid-recording");
        assert_eq!(
            invoke(None, recording.clone()),
            "Missing editor document generation"
        );
        assert!(invoke(Some("0"), recording.clone())
            .as_str()
            .unwrap()
            .contains("expired"));
        assert!(invoke(Some("1"), recording.clone())
            .as_str()
            .unwrap()
            .contains("state not managed for field `runtime`"));
        assert_eq!(core.owner.lock().unwrap().active, 0);

        let root = tempfile::tempdir().unwrap();
        let runtime =
            stream_dock_manager_runtime::test_support::headless(root.path().join("manager"))
                .unwrap();
        app.manage(runtime.clone());
        assert!(invoke(Some("1"), serde_json::json!(42))
            .as_str()
            .unwrap()
            .contains("recording"));
        assert_eq!(core.owner.lock().unwrap().active, 0);
        assert_eq!(invoke(Some("1"), recording.clone()), "invalid asset ID");
        assert_eq!(core.owner.lock().unwrap().active, 0);
        core.owner.lock().unwrap().fenced = true;
        assert_ne!(invoke(Some("1"), recording), "invalid asset ID");
        assert_eq!(core.owner.lock().unwrap().active, 0);
        runtime.shutdown();
    }
    #[test]
    // Verify selected native worker keeps its permit until retirement
    fn selected_native_worker_keeps_its_permit_until_retirement() {
        let core = Arc::new(EditorLifecycle::default());
        core.owner.lock().unwrap().active = 1;
        let permit = EditorWork(Some(core.clone()));
        let (selected, run) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _editor_work = permit;
            run.recv().unwrap();
        });
        assert_eq!(core.owner.lock().unwrap().active, 1);
        selected.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(core.owner.lock().unwrap().active, 0);
    }

    #[test]
    // Verify close requires confirmation and drained permits
    fn close_requires_confirmation_and_drained_permits() {
        let mut state = Owner {
            ready: true,
            ..Owner::default()
        };
        let action = state.begin_action(false).unwrap();
        assert!(state.start_drain(1, action).is_err());
        assert!(state.confirm_close(0, action).is_err());
        state.confirm_close(1, action).unwrap();
        assert!(state.confirm_close(1, action).is_err());
        assert!(state.admit(1, "cancel_media_preparation").is_ok());
        state.active = 1;
        state.start_drain(1, action).unwrap();
        assert!(!state.can_commit(1, action));
        assert!(state.admit(1, "apply_edits").is_err());
        assert!(state.admit(1, "abort_window_action").is_ok());
        state.active = 0;
        assert!(state.can_commit(1, action));
    }
    #[test]
    // Verify pending close cancels on reveal but confirmed close queues reopen
    fn pending_close_cancels_on_reveal_but_confirmed_close_queues_reopen() {
        let mut state = Owner::default();
        let cancelled = state.begin_action(false).unwrap();
        assert_eq!(state.begin_action(false), Some(cancelled));
        assert!(state.reveal());
        assert!(state.confirm_close(1, cancelled).is_err());
        let confirmed = state.begin_action(false).unwrap();
        state.confirm_close(1, confirmed).unwrap();
        assert!(!state.reveal());
        assert!(state.pending_reveal);
        assert_eq!(state.confirmed_close, Some(confirmed));
        assert!(state.begin_creation().is_none());
        state.start_drain(1, confirmed).unwrap();
        assert!(state.can_commit(1, confirmed));
        state.destroying = true;
        assert!(!state.reveal());
    }
    #[test]
    // Verify abort is exact and releases only current close
    fn abort_is_exact_and_releases_only_current_close() {
        let mut state = Owner::default();
        let action = state.begin_action(false).unwrap();
        state.confirm_close(1, action).unwrap();
        state.start_drain(1, action).unwrap();
        assert!(state.abort_action(0, action).is_err());
        assert!(state.abort_action(1, action + 1).is_err());
        assert!(state.abort_action(1, action).unwrap());
        assert!(!state.fenced && !state.draining);
        assert!(state.confirmed_close.is_none());
        assert!(state.reveal());
        assert!(state.admit(1, "apply_edits").is_ok());
    }
    #[test]
    // Verify absent startup creates once and action counters are bounded
    fn absent_startup_creates_once_and_action_counters_are_bounded() {
        let mut state = Owner {
            present: false,
            generation: 0,
            ..Owner::default()
        };
        assert!(state.admit(0, "save_draft").is_err());
        assert_eq!(state.begin_creation(), Some(1));
        assert!(state.begin_creation().is_none());
        state.creating = false;
        state.next_action = MAX_SEQUENCE;
        assert!(state.begin_action(false).is_none());
    }
    #[test]
    // Verify missing tray refuses close without mutating decision
    fn missing_tray_refuses_close_without_mutating_decision() {
        let mut state = Owner {
            tray_available: false,
            ..Owner::default()
        };
        let action = state.begin_action(false).unwrap();
        assert!(state.confirm_close(1, action).is_err());
        assert!(state.confirmed_close.is_none());
    }
    #[test]
    // Verify maximized and fullscreen presentation never block a confirmed close
    fn maximized_and_fullscreen_presentation_never_block_a_confirmed_close() {
        let mut state = Owner {
            maximized: true,
            fullscreen: true,
            placement: None,
            ..Owner::default()
        };
        let action = state.begin_action(false).unwrap();
        state.confirm_close(1, action).unwrap();
        state.start_drain(1, action).unwrap();
        assert!(state.can_commit(1, action));
        // Quit arriving during confirmed cleanup is serviced after destruction
        state.pending_quit = true;
        assert!(state.can_commit(1, action));
        assert!(!state.accepts_close_request());
    }
    #[cfg(target_os = "macos")]
    #[test]
    // Verify recreated fullscreen keeps normal bounds through entry retry until exit
    fn recreated_fullscreen_keeps_normal_bounds_through_entry_retry_until_exit() {
        let placement = Placement {
            position: tauri::PhysicalPosition::new(240, 180),
            size: tauri::PhysicalSize::new(2400, 1600),
            scale: 2.0,
        };
        let mut state = Owner {
            present: false,
            fullscreen: true,
            placement: Some(placement),
            ..Owner::default()
        };
        state.begin_creation().unwrap();
        assert!(state.normal_after_fullscreen(None).is_none());
        assert!(state.normal_after_fullscreen(Some(true)).is_none());
        assert!(state.pending_normal.is_some());
        let restored = state.normal_after_fullscreen(Some(false)).unwrap();
        assert_eq!(restored.position, placement.position);
        assert_eq!(restored.size, placement.size);
        assert!(!state.fullscreen && !state.maximized);
        assert!(state.normal_after_fullscreen(Some(false)).is_none());
    }
    #[test]
    // Verify reloaded document readers retire without the lost javascript handles
    fn reloaded_document_readers_retire_without_the_lost_javascript_handles() {
        let directory = tempfile::tempdir().unwrap();
        let runtime =
            stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
                .unwrap();
        struct Shutdown(Arc<Runtime>);
        impl Drop for Shutdown {
            // Release resources held by this owner
            fn drop(&mut self) {
                self.0.shutdown();
            }
        }
        let _shutdown = Shutdown(runtime.clone());
        let core = Arc::new(EditorLifecycle::default());
        let app = tauri::test::mock_builder()
            .manage(runtime.clone())
            .build(crate::desktop_context())
            .unwrap();
        let permit = || {
            core.owner.lock().unwrap().active += 1;
            EditorWork(Some(core.clone()))
        };

        // The old page disappeared without delivering either unsubscribe. A successful
        // channel send only enqueues JS evaluation; it cannot detect a lost JS callback.
        let (entered, started) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Mutex::new(Some(wait));
        let old_workspace = crate::commands::subscribe_workspace(
            permit(),
            app.state(),
            tauri::ipc::Channel::new(move |_| {
                if let Some(wait) = wait.lock().unwrap().take() {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            }),
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let old_preview = crate::commands::subscribe_surfaces(
            permit(),
            app.state(),
            "fixture".into(),
            tauri::ipc::Channel::new(|_| Ok(())),
        )
        .unwrap();

        // A replacement page can only unsubscribe its own newly acquired handles
        let current = crate::commands::subscribe_workspace(
            permit(),
            app.state(),
            tauri::ipc::Channel::new(|_| Ok(())),
        );
        crate::commands::unsubscribe_workspace(permit(), app.state(), current);
        assert!(runtime
            .workspace_cancellations
            .lock()
            .unwrap()
            .contains_key(&old_workspace));
        assert!(runtime
            .preview_subscriptions
            .is_canceled(&old_preview)
            .is_some());
        assert!(core.start_close_drain(&runtime, 1, 0).is_err());
        assert!(
            !runtime.workspace_cancellations.lock().unwrap()[&old_workspace].load(Ordering::SeqCst)
        );
        assert_eq!(
            runtime.preview_subscriptions.is_canceled(&old_preview),
            Some(false)
        );
        let action = {
            let mut state = core.owner.lock().unwrap();
            let action = state.begin_action(false).unwrap();
            state.confirm_close(1, action).unwrap();
            action
        };
        core.start_close_drain(&runtime, 1, action).unwrap();
        tauri::async_runtime::block_on(async {
            // Signalling cancellation must not pretend a still-running worker has exited
            assert!(
                !core
                    .wait_for_drain(1, action, Duration::from_millis(10), || {
                        runtime.editor_retirement_blocked()
                    })
                    .await
            );
            release.send(()).unwrap();
            assert!(
                core.wait_for_drain(1, action, Duration::from_secs(2), || {
                    runtime.editor_retirement_blocked()
                })
                .await,
                "Reloaded-page subscriptions still veto editor close"
            );
        });
        assert_eq!(core.owner.lock().unwrap().active, 0);
        assert!(core.owner.lock().unwrap().can_commit(1, action));
    }

    #[test]
    // Verify native drain waits for worker retirement and bounds failure
    fn native_drain_waits_for_worker_retirement_and_bounds_failure() {
        let core = Arc::new(EditorLifecycle::default());
        let action = {
            let mut state = core.owner.lock().unwrap();
            let action = state.begin_action(false).unwrap();
            state.confirm_close(1, action).unwrap();
            state.start_drain(1, action).unwrap();
            state.active = 1;
            action
        };
        let worker = EditorWork(Some(core.clone()));
        tauri::async_runtime::block_on(async {
            assert!(
                !core
                    .wait_for_drain(1, action, Duration::from_millis(5), || false)
                    .await
            );
            assert_eq!(core.owner.lock().unwrap().active, 1);
            drop(worker);
            assert!(
                core.wait_for_drain(1, action, Duration::from_millis(5), || false)
                    .await
            );
            let restoring = Arc::new(std::sync::atomic::AtomicBool::new(true));
            assert!(
                !core
                    .wait_for_drain(1, action, Duration::from_millis(5), || restoring
                        .load(Ordering::SeqCst))
                    .await
            );
            let completion = restoring.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                completion.store(false, Ordering::SeqCst);
            });
            assert!(
                core.wait_for_drain(1, action, Duration::from_millis(100), || restoring
                    .load(Ordering::SeqCst))
                    .await
            );
            assert!(core.owner.lock().unwrap().fenced);
            core.owner.lock().unwrap().abort_action(1, action).unwrap();
            assert!(
                !core
                    .wait_for_drain(1, action, Duration::from_millis(5), || false)
                    .await
            );
        });
    }
}
