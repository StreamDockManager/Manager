//! Disposable lifecycle tests: authentic archive bytes, independently authored HTML fixture
#![cfg(any(unix, windows))]

use super::*;
use serde_json::{json, Value};
use std::sync::Arc;

const PLUGIN: &str = "com.elgato.lightsout";
const DEVICE: &str = "simulator:m3:vendor-profile-tests";
const ARCHIVE: &[u8] = include_bytes!(
    "../../../tests/fixtures/plugins/vendor/elgato/lights-out-profiles/sd.streamDeckProfile"
);

// Construct an isolated vendor profiles tests fixture
fn fixture(declarations: Value) -> (tempfile::TempDir, Arc<Runtime>) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("com.elgato.lightsout.sdPlugin");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join("index.html"),
        "<!doctype html><title>Inert profile test fixture</title>",
    )
    .unwrap();
    // This is not an adaptation of the upstream SDK3 executable manifest
    let manifest = json!({
        "UUID":PLUGIN,"Name":"Owned vendor profile test fixture","Version":"0.1.0.0",
        "Author":"Stream Dock Manager contributors","Description":"Disposable inert test package",
        "Icon":"icon","SDKVersion":2,"CodePath":"index.html",
        "Software":{"MinimumVersion":"6.9"},
        "OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],
        "Actions":[
            {"UUID":"com.elgato.lightsout.gamepiece","Name":"Play Piece","Icon":"icon","Controllers":["Keypad"],"States":[{"Image":"icon"},{"Image":"icon"}]},
            {"UUID":"com.elgato.lightsout.reset","Name":"Reset","Icon":"icon","Controllers":["Keypad"],"States":[{"Image":"icon"}]}
        ],"Profiles":declarations
    });
    for declaration in manifest["Profiles"].as_array().unwrap() {
        let name = declaration["Name"].as_str().unwrap();
        std::fs::write(source.join(format!("{name}.streamDeckProfile")), ARCHIVE).unwrap();
    }
    std::fs::write(
        source.join("icon.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>",
    )
    .unwrap();
    std::fs::write(
        source.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let runtime = Runtime::build(root.path().join("manager"), false).unwrap();
    let review = runtime.prepare_plugin_package(&source).unwrap();
    runtime
        .commit_plugin_package(&review.id, false, false)
        .unwrap();
    runtime.simulate_device(true, DEVICE).unwrap();
    (root, runtime)
}
// Construct the ordinary fixture for vendor profiles tests
fn ordinary() -> Value {
    json!([{"Name":"sd","DeviceType":0}])
}
// Construct the one fixture for vendor profiles tests
fn one(runtime: &Runtime) -> VendorProfileReview {
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(pending.warnings.is_empty(), "{:?}", pending.warnings);
    assert_eq!(pending.reviews.len(), 1);
    pending.reviews[0].clone()
}
// Construct the durable fixture for vendor profiles tests
fn durable(runtime: &Runtime) -> (u64, Value, Value) {
    (
        runtime.profiles.storage.generation(),
        serde_json::to_value(runtime.snapshot().profiles).unwrap(),
        serde_json::to_value(runtime.snapshot().sessions).unwrap(),
    )
}
#[test]
// Verify that review and decline are read only and do not execute plugin actions
fn review_and_decline_are_read_only_and_do_not_execute_plugin_actions() {
    let (_root, runtime) = fixture(ordinary());
    let before = durable(&runtime);
    let review = one(&runtime);
    assert_eq!(review.binding_count, 15);
    assert_eq!(durable(&runtime), before);
    runtime
        .resolve_vendor_profile_review(&review.id, false)
        .unwrap();
    assert_eq!(durable(&runtime), before);
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());
    assert!(runtime.vendor_index().unwrap().installed.is_empty());
    assert!(runtime
        .resolve_vendor_profile_review(&review.id, true)
        .is_err());
}
#[test]
// Verify that acceptance commits fresh profile mapping and selection together
fn acceptance_commits_fresh_profile_mapping_and_selection_together() {
    let (_root, runtime) = fixture(ordinary());
    let before = runtime.snapshot();
    let prior = before.session(DEVICE);
    let review = one(&runtime);
    let after = runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    assert_eq!(after.profiles.len(), before.profiles.len() + 1);
    assert_eq!(
        after.configuration_revision,
        before.configuration_revision + 1
    );
    let index = runtime.vendor_index().unwrap();
    assert_eq!(index.installed.len(), 1);
    let entry = &index.installed[0];
    let profile = after
        .profiles
        .iter()
        .find(|p| p.id == entry.profile_id)
        .unwrap();
    assert!(before.profiles.iter().all(|p| p.id != profile.id));
    assert_ne!(profile.pages[0].id, "cca21b93-64c5-4578-8a58-a6c35df9a5f2");
    assert_eq!(profile.pages[0].bindings.len(), 15);
    assert_eq!(after.session(DEVICE).profile_id, profile.id);
    assert_eq!(entry.archive_sha256, digest(ARCHIVE));
    assert_eq!(
        index.previous[&entry.device_key].profile_id,
        prior.profile_id
    );
    for binding in profile.pages[0].bindings.values() {
        let value = serde_json::to_value(binding).unwrap();
        assert_eq!(value["action"]["plugin_id"], PLUGIN);
        assert!(value["plugin_appearance"]["vendor_profile"].is_object());
    }
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());
    let persisted: Value = runtime
        .profiles
        .storage
        .read_json(Domain::LocalPreferences, "sessions", 16 * LIMIT)
        .unwrap()
        .unwrap();
    assert_eq!(persisted[&entry.device_key]["profile_id"], profile.id);
}
#[test]
// Verify that dont auto switch preserves selection and has no return stack
fn dont_auto_switch_preserves_selection_and_has_no_return_stack() {
    let (_root, runtime) =
        fixture(json!([{"Name":"sd","DeviceType":0,"DontAutoSwitchWhenInstalled":true}]));
    let before = runtime.snapshot().session(DEVICE);
    let review = one(&runtime);
    assert!(!review.switch_after_install);
    let after = runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    assert_eq!(after.session(DEVICE).profile_id, before.profile_id);
    assert!(runtime.vendor_index().unwrap().previous.is_empty());
    assert_eq!(runtime.vendor_index().unwrap().installed.len(), 1);
}
#[test]
// Verify that auto install false is lazy and unregistered requests are rejected
fn auto_install_false_is_lazy_and_unregistered_requests_are_rejected() {
    let (_root, runtime) = fixture(json!([{"Name":"sd","DeviceType":0,"AutoInstall":false}]));
    let before = durable(&runtime);
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());
    assert!(runtime
        .stage_vendor_profile(PLUGIN, "sd", DEVICE, Some("unregistered"))
        .is_err());
    assert_eq!(durable(&runtime), before);
    runtime
        .stage_vendor_profile(PLUGIN, "sd", DEVICE, None)
        .unwrap();
    assert_eq!(one(&runtime).manifest_name, "sd");
    assert_eq!(durable(&runtime), before);
}
#[test]
// Verify that readonly profile warns without writing
fn readonly_profile_warns_without_writing() {
    let (_root, runtime) = fixture(json!([{"Name":"sd","DeviceType":0,"Readonly":true}]));
    let before = durable(&runtime);
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(pending.reviews.is_empty());
    assert!(pending.warnings.iter().any(|w| w.contains("Readonly")));
    assert_eq!(durable(&runtime), before);
}
#[test]
// Verify that stale workspace or disconnected device cannot commit
fn stale_workspace_or_disconnected_device_cannot_commit() {
    for disconnect in [false, true] {
        let (_root, runtime) = fixture(ordinary());
        let review = one(&runtime);
        if disconnect {
            runtime.simulate_device(false, DEVICE).unwrap();
        } else {
            let snapshot = runtime.snapshot();
            let mut profile = snapshot.profiles[0].clone();
            profile.name.push_str(" changed");
            runtime
                .save_profile(profile, snapshot.configuration_revision)
                .unwrap();
        }
        let before = durable(&runtime);
        assert!(runtime
            .resolve_vendor_profile_review(&review.id, true)
            .is_err());
        assert_eq!(durable(&runtime), before);
        assert!(runtime.vendor_index().unwrap().installed.is_empty());
    }
}
#[test]
// Verify that archive change between review and acceptance rejects without writes
fn archive_change_between_review_and_acceptance_rejects_without_writes() {
    let (_root, runtime) = fixture(ordinary());
    let review = one(&runtime);
    let directory = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(PLUGIN)
        .unwrap();
    std::fs::write(
        directory.join("sd.streamDeckProfile"),
        b"replaced package bytes",
    )
    .unwrap();
    let before = durable(&runtime);
    assert!(runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap_err()
        .contains("package changed"));
    assert_eq!(durable(&runtime), before);
    assert!(runtime.vendor_index().unwrap().installed.is_empty());
}
#[test]
// Verify that generic profile deletion prunes owned mapping
fn generic_profile_deletion_prunes_owned_mapping() {
    let (_root, runtime) = fixture(ordinary());
    let review = one(&runtime);
    let snapshot = runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    let id = runtime.vendor_index().unwrap().installed[0]
        .profile_id
        .clone();
    runtime
        .delete_profile(&id, snapshot.configuration_revision)
        .unwrap();
    assert!(runtime.vendor_index().unwrap().installed.is_empty());
}
#[test]
// Verify that matching user display name never grants vendor ownership
fn matching_user_display_name_never_grants_vendor_ownership() {
    let (_root, runtime) = fixture(ordinary());
    let snapshot = runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    let user_id = profile.id.clone();
    profile.name = "Lights Out".into();
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    let review = one(&runtime);
    runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    assert_ne!(
        runtime.vendor_index().unwrap().installed[0].profile_id,
        user_id
    );
}
#[test]
// Verify that review limit batches and malformed siblings do not starve valid profiles
fn review_limit_batches_and_malformed_siblings_do_not_starve_valid_profiles() {
    let declarations: Vec<_> = (0..20)
        .map(|i| json!({"Name":format!("profile-{i:02}"),"DeviceType":0}))
        .collect();
    let (_root, runtime) = fixture(json!(declarations));
    let directory = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(PLUGIN)
        .unwrap();
    std::fs::write(directory.join("profile-00.streamDeckProfile"), b"malformed").unwrap();
    let before = durable(&runtime);
    let first = runtime.pending_vendor_profile_reviews().unwrap();
    assert_eq!(first.reviews.len(), 16);
    assert_eq!(first.warnings.len(), 1);
    for review in first.reviews {
        runtime
            .resolve_vendor_profile_review(&review.id, false)
            .unwrap();
    }
    let second = runtime.pending_vendor_profile_reviews().unwrap();
    assert_eq!(second.reviews.len(), 3);
    assert_eq!(durable(&runtime), before);
}
#[test]
// Verify that provenance index rejects missing unknown duplicate and invalid hash fields
fn provenance_index_rejects_missing_unknown_duplicate_and_invalid_hash_fields() {
    let valid = json!({"installed":[{"plugin_id":PLUGIN,"manifest_name":"sd","device_key":DEVICE,"model_id":"n3","vendor_device_model":"20GAA9901","installation":"a".repeat(64),"manifest_sha256":"b".repeat(64),"archive_sha256":"c".repeat(64),"profile_id":"p","page_id":"page"}],"previous":{}});
    let index: Index = serde_json::from_value(valid.clone()).unwrap();
    index.validate().unwrap();
    for value in [
        json!({}),
        json!({"installed":[],"previous":{},"extra":true}),
        json!([]),
        json!(null),
    ] {
        assert!(serde_json::from_value::<Index>(value).is_err());
    }
    let mut invalid = valid.clone();
    invalid["installed"][0]["installation"] = json!("bad");
    assert!(serde_json::from_value::<Index>(invalid)
        .unwrap()
        .validate()
        .is_err());
    let mut duplicate = valid;
    let entry = duplicate["installed"][0].clone();
    duplicate["installed"].as_array_mut().unwrap().push(entry);
    assert!(serde_json::from_value::<Index>(duplicate)
        .unwrap()
        .validate()
        .is_err());
}

#[test]
// Verify that replace import cannot reuse owned IDs to keep vendor provenance
fn replace_import_cannot_reuse_owned_ids_to_keep_vendor_provenance() {
    let (root, runtime) = fixture(ordinary());
    let review = one(&runtime);
    let snapshot = runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    let id = runtime.vendor_index().unwrap().installed[0]
        .profile_id
        .clone();
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == id)
        .unwrap()
        .clone();
    profile.name = "Unrelated user replacement".into();
    profile.pages[0].bindings.clear();
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    // Writable vendor profiles permit ordinary user customization
    assert_eq!(runtime.vendor_index().unwrap().installed.len(), 1);
    let archive = root.path().join("replacement.zip");
    runtime.export_profiles(&archive).unwrap();
    let preview = runtime.prepare_profile_import(&archive).unwrap();
    let preview = runtime
        .update_profile_import(&preview.id, "replace", vec![])
        .unwrap();
    runtime
        .commit_profile_import(&preview.id, preview.expected_revision)
        .unwrap();
    let index = runtime.vendor_index().unwrap();
    assert!(index.installed.is_empty());
    assert!(index.previous.is_empty());
    assert!(runtime.snapshot().profiles.iter().any(|p| p.id == id));
}

#[test]
// Verify that navigation after review rejects stale selection without writes
fn navigation_after_review_rejects_stale_selection_without_writes() {
    let (_root, runtime) = fixture(ordinary());
    let snapshot = runtime.snapshot();
    let selected = snapshot.session(DEVICE);
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == selected.profile_id)
        .unwrap()
        .clone();
    let mut second = profile.pages[0].clone();
    second.id = uuid::Uuid::new_v4().to_string();
    let page_id = second.id.clone();
    profile.pages.push(second);
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    let review = one(&runtime);
    runtime
        .select_device_profile(DEVICE, &selected.profile_id, Some(&page_id), true)
        .unwrap();
    let before = durable(&runtime);
    assert!(runtime
        .resolve_vendor_profile_review(&review.id, true)
        .is_err());
    assert_eq!(durable(&runtime), before);
    assert!(runtime.vendor_index().unwrap().installed.is_empty());
}

#[cfg(unix)]
#[test]
// Verify that symlink archive cannot be read as an owned package profile
fn symlink_archive_cannot_be_read_as_an_owned_package_profile() {
    let (root, runtime) = fixture(ordinary());
    let directory = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(PLUGIN)
        .unwrap();
    let external = root.path().join("external.streamDeckProfile");
    std::fs::write(&external, ARCHIVE).unwrap();
    let archive = directory.join("sd.streamDeckProfile");
    std::fs::remove_file(&archive).unwrap();
    std::os::unix::fs::symlink(&external, &archive).unwrap();
    let before = durable(&runtime);
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(pending.reviews.is_empty());
    assert!(!pending.warnings.is_empty());
    assert_eq!(durable(&runtime), before);
}

#[test]
// Verify that ordinary navigation clears vendor return chain even when automatic
fn ordinary_navigation_clears_vendor_return_chain_even_when_automatic() {
    for manual in [false, true] {
        let (_root, runtime) = fixture(ordinary());
        let previous = runtime.snapshot().session(DEVICE);
        let review = one(&runtime);
        runtime
            .resolve_vendor_profile_review(&review.id, true)
            .unwrap();
        assert_eq!(runtime.vendor_index().unwrap().previous.len(), 1);
        runtime
            .select_device_profile(
                DEVICE,
                &previous.profile_id,
                Some(&previous.page_id),
                manual,
            )
            .unwrap();
        let index = runtime.vendor_index().unwrap();
        assert!(
            index.previous.is_empty(),
            "Ordinary navigation must invalidate the vendor return chain, manual={manual}"
        );
        assert_eq!(
            index.installed.len(),
            1,
            "Navigation must preserve installed ownership"
        );
        assert_eq!(
            runtime.snapshot().session(DEVICE).profile_id,
            previous.profile_id
        );
    }
}

#[test]
// Verify that queued admission rejects navigation and reconnection before session or staging
fn queued_admission_rejects_navigation_and_reconnection_before_session_or_staging() {
    for reconnect in [false, true] {
        let (_root, runtime) = fixture(json!([{"Name":"sd","DeviceType":0,"AutoInstall":false}]));
        let snapshot = runtime.snapshot();
        let selected = snapshot.session(DEVICE);
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == selected.profile_id)
            .unwrap()
            .clone();
        let mut page = profile.pages[0].clone();
        page.id = uuid::Uuid::new_v4().to_string();
        let next_page = page.id.clone();
        profile.pages.push(page);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let snapshot = runtime.snapshot();
        let device = snapshot.devices.iter().find(|d| d.id == DEVICE).unwrap();
        let admission =
            Admission::capture(device, &snapshot, runtime.connection_generation(DEVICE));
        admission
            .check(device, &snapshot, runtime.connection_generation(DEVICE))
            .unwrap();
        let effect = stream_dock_manager_plugin_host::ProfileSwitchEffect {
            plugin: PLUGIN.into(),
            session: "deliberately-unregistered-session".into(),
            device: DEVICE.into(),
            profile: Some("sd".into()),
            page: None,
        };
        // With fresh admission, validation reaches the deliberately invalid session
        assert_eq!(
            runtime
                .handle_vendor_profile_switch_admitted(effect.clone(), Some(&admission))
                .unwrap_err(),
            "Plugin connection changed"
        );
        if reconnect {
            runtime.simulate_device(false, DEVICE).unwrap();
            runtime.simulate_device(true, DEVICE).unwrap();
        } else {
            runtime
                .select_device_profile(DEVICE, &selected.profile_id, Some(&next_page), true)
                .unwrap();
        }
        let before = durable(&runtime);
        let current = runtime.snapshot();
        let current_device = current.devices.iter().find(|d| d.id == DEVICE).unwrap();
        assert!(admission
            .check(
                current_device,
                &current,
                runtime.connection_generation(DEVICE)
            )
            .unwrap_err()
            .contains("Queued profile request became stale"));
        let error = runtime
            .handle_vendor_profile_switch_admitted(effect, Some(&admission))
            .unwrap_err();
        assert!(
            error.contains("Queued profile request became stale"),
            "{error}"
        );
        // Recapturing the current selection inside staging cannot freshen old admission
        for session in [None, Some("deliberately-unregistered-session")] {
            let error = runtime
                .stage_vendor_profile_admitted(PLUGIN, "sd", DEVICE, session, Some(&admission))
                .unwrap_err();
            assert!(
                error.contains("Queued profile request became stale"),
                "{error}"
            );
        }
        assert!(runtime
            .vendor_profile_reviews
            .lock()
            .unwrap()
            .staged
            .is_empty());
        assert!(runtime.vendor_index().unwrap().installed.is_empty());
        assert_eq!(durable(&runtime), before);
    }
}

#[test]
// Verify that actual queue preserves pre navigation admission until dequeue
fn actual_queue_preserves_pre_navigation_admission_until_dequeue() {
    for reconnect in [false, true] {
        let (_root, runtime) = fixture(json!([{"Name":"sd","DeviceType":0,"AutoInstall":false}]));
        let snapshot = runtime.snapshot();
        let selected = snapshot.session(DEVICE);
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == selected.profile_id)
            .unwrap()
            .clone();
        let mut page = profile.pages[0].clone();
        page.id = uuid::Uuid::new_v4().to_string();
        let next_page = page.id.clone();
        profile.pages.push(page);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let effect = stream_dock_manager_plugin_host::ProfileSwitchEffect {
            plugin: PLUGIN.into(),
            session: "deliberately-unregistered-session".into(),
            device: DEVICE.into(),
            profile: Some("sd".into()),
            page: None,
        };
        // Reserve the worker flag so the real enqueue path captures admission,
        // but no background dequeue can race the controlled intervening change
        runtime.vendor_profile_switches.lock().unwrap().running = true;
        runtime.queue_vendor_profile_switch(effect);
        assert_eq!(
            runtime
                .vendor_profile_switches
                .lock()
                .unwrap()
                .pending
                .len(),
            1
        );
        if reconnect {
            runtime.simulate_device(false, DEVICE).unwrap();
            runtime.simulate_device(true, DEVICE).unwrap();
        } else {
            runtime
                .select_device_profile(DEVICE, &selected.profile_id, Some(&next_page), true)
                .unwrap();
        }
        let queued = {
            let mut queue = runtime.vendor_profile_switches.lock().unwrap();
            let queued = queue.pending.pop_first().unwrap().1;
            queue.running = false;
            queued
        };
        let before = durable(&runtime);
        let error = runtime
            .handle_vendor_profile_switch_admitted(queued.effect, Some(&queued.admission))
            .unwrap_err();
        assert!(
            error.contains("Queued profile request became stale"),
            "{error}"
        );
        assert!(runtime
            .vendor_profile_reviews
            .lock()
            .unwrap()
            .staged
            .is_empty());
        assert!(runtime.vendor_index().unwrap().installed.is_empty());
        assert!(runtime
            .vendor_profile_switches
            .lock()
            .unwrap()
            .pending
            .is_empty());
        assert_eq!(durable(&runtime), before);
    }
}

#[test]
// Verify that expired full review cache is pruned before candidate capacity checks
fn expired_full_review_cache_is_pruned_before_candidate_capacity_checks() {
    let declarations: Vec<_> = (0..17)
        .map(|i| json!({"Name":format!("profile-{i:02}"),"DeviceType":0}))
        .collect();
    let (_root, runtime) = fixture(json!(declarations));
    let before = durable(&runtime);
    let first = runtime.pending_vendor_profile_reviews().unwrap();
    assert_eq!(first.reviews.len(), 16);
    let old_ids: BTreeSet<_> = first.reviews.iter().map(|r| r.id.clone()).collect();
    {
        let mut reviews = runtime.vendor_profile_reviews.lock().unwrap();
        for stage in reviews.staged.values_mut() {
            stage.created = Instant::now() - Duration::from_secs(3601);
        }
    }
    let fresh = runtime.pending_vendor_profile_reviews().unwrap();
    assert_eq!(fresh.reviews.len(), 16);
    assert!(fresh.warnings.is_empty());
    assert!(fresh.reviews.iter().all(|r| !old_ids.contains(&r.id)));
    assert!(runtime
        .vendor_profile_reviews
        .lock()
        .unwrap()
        .staged
        .values()
        .all(|s| s.created.elapsed() < Duration::from_secs(3600)));
    assert_eq!(durable(&runtime), before);
}

#[test]
// Verify that removed and replaced installation cache entries do not wedge new reviews
fn removed_and_replaced_installation_cache_entries_do_not_wedge_new_reviews() {
    let (_root, runtime) = fixture(ordinary());
    let device_key = runtime
        .snapshot()
        .devices
        .iter()
        .find(|d| d.id == DEVICE)
        .unwrap()
        .persistence_key
        .clone();
    let before = durable(&runtime);
    let mut old_keys = Vec::new();
    {
        let mut reviews = runtime.vendor_profile_reviews.lock().unwrap();
        for i in 0..128 {
            // Half are removed plugins; half refer to the installed plugin's
            // replaced installation. Manifest failures use the same key format
            let plugin = if i % 2 == 0 { "removed.plugin" } else { PLUGIN };
            let name = if i == 1 {
                "@manifest".into()
            } else {
                format!("old-{i}")
            };
            let key = review_key(plugin, &name, &device_key, "old-installation-token");
            reviews
                .failures
                .insert(key.clone(), "Obsolete package failure".into());
            reviews.dismissed.insert(key.clone());
            old_keys.push(key);
        }
        // This exact declaration was dismissed for a former installation
        let key = review_key(PLUGIN, "sd", &device_key, "old-installation-token");
        reviews.dismissed.insert(key.clone());
        old_keys.push(key);
    }
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert_eq!(pending.reviews.len(), 1);
    assert_eq!(pending.reviews[0].manifest_name, "sd");
    assert!(pending.warnings.is_empty(), "{:?}", pending.warnings);
    {
        let reviews = runtime.vendor_profile_reviews.lock().unwrap();
        for key in old_keys {
            assert!(!reviews.failures.contains_key(&key));
            assert!(!reviews.dismissed.contains(&key));
        }
    }
    assert_eq!(durable(&runtime), before);
}

#[test]
// Verify that requested review after automatic owner cap retains live installation
fn requested_review_after_automatic_owner_cap_retains_live_installation() {
    let (_root, runtime) = fixture(json!([{"Name":"sd","DeviceType":0,"AutoInstall":false}]));
    let source = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(PLUGIN)
        .unwrap();
    let plugins_root = source.parent().unwrap();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(source.join("manifest.json")).unwrap()).unwrap();
    // Add inert, disabled owners sorted ahead of the requested owner. This tests
    // the real inventory/scan boundary, not a separately reconstructed liveness set
    for i in 0..100 {
        let id = format!("com.fixture.owner{i:03}");
        let folder = plugins_root.join(format!("{id}.sdPlugin"));
        std::fs::create_dir(&folder).unwrap();
        let mut other = manifest.clone();
        other["UUID"] = json!(id);
        other["Name"] = json!(format!("A inert inventory fixture {i:03}"));
        other.as_object_mut().unwrap().remove("Profiles");
        for (index, action) in other["Actions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            action["UUID"] = json!(format!("{id}.action{index}"));
        }
        std::fs::write(
            folder.join("manifest.json"),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        for file in ["index.html", "icon.svg"] {
            std::fs::copy(source.join(file), folder.join(file)).unwrap();
        }
    }
    {
        let mut host = runtime.plugins.lock().unwrap();
        assert_eq!(host.reload().unwrap().added, 100);
        assert_eq!(
            host.list()
                .iter()
                .filter(|p| p.dialect == "streamdeck")
                .position(|p| p.id == PLUGIN),
            Some(100)
        );
        assert!(host.list().iter().all(|p| !p.enabled));
    }
    runtime
        .stage_vendor_profile(PLUGIN, "sd", DEVICE, None)
        .unwrap();
    let before = durable(&runtime);
    let review_id = runtime
        .vendor_profile_reviews
        .lock()
        .unwrap()
        .staged
        .values()
        .next()
        .unwrap()
        .review
        .id
        .clone();
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(pending.warnings.is_empty(), "{:?}", pending.warnings);
    assert_eq!(pending.reviews.len(), 1);
    assert_eq!(pending.reviews[0].id, review_id);
    assert_eq!(pending.reviews[0].plugin_id, PLUGIN);
    assert_eq!(durable(&runtime), before);
}
