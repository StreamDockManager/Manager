//! Installation-local, source-aware update holds and skipped semantic versions
use crate::Runtime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginUpdatePolicy {
    pub id: String,
    pub repository: String,
    pub channel: String,
    pub installation: String,
    pub held: bool,
    pub skipped_versions: Vec<String>,
    pub managed: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPolicies {
    schema_version: u32,
    entries: Vec<PluginUpdatePolicy>,
}
impl PluginUpdatePolicy {
    // Validate the supplied plugin policy value
    fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 512
            || !crate::updates::repository_valid(&self.repository)
            || !["stable", "development"].contains(&self.channel.as_str())
            || self.installation.len() != 64
            || !self.installation.bytes().all(|b| b.is_ascii_hexdigit())
            || !self.managed
            || self.skipped_versions.len() > 32
        {
            return Err("Invalid plugin update preference".into());
        }
        let mut versions = BTreeSet::new();
        for version in &self.skipped_versions {
            if version.len() > 80
                || crate::updates::stable_version(version).is_err()
                || !versions.insert(version)
            {
                return Err("Use at most 32 distinct stable semantic versions".into());
            }
        }
        Ok(())
    }
    // Report the suppresses condition for plugin policy
    pub(crate) fn suppresses(
        &self,
        id: &str,
        repository: &str,
        channel: &str,
        installation: &str,
        version: &str,
    ) -> bool {
        self.id == id
            && self.repository == repository
            && self.channel == channel
            && self.installation == installation
            && (self.held || self.skipped_versions.iter().any(|v| v == version))
    }
}

// Validate stored
fn validate_stored(store: StoredPolicies) -> Result<Vec<PluginUpdatePolicy>, String> {
    if store.schema_version != 1 || store.entries.len() > 1024 {
        return Err("Unsupported plugin update preferences; file preserved".into());
    }
    let mut keys = BTreeSet::new();
    for policy in &store.entries {
        policy.validate()?;
        if !keys.insert((&policy.id, &policy.repository, &policy.channel)) {
            return Err("Duplicate plugin update preferences; file preserved".into());
        }
    }
    Ok(store.entries)
}
// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let Some(store): Option<StoredPolicies> =
        crate::profile_database::LocalPreference::PluginUpdatePolicies
            .snapshot_record()
            .read_from(connection)?
    else {
        return Ok(());
    };
    validate_stored(store).map(|_| ())
}
impl Runtime {
    // Reconcile plugin update preferences
    pub(crate) fn reconcile_plugin_update_preferences(
        &self,
        installed: &BTreeSet<String>,
    ) -> Result<(), String> {
        let _change = self.plugin_preferences.lock().unwrap();
        let mut entries = self.read_plugin_policies()?;
        let before = entries.len();
        entries.retain(|p| installed.contains(&p.id));
        if entries.len() != before {
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::PluginUpdatePolicies,
                &StoredPolicies {
                    schema_version: 1,
                    entries,
                },
            )?;
        }
        Ok(())
    }
    // Read plugin update policies while serializing preference changes
    pub(crate) fn stored_plugin_policies(&self) -> Result<Vec<PluginUpdatePolicy>, String> {
        let _change = self.plugin_preferences.lock().unwrap();
        self.read_plugin_policies()
    }
    // Read plugin policies
    fn read_plugin_policies(&self) -> Result<Vec<PluginUpdatePolicy>, String> {
        let Some(store): Option<StoredPolicies> = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::PluginUpdatePolicies)?
        else {
            return Ok(vec![]);
        };
        validate_stored(store)
    }
    // Read the plugin’s configured update policy
    pub fn plugin_update_policy(&self, id: &str) -> Result<PluginUpdatePolicy, String> {
        let host = self.plugins.lock().unwrap();
        let installation = host
            .installation_token(id)
            .ok_or("Plugin is not installed")?;
        let provenance = host.provenance(id);
        drop(host);
        let repository = provenance
            .as_ref()
            .and_then(|p| p["repository"].as_str())
            .unwrap_or("")
            .to_string();
        let channel = provenance
            .as_ref()
            .and_then(|p| p["channel"].as_str())
            .unwrap_or("")
            .to_string();
        let managed = crate::updates::repository_valid(&repository)
            && ["stable", "development"].contains(&channel.as_str());
        let entries = self.stored_plugin_policies()?;
        Ok(entries
            .into_iter()
            .find(|p| {
                p.id == id
                    && p.repository == repository
                    && p.channel == channel
                    && p.installation == installation
            })
            .unwrap_or(PluginUpdatePolicy {
                id: id.into(),
                repository,
                channel,
                installation,
                held: false,
                skipped_versions: vec![],
                managed,
            }))
    }
    // Save plugin update policy
    pub fn save_plugin_update_policy(
        &self,
        mut value: PluginUpdatePolicy,
    ) -> Result<PluginUpdatePolicy, String> {
        value.skipped_versions.sort();
        value.skipped_versions.dedup();
        value.validate()?;
        // Capture package identity before taking the preference writer; installation commits use
        // the host lock and never acquire this writer while holding it
        let current = self.plugin_update_policy(&value.id)?;
        if !current.managed
            || current.installation != value.installation
            || current.repository != value.repository
            || current.channel != value.channel
        {
            return Err("Installed package/source changed; reload update preferences".into());
        }
        let _change = self.plugin_preferences.lock().unwrap();
        let host = self.plugins.lock().unwrap();
        let provenance = host.provenance(&value.id);
        if host.installation_token(&value.id).as_deref() != Some(value.installation.as_str())
            || provenance.as_ref().is_none_or(|p| {
                p["repository"] != value.repository || p["channel"] != value.channel
            })
        {
            return Err("Installed package/source changed; reload update preferences".into());
        }
        let mut entries = self.read_plugin_policies()?;
        entries.retain(|p| p.id != value.id);
        if value.held || !value.skipped_versions.is_empty() {
            entries.push(value.clone());
        }
        if entries.len() > 1024 {
            return Err("Plugin update preferences exceed 1,024 entries".into());
        }
        let store = StoredPolicies {
            schema_version: 1,
            entries,
        };
        if serde_json::to_vec(&store).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
            return Err("Plugin update preferences exceed 1 MiB".into());
        }
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::PluginUpdatePolicies,
            &store,
        )?;
        drop(host);
        Ok(value)
    }
}

// Discard the selected plugin in state
pub(crate) fn forget_plugin_in(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    id: &str,
) -> Result<(), String> {
    validate_storage_snapshot(connection)?;
    if let Some(mut stored) = crate::profile_database::LocalPreference::PluginUpdatePolicies
        .record::<StoredPolicies>()
        .read_from(connection)?
    {
        let count = stored.entries.len();
        stored.entries.retain(|entry| entry.id != id);
        if count != stored.entries.len() {
            crate::profile_database::LocalPreference::PluginUpdatePolicies
                .record::<StoredPolicies>()
                .write_to(connection, &stored)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that suppression is bound to source channel installation and semantic version
    fn suppression_is_bound_to_source_channel_installation_and_semantic_version() {
        let p = PluginUpdatePolicy {
            id: "plugin".into(),
            repository: "owner/repository".into(),
            channel: "stable".into(),
            installation: "a".repeat(64),
            held: false,
            skipped_versions: vec!["1.2.0".into()],
            managed: true,
        };
        p.validate().unwrap();
        assert!(p.suppresses(
            "plugin",
            "owner/repository",
            "stable",
            &"a".repeat(64),
            "1.2.0"
        ));
        assert!(!p.suppresses(
            "plugin",
            "owner/repository",
            "development",
            &"a".repeat(64),
            "1.2.0"
        ));
        assert!(!p.suppresses(
            "plugin",
            "other/repository",
            "stable",
            &"a".repeat(64),
            "1.2.0"
        ));
        assert!(!p.suppresses(
            "plugin",
            "owner/repository",
            "stable",
            &"b".repeat(64),
            "1.2.0"
        ));
        assert!(!p.suppresses(
            "plugin",
            "owner/repository",
            "stable",
            &"a".repeat(64),
            "1.2.1"
        ));
    }
}
