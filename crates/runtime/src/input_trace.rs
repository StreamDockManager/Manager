//! One connection-scoped physical input observation. Traces never enter event history.
use crate::Runtime;
use hardware_core::{ControlEvent, Input};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS, schemars::JsonSchema)]
pub struct InputTrace {
    pub token: String,
    pub device_id: String,
    pub control: String,
    pub steps: Vec<String>,
    pub outcome: Option<String>,
}

#[derive(Clone)]
pub(crate) struct InputOrigin {
    pub token: std::sync::Arc<str>,
    pub received: Instant,
}
struct Capture {
    awaiting_display: bool,
    received: Option<Instant>,
    view: InputTrace,
    epoch: u64,
    deadline: Instant,
    awaiting_release: bool,
    receipts: Vec<stream_dock_manager_plugin_host::ActionReceipt>,
    run_outcome: Option<String>,
    plugin_failed: bool,
}
#[derive(Default)]
pub(crate) struct Tracer {
    listening: AtomicBool,
    capture: Mutex<Option<Capture>>,
    watchdog: AtomicBool,
}

impl Tracer {
    /// Regular device dispatch returns immediately. Only an armed observer retains completion receipts.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_plugin(
        &self,
        host: &mut stream_dock_manager_plugin_host::PluginHost,
        token: Option<&str>,
        activation: Option<&str>,
        plugin: &str,
        action: &str,
        device: &str,
        control: &str,
        scope: &str,
        input: &Input,
        settings: &serde_json::Value,
        desired: Option<u8>,
    ) -> Result<(), String> {
        let Some(token) = token else {
            return host.dispatch_with_activation(
                plugin, action, device, control, scope, input, settings, desired, activation,
            );
        };
        let receipt = host.dispatch_tracked_with_activation(
            plugin, action, device, control, scope, input, settings, desired, activation,
        )?;
        let mut state = self.capture.lock().unwrap();
        if let Some(c) = state
            .as_mut()
            .filter(|c| c.view.token == token && c.view.outcome.is_none())
        {
            let message = if !receipt.completion_supported() {
                "Plugin acknowledged dispatch only"
            } else if c.receipts.len() >= 64 {
                "Plugin dispatched; completion observation limit reached"
            } else {
                c.receipts.push(receipt);
                "Plugin dispatched; awaiting completion"
            };
            if c.view.steps.len() < 64 {
                c.view.steps.push(message.into());
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn ingress(&self, device: &str, epoch: u64, event: &ControlEvent) -> Option<String> {
        self.ingress_at(device, epoch, event, Instant::now())
    }
    pub(crate) fn origin(
        &self,
        token: Option<&str>,
        device: &str,
        epoch: u64,
    ) -> Option<InputOrigin> {
        let token = token?;
        let mut state = self.capture.lock().unwrap();
        let c = state.as_mut().filter(|c| {
            c.view.token == token
                && c.epoch == epoch
                && c.view.device_id == device
                && Instant::now() < c.deadline
        })?;
        let received = c.received?;
        c.awaiting_display = true;
        Some(InputOrigin {
            token: token.into(),
            received,
        })
    }
    pub(crate) fn wrote(
        &self,
        origin: &InputOrigin,
        epoch: u64,
        outcome: &hardware_core::OutputOutcome,
        elapsed_ms: f64,
    ) {
        if let Some(c) = self
            .capture
            .lock()
            .unwrap()
            .as_mut()
            .filter(|c| c.view.token == origin.token.as_ref() && c.view.outcome.is_none())
        {
            c.awaiting_display = false;
            if c.view.steps.len() >= 64 {
                return;
            }
            c.view.steps.push(format!(
                "{} write completed in {elapsed_ms:.2} ms (epoch {epoch}, scene {}, revision {})",
                outcome.surface_id, outcome.scene_revision, outcome.content_revision
            ));
        }
    }
    pub(crate) fn ingress_at(
        &self,
        device: &str,
        epoch: u64,
        event: &ControlEvent,
        received: Instant,
    ) -> Option<String> {
        if !self.listening.load(Ordering::Acquire) {
            return None;
        }
        let mut state = self.capture.lock().unwrap();
        let c = state.as_mut()?;
        if c.view.device_id != device
            || c.epoch != epoch
            || c.view.outcome.is_some()
            || Instant::now() >= c.deadline
        {
            return None;
        }
        if !c.view.control.is_empty() {
            if c.awaiting_release
                && c.view.control == event.control_id
                && matches!(event.input, Input::Button { pressed: false })
            {
                c.awaiting_release = false;
                if c.view.steps.len() < 64 {
                    c.view.steps.push("Physical release".into());
                }
            }
            return None;
        }
        let gesture = match &event.input {
            Input::Button { pressed: true } => "Press",
            Input::Button { pressed: false } => "Release",
            Input::Pulse => "Pulse",
            Input::Rotate { delta } if *delta < 0 => "Turn left",
            Input::Rotate { .. } => "Turn right",
            Input::Touch { phase, .. } if phase == "end" => "Touch end",
            Input::Touch { .. } => "Touch contact",
            Input::Swipe { .. } => "Swipe",
        };
        c.awaiting_release = matches!(event.input, Input::Button { pressed: true });
        c.received = Some(received);
        c.view.control = event.control_id.chars().take(128).collect();
        c.view.steps.push(gesture.into());
        Some(c.view.token.clone())
    }

    pub(crate) fn record(&self, token: Option<&str>, message: &str, terminal: bool) {
        let Some(token) = token else { return };
        let mut state = self.capture.lock().unwrap();
        let Some(c) = state
            .as_mut()
            .filter(|c| c.view.token == token && c.view.outcome.is_none())
        else {
            return;
        };
        if Instant::now() >= c.deadline {
            c.view.outcome = Some("Trace timed out".into());
            c.receipts.clear();
            self.listening.store(false, Ordering::Release);
            return;
        }
        if terminal {
            c.run_outcome = Some(message.chars().take(256).collect());
            if c.receipts.is_empty() && !c.awaiting_display {
                c.view.outcome = if c.plugin_failed {
                    Some("Plugin completion failed".into())
                } else {
                    c.run_outcome.take()
                };
                self.listening.store(false, Ordering::Release);
            }
        } else if c.view.steps.len() < 64 {
            c.view.steps.push(message.chars().take(256).collect());
        }
    }
}
impl Runtime {
    pub fn arm_input_trace(
        self: &std::sync::Arc<Self>,
        device: &str,
    ) -> Result<InputTrace, String> {
        let epochs = self.connection_epochs.lock().unwrap();
        let epoch = *epochs
            .get(device)
            .ok_or("Connect the selected physical device.")?;
        if !self
            .state
            .lock()
            .unwrap()
            .devices
            .iter()
            .any(|d| d.id == device && d.connected && !d.simulated)
        {
            return Err("Connect the selected physical device.".into());
        }
        let view = InputTrace {
            token: uuid::Uuid::new_v4().to_string(),
            device_id: device.into(),
            control: String::new(),
            steps: vec![],
            outcome: None,
        };
        let mut state = self.input_trace.capture.lock().unwrap();
        *state = Some(Capture {
            received: None,
            awaiting_display: false,
            view: view.clone(),
            epoch,
            deadline: Instant::now() + Duration::from_secs(30),
            awaiting_release: false,
            receipts: Vec::new(),
            run_outcome: None,
            plugin_failed: false,
        });
        self.input_trace.listening.store(true, Ordering::Release);
        if !self.input_trace.watchdog.swap(true, Ordering::AcqRel) {
            let weak = std::sync::Arc::downgrade(self);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(runtime) = weak.upgrade() else { break };
                let token = {
                    let state = runtime.input_trace.capture.lock().unwrap();
                    match state.as_ref().filter(|c| c.view.outcome.is_none()) {
                        Some(c) => c.view.token.clone(),
                        None => {
                            runtime.input_trace.watchdog.store(false, Ordering::Release);
                            break;
                        }
                    }
                };
                let _ = runtime.read_input_trace(&token);
            });
        }
        Ok(view)
    }

    pub fn read_input_trace(&self, token: &str) -> Result<InputTrace, String> {
        let epochs = self.connection_epochs.lock().unwrap();
        let mut state = self.input_trace.capture.lock().unwrap();
        let capture = state
            .as_mut()
            .filter(|c| c.view.token == token)
            .ok_or("Input trace was dismissed or replaced.")?;
        if capture.view.outcome.is_none() {
            if epochs.get(&capture.view.device_id) != Some(&capture.epoch) {
                capture.view.outcome = Some("Device disconnected".into());
            } else if Instant::now() >= capture.deadline {
                capture.view.outcome = Some("Trace timed out".into());
            }
            if capture.view.outcome.is_none() {
                capture.receipts.retain(|receipt| {
                    if let Some(ok) = receipt.try_completion() {
                        capture.plugin_failed |= !ok;
                        if capture.view.steps.len() < 64 {
                            capture.view.steps.push(
                                if ok {
                                    "Plugin completed"
                                } else {
                                    "Plugin completion failed"
                                }
                                .into(),
                            );
                        }
                        false
                    } else {
                        true
                    }
                });
                if capture.receipts.is_empty()
                    && !capture.awaiting_display
                    && capture.run_outcome.is_some()
                {
                    capture.view.outcome = if capture.plugin_failed {
                        Some("Plugin completion failed".into())
                    } else {
                        capture.run_outcome.take()
                    };
                }
            }
            if capture.view.outcome.is_some() {
                capture.receipts.clear();
                self.input_trace.listening.store(false, Ordering::Release);
            }
        }
        Ok(capture.view.clone())
    }
    pub fn dismiss_input_trace(&self, token: &str) {
        let mut state = self.input_trace.capture.lock().unwrap();
        if state.as_ref().is_some_and(|c| c.view.token == token) {
            *state = None;
            self.input_trace.listening.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_and_wrong_connection_events_cannot_claim_a_new_capture() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(dir.path().into(), false).unwrap();
        let id = "simulator:trace-lifetime";
        runtime.simulate_device(true, id).unwrap();
        assert!(runtime.arm_input_trace(id).is_err());
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let old = runtime.arm_input_trace(id).unwrap();
        let new = runtime.arm_input_trace(id).unwrap();
        runtime.dismiss_input_trace(&old.token);
        let event = ControlEvent {
            control_id: "key-2".into(),
            input: Input::Button { pressed: true },
        };
        let epoch = runtime.connection_epochs.lock().unwrap()[id];
        assert!(runtime.input_trace.ingress(id, epoch + 1, &event).is_none());
        assert_eq!(
            runtime.input_trace.ingress(id, epoch, &event),
            Some(new.token.clone())
        );
        runtime.input_trace.record(Some(&old.token), "stale", true);
        for _ in 0..100 {
            runtime
                .input_trace
                .record(Some(&new.token), "Observed", false);
        }
        assert_eq!(
            runtime.read_input_trace(&new.token).unwrap().steps.len(),
            64
        );
        runtime
            .input_trace
            .capture
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .deadline = Instant::now();
        runtime.input_trace.record(Some(&new.token), "late", true);
        assert_eq!(
            runtime
                .read_input_trace(&new.token)
                .unwrap()
                .outcome
                .as_deref(),
            Some("Trace timed out")
        );
        assert!(!runtime.input_trace.listening.load(Ordering::Acquire));
        let fresh = runtime.arm_input_trace(id).unwrap();
        runtime.connection_epochs.lock().unwrap().remove(id);
        assert_eq!(
            runtime
                .read_input_trace(&fresh.token)
                .unwrap()
                .outcome
                .as_deref(),
            Some("Device disconnected")
        );
        runtime.dismiss_input_trace(&fresh.token);
        assert!(runtime.input_trace.capture.lock().unwrap().is_none());
        runtime.shutdown();
    }
}
