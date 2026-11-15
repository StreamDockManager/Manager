//! Interruptible sampler deadlines without holding a Runtime alive while idle
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct Signal {
    state: Mutex<(u64, bool)>,
    changed: Condvar,
}
impl Signal {
    // Notify consumers of the sampler change
    pub fn notify(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = state.0.wrapping_add(1);
        self.changed.notify_all();
    }
    // Stop the sampler operation
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
    // Wait for the sampler condition or result
    pub fn wait(&self, observed: &mut u64, timeout: Option<Duration>) -> bool {
        let state = self.state.lock().unwrap();
        let unchanged = |state: &mut (u64, bool)| state.0 == *observed && !state.1;
        let state = match timeout {
            Some(timeout) => {
                self.changed
                    .wait_timeout_while(state, timeout, unchanged)
                    .unwrap()
                    .0
            }
            None => self.changed.wait_while(state, unchanged).unwrap(),
        };
        *observed = state.0;
        !state.1
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that notifications before wait are not lost and stop interrupts waiters
    fn notifications_before_wait_are_not_lost_and_stop_interrupts_waiters() {
        let signal = Signal::default();
        let mut revision = 0;
        signal.notify();
        assert!(signal.wait(&mut revision, None));
        assert_eq!(revision, 1);
        signal.stop();
        assert!(!signal.wait(&mut revision, None));
    }
}
