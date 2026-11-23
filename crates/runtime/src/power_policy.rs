//! Optional local device suppression, derived from the saved manual baseline
//! Policy decisions never write the baseline or enter workspace history/exports
use crate::{automation::Schedule, workspace::DeviceOutput};
use hardware_core::{DeviceCapabilities, Input};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

mod controller;
mod native;
mod preferences;
pub(crate) use controller::{start, Controller};
pub use controller::{DevicePowerCapabilities, DevicePowerSettings};
pub use native::PowerPlatformCapabilities;
pub(crate) use preferences::validate_storage_snapshot;

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) fn session_locked() -> Option<bool> {
    native::sample(false, true).locked
}

const IDLE: u8 = 1;
const LOCK: u8 = 2;
const SCHEDULE: u8 = 4;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    #[default]
    Off,
    Dim,
    Sleep,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DevicePowerPolicy {
    pub enabled: bool,
    pub idle_action: PowerAction,
    pub idle_minutes: u16,
    pub lock_action: PowerAction,
    pub schedule_action: PowerAction,
    pub schedule: Schedule,
    pub dim_brightness: u8,
}
impl Default for DevicePowerPolicy {
    // Construct the default power policy value
    fn default() -> Self {
        Self {
            enabled: false,
            idle_action: PowerAction::Off,
            idle_minutes: 10,
            lock_action: PowerAction::Off,
            schedule_action: PowerAction::Off,
            schedule: Schedule {
                days: vec![0, 1, 2, 3, 4, 5, 6],
                start: 22 * 60,
                end: 7 * 60,
            },
            dim_brightness: 20,
        }
    }
}
impl DevicePowerPolicy {
    // Validate the supplied power policy value
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1440).contains(&self.idle_minutes) || self.dim_brightness > 100 {
            return Err(
                "Choose an idle delay of 1–1,440 minutes and dim brightness of 0–100%".into(),
            );
        }
        if self.schedule.start >= 1440
            || self.schedule.end >= 1440
            || self.schedule.days.len() > 7
            || self.schedule.days.iter().any(|day| *day > 6)
            || (self.schedule_action != PowerAction::Off && self.schedule.days.is_empty())
        {
            return Err("Choose valid schedule times and at least one scheduled day".into());
        }
        let mut days = self.schedule.days.clone();
        days.sort_unstable();
        days.dedup();
        if days.len() != self.schedule.days.len() {
            return Err("Schedule days must be unique".into());
        }
        if self.enabled
            && self.idle_action == PowerAction::Off
            && self.lock_action == PowerAction::Off
            && self.schedule_action == PowerAction::Off
        {
            return Err("Choose at least one automatic dim or sleep rule".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DevicePowerStatus {
    pub enabled: bool,
    pub effective: DeviceOutput,
    pub automatic_sleeping: bool,
    pub dimmed: bool,
    pub reasons: Vec<String>,
    pub overridden: Vec<String>,
    pub unavailable: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct SensorSample {
    idle: Option<Duration>,
    locked: Option<bool>,
}
#[derive(Default)]
pub(crate) struct DecisionState {
    active: u8,
    overrides: u8,
    last_interaction: Option<Instant>,
}
impl DecisionState {
    // Wake the power policy worker
    fn wake(&mut self, now: Instant) {
        self.overrides |= self.active;
        self.last_interaction = Some(now);
    }
    // Record the latest device interaction time for idle-policy evaluation
    fn interaction(&mut self, now: Instant) {
        self.last_interaction = Some(now);
    }
    // Keep the explicit sensor/clock inputs together for deterministic transition tests
    #[allow(clippy::too_many_arguments)]
    // Evaluate the power policy rules
    fn evaluate(
        &mut self,
        policy: &DevicePowerPolicy,
        baseline: &DeviceOutput,
        capabilities: &DeviceCapabilities,
        sample: &SensorSample,
        weekday: u8,
        minute: u16,
        now: Instant,
    ) -> DevicePowerStatus {
        let mut result = DevicePowerStatus {
            enabled: policy.enabled,
            effective: baseline.clone(),
            automatic_sleeping: false,
            dimmed: false,
            reasons: vec![],
            overridden: vec![],
            unavailable: vec![],
        };
        if !policy.enabled {
            self.active = 0;
            self.overrides = 0;
            return result;
        }
        let idle = sample.idle.map(|idle| {
            self.last_interaction
                .map_or(idle, |at| idle.min(now.saturating_duration_since(at)))
        });
        let mut active = 0;
        if policy.idle_action != PowerAction::Off {
            match idle {
                Some(idle) if idle >= Duration::from_secs(u64::from(policy.idle_minutes) * 60) => {
                    active |= IDLE
                }
                Some(_) => {}
                None => {
                    active |= self.active & IDLE;
                    result.unavailable.push("System input idle is unavailable; an existing suppression is retained until it can be checked".into());
                }
            }
        }
        if policy.lock_action != PowerAction::Off {
            match sample.locked {
                Some(true) => active |= LOCK,
                Some(false) => {}
                None => {
                    active |= self.active & LOCK;
                    result.unavailable.push("Session lock state is unavailable; an existing suppression is retained until it can be checked".into());
                }
            }
        }
        if policy.schedule_action != PowerAction::Off && policy.schedule.matches(weekday, minute) {
            active |= SCHEDULE;
        }
        self.active = active;
        self.overrides &= active; // A cleared condition can suppress again on its next activation
        let mut sleep = false;
        let mut dim = false;
        for (bit, action, label) in [
            (IDLE, policy.idle_action, "Idle"),
            (LOCK, policy.lock_action, "Session locked"),
            (SCHEDULE, policy.schedule_action, "Schedule"),
        ] {
            if active & bit == 0 {
                continue;
            }
            if self.overrides & bit != 0 {
                result.overridden.push(label.into());
                continue;
            }
            match action {
                PowerAction::Off => {}
                PowerAction::Sleep if capabilities.sleep => {
                    sleep = true;
                    result.reasons.push(label.into());
                }
                PowerAction::Dim if capabilities.brightness || capabilities.lighting => {
                    dim = true;
                    result.reasons.push(label.into());
                }
                _ => result.unavailable.push(format!(
                    "{label}: this device does not support the selected output action"
                )),
            }
        }
        result.automatic_sleeping = sleep && !baseline.sleeping;
        result.effective.sleeping |= sleep;
        if dim && !result.effective.sleeping {
            if capabilities.brightness {
                result.effective.brightness = baseline.brightness.min(policy.dim_brightness);
            }
            if capabilities.lighting {
                result.effective.lighting_brightness =
                    baseline.lighting_brightness.min(policy.dim_brightness);
            }
            result.dimmed = result.effective != *baseline;
        }
        result
    }
}
// Report the positive condition for power policy
pub(crate) fn positive(input: &Input) -> bool {
    match input {
        Input::Button { pressed } => *pressed,
        Input::Rotate { delta } => *delta != 0,
        Input::Touch { phase, .. } => !matches!(phase.as_str(), "up" | "end" | "cancel"),
        _ => true,
    }
}

#[cfg(test)]
mod tests;
