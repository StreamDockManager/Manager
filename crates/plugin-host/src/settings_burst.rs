//! Adaptive buffering for legacy VSD Craft action settings
use crate::OwnedSettingsWrite;
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const SAMPLE: Duration = Duration::from_secs(1);
const ATTEMPTS_PER_STEP: usize = 3;
const MAX_STEPS: usize = 6;
const MAX_ATTEMPTS: usize = ATTEMPTS_PER_STEP * MAX_STEPS;
const STEP_SECONDS: u64 = 5;
const MAX_SOURCES: usize = 128;
const MAX_TARGETS: usize = 32;
const MAX_TOTAL_TARGETS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct SettingsBursts {
    sources: HashMap<String, Source>,
    sequence: u64,
}

struct Source {
    last: Instant,
    attempts: VecDeque<Instant>,
    window: Option<(Instant, Duration)>,
    writes: HashMap<String, (u64, OwnedSettingsWrite)>,
}

// Identify the durable binding without transient inspector metadata
fn key(address: &Value) -> String {
    serde_json::json!([
        address["profile_id"],
        address["page_id"],
        address["control_id"],
        address["macro_id"],
        address["path"],
    ])
    .to_string()
}

// Combine independent object fields while replacing arrays and scalar values
fn merge(old: &mut Value, newer: Value) {
    match (old, newer) {
        (Value::Object(old), Value::Object(newer)) => {
            for (field, value) in newer {
                match old.entry(field) {
                    serde_json::map::Entry::Occupied(mut slot) => merge(slot.get_mut(), value),
                    serde_json::map::Entry::Vacant(slot) => {
                        slot.insert(value);
                    }
                }
            }
        }
        (old, newer) => *old = newer,
    }
}

impl SettingsBursts {
    // Allow occasional writes through immediately and collect writes after detecting a burst
    pub(super) fn offer(
        &mut self,
        mut request: OwnedSettingsWrite,
        now: Instant,
    ) -> Result<Option<OwnedSettingsWrite>, String> {
        self.sources.retain(|_, source| {
            !source.writes.is_empty() || now.duration_since(source.last) < Duration::from_secs(30)
        });
        if !self.sources.contains_key(&request.plugin_id) && self.sources.len() >= MAX_SOURCES {
            return Err("Too many VSD settings writers".into());
        }
        let total = self
            .sources
            .values()
            .map(|source| source.writes.len())
            .sum::<usize>();
        let source = self
            .sources
            .entry(request.plugin_id.clone())
            .or_insert_with(|| Source {
                last: now,
                attempts: Default::default(),
                window: None,
                writes: Default::default(),
            });
        // Resume normal saves after a quiet, fully flushed window
        if source.writes.is_empty()
            && source
                .window
                .is_some_and(|(start, delay)| now >= start + delay)
            && now.duration_since(source.last) >= SAMPLE
        {
            source.window = None;
        }
        source.last = now;
        source
            .attempts
            .retain(|at| now.duration_since(*at) < SAMPLE);
        source.attempts.push_back(now);
        if source.attempts.len() > MAX_ATTEMPTS {
            source.attempts.pop_front();
        }
        let attempts = source.attempts.len();
        if source.window.is_none() && attempts < 3 {
            return Ok(Some(request));
        }
        let steps = (attempts / ATTEMPTS_PER_STEP).clamp(1, MAX_STEPS);
        let pressure = Duration::from_secs(STEP_SECONDS * steps as u64);
        let window = source.window.get_or_insert((now, pressure));
        // The window can grow under pressure but its anchor never moves while collecting
        window.1 = window.1.max(pressure);
        let target = key(&request.address);
        if let Some((_, previous)) = source.writes.get(&target).filter(|(_, previous)| {
            previous.session == request.session && previous.action_id == request.action_id
        }) {
            let mut merged = previous.settings.clone();
            merge(&mut merged, request.settings.clone());
            if merged.to_string().len() <= MAX_BYTES {
                request.settings = merged;
            }
        } else if !source.writes.contains_key(&target)
            && (source.writes.len() >= MAX_TARGETS || total >= MAX_TOTAL_TARGETS)
        {
            return Err("Too many buffered VSD settings targets".into());
        }
        self.sequence = self.sequence.wrapping_add(1);
        source.writes.insert(target, (self.sequence, request));
        Ok(None)
    }

    // Collect all due writes together so one workspace transaction can persist them
    pub(super) fn take_due(&mut self, now: Instant) -> Vec<OwnedSettingsWrite> {
        let mut due = Vec::new();
        for source in self.sources.values_mut() {
            if !source.writes.is_empty()
                && source
                    .window
                    .is_some_and(|(start, delay)| now >= start + delay)
            {
                due.extend(source.writes.drain().map(|(_, write)| write));
                source.window = Some((now, Duration::from_secs(STEP_SECONDS)));
            }
        }
        due.sort_unstable_by_key(|(sequence, _)| *sequence);
        due.into_iter().map(|(_, write)| write).collect()
    }

    // Expose buffered state only to the session and context that wrote it
    pub(super) fn pending(
        &self,
        plugin: &str,
        session: &str,
        context: &str,
        address: &Value,
    ) -> Option<&Value> {
        let (_, write) = self.sources.get(plugin)?.writes.get(&key(address))?;
        (write.session == session && write.context == context && write.address == *address)
            .then_some(&write.settings)
    }

    // Never let an older background save overwrite a newer immediate inspector edit
    pub(super) fn discard_target(&mut self, plugin: &str, address: &Value) {
        if let Some(source) = self.sources.get_mut(plugin) {
            source.writes.remove(&key(address));
        }
    }

    pub(super) fn revoke_owner(&mut self, owner: &str) {
        self.sources.remove(owner);
    }

    // Release buffered values when the originating session closes
    pub(super) fn revoke_session(&mut self, session: &str) {
        for source in self.sources.values_mut() {
            source
                .writes
                .retain(|_, (_, write)| write.session != session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Construct a saved action write without a live transport
    fn write(target: &str, settings: Value) -> OwnedSettingsWrite {
        OwnedSettingsWrite {
            operation_id: "operation".into(),
            plugin_id: "vendor".into(),
            action_id: "action".into(),
            context: target.into(),
            address: json!({"control_id":target,"path":[]}),
            settings,
            session: "session".into(),
            resources: None,
            resource_snapshots: vec![],
        }
    }

    #[test]
    // Verify that ordinary saves remain immediate and a burst commits all targets at once
    fn bursts_merge_objects_and_flush_once_after_the_window() {
        let start = Instant::now();
        let mut writes = SettingsBursts::default();
        for _ in 0..2 {
            assert!(writes
                .offer(write("key-1", json!({})), start)
                .unwrap()
                .is_some());
        }
        for (target, settings) in [
            (
                "key-1",
                json!({"nested":{"a":1}, "array":[1,2], "replace":1}),
            ),
            (
                "key-1",
                json!({"nested":{"b":2}, "array":[3], "replace":null}),
            ),
            ("key-2", json!({"other":true})),
        ] {
            assert!(writes
                .offer(write(target, settings), start)
                .unwrap()
                .is_none());
        }
        let address = write("key-1", Value::Null).address;
        let merged = json!({"nested":{"a":1,"b":2}, "array":[3], "replace":null});
        assert_eq!(
            writes.pending("vendor", "session", "key-1", &address),
            Some(&merged)
        );
        assert_eq!(writes.pending("vendor", "other", "key-1", &address), None);
        assert!(writes.take_due(start + Duration::from_secs(4)).is_empty());
        let batch = writes.take_due(start + Duration::from_secs(5));
        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch
                .iter()
                .find(|w| w.context == "key-1")
                .unwrap()
                .settings,
            merged
        );
        assert!(writes.take_due(start + Duration::from_secs(5)).is_empty());
    }

    #[test]
    // Verify every pressure step from five seconds through the thirty-second ceiling
    fn pressure_has_six_five_second_steps() {
        let start = Instant::now();
        for step in 1..=6 {
            let mut writes = SettingsBursts::default();
            for _ in 0..step * ATTEMPTS_PER_STEP {
                writes.offer(write("key-1", json!({})), start).unwrap();
            }
            let wait = STEP_SECONDS * step as u64;
            assert!(writes
                .take_due(start + Duration::from_secs(wait - 1))
                .is_empty());
            assert_eq!(writes.take_due(start + Duration::from_secs(wait)).len(), 1);
        }
    }

    #[test]
    // Verify that continuous pressure reaches but never extends past the 30 second ceiling
    fn continuous_pressure_grows_to_thirty_seconds_without_starving() {
        let start = Instant::now();
        let mut writes = SettingsBursts::default();
        for i in 0..600 {
            let now = start + Duration::from_millis(i * 50);
            writes
                .offer(write("key-1", json!({"value":i})), now)
                .unwrap();
            assert!(writes.take_due(now).is_empty());
        }
        let batch = writes.take_due(start + Duration::from_millis(30_100));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].settings, json!({"value":599}));
        writes
            .offer(
                write("key-1", json!({"new":true})),
                start + Duration::from_millis(30_101),
            )
            .unwrap();
        writes.revoke_session("session");
        assert!(writes.take_due(start + Duration::from_secs(61)).is_empty());
    }
}
