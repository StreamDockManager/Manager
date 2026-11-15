//! Input ownership spans navigation requests, preparation and completed transfer
use hardware_core::{ControlEvent, Input};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

#[derive(Default)]
pub(crate) struct Transitions {
    devices: HashMap<String, Device>,
}
struct Device {
    epoch: u64,
    generation: u64,
    blocked: bool,
    changed: Instant,
    suppressed: HashSet<String>,
}
impl Transitions {
    // Begin tracking the transition input operation
    pub fn begin(&mut self, id: &str, epoch: u64) -> u64 {
        let device = self.devices.entry(id.into()).or_insert_with(|| Device {
            epoch,
            generation: 0,
            blocked: false,
            changed: Instant::now(),
            suppressed: HashSet::new(),
        });
        if device.epoch != epoch {
            device.suppressed.clear();
        }
        device.epoch = epoch;
        device.generation = device.generation.wrapping_add(1).max(1);
        device.blocked = true;
        device.changed = Instant::now();
        device.generation
    }
    // Read the blocked transition generation only when the device epoch still matches
    pub fn pending(&self, id: &str, epoch: u64) -> Option<u64> {
        self.devices
            .get(id)
            .filter(|d| d.epoch == epoch && d.blocked)
            .map(|d| d.generation)
    }
    // Return the current transition input value
    pub fn current(&self, id: &str, epoch: u64, generation: u64) -> bool {
        self.devices
            .get(id)
            .is_some_and(|d| d.epoch == epoch && d.generation == generation)
    }
    // Complete the pending transition input operation
    pub fn complete(&mut self, id: &str, epoch: u64, generation: u64) {
        if let Some(d) = self
            .devices
            .get_mut(id)
            .filter(|d| d.epoch == epoch && d.generation == generation)
        {
            d.blocked = false;
        }
    }
    // Suppress the selected transition input event
    pub fn suppress(&mut self, id: &str, control: &str) {
        if let Some(d) = self.devices.get_mut(id) {
            d.suppressed.insert(control.into());
        }
    }
    // Report the discard condition for transition input
    pub fn discard(&mut self, id: &str, epoch: u64, event: &ControlEvent) -> bool {
        let Some(d) = self.devices.get_mut(id).filter(|d| d.epoch == epoch) else {
            return false;
        };
        let suppressed = d.suppressed.contains(&event.control_id);
        match &event.input {
            Input::Button { pressed: false } => {
                d.suppressed.remove(&event.control_id);
            }
            Input::Touch { phase, .. } if phase == "end" => {
                d.suppressed.remove(&event.control_id);
            }
            Input::Button { pressed: true } | Input::Touch { .. } if d.blocked => {
                d.suppressed.insert(event.control_id.clone());
            }
            _ => {}
        }
        d.blocked || suppressed
    }
    // Check permission for request
    pub fn permits_request(&self, id: &str, epoch: Option<u64>, queued: Instant) -> bool {
        self.devices
            .get(id)
            .is_none_or(|d| epoch != Some(d.epoch) || (!d.blocked && queued >= d.changed))
    }
    // Forget the selected transition input entry
    pub fn forget(&mut self, id: &str) {
        self.devices.remove(id);
    }
}

impl crate::Runtime {
    // Release transition inputs
    pub(crate) fn release_transition_inputs(&self, device: &str) {
        let prefix = format!("{device}|");
        let captures: Vec<_> = {
            let mut held = self.builtin_state.captures.lock().unwrap();
            let keys: Vec<_> = held
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| {
                    held.remove(&key)
                        .map(|value| (key[prefix.len()..].to_owned(), value))
                })
                .collect()
        };
        for (control, capture) in captures {
            self.transition_inputs
                .lock()
                .unwrap()
                .suppress(device, &control);
            capture
                .pressed
                .store(false, std::sync::atomic::Ordering::SeqCst);
            if let crate::actions::Action::Plugin {
                plugin_id,
                action_id,
                settings,
                user_desired_state,
                ..
            } = capture.action
            {
                let _ = self.plugins.lock().unwrap().dispatch_with_state(
                    &plugin_id,
                    &action_id,
                    device,
                    &control,
                    &capture.scope,
                    &Input::Button { pressed: false },
                    &settings,
                    user_desired_state,
                );
            }
        }
        self.builtin_state.cancel_device(device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that transitions drop held and new inputs until completion and require fresh edges
    fn transitions_drop_held_and_new_inputs_until_completion_and_require_fresh_edges() {
        let mut gate = Transitions::default();
        let old_request = Instant::now();
        let first = gate.begin("a", 1);
        gate.suppress("a", "held");
        let press = |id: &str, pressed| ControlEvent {
            control_id: id.into(),
            input: Input::Button { pressed },
        };
        assert!(gate.discard("a", 1, &press("new", true)));
        assert!(gate.discard(
            "a",
            1,
            &ControlEvent {
                control_id: "dial".into(),
                input: Input::Rotate { delta: 1 }
            }
        ));
        assert!(!gate.discard("b", 1, &press("new", true)));
        let second = gate.begin("a", 1);
        gate.complete("a", 1, first);
        assert!(gate.pending("a", 1).is_some());
        gate.complete("a", 1, second);
        for id in ["held", "new"] {
            assert!(gate.discard("a", 1, &press(id, true)));
            assert!(gate.discard("a", 1, &press(id, false)));
            assert!(!gate.discard("a", 1, &press(id, true)));
        }
        assert!(!gate.permits_request("a", Some(1), old_request));
        assert!(gate.permits_request("a", Some(1), Instant::now()));
        let reconnect = gate.begin("a", 2);
        gate.complete("a", 1, second);
        assert!(gate.pending("a", 2).is_some());
        gate.complete("a", 2, reconnect);
        assert!(!gate.discard("a", 2, &press("held", true)));
    }
}
