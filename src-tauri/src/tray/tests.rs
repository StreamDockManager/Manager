use super::*;
use stream_dock_manager_runtime::DeviceInfo;

// Construct the device test fixture
fn device(id: &str, name: &str) -> DeviceInfo {
    let manifest = hardware_registry::manifests()
        .into_iter()
        .find(|m| m.model_id == "n4-pro")
        .unwrap();
    DeviceInfo {
        id: id.into(),
        persistence_key: format!("saved:{id}"),
        identity_port_dependent: false,
        model_id: manifest.model_id.clone(),
        name: name.into(),
        variant_id: "fixture".into(),
        firmware: "".into(),
        usb_release: None,
        connected: true,
        simulated: false,
        manifest,
    }
}
// Construct the snapshot test fixture
fn snapshot(devices: Vec<DeviceInfo>) -> Snapshot {
    Snapshot {
        binding_overrides: Default::default(),
        revision: 0,
        configuration_revision: 0,
        sessions: Default::default(),
        saved_macros: vec![],
        styles: vec![],
        rules: vec![],
        history: Default::default(),
        devices,
        profiles: vec![],
        active_profile: "".into(),
        plugins: vec![],
    }
}

#[test]
// Verify connected devices use their persisted sleep state and current capabilities
fn connected_devices_use_their_persisted_sleep_state_and_current_capabilities() {
    let a = device("usb:a", "N4 Pro");
    let mut b = device("usb:b", "M18");
    b.manifest.capabilities.sleep = false;
    let mut disconnected = device("usb:gone", "Disconnected");
    disconnected.connected = false;
    let mut state = snapshot(vec![a, b, disconnected]);
    let mut session = state.session("usb:a");
    session.output.sleeping = true;
    state.sessions.insert("saved:usb:a".into(), session);
    let entries = devices(&state);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "usb:b");
    assert!(!entries[0].supports_sleep);
    assert!(!entries[0].sleeping);
    assert!(entries[1].sleeping);
    assert!(entries[1].supports_sleep);
    // A wake performed outside the tray changes the menu model on the next output event
    state
        .sessions
        .get_mut("saved:usb:a")
        .unwrap()
        .output
        .sleeping = false;
    assert_ne!(entries, devices(&state));
    state.devices.clear();
    assert!(devices(&state).is_empty());
}

#[test]
// Verify duplicate devices have stable names and simulators are distinguished
fn duplicate_devices_have_stable_names_and_simulators_are_distinguished() {
    let mut simulator = device("simulator:n4-pro", "N4 Pro");
    simulator.simulated = true;
    let mut state = snapshot(vec![
        device("usb:b", "N4 Pro"),
        simulator,
        device("usb:a", "N4 Pro"),
    ]);
    let entries = devices(&state);
    assert_eq!(entries[0].label, "N4 Pro (Simulator)");
    assert_eq!(entries[1].id, "usb:a");
    assert_eq!(entries[1].label, "N4 Pro (1)");
    assert_eq!(entries[2].label, "N4 Pro (2)");
    state.devices.reverse();
    assert_eq!(devices(&state), entries);
}

#[test]
// Verify device actions preserve full IDs and record explicit sleep or wake intent
fn device_actions_preserve_full_ids_and_record_explicit_sleep_or_wake_intent() {
    let id = "usb:0483:1234:/path/设备";
    assert_eq!(
        device_action(&format!("tray:sleep:{id}")),
        Some(DeviceAction::Sleep(id, true))
    );
    assert_eq!(
        device_action(&format!("tray:wake:{id}")),
        Some(DeviceAction::Sleep(id, false))
    );
    assert_eq!(
        device_action(&format!("tray:open:{id}")),
        Some(DeviceAction::Open(id))
    );
    for invalid in [
        "show",
        "quit",
        "tray:device:usb:a",
        "tray:sleep:",
        "tray:wake:",
        "tray:open:",
    ] {
        assert_eq!(device_action(invalid), None);
    }
}

#[test]
// Verify an existing simulator label is not repeated
fn an_existing_simulator_label_is_not_repeated() {
    let mut simulator = device("simulator:n4-pro", "N4 Pro Simulator (preview)");
    simulator.simulated = true;
    assert_eq!(
        devices(&snapshot(vec![simulator]))[0].label,
        "N4 Pro Simulator (preview)"
    );
}

#[test]
// Verify page reordering updates menu order without changing selection
fn page_reordering_updates_menu_order_without_changing_selection() {
    let mut state = snapshot(vec![device("usb:a", "N4 Pro")]);
    let mut profile = stream_dock_manager_runtime::ProfileData::default()
        .profiles
        .remove(0);
    let first = profile.pages[0].id.clone();
    profile.pages.extend(
        ["second", "third"].map(|id| stream_dock_manager_runtime::Page {
            id: id.into(),
            name: id.into(),
            bindings: Default::default(),
        }),
    );
    state.active_profile = profile.id.clone();
    state.profiles.push(profile);
    let mut session = state.session("usb:a");
    session.page_id = "second".into();
    state.sessions.insert("saved:usb:a".into(), session);
    let before = devices(&state);
    state.profiles[0].pages.rotate_right(1);
    let after = devices(&state);
    assert_ne!(
        before, after,
        "reordering invalidates the cached tray model"
    );
    let pages = &after[0].profiles[0].pages;
    assert_eq!(
        pages.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
        ["third", &first, "second"]
    );
    assert_eq!(
        pages
            .iter()
            .filter(|p| p.2)
            .map(|p| p.0.as_str())
            .collect::<Vec<_>>(),
        ["second"]
    );
}

#[test]
// Verify profile edits and navigation refresh the taskbar listing
fn profile_edits_and_navigation_refresh_the_taskbar_listing() {
    for kind in [
        "profiles",
        "sessions",
        "devices",
        "device-output",
        "device-power",
        "plugins",
    ] {
        assert!(needs_refresh(kind), "{kind}");
    }
    for kind in ["plugin-log", "action-run", "performance"] {
        assert!(!needs_refresh(kind), "{kind}");
    }
}
