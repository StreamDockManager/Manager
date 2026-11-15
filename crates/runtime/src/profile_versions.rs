//! Ordinary profile history uses the same integrity-checked records as recovery
//! It never opens the exclusive recovery writer or restores unrelated workspace state
use crate::{
    configuration_recovery::{
        ConfigurationRecoveryDocument, ConfigurationRecoveryVersion, ConfigurationRecoveryVersions,
    },
    Runtime,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
// Compute a content digest for profile versions
fn digest(profile: &crate::profiles::Profile) -> Result<Vec<u8>, String> {
    Ok(Sha256::digest(serde_json::to_vec(profile).map_err(|e| e.to_string())?).to_vec())
}
impl Runtime {
    // Enumerate distinct retained profile versions within revision, history-count and byte limits
    pub fn profile_versions(
        &self,
        profile_id: &str,
        expected: u64,
    ) -> Result<ConfigurationRecoveryVersions, String> {
        let data = self.profiles.load()?;
        if data.configuration_revision != expected {
            return Err("Workspace changed. Reopen profile versions.".into());
        }
        let current = data
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile not found")?;
        let mut seen = HashSet::from([digest(current)?]);
        let mut items = data
            .history
            .undo
            .into_iter()
            .chain(data.history.redo)
            .collect::<Vec<_>>();
        items.sort_by_key(|i| std::cmp::Reverse(i.timestamp_ms));
        let mut versions = Vec::new();
        let mut warnings = Vec::new();
        if items.len() > 80 {
            warnings.push("Only the latest 80 retained history entries were inspected.".into());
        }
        let mut scanned = 0usize;
        for item in items.into_iter().take(80) {
            scanned = scanned.saturating_add(item.bytes);
            if scanned > 160 * 1024 * 1024 {
                warnings.push("History scan limited to 160 MiB.".into());
                break;
            }
            let (_, before, after, _lease) = self.profiles.inspect_history(expected, &item.id)?;
            for (side, data) in [("before", before), ("after", after)] {
                if let Some(profile) = data.profiles.iter().find(|p| p.id == profile_id) {
                    if seen.insert(digest(profile)?) {
                        versions.push(ConfigurationRecoveryVersion {
                            id: format!("{}:{side}", item.id),
                            label: format!("{side}: {}", item.description),
                            source: "history".into(),
                            timestamp_ms: Some(item.timestamp_ms),
                            valid: true,
                            issues: vec![],
                        });
                    }
                }
            }
            if versions.len() >= 64 {
                versions.truncate(64);
                warnings.push("Showing at most 64 retained versions.".into());
                break;
            }
        }
        warnings.push("Retained history only. Older local backups are available in configuration recovery. Restore affects this profile only, not installed plugins, media files or shared macros. Current workspace validation still applies.".into());
        Ok(ConfigurationRecoveryVersions {
            revision: expected.to_string(),
            versions,
            warnings,
        })
    }
    // Read the selected before-or-after profile version as a revision-bound recovery document
    pub fn profile_version(
        &self,
        profile_id: &str,
        expected: u64,
        version: &str,
    ) -> Result<ConfigurationRecoveryDocument, String> {
        let (id, side) = version.rsplit_once(':').ok_or("Invalid profile version")?;
        let (_, before, after, _lease) = self.profiles.inspect_history(expected, id)?;
        let data = match side {
            "before" => before,
            "after" => after,
            _ => return Err("Invalid profile version side".into()),
        };
        let profile = data
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile is absent from this version")?;
        Ok(ConfigurationRecoveryDocument {
            id: profile_id.into(),
            revision: expected.to_string(),
            json: serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?,
            issues: vec![],
            diagnostics: None,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that versions are read only and revision guarded
    fn versions_are_read_only_and_revision_guarded() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let before = runtime.snapshot();
        let mut profile = before.profiles[0].clone();
        let original = profile.name.clone();
        profile.name = "Changed".into();
        let changed = runtime
            .save_profile(profile.clone(), before.configuration_revision)
            .unwrap();
        let list = runtime
            .profile_versions(&profile.id, changed.configuration_revision)
            .unwrap();
        let old = list
            .versions
            .iter()
            .find(|v| v.label.starts_with("before:"))
            .unwrap();
        let doc = runtime
            .profile_version(&profile.id, changed.configuration_revision, &old.id)
            .unwrap();
        let restored: crate::profiles::Profile = serde_json::from_str(&doc.json).unwrap();
        assert_eq!(restored.name, original);
        assert_eq!(
            runtime.snapshot().configuration_revision,
            changed.configuration_revision
        );
        assert!(runtime
            .profile_version(&profile.id, before.configuration_revision, &old.id)
            .is_err());
        let result = runtime
            .save_profile(restored, changed.configuration_revision)
            .unwrap();
        assert_eq!(
            result
                .profiles
                .iter()
                .find(|p| p.id == profile.id)
                .unwrap()
                .name,
            original
        );
    }
}
