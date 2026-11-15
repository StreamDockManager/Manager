//! Installation-local favorites and tags, excluded from portable workspace/history
use crate::Runtime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryIdentity {
    Builtin {
        id: String,
    },
    Plugin {
        plugin_id: String,
        action_id: String,
    },
    Macro {
        id: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryMark {
    pub identity: LibraryIdentity,
    pub favorite: bool,
    pub tags: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryOrganization {
    #[ts(type = "number")]
    pub revision: u64,
    pub entries: Vec<LibraryMark>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredOrganization {
    schema_version: u32,
    organization: LibraryOrganization,
}
impl LibraryIdentity {
    // Report the valid condition for library organization
    fn valid(&self) -> bool {
        let valid =
            |id: &str| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control);
        match self {
            Self::Builtin { id } | Self::Macro { id } => valid(id),
            Self::Plugin {
                plugin_id,
                action_id,
                ..
            } => valid(plugin_id) && valid(action_id),
        }
    }
    // Report the exists condition for library organization
    fn exists(&self, runtime: &Runtime) -> bool {
        match self {
            Self::Builtin { id } => {
                crate::builtins::catalog()
                    .iter()
                    .any(|entry| entry["id"] == *id)
                    || serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
                        "../../../src/builtin-catalog.json"
                    ))
                    .is_ok_and(|entries| entries.iter().any(|entry| entry["id"] == *id))
            }
            Self::Macro { id } => runtime.snapshot().saved_macros.iter().any(|m| m.id == *id),
            Self::Plugin {
                plugin_id,
                action_id,
                ..
            } => runtime.snapshot().plugins.iter().any(|plugin| {
                plugin.id == *plugin_id
                    && plugin.actions.iter().any(|action| action.id == *action_id)
            }),
        }
    }
}
// Trim, validate and deduplicate library tags
fn tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut result = vec![];
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        if tag.len() > 80 || tag.contains(',') || tag.chars().any(char::is_control) {
            return Err("Tags allow at most 80 bytes and no commas or control characters".into());
        }
        if seen.insert(tag.to_lowercase()) {
            result.push(tag.to_string());
        }
        if result.len() > 16 {
            return Err("Use at most 16 tags per library item".into());
        }
    }
    result.sort_by_key(|tag| tag.to_lowercase());
    Ok(result)
}

// Validate stored
fn validate_stored(stored: StoredOrganization) -> Result<LibraryOrganization, String> {
    if stored.schema_version != 1
        || stored.organization.entries.len() > 4096
        || stored.organization.revision > 9_007_199_254_740_991
    {
        return Err("Unsupported library organization preferences; original file preserved".into());
    }
    let mut identities = BTreeSet::new();
    for entry in &stored.organization.entries {
        if !entry.identity.valid()
            || tags(&entry.tags)? != entry.tags
            || !identities.insert(serde_json::to_string(&entry.identity).unwrap())
        {
            return Err(
                "Invalid or duplicate library organization entry; original file preserved".into(),
            );
        }
    }
    Ok(stored.organization)
}
// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let Some(stored): Option<StoredOrganization> =
        crate::profile_database::LocalPreference::LibraryOrganization
            .snapshot_record()
            .read_from(connection)?
    else {
        return Ok(());
    };
    validate_stored(stored).map(|_| ())
}
impl Runtime {
    // Reconcile plugin library marks
    pub(crate) fn reconcile_plugin_library_marks(
        &self,
        installed: &BTreeSet<String>,
    ) -> Result<(), String> {
        let _change = self.library_preferences.lock().unwrap();
        let mut organization = self.read_library_organization()?;
        let before = organization.entries.len();
        organization.entries.retain(|entry| match &entry.identity {
            LibraryIdentity::Plugin { plugin_id, .. } => installed.contains(plugin_id),
            _ => true,
        });
        if organization.entries.len() != before {
            organization.revision += 1;
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::LibraryOrganization,
                &StoredOrganization {
                    schema_version: 1,
                    organization,
                },
            )?;
        }
        Ok(())
    }
    // Read library organization
    fn read_library_organization(&self) -> Result<LibraryOrganization, String> {
        let Some(stored): Option<StoredOrganization> = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::LibraryOrganization)?
        else {
            return Ok(Default::default());
        };
        validate_stored(stored)
    }
    // Read library organization while serializing preference access
    pub fn library_organization(&self) -> Result<LibraryOrganization, String> {
        let _change = self.library_preferences.lock().unwrap();
        self.read_library_organization()
    }
    // Save library mark
    pub fn save_library_mark(
        &self,
        expected_revision: u64,
        mut mark: LibraryMark,
    ) -> Result<LibraryOrganization, String> {
        let _change = self.library_preferences.lock().unwrap();
        if !mark.identity.valid() {
            return Err("Invalid library identity".into());
        }
        mark.tags = tags(&mark.tags)?;
        let mut organization = self.read_library_organization()?;
        if organization.revision != expected_revision {
            return Err("Personal organization changed. Reload before saving.".into());
        }
        let empty = !mark.favorite && mark.tags.is_empty();
        if !empty && !mark.identity.exists(self) {
            return Err("This library item is no longer available".into());
        }
        organization
            .entries
            .retain(|entry| entry.identity != mark.identity);
        if !empty {
            organization.entries.push(mark);
        }
        if organization.entries.len() > 4096 {
            return Err("Personal organization exceeds 4,096 library entries".into());
        }
        organization.revision = organization
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= 9_007_199_254_740_991)
            .ok_or("Library preference revision exhausted")?;
        let stored = StoredOrganization {
            schema_version: 1,
            organization,
        };
        if serde_json::to_vec(&stored)
            .map_err(|e| e.to_string())?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("Personal organization exceeds 2 MiB".into());
        }
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::LibraryOrganization,
            &stored,
        )?;
        Ok(stored.organization)
    }
}

// Discard the selected plugin in state
pub(crate) fn forget_plugin_in(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    id: &str,
) -> Result<(), String> {
    validate_storage_snapshot(connection)?;
    if let Some(mut stored) = crate::profile_database::LocalPreference::LibraryOrganization
        .record::<StoredOrganization>()
        .read_from(connection)?
    {
        let count = stored.organization.entries.len();
        stored.organization.entries.retain(|mark| !matches!(&mark.identity, LibraryIdentity::Plugin { plugin_id, .. } if plugin_id == id));
        if count != stored.organization.entries.len() {
            stored.organization.revision = stored
                .organization
                .revision
                .checked_add(1)
                .ok_or("Library revision exhausted")?;
            crate::profile_database::LocalPreference::LibraryOrganization
                .record::<StoredOrganization>()
                .write_to(connection, &stored)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires disposable host loopback sockets; local preferences only"]
    // Verify that favorites are local atomic and revision checked
    fn favorites_are_local_atomic_and_revision_checked() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let mark = LibraryMark {
            identity: LibraryIdentity::Builtin {
                id: "type_text".into(),
            },
            favorite: true,
            tags: vec![" Work ".into(), "work".into(), "travel".into()],
        };
        let result = runtime.save_library_mark(0, mark.clone()).unwrap();
        assert_eq!(result.revision, 1);
        assert_eq!(result.entries[0].tags, ["travel", "Work"]);
        assert!(runtime
            .save_library_mark(0, mark.clone())
            .unwrap_err()
            .contains("changed"));
        assert_eq!(runtime.library_organization().unwrap().revision, 1);
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        assert!(runtime.snapshot().history.undo.is_empty());
        let archive = root.path().join("portable.zip");
        runtime.profiles.export(&archive).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        assert!((0..zip.len()).all(|index| !zip
            .by_index(index)
            .unwrap()
            .name()
            .unwrap()
            .contains("organization")));
        let mut cleared = mark;
        cleared.favorite = false;
        cleared.tags.clear();
        assert!(runtime
            .save_library_mark(1, cleared)
            .unwrap()
            .entries
            .is_empty());
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no effects"]
    // Verify that invalid preferences and unknown identities do not replace existing data
    fn invalid_preferences_and_unknown_identities_do_not_replace_existing_data() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let missing = LibraryMark {
            identity: LibraryIdentity::Macro {
                id: "not-imported".into(),
            },
            favorite: true,
            tags: vec![],
        };
        assert!(runtime.save_library_mark(0, missing.clone()).is_err());
        runtime
            .profiles
            .write_preference(
                crate::profile_database::LocalPreference::LibraryOrganization,
                &serde_json::json!(["invalid original"]),
            )
            .unwrap();
        assert!(runtime.library_organization().is_err());
        assert!(runtime.save_library_mark(0, missing).is_err());
        assert_eq!(
            runtime
                .profiles
                .read_preference::<serde_json::Value>(
                    crate::profile_database::LocalPreference::LibraryOrganization
                )
                .unwrap()
                .unwrap(),
            serde_json::json!(["invalid original"])
        );
        runtime.shutdown();
    }
}
