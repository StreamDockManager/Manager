//! Foreground data is transient. Window titles never enter workspace history or diagnostics
use crate::{workspace::AppRule, Runtime};
mod signal;
pub(crate) use signal::ObservationSignal;
#[cfg(target_os = "linux")]
mod linux_wait;
#[cfg(target_os = "macos")]
mod native_wait;
#[cfg(target_os = "windows")]
mod windows_wait;

const OBSERVATION_FALLBACK: Duration = Duration::from_secs(1);
const IDENTITY_SAFETY: Duration = Duration::from_secs(30);
const FOREGROUND_DEBOUNCE: Duration = Duration::from_millis(250);
use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TitleCondition {
    pub value: String,
    pub mode: String,
    pub case_sensitive: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub days: Vec<u8>,
    pub start: u16,
    pub end: u16,
}
impl Schedule {
    // Report the matches condition for automation
    pub fn matches(&self, weekday: u8, minute: u16) -> bool {
        if self.start == self.end {
            return self.days.contains(&weekday);
        }
        if self.start < self.end {
            self.days.contains(&weekday) && minute >= self.start && minute < self.end
        } else {
            (self.days.contains(&weekday) && minute >= self.start)
                || (self.days.contains(&((weekday + 6) % 7)) && minute < self.end)
        }
    }
}
// Rust's regex engine has no backtracking; bound compilation and DFA memory too
fn title_regex(condition: &TitleCondition) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(&condition.value)
        .case_insensitive(!condition.case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|error| format!("Invalid window-title regular expression: {error}"))
}
impl AppRule {
    // Validate the supplied automation value
    pub fn validate(&self) -> Result<(), String> {
        if let Some(t) = &self.title {
            if t.value.trim().is_empty()
                || t.value.len() > 4096
                || !["exact", "contains", "regex"].contains(&t.mode.as_str())
            {
                return Err("Invalid title condition".into());
            }
            if t.mode == "regex" {
                title_regex(t)?;
            }
        }
        if let Some(s) = &self.schedule {
            if s.days.is_empty()
                || s.days.len() > 7
                || s.days.iter().any(|d| *d > 6)
                || s.start >= 1440
                || s.end >= 1440
            {
                return Err("Invalid schedule".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct AutomationStatus {
    pub available: bool,
    pub reason: String,
    pub application: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub title_available: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub schedules_available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RuleEvaluation {
    pub id: String,
    pub matched: bool,
    pub winner: bool,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfigurationDraft {
    pub device_id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub expected_enabled: bool,
    pub expected_paused: bool,
    pub expected_restore_on_exit: bool,
    pub rules: Vec<AppRule>,
    pub enabled: bool,
    pub paused: bool,
    pub restore_on_exit: bool,
}
// Return the current automation status
pub fn status() -> AutomationStatus {
    let wayland = cfg!(target_os = "linux")
        && (std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland"));
    AutomationStatus {
        available: !wayland,
        reason: if wayland {
            "Application and title detection are unavailable on Wayland. Schedule-only rules work."
                .into()
        } else {
            "Application, title, and local-time schedule rules".into()
        },
        application: None,
        title_available: !wayland && title_permission(false),
        schedules_available: true,
    }
}
// Report the monitoring enabled condition for automation
fn monitoring_enabled(runtime: &Weak<Runtime>) -> bool {
    // Keep the last external application available before the first rule is enabled
    // This is transient identity only; window titles remain separately demand-gated
    runtime.upgrade().is_some_and(|r| !r.is_stopping())
}
// Retain title observations for explicit rule previews even when automatic switching is paused
fn title_needed(r: &Runtime) -> bool {
    r.state
        .lock()
        .unwrap()
        .rules
        .iter()
        .any(|r| r.title.is_some())
}
// Publish the current automation result
fn publish(
    runtime: &Weak<Runtime>,
    sender: &Arc<ObservationSignal>,
    application: String,
    title: Option<String>,
) {
    if application.is_empty() || is_manager(&application) {
        return;
    }
    if let Some(r) = runtime.upgrade() {
        *r.foreground_title.lock().unwrap() = title;
    }
    sender.publish(application);
}
// Start the automation operation
pub fn start(runtime: Weak<Runtime>) {
    let Some(owner) = runtime.upgrade() else {
        return;
    };
    let signal = owner.automation_signal.clone();
    let sender = signal.clone();
    drop(owner);
    let observed = runtime.clone();
    std::thread::spawn(move || {
        if let Err(error) = observe(sender, observed.clone()) {
            if let Some(r) = observed.upgrade() {
                let mut s = r.automation_status.lock().unwrap();
                s.available = false;
                s.title_available = false;
                s.reason = error;
                drop(s);
                r.automation_signal.wake();
            }
        }
    });
    std::thread::spawn(move || {
        let mut current = String::new();
        let mut signature = None;
        let mut previous_title = None;
        let mut previous_capabilities = None;
        let mut pending: Option<(String, std::time::Instant)> = None;
        loop {
            let (revision, application) = signal.take();
            if let Some(application) = application {
                offer_foreground(&current, &mut pending, application);
            }
            let Some(r) = runtime.upgrade() else { break };
            if r.is_stopping() {
                break;
            }
            if let Some((_, changed)) = &pending {
                if changed.elapsed() < FOREGROUND_DEBOUNCE {
                    let wait = FOREGROUND_DEBOUNCE.saturating_sub(changed.elapsed());
                    drop(r);
                    signal.wait(revision, wait);
                    continue;
                }
            }
            let changed = if let Some((application, _)) = pending.take() {
                current = application;
                true
            } else {
                false
            };
            // Identity tracking serves the application picker even with no enabled rules
            let title = r.foreground_title.lock().unwrap().clone();
            let observation_changed = changed || title != previous_title;
            previous_title = title.clone();
            update_foreground_status(&r, &current, observation_changed);
            let evaluated_at = std::time::Instant::now();
            let now = chrono::Local::now();
            let capabilities = r.automation_status.lock().unwrap().clone();
            let capability_state = (
                capabilities.available,
                capabilities.title_available,
                capabilities.schedules_available,
            );
            let capabilities_changed = previous_capabilities != Some(capability_state);
            previous_capabilities = Some(capability_state);
            if capabilities_changed && !observation_changed {
                r.emit("foreground", None, None, None);
            }
            let next = evaluation_signature(
                &r.state.lock().unwrap(),
                &current,
                title.as_deref(),
                &capabilities,
                now.weekday().num_days_from_monday() as u8,
                (now.hour() * 60 + now.minute()) as u16,
            );
            if next.is_some() && next != signature {
                evaluate(&r, &current);
                if !observation_changed && !capabilities_changed {
                    r.emit("foreground", None, None, None);
                    feedback_wake(&r);
                }
            }
            signature = next;
            let wait = evaluation_wait(&r.state.lock().unwrap(), now)
                .saturating_sub(evaluated_at.elapsed());
            drop(r);
            signal.wait(revision, wait);
        }
    });
}
// Title-dependent rules retain the bounded fallback; identity uses native events plus a slow safety read.
fn observation_interval(runtime: &Weak<Runtime>) -> Duration {
    if runtime.upgrade().is_some_and(|r| title_needed(&r)) {
        OBSERVATION_FALLBACK
    } else {
        IDENTITY_SAFETY
    }
}
// Bound wall-clock/timezone correction to 30 seconds, but wake at actual calendar edges sooner.
fn evaluation_wait<T: chrono::TimeZone>(
    state: &crate::Snapshot,
    now: chrono::DateTime<T>,
) -> Duration {
    let day = now.weekday().num_days_from_monday() as u8;
    let minute = (now.hour() * 60 + now.minute()) as u16;
    let fraction =
        Duration::from_secs(now.second() as u64) + Duration::from_nanos(now.nanosecond() as u64);
    state
        .rules
        .iter()
        .filter(|rule| {
            state.devices.iter().any(|device| {
                let session = state.session(&device.id);
                !device.simulated
                    && device.persistence_key == rule.device_key
                    && session.automation_enabled
                    && !session.automation_paused
            })
        })
        .filter_map(|rule| rule.schedule.as_ref())
        .map(|schedule| schedule_boundary(schedule, day, minute).saturating_sub(fraction))
        .fold(
            // Accessibility permission may change while Manager has focus and publish skips it.
            // Retain the title/preview capability cadence even without new external observations.
            if state.rules.iter().any(|rule| rule.title.is_some()) {
                OBSERVATION_FALLBACK
            } else {
                IDENTITY_SAFETY
            },
            Duration::min,
        )
}
// Candidate edges cover overnight spans and all-day rules without serializing each minute.
fn schedule_boundary(schedule: &Schedule, day: u8, minute: u16) -> Duration {
    let current = u32::from(day) * 1440 + u32::from(minute);
    let mut next = 7 * 1440;
    for edge_day in 0..7u8 {
        for edge_minute in [0, schedule.start, schedule.end] {
            let previous_day = if edge_minute == 0 {
                (edge_day + 6) % 7
            } else {
                edge_day
            };
            let previous_minute = if edge_minute == 0 {
                1439
            } else {
                edge_minute - 1
            };
            if schedule.matches(edge_day, edge_minute)
                == schedule.matches(previous_day, previous_minute)
            {
                continue;
            }
            let edge = u32::from(edge_day) * 1440 + u32::from(edge_minute);
            let distance = (edge + 7 * 1440 - current) % (7 * 1440);
            // The current minute has already been evaluated; don't spin on its start.
            next = next.min(if distance == 0 { 7 * 1440 } else { distance });
        }
    }
    Duration::from_secs(u64::from(next) * 60)
}
// Keep external application identity current without triggering rule work for idle installations
fn update_foreground_status(runtime: &Runtime, application: &str, changed: bool) {
    {
        let mut status = runtime.automation_status.lock().unwrap();
        status.application = (!application.is_empty()).then(|| application.to_owned());
        status.title_available = status.available && title_permission(false);
    }
    if changed {
        runtime.emit("foreground", None, None, None);
        feedback_wake(runtime);
    }
}
// Fingerprint only automation inputs and skip serialization entirely when no device needs evaluation
fn evaluation_signature(
    state: &crate::Snapshot,
    application: &str,
    title: Option<&str>,
    capabilities: &AutomationStatus,
    day: u8,
    minute: u16,
) -> Option<String> {
    let mut sessions = Vec::new();
    let mut schedules = Vec::new();
    let mut titled = false;
    for device in &state.devices {
        let session = state.session(&device.id);
        if device.simulated || !session.automation_enabled || session.automation_paused {
            continue;
        }
        let rules: Vec<_> = state
            .rules
            .iter()
            .filter(|rule| rule.device_key == device.persistence_key)
            .collect();
        // Removing the final rule still needs one evaluation to restore an automatic selection
        if rules.is_empty()
            && session.automation_origin.is_none()
            && !session.automation_edge.as_deref().is_some_and(|edge| {
                serde_json::from_str::<serde_json::Value>(edge)
                    .is_ok_and(|value| value.get(1).is_some_and(serde_json::Value::is_object))
            })
        {
            continue;
        }
        schedules.extend(
            rules
                .iter()
                .filter_map(|rule| rule.schedule.as_ref())
                .map(|s| s.matches(day, minute)),
        );
        titled |= rules.iter().any(|rule| rule.title.is_some());
        sessions.push((
            device.id.clone(),
            session.profile_id,
            session.page_id,
            session.manual_profile,
            session.automation_restore_on_exit,
            session.automation_origin,
            session.automation_edge,
        ));
    }
    if sessions.is_empty() {
        return None;
    }
    serde_json::to_string(&(
        application,
        (
            capabilities.available,
            capabilities.title_available,
            capabilities.schedules_available,
        ),
        titled.then_some(title),
        state.configuration_revision,
        sessions,
        schedules,
    ))
    .ok()
}
// Offer the latest foreground observation to automatic profile selection
fn offer_foreground(
    current: &str,
    pending: &mut Option<(String, std::time::Instant)>,
    application: String,
) {
    if application == current {
        *pending = None;
    } else if pending
        .as_ref()
        .is_none_or(|(candidate, _)| *candidate != application)
    {
        *pending = Some((application, std::time::Instant::now()));
    }
}
// Wake automation after feedback changes
fn feedback_wake(r: &Runtime) {
    let _ = r.device_commands.send(crate::devices::Request::Wake);
}
// Report whether manager
fn is_manager(application: &str) -> bool {
    application == "app.streamdockmanager.manager"
        || std::env::current_exe().is_ok_and(|p| {
            if cfg!(windows) {
                p.to_string_lossy().eq_ignore_ascii_case(application)
            } else {
                p.to_string_lossy() == application
            }
        })
}

// Explain which automatic profile rules match the supplied observation
pub fn explain_rules(
    rules: &[AppRule],
    device_key: &str,
    application: &str,
    title: Option<&str>,
    capabilities: &AutomationStatus,
    day: u8,
    minute: u16,
) -> Vec<RuleEvaluation> {
    let mut found = false;
    rules
        .iter()
        .filter(|r| r.device_key == device_key)
        .map(|rule| {
            let mut reasons = vec![];
            if !rule.application.is_empty()
                && (!capabilities.available
                    || !(if cfg!(windows) {
                        rule.application.eq_ignore_ascii_case(application)
                    } else {
                        rule.application == application
                    }))
            {
                reasons.push(
                    if capabilities.available {
                        "Application does not match"
                    } else {
                        "Application detection unavailable"
                    }
                    .into(),
                );
            }
            if let Some(condition) = &rule.title {
                if !capabilities.title_available {
                    reasons.push("Window-title permission or capability unavailable".into());
                } else {
                    let matched = title.is_some_and(|title| {
                        if condition.mode == "regex" {
                            return title_regex(condition).is_ok_and(|regex| regex.is_match(title));
                        }
                        let (title, value) = if condition.case_sensitive {
                            (title.to_string(), condition.value.clone())
                        } else {
                            (title.to_lowercase(), condition.value.to_lowercase())
                        };
                        match condition.mode.as_str() {
                            "exact" => title == value,
                            "contains" => title.contains(&value),
                            _ => false,
                        }
                    });
                    if !matched {
                        reasons.push("Window title does not match".into());
                    }
                }
            }
            if rule
                .schedule
                .as_ref()
                .is_some_and(|s| !s.matches(day, minute))
            {
                reasons.push("Outside the scheduled days or hours".into());
            }
            let matched = reasons.is_empty();
            let winner = matched && !found;
            found |= matched;
            if matched {
                reasons.push(
                    if winner {
                        "First matching rule"
                    } else {
                        "An earlier rule takes priority"
                    }
                    .into(),
                );
            }
            RuleEvaluation {
                id: rule.id.clone(),
                matched,
                winner,
                reasons,
            }
        })
        .collect()
}
// Evaluate automatic profile rules against current device, foreground and clock observations
pub fn preview(runtime: &Runtime, device_id: &str) -> Vec<RuleEvaluation> {
    let state = runtime.snapshot();
    let status = runtime.automation_status.lock().unwrap().clone();
    let title = runtime.foreground_title.lock().unwrap().clone();
    let now = chrono::Local::now();
    let key = state
        .devices
        .iter()
        .find(|d| d.id == device_id)
        .map(|d| d.persistence_key.as_str())
        .unwrap_or(device_id);
    explain_rules(
        &state.rules,
        key,
        status.application.as_deref().unwrap_or(""),
        title.as_deref(),
        &status,
        now.weekday().num_days_from_monday() as u8,
        (now.hour() * 60 + now.minute()) as u16,
    )
}

fn destination<'a>(
    state: &'a crate::Snapshot,
    device: &crate::DeviceInfo,
    session: &'a crate::workspace::DeviceSession,
    rule: Option<&'a AppRule>,
) -> Option<(&'a str, Option<&'a str>)> {
    Some(if let Some(rule) = rule {
        (rule.profile_id.as_str(), rule.page_id.as_deref())
    } else if session.automation_restore_on_exit {
        let (profile_id, page_id) = session.automation_origin.as_ref()?;
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|p| p.id == *profile_id && p.model_id == device.model_id)
        {
            (
                profile.id.as_str(),
                profile
                    .pages
                    .iter()
                    .find(|p| p.id == *page_id)
                    .map(|p| p.id.as_str())
                    .or(Some(profile.active_page.as_str())),
            )
        } else {
            (session.manual_profile.as_str(), None)
        }
    } else {
        (session.manual_profile.as_str(), None)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleTest {
    pub device_id: String,
    pub rules: Vec<AppRule>,
    pub application: String,
    pub title: Option<String>,
    pub weekday: u8,
    pub minute: u16,
    pub enabled: bool,
    pub paused: bool,
    pub restore_on_exit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RuleTestResult {
    pub evaluations: Vec<RuleEvaluation>,
    pub profile_id: Option<String>,
    pub page_id: Option<String>,
    pub outcome: String,
}

pub fn test_rules(state: &crate::Snapshot, test: RuleTest) -> Result<RuleTestResult, String> {
    if test.weekday > 6
        || test.minute >= 1440
        || test.rules.len() > 100
        || test.application.len() > 4096
        || test.title.as_ref().is_some_and(|text| text.len() > 4096)
    {
        return Err("Choose a valid weekday, time and bounded application and title.".into());
    }
    let device = state
        .devices
        .iter()
        .find(|d| d.id == test.device_id)
        .ok_or("Test device is unavailable")?;
    let mut ids = std::collections::HashSet::new();
    for rule in &test.rules {
        rule.validate()?;
        if rule.device_key != device.persistence_key
            || rule.id.is_empty()
            || rule.id.len() > 1024
            || !ids.insert(&rule.id)
            || rule.application.len() > 4096
            || (rule.application.trim().is_empty()
                && rule.title.is_none()
                && rule.schedule.is_none())
            || !state.profiles.iter().any(|p| {
                p.id == rule.profile_id
                    && p.model_id == device.model_id
                    && rule
                        .page_id
                        .as_ref()
                        .is_none_or(|id| p.pages.iter().any(|page| &page.id == id))
            })
        {
            return Err(
                "Each test rule needs a unique ID, a condition and a compatible destination."
                    .into(),
            );
        }
    }
    let mut session = state.session(&device.id);
    session.automation_restore_on_exit = test.restore_on_exit;
    let capabilities = AutomationStatus {
        available: true,
        title_available: true,
        schedules_available: true,
        ..Default::default()
    };
    let evaluations = explain_rules(
        &test.rules,
        &device.persistence_key,
        &test.application,
        test.title.as_deref(),
        &capabilities,
        test.weekday,
        test.minute,
    );
    let winner = evaluations
        .iter()
        .position(|r| r.winner)
        .map(|i| &test.rules[i]);
    let target = destination(state, device, &session, winner).and_then(|(id, page)| {
        let p = state
            .profiles
            .iter()
            .find(|p| p.id == id && p.model_id == device.model_id)?;
        let selected = if page.is_none() && id == session.profile_id {
            session.page_id.clone()
        } else {
            session.destination_page(p, page)
        };
        Some((id.to_owned(), selected))
    });
    let edge = serde_json::to_string(&(&test.application, winner)).map_err(|e| e.to_string())?;
    let outcome = if !test.enabled {
        "Automatic switching is off"
    } else if test.paused {
        "Paused after manual selection"
    } else if device.simulated {
        "Automatic switching is inactive on simulators"
    } else if is_manager(&test.application) {
        "Manager foreground is ignored"
    } else if session.automation_edge.as_deref() == Some(&edge) {
        "This foreground and rule were already handled"
    } else if let Some((id, page)) = &target {
        if id == &session.profile_id && page == &session.page_id {
            "Destination already active"
        } else {
            "Would switch"
        }
    } else {
        "No destination to switch to"
    };
    let (profile_id, page_id) = target.unzip();
    Ok(RuleTestResult {
        evaluations,
        profile_id,
        page_id,
        outcome: outcome.into(),
    })
}

// Evaluate the automation rules
pub fn evaluate(runtime: &Arc<Runtime>, application: &str) {
    if is_manager(application) {
        return;
    }
    let state = runtime.snapshot();
    for device in &state.devices {
        let session = state.session(&device.id);
        if !session.automation_enabled || session.automation_paused || device.simulated {
            continue;
        }
        let capabilities = runtime.automation_status.lock().unwrap().clone();
        let title = runtime.foreground_title.lock().unwrap().clone();
        let now = chrono::Local::now();
        let results = explain_rules(
            &state.rules,
            &device.persistence_key,
            application,
            title.as_deref(),
            &capabilities,
            now.weekday().num_days_from_monday() as u8,
            (now.hour() * 60 + now.minute()) as u16,
        );
        let rule = results
            .iter()
            .find(|r| r.winner)
            .and_then(|r| state.rules.iter().find(|rule| rule.id == r.id));
        // Navigation is an edge-triggered action, not a continuously enforced page
        // Ignore ordinary session/page edits and title changes that keep the same winner
        let edge = serde_json::to_string(&(application, rule)).unwrap_or_default();
        {
            let mut live = runtime.state.lock().unwrap();
            let mut current = live.session(&device.id);
            if live.configuration_revision != state.configuration_revision
                || current.profile_id != session.profile_id
                || current.page_id != session.page_id
                || !current.automation_enabled
                || current.automation_paused
                || current.automation_edge.as_ref() == Some(&edge)
            {
                continue;
            }
            current.automation_edge = Some(edge);
            live.sessions
                .insert(device.persistence_key.clone(), current);
        }
        let Some((id, page)) = destination(&state, device, &session, rule) else {
            continue;
        };
        if id != session.profile_id
            || page.is_some_and(|p| p != session.page_id)
            || (session.automation_restore_on_exit
                && (rule.is_some() != session.automation_origin.is_some()))
        {
            if let Err(error) = runtime.select_automatic_profile(
                &device.id,
                id,
                page,
                rule.is_some(),
                state.configuration_revision,
                &session,
            ) {
                runtime.emit(
                    "automation-warning",
                    Some(device.id.clone()),
                    None,
                    Some(error),
                );
            }
        }
    }
}
#[cfg(target_os = "macos")]
// Report the title permission condition for automation
pub fn title_permission(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            AXIsProcessTrusted()
        } else {
            crate::feedback::accessibility_prompt();
            AXIsProcessTrusted()
        }
    }
}
#[cfg(not(target_os = "macos"))]
// Report the title permission condition for automation
pub fn title_permission(_: bool) -> bool {
    true
}
#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> *const std::ffi::c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const std::ffi::c_void,
        attribute: *const std::ffi::c_void,
        value: *mut *const std::ffi::c_void,
    ) -> i32;
}
#[cfg(target_os = "macos")]
// Read the focused window title only when Accessibility access is already available
fn mac_title(pid: i32) -> Option<String> {
    use crate::feedback::{cf_string, cf_to_string, CFRelease};
    if !title_permission(false) {
        return None;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let attr = cf_string("AXFocusedWindow");
        let mut window = std::ptr::null();
        let result = AXUIElementCopyAttributeValue(app, attr, &mut window);
        CFRelease(attr);
        CFRelease(app);
        if result != 0 || window.is_null() {
            return None;
        }
        let attr = cf_string("AXTitle");
        let mut value = std::ptr::null();
        let result = AXUIElementCopyAttributeValue(window, attr, &mut value);
        CFRelease(attr);
        CFRelease(window);
        if result != 0 || value.is_null() {
            return None;
        }
        let text = cf_to_string(value);
        CFRelease(value);
        text
    }
}
#[cfg(target_os = "macos")]
// Update automation from the new observation
fn observe(sender: Arc<ObservationSignal>, runtime: Weak<Runtime>) -> Result<(), String> {
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidWakeNotification,
    };
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSSystemClockDidChangeNotification,
    };
    let wait = native_wait::NativeWait::new(sender.clone())?;
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let observed = runtime.clone();
    let published = sender.clone();
    let notify = Arc::new(move || {
        if !monitoring_enabled(&observed) {
            return;
        }
        if let Some(app) = NSWorkspace::sharedWorkspace().frontmostApplication() {
            if let Some(id) = app.bundleIdentifier() {
                let application = id.to_string();
                // Do not request Manager's title while retaining the previous external app.
                let title = observed
                    .upgrade()
                    .filter(|r| title_needed(r) && !is_manager(&application))
                    .and_then(|_| mac_title(app.processIdentifier()));
                publish(&observed, &published, application, title);
            }
        }
    });
    let callback_notify = notify.clone();
    let callback =
        block2::RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| callback_notify());
    let changed = sender.clone();
    let calendar = block2::RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| changed.wake());
    let defaults = NSNotificationCenter::defaultCenter();
    let (activation, wake, clock, timezone) = unsafe {
        (
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidActivateApplicationNotification),
                None,
                None,
                &callback,
            ),
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &calendar,
            ),
            defaults.addObserverForName_object_queue_usingBlock(
                Some(NSSystemClockDidChangeNotification),
                None,
                None,
                &calendar,
            ),
            defaults.addObserverForName_object_queue_usingBlock(
                Some(objc2_foundation::ns_string!(
                    "NSSystemTimeZoneDidChangeNotification"
                )),
                None,
                None,
                &calendar,
            ),
        )
    };
    // Subscribe before the first sample so an activation racing startup is not lost.
    while monitoring_enabled(&runtime) {
        let revision = sender.configuration();
        objc2::rc::autoreleasepool(|_| notify());
        wait.wait(revision, observation_interval(&runtime));
    }
    unsafe {
        center.removeObserver((*activation).as_ref());
        center.removeObserver((*wake).as_ref());
        defaults.removeObserver((*clock).as_ref());
        defaults.removeObserver((*timezone).as_ref());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
// Update automation from the new observation
fn observe(sender: Arc<ObservationSignal>, runtime: Weak<Runtime>) -> Result<(), String> {
    use std::ffi::c_void;
    type H = *mut c_void;
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct Msg {
        hwnd: H,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        point: Point,
        private: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn SetWinEventHook(
            min: u32,
            max: u32,
            module: H,
            callback: Option<unsafe extern "system" fn(H, u32, H, i32, i32, u32, u32)>,
            process: u32,
            thread: u32,
            flags: u32,
        ) -> H;
        fn UnhookWinEvent(hook: H) -> i32;
        fn GetForegroundWindow() -> H;
        fn GetWindowTextW(window: H, text: *mut u16, max: i32) -> i32;
        fn GetWindowThreadProcessId(window: H, process: *mut u32) -> u32;
        fn PeekMessageW(msg: *mut Msg, window: H, min: u32, max: u32, remove: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> H;
        fn QueryFullProcessImageNameW(
            process: H,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: H) -> i32;
    }
    thread_local! { static SENDER: std::cell::RefCell<Option<(Arc<ObservationSignal>,Weak<Runtime>)>> = const { std::cell::RefCell::new(None) }; }
    // Route the native platform event to its registered runtime owner
    unsafe extern "system" fn callback(
        _: H,
        event: u32,
        window: H,
        object: i32,
        _: i32,
        _: u32,
        _: u32,
    ) {
        if event == 0x800c
            && (!SENDER.with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|(_, r)| r.upgrade())
                    .is_some_and(|r| title_needed(&r))
            }) || object != 0
                || window != GetForegroundWindow())
        {
            return;
        }
        if !SENDER.with(|s| {
            s.borrow()
                .as_ref()
                .is_some_and(|(_, runtime)| monitoring_enabled(runtime))
        }) {
            return;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        let process = OpenProcess(0x1000, 0, pid);
        if process.is_null() {
            SENDER.with(|s| {
                if let Some((sender, _)) = s.borrow().as_ref() {
                    if let Some((_, runtime)) = s.borrow().as_ref() {
                        publish(runtime, sender, String::new(), None);
                    }
                }
            });
            return;
        }
        let mut name = vec![0u16; 32768];
        let mut len = name.len() as u32;
        if QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut len) != 0 {
            let name = String::from_utf16_lossy(&name[..len as usize]);
            SENDER.with(|s| {
                if let Some((sender, _)) = s.borrow().as_ref() {
                    let mut text = vec![0u16; 4096];
                    let title = if s
                        .borrow()
                        .as_ref()
                        .and_then(|(_, r)| r.upgrade())
                        .is_some_and(|r| title_needed(&r))
                    {
                        let len = GetWindowTextW(window, text.as_mut_ptr(), 4096);
                        Some(String::from_utf16_lossy(&text[..len.max(0) as usize]))
                    } else {
                        None
                    };
                    if let Some((_, runtime)) = s.borrow().as_ref() {
                        publish(runtime, sender, name, title);
                    }
                }
            });
        }
        CloseHandle(process);
    }
    let wait = windows_wait::NativeWait::new(sender.clone())?;
    SENDER.with(|s| *s.borrow_mut() = Some((sender.clone(), runtime.clone())));
    unsafe {
        let hook = SetWinEventHook(3, 3, std::ptr::null_mut(), Some(callback), 0, 0, 0);
        if hook.is_null() {
            return Err("Windows foreground observer could not start".into());
        }
        // Window name events complement foreground activation; polling covers unavailable hooks
        let title_hook = SetWinEventHook(
            0x800c,
            0x800c,
            std::ptr::null_mut(),
            Some(callback),
            0,
            0,
            0,
        );
        callback(hook, 3, GetForegroundWindow(), 0, 0, 0, 0);
        let mut sampled = std::time::Instant::now();
        let mut configuration = sender.configuration();
        let mut result = Ok(());
        while runtime
            .upgrade()
            .is_some_and(|r| !r.stopping.load(std::sync::atomic::Ordering::SeqCst))
        {
            let revision = sender.configuration();
            if revision != configuration || sampled.elapsed() >= observation_interval(&runtime) {
                configuration = revision;
                callback(hook, 3, GetForegroundWindow(), 0, 0, 0, 0);
                sampled = std::time::Instant::now();
            }
            let mut msg: Msg = std::mem::zeroed();
            for _ in 0..256 {
                if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 1) == 0 {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Err(error) =
                wait.wait(observation_interval(&runtime).saturating_sub(sampled.elapsed()))
            {
                result = Err(error);
                break;
            }
        }
        if !title_hook.is_null() {
            UnhookWinEvent(title_hook);
        }
        UnhookWinEvent(hook);
        SENDER.with(|sender| *sender.borrow_mut() = None);
        result
    }
}

#[cfg(target_os = "linux")]
// Update automation from the new observation
fn observe(sender: Arc<ObservationSignal>, runtime: Weak<Runtime>) -> Result<(), String> {
    use x11rb::{
        connection::Connection,
        protocol::{
            xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask},
            Event,
        },
    };
    if !status().available {
        return Err(status().reason);
    }
    let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = connection.setup().roots[screen].root;
    let active = connection
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .atom;
    let pid_atom = connection
        .intern_atom(false, b"_NET_WM_PID")
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .atom;
    let title_atoms: Vec<_> = [b"_NET_WM_NAME".as_slice(), b"WM_NAME".as_slice()]
        .iter()
        .filter_map(|name| {
            connection
                .intern_atom(false, name)
                .ok()?
                .reply()
                .ok()
                .map(|reply| reply.atom)
        })
        .collect();
    let root_events = connection
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .ok()
        .is_some_and(|cookie| cookie.check().is_ok());
    let mut observed_window = None;
    let mut sampled = std::time::Instant::now() - IDENTITY_SAFETY;
    let wait = linux_wait::NativeWait::new(sender.clone())?;
    let mut configuration = sender.configuration();
    while runtime.upgrade().is_some_and(|r| !r.is_stopping()) {
        let revision = sender.configuration();
        let mut changed = revision != configuration;
        configuration = revision;
        // Drain notifications without repeating property round trips for every queued event
        let mut drained = 0;
        for _ in 0..256 {
            let Some(event) = connection.poll_for_event().map_err(|e| e.to_string())? else {
                break;
            };
            drained += 1;
            changed |= match event {
                Event::PropertyNotify(event) => {
                    (root_events && event.window == root && event.atom == active)
                        || (Some(event.window) == observed_window
                            && (event.atom == pid_atom
                                || (runtime.upgrade().is_some_and(|r| title_needed(&r))
                                    && title_atoms.contains(&event.atom))))
                }
                Event::DestroyNotify(event) => Some(event.window) == observed_window,
                _ => false,
            };
        }
        let interval = if root_events {
            observation_interval(&runtime)
        } else {
            OBSERVATION_FALLBACK
        };
        if !changed && sampled.elapsed() < interval {
            // x11rb may have already buffered event 257; readability alone cannot reveal it.
            if drained == 256 {
                continue;
            }
            wait.wait(
                connection.stream(),
                interval.saturating_sub(sampled.elapsed()),
            )?;
            continue;
        }
        sampled = std::time::Instant::now();
        if monitoring_enabled(&runtime) {
            let window = connection
                .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .value32()
                .and_then(|mut values| values.next())
                .filter(|w| *w != 0);
            if window != observed_window {
                if let Some(old) = observed_window {
                    let _ = connection
                        .change_window_attributes(
                            old,
                            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
                        )
                        .map(|cookie| cookie.check());
                }
                observed_window = window;
                if let Some(window) = window {
                    let _ = connection
                        .change_window_attributes(
                            window,
                            &ChangeWindowAttributesAux::new().event_mask(
                                EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY,
                            ),
                        )
                        .map(|cookie| cookie.check());
                }
            }
            let application = window
                .and_then(|window| {
                    connection
                        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
                        .ok()?
                        .reply()
                        .ok()?
                        .value32()?
                        .next()
                })
                .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let title = if runtime.upgrade().is_some_and(|r| title_needed(&r))
                && !is_manager(&application)
            {
                window.and_then(|window| {
                    title_atoms.iter().find_map(|&atom| {
                        let reply = connection
                            .get_property(false, window, atom, AtomEnum::ANY, 0, 1024)
                            .ok()?
                            .reply()
                            .ok()?;
                        if reply.value.is_empty() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(&reply.value).into())
                        }
                    })
                })
            } else {
                None
            };
            publish(&runtime, &sender, application, title);
        }
    }
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    #[test]
    // Verify that foreground filter recognizes the packaged application and current executable
    fn foreground_filter_recognizes_the_packaged_application_and_current_executable() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../../src-tauri/tauri.conf.json")).unwrap();
        assert!(super::is_manager(config["identifier"].as_str().unwrap()));
        assert!(super::is_manager(
            std::env::current_exe().unwrap().to_str().unwrap()
        ));
        assert!(!super::is_manager("app.streamdockmanager.unrelated"));
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    // Build a non-simulated device snapshot without starting native observation
    fn fixture() -> (tempfile::TempDir, Arc<Runtime>, crate::Snapshot, AppRule) {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut state = runtime
            .simulate_device(true, "simulator:automation-demand")
            .unwrap();
        state.devices[0].simulated = false;
        let device = state.devices[0].clone();
        let mut session = state.session(&device.id);
        session.automation_enabled = true;
        state
            .sessions
            .insert(device.persistence_key.clone(), session.clone());
        let rule = AppRule {
            id: "demand".into(),
            device_key: device.persistence_key,
            application: "external.app".into(),
            title: None,
            schedule: None,
            profile_id: session.profile_id,
            page_id: None,
        };
        (root, runtime, state, rule)
    }

    #[test]
    fn draft_winner_changes_order_without_touching_the_runtime() {
        let (_root, runtime, state, mut a) = fixture();
        a.schedule = Some(Schedule {
            days: vec![6],
            start: 1380,
            end: 60,
        });
        a.title = Some(TitleCondition {
            value: "editor".into(),
            mode: "contains".into(),
            case_sensitive: false,
        });
        let mut b = a.clone();
        b.id = "second".into();
        b.title.as_mut().unwrap().mode = "regex".into();
        let device = state.devices[0].id.clone();
        let test = RuleTest {
            device_id: device,
            rules: vec![a.clone(), b.clone()],
            application: a.application.clone(),
            title: Some("Editor window".into()),
            weekday: 0,
            minute: 15,
            enabled: true,
            paused: false,
            restore_on_exit: false,
        };
        let before = serde_json::to_value(&state).unwrap();
        let one = test_rules(&state, test.clone()).unwrap();
        assert!(one.evaluations[0].winner && one.evaluations[1].matched);
        let mut other = test;
        other.rules.reverse();
        other.paused = true;
        let two = test_rules(&state, other).unwrap();
        assert_eq!(two.evaluations[0].id, b.id);
        assert!(two.outcome.contains("Paused"));
        assert_eq!(one.page_id, two.page_id);
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
        runtime.shutdown();
    }
    #[test]
    // Retain external application identity without requesting evaluation for a zero-rule installation
    fn zero_rules_skip_evaluation_but_keep_external_application_picker_current() {
        let (_root, runtime, state, _) = fixture();
        assert!(evaluation_signature(
            &state,
            "external.app",
            None,
            &AutomationStatus::default(),
            0,
            0
        )
        .is_none());
        update_foreground_status(&runtime, "external.app", true);
        assert_eq!(
            runtime
                .automation_status
                .lock()
                .unwrap()
                .application
                .as_deref(),
            Some("external.app")
        );
        runtime.shutdown();
    }

    #[test]
    // Ignore unrelated navigation history while retaining title-only and scheduled rule dependencies
    fn signatures_track_only_required_title_clock_and_automation_inputs() {
        let (_root, runtime, mut state, mut rule) = fixture();
        state.rules = vec![rule.clone()];
        let key = evaluation_signature(
            &state,
            "external.app",
            Some("one"),
            &AutomationStatus::default(),
            0,
            0,
        );
        assert_eq!(
            key,
            evaluation_signature(
                &state,
                "external.app",
                Some("two"),
                &AutomationStatus::default(),
                1,
                90
            )
        );
        state
            .sessions
            .values_mut()
            .next()
            .unwrap()
            .remembered_pages
            .insert("unrelated".into(), "page".into());
        assert_eq!(
            key,
            evaluation_signature(
                &state,
                "external.app",
                Some("one"),
                &AutomationStatus::default(),
                0,
                0
            )
        );
        rule.application.clear();
        rule.title = Some(TitleCondition {
            value: "^Document".into(),
            mode: "regex".into(),
            case_sensitive: true,
        });
        state.rules = vec![rule.clone()];
        assert_ne!(
            evaluation_signature(
                &state,
                "external.app",
                Some("Document A"),
                &AutomationStatus::default(),
                0,
                0
            ),
            evaluation_signature(
                &state,
                "external.app",
                Some("Document B"),
                &AutomationStatus::default(),
                0,
                0
            )
        );
        let capabilities = AutomationStatus {
            available: true,
            title_available: true,
            ..Default::default()
        };
        assert!(
            explain_rules(
                &state.rules,
                &rule.device_key,
                "different.app",
                Some("Document A"),
                &capabilities,
                0,
                0
            )[0]
            .winner
        );
        state.rules[0].schedule = Some(Schedule {
            days: vec![0],
            start: 0,
            end: 60,
        });
        assert_ne!(
            evaluation_signature(
                &state,
                "external.app",
                None,
                &AutomationStatus::default(),
                0,
                0
            ),
            evaluation_signature(
                &state,
                "external.app",
                None,
                &AutomationStatus::default(),
                0,
                60
            )
        );
        runtime.shutdown();
    }

    #[test]
    // Continue evaluation long enough to leave a removed rule before becoming idle
    fn removed_rules_retain_restoration_demand_and_paused_devices_skip_work() {
        let (_root, runtime, mut state, rule) = fixture();
        let session = state.sessions.values_mut().next().unwrap();
        session.automation_edge =
            Some(serde_json::to_string(&("external.app", Some(&rule))).unwrap());
        assert!(evaluation_signature(
            &state,
            "external.app",
            None,
            &AutomationStatus::default(),
            0,
            0
        )
        .is_some());
        let session = state.sessions.values_mut().next().unwrap();
        session.automation_edge =
            Some(serde_json::to_string(&("external.app", None::<AppRule>)).unwrap());
        assert!(evaluation_signature(
            &state,
            "external.app",
            None,
            &AutomationStatus::default(),
            0,
            0
        )
        .is_none());
        state
            .sessions
            .values_mut()
            .next()
            .unwrap()
            .automation_origin = Some(("original".into(), "page".into()));
        assert!(evaluation_signature(
            &state,
            "external.app",
            None,
            &AutomationStatus::default(),
            0,
            0
        )
        .is_some());
        state
            .sessions
            .values_mut()
            .next()
            .unwrap()
            .automation_paused = true;
        assert!(evaluation_signature(
            &state,
            "external.app",
            None,
            &AutomationStatus::default(),
            0,
            0
        )
        .is_none());
        runtime.shutdown();
    }

    #[test]
    // Repeated observations of one candidate must not postpone its debounce deadline
    fn duplicate_foreground_events_preserve_candidate_deadline() {
        let mut pending = None;
        offer_foreground("first", &mut pending, "second".into());
        let started = pending.as_ref().unwrap().1;
        offer_foreground("first", &mut pending, "second".into());
        assert_eq!(pending.as_ref().unwrap().1, started);
        offer_foreground("first", &mut pending, "first".into());
        assert!(pending.is_none());
        assert_eq!(OBSERVATION_FALLBACK, Duration::from_secs(1));
    }
    #[test]
    // Losing a native capability must invalidate an unchanged app-only rule observation
    fn capability_failure_invalidates_rule_signature_without_a_foreground_change() {
        let (_root, runtime, mut state, rule) = fixture();
        state.rules = vec![rule];
        let mut capabilities = AutomationStatus {
            available: true,
            title_available: true,
            schedules_available: true,
            ..Default::default()
        };
        let before = evaluation_signature(&state, "external.app", None, &capabilities, 0, 0);
        capabilities.available = false;
        capabilities.title_available = false;
        assert_ne!(
            before,
            evaluation_signature(&state, "external.app", None, &capabilities, 0, 0)
        );
        runtime.shutdown();
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn overnight_and_all_day_boundaries_include_midnight_and_week_rollover() {
        let schedule = Schedule {
            days: vec![6],
            start: 23 * 60,
            end: 60,
        };
        assert_eq!(
            schedule_boundary(&schedule, 6, 23 * 60 + 59),
            Duration::from_secs(61 * 60)
        );
        assert_eq!(schedule_boundary(&schedule, 0, 59), Duration::from_secs(60));
        let all_day = Schedule {
            days: vec![0, 1],
            start: 0,
            end: 0,
        };
        assert_eq!(
            schedule_boundary(&all_day, 0, 1439),
            Duration::from_secs(1441 * 60)
        );
        assert_eq!(
            schedule_boundary(&all_day, 1, 1439),
            Duration::from_secs(60)
        );
        assert_eq!(
            schedule_boundary(&all_day, 2, 0),
            Duration::from_secs(5 * 24 * 60 * 60)
        );
    }
    #[test]
    fn identity_only_mode_is_slow_but_title_previews_retain_fast_fallback() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let weak = Arc::downgrade(&runtime);
        assert_eq!(observation_interval(&weak), IDENTITY_SAFETY);
        runtime.state.lock().unwrap().rules.push(AppRule {
            id: "title".into(),
            device_key: "test".into(),
            application: String::new(),
            title: Some(TitleCondition {
                value: "title".into(),
                mode: "contains".into(),
                case_sensitive: false,
            }),
            schedule: None,
            profile_id: "test".into(),
            page_id: None,
        });
        assert_eq!(observation_interval(&weak), OBSERVATION_FALLBACK);
        assert_eq!(
            evaluation_wait(&runtime.state.lock().unwrap(), chrono::Utc::now()),
            OBSERVATION_FALLBACK
        );
        runtime.shutdown();
    }
    #[test]
    fn calendar_edges_recompute_from_wall_time_after_timezone_or_resume_changes() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut state = runtime
            .simulate_device(true, "simulator:clock-test")
            .unwrap();
        state.devices[0].simulated = false;
        let device = state.devices[0].clone();
        let mut session = state.session(&device.id);
        session.automation_enabled = true;
        state
            .sessions
            .insert(device.persistence_key.clone(), session.clone());
        state.rules.push(AppRule {
            id: "clock".into(),
            device_key: device.persistence_key.clone(),
            application: String::new(),
            title: None,
            schedule: Some(Schedule {
                days: vec![0],
                start: 9 * 60,
                end: 10 * 60,
            }),
            profile_id: session.profile_id,
            page_id: None,
        });
        let utc = chrono::Utc
            .with_ymd_and_hms(2026, 11, 16, 8, 59, 50)
            .unwrap();
        assert_eq!(evaluation_wait(&state, utc), Duration::from_secs(10));
        let moved_zone = utc.with_timezone(&chrono::FixedOffset::west_opt(3600).unwrap());
        assert_eq!(evaluation_wait(&state, moved_zone), IDENTITY_SAFETY);
        let resumed = chrono::Utc
            .with_ymd_and_hms(2026, 11, 16, 9, 59, 55)
            .unwrap();
        assert_eq!(evaluation_wait(&state, resumed), Duration::from_secs(5));
        state
            .sessions
            .get_mut(&device.persistence_key)
            .unwrap()
            .automation_paused = true;
        assert_eq!(evaluation_wait(&state, resumed), IDENTITY_SAFETY);
        runtime.shutdown();
    }
}
