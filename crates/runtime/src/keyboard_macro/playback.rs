//! Bounded, synchronous playback. All waits are cumulative and cancellation-aware;
//! raw event content never enters diagnostics or the ordinary desktop-input queue
use super::{types::*, validation};
use std::time::{Duration, Instant};

pub type PressGuard = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

const CHECK_INTERVAL: Duration = Duration::from_millis(10);

/// Recording and playback share admission with shortcuts, held keys and plugins
pub struct RecordingLease(crate::builtins::native::KeyboardLease);
impl RecordingLease {
    // Check the current playback conditions
    pub fn check(&self) -> Result<(), String> {
        self.0.check()
    }
}
// Attempt recording lease
pub fn try_recording_lease() -> Result<RecordingLease, String> {
    crate::builtins::native::try_recording_lease().map(RecordingLease)
}
// Explain why the current operation is unavailable
pub fn unavailable_reason() -> Option<String> {
    super::platform::playback_unavailable_reason()
}

// Start the selected playback playback
pub fn play(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
    owner: &str,
    press_guard: PressGuard,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    validation::validate(recording, playback)?;
    check()?;
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    let lease = crate::builtins::native::try_keyboard_lease(owner)?;
    let lease_guard = lease.press_guard();
    let press_guard: PressGuard = std::sync::Arc::new(move || {
        press_guard()?;
        lease_guard()
    });
    let mut backend = super::platform::NativeBackend::new(owner)?;
    backend.set_press_guard(press_guard);
    let mut clock = MonotonicClock(Instant::now());
    execute(recording, playback, &mut backend, &mut clock, || {
        check()?;
        lease.check()
    })
}

/// Scratch tests pin a native-owned window in addition to the ordinary sticky
/// focus observer. No caller-supplied process/window identifier is trusted
pub(crate) fn play_in_target(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
    owner: &str,
    target: &mut dyn super::test_target::TestTarget,
    press_guard: PressGuard,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    validation::validate(recording, playback)?;
    if playback.focus_policy != KeyboardMacroFocusPolicy::StayOnTarget {
        return Err("Scratch tests must stay in the selected target".into());
    }
    check()?;
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    target.check_focused()?;
    let lease = crate::builtins::native::try_keyboard_lease(owner)?;
    let lease_guard = lease.press_guard();
    let press_guard: PressGuard = std::sync::Arc::new(move || {
        press_guard()?;
        lease_guard()
    });
    let mut backend = TargetBackend {
        inner: super::platform::NativeBackend::new(owner)?,
        target,
    };
    backend.inner.set_press_guard(press_guard);
    execute(
        recording,
        playback,
        &mut backend,
        &mut MonotonicClock(Instant::now()),
        || {
            check()?;
            lease.check()
        },
    )
}
struct TargetBackend<'a, B> {
    inner: B,
    target: &'a mut dyn super::test_target::TestTarget,
}
impl<B: Backend> Backend for TargetBackend<'_, B> {
    type Code = B::Code;
    // Check playback prerequisites before proceeding
    fn preflight(
        &mut self,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
    ) -> Result<(), String> {
        self.target.check_alive()?;
        self.inner.preflight(recording, playback)
    }
    // Resolve the supplied playback request
    fn resolve(&mut self, key: PhysicalKey) -> Result<Self::Code, String> {
        self.inner.resolve(key)
    }
    // Begin tracking the playback operation
    fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String> {
        if policy != KeyboardMacroFocusPolicy::StayOnTarget {
            return Err("Scratch tests must stay in the selected target".into());
        }
        self.target.check_focused()?;
        self.inner.begin(policy)?;
        self.target.check_focused()
    }
    // Check the current playback conditions
    fn check(&mut self) -> Result<(), String> {
        self.target.check_focused()?;
        self.inner.check()
    }
    // Deliver the playback event to its consumer
    fn emit(&mut self, code: &Self::Code, kind: KeyboardMacroEventKind) -> Result<(), String> {
        // Cleanup must still release this run's keys when the target disappears
        self.inner.emit(code, kind)
    }
    // Finalize the pending playback operation
    fn finish(&mut self) -> Result<(), String> {
        self.inner.finish()
    }
}

/// `emit` may fail after sending: an attempted press remains cleanup-owned until
/// its release succeeds. Codes are resolved once, before begin or any output
/// Put blocking press-admission verification in `check`, not `emit`, so the
/// scheduler can recheck cancellation after verification and before each press
/// Cleanup releases still perform their independent ownership/route checks
pub(crate) trait Backend {
    type Code: Clone + Eq;
    // Set press guard
    fn set_press_guard(&mut self, _guard: PressGuard) {}
    // Check playback prerequisites before proceeding
    fn preflight(
        &mut self,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
    ) -> Result<(), String>;
    // Resolve the supplied playback request
    fn resolve(&mut self, key: PhysicalKey) -> Result<Self::Code, String>;
    // Begin tracking the playback operation
    fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String>;
    // Check the current playback conditions
    fn check(&mut self) -> Result<(), String>;
    // Deliver the playback event to its consumer
    fn emit(&mut self, code: &Self::Code, kind: KeyboardMacroEventKind) -> Result<(), String>;
    /// Detach every scoped observer before completion releases transaction admission
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}
pub(crate) trait Clock {
    // Read the clock used for playback
    fn now(&self) -> Duration;
    // Wait for the playback condition or result
    fn wait(&mut self, duration: Duration);
}
struct MonotonicClock(Instant);
impl Clock for MonotonicClock {
    // Read the clock used for playback
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    // Wait for the playback condition or result
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
struct OwnedOutput<'a, B: Backend> {
    backend: &'a mut B,
    held: Vec<(PhysicalKey, B::Code)>,
    finished: bool,
}
impl<B: Backend> OwnedOutput<'_, B> {
    // Clean up resources retained by playback
    fn cleanup(&mut self) -> Result<(), String> {
        let mut failures = 0;
        for index in (0..self.held.len()).rev() {
            if self
                .backend
                .emit(&self.held[index].1, KeyboardMacroEventKind::KeyUp)
                .is_ok()
            {
                self.held.remove(index);
            } else {
                failures += 1;
            }
        }
        if failures == 0 {
            Ok(())
        } else {
            Err(format!(
                "Keyboard macro cleanup could not release {failures} owned key(s)"
            ))
        }
    }
}
impl<B: Backend> OwnedOutput<'_, B> {
    // Finalize the pending playback operation
    fn finish(&mut self) -> Result<(), String> {
        let cleanup = self.cleanup();
        self.finished = true;
        let teardown = self.backend.finish();
        match (cleanup, teardown) {
            (Err(a), Err(b)) => Err(format!("{a}; {b}")),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}
impl<B: Backend> Drop for OwnedOutput<'_, B> {
    // Release this playback owner when it leaves scope
    fn drop(&mut self) {
        // Unwinding still releases keys before observer teardown. Never send a
        // retry after detaching physical-state observation
        if !self.finished {
            let _ = self.finish();
        }
    }
}
// Execute the requested playback operation
pub(crate) fn execute<B: Backend, C: Clock>(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
    backend: &mut B,
    clock: &mut C,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    validation::validate(recording, playback)?;
    check()?;
    backend.preflight(recording, playback)?;
    let mut resolved = std::collections::HashMap::new();
    let mut schedule = Vec::with_capacity(recording.events.len());
    let mut cumulative_ms = 0u64;
    for (index, event) in recording.events.iter().enumerate() {
        if let std::collections::hash_map::Entry::Vacant(entry) = resolved.entry(event.key) {
            entry.insert(backend.resolve(event.key).map_err(|_| {
                format!("Keyboard macro event {index} has no supported native mapping")
            })?);
        }
        cumulative_ms = cumulative_ms
            .checked_add(u64::from(event.delay_ms))
            .ok_or("Keyboard macro timing overflow")?;
        schedule.push(
            Duration::try_from_secs_f64(cumulative_ms as f64 / 1000.0 / playback.speed)
                .map_err(|_| "Keyboard macro timing overflow")?,
        );
    }
    check()?;
    let mut output = OwnedOutput {
        backend,
        held: vec![],
        finished: false,
    };
    let result = (|| {
        output.backend.begin(playback.focus_policy)?;
        output.backend.check()?;
        let start = clock.now();
        for (event, offset) in recording.events.iter().zip(schedule) {
            let deadline = start
                .checked_add(offset)
                .ok_or("Keyboard macro timing overflow")?;
            loop {
                check()?;
                output.backend.check()?;
                // A fresh native verification may wait for another thread or
                // process. Cancellation during that wait must win over output
                check()?;
                let now = clock.now();
                if now >= deadline {
                    break;
                }
                clock.wait((deadline - now).min(CHECK_INTERVAL));
            }
            // A failed press can have an uncertain native outcome. Reserve the
            // exact code before dispatch so the scope guard still releases it
            match event.kind {
                KeyboardMacroEventKind::KeyDown => {
                    let code = resolved[&event.key].clone();
                    output.held.push((event.key, code.clone()));
                    output
                        .backend
                        .emit(&code, event.kind)
                        .map_err(|_| "Keyboard macro native output failed")?;
                }
                KeyboardMacroEventKind::KeyUp | KeyboardMacroEventKind::Repeat => {
                    let index = output
                        .held
                        .iter()
                        .position(|(key, _)| *key == event.key)
                        .ok_or("Keyboard macro lost key ownership")?;
                    output
                        .backend
                        .emit(&output.held[index].1, event.kind)
                        .map_err(|_| "Keyboard macro native output failed")?;
                    if event.kind == KeyboardMacroEventKind::KeyUp {
                        output.held.remove(index);
                    }
                }
            }
        }
        check()?;
        output.backend.check()
    })();
    let cleanup = output.finish();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::HashSet, rc::Rc};
    #[derive(Default)]
    struct FakeClock {
        time: Rc<Cell<Duration>>,
        waits: Vec<Duration>,
    }
    impl Clock for FakeClock {
        // Read the clock used for playback
        fn now(&self) -> Duration {
            self.time.get()
        }
        // Wait for the playback condition or result
        fn wait(&mut self, duration: Duration) {
            self.waits.push(duration);
            self.time.set(self.time.get() + duration);
        }
    }
    #[derive(Default)]
    struct Fake {
        time: Rc<Cell<Duration>>,
        events: Vec<(u16, KeyboardMacroEventKind, Duration)>,
        held: HashSet<u16>,
        calls: usize,
        fail_at: Option<usize>,
        fail_releases: bool,
        unsupported: Option<PhysicalKey>,
        reject: bool,
        interrupt_at: Option<Duration>,
        began: bool,
        finish_calls: usize,
        fail_finish: bool,
        fail_begin: bool,
        dispatch_cost: Duration,
        check_cost: Duration,
        repeat_mode: KeyboardMacroRepeatMode,
    }
    impl Backend for Fake {
        type Code = u16;
        // Check playback prerequisites before proceeding
        fn preflight(
            &mut self,
            _: &KeyboardMacroRecording,
            playback: &KeyboardMacroPlayback,
        ) -> Result<(), String> {
            self.repeat_mode = playback.repeat_mode;
            if self.reject {
                Err("Capability unavailable".into())
            } else {
                Ok(())
            }
        }
        // Resolve the supplied playback request
        fn resolve(&mut self, key: PhysicalKey) -> Result<u16, String> {
            if self.unsupported == Some(key) {
                return Err("Unsupported".into());
            }
            Ok(key as u16)
        }
        // Begin tracking the playback operation
        fn begin(&mut self, _: KeyboardMacroFocusPolicy) -> Result<(), String> {
            self.began = true;
            if self.fail_begin {
                Err("Observer startup failed".into())
            } else {
                Ok(())
            }
        }
        // Check the current playback conditions
        fn check(&mut self) -> Result<(), String> {
            self.time.set(self.time.get() + self.check_cost);
            if self.interrupt_at.is_some_and(|at| self.time.get() >= at) {
                Err("Target, permission or physical input changed".into())
            } else {
                Ok(())
            }
        }
        // Deliver the playback event to its consumer
        fn emit(&mut self, key: &u16, kind: KeyboardMacroEventKind) -> Result<(), String> {
            assert_eq!(self.finish_calls, 0, "output after observer teardown");
            if super::super::platform::repeat::skip_recorded_repeat(self.repeat_mode, kind) {
                assert!(self.held.contains(key));
                return Ok(());
            }
            let call = self.calls;
            self.calls += 1;
            if self.fail_releases && kind == KeyboardMacroEventKind::KeyUp {
                return Err("release failed".into());
            }
            self.events.push((*key, kind, self.time.get()));
            match kind {
                KeyboardMacroEventKind::KeyDown => {
                    self.held.insert(*key);
                }
                KeyboardMacroEventKind::KeyUp => {
                    self.held.remove(key);
                }
                KeyboardMacroEventKind::Repeat => {
                    assert!(self.held.contains(key));
                }
            }
            self.time.set(self.time.get() + self.dispatch_cost);
            if self.fail_at == Some(call) {
                Err("uncertain native failure".into())
            } else {
                Ok(())
            }
        }
        // Finalize the pending playback operation
        fn finish(&mut self) -> Result<(), String> {
            self.finish_calls += 1;
            if self.fail_finish {
                Err("Observer teardown failed".into())
            } else {
                Ok(())
            }
        }
    }
    // Construct the keyboard recording used by playback tests
    fn recording() -> KeyboardMacroRecording {
        use KeyboardMacroEventKind::*;
        use PhysicalKey::*;
        KeyboardMacroRecording {
            events: [
                (KeyDown, ControlLeft, 0),
                (KeyDown, KeyA, 10),
                (Repeat, KeyA, 100),
                (KeyUp, KeyA, 10),
                (KeyUp, ControlLeft, 10),
            ]
            .into_iter()
            .map(|(kind, key, delay_ms)| KeyboardMacroEvent {
                kind,
                key,
                delay_ms,
            })
            .collect(),
            ..Default::default()
        }
    }
    // Construct an isolated playback fixture
    fn fixture() -> (Fake, FakeClock) {
        let clock = FakeClock::default();
        (
            Fake {
                time: clock.time.clone(),
                ..Default::default()
            },
            clock,
        )
    }
    #[test]
    // Verify that unsupported last key preflights without starting or output
    fn unsupported_last_key_preflights_without_starting_or_output() {
        let (mut fake, mut clock) = fixture();
        let mut recording = recording();
        recording.events.extend([
            KeyboardMacroEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::F24,
                delay_ms: 0,
            },
            KeyboardMacroEvent {
                kind: KeyboardMacroEventKind::KeyUp,
                key: PhysicalKey::F24,
                delay_ms: 0,
            },
        ]);
        fake.unsupported = Some(PhysicalKey::F24);
        assert!(execute(
            &recording,
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(())
        )
        .is_err());
        assert!(fake.events.is_empty());
        assert!(!fake.began);
    }
    #[test]
    // Verify that cancellation during native verification prevents the next press
    fn cancellation_during_native_verification_prevents_the_next_press() {
        let (mut fake, mut clock) = fixture();
        fake.check_cost = Duration::from_millis(80);
        let time = clock.time.clone();
        let result = execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || {
                if time.get() >= Duration::from_millis(100) {
                    Err("Cancelled".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.unwrap_err(), "Cancelled");
        assert!(fake.events.is_empty());
        assert_eq!(fake.finish_calls, 1);
    }
    #[test]
    // Verify that every event failure releases owned keys even after uncertain press
    fn every_event_failure_releases_owned_keys_even_after_uncertain_press() {
        for fail_at in 0..recording().events.len() {
            let (mut fake, mut clock) = fixture();
            fake.fail_at = Some(fail_at);
            assert!(execute(
                &recording(),
                &Default::default(),
                &mut fake,
                &mut clock,
                || Ok(())
            )
            .is_err());
            assert!(fake.held.is_empty(), "failure at {fail_at}");
        }
    }
    #[test]
    // Verify that cancellation before during and after events cleans up
    fn cancellation_before_during_and_after_events_cleans_up() {
        for cancel_ms in [0, 5, 10, 50, 110, 120, 130] {
            let (mut fake, mut clock) = fixture();
            let time = clock.time.clone();
            let result = execute(
                &recording(),
                &Default::default(),
                &mut fake,
                &mut clock,
                || {
                    if time.get() >= Duration::from_millis(cancel_ms) {
                        Err("Cancelled".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            assert!(fake.held.is_empty());
            assert!(clock.waits.iter().all(|wait| *wait <= CHECK_INTERVAL));
        }
    }
    #[test]
    // Verify that cumulative deadlines do not accumulate dispatch latency
    fn cumulative_deadlines_do_not_accumulate_dispatch_latency() {
        let (mut fake, mut clock) = fixture();
        fake.dispatch_cost = Duration::from_millis(3);
        let playback = KeyboardMacroPlayback {
            speed: 2.0,
            ..Default::default()
        };
        execute(&recording(), &playback, &mut fake, &mut clock, || Ok(())).unwrap();
        let times: Vec<_> = fake
            .events
            .iter()
            .map(|event| event.2.as_millis())
            .collect();
        assert_eq!(times, [0, 5, 55, 60, 65]);
        assert!(fake.held.is_empty());
    }
    #[test]
    // Verify that system repeat retains timing and owned releases without duplicate output
    fn system_repeat_retains_timing_and_owned_releases_without_duplicate_output() {
        let (mut fake, mut clock) = fixture();
        let playback = KeyboardMacroPlayback {
            repeat_mode: KeyboardMacroRepeatMode::System,
            ..Default::default()
        };
        execute(&recording(), &playback, &mut fake, &mut clock, || Ok(())).unwrap();
        assert_eq!(
            fake.events
                .iter()
                .map(|event| event.2.as_millis())
                .collect::<Vec<_>>(),
            [0, 10, 120, 130]
        );
        assert!(!fake
            .events
            .iter()
            .any(|event| event.1 == KeyboardMacroEventKind::Repeat));
        assert!(fake.held.is_empty());
        assert_eq!(fake.finish_calls, 1);
    }
    #[test]
    // Verify that system repeat still checks interference and cleans up during hold
    fn system_repeat_still_checks_interference_and_cleans_up_during_hold() {
        let (mut fake, mut clock) = fixture();
        fake.interrupt_at = Some(Duration::from_millis(110));
        let playback = KeyboardMacroPlayback {
            repeat_mode: KeyboardMacroRepeatMode::System,
            ..Default::default()
        };
        assert!(execute(&recording(), &playback, &mut fake, &mut clock, || Ok(())).is_err());
        assert!(!fake
            .events
            .iter()
            .any(|event| event.1 == KeyboardMacroEventKind::Repeat));
        assert!(fake.held.is_empty());
        assert_eq!(fake.finish_calls, 1);
    }
    #[test]
    // Verify that interference focus and permission changes stop during long wait
    fn interference_focus_and_permission_changes_stop_during_long_wait() {
        let (mut fake, mut clock) = fixture();
        fake.interrupt_at = Some(Duration::from_millis(40));
        assert!(execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(())
        )
        .is_err());
        assert_eq!(clock.time.get(), Duration::from_millis(40));
        assert!(fake.held.is_empty());
        assert_eq!(
            fake.events.last().unwrap().0,
            PhysicalKey::ControlLeft as u16
        );
    }
    #[test]
    // Verify that release failure is reported without key content
    fn release_failure_is_reported_without_key_content() {
        let (mut fake, mut clock) = fixture();
        fake.fail_releases = true;
        let error = execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("cleanup"));
        assert!(!error.contains("ControlLeft"));
        assert!(!fake.held.is_empty());
    }
    #[test]
    // Verify that completion waits for observer teardown and reports failure
    fn completion_waits_for_observer_teardown_and_reports_failure() {
        let (mut fake, mut clock) = fixture();
        fake.fail_finish = true;
        assert!(execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(())
        )
        .unwrap_err()
        .contains("teardown"));
        assert!(fake.held.is_empty());
        assert_eq!(fake.finish_calls, 1);
    }
    #[test]
    // Verify that partial observer startup is synchronously torn down
    fn partial_observer_startup_is_synchronously_torn_down() {
        let (mut fake, mut clock) = fixture();
        fake.fail_begin = true;
        assert!(execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(())
        )
        .is_err());
        assert!(fake.events.is_empty());
        assert_eq!(fake.finish_calls, 1);
    }
    #[test]
    // Verify that readiness failure sends nothing
    fn readiness_failure_sends_nothing() {
        let (mut fake, mut clock) = fixture();
        fake.reject = true;
        assert!(execute(
            &recording(),
            &Default::default(),
            &mut fake,
            &mut clock,
            || Ok(())
        )
        .is_err());
        assert!(fake.events.is_empty());
        assert!(!fake.began);
    }
    #[test]
    // Verify that scratch target change stops before next event but owned cleanup still runs
    fn scratch_target_change_stops_before_next_event_but_owned_cleanup_still_runs() {
        struct Target(Rc<Cell<bool>>);
        impl crate::keyboard_macro::test_target::TestTarget for Target {
            // Return the fixture target label
            fn label(&self) -> &str {
                "Scratch target"
            }
            // Reject an input target whose original process is no longer alive
            fn check_alive(&mut self) -> Result<(), String> {
                Ok(())
            }
            // Reject an input target that no longer holds the required focus
            fn check_focused(&mut self) -> Result<(), String> {
                if self.0.get() {
                    Ok(())
                } else {
                    Err("Scratch target changed".into())
                }
            }
        }
        struct ChangesFocus {
            fake: Fake,
            focused: Rc<Cell<bool>>,
        }
        impl Backend for ChangesFocus {
            type Code = u16;
            // Check playback prerequisites before proceeding
            fn preflight(
                &mut self,
                recording: &KeyboardMacroRecording,
                playback: &KeyboardMacroPlayback,
            ) -> Result<(), String> {
                self.fake.preflight(recording, playback)
            }
            // Resolve the supplied playback request
            fn resolve(&mut self, key: PhysicalKey) -> Result<u16, String> {
                self.fake.resolve(key)
            }
            // Begin tracking the playback operation
            fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String> {
                self.fake.begin(policy)
            }
            // Check the current playback conditions
            fn check(&mut self) -> Result<(), String> {
                self.fake.check()
            }
            // Deliver the playback event to its consumer
            fn emit(&mut self, code: &u16, kind: KeyboardMacroEventKind) -> Result<(), String> {
                self.fake.emit(code, kind)?;
                if kind == KeyboardMacroEventKind::KeyDown {
                    self.focused.set(false);
                }
                Ok(())
            }
            // Finalize the pending playback operation
            fn finish(&mut self) -> Result<(), String> {
                self.fake.finish()
            }
        }
        let (fake, mut clock) = fixture();
        let focused = Rc::new(Cell::new(true));
        let mut target = Target(focused.clone());
        let mut backend = TargetBackend {
            inner: ChangesFocus { fake, focused },
            target: &mut target,
        };
        let error = execute(
            &recording(),
            &Default::default(),
            &mut backend,
            &mut clock,
            || Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("target changed"));
        assert_eq!(backend.inner.fake.events.len(), 2);
        assert_eq!(
            backend.inner.fake.events[0].1,
            KeyboardMacroEventKind::KeyDown
        );
        assert_eq!(
            backend.inner.fake.events[1].1,
            KeyboardMacroEventKind::KeyUp
        );
        assert!(backend.inner.fake.held.is_empty());
        assert_eq!(backend.inner.fake.finish_calls, 1);
    }
}
