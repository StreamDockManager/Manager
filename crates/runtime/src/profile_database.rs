//! Database representation of the workspace. Public archives keep ProfileData v1.
use crate::profiles::{ProfileData, ProfileStore};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use stream_dock_manager_storage::{rusqlite::Connection, Domain, EncodedJson};

/// Closed inventory of current workspace preference records. These are SQL keys,
/// never filesystem paths. Removed keys are not aliases
#[derive(Clone, Copy, Debug)]
pub(crate) enum LocalPreference {
    Sessions,
    Drafts,
    ParameterOverrides,
    PluginProfilePrompts,
    LibraryOrganization,
    PluginUpdatePolicies,
    ImportRepairRecipes,
    Updates,
    BuiltinValues,
    DevicePower,
    ResourceAlerts,
    RestartRecoveryOnce,
    ConfigurationRecoveryBypass,
}
impl LocalPreference {
    pub(crate) const ALL: [Self; 13] = [
        Self::Updates,
        Self::BuiltinValues,
        Self::Sessions,
        Self::ParameterOverrides,
        Self::PluginProfilePrompts,
        Self::Drafts,
        Self::DevicePower,
        Self::ResourceAlerts,
        Self::LibraryOrganization,
        Self::PluginUpdatePolicies,
        Self::ImportRepairRecipes,
        Self::RestartRecoveryOnce,
        Self::ConfigurationRecoveryBypass,
    ];
    // Return the key identifying profile database
    pub(crate) const fn key(self) -> &'static str {
        self.metadata().0
    }
    // Live read/write and snapshot limits intentionally differ for import recipes
    // Do not reconcile that historical acceptance boundary as a mechanical refactor
    const fn metadata(self) -> (&'static str, usize, usize, bool) {
        match self {
            Self::Sessions => ("sessions", LIMIT, LIMIT, true),
            Self::Drafts => ("drafts", LIMIT, LIMIT, true),
            Self::ParameterOverrides => ("parameter-overrides", LIMIT, LIMIT, true),
            Self::PluginProfilePrompts => ("plugin-profile-prompts", LIMIT, LIMIT, true),
            Self::LibraryOrganization => ("library-organization", LIMIT, LIMIT, true),
            Self::PluginUpdatePolicies => ("plugin-update-policies", LIMIT, LIMIT, true),
            Self::ImportRepairRecipes => ("import-repair-recipes", LIMIT, 1024 * 1024, true),
            Self::Updates => ("updates", LIMIT, LIMIT, true),
            Self::BuiltinValues => ("builtin-values", LIMIT, LIMIT, true),
            Self::DevicePower => ("device-power", 1024 * 1024, 1024 * 1024, true),
            Self::ResourceAlerts => ("resource-alerts", 1024 * 1024, 1024 * 1024, true),
            Self::RestartRecoveryOnce => ("restart-recovery-once", 16, 16, false),
            Self::ConfigurationRecoveryBypass => ("configuration-recovery-bypass", 16, 16, false),
        }
    }
    // Record the new profile database observation
    pub(crate) fn record<T: serde::de::DeserializeOwned>(
        self,
    ) -> stream_dock_manager_storage::JsonRecord<T> {
        stream_dock_manager_storage::JsonRecord {
            domain: Domain::LocalPreferences,
            key: self.key(),
            max_bytes: self.metadata().1,
            decode: decode_record,
        }
    }
    // Build the typed snapshot record with its preference-specific byte limit
    pub(crate) fn snapshot_record<T: serde::de::DeserializeOwned>(
        self,
    ) -> stream_dock_manager_storage::JsonRecord<T> {
        stream_dock_manager_storage::JsonRecord {
            max_bytes: self.metadata().2,
            ..self.record()
        }
    }
}
// Decode record
pub(crate) fn decode_record<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|_| "Stored record has invalid domain data".into())
}
// Validate object
fn validate_object<T>(
    record: &stream_dock_manager_storage::JsonRecord<T>,
    connection: &Connection,
) -> Result<(), String> {
    if stream_dock_manager_storage::read_json::<serde_json::Value>(
        connection,
        record.domain,
        record.key,
        record.max_bytes,
    )?
    .is_some_and(|value| !value.is_object())
    {
        return Err(format!(
            "Invalid stored object in {}/{}; database preserved",
            record.domain.table(),
            record.key
        ));
    }
    Ok(())
}

pub(crate) const DELETED_MEDIA_RECORD: stream_dock_manager_storage::JsonRecord<
    crate::media_deletion::DeletedMedia,
> = stream_dock_manager_storage::JsonRecord {
    domain: Domain::MediaTombstones,
    key: "deleted",
    max_bytes: 4 * 1024 * 1024,
    decode: decode_record,
};

const LIMIT: usize = 16 * 1024 * 1024;

/// Pure validation shared by live recovery points and cold restore. Each decoder
/// uses the same domain type as normal startup, without constructing services
pub(crate) fn validate_snapshot(connection: &Connection) -> Result<(), String> {
    use std::collections::BTreeMap;
    // Serde's struct visitor also accepts sequence syntax. These persisted
    // records require JSON objects; an array must not silently become defaults
    validate_object(&crate::application_data::ACCESSIBILITY_RECORD, connection)?;
    for name in LocalPreference::ALL {
        if name.metadata().3 {
            validate_object(&name.snapshot_record::<serde_json::Value>(), connection)?;
        }
    }
    validate_object(&DELETED_MEDIA_RECORD, connection)?;
    let mut keys = connection
        .prepare("SELECT key FROM local_service_preferences")
        .map_err(|e| e.to_string())?;
    let keys = keys
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    for key in keys {
        let key = key.map_err(|e| e.to_string())?;
        if !LocalPreference::ALL.iter().any(|name| name.key() == key)
            && !["plugin-startup-attempts-v1", "vendor-profile-installations"]
                .contains(&key.as_str())
        {
            return Err(format!(
                "Unsupported preference record {key}; database preserved"
            ));
        }
    }
    LocalPreference::RestartRecoveryOnce
        .snapshot_record::<bool>()
        .read_from(connection)?;
    LocalPreference::ConfigurationRecoveryBypass
        .snapshot_record::<bool>()
        .read_from(connection)?;
    if let Some(data) = load(connection)? {
        let retained: HashSet<&str> = data
            .history
            .undo
            .iter()
            .chain(&data.history.redo)
            .map(|item| item.id.as_str())
            .collect();
        for domain in [Domain::History, Domain::HistoryPlugins] {
            let mut statement = connection
                .prepare(&format!("SELECT key FROM {}", domain.table()))
                .map_err(|e| e.to_string())?;
            for key in statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
            {
                if !retained.contains(key.map_err(|e| e.to_string())?.as_str()) {
                    return Err("Orphaned history record; database preserved".into());
                }
            }
        }
    }
    crate::vendor_profiles::validate_snapshot(connection)?;
    crate::workspace::snapshot_media(connection)?;
    crate::builtins::validate_storage_snapshot(connection)?;
    crate::updates::validate_storage_snapshot(connection)?;
    crate::resource_alerts::validate_storage_snapshot(connection)?;
    crate::power_policy::validate_storage_snapshot(connection)?;
    crate::library_organization::validate_storage_snapshot(connection)?;
    crate::plugin_policy::validate_storage_snapshot(connection)?;
    crate::import_recipes::validate_storage_snapshot(connection)?;
    crate::desktop::SETTINGS_RECORD.read_from(connection)?;
    crate::application_data::ACCESSIBILITY_RECORD.read_from(connection)?;
    crate::application_data::OFFLINE_ICONS_RECORD.read_from(connection)?;
    crate::application_data::DIAGNOSTICS_RECORD.read_from(connection)?;
    if let Some(value) = LocalPreference::Sessions
        .snapshot_record::<serde_json::Value>()
        .read_from(connection)?
    {
        let sessions =
            serde_json::from_value::<BTreeMap<String, crate::workspace::DeviceSession>>(value)
                .map_err(|error| {
                    format!("Unsupported device sessions; original preserved: {error}")
                })?;
        crate::keyboard::validate_sessions(&sessions)?;
    }
    if let Some(overrides) = LocalPreference::ParameterOverrides
        .snapshot_record::<crate::parameters::ParameterOverrides>()
        .read_from(connection)?
    {
        validate_overrides(&overrides, &parameter_definitions(connection)?)?;
    }
    let _: Option<BTreeMap<String, String>> = LocalPreference::PluginProfilePrompts
        .snapshot_record()
        .read_from(connection)?;
    let drafts: BTreeMap<String, crate::workspace::Draft> = LocalPreference::Drafts
        .snapshot_record()
        .read_from(connection)?
        .unwrap_or_default();
    for draft in drafts.values() {
        draft.validate()?;
    }
    if let Some(deleted) = DELETED_MEDIA_RECORD.read_from(connection)? {
        if deleted.schema_version != 1 || deleted.assets.len() > 65536 {
            return Err("Invalid deleted media index in recovery snapshot".into());
        }
        for asset in &deleted.assets {
            crate::profiles::validate_asset_id(asset)?;
        }
    }
    Ok(())
}

type EncodedRecords = Vec<(Domain, String, EncodedJson)>;
type EncodedWorkspace = (PreparedRecords, EncodedRecords);

pub(crate) struct PreparedWorkspace {
    data: ProfileData,
    expected: u64,
    history: Option<(String, serde_json::Value)>,
    sidecar: Option<(String, serde_json::Value)>,
    encoded: std::sync::OnceLock<Result<EncodedWorkspace, String>>,
}
impl PreparedWorkspace {
    // Initialize profile database state from the supplied inputs
    pub(crate) fn new(
        data: ProfileData,
        expected: u64,
        history: Option<(String, serde_json::Value)>,
        sidecar: Option<(String, serde_json::Value)>,
    ) -> Self {
        Self {
            data,
            expected,
            history,
            sidecar,
            encoded: std::sync::OnceLock::new(),
        }
    }
    // Set sidecar
    pub(crate) fn set_sidecar(&mut self, sidecar: (String, serde_json::Value)) {
        self.sidecar = Some(sidecar);
        self.encoded.take();
    }
    // Lazily encode workspace records and bounded history sidecars for reuse during persistence
    fn encoded(&self) -> Result<&EncodedWorkspace, String> {
        self.encoded
            .get_or_init(|| {
                let records = PreparedRecords::new(&self.data)?;
                let mut extra = Vec::new();
                for (domain, record) in [
                    (Domain::History, &self.history),
                    (Domain::HistoryPlugins, &self.sidecar),
                ] {
                    if let Some((id, value)) = record {
                        extra.push((
                            domain,
                            id.clone(),
                            EncodedJson::new(value, 32 * 1024 * 1024)?,
                        ));
                    }
                }
                Ok((records, extra))
            })
            .as_ref()
            .map_err(Clone::clone)
    }
    /// Charge all immutable inputs retained by the queued transaction
    pub(crate) fn retained_bytes(&self) -> Result<usize, String> {
        let (records, extra) = self.encoded()?;
        let mut bytes = records.1;
        // Values and serialized bytes are both retained until commit. Charge both
        // representations, using the prepared byte lengths instead of serializing again
        for (_, key, value) in extra {
            bytes = bytes
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .ok_or("Workspace transaction exceeds storage admission limit")?;
        }
        for (_, key, value) in records.0.iter().chain(extra) {
            bytes = bytes
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .ok_or("Workspace transaction exceeds storage admission limit")?;
        }
        if bytes > stream_dock_manager_storage::MAX_ADMITTED_BYTES {
            return Err("Workspace transaction exceeds storage admission limit".into());
        }
        Ok(bytes)
    }

    // Commit the staged profile database change
    pub fn commit(self, connection: &Connection) -> Result<ProfileData, String> {
        let current = load(connection)?.ok_or("Workspace is not initialized")?;
        if current.configuration_revision != self.expected {
            return Err("Configuration changed. Refresh before saving.".into());
        }
        let (records, extra) = self.encoded()?;
        for (domain, id, value) in extra {
            value.write(connection, *domain, id)?;
        }
        let mut sessions: std::collections::BTreeMap<String, crate::workspace::DeviceSession> =
            LocalPreference::Sessions
                .record::<std::collections::BTreeMap<String, crate::workspace::DeviceSession>>()
                .read_from(connection)?
                .unwrap_or_default();
        if crate::workspace::repair_removed_pages(
            &current.profiles,
            &self.data.profiles,
            &mut sessions,
        ) {
            LocalPreference::Sessions
                .record::<std::collections::BTreeMap<String, crate::workspace::DeviceSession>>()
                .write_to(connection, &sessions)?;
        }
        records.commit(connection)?;
        crate::vendor_profiles::prune(connection, &self.data.profiles)?;
        let retained: std::collections::HashSet<_> = self
            .data
            .history
            .undo
            .iter()
            .chain(&self.data.history.redo)
            .map(|item| item.id.as_str())
            .collect();
        for domain in [Domain::History, Domain::HistoryPlugins] {
            prune(connection, domain, &retained)?;
        }
        Ok(self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::Storage;

    #[test]
    // Verify that prepared records are reused and sidecar changes invalidate admission
    fn prepared_records_are_reused_and_sidecar_changes_invalidate_admission() {
        let mut prepared = PreparedWorkspace::new(ProfileData::default(), 0, None, None);
        let first_cost = prepared.retained_bytes().unwrap();
        let first = prepared.encoded().unwrap() as *const _;
        assert_eq!(prepared.retained_bytes().unwrap(), first_cost);
        assert_eq!(prepared.encoded().unwrap() as *const _, first);
        prepared.set_sidecar(("id".into(), serde_json::json!({"value":"é"})));
        assert!(prepared.retained_bytes().unwrap() > first_cost);
        assert_eq!(prepared.encoded().unwrap().1.len(), 1);
    }

    #[test]
    // Verify that prepared failure rolls back history and workspace and pruning is domain local
    fn prepared_failure_rolls_back_history_and_workspace_and_pruning_is_domain_local() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path().to_owned()).unwrap();
        let data = ProfileData::default();
        let expected = data.configuration_revision;
        storage.transaction(move |tx| save(tx, &data)).unwrap();
        for domain in [Domain::History, Domain::HistoryPlugins] {
            storage
                .write_json(domain, "retained", &serde_json::json!({"value":1}), 100)
                .unwrap();
            storage
                .write_json(domain, "discard", &serde_json::json!({"value":2}), 100)
                .unwrap();
        }
        storage
            .transaction(|tx| prune(tx, Domain::History, &HashSet::from(["retained"])))
            .unwrap();
        assert!(storage
            .read_json::<serde_json::Value>(Domain::History, "discard", 100)
            .unwrap()
            .is_none());
        assert_eq!(
            storage
                .read_json::<serde_json::Value>(Domain::HistoryPlugins, "discard", 100)
                .unwrap(),
            Some(serde_json::json!({"value":2}))
        );
        let mut prepared = PreparedWorkspace::new(
            ProfileData::default(),
            expected,
            Some(("new".into(), serde_json::json!({"value":1}))),
            None,
        );
        // Invalid key fails after a history write; the transaction must roll it back
        prepared.data.profiles[0].id = "".into();
        let cost = prepared.retained_bytes().unwrap();
        assert!(storage
            .transaction_with_cost(cost, move |tx| prepared.commit(tx))
            .is_err());
        assert!(storage
            .read_json::<serde_json::Value>(Domain::History, "new", 100)
            .unwrap()
            .is_none());
        assert_eq!(
            storage
                .run(|connection| load(connection))
                .unwrap()
                .unwrap()
                .configuration_revision,
            expected
        );
        let stale = PreparedWorkspace::new(
            ProfileData::default(),
            expected + 1,
            Some(("stale".into(), serde_json::json!({"value":1}))),
            None,
        );
        assert!(storage
            .transaction_with_cost(stale.retained_bytes().unwrap(), move |tx| stale.commit(tx))
            .is_err());
        assert!(storage
            .read_json::<serde_json::Value>(Domain::History, "stale", 100)
            .unwrap()
            .is_none());
    }

    #[test]
    // Verify that session recovery discards only retired appearance and preserves source
    fn session_recovery_discards_only_retired_appearance_and_preserves_source() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path().to_owned()).unwrap();
        storage
            .transaction(|tx| save(tx, &ProfileData::default()))
            .unwrap();
        for mode in ["saved", "palette"] {
            let mut session =
                serde_json::to_value(crate::workspace::DeviceSession::default()).unwrap();
            session["display_appearance"] = serde_json::json!(mode);
            let value = serde_json::json!({"simulator": session});
            storage
                .write_json(Domain::LocalPreferences, "sessions", &value, LIMIT)
                .unwrap();
            storage
                .run(|connection| validate_snapshot(connection))
                .unwrap();
            storage
                .run(|connection| crate::application_data::validate_snapshot(connection))
                .unwrap();
            let decoded: std::collections::BTreeMap<String, crate::workspace::DeviceSession> =
                storage
                    .read_json(Domain::LocalPreferences, "sessions", LIMIT)
                    .unwrap()
                    .unwrap();
            assert!(serde_json::to_value(decoded).unwrap()["simulator"]
                .get("display_appearance")
                .is_none());
            assert_eq!(
                storage
                    .read_json::<serde_json::Value>(Domain::LocalPreferences, "sessions", LIMIT)
                    .unwrap(),
                Some(value.clone())
            );
            let mut invalid = value;
            invalid["simulator"]["unexpected"] = serde_json::json!(true);
            storage
                .write_json(Domain::LocalPreferences, "sessions", &invalid, LIMIT)
                .unwrap();
            assert!(storage
                .run(|connection| crate::application_data::validate_snapshot(connection))
                .is_err());
            assert_eq!(
                storage
                    .read_json::<serde_json::Value>(Domain::LocalPreferences, "sessions", LIMIT)
                    .unwrap(),
                Some(invalid)
            );
        }
    }

    #[test]
    // Verify that runtime preference snapshot validation rejects unreadable domains without mutating them
    fn runtime_preference_snapshot_validation_rejects_unreadable_domains_without_mutating_them() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path().to_owned()).unwrap();
        storage
            .transaction(|tx| save(tx, &ProfileData::default()))
            .unwrap();
        storage
            .run(|connection| validate_snapshot(connection))
            .unwrap();
        for (domain, key) in [
            (Domain::ManagerPreferences, "desktop"),
            (Domain::ManagerPreferences, "accessibility"),
            (Domain::ManagerPreferences, "offline_icons"),
            (Domain::ManagerPreferences, "diagnostics_sampling"),
            (Domain::LocalPreferences, "updates"),
            (Domain::LocalPreferences, "builtin-values"),
            (Domain::LocalPreferences, "sessions"),
            (Domain::LocalPreferences, "parameter-overrides"),
            (Domain::LocalPreferences, "plugin-profile-prompts"),
            (Domain::LocalPreferences, "drafts"),
            (Domain::LocalPreferences, "device-power"),
            (Domain::LocalPreferences, "resource-alerts"),
            (Domain::LocalPreferences, "library-organization"),
            (Domain::LocalPreferences, "plugin-update-policies"),
            (Domain::LocalPreferences, "import-repair-recipes"),
            (Domain::MediaTombstones, "deleted"),
        ] {
            let invalid = serde_json::json!(["invalid preferences"]);
            storage.write_json(domain, key, &invalid, 1024).unwrap();
            assert!(
                storage
                    .run(|connection| validate_snapshot(connection))
                    .is_err(),
                "{domain:?}/{key}"
            );
            assert_eq!(
                storage
                    .read_json::<serde_json::Value>(domain, key, 1024)
                    .unwrap(),
                Some(invalid)
            );
            storage.delete(domain, key).unwrap();
        }
        storage.add_snapshot_validator(validate_snapshot).unwrap();
        let valid = storage
            .backup(directory.path().join("valid.sqlite3"))
            .unwrap();
        storage
            .write_json(
                Domain::LocalPreferences,
                "updates",
                &serde_json::json!(["invalid"]),
                1024,
            )
            .unwrap();
        assert!(storage
            .backup(directory.path().join("invalid.sqlite3"))
            .is_err());
        storage
            .read_snapshot(&valid.path, validate_snapshot)
            .unwrap();
        assert!(!directory.path().join("invalid.sqlite3").exists());
    }

    #[test]
    // Verify that runtime snapshot validation uses existing preference invariants
    fn runtime_snapshot_validation_uses_existing_preference_invariants() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path().to_owned()).unwrap();
        storage
            .transaction(|tx| save(tx, &ProfileData::default()))
            .unwrap();
        for (key, invalid) in [
            (
                "device-power",
                serde_json::json!({"schema_version":2,"revision":0,"entries":{}}),
            ),
            (
                "resource-alerts",
                serde_json::json!({"schema_version":1,"revision":0,"entries":{"plugin":{"enabled":true,"cpu_percent":101,"memory_mib":null}}}),
            ),
            (
                "library-organization",
                serde_json::json!({"schema_version":1,"organization":{"revision":0,"entries":[{"identity":{"kind":"macro","id":"x"},"favorite":true,"tags":[" duplicate "]}]}}),
            ),
            (
                "plugin-update-policies",
                serde_json::json!({"schema_version":2,"entries":[]}),
            ),
            (
                "import-repair-recipes",
                serde_json::json!({"schema_version":1,"entries":[{"id":"invalid","kind":"path_exact","context":"","source":"/a","target":"/b"}]}),
            ),
            (
                "updates",
                serde_json::json!({"preferences":{"plugin_channel":"invalid"}}),
            ),
        ] {
            storage
                .write_json(Domain::LocalPreferences, key, &invalid, 4096)
                .unwrap();
            assert!(
                storage
                    .run(|connection| validate_snapshot(connection))
                    .is_err(),
                "{key}"
            );
            storage.delete(Domain::LocalPreferences, key).unwrap();
        }
    }

    #[test]
    // Verify that retained entity revisions only change with their payloads
    fn retained_entity_revisions_only_change_with_their_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let first = data.profiles[0].id.clone();
        let revision = |id: &str| {
            let id = id.to_owned();
            store
                .storage
                .run(move |connection| {
                    stream_dock_manager_storage::revision(connection, Domain::Profiles, &id)
                })
                .unwrap()
        };
        assert_eq!(revision(&first), Some(1));
        store.save(&data).unwrap();
        assert_eq!(revision(&first), Some(1));
        data.profiles[0].name = "Changed".into();
        store.save(&data).unwrap();
        assert_eq!(revision(&first), Some(2));
        let mut second = data.profiles[0].clone();
        second.id = "second".into();
        data.profiles.push(second);
        store.save(&data).unwrap();
        assert_eq!(revision(&first), Some(2));
        assert_eq!(revision("second"), Some(1));
        data.profiles.pop();
        store.save(&data).unwrap();
        assert_eq!(revision(&first), Some(2));
        assert_eq!(revision("second"), None);
    }

    #[test]
    // Verify that missing workspace header preserves related rows and rejects backup
    fn missing_workspace_header_preserves_related_rows_and_rejects_backup() {
        for domain in [
            Domain::Profiles,
            Domain::SavedMacros,
            Domain::AppearancePresets,
            Domain::ProfileRules,
            Domain::History,
            Domain::HistoryPlugins,
            Domain::MediaTombstones,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let storage = Storage::open(directory.path().to_owned()).unwrap();
            storage
                .write_json(domain, "orphan", &serde_json::json!({}), LIMIT)
                .unwrap();
            assert!(storage
                .run(|connection| load(connection))
                .unwrap_err()
                .contains("metadata is missing"));
            storage
                .add_snapshot_validator(|connection| {
                    crate::workspace::snapshot_media(connection).map(|_| ())
                })
                .unwrap();
            assert!(storage
                .backup(directory.path().join("invalid.sqlite3"))
                .is_err());
            assert!(!directory.path().join("invalid.sqlite3").exists());
            assert!(storage
                .read_json::<serde_json::Value>(Domain::Workspace, "current", LIMIT)
                .unwrap()
                .is_none());
            assert!(storage
                .read_json::<serde_json::Value>(domain, "orphan", LIMIT)
                .unwrap()
                .is_some());
        }
    }

    #[test]
    // Verify that profile load and save do not initialize missing damaged header
    fn profile_load_and_save_do_not_initialize_missing_damaged_header() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        store.storage.delete(Domain::Workspace, "current").unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&data).is_err());
        assert!(store
            .storage
            .read_json::<serde_json::Value>(Domain::Workspace, "current", LIMIT)
            .unwrap()
            .is_none());
        assert!(store
            .storage
            .read_json::<crate::profiles::Profile>(Domain::Profiles, &data.profiles[0].id, LIMIT)
            .unwrap()
            .is_some());
    }

    #[test]
    // Verify that decomposed indexes reject duplicates orphans embedded and mismatched IDs
    fn decomposed_indexes_reject_duplicates_orphans_embedded_and_mismatched_ids() {
        for corruption in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let storage = Storage::open(directory.path().to_owned()).unwrap();
            storage
                .transaction(|tx| save(tx, &ProfileData::default()))
                .unwrap();
            storage
                .transaction(move |tx| {
                    let mut record: WorkspaceRecord = stream_dock_manager_storage::read_json(
                        tx,
                        Domain::Workspace,
                        "current",
                        LIMIT,
                    )?
                    .unwrap();
                    let profile = ProfileData::default().profiles.remove(0);
                    match corruption {
                        0 => record.profiles.push(record.profiles[0].clone()),
                        1 => stream_dock_manager_storage::write_json(
                            tx,
                            Domain::Profiles,
                            "unlisted",
                            &profile,
                            LIMIT,
                        )?,
                        2 => record.document.profiles.push(profile),
                        _ => {
                            let mut profile = profile;
                            profile.id = "wrong".into();
                            stream_dock_manager_storage::write_json(
                                tx,
                                Domain::Profiles,
                                &record.profiles[0],
                                &profile,
                                LIMIT,
                            )?;
                        }
                    }
                    stream_dock_manager_storage::write_json(
                        tx,
                        Domain::Workspace,
                        "current",
                        &record,
                        LIMIT,
                    )
                })
                .unwrap();
            assert!(
                storage.run(|connection| load(connection)).is_err(),
                "corruption {corruption}"
            );
        }
    }

    #[test]
    // Verify that prepared admission includes workspace history and sidecar and is bounded
    fn prepared_admission_includes_workspace_history_and_sidecar_and_is_bounded() {
        let prepared = PreparedWorkspace {
            encoded: std::sync::OnceLock::new(),
            data: ProfileData::default(),
            expected: 1,
            history: Some(("history".into(), serde_json::json!({"value":"é"}))),
            sidecar: Some(("sidecar".into(), serde_json::json!({"value":"😀"}))),
        };
        let expected = serde_json::to_vec(&prepared.data).unwrap().len()
            + prepared
                .history
                .as_ref()
                .map(|(id, value)| id.len() + serde_json::to_vec(value).unwrap().len())
                .unwrap()
            + prepared
                .sidecar
                .as_ref()
                .map(|(id, value)| id.len() + serde_json::to_vec(value).unwrap().len())
                .unwrap();
        let (records, extra) = prepared.encoded().unwrap();
        let encoded_cost: usize = records
            .0
            .iter()
            .chain(extra)
            .map(|(_, key, value)| key.len() + value.len())
            .sum();
        assert_eq!(prepared.retained_bytes().unwrap(), expected + encoded_cost);
        assert!(encoded_size(&serde_json::json!("😀"), 5).is_err());
        assert_eq!(encoded_size(&serde_json::json!("😀"), 6).unwrap(), 6);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceRecord {
    document: ProfileData,
    profiles: Vec<String>,
    macros: Vec<String>,
    styles: Vec<String>,
}

// Load the requested profile database state
pub(crate) fn load(connection: &Connection) -> Result<Option<ProfileData>, String> {
    for domain in [
        Domain::Workspace,
        Domain::Profiles,
        Domain::SavedMacros,
        Domain::AppearancePresets,
        Domain::History,
        Domain::HistoryPlugins,
    ] {
        let invalid: bool = connection
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE json_type(payload) != 'object')",
                    domain.table()
                ),
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if invalid {
            return Err(format!(
                "Unsupported non-object record in {}; original preserved",
                domain.table()
            ));
        }
    }
    let Some(record): Option<WorkspaceRecord> =
        stream_dock_manager_storage::read_json(connection, Domain::Workspace, "current", LIMIT)?
    else {
        // A genuinely unused database has no workspace-owned rows. Losing only
        // the header must never replace its surviving records with defaults
        for domain in [
            Domain::Workspace,
            Domain::Profiles,
            Domain::SavedMacros,
            Domain::AppearancePresets,
            Domain::ProfileRules,
            Domain::History,
            Domain::HistoryPlugins,
            Domain::MediaTombstones,
        ] {
            if row_count(connection, domain)? != 0 {
                return Err("Workspace metadata is missing; database preserved".into());
            }
        }
        let related_preferences: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_service_preferences WHERE key IN ('sessions','drafts','parameter-overrides'))",
            [], |row| row.get(0)).map_err(|e| e.to_string())?;
        if related_preferences {
            return Err("Workspace metadata is missing; database preserved".into());
        }
        return Ok(None);
    };
    if row_count(connection, Domain::Workspace)? != 1
        || row_count(connection, Domain::ProfileRules)? != 1
        || !record.document.profiles.is_empty()
        || !record.document.saved_macros.is_empty()
        || !record.document.styles.is_empty()
        || !record.document.rules.is_empty()
    {
        return Err("Invalid decomposed workspace metadata; database preserved".into());
    }
    // Check the complete encoded record budget before decoding each referenced
    // entity. Index strings add overhead to the public 16 MiB document format
    let mut stored_bytes = 0u64;
    for domain in [
        Domain::Workspace,
        Domain::Profiles,
        Domain::SavedMacros,
        Domain::AppearancePresets,
        Domain::ProfileRules,
    ] {
        let bytes: u64 = connection
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM {}",
                    domain.table()
                ),
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        stored_bytes = stored_bytes
            .checked_add(bytes)
            .ok_or("Workspace document exceeds size limit")?;
        if stored_bytes > (2 * LIMIT) as u64 {
            return Err("Workspace records exceed size limit".into());
        }
    }
    check_index(connection, Domain::Profiles, &record.profiles)?;
    check_index(connection, Domain::SavedMacros, &record.macros)?;
    check_index(connection, Domain::AppearancePresets, &record.styles)?;
    let mut data = record.document;
    for id in record.profiles {
        let profile: crate::profiles::Profile =
            stream_dock_manager_storage::read_json(connection, Domain::Profiles, &id, LIMIT)?
                .ok_or("Missing workspace profile record")?;
        if profile.id != id {
            return Err("Workspace profile identity mismatch".into());
        }
        data.profiles.push(profile);
    }
    for id in record.macros {
        let saved: crate::builtins::SavedMacro =
            stream_dock_manager_storage::read_json(connection, Domain::SavedMacros, &id, LIMIT)?
                .ok_or("Missing saved macro record")?;
        if saved.id != id {
            return Err("Saved macro identity mismatch".into());
        }
        data.saved_macros.push(saved);
    }
    for id in record.styles {
        let style: crate::workspace::AppearancePreset = stream_dock_manager_storage::read_json(
            connection,
            Domain::AppearancePresets,
            &id,
            LIMIT,
        )?
        .ok_or("Missing appearance record")?;
        if style.id != id {
            return Err("Appearance preset identity mismatch".into());
        }
        data.styles.push(style);
    }
    data.rules =
        stream_dock_manager_storage::read_json(connection, Domain::ProfileRules, "current", LIMIT)?
            .ok_or("Missing workspace rule record")?;
    encoded_size(&data, LIMIT)?;
    Ok(Some(data))
}

// Count rows in the selected storage domain
fn row_count(connection: &Connection, domain: Domain) -> Result<u64, String> {
    connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {}", domain.table()),
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
}

// Check index
fn check_index(connection: &Connection, domain: Domain, ids: &[String]) -> Result<(), String> {
    let unique: HashSet<_> = ids.iter().collect();
    if unique.len() != ids.len() || row_count(connection, domain)? != ids.len() as u64 {
        return Err("Workspace record index is inconsistent; database preserved".into());
    }
    Ok(())
}

/// Count UTF-8 JSON without allocating an additional document, stopping at the
/// admission limit even if a caller supplies an oversized in-memory candidate
pub(crate) fn encoded_size(
    value: &(impl Serialize + ?Sized),
    limit: usize,
) -> Result<usize, String> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        // Write the supplied profile database data
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other(
                    "Workspace document exceeds size limit",
                ));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        // Flush pending profile database writes
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| "Workspace document exceeds size limit or cannot be encoded".to_string())?;
    Ok(counter.bytes)
}

// Prune obsolete profile database entries
fn prune(connection: &Connection, domain: Domain, retained: &HashSet<&str>) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("SELECT key FROM {}", domain.table()))
        .map_err(|e| e.to_string())?;
    let keys = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for key in keys {
        if !retained.contains(key.as_str()) {
            stream_dock_manager_storage::delete(connection, domain, &key)?;
        }
    }
    Ok(())
}

/// Caller holds the domain transaction gate. All record families publish together
pub(crate) fn save(connection: &Connection, data: &ProfileData) -> Result<(), String> {
    PreparedRecords::new(data)?.commit(connection)
}

type ParameterDefinitions =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, (String, bool)>>;

// Load parameter names, types and sensitivity from persisted profiles
fn parameter_definitions(connection: &Connection) -> Result<ParameterDefinitions, String> {
    let profiles = stream_dock_manager_storage::list_json::<crate::profiles::Profile>(
        connection,
        Domain::Profiles,
        4096,
        LIMIT,
    )?;
    Ok(profiles
        .into_iter()
        .map(|(id, profile)| {
            (
                id,
                profile
                    .parameters
                    .into_iter()
                    .map(|parameter| (parameter.name, (parameter.r#type, parameter.sensitive)))
                    .collect(),
            )
        })
        .collect())
}

// Validate overrides
fn validate_overrides(
    overrides: &crate::parameters::ParameterOverrides,
    definitions: &ParameterDefinitions,
) -> Result<(), String> {
    for (profile, values) in overrides.installation.iter().chain(
        overrides
            .devices
            .values()
            .flat_map(|profiles| profiles.iter()),
    ) {
        let parameters = definitions
            .get(profile)
            .ok_or("Unknown parameter override profile; original preserved")?;
        for (name, value) in values {
            let (kind, sensitive) = parameters
                .get(name)
                .ok_or("Unknown parameter override definition; original preserved")?;
            if *sensitive && crate::credentials::reference(value).is_none() {
                return Err("Unsupported plaintext credential override; original preserved. Re-enter credentials in a fresh configuration.".into());
            }
            if !sensitive && !crate::parameters::valid_type(kind, value) {
                return Err("Parameter override type mismatch".into());
            }
        }
    }
    Ok(())
}

/// Local overrides are outside portable undo history. Removing a definition or
/// changing its type/sensitivity removes its local values in the same SQL
/// transaction, rather than leaving a database that cannot reopen. Undo restores
/// definitions, not discarded installation secrets/values. OS vault entries are
/// not deleted inside a SQLite transaction
fn reconcile_overrides(
    overrides: &mut crate::parameters::ParameterOverrides,
    before: &ParameterDefinitions,
    after: &ParameterDefinitions,
) {
    let retain_profiles = |profiles: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, serde_json::Value>,
    >| {
        profiles.retain(|profile, values| {
            values.retain(|name, _| {
                before
                    .get(profile)
                    .and_then(|parameters| parameters.get(name))
                    .is_some_and(|definition| {
                        after
                            .get(profile)
                            .and_then(|parameters| parameters.get(name))
                            == Some(definition)
                    })
            });
            !values.is_empty()
        });
    };
    retain_profiles(&mut overrides.installation);
    overrides.devices.retain(|_, profiles| {
        retain_profiles(profiles);
        !profiles.is_empty()
    });
}

pub(crate) struct PreparedRecords(EncodedRecords, usize);
impl PreparedRecords {
    // Initialize profile database state from the supplied inputs
    pub(crate) fn new(data: &ProfileData) -> Result<Self, String> {
        let document_bytes = encoded_size(data, LIMIT)?;
        let mut document = data.clone();
        let profiles = std::mem::take(&mut document.profiles);
        let macros = std::mem::take(&mut document.saved_macros);
        let styles = std::mem::take(&mut document.styles);
        let rules = std::mem::take(&mut document.rules);
        let mut records = Vec::new();
        for profile in &profiles {
            records.push((
                Domain::Profiles,
                profile.id.clone(),
                EncodedJson::new(profile, LIMIT)?,
            ));
        }
        for saved in &macros {
            records.push((
                Domain::SavedMacros,
                saved.id.clone(),
                EncodedJson::new(saved, LIMIT)?,
            ));
        }
        for style in &styles {
            records.push((
                Domain::AppearancePresets,
                style.id.clone(),
                EncodedJson::new(style, LIMIT)?,
            ));
        }
        records.push((
            Domain::ProfileRules,
            "current".into(),
            EncodedJson::new(&rules, LIMIT)?,
        ));
        records.push((
            Domain::Workspace,
            "current".into(),
            EncodedJson::new(
                &WorkspaceRecord {
                    document,
                    profiles: profiles.iter().map(|p| p.id.clone()).collect(),
                    macros: macros.iter().map(|p| p.id.clone()).collect(),
                    styles: styles.iter().map(|p| p.id.clone()).collect(),
                },
                LIMIT,
            )?,
        ));
        Ok(Self(records, document_bytes))
    }
    // Sum retained transaction key and value bytes while enforcing the storage admission limit
    pub(crate) fn retained_bytes(&self) -> Result<usize, String> {
        self.0.iter().try_fold(0usize, |bytes, (_, key, value)| {
            bytes
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .filter(|n| *n <= stream_dock_manager_storage::MAX_ADMITTED_BYTES)
                .ok_or_else(|| "Workspace transaction exceeds storage admission limit".into())
        })
    }
    // Commit the staged profile database change
    pub(crate) fn commit(&self, connection: &Connection) -> Result<(), String> {
        let overrides: Option<crate::parameters::ParameterOverrides> =
            LocalPreference::ParameterOverrides
                .record::<crate::parameters::ParameterOverrides>()
                .read_from(connection)?;
        let prior = if let Some(overrides) = &overrides {
            let definitions = parameter_definitions(connection)?;
            validate_overrides(overrides, &definitions)?;
            Some(definitions)
        } else {
            None
        };
        for domain in [
            Domain::Profiles,
            Domain::SavedMacros,
            Domain::AppearancePresets,
        ] {
            prune(
                connection,
                domain,
                &self
                    .0
                    .iter()
                    .filter(|r| r.0 == domain)
                    .map(|r| r.1.as_str())
                    .collect(),
            )?;
        }
        for (domain, key, value) in &self.0 {
            value.write_changed(connection, *domain, key)?;
        }
        if let (Some(mut overrides), Some(before)) = (overrides, prior) {
            let original = serde_json::to_value(&overrides).map_err(|e| e.to_string())?;
            reconcile_overrides(&mut overrides, &before, &parameter_definitions(connection)?);
            if serde_json::to_value(&overrides).map_err(|e| e.to_string())? != original {
                LocalPreference::ParameterOverrides
                    .record::<crate::parameters::ParameterOverrides>()
                    .write_to(connection, &overrides)?;
            }
        }
        Ok(())
    }
}

impl ProfileStore {
    /// Recheck definitions on the database worker, in the same transaction as the
    /// write. UI validation may have happened before a concurrent workspace edit
    pub(crate) fn write_parameter_overrides(
        &self,
        overrides: &crate::parameters::ParameterOverrides,
    ) -> Result<(), String> {
        let record =
            LocalPreference::ParameterOverrides.record::<crate::parameters::ParameterOverrides>();
        let encoded = EncodedJson::new(overrides, record.max_bytes)?;
        let overrides = overrides.clone();
        self.storage
            .transaction_with_cost(encoded.len(), move |connection| {
                validate_overrides(&overrides, &parameter_definitions(connection)?)?;
                encoded.write(connection, record.domain, record.key)
            })
    }

    // Normalize a candidate workspace, detach deleted media and validate requirements and size
    pub(crate) fn prepared_data(&self, data: &ProfileData) -> Result<ProfileData, String> {
        let mut data = data.clone();
        crate::keyboard_navigation::normalize(&mut data)?;
        self.detach_deleted_media(&mut data);
        data.update_requirements();
        crate::profiles::validate_data(&data)?;
        encoded_size(&data, LIMIT)?;
        Ok(data)
    }
    // Read and serialize a retained history record after validating its identifier
    pub(crate) fn history_bytes(&self, id: &str) -> Result<Vec<u8>, String> {
        uuid::Uuid::parse_str(id).map_err(|_| "Invalid history ID")?;
        let value: serde_json::Value = self
            .storage
            .read_json(Domain::History, id, 32 * 1024 * 1024)?
            .ok_or("History entry is no longer retained")?;
        serde_json::to_vec(&value).map_err(|e| e.to_string())
    }
    // Read preference
    pub(crate) fn read_preference<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        name: crate::profile_database::LocalPreference,
    ) -> Result<Option<T>, String> {
        name.record().read(&self.storage)
    }
}

#[cfg(test)]
mod current_contract_tests {
    use super::*;
    use stream_dock_manager_storage::Storage;

    #[test]
    // Verify that obsolete keys and plaintext credentials are rejected before original writes
    fn obsolete_keys_and_plaintext_credentials_are_rejected_before_original_writes() {
        for plaintext in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let storage = Storage::open(root.path().into()).unwrap();
            let mut data = ProfileData::default();
            data.profiles[0]
                .parameters
                .push(crate::parameters::ProfileParameter {
                    name: "secret".into(),
                    label: "Secret".into(),
                    r#type: "string".into(),
                    required: false,
                    default: None,
                    sensitive: true,
                });
            let id = data.profiles[0].id.clone();
            storage.transaction(move |tx| save(tx, &data)).unwrap();
            if plaintext {
                storage.write_json(Domain::LocalPreferences, "parameter-overrides", &serde_json::json!({"installation":{id:{"secret":"do-not-print"}},"devices":{}}), LIMIT).unwrap();
            } else {
                storage
                    .write_json(
                        Domain::LocalPreferences,
                        "profiles/sessions.json",
                        &serde_json::json!({}),
                        LIMIT,
                    )
                    .unwrap();
            }
            drop(storage);
            let path = root.path().join(stream_dock_manager_storage::DATABASE_NAME);
            let before = std::fs::read(&path).unwrap();
            let error = Storage::open_validated(root.path().into(), validate_snapshot).unwrap_err();
            assert!(!error.contains("do-not-print"));
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
    }

    #[test]
    // Verify that current defaults roundtrip but missing required fields do not
    fn current_defaults_roundtrip_but_missing_required_fields_do_not() {
        let current = serde_json::to_value(ProfileData::default()).unwrap();
        crate::profiles::parse_data(current.clone()).unwrap();
        for key in ["configuration_revision", "saved_macros", "rules", "history"] {
            let mut missing = current.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(crate::profiles::parse_data(missing).is_err(), "{key}");
        }
        let current = serde_json::to_value(crate::workspace::DeviceSession::default()).unwrap();
        serde_json::from_value::<crate::workspace::DeviceSession>(current.clone()).unwrap();
        let mut missing = current;
        missing.as_object_mut().unwrap().remove("output");
        assert!(serde_json::from_value::<crate::workspace::DeviceSession>(missing).is_err());
    }
}

#[cfg(test)]
mod current_dial_and_draft_tests {
    use super::*;
    use crate::actions::Action;
    #[test]
    // Verify that shared dial bindings and obsolete embedded drafts are rejected
    fn shared_dial_bindings_and_obsolete_embedded_drafts_are_rejected() {
        let mut profile = ProfileData::default().profiles.remove(0);
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|manifest| {
                manifest
                    .controls
                    .iter()
                    .any(|control| control.kind == "encoder")
            })
            .unwrap();
        let encoder = manifest
            .controls
            .iter()
            .find(|control| control.kind == "encoder")
            .unwrap()
            .id
            .clone();
        profile.model_id = manifest.model_id;
        let mut binding = profile.pages[0].bindings.values().next().unwrap().clone();
        binding.action = Action::Volume { delta: 5 };
        for page in &mut profile.pages {
            page.bindings.clear();
        }
        profile.pages[0]
            .bindings
            .insert(encoder.clone(), binding.clone());
        assert!(profile.validate().unwrap_err().contains("explicit left"));
        binding.action = Action::Dial {
            linked_turns: false,
            actions: vec![
                Action::Volume { delta: -5 },
                Action::Volume { delta: 5 },
                Action::Mute,
            ],
        };
        profile.pages[0].bindings.insert(encoder, binding);
        profile.validate().unwrap();
        let draft = crate::workspace::Draft {
            key: "draft".into(),
            base: None,
            value: serde_json::json!({"dialActions":[{"action":{"kind":"none"}}]}),
        };
        assert!(draft.validate().is_err());
        let draft = crate::workspace::Draft {
            key: "draft".into(),
            base: None,
            value: serde_json::json!({"actionDraft":{"action":{"kind":"retired_action"}}}),
        };
        assert!(draft.validate().is_err());
    }
    #[test]
    // Verify that linked turn preference round trips without coupling plugin contexts
    fn linked_turn_preference_round_trips_without_coupling_plugin_contexts() {
        let mut action: Action = serde_json::from_value(serde_json::json!({
            "kind":"dial", "linked_turns":true,
            "actions":[{"kind":"none"},{"kind":"none"},{"kind":"none"}]
        }))
        .unwrap();
        action.validate(0).unwrap();
        *action.child_mut(1).unwrap() = Action::Mute;
        // Host updates to one lane must not invalidate the saved editor preference
        action.validate(0).unwrap();
        assert_eq!(serde_json::to_value(&action).unwrap()["linked_turns"], true);
        for mode in [
            serde_json::json!(null),
            serde_json::json!(1),
            serde_json::json!("true"),
        ] {
            let mut encoded = serde_json::to_value(&action).unwrap();
            encoded["linked_turns"] = mode;
            assert!(serde_json::from_value::<Action>(encoded).is_err());
        }
        let mut draft = crate::workspace::Draft {
            key: "dial".into(),
            base: None,
            value: serde_json::json!({"dialActions":[{"action":{"kind":"none"}},{"action":{"kind":"none"}},{"action":{"kind":"none"}}],"dialLinkedTurns":true}),
        };
        draft.validate().unwrap();
        draft.value["dialLinkedTurns"] = serde_json::json!("true");
        assert!(draft.validate().is_err());
        draft.value["dialLinkedTurns"] = serde_json::json!(false);
        draft.value["obsoleteEntries"] = serde_json::json!([]);
        assert!(draft.validate().is_err());
    }

    #[test]
    // Verify that explicitly unlinked empty dial preserves false on round trip
    fn explicitly_unlinked_empty_dial_preserves_false_on_round_trip() {
        let action = Action::Dial {
            actions: vec![Action::None; 3],
            linked_turns: false,
        };
        let encoded = serde_json::to_value(&action).unwrap();
        assert_eq!(encoded["linked_turns"], false);
        let restored: Action = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), encoded);
    }

    #[test]
    // Verify that unsupported action records are rejected without replacing the database
    fn unsupported_action_records_are_rejected_without_replacing_the_database() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let data = store.load().unwrap();
        let profile = &data.profiles[0];
        let id = profile.id.clone();
        let mut value = serde_json::to_value(profile).unwrap();
        let bindings = value["pages"][0]["bindings"].as_object_mut().unwrap();
        bindings.values_mut().next().unwrap()["action"] =
            serde_json::json!({"kind":"retired_action","actions":[]});
        store
            .storage
            .write_json(Domain::Profiles, &id, &value, LIMIT)
            .unwrap();
        assert!(store.load().is_err());
        drop(store);
        let path = root.path().join(stream_dock_manager_storage::DATABASE_NAME);
        let before = std::fs::read(&path).unwrap();
        assert!(ProfileStore::open(root.path().into()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[cfg(test)]
mod parameter_override_reconciliation_tests {
    use super::*;
    use crate::parameters::{ParameterOverrides, ProfileParameter};
    use std::collections::BTreeMap;

    #[test]
    // Verify that edits undo and restart keep overrides consistent with current definitions
    fn edits_undo_and_restart_keep_overrides_consistent_with_current_definitions() {
        for edit_kind in [
            "remove_parameter",
            "change_type",
            "make_sensitive",
            "remove_profile",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(directory.path().into()).unwrap();
            let mut data = store.load().unwrap();
            let profile = data.profiles[0].id.clone();
            data.profiles[0].parameters = vec![ProfileParameter {
                name: "value".into(),
                label: "Value".into(),
                r#type: "number".into(),
                required: false,
                default: None,
                sensitive: false,
            }];
            store.save(&data).unwrap();
            let values = BTreeMap::from([(
                profile.clone(),
                BTreeMap::from([("value".into(), serde_json::json!(42))]),
            )]);
            store
                .write_preference(
                    LocalPreference::ParameterOverrides,
                    &ParameterOverrides {
                        installation: values.clone(),
                        devices: BTreeMap::from([("device".into(), values)]),
                    },
                )
                .unwrap();
            let edited = store
                .edit(data.configuration_revision, "Change definition", |data| {
                    match edit_kind {
                        "remove_parameter" => data.profiles[0].parameters.clear(),
                        "change_type" => data.profiles[0].parameters[0].r#type = "boolean".into(),
                        "make_sensitive" => {
                            data.profiles[0].parameters[0].r#type = "string".into();
                            data.profiles[0].parameters[0].sensitive = true;
                        }
                        "remove_profile" => {
                            data.profiles.clear();
                            data.active_profile.clear();
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                })
                .unwrap();
            let local: ParameterOverrides = store
                .read_preference(LocalPreference::ParameterOverrides)
                .unwrap()
                .unwrap();
            assert!(
                local.installation.is_empty() && local.devices.is_empty(),
                "{edit_kind}"
            );
            store
                .storage
                .backup(directory.path().join("current-backup.sqlite3"))
                .unwrap();
            let undone = store
                .history_action(edited.configuration_revision, "undo")
                .unwrap();
            assert_eq!(undone.profiles[0].parameters[0].r#type, "number");
            let local: ParameterOverrides = store
                .read_preference(LocalPreference::ParameterOverrides)
                .unwrap()
                .unwrap();
            assert!(local.installation.is_empty() && local.devices.is_empty());
            store
                .history_action(undone.configuration_revision, "redo")
                .unwrap();
            drop(store);
            let reopened = ProfileStore::open(directory.path().into()).unwrap();
            reopened.load().unwrap();
            reopened
                .storage
                .backup(directory.path().join("reopened-backup.sqlite3"))
                .unwrap();
        }
    }

    #[test]
    // Verify that compatible values survive and failed reconciliation rolls back workspace
    fn compatible_values_survive_and_failed_reconciliation_rolls_back_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().into()).unwrap();
        let mut data = store.load().unwrap();
        let profile = data.profiles[0].id.clone();
        data.profiles[0].parameters = vec![ProfileParameter {
            name: "value".into(),
            label: "Value".into(),
            r#type: "number".into(),
            required: false,
            default: None,
            sensitive: false,
        }];
        store.save(&data).unwrap();
        let original = ParameterOverrides {
            installation: BTreeMap::from([(
                profile,
                BTreeMap::from([("value".into(), serde_json::json!(42))]),
            )]),
            devices: BTreeMap::new(),
        };
        store
            .write_preference(LocalPreference::ParameterOverrides, &original)
            .unwrap();
        let renamed = store
            .edit(data.configuration_revision, "Rename", |data| {
                data.profiles[0].name = "Renamed".into();
                Ok(())
            })
            .unwrap();
        let local: ParameterOverrides = store
            .read_preference(LocalPreference::ParameterOverrides)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(local).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        store.storage.run(|connection| connection.execute_batch("CREATE TRIGGER reject_override_reconciliation BEFORE UPDATE ON local_service_preferences WHEN NEW.key='parameter-overrides' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").map_err(|e| e.to_string())).unwrap();
        assert!(store
            .edit(renamed.configuration_revision, "Remove parameter", |data| {
                data.profiles[0].parameters.clear();
                Ok(())
            })
            .is_err());
        assert_eq!(
            store.load().unwrap().configuration_revision,
            renamed.configuration_revision
        );
        assert_eq!(store.load().unwrap().profiles[0].parameters.len(), 1);
        let local: ParameterOverrides = store
            .read_preference(LocalPreference::ParameterOverrides)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(local).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        store
            .storage
            .run(|connection| {
                connection
                    .execute_batch("DROP TRIGGER reject_override_reconciliation;")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        drop(store);
        ProfileStore::open(directory.path().into()).unwrap();
    }
}

#[cfg(test)]
mod override_write_interleaving_tests {
    use super::*;
    use crate::parameters::{ParameterOverrides, ProfileParameter};
    use std::collections::BTreeMap;

    #[test]
    // Verify that stale override writer revalidates after definition commit
    fn stale_override_writer_revalidates_after_definition_commit() {
        for change in ["delete", "type", "sensitive"] {
            let root = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(root.path().into()).unwrap();
            let mut data = store.load().unwrap();
            data.profiles[0].parameters = vec![ProfileParameter {
                name: "value".into(),
                label: "Value".into(),
                r#type: "number".into(),
                required: false,
                default: None,
                sensitive: false,
            }];
            store.save(&data).unwrap();
            let values = BTreeMap::from([(
                data.profiles[0].id.clone(),
                BTreeMap::from([("value".into(), serde_json::json!(42))]),
            )]);
            let pending = ParameterOverrides {
                installation: values.clone(),
                devices: BTreeMap::from([("device".into(), values)]),
            };
            // Deterministic interleaving: writer A completes UI validation, then
            // workspace writer B commits before A's queued persistence operation
            store
                .storage
                .run({
                    let pending = pending.clone();
                    move |connection| {
                        validate_overrides(&pending, &parameter_definitions(connection)?)
                    }
                })
                .unwrap();
            store
                .edit(
                    data.configuration_revision,
                    "Concurrent definition edit",
                    |data| {
                        match change {
                            "delete" => data.profiles[0].parameters.clear(),
                            "type" => data.profiles[0].parameters[0].r#type = "boolean".into(),
                            "sensitive" => {
                                data.profiles[0].parameters[0].r#type = "string".into();
                                data.profiles[0].parameters[0].sensitive = true;
                            }
                            _ => unreachable!(),
                        }
                        Ok(())
                    },
                )
                .unwrap();
            assert!(
                store.write_parameter_overrides(&pending).is_err(),
                "{change}"
            );
            assert!(store
                .read_preference::<ParameterOverrides>(LocalPreference::ParameterOverrides)
                .unwrap()
                .is_none());
            store
                .storage
                .backup(root.path().join("validated.sqlite3"))
                .unwrap();
            drop(store);
            ProfileStore::open(root.path().into()).unwrap();
        }
    }
}

#[cfg(test)]
mod keyboard_macro_storage_tests {
    use super::*;
    use crate::{
        actions::Action,
        builtins::{BuiltinAction, SavedMacro},
        keyboard_macro::*,
    };
    use serde_json::json;

    // Construct a balanced keyboard macro action for persistence tests
    fn keyboard_action() -> Action {
        Action::Builtin {
            action: Box::new(BuiltinAction::KeyboardMacro {
                recording: KeyboardMacroRecording {
                    events: vec![
                        KeyboardMacroEvent {
                            kind: KeyboardMacroEventKind::KeyDown,
                            key: PhysicalKey::KeyA,
                            delay_ms: 0,
                        },
                        KeyboardMacroEvent {
                            kind: KeyboardMacroEventKind::KeyUp,
                            key: PhysicalKey::KeyA,
                            delay_ms: 100,
                        },
                    ],
                    ..Default::default()
                },
                playback: Default::default(),
            }),
        }
    }
    // Construct profile data containing both a keyboard binding and a saved macro fixture
    fn data() -> ProfileData {
        let mut data = ProfileData::default();
        let binding = serde_json::from_value(
            json!({"title":"Recording", "color":"#000000", "action":keyboard_action()}),
        )
        .unwrap();
        data.profiles[0].pages[0]
            .bindings
            .insert("key-1".into(), binding);
        data.saved_macros.push(SavedMacro {
            id: "keyboard-test".into(),
            name: "Recording".into(),
            action: Action::Macro {
                actions: vec![keyboard_action()],
            },
            inputs: vec![],
        });
        data
    }
    #[test]
    // Verify that keyboard macro workspace roundtrip export backup and invalid save preservation
    fn keyboard_macro_workspace_roundtrip_export_backup_and_invalid_save_preservation() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = data();
        store.save(&data).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            serde_json::to_value(&loaded.saved_macros).unwrap(),
            serde_json::to_value(&data.saved_macros).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&loaded.profiles[0].pages[0].bindings["key-1"].action).unwrap(),
            serde_json::to_value(keyboard_action()).unwrap()
        );
        let archive = root.path().join("recording.zip");
        store.export(&archive).unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let exported: serde_json::Value =
            serde_json::from_reader(archive.by_name("profiles.json").unwrap()).unwrap();
        let exported = crate::profiles::parse_data(exported).unwrap();
        assert_eq!(
            serde_json::to_value(&exported.saved_macros).unwrap(),
            serde_json::to_value(&data.saved_macros).unwrap()
        );
        let backup = store
            .storage()
            .backup(root.path().join("recording.sqlite3"))
            .unwrap();
        store
            .storage()
            .read_snapshot(&backup.path, validate_snapshot)
            .unwrap();
        let before = serde_json::to_value(store.load().unwrap()).unwrap();
        let mut invalid = data;
        if let Action::Builtin { action } = &mut invalid.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action
        {
            if let BuiltinAction::KeyboardMacro { recording, .. } = action.as_mut() {
                recording.events.pop();
            }
        }
        assert!(store.save(&invalid).unwrap_err().contains("release"));
        assert_eq!(serde_json::to_value(store.load().unwrap()).unwrap(), before);
    }
    #[test]
    // Verify that malformed persisted recording rejects open and backup without rewriting
    fn malformed_persisted_recording_rejects_open_and_backup_without_rewriting() {
        for malformed in ["version", "balance", "unknown"] {
            let root = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(root.path().join("profiles")).unwrap();
            store.save(&data()).unwrap();
            let mut saved = serde_json::to_value(&data().saved_macros[0]).unwrap();
            let recording = &mut saved["action"]["actions"][0]["action"]["recording"];
            match malformed {
                "version" => recording["version"] = json!(2),
                "balance" => {
                    recording["events"].as_array_mut().unwrap().pop();
                }
                _ => recording["events"][0]["key"] = json!("UnsupportedSensitiveKey"),
            }
            store
                .storage()
                .write_json(Domain::SavedMacros, "keyboard-test", &saved, LIMIT)
                .unwrap();
            assert!(store
                .storage()
                .backup(root.path().join("invalid.sqlite3"))
                .is_err());
            assert_eq!(
                store
                    .storage()
                    .read_json::<serde_json::Value>(Domain::SavedMacros, "keyboard-test", LIMIT)
                    .unwrap(),
                Some(saved)
            );
            drop(store);
            let path = root.path().join(stream_dock_manager_storage::DATABASE_NAME);
            let before = std::fs::read(&path).unwrap();
            assert!(ProfileStore::open(root.path().join("profiles")).is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
    }
    #[test]
    // Verify that aggregate reference expansion is rejected at workspace ingress
    fn aggregate_reference_expansion_is_rejected_at_workspace_ingress() {
        let mut data = data();
        let call = Action::Builtin {
            action: Box::new(BuiltinAction::SavedMacro {
                macro_id: "keyboard-test".into(),
                arguments: Default::default(),
            }),
        };
        let repeat = Action::Builtin {
            action: Box::new(BuiltinAction::RepeatActions {
                count: 1000,
                interval_ms: 0,
                children: vec![call],
            }),
        };
        data.saved_macros.push(SavedMacro {
            id: "oversized".into(),
            name: "Too large".into(),
            inputs: vec![],
            action: Action::Builtin {
                action: Box::new(BuiltinAction::RepeatActions {
                    count: 1000,
                    interval_ms: 0,
                    children: vec![repeat],
                }),
            },
        });
        assert!(crate::profiles::validate_data(
            &crate::profiles::parse_data(serde_json::to_value(data).unwrap()).unwrap()
        )
        .unwrap_err()
        .contains("event budget"));
    }
}

#[cfg(test)]
mod fixed_record_descriptor_tests {
    use super::*;
    use serde_json::{json, Value};
    #[test]
    // Verify that inventory is unique and live snapshot decoders share bytes
    fn inventory_is_unique_and_live_snapshot_decoders_share_bytes() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        let mut keys = HashSet::new();
        for name in LocalPreference::ALL {
            assert!(keys.insert(name.key()));
            let live = name.record::<Value>();
            let snapshot = name.snapshot_record::<Value>();
            assert_eq!(live.key, snapshot.key);
            assert!(live.read(&storage).unwrap().is_none());
            let value = if name.metadata().3 {
                json!({"example": "é"})
            } else {
                json!(false)
            };
            live.write(&storage, &value).unwrap();
            assert_eq!(live.read(&storage).unwrap(), Some(value.clone()));
            let decoded = storage
                .run(move |connection| snapshot.read_from(connection))
                .unwrap();
            assert_eq!(decoded, Some(value));
            let invalid = name.record::<Value>();
            storage
                .run(move |connection| {
                    connection
                        .execute_batch("PRAGMA ignore_check_constraints=ON")
                        .map_err(|e| e.to_string())?;
                    connection
                        .execute(
                            "UPDATE local_service_preferences SET payload='{' WHERE key=?1",
                            [invalid.key],
                        )
                        .map_err(|e| e.to_string())?;
                    assert!(invalid.read_from(connection).is_err());
                    assert!(name
                        .snapshot_record::<Value>()
                        .read_from(connection)
                        .is_err());
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            keys,
            HashSet::from([
                "sessions",
                "drafts",
                "parameter-overrides",
                "plugin-profile-prompts",
                "library-organization",
                "plugin-update-policies",
                "import-repair-recipes",
                "updates",
                "builtin-values",
                "device-power",
                "resource-alerts",
                "restart-recovery-once",
                "configuration-recovery-bypass",
            ])
        );
    }
    #[test]
    // Verify that snapshot container and recipe bound do not narrow live acceptance
    fn snapshot_container_and_recipe_bound_do_not_narrow_live_acceptance() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        for name in LocalPreference::ALL
            .into_iter()
            .filter(|name| name.metadata().3)
        {
            name.record::<Value>().write(&storage, &json!([])).unwrap();
            assert_eq!(
                name.record::<Value>().read(&storage).unwrap(),
                Some(json!([]))
            );
            storage
                .run(move |connection| {
                    assert!(validate_object(&name.snapshot_record::<Value>(), connection).is_err());
                    Ok(())
                })
                .unwrap();
        }
        let recipes = LocalPreference::ImportRepairRecipes;
        let value = json!({"padding": "x".repeat(1024 * 1024)});
        recipes.record::<Value>().write(&storage, &value).unwrap();
        assert!(recipes.record::<Value>().read(&storage).unwrap().is_some());
        storage
            .run(move |connection| {
                assert!(recipes
                    .snapshot_record::<Value>()
                    .read_from(connection)
                    .is_err());
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify that accessibility and boolean descriptors preserve normal and snapshot rules
    fn accessibility_and_boolean_descriptors_preserve_normal_and_snapshot_rules() {
        use crate::application_data::{
            ACCESSIBILITY_RECORD, DIAGNOSTICS_RECORD, OFFLINE_ICONS_RECORD,
        };
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        assert!(ACCESSIBILITY_RECORD.read(&storage).unwrap().is_none());
        ACCESSIBILITY_RECORD
            .write(&storage, &Default::default())
            .unwrap();
        OFFLINE_ICONS_RECORD.write(&storage, &false).unwrap();
        DIAGNOSTICS_RECORD.write(&storage, &true).unwrap();
        storage
            .run(|connection| {
                validate_object(&ACCESSIBILITY_RECORD, connection)?;
                assert!(ACCESSIBILITY_RECORD.read_from(connection)?.is_some());
                assert_eq!(OFFLINE_ICONS_RECORD.read_from(connection)?, Some(false));
                assert_eq!(DIAGNOSTICS_RECORD.read_from(connection)?, Some(true));
                Ok(())
            })
            .unwrap();
        // Preserve the prior live typed-array behavior; snapshots explicitly require an object
        storage
            .write_json(
                Domain::ManagerPreferences,
                ACCESSIBILITY_RECORD.key,
                &json!([false, false, "system", "system", true]),
                4096,
            )
            .unwrap();
        assert!(ACCESSIBILITY_RECORD.read(&storage).unwrap().is_some());
        storage
            .run(|connection| {
                assert!(validate_object(&ACCESSIBILITY_RECORD, connection).is_err());
                Ok(())
            })
            .unwrap();
    }
}
