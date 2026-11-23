//! Resolve implicit assignment targets only inside the caller's reviewed mutation or staging scope
use crate::{
    profiles::{Page, Profile, ProfileData, DEFAULT_PROFILE_NAME},
    DeviceInfo, Snapshot,
};

// Require an explicit connected device when more than one device could own an implicit assignment
pub(crate) fn connected_device<'a>(
    state: &'a Snapshot,
    device_id: Option<&str>,
) -> Result<&'a DeviceInfo, String> {
    if let Some(id) = device_id.filter(|id| !id.is_empty()) {
        return state
            .devices
            .iter()
            .find(|device| device.id == id && device.connected)
            .ok_or_else(|| "Assignment device is missing or disconnected".into());
    }
    let mut connected = state.devices.iter().filter(|device| device.connected);
    let device = connected
        .next()
        .ok_or("Connect a device before assigning a binding")?;
    if connected.next().is_some() {
        return Err("Choose a connected device before assigning a binding".into());
    }
    Ok(device)
}

// Resolve exact IDs strictly or create an inert Default candidate for the selected device's model
// Mutating this working copy does not persist anything; the caller must validate and commit atomically
pub(crate) fn resolve(
    state: &Snapshot,
    data: &mut ProfileData,
    device_id: Option<&str>,
    profile_id: &str,
    page_id: &str,
) -> Result<(String, String), String> {
    let selected = if profile_id.is_empty() || device_id.is_some_and(|id| !id.is_empty()) {
        Some(connected_device(state, device_id)?)
    } else {
        None
    };
    let profile_index = if !profile_id.is_empty() {
        let index = data
            .profiles
            .iter()
            .position(|profile| profile.id == profile_id)
            .ok_or("Assignment profile is missing")?;
        if selected.is_some_and(|device| data.profiles[index].model_id != device.model_id) {
            return Err("Assignment profile belongs to a different device model".into());
        }
        index
    } else {
        let device = selected.unwrap();
        let session = state.session(&device.id);
        let existing = data
            .profiles
            .iter()
            .position(|profile| {
                profile.id == session.profile_id && profile.model_id == device.model_id
            })
            .or_else(|| {
                data.profiles
                    .iter()
                    .position(|profile| profile.model_id == device.model_id)
            });
        if let Some(index) = existing {
            index
        } else {
            // An explicit stale page must not be silently replaced with a newly generated page
            if !page_id.is_empty() {
                return Err("Assignment page is missing".into());
            }
            let page = uuid::Uuid::new_v4().to_string();
            let profile = Profile {
                linked_controls: Default::default(),
                id: uuid::Uuid::new_v4().to_string(),
                name: DEFAULT_PROFILE_NAME.into(),
                model_id: device.model_id.clone(),
                active_page: page.clone(),
                pages: vec![Page {
                    id: page,
                    name: "Main page".into(),
                    bindings: Default::default(),
                }],
                palette: None,
                parameters: vec![],
                background_asset: None,
                background_media: Default::default(),
            };
            if data.profiles.is_empty() {
                data.active_profile = profile.id.clone();
            }
            data.profiles.push(profile);
            data.profiles.len() - 1
        }
    };
    let profile = &data.profiles[profile_index];
    let page = if !page_id.is_empty() {
        profile
            .pages
            .iter()
            .find(|page| page.id == page_id)
            .ok_or("Assignment page is missing")?
    } else {
        let session_page = selected
            .map(|device| state.session(&device.id))
            .filter(|session| session.profile_id == profile.id)
            .map(|session| session.page_id);
        profile
            .pages
            .iter()
            .find(|page| session_page.as_deref() == Some(page.id.as_str()))
            .or_else(|| {
                profile
                    .pages
                    .iter()
                    .find(|page| page.id == profile.active_page)
            })
            .ok_or("Assignment page is missing")?
    };
    Ok((profile.id.clone(), page.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        workspace::{AssignmentScope, Edit},
        Binding, Runtime,
    };
    use std::{collections::BTreeMap, sync::Arc};

    // Use a disposable runtime with one connected simulator and no saved profiles
    fn fixture() -> (tempfile::TempDir, Arc<Runtime>) {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime
            .simulate_device(true, "simulator:assignment")
            .unwrap();
        let snapshot = runtime.snapshot();
        runtime
            .apply_edits(
                snapshot.configuration_revision,
                "Empty fixture",
                snapshot
                    .profiles
                    .iter()
                    .map(|profile| Edit::DeleteProfile {
                        id: profile.id.clone(),
                    })
                    .collect(),
            )
            .unwrap();
        (root, runtime)
    }

    // Construct a valid assignment without invoking the action
    fn binding() -> Binding {
        serde_json::from_value(serde_json::json!({"title":"Assigned","color":"#00000000","action":{"kind":"volume","delta":1}})).unwrap()
    }

    // Represent an actual assignment with implicit profile and page selection
    fn edit(control: &str) -> Edit {
        Edit::AssignControl {
            profile_id: String::new(),
            page_id: String::new(),
            control_id: control.into(),
            scope: AssignmentScope::Page,
            binding: binding(),
        }
    }

    #[test]
    // Commit Default creation and the first assignment as one revision and one undoable edit
    fn implicit_assignment_creates_default_atomically_and_reuses_it() {
        let (_root, runtime) = fixture();
        let before = runtime.snapshot();
        let batch = runtime
            .apply_workspace_edits_for_device(
                before.configuration_revision,
                "First assignment",
                vec![edit("key-1")],
                Some("simulator:assignment"),
            )
            .unwrap();
        let after = runtime.snapshot();
        assert_eq!(
            after.configuration_revision,
            before.configuration_revision + 1
        );
        assert_eq!(after.profiles.len(), 1);
        let profile = &after.profiles[0];
        assert_eq!(profile.name, DEFAULT_PROFILE_NAME);
        assert!(profile.pages[0].bindings.contains_key("key-1"));
        assert_eq!(after.session("simulator:assignment").profile_id, profile.id);
        assert!(serde_json::to_string(&batch).unwrap().contains(&profile.id));
        runtime
            .apply_edits(
                after.configuration_revision,
                "Second assignment",
                vec![Edit::Bindings {
                    profile_id: String::new(),
                    page_id: String::new(),
                    bindings: BTreeMap::from([("key-2".into(), Some(binding()))]),
                }],
            )
            .unwrap();
        let second = runtime.snapshot();
        assert_eq!(second.profiles.len(), 1);
        assert_eq!(second.profiles[0].id, profile.id);
        runtime
            .history_action("undo", second.configuration_revision)
            .unwrap();
        let undo = runtime.snapshot();
        assert!(!undo.profiles[0].pages[0].bindings.contains_key("key-2"));
        runtime
            .history_action("undo", undo.configuration_revision)
            .unwrap();
        assert!(runtime.snapshot().profiles.is_empty());
        runtime.shutdown();
    }

    #[test]
    // Invalid assignments and stale revisions must leave no newly created profile behind
    fn failures_and_removal_only_edits_do_not_create_defaults() {
        let (_root, runtime) = fixture();
        let before = runtime.snapshot();
        for edits in [
            vec![edit("missing-control")],
            vec![],
            vec![edit("key-1"); 101],
            vec![Edit::Bindings {
                profile_id: String::new(),
                page_id: String::new(),
                bindings: BTreeMap::from([("key-1".into(), None)]),
            }],
        ] {
            assert!(runtime
                .apply_workspace_edits_for_device(
                    before.configuration_revision,
                    "Invalid",
                    edits,
                    Some("simulator:assignment")
                )
                .is_err());
            assert!(runtime.snapshot().profiles.is_empty());
        }
        assert!(runtime
            .apply_workspace_edits_for_device(
                before.configuration_revision + 1,
                "Stale",
                vec![edit("key-1")],
                Some("simulator:assignment")
            )
            .is_err());
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        runtime.shutdown();
    }

    #[test]
    // Never redirect explicit stale IDs or choose among multiple connected devices
    fn explicit_missing_and_ambiguous_targets_are_rejected() {
        let (_root, runtime) = fixture();
        let mut state = runtime.snapshot();
        let mut data = ProfileData::default();
        data.profiles.clear();
        data.active_profile.clear();
        assert!(resolve(&state, &mut data, Some("missing"), "", "").is_err());
        assert!(resolve(
            &state,
            &mut data,
            Some("simulator:assignment"),
            "missing",
            ""
        )
        .is_err());
        assert!(resolve(
            &state,
            &mut data,
            Some("simulator:assignment"),
            "",
            "missing"
        )
        .is_err());
        let mut second = state.devices[0].clone();
        second.id = "other".into();
        second.persistence_key = "other".into();
        state.devices.push(second);
        assert!(resolve(&state, &mut data, None, "", "").is_err());
        state.devices[0].connected = false;
        assert!(resolve(&state, &mut data, Some("simulator:assignment"), "", "").is_err());
        assert!(data.profiles.is_empty());
        runtime.shutdown();
    }

    #[test]
    // A selected device with no compatible profile gets its own Default without touching another model
    fn selected_model_does_not_overwrite_an_unrelated_profile() {
        let (_root, runtime) = fixture();
        let mut state = runtime.snapshot();
        let mut data = ProfileData::default();
        let old = serde_json::to_value(&data.profiles[0]).unwrap();
        state.devices[0].model_id = "k1-pro".into();
        let (profile, _) =
            resolve(&state, &mut data, Some("simulator:assignment"), "", "").unwrap();
        assert_eq!(data.profiles.len(), 2);
        assert_eq!(serde_json::to_value(&data.profiles[0]).unwrap(), old);
        assert_eq!(data.profiles[1].id, profile);
        assert_eq!(data.profiles[1].model_id, "k1-pro");
        assert_eq!(data.profiles[1].name, DEFAULT_PROFILE_NAME);
        assert!(resolve(
            &state,
            &mut data,
            Some("simulator:assignment"),
            "default",
            "main"
        )
        .is_err());
        runtime.shutdown();
    }

    #[test]
    // Explicit saved destinations remain editable offline when no connected-device target was requested
    fn explicit_profile_without_device_preserves_offline_editing() {
        let (_root, runtime) = fixture();
        runtime
            .apply_edits(
                runtime.snapshot().configuration_revision,
                "First assignment",
                vec![edit("key-1")],
            )
            .unwrap();
        let saved = runtime.snapshot();
        let profile = &saved.profiles[0];
        runtime.state.lock().unwrap().devices[0].connected = false;
        let assignment = Edit::Bindings {
            profile_id: profile.id.clone(),
            page_id: profile.active_page.clone(),
            bindings: BTreeMap::from([("key-2".into(), Some(binding()))]),
        };
        assert!(runtime
            .apply_workspace_edits_for_device(
                saved.configuration_revision,
                "Explicit disconnected device",
                vec![assignment.clone()],
                Some("simulator:assignment")
            )
            .is_err());
        let after = runtime
            .apply_edits(
                saved.configuration_revision,
                "Offline explicit profile",
                vec![assignment],
            )
            .unwrap();
        assert_eq!(after.profiles.len(), 1);
        assert!(after.profiles[0].pages[0].bindings.contains_key("key-2"));
        runtime.shutdown();
    }
}
