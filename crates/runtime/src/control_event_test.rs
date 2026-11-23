//! Bounded, editor-only observation using a private gesture recognizer
use crate::{
    actions::{EventSlot, MAX_HOLD_MS, MAX_MULTI_PRESS_MS, MIN_HOLD_MS, MIN_MULTI_PRESS_MS},
    control_events::{EventMask, Gesture, RoutedInput, Timing},
    Runtime, Snapshot,
};
use hardware_core::{ControlEvent, Input};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

const TEST_DURATION: Duration = Duration::from_secs(8);
const RELEASE_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ControlEventTestStatus {
    Waiting,
    Matched,
    NoMatch,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, serde::Serialize, ts_rs::TS)]
pub struct ControlEventTest {
    pub token: String,
    pub status: ControlEventTestStatus,
    pub recognized: Vec<usize>,
}

struct Capture {
    view: ControlEventTest,
    device: String,
    control: String,
    related_touch: Vec<String>,
    epoch: u64,
    revision: u64,
    profile: String,
    page: String,
    mask: EventMask,
    supported: EventMask,
    requested: EventSlot,
    timing: Timing,
    end_only: bool,
    observed: bool,
    held: bool,
    gesture: Gesture,
    deadline: Instant,
    release_until: Instant,
}

impl Capture {
    // Invalidate tests when the saved binding context or hardware connection changes
    fn current(&self, snapshot: &Snapshot, epoch: Option<u64>) -> bool {
        let session = snapshot.session(&self.device);
        epoch == Some(self.epoch)
            && snapshot.configuration_revision == self.revision
            && session.profile_id == self.profile
            && session.page_id == self.page
            && snapshot
                .devices
                .iter()
                .any(|device| device.id == self.device && device.connected && !device.simulated)
    }

    fn finish(&mut self, status: ControlEventTestStatus, now: Instant) {
        if self.view.status == ControlEventTestStatus::Waiting {
            self.view.status = status;
            self.release_until = self.release_until.max(now + RELEASE_GRACE);
        }
    }

    fn record(&mut self, routed: Vec<RoutedInput>, now: Instant) {
        for event in routed {
            if self.view.recognized.len() < EventSlot::COUNT {
                self.view.recognized.push(event.slot.index());
            }
            if event.slot == self.requested {
                self.finish(ControlEventTestStatus::Matched, now);
            }
        }
        if self.observed && self.gesture.is_idle() {
            self.finish(ControlEventTestStatus::NoMatch, now);
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.view.status != ControlEventTestStatus::Waiting {
            return;
        }
        let routed = self.gesture.advance(now.min(self.deadline), self.mask);
        self.record(routed, now);
        if now >= self.deadline {
            self.finish(ControlEventTestStatus::TimedOut, now);
        }
    }

    // Only selected reports reach this private recognizer; no dispatch or host output occurs
    fn observe(&mut self, event: &ControlEvent, now: Instant) -> bool {
        self.tick(now);
        if self.view.status != ControlEventTestStatus::Waiting {
            if !self.held || now >= self.release_until {
                return false;
            }
            if matches!(&event.input, Input::Button { pressed: false })
                || matches!(&event.input, Input::Touch { phase, .. } if phase == "end" || phase == "up" || phase == "cancel")
                || matches!(&event.input, Input::Swipe { .. })
            {
                self.held = false;
            }
            return true;
        }
        // Native touch strips report contact start separately from the final zone
        if let Input::Touch { phase, .. } = &event.input {
            if matches!(phase.as_str(), "start" | "begin" | "down") {
                self.held = true;
            } else if matches!(phase.as_str(), "end" | "up" | "cancel") {
                self.held = false;
            }
        }
        if event.control_id != self.control {
            if matches!(&event.input, Input::Swipe { .. }) {
                self.gesture.cancel();
                self.held = false;
                self.finish(ControlEventTestStatus::NoMatch, now);
            }
            return true;
        }
        let normalized =
            crate::event_bindings::normalize_input(&event.input, self.supported, self.end_only);
        let input = normalized.as_ref().unwrap_or(&event.input);
        if let Input::Button { pressed } = input {
            self.held = *pressed;
        }
        let swipe_slots = EventSlot::SwipeLeft.bit()
            | EventSlot::SwipeRight.bit()
            | EventSlot::SwipeUp.bit()
            | EventSlot::SwipeDown.bit();
        if matches!(input, Input::Swipe { .. }) {
            self.held = false;
        }
        if matches!(input, Input::Rotate { delta: 0 })
            || matches!(input, Input::Touch { phase, .. } if phase == "move" || self.supported & !swipe_slots == 0)
        {
            return true;
        }
        self.observed = true;
        let routed = self.gesture.input(input, now, self.mask, self.timing);
        self.record(routed, now);
        true
    }

    fn covers(&self, device: &str, epoch: u64, control: &str) -> bool {
        self.device == device
            && self.epoch == epoch
            && (self.control == control || self.related_touch.iter().any(|id| id == control))
    }
}

// Remember captured paired dial contacts independently of the bounded test result
#[derive(Default)]
struct DialTails {
    held: HashMap<String, HashMap<String, u64>>,
}

impl DialTails {
    fn observe(
        &mut self,
        device: &str,
        epoch: u64,
        event: &ControlEvent,
        captured_dial_down: bool,
    ) -> bool {
        let control = &event.control_id;
        if captured_dial_down {
            self.held
                .entry(device.into())
                .or_default()
                .insert(control.clone(), epoch);
            return true;
        }
        let Some(controls) = self.held.get_mut(device) else {
            return false;
        };
        let Some(&held_epoch) = controls.get(control) else {
            return false;
        };
        if held_epoch == epoch && matches!(event.input, Input::Rotate { .. }) {
            return true;
        }
        if held_epoch != epoch || matches!(event.input, Input::Button { .. }) {
            controls.remove(control);
            if controls.is_empty() {
                self.held.remove(device);
            }
        }
        // A new down recovers controls whose terminal could not be observed
        held_epoch == epoch && matches!(event.input, Input::Button { pressed: false })
    }
}

#[derive(Default)]
pub(crate) struct Listener {
    active: AtomicBool,
    capture: Mutex<Option<Capture>>,
    dial_tails: Mutex<DialTails>,
    draining_dial: AtomicBool,
}

impl Listener {
    pub(crate) fn observe(
        &self,
        device: &str,
        epoch: u64,
        snapshot: &Snapshot,
        event: &ControlEvent,
    ) -> bool {
        let mut captured_dial_down = false;
        let mut consumed = false;
        let mut guard = self
            .active
            .load(Ordering::Relaxed)
            .then(|| self.capture.lock().unwrap());
        if let Some(guard) = guard.as_mut() {
            if let Some(capture) = guard
                .as_mut()
                .filter(|c| c.covers(device, epoch, &event.control_id))
            {
                // Timestamp while owning the recognizer, after any concurrent status poll
                let now = Instant::now();
                if !capture.current(snapshot, Some(epoch)) {
                    capture.finish(ControlEventTestStatus::Cancelled, now);
                }
                consumed = capture.observe(event, now);
                captured_dial_down = consumed
                    && capture.control == event.control_id
                    && capture.supported & EventSlot::PressUp.bit() != 0
                    && capture.supported & (EventSlot::TurnLeft.bit() | EventSlot::TurnRight.bit())
                        != 0
                    && matches!(event.input, Input::Button { pressed: true });
                self.active.store(
                    capture.view.status == ControlEventTestStatus::Waiting
                        || (capture.held && now < capture.release_until),
                    Ordering::Relaxed,
                );
            }
        }
        if captured_dial_down
            || (self.draining_dial.load(Ordering::Relaxed)
                && matches!(event.input, Input::Button { .. } | Input::Rotate { .. }))
        {
            let mut tails = self.dial_tails.lock().unwrap();
            let drained = tails.observe(device, epoch, event, captured_dial_down);
            self.draining_dial
                .store(!tails.held.is_empty(), Ordering::Relaxed);
            consumed |= drained;
        }
        consumed
    }

    pub(crate) fn retire_device(&self, device: &str) {
        let mut guard = self.capture.lock().unwrap();
        if let Some(capture) = guard.as_mut().filter(|capture| capture.device == device) {
            capture.finish(ControlEventTestStatus::Cancelled, Instant::now());
            capture.held = false;
            self.active.store(false, Ordering::Relaxed);
        }
        let mut tails = self.dial_tails.lock().unwrap();
        tails.held.remove(device);
        self.draining_dial
            .store(!tails.held.is_empty(), Ordering::Relaxed);
    }

    pub(crate) fn cancel_for(&self, device: Option<&str>) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let mut guard = self.capture.lock().unwrap();
        if let Some(capture) = guard
            .as_mut()
            .filter(|capture| device.is_none_or(|id| capture.device == id))
        {
            let now = Instant::now();
            capture.finish(ControlEventTestStatus::Cancelled, now);
            self.active.store(
                capture.held && now < capture.release_until,
                Ordering::Relaxed,
            );
        }
    }

    // Reject a queued pre-test gesture headed to the same control
    pub(crate) fn blocks_pending(&self, device: &str, epoch: u64, control: &str) -> bool {
        self.active.load(Ordering::Relaxed)
            && self.capture.lock().unwrap().as_ref().is_some_and(|c| {
                c.covers(device, epoch, control)
                    && Instant::now() < c.deadline
                    && c.view.status == ControlEventTestStatus::Waiting
            })
    }
}

impl Runtime {
    pub fn arm_control_event_test(
        &self,
        device_id: &str,
        control_id: &str,
        event_index: usize,
        assigned: &[usize],
        multi_press_ms: u16,
        hold_ms: u16,
    ) -> Result<ControlEventTest, String> {
        let epoch = self
            .connection_generation(device_id)
            .ok_or("Connect the selected physical device")?;
        let state = self.snapshot();
        let device = state
            .devices
            .iter()
            .find(|device| device.id == device_id && device.connected && !device.simulated)
            .ok_or("Connect the selected physical device")?;
        let session = state.session(device_id);
        if self.is_stopping()
            || self
                .power
                .effective(device_id, epoch, &session.output)
                .sleeping
            || self
                .builtin_state
                .paused
                .lock()
                .unwrap()
                .contains_key(device_id)
        {
            return Err("Wake the device and resume bindings before testing".into());
        }
        let supported = crate::control_events::supported(&device.manifest, control_id);
        let requested = EventSlot::from_index(event_index)
            .filter(|slot| supported & slot.bit() != 0)
            .ok_or("This control does not support that event")?;
        if assigned.len() > EventSlot::COUNT
            || !(MIN_MULTI_PRESS_MS..=MAX_MULTI_PRESS_MS).contains(&multi_press_ms)
            || !(MIN_HOLD_MS..=MAX_HOLD_MS).contains(&hold_ms)
        {
            return Err("Invalid event test timing or assignments".into());
        }
        let mask = assigned.iter().try_fold(requested.bit(), |mask, index| {
            EventSlot::from_index(*index)
                .map(|slot| mask | (supported & slot.bit()))
                .ok_or("Invalid assigned event")
        })?;
        let control = device
            .manifest
            .control(control_id)
            .map_err(|e| e.to_string())?;
        let end_only = control.kind == "touch"
            && !control
                .semantics
                .iter()
                .any(|s| s == "touch_down" || s == "swipe");
        let related_touch: Vec<String> = if control.kind == "touch" {
            device
                .manifest
                .capabilities
                .touch_panel
                .as_ref()
                .filter(|p| {
                    p.gesture_control == control_id
                        || p.button_controls.iter().any(|id| id == control_id)
                })
                .map(|panel| {
                    std::iter::once(&panel.gesture_control)
                        .chain(&panel.button_controls)
                        .filter(|id| id.as_str() != control_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut guard = self.control_event_test.capture.lock().unwrap();
        let now = Instant::now();
        if let Some(old) = guard.as_mut() {
            old.tick(now);
            if old.held && now < old.release_until {
                return Err("Release the previously tested control first".into());
            }
        }
        if self
            .builtin_state
            .captures
            .lock()
            .unwrap()
            .contains_key(&format!("{device_id}|{control_id}"))
            || self.event_bindings.is_tracking(device_id, control_id)
        {
            return Err("Finish the previous gesture before starting the test".into());
        }
        let view = ControlEventTest {
            token: uuid::Uuid::new_v4().to_string(),
            status: ControlEventTestStatus::Waiting,
            recognized: Vec::new(),
        };
        *guard = Some(Capture {
            view: view.clone(),
            device: device_id.into(),
            control: control_id.into(),
            related_touch,
            epoch,
            revision: state.configuration_revision,
            profile: session.profile_id,
            page: session.page_id,
            mask,
            supported,
            requested,
            timing: Timing {
                multi_press_ms,
                hold_ms,
            },
            end_only,
            observed: false,
            held: false,
            gesture: Gesture::default(),
            deadline: now + TEST_DURATION,
            release_until: now + TEST_DURATION + RELEASE_GRACE,
        });
        self.control_event_test
            .active
            .store(true, Ordering::Relaxed);
        Ok(view)
    }

    pub fn read_control_event_test(&self, token: &str) -> Result<ControlEventTest, String> {
        let device = {
            let guard = self.control_event_test.capture.lock().unwrap();
            let capture = guard
                .as_ref()
                .filter(|c| c.view.token == token)
                .ok_or("Event test is no longer active")?;
            capture.device.clone()
        };
        let epoch = self.connection_generation(&device);
        let snapshot = self.state.lock().unwrap();
        let now = Instant::now();
        let device_usable = epoch.is_some_and(|epoch| {
            !self
                .power
                .effective(&device, epoch, &snapshot.session(&device).output)
                .sleeping
        }) && !self
            .builtin_state
            .paused
            .lock()
            .unwrap()
            .contains_key(&device)
            && !self.is_stopping();
        let mut guard = self.control_event_test.capture.lock().unwrap();
        let capture = guard
            .as_mut()
            .filter(|c| c.view.token == token)
            .ok_or("Event test is no longer active")?;
        let usable = device_usable && capture.current(&snapshot, epoch);
        if usable {
            capture.tick(now);
        } else {
            capture.finish(ControlEventTestStatus::Cancelled, now);
        }
        self.control_event_test.active.store(
            capture.view.status == ControlEventTestStatus::Waiting
                || (capture.held && now < capture.release_until),
            Ordering::Relaxed,
        );
        Ok(capture.view.clone())
    }

    pub fn cancel_control_event_test(&self, token: &str) {
        let mut guard = self.control_event_test.capture.lock().unwrap();
        if let Some(capture) = guard.as_mut().filter(|c| c.view.token == token) {
            let now = Instant::now();
            capture.finish(ControlEventTestStatus::Cancelled, now);
            self.control_event_test.active.store(
                capture.held && now < capture.release_until,
                Ordering::Relaxed,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(requested: EventSlot, supported: EventMask, now: Instant) -> Capture {
        Capture {
            view: ControlEventTest {
                token: "token".into(),
                status: ControlEventTestStatus::Waiting,
                recognized: Vec::new(),
            },
            device: "physical".into(),
            control: "control".into(),
            related_touch: vec![],
            epoch: 5,
            revision: 1,
            profile: "profile".into(),
            page: "page".into(),
            mask: requested.bit(),
            supported,
            requested,
            timing: Timing::default(),
            end_only: false,
            observed: false,
            held: false,
            gesture: Gesture::default(),
            deadline: now + TEST_DURATION,
            release_until: now + TEST_DURATION + RELEASE_GRACE,
        }
    }

    #[test]
    fn matches_actual_report_sequences_and_distinguishes_no_match_from_timeout() {
        let now = Instant::now();
        let pulse = ControlEvent {
            control_id: "control".into(),
            input: Input::Pulse,
        };
        let mut test = capture(EventSlot::DoublePress, EventSlot::DoublePress.bit(), now);
        assert!(test.observe(&pulse, now));
        assert_eq!(test.view.status, ControlEventTestStatus::Waiting);
        assert!(test.observe(&pulse, now + Duration::from_millis(120)));
        assert_eq!(test.view.status, ControlEventTestStatus::Matched);
        assert_eq!(test.view.recognized, [EventSlot::DoublePress.index()]);

        let mut test = capture(EventSlot::DoublePress, EventSlot::DoublePress.bit(), now);
        assert!(test.observe(&pulse, now));
        test.tick(now + Duration::from_secs(1));
        assert_eq!(test.view.status, ControlEventTestStatus::NoMatch);

        let mut test = capture(EventSlot::DoublePress, EventSlot::DoublePress.bit(), now);
        test.tick(now + TEST_DURATION);
        assert_eq!(test.view.status, ControlEventTestStatus::TimedOut);
        assert!(!test.observe(&pulse, now + TEST_DURATION));
        assert!(!test.covers("physical", 6, "control"));
        assert!(!test.covers("physical", 5, "another"));
    }

    #[test]
    fn keep_completed_or_cancelled_press_tail_out_of_dispatch() {
        let now = Instant::now();
        let down = ControlEvent {
            control_id: "control".into(),
            input: Input::Button { pressed: true },
        };
        let up = ControlEvent {
            control_id: "control".into(),
            input: Input::Button { pressed: false },
        };
        let edges = EventSlot::PressDown.bit() | EventSlot::PressUp.bit();
        let mut test = capture(EventSlot::PressDown, edges, now);
        assert!(test.observe(&down, now));
        assert_eq!(test.view.status, ControlEventTestStatus::Matched);
        assert!(test.observe(&up, now + Duration::from_secs(4)));
        assert!(!test.observe(&down, now + Duration::from_secs(5)));

        let mut test = capture(EventSlot::PressUp, edges, now);
        assert!(test.observe(&down, now));
        test.finish(
            ControlEventTestStatus::Cancelled,
            now + Duration::from_secs(1),
        );
        assert!(test.observe(&up, now + Duration::from_secs(2)));
        assert_eq!(test.view.status, ControlEventTestStatus::Cancelled);
        assert!(test.view.recognized.is_empty());
    }

    #[test]
    fn paired_button_tail_blocks_late_held_turns_without_affecting_other_controls() {
        let now = Instant::now();
        let event = |control: &str, input| ControlEvent {
            control_id: control.into(),
            input,
        };
        let down = event("control", Input::Button { pressed: true });
        let up = event("control", Input::Button { pressed: false });
        let turn = event("control", Input::Rotate { delta: 1 });
        let mut test = capture(
            EventSlot::PressDown,
            EventSlot::PressDown.bit() | EventSlot::PressUp.bit(),
            now,
        );
        let mut tails = DialTails::default();
        assert!(test.observe(&down, now));
        assert!(tails.observe("physical", 5, &down, true));
        let late = now + TEST_DURATION + RELEASE_GRACE + Duration::from_secs(1);
        assert!(!test.observe(&turn, late));
        assert!(tails.observe("physical", 5, &turn, false));
        assert!(!tails.observe(
            "physical",
            5,
            &event("other", Input::Rotate { delta: 1 }),
            false
        ));
        assert!(!tails.observe("unrelated", 5, &turn, false));
        assert!(tails.observe("physical", 5, &up, false));
        assert!(tails.held.is_empty());
        assert!(!tails.observe("physical", 5, &turn, false));

        assert!(tails.observe("physical", 5, &down, true));
        assert!(!tails.observe("physical", 6, &turn, false));
        assert!(tails.held.is_empty());
        assert!(tails.observe("physical", 6, &down, true));
        assert!(!tails.observe("physical", 6, &down, false));
        assert!(tails.held.is_empty());
    }

    #[test]
    fn cancelled_zone_test_consumes_the_terminal_of_a_related_strip_start() {
        let now = Instant::now();
        let mut test = capture(EventSlot::SinglePress, EventSlot::SinglePress.bit(), now);
        test.related_touch.push("strip".into());
        test.end_only = true;
        let start = ControlEvent {
            control_id: "strip".into(),
            input: Input::Touch {
                phase: "start".into(),
                x: 14,
                y: 0,
            },
        };
        let end = ControlEvent {
            control_id: "control".into(),
            input: Input::Touch {
                phase: "end".into(),
                x: 14,
                y: 0,
            },
        };
        assert!(test.observe(&start, now));
        test.finish(
            ControlEventTestStatus::Cancelled,
            now + Duration::from_secs(1),
        );
        assert!(test.observe(&end, now + Duration::from_secs(2)));
        assert!(!test.observe(&end, now + Duration::from_secs(3)));
        assert!(test.view.recognized.is_empty());
    }

    #[test]
    fn swipe_preview_waits_through_touch_start_and_coordinate_reports() {
        let now = Instant::now();
        let mut test = capture(EventSlot::SwipeLeft, EventSlot::SwipeLeft.bit(), now);
        let event = |input| ControlEvent {
            control_id: "control".into(),
            input,
        };
        assert!(test.observe(
            &event(Input::Touch {
                phase: "start".into(),
                x: 14,
                y: 0
            }),
            now
        ));
        assert!(test.observe(
            &event(Input::Touch {
                phase: "move".into(),
                x: 10,
                y: 2
            }),
            now
        ));
        assert_eq!(test.view.status, ControlEventTestStatus::Waiting);
        assert!(test.observe(
            &event(Input::Swipe {
                direction: "left".into()
            }),
            now
        ));
        assert_eq!(test.view.status, ControlEventTestStatus::Matched);
    }
}
