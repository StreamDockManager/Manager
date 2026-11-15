use crate::{credentials::*, parameter_credentials::*, parameters::*, *};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
#[derive(Default)]
struct Vault {
    values: Mutex<BTreeMap<String, String>>,
    locked: AtomicBool,
}
impl CredentialProvider for Vault {
    // Read the requested parameter credentials tests data
    fn read(&self, id: &str) -> Result<String, CredentialState> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(CredentialState::Locked);
        }
        self.values
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(CredentialState::Missing)
    }
    // Write the supplied parameter credentials tests data
    fn write(&self, id: &str, value: &str) -> Result<(), CredentialState> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(CredentialState::Locked);
        }
        self.values.lock().unwrap().insert(id.into(), value.into());
        Ok(())
    }
    // Remove the selected parameter credentials tests entry
    fn delete(&self, id: &str) -> Result<(), CredentialState> {
        self.values.lock().unwrap().remove(id);
        Ok(())
    }
}
// Construct an isolated parameter credentials tests fixture
fn fixture() -> (
    tempfile::TempDir,
    Arc<Runtime>,
    Arc<Vault>,
    CredentialTarget,
) {
    let root = tempfile::tempdir().unwrap();
    let vault = Arc::new(Vault::default());
    let runtime = Runtime::build_credentials(
        root.path().into(),
        false,
        Credentials::with_provider(vault.clone()),
    )
    .unwrap();
    let mut s = runtime.snapshot();
    s.profiles[0].parameters = vec![ProfileParameter {
        name: "token".into(),
        label: "API token".into(),
        r#type: "string".into(),
        required: false,
        default: None,
        sensitive: true,
    }];
    let target = CredentialTarget {
        profile_id: s.profiles[0].id.clone(),
        name: "token".into(),
        device_id: None,
    };
    runtime
        .save_profile(s.profiles.remove(0), s.configuration_revision)
        .unwrap();
    (root, runtime, vault, target)
}
// Write the supplied parameter credentials tests data
fn write(
    runtime: &Runtime,
    target: &CredentialTarget,
    value: &str,
) -> Result<CredentialState, String> {
    runtime.write_credential(CredentialWrite {
        target: target.clone(),
        expected_revision: runtime.snapshot().configuration_revision,
        value: value.into(),
    })
}
#[test]
// Verify that credentials are os held with local precedence and status only reads
fn credentials_are_os_held_with_local_precedence_and_status_only_reads() {
    let (_root, runtime, vault, target) = fixture();
    assert_eq!(
        runtime.credential_status(&target).unwrap(),
        CredentialState::Missing
    );
    write(&runtime, &target, "installation-secret").unwrap();
    let device = CredentialTarget {
        device_id: Some("device".into()),
        ..target.clone()
    };
    write(&runtime, &device, "device-secret").unwrap();
    assert_eq!(
        runtime
            .resolve_parameter("token", "device", &target.profile_id)
            .unwrap(),
        "device-secret"
    );
    assert_eq!(
        runtime
            .resolve_parameter("token", "other", &target.profile_id)
            .unwrap(),
        "installation-secret"
    );
    let public = serde_json::to_string(&runtime.parameter_overrides().unwrap()).unwrap();
    assert!(!public.contains("-secret"));
    assert!(!serde_json::to_string(&runtime.snapshot())
        .unwrap()
        .contains("-secret"));
    assert_eq!(
        runtime.credential_status(&target).unwrap(),
        CredentialState::Configured
    );
    let events = runtime.subscribe();
    runtime.emit(
        "plugin-log",
        None,
        None,
        Some("contains device-secret and installation-secret".into()),
    );
    assert_eq!(
        events.recv().unwrap().message.unwrap(),
        "contains [credential] and [credential]"
    );
    vault.locked.store(true, Ordering::SeqCst);
    assert_eq!(
        runtime.credential_status(&target).unwrap(),
        CredentialState::Locked
    );
    assert!(runtime
        .resolve_parameter("token", "device", &target.profile_id)
        .unwrap_err()
        .contains("locked"));
    vault.locked.store(false, Ordering::SeqCst);
    runtime
        .clear_credential(&device, runtime.snapshot().configuration_revision)
        .unwrap();
    assert_eq!(
        runtime
            .resolve_parameter("token", "device", &target.profile_id)
            .unwrap(),
        "installation-secret"
    );
    assert_eq!(vault.values.lock().unwrap().len(), 1);
    runtime.shutdown();
}
#[test]
// Verify that replacement preserves previous value on failure and rejects forged refs
fn replacement_preserves_previous_value_on_failure_and_rejects_forged_refs() {
    let (_root, runtime, vault, target) = fixture();
    write(&runtime, &target, "current-secret").unwrap();
    let stored = runtime.stored_parameter_overrides().unwrap();
    vault.locked.store(true, Ordering::SeqCst);
    assert!(write(&runtime, &target, "replacement-secret").is_err());
    assert_eq!(
        serde_json::to_value(runtime.stored_parameter_overrides().unwrap()).unwrap(),
        serde_json::to_value(&stored).unwrap()
    );
    let mut forged = stored.clone();
    forged
        .installation
        .get_mut(&target.profile_id)
        .unwrap()
        .insert(
            "token".into(),
            json!({"$credential":format!("parameter-{}",uuid::Uuid::new_v4())}),
        );
    assert!(runtime.save_parameter_overrides(forged).is_err());
    runtime
        .save_parameter_overrides(runtime.parameter_overrides().unwrap())
        .unwrap();
    vault.locked.store(false, Ordering::SeqCst);
    assert_eq!(
        runtime
            .resolve_parameter("token", "device", &target.profile_id)
            .unwrap(),
        "current-secret"
    );
    assert!(runtime
        .write_credential(CredentialWrite {
            target,
            expected_revision: 0,
            value: "stale-secret".into()
        })
        .is_err());
    assert_eq!(vault.values.lock().unwrap().len(), 1);
    runtime.shutdown();
}

#[test]
// Verify that failed local reference commit removes the new os entry
fn failed_local_reference_commit_removes_the_new_os_entry() {
    let (_root, runtime, vault, target) = fixture();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(write(&runtime, &target, "uncommitted-secret").is_err());
    assert!(vault.values.lock().unwrap().is_empty());
    assert_eq!(
        runtime.credential_status(&target).unwrap(),
        CredentialState::Missing
    );
    runtime.shutdown();
}
#[test]
// Verify that changing a sensitive definition cannot expose its existing override
fn changing_a_sensitive_definition_cannot_expose_its_existing_override() {
    let (_root, runtime, _vault, target) = fixture();
    write(&runtime, &target, "protected-secret").unwrap();
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.parameters[0].sensitive = false;
    assert!(runtime
        .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
        .unwrap_err()
        .contains("Clear credential overrides"));
    assert!(runtime.snapshot().profiles[0].parameters[0].sensitive);
    runtime
        .clear_credential(&target, runtime.snapshot().configuration_revision)
        .unwrap();
    runtime
        .save_profile(profile, runtime.snapshot().configuration_revision)
        .unwrap();
    runtime.shutdown();
}
