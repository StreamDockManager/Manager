//! Desired Dock presence is separate from the optional main WebView
use std::time::{Duration, Instant};

// Tao 0.37.2 ignores hides within one second of its last show request
const HIDE_GUARD: Duration = Duration::from_millis(1100);

#[derive(Clone, Copy, Debug)]
struct Transition {
    sequence: u64,
    visible: bool,
    delay: Duration,
}

#[derive(Default)]
struct State {
    desired: bool,
    requested: Option<bool>,
    last_show: Option<Instant>,
    sequence: u64,
    deferred: bool,
}
impl State {
    // Process the request through this subsystem
    fn request(&mut self, visible: bool, now: Instant) -> Option<Transition> {
        if self.desired == visible && (self.deferred || self.requested == Some(visible)) {
            return None;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.desired = visible;
        let delay = if visible {
            self.last_show = Some(now);
            Duration::ZERO
        } else {
            self.last_show
                .map(|shown| HIDE_GUARD.saturating_sub(now.saturating_duration_since(shown)))
                .unwrap_or_default()
        };
        self.deferred = !delay.is_zero();
        if !self.deferred {
            self.requested = Some(visible);
        }
        Some(Transition {
            sequence: self.sequence,
            visible,
            delay,
        })
    }
    // Finish deferred
    fn finish_deferred(&mut self, sequence: u64) -> bool {
        if self.sequence != sequence || self.desired || !self.deferred {
            return false;
        }
        self.deferred = false;
        self.requested = Some(false);
        true
    }
    // Handle the native operation failure
    fn failed(&mut self, sequence: u64) {
        if self.sequence == sequence {
            self.requested = None;
            self.deferred = false;
        }
    }
    // Cancel for the current operation
    fn cancel(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        self.deferred = false;
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::editor_lifecycle::EditorLifecycle;
    use std::sync::{Arc, Mutex};
    use tauri::Manager;

    #[derive(Default)]
    pub struct Controller {
        state: Mutex<State>,
        timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    }
    impl Controller {
        // Cancel for the current operation
        pub fn cancel(&self) {
            self.state.lock().unwrap().cancel();
            self.cancel_timer();
        }
        // Cancel timer
        fn cancel_timer(&self) {
            if let Some(timer) = self.timer.lock().unwrap().take() {
                timer.abort();
            }
        }
    }

    // Apply for the current operation
    fn apply(app: &tauri::AppHandle, transition: Transition) {
        // Tauri acknowledges dispatch, not an observed change in the Dock
        if let Err(error) = app.set_dock_visibility(transition.visible) {
            app.state::<Arc<EditorLifecycle>>()
                .dock
                .state
                .lock()
                .unwrap()
                .failed(transition.sequence);
            crate::manager_log::warning("Could not update the application Dock visibility");
            eprintln!("Could not update Stream Dock Manager Dock visibility: {error}");
            crate::editor_lifecycle::show_error(
                app,
                format!("Could not update the application Dock icon: {error}. The tray remains available."),
            );
        }
    }

    // Process the request through this subsystem
    pub fn request(app: &tauri::AppHandle, visible: bool) {
        let core = app.state::<Arc<EditorLifecycle>>();
        let transition = core
            .dock
            .state
            .lock()
            .unwrap()
            .request(visible, Instant::now());
        let Some(transition) = transition else { return };
        core.dock.cancel_timer();
        if transition.delay.is_zero() {
            apply(app, transition);
            return;
        }
        let app = app.clone();
        let timer = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(transition.delay).await;
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                let core = handle.state::<Arc<EditorLifecycle>>();
                {
                    let owner = core.owner.lock().unwrap();
                    if owner.present || owner.creating || owner.pending_reveal || owner.pending_quit
                    {
                        return;
                    }
                }
                let ready = core
                    .dock
                    .state
                    .lock()
                    .unwrap()
                    .finish_deferred(transition.sequence);
                if ready {
                    apply(&handle, transition);
                }
            });
        });
        *core.dock.timer.lock().unwrap() = Some(timer);
    }
}
#[cfg(target_os = "macos")]
pub use native::{request, Controller};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify background start hides and duplicate requests coalesce
    fn background_start_hides_and_duplicate_requests_coalesce() {
        let mut state = State::default();
        let now = Instant::now();
        let hide = state.request(false, now).unwrap();
        assert!(!hide.visible && hide.delay.is_zero());
        assert!(state.request(false, now).is_none());
        assert!(state.request(true, now).unwrap().visible);
        assert!(state.request(true, now).is_none());
    }
    #[test]
    // Verify rapid close waits for tao guard without delaying window destruction
    fn rapid_close_waits_for_tao_guard_without_delaying_window_destruction() {
        let mut state = State::default();
        let now = Instant::now();
        state.request(true, now).unwrap();
        let hide = state
            .request(false, now + Duration::from_millis(100))
            .unwrap();
        assert_eq!(hide.delay, Duration::from_millis(1000));
        assert!(state.request(false, now).is_none());
        assert!(state.finish_deferred(hide.sequence));
        assert!(!state.finish_deferred(hide.sequence));
        assert!(state.request(false, now + HIDE_GUARD).is_none());
    }
    #[test]
    // Verify reopen and quit cancel stale hides
    fn reopen_and_quit_cancel_stale_hides() {
        let mut state = State::default();
        let now = Instant::now();
        state.request(true, now);
        let old = state.request(false, now).unwrap();
        state.request(true, now);
        assert!(!state.finish_deferred(old.sequence));
        let next = state.request(false, now).unwrap();
        state.cancel();
        assert!(!state.finish_deferred(next.sequence));
    }
    #[test]
    // Verify failed dispatch can retry without invalidating newer state
    fn failed_dispatch_can_retry_without_invalidating_newer_state() {
        let mut state = State::default();
        let now = Instant::now();
        let hide = state.request(false, now).unwrap();
        state.failed(hide.sequence);
        assert!(state.request(false, now).is_some());
        let shown = state.request(true, now).unwrap();
        state.failed(hide.sequence);
        assert_eq!(state.requested, Some(true));
        state.failed(shown.sequence);
        assert!(state.request(true, now).is_some());
    }
}
