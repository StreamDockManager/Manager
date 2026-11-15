//! Bounded, independent workspace stream. Values are captured under the state lock;
//! the IPC consumer serializes them after the lock has been released
use crate::{
    workspace::{AppRule, AppearancePreset, DeviceSession, Edit, HistoryIndex},
    *,
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceChanges {
    pub profiles: Vec<Profile>,
    pub removed_profiles: Vec<String>,
    pub profile_order: Option<Vec<String>>,
    pub macros: Vec<builtins::SavedMacro>,
    pub removed_macros: Vec<String>,
    pub macro_order: Option<Vec<String>>,
    pub styles: Vec<AppearancePreset>,
    pub removed_styles: Vec<String>,
    pub style_order: Option<Vec<String>>,
    pub devices: Option<Vec<DeviceInfo>>,
    pub plugins: Option<Vec<stream_dock_manager_plugin_host::PluginInfo>>,
    pub sessions: Option<BTreeMap<String, DeviceSession>>,
    pub rules: Option<Vec<AppRule>>,
    pub history: Option<HistoryIndex>,
    pub active_profile: Option<String>,
    pub binding_overrides: Option<BTreeMap<String, BTreeMap<String, builtins::EffectiveBinding>>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct WorkspaceBatch {
    pub generation: String,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub configuration_revision: u64,
    pub reset: Option<Snapshot>,
    pub reset_required: bool,
    pub changes: WorkspaceChanges,
}
pub(crate) struct WorkspaceStream {
    generation: String,
    sequence: u64,
    subscribers: Vec<mpsc::SyncSender<Arc<WorkspaceBatch>>>,
}
impl Default for WorkspaceStream {
    // Construct the default workspace stream value
    fn default() -> Self {
        Self {
            generation: uuid::Uuid::new_v4().to_string(),
            sequence: 0,
            subscribers: vec![],
        }
    }
}
impl WorkspaceStream {
    // Assemble a workspace event batch with its generation, sequence and revision metadata
    fn frame(
        &self,
        state: &Snapshot,
        reset: Option<Snapshot>,
        changes: WorkspaceChanges,
    ) -> WorkspaceBatch {
        WorkspaceBatch {
            generation: self.generation.clone(),
            sequence: self.sequence,
            revision: state.revision,
            configuration_revision: state.configuration_revision,
            reset,
            reset_required: false,
            changes,
        }
    }
    // Publish the current workspace stream result
    pub(crate) fn publish(
        &mut self,
        state: &Snapshot,
        reset: Option<Snapshot>,
        changes: WorkspaceChanges,
    ) -> WorkspaceBatch {
        self.sequence += 1;
        let frame = self.frame(state, reset, changes);
        let shared = Arc::new(frame.clone());
        // Overflow disconnects this subscriber. After draining its bounded queue,
        // the IPC adapter explicitly requests a new snapshot, even if no edit follows
        self.subscribers
            .retain(|tx| tx.try_send(shared.clone()).is_ok());
        frame
    }
}
impl Runtime {
    // Subscribe to workspace
    pub fn subscribe_workspace(&self) -> mpsc::Receiver<Arc<WorkspaceBatch>> {
        let mut state = self.state.lock().unwrap();
        state.plugins = self.plugins.lock().unwrap().list();
        state.binding_overrides = self.builtin_state.overrides(&state);
        let mut stream = self.workspace_stream.lock().unwrap();
        let (tx, rx) = mpsc::sync_channel(64);
        tx.try_send(Arc::new(stream.frame(
            &state,
            Some(state.clone()),
            Default::default(),
        )))
        .unwrap();
        stream.subscribers.push(tx);
        rx
    }
    // Translate a runtime event into scoped workspace changes or a required full reset
    pub(crate) fn workspace_event(&self, kind: &str, state: &mut Snapshot) {
        let mut changes = WorkspaceChanges::default();
        let mut reset = None;
        match kind {
            "profiles" => {
                state.plugins = self.plugins.lock().unwrap().list();
                state.binding_overrides = self.builtin_state.overrides(state);
                reset = Some(state.clone());
            }
            "devices" => {
                changes.devices = Some(state.devices.clone());
                changes.sessions = Some(state.sessions.clone());
            }
            "sessions" | "device-output" => changes.sessions = Some(state.sessions.clone()),
            "live-feedback" => {
                state.binding_overrides = self.builtin_state.overrides(state);
                changes.binding_overrides = Some(state.binding_overrides.clone());
            }
            "plugins" | "plugin-health" | "plugin-runtime" | "plugin-lifecycle" => {
                state.plugins = self.plugins.lock().unwrap().list();
                changes.plugins = Some(state.plugins.clone());
            }
            _ => return,
        }
        self.workspace_stream
            .lock()
            .unwrap()
            .publish(state, reset, changes);
    }
    // Collect changed workspace entities from edits and publish a revisioned update batch
    pub(crate) fn edited_workspace(&self, state: &mut Snapshot, edits: &[Edit]) -> WorkspaceBatch {
        let mut changes = WorkspaceChanges {
            history: Some(state.history.clone()),
            sessions: Some(state.sessions.clone()),
            active_profile: Some(state.active_profile.clone()),
            ..Default::default()
        };
        let (mut profiles, mut macros, mut styles) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let mut bulk = false;
        for edit in edits {
            match edit {
                Edit::SaveProfile { profile } => {
                    profiles.insert(profile.id.clone());
                }
                Edit::Bindings { profile_id, .. } => {
                    profiles.insert(profile_id.clone());
                }
                Edit::AssignControl { .. } => bulk = true,
                Edit::SaveMacro { value } => {
                    macros.insert(value.id.clone());
                }
                Edit::DeleteMacro { id } => {
                    macros.insert(id.clone());
                }
                Edit::SaveStyle { style } => {
                    styles.insert(style.id.clone());
                }
                Edit::DeleteStyle { id } => {
                    styles.insert(id.clone());
                }
                Edit::Rules { .. } => changes.rules = Some(state.rules.clone()),
                // Deletion repairs navigation and rule references across entities
                Edit::DeleteProfile { .. } | Edit::DeletePage { .. } => bulk = true,
            }
        }
        if !profiles.is_empty() {
            changes.profile_order = Some(state.profiles.iter().map(|v| v.id.clone()).collect());
        }
        for id in profiles {
            if let Some(value) = state.profiles.iter().find(|v| v.id == id) {
                changes.profiles.push(value.clone())
            } else {
                changes.removed_profiles.push(id)
            }
        }
        if !macros.is_empty() {
            changes.macro_order = Some(state.saved_macros.iter().map(|v| v.id.clone()).collect());
        }
        for id in macros {
            if let Some(value) = state.saved_macros.iter().find(|v| v.id == id) {
                changes.macros.push(value.clone())
            } else {
                changes.removed_macros.push(id)
            }
        }
        if !styles.is_empty() {
            changes.style_order = Some(state.styles.iter().map(|v| v.id.clone()).collect());
        }
        for id in styles {
            if let Some(value) = state.styles.iter().find(|v| v.id == id) {
                changes.styles.push(value.clone())
            } else {
                changes.removed_styles.push(id)
            }
        }
        state.binding_overrides = self.builtin_state.overrides(state);
        changes.binding_overrides = Some(state.binding_overrides.clone());
        let reset = bulk.then(|| {
            state.plugins = self.plugins.lock().unwrap().list();
            state.clone()
        });
        self.workspace_stream
            .lock()
            .unwrap()
            .publish(state, reset, changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that ordinary edits capture only changed entities with atomic revisions and bounded recovery
    fn ordinary_edits_capture_only_changed_entities_with_atomic_revisions_and_bounded_recovery() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let receiver = runtime.subscribe_workspace();
        let initial = receiver.recv().unwrap();
        assert!(initial.reset.is_some());
        let mut profile = initial.reset.as_ref().unwrap().profiles[0].clone();
        profile.name = "Changed".into();
        let next = runtime
            .apply_workspace_edits(
                initial.configuration_revision,
                "Rename",
                vec![Edit::SaveProfile { profile }],
            )
            .unwrap();
        assert!(next.reset.is_none());
        assert_eq!(next.changes.profiles.len(), 1);
        assert!(next.changes.plugins.is_none());
        assert!(next.changes.devices.is_none());
        assert_eq!(
            next.configuration_revision,
            initial.configuration_revision + 1
        );
        assert_eq!(receiver.recv().unwrap().sequence, next.sequence);
        runtime.emit_plugin_runtime();
        let plugin = receiver.recv().unwrap();
        assert_eq!(plugin.sequence, next.sequence + 1);
        assert!(plugin.changes.plugins.is_some());
        assert!(plugin.changes.profiles.is_empty());
        assert_eq!(plugin.configuration_revision, next.configuration_revision);
        // Event-only traffic has no workspace sequence and cannot create gaps
        runtime.emit("input", None, None, None);
        assert!(receiver.try_recv().is_err());
        for _ in 0..65 {
            runtime.emit("sessions", None, None, None);
        }
        let mut count = 0;
        while receiver.recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 64);
        assert!(runtime
            .subscribe_workspace()
            .recv()
            .unwrap()
            .reset
            .is_some());
        runtime.shutdown();
    }
}

#[cfg(test)]
mod payload_tests {
    use super::*;
    #[test]
    // Verify that single macro edit payload excludes unrelated macro bodies
    fn single_macro_edit_payload_excludes_unrelated_macro_bodies() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let edits = (0..80)
            .map(|i| Edit::SaveMacro {
                value: builtins::SavedMacro {
                    id: format!("macro-{i}"),
                    name: format!("Macro {i}"),
                    inputs: vec![],
                    action: actions::Action::Command {
                        program: "tool".into(),
                        args: vec!["a".repeat(2048)],
                    },
                },
            })
            .collect();
        runtime
            .apply_workspace_edits(
                runtime.snapshot().configuration_revision,
                "Seed library",
                edits,
            )
            .unwrap();
        let initial = runtime.subscribe_workspace().recv().unwrap();
        let baseline = serde_json::to_vec(initial.as_ref()).unwrap().len();
        let mut value = initial.reset.as_ref().unwrap().saved_macros[0].clone();
        value.name = "Renamed".into();
        let delta = runtime
            .apply_workspace_edits(
                initial.configuration_revision,
                "Rename macro",
                vec![Edit::SaveMacro { value }],
            )
            .unwrap();
        assert!(delta.reset.is_none());
        assert_eq!(delta.changes.macros.len(), 1);
        let bytes = serde_json::to_vec(&delta).unwrap().len();
        assert!(bytes < baseline / 10);
        println!("workspace payload measurement: initial={baseline} bytes, single-macro rename={bytes} bytes; 80 macros, 2048-byte argument each");
        runtime.shutdown();
    }
}
