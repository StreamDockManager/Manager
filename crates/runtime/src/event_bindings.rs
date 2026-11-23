//! Deadline-driven routing for explicitly advanced device bindings
use crate::{
    actions::{Action, EventSlot, Request},
    builtins::EffectiveBinding,
    control_events::{supported, EventMask, Gesture, RoutedInput, Timing},
    Runtime, Snapshot,
};
use hardware_core::{ControlEvent, Input};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    time::Instant,
};

const MAX_TRACKED_CONTROLS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    device: String,
    control: String,
    epoch: u64,
    profile: String,
    page: String,
    scope: String,
    signature: String,
}

struct Track {
    target: Target,
    mask: EventMask,
    gesture: Gesture,
    trace: Option<String>,
    physical: Option<Arc<AtomicBool>>,
    pressed: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

struct Delivery {
    target: Target,
    input: RoutedInput,
    trace: Option<String>,
    physical: Option<Arc<AtomicBool>>,
    pressed: Arc<AtomicBool>,
    observed: Instant,
    live: Arc<AtomicBool>,
}

impl Track {
    // Carry the original connection and control into every scheduled action
    fn deliveries(
        &self,
        inputs: Vec<RoutedInput>,
        observed: Instant,
        trusted: bool,
    ) -> Vec<Delivery> {
        inputs
            .into_iter()
            .map(|input| Delivery {
                target: self.target.clone(),
                input,
                trace: self.trace.clone(),
                physical: if trusted { self.physical.clone() } else { None },
                pressed: self.pressed.clone(),
                observed,
                live: self.live.clone(),
            })
            .collect()
    }
}

#[derive(Default)]
struct Pending {
    tracks: HashMap<(String, String), Track>,
    device_leases: HashMap<String, Arc<AtomicBool>>,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct EventBindings {
    pending: Mutex<Pending>,
    signal: Condvar,
}

impl EventBindings {
    // Wait for the earliest real deadline with no polling when the router is idle
    pub(crate) fn start(self: &Arc<Self>, runtime: Weak<Runtime>) {
        let router = self.clone();
        std::thread::spawn(move || {
            while let Some(batch) = router.wait() {
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                for delivery in batch {
                    runtime.deliver_bound_event(delivery);
                }
            }
        });
    }

    // Resolve due timers and collect work before dropping the scheduler lock
    fn wait(&self) -> Option<Vec<Delivery>> {
        let mut pending = self.pending.lock().unwrap();
        loop {
            if pending.closed {
                return None;
            }
            let now = Instant::now();
            let deadline = pending
                .tracks
                .values()
                .filter_map(|track| track.gesture.deadline())
                .min();
            match deadline {
                Some(at) if at <= now => {
                    let mut output = Vec::new();
                    pending.tracks.retain(|_, track| {
                        if track.gesture.deadline().is_some_and(|due| due <= now) {
                            let result = track.gesture.advance(now, track.mask);
                            // Delayed gestures cannot create a fresh physical permission grant
                            output.extend(track.deliveries(result, now, false));
                        }
                        !track.gesture.is_idle()
                    });
                    if !output.is_empty() {
                        return Some(output);
                    }
                }
                Some(at) => {
                    pending = self.signal.wait_timeout(pending, at - now).unwrap().0;
                }
                None => pending = self.signal.wait(pending).unwrap(),
            }
        }
    }

    // Retire existing gesture state without delivering a synthetic Up action
    pub(crate) fn cancel(&self, device: Option<&str>) {
        let mut pending = self.pending.lock().unwrap();
        if let Some(device) = device {
            if let Some(lease) = pending.device_leases.remove(device) {
                lease.store(false, Ordering::Release);
            }
        } else {
            for (_, lease) in pending.device_leases.drain() {
                lease.store(false, Ordering::Release);
            }
        }
        pending.tracks.retain(|(current, _), track| {
            let keep = device.is_some_and(|value| value != current);
            if !keep {
                track.pressed.store(false, Ordering::Release);
            }
            keep
        });
        self.signal.notify_one();
    }

    pub(crate) fn is_tracking(&self, device: &str, control: &str) -> bool {
        self.pending
            .lock()
            .unwrap()
            .tracks
            .keys()
            .any(|(current, id)| current == device && id == control)
    }

    // Close the scheduler and release any physical press tokens
    pub(crate) fn shutdown(&self) {
        self.cancel(None);
        self.pending.lock().unwrap().closed = true;
        self.signal.notify_one();
    }

    // Consume tap/hold state on the panel's other controls once a swipe is recognized
    fn swipe(&self, device: &str, touch_ids: &[String]) {
        let mut pending = self.pending.lock().unwrap();
        pending.tracks.retain(|(current, control), track| {
            let keep = current != device || !touch_ids.contains(control);
            if !keep {
                track.pressed.store(false, Ordering::Release);
            }
            keep
        });
        self.signal.notify_one();
    }

    // Feed one input report and collect immediate actions outside the scheduler lock
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep device identity, gesture policy, timing, trace and physical trust explicit"
    )]
    fn input(
        &self,
        target: Target,
        mask: EventMask,
        timing: Timing,
        input: &Input,
        now: Instant,
        trace: Option<String>,
        physical: Option<Arc<AtomicBool>>,
    ) -> Vec<Delivery> {
        let mut pending = self.pending.lock().unwrap();
        if pending.closed {
            return Vec::new();
        }
        let key = (target.device.clone(), target.control.clone());
        if pending
            .tracks
            .get(&key)
            .is_some_and(|track| track.target != target)
        {
            if let Some(old) = pending.tracks.remove(&key) {
                old.pressed.store(false, Ordering::Release);
            }
        }
        if pending.tracks.len() >= MAX_TRACKED_CONTROLS && !pending.tracks.contains_key(&key) {
            return Vec::new();
        }
        let live = pending
            .device_leases
            .entry(key.0.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone();
        let track = pending.tracks.entry(key.clone()).or_insert_with(|| Track {
            target,
            mask,
            gesture: Gesture::default(),
            trace: None,
            physical: None,
            pressed: Arc::new(AtomicBool::new(false)),
            live,
        });
        track.trace = trace;
        let trusted = physical.is_some();
        track.physical = physical;
        if matches!(input, Input::Button { pressed: true })
            && !track.pressed.load(Ordering::Acquire)
        {
            track.pressed = Arc::new(AtomicBool::new(true));
        }
        if matches!(input, Input::Button { pressed: false }) {
            track.pressed.store(false, Ordering::Release);
        }
        let actions = track.gesture.input(input, now, mask, timing);
        let deliveries = track.deliveries(actions, now, trusted);
        if track.gesture.is_idle() {
            pending.tracks.remove(&key);
        }
        self.signal.notify_one();
        deliveries
    }
}

// Intersect configured event lanes with the actual device and control capabilities
pub(crate) fn active_slots(
    snapshot: &Snapshot,
    device: &str,
    control: &str,
    actions: &[Action],
) -> EventMask {
    let Some(info) = snapshot.devices.iter().find(|entry| entry.id == device) else {
        return 0;
    };
    let capability = supported(&info.manifest, control);
    actions.iter().enumerate().fold(0, |mask, (index, action)| {
        if matches!(action, Action::None) || index >= u16::BITS as usize {
            mask
        } else {
            mask | ((1 << index) & capability)
        }
    })
}

// Give an event root a stable context without making a standalone plugin action a multi-action
pub(crate) fn event_scope(scope: &str, slot: EventSlot) -> String {
    format!("{scope}#event/{}", slot.index())
}

pub(crate) fn strip_event_scope(scope: &str) -> Option<&str> {
    let (base, slot) = scope.rsplit_once("#event/")?;
    EventSlot::from_index(slot.parse().ok()?).map(|_| base)
}

// Prefer the ordinary press as the display owner and otherwise select the first configured lane
pub(crate) fn display_slot(mask: EventMask) -> Option<EventSlot> {
    std::iter::once(EventSlot::SinglePress)
        .chain(EventSlot::ALL)
        .find(|slot| mask & slot.bit() != 0)
}

// Normalize protocol pulse and end-only touch reports before either recognizer
pub(crate) fn normalize_input(
    input: &Input,
    capabilities: EventMask,
    is_end_only: bool,
) -> Option<Input> {
    match input {
        Input::Touch { phase, x, y } if phase == "end" && is_end_only => Some(Input::Touch {
            phase: if capabilities & EventSlot::PressUp.bit() != 0 {
                "lift"
            } else {
                "tap"
            }
            .into(),
            x: *x,
            y: *y,
        }),
        Input::Button { pressed: true } if capabilities & EventSlot::PressUp.bit() == 0 => {
            Some(Input::Pulse)
        }
        _ => None,
    }
}

impl Runtime {
    // Identify an action and freeze its binding and session ownership
    fn event_target(
        &self,
        snapshot: &Snapshot,
        effective: &EffectiveBinding,
        device: &str,
        control: &str,
        epoch: u64,
    ) -> Option<Target> {
        if effective.binding.disabled {
            return None;
        }
        let session = snapshot.session(device);
        Some(Target {
            device: device.into(),
            control: control.into(),
            epoch,
            profile: session.profile_id,
            page: session.page_id,
            scope: effective.scope.clone(),
            signature: serde_json::to_string(&effective.binding).ok()?,
        })
    }

    // Route advanced controls; an explicit strip binding owns all of its swipe directions
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep event routing and physical input identity explicit at the runtime boundary"
    )]
    pub(crate) fn route_bound_events(
        &self,
        snapshot: &Snapshot,
        effective: Option<&EffectiveBinding>,
        device: &str,
        event: &ControlEvent,
        input: &Input,
        epoch: u64,
        observed: Instant,
        trace: Option<String>,
        physical: Option<Arc<AtomicBool>>,
    ) -> bool {
        let info = snapshot.devices.iter().find(|entry| entry.id == device);
        let control = info.and_then(|info| info.manifest.control(&event.control_id).ok());
        let panel = info.and_then(|info| info.manifest.capabilities.touch_panel.as_ref());
        let strip_swipe = matches!(input, Input::Swipe { .. })
            && panel.is_some_and(|panel| panel.gesture_control == event.control_id);
        if matches!(input, Input::Swipe { .. }) {
            if let Some(panel) = panel {
                self.event_bindings.swipe(device, &panel.button_controls);
            }
        }
        let Some(effective) = effective else {
            return false;
        };
        let Action::Events {
            actions,
            multi_press_ms,
            hold_ms,
        } = &effective.binding.action
        else {
            return false;
        };
        if self
            .builtin_state
            .paused
            .lock()
            .unwrap()
            .contains_key(device)
            || effective.binding.disabled
        {
            self.event_bindings.cancel(Some(device));
            return strip_swipe;
        }
        let Some(target) = self.event_target(snapshot, effective, device, &event.control_id, epoch)
        else {
            return strip_swipe;
        };
        let mask = active_slots(snapshot, device, &event.control_id, actions);
        let capabilities = info.map_or(0, |device| supported(&device.manifest, &event.control_id));
        let is_end_only = control.is_some_and(|control| {
            control.kind == "touch"
                && !control
                    .semantics
                    .iter()
                    .any(|s| s == "touch_down" || s == "swipe")
        });
        let normalized = normalize_input(input, capabilities, is_end_only);
        let input = normalized.as_ref().unwrap_or(input);
        let deliveries = self.event_bindings.input(
            target,
            mask,
            Timing {
                multi_press_ms: *multi_press_ms,
                hold_ms: *hold_ms,
            },
            input,
            observed,
            trace.clone(),
            physical,
        );
        let routed = !deliveries.is_empty();
        if !routed {
            self.input_trace.record(
                trace.as_deref(),
                "No immediate event binding or gesture pending",
                true,
            );
        }
        for delivery in deliveries {
            self.deliver_bound_event(delivery);
        }
        // An empty slot is explicitly unassigned once the Events binding has been saved.
        strip_swipe || routed || !matches!(input, Input::Swipe { .. })
    }

    // Revalidate a pending event before it can reach the normal action queue
    fn deliver_bound_event(&self, delivery: Delivery) {
        let Delivery {
            target,
            input: routed,
            trace,
            physical,
            pressed,
            observed,
            live,
        } = delivery;
        if !live.load(Ordering::Acquire)
            || self
                .control_event_test
                .blocks_pending(&target.device, target.epoch, &target.control)
            || self.is_stopping()
            || self.connection_generation(&target.device) != Some(target.epoch)
            || self
                .builtin_state
                .paused
                .lock()
                .unwrap()
                .contains_key(&target.device)
            || self
                .effective_device_output(&target.device)
                .is_ok_and(|output| output.sleeping)
        {
            return;
        }
        let snapshot = self.snapshot();
        let Some(effective) =
            self.resolve_input_binding(&snapshot, &target.device, &target.control)
        else {
            return;
        };
        if self
            .event_target(
                &snapshot,
                &effective,
                &target.device,
                &target.control,
                target.epoch,
            )
            .as_ref()
            != Some(&target)
        {
            self.input_trace.record(
                trace.as_deref(),
                "Event binding retired before dispatch",
                true,
            );
            return;
        }
        let Action::Events { actions, .. } = &effective.binding.action else {
            return;
        };
        if active_slots(&snapshot, &target.device, &target.control, actions) & routed.slot.bit()
            == 0
        {
            return;
        }
        let Some(action) = actions.get(routed.slot as usize).cloned() else {
            return;
        };
        if !live.load(Ordering::Acquire) {
            return;
        }
        let scope = event_scope(&target.scope, routed.slot);
        let physical_activation = match (&action, &physical, &routed.input) {
            (
                Action::Plugin {
                    plugin_id,
                    action_id,
                    ..
                },
                Some(stop),
                Input::Pulse | Input::Button { pressed: true },
            ) if !stop.load(Ordering::Acquire) => {
                self.plugins.lock().unwrap().begin_physical_permission(
                    plugin_id,
                    action_id,
                    &target.device,
                    &target.control,
                    &scope,
                    stop.clone(),
                )
            }
            _ => None,
        };
        self.input_trace
            .record(trace.as_deref(), &format!("Event {:?}", routed.slot), false);
        let mut request = Request {
            trace: trace.clone(),
            event_slot: Some(routed.slot),
            physical_activation,
            manual: false,
            activation_policy: effective.binding.activation_policy,
            icon_signature: None,
            pressed: (routed.slot == EventSlot::PressDown).then_some(pressed),
            action,
            device_id: target.device.clone(),
            control_id: target.control,
            binding_scope: scope,
            queued_at: observed,
            connection_epoch: Some(target.epoch),
            input: routed.input,
            cancellation_generation: self.cancellation.load(Ordering::SeqCst),
        };
        request.icon_signature = crate::feedback::request_icon_signature(self, &request);
        if let Err(error) = self.queue_action(request) {
            self.input_trace
                .record(trace.as_deref(), "Event action was not admitted", true);
            self.emit("action-error", Some(target.device), None, Some(error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    // Verify two event rows of the same plugin have separate settings, contexts, and dispatch
    #[test]
    fn event_rows_use_distinct_real_plugin_contexts_and_only_matching_gestures() {
        const DEVICE: &str = "simulator:n4-pro:event-bindings";
        const OWNER: &str = "org.fixture.events";
        const ACTION: &str = "org.fixture.events.turn";
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/org.fixture.events.sdPlugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("index.html"),
            "<!doctype html><title>Events fixture</title>",
        )
        .unwrap();
        std::fs::write(
            package.join("icon.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"/>"#,
        )
        .unwrap();
        std::fs::write(package.join("manifest.json"), json!({
            "UUID":OWNER,"Name":"Events fixture","Version":"1.0.0","SDKVersion":1,
            "CodePath":"index.html","Icon":"icon","Author":"Fixture","Description":"Test",
            "Actions":[{"UUID":ACTION,"Name":"Fixture","Controllers":["Encoder"],
                        "Icon":"icon","States":[{"Image":"icon"}],"PropertyInspectorPath":"index.html"}]
        }).to_string()).unwrap();
        let runtime = crate::test_support::headless(root.path().into()).unwrap();
        runtime.simulate_device(true, DEVICE).unwrap();
        let snapshot = runtime.snapshot();
        let session = snapshot.session(DEVICE);
        let mut profile = snapshot
            .profiles
            .into_iter()
            .find(|p| p.id == session.profile_id)
            .unwrap();
        let page = profile
            .pages
            .iter_mut()
            .find(|p| p.id == session.page_id)
            .unwrap();
        let action = |marker: &str| Action::Plugin {
            plugin_id: OWNER.into(),
            action_id: ACTION.into(),
            settings: json!({"marker":marker}),
            user_desired_state: None,
            resources: Default::default(),
        };
        let mut actions = vec![Action::None; EventSlot::COUNT];
        actions[EventSlot::DoublePress.index()] = action("ALPHA");
        actions[EventSlot::TriplePress.index()] = action("BRAVO");
        let binding: crate::Binding = serde_json::from_value(json!({
            "title":"Event fixture","color":"#000000","action": Action::Events {
                actions, multi_press_ms: 200, hold_ms: 600,
            }
        }))
        .unwrap();
        page.bindings.insert("dial-1".into(), binding);
        profile.pages.push(crate::profiles::Page {
            id: "alternate-page".into(),
            name: "Alternate".into(),
            bindings: Default::default(),
        });
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        runtime.plugins.lock().unwrap().enable(OWNER, true).unwrap();
        crate::test_support::synchronize_plugins(&runtime);
        let url = {
            let mut host = runtime.plugins.lock().unwrap();
            host.poll();
            host.browser_view(OWNER)
                .unwrap()
                .unwrap_or_else(|| panic!("Plugin did not start: {:?}", host.list()))
                .url
        };
        crate::test_support::synchronize_plugins(&runtime);
        let parts = url.split('/').collect::<Vec<_>>();
        let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
        }
        socket
            .send(tungstenite::Message::Text(
                json!({
                    "event":"registerPlugin", "uuid":parts[4],
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let mut read_matching = |wanted: &str| loop {
            let msg = socket.read().unwrap();
            if let Ok(text) = msg.to_text() {
                let value: Value = serde_json::from_str(text).unwrap();
                if value["event"] == wanted {
                    break value;
                }
            }
        };
        let mut contexts = HashMap::new();
        for _ in 0..2 {
            let message = read_matching("willAppear");
            assert_eq!(message["payload"]["isInMultiAction"], false);
            contexts.insert(
                message["payload"]["settings"]["marker"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                message["context"].as_str().unwrap().to_owned(),
            );
        }
        assert_ne!(contexts["ALPHA"], contexts["BRAVO"]);
        for (slot, marker) in [
            (EventSlot::DoublePress, "ALPHA"),
            (EventSlot::TriplePress, "BRAVO"),
        ] {
            let panel = runtime.open_vendor_inspector(OWNER, ACTION, json!({
                "profile_id":session.profile_id,"page_id":session.page_id,"control_id":"dial-1","path":[slot.index()],
            })).unwrap();
            let mut host = runtime.plugins.lock().unwrap();
            assert_eq!(
                host.panel_value(&panel.token).unwrap()["settings"]["marker"],
                marker
            );
            host.close_inspector(&panel.token);
        }
        for (clicks, marker) in [(3, "BRAVO"), (2, "ALPHA")] {
            for _ in 0..clicks {
                runtime
                    .simulate_input(
                        DEVICE,
                        ControlEvent {
                            control_id: "dial-1".into(),
                            input: Input::Pulse,
                        },
                    )
                    .unwrap();
            }
            let down = read_matching("dialDown");
            assert_eq!(down["context"], contexts[marker].as_str());
            assert_eq!(down["payload"]["settings"]["marker"], marker);
            let up = read_matching("dialUp");
            assert_eq!(up["context"], contexts[marker].as_str());
        }
        // The non-display lane can receive its own effects without taking over the LCD
        for (marker, title) in [("ALPHA", "display owner"), ("BRAVO", "late other lane")] {
            socket
                .send(tungstenite::Message::Text(
                    json!({
                        "event":"setTitle", "context":contexts[marker], "payload":{"title":title}
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
        }
        let base = format!("{}:{}", session.profile_id, session.page_id);
        let late_scope = event_scope(&base, EventSlot::TriplePress);
        let deadline = Instant::now() + Duration::from_secs(4);
        while !runtime
            .plugins
            .lock()
            .unwrap()
            .surface(DEVICE, "dial-1", &late_scope)
            .is_some_and(|value| value["title"] == "late other lane")
        {
            assert!(
                Instant::now() < deadline,
                "late plugin feedback did not arrive"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let scene = runtime.render_scene(DEVICE).unwrap();
        assert_eq!(scene.visuals["dial-1"]["title"], "display owner");
        runtime
            .simulate_input(
                DEVICE,
                ControlEvent {
                    control_id: "dial-1".into(),
                    input: Input::Pulse,
                },
            )
            .unwrap();
        assert!(!runtime
            .event_bindings
            .pending
            .lock()
            .unwrap()
            .tracks
            .is_empty());
        runtime
            .select_device_profile(DEVICE, &session.profile_id, Some("alternate-page"), false)
            .unwrap();
        assert!(runtime
            .event_bindings
            .pending
            .lock()
            .unwrap()
            .tracks
            .is_empty());
        runtime.shutdown();
    }

    // Verify a due click sequence cannot cross a reconnection or binding scope
    #[test]
    fn rebind_retires_pending_clicks_without_dispatch() {
        let router = EventBindings::default();
        let target = Target {
            device: "device".into(),
            control: "dial".into(),
            epoch: 1,
            profile: "profile".into(),
            page: "page".into(),
            scope: "profile:page".into(),
            signature: "old binding".into(),
        };
        let now = Instant::now();
        let mask = EventSlot::SinglePress.bit() | EventSlot::DoublePress.bit();
        assert!(router
            .input(
                target.clone(),
                mask,
                Timing::default(),
                &Input::Pulse,
                now,
                None,
                None
            )
            .is_empty());
        let next = Target {
            epoch: 2,
            signature: "new binding".into(),
            ..target
        };
        let result = router.input(
            next,
            EventSlot::SinglePress.bit(),
            Timing::default(),
            &Input::Pulse,
            now + Duration::from_millis(10),
            None,
            None,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].target.epoch, 2);
        assert_eq!(result[0].input.slot, EventSlot::SinglePress);
        assert!(router.pending.lock().unwrap().tracks.is_empty());
    }
}
