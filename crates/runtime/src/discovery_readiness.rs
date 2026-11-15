use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    complete: bool,
    stopped: bool,
}
#[derive(Default)]
pub(crate) struct Readiness {
    state: Mutex<State>,
    changed: Condvar,
}
impl Readiness {
    // Finalize the pending discovery readiness operation
    pub(crate) fn finish(&self) {
        self.state.lock().unwrap().complete = true;
        self.changed.notify_all();
    }
    // Stop the discovery readiness operation
    pub(crate) fn stop(&self) {
        self.state.lock().unwrap().stopped = true;
        self.changed.notify_all();
    }
    // Wait for the discovery readiness condition or result
    pub(crate) fn wait(&self, timeout: Duration) -> Result<(), String> {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), timeout, |state| {
                !state.complete && !state.stopped
            })
            .unwrap();
        if state.complete {
            Ok(())
        } else if state.stopped {
            Err("Device discovery stopped before its first pass".into())
        } else {
            Err("Device discovery timed out before its first pass".into())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that completed empty pass is ready without delay
    fn completed_empty_pass_is_ready_without_delay() {
        let ready = Readiness::default();
        ready.finish();
        assert!(ready.wait(Duration::ZERO).is_ok());
    }
    #[test]
    // Verify that readiness is bounded and stop wakes waiters
    fn readiness_is_bounded_and_stop_wakes_waiters() {
        let ready = std::sync::Arc::new(Readiness::default());
        assert!(ready
            .wait(Duration::ZERO)
            .unwrap_err()
            .contains("timed out"));
        let waiting = ready.clone();
        let thread = std::thread::spawn(move || waiting.wait(Duration::from_secs(10)));
        ready.stop();
        assert!(thread.join().unwrap().unwrap_err().contains("stopped"));
    }
    #[test]
    // Verify that completion wakes waiters and is latched
    fn completion_wakes_waiters_and_is_latched() {
        let ready = std::sync::Arc::new(Readiness::default());
        let waiting = ready.clone();
        let thread = std::thread::spawn(move || waiting.wait(Duration::from_secs(10)));
        ready.finish();
        assert!(thread.join().unwrap().is_ok());
        assert!(ready.wait(Duration::ZERO).is_ok());
    }
}
