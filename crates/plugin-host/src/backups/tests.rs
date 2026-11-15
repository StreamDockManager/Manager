use super::*;
const ID: &str = "org.test.rollback";
// Construct the package representation
fn package(base: &Path, version: &str) -> PathBuf {
    let path = base.join(ID);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("manifest.json"),serde_json::to_vec(&json!({"UUID":ID,"Name":"Rollback fixture","Version":version,"CodePath":"runtime.html","SDKVersion":2,"Actions":[{"UUID":format!("{ID}.action"),"Name":"Action"}]})).unwrap()).unwrap();
    std::fs::write(
        path.join("runtime.html"),
        format!("<!doctype html><title>{version}</title>"),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    manifest["SDKVersion"] = json!(1);
    manifest["StreamDockManager"] = json!({"ManifestDialect":"vsd"});
    crate::lifecycle_tests::current_descriptor(&path, &mut manifest);
    path
}
// Construct the isolated test fixture
fn fixture() -> (tempfile::TempDir, PluginHost, String) {
    let root = tempfile::tempdir().unwrap();
    let mut host = PluginHost::new(root.path().join("plugins")).unwrap();
    host.set_registration_environment(
        crate::RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap(),
    );
    let one = package(&root.path().join("one"), "1.0.0");
    let prepared = PluginHost::prepare_install(&host.root, &one).unwrap();
    host.register_prepared(prepared).unwrap();
    let two = package(&root.path().join("two"), "2.0.0");
    let prepared = PluginHost::prepare_install(&host.root, &two).unwrap();
    host.replace_prepared(prepared).unwrap();
    let backups = host.plugin_backups(ID, 1).unwrap();
    assert_eq!(backups.total, 1);
    let id = backups.entries[0].id.clone();
    (root, host, id)
}
#[test]
// Verify resource warnings require the current running instance and do not count as crashes
fn resource_warnings_require_the_current_running_instance_and_do_not_count_as_crashes() {
    use crate::RuntimeState;
    let (_root, mut host, _) = fixture();
    let plugin = host.plugins.get_mut(ID).unwrap();
    plugin.info.enabled = true;
    plugin.info.runtime.state = RuntimeState::Running;
    plugin.info.runtime.run_id = Some("current-run".into());
    host.hub
        .lock()
        .unwrap()
        .run_details
        .insert(ID.into(), ("2.0.0".into(), Some("current-run".into())));
    assert!(!host.record_resource_warning(ID, "old-run", "old"));
    assert!(host.record_resource_warning(ID, "current-run", "Resource alert"));
    let h = host.hub.lock().unwrap();
    let health = &h.health[ID];
    assert_eq!(health.events.len(), 1);
    assert_eq!(health.events[0].severity, "warning");
    assert_eq!(health.events[0].run_id.as_deref(), Some("current-run"));
    assert!(health.recent_failures.is_empty());
    assert_eq!(health.totals.crashes, 0);
    assert_eq!(health.totals.failed_starts, 0);
    assert_eq!(health.totals.automatic_restarts, 0);
    drop(h);
    assert_eq!(host.plugins[ID].info.runtime.state, RuntimeState::Running);
    assert!(host.plugins[ID].child.is_none());
    host.plugins.get_mut(ID).unwrap().info.runtime.state = RuntimeState::Idle;
    assert!(!host.record_resource_warning(ID, "current-run", "after exit"));
}
#[test]
// Verify rollback preserves current manager settings replaced files and disabled state
fn rollback_preserves_current_manager_settings_replaced_files_and_disabled_state() {
    let (_root, mut host, backup) = fixture();
    let before = host.installation_token(ID).unwrap();
    {
        let mut h = host.hub.lock().unwrap();
        h.globals
            .insert(ID.into(), json!({"token":"latest global"}));
        h.action_defaults.insert(
            ID.into(),
            HashMap::from([("action".into(), json!({"location":"new location"}))]),
        );
        h.context_state.settings.insert(
            format!("{ID}:context"),
            json!({"unknown":{"preserve":false}}),
        );
        h.enabled.insert(ID.into());
        h.persist().unwrap();
    }
    let prepared = host.prepare_rollback(ID, &backup).unwrap();
    assert!(prepared.review.backup.recorded);
    assert!(!prepared.review.backup.trusted_origin);
    assert_eq!(prepared.review.backup.version.as_deref(), Some("1.0.0"));
    host.hub
        .lock()
        .unwrap()
        .globals
        .insert(ID.into(), json!({"token":"changed during review"}));
    host.hub.lock().unwrap().persist().unwrap();
    let claim = host.reserve_replacement(ID).unwrap();
    assert!(host.reserve_replacement(ID).is_err());
    host.stop_for_replacement(ID).unwrap();
    host.verify_rollback(&prepared.review, &prepared.backup_identity)
        .unwrap();
    let restored = host.restore_prepared(prepared.prepared).unwrap();
    drop(claim);
    assert!(!restored.enabled);
    assert_ne!(host.installation_token(ID).unwrap(), before);
    assert!(
        std::fs::read_to_string(host.root.join(ID).join("runtime.html"))
            .unwrap()
            .contains("1.0.0")
    );
    {
        let h = host.hub.lock().unwrap();
        assert_eq!(h.globals[ID]["token"], "changed during review");
        assert_eq!(h.action_defaults[ID]["action"]["location"], "new location");
        assert_eq!(
            h.context_state.settings[&format!("{ID}:context")]["unknown"]["preserve"],
            false
        );
        assert!(!h.enabled.contains(ID));
    }
    let backups = host.plugin_backups(ID, 1).unwrap();
    assert_eq!(backups.total, 2);
    assert!(backups
        .entries
        .iter()
        .any(|b| b.version.as_deref() == Some("2.0.0")));
    assert!(host.backup_path(ID, &backup).unwrap().is_dir());
    let root = host.root.clone();
    let token = host.installation_token(ID).unwrap();
    drop(host);
    let host = PluginHost::new(root).unwrap();
    assert_eq!(host.installation_token(ID).unwrap(), token);
    assert_eq!(host.plugin_backups(ID, 1).unwrap().total, 2);
    assert!(!host.list()[0].enabled);
}
#[test]
// Verify altered backup record files or installed identity reject a review
fn altered_backup_record_files_or_installed_identity_reject_a_review() {
    let (root, mut host, backup) = fixture();
    let stage = host.prepare_rollback(ID, &backup).unwrap();
    let original =
        std::fs::read(host.backup_path(ID, &backup).unwrap().join("runtime.html")).unwrap();
    std::fs::write(
        host.backup_path(ID, &backup).unwrap().join("runtime.html"),
        "altered",
    )
    .unwrap();
    assert!(host
        .verify_rollback(&stage.review, &stage.backup_identity)
        .is_err());
    assert!(host.prepare_rollback(ID, &backup).is_err());
    std::fs::write(
        host.backup_path(ID, &backup).unwrap().join("runtime.html"),
        original,
    )
    .unwrap();
    host.verify_rollback(&stage.review, &stage.backup_identity)
        .unwrap();
    let next = package(&root.path().join("next"), "2.0.0");
    let package = PluginHost::prepare_install(&host.root, &next).unwrap();
    host.replace_prepared(package).unwrap();
    assert!(host
        .verify_rollback(&stage.review, &stage.backup_identity)
        .unwrap_err()
        .contains("Installed package changed"));
    std::fs::write(
        host.root
            .join(".backup-index")
            .join(format!("{backup}.json")),
        "altered metadata",
    )
    .unwrap();
    assert!(host.prepare_rollback(ID, &backup).is_err());
    assert!(host
        .plugin_backups(ID, 1)
        .unwrap()
        .entries
        .iter()
        .find(|b| b.id == backup)
        .unwrap()
        .error
        .is_some());
}
#[test]
// Verify deleting backups is scoped and releases persisted receipts
fn deleting_backups_is_scoped_and_releases_persisted_receipts() {
    let (_root, host, backup) = fixture();
    let extra = uuid::Uuid::new_v4().to_string();
    let shared = host.root.join(".backups").join(&extra);
    std::fs::create_dir_all(shared.join(ID)).unwrap();
    let other = shared.join("org.test.other");
    std::fs::create_dir(&other).unwrap();
    let installed = host.installation_token(ID).unwrap();

    assert!(host.delete_plugin_backups(ID, None, false).is_err());
    assert!(host.delete_plugin_backups(ID, Some("../"), false).is_err());
    assert!(host.delete_plugin_backups(ID, Some(&backup), true).is_err());
    let claim = host.reserve_replacement(ID).unwrap();
    assert!(host.delete_plugin_backups(ID, None, true).is_err());
    drop(claim);
    assert_eq!(host.plugin_backups(ID, 1).unwrap().total, 2);

    assert_eq!(
        host.delete_plugin_backups(ID, Some(&backup), false)
            .unwrap(),
        1
    );
    assert_eq!(host.plugin_backups(ID, 1).unwrap().total, 1);
    assert!(!host
        .root
        .join(".backup-index")
        .join(format!("{backup}.json"))
        .exists());
    assert_eq!(host.delete_plugin_backups(ID, None, true).unwrap(), 1);
    assert!(other.is_dir());
    assert_eq!(host.plugin_backups("org.test.other", 1).unwrap().total, 1);
    assert_eq!(host.installation_token(ID).unwrap(), installed);
    let root = host.root.clone();
    drop(host);
    let host = PluginHost::new(root).unwrap();
    assert_eq!(host.plugin_backups(ID, 1).unwrap().total, 0);
    assert!(!host
        .hub
        .lock()
        .unwrap()
        .backup_receipts
        .contains_key(&backup));
}

#[test]
// Verify missing receipt metadata never grants package asserted provenance
fn missing_receipt_metadata_never_grants_package_asserted_provenance() {
    let (_root, mut host, backup) = fixture();
    std::fs::remove_file(
        host.root
            .join(".backup-index")
            .join(format!("{backup}.json")),
    )
    .unwrap();
    std::fs::write(
        host.backup_path(ID, &backup)
            .unwrap()
            .join(".stream-dock-manager-provenance.json"),
        serde_json::to_vec(
            &json!({"repository":"forged/official","channel":"stable","version":"1.0.0"}),
        )
        .unwrap(),
    )
    .unwrap();
    let stage = host.prepare_rollback(ID, &backup).unwrap();
    assert!(!stage.review.backup.recorded);
    assert!(!stage.review.backup.trusted_origin);
    assert!(stage.review.backup.provenance.is_none());
    host.restore_prepared(stage.prepared).unwrap();
    assert_eq!(host.provenance(ID).unwrap()["trust"], "unverified_rollback");
    assert!(host.provenance(ID).unwrap().get("repository").is_none());
}
#[test]
// Verify demand cannot restart a reserved plugin and claim drop rearms it
fn demand_cannot_restart_a_reserved_plugin_and_claim_drop_rearms_it() {
    let (_root, mut host, _backup) = fixture();
    // This replacement test models a plugin with a visible action
    host.visible_plugins.insert(ID.into());
    let claim = host.reserve_replacement(ID).unwrap();
    host.hub.lock().unwrap().enabled.insert(ID.into());
    host.required.insert(
        ID.into(),
        vec![PluginDemand {
            device_id: "device".into(),
            profile_id: "profile".into(),
        }],
    );
    host.reconcile();
    assert!(!host.eligible(ID));
    assert!(!host.startup.iter().any(|id| id == ID));
    assert!(host.launch(ID, true, false).is_err());
    drop(claim);
    host.reconcile();
    assert!(host.eligible(ID));
    assert!(host.startup.iter().any(|id| id == ID));
    host.enable(ID, false).unwrap();
}
#[cfg(unix)]
#[test]
// Verify linked backup or metadata files are rejected and preserved
fn linked_backup_or_metadata_files_are_rejected_and_preserved() {
    use std::os::unix::fs::symlink;
    let (root, host, backup) = fixture();
    let record = host
        .root
        .join(".backup-index")
        .join(format!("{backup}.json"));
    let bytes = std::fs::read(&record).unwrap();
    std::fs::remove_file(&record).unwrap();
    let external = root.path().join("external-record.json");
    std::fs::write(&external, &bytes).unwrap();
    symlink(&external, &record).unwrap();
    assert!(host.prepare_rollback(ID, &backup).is_err());
    assert_eq!(std::fs::read(&external).unwrap(), bytes);
    assert!(host.prepare_rollback(ID, "../outside").is_err());
}
