//! Explicit, short-lived scratch-target tests. Native target identities never
//! leave the owning worker and are never persisted or accepted from the frontend
use super::{test_target, KeyboardMacroFocusPolicy, KeyboardMacroPlayback, KeyboardMacroRecording};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const COUNTDOWN: Duration = Duration::from_secs(3);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const TARGET_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);
const CANCELED: &str = "Keyboard macro test canceled";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroTestState {
    SelectingTarget,
    TargetReady,
    Countdown,
    Running,
    Canceling,
    Completed,
    Canceled,
    Failed,
}
impl KeyboardMacroTestState {
    // Open the requested command in a terminal
    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Canceled | Self::Failed)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroTestSession {
    pub session_id: String,
    pub state: KeyboardMacroTestState,
    pub target_label: Option<String>,
    pub remaining_ms: u32,
    pub event_count: usize,
    pub error: Option<String>,
}
struct Live {
    view: KeyboardMacroTestSession,
    heartbeat: Instant,
}
struct Shared {
    live: Mutex<Live>,
    canceled: Arc<AtomicBool>,
}
impl Shared {
    // Initialize test session state from the supplied inputs
    fn new(now: Instant) -> Self {
        Self {
            live: Mutex::new(Live {
                view: KeyboardMacroTestSession {
                    session_id: uuid::Uuid::new_v4().to_string(),
                    state: KeyboardMacroTestState::SelectingTarget,
                    target_label: None,
                    remaining_ms: COUNTDOWN.as_millis() as u32,
                    event_count: 0,
                    error: None,
                },
                heartbeat: now,
            }),
            canceled: Arc::new(AtomicBool::new(false)),
        }
    }
    // Check the current test session conditions
    fn check(&self, now: Instant) -> Result<(), String> {
        if self.canceled.load(Ordering::Acquire) {
            return Err(CANCELED.into());
        }
        let live = self
            .live
            .lock()
            .map_err(|_| "Keyboard macro test state is unavailable")?;
        if now.saturating_duration_since(live.heartbeat) > HEARTBEAT_TIMEOUT {
            return Err("Keyboard macro test stopped because the editor heartbeat expired".into());
        }
        Ok(())
    }
    // Return a snapshot of test session state
    fn snapshot(&self, heartbeat: bool) -> Result<KeyboardMacroTestSession, String> {
        let mut live = self
            .live
            .lock()
            .map_err(|_| "Keyboard macro test state is unavailable")?;
        if heartbeat {
            live.heartbeat = Instant::now();
        }
        Ok(live.view.clone())
    }
    // Apply the new test session state
    fn update(&self, change: impl FnOnce(&mut KeyboardMacroTestSession)) -> Result<(), String> {
        let mut live = self
            .live
            .lock()
            .map_err(|_| "Keyboard macro test state is unavailable")?;
        change(&mut live.view);
        if self.canceled.load(Ordering::Acquire) && !live.view.state.terminal() {
            live.view.state = KeyboardMacroTestState::Canceling;
            live.view.remaining_ms = 0;
        }
        Ok(())
    }
    // Complete the pending test session operation
    fn complete(&self, result: Result<(), String>) {
        let _ = self.update(|view| {
            // Target labels, like native handles, are session data, not history
            view.target_label = None;
            view.remaining_ms = 0;
            match result {
                Ok(()) => view.state = KeyboardMacroTestState::Completed,
                Err(error) => {
                    view.state = if self.canceled.load(Ordering::Acquire) && error == CANCELED {
                        KeyboardMacroTestState::Canceled
                    } else {
                        KeyboardMacroTestState::Failed
                    };
                    view.error = Some(error);
                }
            }
        });
    }
}
enum Request {
    Start {
        recording: KeyboardMacroRecording,
        playback: KeyboardMacroPlayback,
    },
}
struct Control {
    owner: String,
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Request>,
    worker: Option<JoinHandle<()>>,
}
impl Control {
    // Request cancel
    fn request_cancel(&self) -> Result<(), String> {
        self.shared.canceled.store(true, Ordering::Release);
        self.shared.update(|view| {
            if !view.state.terminal() {
                view.state = KeyboardMacroTestState::Canceling;
                view.remaining_ms = 0;
            }
        })
    }
    // Report the running condition for test session
    fn running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }
    // Collect completed test session workers
    fn reap(&mut self) {
        // Never join a live native worker on an IPC/lifecycle/UI path. Its owned
        // handle stays in STORE and remains a native editor-retirement blocker
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && self.worker.take().expect("finished worker").join().is_err()
        {
            self.shared
                .complete(Err("Keyboard macro test worker failed".into()));
        }
    }
}
static STORE: OnceLock<Mutex<Option<Control>>> = OnceLock::new();
// Store the test session value
fn store() -> &'static Mutex<Option<Control>> {
    STORE.get_or_init(|| Mutex::new(None))
}
// Report the owner matches condition for test session
fn owner_matches(control: &Control, owner: &str, id: &str) -> bool {
    control.owner == owner
        && control
            .shared
            .snapshot(false)
            .is_ok_and(|view| view.session_id == id)
}
// Explain why the current operation is unavailable
pub fn unavailable_reason() -> Option<String> {
    super::playback::unavailable_reason().or_else(test_target::unavailable_reason)
}
// Begin tracking the test session operation
pub fn begin(owner: &str, editor_id: &str) -> Result<KeyboardMacroTestSession, String> {
    if editor_id.is_empty() || editor_id.len() > 128 {
        return Err("Keyboard macro test owner is invalid".into());
    }
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    let mut slot = store()
        .lock()
        .map_err(|_| "Keyboard macro test service is unavailable")?;
    if let Some(control) = slot.as_mut() {
        control.reap();
        if control.running() || !control.shared.snapshot(false)?.state.terminal() {
            return Err("A keyboard macro test is active or still releasing its keys".into());
        }
    }
    // A completed worker is joined before replacing its only control handle
    slot.take();
    let shared = Arc::new(Shared::new(Instant::now()));
    let result = shared.snapshot(false)?;
    let signal = shared.clone();
    let owner_string = owner.to_string();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_owner = format!("{owner}:keyboard-test:{}", result.session_id);
    let worker = std::thread::Builder::new()
        .name("keyboard-scratch-test".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_session(&mut NativeEnvironment, &signal, &receiver, &worker_owner)
            }))
            .unwrap_or_else(|_| Err("Keyboard macro test worker failed".into()));
            // run_session drops its selected target and all native observers first
            signal.complete(result);
        })
        .map_err(|_| "Keyboard macro test worker could not start")?;
    *slot = Some(Control {
        owner: owner_string,
        shared,
        sender,
        worker: Some(worker),
    });
    Ok(result)
}
// Start the test session operation
pub fn start(
    owner: &str,
    id: &str,
    recording: KeyboardMacroRecording,
    playback: KeyboardMacroPlayback,
) -> Result<KeyboardMacroTestSession, String> {
    super::validation::validate(&recording, &playback)?;
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    let slot = store()
        .lock()
        .map_err(|_| "Keyboard macro test service is unavailable")?;
    let control = slot
        .as_ref()
        .filter(|control| owner_matches(control, owner, id))
        .ok_or("Keyboard macro test session expired")?;
    queue_start(control, recording, playback, Instant::now())
}
// Force fixed-target playback, validate the recording and queue the scratch test
fn queue_start(
    control: &Control,
    recording: KeyboardMacroRecording,
    mut playback: KeyboardMacroPlayback,
    now: Instant,
) -> Result<KeyboardMacroTestSession, String> {
    // Test does not inherit a saved action's permission to change target
    playback.focus_policy = KeyboardMacroFocusPolicy::StayOnTarget;
    super::validation::validate(&recording, &playback)?;
    control.shared.check(now)?;
    let mut live = control
        .shared
        .live
        .lock()
        .map_err(|_| "Keyboard macro test state is unavailable")?;
    if live.view.state != KeyboardMacroTestState::TargetReady {
        return Err("Select and confirm a scratch target before testing".into());
    }
    let count = recording.events.len();
    control
        .sender
        .try_send(Request::Start {
            recording,
            playback,
        })
        .map_err(|_| "Keyboard macro test could not be started")?;
    live.heartbeat = now;
    live.view.state = KeyboardMacroTestState::Countdown;
    live.view.remaining_ms = COUNTDOWN.as_millis() as u32;
    live.view.event_count = count;
    Ok(live.view.clone())
}
// Return the current test session status
pub fn status(owner: &str, id: &str) -> Result<KeyboardMacroTestSession, String> {
    let mut slot = store()
        .lock()
        .map_err(|_| "Keyboard macro test service is unavailable")?;
    let control = slot
        .as_mut()
        .filter(|control| owner_matches(control, owner, id))
        .ok_or("Keyboard macro test session expired")?;
    control.reap();
    control.shared.snapshot(true)
}
// Cancel the pending test session operation
pub fn cancel(owner: &str, id: &str) -> Result<Option<KeyboardMacroTestSession>, String> {
    let mut slot = store()
        .lock()
        .map_err(|_| "Keyboard macro test service is unavailable")?;
    if let Some(control) = slot
        .as_mut()
        .filter(|control| owner_matches(control, owner, id))
    {
        control.request_cancel()?;
        control.reap();
        // Keep a bounded terminal receipt. Retrying cancellation must not turn a
        // failed release/teardown into an apparently successful missing session
        return control.shared.snapshot(false).map(Some);
    }
    Ok(None)
}
// Cancel owner
pub fn cancel_owner(owner: &str) {
    if let Ok(mut slot) = store().lock() {
        if let Some(control) = slot.as_mut().filter(|control| {
            control.owner == owner || owner == "main" && control.owner.starts_with("main:")
        }) {
            let _ = control.request_cancel();
            control.reap();
        }
    }
}
// Cancel all pending test session operations
pub fn cancel_all() {
    if let Ok(mut slot) = store().lock() {
        if let Some(control) = slot.as_mut() {
            let _ = control.request_cancel();
            control.reap();
        }
    }
}
/// Native window/quit coordination must wait for actual scratch-worker exit
pub fn editor_retirement_blocked() -> bool {
    store()
        .try_lock()
        .map_or(true, |slot| slot.as_ref().is_some_and(Control::running))
}

trait Environment {
    // Read the clock used for test session
    fn now(&self) -> Instant;
    // Wait for the test session condition or result
    fn wait(&mut self, duration: Duration);
    // Capture the current test session observation
    fn capture(&mut self) -> Result<Box<dyn test_target::TestTarget>, String>;
    // Start the selected test session playback
    fn play(
        &mut self,
        target: &mut dyn test_target::TestTarget,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
        owner: &str,
        shared: &Shared,
    ) -> Result<(), String>;
}
struct NativeEnvironment;
impl Environment for NativeEnvironment {
    // Read the clock used for test session
    fn now(&self) -> Instant {
        Instant::now()
    }
    // Wait for the test session condition or result
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    // Capture the current test session observation
    fn capture(&mut self) -> Result<Box<dyn test_target::TestTarget>, String> {
        test_target::capture()
    }
    // Start the selected test session playback
    fn play(
        &mut self,
        target: &mut dyn test_target::TestTarget,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
        owner: &str,
        shared: &Shared,
    ) -> Result<(), String> {
        let canceled = shared.canceled.clone();
        let guard = Arc::new(move || {
            if canceled.load(Ordering::Acquire) {
                Err(CANCELED.into())
            } else {
                Ok(())
            }
        });
        super::playback::play_in_target(recording, playback, owner, target, guard, || {
            shared.check(Instant::now())
        })
    }
}
// Update the countdown while checking cancellation and target liveness
fn countdown(
    environment: &mut impl Environment,
    shared: &Shared,
    mut target: Option<&mut dyn test_target::TestTarget>,
) -> Result<(), String> {
    let deadline = environment.now() + COUNTDOWN;
    loop {
        let now = environment.now();
        shared.check(now)?;
        if let Some(target) = target.as_deref_mut() {
            target.check_alive()?;
        }
        let remaining = deadline.saturating_duration_since(now);
        shared.update(|view| {
            view.remaining_ms = remaining.as_millis().min(u128::from(u32::MAX)) as u32
        })?;
        if remaining.is_zero() {
            return Ok(());
        }
        environment.wait(remaining.min(POLL));
    }
}
// Run session
fn run_session(
    environment: &mut impl Environment,
    shared: &Shared,
    receiver: &mpsc::Receiver<Request>,
    owner: &str,
) -> Result<(), String> {
    countdown(environment, shared, None)?;
    let mut target = environment.capture()?;
    let result = run_selected(environment, shared, receiver, owner, target.as_mut());
    let teardown = target.finish();
    match (result, teardown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(teardown)) => Err(format!("{error}; {teardown}")),
    }
}
// Run selected
fn run_selected(
    environment: &mut impl Environment,
    shared: &Shared,
    receiver: &mpsc::Receiver<Request>,
    owner: &str,
    target: &mut dyn test_target::TestTarget,
) -> Result<(), String> {
    shared.check(environment.now())?;
    let label = target
        .label()
        .chars()
        .filter(|value| !value.is_control())
        .take(256)
        .collect::<String>();
    if label.trim().is_empty() {
        return Err("The scratch target has no verifiable application/window label".into());
    }
    shared.update(|view| {
        view.state = KeyboardMacroTestState::TargetReady;
        view.target_label = Some(label);
        view.remaining_ms = 0;
    })?;
    let selected_at = environment.now();
    let Request::Start {
        recording,
        mut playback,
    } = loop {
        let now = environment.now();
        shared.check(now)?;
        target.check_alive()?;
        if now.saturating_duration_since(selected_at) >= TARGET_TIMEOUT {
            return Err("The selected scratch target expired; choose it again".into());
        }
        match receiver.try_recv() {
            Ok(request) => break request,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("Keyboard macro test canceled".into())
            }
            Err(mpsc::TryRecvError::Empty) => environment.wait(POLL),
        }
    };
    playback.focus_policy = KeyboardMacroFocusPolicy::StayOnTarget;
    super::validation::validate(&recording, &playback)?;
    shared.update(|view| {
        view.state = KeyboardMacroTestState::Countdown;
        view.event_count = recording.events.len();
    })?;
    countdown(environment, shared, Some(target))?;
    target.check_focused()?;
    shared.check(environment.now())?;
    shared.update(|view| {
        view.state = KeyboardMacroTestState::Running;
        view.remaining_ms = 0;
    })?;
    environment.play(target, &recording, &playback, owner, shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard_macro::{KeyboardMacroEvent, KeyboardMacroEventKind, PhysicalKey};
    use std::{cell::Cell, rc::Rc};
    // Construct the balanced key-press recording used by the test
    fn recording() -> KeyboardMacroRecording {
        KeyboardMacroRecording {
            events: vec![
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key: PhysicalKey::KeyA,
                    delay_ms: 0,
                },
                KeyboardMacroEvent {
                    kind: KeyboardMacroEventKind::KeyUp,
                    key: PhysicalKey::KeyA,
                    delay_ms: 20,
                },
            ],
            ..Default::default()
        }
    }
    struct Probe {
        alive: Cell<bool>,
        focused: Cell<bool>,
        finished: Cell<usize>,
        dropped: Cell<usize>,
        teardown_failure: Cell<bool>,
    }
    impl Probe {
        // Initialize test session state from the supplied inputs
        fn new() -> Rc<Self> {
            Rc::new(Self {
                alive: Cell::new(true),
                focused: Cell::new(true),
                finished: Cell::new(0),
                dropped: Cell::new(0),
                teardown_failure: Cell::new(false),
            })
        }
    }
    struct Target(Rc<Probe>);
    impl test_target::TestTarget for Target {
        // Return the display label for test session
        fn label(&self) -> &str {
            "Scratch editor: Blank document"
        }
        // Reject an input target whose original process is no longer alive
        fn check_alive(&mut self) -> Result<(), String> {
            if self.0.alive.get() {
                Ok(())
            } else {
                Err("Selected target was destroyed".into())
            }
        }
        // Reject an input target that no longer holds the required focus
        fn check_focused(&mut self) -> Result<(), String> {
            self.check_alive()?;
            if self.0.focused.get() {
                Ok(())
            } else {
                Err("Selected target is not focused".into())
            }
        }
        // Finalize the pending test session operation
        fn finish(&mut self) -> Result<(), String> {
            self.0.finished.set(self.0.finished.get() + 1);
            if self.0.teardown_failure.get() {
                Err("Target observer teardown failed".into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Target {
        // Release this test session owner when it leaves scope
        fn drop(&mut self) {
            self.0.dropped.set(self.0.dropped.get() + 1);
        }
    }
    struct Fake<'a> {
        shared: &'a Shared,
        origin: Instant,
        now: Instant,
        probe: Rc<Probe>,
        captured: usize,
        played: usize,
        cancel_at: Option<Duration>,
        destroy_at: Option<Duration>,
        keep_alive: bool,
        cancel_during_play: bool,
    }
    impl<'a> Fake<'a> {
        // Initialize test session state from the supplied inputs
        fn new(shared: &'a Shared, now: Instant) -> Self {
            Self {
                shared,
                origin: now,
                now,
                probe: Probe::new(),
                captured: 0,
                played: 0,
                cancel_at: None,
                destroy_at: None,
                keep_alive: false,
                cancel_during_play: false,
            }
        }
    }
    impl Environment for Fake<'_> {
        // Read the clock used for test session
        fn now(&self) -> Instant {
            self.now
        }
        // Wait for the test session condition or result
        fn wait(&mut self, duration: Duration) {
            self.now += duration;
            if self.keep_alive {
                self.shared.live.lock().unwrap().heartbeat = self.now;
            }
            let elapsed = self.now.duration_since(self.origin);
            if self.cancel_at.is_some_and(|time| elapsed >= time) {
                self.shared.canceled.store(true, Ordering::Release);
            }
            if self.destroy_at.is_some_and(|time| elapsed >= time) {
                self.probe.alive.set(false);
            }
        }
        // Capture the current test session observation
        fn capture(&mut self) -> Result<Box<dyn test_target::TestTarget>, String> {
            self.captured += 1;
            Ok(Box::new(Target(self.probe.clone())))
        }
        // Start the selected test session playback
        fn play(
            &mut self,
            target: &mut dyn test_target::TestTarget,
            _: &KeyboardMacroRecording,
            playback: &KeyboardMacroPlayback,
            _: &str,
            shared: &Shared,
        ) -> Result<(), String> {
            assert_eq!(
                playback.focus_policy,
                KeyboardMacroFocusPolicy::StayOnTarget
            );
            assert!(self.now.duration_since(self.origin) >= COUNTDOWN * 2);
            target.check_focused()?;
            self.played += 1;
            if self.cancel_during_play {
                shared.canceled.store(true, Ordering::Release);
            }
            shared.check(self.now)
        }
    }
    // Submit the test session request
    fn request(sender: &mpsc::SyncSender<Request>) {
        sender
            .send(Request::Start {
                recording: recording(),
                playback: KeyboardMacroPlayback {
                    focus_policy: KeyboardMacroFocusPolicy::AllowFocusChanges,
                    ..Default::default()
                },
            })
            .unwrap();
    }
    #[test]
    // Verify that selection and confirmation have separate countdowns and force strict focus
    fn selection_and_confirmation_have_separate_countdowns_and_force_strict_focus() {
        let now = Instant::now();
        let shared = Shared::new(now);
        let mut fake = Fake::new(&shared, now);
        let (sender, receiver) = mpsc::sync_channel(1);
        request(&sender);
        let result = run_session(&mut fake, &shared, &receiver, "main:1:test");
        assert!(result.is_ok());
        assert_eq!((fake.captured, fake.played), (1, 1));
        assert_eq!(
            (fake.probe.finished.get(), fake.probe.dropped.get()),
            (1, 1)
        );
        shared.complete(result);
        let view = shared.snapshot(false).unwrap();
        assert_eq!(view.state, KeyboardMacroTestState::Completed);
        assert_eq!(view.event_count, 2);
        assert!(view.target_label.is_none());
    }
    #[test]
    // Verify that no confirmation or lost heartbeat never plays
    fn no_confirmation_or_lost_heartbeat_never_plays() {
        let now = Instant::now();
        let shared = Shared::new(now);
        let mut fake = Fake::new(&shared, now);
        let (_sender, receiver) = mpsc::sync_channel(1);
        let error = run_session(&mut fake, &shared, &receiver, "main:1:test").unwrap_err();
        assert!(error.contains("heartbeat"));
        assert_eq!(fake.played, 0);
        assert_eq!(fake.probe.finished.get(), 1);
    }
    #[test]
    // Verify that heartbeat cannot keep an unconfirmed target forever
    fn heartbeat_cannot_keep_an_unconfirmed_target_forever() {
        let now = Instant::now();
        let shared = Shared::new(now);
        let mut fake = Fake::new(&shared, now);
        fake.keep_alive = true;
        let (_sender, receiver) = mpsc::sync_channel(1);
        assert!(run_session(&mut fake, &shared, &receiver, "main:1:test")
            .unwrap_err()
            .contains("target expired"));
        assert_eq!(fake.played, 0);
        assert_eq!(fake.probe.finished.get(), 1);
    }
    #[test]
    // Verify that returning to manager or destroying target blocks output
    fn returning_to_manager_or_destroying_target_blocks_output() {
        for destroyed in [false, true] {
            let now = Instant::now();
            let shared = Shared::new(now);
            let mut fake = Fake::new(&shared, now);
            if destroyed {
                fake.destroy_at = Some(Duration::from_secs(4));
            } else {
                fake.probe.focused.set(false);
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            request(&sender);
            assert!(run_session(&mut fake, &shared, &receiver, "main:1:test").is_err());
            assert_eq!(fake.played, 0);
            assert_eq!(fake.probe.finished.get(), 1);
        }
    }
    #[test]
    // Verify that cancellation is checked during both countdowns and playback
    fn cancellation_is_checked_during_both_countdowns_and_playback() {
        for phase in 0..3 {
            let now = Instant::now();
            let shared = Shared::new(now);
            let mut fake = Fake::new(&shared, now);
            match phase {
                0 => fake.cancel_at = Some(Duration::from_secs(1)),
                1 => fake.cancel_at = Some(Duration::from_secs(4)),
                _ => fake.cancel_during_play = true,
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            request(&sender);
            let result = run_session(&mut fake, &shared, &receiver, "main:1:test");
            assert_eq!(result.as_ref().unwrap_err(), CANCELED);
            shared.complete(result);
            assert_eq!(
                shared.snapshot(false).unwrap().state,
                KeyboardMacroTestState::Canceled
            );
            assert_eq!(fake.probe.finished.get(), usize::from(phase != 0));
        }
    }
    #[test]
    // Verify that teardown failure is not hidden by completion or cancellation
    fn teardown_failure_is_not_hidden_by_completion_or_cancellation() {
        for canceled in [false, true] {
            let now = Instant::now();
            let shared = Shared::new(now);
            let mut fake = Fake::new(&shared, now);
            fake.probe.teardown_failure.set(true);
            fake.cancel_during_play = canceled;
            let (sender, receiver) = mpsc::sync_channel(1);
            request(&sender);
            let result = run_session(&mut fake, &shared, &receiver, "main:1:test");
            assert!(result.as_ref().unwrap_err().contains("teardown failed"));
            shared.complete(result);
            let view = shared.snapshot(false).unwrap();
            assert_eq!(view.state, KeyboardMacroTestState::Failed);
            assert!(view.target_label.is_none());
        }
    }
    #[test]
    // Verify that native owner token and ready phase guard one immutable request
    fn native_owner_token_and_ready_phase_guard_one_immutable_request() {
        let now = Instant::now();
        let shared = Arc::new(Shared::new(now));
        let (sender, receiver) = mpsc::sync_channel(1);
        let control = Control {
            owner: "main:7".into(),
            shared,
            sender,
            worker: None,
        };
        let id = control.shared.snapshot(false).unwrap().session_id;
        assert!(owner_matches(&control, "main:7", &id));
        assert!(!owner_matches(&control, "main:8", &id));
        assert!(!owner_matches(&control, "main:7", "stale"));
        assert!(queue_start(&control, recording(), KeyboardMacroPlayback::default(), now).is_err());
        control
            .shared
            .update(|view| view.state = KeyboardMacroTestState::TargetReady)
            .unwrap();
        assert!(queue_start(
            &control,
            KeyboardMacroRecording::default(),
            KeyboardMacroPlayback::default(),
            now
        )
        .is_err());
        queue_start(
            &control,
            recording(),
            KeyboardMacroPlayback {
                focus_policy: KeyboardMacroFocusPolicy::AllowFocusChanges,
                ..Default::default()
            },
            now,
        )
        .unwrap();
        assert!(queue_start(&control, recording(), KeyboardMacroPlayback::default(), now).is_err());
        let Request::Start {
            recording,
            playback,
        } = receiver.try_recv().unwrap();
        assert_eq!(recording.events.len(), 2);
        assert_eq!(
            playback.focus_policy,
            KeyboardMacroFocusPolicy::StayOnTarget
        );
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    // Verify that cancellation is pending until worker teardown finishes without blocking
    fn cancellation_is_pending_until_worker_teardown_finishes_without_blocking() {
        let shared = Arc::new(Shared::new(Instant::now()));
        let (sender, _receiver) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let signal = shared.clone();
        let worker = std::thread::spawn(move || {
            released.recv().unwrap();
            signal.complete(Err(CANCELED.into()));
        });
        let mut control = Control {
            owner: "main:7".into(),
            shared,
            sender,
            worker: Some(worker),
        };
        control.request_cancel().unwrap();
        control.reap();
        assert!(control.running());
        assert_eq!(
            control.shared.snapshot(false).unwrap().state,
            KeyboardMacroTestState::Canceling
        );
        // A late capture/countdown update cannot claim readiness after cancel
        control
            .shared
            .update(|view| {
                view.state = KeyboardMacroTestState::TargetReady;
                view.remaining_ms = 500;
            })
            .unwrap();
        let pending = control.shared.snapshot(false).unwrap();
        assert_eq!(pending.state, KeyboardMacroTestState::Canceling);
        assert_eq!(pending.remaining_ms, 0);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while control.running() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!control.running(), "test worker did not terminate");
        control.reap();
        assert!(control.worker.is_none());
        assert_eq!(
            control.shared.snapshot(false).unwrap().state,
            KeyboardMacroTestState::Canceled
        );
    }
    #[test]
    // Verify that cancellation retries preserve the failed terminal receipt
    fn cancellation_retries_preserve_the_failed_terminal_receipt() {
        let shared = Arc::new(Shared::new(Instant::now()));
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut control = Control {
            owner: "main:7".into(),
            shared,
            sender,
            worker: None,
        };
        let id = control.shared.snapshot(false).unwrap().session_id;
        control.request_cancel().unwrap();
        control
            .shared
            .complete(Err(format!("{CANCELED}; Target observer teardown failed")));
        for _ in 0..2 {
            control.request_cancel().unwrap();
            control.reap();
            let view = control.shared.snapshot(false).unwrap();
            assert_eq!(view.session_id, id);
            assert_eq!(view.state, KeyboardMacroTestState::Failed);
            assert!(view.error.unwrap().contains("teardown failed"));
            assert!(!control.running());
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    // Verify that production test admission stays closed until platform prerequisites pass
    fn production_test_admission_stays_closed_until_platform_prerequisites_pass() {
        assert!(unavailable_reason().is_some());
        assert!(begin("main:1", "editor").is_err());
    }
}
