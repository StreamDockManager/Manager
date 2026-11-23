//! Persistent workspace edits. Navigation and drafts are deliberately outside edit history
use crate::{actions::Action, profiles::*, Snapshot};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default, Serialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceSession {
    pub profile_id: String,
    pub page_id: String,
    pub remembered_pages: BTreeMap<String, String>,
    pub manual_profile: String,
    pub automation_enabled: bool,
    pub automation_paused: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub automation_restore_on_exit: bool,
    #[serde(skip)]
    #[ts(skip)]
    pub automation_origin: Option<(String, String)>,
    #[serde(skip)]
    #[ts(skip)]
    pub automation_edge: Option<String>,
    pub output: DeviceOutput,
}

impl DeviceSession {
    pub(super) fn destination_page(&self, profile: &Profile, page_id: Option<&str>) -> String {
        page_id
            .map(String::from)
            .or_else(|| self.remembered_pages.get(&profile.id).cloned())
            .filter(|id| profile.pages.iter().any(|p| &p.id == id))
            .unwrap_or_else(|| profile.active_page.clone())
    }
}

// Accept only the retired appearance field at the session load boundary. It is
// absent from the live model, serializers and generated current contracts
impl<'de> Deserialize<'de> for DeviceSession {
    // Decode workspace from the incoming serialized value
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            profile_id: String,
            page_id: String,
            remembered_pages: BTreeMap<String, String>,
            manual_profile: String,
            automation_enabled: bool,
            automation_paused: bool,
            #[serde(default)]
            automation_restore_on_exit: bool,
            output: DeviceOutput,
            #[serde(default, rename = "display_appearance")]
            _discarded_appearance: Option<serde::de::IgnoredAny>,
        }
        let input = Input::deserialize(deserializer)?;
        Ok(Self {
            profile_id: input.profile_id,
            page_id: input.page_id,
            remembered_pages: input.remembered_pages,
            manual_profile: input.manual_profile,
            automation_enabled: input.automation_enabled,
            automation_paused: input.automation_paused,
            automation_restore_on_exit: input.automation_restore_on_exit,
            output: input.output,
            ..Self::default()
        })
    }
}

/// Repair only references to removed pages. Navigation remains outside history: undo
/// never moves a session off a valid page, while redo repairs a newly selected page
pub(crate) fn repair_removed_pages(
    before: &[Profile],
    after: &[Profile],
    sessions: &mut BTreeMap<String, DeviceSession>,
) -> bool {
    let mut changed = false;
    for old in before {
        let Some(new) = after.iter().find(|profile| profile.id == old.id) else {
            continue;
        };
        for (index, page) in old.pages.iter().enumerate() {
            if new.pages.iter().any(|candidate| candidate.id == page.id) {
                continue;
            }
            let fallback = old.pages[..index]
                .iter()
                .rev()
                .find(|candidate| new.pages.iter().any(|p| p.id == candidate.id))
                .or_else(|| new.pages.first());
            let Some(fallback) = fallback else { continue };
            for session in sessions.values_mut() {
                if session.profile_id == old.id && session.page_id == page.id {
                    session.page_id = fallback.id.clone();
                    changed = true;
                }
                if session.remembered_pages.get(&old.id) == Some(&page.id) {
                    session
                        .remembered_pages
                        .insert(old.id.clone(), fallback.id.clone());
                    changed = true;
                }
            }
        }
    }
    changed
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceOutput {
    /// App preference only. Applied solely by the explicit keyboard command
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub keyboard: Option<hardware_core::KeyboardSettings>,
    pub sleeping: bool,
    #[serde(default = "default_screen_brightness")]
    pub brightness: u8,
    pub lighting_brightness: u8,
    pub lighting_colors: Vec<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vibration_enabled: Option<bool>,
    /// Explicit neighboring hardware choices; absent until the first review
    #[serde(default)]
    pub device_configuration: BTreeMap<String, bool>,
    /// Missing policies use grouped key updates when supported, otherwise frame scheduling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frame_policy: Option<DeviceFramePolicy>,
}
// Supply screen brightness only when no saved screen preference exists
fn default_screen_brightness() -> u8 {
    50
}

impl Default for DeviceOutput {
    // Construct the default workspace value
    fn default() -> Self {
        Self {
            keyboard: None,
            sleeping: false,
            brightness: default_screen_brightness(),
            lighting_brightness: 15,
            lighting_colors: vec![],
            vibration_enabled: None,
            device_configuration: BTreeMap::new(),
            frame_policy: None,
        }
    }
}
/// Rendering policy; host cadence is independent of firmware qualification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceFramePolicy {
    pub policy: hardware_core::OutputPolicy,
    pub encoding: hardware_core::FrameEncoding,
}
impl DeviceFramePolicy {
    // Validate the supplied workspace value
    pub fn validate(&self, capabilities: &hardware_core::DeviceCapabilities) -> Result<(), String> {
        self.policy.validate().map_err(|error| error.to_string())?;
        if self.policy.mode != hardware_core::OutputMode::IndividualUpdates
            && !capabilities
                .frame_output
                .as_ref()
                .is_some_and(|capability| capability.frame_scheduling)
        {
            return Err("Frame scheduling is unavailable for this device".into());
        }
        if self.encoding == hardware_core::FrameEncoding::GroupedKeyUpdates
            && (!capabilities
                .frame_output
                .as_ref()
                .is_some_and(|capability| capability.grouped_key_updates)
                || self.policy.mode != hardware_core::OutputMode::FrameScheduling)
        {
            return Err("Grouped key updates require supported frame scheduling".into());
        }
        Ok(())
    }
}
impl DeviceOutput {
    // Return configured lighting colors when their count matches or generate the fallback palette
    pub fn colors(&self, count: usize) -> Vec<[u8; 3]> {
        if self.lighting_colors.len() == count {
            self.lighting_colors.clone()
        } else {
            vec![[198, 30, 50]; count]
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppearancePreset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_source: Option<crate::profiles::BackgroundSource>,
    pub id: String,
    pub name: String,
    pub appearance: hardware_core::appearance::Appearance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_appearance: Option<Box<hardware_core::plugin_display::PluginAppearance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub composition: Option<Box<hardware_core::composition::DisplayComposition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub overlay_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppRule {
    pub id: String,
    pub device_key: String,
    pub application: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<crate::automation::TitleCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub schedule: Option<crate::automation::Schedule>,
    pub profile_id: String,
    pub page_id: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryIndex {
    pub undo: Vec<HistoryItem>,
    pub redo: Vec<HistoryItem>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HistoryItem {
    pub id: String,
    pub description: String,
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub bytes: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryEntry {
    before: serde_json::Value,
    after: serde_json::Value,
    before_hash: String,
    after_hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum StoredHistory {
    Full(Box<HistoryEntry>),
    Patch {
        base: serde_json::Value,
        forward: Vec<Patch>,
        reverse: Vec<Patch>,
        before_hash: String,
        after_hash: String,
    },
}
// Decode a stored history record into its validated before and after profile documents
pub(crate) fn recovery_history_documents(
    value: serde_json::Value,
) -> Result<[ProfileData; 2], String> {
    let entry: StoredHistory = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok([entry.clone().restore(false)?, entry.restore(true)?])
}

// Validate recovery history
pub(crate) fn validate_recovery_history(value: serde_json::Value) -> Result<(), String> {
    let entry: StoredHistory = serde_json::from_value(value).map_err(|e| e.to_string())?;
    for document in [entry.clone().restore(false)?, entry.restore(true)?] {
        crate::profiles::validate_data(&document)?;
    }
    Ok(())
}

/// Exact-content proofs retain asset IDs only, never snapshots or media bytes
/// File type/size/hash are still checked on every retention scan
#[derive(Default)]
pub(crate) struct HistoryMediaCache {
    entries: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    order: std::collections::VecDeque<String>,
    references: usize,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
}
impl HistoryMediaCache {
    // Return the requested workspace value
    fn get(&mut self, hash: &str) -> Option<std::collections::BTreeSet<String>> {
        if let Some(ids) = self.entries.get(hash) {
            self.hits += 1;
            self.order.retain(|value| value != hash);
            self.order.push_back(hash.into());
            Some(ids.clone())
        } else {
            self.misses += 1;
            None
        }
    }
    // Insert an entry into workspace
    fn insert(&mut self, hash: String, ids: std::collections::BTreeSet<String>) {
        if ids.len() > 32768 {
            return;
        }
        if let Some(old) = self.entries.remove(&hash) {
            self.references -= old.len();
        }
        self.order.retain(|value| value != &hash);
        while self.entries.len() >= 400 || self.references + ids.len() > 32768 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.references -= old.len();
            }
        }
        self.references += ids.len();
        self.order.push_back(hash.clone());
        self.entries.insert(hash, ids);
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Patch {
    path: Vec<String>,
    value: Option<serde_json::Value>,
}
// Compute the canonical JSON integrity digest used by workspace history
fn integrity(value: &serde_json::Value) -> String {
    stream_dock_manager_storage::json_digest::sha256(value)
        .expect("JSON values serialize without failure")
}

// Count serialized JSON bytes with checked arithmetic without retaining the encoded document
fn encoded_len(value: &impl Serialize) -> Result<usize, String> {
    #[derive(Default)]
    struct Counter(usize);
    impl Write for Counter {
        // Write the supplied workspace data
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("History size overflow"))?;
            Ok(bytes.len())
        }
        // Flush pending workspace writes
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value).map_err(|e| e.to_string())?;
    Ok(counter.0)
}
// Diff object fields and equal-length arrays recursively, replacing other changed values as a unit
fn patches(
    before: &serde_json::Value,
    after: &serde_json::Value,
    path: &mut Vec<String>,
    out: &mut Vec<Patch>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for key in a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                path.push(key.clone());
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => patches(a, b, path, out),
                    (_, value) => out.push(Patch {
                        path: path.clone(),
                        value: value.cloned(),
                    }),
                }
                path.pop();
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                path.push(i.to_string());
                patches(a, b, path, out);
                path.pop();
            }
        }
        _ => out.push(Patch {
            path: path.clone(),
            value: Some(after.clone()),
        }),
    }
}
// Apply patch
fn apply_patch(value: &mut serde_json::Value, patches: &[Patch]) -> Result<(), String> {
    for patch in patches {
        if patch.path.is_empty() {
            *value = patch.value.clone().ok_or("Invalid root removal")?;
            continue;
        }
        let mut node = &mut *value;
        for key in &patch.path[..patch.path.len() - 1] {
            node = match node {
                serde_json::Value::Object(v) => v.get_mut(key),
                serde_json::Value::Array(v) => key.parse::<usize>().ok().and_then(|i| v.get_mut(i)),
                _ => None,
            }
            .ok_or("Invalid history patch path")?;
        }
        let key = patch.path.last().unwrap();
        match node {
            serde_json::Value::Object(v) => {
                if let Some(value) = &patch.value {
                    v.insert(key.clone(), value.clone());
                } else {
                    v.remove(key);
                }
            }
            serde_json::Value::Array(v) => {
                *v.get_mut(key.parse::<usize>().map_err(|_| "Invalid patch index")?)
                    .ok_or("Patch index outside array")? =
                    patch.value.clone().ok_or("Invalid array removal")?;
            }
            _ => return Err("Invalid patch destination".into()),
        }
    }
    Ok(())
}
impl StoredHistory {
    // Encode the workspace representation
    fn encode(
        base: serde_json::Value,
        next: serde_json::Value,
        checkpoint: bool,
    ) -> Result<Vec<u8>, String> {
        // Borrow canonical trees throughout a transaction. Count candidates into
        // a sink, then allocate only the winning representation's byte buffer
        #[derive(Serialize)]
        struct Full<'a> {
            before: &'a serde_json::Value,
            after: &'a serde_json::Value,
            before_hash: &'a str,
            after_hash: &'a str,
        }
        #[derive(Serialize)]
        struct Delta<'a> {
            base: &'a serde_json::Value,
            forward: &'a [Patch],
            reverse: &'a [Patch],
            before_hash: &'a str,
            after_hash: &'a str,
        }
        let before_hash = integrity(&base);
        let after_hash = integrity(&next);
        let full = Full {
            before: &base,
            after: &next,
            before_hash: &before_hash,
            after_hash: &after_hash,
        };
        if checkpoint
            || stream_dock_manager_storage::plugin_settings::contains_plugin_action(&base)
            || stream_dock_manager_storage::plugin_settings::contains_plugin_action(&next)
        {
            return serde_json::to_vec(&full).map_err(|e| e.to_string());
        }
        let mut forward = vec![];
        let mut reverse = vec![];
        patches(&base, &next, &mut vec![], &mut forward);
        patches(&next, &base, &mut vec![], &mut reverse);
        let delta = Delta {
            base: &base,
            forward: &forward,
            reverse: &reverse,
            before_hash: &before_hash,
            after_hash: &after_hash,
        };
        let full_len = encoded_len(&full)?;
        let delta_len = encoded_len(&delta)?;
        let mut bytes = Vec::with_capacity(delta_len.min(full_len));
        if delta_len < full_len {
            serde_json::to_writer(&mut bytes, &delta)
        } else {
            serde_json::to_writer(&mut bytes, &full)
        }
        .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
    // Restore the reviewed workspace state
    fn restore(self, redo: bool) -> Result<ProfileData, String> {
        match self {
            Self::Full(entry) => {
                if entry.before_hash != integrity(&entry.before)
                    || entry.after_hash != integrity(&entry.after)
                {
                    return Err("History snapshot integrity failure".into());
                }
                crate::profiles::parse_data(if redo { entry.after } else { entry.before })
            }
            Self::Patch {
                base,
                forward,
                reverse,
                before_hash,
                after_hash,
            } => {
                if integrity(&base) != before_hash {
                    return Err("History base integrity failure".into());
                }
                let mut after = base.clone();
                apply_patch(&mut after, &forward)?;
                if integrity(&after) != after_hash {
                    return Err("History forward integrity failure".into());
                }
                let mut before = after.clone();
                apply_patch(&mut before, &reverse)?;
                if before != base {
                    return Err("History reverse integrity failure".into());
                }
                crate::profiles::parse_data(if redo { after } else { before })
            }
        }
    }
}

// Rewrite history
pub(crate) fn rewrite_history(
    tx: &stream_dock_manager_storage::rusqlite::Connection,
    history: &mut HistoryIndex,
    plugin_id: &str,
    purge: bool,
    clear: impl Fn(&mut ProfileData) -> bool,
) -> Result<bool, String> {
    use stream_dock_manager_storage::{read_json, write_json, Domain};
    const LIMIT: usize = 32 * 1024 * 1024;
    // Report the mentions condition for workspace
    fn mentions(value: &serde_json::Value, id: &str) -> bool {
        match value {
            serde_json::Value::String(value) => value == id,
            serde_json::Value::Array(items) => items.iter().any(|value| mentions(value, id)),
            serde_json::Value::Object(fields) => fields.values().any(|value| mentions(value, id)),
            _ => false,
        }
    }
    let mut changed = false;
    for item in history.undo.iter_mut().chain(&mut history.redo) {
        if purge {
            if let Some(sidecar) =
                read_json::<serde_json::Value>(tx, Domain::HistoryPlugins, &item.id, LIMIT)?
            {
                if sidecar["plugin_id"] == plugin_id {
                    item.bytes = item.bytes.saturating_sub(encoded_len(&sidecar)?);
                    stream_dock_manager_storage::delete(tx, Domain::HistoryPlugins, &item.id)?;
                    changed = true;
                }
            }
        }
        let value = read_json(tx, Domain::History, &item.id, LIMIT)?
            .ok_or("Missing workspace history entry")?;
        if !mentions(&value, plugin_id) {
            continue;
        }
        let old_size = encoded_len(&value)?;
        let record: StoredHistory = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let mut before = record.clone().restore(false)?;
        let mut after = record.restore(true)?;
        if !(clear(&mut before) | clear(&mut after)) {
            continue;
        }
        before.update_requirements();
        after.update_requirements();
        let bytes = StoredHistory::encode(
            serde_json::to_value(before).map_err(|e| e.to_string())?,
            serde_json::to_value(after).map_err(|e| e.to_string())?,
            false,
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        write_json(tx, Domain::History, &item.id, &value, LIMIT)?;
        item.bytes = item.bytes.saturating_sub(old_size) + bytes.len();
        changed = true;
    }
    Ok(changed)
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub key: String,
    pub base: Option<Binding>,
    pub value: serde_json::Value,
}
impl Draft {
    // Validate the supplied workspace value
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.key.len() > 512 || !self.value.is_object() {
            return Err("Invalid saved editor draft".into());
        }
        if let Some(binding) = &self.base {
            binding.validate()?;
        }
        let event_fields = [
            "eventActions",
            "eventActionIndex",
            "eventMultiPressMs",
            "eventHoldMs",
        ];
        let events = event_fields
            .iter()
            .any(|key| self.value.get(*key).is_some());
        if self.value.get("actionDraft").is_some()
            || self.value.get("dialActions").is_some()
            || events
        {
            const FIELDS: &[&str] = &[
                "title",
                "liveLabel",
                "color",
                "customBackground",
                "backgroundSource",
                "asset",
                "media",
                "appearance",
                "appearanceEnabled",
                "pluginAppearance",
                "composition",
                "feedback",
                "disabled",
                "activationPolicy",
                "dialCurve",
                "dialMaxSteps",
                "actionDraft",
                "dialActions",
                "dialActionIndex",
                "dialLinkedTurns",
                "eventActions",
                "eventActionIndex",
                "eventMultiPressMs",
                "eventHoldMs",
            ];
            let unsupported = self
                .value
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| !FIELDS.contains(&key.as_str()))
                .map(|key| format!("$.value[{key:?}]"))
                .collect::<Vec<_>>();
            if !unsupported.is_empty() {
                return Err(format!(
                    "Unsupported saved binding draft fields: {}",
                    unsupported.join(", ")
                ));
            }
        }
        if let Some(label) = self.value.get("liveLabel") {
            let segments = Vec::<crate::live_label::LabelSegment>::deserialize(label)
                .map_err(|error| format!("Unsupported saved live label draft: {error}"))?;
            crate::live_label::validate(&segments)?;
        }
        if let Some(linked) = self.value.get("dialLinkedTurns") {
            if !linked.is_boolean() {
                return Err("Saved dial link mode must be a boolean".into());
            }
        }
        let parse_action = |value: &serde_json::Value| -> Result<Action, String> {
            let action: Action =
                Action::deserialize(value.get("action").ok_or("Invalid saved action draft")?)
                    .map_err(|e| format!("Unsupported saved action draft: {e}"))?;
            if matches!(action, Action::Events { .. }) {
                return Err("Saved editor action lanes cannot contain event assignments".into());
            }
            Ok(action)
        };
        let active = self.value.get("actionDraft");
        let dial_lanes = self.value.get("dialActions").filter(|v| !v.is_null());
        if events {
            if dial_lanes.is_some() {
                return Err("A saved draft cannot contain both event and dial lanes".into());
            }
            let number = |key: &str| -> Result<u16, String> {
                self.value[key]
                    .as_u64()
                    .and_then(|value| value.try_into().ok())
                    .ok_or_else(|| format!("Invalid saved event draft field $.value[{key:?}]"))
            };
            let lanes = self.value["eventActions"]
                .as_array()
                .filter(|lanes| lanes.len() == crate::actions::EventSlot::COUNT)
                .ok_or("Saved event draft requires exactly 14 action slots")?;
            let active_index = usize::from(number("eventActionIndex")?);
            let selected = lanes
                .get(active_index)
                .ok_or("Saved event draft slot is outside range")?;
            if active != Some(selected) {
                return Err("Saved event draft selection does not match its action slot".into());
            }
            Action::Events {
                actions: lanes.iter().map(parse_action).collect::<Result<_, _>>()?,
                multi_press_ms: number("eventMultiPressMs")?,
                hold_ms: number("eventHoldMs")?,
            }
            .validate_draft(0)?;
        } else {
            if let Some(action) = active {
                parse_action(action)?.validate_draft(0)?;
            }
            if let Some(lanes) = dial_lanes {
                let lanes = lanes
                    .as_array()
                    .filter(|lanes| lanes.len() == 3)
                    .ok_or("Saved dial draft requires three explicit lanes")?;
                for lane in lanes {
                    parse_action(lane)?.validate_draft(0)?;
                }
            }
        }
        Ok(())
    }
}

/// A one-time assignment copy, not a linked binding or future-page default
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentScope {
    Page,
    Profile,
    Model,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    SaveMacro {
        value: crate::builtins::SavedMacro,
    },
    DeleteMacro {
        id: String,
    },
    SaveProfile {
        profile: Profile,
    },
    DeleteProfile {
        id: String,
    },
    DeletePage {
        profile_id: String,
        page_id: String,
    },
    Bindings {
        profile_id: String,
        page_id: String,
        bindings: BTreeMap<String, Option<Binding>>,
    },
    LinkControl {
        profile_id: String,
        source: LinkMember,
        target: LinkMember,
    },
    DetachControl {
        profile_id: String,
        member: LinkMember,
    },
    AssignControl {
        profile_id: String,
        page_id: String,
        control_id: String,
        scope: AssignmentScope,
        binding: Binding,
    },
    Rules {
        rules: Vec<AppRule>,
    },
    SaveStyle {
        style: AppearancePreset,
    },
    DeleteStyle {
        id: String,
    },
}

pub(super) fn validate_assignment_targets(
    profile: &Profile,
    profiles: &[Profile],
    binding: &Binding,
) -> Result<(), String> {
    let mut error = None;
    binding.action.visit(&mut |action| {
            let local_page = match action {
                Action::Page { page_id } => Some(page_id),
                Action::Builtin { action } => match action.as_ref() {
                    crate::builtins::BuiltinAction::MomentaryPage { page_id }
                    | crate::builtins::BuiltinAction::BindingLayer { page_id, .. } => Some(page_id),
                    _ => None,
                },
                _ => None,
            };
            if let Some(id) = local_page {
                if crate::parameters::reference(id).is_none()
                    && crate::macro_inputs::reference(id).is_none()
                    && !profile.pages.iter().any(|page| page.id == *id) {
                    error = Some(format!("Page target {id} does not exist in profile {}. Use relative navigation or choose a narrower scope.", profile.name));
                }
            }
            if let Action::Profile { profile_id: id } = action {
                if crate::parameters::reference(id).is_none()
                    && crate::macro_inputs::reference(id).is_none()
                    && !profiles.iter().any(|p| p.id == *id && p.model_id == profile.model_id) {
                    error = Some("Profile target is missing or belongs to a different device model".into());
                }
            }
        });
    if let Some(error) = error {
        return Err(error);
    }
    Ok(())
}
// Assign a binding across the requested scope while preserving model and protected-control constraints
fn assign_control(
    data: &mut ProfileData,
    profile_id: &str,
    page_id: &str,
    control_id: &str,
    scope: AssignmentScope,
    binding: Binding,
) -> Result<(), String> {
    let source = data
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or("Source profile missing")?;
    if !source.pages.iter().any(|page| page.id == page_id) {
        return Err("Source page missing".into());
    }
    let model_id = source.model_id.clone();
    if crate::keyboard_navigation::protected_control(&model_id, page_id, control_id) {
        let saved = source
            .pages
            .iter()
            .find(|page| page.id == page_id)
            .and_then(|page| page.bindings.get(control_id));
        if !matches!(scope, AssignmentScope::Page)
            || !saved
                .is_some_and(|saved| crate::keyboard_navigation::same_behavior(saved, &binding))
        {
            return Err("The keyboard navigation actions are protected; only their appearance can be changed".into());
        }
        binding.validate()?;
        data.profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .unwrap()
            .pages
            .iter_mut()
            .find(|page| page.id == page_id)
            .unwrap()
            .bindings
            .insert(control_id.into(), binding);
        return Ok(());
    }
    let manifests = hardware_registry::manifests();
    let manifest = manifests
        .iter()
        .find(|manifest| manifest.model_id == model_id)
        .ok_or("Source model missing")?;
    manifest.control(control_id).map_err(|e| e.to_string())?;
    binding.validate()?;
    let targets = data.profiles.iter().filter(|profile| {
        profile.model_id == model_id
            && (matches!(scope, AssignmentScope::Model) || profile.id == profile_id)
    });
    // Reject literal local page references that would stop working in another profile
    // The full runtime/domain validators still validate the completed workspace atomically
    for profile in targets {
        validate_assignment_targets(profile, &data.profiles, &binding)?;
    }
    for profile in data.profiles.iter_mut().filter(|profile| {
        profile.model_id == model_id
            && (matches!(scope, AssignmentScope::Model) || profile.id == profile_id)
    }) {
        let pages: Vec<_> = profile
            .pages
            .iter()
            .filter(|page| !matches!(scope, AssignmentScope::Page) || page.id == page_id)
            .filter(|page| {
                !crate::keyboard_navigation::protected_control(&model_id, &page.id, control_id)
            })
            .map(|p| p.id.clone())
            .collect();
        for id in pages {
            profile
                .pages
                .iter_mut()
                .find(|p| p.id == id)
                .unwrap()
                .bindings
                .insert(control_id.into(), binding.clone());
            profile.sync_link(&id, control_id)?;
        }
    }
    Ok(())
}

// Read the current timestamp in milliseconds
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Recovery snapshots retain their workspace, history and draft media independently
/// of the live history cursor. Invalid snapshot data blocks collection
pub(crate) fn snapshot_media(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<std::collections::BTreeSet<String>, String> {
    let data = crate::profile_database::load(connection)?;
    snapshot_media_for_workspace(connection, data.as_ref())
}

pub(crate) fn snapshot_media_for_workspace(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    data: Option<&ProfileData>,
) -> Result<std::collections::BTreeSet<String>, String> {
    use stream_dock_manager_storage::{read_json, Domain};
    let mut assets = std::collections::BTreeSet::new();
    if let Some(data) = data {
        crate::profiles::validate_data(data)?;
        assets.extend(crate::profiles::referenced_assets(data));
        let mut total = 0;
        for item in data.history.undo.iter().chain(&data.history.redo) {
            let value: serde_json::Value =
                read_json(connection, Domain::History, &item.id, 32 * 1024 * 1024)?
                    .ok_or("Missing snapshot history")?;
            total += serde_json::to_vec(&value).map_err(|e| e.to_string())?.len();
            if total > 160 * 1024 * 1024 {
                return Err("Recovery history exceeds limit".into());
            }
            let entry: StoredHistory = serde_json::from_value(value).map_err(|e| e.to_string())?;
            for document in [entry.clone().restore(false)?, entry.restore(true)?] {
                crate::profiles::validate_data(&document)?;
                assets.extend(crate::profiles::referenced_assets(&document));
            }
        }
    }
    let drafts: BTreeMap<String, Draft> = crate::profile_database::LocalPreference::Drafts
        .snapshot_record()
        .read_from(connection)?
        .unwrap_or_default();
    if drafts.len() > 1000 {
        return Err("Recovery draft retention exceeds limit".into());
    }
    for draft in drafts.values() {
        if let Some(id) = draft
            .base
            .as_ref()
            .and_then(|binding| binding.asset.as_ref())
        {
            crate::profiles::validate_asset_id(id)?;
            assets.insert(id.clone());
        }
        if let Some(value) = draft.value.get("asset").filter(|value| !value.is_null()) {
            let id = value.as_str().ok_or("Invalid recovery draft asset")?;
            if !id.is_empty() {
                crate::profiles::validate_asset_id(id)?;
                assets.insert(id.into());
            }
        }
    }
    Ok(assets)
}

// Write JSON
pub(crate) fn write_json(path: &Path, data: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(data).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Workspace document exceeds 16 MiB".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing parent")?)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    atomic_replace(file.path(), path)
}

impl ProfileStore {
    #[cfg(test)]
    // Read a local preference and fall back to its default when absent or unreadable
    pub(crate) fn local<T: serde::de::DeserializeOwned + Default + Send + 'static>(
        &self,
        name: crate::profile_database::LocalPreference,
    ) -> T {
        self.read_preference(name)
            .ok()
            .flatten()
            .unwrap_or_default()
    }
    // Write preference
    pub(crate) fn write_preference(
        &self,
        name: crate::profile_database::LocalPreference,
        value: &impl Serialize,
    ) -> Result<(), String> {
        let record = name.record::<serde_json::Value>();
        self.storage
            .write_json(record.domain, record.key, value, record.max_bytes)
    }
    // Edit the selected workspace value
    pub fn edit(
        &self,
        expected: u64,
        description: &str,
        edit: impl FnOnce(&mut ProfileData) -> Result<(), String>,
    ) -> Result<ProfileData, String> {
        let _transaction = self.transaction.lock().unwrap();
        self.edit_under_transaction(expected, description, edit)
    }
    /// The caller must already hold `transaction`; used by imports that publish media and
    /// workspace content together, in transaction → preparation-gate order
    pub(crate) fn edit_under_transaction(
        &self,
        expected: u64,
        description: &str,
        edit: impl FnOnce(&mut ProfileData) -> Result<(), String>,
    ) -> Result<ProfileData, String> {
        self.edit_import_transaction(expected, description, None, 0, edit)
    }
    // Prepare and atomically commit a revision-checked import with retained-history cost accounting
    pub(crate) fn edit_import_transaction(
        &self,
        expected: u64,
        description: &str,
        history_id: Option<&str>,
        history_extra_bytes: usize,
        edit: impl FnOnce(&mut ProfileData) -> Result<(), String>,
    ) -> Result<ProfileData, String> {
        let prepared = self.prepare_import_transaction(
            expected,
            description,
            history_id,
            history_extra_bytes,
            edit,
        )?;
        self.storage
            .transaction_with_cost(prepared.retained_bytes()?, move |tx| prepared.commit(tx))
    }
    // Prepare import transaction
    pub(crate) fn prepare_import_transaction(
        &self,
        expected: u64,
        description: &str,
        history_id: Option<&str>,
        history_extra_bytes: usize,
        edit: impl FnOnce(&mut ProfileData) -> Result<(), String>,
    ) -> Result<crate::profile_database::PreparedWorkspace, String> {
        let (mut data, stamp) = self.load_for_edit(true)?;
        if data.configuration_revision != expected {
            return Err("Configuration changed. Refresh before saving.".into());
        }
        let mut before = data.clone();
        before.history = HistoryIndex::default();
        edit(&mut data)?;
        crate::keyboard_navigation::validate_edit(&before, &data)?;
        crate::keyboard_navigation::normalize(&mut data)?;
        self.detach_deleted_media(&mut data);
        data.update_requirements();
        let validation = {
            use stream_dock_manager_storage::timing::{start, Phase};
            let _validation = start(Phase::WorkspaceValidation);
            crate::validation::validate_edit(&before, &data)?
        };
        if !validation.is_current(data.configuration_revision) {
            return Err("Stale workspace validation".into());
        }
        let mut after = data.clone();
        after.history = HistoryIndex::default();
        let before_json = serde_json::to_value(&before).map_err(|e| e.to_string())?;
        let after_json = serde_json::to_value(&after).map_err(|e| e.to_string())?;
        let rules_changed = before_json["rules"] != after_json["rules"];
        if history_id.is_none() && before_json == after_json {
            return Ok(
                crate::profile_database::PreparedWorkspace::new(data, expected, None, None)
                    .with_scoped_records(stamp, validation, rules_changed),
            );
        }
        data.configuration_revision += 1;
        let id = history_id
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let history_assets = crate::profiles::referenced_assets(&before)
            .into_iter()
            .chain(crate::profiles::referenced_assets(&after))
            .collect();
        drop(before);
        drop(after);
        let bytes = StoredHistory::encode(
            before_json,
            after_json,
            data.configuration_revision.is_multiple_of(10),
        )?;
        data.history.redo.clear();
        data.history.undo.push(HistoryItem {
            id: id.clone(),
            description: description.chars().take(160).collect(),
            timestamp_ms: now_ms(),
            bytes: bytes.len() + history_extra_bytes,
        });
        while data.history.undo.len() > 100
            || data.history.undo.iter().map(|e| e.bytes).sum::<usize>() > 64 * 1024 * 1024
        {
            data.history.undo.remove(0);
        }
        self.remember_validation(&data)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        {
            use sha2::{Digest, Sha256};
            let canonical = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
            self.history_media_cache
                .lock()
                .unwrap()
                .insert(hex::encode(Sha256::digest(canonical)), history_assets);
        }
        Ok(
            crate::profile_database::PreparedWorkspace::new(
                data,
                expected,
                Some((id, value)),
                None,
            )
            .with_scoped_records(stamp, validation, rules_changed),
        )
    }
    // Inspect history
    pub(crate) fn inspect_history(
        &self,
        expected: u64,
        id: &str,
    ) -> Result<
        (
            HistoryItem,
            ProfileData,
            ProfileData,
            crate::asset_leases::AssetLease,
        ),
        String,
    > {
        let _transaction = self.transaction.lock().unwrap();
        let data = self.load()?;
        if data.configuration_revision != expected {
            return Err("Configuration changed. Select the history entry again.".into());
        }
        uuid::Uuid::parse_str(id).map_err(|_| "Invalid history ID")?;
        let item = data
            .history
            .undo
            .iter()
            .chain(&data.history.redo)
            .find(|item| item.id == id)
            .ok_or("History entry is no longer retained")?
            .clone();
        let bytes = self.history_bytes(id)?;
        let entry: StoredHistory = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut before = entry.clone().restore(false)?;
        let mut after = entry.restore(true)?;
        self.detach_deleted_media(&mut before);
        self.detach_deleted_media(&mut after);
        crate::profiles::validate_data(&before)?;
        crate::profiles::validate_data(&after)?;
        let assets = self.asset_leases.pin(
            crate::profiles::referenced_assets(&before)
                .into_iter()
                .chain(crate::profiles::referenced_assets(&after)),
            "History inspection",
        )?;
        Ok((item, before, after, assets))
    }
    // Serialize the requested history operation behind the workspace transaction lock
    pub fn history_action(&self, expected: u64, operation: &str) -> Result<ProfileData, String> {
        let _transaction = self.transaction.lock().unwrap();
        self.history_under_transaction(expected, operation)
    }
    // Prepare and commit a history operation within an already-held workspace transaction
    pub(crate) fn history_under_transaction(
        &self,
        expected: u64,
        operation: &str,
    ) -> Result<ProfileData, String> {
        let prepared = self.prepare_history(expected, operation)?;
        self.storage
            .transaction_with_cost(prepared.retained_bytes()?, move |tx| prepared.commit(tx))
    }
    // Prepare history
    pub(crate) fn prepare_history(
        &self,
        expected: u64,
        operation: &str,
    ) -> Result<crate::profile_database::PreparedWorkspace, String> {
        let mut data = self.load()?;
        if data.configuration_revision != expected {
            return Err("Configuration changed. Refresh before changing history.".into());
        }
        if operation == "clear" {
            data.history = HistoryIndex::default();
        } else {
            let redo = operation == "redo";
            if !redo && operation != "undo" {
                return Err("Unknown history operation".into());
            }
            let item = if redo {
                data.history.redo.pop()
            } else {
                data.history.undo.pop()
            }
            .ok_or("No history available")?;
            uuid::Uuid::parse_str(&item.id).map_err(|_| "Invalid history ID")?;
            let bytes = self.history_bytes(&item.id)?;
            let entry: StoredHistory = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let mut restored = entry.restore(redo)?;
            restored.history = data.history;
            if redo {
                restored.history.undo.push(item);
            } else {
                restored.history.redo.push(item);
            }
            restored.configuration_revision = data.configuration_revision;
            data = restored;
        }
        data.configuration_revision += 1;
        self.detach_deleted_media(&mut data);
        let data = self.prepared_data(&data)?;
        Ok(crate::profile_database::PreparedWorkspace::new(
            data, expected, None, None,
        ))
    }
}

// Apply edits
pub fn apply_edits(data: &mut ProfileData, edits: Vec<Edit>) -> Result<(), String> {
    if edits.is_empty() || edits.len() > 100 {
        return Err("Choose 1–100 edits".into());
    }
    for edit in edits {
        match edit {
            Edit::SaveStyle { style } => {
                if let Some(existing) = data.styles.iter_mut().find(|s| s.id == style.id) {
                    *existing = style;
                } else {
                    data.styles.push(style);
                }
            }
            Edit::DeleteStyle { id } => data.styles.retain(|s| s.id != id),
            Edit::SaveProfile { profile } => {
                if data.profiles.is_empty() {
                    data.active_profile = profile.id.clone();
                }
                if let Some(old) = data.profiles.iter_mut().find(|p| p.id == profile.id) {
                    *old = profile;
                } else {
                    data.profiles.push(profile);
                }
            }
            Edit::DeleteProfile { id } => data.delete_profile(&id)?,
            Edit::DeletePage {
                profile_id,
                page_id,
            } => data.delete_page(&profile_id, &page_id)?,
            Edit::Bindings {
                profile_id,
                page_id,
                bindings,
            } => {
                let profile = data
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == profile_id)
                    .ok_or("Profile missing")?;
                for (id, binding) in bindings {
                    let page = profile
                        .pages
                        .iter_mut()
                        .find(|p| p.id == page_id)
                        .ok_or("Page missing")?;
                    if let Some(binding) = binding {
                        page.bindings.insert(id.clone(), binding);
                        profile.sync_link(&page_id, &id)?;
                    } else {
                        page.bindings.remove(&id);
                        profile.prune_links(|m| m.page_id != page_id || m.control_id != id);
                    }
                }
            }
            Edit::LinkControl {
                profile_id,
                source,
                target,
            } => {
                data.profiles
                    .iter_mut()
                    .find(|p| p.id == profile_id)
                    .ok_or("Profile missing")?
                    .link(source, target)?;
            }
            Edit::DetachControl { profile_id, member } => {
                data.profiles
                    .iter_mut()
                    .find(|p| p.id == profile_id)
                    .ok_or("Profile missing")?
                    .prune_links(|m| m != &member);
            }
            Edit::AssignControl {
                profile_id,
                page_id,
                control_id,
                scope,
                binding,
            } => {
                assign_control(data, &profile_id, &page_id, &control_id, scope, binding)?;
            }
            Edit::SaveMacro { value } => {
                value.action.validate(0)?;
                if let Some(old) = data.saved_macros.iter_mut().find(|m| m.id == value.id) {
                    *old = value
                } else {
                    data.saved_macros.push(value)
                }
            }
            Edit::DeleteMacro { id } => data.saved_macros.retain(|m| m.id != id),
            Edit::Rules { rules } => {
                for rule in &rules {
                    rule.validate()?;
                }
                if rules.len() > 100
                    || rules.iter().any(|r| {
                        (r.application.trim().is_empty()
                            && r.title.is_none()
                            && r.schedule.is_none())
                            || !data.profiles.iter().any(|p| {
                                p.id == r.profile_id
                                    && r.page_id
                                        .as_ref()
                                        .is_none_or(|id| p.pages.iter().any(|page| &page.id == id))
                            })
                    })
                {
                    return Err("Rule needs a condition and valid profile/page".into());
                }
                data.rules = rules;
            }
        }
    }
    Ok(())
}

impl Snapshot {
    // Resolve a device session to a compatible profile and valid active page with deterministic fallbacks
    pub fn session(&self, device_id: &str) -> DeviceSession {
        let device = self.devices.iter().find(|d| d.id == device_id);
        let key = device
            .map(|d| d.persistence_key.as_str())
            .unwrap_or(device_id);
        let mut session = self.sessions.get(key).cloned().unwrap_or_default();
        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id && device.is_none_or(|d| p.model_id == d.model_id))
            .or_else(|| {
                self.profiles.iter().find(|p| {
                    p.id == self.active_profile && device.is_none_or(|d| p.model_id == d.model_id)
                })
            })
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|p| device.is_none_or(|d| p.model_id == d.model_id))
            });
        if let Some(profile) = profile {
            session.profile_id = profile.id.clone();
            if !profile.pages.iter().any(|p| p.id == session.page_id) {
                session.page_id = session
                    .remembered_pages
                    .get(&profile.id)
                    .filter(|id| profile.pages.iter().any(|p| &p.id == *id))
                    .cloned()
                    .unwrap_or_else(|| profile.active_page.clone());
            }
            if !self.profiles.iter().any(|p| p.id == session.manual_profile) {
                session.manual_profile = profile.id.clone();
            }
        } else {
            session.profile_id.clear();
            session.page_id =
                if device.is_some_and(|d| crate::keyboard_navigation::supported(&d.model_id)) {
                    crate::keyboard_navigation::PAGE_ID.into()
                } else {
                    String::new()
                };
            session.manual_profile.clear();
            session.automation_origin = None;
        }
        session
    }
}

// Borrow mutable action at state for workspace
pub fn action_at_mut<'a>(
    mut action: &'a mut Action,
    path: &[usize],
) -> Result<&'a mut Action, String> {
    for index in path {
        action = action.child_mut(*index).ok_or("Action branch missing")?;
    }
    Ok(action)
}

impl ProfileStore {
    /// The caller owns the workspace transaction. Missing, oversized or unverifiable retention
    /// blocks garbage collection; a scan never repairs data or discards history
    pub(crate) fn retained_media(
        &self,
        ids: &std::collections::BTreeSet<String>,
    ) -> Result<(std::collections::BTreeSet<String>, Vec<String>), String> {
        use sha2::{Digest, Sha256};
        let mut assets = std::collections::BTreeSet::new();
        let mut hashes = vec![];
        let mut total = 0usize;
        if ids.len() > 400 {
            return Err("Retained history scan exceeds 400 entries".into());
        }
        for id in ids {
            if uuid::Uuid::parse_str(id)
                .map_err(|_| "Invalid retained history ID")?
                .to_string()
                != *id
            {
                return Err("Noncanonical retained history ID".into());
            }
            let bytes = self.history_bytes(id)?;
            total = total
                .checked_add(bytes.len())
                .ok_or("History scan size overflow")?;
            if total > 160 * 1024 * 1024 {
                return Err("Retained history scan exceeds 160 MiB".into());
            }
            let hash = hex::encode(Sha256::digest(&bytes));
            hashes.push(format!("{id}:{hash}"));
            if let Some(cached) = self.history_media_cache.lock().unwrap().get(&hash) {
                assets.extend(cached);
                continue;
            }
            let entry: StoredHistory =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid retained history document")?;
            let before = entry.clone().restore(false)?;
            crate::profiles::validate_data(&before)?;
            let mut references = crate::profiles::referenced_assets(&before);
            drop(before);
            let after = entry.restore(true)?;
            crate::profiles::validate_data(&after)?;
            references.extend(crate::profiles::referenced_assets(&after));
            self.history_media_cache
                .lock()
                .unwrap()
                .insert(hash, references.clone());
            assets.extend(references);
        }
        Ok((assets, hashes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the assignment fixture
    fn assignment_fixture() -> (tempfile::TempDir, ProfileStore) {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let data = store.load().unwrap();
        store
            .edit(data.configuration_revision, "Fixture", |data| {
                let first = &mut data.profiles[0];
                let mut page = first.pages[0].clone();
                page.id = "second-page".into();
                first.pages.push(page);
                let mut second = first.clone();
                second.id = "second-profile".into();
                second.name = "Second profile".into();
                data.profiles.push(second);
                Ok(())
            })
            .unwrap();
        (root, store)
    }
    // Create a scoped binding-assignment edit for workspace tests
    fn assignment_edit(data: &ProfileData, scope: AssignmentScope) -> Edit {
        let mut binding = data.profiles[0].pages[0].bindings["key-1"].clone();
        binding.title = "Everywhere".into();
        binding.action = Action::Mute;
        Edit::AssignControl {
            profile_id: data.profiles[0].id.clone(),
            page_id: data.profiles[0].pages[0].id.clone(),
            control_id: "key-1".into(),
            scope,
            binding,
        }
    }
    #[test]
    // Verify that scoped assignment is one atomic undoable copy
    fn scoped_assignment_is_one_atomic_undoable_copy() {
        let (_root, store) = assignment_fixture();
        let before = store.load().unwrap();
        let edit = assignment_edit(&before, AssignmentScope::Model);
        let saved = store
            .edit(before.configuration_revision, "Assign control", |data| {
                apply_edits(data, vec![edit])
            })
            .unwrap();
        assert_eq!(saved.history.undo.len(), before.history.undo.len() + 1);
        assert_eq!(
            saved.configuration_revision,
            before.configuration_revision + 1
        );
        for (profile, original) in saved.profiles.iter().zip(&before.profiles) {
            for (page, old_page) in profile.pages.iter().zip(&original.pages) {
                assert_eq!(page.bindings["key-1"].title, "Everywhere");
                for (id, binding) in &old_page.bindings {
                    if id != "key-1" {
                        assert_eq!(
                            serde_json::to_value(&page.bindings[id]).unwrap(),
                            serde_json::to_value(binding).unwrap()
                        );
                    }
                }
            }
        }
        let undo = store
            .history_action(saved.configuration_revision, "undo")
            .unwrap();
        assert_eq!(
            serde_json::to_value(&undo.profiles).unwrap(),
            serde_json::to_value(&before.profiles).unwrap()
        );
        let redo = store
            .history_action(undo.configuration_revision, "redo")
            .unwrap();
        assert_eq!(
            serde_json::to_value(&redo.profiles).unwrap(),
            serde_json::to_value(&saved.profiles).unwrap()
        );
    }
    #[test]
    fn linked_edits_from_either_page_are_atomic_and_detach_preserves_the_assignment() {
        let (_root, store) = assignment_fixture();
        let before = store.load().unwrap();
        let profile = &before.profiles[0];
        let src = LinkMember {
            page_id: profile.pages[0].id.clone(),
            control_id: "key-1".into(),
        };
        let dst = LinkMember {
            page_id: profile.pages[1].id.clone(),
            control_id: "key-1".into(),
        };
        let edit = |rev, e| store.edit(rev, "Link fixture", |d| apply_edits(d, vec![e]));
        let linked = edit(
            before.configuration_revision,
            Edit::LinkControl {
                profile_id: profile.id.clone(),
                source: src.clone(),
                target: dst.clone(),
            },
        )
        .unwrap();
        let mut b = profile.pages[0].bindings["key-1"].clone();
        b.title = "Edited from second page".into();
        let update = || Edit::Bindings {
            profile_id: profile.id.clone(),
            page_id: dst.page_id.clone(),
            bindings: BTreeMap::from([("key-1".into(), Some(b.clone()))]),
        };
        let saved = edit(linked.configuration_revision, update()).unwrap();
        assert!(edit(linked.configuration_revision, update()).is_err());
        assert!(saved.profiles[0]
            .pages
            .iter()
            .all(|p| p.bindings["key-1"].title == b.title));
        let undone = store
            .history_action(saved.configuration_revision, "undo")
            .unwrap();
        assert_eq!(
            undone.profiles[0].pages[0].bindings["key-1"].title,
            profile.pages[0].bindings["key-1"].title
        );
        let redo = store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        let mut invalid = redo.profiles[0].clone();
        invalid.pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .title
            .clear();
        assert!(invalid.validate().is_err());
        let deleted = edit(
            redo.configuration_revision,
            Edit::DeletePage {
                profile_id: profile.id.clone(),
                page_id: dst.page_id.clone(),
            },
        )
        .unwrap();
        assert!(deleted.profiles[0].linked_controls.is_empty());
        assert_eq!(
            deleted.profiles[0].pages[0].bindings["key-1"].title,
            b.title
        );
        let restored = store
            .history_action(deleted.configuration_revision, "undo")
            .unwrap();
        assert_eq!(restored.profiles[0].linked_controls.len(), 1);
        let detached = edit(
            restored.configuration_revision,
            Edit::DetachControl {
                profile_id: profile.id.clone(),
                member: dst.clone(),
            },
        )
        .unwrap();
        assert!(detached.profiles[0].linked_controls.is_empty());
        assert_eq!(
            detached.profiles[0].pages[1].bindings["key-1"].title,
            b.title
        );
        b.title = "Independent after detach".into();
        let after = edit(
            detached.configuration_revision,
            Edit::Bindings {
                profile_id: profile.id.clone(),
                page_id: dst.page_id.clone(),
                bindings: BTreeMap::from([("key-1".into(), Some(b.clone()))]),
            },
        )
        .unwrap();
        assert_ne!(after.profiles[0].pages[0].bindings["key-1"].title, b.title);
        assert_eq!(
            store.load().unwrap().profiles[0].pages[1].bindings["key-1"].title,
            b.title
        );
    }
    #[test]
    fn linked_dial_settings_and_gestures_survive_a_reopen() {
        let (root, store) = assignment_fixture();
        let data = store.load().unwrap();
        let mut profile = data.profiles[0].clone();
        profile.model_id = "n4-pro".into();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.action = Action::Dial {
            linked_turns: false,
            actions: vec![
                Action::Mute,
                Action::None,
                Action::Page {
                    page_id: profile.pages[1].id.clone(),
                },
            ],
        };
        for page in &mut profile.pages {
            page.bindings.clear();
        }
        profile.pages[0].bindings.insert("dial-1".into(), binding);
        let source = LinkMember {
            page_id: profile.pages[0].id.clone(),
            control_id: "dial-1".into(),
        };
        let target = LinkMember {
            page_id: profile.pages[1].id.clone(),
            control_id: "dial-2".into(),
        };
        profile.link(source, target).unwrap();
        store
            .edit(data.configuration_revision, "Linked dials", |d| {
                apply_edits(d, vec![Edit::SaveProfile { profile }])
            })
            .unwrap();
        drop(store);
        let reopened = ProfileStore::open(root.path().into())
            .unwrap()
            .load()
            .unwrap();
        let p = &reopened.profiles[0];
        p.validate_links().unwrap();
        assert!(
            matches!(&p.pages[1].bindings["dial-2"].action, Action::Dial{actions,..} if actions.len()==3)
        );
    }

    #[test]
    // Verify that scoped assignment respects page profile and exact model boundaries
    fn scoped_assignment_respects_page_profile_and_exact_model_boundaries() {
        for (scope, expected) in [
            (AssignmentScope::Page, 1),
            (AssignmentScope::Profile, 2),
            (AssignmentScope::Model, 4),
        ] {
            let (_root, store) = assignment_fixture();
            let before = store.load().unwrap();
            // Another valid model is not compatible merely because it shares key IDs
            let seeded = store
                .edit(before.configuration_revision, "Other model", |data| {
                    let mut other = data.profiles[0].clone();
                    other.id = "other-model".into();
                    other.model_id = "k1-pro".into();
                    for page in &mut other.pages {
                        page.bindings.retain(|id, _| id == "key-1");
                    }
                    data.profiles.push(other);
                    Ok(())
                })
                .unwrap();
            let edit = assignment_edit(&seeded, scope);
            let saved = store
                .edit(seeded.configuration_revision, "Assign", |data| {
                    apply_edits(data, vec![edit])
                })
                .unwrap();
            assert_eq!(
                saved
                    .profiles
                    .iter()
                    .flat_map(|p| &p.pages)
                    .filter(|p| p.bindings["key-1"].title == "Everywhere")
                    .count(),
                expected
            );
            assert_eq!(
                saved
                    .profiles
                    .last()
                    .unwrap()
                    .pages
                    .iter()
                    .find(|page| page.id == "main")
                    .unwrap()
                    .bindings["key-1"]
                    .title,
                "Browser"
            );
        }
    }
    #[test]
    // Verify that scoped assignment rejects invalid destinations and references without partial save
    fn scoped_assignment_rejects_invalid_destinations_and_references_without_partial_save() {
        let (_root, store) = assignment_fixture();
        let before = store.load().unwrap();
        for case in 0..5 {
            let mut edit = assignment_edit(&before, AssignmentScope::Model);
            if let Edit::AssignControl {
                profile_id,
                page_id,
                control_id,
                binding,
                ..
            } = &mut edit
            {
                match case {
                    0 => *profile_id = "missing".into(),
                    1 => *page_id = "missing".into(),
                    2 => *control_id = "missing".into(),
                    3 => {
                        binding.action = Action::Page {
                            page_id: "missing".into(),
                        }
                    }
                    _ => {
                        binding.action = Action::Dial {
                            linked_turns: false,
                            actions: vec![Action::Mute, Action::Mute, Action::Mute],
                        }
                    }
                }
            }
            assert!(store
                .edit(
                    before.configuration_revision,
                    "Invalid",
                    |data| apply_edits(data, vec![edit])
                )
                .is_err());
            assert_eq!(
                serde_json::to_value(store.load().unwrap()).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
        }
    }
    #[test]
    // Verify that scoped assignment covers more than one hundred pages in one edit
    fn scoped_assignment_covers_more_than_one_hundred_pages_in_one_edit() {
        let (_root, store) = assignment_fixture();
        let before = store.load().unwrap();
        let seeded = store
            .edit(before.configuration_revision, "Many pages", |data| {
                let template = data.profiles[0].pages[0].clone();
                for i in 0..105 {
                    let mut page = template.clone();
                    page.id = format!("many-{i}");
                    data.profiles[0].pages.push(page);
                }
                Ok(())
            })
            .unwrap();
        let edit = assignment_edit(&seeded, AssignmentScope::Profile);
        let saved = store
            .edit(seeded.configuration_revision, "Assign", |data| {
                apply_edits(data, vec![edit])
            })
            .unwrap();
        assert_eq!(saved.profiles[0].pages.len(), 107);
        assert!(saved.profiles[0]
            .pages
            .iter()
            .all(|page| page.bindings["key-1"].title == "Everywhere"));
        assert!(store
            .edit(seeded.configuration_revision, "Stale", |_| Ok(()))
            .is_err());
    }
    #[test]
    // Verify that oversized and malformed history leave the workspace untouched
    fn oversized_and_malformed_history_leave_the_workspace_untouched() {
        use stream_dock_manager_storage::Domain;
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().into()).unwrap();
        let data = store.load().unwrap();
        let edited = store
            .edit(data.configuration_revision, "Rename", |data| {
                data.profiles[0].name = "Changed".into();
                Ok(())
            })
            .unwrap();
        let current = serde_json::to_value(&edited).unwrap();
        let id = edited.history.undo[0].id.clone();
        let original: serde_json::Value = store
            .storage
            .read_json(Domain::History, &id, 32 * 1024 * 1024)
            .unwrap()
            .unwrap();
        let bad_id = id.clone();
        store
            .storage
            .run(move |connection| {
                connection
                    .execute(
                        "UPDATE history_entries SET payload=?1 WHERE key=?2",
                        (format!("\"{}\"", "x".repeat(32 * 1024 * 1024)), bad_id),
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .unwrap_err();
        assert_eq!(
            store
                .storage
                .read_json::<serde_json::Value>(Domain::History, &id, 32 * 1024 * 1024)
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            serde_json::to_value(store.load().unwrap()).unwrap(),
            current
        );
        store
            .storage
            .write_json(
                Domain::History,
                &id,
                &serde_json::json!(["invalid history"]),
                1024,
            )
            .unwrap();
        assert!(store
            .history_action(edited.configuration_revision, "undo")
            .is_err());
        assert!(store.load().is_err());

        store
            .storage
            .write_json(Domain::History, &id, &original, 32 * 1024 * 1024)
            .unwrap();
        assert_eq!(
            serde_json::to_value(store.load().unwrap()).unwrap(),
            current
        );
        assert!(store
            .history_action(edited.configuration_revision, "undo")
            .is_ok());
    }
    #[test]
    // Verify that profile read accepts exact limit and recovers oversized primary
    fn profile_read_accepts_exact_limit_and_recovers_oversized_primary() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().into()).unwrap();
        let data = store.load().unwrap();
        store.save(&data).unwrap();
        let path = store.root.join("external-profile.json");
        std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let mut remaining = 16 * 1024 * 1024 - file.metadata().unwrap().len() as usize;
        let padding = [b' '; 8192];
        while remaining > 0 {
            let count = remaining.min(padding.len());
            file.write_all(&padding[..count]).unwrap();
            remaining -= count;
        }
        drop(file);
        assert!(store.read(&path).is_ok());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b" ")
            .unwrap();
        assert!(store.read(&path).unwrap_err().contains("too large"));
        assert_eq!(store.load().unwrap().schema_version, 1);
    }
    #[test]
    // Verify that history media proofs are bounded and content changes are revalidated
    fn history_media_proofs_are_bounded_and_content_changes_are_revalidated() {
        use std::collections::BTreeSet;
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        let source = root.path().join("pixel.png");
        std::fs::write(
            &source,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    4,
                    4,
                    image::Rgb([10, 20, 30]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let asset = store.import_asset(&source).unwrap();
        let edited = store
            .edit(data.configuration_revision, "Fixture media", |data| {
                data.profiles[0].background_asset = Some(asset.clone());
                Ok(())
            })
            .unwrap();
        let ids = BTreeSet::from([edited.history.undo[0].id.clone()]);
        let first = store.retained_media(&ids).unwrap();
        assert!(first.0.contains(&asset));
        assert_eq!(
            store.history_media_cache.lock().unwrap().misses,
            0,
            "new edits prime exact-content proofs"
        );
        assert_eq!(store.retained_media(&ids).unwrap(), first);
        assert_eq!(store.history_media_cache.lock().unwrap().hits, 2);
        drop(store);
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        assert_eq!(store.retained_media(&ids).unwrap(), first);
        assert_eq!(store.history_media_cache.lock().unwrap().misses, 1);
        assert_eq!(store.retained_media(&ids).unwrap(), first);
        assert_eq!(store.history_media_cache.lock().unwrap().hits, 1);
        let id = ids.first().unwrap();
        let original: serde_json::Value = store
            .storage
            .read_json(
                stream_dock_manager_storage::Domain::History,
                id,
                32 * 1024 * 1024,
            )
            .unwrap()
            .unwrap();
        store
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                id,
                &serde_json::json!(["corrupt"]),
                1024,
            )
            .unwrap();
        assert!(
            store.retained_media(&ids).is_err(),
            "changed records cannot reuse a proof"
        );
        store
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                id,
                &original,
                32 * 1024 * 1024,
            )
            .unwrap();
        assert_eq!(store.retained_media(&ids).unwrap(), first);
        let mut cache = HistoryMediaCache::default();
        for i in 0..401 {
            cache.insert(format!("{i:064x}"), BTreeSet::from([asset.clone()]));
        }
        assert_eq!(cache.entries.len(), 400);
        assert!(cache.get(&format!("{:064x}", 0)).is_none());
        assert_eq!(cache.references, 400);
    }
    #[test]
    // Verify that deleted page repairs saved sessions atomically across restart and history
    fn deleted_page_repairs_saved_sessions_atomically_across_restart_and_history() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("profiles");
        let store = ProfileStore::open(root.clone()).unwrap();
        let mut data = store.load().unwrap();
        data = store
            .edit(data.configuration_revision, "Four pages", |data| {
                let profile = &mut data.profiles[0];
                for id in ["second", "third", "fourth"] {
                    profile.pages.push(Page {
                        id: id.into(),
                        name: id.into(),
                        bindings: Default::default(),
                    });
                }
                profile.active_page = "third".into();
                Ok(())
            })
            .unwrap();
        let mut sessions = BTreeMap::from([
            (
                "disconnected".to_owned(),
                DeviceSession {
                    profile_id: "default".into(),
                    page_id: "third".into(),
                    remembered_pages: BTreeMap::from([("default".into(), "third".into())]),
                    ..Default::default()
                },
            ),
            (
                "unaffected".to_owned(),
                DeviceSession {
                    profile_id: "default".into(),
                    page_id: "fourth".into(),
                    ..Default::default()
                },
            ),
        ]);
        store
            .write_preference(
                crate::profile_database::LocalPreference::Sessions,
                &sessions,
            )
            .unwrap();
        // A failure writing the sessions must roll back workspace and history too
        store.storage.run(|db| db.execute_batch("CREATE TRIGGER reject_session_update BEFORE UPDATE ON local_service_preferences WHEN NEW.key = 'sessions' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").map_err(|e| e.to_string())).unwrap();
        assert!(store
            .edit(data.configuration_revision, "Remove page", |d| d
                .delete_page("default", "third"))
            .is_err());
        assert_eq!(
            store.load().unwrap().configuration_revision,
            data.configuration_revision
        );
        store
            .storage
            .run(|db| {
                db.execute_batch("DROP TRIGGER reject_session_update")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        data = store
            .edit(data.configuration_revision, "Remove page", |d| {
                d.delete_page("default", "third")
            })
            .unwrap();
        assert_eq!(data.profiles[0].active_page, "second");
        assert!(store
            .edit(data.configuration_revision - 1, "Stale", |d| d
                .delete_page("default", "main"))
            .is_err());
        drop(store);
        let store = ProfileStore::open(root).unwrap();
        sessions = store.local(crate::profile_database::LocalPreference::Sessions);
        assert_eq!(sessions["disconnected"].page_id, "second");
        assert_eq!(
            sessions["disconnected"].remembered_pages["default"],
            "second"
        );
        assert_eq!(sessions["unaffected"].page_id, "fourth");
        data = store
            .history_action(data.configuration_revision, "undo")
            .unwrap();
        let saved: BTreeMap<String, DeviceSession> =
            store.local(crate::profile_database::LocalPreference::Sessions);
        assert_eq!(saved["disconnected"].page_id, "second");
        sessions.get_mut("disconnected").unwrap().page_id = "third".into();
        store
            .write_preference(
                crate::profile_database::LocalPreference::Sessions,
                &sessions,
            )
            .unwrap();
        data = store
            .history_action(data.configuration_revision, "redo")
            .unwrap();
        let saved: BTreeMap<String, DeviceSession> =
            store.local(crate::profile_database::LocalPreference::Sessions);
        assert_eq!(saved["disconnected"].page_id, "second");
        sessions.get_mut("disconnected").unwrap().page_id = "main".into();
        store
            .write_preference(
                crate::profile_database::LocalPreference::Sessions,
                &sessions,
            )
            .unwrap();
        store
            .edit(data.configuration_revision, "Remove first", |d| {
                d.delete_page("default", "main")
            })
            .unwrap();
        let saved: BTreeMap<String, DeviceSession> =
            store.local(crate::profile_database::LocalPreference::Sessions);
        assert_eq!(saved["disconnected"].page_id, "second");
    }

    #[test]
    // Verify that undo redo crosses multiple checkpoints after restart
    fn undo_redo_crosses_multiple_checkpoints_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let original = data.profiles[0].name.clone();
        for i in 1..=23 {
            data = store
                .edit(data.configuration_revision, "Rename", |data| {
                    data.profiles[0].name = format!("Profile {i}");
                    Ok(())
                })
                .unwrap();
        }
        let root = store.root.clone();
        drop(store);
        let store = ProfileStore::open(root).unwrap();
        for i in (0..23).rev() {
            data = store
                .history_action(data.configuration_revision, "undo")
                .unwrap();
            assert_eq!(
                data.profiles[0].name,
                if i == 0 {
                    original.clone()
                } else {
                    format!("Profile {i}")
                }
            );
        }
        for i in 1..=23 {
            data = store
                .history_action(data.configuration_revision, "redo")
                .unwrap();
            assert_eq!(data.profiles[0].name, format!("Profile {i}"));
        }
    }
    #[test]
    // Verify that schema v1 history survives restart
    fn schema_v1_history_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        assert_eq!(data.schema_version, 1);
        let saved = store
            .edit(data.configuration_revision, "Rename profile", |data| {
                data.profiles[0].name = "Changed".into();
                Ok(())
            })
            .unwrap();
        assert_eq!(saved.history.undo.len(), 1);
        let root = store.root.clone();
        drop(store);
        let store = ProfileStore::open(root).unwrap();
        let undone = store
            .history_action(saved.configuration_revision, "undo")
            .unwrap();
        assert_eq!(undone.schema_version, 1);
        assert_eq!(undone.profiles[0].name, "Default");
        let redone = store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        assert_eq!(redone.schema_version, 1);
        assert_eq!(redone.profiles[0].name, "Changed");
        assert!(store
            .edit(data.configuration_revision, "Stale", |data| {
                data.profiles[0].name = "Wrong".into();
                Ok(())
            })
            .is_err());
        assert_eq!(store.load().unwrap().profiles[0].name, "Changed");
    }
    #[test]
    // Verify that explicit database recovery restores validated workspace
    fn explicit_database_recovery_restores_validated_workspace() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let data = store.load().unwrap();
        let backup = store.storage.backup_recovery().unwrap();
        store
            .edit(data.configuration_revision, "Changed", |data| {
                data.profiles[0].name = "Later".into();
                Ok(())
            })
            .unwrap();
        {
            let maintenance = store.storage.maintenance().unwrap();
            maintenance
                .restore(backup.path, |connection| {
                    snapshot_media(connection).map(|_| ())
                })
                .unwrap();
        }
        assert_eq!(
            store.load().unwrap().profiles[0].name,
            data.profiles[0].name
        );
        assert!(!store.root.join("profiles.json").exists());
    }
    #[test]
    // Verify that batch validation is atomic and new edit clears redo
    fn batch_validation_is_atomic_and_new_edit_clears_redo() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let data = store.load().unwrap();
        let mut bindings = BTreeMap::new();
        let mut binding = data.profiles[0].pages[0].bindings["key-1"].clone();
        binding.title = "Edited".into();
        bindings.insert("key-1".into(), Some(binding.clone()));
        bindings.insert("unknown-key".into(), Some(binding));
        assert!(store
            .edit(data.configuration_revision, "Invalid batch", |d| {
                apply_edits(
                    d,
                    vec![Edit::Bindings {
                        profile_id: "default".into(),
                        page_id: "main".into(),
                        bindings,
                    }],
                )
            })
            .is_err());
        let unchanged = store.load().unwrap();
        assert_eq!(
            unchanged.profiles[0].pages[0].bindings["key-1"].title,
            "Browser"
        );
        assert!(unchanged.history.undo.is_empty());
        let saved = store
            .edit(data.configuration_revision, "Delete binding", |d| {
                d.profiles[0].pages[0].bindings.remove("key-1");
                Ok(())
            })
            .unwrap();
        let undone = store
            .history_action(saved.configuration_revision, "undo")
            .unwrap();
        let saved = store
            .edit(undone.configuration_revision, "Rename", |d| {
                d.profiles[0].name = "Renamed".into();
                Ok(())
            })
            .unwrap();
        assert!(saved.history.redo.is_empty());
    }
    #[test]
    // Verify that page order persists and supports undo redo without changing page identity
    fn page_order_persists_and_supports_undo_redo_without_changing_page_identity() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let initial = store.load().unwrap();
        let seeded = store
            .edit(initial.configuration_revision, "Add pages", |data| {
                data.profiles[0]
                    .pages
                    .extend(["second", "third"].map(|id| Page {
                        id: id.into(),
                        name: id.into(),
                        bindings: Default::default(),
                    }));
                data.profiles[0].active_page = "second".into();
                Ok(())
            })
            .unwrap();
        let mut profile = seeded.profiles[0].clone();
        profile.pages.rotate_right(1);
        store
            .edit(seeded.configuration_revision, "Reorder pages", |data| {
                apply_edits(data, vec![Edit::SaveProfile { profile }])
            })
            .unwrap();
        drop(store);
        let store = ProfileStore::open(root.path().into()).unwrap();
        let saved = store.load().unwrap();
        let ids = |data: &ProfileData| {
            data.profiles[0]
                .pages
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&saved), ["third", "main", "second"]);
        assert_eq!(saved.profiles[0].active_page, "second");
        assert_eq!(
            saved.profiles[0].pages[1].bindings["key-1"].title,
            "Browser"
        );
        let undone = store
            .history_action(saved.configuration_revision, "undo")
            .unwrap();
        assert_eq!(ids(&undone), ids(&seeded));
        let redone = store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        assert_eq!(ids(&redone), ids(&saved));
        assert_eq!(redone.profiles[0].active_page, "second");
    }
    #[test]
    // Verify that independent device pages share profile content
    fn independent_device_pages_share_profile_content() {
        let data = ProfileData::default();
        let mut profile = data.profiles[0].clone();
        profile.pages.push(Page {
            id: "second".into(),
            name: "Second".into(),
            bindings: Default::default(),
        });
        let manifest = hardware_registry::modules()[0].manifest().clone();
        let device = |id: &str| crate::DeviceInfo {
            id: id.into(),
            persistence_key: id.into(),
            identity_port_dependent: false,
            model_id: "n4-pro".into(),
            name: id.into(),
            variant_id: "simulated".into(),
            firmware: "simulated".into(),
            usb_release: None,
            connected: true,
            simulated: true,
            manifest: manifest.clone(),
        };
        let state = Snapshot {
            binding_overrides: Default::default(),
            saved_macros: vec![],

            revision: 1,
            configuration_revision: 1,
            profiles: vec![profile],
            active_profile: "default".into(),
            devices: vec![device("a"), device("b")],
            plugins: vec![],

            styles: vec![],
            rules: vec![],
            history: Default::default(),
            sessions: BTreeMap::from([
                (
                    "a".into(),
                    DeviceSession {
                        profile_id: "default".into(),
                        page_id: "main".into(),
                        ..Default::default()
                    },
                ),
                (
                    "b".into(),
                    DeviceSession {
                        profile_id: "default".into(),
                        page_id: "second".into(),
                        ..Default::default()
                    },
                ),
            ]),
        };
        assert_eq!(state.session("a").page_id, "main");
        assert_eq!(state.session("b").page_id, "second");
        assert_eq!(state.session("a").profile_id, state.session("b").profile_id);
    }
    #[test]
    // Verify that edited media keeps source and cached output
    fn edited_media_keeps_source_and_cached_output() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("wide.png");
        let mut pixels = image::RgbImage::new(1024, 512);
        for (x, _, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgb(if x < 512 { [255, 0, 0] } else { [0, 0, 255] });
        }
        pixels.save(&source).unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let id = store.import_asset(&source).unwrap();
        assert_eq!(
            image::load_from_memory(&store.asset(&id).unwrap())
                .unwrap()
                .width(),
            1024
        );
        let options = crate::media::MediaOptions {
            crop: [0.5, 0.0, 0.5, 1.0],
            ..Default::default()
        };
        let first = store
            .render_media(&id, std::time::Duration::ZERO, 32, 32, 0, "png", &options)
            .unwrap();
        let second = store
            .render_media(&id, std::time::Duration::ZERO, 32, 32, 0, "png", &options)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            image::load_from_memory(&first)
                .unwrap()
                .to_rgb8()
                .get_pixel(16, 16)
                .0,
            [0, 0, 255]
        );
        assert_eq!(store.render_cache.lock().unwrap().hits, 1);
        let full = store
            .render_media(
                &id,
                std::time::Duration::ZERO,
                32,
                32,
                0,
                "png",
                &Default::default(),
            )
            .unwrap();
        assert_ne!(full, first);
    }
}
#[cfg(test)]
mod patch_tests {
    use super::*;
    #[test]
    // Verify that forward reverse and integrity are checked
    fn forward_reverse_and_integrity_are_checked() {
        let before = ProfileData::default();
        let mut after = before.clone();
        after.profiles[0].name = "Renamed".into();
        let bytes = StoredHistory::encode(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap(),
            false,
        )
        .unwrap();
        let restored = serde_json::from_slice::<StoredHistory>(&bytes)
            .unwrap()
            .restore(true)
            .unwrap();
        assert_eq!(restored.profiles[0].name, "Renamed");
        let restored = serde_json::from_slice::<StoredHistory>(&bytes)
            .unwrap()
            .restore(false)
            .unwrap();
        assert_eq!(restored.profiles[0].name, before.profiles[0].name);
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if value.get("base").is_some() {
            value["before_hash"] = serde_json::json!("corrupt");
            let entry: StoredHistory = serde_json::from_value(value).unwrap();
            assert!(entry.restore(true).is_err())
        }
    }
}

#[cfg(test)]
mod allocation_regressions {
    use super::*;
    // Compute the original history JSON hash for compatibility comparison tests
    fn original_integrity(value: &serde_json::Value) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(serde_json::to_vec(value).unwrap()))
    }
    // Encode the prior full-or-patch history representation for regression comparison
    fn original_encode(
        before: ProfileData,
        after: ProfileData,
        checkpoint: bool,
    ) -> Result<Vec<u8>, String> {
        let full = serde_json::to_vec(&HistoryEntry {
            before: serde_json::to_value(&before).map_err(|e| e.to_string())?,
            after: serde_json::to_value(&after).map_err(|e| e.to_string())?,
            before_hash: original_integrity(&serde_json::to_value(&before).unwrap()),
            after_hash: original_integrity(&serde_json::to_value(&after).unwrap()),
        })
        .map_err(|e| e.to_string())?;
        if checkpoint {
            return Ok(full);
        }
        let base = serde_json::to_value(before).map_err(|e| e.to_string())?;
        let next = serde_json::to_value(after).map_err(|e| e.to_string())?;
        let mut forward = vec![];
        let mut reverse = vec![];
        patches(&base, &next, &mut vec![], &mut forward);
        patches(&next, &base, &mut vec![], &mut reverse);
        let delta = serde_json::to_vec(&StoredHistory::Patch {
            before_hash: original_integrity(&base),
            after_hash: original_integrity(&next),
            base,
            forward,
            reverse,
        })
        .map_err(|e| e.to_string())?;
        Ok(if delta.len() < full.len() {
            delta
        } else {
            full
        })
    }

    #[test]
    // Verify that history encoding preserves exact bytes and reduces allocation traffic
    fn history_encoding_preserves_exact_bytes_and_reduces_allocation_traffic() {
        use crate::media::allocation_tests::measure_allocations;
        let mut before = ProfileData::default();
        for index in 0..80 {
            let mut profile = before.profiles[0].clone();
            profile.id = format!("fixture-{index}");
            profile.name = "é".repeat(8 * 1024);
            before.profiles.push(profile);
        }
        for (shape, checkpoint) in [
            ("small-edit", false),
            ("checkpoint", true),
            ("large-edit", false),
        ] {
            let mut after = before.clone();
            if shape == "large-edit" {
                after.profiles.clear();
            } else {
                after.profiles[0].name = "Changed".into();
            }
            for repetition in 0..5 {
                let original_before = before.clone();
                let original_after = after.clone();
                let (old, old_count, old_bytes) = measure_allocations(|| {
                    original_encode(original_before, original_after, checkpoint).unwrap()
                });
                let (new, new_count, new_bytes) = measure_allocations(|| {
                    StoredHistory::encode(
                        serde_json::to_value(&before).unwrap(),
                        serde_json::to_value(&after).unwrap(),
                        checkpoint,
                    )
                    .unwrap()
                });
                assert_eq!(new, old, "history representation changed");
                assert!(new_bytes < old_bytes);
                assert_eq!(
                    integrity(&serde_json::to_value(&before).unwrap()),
                    original_integrity(&serde_json::to_value(&before).unwrap())
                );
                eprintln!(
                    "HISTORY_ALLOCATION shape={shape} repetition={repetition} old_count={old_count} new_count={new_count} old_bytes={old_bytes} new_bytes={new_bytes} output_bytes={}",
                    new.len()
                );
            }
        }
    }
}

#[cfg(test)]
mod strict_contract_tests {
    use super::*;
    #[test]
    // Verify that unsupported persisted preferences fail without replacing database values
    fn unsupported_persisted_preferences_fail_without_replacing_database_values() {
        use stream_dock_manager_storage::{Domain, Storage};
        for (domain, key, value, rejected) in [
            (
                Domain::ManagerPreferences,
                "desktop",
                serde_json::json!({"notifications":true,"low_memory_tray":false}),
                "low_memory_tray",
            ),
            (
                Domain::LocalPreferences,
                "sessions",
                serde_json::json!({"device":{"touch_panel_modes":{}}}),
                "touch_panel_modes",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(root.path().join("profiles")).unwrap();
            let before = serde_json::to_value(store.load().unwrap()).unwrap();
            store
                .storage
                .write_json(domain, key, &value, 65536)
                .unwrap();
            drop(store);
            let error = ProfileStore::open(root.path().join("profiles"))
                .err()
                .unwrap();
            assert!(error.contains(rejected), "{error}");
            let storage = Storage::open(root.path().into()).unwrap();
            assert_eq!(
                storage
                    .read_json::<serde_json::Value>(domain, key, 65536)
                    .unwrap()
                    .unwrap(),
                value
            );
            let current = storage
                .run(|connection| crate::profile_database::load(connection))
                .unwrap()
                .unwrap();
            assert_eq!(serde_json::to_value(current).unwrap(), before);
        }
    }
    #[test]
    // Verify that hashless stored history blocks restore and restart without erasing evidence
    fn hashless_stored_history_blocks_restore_and_restart_without_erasing_evidence() {
        use stream_dock_manager_storage::{Domain, Storage};
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let initial = store.load().unwrap();
        let edited = store
            .edit(initial.configuration_revision, "Change name", |data| {
                data.profiles[0].name = "Updated".into();
                Ok(())
            })
            .unwrap();
        let id = &edited.history.undo.last().unwrap().id;
        let mut record: serde_json::Value =
            serde_json::from_slice(&store.history_bytes(id).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("before_hash");
        store
            .storage
            .write_json(Domain::History, id, &record, 32 * 1024 * 1024)
            .unwrap();
        assert!(store
            .history_action(edited.configuration_revision, "undo")
            .is_err());
        assert_eq!(
            store.load().unwrap().configuration_revision,
            edited.configuration_revision
        );
        drop(store);
        assert!(ProfileStore::open(root.path().join("profiles")).is_err());
        let storage = Storage::open(root.path().into()).unwrap();
        assert_eq!(
            storage
                .read_json::<serde_json::Value>(Domain::History, id, 32 * 1024 * 1024)
                .unwrap()
                .unwrap(),
            record
        );
    }

    #[test]
    // Verify that retired appearance is discarded without relaxing session validation
    fn retired_appearance_is_discarded_without_relaxing_session_validation() {
        for mode in ["saved", "palette"] {
            let mut value = serde_json::to_value(DeviceSession::default()).unwrap();
            value["display_appearance"] = serde_json::json!(mode);
            let decoded: DeviceSession = serde_json::from_value(value.clone()).unwrap();
            assert!(serde_json::to_value(decoded)
                .unwrap()
                .get("display_appearance")
                .is_none());
            value["unexpected"] = serde_json::json!(true);
            assert!(serde_json::from_value::<DeviceSession>(value).is_err());
        }
    }

    #[test]
    // Verify that retired session and policy fields are rejected
    fn retired_session_and_policy_fields_are_rejected() {
        for key in ["touch_panel_modes", "unknown_field"] {
            let mut value = serde_json::to_value(DeviceSession::default()).unwrap();
            value[key] = serde_json::json!({});
            assert!(serde_json::from_value::<DeviceSession>(value)
                .unwrap_err()
                .to_string()
                .contains(key));
        }
        let value = serde_json::json!({"policy": hardware_core::OutputPolicy::default(), "encoding":"individual_updates", "acknowledge_unqualified":true});
        assert!(serde_json::from_value::<DeviceFramePolicy>(value)
            .unwrap_err()
            .to_string()
            .contains("acknowledge_unqualified"));
    }
    #[test]
    // Verify that full and patch history require hashes and reject retired workspace fields
    fn full_and_patch_history_require_hashes_and_reject_retired_workspace_fields() {
        let before = serde_json::to_value(ProfileData::default()).unwrap();
        let mut after = before.clone();
        after["profiles"][0]["name"] = serde_json::json!("Changed");
        for checkpoint in [false, true] {
            let bytes = StoredHistory::encode(before.clone(), after.clone(), checkpoint).unwrap();
            let entry: StoredHistory = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(matches!(entry, StoredHistory::Full(_)), checkpoint);
            assert_eq!(
                entry.clone().restore(false).unwrap().profiles[0].name,
                ProfileData::default().profiles[0].name
            );
            assert_eq!(entry.restore(true).unwrap().profiles[0].name, "Changed");
            for key in ["before_hash", "after_hash"] {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value.as_object_mut().unwrap().remove(key);
                assert!(serde_json::from_value::<StoredHistory>(value).is_err());
            }
        }
        for key in ["variables", "templates"] {
            let mut old = before.clone();
            old[key] = serde_json::json!([]);
            let bytes = StoredHistory::encode(old.clone(), old, true).unwrap();
            let entry: StoredHistory = serde_json::from_slice(&bytes).unwrap();
            assert!(entry.restore(false).unwrap_err().contains(key));
        }
    }
    #[test]
    // Retain all selected and unselected event action drafts and reject ambiguous layouts
    fn saved_event_draft_validates_shape_without_changing_child_data() {
        use serde_json::json;
        let active = json!({"action":{"kind":"mute"},"advanced":false,"rawValue":null});
        let mut lanes = vec![json!({"action":{"kind":"none"}}); crate::actions::EventSlot::COUNT];
        lanes[0] = json!({"action":{"kind":"volume","delta":13},"rawValue":"{"});
        lanes[2] = active.clone();
        let draft = Draft {
            key: "key".into(),
            base: None,
            value: json!({
                "actionDraft": active,
                "dialActions": null,
                "dialActionIndex": 2,
                "dialLinkedTurns": false,
                "eventActions": lanes,
                "eventActionIndex": 2,
                "eventMultiPressMs": 300,
                "eventHoldMs": 600
            }),
        };
        let original = draft.value.clone();
        draft.validate().unwrap();
        assert_eq!(original, draft.value);
        for (key, value) in [
            ("eventActionIndex", json!(14)),
            ("eventActionIndex", json!(1)),
            ("eventMultiPressMs", json!("300")),
            ("eventHoldMs", json!(3000)),
            ("eventActions", json!(null)),
            ("dialActions", json!([{"action":{"kind":"none"}}])),
        ] {
            let mut invalid = draft.clone();
            invalid.value[key] = value;
            assert!(invalid.validate().is_err(), "{key}");
        }
        let mut invalid = draft;
        invalid.value["eventActions"][13]["action"] = json!({"kind":"retired"});
        assert!(invalid.validate().is_err());
    }

    #[test]
    // Verify that invalid draft reports each unsupported field path
    fn invalid_draft_reports_each_unsupported_field_path() {
        let draft = Draft {
            key: "control".into(),
            base: None,
            value: serde_json::json!({
                "actionDraft": {},
                "unexpectedA": null,
                "unexpectedB": []
            }),
        };
        let error = draft.validate().unwrap_err();
        assert!(error.contains("$.value[\"unexpectedA\"]"));
        assert!(error.contains("$.value[\"unexpectedB\"]"));
    }
}
