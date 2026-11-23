//! Pure gesture recognition for one scoped physical control and a monotonic clock
pub use crate::actions::EventSlot;
use crate::actions::{
    DEFAULT_HOLD_MS, DEFAULT_MULTI_PRESS_MS, MAX_HOLD_MS, MAX_MULTI_PRESS_MS, MIN_HOLD_MS,
    MIN_MULTI_PRESS_MS,
};
use hardware_core::{Input, ModelManifest};
use std::time::{Duration, Instant};

pub type EventMask = u16;
use EventSlot::*;

const CLICKS: EventMask = SinglePress.bit() | DoublePress.bit() | TriplePress.bit();

// Query only capabilities advertised by the selected model, variant and firmware contract
pub fn supported(manifest: &ModelManifest, control_id: &str) -> EventMask {
    let Ok(control) = manifest.control(control_id) else {
        return 0;
    };
    let has = |value| control.semantics.iter().any(|entry| entry == value);
    match control.kind.as_str() {
        "button" | "encoder" => {
            let encoder = control.kind == "encoder";
            let reliable = if encoder {
                manifest.capabilities.reliable_encoder_release
            } else {
                manifest.capabilities.reliable_button_release
            };
            let edges = reliable && has("press") && has("release");
            let mut mask = if edges {
                PressDown.bit() | PressUp.bit() | LongHold.bit() | CLICKS
            } else if has("pulse") {
                CLICKS
            } else if has("press") {
                SinglePress.bit()
            } else {
                0
            };
            if encoder && has("rotate") {
                mask |= TurnLeft.bit() | TurnRight.bit();
                // A rotate report while released is not evidence for pressed rotation
                if edges && has("rotate_held") {
                    mask |= HeldTurnLeft.bit() | HeldTurnRight.bit();
                }
            }
            mask
        }
        "touch" => {
            let panel = manifest.capabilities.touch_panel.as_ref();
            let strip = panel.is_some_and(|p| p.gesture_control == control_id);
            let mut mask = if !strip && has("touch") { CLICKS } else { 0 };
            if has("touch_down") {
                mask |= PressDown.bit();
            }
            if has("touch_up") {
                mask |= PressUp.bit();
            }
            if has("touch_down") && has("touch_up") && has("touch_hold") {
                mask |= LongHold.bit();
            }
            if has("swipe") {
                if let Some(panel) = panel.filter(|_| strip) {
                    for direction in &panel.swipe_directions {
                        if let Some(slot) = swipe(direction) {
                            mask |= slot.bit();
                        }
                    }
                } else if panel.is_none() {
                    // Legacy strip contract advertises left and right, not vertical swipes
                    mask |= SwipeLeft.bit() | SwipeRight.bit();
                }
            }
            mask
        }
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub multi_press_ms: u16,
    pub hold_ms: u16,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            multi_press_ms: DEFAULT_MULTI_PRESS_MS,
            hold_ms: DEFAULT_HOLD_MS,
        }
    }
}

impl Timing {
    // Bound all deadlines even if the consumer has not validated the stored values
    fn gap(self) -> Duration {
        Duration::from_millis(u64::from(
            self.multi_press_ms
                .clamp(MIN_MULTI_PRESS_MS, MAX_MULTI_PRESS_MS),
        ))
    }

    fn hold(self) -> Duration {
        Duration::from_millis(u64::from(self.hold_ms.clamp(MIN_HOLD_MS, MAX_HOLD_MS)))
    }
}

#[derive(Debug, PartialEq)]
pub struct RoutedInput {
    pub slot: EventSlot,
    pub input: Input,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContactKind {
    Button,
    Touch,
}

struct Contact {
    kind: ContactKind,
    hold: Option<Instant>,
    consumed: bool,
    initiated: Input,
}

#[derive(Default)]
pub struct Gesture {
    contact: Option<Contact>,
    clicks: Vec<Input>,
    click_deadline: Option<Instant>,
    last_seen: Option<Instant>,
}

impl Gesture {
    // Invalidate the original owner without interpreting cleanup as a physical release
    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    pub fn is_idle(&self) -> bool {
        self.contact.is_none() && self.clicks.is_empty()
    }

    // Arm one scheduler for the earliest pending recognition instead of polling
    pub fn deadline(&self) -> Option<Instant> {
        match (
            self.click_deadline,
            self.contact.as_ref().and_then(|contact| contact.hold),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (clicks, hold) => clicks.or(hold),
        }
    }

    // Process deadlines even when the device did not deliver another report
    pub fn advance(&mut self, now: Instant, slots: EventMask) -> Vec<RoutedInput> {
        let mut routed = Vec::new();
        if self.accept_time(now) {
            self.fire_due(now, slots, &mut routed);
        }
        routed
    }

    // Interpret normalized physical input using only configured and qualified slots
    pub fn input(
        &mut self,
        input: &Input,
        now: Instant,
        slots: EventMask,
        timing: Timing,
    ) -> Vec<RoutedInput> {
        let mut routed = Vec::new();
        if !self.accept_time(now) {
            return routed;
        }
        if matches!(input, Input::Touch { phase, .. } if phase == "cancel") {
            self.cancel();
            self.last_seen = Some(now);
            return routed;
        }
        // A recognized swipe wins over a still queued click or hold, without inventing an up
        if let Input::Swipe { direction } = input {
            if let Some(slot) = swipe(direction) {
                self.consume();
                push(slots, slot, input.clone(), &mut routed);
            }
            return routed;
        }
        self.fire_due(now, slots, &mut routed);
        match input {
            Input::Pulse if self.contact.is_none() => {
                self.click(Input::Pulse, now, slots, timing, &mut routed)
            }
            Input::Button { pressed: true } => {
                self.start_contact(ContactKind::Button, input, now, slots, timing, &mut routed);
            }
            Input::Button { pressed: false } => {
                self.end_contact(ContactKind::Button, input, now, slots, timing, &mut routed);
            }
            Input::Rotate { delta } if *delta != 0 => {
                let (ordinary, held) = if *delta < 0 {
                    (TurnLeft, HeldTurnLeft)
                } else {
                    (TurnRight, HeldTurnRight)
                };
                let slot = if self
                    .contact
                    .as_ref()
                    .is_some_and(|contact| contact.kind == ContactKind::Button)
                    && slots & held.bit() != 0
                {
                    self.consume();
                    held
                } else {
                    ordinary
                };
                push(slots, slot, input.clone(), &mut routed);
            }
            Input::Touch { phase, .. } if matches!(phase.as_str(), "start" | "begin" | "down") => {
                self.start_contact(ContactKind::Touch, input, now, slots, timing, &mut routed);
            }
            Input::Touch { phase, .. } if matches!(phase.as_str(), "end" | "up") => {
                self.end_contact(ContactKind::Touch, input, now, slots, timing, &mut routed);
            }
            Input::Touch { phase, .. } if phase == "lift" && self.contact.is_none() => {
                // Only the runtime may normalize a verified end-only protocol to a physical lift
                push(slots, PressUp, one_shot(input, "up"), &mut routed);
                self.click(one_shot(input, "tap"), now, slots, timing, &mut routed);
            }
            Input::Touch { phase, .. } if phase == "tap" && self.contact.is_none() => {
                self.click(input.clone(), now, slots, timing, &mut routed);
            }
            _ => {}
        }
        routed
    }

    // Reject backward timestamps without transferring ownership to a later scope
    fn accept_time(&mut self, now: Instant) -> bool {
        if self.last_seen.is_some_and(|previous| now < previous) {
            return false;
        }
        self.last_seen = Some(now);
        true
    }

    // Keep the contact for a possible physical up but suppress interpreted actions
    fn consume(&mut self) {
        self.clicks.clear();
        self.click_deadline = None;
        if let Some(contact) = &mut self.contact {
            contact.consumed = true;
            contact.hold = None;
        }
    }

    fn start_contact(
        &mut self,
        kind: ContactKind,
        input: &Input,
        now: Instant,
        slots: EventMask,
        timing: Timing,
        routed: &mut Vec<RoutedInput>,
    ) {
        let contact_slots = CLICKS
            | PressDown.bit()
            | PressUp.bit()
            | LongHold.bit()
            | HeldTurnLeft.bit()
            | HeldTurnRight.bit();
        if self.contact.is_some() || slots & contact_slots == 0 {
            return;
        }
        self.contact = Some(Contact {
            kind,
            hold: (slots & LongHold.bit() != 0).then(|| now + timing.hold()),
            consumed: false,
            initiated: input.clone(),
        });
        push(slots, PressDown, input.clone(), routed);
    }

    fn end_contact(
        &mut self,
        kind: ContactKind,
        input: &Input,
        now: Instant,
        slots: EventMask,
        timing: Timing,
        routed: &mut Vec<RoutedInput>,
    ) {
        if !self
            .contact
            .as_ref()
            .is_some_and(|contact| contact.kind == kind)
        {
            return;
        }
        let contact = self.contact.take().unwrap();
        push(slots, PressUp, input.clone(), routed);
        if !contact.consumed {
            self.click(one_shot(input, "tap"), now, slots, timing, routed);
        }
    }

    // Settle at most three completed clicks and restart the inter-click gap
    fn click(
        &mut self,
        input: Input,
        now: Instant,
        slots: EventMask,
        timing: Timing,
        routed: &mut Vec<RoutedInput>,
    ) {
        if slots & CLICKS == 0 {
            return;
        }
        self.clicks.push(input);
        let could_extend = match self.clicks.len() {
            1 => slots & (DoublePress.bit() | TriplePress.bit()) != 0,
            2 => slots & TriplePress.bit() != 0,
            _ => false,
        };
        if could_extend {
            self.click_deadline = Some(now + timing.gap());
        } else {
            self.resolve_clicks(slots, routed);
        }
    }

    // Prefer the longest configured match and preserve unmatched single clicks
    fn resolve_clicks(&mut self, slots: EventMask, routed: &mut Vec<RoutedInput>) {
        self.click_deadline = None;
        for (count, slot) in [(3, TriplePress), (2, DoublePress), (1, SinglePress)] {
            if slots & slot.bit() == 0 {
                continue;
            }
            while self.clicks.len() >= count {
                let input = self.clicks.drain(..count).next_back().unwrap();
                routed.push(RoutedInput { slot, input });
            }
        }
        self.clicks.clear();
    }

    // Resolve in deadline order, letting a hold consume unresolved click gestures
    fn fire_due(&mut self, now: Instant, slots: EventMask, routed: &mut Vec<RoutedInput>) {
        let hold = self.contact.as_ref().and_then(|contact| contact.hold);
        if self
            .click_deadline
            .is_some_and(|click| click <= now && hold.is_none_or(|hold| click < hold))
        {
            self.resolve_clicks(slots, routed);
        }
        if hold.is_some_and(|hold| hold <= now) {
            let input = one_shot(&self.contact.as_ref().unwrap().initiated, "hold");
            self.consume();
            push(slots, LongHold, input, routed);
        }
    }
}

fn one_shot(input: &Input, touch_phase: &str) -> Input {
    match input {
        Input::Touch { x, y, .. } => Input::Touch {
            phase: touch_phase.into(),
            x: *x,
            y: *y,
        },
        _ => Input::Pulse,
    }
}

fn push(slots: EventMask, slot: EventSlot, input: Input, routed: &mut Vec<RoutedInput>) {
    if slots & slot.bit() != 0 {
        routed.push(RoutedInput { slot, input });
    }
}

fn swipe(direction: &str) -> Option<EventSlot> {
    match direction {
        "left" => Some(SwipeLeft),
        "right" => Some(SwipeRight),
        "up" => Some(SwipeUp),
        "down" => Some(SwipeDown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, elapsed_ms: u64) -> Instant {
        start + Duration::from_millis(elapsed_ms)
    }

    fn slots(output: Vec<RoutedInput>) -> Vec<EventSlot> {
        output.into_iter().map(|value| value.slot).collect()
    }

    fn event(
        gesture: &mut Gesture,
        input: &Input,
        start: Instant,
        ms: u64,
        mask: EventMask,
    ) -> Vec<EventSlot> {
        slots(gesture.input(input, at(start, ms), mask, Timing::default()))
    }

    #[test]
    // Pulse-only hardware never derives physical edges or a hold from any number of clicks
    fn pulse_only_never_invents_physical_release() {
        let mut g = Gesture::default();
        let start = Instant::now();
        let mask = PressDown.bit() | PressUp.bit() | LongHold.bit() | SinglePress.bit();
        assert_eq!(event(&mut g, &Input::Pulse, start, 0, mask), [SinglePress]);
        assert!(g.is_idle());
        assert!(g.deadline().is_none());
        assert!(g.advance(at(start, 1000), mask).is_empty());
    }

    #[test]
    // Higher-count bindings defer only while a longer configured sequence is possible
    fn triple_and_fourth_press_use_separate_sequences() {
        let mut g = Gesture::default();
        let start = Instant::now();
        for ms in [0, 299] {
            assert!(event(&mut g, &Input::Pulse, start, ms, CLICKS).is_empty());
            assert_eq!(g.deadline(), Some(at(start, ms + 300)));
        }
        assert_eq!(
            event(&mut g, &Input::Pulse, start, 598, CLICKS),
            [TriplePress]
        );
        assert!(event(&mut g, &Input::Pulse, start, 700, CLICKS).is_empty());
        assert!(g.advance(at(start, 999), CLICKS).is_empty());
        assert_eq!(slots(g.advance(at(start, 1000), CLICKS)), [SinglePress]);
        assert!(g.is_idle());
        let mask = CLICKS & !TriplePress.bit();
        assert!(event(&mut g, &Input::Pulse, start, 1200, mask).is_empty());
        assert_eq!(
            event(&mut g, &Input::Pulse, start, 1250, mask),
            [DoublePress]
        );
    }

    #[test]
    // Missing double falls back to two singles and an unbound single produces nothing
    fn click_timeout_preserves_only_configured_fallbacks() {
        let start = Instant::now();
        let mut g = Gesture::default();
        let mask = SinglePress.bit() | TriplePress.bit();
        assert!(event(&mut g, &Input::Pulse, start, 0, mask).is_empty());
        assert!(event(&mut g, &Input::Pulse, start, 100, mask).is_empty());
        assert_eq!(
            slots(g.advance(at(start, 400), mask)),
            [SinglePress, SinglePress]
        );
        let mask = DoublePress.bit() | TriplePress.bit();
        assert!(event(&mut g, &Input::Pulse, start, 450, mask).is_empty());
        assert!(g.advance(at(start, 750), mask).is_empty());
        assert!(event(&mut g, &Input::Pulse, start, 900, mask).is_empty());
        assert!(event(&mut g, &Input::Pulse, start, 910, mask).is_empty());
        assert_eq!(slots(g.advance(at(start, 1210), mask)), [DoublePress]);
    }

    #[test]
    // Duplicate and backward reports cannot rearm a held contact or generate a new click
    fn genuine_edges_and_hold_have_independent_delivery() {
        let start = Instant::now();
        let mut g = Gesture::default();
        let mask = PressDown.bit() | PressUp.bit() | LongHold.bit() | SinglePress.bit();
        let down = Input::Button { pressed: true };
        let up = Input::Button { pressed: false };
        assert!(event(&mut g, &up, start, 0, mask).is_empty());
        assert_eq!(event(&mut g, &down, start, 10, mask), [PressDown]);
        assert!(event(&mut g, &down, start, 100, mask).is_empty());
        assert!(event(&mut g, &up, start, 99, mask).is_empty());
        assert_eq!(g.deadline(), Some(at(start, 610)));
        assert!(g.advance(at(start, 609), mask).is_empty());
        assert_eq!(slots(g.advance(at(start, 610), mask)), [LongHold]);
        assert!(g.advance(at(start, 620), mask).is_empty());
        assert_eq!(event(&mut g, &up, start, 650, mask), [PressUp]);
        assert!(event(&mut g, &up, start, 660, mask).is_empty());
        assert_eq!(event(&mut g, &down, start, 700, mask), [PressDown]);
        assert_eq!(event(&mut g, &up, start, 710, mask), [PressUp, SinglePress]);
    }

    #[test]
    // Held-turn has direction-specific precedence and keeps the full signed delta
    fn held_rotation_consumes_click_and_hold_but_not_raw_up() {
        let start = Instant::now();
        let mut g = Gesture::default();
        let mask = PressUp.bit()
            | LongHold.bit()
            | SinglePress.bit()
            | TurnLeft.bit()
            | TurnRight.bit()
            | HeldTurnRight.bit();
        let down = Input::Button { pressed: true };
        let up = Input::Button { pressed: false };
        assert!(event(&mut g, &down, start, 0, mask).is_empty());
        let left = Input::Rotate { delta: -9 };
        let right = Input::Rotate { delta: 27 };
        assert_eq!(
            g.input(&left, at(start, 20), mask, Timing::default()),
            [RoutedInput {
                slot: TurnLeft,
                input: left
            }]
        );
        assert_eq!(
            g.input(&right, at(start, 40), mask, Timing::default()),
            [RoutedInput {
                slot: HeldTurnRight,
                input: right
            }]
        );
        assert!(g.deadline().is_none());
        assert_eq!(event(&mut g, &up, start, 710, mask), [PressUp]);
        assert!(event(&mut g, &Input::Rotate { delta: 0 }, start, 720, mask).is_empty());
    }

    #[test]
    // Touch taps retain their location while a swipe or cancellation may consume the sequence
    fn touch_swipe_cancels_pending_tap_without_fake_edge() {
        let start = Instant::now();
        let mut g = Gesture::default();
        let touch = |phase: &str, x| Input::Touch {
            phase: phase.into(),
            x,
            y: 8,
        };
        let mask = CLICKS | SwipeRight.bit() | PressDown.bit() | PressUp.bit();
        assert!(event(&mut g, &touch("end", 10), start, 0, mask).is_empty());
        assert!(event(&mut g, &touch("tap", 20), start, 10, mask).is_empty());
        assert_eq!(
            event(
                &mut g,
                &Input::Swipe {
                    direction: "right".into()
                },
                start,
                20,
                mask
            ),
            [SwipeRight]
        );
        assert!(g.advance(at(start, 400), mask).is_empty());
        assert_eq!(
            event(&mut g, &touch("start", 30), start, 411, mask),
            [PressDown]
        );
        assert!(event(&mut g, &touch("cancel", 40), start, 421, mask).is_empty());
        assert!(event(&mut g, &touch("end", 50), start, 431, mask).is_empty());
        let tap = touch("tap", 90);
        assert_eq!(
            g.input(&tap, at(start, 450), SinglePress.bit(), Timing::default()),
            [RoutedInput {
                slot: SinglePress,
                input: tap
            }]
        );
    }

    #[test]
    // Separately scoped controls do not combine clicks and cancellation cannot emit delayed output
    fn simultaneous_controls_and_devices_keep_distinct_sequences() {
        let start = Instant::now();
        let mut controls = [Gesture::default(), Gesture::default(), Gesture::default()];
        for control in &mut controls {
            assert!(event(control, &Input::Pulse, start, 0, CLICKS).is_empty());
        }
        controls[0].cancel();
        assert!(event(&mut controls[1], &Input::Pulse, start, 150, CLICKS).is_empty());
        assert_eq!(
            slots(controls[2].advance(at(start, 300), CLICKS)),
            [SinglePress]
        );
        assert_eq!(
            slots(controls[1].advance(at(start, 450), CLICKS)),
            [DoublePress]
        );
        assert!(controls[0].advance(at(start, 500), CLICKS).is_empty());
    }

    #[test]
    // Check the actual N4 Pro contract rather than synthetic release capabilities
    fn n4_pro_pulse_dials_never_offer_release_hold_or_held_rotation() {
        let module = hardware_registry::modules()
            .into_iter()
            .find(|module| module.manifest().model_id == "n4-pro")
            .unwrap();
        let contract = module.representative_contract().unwrap();
        let model = &contract.manifest;
        for index in 1..=4 {
            assert_eq!(
                supported(model, &format!("dial-{index}")),
                CLICKS | TurnLeft.bit() | TurnRight.bit()
            );
            assert_eq!(supported(model, &format!("touch-{index}")), CLICKS);
        }
        assert_eq!(
            supported(model, "touch-strip"),
            SwipeLeft.bit() | SwipeRight.bit() | SwipeUp.bit() | SwipeDown.bit()
        );
        assert_ne!(supported(model, "key-1") & PressUp.bit(), 0);
        assert_eq!(supported(model, "unknown"), 0);
    }
}
