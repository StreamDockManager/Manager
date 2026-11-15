//! Manager gesture recognition. These timings are user-configured, not a claim
//! about the undocumented timing or nested event contract of Elgato Key Logic
use super::*;

// Validate plugins
pub(crate) fn validate_plugins<'a>(
    action: &Action,
    lookup: impl Fn(&str) -> Option<&'a Action>,
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
) -> Result<(), String> {
    #[derive(Default)]
    struct Budget {
        checked: std::collections::HashSet<(String, bool, usize)>,
        active: std::collections::HashSet<String>,
        visits: usize,
    }
    // Visit the current key logic value
    fn visit<'a>(
        action: &Action,
        lookup: &impl Fn(&str) -> Option<&'a Action>,
        plugins: &[stream_dock_manager_plugin_host::PluginInfo],
        inside: bool,
        depth: usize,
        budget: &mut Budget,
    ) -> Result<(), String> {
        budget.visits += 1;
        if budget.visits > 10_000 {
            return Err("Action eligibility check exceeds 10000 nodes".into());
        }
        if depth > 8 {
            return Err("Expanded action nesting exceeds 8".into());
        }
        let inside = inside
            || matches!(action, Action::Builtin { action } if matches!(**action, BuiltinAction::Gesture { .. }));
        if inside {
            if let Action::Plugin {
                plugin_id,
                action_id,
                ..
            } = action
            {
                validate_plugin(plugin_id, action_id, plugins)?;
            }
        }
        if let Action::Builtin { action } = action {
            if let BuiltinAction::SavedMacro { macro_id, .. } = action.as_ref() {
                if let Some(saved) = lookup(macro_id) {
                    if budget.active.contains(macro_id) {
                        return Err("Saved macro reference cycle".into());
                    }
                    let key = (macro_id.clone(), inside, depth + 1);
                    if !budget.checked.contains(&key) {
                        budget.active.insert(macro_id.clone());
                        visit(saved, lookup, plugins, inside, depth + 1, budget)?;
                        budget.active.remove(macro_id);
                        budget.checked.insert(key);
                    }
                }
            }
        }
        if let Action::Toggle { on, off } = action {
            visit(on, lookup, plugins, inside, depth + 1, budget)?;
            visit(off, lookup, plugins, inside, depth + 1, budget)?;
        } else {
            for child in action.children() {
                visit(child, lookup, plugins, inside, depth + 1, budget)?;
            }
        }
        Ok(())
    }
    visit(action, &lookup, plugins, false, 0, &mut Budget::default())
}

// Check for gesture
pub(super) fn contains_gesture(action: &Action, macros: &MacroLibrary) -> Result<bool, String> {
    struct Scan<'a> {
        macros: &'a MacroLibrary,
        checked: HashMap<(String, usize), bool>,
        active: std::collections::HashSet<String>,
        visits: usize,
    }
    impl Scan<'_> {
        // Visit the current key logic value
        fn visit(&mut self, action: &Action, depth: usize) -> Result<bool, String> {
            self.visits += 1;
            if self.visits > 10_000 {
                return Err("Action eligibility check exceeds 10000 nodes".into());
            }
            if depth > 8 {
                return Err("Expanded action nesting exceeds 8".into());
            }
            if let Action::Builtin { action } = action {
                if matches!(**action, BuiltinAction::Gesture { .. }) {
                    return Ok(true);
                }
                if let BuiltinAction::SavedMacro { macro_id, .. } = action.as_ref() {
                    if self.active.contains(macro_id) {
                        return Err("Saved macro reference cycle".into());
                    }
                    let key = (macro_id.clone(), depth + 1);
                    if let Some(found) = self.checked.get(&key) {
                        return Ok(*found);
                    }
                    if let Some(saved) = self.macros.iter().find(|saved| saved.id == *macro_id) {
                        self.active.insert(macro_id.clone());
                        let found = self.visit(&saved.action, depth + 1)?;
                        self.active.remove(macro_id);
                        self.checked.insert(key, found);
                        return Ok(found);
                    }
                }
            }
            let mut index = 0;
            while let Some(child) = action.child(index) {
                if self.visit(child, depth + 1)? {
                    return Ok(true);
                }
                index += 1;
            }
            Ok(false)
        }
    }
    Scan {
        macros,
        checked: Default::default(),
        active: Default::default(),
        visits: 0,
    }
    .visit(action, 0)
}

// Validate plugin
pub(super) fn validate_plugin(
    plugin_id: &str,
    action_id: &str,
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
) -> Result<(), String> {
    if let Some(plugin) = plugins.iter().find(|plugin| plugin.id == plugin_id) {
        if plugin.dialect == "streamdeck" && plugin.capabilities.is_none() {
            if let Some(declared) = plugin.actions.iter().find(|action| action.id == action_id) {
                if !declared.supported_in_key_logic_actions {
                    return Err(format!("{} does not support key logic", declared.name));
                }
                // Retain the existing nested pulse protocol; do not infer
                // an undocumented vendor Key Logic event contract
                if !declared.supported_in_multi_actions {
                    return Err(format!("{} cannot run in Manager key logic: nested plugin execution also requires multi-action support", declared.name));
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct TapWindows {
    windows: Mutex<HashMap<String, Arc<TapWindow>>>,
    recognitions: Mutex<Recognitions>,
}
#[derive(Default)]
struct Recognitions {
    active: HashMap<String, (String, Weak<AtomicBool>)>,
    retired: HashMap<String, Instant>,
    all_retired: Option<Instant>,
}
struct TapWindow {
    deadline: Instant,
    paired: AtomicBool,
}
pub(super) struct TapLease<'a> {
    windows: &'a TapWindows,
    key: String,
    window: Arc<TapWindow>,
}
impl TapWindows {
    // Register cancellable gesture recognition unless its admission was already retired
    fn recognition(
        &self,
        key: &str,
        device: &str,
        admitted: Instant,
    ) -> Result<Arc<AtomicBool>, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut state = self.recognitions.lock().unwrap();
        if state.all_retired.is_some_and(|time| admitted <= time)
            || state
                .retired
                .get(device)
                .is_some_and(|time| admitted <= *time)
        {
            return Err("Cancelled".into());
        }
        state
            .active
            .retain(|_, (_, value)| value.strong_count() > 0);
        state.active.insert(
            format!("{key}:{}", uuid::Uuid::new_v4()),
            (device.into(), Arc::downgrade(&cancelled)),
        );
        Ok(cancelled)
    }
    #[cfg(test)]
    // Count gesture windows still awaiting recognition
    pub(super) fn pending(&self) -> usize {
        self.windows.lock().unwrap().len()
    }
    #[cfg(test)]
    // Report the contains condition for key logic
    pub(super) fn contains(&self, key: &str) -> bool {
        self.windows.lock().unwrap().contains_key(key)
    }
    /// Two releases before the first release's deadline form one pair. Remove
    /// the pair immediately, so a third release starts a fresh recognition
    pub(super) fn register(
        &self,
        key: String,
        now: Instant,
        duration: Duration,
    ) -> Option<TapLease<'_>> {
        let mut windows = self.windows.lock().unwrap();
        if let Some(window) = windows.get(&key) {
            if now < window.deadline {
                window.paired.store(true, Ordering::SeqCst);
                windows.remove(&key);
                return None;
            }
        }
        let window = Arc::new(TapWindow {
            deadline: now + duration,
            paired: AtomicBool::new(false),
        });
        windows.insert(key.clone(), window.clone());
        Some(TapLease {
            windows: self,
            key,
            window,
        })
    }
    // Clean up resources retained by key logic
    pub(super) fn cleanup(&self, device: Option<&str>) {
        {
            let mut state = self
                .recognitions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let now = Instant::now();
            if let Some(device) = device {
                state.retired.insert(device.into(), now);
            } else {
                state.all_retired = Some(now);
                state.retired.clear();
            }
            state.active.retain(|_, (owner, value)| {
                let Some(value) = value.upgrade() else {
                    return false;
                };
                if device.is_none_or(|device| device == owner) {
                    value.store(true, Ordering::SeqCst);
                    false
                } else {
                    true
                }
            });
        }
        self.cleanup_prefix(
            &device
                .map(|device| format!("{device}|"))
                .unwrap_or_default(),
        );
    }
    // Cancel and remove gesture recognition owned by the matching context prefix
    pub(super) fn cleanup_prefix(&self, prefix: &str) {
        self.recognitions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .retain(|key, (_, value)| {
                let Some(value) = value.upgrade() else {
                    return false;
                };
                if key.starts_with(prefix) {
                    value.store(true, Ordering::SeqCst);
                    false
                } else {
                    true
                }
            });
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        windows.retain(|key, window| {
            let retain = !key.starts_with(prefix);
            if !retain {
                // A retired owner must never fire even if its cancellation
                // check races cleanup. This is not a new double activation
                window.paired.store(true, Ordering::SeqCst);
            }
            retain
        });
    }
}
impl Drop for TapLease<'_> {
    // Release this key logic owner when it leaves scope
    fn drop(&mut self) {
        let mut windows = self.windows.windows.lock().unwrap();
        if windows
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.window))
        {
            windows.remove(&self.key);
        }
    }
}

// Execute gesture
pub(super) fn execute_gesture(
    run: &mut Run,
    action: &BuiltinAction,
    depth: usize,
    path: &[usize],
) -> Result<(), String> {
    let cancelled = run.runtime.builtin_state.taps.recognition(
        &run.key(),
        &run.request.device_id,
        run.request.queued_at,
    )?;
    run.branch_cancellations.push(cancelled);
    let previous = run.key_logic;
    run.key_logic = true;
    let result = recognize_gesture(run, action, depth, path);
    run.key_logic = previous;
    run.branch_cancellations.pop();
    result
}
// Match input transitions to the configured gesture
fn recognize_gesture(
    run: &mut Run,
    action: &BuiltinAction,
    depth: usize,
    path: &[usize],
) -> Result<(), String> {
    let BuiltinAction::Gesture {
        double_ms,
        hold_ms,
        children,
    } = action
    else {
        unreachable!()
    };
    if !matches!(run.request.input, Input::Button { pressed: true }) {
        return Err("Key logic requires button release events".into());
    }
    let check_plugins = |run: &Run| {
        validate_plugins(
            &Action::Builtin {
                action: Box::new(action.clone()),
            },
            |id| {
                run.macros
                    .iter()
                    .find(|saved| saved.id == id)
                    .map(|saved| &saved.action)
            },
            &run.runtime.plugins.lock().unwrap().list(),
        )
    };
    check_plugins(run)?;
    let start = Instant::now();
    while run.pressed.load(Ordering::SeqCst) && start.elapsed().as_millis() < *hold_ms as u128 {
        run.sleep(10)?;
    }
    run.check()?;
    if run.pressed.load(Ordering::SeqCst) {
        check_plugins(run)?;
        branch(run, children, 2, depth, path)?;
        return run.release(0);
    }
    let runtime = run.runtime.clone();
    let key = format!(
        "{}:{path:?}:{}:{}:{:?}",
        run.key(),
        run.request.cancellation_generation,
        run.device_generation,
        run.request.connection_epoch
    );
    let lease = runtime.builtin_state.taps.register(
        key,
        Instant::now(),
        Duration::from_millis(*double_ms as u64),
    );
    let branch_index = if let Some(lease) = lease {
        while Instant::now() < lease.window.deadline {
            run.check()?;
            if lease.window.paired.load(Ordering::SeqCst) {
                return Ok(());
            }
            run.sleep(10)?;
        }
        // Serialize expiry with a possible second tap, and never delete a new
        // window owned by a subsequent activation or cancellation generation
        let paired = {
            let mut windows = runtime.builtin_state.taps.windows.lock().unwrap();
            if windows
                .get(&lease.key)
                .is_some_and(|current| Arc::ptr_eq(current, &lease.window))
            {
                windows.remove(&lease.key);
            }
            lease.window.paired.load(Ordering::SeqCst)
        };
        if paired {
            return Ok(());
        }
        0
    } else {
        1
    };
    run.check()?;
    check_plugins(run)?;
    branch(run, children, branch_index, depth, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that tap pairs are exact and expired or cancelled owners cannot remove replacements
    fn tap_pairs_are_exact_and_expired_or_cancelled_owners_cannot_remove_replacements() {
        let windows = TapWindows::default();
        let now = Instant::now();
        let duration = Duration::from_millis(100);
        let first = windows.register("d|key".into(), now, duration).unwrap();
        assert!(windows
            .register("d|key".into(), now + Duration::from_millis(99), duration)
            .is_none());
        let third = windows
            .register("d|key".into(), now + Duration::from_millis(99), duration)
            .unwrap();
        assert!(first.window.paired.load(Ordering::SeqCst));
        drop(first);
        assert_eq!(windows.windows.lock().unwrap().len(), 1);
        let expired = windows
            .register("d|key".into(), third.window.deadline, duration)
            .unwrap();
        drop(third);
        assert_eq!(windows.windows.lock().unwrap().len(), 1);
        windows.cleanup(Some("d"));
        assert!(expired.window.paired.load(Ordering::SeqCst));
        let replacement = windows.register("d|key".into(), now, duration).unwrap();
        drop(expired);
        assert_eq!(windows.windows.lock().unwrap().len(), 1);
        drop(replacement);
        assert!(windows.windows.lock().unwrap().is_empty());
    }
}
