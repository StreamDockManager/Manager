//! Owner-local generation signal. Never acquire runtime state while holding this mutex.
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[derive(Default)]
struct State {
    revision: u64,
    configuration: u64,
    application: Option<String>,
    native: Option<Arc<dyn Fn() + Send + Sync>>,
}
#[derive(Default)]
pub(crate) struct ObservationSignal {
    state: Mutex<State>,
    changed: Condvar,
}
impl ObservationSignal {
    pub(crate) fn wake(&self) {
        let mut state = self.state.lock().unwrap();
        state.revision = state.revision.wrapping_add(1);
        state.configuration = state.configuration.wrapping_add(1);
        // The native callback only schedules a run-loop wake; it never calls runtime code.
        if let Some(native) = &state.native {
            native();
        }
        self.changed.notify_all();
    }
    pub(super) fn publish(&self, application: String) {
        let mut state = self.state.lock().unwrap();
        state.application = Some(application);
        state.revision = state.revision.wrapping_add(1);
        self.changed.notify_all();
    }
    pub(super) fn take(&self) -> (u64, Option<String>) {
        let mut state = self.state.lock().unwrap();
        (state.revision, state.application.take())
    }
    pub(super) fn wait(&self, revision: u64, timeout: Duration) {
        let state = self.state.lock().unwrap();
        drop(
            self.changed
                .wait_timeout_while(state, timeout, |s| s.revision == revision)
                .unwrap(),
        );
    }
    pub(super) fn configuration(&self) -> u64 {
        self.state.lock().unwrap().configuration
    }
    pub(super) fn native(&self, native: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.state.lock().unwrap().native = native;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pre_wait_notifications_are_not_lost_and_identity_is_coalesced() {
        let signal = ObservationSignal::default();
        let (revision, _) = signal.take();
        signal.publish("first".into());
        signal.publish("second".into());
        let now = std::time::Instant::now();
        signal.wait(revision, Duration::from_secs(2));
        assert!(now.elapsed() < Duration::from_secs(1));
        assert_eq!(signal.take().1.as_deref(), Some("second"));
        let (revision, _) = signal.take();
        signal.wake();
        signal.wait(revision, Duration::from_secs(2));
    }
}
