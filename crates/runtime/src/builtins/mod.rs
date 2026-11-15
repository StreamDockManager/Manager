//! First-party action catalog, isolated run contexts, and cancellable execution
#[cfg(test)]
use std::path::PathBuf;
mod execution_inputs;
mod execution_state;
mod key_logic;
pub(crate) use key_logic::validate_plugins as validate_key_logic_plugins;
#[cfg(test)]
mod key_logic_tests;
mod persisted_values;
use execution_state::DebugStateLease;
#[cfg(test)]
mod cancellation_feedback_tests;
#[cfg(test)]
mod lifetime_tests;
pub(crate) mod native;
use execution_inputs::{ExecutionInputs, MacroLibrary};
pub use native::{condition_target_path, condition_targets};
#[cfg(test)]
mod ownership_tests;
mod simulation;
mod types;
use crate::{
    actions::{Action, Request},
    Runtime,
};
use hardware_core::Input;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub(crate) use native::command_result as bounded_command_result;
pub(crate) use native::{
    chord as send_shortcut, paste as paste_text, plugin_hold, plugin_release,
    set_clipboard as copy_text,
};
#[cfg(target_os = "linux")]
pub(crate) use native::{invalidate_input, wayland as on_wayland};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};
pub use types::*;

// Load the embedded built-in catalog once and exclude top-level-only entries
pub fn catalog() -> &'static [Value] {
    static CATALOG: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<Vec<Value>>(include_str!("../../../../src/builtin-catalog.json"))
            .expect("checked built-in catalog")
            .into_iter()
            .filter(|v| v["topLevel"] != true)
            .collect()
    })
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SavedMacro {
    pub id: String,
    pub name: String,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::macro_inputs::MacroInput>>")]
    pub inputs: Vec<crate::macro_inputs::MacroInput>,
}

// Require macro plugins
pub(crate) fn require_macro_plugins<'a>(
    action: &Action,
    lookup: impl Fn(&str) -> Option<&'a Action>,
    has_action: impl Fn(&str, &str) -> bool,
) -> Result<(), String> {
    // Visit the current builtins value
    fn visit<'a>(
        root: &Action,
        lookup: &impl Fn(&str) -> Option<&'a Action>,
        has_action: &impl Fn(&str, &str) -> bool,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        let mut result = Ok(());
        root.visit(&mut |action| {
            if result.is_err() {
                return;
            }
            match action {
                Action::Plugin {
                    plugin_id,
                    action_id,
                    ..
                } if !has_action(plugin_id, action_id) => {
                    result = Err(format!(
                        "Incompatible macro: install {plugin_id} with action {action_id}."
                    ));
                }
                Action::Builtin { action } => {
                    if let BuiltinAction::SavedMacro { macro_id, .. } = action.as_ref() {
                        if visited.insert(macro_id.clone()) {
                            if let Some(nested) = lookup(macro_id) {
                                result = visit(nested, lookup, has_action, visited);
                            }
                        }
                    }
                }
                _ => {}
            }
        });
        result
    }
    visit(action, &lookup, &has_action, &mut Default::default())
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredValue {
    value: Value,
    persist: bool,
}

// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let _: Option<BTreeMap<String, StoredValue>> =
        crate::profile_database::LocalPreference::BuiltinValues
            .snapshot_record()
            .read_from(connection)?;
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Capture {
    pub action: Action,
    pub scope: String,
    pub pressed: Arc<AtomicBool>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct EffectiveBinding {
    pub profile_id: String,
    pub page_id: String,
    pub binding: crate::Binding,
    pub scope: String,
}
#[derive(Clone)]
struct Layer {
    owner: String,
    profile: String,
    page: String,
    controls: Vec<String>,
}
type PendingReply = (Value, mpsc::SyncSender<Result<Option<String>, String>>);
pub struct State {
    pub(crate) debug_sessions: Mutex<HashMap<String, Arc<crate::macro_debug::Session>>>,
    values: Mutex<BTreeMap<String, StoredValue>>,
    storage: stream_dock_manager_storage::Storage,
    pub(crate) captures: Mutex<HashMap<String, Capture>>,
    indices: Mutex<HashMap<String, usize>>,
    cooldowns: Mutex<HashMap<String, Instant>>,
    taps: key_logic::TapWindows,
    lanes: Mutex<HashMap<String, Weak<Mutex<VecDeque<String>>>>>,
    history: Mutex<HashMap<String, Vec<(String, String)>>>,
    layers: Mutex<HashMap<String, Vec<Layer>>>,
    layer_revision: AtomicU64,
    pub(crate) paused: Mutex<HashMap<String, String>>,
    device_cancellations: Mutex<HashMap<String, Arc<AtomicU64>>>,
    pending: Mutex<HashMap<String, PendingReply>>,
    ui_lane: Mutex<()>,
    execution_inputs: Mutex<Option<ExecutionInputs>>,
    active: AtomicUsize,
    desktop_host: AtomicBool,
}
impl State {
    #[cfg(test)]
    // Initialize builtins state from the supplied inputs
    pub fn new(path: PathBuf) -> Self {
        Self::with_storage(
            stream_dock_manager_storage::Storage::open(path.parent().unwrap().to_path_buf())
                .unwrap(),
        )
        .unwrap()
    }
    // Construct using storage
    pub fn with_storage(storage: stream_dock_manager_storage::Storage) -> Result<Self, String> {
        let mut values: BTreeMap<String, StoredValue> =
            crate::profile_database::LocalPreference::BuiltinValues
                .record()
                .read(&storage)?
                .unwrap_or_default();
        values.retain(|name, _| name.starts_with("clipboard:"));
        Ok(Self {
            debug_sessions: Default::default(),
            values: Mutex::new(values),
            storage,
            captures: Default::default(),
            indices: Default::default(),
            cooldowns: Default::default(),
            taps: Default::default(),
            lanes: Default::default(),
            history: Default::default(),
            layers: Default::default(),
            layer_revision: AtomicU64::new(0),
            paused: Default::default(),
            device_cancellations: Default::default(),
            pending: Default::default(),
            ui_lane: Default::default(),
            execution_inputs: Default::default(),
            active: AtomicUsize::new(0),
            desktop_host: AtomicBool::new(false),
        })
    }
    // Store the builtins value
    fn store(&self, name: String, value: Value, persist: bool) -> Result<(), String> {
        self.mutate(name, persist, |_| Ok(value))
    }
    // Apply a mutation to builtins
    fn mutate(
        &self,
        name: String,
        persist: bool,
        update: impl FnOnce(Option<Value>) -> Result<Value, String>,
    ) -> Result<(), String> {
        let mut values = self.values.lock().unwrap();
        let next = update(values.get(&name).map(|v| v.value.clone()))?;
        let mut updated = values.clone();
        updated.insert(
            name,
            StoredValue {
                value: next,
                persist,
            },
        );
        self.commit_values(&mut values, updated)
    }
    // Commit values
    fn commit_values(
        &self,
        values: &mut BTreeMap<String, StoredValue>,
        updated: BTreeMap<String, StoredValue>,
    ) -> Result<(), String> {
        if serde_json::to_vec(&updated)
            .map_err(|e| e.to_string())?
            .len()
            > 4 * 1024 * 1024
        {
            return Err("Runtime values exceed 4 MiB".into());
        }
        let saved: BTreeMap<_, _> = updated.iter().filter(|(_, v)| v.persist).collect();
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?;
        if bytes.len() > persisted_values::MAX_FILE_BYTES {
            return Err("Stored builtin values exceed the 16 MiB file limit".into());
        }
        let record = crate::profile_database::LocalPreference::BuiltinValues
            .record::<BTreeMap<String, StoredValue>>();
        self.storage
            .write_json(record.domain, record.key, &saved, record.max_bytes)?;
        *values = updated;
        Ok(())
    }
    // Return the requested builtins value
    pub(crate) fn get(&self, name: &str) -> Option<Value> {
        self.values
            .lock()
            .unwrap()
            .get(name)
            .map(|v| v.value.clone())
    }
    // Read the user-interface request awaiting a response
    pub fn pending_ui(&self, id: &str) -> Option<Value> {
        self.pending.lock().unwrap().get(id).map(|(v, _)| v.clone())
    }
    // Complete the pending user-interface request with its response
    pub fn reply_ui(&self, id: &str, value: Option<String>) -> Result<(), String> {
        let (_, sender) = self
            .pending
            .lock()
            .unwrap()
            .remove(id)
            .ok_or("Prompt expired or cancelled")?;
        sender
            .send(Ok(value))
            .map_err(|_| "Prompt expired or cancelled".into())
    }
    // Attach desktop host
    pub fn attach_desktop_host(&self) {
        self.desktop_host.store(true, Ordering::SeqCst);
    }
    // Complete the pending user-interface request with an error
    pub fn fail_ui(&self, id: &str, error: String) {
        if let Some((_, sender)) = self.pending.lock().unwrap().remove(id) {
            let _ = sender.send(Err(error));
        }
    }
    // Resume the suspended builtins operation
    pub fn resume(&self, device: &str) {
        self.paused.lock().unwrap().remove(device);
    }
    // Cancel device
    pub fn cancel_device(&self, device: &str) {
        self.device_cancellations
            .lock()
            .unwrap()
            .entry(device.into())
            .or_default()
            .fetch_add(1, Ordering::SeqCst);
        self.cleanup(Some(device));
    }
    // Cancel key logic
    pub(crate) fn cancel_key_logic(&self, device: Option<&str>) {
        self.taps.cleanup(device);
    }
    // Clean up resources retained by builtins
    pub fn cleanup(&self, device: Option<&str>) {
        self.taps.cleanup(device);
        for (key, capture) in self.captures.lock().unwrap().iter() {
            if device.is_none_or(|d| key.starts_with(&format!("{d}|"))) {
                capture.pressed.store(false, Ordering::SeqCst);
            }
        }
        {
            let mut layers = self.layers.lock().unwrap();
            layers.retain(|d, _| device.is_some_and(|x| d != x));
            self.layer_revision.fetch_add(1, Ordering::SeqCst);
        }
        native::release_prefix(&device.map(|d| format!("{d}|")).unwrap_or_default());
    }
    // Read the generation identifying the current temporary binding layers
    pub(crate) fn layer_revision(&self) -> u64 {
        self.layer_revision.load(Ordering::SeqCst)
    }
    /// Caller holds runtime state; layer mutation never acquires runtime state
    /// while holding this lock. Only this device's active overrides are copied
    pub(crate) fn device_overrides(
        &self,
        snapshot: &crate::Snapshot,
        device: &str,
    ) -> (u64, BTreeMap<String, EffectiveBinding>) {
        let layers = self.layers.lock().unwrap();
        let mut result = BTreeMap::new();
        for layer in layers.get(device).into_iter().flatten() {
            if let Some(page) = snapshot
                .profiles
                .iter()
                .find(|p| p.id == layer.profile)
                .and_then(|p| p.pages.iter().find(|p| p.id == layer.page))
            {
                for (control, binding) in &page.bindings {
                    if layer.controls.is_empty() || layer.controls.contains(control) {
                        result.insert(
                            control.clone(),
                            EffectiveBinding {
                                profile_id: layer.profile.clone(),
                                page_id: layer.page.clone(),
                                binding: binding.clone(),
                                scope: format!("{}:{}", layer.profile, layer.page),
                            },
                        );
                    }
                }
            }
        }
        (self.layer_revision(), result)
    }
    // Compose effective binding overrides from each device’s active layer stack
    pub fn overrides(
        &self,
        snapshot: &crate::Snapshot,
    ) -> BTreeMap<String, BTreeMap<String, EffectiveBinding>> {
        let mut result = BTreeMap::new();
        let layers = self.layers.lock().unwrap();
        for (device, stack) in layers.iter() {
            let map = result.entry(device.clone()).or_insert_with(BTreeMap::new);
            for layer in stack {
                if let Some(page) = snapshot
                    .profiles
                    .iter()
                    .find(|p| p.id == layer.profile)
                    .and_then(|p| p.pages.iter().find(|p| p.id == layer.page))
                {
                    for (control, binding) in &page.bindings {
                        if layer.controls.is_empty() || layer.controls.contains(control) {
                            map.insert(
                                control.clone(),
                                EffectiveBinding {
                                    profile_id: layer.profile.clone(),
                                    page_id: layer.page.clone(),
                                    binding: binding.clone(),
                                    scope: format!("{}:{}", layer.profile, layer.page),
                                },
                            );
                        }
                    }
                }
            }
        }
        result
    }
    // Resolve effective binding
    pub(crate) fn effective_binding(
        &self,
        device: &str,
        control: &str,
        snapshot: &crate::Snapshot,
    ) -> Option<EffectiveBinding> {
        let layers = self.layers.lock().unwrap();
        for layer in layers.get(device).into_iter().flatten().rev() {
            if !layer.controls.is_empty() && !layer.controls.iter().any(|x| x == control) {
                continue;
            }
            if let Some(binding) = snapshot
                .profiles
                .iter()
                .find(|p| p.id == layer.profile)
                .and_then(|p| p.pages.iter().find(|p| p.id == layer.page))
                .and_then(|p| p.bindings.get(control))
            {
                return Some(EffectiveBinding {
                    profile_id: layer.profile.clone(),
                    page_id: layer.page.clone(),
                    binding: binding.clone(),
                    scope: format!("{}:{}", layer.profile, layer.page),
                });
            }
        }
        None
    }
    // Delegate native capability availability checks to the platform input backend
    pub fn capability(name: &str) -> Option<String> {
        native::capability(name)
    }
}
// Report the valid condition condition for builtins
fn valid_condition(condition: &Condition) -> bool {
    ["clipboard", "file", "process", "application"].contains(&condition.source.as_str())
        && [
            "equals",
            "not_equals",
            "contains",
            "exists",
            "missing",
            "greater",
            "less",
        ]
        .contains(&condition.operator.as_str())
        && (condition.value.is_string()
            || condition.value.is_number()
            || condition.value.is_boolean())
}
// Validate the supplied builtins value
pub fn validate(action: &BuiltinAction, depth: usize) -> Result<(), String> {
    validate_with_draft(action, depth, false)
}
// Validate draft
pub(crate) fn validate_draft(action: &BuiltinAction, depth: usize) -> Result<(), String> {
    validate_with_draft(action, depth, true)
}
// Validate with draft
fn validate_with_draft(action: &BuiltinAction, depth: usize, draft: bool) -> Result<(), String> {
    if let BuiltinAction::KeyboardMacro {
        recording,
        playback,
    } = action
    {
        if draft {
            crate::keyboard_macro::validation::validate_draft(recording, playback)?;
        } else {
            crate::keyboard_macro::validation::validate(recording, playback)?;
        }
    }
    if let BuiltinAction::SavedMacro { arguments, .. } = action {
        crate::macro_inputs::validate_assignments(arguments)?;
    }
    if depth > 8 {
        return Err("Action nesting exceeds 8".into());
    }
    let value = serde_json::to_value(action).map_err(|e| e.to_string())?;
    let kind = value["kind"].as_str().unwrap();
    let entry = catalog()
        .iter()
        .find(|e| e["id"] == kind)
        .ok_or("Unknown built-in action")?;
    for f in entry["fields"].as_array().unwrap() {
        let key = f["key"].as_str().unwrap();
        let v = &value[key];
        if let Some(options) = f.get("options").and_then(Value::as_array) {
            if !options.contains(v) {
                return Err(format!("Invalid {key}"));
            }
        }
        if let Some(text) = v.as_str() {
            if text.len() > 256 * 1024 {
                return Err(format!("{key} exceeds 256 KiB"));
            }
        }
        if (key.ends_with("_ms") || key == "milliseconds")
            && v.as_i64().is_some_and(|n| !(0..=3_600_000).contains(&n))
        {
            return Err(format!("Invalid {key}"));
        }
    }
    if action.children().len() > 100 {
        return Err("Action container exceeds 100 children".into());
    }
    match action {
        BuiltinAction::Conditional { children, .. }
        | BuiltinAction::PressRelease { children }
        | BuiltinAction::FailurePolicy { children, .. }
            if children.len() != 2 =>
        {
            return Err("Action requires two branches".into())
        }
        BuiltinAction::Gesture { children, .. } if children.len() != 3 => {
            return Err("Gesture requires three branches".into())
        }
        BuiltinAction::Cooldown { children, .. } if children.len() != 1 => {
            return Err("Cooldown requires one branch".into())
        }
        BuiltinAction::RepeatActions { count, .. } if !(1..=1000).contains(count) => {
            return Err("Repeat count must be 1–1000".into())
        }
        BuiltinAction::FailurePolicy {
            retries, delay_ms, ..
        } if !(0..=5).contains(retries) || !(0..=60_000).contains(delay_ms) => {
            return Err(
                "Retry policy allows at most five retries and 60 seconds between attempts".into(),
            )
        }
        BuiltinAction::RepeatHeld { interval_ms, .. } if *interval_ms < 20 => {
            return Err("Repeat interval must be at least 20 ms".into())
        }
        BuiltinAction::MouseClick { clicks, .. } if !(1..=2).contains(clicks) => {
            return Err("Click count must be 1 or 2".into())
        }
        BuiltinAction::DateTime { format, .. }
            if chrono::format::StrftimeItems::new(format)
                .any(|i| matches!(i, chrono::format::Item::Error)) =>
        {
            return Err("Invalid date/time format".into())
        }
        BuiltinAction::HoldKeys { keys } | BuiltinAction::LatchKeys { keys } => {
            Action::Hotkey { keys: keys.clone() }.validate(depth)?;
        }
        BuiltinAction::Conditional { condition, .. }
        | BuiltinAction::WaitCondition { condition, .. }
            if !valid_condition(condition) =>
        {
            return Err("Unsupported condition".into())
        }
        _ => {}
    }
    for child in action.children() {
        if draft {
            child.validate_draft(depth + 1)?;
        } else {
            child.validate(depth + 1)?;
        }
    }
    Ok(())
}
type CompletionJobs = Arc<Mutex<Vec<std::thread::JoinHandle<Result<(), String>>>>>;
#[derive(Clone)]
struct Run {
    _debug_state: Option<Arc<DebugStateLease>>,
    debug: Option<Arc<crate::macro_debug::Session>>,
    simulation: Option<Arc<Mutex<simulation::Simulation>>>,
    id: crate::actions::RunId,
    parameters: Option<Arc<crate::parameters::FrozenParameters>>,
    runtime: Arc<Runtime>,
    request: Arc<Request>,
    dial_plugin_input: Option<Input>,
    key_logic: bool,
    // Debugger node paths stay editor-relative; vendor context paths include the saved-macro call
    debug_macro_root: bool,
    pressed: Arc<AtomicBool>,
    locals: BTreeMap<String, Value>,
    inputs: crate::macro_inputs::InputValues,
    macros: MacroLibrary,
    calls: Vec<String>,
    target: Option<String>,
    device_generation: u64,
    branch_cancellations: Vec<Arc<AtomicBool>>,
    completion_jobs: CompletionJobs,
    keyboard_events: Arc<AtomicUsize>,
}
impl Run {
    // Resolve the plugin ownership scope for this execution
    fn plugin_scope(&self, path: &[usize]) -> String {
        if self.debug_macro_root {
            let call_path: Vec<_> = std::iter::once(0).chain(path.iter().copied()).collect();
            crate::actions::node_scope(&self.request.binding_scope, &call_path)
        } else {
            crate::actions::node_scope(&self.request.binding_scope, path)
        }
    }
    // Route the plugin’s requested input operation
    fn plugin_input(&self, path: &[usize]) -> Input {
        self.dial_plugin_input.clone().unwrap_or_else(|| {
            if path.is_empty() {
                self.request.input.clone()
            } else {
                Input::Pulse
            }
        })
    }
    // Emit the final action-run outcome with its device and control identity
    fn finished(&self, outcome: crate::actions::RunOutcome) {
        self.runtime.emit(
            "action-run",
            Some(self.request.device_id.clone()),
            None,
            Some(
                json!({"run_id":self.id,"control_id":self.request.control_id,"outcome":outcome})
                    .to_string(),
            ),
        );
    }
    // Return the key identifying builtins
    fn key(&self) -> String {
        if self.debug.is_some() {
            return format!(
                "debug:{}|{}|{}",
                self.id, self.request.device_id, self.request.control_id
            );
        }
        format!(
            "{}|{}|{}",
            self.request.device_id, self.request.binding_scope, self.request.control_id
        )
    }
    // Check the current builtins conditions
    fn check(&self) -> Result<(), String> {
        if let Some(session) = &self.debug {
            session.check()?;
        }
        let transition_permitted = self.simulation.is_some()
            || self.request.manual
            || self
                .runtime
                .transition_inputs
                .lock()
                .unwrap()
                .permits_request(
                    &self.request.device_id,
                    self.request.connection_epoch,
                    self.request.queued_at,
                );
        if self
            .branch_cancellations
            .iter()
            .any(|c| c.load(Ordering::SeqCst))
            || !transition_permitted
            || self.runtime.stopping.load(Ordering::SeqCst)
            || self.request.cancellation_generation
                != self.runtime.cancellation.load(Ordering::SeqCst)
            || (self.simulation.is_none()
                && self.request.connection_epoch.is_some_and(|epoch| {
                    self.runtime
                        .power
                        .suppressed(&self.request.device_id, epoch)
                        || self
                            .runtime
                            .connection_epochs
                            .lock()
                            .unwrap()
                            .get(&self.request.device_id)
                            != Some(&epoch)
                        || self
                            .runtime
                            .builtin_state
                            .device_cancellations
                            .lock()
                            .unwrap()
                            .get(&self.request.device_id)
                            .map(|value| value.load(Ordering::SeqCst))
                            .unwrap_or(0)
                            != self.device_generation
                }))
        {
            return Err("Cancelled".into());
        }
        Ok(())
    }
    /// Cancellation is a lifecycle outcome, not an action failure. Keep it in
    /// run history, but do not toast when navigation retires the originating run
    /// A coincident cancellation must never conceal a genuine execution error
    fn completion_result(&self, result: Result<(), String>) -> Result<(), String> {
        result.and_then(|()| self.check())
    }
    // Report an action execution failure through the runtime diagnostics
    fn report_execution_error(&self, error: &str) {
        if error == "Keyboard macro busy" || (error == "Cancelled" && self.check().is_err()) {
            return;
        }
        self.runtime.emit(
            "action-error",
            Some(self.request.device_id.clone()),
            None,
            Some(error.into()),
        );
    }
    // Read the current timestamp in milliseconds
    fn clock_ms(&self) -> u64 {
        self.simulation
            .as_ref()
            .map(|s| s.lock().unwrap().elapsed_ms)
            .unwrap_or_else(|| self.runtime.media_start.elapsed().as_millis() as u64)
    }
    // Wait through the requested builtins delay
    fn sleep(&self, ms: i32) -> Result<(), String> {
        self.check()?;
        if let Some(simulation) = &self.simulation {
            let mut simulation = simulation.lock().unwrap();
            simulation.elapsed_ms = simulation.elapsed_ms.saturating_add(ms.max(0) as u64);
            if simulation.elapsed_ms > 86_400_000 {
                return Err("Simulation exceeds one day of virtual time".into());
            }
            return Ok(());
        }
        let _waiting = DebugWait::new(self.debug.clone());
        let end = Instant::now() + Duration::from_millis(ms.max(0) as u64);
        while Instant::now() < end {
            self.check()?;
            std::thread::sleep(
                Duration::from_millis(10).min(end.saturating_duration_since(Instant::now())),
            );
        }
        self.check()
    }
    // Release the owned builtins resource
    fn release(&self, timeout: i32) -> Result<(), String> {
        if self.debug.is_some() {
            return self.check();
        }
        if !matches!(self.request.input, Input::Button { pressed: true }) {
            return Err("This action requires a button with release events".into());
        }
        let start = Instant::now();
        while self.pressed.load(Ordering::SeqCst) {
            self.sleep(10)?;
            if timeout > 0 && start.elapsed().as_millis() > timeout as u128 {
                return Err("Timed out waiting for release".into());
            }
        }
        Ok(())
    }
    // Expand template expressions against the run’s values when interpolation is enabled
    fn text(&self, text: &str, template: bool) -> Result<String, String> {
        if !template {
            return Ok(text.into());
        }
        let mut result = String::new();
        let mut rest = text;
        while let Some(index) = rest.find("${") {
            result.push_str(&rest[..index]);
            rest = &rest[index + 2..];
            let end = rest.find('}').ok_or("Unclosed variable reference")?;
            let name = &rest[..end];
            let value = if let Some(n) = name.strip_prefix("run.") {
                self.locals.get(n).cloned()
            } else if let Some(n) = name.strip_prefix("input.") {
                self.inputs.get(n).cloned()
            } else {
                None
            }
            .ok_or_else(|| format!("Unknown variable {name}"))?;
            result.push_str(&display(&value));
            rest = &rest[end + 1..];
        }
        result.push_str(rest);
        if result.len() > 256 * 1024 {
            return Err("Expanded text exceeds 256 KiB".into());
        }
        Ok(result)
    }
    // Request a user-interface response or supply the configured simulated response
    fn ui(&self, kind: &str, data: Value) -> Result<String, String> {
        #[cfg(test)]
        if native::testing::enabled() {
            return Ok(if kind == "menu" {
                "0".into()
            } else if kind == "path" {
                self.locals
                    .get("test_path")
                    .map(display)
                    .unwrap_or_default()
            } else {
                "test input".into()
            });
        }

        if !self
            .runtime
            .builtin_state
            .desktop_host
            .load(Ordering::SeqCst)
        {
            return Err("This action requires the running desktop application".into());
        }
        let state = &self.runtime.builtin_state;
        let _lane = loop {
            self.check()?;
            if let Ok(lock) = state.ui_lane.try_lock() {
                break lock;
            }
            self.sleep(20)?;
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::sync_channel(1);
        let mut data = data;
        data["id"] = json!(id);
        data["kind"] = json!(kind);
        state
            .pending
            .lock()
            .unwrap()
            .insert(id.clone(), (data.clone(), tx));
        self.runtime.emit(
            "action-ui",
            Some(self.request.device_id.clone()),
            None,
            Some(data.to_string()),
        );
        let result = loop {
            if let Err(e) = self.check() {
                break Err(e);
            }
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(Some(value))) => break Ok(value),
                Ok(Err(error)) => break Err(error),
                Ok(Ok(None)) => break Err("Cancelled".into()),
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err("Cancelled".into()),
                Err(_) => {}
            }
        };
        state.pending.lock().unwrap().remove(&id);
        self.runtime.emit("action-ui-close", None, None, Some(id));
        if result.is_ok() {
            native::restore_target(self.target.as_deref());
        }
        result
    }
}
// Display JSON strings without quotes and serialize other JSON values
fn display(value: &Value) -> String {
    value
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}
struct LaneGuard {
    lane: Arc<Mutex<VecDeque<String>>>,
    ticket: String,
}
impl Drop for LaneGuard {
    // Release this builtins owner when it leaves scope
    fn drop(&mut self) {
        self.lane
            .lock()
            .unwrap()
            .retain(|ticket| ticket != &self.ticket);
    }
}
// Reconcile input generation
fn reconcile_input_generation(runtime: &Runtime, generation: &mut usize) {
    let current = native::input_generation();
    if current != *generation {
        *generation = current;
        runtime.cancel_macros();
    }
}
// Start the builtins operation
pub fn start(runtime: Weak<Runtime>, receiver: mpsc::Receiver<Request>) {
    std::thread::spawn(move || {
        let mut generation = native::input_generation();
        loop {
            let received = receiver.recv_timeout(Duration::from_millis(100));
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.stopping.load(Ordering::SeqCst) {
                break;
            }
            reconcile_input_generation(&runtime, &mut generation);
            let request = match received {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            };
            if runtime.builtin_state.active.fetch_add(1, Ordering::SeqCst) >= 128 {
                runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
                crate::feedback::finished(&runtime, &request, "Failed", false);
                runtime.emit(
                    "action-error",
                    Some(request.device_id),
                    None,
                    Some("Action queue is full".into()),
                );
                continue;
            }
            if !request.manual
                && !runtime.transition_inputs.lock().unwrap().permits_request(
                    &request.device_id,
                    request.connection_epoch,
                    request.queued_at,
                )
            {
                runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
                crate::feedback::finished(&runtime, &request, "Cancelled", false);
                continue;
            }
            let inputs = runtime.builtin_state.execution_inputs(&runtime);
            let key = format!("{}|{}", request.device_id, request.control_id);
            let pressed = request
                .pressed
                .clone()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
            let run = Run {
                id: crate::actions::RunId(uuid::Uuid::new_v4().to_string()),
                _debug_state: None,
                debug: None,
                simulation: None,
                parameters: Some(Arc::new(inputs.freeze_parameters(&runtime))),
                macros: inputs.macros,
                device_generation: runtime
                    .builtin_state
                    .device_cancellations
                    .lock()
                    .unwrap()
                    .get(&request.device_id)
                    .map(|value| value.load(Ordering::SeqCst))
                    .unwrap_or(0),
                runtime: runtime.clone(),
                dial_plugin_input: None,
                key_logic: false,
                debug_macro_root: false,
                request: Arc::new(request),
                pressed,
                locals: Default::default(),
                inputs: Default::default(),
                calls: vec![],
                branch_cancellations: vec![],
                completion_jobs: Default::default(),
                keyboard_events: Default::default(),
                target: None,
            };
            let lane = runtime.builtin_state.lane(key);
            let gesture = match key_logic::contains_gesture(&run.request.action, &run.macros) {
                Ok(gesture) => gesture,
                Err(error) => {
                    run.report_execution_error(&error);
                    run.finished(crate::actions::RunOutcome::Failed);
                    crate::feedback::finished(&runtime, &run.request, "Failed", false);
                    runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            };
            let ticket = uuid::Uuid::new_v4().to_string();
            if !gesture {
                let mut tickets = lane.lock().unwrap();
                let keyboard_macro = crate::keyboard_macro::validation::expanded_event_count(
                    &run.request.action,
                    |id| {
                        run.macros
                            .iter()
                            .find(|saved| saved.id == id)
                            .map(|saved| &saved.action)
                    },
                )
                .unwrap_or(0)
                    > 0;
                if (keyboard_macro && native::keyboard_macro_busy())
                    || (!tickets.is_empty()
                        && (keyboard_macro
                            || run.request.activation_policy
                                == crate::actions::ActivationPolicy::IgnoreBusy))
                {
                    drop(tickets);
                    run.finished(crate::actions::RunOutcome::Ignored);
                    crate::feedback::finished(&runtime, &run.request, "Ignored while busy", false);
                    runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                if run.request.activation_policy == crate::actions::ActivationPolicy::ReplacePending
                {
                    tickets.truncate(1);
                }
                tickets.push_back(ticket.clone());
            }
            std::thread::spawn(move || {
                let _lane = if gesture {
                    None
                } else {
                    Some(LaneGuard {
                        lane: lane.clone(),
                        ticket: ticket.clone(),
                    })
                };
                let mut needs_target = false;
                run.request.action.visit(&mut |a| {
                    if let Action::Builtin { action } = a {
                        if matches!(
                            **action,
                            BuiltinAction::TypeText { .. }
                                | BuiltinAction::PasteText { .. }
                                | BuiltinAction::DateTime { .. }
                                | BuiltinAction::TransformText { .. }
                                | BuiltinAction::SearchSelection { .. }
                                | BuiltinAction::InputPrompt { .. }
                                | BuiltinAction::ActionMenu { .. }
                                | BuiltinAction::ChoosePath { .. }
                                | BuiltinAction::SavedMacro { .. }
                        ) {
                            needs_target = true;
                        }
                    }
                });
                let target = if needs_target { native::target() } else { None };
                if !gesture {
                    while lane.lock().unwrap().front() != Some(&ticket) {
                        if !lane.lock().unwrap().contains(&ticket) {
                            run.finished(crate::actions::RunOutcome::Superseded);
                            crate::feedback::finished(
                                &run.runtime,
                                &run.request,
                                "Replaced pending activation",
                                false,
                            );
                            run.runtime
                                .builtin_state
                                .active
                                .fetch_sub(1, Ordering::SeqCst);
                            return;
                        }
                        if run.check().is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                let mut run = run;
                crate::feedback::running(&run.runtime, &run.request);
                run.target = target;
                let request = run.request.clone();
                let mut result = execute(&mut run, &request.action, 0, vec![]);
                // Detached branches retain their execution behavior, but an icon
                // cannot announce success until all of the binding's work finishes
                loop {
                    let handles = std::mem::take(&mut *run.completion_jobs.lock().unwrap());
                    if handles.is_empty() {
                        break;
                    }
                    for handle in handles {
                        let completed = handle
                            .join()
                            .unwrap_or_else(|_| Err("Parallel worker failed".into()));
                        if result.is_ok() {
                            result = completed;
                        }
                    }
                }
                let result = run.completion_result(result);
                if result.is_ok() {
                    crate::feedback::icon_succeeded(&run.runtime, &run.request);
                }
                let status = match &result {
                    Ok(_) => "Completed",
                    Err(e) if e == "Cancelled" => "Cancelled",
                    Err(e) if e == "Keyboard macro busy" => "Ignored while busy",
                    Err(_) => "Failed",
                };
                run.finished(match &result {
                    Ok(_) => crate::actions::RunOutcome::Completed,
                    Err(e) if e == "Cancelled" => crate::actions::RunOutcome::Cancelled,
                    Err(e) if e == "Keyboard macro busy" => crate::actions::RunOutcome::Ignored,
                    Err(_) => crate::actions::RunOutcome::Failed,
                });
                crate::feedback::finished(&run.runtime, &run.request, status, true);
                if let Err(error) = result {
                    run.report_execution_error(&error);
                }
                run.runtime
                    .builtin_state
                    .active
                    .fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
}
struct DebugWait(Option<Arc<crate::macro_debug::Session>>);
impl DebugWait {
    // Initialize builtins state from the supplied inputs
    fn new(session: Option<Arc<crate::macro_debug::Session>>) -> Self {
        if let Some(s) = &session {
            s.wait(true);
        }
        Self(session)
    }
}
impl Drop for DebugWait {
    // Release this builtins owner when it leaves scope
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            s.wait(false);
        }
    }
}
// Check macro plugins
fn check_macro_plugins(run: &Run, action: &Action) -> Result<(), String> {
    let host = run.runtime.plugins.lock().unwrap();
    require_macro_plugins(
        action,
        |id| {
            run.macros
                .iter()
                .find(|saved| saved.id == id)
                .map(|saved| &saved.action)
        },
        |plugin, action| host.declares_action(plugin, action),
    )
}

// Execute the requested builtins operation
fn execute(run: &mut Run, action: &Action, depth: usize, path: Vec<usize>) -> Result<(), String> {
    run.check()?;
    if depth == 0 && matches!(action, Action::Macro { .. }) {
        check_macro_plugins(run, action)?;
    }
    let boundary = if let Some(session) = &run.debug {
        let kind = match action {
            Action::Builtin { action } => serde_json::to_value(action).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string(),
            _ => serde_json::to_value(action).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string(),
        };
        let leaf = action.child(0).is_none()
            && !matches!(action, Action::Builtin { action } if matches!(**action, BuiltinAction::SavedMacro {..} | BuiltinAction::TriggerBinding{..}));
        Some(session.before(path.clone(), kind, leaf, || run.check())?)
    } else {
        None
    };
    let mut result = execute_inner(run, action, depth, path);
    if let Some(boundary) = boundary {
        if serde_json::to_vec(&run.locals).is_ok_and(|bytes| bytes.len() > 256 * 1024) {
            run.locals.clear();
            result = Err("Debug output exceeds 256 KiB".into());
        }
        let detail = run
            .runtime
            .credentials
            .redact(json!({"outputs":run.locals,"error":result.as_ref().err()}).to_string());
        boundary.finish(&result, detail);
    }
    result
}
// Execute inner
fn execute_inner(
    run: &mut Run,
    action: &Action,
    depth: usize,
    path: Vec<usize>,
) -> Result<(), String> {
    run.check()?;
    // Resolve this boundary only. Descendants must retain portable references
    // until their own dispatch (and their own lexical macro frame)
    let mut local = action.clone();
    let mut children = vec![];
    while let Some(child) = local.child_mut(children.len()) {
        children.push(std::mem::replace(child, Action::None));
    }
    let resolved = if run.simulation.is_some() && run.request.binding_scope.starts_with("::debug:")
    {
        crate::parameters::resolve_action_values(
            &local,
            &|id, action| run.runtime.plugin_parameter_fields(id, action),
            &|name| Err(format!("Select a test profile to resolve parameter {name}")),
        )?
    } else if let Some(parameters) = &run.parameters {
        run.runtime.resolve_frozen_parameters(
            &local,
            &run.request.device_id,
            &run.request.binding_scope,
            parameters,
        )?
    } else {
        run.runtime.resolve_parameters(
            &local,
            &run.request.device_id,
            &run.request.binding_scope,
        )?
    };
    let mut resolved =
        crate::macro_inputs::resolve_action(&resolved, &run.inputs, &|id, action_id| {
            run.runtime.plugin_parameter_fields(id, action_id)
        })?;
    for (index, child) in children.into_iter().enumerate() {
        *resolved.child_mut(index).expect("same action structure") = child;
    }
    if let Action::Builtin { action } = &resolved {
        if let BuiltinAction::KeyboardMacro {
            recording,
            playback,
        } = action.as_ref()
        {
            let summary = crate::keyboard_macro::validation::validate(recording, playback)?;
            run.keyboard_events
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                    used.checked_add(summary.event_count).filter(|total| {
                        *total <= crate::keyboard_macro::validation::MAX_ACTION_EVENTS
                    })
                })
                .map_err(|_| "Keyboard macro run budget exceeds 100,000 events")?;
        }
    }
    if let Some(result) = simulation::effect(run, &resolved, depth, path.clone()) {
        return result;
    }
    if run.simulation.is_none() && run.request.connection_epoch.is_none() {
        use BuiltinAction::*;
        let requires_device = match &resolved {
            Action::Profile { .. }
            | Action::Page { .. }
            | Action::Navigate { .. }
            | Action::SwipeNavigation { .. }
            | Action::Brightness { .. }
            | Action::Lighting { .. }
            | Action::Sleep => true,
            Action::Builtin { action } => match &**action {
                PageStep { .. }
                | PageBack
                | MomentaryPage { .. }
                | BrightnessStep { .. }
                | AutomationControl { .. }
                | BindingLayer { .. }
                | EditBinding { .. }
                | PauseBindings { .. } => true,
                TriggerBinding { device_id, .. } => device_id.is_empty(),
                _ => false,
            },
            _ => false,
        };
        if requires_device {
            return Err("This macro step requires a connected device or simulator".into());
        }
    }
    if let (
        Action::Plugin {
            plugin_id,
            action_id,
            settings: durable,
            user_desired_state,
            resources,
            ..
        },
        Action::Plugin {
            settings: resolved, ..
        },
    ) = (action, &resolved)
    {
        let scope = run.plugin_scope(&path);
        let resource_paths = crate::vendor_resources::paths(&run.runtime.profiles.root, resources)?;
        {
            let mut host = run.runtime.plugins.lock().unwrap();
            if run.key_logic {
                key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
            }
            host.set_action_resources(
                plugin_id,
                &scope,
                &run.request.device_id,
                &run.request.control_id,
                serde_json::json!(resource_paths),
            );
        }
        if run.request.manual {
            action.validate(depth)?;
            let lease = {
                let host = run.runtime.plugins.lock().unwrap();
                if run.key_logic {
                    key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
                }
                host.begin_manual_action(plugin_id, action_id)?
            };
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                run.check()?;
                let sent = {
                    let mut host = run.runtime.plugins.lock().unwrap();
                    if run.key_logic {
                        key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
                    }
                    run.check()?;
                    host.try_dispatch_manual_action(
                        &lease,
                        &run.request.device_id,
                        &run.request.control_id,
                        &scope,
                        durable,
                        resolved,
                        *user_desired_state,
                        || run.runtime.credentials.redactions(),
                    )?
                };
                if sent {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!("Timed out waiting for plugin {plugin_id} to start"));
                }
                run.sleep(25)?;
            }
        }
        let mut host = run.runtime.plugins.lock().unwrap();
        if run.key_logic {
            key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
        }
        if run.debug.is_some() {
            host.begin_temporary_context(
                plugin_id,
                &scope,
                &run.request.device_id,
                &run.request.control_id,
            )?;
        }
        host.set_execution_settings(
            plugin_id,
            &scope,
            &run.request.device_id,
            &run.request.control_id,
            durable,
            resolved,
            run.runtime.credentials.redactions(),
        );
    }
    let action = &resolved;
    action.validate(depth)?;
    match action {
        Action::Dial { actions, .. } => {
            let Some((index, input)) = crate::actions::dial_input(&run.request.input) else {
                return Ok(());
            };
            if matches!(actions[index], Action::Macro { .. }) {
                check_macro_plugins(run, &actions[index])?;
            }
            let previous = run.request.clone();
            let previous_plugin_input = run.dial_plugin_input.clone();
            run.dial_plugin_input =
                Some(
                    previous_plugin_input
                        .clone()
                        .unwrap_or_else(|| match &previous.input {
                            Input::Rotate { .. } => previous.input.clone(),
                            _ => Input::Pulse,
                        }),
                );
            Arc::make_mut(&mut run.request).input = input;
            let mut child_path = path;
            child_path.push(index);
            let result = execute(run, &actions[index], depth + 1, child_path);
            run.request = previous;
            run.dial_plugin_input = previous_plugin_input;
            result
        }
        Action::Macro { actions } => {
            for (i, a) in actions.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                execute(run, a, depth + 1, p)?;
            }
            Ok(())
        }
        Action::Delay { milliseconds } => run.sleep(*milliseconds as i32),
        Action::Toggle { on, off } => {
            let key = format!("{}:{path:?}", run.key());
            let n = run
                .runtime
                .builtin_state
                .indices
                .lock()
                .unwrap()
                .get(&key)
                .copied()
                .unwrap_or(0);
            let index = n % 2;
            let mut p = path;
            p.push(index);
            execute(run, if index == 0 { on } else { off }, depth + 1, p)?;
            run.runtime
                .builtin_state
                .indices
                .lock()
                .unwrap()
                .insert(key, n + 1);
            Ok(())
        }
        Action::Command { program, args } => {
            let job = run.clone();
            let program = program.clone();
            let args = args.clone();
            crate::native_pool::run(
                move || {
                    job.check()?;
                    native::spawn_supervised(program, args, Box::new(move || job.check()))
                },
                || run.check(),
            )
        }
        Action::Hotkey { keys } => native::chord(keys),
        Action::Builtin { action } if blocking_builtin(action) => {
            #[cfg(test)]
            if native::testing::enabled() {
                return builtin(run, action, depth, path);
            }
            let mut job = run.clone();
            let action = action.clone();
            let (result, locals) = crate::native_pool::run(
                move || {
                    job.check()?;
                    let result = builtin(&mut job, &action, depth, path);
                    Ok((result, job.locals))
                },
                || run.check(),
            )?;
            run.locals = locals;
            result
        }
        Action::Builtin { action } => builtin(run, action, depth, path),
        Action::Plugin {
            plugin_id,
            action_id,
            settings,
            user_desired_state,
            ..
        } if run.request.icon_signature.is_some() || run.debug.is_some() => {
            let scope = run.plugin_scope(&path);
            let input = run.plugin_input(&path);
            let receipt = {
                let mut host = run.runtime.plugins.lock().unwrap();
                if run.key_logic {
                    key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
                }
                run.check()?;
                host.dispatch_tracked_with_state(
                    plugin_id,
                    action_id,
                    &run.request.device_id,
                    &run.request.control_id,
                    &scope,
                    &input,
                    settings,
                    *user_desired_state,
                )?
            };
            receipt.wait(Duration::from_secs(30), || run.check())
        }
        Action::Plugin {
            plugin_id,
            action_id,
            settings,
            user_desired_state,
            ..
        } => {
            let scope = run.plugin_scope(&path);
            let input = run.plugin_input(&path);
            let mut host = run.runtime.plugins.lock().unwrap();
            if run.key_logic {
                key_logic::validate_plugin(plugin_id, action_id, &host.list())?;
            }
            run.check()?;
            host.dispatch_with_state(
                plugin_id,
                action_id,
                &run.request.device_id,
                &run.request.control_id,
                &scope,
                &input,
                settings,
                *user_desired_state,
            )
        }
        _ => {
            let scope = if matches!(action, Action::Plugin { .. }) {
                run.plugin_scope(&path)
            } else {
                crate::actions::node_scope(&run.request.binding_scope, &path)
            };
            let input = if matches!(action, Action::Plugin { .. }) {
                run.plugin_input(&path)
            } else {
                run.request.input.clone()
            };
            if matches!(
                action,
                Action::OpenUrl { .. }
                    | Action::OpenPath { .. }
                    | Action::Application { .. }
                    | Action::Media { .. }
                    | Action::Volume { .. }
                    | Action::Mute
            ) {
                #[cfg(test)]
                if native::testing::enabled() {
                    return crate::actions::execute_leaf(
                        &run.runtime,
                        action,
                        &run.request.device_id,
                        &run.request.control_id,
                        &scope,
                        run.request.connection_epoch,
                        &input,
                        run.request.cancellation_generation,
                    );
                }
                let job = run.clone();
                let action = action.clone();
                return crate::native_pool::run(
                    move || {
                        job.check()?;
                        crate::actions::execute_leaf(
                            &job.runtime,
                            &action,
                            &job.request.device_id,
                            &job.request.control_id,
                            &scope,
                            job.request.connection_epoch,
                            &input,
                            job.request.cancellation_generation,
                        )
                    },
                    || run.check(),
                );
            }
            crate::actions::execute_leaf(
                &run.runtime,
                action,
                &run.request.device_id,
                &run.request.control_id,
                &scope,
                run.request.connection_epoch,
                &input,
                run.request.cancellation_generation,
            )
        }
    }
}
// Report the blocking builtin condition for builtins
fn blocking_builtin(action: &BuiltinAction) -> bool {
    matches!(
        action,
        BuiltinAction::TypeText { .. }
            | BuiltinAction::PasteText { .. }
            | BuiltinAction::SetClipboard { .. }
            | BuiltinAction::ClearClipboard
            | BuiltinAction::OpenLink { .. }
            | BuiltinAction::RevealFile { .. }
            | BuiltinAction::DateTime { .. }
            | BuiltinAction::TransformText { .. }
            | BuiltinAction::SearchSelection { .. }
            | BuiltinAction::ClipboardSlot { .. }
            | BuiltinAction::OpenWith { .. }
            | BuiltinAction::OpenTerminal { .. }
            | BuiltinAction::CommandResult { .. }
            | BuiltinAction::FileOperation { .. }
            | BuiltinAction::WriteFile { .. }
    )
}
// Execute one child action while extending its diagnostic path
fn branch(
    run: &mut Run,
    children: &[Action],
    index: usize,
    depth: usize,
    path: &[usize],
) -> Result<(), String> {
    let mut p = path.to_vec();
    p.push(index);
    if let Some(session) = &run.debug {
        session.record(
            path.to_vec(),
            "branch",
            format!("Selected branch {}", index + 1),
        );
    }
    execute(
        run,
        children.get(index).ok_or("Action branch missing")?,
        depth + 1,
        p,
    )
}
// Execute child actions in order and stop on the first failure
fn sequence(
    run: &mut Run,
    children: &[Action],
    depth: usize,
    path: &[usize],
) -> Result<(), String> {
    for i in 0..children.len() {
        branch(run, children, i, depth, path)?
    }
    Ok(())
}
// Evaluate a condition against simulated or live clipboard, file and application state
fn condition(run: &Run, c: &Condition) -> Result<bool, String> {
    let value = if let Some(simulation) = &run.simulation {
        let simulation = simulation.lock().unwrap();
        match c.source.as_str() {
            "clipboard" => json!(simulation.config.clipboard),
            "file" => json!(simulation.config.files.contains_key(&c.subject)),
            "application" => json!(simulation.config.application),
            "process" => json!(simulation.config.processes.contains(&c.subject)),
            _ => return Err("Unsupported condition source".into()),
        }
    } else {
        match c.source.as_str() {
            "clipboard" => json!(native::read_clipboard()?),
            "file" => json!(std::path::Path::new(&c.subject).exists()),
            "application" => json!(native::foreground_app().ok_or(
                "Foreground application detection is unavailable in this desktop session"
            )?),
            "process" => json!(native::process_running(&c.subject)?),
            _ => return Err("Unsupported condition source".into()),
        }
    };
    Ok(match c.operator.as_str() {
        "equals" => value == c.value,
        "not_equals" => value != c.value,
        "contains" => display(&value).contains(&display(&c.value)),
        "exists" => value.as_bool().unwrap_or(!value.is_null()),
        "missing" => !value.as_bool().unwrap_or(!value.is_null()),
        "greater" => value
            .as_f64()
            .zip(c.value.as_f64())
            .is_some_and(|(a, b)| a > b),
        "less" => value
            .as_f64()
            .zip(c.value.as_f64())
            .is_some_and(|(a, b)| a < b),
        _ => return Err("Unsupported comparison".into()),
    })
}
// Dispatch the built-in action while preserving execution depth and diagnostic ownership
fn builtin(run: &mut Run, a: &BuiltinAction, depth: usize, path: Vec<usize>) -> Result<(), String> {
    use BuiltinAction::*;
    let state = &run.runtime.builtin_state;
    let key = format!("{}:{path:?}", run.key());
    let device = run.request.device_id.clone();
    match a {
        FailurePolicy {
            operation,
            retries,
            delay_ms,
            children,
        } => {
            let attempts = if operation == "retry" {
                *retries + 1
            } else {
                1
            };
            for attempt in 0..attempts {
                match branch(run, children, 0, depth, &path) {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        // Generation, branch and session cancellation always bypass recovery
                        run.check()?;
                        if error == "Cancelled" || error == "Keyboard macro busy" {
                            return Err(error);
                        }
                        if operation == "continue" {
                            return Ok(());
                        }
                        if operation == "fallback" {
                            return branch(run, children, 1, depth, &path);
                        }
                        if attempt + 1 == attempts {
                            return Err(error);
                        }
                        run.sleep(*delay_ms)?;
                    }
                }
            }
            Ok(())
        }
        TypeText { text, template } => {
            native::type_text(&run.text(text, *template)?, run.target.as_deref())
        }
        PasteText {
            text,
            template,
            restore,
        } => native::paste(&run.text(text, *template)?, *restore, run.target.as_deref()),
        SetClipboard { text, template } => native::set_clipboard(&run.text(text, *template)?),
        ClearClipboard => native::set_clipboard(""),
        KeyboardMacro {
            recording,
            playback,
        } => {
            // Generic handler tests must never depend on live permissions as a
            // fence against native input. Timing/ownership use fake Backend tests
            #[cfg(test)]
            if native::testing::enabled() {
                crate::keyboard_macro::validation::validate(recording, playback)?;
                run.check()?;
                native::testing::record("keyboard_macro");
                return Ok(());
            }
            let owner = format!("{key}:keyboard:{}:{}", run.id, uuid::Uuid::new_v4());
            // Snapshot only owned atomics here, before taking native input locks
            // The queued guard must never call Run::check or acquire its mutexes
            let runtime = run.runtime.clone();
            let expected_global = run.request.cancellation_generation;
            let expected_device = run.device_generation;
            let device = runtime
                .builtin_state
                .device_cancellations
                .lock()
                .unwrap()
                .entry(run.request.device_id.clone())
                .or_default()
                .clone();
            let branches = run.branch_cancellations.clone();
            let debug = run.debug.clone();
            let guard = Arc::new(move || {
                if runtime.stopping.load(Ordering::SeqCst)
                    || runtime.cancellation.load(Ordering::SeqCst) != expected_global
                    || device.load(Ordering::SeqCst) != expected_device
                    || branches.iter().any(|flag| flag.load(Ordering::SeqCst))
                    || debug
                        .as_ref()
                        .is_some_and(|session| session.cancelled.load(Ordering::SeqCst))
                {
                    Err("Cancelled".into())
                } else {
                    Ok(())
                }
            });
            crate::keyboard_macro::playback::play(recording, playback, &owner, guard, || {
                run.check()
            })
            .map_err(|error| {
                if matches!(
                    error.as_str(),
                    "Keyboard input is busy"
                        | "Keyboard input is busy recording or playing a macro"
                ) {
                    "Keyboard macro busy".into()
                } else {
                    error
                }
            })
        }
        HoldKeys { keys } => {
            native::hold(&key, keys)?;
            let result = run.release(0);
            native::release(&key);
            result
        }
        LatchKeys { keys } => native::latch(&key, keys),
        MouseClick { button, clicks } => native::click(button, *clicks),
        MouseMove { x, y, relative } => native::move_pointer(*x, *y, *relative),
        MouseScroll { x, y } => {
            let ticks = if let Input::Rotate { delta } = run.request.input {
                delta
            } else {
                1
            };
            native::scroll(x.saturating_mul(ticks), y.saturating_mul(ticks))
        }
        MouseDrag {
            button,
            x,
            y,
            relative,
            milliseconds,
        } => {
            native::button(&key, button, true)?;
            let result = (|| {
                run.sleep(*milliseconds)?;
                native::move_pointer(*x, *y, *relative)
            })();
            let release = native::button(&key, button, false);
            result.and(release)
        }
        OpenLink { url, template } => native::open_link(&run.text(url, *template)?),
        RevealFile { path, template } => native::reveal(&run.text(path, *template)?),
        PageStep { direction, wrap } => {
            let snapshot = run.runtime.snapshot();
            let session = snapshot.session(&device);
            let profile = snapshot
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id)
                .ok_or("Profile missing")?;
            let index = profile
                .pages
                .iter()
                .position(|p| p.id == session.page_id)
                .ok_or("Page missing")?;
            let n = profile.pages.len() as i32;
            let mut next = index as i32 + if direction == "previous" { -1 } else { 1 };
            if *wrap {
                next = next.rem_euclid(n)
            }
            if next >= 0 && next < n {
                navigate(run, &profile.id, &profile.pages[next as usize].id, true)?;
            }
            Ok(())
        }
        PageBack => {
            let prev = state
                .history
                .lock()
                .unwrap()
                .entry(device)
                .or_default()
                .pop();
            if let Some((profile, page)) = prev {
                navigate(run, &profile, &page, false)?
            }
            Ok(())
        }
        MomentaryPage { page_id } => {
            let snapshot = run.runtime.snapshot();
            let s = snapshot.session(&device);
            navigate(run, &s.profile_id, page_id, false)?;
            let result = run.release(0);
            let now = run.runtime.snapshot().session(&device);
            if now.profile_id == s.profile_id && now.page_id == *page_id {
                let _ = navigate(run, &s.profile_id, &s.page_id, false);
            }
            result
        }
        CycleActions { children } => {
            if children.is_empty() {
                return Err("Add actions to the cycle".into());
            }
            let n = state
                .indices
                .lock()
                .unwrap()
                .get(&key)
                .copied()
                .unwrap_or(0);
            branch(run, children, n % children.len(), depth, &path)?;
            run.runtime
                .builtin_state
                .indices
                .lock()
                .unwrap()
                .insert(key, n + 1);
            Ok(())
        }
        RepeatActions {
            count,
            interval_ms,
            children,
        } => {
            for i in 0..*count {
                sequence(run, children, depth, &path)?;
                if i + 1 < *count {
                    run.sleep(*interval_ms)?
                }
            }
            Ok(())
        }
        RepeatHeld {
            interval_ms,
            children,
        } => {
            if !matches!(run.request.input, Input::Button { pressed: true }) {
                return Err("Repeat while held requires a button with release events".into());
            }
            while run.pressed.load(Ordering::SeqCst) {
                sequence(run, children, depth, &path)?;
                run.sleep(*interval_ms)?;
            }
            Ok(())
        }
        Conditional {
            condition: c,
            children,
        } => {
            let i = if condition(run, c)? { 0 } else { 1 };
            branch(run, children, i, depth, &path)
        }
        WaitCondition {
            condition: c,
            timeout_ms,
            poll_ms,
        } => {
            let start = run.clock_ms();
            while !condition(run, c)? {
                if run.clock_ms().saturating_sub(start) >= *timeout_ms as u64 {
                    return Err("Condition timed out".into());
                }
                run.sleep((*poll_ms).max(20))?;
            }
            Ok(())
        }
        CancelMacros => {
            run.runtime.cancel_macros();
            Ok(())
        }
        BrightnessStep { delta } => {
            let mut output = run.runtime.snapshot().session(&device).output;
            let ticks = if let Input::Rotate { delta } = run.request.input {
                delta
            } else {
                1
            };
            output.brightness =
                (output.brightness as i32 + delta.saturating_mul(ticks)).clamp(0, 100) as u8;
            run.runtime.set_device_output(&device, output).map(|_| ())
        }
        AutomationControl { operation } => {
            let s = run.runtime.snapshot().session(&device);
            let (enabled, paused) = match operation.as_str() {
                "enable" | "resume" => (true, false),
                "disable" => (false, false),
                "pause" => (s.automation_enabled, true),
                _ => (!s.automation_enabled, false),
            };
            run.runtime
                .set_automation(&device, enabled, paused)
                .map(|_| ())
        }
        Notification {
            title,
            text,
            template,
        } => {
            if !state.desktop_host.load(Ordering::SeqCst) {
                return Err("Notifications require the desktop application".into());
            }
            let title = run.text(title, *template)?;
            let text = run.text(text, *template)?;
            run.runtime.emit(
                "action-notification",
                Some(device),
                None,
                Some(json!({"title":title,"text":text}).to_string()),
            );
            Ok(())
        }
        DateTime { format, utc } => {
            let text = if *utc {
                chrono::Utc::now().format(format).to_string()
            } else {
                chrono::Local::now().format(format).to_string()
            };
            native::type_text(&text, run.target.as_deref())
        }
        TransformText { operation } => {
            let text = native::selection(run.target.as_deref())?;
            let text = transform(&text, operation);
            native::paste(&text, true, run.target.as_deref())
        }
        SearchSelection { url, source } => {
            let text = if source == "clipboard" {
                native::read_clipboard()?
            } else {
                native::selection(run.target.as_deref())?
            };
            let query: String = text
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            let url = url.replace("{query}", &query);
            Action::OpenUrl { url: url.clone() }.validate(0)?;
            native::open_web(&url)
        }
        ClipboardSlot {
            name,
            operation,
            persist,
        } => {
            let name = format!("clipboard:{name}");
            match operation.as_str() {
                "save" => state.store(name, json!(native::read_clipboard()?), *persist),
                "restore" => native::set_clipboard(&display(
                    &state
                        .get(&name)
                        .filter(|v| !v.is_null())
                        .ok_or("Clipboard slot is empty")?,
                )),
                _ => state.store(name, Value::Null, false),
            }
        }
        InputPrompt {
            title,
            text,
            output,
        } => {
            let result = run.ui("input", json!({"title":title,"text":text}))?;
            run.locals.insert(output.clone(), json!(result));
            Ok(())
        }
        ActionMenu {
            title,
            labels,
            children,
        } => {
            let selected = run.ui("menu", json!({"title":title,"labels":labels}))?;
            branch(
                run,
                children,
                selected.parse().map_err(|_| "Invalid menu selection")?,
                depth,
                &path,
            )
        }
        Gesture { .. } => key_logic::execute_gesture(run, a, depth, &path),
        PressRelease { children } => {
            branch(run, children, 0, depth, &path)?;
            run.release(0)?;
            branch(run, children, 1, depth, &path)
        }
        WaitRelease { timeout_ms } => run.release(*timeout_ms),
        Parallel { children, wait } => {
            let mut handles: Vec<std::thread::JoinHandle<Result<(), String>>> = vec![];
            let failed = Arc::new(AtomicBool::new(false));
            for (i, child) in children.iter().enumerate() {
                if run
                    .runtime
                    .builtin_state
                    .active
                    .fetch_add(1, Ordering::SeqCst)
                    >= 128
                {
                    run.runtime
                        .builtin_state
                        .active
                        .fetch_sub(1, Ordering::SeqCst);
                    failed.store(true, Ordering::SeqCst);
                    for handle in handles {
                        let _: Result<Result<(), String>, _> = handle.join();
                    }
                    return Err("Parallel action limit reached".into());
                }
                let mut child_run = run.clone();
                child_run.branch_cancellations.push(failed.clone());
                let child = child.clone();
                let mut p = path.clone();
                p.push(i);
                let failed = failed.clone();
                handles.push(std::thread::spawn(move || {
                    let result = execute(&mut child_run, &child, depth + 1, p);
                    if result.is_err() {
                        failed.store(true, Ordering::SeqCst)
                    }
                    child_run
                        .runtime
                        .builtin_state
                        .active
                        .fetch_sub(1, Ordering::SeqCst);
                    if let Err(error) = &result {
                        if child_run.debug.is_none() {
                            child_run.report_execution_error(error);
                        }
                    }
                    result
                }));
            }
            if *wait {
                let mut error = None;
                for h in handles {
                    if let Err(e) = h.join().map_err(|_| "Parallel worker failed")? {
                        error.get_or_insert(e);
                    }
                }
                if let Some(e) = error {
                    return Err(e);
                }
            } else if run.request.icon_signature.is_some() || run.debug.is_some() {
                run.completion_jobs.lock().unwrap().extend(handles);
            }
            Ok(())
        }
        SavedMacro {
            macro_id,
            arguments,
        } => {
            let call = format!("macro:{macro_id}");
            if run.calls.contains(&call) {
                return Err("Saved macro reference cycle".into());
            }
            let macros = run.macros.clone();
            let macro_ = macros
                .iter()
                .find(|m| m.id == *macro_id)
                .ok_or("Saved macro missing")?;
            check_macro_plugins(run, &macro_.action)?;
            let inputs = crate::macro_inputs::bind(
                &macro_.inputs,
                arguments,
                &run.inputs,
                &run.locals,
                |name| {
                    if let Some(frozen) = &run.parameters {
                        let profile = frozen
                            .profiles
                            .iter()
                            .find(|p| {
                                p.id == run.request.binding_scope.split(':').next().unwrap_or("")
                            })
                            .ok_or("Parameter profile is missing")?;
                        let mut overrides = frozen.overrides.clone();
                        run.runtime.resolve_sensitive_override(
                            profile,
                            &mut overrides,
                            &run.request.device_id,
                            name,
                        )?;
                        crate::parameters::resolve(
                            profile,
                            &overrides,
                            &run.request.device_id,
                            name,
                        )
                    } else {
                        run.runtime.resolve_parameter(
                            name,
                            &run.request.device_id,
                            &run.request.binding_scope,
                        )
                    }
                },
            )?;
            let previous_inputs = std::mem::replace(&mut run.inputs, inputs);
            let action = &macro_.action;
            run.calls.push(call);
            let mut path = path;
            path.push(0);
            let result = execute(run, action, depth + 1, path);
            run.inputs = previous_inputs;
            run.calls.pop();
            result
        }
        RandomAction { children } => {
            if children.is_empty() {
                return Err("Add random action branches".into());
            }
            let n = children.len() as u64;
            let limit = (1u64 << 48) - (1u64 << 48) % n;
            let index = loop {
                let b = *uuid::Uuid::new_v4().as_bytes();
                let v = u64::from_be_bytes([0, 0, b[0], b[1], b[2], b[3], b[4], b[5]]);
                if v < limit {
                    break (v % n) as usize;
                }
            };
            branch(run, children, index, depth, &path)
        }
        ResetState { .. } => {
            state.indices.lock().unwrap().clear();
            state.cooldowns.lock().unwrap().clear();
            state.cleanup(None);
            Ok(())
        }
        Cooldown {
            milliseconds,
            children,
        } => {
            let mut times = state.cooldowns.lock().unwrap();
            if times
                .get(&key)
                .is_some_and(|t| t.elapsed().as_millis() < *milliseconds as u128)
            {
                return Ok(());
            }
            times.insert(key, Instant::now());
            drop(times);
            branch(run, children, 0, depth, &path)
        }
        BindingLayer {
            page_id,
            controls,
            toggle,
        } => {
            let snapshot = run.runtime.snapshot();
            let session = snapshot.session(&device);
            if !snapshot
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id)
                .is_some_and(|p| p.pages.iter().any(|p| p.id == *page_id))
            {
                return Err("Layer source page missing".into());
            }
            {
                let mut layers = state.layers.lock().unwrap();
                state.layer_revision.fetch_add(1, Ordering::SeqCst);
                let list = layers.entry(device.clone()).or_default();
                if *toggle && list.iter().any(|l| l.owner == key) {
                    list.retain(|l| l.owner != key);
                    drop(layers);
                    run.runtime
                        .emit("sessions", Some(device.clone()), None, None);
                    let _ = run
                        .runtime
                        .device_commands
                        .send(crate::devices::Request::Restore);
                    return Ok(());
                }
                list.push(Layer {
                    owner: key.clone(),
                    profile: session.profile_id,
                    page: page_id.clone(),
                    controls: controls.clone(),
                });
            }
            run.runtime
                .emit("sessions", Some(device.clone()), None, None);
            let _ = run
                .runtime
                .device_commands
                .send(crate::devices::Request::Restore);
            let result = if *toggle { Ok(()) } else { run.release(0) };
            if !*toggle {
                {
                    let mut layers = state.layers.lock().unwrap();
                    layers
                        .entry(device.clone())
                        .or_default()
                        .retain(|l| l.owner != key);
                    state.layer_revision.fetch_add(1, Ordering::SeqCst);
                }
                run.runtime.emit("sessions", Some(device), None, None);
                let _ = run
                    .runtime
                    .device_commands
                    .send(crate::devices::Request::Restore);
            }
            result
        }
        TriggerBinding {
            device_id,
            profile_id,
            page_id,
            control_id,
        } => {
            let device = if device_id.is_empty() {
                device
            } else {
                device_id.clone()
            };
            let snapshot = run.runtime.snapshot();
            let session = snapshot.session(&device);
            let profile = if profile_id.is_empty() {
                &session.profile_id
            } else {
                profile_id
            };
            let page = if page_id.is_empty() {
                &session.page_id
            } else {
                page_id
            };
            let call = format!("binding:{device}:{profile}:{page}:{control_id}");
            if run.calls.contains(&call) {
                return Err("Binding reference cycle".into());
            }
            let profiles = run
                .parameters
                .as_ref()
                .map(|p| p.profiles.as_ref())
                .unwrap_or(&snapshot.profiles);
            let target_profile = profiles
                .iter()
                .find(|p| p.id == *profile)
                .ok_or("Target profile is missing")?;
            let models = hardware_registry::manifests();
            let target_control = models
                .iter()
                .find(|model| model.model_id == target_profile.model_id)
                .ok_or("Target model is unavailable")?
                .control(control_id)
                .map_err(|e| e.to_string())?;
            if target_control.kind == "display" {
                return Err("A display-only surface has no action input".into());
            }
            let action = target_profile
                .pages
                .iter()
                .find(|p| p.id == *page)
                .and_then(|p| p.bindings.get(control_id))
                .filter(|b| !b.disabled)
                .ok_or("Target binding missing or disabled")?
                .action
                .clone();
            let mut child = run.clone();
            child.calls.push(call);
            Arc::make_mut(&mut child.request).manual = false;
            Arc::make_mut(&mut child.request).device_id = device.clone();
            Arc::make_mut(&mut child.request).control_id = control_id.clone();
            Arc::make_mut(&mut child.request).binding_scope = if run.debug.is_some() {
                format!("{profile}:{page}:debug:{}", run.id)
            } else {
                format!("{profile}:{page}")
            };
            let epoch = *run
                .runtime
                .connection_epochs
                .lock()
                .unwrap()
                .get(&device)
                .ok_or("Target device disconnected")?;
            Arc::make_mut(&mut child.request).connection_epoch = Some(epoch);
            child.device_generation = run
                .runtime
                .builtin_state
                .device_cancellations
                .lock()
                .unwrap()
                .get(&device)
                .map(|value| value.load(Ordering::SeqCst))
                .unwrap_or(0);
            if run
                .runtime
                .builtin_state
                .paused
                .lock()
                .unwrap()
                .contains_key(&device)
                && !matches!(&action,Action::Builtin{action}if matches!(**action,BuiltinAction::PauseBindings{..}))
            {
                return Err("Target device bindings are paused".into());
            }
            Arc::make_mut(&mut child.request).input = Input::Pulse;
            child.pressed = Arc::new(AtomicBool::new(false));
            execute(&mut child, &action, depth + 1, vec![])
        }
        ShowManager | EditBinding { .. } => {
            if !state.desktop_host.load(Ordering::SeqCst) {
                return Err("This action requires the desktop application".into());
            }

            run.runtime.emit(
                "action-show-manager",
                Some(device),
                None,
                Some(serde_json::to_string(a).map_err(|e| e.to_string())?),
            );
            Ok(())
        }
        PauseBindings { operation } => {
            let mut paused = state.paused.lock().unwrap();
            let pause = match operation.as_str() {
                "pause" => true,
                "resume" => false,
                _ => !paused.contains_key(&device),
            };
            if pause {
                paused.insert(device.clone(), run.request.control_id.clone());
                drop(paused);
                let generation = {
                    let mut cancellations = state.device_cancellations.lock().unwrap();
                    let generation = cancellations.entry(device.clone()).or_default();
                    generation.fetch_add(1, Ordering::SeqCst) + 1
                };
                // Pausing is itself a successful root action; cancel other runs
                // without treating this button's completed pause as a failure
                if depth == 0 {
                    run.device_generation = generation;
                }
                state.cleanup(Some(&device));
            } else {
                paused.remove(&device);
            }
            run.runtime.emit("sessions", Some(device), None, None);
            Ok(())
        }
        OpenWith {
            application,
            path,
            template,
        } => native::open_with(
            &run.text(application, *template)?,
            &run.text(path, *template)?,
        ),
        OpenTerminal {
            program,
            path,
            template,
        } => native::terminal(&run.text(program, *template)?, &run.text(path, *template)?),
        CommandResult {
            program,
            args,
            path,
            timeout_ms,
            output,
            template,
        } => {
            let args = args
                .iter()
                .map(|a| run.text(a, *template))
                .collect::<Result<Vec<_>, _>>()?;
            let result = native::command_result(
                &run.text(program, *template)?,
                &args,
                &run.text(path, *template)?,
                *timeout_ms,
                || run.check(),
            )?;
            run.locals.insert(output.clone(), result.clone());
            for key in ["stdout", "stderr", "exit_status"] {
                run.locals
                    .insert(format!("{output}.{key}"), result[key].clone());
            }
            if result["exit_status"] != 0 {
                return Err("Command exited unsuccessfully; result retained".into());
            }
            Ok(())
        }
        FileOperation {
            operation,
            source,
            destination,
            replace,
            template,
        } => native::file_operation(
            operation,
            &run.text(source, *template)?,
            &run.text(destination, *template)?,
            *replace,
        ),
        WriteFile {
            path,
            text,
            operation,
            replace,
            template,
        } => native::write_file(
            &run.text(path, *template)?,
            &run.text(text, *template)?,
            operation,
            *replace,
        ),
        ChoosePath { directory, output } => {
            let result = run.ui("path", json!({"directory":directory}))?;
            run.locals.insert(output.clone(), json!(result));
            Ok(())
        }
    }
}
// Navigate to the requested builtins destination
fn navigate(run: &Run, profile: &str, page: &str, remember: bool) -> Result<(), String> {
    let old = run.runtime.snapshot().session(&run.request.device_id);
    run.runtime
        .select_device_profile(&run.request.device_id, profile, Some(page), false)?;
    if remember {
        let mut history = run.runtime.builtin_state.history.lock().unwrap();
        let history = history.entry(run.request.device_id.clone()).or_default();
        history.push((old.profile_id, old.page_id));
        if history.len() > 64 {
            history.remove(0);
        }
    }
    Ok(())
}
// Transform the builtins value
fn transform(text: &str, op: &str) -> String {
    let words: Vec<_> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match op {
        "uppercase" => text.to_uppercase(),
        "lowercase" => text.to_lowercase(),
        "trim" => text.trim().into(),
        "title" => words
            .iter()
            .map(|w| {
                let mut chars = w.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().to_string() + &chars.as_str().to_lowercase())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
        "snake_case" => words.join("_").to_lowercase(),
        "kebab-case" => words.join("-").to_lowercase(),
        "camelCase" => {
            let mut result = String::new();
            for (i, w) in words.iter().enumerate() {
                if i == 0 {
                    result.push_str(&w.to_lowercase())
                } else {
                    let mut c = w.chars();
                    if let Some(first) = c.next() {
                        result.push_str(&first.to_uppercase().to_string());
                        result.push_str(&c.as_str().to_lowercase())
                    }
                }
            }
            result
        }
        _ => text.into(),
    }
}

// Validate macro graph
pub fn validate_macro_graph(macros: &[SavedMacro]) -> Result<(), String> {
    for saved in macros {
        crate::keyboard_macro::validation::expanded_event_count(&saved.action, |id| {
            macros
                .iter()
                .find(|saved| saved.id == id)
                .map(|saved| &saved.action)
        })?;
    }
    validate_macro_graph_refs(&macros.iter().collect::<Vec<_>>())
}
// Validate macro graph refs
fn validate_macro_graph_refs(macros: &[&SavedMacro]) -> Result<(), String> {
    // Check the current builtins conditions
    fn check(
        action: &Action,
        macros: &[&SavedMacro],
        stack: &mut Vec<String>,
        depth: usize,
        checked: &mut std::collections::HashSet<(String, usize)>,
        caller: &[crate::macro_inputs::MacroInput],
    ) -> Result<(), String> {
        if depth > 8 {
            return Err("Expanded macro nesting exceeds 8".into());
        }
        if let Action::Builtin { action: a } = action {
            if let BuiltinAction::SavedMacro {
                macro_id,
                arguments,
            } = &**a
            {
                if stack.contains(macro_id) {
                    return Err("Saved macro reference cycle".into());
                }
                if let Some(m) = macros.iter().find(|m| m.id == *macro_id) {
                    crate::macro_inputs::validate_call(m, arguments, caller)?;
                    // Shared macros can form wide DAGs. Validate each expansion depth once
                    // rather than repeatedly expanding every path in an imported archive
                    let key = (macro_id.clone(), depth + 1);
                    if !checked.contains(&key) {
                        stack.push(macro_id.clone());
                        check(&m.action, macros, stack, depth + 1, checked, &m.inputs)?;
                        stack.pop();
                        checked.insert(key);
                    }
                }
            }
        }
        if let Action::Toggle { on, off } = action {
            check(on, macros, stack, depth + 1, checked, caller)?;
            check(off, macros, stack, depth + 1, checked, caller)?;
        } else {
            for c in action.children() {
                check(c, macros, stack, depth + 1, checked, caller)?
            }
        }
        Ok(())
    }
    let mut checked = std::collections::HashSet::new();
    for m in macros {
        crate::macro_inputs::validate_definitions(&m.inputs)?;
        check(
            &m.action,
            macros,
            &mut vec![m.id.clone()],
            0,
            &mut checked,
            &m.inputs,
        )?;
    }
    Ok(())
}
// Remap macros
pub fn remap_macros(data: &mut crate::ProfileData, map: &HashMap<String, String>) {
    // Apply the builtins correction
    fn fix(action: &mut Action, map: &HashMap<String, String>) {
        if let Action::Builtin { action: a } = action {
            if let BuiltinAction::SavedMacro { macro_id, .. } = &mut **a {
                if let Some(id) = map.get(macro_id) {
                    *macro_id = id.clone();
                }
            }
        }
        if let Action::Toggle { on, off } = action {
            fix(on, map);
            fix(off, map);
        } else {
            for child in action.children_mut() {
                fix(child, map);
            }
        }
    }
    for macro_ in &mut data.saved_macros {
        if let Some(id) = map.get(&macro_.id) {
            macro_.id = id.clone();
        }
        fix(&mut macro_.action, map);
    }
    for profile in &mut data.profiles {
        for page in &mut profile.pages {
            for binding in page.bindings.values_mut() {
                fix(&mut binding.action, map);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Build a test action from catalog defaults with explicit field overrides
    pub(super) fn action(id: &str, overrides: Value) -> Action {
        let entry = catalog().iter().find(|e| e["id"] == id).unwrap();
        let mut fields = entry["defaults"].clone();
        fields["kind"] = json!(id);
        for (k, v) in overrides.as_object().unwrap() {
            fields[k] = v.clone();
        }
        serde_json::from_value(json!({"kind":"builtin","action":fields})).unwrap()
    }
    // Construct an isolated builtins fixture
    pub(super) fn fixture(root: &std::path::Path) -> Run {
        let runtime = Runtime::build(root.join("app"), false).unwrap();
        runtime.builtin_state.attach_desktop_host();
        runtime.simulate_device(true, "simulator:builtins").unwrap();
        let epoch = *runtime
            .connection_epochs
            .lock()
            .unwrap()
            .get("simulator:builtins")
            .unwrap();
        Run {
            id: crate::actions::RunId(uuid::Uuid::new_v4().to_string()),
            _debug_state: None,
            debug: None,
            simulation: None,
            parameters: None,
            runtime,
            dial_plugin_input: None,
            key_logic: false,
            debug_macro_root: false,
            request: Arc::new(Request {
                manual: false,
                activation_policy: Default::default(),
                icon_signature: None,
                pressed: None,
                action: Action::None,
                queued_at: Instant::now(),
                device_id: "simulator:builtins".into(),
                control_id: "key-1".into(),
                binding_scope: "default:main".into(),
                connection_epoch: Some(epoch),
                input: Input::Button { pressed: true },
                cancellation_generation: 0,
            }),
            pressed: Arc::new(AtomicBool::new(false)),
            locals: BTreeMap::new(),
            inputs: Default::default(),
            macros: Default::default(),
            calls: vec![],
            target: Some("mock-target".into()),
            device_generation: 0,
            branch_cancellations: vec![],
            completion_jobs: Default::default(),
            keyboard_events: Default::default(),
        }
    }
    #[test]
    // Verify that retained catalog entries round trip with typed editable defaults
    fn retained_catalog_entries_round_trip_with_typed_editable_defaults() {
        let mut ids = std::collections::HashSet::new();
        assert_eq!(catalog().len(), 51);
        for entry in catalog() {
            let id = entry["id"].as_str().unwrap();
            assert!(ids.insert(id));
            let a = action(id, json!({}));
            if id == "keyboard_macro" {
                // Empty editable drafts are deliberately not executable/saveable
                assert!(a.validate(0).unwrap_err().contains("recording.events"));
            } else {
                a.validate(0).unwrap_or_else(|e| panic!("{id}: {e}"));
            }
            assert_eq!(serde_json::to_value(a).unwrap()["action"]["kind"], id);
            for field in entry["fields"].as_array().unwrap() {
                assert!(entry["defaults"]
                    .get(field["key"].as_str().unwrap())
                    .is_some());
            }
        }
    }
    #[test]
    // Verify that clipboard values are atomic and persist only when opted in
    fn clipboard_values_are_atomic_and_persist_only_when_opted_in() {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::new(root.path().join("values.json")));
        state.store("clipboard:n".into(), json!(0), true).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        state
                            .mutate("clipboard:n".into(), true, |v| {
                                Ok(json!(v.unwrap().as_i64().unwrap() + 1))
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        state
            .store("clipboard:private".into(), json!("sensitive"), false)
            .unwrap();
        let restored = State::with_storage(state.storage.clone()).unwrap();
        assert_eq!(restored.get("clipboard:n"), Some(json!(160)));
        assert_eq!(restored.get("clipboard:private"), None);
        assert!(!state
            .storage
            .read_json::<Value>(
                stream_dock_manager_storage::Domain::LocalPreferences,
                "builtin-values",
                persisted_values::MAX_FILE_BYTES
            )
            .unwrap()
            .unwrap()
            .to_string()
            .contains("sensitive"));
    }
    #[test]
    // Verify that sequence conditions cycles repeats and run outputs compose
    fn sequence_conditions_cycles_repeats_and_run_outputs_compose() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let file = root.path().join("counter");
        let append = action(
            "write_file",
            json!({"path":file,"text":"x","operation":"append"}),
        );
        execute(
            &mut run,
            &action("repeat_actions", json!({"count":3,"children":[append]})),
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "xxx");
        let condition = action(
            "conditional",
            json!({"condition":{"source":"file","subject":file,"operator":"exists","value":true},"children":[action("write_file",json!({"path":file,"text":"branch","operation":"replace","replace":true})),Action::None]}),
        );
        execute(&mut run, &condition, 0, vec![]).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "branch");
        run.locals.insert("answer".into(), json!(42));
        assert_eq!(run.text("n=${run.answer}", true).unwrap(), "n=42");
        let cycle = action(
            "cycle_actions",
            json!({"children":[action("write_file",json!({"path":file,"text":"first","operation":"replace","replace":true})),action("write_file",json!({"path":file,"text":"second","operation":"replace","replace":true}))]}),
        );
        execute(&mut run, &cycle, 0, vec![1]).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");
        execute(&mut run, &cycle, 0, vec![1]).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        run.runtime.shutdown();
    }
    #[test]
    // Verify that command and file adapters use real temporary resources
    fn command_and_file_adapters_use_real_temporary_resources() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        let copied = root.path().join("copied.txt");
        native::write_file(source.to_str().unwrap(), "héllo", "create", false).unwrap();
        assert!(native::write_file(source.to_str().unwrap(), "bad", "create", false).is_err());
        native::write_file(source.to_str().unwrap(), " world", "append", false).unwrap();
        native::file_operation(
            "copy",
            source.to_str().unwrap(),
            copied.to_str().unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "héllo world");
        assert!(native::file_operation(
            "copy",
            source.to_str().unwrap(),
            copied.to_str().unwrap(),
            false
        )
        .is_err());
        #[cfg(unix)]
        {
            let output = native::command_result(
                "/bin/sh",
                &["-c".into(), "printf 'ok'; printf 'warning' >&2".into()],
                root.path().to_str().unwrap(),
                1000,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(output["stdout"], "ok");
            assert_eq!(output["stderr"], "warning");
            assert_eq!(output["exit_status"], 0);
            assert!(native::command_result(
                "/bin/sh",
                &["-c".into(), "sleep 10".into()],
                "",
                30,
                || Ok(())
            )
            .unwrap_err()
            .contains("timed out"));
        }
    }
    #[test]
    // Verify that macro cycles and expanded depth are rejected
    fn macro_cycles_and_expanded_depth_are_rejected() {
        let one = SavedMacro {
            inputs: Default::default(),
            id: "one".into(),
            name: "One".into(),
            action: action("saved_macro", json!({"macro_id":"two"})),
        };
        let two = SavedMacro {
            inputs: Default::default(),
            id: "two".into(),
            name: "Two".into(),
            action: action("saved_macro", json!({"macro_id":"one"})),
        };
        assert!(validate_macro_graph(&[one, two])
            .unwrap_err()
            .contains("cycle"));
    }
    #[test]
    // Verify that dial dispatch uses the effective layer binding and preserves the raw report
    fn dial_dispatch_uses_the_effective_layer_binding_and_preserves_the_raw_report() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let runtime = &run.runtime;
        let device = "simulator:builtins";
        let snapshot = runtime.snapshot();
        let session = snapshot.session(device);
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .clone();
        let control = snapshot
            .devices
            .iter()
            .find(|d| d.id == device)
            .unwrap()
            .manifest
            .controls
            .iter()
            .find(|c| c.kind == "encoder")
            .unwrap()
            .id
            .clone();
        let mut binding: crate::Binding = serde_json::from_value(json!({"title":"Dial", "color":"#000000", "action":{"kind":"dial", "actions":[{"kind":"none"},{"kind":"builtin", "action":{"kind":"brightness_step", "delta":-1}},{"kind":"none"}]}})).unwrap();
        binding.dial_response = Some(crate::dial_response::DialResponse {
            curve: crate::dial_response::DialCurve::Linear,
            max_steps: 1,
        });
        profile
            .pages
            .iter_mut()
            .find(|p| p.id == session.page_id)
            .unwrap()
            .bindings
            .insert(control.clone(), binding.clone());
        binding.dial_response.as_mut().unwrap().max_steps = 3;
        profile.pages.push(crate::Page {
            id: "dial-layer".into(),
            name: "Dial layer".into(),
            bindings: BTreeMap::from([(control.clone(), binding)]),
        });
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let mut output = session.output.clone();
        output.brightness = 50;
        runtime.set_device_output(device, output).unwrap();
        runtime.builtin_state.layers.lock().unwrap().insert(
            device.into(),
            vec![Layer {
                owner: "test".into(),
                profile: session.profile_id,
                page: "dial-layer".into(),
                controls: vec![control.clone()],
            }],
        );
        let report = hardware_core::ControlEvent {
            control_id: control,
            input: Input::Rotate { delta: 9 },
        };
        runtime.dispatch(device, &report);
        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime.snapshot().session(device).output.brightness == 50
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(runtime.snapshot().session(device).output.brightness, 47);
        assert_eq!(report.input, Input::Rotate { delta: 9 });
        runtime.shutdown();
    }
    #[test]
    // Verify that input revocation cancels jobs and removes temporary overrides
    fn input_revocation_cancels_jobs_and_removes_temporary_overrides() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let state = &run.runtime.builtin_state;
        let pressed = Arc::new(AtomicBool::new(true));
        state.captures.lock().unwrap().insert(
            "simulator:builtins|key-1".into(),
            Capture {
                action: Action::None,
                scope: "default:main".into(),
                pressed: pressed.clone(),
            },
        );
        state.layers.lock().unwrap().insert(
            "simulator:builtins".into(),
            vec![Layer {
                owner: run.key(),
                profile: "default".into(),
                page: "main".into(),
                controls: vec![],
            }],
        );
        let mut old_generation = native::input_generation().wrapping_sub(1);
        reconcile_input_generation(&run.runtime, &mut old_generation);
        assert_eq!(old_generation, native::input_generation());
        assert!(!pressed.load(Ordering::SeqCst));
        assert!(state.layers.lock().unwrap().is_empty());
        assert_eq!(run.check().unwrap_err(), "Cancelled");
        let cancellation = run.runtime.cancellation.load(Ordering::SeqCst);
        reconcile_input_generation(&run.runtime, &mut old_generation);
        assert_eq!(
            run.runtime.cancellation.load(Ordering::SeqCst),
            cancellation
        );
        run.runtime.shutdown();
    }
    #[test]
    // Verify that popup replies are correlated single use and cancelable
    fn popup_replies_are_correlated_single_use_and_cancelable() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("values.json"));
        let (tx, rx) = mpsc::sync_channel(1);
        state
            .pending
            .lock()
            .unwrap()
            .insert("live".into(), (json!({"id":"live"}), tx));
        assert!(state.reply_ui("stale", Some("wrong".into())).is_err());
        state.reply_ui("live", None).unwrap();
        assert_eq!(rx.recv().unwrap(), Ok(None));
        assert!(state.reply_ui("live", Some("replay".into())).is_err());
    }
    #[test]
    // Verify that text input and prompt actions use injected native adapter
    fn text_input_and_prompt_actions_use_injected_native_adapter() {
        native::testing::enable();
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        for (id, fields) in [
            ("type_text", json!({"text":"héllo\n世界"})),
            ("paste_text", json!({"text":"snippet"})),
            ("set_clipboard", json!({"text":"saved"})),
            ("clipboard_slot", json!({"name":"slot","operation":"save"})),
            ("set_clipboard", json!({"text":"other"})),
            (
                "clipboard_slot",
                json!({"name":"slot","operation":"restore"}),
            ),
            ("input_prompt", json!({})),
            ("choose_path", json!({})),
            ("mouse_click", json!({})),
            ("mouse_move", json!({})),
            ("mouse_scroll", json!({})),
            ("mouse_drag", json!({"milliseconds":0})),
            ("hold_keys", json!({})),
            ("latch_keys", json!({})),
            ("open_with", json!({})),
            ("open_terminal", json!({})),
            ("reveal_file", json!({})),
            ("open_link", json!({})),
            ("transform_text", json!({})),
            ("date_time", json!({})),
        ] {
            execute(&mut run, &action(id, fields), 0, vec![])
                .unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        assert_eq!(run.locals["input"], "test input");
        assert_eq!(native::testing::clipboard(), "saved");
        native::testing::STATE.with(|s| {
            let s = s.borrow();
            assert!(s.events.contains(&"héllo\n世界".into()));
            assert!(s.events.contains(&"snippet".into()));
            assert!(s.events.contains(&"SAVED".into()));
        });
        run.runtime.shutdown();
    }
    #[test]
    // Verify that wait timeout and cancellation do not execute following steps
    fn wait_timeout_and_cancellation_do_not_execute_following_steps() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let wait = action(
            "wait_condition",
            json!({"condition":{"source":"file","subject":root.path().join("absent").to_string_lossy(),"operator":"exists","value":true},"timeout_ms":10,"poll_ms":20}),
        );
        assert!(execute(&mut run, &wait, 0, vec![])
            .unwrap_err()
            .contains("timed out"));
        run.runtime.cancel_macros();
        assert_eq!(
            execute(
                &mut run,
                &action("write_file", json!({"path":root.path().join("must_not_run"),"text":"true","operation":"replace","replace":true})),
                0,
                vec![]
            )
            .unwrap_err(),
            "Cancelled"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("must_not_run")).ok(),
            None
        );
        run.runtime.shutdown();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    // Verify that portable macro dependencies are pruned remapped and redacted
    fn portable_macro_dependencies_are_pruned_remapped_and_redacted() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let plugin = Action::Plugin {
            user_desired_state: None,
            plugin_id: "example".into(),
            action_id: "action".into(),
            settings: json!({"secret":"private"}),
            resources: Default::default(),
        };
        data.saved_macros = vec![
            SavedMacro {
                inputs: Default::default(),
                id: "used".into(),
                name: "Used".into(),
                action: Action::Builtin {
                    action: Box::new(BuiltinAction::Parallel {
                        wait: true,
                        children: vec![plugin],
                    }),
                },
            },
            SavedMacro {
                inputs: Default::default(),
                id: "unused".into(),
                name: "Unused".into(),
                action: Action::None,
            },
        ];
        data.profiles[0].pages[0].bindings.insert(
            "key-1".into(),
            crate::Binding {
                dial_response: None,
                title: "Macro".into(),
                color: "#222222".into(),
                custom_background: false,
                background_source: None,
                action: Action::Builtin {
                    action: Box::new(BuiltinAction::SavedMacro {
                        arguments: Default::default(),
                        macro_id: "used".into(),
                    }),
                },
                asset: None,
                media: Default::default(),
                plugin_appearance: None,
                composition: None,
                appearance: None,
                feedback: None,
                activation_policy: Default::default(),
                disabled: false,
            },
        );
        store.save(&data).unwrap();
        let archive = root.path().join("template.zip");
        store
            .export_with_options(
                &archive,
                &crate::profiles::ProfileExportOptions {
                    profile_ids: Some(vec![data.profiles[0].id.clone()]),
                    include_plugin_settings: false,
                },
            )
            .unwrap();
        let mut imported = store.import_for_repair(&archive).unwrap();
        assert_eq!(imported.saved_macros.len(), 1);
        assert_eq!(imported.saved_macros[0].id, "used");
        let bytes = serde_json::to_string(&imported).unwrap();
        assert!(!bytes.contains("private"));
        assert!(!bytes.contains("Unused"));
        remap_macros(
            &mut imported,
            &HashMap::from([("used".into(), "remapped".into())]),
        );
        assert_eq!(imported.saved_macros[0].id, "remapped");
        assert!(serde_json::to_string(&imported.profiles)
            .unwrap()
            .contains("remapped"));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::tests::{action, fixture};
    use super::*;
    #[test]
    // Verify that every new action executes a configured handler
    fn every_new_action_executes_a_configured_handler() {
        native::testing::enable();
        for descriptor in catalog() {
            let id = descriptor["id"].as_str().unwrap();
            let root = tempfile::tempdir().unwrap();
            let mut run = fixture(root.path());
            let source = root.path().join("source.txt");
            std::fs::write(&source, "source").unwrap();
            let destination = root.path().join("destination.txt");
            {
                let mut state = run.runtime.state.lock().unwrap();
                let profile = &mut state.profiles[0];
                profile.pages.push(crate::Page {
                    id: "second".into(),
                    name: "Second".into(),
                    bindings: Default::default(),
                });
                profile.pages[0].bindings.get_mut("key-2").unwrap().action = Action::None;
            }
            run.macros = vec![SavedMacro {
                inputs: Default::default(),
                id: "fixture".into(),
                name: "Fixture".into(),
                action: Action::None,
            }]
            .into();
            let fields = match id {
                "keyboard_macro" => json!({"recording":{
                    "version":1,"platform":"unknown","key_mode":"physical","events":[
                        {"kind":"key_down","key":"KeyA","delay_ms":0},
                        {"kind":"key_up","key":"KeyA","delay_ms":1}
                    ]
                }}),
                "momentary_page" | "binding_layer" => json!({"page_id":"second"}),
                "saved_macro" => json!({"macro_id":"fixture"}),
                "trigger_binding" => json!({"control_id":"key-2"}),
                "gesture" => json!({"double_ms":1,"hold_ms":1}),
                "wait_condition" => {
                    json!({"condition":{"source":"file","subject":source.to_string_lossy(),"operator":"exists","value":true}})
                }
                "mouse_drag" => json!({"milliseconds":0}),
                "search_selection" => json!({"source":"clipboard"}),
                "write_file" => json!({"path":destination.to_string_lossy(),"text":"written"}),
                "file_operation" => {
                    json!({"source":source.to_string_lossy(),"destination":destination.to_string_lossy()})
                }
                "command_result" => {
                    if cfg!(windows) {
                        json!({"program":"cmd.exe","args":["/C","echo output"]})
                    } else {
                        json!({"program":"/bin/sh","args":["-c","printf output"]})
                    }
                }
                _ => json!({}),
            };
            execute(&mut run, &action(id, fields), 0, vec![])
                .unwrap_or_else(|error| panic!("{id}: {error}"));
            if id == "keyboard_macro" {
                native::testing::STATE.with(|state| {
                    assert!(state
                        .borrow()
                        .events
                        .iter()
                        .any(|event| event == "keyboard_macro"));
                });
            }
            if id == "command_result" {
                assert!(run.locals["command.stdout"]
                    .as_str()
                    .unwrap()
                    .contains("output"));
            }
            if id == "write_file" {
                assert_eq!(std::fs::read_to_string(&destination).unwrap(), "written");
            }
            if id == "file_operation" {
                assert_eq!(std::fs::read_to_string(&destination).unwrap(), "source");
            }
            run.runtime.shutdown();
        }
    }
    #[test]
    // Verify that held repetition and hold gestures observe release
    fn held_repetition_and_hold_gestures_observe_release() {
        native::testing::enable();
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        run.pressed.store(true, Ordering::SeqCst);
        let pressed = run.pressed.clone();
        let repeated = root.path().join("repeated");
        let release = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let repeated_twice = loop {
                if std::fs::read(&repeated).is_ok_and(|bytes| bytes.len() >= 2) {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            pressed.store(false, Ordering::SeqCst);
            repeated_twice
        });
        let child = action(
            "write_file",
            json!({"path":root.path().join("repeated"),"text":"x","operation":"append"}),
        );
        execute(
            &mut run,
            &action("repeat_held", json!({"interval_ms":20,"children":[child]})),
            0,
            vec![],
        )
        .unwrap();
        assert!(release.join().unwrap(), "Held action did not repeat");
        assert!(
            std::fs::read_to_string(root.path().join("repeated"))
                .unwrap()
                .len()
                >= 2
        );
        run.pressed.store(true, Ordering::SeqCst);
        let pressed = run.pressed.clone();
        let held = root.path().join("held");
        let release = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let held_action_ran = loop {
                if std::fs::read_to_string(&held).is_ok_and(|text| text == "true") {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            pressed.store(false, Ordering::SeqCst);
            held_action_ran
        });
        let hold = action(
            "write_file",
            json!({"path":root.path().join("held"),"text":"true","operation":"replace","replace":true}),
        );
        execute(
            &mut run,
            &action(
                "gesture",
                json!({"hold_ms":5,"children":[Action::None,Action::None,hold]}),
            ),
            0,
            vec![1],
        )
        .unwrap();
        assert!(release.join().unwrap(), "Hold gesture did not run");
        assert_eq!(
            std::fs::read_to_string(root.path().join("held")).ok(),
            Some("true".into())
        );
        run.runtime.shutdown();
    }
    #[test]
    // Verify that parallel branches isolate outputs and cancel siblings on failure
    fn parallel_branches_isolate_outputs_and_cancel_siblings_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let a = action(
            "write_file",
            json!({"path":root.path().join("a"),"text":"true","operation":"replace","replace":true}),
        );
        let b = action(
            "write_file",
            json!({"path":root.path().join("b"),"text":"true","operation":"replace","replace":true}),
        );
        execute(
            &mut run,
            &action("parallel", json!({"children":[a,b],"wait":true})),
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("a")).ok(),
            Some("true".into())
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("b")).ok(),
            Some("true".into())
        );
        assert!(run.locals.is_empty());
        let waiting = Action::Macro {
            actions: vec![
                Action::Delay { milliseconds: 1000 },
                action(
                    "write_file",
                    json!({"path":root.path().join("must_not_run"),"text":"true","operation":"replace","replace":true}),
                ),
            ],
        };
        let bad = action("saved_macro", json!({"macro_id":"missing"}));
        let start = Instant::now();
        assert!(execute(
            &mut run,
            &action("parallel", json!({"children":[waiting,bad],"wait":true})),
            0,
            vec![]
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(
            std::fs::read_to_string(root.path().join("must_not_run")).ok(),
            None
        );
        run.runtime.shutdown();
    }
}

// Report whether this requires release
pub fn requires_release(action: &BuiltinAction) -> bool {
    matches!(
        action,
        BuiltinAction::HoldKeys { .. }
            | BuiltinAction::MomentaryPage { .. }
            | BuiltinAction::RepeatHeld { .. }
            | BuiltinAction::Gesture { .. }
            | BuiltinAction::PressRelease { .. }
            | BuiltinAction::WaitRelease { .. }
            | BuiltinAction::BindingLayer { toggle: false, .. }
    )
}

/// Resolve an expanded execution path to its editable binding or saved-macro root
pub fn resolve_reference_path(
    action: &Action,
    path: &[usize],
    macros: &[SavedMacro],
) -> Result<(Option<String>, Vec<usize>), String> {
    let mut node = action;
    let mut owner = None;
    let mut local = vec![];
    if path.len() > 8 {
        return Err("Action address exceeds 8 levels".into());
    }
    for index in path {
        if let Action::Builtin { action } = node {
            if let BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                if *index != 0 {
                    return Err("Saved macro branch missing".into());
                }
                let macro_ = macros
                    .iter()
                    .find(|m| m.id == *macro_id)
                    .ok_or("Saved macro missing")?;
                owner = Some(macro_.id.clone());
                local.clear();
                node = &macro_.action;
                continue;
            }
        }
        node = node.child(*index).ok_or("Action branch missing")?;
        local.push(*index);
    }
    Ok((owner, local))
}

#[cfg(test)]
mod routing_tests {
    use super::tests::{action, fixture};
    use super::*;
    // Wait for the builtins condition or result
    fn wait(mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(condition(), "Timed out waiting for action");
    }
    #[test]
    // Verify that queued requests are fifo and other controls remain responsive
    fn queued_requests_are_fifo_and_other_controls_remain_responsive() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let log = root.path().join("ordered.txt");
        let other = root.path().join("other.txt");
        for index in 0..10 {
            let mut request = (*run.request).clone();
            request.input = Input::Pulse;
            let write = action(
                "write_file",
                json!({"path":log.to_string_lossy(),"text":format!("{index}\n"),"operation":"append"}),
            );
            request.action = if index == 0 {
                Action::Macro {
                    actions: vec![Action::Delay { milliseconds: 400 }, write],
                }
            } else {
                write
            };
            run.runtime.action_sender.send(request).unwrap();
        }
        let mut request = (*run.request).clone();
        request.control_id = "key-2".into();
        request.input = Input::Pulse;
        request.action = action(
            "write_file",
            json!({"path":other.to_string_lossy(),"text":"independent"}),
        );
        run.runtime.action_sender.send(request).unwrap();
        wait(|| other.exists());
        assert!(!log.exists());
        wait(|| std::fs::read_to_string(&log).is_ok_and(|text| text.lines().count() == 10));
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            (0..10).map(|i| format!("{i}\n")).collect::<String>()
        );
        run.runtime.shutdown();
    }
    #[test]
    // Verify that release returns to the original page and duplicate down is ignored
    fn release_returns_to_the_original_page_and_duplicate_down_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let snapshot = run.runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.pages.push(crate::Page {
            id: "held".into(),
            name: "Held".into(),
            bindings: Default::default(),
        });
        profile.pages[0].bindings.get_mut("key-1").unwrap().action =
            action("momentary_page", json!({"page_id":"held"}));
        run.runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        run.runtime
            .simulate_input(
                "simulator:builtins",
                hardware_core::ControlEvent {
                    control_id: "key-1".into(),
                    input: Input::Button { pressed: true },
                },
            )
            .unwrap();
        wait(|| run.runtime.snapshot().session("simulator:builtins").page_id == "held");
        run.runtime
            .simulate_input(
                "simulator:builtins",
                hardware_core::ControlEvent {
                    control_id: "key-1".into(),
                    input: Input::Button { pressed: true },
                },
            )
            .unwrap();
        run.runtime
            .simulate_input(
                "simulator:builtins",
                hardware_core::ControlEvent {
                    control_id: "key-1".into(),
                    input: Input::Button { pressed: false },
                },
            )
            .unwrap();
        wait(|| run.runtime.snapshot().session("simulator:builtins").page_id == "main");
        assert!(run
            .runtime
            .builtin_state
            .captures
            .lock()
            .unwrap()
            .is_empty());
        run.runtime.shutdown();
    }
    #[test]
    // Verify that saved macro plugin addresses resolve to shared configuration
    fn saved_macro_plugin_addresses_resolve_to_shared_configuration() {
        let plugin = Action::Plugin {
            user_desired_state: None,
            plugin_id: "example".into(),
            action_id: "action".into(),
            settings: json!({}),
            resources: Default::default(),
        };
        let macros = vec![SavedMacro {
            inputs: Default::default(),
            id: "shared".into(),
            name: "Shared".into(),
            action: Action::Macro {
                actions: vec![plugin],
            },
        }];
        let root = action("saved_macro", json!({"macro_id":"shared"}));
        assert_eq!(
            resolve_reference_path(&root, &[0, 0], &macros).unwrap(),
            (Some("shared".into()), vec![0])
        );
        assert_eq!(
            resolve_reference_path(
                &Action::Macro {
                    actions: vec![Action::None]
                },
                &[0],
                &macros
            )
            .unwrap(),
            (None, vec![0])
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::tests::{action, fixture};
    use super::*;

    #[test]
    // Verify that hardware cancel bypasses a full action queue
    fn hardware_cancel_bypasses_a_full_action_queue() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let snapshot = run.runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().action =
            action("cancel_macros", json!({}));
        run.runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let before = run.runtime.cancellation.load(Ordering::SeqCst);
        run.runtime
            .builtin_state
            .active
            .store(128, Ordering::SeqCst);
        run.runtime
            .simulate_input(
                "simulator:builtins",
                hardware_core::ControlEvent {
                    control_id: "key-1".into(),
                    input: Input::Pulse,
                },
            )
            .unwrap();
        assert!(run.runtime.cancellation.load(Ordering::SeqCst) > before);
        run.runtime.builtin_state.active.store(0, Ordering::SeqCst);
        run.runtime.shutdown();
    }
    #[test]
    // Verify that desktop prompts fail clearly without a desktop host
    fn desktop_prompts_fail_clearly_without_a_desktop_host() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        run.runtime
            .builtin_state
            .desktop_host
            .store(false, Ordering::SeqCst);
        assert!(run
            .ui("input", json!({"title":"Input"}))
            .unwrap_err()
            .contains("desktop application"));
        assert!(run.runtime.builtin_state.pending.lock().unwrap().is_empty());
        run.runtime.shutdown();
    }
    #[test]
    #[cfg(unix)]
    // Verify that command timeout covers descendants holding output after parent exit
    fn command_timeout_covers_descendants_holding_output_after_parent_exit() {
        let start = Instant::now();
        assert!(native::command_result(
            "/bin/sh",
            &["-c".into(), "sleep 10 & exit 0".into()],
            "",
            50,
            || Ok(())
        )
        .unwrap_err()
        .contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

#[cfg(test)]
mod cross_device_tests {
    use super::tests::{action, fixture};
    use super::*;
    #[test]
    // Verify that target cancellation generation and pause are respected
    fn target_cancellation_generation_and_pause_are_respected() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        run.runtime
            .simulate_device(true, "simulator:target")
            .unwrap();
        run.runtime.builtin_state.cancel_device("simulator:target");
        run.runtime.state.lock().unwrap().profiles[0].pages[0]
            .bindings
            .get_mut("key-2")
            .unwrap()
            .action = action(
            "write_file",
            json!({"path":root.path().join("cross_device"),"text":"true","operation":"replace","replace":true}),
        );
        let target = action(
            "trigger_binding",
            json!({"device_id":"simulator:target","control_id":"key-2"}),
        );
        execute(&mut run, &target, 0, vec![]).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("cross_device")).ok(),
            Some("true".into())
        );
        run.runtime
            .builtin_state
            .paused
            .lock()
            .unwrap()
            .insert("simulator:target".into(), "key-1".into());
        assert!(execute(&mut run, &target, 0, vec![])
            .unwrap_err()
            .contains("paused"));
        run.runtime.shutdown();
    }
}

#[cfg(test)]
mod lexical_input_tests {
    use super::*;
    use crate::macro_inputs::{MacroArgument, MacroInput};
    // Handle the incoming builtins input
    fn input() -> MacroInput {
        MacroInput {
            name: "value".into(),
            label: "Value".into(),
            r#type: "string".into(),
            required: true,
            default: None,
        }
    }
    // Construct a saved-macro call fixture with its value argument
    fn call(id: &str, argument: MacroArgument) -> Action {
        Action::Builtin {
            action: Box::new(BuiltinAction::SavedMacro {
                macro_id: id.into(),
                arguments: BTreeMap::from([("value".into(), argument)]),
            }),
        }
    }
    #[test]
    // Verify that nested calls forward explicitly and restore the caller on success and error
    fn nested_calls_forward_explicitly_and_restore_the_caller_on_success_and_error() {
        let root = tempfile::tempdir().unwrap();
        let mut run = tests::fixture(root.path());
        let path = root.path().join("output.txt").display().to_string();
        let write = tests::action(
            "write_file",
            json!({"path":path,"text":"${input.value}","replace":true}),
        );
        run.macros = vec![
            SavedMacro {
                id: "leaf".into(),
                name: "Leaf".into(),
                inputs: vec![input()],
                action: write.clone(),
            },
            SavedMacro {
                id: "parent".into(),
                name: "Parent".into(),
                inputs: vec![input()],
                action: call(
                    "leaf",
                    MacroArgument::Input {
                        name: "value".into(),
                    },
                ),
            },
        ]
        .into();
        run.inputs.insert("value".into(), json!("caller"));
        execute(
            &mut run,
            &call(
                "parent",
                MacroArgument::Literal {
                    value: json!("callee"),
                },
            ),
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "callee");
        assert_eq!(run.inputs["value"], "caller");
        assert!(run.calls.is_empty());
        Arc::make_mut(&mut run.macros.saved)[0].action = call(
            "missing",
            MacroArgument::Literal {
                value: json!("failure"),
            },
        );
        assert!(execute(
            &mut run,
            &call(
                "parent",
                MacroArgument::Literal {
                    value: json!("callee")
                }
            ),
            0,
            vec![]
        )
        .is_err());
        assert_eq!(run.inputs["value"], "caller");
        assert!(run.calls.is_empty());
        run.runtime.shutdown();
    }
}

// Start a debug run with the selected execution inputs and debug session
pub(crate) fn launch_debug(
    runtime: Arc<Runtime>,
    request: crate::macro_debug::DebugStart,
    session: Arc<crate::macro_debug::Session>,
) -> Result<(), String> {
    use crate::macro_debug::DebugMode;
    let inputs = runtime.builtin_state.execution_inputs(&runtime);
    let standalone = request.mode == DebugMode::Simulation
        && request.profile_id.is_empty()
        && request.page_id.is_empty();
    if !standalone {
        let profile = inputs
            .profiles
            .iter()
            .find(|p| p.id == request.profile_id)
            .ok_or("Select a profile before debugging")?;
        if !profile.pages.iter().any(|p| p.id == request.page_id) {
            return Err("Debug page is missing".into());
        }
    }
    if request.device_id.len() > 512 || request.control_id.len() > 512 {
        return Err("Debug target identity is too long".into());
    }
    let epoch = runtime
        .connection_epochs
        .lock()
        .unwrap()
        .get(&request.device_id)
        .copied();
    if request.mode == DebugMode::Live && epoch.is_none() {
        return Err("Live debugging requires a connected target device".into());
    }
    let root_id = request
        .macro_id
        .clone()
        .unwrap_or_else(|| format!("debug-root-{}", session.id));
    let macros = inputs.macros.with_draft(SavedMacro {
        id: root_id.clone(),
        name: "Debug draft".into(),
        inputs: request.inputs.clone(),
        action: request.action.clone(),
    });
    validate_macro_graph_refs(&macros.iter().collect::<Vec<_>>())?;
    // Report the latched condition for builtins
    fn latched(
        action: &Action,
        macros: &MacroLibrary,
        profiles: &[crate::Profile],
        depth: usize,
    ) -> bool {
        if depth > 8 {
            return true;
        }
        if let Action::Builtin { action } = action {
            if matches!(**action, BuiltinAction::LatchKeys { .. }) {
                return true;
            }
            if let BuiltinAction::TriggerBinding {
                profile_id,
                page_id,
                control_id,
                ..
            } = &**action
            {
                if profiles
                    .iter()
                    .filter(|p| profile_id.is_empty() || p.id == *profile_id)
                    .flat_map(|p| &p.pages)
                    .filter(|p| page_id.is_empty() || p.id == *page_id)
                    .filter_map(|p| p.bindings.get(control_id))
                    .filter(|b| !b.disabled)
                    .any(|b| latched(&b.action, macros, profiles, depth + 1))
                {
                    return true;
                }
            }
            if let BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                return macros
                    .iter()
                    .find(|m| m.id == *macro_id)
                    .is_none_or(|m| latched(&m.action, macros, profiles, depth + 1));
            }
        }
        (0..)
            .map_while(|index| action.child(index))
            .any(|a| latched(a, macros, profiles, depth + 1))
    }
    if request.mode == DebugMode::Live && latched(&request.action, &macros, &inputs.profiles, 0) {
        return Err("Latched-key actions support simulation and ordinary execution; live debugging is unavailable for this macro closure".into());
    }
    let mut parameters = inputs.freeze_parameters(&runtime);
    let simulation = if request.mode == DebugMode::Simulation {
        if let Some(clock) = &request.simulation.clock {
            chrono::DateTime::parse_from_rfc3339(clock)
                .map_err(|_| "Simulation clock must be RFC3339")?;
        }
        if parameters.profiles.iter().any(|profile| {
            profile
                .parameters
                .iter()
                .any(|parameter| parameter.sensitive)
        }) {
            for profile in Arc::make_mut(&mut parameters.profiles) {
                for parameter in &mut profile.parameters {
                    if parameter.sensitive {
                        parameter.sensitive = false;
                        parameters
                            .overrides
                            .installation
                            .entry(profile.id.clone())
                            .or_default()
                            .remove(&parameter.name);
                        for profiles in parameters.overrides.devices.values_mut() {
                            if let Some(values) = profiles.get_mut(&profile.id) {
                                values.remove(&parameter.name);
                            }
                        }
                    }
                }
            }
        }
        for profile in parameters
            .profiles
            .iter()
            .filter(|profile| profile.id == request.profile_id)
        {
            for parameter in &profile.parameters {
                if let Some(value) = request.simulation.parameters.get(&parameter.name) {
                    parameters
                        .overrides
                        .devices
                        .entry(request.device_id.clone())
                        .or_default()
                        .entry(profile.id.clone())
                        .or_default()
                        .insert(parameter.name.clone(), value.clone());
                }
            }
        }
        Some(Arc::new(Mutex::new(simulation::Simulation::new(
            request.simulation.clone(),
            request.profile_id.clone(),
            request.page_id.clone(),
        ))))
    } else {
        None
    };
    let frame = parameters
        .profiles
        .iter()
        .find(|p| p.id == request.profile_id);
    let inputs = crate::macro_inputs::bind(
        &request.inputs,
        &request.arguments,
        &Default::default(),
        &Default::default(),
        |name| {
            let frame = frame.ok_or("Select a test profile to resolve macro arguments")?;
            let mut overrides = parameters.overrides.clone();
            runtime.resolve_sensitive_override(frame, &mut overrides, &request.device_id, name)?;
            crate::parameters::resolve(frame, &overrides, &request.device_id, name)
        },
    )?;
    let mut run = Run {
        id: crate::actions::RunId(session.id.clone()),
        _debug_state: Some(DebugStateLease::new(&runtime, &session.id)),
        debug: Some(session.clone()),
        simulation,
        parameters: Some(Arc::new(parameters)),
        dial_plugin_input: None,
        key_logic: false,
        debug_macro_root: request.macro_id.is_some(),
        request: Arc::new(Request {
            manual: false,
            activation_policy: Default::default(),
            icon_signature: None,
            pressed: None,
            action: request.action.clone(),
            queued_at: Instant::now(),
            device_id: request.device_id.clone(),
            control_id: request.control_id,
            binding_scope: format!(
                "{}:{}:debug:{}",
                request.profile_id, request.page_id, session.id
            ),
            connection_epoch: epoch,
            input: Input::Pulse,
            cancellation_generation: runtime.cancellation.load(Ordering::SeqCst),
        }),
        pressed: Arc::new(AtomicBool::new(false)),
        locals: Default::default(),
        inputs,
        macros,
        calls: vec![format!("macro:{root_id}")],
        target: None,
        device_generation: runtime
            .builtin_state
            .device_cancellations
            .lock()
            .unwrap()
            .get(&request.device_id)
            .map(|value| value.load(Ordering::SeqCst))
            .unwrap_or(0),
        branch_cancellations: vec![],
        completion_jobs: Default::default(),
        keyboard_events: Default::default(),
        runtime: runtime.clone(),
    };
    if runtime.builtin_state.active.fetch_add(1, Ordering::SeqCst) >= 128 {
        runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
        return Err("Action queue is full".into());
    }
    std::thread::spawn(move || {
        let mut needs_target = false;
        request.action.visit(&mut |action| {
            if matches!(action, Action::Builtin { action } if matches!(**action, BuiltinAction::TypeText{..} | BuiltinAction::PasteText{..} | BuiltinAction::TransformText{..} | BuiltinAction::SearchSelection{..} | BuiltinAction::DateTime{..} | BuiltinAction::SavedMacro{..})) { needs_target = true; }
        });
        if run.simulation.is_none() && needs_target {
            run.target = native::target();
        }
        let mut result = execute(&mut run, &request.action, 0, vec![]);
        loop {
            let handles = std::mem::take(&mut *run.completion_jobs.lock().unwrap());
            if handles.is_empty() {
                break;
            }
            for handle in handles {
                let finished = handle
                    .join()
                    .unwrap_or_else(|_| Err("Parallel worker failed".into()));
                if result.is_ok() {
                    result = finished;
                }
            }
        }
        let result = run.completion_result(result);
        let outcome = match &result {
            Ok(()) => crate::actions::RunOutcome::Completed,
            Err(error) if error == "Cancelled" => crate::actions::RunOutcome::Cancelled,
            Err(_) => crate::actions::RunOutcome::Failed,
        };
        // Processes and held input use this session's owner; finishing cannot leave
        // detached debug resources alive or release another binding's input
        session.cancelled.store(true, Ordering::SeqCst);
        if run.simulation.is_none() {
            native::release_prefix(&format!("debug:{}|", run.id));
            let releases = runtime
                .plugins
                .lock()
                .unwrap()
                .clear_action_scope_suffix(&format!(":debug:{}", run.id));
            for release in releases {
                if let stream_dock_manager_plugin_host::HostEffect::DesktopInput(release) = release
                {
                    crate::desktop_input::submit(&runtime, &release);
                }
            }
        }
        session.complete(outcome);
        runtime.builtin_state.active.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(())
}

#[cfg(test)]
mod dial_execution_tests {
    use super::tests::{action, fixture};
    use super::*;

    #[test]
    // Verify that navigate executes relative page and profile changes on simulator
    fn navigate_executes_relative_page_and_profile_changes_on_simulator() {
        use crate::actions::{NavigationDirection as Direction, NavigationTarget as Target};
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let snapshot = run.runtime.snapshot();
        let session = snapshot.session(&run.request.device_id);
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .clone();
        let original_page = session.page_id.clone();
        let mut page = profile.pages[0].clone();
        page.id = "navigate-extra".into();
        profile.pages.push(page);
        run.runtime
            .save_profile(profile.clone(), snapshot.configuration_revision)
            .unwrap();
        execute(
            &mut run,
            &Action::Navigate {
                target: Target::Page,
                direction: Direction::Previous,
            },
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            run.runtime
                .snapshot()
                .session(&run.request.device_id)
                .page_id,
            "navigate-extra"
        );
        execute(
            &mut run,
            &Action::Navigate {
                target: Target::Page,
                direction: Direction::Next,
            },
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            run.runtime
                .snapshot()
                .session(&run.request.device_id)
                .page_id,
            original_page
        );
        profile.id = "navigate-second".into();
        run.runtime
            .save_profile(profile, run.runtime.snapshot().configuration_revision)
            .unwrap();
        execute(
            &mut run,
            &Action::Navigate {
                target: Target::Profile,
                direction: Direction::Next,
            },
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            run.runtime
                .snapshot()
                .session(&run.request.device_id)
                .profile_id,
            "navigate-second"
        );
        execute(
            &mut run,
            &Action::Navigate {
                target: Target::Profile,
                direction: Direction::Previous,
            },
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            run.runtime
                .snapshot()
                .session(&run.request.device_id)
                .profile_id,
            session.profile_id
        );
        run.runtime.shutdown();
    }

    #[test]
    // Verify that dial plugins preserve signed rotation in direct and macro lanes
    fn dial_plugins_preserve_signed_rotation_in_direct_and_macro_lanes() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        for delta in [-3, 2] {
            let original = Input::Rotate { delta };
            let (lane, normalized) = crate::actions::dial_input(&original).unwrap();
            Arc::make_mut(&mut run.request).input = normalized;
            run.dial_plugin_input = Some(original.clone());
            // Direct action and each nested multi-action use the same signed event
            for path in [vec![lane], vec![lane, 0], vec![lane, 1, 0]] {
                assert_eq!(run.plugin_input(&path), original);
            }
            assert_eq!(run.request.input, Input::Rotate { delta: delta.abs() });
        }
        run.dial_plugin_input = Some(Input::Pulse);
        assert_eq!(run.plugin_input(&[2, 0]), Input::Pulse);
        // Outside dial routing, retain the existing plugin macro pulse contract
        run.dial_plugin_input = None;
        Arc::make_mut(&mut run.request).input = Input::Rotate { delta: -3 };
        assert_eq!(run.plugin_input(&[]), Input::Rotate { delta: -3 });
        assert_eq!(run.plugin_input(&[0]), Input::Pulse);
        run.runtime.shutdown();
    }

    #[test]
    // Verify that dial sequences are independent and preserve original input
    fn dial_sequences_are_independent_and_preserve_original_input() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let log = root.path().join("dial.txt");
        let write = |text: &str| {
            action(
                "write_file",
                json!({"path": log.to_string_lossy(), "text": text, "operation":"append"}),
            )
        };
        let dial = Action::Dial {
            linked_turns: false,
            actions: vec![
                Action::Macro {
                    actions: vec![write("L1"), write("L2")],
                },
                Action::Macro {
                    actions: vec![write("R1"), write("R2")],
                },
                Action::Macro {
                    actions: vec![write("P1"), write("P2")],
                },
            ],
        };
        Arc::make_mut(&mut run.request).action = dial.clone();
        for input in [
            Input::Rotate { delta: -2 },
            Input::Rotate { delta: 3 },
            Input::Pulse,
            Input::Button { pressed: false },
            Input::Rotate { delta: 0 },
        ] {
            Arc::make_mut(&mut run.request).input = input.clone();
            execute(&mut run, &dial, 0, vec![]).unwrap();
            assert_eq!(run.request.input, input);
        }
        assert_eq!(std::fs::read_to_string(log).unwrap(), "L1L2R1R2P1P2");
    }

    #[test]
    // Verify that dial saved binding dispatches each input and macro lane
    fn dial_saved_binding_dispatches_each_input_and_macro_lane() {
        let root = tempfile::tempdir().unwrap();
        let run = fixture(root.path());
        let runtime = &run.runtime;
        let device = run.request.device_id.clone();
        let snapshot = runtime.snapshot();
        let session = snapshot.session(&device);
        let control = snapshot
            .devices
            .iter()
            .find(|d| d.id == device)
            .unwrap()
            .manifest
            .controls
            .iter()
            .find(|c| c.kind == "encoder")
            .unwrap()
            .id
            .clone();
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .clone();
        let mut binding = profile.pages[0].bindings.values().next().unwrap().clone();
        let log = root.path().join("dispatch.txt");
        let write = |text: &str| {
            action(
                "write_file",
                json!({"path": log.to_string_lossy(), "text": text, "operation":"append"}),
            )
        };
        binding.action = Action::Dial {
            linked_turns: false,
            actions: vec![
                write("L"),
                Action::Macro {
                    actions: vec![write("R"), write("r")],
                },
                write("P"),
            ],
        };
        profile
            .pages
            .iter_mut()
            .find(|p| p.id == session.page_id)
            .unwrap()
            .bindings
            .insert(control.clone(), binding);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        for input in [
            Input::Rotate { delta: -1 },
            Input::Rotate { delta: 1 },
            Input::Pulse,
            Input::Button { pressed: false },
        ] {
            runtime.dispatch(
                &device,
                &hardware_core::ControlEvent {
                    control_id: control.clone(),
                    input,
                },
            );
        }
        let until = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&log).unwrap_or_default() != "LRrP" && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "LRrP");
        runtime.shutdown();
    }

    #[test]
    // Verify that dial left step keeps configured negative direction and legacy is unchanged
    fn dial_left_step_keeps_configured_negative_direction_and_legacy_is_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let mut run = fixture(root.path());
        let device = run.request.device_id.clone();
        let mut output = run.runtime.snapshot().session(&device).output;
        output.brightness = 50;
        run.runtime.set_device_output(&device, output).unwrap();
        let dial = Action::Dial {
            linked_turns: false,
            actions: vec![
                action("brightness_step", json!({"delta":-5})),
                action("brightness_step", json!({"delta":5})),
                Action::None,
            ],
        };
        Arc::make_mut(&mut run.request).action = dial.clone();
        Arc::make_mut(&mut run.request).input = Input::Rotate { delta: -2 };
        execute(&mut run, &dial, 0, vec![]).unwrap();
        assert_eq!(
            run.runtime.snapshot().session(&device).output.brightness,
            40
        );
        execute(
            &mut run,
            &action("brightness_step", json!({"delta":5})),
            0,
            vec![],
        )
        .unwrap();
        assert_eq!(
            run.runtime.snapshot().session(&device).output.brightness,
            30
        );
    }
}

#[cfg(test)]
mod desired_state_debug_tests;
