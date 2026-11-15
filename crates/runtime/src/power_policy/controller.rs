use super::{
    native, preferences::Preferences, DecisionState, DevicePowerPolicy, DevicePowerStatus,
    PowerAction, PowerPlatformCapabilities, SensorSample,
};
use crate::{sampler, workspace::DeviceOutput, DeviceInfo, Runtime};
use chrono::{Datelike, Timelike};
use hardware_core::{ControlEvent, Input};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DevicePowerCapabilities {
    pub platform: PowerPlatformCapabilities,
    pub dim: bool,
    pub sleep: bool,
    pub wake_input: bool,
    pub wake_description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DevicePowerSettings {
    pub device_id: String,
    pub device_key: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub policy: DevicePowerPolicy,
    pub capabilities: DevicePowerCapabilities,
    pub status: DevicePowerStatus,
}
#[derive(Clone)]
struct View {
    device: DeviceInfo,
    baseline: DeviceOutput,
    epoch: u64,
}
struct Change {
    id: String,
    epoch: u64,
    entering_sleep: bool,
    output_changed: bool,
    visibility_changed: bool,
}
struct DeviceState {
    key: String,
    epoch: u64,
    policy: DevicePowerPolicy,
    decision: DecisionState,
    status: DevicePowerStatus,
    controls: HashSet<String>,
}
#[derive(Default)]
struct State {
    devices: HashMap<String, DeviceState>,
    consumed: HashSet<(String, u64, String)>,
    sample: SensorSample,
    sampled_at: Option<Instant>,
}
pub(crate) struct Controller {
    preferences: Mutex<Preferences>,
    state: Mutex<State>,
    pub signal: Arc<sampler::Signal>,
    stopped: AtomicBool,
}
impl DevicePowerStatus {
    // Construct a manual power status that preserves the supplied output unchanged
    fn manual(output: DeviceOutput) -> Self {
        Self {
            enabled: false,
            effective: output,
            automatic_sleeping: false,
            dimmed: false,
            reasons: vec![],
            overridden: vec![],
            unavailable: vec![],
        }
    }
}
// Snapshot connected devices with baseline output and current connection epochs
fn views(runtime: &Runtime) -> Vec<View> {
    let devices = {
        let state = runtime.state.lock().unwrap();
        state
            .devices
            .iter()
            .filter(|device| device.connected)
            .map(|device| (device.clone(), state.session(&device.id).output))
            .collect::<Vec<_>>()
    };
    let epochs = runtime.connection_epochs.lock().unwrap();
    devices
        .into_iter()
        .filter_map(|(device, baseline)| {
            epochs.get(&device.id).map(|epoch| View {
                device,
                baseline,
                epoch: *epoch,
            })
        })
        .collect()
}
// Snapshot one connected device with its baseline output and current connection epoch
fn view(runtime: &Runtime, id: &str) -> Option<View> {
    let (device, baseline) = {
        let state = runtime.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|device| device.id == id && device.connected)?;
        (device.clone(), state.session(id).output)
    };
    let epoch = runtime.connection_epochs.lock().unwrap().get(id).copied()?;
    Some(View {
        device,
        baseline,
        epoch,
    })
}
// Describe supported controller capabilities
fn capabilities(device: &DeviceInfo) -> DevicePowerCapabilities {
    let caps = &device.manifest.capabilities;
    DevicePowerCapabilities {
        platform: native::capabilities(),
        dim: caps.brightness || caps.lighting,
        sleep: caps.sleep,
        wake_input: caps.sleep && !device.manifest.controls.is_empty(),
        wake_description: if device.simulated {
            "Simulator input only; this does not qualify physical wake reports"
        } else {
            "The first received dock interaction is consumed. Asleep firmware input delivery still requires physical qualification"
        }.into(),
    }
}
impl Controller {
    #[cfg(test)]
    // Open the requested controller resource
    pub fn open(path: PathBuf) -> Self {
        Self::with_storage(
            stream_dock_manager_storage::Storage::open(path.parent().unwrap().to_path_buf())
                .unwrap(),
        )
        .unwrap()
    }
    // Construct using storage
    pub fn with_storage(storage: stream_dock_manager_storage::Storage) -> Result<Self, String> {
        Ok(Self {
            preferences: Mutex::new(Preferences::with_storage(storage)?),
            state: Default::default(),
            signal: Default::default(),
            stopped: AtomicBool::new(false),
        })
    }
    // Report the manages condition for controller
    pub fn manages(&self, id: &str) -> bool {
        self.state.lock().unwrap().devices.contains_key(id)
    }
    // Report the suppressed condition for controller
    pub fn suppressed(&self, id: &str, epoch: u64) -> bool {
        self.state
            .lock()
            .unwrap()
            .devices
            .get(id)
            .is_some_and(|entry| entry.epoch == epoch && entry.status.effective.sleeping)
    }
    // Stop the controller operation
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.signal.stop();
    }
    // Use the policy-adjusted output only for the matching connection epoch
    pub fn effective(&self, id: &str, epoch: u64, baseline: &DeviceOutput) -> DeviceOutput {
        self.state
            .lock()
            .unwrap()
            .devices
            .get(id)
            .filter(|entry| entry.epoch == epoch)
            .map(|entry| entry.status.effective.clone())
            .unwrap_or_else(|| baseline.clone())
    }
    // Return the current controller status
    pub fn status(&self, id: &str, epoch: u64, baseline: &DeviceOutput) -> DevicePowerStatus {
        let mut status = self
            .state
            .lock()
            .unwrap()
            .devices
            .get(id)
            .filter(|entry| entry.epoch == epoch)
            .map(|entry| entry.status.clone())
            .unwrap_or_else(|| DevicePowerStatus::manual(baseline.clone()));
        if let Some(error) = &self.preferences.lock().unwrap().failure {
            status.unavailable.push(error.clone());
        }
        status
    }
    // Reconcile controller state with current persisted evidence
    fn reconcile(
        &self,
        views: &[View],
        policies: &BTreeMap<String, DevicePowerPolicy>,
        sample: Option<SensorSample>,
    ) -> Vec<Change> {
        let now = Instant::now();
        let local = chrono::Local::now();
        let mut state = self.state.lock().unwrap();
        if let Some(sample) = sample {
            state.sample = sample;
            state.sampled_at = Some(now);
        }
        let sample = if state
            .sampled_at
            .is_some_and(|at| now.saturating_duration_since(at) <= Duration::from_secs(7))
        {
            state.sample.clone()
        } else {
            SensorSample::default()
        };
        let connected: HashSet<_> = views
            .iter()
            .map(|view| (view.device.id.clone(), view.epoch))
            .collect();
        state.devices.retain(|id, entry| {
            views.iter().any(|view| {
                &view.device.id == id
                    && view.epoch == entry.epoch
                    && view.device.persistence_key == entry.key
            })
        });
        state
            .consumed
            .retain(|(id, epoch, _)| connected.contains(&(id.clone(), *epoch)));
        let mut changes = vec![];
        for view in views {
            let id = &view.device.id;
            let policy = policies
                .get(&view.device.persistence_key)
                .cloned()
                .unwrap_or_default();
            if !policy.enabled && !state.devices.contains_key(id) {
                continue;
            }
            let entry = state
                .devices
                .entry(id.clone())
                .or_insert_with(|| DeviceState {
                    key: view.device.persistence_key.clone(),
                    epoch: view.epoch,
                    policy: policy.clone(),
                    decision: Default::default(),
                    status: DevicePowerStatus::manual(view.baseline.clone()),
                    controls: view
                        .device
                        .manifest
                        .controls
                        .iter()
                        .map(|control| control.id.clone())
                        .collect(),
                });
            if entry.policy != policy {
                entry.decision = Default::default();
                entry.policy = policy.clone();
            }
            let next = entry.decision.evaluate(
                &policy,
                &view.baseline,
                &view.device.manifest.capabilities,
                &sample,
                local.weekday().num_days_from_monday() as u8,
                (local.hour() * 60 + local.minute()) as u16,
                now,
            );
            if entry.status != next {
                let entering_sleep = !entry.status.effective.sleeping && next.effective.sleeping;
                let output_changed = entry.status.effective != next.effective;
                let visibility_changed = entry.status.effective.sleeping != next.effective.sleeping;
                entry.status = next;
                changes.push(Change {
                    id: id.clone(),
                    epoch: view.epoch,
                    entering_sleep,
                    output_changed,
                    visibility_changed,
                });
            }
            if !policy.enabled {
                state.devices.remove(id);
            }
        }
        changes
    }
    // Wake the controller worker
    fn wake(&self, id: &str, epoch: u64) {
        if let Some(entry) = self
            .state
            .lock()
            .unwrap()
            .devices
            .get_mut(id)
            .filter(|entry| entry.epoch == epoch)
        {
            entry.decision.wake(Instant::now());
        }
    }
    // Retain ownership of the entire waking gesture, including its release, even
    // if a policy is disabled or a different condition clears during that gesture
    fn input(&self, id: &str, epoch: u64, event: &ControlEvent) -> (bool, bool) {
        let mut state = self.state.lock().unwrap();
        let key = (id.into(), epoch, event.control_id.clone());
        if state.consumed.contains(&key) {
            if matches!(&event.input, Input::Button { pressed: false })
                || matches!(&event.input,Input::Touch{phase,..} if matches!(phase.as_str(),"up"|"end"|"cancel"|"tap"))
            {
                state.consumed.remove(&key);
            }
            return (true, false);
        }
        let Some(entry) = state
            .devices
            .get_mut(id)
            .filter(|entry| entry.epoch == epoch && entry.controls.contains(&event.control_id))
        else {
            return (false, false);
        };
        if !entry.policy.enabled {
            return (false, false);
        }
        if entry.status.effective.sleeping && !entry.status.automatic_sleeping {
            return (false, false);
        } // Manual sleep stays manual
        let positive = super::positive(&event.input);
        let waking = entry.status.automatic_sleeping && positive;
        let consumed = entry.status.automatic_sleeping;
        if positive {
            if waking {
                entry.decision.wake(Instant::now());
            } else {
                entry.decision.interaction(Instant::now());
            }
        }
        if waking
            && (matches!(&event.input, Input::Button { pressed: true })
                || matches!(&event.input,Input::Touch{phase,..} if !matches!(phase.as_str(),"tap"|"end"|"up"|"cancel")))
        {
            state.consumed.insert(key);
        }
        (consumed, positive)
    }
}
// Apply the reviewed controller change
fn apply(runtime: &Runtime, changes: Vec<Change>) {
    let mut visibility_changed = false;
    for change in changes {
        let epochs = runtime.connection_epochs.lock().unwrap();
        if epochs.get(&change.id) != Some(&change.epoch) {
            continue;
        }
        // Run checks take the same epoch -> cancellation order. Native input
        // release does not acquire runtime/epoch locks. Reconnect cannot publish
        // a new epoch until cleanup of this exact connection is finished
        if change.entering_sleep {
            runtime.builtin_state.cancel_device(&change.id);
        }
        if change.output_changed {
            let _ = runtime
                .device_commands
                .send(crate::devices::Request::RestoreDevice(
                    change.id.clone(),
                    change.epoch,
                ));
        }
        drop(epochs);
        visibility_changed |= change.visibility_changed;
        runtime.emit("device-power", Some(change.id), None, None);
    }
    if visibility_changed {
        crate::devices::sync_plugins(runtime);
    }
}

impl Runtime {
    // Read power-policy status for a currently connected device
    pub fn device_power_status(&self, device_id: &str) -> Result<DevicePowerStatus, String> {
        let view = view(self, device_id).ok_or("Device is not connected")?;
        Ok(self.power.status(device_id, view.epoch, &view.baseline))
    }
    // Reload device power preferences and return their revision, capabilities and effective status
    pub fn device_power_settings(&self, device_id: &str) -> Result<DevicePowerSettings, String> {
        let view = view(self, device_id).ok_or("Device is not connected")?;
        let mut prefs = self.power.preferences.lock().unwrap();
        let loaded = prefs.reload();
        let (revision, policy) = prefs.value(&view.device.persistence_key);
        drop(prefs);
        self.power.signal.notify();
        loaded?;
        Ok(DevicePowerSettings {
            device_id: device_id.into(),
            device_key: view.device.persistence_key.clone(),
            revision,
            policy,
            capabilities: capabilities(&view.device),
            status: self.power.status(device_id, view.epoch, &view.baseline),
        })
    }
    // Save device power policy
    pub fn save_device_power_policy(
        &self,
        device_id: &str,
        device_key: &str,
        revision: u64,
        policy: DevicePowerPolicy,
    ) -> Result<DevicePowerSettings, String> {
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|device| {
                device.id == device_id && device.connected && device.persistence_key == device_key
            })
            .ok_or("Device changed; reopen its power policy")?;
        let caps = capabilities(device);
        if policy.enabled {
            if policy.idle_action != PowerAction::Off && !caps.platform.idle {
                return Err(caps.platform.idle_description);
            }
            if policy.lock_action != PowerAction::Off && !caps.platform.lock {
                return Err(caps.platform.lock_description);
            }
            for action in [
                policy.idle_action,
                policy.lock_action,
                policy.schedule_action,
            ] {
                if action == PowerAction::Sleep && !caps.sleep {
                    return Err("Device sleep is unavailable".into());
                }
                if action == PowerAction::Dim && !caps.dim {
                    return Err("Device dimming is unavailable".into());
                }
            }
        }
        self.power
            .preferences
            .lock()
            .unwrap()
            .save(device_key, revision, policy)?;
        drop(state);
        self.refresh_power();
        self.power.signal.notify();
        self.device_power_settings(device_id)
    }
    // Refresh power
    pub(crate) fn refresh_power(&self) {
        let policies = self.power.preferences.lock().unwrap().entries();
        let current = views(self);
        apply(self, self.power.reconcile(&current, &policies, None));
    }
    // Wake the current device connection and request power-policy reevaluation
    pub(crate) fn power_wake(&self, id: &str) {
        if let Some(epoch) = self.connection_epochs.lock().unwrap().get(id).copied() {
            self.power.wake(id, epoch);
        }
        self.refresh_power();
        self.power.signal.notify();
    }
    // Report the power input condition for controller
    pub(crate) fn power_input(&self, id: &str, epoch: u64, event: &ControlEvent) -> bool {
        let (consumed, changed) = self.power.input(id, epoch, event);
        if changed {
            self.refresh_power();
            self.power.signal.notify();
        }
        consumed
    }
    // Resolve effective device output
    pub fn effective_device_output(&self, id: &str) -> Result<DeviceOutput, String> {
        let view = view(self, id).ok_or("Device is not connected")?;
        Ok(self.power.effective(id, view.epoch, &view.baseline))
    }
}
// Start the controller operation
pub(crate) fn start(runtime: Weak<Runtime>) {
    let _ = start_with(runtime, Arc::new(native::sample));
}
type Sample = dyn Fn(bool, bool) -> SensorSample + Send + Sync;
// Start with
pub(crate) fn start_with(
    runtime: Weak<Runtime>,
    sample: Arc<Sample>,
) -> Option<std::thread::JoinHandle<()>> {
    let owner = runtime.upgrade()?;
    let signal = owner.power.signal.clone();
    drop(owner);
    Some(std::thread::spawn(move || {
        let mut observed = 0;
        loop {
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.is_stopping() || runtime.power.stopped.load(Ordering::SeqCst) {
                break;
            }
            let policies = runtime.power.preferences.lock().unwrap().entries();
            let current = views(&runtime);
            let needed: Vec<_> = current
                .iter()
                .filter_map(|view| {
                    policies
                        .get(&view.device.persistence_key)
                        .filter(|policy| policy.enabled)
                })
                .collect();
            let idle = needed
                .iter()
                .any(|policy| policy.idle_action != PowerAction::Off);
            let lock = needed
                .iter()
                .any(|policy| policy.lock_action != PowerAction::Off);
            let readings = (idle || lock).then(|| sample(idle, lock));
            if runtime.is_stopping() || runtime.power.stopped.load(Ordering::SeqCst) {
                break;
            }
            // Refresh identities and preferences after bounded native work; a late
            // sample cannot restore a disconnected or newly replaced connection
            let latest = views(&runtime);
            let policies = runtime.power.preferences.lock().unwrap().entries();
            apply(
                &runtime,
                runtime.power.reconcile(&latest, &policies, readings),
            );
            let timeout = (!needed.is_empty()).then_some(Duration::from_secs(1));
            drop(runtime);
            if !signal.wait(&mut observed, timeout) {
                break;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct an isolated controller fixture
    fn fixture() -> (
        tempfile::TempDir,
        Controller,
        View,
        BTreeMap<String, DevicePowerPolicy>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let owner = Controller::open(root.path().join("power.json"));
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|model| model.model_id == "n4-pro")
            .unwrap();
        let view = View {
            device: DeviceInfo {
                id: "simulator:power".into(),
                persistence_key: "saved-power".into(),
                identity_port_dependent: false,
                model_id: manifest.model_id.clone(),
                name: manifest.name.clone(),
                variant_id: "simulated".into(),
                firmware: "simulated".into(),
                usb_release: None,
                connected: true,
                simulated: true,
                manifest,
            },
            baseline: DeviceOutput::default(),
            epoch: 7,
        };
        let policy = DevicePowerPolicy {
            enabled: true,
            schedule_action: PowerAction::Sleep,
            schedule: crate::automation::Schedule {
                days: vec![0, 1, 2, 3, 4, 5, 6],
                start: 0,
                end: 0,
            },
            ..Default::default()
        };
        let policies = BTreeMap::from([(view.device.persistence_key.clone(), policy)]);
        (root, owner, view, policies)
    }
    // Construct a key-1 press or release event for the test
    fn button(pressed: bool) -> ControlEvent {
        ControlEvent {
            control_id: "key-1".into(),
            input: Input::Button { pressed },
        }
    }
    #[test]
    // Verify that waking press and release remain consumed when the policy is disabled mid gesture
    fn waking_press_and_release_remain_consumed_when_the_policy_is_disabled_mid_gesture() {
        let (_root, owner, view, policies) = fixture();
        owner.reconcile(std::slice::from_ref(&view), &policies, None);
        assert!(
            owner
                .status(&view.device.id, view.epoch, &view.baseline)
                .automatic_sleeping
        );
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(false)),
            (true, false)
        );
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(true)),
            (true, true)
        );
        owner.reconcile(std::slice::from_ref(&view), &BTreeMap::new(), None);
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(true)),
            (true, false)
        );
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(false)),
            (true, false)
        );
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(true)),
            (false, false)
        );
    }
    #[test]
    // Verify that reconnect and other controls cannot inherit a consumed gesture
    fn reconnect_and_other_controls_cannot_inherit_a_consumed_gesture() {
        let (_root, owner, mut view, policies) = fixture();
        owner.reconcile(std::slice::from_ref(&view), &policies, None);
        owner.input(&view.device.id, view.epoch, &button(true));
        let other = ControlEvent {
            control_id: "key-2".into(),
            input: Input::Pulse,
        };
        owner.reconcile(std::slice::from_ref(&view), &policies, None);
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &other),
            (false, true)
        );
        view.epoch += 1;
        owner.reconcile(std::slice::from_ref(&view), &BTreeMap::new(), None);
        assert_eq!(
            owner.input(&view.device.id, view.epoch, &button(true)),
            (false, false)
        );
        assert!(owner.state.lock().unwrap().consumed.is_empty());
    }
    #[test]
    // Verify that native changes are scoped to output transitions and same epoch identity
    fn native_changes_are_scoped_to_output_transitions_and_same_epoch_identity() {
        let (_root, owner, mut view, policies) = fixture();
        let changes = owner.reconcile(std::slice::from_ref(&view), &policies, None);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].entering_sleep);
        assert!(changes[0].visibility_changed);
        assert!(owner
            .reconcile(std::slice::from_ref(&view), &policies, None)
            .is_empty());
        view.device.persistence_key = "new-device".into();
        owner.reconcile(std::slice::from_ref(&view), &BTreeMap::new(), None);
        assert!(
            !owner
                .status(&view.device.id, view.epoch, &view.baseline)
                .enabled
        );
    }

    #[test]
    // Verify that touch gestures dials and pulses consume only the waking interaction
    fn touch_gestures_dials_and_pulses_consume_only_the_waking_interaction() {
        for input in [
            Input::Rotate { delta: 1 },
            Input::Pulse,
            Input::Touch {
                phase: "begin".into(),
                x: 1,
                y: 2,
            },
        ] {
            let (_root, owner, view, policies) = fixture();
            owner.reconcile(std::slice::from_ref(&view), &policies, None);
            let event = ControlEvent {
                control_id: "key-1".into(),
                input: input.clone(),
            };
            assert_eq!(
                owner.input(&view.device.id, view.epoch, &event),
                (true, true)
            );
            owner.reconcile(std::slice::from_ref(&view), &policies, None);
            if matches!(input, Input::Touch { .. }) {
                for phase in ["move", "end"] {
                    let event = ControlEvent {
                        control_id: "key-1".into(),
                        input: Input::Touch {
                            phase: phase.into(),
                            x: 2,
                            y: 3,
                        },
                    };
                    assert!(owner.input(&view.device.id, view.epoch, &event).0);
                }
            }
            assert_eq!(
                owner.input(&view.device.id, view.epoch, &button(true)),
                (false, true)
            );
        }
    }
    #[test]
    #[ignore = "Disposable loopback host; controlled sensors only"]
    // Verify that default worker is idle and stop discards a late native sample
    fn default_worker_is_idle_and_stop_discards_a_late_native_sample() {
        use std::sync::{atomic::AtomicUsize, mpsc};
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:power-worker";
        runtime.simulate_device(true, id).unwrap();
        let key = runtime.snapshot().devices[0].persistence_key.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let (started_tx, started) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let resumed = Mutex::new(resumed);
        let worker = start_with(
            Arc::downgrade(&runtime),
            Arc::new(move |idle, lock| {
                assert!(idle && !lock);
                counted.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                resumed.lock().unwrap().recv().unwrap();
                SensorSample {
                    idle: Some(Duration::from_secs(3600)),
                    locked: None,
                }
            }),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        runtime
            .power
            .preferences
            .lock()
            .unwrap()
            .save(
                &key,
                0,
                DevicePowerPolicy {
                    enabled: true,
                    idle_action: PowerAction::Sleep,
                    ..Default::default()
                },
            )
            .unwrap();
        runtime.power.signal.notify();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        runtime.power.stop();
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(!runtime.device_power_status(id).unwrap().effective.sleeping);
        let weak = Arc::downgrade(&runtime);
        runtime.shutdown();
        drop(runtime);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    #[ignore = "Disposable authenticated SDK sockets; no plugin executable or HID"]
    // Verify that sleep hides only its contexts without stopping profile plugin demand
    fn sleep_hides_only_its_contexts_without_stopping_profile_plugin_demand() {
        use serde_json::{json, Value};
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        std::fs::write(package.join("manifest.json"),json!({"UUID":"fixture.power","Name":"Power fixture","Version":"0.1.0","CodePath":"runtime.html","Actions":[{"UUID":"fixture.power.action","Name":"Action"}]}).to_string()).unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let first = "simulator:n4-pro:power-first";
        let second = "simulator:n4-pro:power-second";
        runtime.simulate_device(true, first).unwrap();
        runtime.simulate_device(true, second).unwrap();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().action =
            crate::actions::Action::Plugin {
                user_desired_state: None,
                plugin_id: "fixture.power".into(),
                action_id: "fixture.power.action".into(),
                settings: json!({}),
                resources: Default::default(),
            };
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("fixture.power", true)
            .unwrap();
        crate::test_support::synchronize_plugins(&runtime);
        let url = {
            let mut plugins = runtime.plugins.lock().unwrap();
            plugins.poll();
            plugins.browser_view("fixture.power").unwrap().unwrap().url
        };
        // The disposable runtime has no device/poll worker; publish visibility
        // again after poll has moved this fixture from queued to starting
        crate::test_support::synchronize_plugins(&runtime);
        let parts: Vec<_> = url.split('/').collect();
        let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
        }
        socket
            .send(tungstenite::Message::Text(
                json!({"event":"registerPlugin","uuid":parts[4]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        let next = |socket: &mut tungstenite::WebSocket<
            tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
        >,
                    kind: &str|
         -> Value {
            loop {
                let message = socket.read().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let value: Value = serde_json::from_str(text).unwrap();
                if value["event"] == kind {
                    return value;
                }
            }
        };
        let a = next(&mut socket, "willAppear");
        let b = next(&mut socket, "willAppear");
        assert_ne!(a["device"], b["device"]);
        let before = runtime.plugins.lock().unwrap().list()[0].runtime.clone();
        assert_eq!(before.demand.len(), 2);
        let key = runtime
            .snapshot()
            .devices
            .iter()
            .find(|device| device.id == first)
            .unwrap()
            .persistence_key
            .clone();
        runtime
            .save_device_power_policy(
                first,
                &key,
                0,
                DevicePowerPolicy {
                    enabled: true,
                    schedule_action: PowerAction::Sleep,
                    schedule: crate::automation::Schedule {
                        days: vec![0, 1, 2, 3, 4, 5, 6],
                        start: 0,
                        end: 0,
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        let hidden = next(&mut socket, "willDisappear");
        assert_eq!(hidden["device"], first);
        let asleep = runtime.plugins.lock().unwrap().list()[0].runtime.clone();
        assert_eq!(asleep.demand.len(), 2);
        assert_eq!(before.run_id, asleep.run_id);
        assert!(
            !runtime
                .device_power_status(second)
                .unwrap()
                .effective
                .sleeping
        );
        runtime.set_device_sleep(first, false).unwrap();
        assert_eq!(next(&mut socket, "willAppear")["device"], first);
        assert_eq!(
            before.run_id,
            runtime.plugins.lock().unwrap().list()[0].runtime.run_id
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Disposable loopback host and marker file only; no native input or HID"]
    #[cfg(unix)]
    // Verify that native saved policy preserves baseline history and waking input cannot execute a binding
    fn native_saved_policy_preserves_baseline_history_and_waking_input_cannot_execute_a_binding() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:power-native";
        runtime.simulate_device(true, id).unwrap();
        let snapshot = runtime.snapshot();
        let key = snapshot.devices[0].persistence_key.clone();
        let marker = root.path().join("pressed.txt");
        let mut profile = snapshot.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().action =
            crate::actions::Action::Command {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "printf pressed > \"$1\"".into(),
                    "fixture".into(),
                    marker.to_string_lossy().into(),
                ],
            };
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let before = runtime.snapshot();
        let policy = DevicePowerPolicy {
            enabled: true,
            schedule_action: PowerAction::Sleep,
            schedule: crate::automation::Schedule {
                days: vec![0, 1, 2, 3, 4, 5, 6],
                start: 0,
                end: 0,
            },
            ..Default::default()
        };
        runtime
            .save_device_power_policy(id, &key, 0, policy)
            .unwrap();
        assert!(runtime.device_power_status(id).unwrap().automatic_sleeping);
        runtime.simulate_input(id, button(true)).unwrap();
        runtime.simulate_input(id, button(false)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists());
        assert!(!runtime.device_power_status(id).unwrap().effective.sleeping);
        runtime.simulate_input(id, button(true)).unwrap();
        runtime.simulate_input(id, button(false)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let after = runtime.snapshot();
        assert_eq!(before.configuration_revision, after.configuration_revision);
        assert_eq!(before.session(id).output, after.session(id).output);
        assert_eq!(before.history.undo.len(), after.history.undo.len());
        runtime.set_device_sleep(id, true).unwrap();
        runtime.refresh_power();
        assert!(runtime.device_power_status(id).unwrap().effective.sleeping);
        assert!(runtime.simulate_input(id, button(true)).is_err());
        let weak = Arc::downgrade(&runtime);
        runtime.shutdown();
        drop(runtime);
        let deadline = Instant::now() + Duration::from_secs(3);
        while weak.upgrade().is_some() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let restored = Runtime::build(root.path().into(), false).unwrap();
        restored.simulate_device(true, id).unwrap();
        assert!(restored.device_power_settings(id).unwrap().policy.enabled);
        assert!(restored.snapshot().session(id).output.sleeping);
        restored.shutdown();
    }
}
