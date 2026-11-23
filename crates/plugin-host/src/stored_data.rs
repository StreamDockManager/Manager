//! Metadata-only inventory and generation-checked deletion of plugin-owned durable data
use super::*;
use sha2::{Digest, Sha256};
use stream_dock_manager_storage::{rusqlite::Connection, Domain};

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct StoredPluginData {
    pub plugin_id: String,
    pub name: String,
    pub installed: bool,
    pub enabled: bool,
    pub retained: bool,
    /// Current HostStore namespaces are durable; no disposable cache kind is declared in v1
    #[ts(type = "number")]
    pub cache_bytes: u64,
    #[ts(type = "number | null")]
    pub updated_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub removed_ms: Option<u64>,
    #[ts(type = "number")]
    pub record_count: u64,
    #[ts(type = "number")]
    pub logical_bytes: u64,
    pub generation: String,
    pub review_token: String,
}
// Read the current time in milliseconds
pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
// Mark updated
pub(super) fn mark_updated(connection: &Connection, owner: &str) -> Result<(), String> {
    stream_dock_manager_storage::write_json(
        connection,
        Domain::PluginMetadata,
        &format!("storage-updated:{owner}"),
        &now_ms(),
        128,
    )
}
// Build the stored-data summary
fn summary(
    conn: &Connection,
    owner: &str,
    name: String,
    installed: bool,
    retained: bool,
) -> Result<StoredPluginData, String> {
    let generation = storage_rpc::plugin_storage_generation(conn, owner)?;
    let mut digest = Sha256::new();
    digest.update(generation.as_bytes());
    let mut count = 0u64;
    let mut bytes = 0u64;
    for domain in [Domain::PluginGlobals, Domain::PluginDefaults] {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT revision FROM {} WHERE key=?1",
                domain.table()
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([owner]).map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let revision: i64 = row.get(0).map_err(|e| e.to_string())?;
            // Measure the logical settings, including values now held in the encrypted database.
            let size = stream_dock_manager_storage::read_json::<Value>(
                conn,
                domain,
                owner,
                stream_dock_manager_storage::MAX_RECORD_BYTES,
            )?
            .ok_or("Plugin settings disappeared while reading their size")?
            .to_string()
            .len() as u64;
            count += 1;
            bytes += size;
            digest.update(format!("{}:{revision}:{size};", domain.table()).as_bytes());
        }
    }
    let (private_count,private_bytes):(u64,u64)=conn.query_row("SELECT count(*),coalesce(sum(encoded_bytes),0) FROM settings_vault.plugin_storage_records WHERE plugin_id=?1",[owner],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    count += private_count;
    bytes += private_bytes;
    let mut statement=conn.prepare("SELECT namespace,revision FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1 ORDER BY namespace COLLATE BINARY").map_err(|e|e.to_string())?;
    let namespaces = statement
        .query_map([owner], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, [u8; 8]>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in namespaces {
        let (namespace, revision) = row.map_err(|e| e.to_string())?;
        bytes += namespace.len() as u64;
        digest.update(serde_json::to_vec(&(namespace, revision)).map_err(|e| e.to_string())?);
    }
    digest.update(format!("{count}:{bytes}:{retained}").as_bytes());
    let updated_ms = stream_dock_manager_storage::read_json(
        conn,
        Domain::PluginMetadata,
        &format!("storage-updated:{owner}"),
        128,
    )?;
    let removed_ms = stream_dock_manager_storage::read_json(
        conn,
        Domain::PluginMetadata,
        &format!("storage-removed:{owner}"),
        128,
    )?;
    digest
        .update(serde_json::to_vec(&(updated_ms, removed_ms)).map_err(|error| error.to_string())?);
    Ok(StoredPluginData {
        enabled: false,
        cache_bytes: 0,
        updated_ms,
        removed_ms,
        plugin_id: owner.into(),
        name,
        installed,
        retained,
        record_count: count,
        logical_bytes: bytes,
        generation,
        review_token: hex::encode(digest.finalize()),
    })
}
impl PluginHost {
    // Inspect the data retained for the selected plugin
    pub fn stored_data(&self) -> Result<Vec<StoredPluginData>, String> {
        let names: HashMap<_, _> = self
            .plugins
            .iter()
            .map(|(id, p)| (id.clone(), p.info.name.clone()))
            .collect();
        let retained = self.hub.lock().unwrap().retained.clone();
        let enabled: HashSet<_> = self
            .plugins
            .iter()
            .filter(|(_, plugin)| plugin.info.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        self.storage().run(move|conn| {
            let mut ids:std::collections::BTreeSet<String>=names.keys().chain(retained.keys()).cloned().collect();
            let mut stmt=conn.prepare("SELECT key FROM plugin_globals UNION SELECT key FROM plugin_defaults UNION SELECT plugin_id FROM settings_vault.plugin_storage_namespaces").map_err(|e|e.to_string())?;
            for id in stmt.query_map([],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())? {ids.insert(id.map_err(|e|e.to_string())?);}
            if ids.len()>4096 {return Err("Stored plugin inventory exceeds limit".into());}
            ids.into_iter().map(|id| {let mut row=summary(conn,&id,names.get(&id).or_else(||retained.get(&id)).cloned().unwrap_or_else(||id.clone()),names.contains_key(&id),retained.contains_key(&id))?;row.enabled=enabled.contains(&id);Ok(row)}).collect()
        })
    }
    // Restore stored data
    pub fn restore_stored_data(
        &mut self,
        id: &str,
        expected_review_token: &str,
    ) -> Result<(), String> {
        let plugin = self
            .plugins
            .get(id)
            .ok_or("Install the plugin before restoring its retained data")?;
        if plugin.info.enabled {
            return Err("Disable the plugin before restoring retained data".into());
        }
        let mut hub = self.hub.lock().unwrap();
        let name = hub
            .retained
            .get(id)
            .cloned()
            .ok_or("This plugin has no retained data")?;
        let owner = id.to_owned();
        let expected = expected_review_token.to_owned();
        hub.retained.remove(id);
        if let Err(error) = hub.persist_with(move |tx| {
            // The stored marker still exists until this transaction commits its candidate
            if summary(tx, &owner, String::new(), true, true)?.review_token != expected {
                return Err(
                    "Stored plugin data changed; refresh the review before restoring".into(),
                );
            }
            stream_dock_manager_storage::delete(
                tx,
                Domain::PluginMetadata,
                &format!("storage-removed:{owner}"),
            )?;
            Ok(())
        }) {
            hub.retained.insert(id.into(), name);
            return Err(error);
        }
        Ok(())
    }
    // Remove the selected plugin’s retained data
    pub fn purge_stored_data(
        &mut self,
        id: &str,
        expected_review_token: &str,
    ) -> Result<(), String> {
        self.purge_stored_data_batch(vec![(id.into(), expected_review_token.into())])
    }
    /// All selected reviews are checked again inside one transaction before any owned data is removed
    pub fn purge_stored_data_batch(
        &mut self,
        selections: Vec<(String, String)>,
    ) -> Result<(), String> {
        let mut selected = HashSet::new();
        if selections.is_empty()
            || selections.len() > 256
            || selections.iter().any(|(id, token)| {
                !safe_id(id)
                    || token.len() != 64
                    || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || !selected.insert(id.clone())
            })
        {
            return Err("Invalid or duplicate plugin data selection".into());
        }
        let reviews = self.stored_data()?;
        for (id, token) in &selections {
            if reviews
                .iter()
                .find(|review| &review.plugin_id == id)
                .is_none_or(|review| &review.review_token != token)
            {
                return Err(
                    "Stored plugin data changed; refresh the review before deleting".into(),
                );
            }
        }
        for id in &selected {
            if self
                .plugins
                .get(id)
                .is_some_and(|plugin| plugin.info.enabled)
            {
                self.enable(id, false)?;
            }
        }
        let tokens: Vec<_> = self
            .hub
            .lock()
            .unwrap()
            .sessions
            .tokens
            .iter()
            .filter(|(_, owner)| selected.contains(*owner))
            .map(|(token, _)| token.clone())
            .collect();
        for token in tokens {
            self.close_inspector(&token);
        }
        let mut hub = self.hub.lock().unwrap();
        let owners = selected.clone();
        self.storage().transaction_with_cost(
            selections
                .iter()
                .map(|(id, token)| id.len() + token.len())
                .sum(),
            move |tx| {
                for (owner, expected) in &selections {
                    if summary(
                        tx,
                        owner,
                        String::new(),
                        false,
                        stream_dock_manager_storage::plugin_installations::get(tx, owner)?
                            .is_some_and(|state| state.retained_name.is_some()),
                    )?
                    .review_token
                        != *expected
                    {
                        return Err(
                            "Stored plugin data changed; refresh the review before deleting".into(),
                        );
                    }
                }
                for owner in &owners {
                    stream_dock_manager_storage::delete(tx, Domain::PluginGlobals, owner)?;
                    stream_dock_manager_storage::delete(tx, Domain::PluginDefaults, owner)?;
                    storage_rpc::purge(tx, owner)?;
                    mark_updated(tx, owner)?;
                    stream_dock_manager_storage::delete(
                        tx,
                        Domain::PluginMetadata,
                        &format!("storage-removed:{owner}"),
                    )?;
                    stream_dock_manager_storage::delete(
                        tx,
                        Domain::PluginMetadata,
                        &format!("retained-family:{owner}"),
                    )?;
                    stream_dock_manager_storage::plugin_installations::clear_retained(tx, owner)?;
                }
                Ok(())
            },
        )?;
        hub.invalidate_persistence_sizes();
        for id in &selected {
            hub.context_state
                .binding_settings
                .retain(|key, _| !key.starts_with(&format!("{id}:")));
            hub.globals.remove(id);
            hub.action_defaults.remove(id);
            hub.retained.remove(id);
            hub.context_state
                .settings
                .retain(|context, _| !context.starts_with(&format!("{id}:")));
        }
        hub.refresh_log_secrets();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify retained data survives uninstall restart and requires explicit restore after reinstall
    fn retained_data_survives_uninstall_restart_and_requires_explicit_restore_after_reinstall() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("package"), "fixture.retained");
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals
                .insert("fixture.retained".into(), json!({"unknown":"saved"}));
            hub.persist().unwrap();
        }
        host.delete_with_retention("fixture.retained", true)
            .unwrap();
        let row = host.stored_data().unwrap().pop().unwrap();
        assert!(!row.installed && row.retained);
        assert_eq!(row.record_count, 1);
        assert!(host.reload().unwrap().removed.is_empty());
        drop(host);
        let mut host = lifecycle_tests::host(root.path());
        assert!(host.stored_data().unwrap()[0].retained);
        lifecycle_tests::package(&root.path().join("package"), "fixture.retained");
        host.reload().unwrap();
        assert!(host
            .enable("fixture.retained", true)
            .unwrap_err()
            .contains("retained data"));
        let row = host.stored_data().unwrap().pop().unwrap();
        host.restore_stored_data("fixture.retained", &row.review_token)
            .unwrap();
        host.enable("fixture.retained", true).unwrap();
        assert_eq!(
            host.hub.lock().unwrap().globals["fixture.retained"]["unknown"],
            "saved"
        );
    }
    #[test]
    // Verify purge checks latest review and keeps workspace references
    fn purge_checks_latest_review_and_keeps_workspace_references() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("package"), "fixture.purge");
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals
                .insert("fixture.purge".into(), json!({"value":1}));
            hub.persist().unwrap();
        }
        host.storage()
            .write_json(
                Domain::Workspace,
                "reference",
                &json!({"plugin_id":"fixture.purge"}),
                1024,
            )
            .unwrap();
        let original = host.stored_data().unwrap().pop().unwrap();
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals
                .insert("fixture.purge".into(), json!({"value":2}));
            hub.persist().unwrap();
        }
        assert!(host
            .purge_stored_data("fixture.purge", &original.review_token)
            .is_err());
        assert_eq!(
            host.hub.lock().unwrap().globals["fixture.purge"]["value"],
            2
        );
        let latest = host.stored_data().unwrap().pop().unwrap();
        host.purge_stored_data("fixture.purge", &latest.review_token)
            .unwrap();
        {
            let mut hub = host.hub.lock().unwrap();
            assert!(hub.persistence_sizes.lock().unwrap().is_none());
            hub.globals
                .insert("fixture.after-purge".into(), json!({"saved":true}));
            hub.persist_scoped(persistence::Scope {
                globals: &["fixture.after-purge"],
                defaults: &[],
                metadata: &[],
            })
            .unwrap();
            assert!(!Hub::load(hub.store.as_ref().unwrap())
                .unwrap()
                .globals
                .contains_key("fixture.purge"));
        }
        let row = host
            .stored_data()
            .unwrap()
            .into_iter()
            .find(|row| row.plugin_id == "fixture.purge")
            .unwrap();
        assert_eq!(row.record_count, 0);
        assert_ne!(row.generation, latest.generation);
        assert!(host
            .storage()
            .read_json::<Value>(Domain::Workspace, "reference", 1024)
            .unwrap()
            .is_some());
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    // Verify batch purge rejects stale reviews and rolls back every selected owner on failure
    fn batch_purge_rejects_stale_reviews_and_rolls_back_every_selected_owner_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            for owner in ["first", "second", "unselected"] {
                hub.globals.insert(owner.into(), json!({"value":1}));
            }
            hub.persist().unwrap();
        }
        let initial = host.stored_data().unwrap();
        assert!(initial
            .iter()
            .all(|row| row.updated_ms.is_some() && row.cache_bytes == 0));
        let selections = |rows: &[StoredPluginData]| {
            rows.iter()
                .filter(|row| row.plugin_id != "unselected")
                .map(|row| (row.plugin_id.clone(), row.review_token.clone()))
                .collect::<Vec<_>>()
        };
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals.insert("second".into(), json!({"value":2}));
            hub.persist().unwrap();
        }
        assert!(host.purge_stored_data_batch(selections(&initial)).is_err());
        let latest = host.stored_data().unwrap();
        host.storage().run(|connection|connection.execute_batch("CREATE TRIGGER fail_second_purge BEFORE DELETE ON plugin_globals WHEN old.key='second' BEGIN SELECT RAISE(FAIL,'fixture'); END;").map_err(|e|e.to_string())).unwrap();
        assert!(host.purge_stored_data_batch(selections(&latest)).is_err());
        let restored = host.stored_data().unwrap();
        for row in &restored {
            let prior = latest
                .iter()
                .find(|prior| prior.plugin_id == row.plugin_id)
                .unwrap();
            assert_eq!(row.review_token, prior.review_token);
            assert_eq!(row.generation, prior.generation);
        }
        host.storage()
            .run(|connection| {
                connection
                    .execute_batch("DROP TRIGGER fail_second_purge")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        host.purge_stored_data_batch(selections(&latest)).unwrap();
        let rows = host.stored_data().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].plugin_id, "unselected");
        assert_eq!(
            rows[0].review_token,
            latest
                .iter()
                .find(|row| row.plugin_id == "unselected")
                .unwrap()
                .review_token
        );
    }
}
