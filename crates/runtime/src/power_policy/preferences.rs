use super::DevicePowerPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::PathBuf;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    schema_version: u32,
    revision: u64,
    entries: BTreeMap<String, DevicePowerPolicy>,
}
impl Store {
    // Construct an empty schema-version-1 power preference store
    fn empty() -> Self {
        Self {
            schema_version: 1,
            ..Default::default()
        }
    }
}
pub(super) struct Preferences {
    storage: stream_dock_manager_storage::Storage,
    store: Store,
    pub failure: Option<String>,
}
// Read the requested preferences data
fn read(storage: &stream_dock_manager_storage::Storage) -> Result<Store, String> {
    let store: Store = crate::profile_database::LocalPreference::DevicePower
        .record()
        .read(storage)?
        .unwrap_or_else(Store::empty);
    validate_store(store)
}
// Validate store
fn validate_store(store: Store) -> Result<Store, String> {
    if store.schema_version != 1
        || store.revision > 9_007_199_254_740_991
        || store.entries.len() > 256
    {
        return Err(
            "Unsupported or oversized device power preferences; original file preserved".into(),
        );
    }
    for (key, policy) in &store.entries {
        if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
            return Err("Invalid device power identity; original file preserved".into());
        }
        policy.validate()?;
    }
    Ok(store)
}

// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let store = crate::profile_database::LocalPreference::DevicePower
        .snapshot_record()
        .read_from(connection)?
        .unwrap_or_else(Store::empty);
    validate_store(store).map(|_| ())
}

impl Preferences {
    #[cfg(test)]
    // Open the requested preferences resource
    pub fn open(path: PathBuf) -> Self {
        Self::with_storage(
            stream_dock_manager_storage::Storage::open(path.parent().unwrap().to_path_buf())
                .unwrap(),
        )
        .unwrap()
    }
    // Construct using storage
    pub fn with_storage(storage: stream_dock_manager_storage::Storage) -> Result<Self, String> {
        let store = read(&storage)?;
        Ok(Self {
            storage,
            store,
            failure: None,
        })
    }
    // Reload preferences from its current source
    pub fn reload(&mut self) -> Result<bool, String> {
        match read(&self.storage) {
            Ok(store) => {
                let changed = store != self.store || self.failure.is_some();
                self.store = store;
                self.failure = None;
                Ok(changed)
            }
            Err(error) => {
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }
    // Read the stored policy and revision, using the default policy when absent
    pub fn value(&self, key: &str) -> (u64, DevicePowerPolicy) {
        (
            self.store.revision,
            self.store.entries.get(key).cloned().unwrap_or_default(),
        )
    }
    // Copy stored policies only when preference loading has not failed
    pub fn entries(&self) -> BTreeMap<String, DevicePowerPolicy> {
        if self.failure.is_some() {
            BTreeMap::new()
        } else {
            self.store.entries.clone()
        }
    }
    // Save the supplied preferences state
    pub fn save(
        &mut self,
        key: &str,
        revision: u64,
        policy: DevicePowerPolicy,
    ) -> Result<u64, String> {
        policy.validate()?;
        if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
            return Err("Invalid device power identity".into());
        }
        self.reload()?;
        if self.store.revision != revision {
            return Err("Device power preferences changed; reload before saving".into());
        }
        let mut next = self.store.clone();
        next.entries.insert(key.into(), policy);
        if next.entries.len() > 256 {
            return Err("Device power preferences exceed 256 devices".into());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .filter(|value| *value <= 9_007_199_254_740_991)
            .ok_or("Device power preference revision exhausted")?;
        if serde_json::to_vec(&next)
            .map_err(|error| error.to_string())?
            .len()
            > 1024 * 1024
        {
            return Err("Device power preferences exceed 1 MiB".into());
        }
        crate::profile_database::LocalPreference::DevicePower
            .record::<Store>()
            .write(&self.storage, &next)?;
        self.store = next;
        self.failure = None;
        Ok(self.store.revision)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that policies survive restart are device scoped and stale writes are rejected
    fn policies_survive_restart_are_device_scoped_and_stale_writes_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("device-power.json");
        let mut one = Preferences::open(path.clone());
        let mut two = Preferences::with_storage(one.storage.clone()).unwrap();
        let policy = DevicePowerPolicy {
            idle_action: super::super::PowerAction::Sleep,
            enabled: true,
            ..Default::default()
        };
        let revision = one.save("device:first", 0, policy.clone()).unwrap();
        assert!(two
            .save("device:second", 0, DevicePowerPolicy::default())
            .is_err());
        let restored = Preferences::with_storage(one.storage.clone()).unwrap();
        assert_eq!(restored.value("device:first"), (revision, policy));
        assert!(!restored.value("device:second").1.enabled);
    }
    #[test]
    // Verify that invalid storage and failed writes preserve the last committed policy
    fn invalid_storage_and_failed_writes_preserve_the_last_committed_policy() {
        let root = tempfile::tempdir().unwrap();
        let storage =
            stream_dock_manager_storage::Storage::open(root.path().to_path_buf()).unwrap();
        let mut prefs = Preferences::with_storage(storage.clone()).unwrap();
        prefs.save("device", 0, Default::default()).unwrap();
        let previous = prefs.value("device");
        storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=ON")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(prefs.save("device", 1, Default::default()).is_err());
        assert_eq!(prefs.value("device"), previous);
        storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=OFF")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(
            Preferences::with_storage(storage).unwrap().value("device"),
            previous
        );
    }
}
