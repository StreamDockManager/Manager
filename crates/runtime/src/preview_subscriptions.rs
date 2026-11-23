//! Subscription-local wake predicates. Lock order: registry then subscription state.
//! Workers never acquire the registry while holding their state, and never render under it.
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};

#[derive(Default)]
pub struct PreviewSubscriptions {
    entries: Arc<Mutex<HashMap<String, Arc<SubscriptionState>>>>,
}
struct SubscriptionState {
    device: Option<String>,
    state: Mutex<WakeState>,
    wake: Condvar,
}
#[derive(Default)]
struct WakeState {
    canceled: bool,
    dirty: bool,
    outstanding: Option<(String, u64)>,
    generation: u64,
}
/// Worker-owned completion guard; occupancy lasts until worker exit, including unwind.
pub struct PreviewSubscription {
    id: String,
    state: Arc<SubscriptionState>,
    entries: Arc<Mutex<HashMap<String, Arc<SubscriptionState>>>>,
}
impl PreviewSubscriptions {
    pub fn register(&self, id: String) -> PreviewSubscription {
        self.register_device(id, None)
    }
    pub fn register_device(&self, id: String, device: Option<String>) -> PreviewSubscription {
        let state = Arc::new(SubscriptionState {
            device,
            state: Mutex::new(WakeState {
                dirty: true,
                ..Default::default()
            }),
            wake: Condvar::new(),
        });
        let mut entries = self.entries.lock().unwrap();
        assert!(
            !entries.contains_key(&id),
            "duplicate preview subscription identity"
        );
        entries.insert(id.clone(), state.clone());
        PreviewSubscription {
            id,
            state,
            entries: self.entries.clone(),
        }
    }
    pub fn acknowledge(&self, id: &str, generation: &str, sequence: u64) {
        if let Some(entry) = self.entries.lock().unwrap().get(id) {
            let mut state = entry.state.lock().unwrap();
            if !state.canceled
                && state
                    .outstanding
                    .as_ref()
                    .is_some_and(|(g, s)| g == generation && *s == sequence)
            {
                state.outstanding = None;
                state.generation = state.generation.wrapping_add(1);
                entry.wake.notify_one();
            }
        }
    }
    /// Producer state is authoritative; one dirty bit coalesces any scene burst.
    pub fn scene_changed(&self, kind: &str, device: Option<&str>) {
        if ![
            "profiles",
            "devices",
            "sessions",
            "plugin-feedback",
            "display-theme",
            "live-feedback",
            "vault-presentation-changed",
            "foreground",
            "workspace-reset",
        ]
        .contains(&kind)
        {
            return;
        }
        for entry in self.entries.lock().unwrap().values() {
            if device.is_some()
                && entry.device.as_deref().is_some()
                && device != entry.device.as_deref()
            {
                continue;
            }
            let mut state = entry.state.lock().unwrap();
            if !state.canceled && !state.dirty {
                state.dirty = true;
                state.generation = state.generation.wrapping_add(1);
                // No useful work until ACK; do not wake a stalled consumer for scenes.
                if state.outstanding.is_none() {
                    entry.wake.notify_one();
                }
            }
        }
    }
    pub fn is_canceled(&self, id: &str) -> Option<bool> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|entry| entry.state.lock().unwrap().canceled)
    }
    pub fn cancel(&self, id: &str) {
        if let Some(entry) = self.entries.lock().unwrap().get(id) {
            entry.cancel();
        }
    }
    pub fn cancel_all(&self) {
        for entry in self.entries.lock().unwrap().values() {
            entry.cancel();
        }
    }
    pub fn occupied(&self) -> bool {
        self.entries
            .try_lock()
            .map(|entries| !entries.is_empty())
            .unwrap_or(true)
    }
}
impl SubscriptionState {
    fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.canceled = true;
        state.generation = state.generation.wrapping_add(1);
        self.wake.notify_one();
    }
}
impl PreviewSubscription {
    pub fn canceled(&self) -> bool {
        self.state.state.lock().unwrap().canceled
    }
    pub fn ready(&self) -> bool {
        self.state.state.lock().unwrap().outstanding.is_none()
    }
    /// Atomically consume dirty work only with credit and at the frame deadline.
    /// Clean/stalled subscriptions have no timer; cancellation always unblocks them.
    pub fn wait_for_frame(&self, deadline: Instant) -> bool {
        let mut state = self.state.state.lock().unwrap();
        loop {
            if state.canceled {
                return false;
            }
            if state.dirty && state.outstanding.is_none() {
                let now = Instant::now();
                if now >= deadline {
                    state.dirty = false;
                    return true;
                }
                state = self
                    .state
                    .wake
                    .wait_timeout(state, deadline - now)
                    .unwrap()
                    .0;
            } else {
                state = self.state.wake.wait(state).unwrap();
            }
        }
    }
    pub fn refresh(&self, needed: bool) {
        if needed {
            self.state.state.lock().unwrap().dirty = true;
        }
    }
    pub fn sent(&self, generation: &str, sequence: u64) {
        let mut state = self.state.state.lock().unwrap();
        assert!(state.outstanding.is_none());
        state.outstanding = Some((generation.to_owned(), sequence));
    }
}
impl Drop for PreviewSubscription {
    fn drop(&mut self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries
            .get(&self.id)
            .is_some_and(|state| Arc::ptr_eq(state, &self.state))
        {
            entries.remove(&self.id);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_scene_signal_follows_derived_palette_refresh() {
        use std::{sync::mpsc, time::Duration};
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let worker = runtime.preview_subscriptions.register("ordering".into());
        assert!(worker.wait_for_frame(Instant::now()));
        // Hold the derived-update gate: emitting profiles cannot wake the preview yet.
        let palette_update = runtime.profile_theme_sync.lock().unwrap();
        let (started, start) = mpsc::channel();
        let (done, complete) = mpsc::channel();
        let producer = runtime.clone();
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            producer.emit("profiles", None, None, None);
            done.send(()).unwrap();
        });
        start.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(complete.recv_timeout(Duration::from_millis(30)).is_err());
        let signaled_before_refresh = worker.state.state.lock().unwrap().dirty;
        drop(palette_update);
        complete.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        assert!(
            !signaled_before_refresh,
            "preview woke before derived palette refresh"
        );
        assert!(worker.wait_for_frame(Instant::now()));
    }
    #[test]
    fn stalled_bursts_park_until_matching_ack_and_preserve_credit() {
        use std::sync::mpsc;
        use std::time::Duration;
        let registry = PreviewSubscriptions::default();
        let worker = registry.register_device("one".into(), Some("dock".into()));
        assert!(worker.wait_for_frame(Instant::now()));
        worker.sent("one", 1);
        for _ in 0..1000 {
            registry.scene_changed("live-feedback", Some("dock"));
        }
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            tx.send(worker.wait_for_frame(Instant::now())).unwrap();
            worker
        });
        registry.acknowledge("one", "stale", 1);
        registry.acknowledge("one", "one", 2);
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        registry.acknowledge("one", "one", 1);
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        let worker = handle.join().unwrap();
        worker.sent("one", 2);
        registry.acknowledge("one", "one", 1);
        assert!(!worker.ready());
        registry.cancel_all();
        assert!(!worker.wait_for_frame(Instant::now()));
    }
    #[test]
    fn ack_before_wait_and_changes_during_render_are_not_lost() {
        let registry = PreviewSubscriptions::default();
        let worker = registry.register("one".into());
        assert!(worker.wait_for_frame(Instant::now()));
        registry.scene_changed("profiles", None);
        worker.refresh(false); // finishing a static render cannot erase producer dirtiness
        worker.sent("one", 1);
        registry.acknowledge("one", "one", 1);
        assert!(worker.wait_for_frame(Instant::now()));
    }
    #[test]
    fn clean_worker_ignores_other_devices_and_cancellation_unblocks_without_frames() {
        use std::sync::mpsc;
        use std::time::Duration;
        let registry = PreviewSubscriptions::default();
        let worker = registry.register_device("one".into(), Some("dock".into()));
        assert!(worker.wait_for_frame(Instant::now()));
        registry.scene_changed("metrics", None);
        registry.scene_changed("live-feedback", Some("other"));
        assert!(!worker.state.state.lock().unwrap().dirty);
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            tx.send(worker.wait_for_frame(Instant::now())).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        registry.cancel("one");
        assert!(!rx.recv_timeout(Duration::from_secs(1)).unwrap());
        handle.join().unwrap();
        assert!(!registry.occupied());
        let replacement = registry.register("two".into());
        replacement.sent("two", 1);
        registry.acknowledge("one", "one", 1);
        assert!(!replacement.ready());
    }
    #[test]
    fn refresh_and_ack_preserve_real_frame_deadline() {
        use std::time::Duration;
        let registry = PreviewSubscriptions::default();
        let worker = registry.register("one".into());
        let deadline = Instant::now() + Duration::from_millis(25);
        assert!(worker.wait_for_frame(deadline));
        assert!(Instant::now() >= deadline);
        worker.refresh(true); // animation or render retry retains its real deadline
        let deadline = Instant::now() + Duration::from_millis(25);
        assert!(worker.wait_for_frame(deadline));
        assert!(Instant::now() >= deadline);
    }
    #[test]
    // Verify that canceled worker stays occupied and ignores late acknowledgment
    fn canceled_worker_stays_occupied_and_ignores_late_acknowledgment() {
        let registry = PreviewSubscriptions::default();
        let worker = registry.register("one".into());
        assert!(worker.ready());
        worker.sent("epoch", 1);
        assert!(!worker.ready());
        registry.acknowledge("one", "epoch", 1);
        assert!(worker.ready());
        worker.sent("epoch", 1);
        registry.cancel("one");
        registry.cancel("one");
        registry.acknowledge("one", "epoch", 1);
        assert!(worker.canceled());
        assert!(!worker.ready());
        assert!(registry.occupied());
        drop(worker);
        assert!(!registry.occupied());
        registry.acknowledge("one", "epoch", 1);
    }
    #[test]
    // Verify that completion on error and unwind and instance isolation
    fn completion_on_error_and_unwind_and_instance_isolation() {
        let first = PreviewSubscriptions::default();
        let second = PreviewSubscriptions::default();
        let worker = second.register("same".into());
        let result = std::panic::catch_unwind(|| {
            let _guard = first.register("same".into());
            panic!("worker failure");
        });
        assert!(result.is_err());
        assert!(!first.occupied());
        assert!(second.occupied());
        first.cancel_all();
        assert!(!worker.canceled());
        second.cancel_all();
        assert!(worker.canceled());
        drop(worker);
        assert!(!second.occupied());
    }
}
