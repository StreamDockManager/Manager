//! Owned debug sessions. No action, native permit or runtime lock is held while paused
use crate::{
    actions::{Action, RunOutcome},
    macro_inputs::{Arguments, MacroInput},
    Runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DebugMode {
    #[default]
    Simulation,
    Live,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DebugCommand {
    Pause,
    StepInto,
    StepOver,
    Continue,
    Stop,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DebugStatus {
    Running,
    Pausing,
    Paused,
    Waiting,
    Finished,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SimulationConfig {
    pub clipboard: String,
    pub selection: String,
    pub files: BTreeMap<String, String>,
    pub application: String,
    pub processes: Vec<String>,
    pub responses: BTreeMap<String, Value>,
    pub parameters: BTreeMap<String, Value>,
    pub clock: Option<String>,
    pub choice: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugStart {
    pub action: Action,
    #[serde(default)]
    pub mode: DebugMode,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub start_running: bool,
    pub device_id: String,
    pub profile_id: String,
    pub page_id: String,
    pub control_id: String,
    #[serde(default)]
    pub inputs: Vec<MacroInput>,
    #[serde(default)]
    pub arguments: Arguments,
    pub macro_id: Option<String>,
    #[serde(default)]
    pub breakpoints: Vec<Vec<usize>>,
    #[serde(default)]
    pub simulation: SimulationConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DebugStepEvent {
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub step_id: u64,
    pub path: Vec<usize>,
    pub phase: String,
    pub kind: String,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DebugView {
    pub id: crate::actions::RunId,
    pub mode: DebugMode,
    pub status: DebugStatus,
    pub outcome: Option<RunOutcome>,
    #[ts(type = "number")]
    pub cursor: u64,
    pub truncated: bool,
    pub paused_paths: Vec<Vec<usize>>,
    pub events: Vec<DebugStepEvent>,
}
struct Gate {
    running: bool,
    grant: Option<u64>,
    over: Option<(u64, Vec<usize>)>,
    blocked: BTreeMap<u64, Vec<usize>>,
    leaves: usize,
    waiting: usize,
    next_step: u64,
    sequence: u64,
    events: VecDeque<DebugStepEvent>,
    outcome: Option<RunOutcome>,
    seen: Instant,
}
pub(crate) struct Session {
    pub id: String,
    pub mode: DebugMode,
    pub cancelled: AtomicBool,
    breakpoints: BTreeSet<Vec<usize>>,
    gate: Mutex<Gate>,
    wake: Condvar,
}
pub(crate) struct Boundary {
    session: Arc<Session>,
    id: u64,
    path: Vec<usize>,
    kind: String,
    leaf: bool,
    start: Instant,
    finished: bool,
}
// Truncate text at a UTF-8 boundary and mark the omitted suffix
pub(crate) fn bounded(value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &value[..end])
}
impl Session {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        crate::editor_retirement::unavailable_or(&self.gate, |gate| gate.outcome.is_none())
    }

    // Initialize macro debug state from the supplied inputs
    pub fn new(mode: DebugMode, breakpoints: Vec<Vec<usize>>, start_running: bool) -> Arc<Self> {
        Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            mode,
            cancelled: AtomicBool::new(false),
            breakpoints: breakpoints.into_iter().collect(),
            wake: Condvar::new(),
            gate: Mutex::new(Gate {
                running: start_running,
                grant: None,
                over: None,
                blocked: BTreeMap::new(),
                leaves: 0,
                waiting: 0,
                next_step: 0,
                sequence: 0,
                events: VecDeque::new(),
                outcome: None,
                seen: Instant::now(),
            }),
        })
    }
    // Check the current macro debug conditions
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst)
            || self.gate.lock().unwrap().seen.elapsed() > Duration::from_secs(30)
        {
            self.cancelled.store(true, Ordering::SeqCst);
            return Err("Cancelled".into());
        }
        Ok(())
    }
    // Handle the incoming macro debug event
    fn event(
        g: &mut Gate,
        id: u64,
        path: Vec<usize>,
        phase: &str,
        kind: String,
        elapsed_ms: u64,
        detail: String,
    ) {
        g.sequence += 1;
        let sequence = g.sequence;
        g.events.push_back(DebugStepEvent {
            sequence,
            step_id: id,
            path,
            phase: phase.into(),
            kind,
            elapsed_ms,
            detail: bounded(detail, 8192),
        });
        while g.events.len() > 256 {
            g.events.pop_front();
        }
    }
    // Record the new macro debug observation
    pub fn record(&self, path: Vec<usize>, phase: &str, detail: String) {
        Self::event(
            &mut self.gate.lock().unwrap(),
            0,
            path,
            phase,
            String::new(),
            0,
            detail,
        );
    }
    // Enter a debugger action boundary and wait for the permitted execution step
    pub fn before(
        self: &Arc<Self>,
        path: Vec<usize>,
        kind: String,
        leaf: bool,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Boundary, String> {
        check()?;
        let mut g = self.gate.lock().unwrap();
        if g.next_step >= 10_000 {
            return Err("Debug run exceeds 10,000 step boundaries".into());
        }
        g.next_step += 1;
        let id = g.next_step;
        let breakpoint = self.breakpoints.contains(&path);
        let mut first = true;
        loop {
            let permitted = g.grant == Some(id)
                || g.over
                    .as_ref()
                    .is_some_and(|(_, prefix)| path.starts_with(prefix))
                || (g.running && (!first || !breakpoint));
            first = false;
            if permitted {
                if g.grant == Some(id) {
                    g.grant = None;
                }
                g.blocked.remove(&id);
                if leaf {
                    g.leaves += 1;
                }
                Self::event(
                    &mut g,
                    id,
                    path.clone(),
                    "started",
                    kind.clone(),
                    0,
                    String::new(),
                );
                return Ok(Boundary {
                    session: self.clone(),
                    id,
                    path,
                    kind,
                    leaf,
                    start: Instant::now(),
                    finished: false,
                });
            }
            if !g.blocked.contains_key(&id) {
                if breakpoint {
                    g.running = false;
                }
                g.blocked.insert(id, path.clone());
                Self::event(
                    &mut g,
                    id,
                    path.clone(),
                    "paused",
                    kind.clone(),
                    0,
                    String::new(),
                );
            }
            let (next, _) = self
                .wake
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap();
            g = next;
            drop(g);
            if let Err(error) = check().and_then(|_| self.check()) {
                self.gate.lock().unwrap().blocked.remove(&id);
                return Err(error);
            }
            g = self.gate.lock().unwrap();
        }
    }
    // Apply a debug control command and wake paused execution when appropriate
    pub fn command(&self, command: DebugCommand, path: Option<&[usize]>) -> Result<(), String> {
        let mut g = self.gate.lock().unwrap();
        if command == DebugCommand::Stop {
            self.cancelled.store(true, Ordering::SeqCst);
            self.wake.notify_all();
        }
        if g.outcome.is_some() {
            return Ok(());
        }
        g.seen = Instant::now();
        match command {
            DebugCommand::Stop => self.cancelled.store(true, Ordering::SeqCst),
            DebugCommand::Pause => {
                g.running = false;
                g.over = None;
                g.grant = None;
            }
            DebugCommand::Continue => {
                g.running = true;
                g.over = None;
                g.grant = None;
            }
            DebugCommand::StepInto | DebugCommand::StepOver => {
                let (id, path) = g
                    .blocked
                    .iter()
                    .find(|(_, candidate)| path.is_none_or(|path| candidate.as_slice() == path))
                    .map(|(id, path)| (*id, path.clone()))
                    .ok_or("Wait for a paused boundary before stepping")?;
                if g.leaves > 0 {
                    return Err(
                        "Wait for dispatched effects to reach a boundary before stepping".into(),
                    );
                }
                g.running = false;
                g.grant = Some(id);
                g.over = (command == DebugCommand::StepOver).then_some((id, path));
            }
        }
        self.wake.notify_all();
        Ok(())
    }
    // Complete the pending macro debug operation
    pub fn complete(&self, outcome: RunOutcome) {
        let mut g = self.gate.lock().unwrap();
        g.outcome = Some(outcome);
        g.blocked.clear();
        self.wake.notify_all();
    }
    // Refresh debug-session activity and collect status plus events after the requested revision
    pub fn view(&self, after: u64) -> DebugView {
        let mut g = self.gate.lock().unwrap();
        g.seen = Instant::now();
        let status = if g.outcome.is_some() {
            DebugStatus::Finished
        } else if g.running {
            if g.waiting > 0 {
                DebugStatus::Waiting
            } else {
                DebugStatus::Running
            }
        } else if !g.blocked.is_empty() {
            if g.leaves == 0 {
                DebugStatus::Paused
            } else {
                DebugStatus::Pausing
            }
        } else if g.leaves > 0 && g.over.is_none() && g.grant.is_none() {
            DebugStatus::Pausing
        } else if g.waiting > 0 {
            DebugStatus::Waiting
        } else {
            DebugStatus::Running
        };
        DebugView {
            id: crate::actions::RunId(self.id.clone()),
            mode: self.mode,
            status,
            outcome: g.outcome,
            cursor: g.sequence,
            truncated: g
                .events
                .front()
                .is_some_and(|e| e.sequence > after.saturating_add(1)),
            paused_paths: g.blocked.values().cloned().collect(),
            events: g
                .events
                .iter()
                .filter(|e| e.sequence > after)
                .cloned()
                .collect(),
        }
    }
    // Report the expired condition for macro debug
    pub fn expired(&self) -> bool {
        let g = self.gate.lock().unwrap();
        g.outcome.is_some() && g.seen.elapsed() > Duration::from_secs(300)
    }
    // Wait for the macro debug condition or result
    pub fn wait(&self, waiting: bool) {
        let mut g = self.gate.lock().unwrap();
        if waiting {
            g.waiting += 1;
        } else {
            g.waiting = g.waiting.saturating_sub(1);
        }
    }
}
impl Boundary {
    // Finalize the pending macro debug operation
    pub fn finish(mut self, result: &Result<(), String>, detail: String) {
        let mut g = self.session.gate.lock().unwrap();
        if self.leaf {
            g.leaves = g.leaves.saturating_sub(1);
        }
        if g.over.as_ref().is_some_and(|(id, _)| *id == self.id) {
            g.over = None;
        }
        Session::event(
            &mut g,
            self.id,
            self.path.clone(),
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
            self.kind.clone(),
            self.start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            detail,
        );
        self.finished = true;
    }
}
impl Drop for Boundary {
    // Release this macro debug owner when it leaves scope
    fn drop(&mut self) {
        if !self.finished {
            let mut g = self.session.gate.lock().unwrap();
            if self.leaf {
                g.leaves = g.leaves.saturating_sub(1);
            }
        }
    }
}
impl Runtime {
    // Start macro debug
    pub fn start_macro_debug(self: &Arc<Self>, request: DebugStart) -> Result<DebugView, String> {
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            > 1024 * 1024
            || request.breakpoints.len() > 128
            || request.breakpoints.iter().any(|path| path.len() > 16)
        {
            return Err("Debug request exceeds its limits".into());
        }
        request.action.validate(0)?;
        let session = Session::new(
            request.mode,
            request.breakpoints.clone(),
            request.start_running,
        );
        let mut sessions = self.builtin_state.debug_sessions.lock().unwrap();
        sessions.retain(|_, s| !s.expired());
        if sessions.len() >= 8 {
            let finished = sessions
                .iter()
                .find(|(_, s)| s.gate.lock().unwrap().outcome.is_some())
                .map(|(id, _)| id.clone());
            if let Some(id) = finished {
                sessions.remove(&id);
            } else {
                return Err("At most eight debug sessions may run at once".into());
            }
        }
        crate::builtins::launch_debug(self.clone(), request, session.clone())?;
        sessions.insert(session.id.clone(), session.clone());
        Ok(session.view(0))
    }
    // Read an existing debug session or report that it has expired
    pub fn macro_debug_view(&self, id: &str, after: u64) -> Result<DebugView, String> {
        self.builtin_state
            .debug_sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.view(after))
            .ok_or("Debug session expired".into())
    }
    // Apply a command to an existing debug session and return its updated view
    pub fn macro_debug_command(
        &self,
        id: &str,
        command: DebugCommand,
    ) -> Result<DebugView, String> {
        let session = self
            .builtin_state
            .debug_sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or("Debug session expired")?;
        session.command(command, None)?;
        Ok(session.view(0))
    }
    // Apply a path-specific debug command and return the updated session view
    pub fn macro_debug_command_at(
        &self,
        id: &str,
        command: DebugCommand,
        path: Option<Vec<usize>>,
    ) -> Result<DebugView, String> {
        let session = self
            .builtin_state
            .debug_sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or("Debug session expired")?;
        session.command(command, path.as_deref())?;
        Ok(session.view(0))
    }
    // Cancel debug sessions
    pub fn cancel_debug_sessions(&self) {
        for session in self.builtin_state.debug_sessions.lock().unwrap().values() {
            let _ = session.command(DebugCommand::Stop, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // Decode a built-in test action after supplying its kind discriminator
    fn action(kind: &str, value: Value) -> Action {
        let mut value = value;
        value["kind"] = json!(kind);
        serde_json::from_value(json!({"kind":"builtin","action":value})).unwrap()
    }
    // Submit the macro debug request
    fn request(action: Action) -> DebugStart {
        DebugStart {
            action,
            mode: DebugMode::Simulation,
            start_running: false,
            device_id: "isolated".into(),
            profile_id: "default".into(),
            page_id: "main".into(),
            control_id: "key-1".into(),
            inputs: vec![],
            arguments: Default::default(),
            macro_id: None,
            breakpoints: vec![],
            simulation: Default::default(),
        }
    }
    // Wait for the macro debug condition or result
    fn wait(runtime: &Runtime, id: &str, predicate: impl Fn(&DebugView) -> bool) -> DebugView {
        let start = Instant::now();
        loop {
            let view = runtime.macro_debug_view(id, 0).unwrap();
            if predicate(&view) {
                return view;
            }
            assert!(
                start.elapsed() < Duration::from_secs(4),
                "debug stalled: {view:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; keyboard simulation never captures or injects input"]
    // Verify that keyboard macro simulation reports counts and timing without keys
    fn keyboard_macro_simulation_reports_counts_and_timing_without_keys() {
        use crate::keyboard_macro::*;
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let recording = KeyboardMacroRecording {
            events: vec![
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key: PhysicalKey::ControlRight,
                    delay_ms: 0,
                },
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key: PhysicalKey::KeyS,
                    delay_ms: 25,
                },
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyUp,
                    key: PhysicalKey::KeyS,
                    delay_ms: 25,
                },
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyUp,
                    key: PhysicalKey::ControlRight,
                    delay_ms: 0,
                },
            ],
            ..Default::default()
        };
        let mut request = request(action(
            "keyboard_macro",
            json!({"recording":recording,"playback":KeyboardMacroPlayback {speed:2.0,..Default::default()}}),
        ));
        request.start_running = true;
        let id = runtime.start_macro_debug(request).unwrap().id;
        let result = wait(&runtime, &id, |view| view.status == DebugStatus::Finished);
        assert_eq!(result.outcome, Some(RunOutcome::Completed));
        let effect = result
            .events
            .iter()
            .find(|event| event.phase == "simulated")
            .unwrap();
        assert!(effect.detail.contains("\"event_count\":4"));
        assert!(effect.detail.contains("\"playback_duration_ms\":25"));
        for event in &result.events {
            assert!(!event.detail.contains("KeyS"));
            assert!(!event.detail.contains("ControlRight"));
        }
        runtime.shutdown();
    }
    #[test]
    // Verify that events are bounded and lost cursors are explicit
    fn events_are_bounded_and_lost_cursors_are_explicit() {
        let session = Session::new(DebugMode::Simulation, vec![], false);
        for _ in 0..300 {
            session.record(vec![], "effect", "x".repeat(9000));
        }
        let view = session.view(0);
        assert!(view.truncated);
        assert_eq!(view.events.len(), 256);
        assert!(view.events.iter().all(|event| event.detail.len() < 8300));
        assert!(session.view(view.cursor).events.is_empty());
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no native effects"]
    // Verify that standalone simulation starts at first breakpoint and redacts plugin settings
    fn standalone_simulation_starts_at_first_breakpoint_and_redacts_plugin_settings() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime.state.lock().unwrap().profiles.clear();
        let plugin = Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture.plugin".into(),
            action_id: "step".into(),
            settings: json!({"token":"draft-plugin-secret"}),
            resources: Default::default(),
        };
        let mut request = request(Action::Macro {
            actions: vec![
                Action::Delay { milliseconds: 100 },
                Action::Toggle {
                    on: Box::new(plugin.clone()),
                    off: Box::new(plugin),
                },
            ],
        });
        request.profile_id.clear();
        request.page_id.clear();
        request.device_id.clear();
        request.start_running = true;
        request.breakpoints = vec![vec![1]];
        request
            .simulation
            .responses
            .insert("fixture.plugin/step".into(), json!("fixture output"));

        let id = runtime.start_macro_debug(request.clone()).unwrap().id;
        let paused = wait(&runtime, &id, |view| view.status == DebugStatus::Paused);
        assert_eq!(paused.paused_paths, vec![vec![1]]);
        assert!(paused
            .events
            .iter()
            .any(|e| e.path == [0] && e.phase == "completed"));
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        let completed = wait(&runtime, &id, |view| view.status == DebugStatus::Finished);
        assert_eq!(completed.outcome, Some(RunOutcome::Completed));
        assert!(completed
            .events
            .iter()
            .any(|e| e.phase == "plugin_result" && e.detail.contains("fixture output")));
        assert!(!serde_json::to_string(&completed)
            .unwrap()
            .contains("draft-plugin-secret"));

        request.breakpoints.clear();
        for (step, message) in [
            (
                Action::Page {
                    page_id: "missing".into(),
                },
                "Select a test profile",
            ),
            (
                action(
                    "set_clipboard",
                    json!({"text":"${parameter.token}","template":false}),
                ),
                "resolve parameter token",
            ),
        ] {
            request.action = step;
            let id = runtime.start_macro_debug(request.clone()).unwrap().id;
            let failed = wait(&runtime, &id, |view| view.status == DebugStatus::Finished);
            assert_eq!(failed.outcome, Some(RunOutcome::Failed));
            assert!(failed.events.iter().any(|e| e.detail.contains(message)));
        }
        request.mode = DebugMode::Live;
        assert!(runtime
            .start_macro_debug(request)
            .unwrap_err()
            .contains("Select a profile"));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; simulation never opens HID or performs effects"]
    // Verify that same traversal steps breakpoints and isolated effects
    fn same_traversal_steps_breakpoints_and_isolated_effects() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let disk = root.path().join("must-not-exist.txt");
        let write = action(
            "write_file",
            json!({"path":disk,"text":"simulated","operation":"create","replace":false,"template":false}),
        );
        let mut request = request(Action::Macro {
            actions: vec![
                action("set_clipboard", json!({"text":"fixture","template":false})),
                action(
                    "conditional",
                    json!({"condition":{"source":"clipboard","subject":"","operator":"equals","value":"fixture"},"children":[write,{"kind":"command","program":"must-never-launch","args":[]}]}),
                ),
                Action::Plugin {
                    user_desired_state: None,
                    plugin_id: "not-installed".into(),
                    action_id: "fixture".into(),
                    settings: json!({}),
                    resources: Default::default(),
                },
            ],
        });
        request.breakpoints = vec![vec![2]];
        let id = runtime.start_macro_debug(request).unwrap().id;
        wait(&runtime, &id, |v| {
            v.paused_paths == vec![Vec::<usize>::new()]
        });
        runtime
            .macro_debug_command(&id, DebugCommand::StepInto)
            .unwrap();
        wait(&runtime, &id, |v| v.paused_paths == vec![vec![0]]);
        runtime
            .macro_debug_command(&id, DebugCommand::StepOver)
            .unwrap();
        wait(&runtime, &id, |v| v.paused_paths == vec![vec![1]]);
        runtime
            .macro_debug_command(&id, DebugCommand::StepOver)
            .unwrap();
        wait(&runtime, &id, |v| v.paused_paths == vec![vec![2]]);
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        let view = wait(&runtime, &id, |v| v.status == DebugStatus::Finished);
        assert_eq!(view.outcome, Some(RunOutcome::Completed));
        assert!(view
            .events
            .iter()
            .any(|e| e.phase == "branch" && e.detail == "Selected branch 1"));
        assert!(view.events.iter().any(|e| e.phase == "plugin_result"));
        assert!(!disk.exists());
        assert_eq!(runtime.snapshot().session("isolated").profile_id, "default");
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no native effects"]
    // Verify that recovery is bounded and cancellation bypasses fallback
    fn recovery_is_bounded_and_cancellation_bypasses_fallback() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let command = action(
            "command_result",
            json!({"program":"not-executed","args":[],"path":"","timeout_ms":100,"output":"result","template":false}),
        );
        let mut retry = request(action(
            "failure_policy",
            json!({"operation":"retry","retries":2,"delay_ms":100,"children":[command,{"kind":"none"}]}),
        ));
        retry.simulation.responses.insert("command_result".into(), json!([{"stdout":"first","stderr":"","exit_status":1},{"stdout":"second","stderr":"","exit_status":1},{"stdout":"third","stderr":"","exit_status":0}]));
        let id = runtime.start_macro_debug(retry).unwrap().id;
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        let view = wait(&runtime, &id, |v| v.status == DebugStatus::Finished);
        assert_eq!(view.outcome, Some(RunOutcome::Completed));
        assert_eq!(
            view.events
                .iter()
                .filter(|e| e.kind == "command_result" && e.phase == "started")
                .count(),
            3
        );
        let mut cancel = request(action(
            "failure_policy",
            json!({"operation":"fallback","retries":5,"delay_ms":100,"children":[{"kind":"none"},{"kind":"sleep"}]}),
        ));
        cancel
            .simulation
            .responses
            .insert("none".into(), json!({"error":"Cancelled"}));
        let id = runtime.start_macro_debug(cancel).unwrap().id;
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        let view = wait(&runtime, &id, |v| v.status == DebugStatus::Finished);
        assert_eq!(view.outcome, Some(RunOutcome::Cancelled));
        assert!(!view.events.iter().any(|e| e.kind == "sleep"));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no native effects"]
    // Verify that stop and editor cancellation are owned and live latches are rejected
    fn stop_and_editor_cancellation_are_owned_and_live_latches_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let first = runtime.start_macro_debug(request(Action::None)).unwrap().id;
        let second = runtime.start_macro_debug(request(Action::None)).unwrap().id;
        wait(&runtime, &first, |v| v.status == DebugStatus::Paused);
        runtime
            .macro_debug_command(&first, DebugCommand::Stop)
            .unwrap();
        assert_eq!(
            wait(&runtime, &first, |v| v.status == DebugStatus::Finished).outcome,
            Some(RunOutcome::Cancelled)
        );
        assert_eq!(
            wait(&runtime, &second, |v| v.status == DebugStatus::Paused).outcome,
            None
        );
        runtime.cancel_debug_sessions();
        assert_eq!(
            wait(&runtime, &second, |v| v.status == DebugStatus::Finished).outcome,
            Some(RunOutcome::Cancelled)
        );
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("isolated".into(), 1);
        let mut live = request(action("latch_keys", json!({"keys":["ctrl"]})));
        live.mode = DebugMode::Live;
        assert!(runtime
            .start_macro_debug(live)
            .unwrap_err()
            .contains("Latched-key"));
        runtime
            .state
            .lock()
            .unwrap()
            .saved_macros
            .push(crate::builtins::SavedMacro {
                id: "latched".into(),
                name: "Latched".into(),
                inputs: vec![],
                action: action("latch_keys", json!({"keys":["ctrl"]})),
            });
        let mut transitive = request(action("saved_macro", json!({"macro_id":"latched"})));
        transitive.mode = DebugMode::Live;
        assert!(runtime
            .start_macro_debug(transitive)
            .unwrap_err()
            .contains("Latched-key"));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no native effects"]
    // Verify that parallel step over runs only the selected subtree
    fn parallel_step_over_runs_only_the_selected_subtree() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let child = Action::Macro {
            actions: vec![Action::None],
        };
        let id = runtime
            .start_macro_debug(request(action(
                "parallel",
                json!({"wait":true,"children":[child.clone(),child]}),
            )))
            .unwrap()
            .id;
        wait(&runtime, &id, |v| v.status == DebugStatus::Paused);
        runtime
            .macro_debug_command(&id, DebugCommand::StepInto)
            .unwrap();
        wait(&runtime, &id, |v| v.paused_paths.len() == 2);
        runtime
            .macro_debug_command_at(&id, DebugCommand::StepOver, Some(vec![1]))
            .unwrap();
        let view = wait(&runtime, &id, |v| {
            v.paused_paths == vec![vec![0]]
                && v.events
                    .iter()
                    .any(|e| e.path == vec![1] && e.phase == "completed")
        });
        assert!(!view
            .events
            .iter()
            .any(|e| e.path == vec![0] && e.phase == "started"));
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        assert_eq!(
            wait(&runtime, &id, |v| v.status == DebugStatus::Finished).outcome,
            Some(RunOutcome::Completed)
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; live delay has no native or device effect"]
    // Verify that disconnect cancels a waiting live debug session
    fn disconnect_cancels_a_waiting_live_debug_session() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("isolated".into(), 1);
        let mut request = request(Action::Delay { milliseconds: 5000 });
        request.mode = DebugMode::Live;
        let id = runtime.start_macro_debug(request).unwrap().id;
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        wait(&runtime, &id, |v| v.status == DebugStatus::Waiting);
        assert_eq!(
            runtime
                .macro_debug_command(&id, DebugCommand::Pause)
                .unwrap()
                .status,
            DebugStatus::Pausing
        );
        runtime.connection_epochs.lock().unwrap().remove("isolated");
        assert_eq!(
            wait(&runtime, &id, |v| v.status == DebugStatus::Finished).outcome,
            Some(RunOutcome::Cancelled)
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; fake credential provider and simulation only"]
    // Verify that simulation never reads credentials and freezes referenced macros
    fn simulation_never_reads_credentials_and_freezes_referenced_macros() {
        use crate::credentials::{CredentialProvider, CredentialState, Credentials};
        use std::sync::atomic::AtomicUsize;
        struct NoReads(Arc<AtomicUsize>);
        impl CredentialProvider for NoReads {
            // Read the requested macro debug data
            fn read(&self, _: &str) -> Result<String, CredentialState> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(CredentialState::Locked)
            }
            // Write the supplied macro debug data
            fn write(&self, _: &str, _: &str) -> Result<(), CredentialState> {
                Err(CredentialState::Locked)
            }
            // Remove the selected macro debug entry
            fn delete(&self, _: &str) -> Result<(), CredentialState> {
                Err(CredentialState::Locked)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::build_credentials(
            root.path().into(),
            false,
            Credentials::with_provider(Arc::new(NoReads(reads.clone()))),
        )
        .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            state.profiles[0]
                .parameters
                .push(crate::parameters::ProfileParameter {
                    name: "token".into(),
                    label: "Token".into(),
                    r#type: "string".into(),
                    required: true,
                    default: None,
                    sensitive: true,
                });
            state.saved_macros.push(crate::builtins::SavedMacro {
                id: "shared".into(),
                name: "Shared".into(),
                inputs: vec![],
                action: action("set_clipboard", json!({"text":"before","template":false})),
            });
        }
        runtime.profiles.write_preference(crate::profile_database::LocalPreference::ParameterOverrides, &json!({"installation":{"default":{"token":{"$credential":format!("parameter-{}", uuid::Uuid::new_v4())}}},"devices":{}})).unwrap();
        let mut request = request(Action::Macro {
            actions: vec![
                action("saved_macro", json!({"macro_id":"shared"})),
                action(
                    "set_clipboard",
                    json!({"text":"${parameter.token}","template":false}),
                ),
            ],
        });
        request
            .simulation
            .parameters
            .insert("token".into(), json!("simulated-token"));
        let id = runtime.start_macro_debug(request).unwrap().id;
        wait(&runtime, &id, |v| v.status == DebugStatus::Paused);
        runtime.state.lock().unwrap().saved_macros[0].action =
            action("set_clipboard", json!({"text":"after","template":false}));
        runtime
            .macro_debug_command(&id, DebugCommand::Continue)
            .unwrap();
        let view = wait(&runtime, &id, |v| v.status == DebugStatus::Finished);
        assert_eq!(view.outcome, Some(RunOutcome::Completed));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let effects: Vec<Value> = view
            .events
            .iter()
            .filter(|e| e.phase == "simulated")
            .map(|e| serde_json::from_str(&e.detail).unwrap())
            .collect();
        assert!(effects
            .iter()
            .any(|e| e["action"]["action"]["text"] == "before"));
        assert!(effects
            .iter()
            .any(|e| e["action"]["action"]["text"] == "simulated-token"));
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("$credential"));
        runtime.shutdown();
    }
}
