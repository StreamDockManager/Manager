//! One-shot input capture, before navigation and actions. Terminal ownership
//! survives disarming so a captured press can never release somebody else's action
use crate::Runtime;
use hardware_core::{ControlEvent, Input};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

type Key = (String, u64, String);
struct Armed {
    token: String,
    device: String,
    epoch: u64,
    deadline: Instant,
}
#[derive(Default)]
pub(crate) struct Locate {
    armed: Option<Armed>,
    held: HashSet<Key>,
    consumed: HashSet<Key>,
    touch: HashSet<(String, u64)>,
    captured_touch: HashSet<(String, u64)>,
}
impl Locate {
    // Handle the incoming locate input
    fn input(
        &mut self,
        device: &str,
        epoch: u64,
        event: &ControlEvent,
        editable: bool,
        now: Instant,
    ) -> (bool, Option<String>) {
        self.held
            .retain(|(id, generation, _)| id != device || *generation == epoch);
        self.consumed
            .retain(|(id, generation, _)| id != device || *generation == epoch);
        self.touch
            .retain(|(id, generation)| id != device || *generation == epoch);
        self.captured_touch
            .retain(|(id, generation)| id != device || *generation == epoch);
        let key = (device.into(), epoch, event.control_id.clone());
        let dock = (device.into(), epoch);
        let was_held = self.held.contains(&key);
        let was_touch = self.touch.contains(&dock);
        let armed = self
            .armed
            .as_ref()
            .is_some_and(|arm| arm.device == device && arm.epoch == epoch && arm.deadline > now);
        match &event.input {
            Input::Button { pressed: true } => {
                self.held.insert(key.clone());
            }
            Input::Button { pressed: false } => {
                self.held.remove(&key);
                return (self.consumed.remove(&key), None);
            }
            Input::Touch { phase, .. } if phase != "end" => {
                self.touch.insert(dock.clone());
                if armed && !was_touch {
                    self.captured_touch.insert(dock.clone());
                }
                return (self.captured_touch.contains(&dock), None);
            }
            Input::Touch { .. } | Input::Swipe { .. } => {
                self.touch.remove(&dock);
            }
            _ => {}
        }
        let captured_touch = matches!(event.input, Input::Touch { .. } | Input::Swipe { .. })
            && self.captured_touch.remove(&dock);
        let eligible = editable
            && !was_held
            && match &event.input {
                Input::Button { pressed: true } | Input::Pulse => true,
                Input::Rotate { delta } => *delta != 0,
                Input::Touch { phase, .. } => phase == "end" && (!was_touch || captured_touch),
                _ => false,
            };
        if armed && eligible {
            if matches!(event.input, Input::Button { pressed: true }) {
                self.consumed.insert(key);
            }
            let token = self.armed.take().unwrap().token;
            return (true, Some(token));
        }
        (
            captured_touch
                || self.consumed.contains(&key)
                || armed && !was_touch && matches!(event.input, Input::Swipe { .. }),
            None,
        )
    }
}
impl Runtime {
    // Arm a connection-scoped locate lease and return its correlation token
    pub fn arm_locate(self: &Arc<Self>, device_id: &str) -> Result<String, String> {
        let epochs = self.connection_epochs.lock().unwrap();
        let epoch = *epochs.get(device_id).ok_or("Device is disconnected")?;
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|device| device.id == device_id && device.connected)
            .ok_or("Device is disconnected")?;
        if device.simulated {
            return Err("Locate requires a physical dock".into());
        }
        if self
            .power
            .effective(device_id, epoch, &state.session(device_id).output)
            .sleeping
        {
            return Err("Wake the dock before locating a control".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.locate.lock().unwrap().armed = Some(Armed {
            token: token.clone(),
            device: device_id.into(),
            epoch,
            deadline: Instant::now() + Duration::from_secs(10),
        });
        drop(state);
        drop(epochs);
        let runtime = Arc::downgrade(self);
        let expires = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            if let Some(runtime) = runtime.upgrade() {
                runtime.cancel_locate(Some(&expires));
            }
        });
        Ok(token)
    }
    // Cancel locate
    pub fn cancel_locate(&self, token: Option<&str>) {
        let cancelled = {
            let mut state = self.locate.lock().unwrap();
            if state
                .armed
                .as_ref()
                .is_some_and(|arm| token.is_none_or(|token| token == arm.token))
            {
                state.armed.take()
            } else {
                None
            }
        };
        if let Some(arm) = cancelled {
            self.emit("locate-cancelled", Some(arm.device), None, Some(arm.token));
        }
    }
    // Report the locate input condition for locate
    pub(crate) fn locate_input(&self, device: &str, epoch: u64, event: &ControlEvent) -> bool {
        let editable = self
            .state
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|d| d.id == device)
            .is_some_and(|d| {
                d.manifest.control(&event.control_id).is_ok()
                    && crate::touch_panel::active_control(&d.manifest, &event.control_id)
            });
        let (consumed, token) =
            self.locate
                .lock()
                .unwrap()
                .input(device, epoch, event, editable, Instant::now());
        if let Some(token) = token {
            self.emit(
                "locate-control",
                Some(device.into()),
                Some(event.clone()),
                Some(token),
            );
        }
        consumed
    }
    // Invalidate locate
    pub(crate) fn invalidate_locate(&self, device: Option<&str>) {
        let token = self
            .locate
            .lock()
            .unwrap()
            .armed
            .as_ref()
            .filter(|arm| device.is_none_or(|device| device == arm.device))
            .map(|arm| arm.token.clone());
        if let Some(token) = token {
            self.cancel_locate(Some(&token));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Handle the incoming locate event
    fn event(input: Input) -> ControlEvent {
        ControlEvent {
            control_id: "key".into(),
            input,
        }
    }
    // Arm the locate operation for its next event
    fn arm(state: &mut Locate, now: Instant) {
        state.armed = Some(Armed {
            token: "t".into(),
            device: "a".into(),
            epoch: 1,
            deadline: now + Duration::from_secs(10),
        });
    }
    #[test]
    // Verify that held releases other docks and captured terminals keep their owner
    fn held_releases_other_docks_and_captured_terminals_keep_their_owner() {
        let mut state = Locate::default();
        let now = Instant::now();
        state.input("a", 1, &event(Input::Button { pressed: true }), true, now);
        arm(&mut state, now);
        assert_eq!(
            state.input("a", 1, &event(Input::Button { pressed: false }), true, now),
            (false, None)
        );
        assert_eq!(
            state.input("b", 1, &event(Input::Pulse), true, now),
            (false, None)
        );
        assert_eq!(
            state.input("a", 1, &event(Input::Button { pressed: true }), true, now),
            (true, Some("t".into()))
        );
        assert_eq!(
            state.input("a", 1, &event(Input::Button { pressed: false }), true, now),
            (true, None)
        );
        assert_eq!(
            state.input("a", 1, &event(Input::Pulse), true, now),
            (false, None)
        );
        arm(&mut state, now);
        assert_eq!(
            state.input("a", 2, &event(Input::Pulse), true, now),
            (false, None)
        );
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Pulse),
                true,
                now + Duration::from_secs(10)
            ),
            (false, None)
        );
    }
    #[test]
    // Verify that touch completes on editable region and never dispatches navigation
    fn touch_completes_on_editable_region_and_never_dispatches_navigation() {
        let mut state = Locate::default();
        let now = Instant::now();
        arm(&mut state, now);
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Swipe {
                    direction: "left".into()
                }),
                false,
                now
            ),
            (true, None)
        );
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Touch {
                    phase: "start".into(),
                    x: 0,
                    y: 0
                }),
                false,
                now
            ),
            (true, None)
        );
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Swipe {
                    direction: "left".into()
                }),
                false,
                now
            ),
            (true, None)
        );
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Touch {
                    phase: "end".into(),
                    x: 0,
                    y: 0
                }),
                true,
                now
            ),
            (true, Some("t".into()))
        );
        state.input(
            "a",
            1,
            &event(Input::Touch {
                phase: "start".into(),
                x: 0,
                y: 0,
            }),
            false,
            now,
        );
        arm(&mut state, now);
        assert_eq!(
            state.input(
                "a",
                1,
                &event(Input::Touch {
                    phase: "end".into(),
                    x: 0,
                    y: 0
                }),
                true,
                now
            ),
            (false, None)
        );
    }
}
