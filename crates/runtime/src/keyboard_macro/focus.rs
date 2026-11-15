//! Sticky, session-scoped target-change state. Notifications carry no window title,
//! keyboard content or persistent identifiers. A change cannot be undone by refocus
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const HEALTH_TIMEOUT_MS: u64 = 500;
pub(crate) struct State {
    pub stop: AtomicBool,
    generation: AtomicU64,
    failed: AtomicBool,
    failure: OnceLock<String>,
    ready: AtomicBool,
    pulse_ms: AtomicU64,
    requested_check: AtomicU64,
    completed_check: AtomicU64,
    origin: Instant,
}
impl State {
    // Initialize focus state from the supplied inputs
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            failure: OnceLock::new(),
            ready: AtomicBool::new(false),
            pulse_ms: AtomicU64::new(0),
            requested_check: AtomicU64::new(0),
            completed_check: AtomicU64::new(0),
            origin: Instant::now(),
        }
    }
    // Respond to a focus state change
    pub fn changed(&self) {
        let _ = self
            .generation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                Some(value.saturating_add(1))
            });
    }
    // Mark the focus operation as failed
    pub fn fail(&self) {
        self.fail_with_reason("Keyboard macro target observer is unavailable".into());
    }
    // Invalidate focus admission and record the failure reason
    fn fail_with_reason(&self, reason: String) {
        // Native errors contain only operation names/status codes, never titles or
        // input. Preserve the first failure, including after readiness was sent
        let _ = self.failure.set(reason);
        self.failed.store(true, Ordering::Release);
    }
    // Read the reason focus admission was invalidated
    fn failure_reason(&self) -> Option<String> {
        self.failure.get().cloned()
    }
    // Read the current timestamp in milliseconds
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
    // Deliver a focus wake signal
    pub fn pulse(&self) {
        self.pulse_ms.store(self.now_ms(), Ordering::Release);
    }
    // Publish readiness of the focus observer
    pub fn ready(&self) {
        self.pulse();
        self.ready.store(true, Ordering::Release);
    }
    #[cfg(all(target_os = "macos", feature = "native-session-probe"))]
    // Read the observer generation used in diagnostics
    pub(crate) fn diagnostic_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    // Report the stopped condition for focus
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    // Check the current focus conditions
    pub fn check(&self) -> Result<(), String> {
        self.check_at(self.now_ms())
    }
    // Request check
    fn request_check(&self) -> Result<u64, String> {
        self.check()?;
        self.requested_check
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| "Keyboard macro target verification limit reached".into())
    }
    /// Read before sampling the native target. A request arriving during that
    /// sample must wait for the next sample, never consume this older result
    pub fn requested_check(&self) -> u64 {
        self.requested_check.load(Ordering::Acquire)
    }
    #[cfg(any(target_os = "macos", test))]
    // Report the need for check
    pub fn needs_check(&self) -> bool {
        self.requested_check() > self.completed_check.load(Ordering::Acquire)
    }
    // Complete check
    pub fn complete_check(&self, request: u64) {
        self.completed_check.fetch_max(request, Ordering::Release);
    }
    // Check completed
    fn check_completed(&self, request: u64) -> Result<bool, String> {
        self.check()?;
        Ok(self.completed_check.load(Ordering::Acquire) >= request)
    }
    // Check at
    fn check_at(&self, now_ms: u64) -> Result<(), String> {
        if self.failed.load(Ordering::Acquire) {
            return Err(self
                .failure_reason()
                .unwrap_or_else(|| "Keyboard macro target observer is unavailable".into()));
        }
        if !self.ready.load(Ordering::Acquire)
            || now_ms.saturating_sub(self.pulse_ms.load(Ordering::Acquire)) > HEALTH_TIMEOUT_MS
        {
            return Err("Keyboard macro target observer is unavailable".into());
        }
        if self.generation.load(Ordering::Acquire) != 0 {
            return Err("Keyboard macro stopped because its target changed".into());
        }
        Ok(())
    }
}
pub(crate) struct Observer {
    pub state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}
impl Observer {
    // Start the focus operation
    pub fn start(
        run: impl FnOnce(Arc<State>, mpsc::SyncSender<Result<(), String>>) -> Result<(), String>
            + Send
            + 'static,
    ) -> Result<Self, String> {
        let state = Arc::new(State::new());
        let signal = state.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("keyboard-target".into())
            .spawn(move || {
                let startup = tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(signal.clone(), tx)
                }));
                let failure = match result {
                    Ok(Err(error)) => Some(error),
                    Err(_) => Some("Keyboard macro target observer panicked".into()),
                    Ok(Ok(())) if !signal.stopped() => {
                        Some("Keyboard macro target observer stopped unexpectedly".into())
                    }
                    Ok(Ok(())) => None,
                };
                if let Some(error) = failure {
                    signal.fail_with_reason(error.clone());
                    let _ = startup.try_send(Err(error));
                }
            })
            .map_err(|_| "Keyboard macro target observer could not start")?;
        let observer = Self {
            state,
            worker: Some(worker),
        };
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|_| "Keyboard macro target observer did not become ready")??;
        observer.check()?;
        Ok(observer)
    }
    // Stop the focus operation
    pub fn stop(&mut self) -> Result<(), String> {
        self.state.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                self.state.fail();
            }
        }
        if self.state.failed.load(Ordering::Acquire) {
            Err(self
                .state
                .failure_reason()
                .unwrap_or_else(|| "Keyboard macro target observer failed during teardown".into()))
        } else {
            Ok(())
        }
    }
    // Check the current focus conditions
    pub fn check(&self) -> Result<(), String> {
        if self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(self
                .state
                .failure_reason()
                .unwrap_or_else(|| "Keyboard macro target observer stopped".into()));
        }
        self.state.check()
    }
    /// A live observer and a recent heartbeat are not a fresh focus sample
    /// Native reads stay on their owning worker; this bounded wait acknowledges
    /// only a sample requested by this caller. Timeout never schedules input
    #[cfg(any(target_os = "windows", test))]
    // Reject observations whose validity window has expired
    pub fn check_fresh(&self) -> Result<(), String> {
        self.check_fresh_with_timeout(Duration::from_millis(150))
    }
    /// A backend's wait must cover an in-flight sample that predates this
    /// request plus one new sample. Both remain subject to the health watchdog
    pub fn check_fresh_with_timeout(&self, timeout: Duration) -> Result<(), String> {
        self.check()?;
        let request = self.state.request_check()?;
        let deadline = Instant::now() + timeout;
        loop {
            self.check()?;
            if self.state.check_completed(request)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Keyboard macro target verification timed out".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Observer {
    // Release this focus owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that startup preserves native failure instead of replacing it
    fn startup_preserves_native_failure_instead_of_replacing_it() {
        let error = match Observer::start(|_, _| {
            Err("Reading focused window failed (AX error -25204)".into())
        }) {
            Ok(_) => panic!("startup should fail"),
            Err(error) => error,
        };
        assert_eq!(error, "Reading focused window failed (AX error -25204)");
    }
    #[test]
    // Verify that failures after readiness remain actionable
    fn failures_after_readiness_remain_actionable() {
        let (fail_tx, fail_rx) = mpsc::channel();
        let mut observer = Observer::start(move |state, startup| {
            state.ready();
            startup.send(Ok(())).unwrap();
            while !state.stopped() {
                match fail_rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(()) => {
                        return Err("Window notifications unavailable (AX error -25207)".into())
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => state.pulse(),
                }
            }
            Ok(())
        })
        .unwrap();
        fail_tx.send(()).unwrap();
        while !observer.worker.as_ref().unwrap().is_finished() {
            std::thread::yield_now();
        }
        assert_eq!(
            observer.check().unwrap_err(),
            "Window notifications unavailable (AX error -25207)"
        );
        assert!(observer.stop().unwrap_err().contains("-25207"));
    }
    #[test]
    // Verify that unexpected successful exit is a failure not readiness
    fn unexpected_successful_exit_is_a_failure_not_readiness() {
        let error = match Observer::start(|_, _| Ok(())) {
            Ok(_) => panic!("startup should fail"),
            Err(error) => error,
        };
        assert!(error.contains("stopped unexpectedly"));
    }
    #[test]
    // Verify that first failure is not overwritten by teardown
    fn first_failure_is_not_overwritten_by_teardown() {
        let state = State::new();
        state.ready();
        state.fail_with_reason("specific native failure".into());
        state.fail();
        assert_eq!(state.check().unwrap_err(), "specific native failure");
    }
    #[test]
    // Verify that target change remains stopped after return to original window
    fn target_change_remains_stopped_after_return_to_original_window() {
        let state = State::new();
        state.ready();
        assert!(state.check_at(0).is_ok());
        state.changed();
        state.changed();
        state.pulse();
        assert!(state.check_at(1).unwrap_err().contains("target changed"));
        assert_eq!(state.generation.load(Ordering::Acquire), 2);
    }
    #[test]
    // Verify that missing observer and stalled dispatch fail closed
    fn missing_observer_and_stalled_dispatch_fail_closed() {
        let state = State::new();
        assert!(state.check_at(0).is_err());
        state.ready();
        let pulse = state.pulse_ms.load(Ordering::Acquire);
        assert!(state.check_at(pulse).is_ok());
        assert!(state.check_at(pulse + HEALTH_TIMEOUT_MS + 1).is_err());
        state.fail();
        assert!(state.check_at(0).is_err());
    }
    #[test]
    // Verify that target generation saturates instead of reopening guard
    fn target_generation_saturates_instead_of_reopening_guard() {
        let state = State::new();
        state.ready();
        state.generation.store(u64::MAX, Ordering::Release);
        state.changed();
        assert_eq!(state.generation.load(Ordering::Acquire), u64::MAX);
        assert!(state.check_at(0).is_err());
    }
    #[test]
    // Verify that fresh verification never accepts a sample started before its request
    fn fresh_verification_never_accepts_a_sample_started_before_its_request() {
        let state = State::new();
        state.ready();
        let first = state.request_check().unwrap();
        let sampled_request = state.requested_check();
        let second = state.request_check().unwrap();
        state.complete_check(sampled_request);
        assert!(state.check_completed(first).unwrap());
        assert!(!state.check_completed(second).unwrap());
        assert!(state.needs_check());
        state.complete_check(state.requested_check());
        assert!(state.check_completed(second).unwrap());
        assert!(!state.needs_check());
        state.changed();
        assert!(state.check_completed(second).is_err());
    }
    #[test]
    // Verify that verification request limit fails closed instead of wrapping
    fn verification_request_limit_fails_closed_instead_of_wrapping() {
        let state = State::new();
        state.ready();
        state.requested_check.store(u64::MAX, Ordering::Release);
        assert!(state.request_check().is_err());
        assert_eq!(state.requested_check(), u64::MAX);
    }
    #[test]
    // Verify that a live observer cannot substitute heartbeats for fresh verification
    fn a_live_observer_cannot_substitute_heartbeats_for_fresh_verification() {
        let allow_verification = Arc::new(AtomicBool::new(false));
        let allow = allow_verification.clone();
        let mut observer = Observer::start(move |state, startup| {
            state.ready();
            startup.send(Ok(())).unwrap();
            while !state.stopped() {
                if allow.load(Ordering::Acquire) && state.needs_check() {
                    state.complete_check(state.requested_check());
                }
                state.pulse();
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        })
        .unwrap();
        assert!(observer.check().is_ok());
        assert!(observer.check_fresh().unwrap_err().contains("timed out"));
        allow_verification.store(true, Ordering::Release);
        assert!(observer.check_fresh().is_ok());
        observer.state.changed();
        assert!(observer
            .check_fresh()
            .unwrap_err()
            .contains("target changed"));
        observer.stop().unwrap();
    }
}
