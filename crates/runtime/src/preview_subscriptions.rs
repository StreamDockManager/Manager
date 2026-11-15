//! A subscription is occupied until its worker finishes, not merely until canceled
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Default)]
pub struct PreviewSubscriptions {
    entries: Arc<Mutex<HashMap<String, Arc<SubscriptionState>>>>,
}
struct SubscriptionState {
    canceled: AtomicBool,
    ready: AtomicBool,
}
/// Worker-owned completion guard. Drop covers every exit path, including unwinding
pub struct PreviewSubscription {
    id: String,
    state: Arc<SubscriptionState>,
    entries: Arc<Mutex<HashMap<String, Arc<SubscriptionState>>>>,
}
impl PreviewSubscriptions {
    // Register a preview subscriptions consumer
    pub fn register(&self, id: String) -> PreviewSubscription {
        let state = Arc::new(SubscriptionState {
            canceled: AtomicBool::new(false),
            ready: AtomicBool::new(true),
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
    // Acknowledge the consumed preview subscriptions update
    pub fn acknowledge(&self, id: &str) {
        if let Some(state) = self.entries.lock().unwrap().get(id) {
            if !state.canceled.load(Ordering::SeqCst) {
                state.ready.store(true, Ordering::Release);
            }
        }
    }
    // Report whether canceled
    pub fn is_canceled(&self, id: &str) -> Option<bool> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|state| state.canceled.load(Ordering::SeqCst))
    }
    // Cancel the pending preview subscriptions operation
    pub fn cancel(&self, id: &str) {
        if let Some(state) = self.entries.lock().unwrap().get(id) {
            state.canceled.store(true, Ordering::SeqCst);
        }
    }
    // Cancel all pending preview subscriptions operations
    pub fn cancel_all(&self) {
        for state in self.entries.lock().unwrap().values() {
            state.canceled.store(true, Ordering::SeqCst);
        }
    }
    // Report the occupied condition for preview subscriptions
    pub fn occupied(&self) -> bool {
        self.entries
            .try_lock()
            .map(|entries| !entries.is_empty())
            .unwrap_or(true)
    }
}
impl PreviewSubscription {
    // Report the canceled condition for preview subscriptions
    pub fn canceled(&self) -> bool {
        self.state.canceled.load(Ordering::SeqCst)
    }
    // Report the ready condition for preview subscriptions
    pub fn ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }
    // Clear subscription readiness after the current preview is sent
    pub fn sent(&self) {
        self.state.ready.store(false, Ordering::Release);
    }
}
impl Drop for PreviewSubscription {
    // Release this preview subscriptions owner when it leaves scope
    fn drop(&mut self) {
        // A poisoned lock must not cause a second panic during worker unwinding
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
    // Verify that canceled worker stays occupied and ignores late acknowledgment
    fn canceled_worker_stays_occupied_and_ignores_late_acknowledgment() {
        let registry = PreviewSubscriptions::default();
        let worker = registry.register("one".into());
        assert!(worker.ready());
        worker.sent();
        assert!(!worker.ready());
        registry.acknowledge("one");
        assert!(worker.ready());
        worker.sent();
        registry.cancel("one");
        registry.cancel("one");
        registry.acknowledge("one");
        assert!(worker.canceled());
        assert!(!worker.ready());
        assert!(registry.occupied());
        drop(worker);
        assert!(!registry.occupied());
        registry.acknowledge("one");
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
