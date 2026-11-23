use super::{ActivationTicket, Runtime, State};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};

impl State {
    // Reuse a live FIFO execution lane and discard expired lane references
    pub(super) fn lane(&self, key: String) -> Arc<Mutex<VecDeque<ActivationTicket>>> {
        let mut lanes = self.lanes.lock().unwrap();
        if let Some(lane) = lanes.get(&key).and_then(Weak::upgrade) {
            return lane;
        }
        // Keep ownership with active FIFO users, and retire metadata when a new
        // context arrives. The existing active-run limit bounds live contexts
        lanes.retain(|_, lane| lane.strong_count() > 0);
        let lane = Arc::new(Mutex::new(VecDeque::new()));
        lanes.insert(key, Arc::downgrade(&lane));
        lane
    }
}

/// Each Run clone holds this lease. Finished debug sessions may remain in the
/// UI event history, but only actual execution users retain mutable node state
pub(super) struct DebugStateLease {
    runtime: Weak<Runtime>,
    prefix: String,
}
impl DebugStateLease {
    // Initialize execution state state from the supplied inputs
    pub(super) fn new(runtime: &Arc<Runtime>, id: &str) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            prefix: format!("debug:{id}|"),
        })
    }
}
impl Drop for DebugStateLease {
    // Release this execution state owner when it leaves scope
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let state = &runtime.builtin_state;
        state
            .indices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| !key.starts_with(&self.prefix));
        state
            .cooldowns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| !key.starts_with(&self.prefix));
        state.taps.cleanup_prefix(&self.prefix);
    }
}
