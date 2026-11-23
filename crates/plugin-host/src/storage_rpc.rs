//! Authenticated private storage. The socket thread waits outside the Hub lock
use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};
use stream_dock_manager_storage::{
    rusqlite::{params, Connection, OptionalExtension},
    Storage,
};

#[cfg(test)]
use stream_dock_manager_storage::Domain;

const MAX_ENVELOPE: usize = 512 * 1024;
const MAX_VALUE: usize = 256 * 1024;
const LOGICAL_LIMIT: i64 = 8 * 1024 * 1024;
const RECEIPT_AGE: i64 = 24 * 60 * 60 * 1000;
pub(super) const PROTOCOL: &str = "plugin-storage-v1";

#[derive(Default)]
pub(super) struct Admission {
    owners: Mutex<HashMap<String, (usize, usize)>>,
}
struct Permit {
    admission: Arc<Admission>,
    owner: String,
    bytes: usize,
}
impl Drop for Permit {
    // Release resources held by this owner
    fn drop(&mut self) {
        let mut owners = self.admission.owners.lock().unwrap();
        if let Some((count, bytes)) = owners.get_mut(&self.owner) {
            *count -= 1;
            *bytes -= self.bytes;
            if *count == 0 {
                owners.remove(&self.owner);
            }
        }
    }
}
impl Admission {
    // Admit for the current operation
    fn admit(self: &Arc<Self>, owner: &str, bytes: usize) -> Result<Permit, String> {
        let mut owners = self.owners.lock().unwrap();
        let (count, size) = owners
            .values()
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
        if count >= 64
            || size + bytes > LOGICAL_LIMIT as usize
            || owners.get(owner).is_some_and(|v| v.0 >= 8)
        {
            return Err("busy".into());
        }
        let entry = owners.entry(owner.into()).or_default();
        entry.0 += 1;
        entry.1 += bytes;
        Ok(Permit {
            admission: self.clone(),
            owner: owner.into(),
            bytes,
        })
    }
}
// Execute the storage SQL operation
fn sql<T>(result: stream_dock_manager_storage::rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|error| match error {
        stream_dock_manager_storage::rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                stream_dock_manager_storage::rusqlite::ErrorCode::DatabaseBusy
                    | stream_dock_manager_storage::rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            "busy".into()
        }
        _ => "storage_failure".into(),
    })
}
// Require the requested JSON field to contain a string
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| "invalid_request".into())
}
// Check that the storage name is nonempty, bounded and contains no NUL
fn name(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0')
}
// Check the canonical bounded unsigned decimal representation
fn decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}
// Validate the identifier representation
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
// Validate the fields accepted by the storage request
fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    if !value
        .as_object()
        .is_some_and(|map| map.keys().all(|key| allowed.contains(&key.as_str())))
    {
        return Err("invalid_request".into());
    }
    Ok(())
}
// Read the current time for this subsystem
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Read the owner-specific purge counter from the encrypted SDK store.
pub fn plugin_storage_purge_generation(conn: &Connection, owner: &str) -> Result<u64, String> {
    read_purge_generation(conn, owner).map(|value| value as u64)
}

fn read_purge_generation(conn: &Connection, owner: &str) -> Result<i64, String> {
    if !safe_id(owner) {
        return Err("Invalid plugin identity".into());
    }
    let value: Option<i64> = sql(conn
        .query_row(
            "SELECT generation FROM settings_vault.plugin_storage_generations WHERE plugin_id=?1",
            [owner],
            |row| row.get(0),
        )
        .optional())?;
    match value {
        Some(value) if value > 0 => Ok(value),
        None => Ok(0),
        _ => Err("Invalid durable plugin generation".into()),
    }
}

/// The opaque generation binds both restore epochs and the durable owner purge counter
pub fn plugin_storage_generation(conn: &Connection, owner: &str) -> Result<String, String> {
    let epoch: String = sql(conn.query_row(
        "SELECT value FROM main.database_metadata WHERE key='storage_generation'",
        [],
        |r| r.get(0),
    ))?;
    let private_epoch = read_private_epoch(conn)?;
    let counter = plugin_storage_purge_generation(conn, owner)?;
    let digest = Sha256::digest(format!("{epoch}\0{private_epoch}\0{owner}\0{counter}").as_bytes());
    let bytes: [u8; 16] = digest[..16].try_into().unwrap();
    Ok(u128::from_be_bytes(bytes).to_string())
}
// Read the authenticated attachment's restore epoch
fn read_private_epoch(conn: &Connection) -> Result<String, String> {
    let epoch: String = sql(conn.query_row(
        "SELECT epoch FROM settings_vault.plugin_storage_epoch WHERE singleton=1",
        [],
        |row| row.get(0),
    ))?;
    if !uuid(&epoch) {
        return Err("Invalid private storage restore epoch".into());
    }
    Ok(epoch)
}

// Return the current revision identifier; zero represents no persisted revision
fn revision(conn: &Connection, owner: &str, namespace: &str) -> Result<u64, String> {
    let row: Option<[u8; 8]> = sql(conn.query_row(
        "SELECT revision FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1 AND namespace=?2",
        params![owner, namespace],
        |row| row.get(0),
    ).optional())?;
    match row.map(u64::from_be_bytes) {
        None => Ok(0),
        Some(n) if n > 0 => Ok(n),
        _ => Err("storage_failure".into()),
    }
}
// Validate for the current operation
fn validate(message: &Value, inspector: bool, bytes: usize) -> Result<(), String> {
    if inspector {
        return Err("unauthorized".into());
    }
    if message["protocol_id"] != PROTOCOL {
        return Err("unsupported".into());
    }
    if bytes > MAX_ENVELOPE {
        return Err("quota".into());
    }
    fields(
        message,
        &["event", "protocol_id", "request_id", "method", "payload"],
    )?;
    if !uuid(text(message, "request_id")?) {
        return Err("invalid_request".into());
    }
    let method = text(message, "method")?;
    if !["get", "listKeys", "receipt", "commit"].contains(&method) {
        return Err("unsupported".into());
    }
    let payload = &message["payload"];
    if !name(text(payload, "namespace")?, 128) {
        return Err("invalid_request".into());
    }
    match method {
        "get" => {
            fields(payload, &["namespace", "key"])?;
            if !name(text(payload, "key")?, 256) {
                return Err("invalid_request".into());
            }
        }
        "listKeys" => {
            fields(payload, &["namespace", "cursor"])?;
            if payload
                .get("cursor")
                .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 512))
            {
                return Err("invalid_request".into());
            }
        }
        "receipt" => {
            fields(payload, &["namespace", "generation", "operation_id"])?;
            if !decimal(text(payload, "generation")?) || !uuid(text(payload, "operation_id")?) {
                return Err("invalid_request".into());
            }
        }
        "commit" => {
            fields(
                payload,
                &[
                    "namespace",
                    "generation",
                    "expected_revision",
                    "operation_id",
                    "mutations",
                ],
            )?;
            if !decimal(text(payload, "generation")?)
                || !decimal(text(payload, "expected_revision")?)
                || !uuid(text(payload, "operation_id")?)
            {
                return Err("invalid_request".into());
            }
            let mutations = payload["mutations"]
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= 32)
                .ok_or("invalid_request")?;
            for mutation in mutations {
                if !name(text(mutation, "key")?, 256) {
                    return Err("invalid_request".into());
                }
                match text(mutation, "op")? {
                    "put" => {
                        fields(mutation, &["op", "key", "value"])?;
                        let value = mutation.get("value").ok_or("invalid_request")?;
                        if value.to_string().len() > MAX_VALUE {
                            return Err("quota".into());
                        }
                    }
                    "delete" => fields(mutation, &["op", "key"])?,
                    _ => return Err("invalid_request".into()),
                }
            }
        }
        _ => return Err("unsupported".into()),
    }
    Ok(())
}

// Dispatch for the current operation
pub(super) fn dispatch(
    storage: Storage,
    admission: Arc<Admission>,
    alive: Arc<AtomicBool>,
    owner: String,
    message: Value,
    inspector: bool,
    bytes: usize,
) -> Value {
    let result = (|| {
        validate(&message, inspector, bytes)?;
        let permit = admission.admit(&owner, bytes)?;
        let method = text(&message, "method")?.to_owned();
        let payload = message["payload"].clone();
        let generation = storage.generation();
        storage.run_at_generation(generation, bytes, move |conn| {
            let _permit = permit;
            if !alive.load(Ordering::Acquire) {
                return Err("cancelled_before_commit".into());
            }
            let tx = sql(conn.transaction())?;
            let result = execute(&tx, &owner, &method, &payload)?;
            // Commit and receipt share the transaction. No values or result are published before it
            if !alive.load(Ordering::Acquire) {
                return Err("cancelled_before_commit".into());
            }
            tx.commit().map_err(|_| {
                if method == "commit" {
                    "indeterminate"
                } else {
                    "storage_failure"
                }
            })?;
            Ok(result)
        })
    })();
    // Invalid envelopes must not reflect an unbounded caller-supplied object/string
    let protocol_id = message["protocol_id"]
        .as_str()
        .filter(|value| value.len() <= 64)
        .unwrap_or("");
    let request_id = message["request_id"]
        .as_str()
        .filter(|value| value.len() <= 36)
        .unwrap_or("");
    let mut response = json!({"event":"streamDockManagerRpcResponse","protocol_id":protocol_id,"request_id":request_id});
    match result {
        Ok(value) => response["result"] = value,
        Err(error) => {
            let code = match error.as_str() {
                "unsupported"
                | "unauthorized"
                | "invalid_request"
                | "quota"
                | "conflict"
                | "busy"
                | "cancelled_before_commit"
                | "storage_failure"
                | "indeterminate" => error.as_str(),
                "Storage worker stopped before reporting the operation outcome" => "indeterminate",
                "Storage operation belongs to an expired generation"
                | "Storage operation invalidated by maintenance" => "cancelled_before_commit",
                _ if error.to_lowercase().contains("admission")
                    || error.to_lowercase().contains("busy")
                    || error.to_lowercase().contains("maintenance") =>
                {
                    "busy"
                }
                _ => "storage_failure",
            };
            response["error"] = json!({"code":code,"message":match code {
                "unsupported"=>"Storage protocol or method is unsupported",
                "unauthorized"=>"This session cannot access private storage",
                "invalid_request"=>"Storage request is invalid",
                "quota"=>"Storage limit exceeded",
                "conflict"=>"Storage generation, revision, cursor, or operation conflicts",
                "busy"=>"Storage admission is busy",
                "cancelled_before_commit"=>"Session or storage epoch retired before commit",
                "indeterminate"=>"Commit outcome is uncertain; reconcile the original operation receipt",
                _=>"Storage operation failed",
            }});
        }
    }
    if response.to_string().len() > MAX_ENVELOPE {
        return json!({"event":"streamDockManagerRpcResponse","protocol_id":protocol_id,"request_id":request_id,"error":{"code":"quota","message":"Storage response exceeds its limit"}});
    }
    response
}

// Execute the requested storage operation
fn execute(conn: &Connection, owner: &str, method: &str, payload: &Value) -> Result<Value, String> {
    let namespace = text(payload, "namespace")?;
    let current_generation = plugin_storage_generation(conn, owner)?;
    let current_revision_value = revision(conn, owner, namespace)?;
    let current_revision = current_revision_value.to_string();
    match method {
        "get" => {
            let value:Option<(i64,String)>=sql(conn.query_row("SELECT encoded_bytes,value FROM settings_vault.plugin_storage_records WHERE plugin_id=?1 AND namespace=?2 AND key=?3",params![owner,namespace,text(payload,"key")?],|r|Ok((r.get(0)?,r.get(1)?))).optional())?;
            let mut result = json!({"found":value.is_some(),"generation":current_generation,"revision":current_revision});
            if let Some((bytes, value)) = value {
                if bytes > MAX_VALUE as i64 + 256 {
                    return Err("storage_failure".into());
                }
                result["value"] = serde_json::from_str(&value).map_err(|_| "storage_failure")?;
            }
            Ok(result)
        }
        "listKeys" => {
            let binding = hex::encode(Sha256::digest(format!("{owner}\0{namespace}").as_bytes()));
            let offset = if let Some(cursor) = payload.get("cursor") {
                let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(cursor.as_str().ok_or("invalid_request")?)
                    .map_err(|_| "invalid_request")?;
                let (b, g, r, o): (String, String, String, u32) =
                    serde_json::from_slice(&decoded).map_err(|_| "invalid_request")?;
                if b != binding || g != current_generation || r != current_revision || o > 4096 {
                    return Err("conflict".into());
                }
                o
            } else {
                0
            };
            let mut stmt=sql(conn.prepare("SELECT key FROM settings_vault.plugin_storage_records WHERE plugin_id=?1 AND namespace=?2 ORDER BY key COLLATE BINARY LIMIT 129 OFFSET ?3"))?;
            let mut keys =
                sql(stmt.query_map(params![owner, namespace, offset], |r| r.get::<_, String>(0)))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "storage_failure")?;
            let more = keys.len() > 128;
            keys.truncate(128);
            let mut result =
                json!({"keys":keys,"generation":current_generation,"revision":current_revision});
            if more {
                result["cursor"] = json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&(
                        binding,
                        current_generation,
                        current_revision,
                        offset + 128
                    ))
                    .map_err(|_| "storage_failure")?
                ));
            }
            if result.to_string().len() > 64 * 1024 {
                return Err("quota".into());
            }
            Ok(result)
        }
        "receipt" | "commit" => {
            if text(payload, "generation")? != current_generation {
                return Err("conflict".into());
            }
            let operation = text(payload, "operation_id")?;
            let receipt:Option<(String,[u8; 8])>=sql(conn.query_row("SELECT digest,revision FROM settings_vault.plugin_storage_receipts WHERE plugin_id=?1 AND namespace=?2 AND operation_id=?3 AND created_ms>=?4 AND generation=?5",params![owner,namespace,operation,now()-RECEIPT_AGE,current_generation],|r|Ok((r.get(0)?,r.get(1)?))).optional())?;
            if method == "receipt" {
                let mut result = json!({"found":receipt.is_some(),"generation":current_generation,"operation_id":operation});
                if let Some((_, revision)) = receipt {
                    result["revision"] = json!(u64::from_be_bytes(revision).to_string());
                    result["committed"] = json!(true);
                }
                return Ok(result);
            }
            // serde_json maps are sorted by UTF-8 key bytes; arrays retain their order.
            let digest = hex::encode(Sha256::digest(payload.to_string().as_bytes()));
            if let Some((previous, revision)) = receipt {
                if previous != digest {
                    return Err("conflict".into());
                }
                return Ok(
                    json!({"generation":current_generation,"revision":u64::from_be_bytes(revision).to_string(),"operation_id":operation,"committed":true}),
                );
            }
            if text(payload, "expected_revision")? != current_revision {
                return Err("conflict".into());
            }
            let next = current_revision_value.checked_add(1).ok_or("quota")?;
            let next_blob = next.to_be_bytes();
            let namespaces: i64 = sql(conn.query_row(
                "SELECT count(*) FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1",
                [owner],
                |r| r.get(0),
            ))?;
            if current_revision_value == 0 && namespaces >= 64 {
                return Err("quota".into());
            }
            sql(conn.execute("INSERT INTO settings_vault.plugin_storage_namespaces(plugin_id,namespace,generation,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(plugin_id,namespace) DO UPDATE SET generation=excluded.generation,revision=excluded.revision",params![owner,namespace,current_generation,next_blob]))?;
            for mutation in payload["mutations"].as_array().ok_or("invalid_request")? {
                let key = text(mutation, "key")?;
                if mutation["op"] == "put" {
                    let value = mutation["value"].to_string();
                    sql(conn.execute("INSERT INTO settings_vault.plugin_storage_records(plugin_id,namespace,key,value,encoded_bytes) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(plugin_id,namespace,key) DO UPDATE SET value=excluded.value,encoded_bytes=excluded.encoded_bytes",params![owner,namespace,key,value,(key.len()+value.len()) as i64]))?;
                } else {
                    sql(conn.execute("DELETE FROM settings_vault.plugin_storage_records WHERE plugin_id=?1 AND namespace=?2 AND key=?3",params![owner,namespace,key]))?;
                }
            }
            // Logical quota = UTF-8 key bytes + compact JSON value bytes, plus each namespace once
            let (keys,bytes):(i64,i64)=sql(conn.query_row("SELECT count(*),coalesce(sum(encoded_bytes),0) FROM settings_vault.plugin_storage_records WHERE plugin_id=?1",[owner],|r|Ok((r.get(0)?,r.get(1)?))))?;
            let namespace_bytes:i64=sql(conn.query_row("SELECT coalesce(sum(length(CAST(namespace AS BLOB))),0) FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1",[owner],|r|r.get(0)))?;
            if keys > 4096 || bytes + namespace_bytes > LOGICAL_LIMIT {
                return Err("quota".into());
            }
            sql(conn.execute(
                "DELETE FROM settings_vault.plugin_storage_receipts WHERE plugin_id=?1 AND created_ms<?2",
                params![owner, now() - RECEIPT_AGE],
            ))?;
            stored_data::mark_updated(conn, owner)?;
            // A durable receipt implies the commit succeeded; no result payload is stored
            sql(conn.execute("INSERT OR REPLACE INTO settings_vault.plugin_storage_receipts(plugin_id,namespace,operation_id,digest,generation,revision,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![owner,namespace,operation,digest,current_generation,next_blob,now()]))?;
            // Every receipt's bounded metadata fits 256 bytes; 1,024 entries fit 256 KiB
            sql(conn.execute("DELETE FROM settings_vault.plugin_storage_receipts WHERE rowid IN (SELECT rowid FROM settings_vault.plugin_storage_receipts WHERE plugin_id=?1 ORDER BY created_ms DESC,rowid DESC LIMIT -1 OFFSET 1024)",[owner]))?;
            Ok(
                json!({"generation":current_generation,"revision":next.to_string(),"operation_id":operation,"committed":true}),
            )
        }
        _ => Err("unsupported".into()),
    }
}

// Remove the selected stored data
pub(super) fn purge(conn: &Connection, owner: &str) -> Result<(), String> {
    let counter = read_purge_generation(conn, owner)?
        .checked_add(1)
        .ok_or("Storage generation exhausted")?;
    sql(conn.execute(
        "INSERT INTO settings_vault.plugin_storage_generations(plugin_id,generation) VALUES(?1,?2)
         ON CONFLICT(plugin_id) DO UPDATE SET generation=excluded.generation",
        params![owner, counter],
    ))?;
    sql(conn.execute(
        "DELETE FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1",
        [owner],
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Process the request through this subsystem
    fn request(method: &str, payload: Value) -> Value {
        json!({"event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,"request_id":uuid::Uuid::new_v4().to_string(),"method":method,"payload":payload})
    }
    // Dispatch a plugin storage RPC call
    fn call(storage: &Storage, owner: &str, method: &str, payload: Value) -> Value {
        let message = request(method, payload);
        let bytes = message.to_string().len();
        dispatch(
            storage.clone(),
            Arc::new(Admission::default()),
            Arc::new(AtomicBool::new(true)),
            owner.into(),
            message,
            false,
            bytes,
        )
    }
    // Read for the current operation
    fn read(storage: &Storage, owner: &str, namespace: &str, key: &str) -> Value {
        call(
            storage,
            owner,
            "get",
            json!({"namespace":namespace,"key":key}),
        )["result"]
            .clone()
    }
    // Commit payload
    fn commit_payload(state: &Value, namespace: &str, mutations: Value) -> Value {
        json!({"namespace":namespace,"generation":state["generation"],"expected_revision":state["revision"],"operation_id":uuid::Uuid::new_v4().to_string(),"mutations":mutations})
    }
    #[test]
    // Verify absent reads do not write and null is distinct from missing
    fn absent_reads_do_not_write_and_null_is_distinct_from_missing() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let missing = read(&storage, "owner", "private", "a");
        assert_eq!(missing["found"], false);
        assert_eq!(missing["revision"], "0");
        let rows = storage
            .run(|c| {
                c.query_row(
                    "SELECT count(*) FROM settings_vault.plugin_storage_namespaces",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(rows, 0);
        let payload = commit_payload(
            &missing,
            "private",
            json!([{"op":"put","key":"a","value":null},{"op":"put","key":"unknown","value":{"future":{"preserve":true}}}]),
        );
        let committed = call(&storage, "owner", "commit", payload);
        assert_eq!(committed["result"]["committed"], true, "{committed}");
        assert_eq!(read(&storage, "owner", "private", "a")["found"], true);
        assert!(read(&storage, "owner", "private", "a")["value"].is_null());
        assert_eq!(read(&storage, "other", "private", "a")["found"], false);
        assert_eq!(
            read(&storage, "owner", "private", "unknown")["value"]["future"]["preserve"],
            true
        );
    }
    #[test]
    // Verify dedup precedes cas survives restart and purge fences old handles
    fn dedup_precedes_cas_survives_restart_and_purge_fences_old_handles() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let initial = read(&storage, "owner", "n", "k");
        let payload = commit_payload(&initial, "n", json!([{"op":"put","key":"k","value":42}]));
        let first = call(&storage, "owner", "commit", payload.clone());
        assert_eq!(first["result"]["revision"], "1");
        assert_eq!(
            call(&storage, "owner", "commit", payload.clone())["result"],
            first["result"]
        );
        let mut changed = payload.clone();
        changed["mutations"][0]["value"] = json!(43);
        assert_eq!(
            call(&storage, "owner", "commit", changed)["error"]["code"],
            "conflict"
        );
        drop(storage);
        let storage = Storage::open(root.path().into()).unwrap();
        assert_eq!(
            call(&storage, "owner", "commit", payload.clone())["result"],
            first["result"]
        );
        let receipt = call(
            &storage,
            "owner",
            "receipt",
            json!({"namespace":"n","generation":initial["generation"],"operation_id":payload["operation_id"]}),
        );
        assert_eq!(receipt["result"]["committed"], true);
        storage.transaction(|tx| purge(tx, "owner")).unwrap();
        assert_eq!(
            call(&storage, "owner", "commit", payload)["error"]["code"],
            "conflict"
        );
        let current = read(&storage, "owner", "n", "k");
        assert_eq!(current["found"], false);
        assert_ne!(current["generation"], initial["generation"]);
    }
    #[test]
    // Verify simultaneous expected revision has one winner and failed receipt rolls back
    fn simultaneous_expected_revision_has_one_winner_and_failed_receipt_rolls_back() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let initial = read(&storage, "owner", "n", "k");
        let mut workers = vec![];
        for value in [1, 2] {
            let storage = storage.clone();
            let payload =
                commit_payload(&initial, "n", json!([{"op":"put","key":"k","value":value}]));
            workers.push(std::thread::spawn(move || {
                call(&storage, "owner", "commit", payload)
            }));
        }
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.get("result").is_some()).count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| r["error"]["code"] == "conflict")
                .count(),
            1
        );
        let before = read(&storage, "owner", "n", "k");
        // This timestamp lives in main, unlike the value and receipt in the encrypted file.
        storage
            .write_json(Domain::PluginMetadata, "storage-updated:owner", &1_u64, 128)
            .unwrap();
        storage.run(|c|c.execute_batch("CREATE TRIGGER settings_vault.reject_receipt BEFORE INSERT ON settings_vault.plugin_storage_receipts BEGIN SELECT RAISE(FAIL,'fixture'); END;").map_err(|e|e.to_string())).unwrap();
        let payload = commit_payload(
            &before,
            "n",
            json!([{"op":"put","key":"k","value":"must roll back"},{"op":"put","key":"new","value":true}]),
        );
        assert_eq!(
            call(&storage, "owner", "commit", payload)["error"]["code"],
            "storage_failure"
        );
        assert_eq!(read(&storage, "owner", "n", "k"), before);
        assert_eq!(read(&storage, "owner", "n", "new")["found"], false);
        assert_eq!(
            storage
                .read_json::<u64>(Domain::PluginMetadata, "storage-updated:owner", 128)
                .unwrap(),
            Some(1),
            "receipt failure must also roll back the Manager metadata update",
        );
    }
    #[test]
    // Verify commit stage failure never fabricates success and can be reconciled
    fn commit_stage_failure_never_fabricates_success_and_can_be_reconciled() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let before = read(&storage, "owner", "n", "trigger");
        storage.run(|connection|connection.execute_batch("CREATE TRIGGER settings_vault.fail_at_commit AFTER INSERT ON settings_vault.plugin_storage_records WHEN new.key='trigger' BEGIN INSERT INTO plugin_storage_records(plugin_id,namespace,key,value,encoded_bytes) VALUES('owner','missing-parent','x','null',5); END; PRAGMA defer_foreign_keys=ON;").map_err(|error|error.to_string())).unwrap();
        let payload = commit_payload(
            &before,
            "n",
            json!([{"op":"put","key":"trigger","value":true}]),
        );
        let failed = call(&storage, "owner", "commit", payload.clone());
        assert_eq!(failed["error"]["code"], "indeterminate");
        assert_eq!(read(&storage, "owner", "n", "trigger"), before);
        let receipt = call(
            &storage,
            "owner",
            "receipt",
            json!({"namespace":"n","generation":before["generation"],"operation_id":payload["operation_id"]}),
        );
        assert_eq!(receipt["result"]["found"], false);
    }
    #[test]
    // Verify cursor is owner revision bound and keys are exact utf8
    fn cursor_is_owner_revision_bound_and_keys_are_exact_utf8() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut state = read(&storage, "owner", "n", "k");
        for offset in (0..160).step_by(32) {
            let mutations: Vec<_> = (offset..offset + 32)
                .map(|n| json!({"op":"put","key":format!("key-{n:03}"),"value":n}))
                .collect();
            let result = call(
                &storage,
                "owner",
                "commit",
                commit_payload(&state, "n", json!(mutations)),
            );
            state = result["result"].clone();
        }
        let page = call(&storage, "owner", "listKeys", json!({"namespace":"n"}));
        assert_eq!(page["result"]["keys"].as_array().unwrap().len(), 128);
        let cursor = page["result"]["cursor"].clone();
        let last = call(
            &storage,
            "owner",
            "listKeys",
            json!({"namespace":"n","cursor":cursor}),
        );
        assert_eq!(last["result"]["keys"].as_array().unwrap().len(), 32);
        assert_eq!(
            call(
                &storage,
                "other",
                "listKeys",
                json!({"namespace":"n","cursor":cursor})
            )["error"]["code"],
            "conflict"
        );
        let payload = commit_payload(
            &state,
            "n",
            json!([{"op":"put","key":"é","value":1},{"op":"put","key":"e\u{301}","value":2}]),
        );
        assert!(call(&storage, "owner", "commit", payload)
            .get("result")
            .is_some());
        assert_eq!(
            call(
                &storage,
                "owner",
                "listKeys",
                json!({"namespace":"n","cursor":cursor})
            )["error"]["code"],
            "conflict"
        );
        assert_eq!(read(&storage, "owner", "n", "é")["value"], 1);
        assert_eq!(read(&storage, "owner", "n", "e\u{301}")["value"], 2);
    }
    #[test]
    // Verify inspector unknown handler revoked session and bounds are rejected before writes
    fn inspector_unknown_handler_revoked_session_and_bounds_are_rejected_before_writes() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let message = request("get", json!({"namespace":"n","key":"k"}));
        let admission = Arc::new(Admission::default());
        let denied = dispatch(
            storage.clone(),
            admission.clone(),
            Arc::new(AtomicBool::new(true)),
            "owner".into(),
            message.clone(),
            true,
            100,
        );
        assert_eq!(denied["error"]["code"], "unauthorized");
        let cancelled = dispatch(
            storage.clone(),
            admission.clone(),
            Arc::new(AtomicBool::new(false)),
            "owner".into(),
            message.clone(),
            false,
            100,
        );
        assert_eq!(cancelled["error"]["code"], "cancelled_before_commit");
        let mut unsupported = message;
        unsupported["protocol_id"] = json!("future-v2");
        assert_eq!(
            dispatch(
                storage.clone(),
                admission,
                Arc::new(AtomicBool::new(true)),
                "owner".into(),
                unsupported,
                false,
                100
            )["error"]["code"],
            "unsupported"
        );
        let state = read(&storage, "owner", "n", "k");
        let payload = commit_payload(
            &state,
            "n",
            json!([{"op":"put","key":"k","value":"x".repeat(MAX_VALUE)}]),
        );
        assert_eq!(
            call(&storage, "owner", "commit", payload)["error"]["code"],
            "quota"
        );
        assert_eq!(
            call(
                &storage,
                "owner",
                "get",
                json!({"namespace":"n","key":"é".repeat(129)})
            )["error"]["code"],
            "invalid_request"
        );
        assert_eq!(read(&storage, "owner", "n", "k"), state);
        let admission = Arc::new(Admission::default());
        let mut permits = vec![];
        for _ in 0..8 {
            permits.push(admission.admit("owner", 1).unwrap());
        }
        assert!(admission.admit("owner", 1).is_err());
        drop(permits);
        assert!(admission.admit("owner", 1).is_ok());
    }
}

// Validate snapshot
pub(super) fn validate_snapshot(conn: &Connection) -> Result<(), String> {
    // Portable redacted Manager snapshots have no encrypted attachment to validate.
    if !stream_dock_manager_storage::plugin_storage_attached(conn)? {
        return Ok(());
    }
    read_private_epoch(conn)?;
    let mut statement = sql(conn.prepare(
        "SELECT plugin_id,generation FROM settings_vault.plugin_storage_generations LIMIT 16385",
    ))?;
    let counters = sql(statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    }))?;
    for (index, row) in counters.enumerate() {
        let (owner, counter) = sql(row)?;
        if index >= 16384 || !safe_id(&owner) || counter <= 0 {
            return Err("Invalid durable plugin generation".into());
        }
    }
    let malformed: i64 = sql(conn.query_row("SELECT (SELECT count(*) FROM settings_vault.plugin_storage_namespaces WHERE length(CAST(plugin_id AS BLOB)) NOT BETWEEN 1 AND 159 OR length(CAST(namespace AS BLOB)) NOT BETWEEN 1 AND 128 OR length(generation)>39 OR length(revision)!=8) + (SELECT count(*) FROM settings_vault.plugin_storage_receipts WHERE length(operation_id)!=36 OR length(digest)!=64 OR length(generation)>39 OR length(revision)!=8)", [], |row|row.get(0)))?;
    if malformed != 0 {
        return Err("Private storage metadata exceeds bounds".into());
    }
    let mut owners = HashSet::new();
    let mut statement = sql(conn
        .prepare("SELECT plugin_id,namespace,generation,revision FROM settings_vault.plugin_storage_namespaces"))?;
    let rows = sql(statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, [u8; 8]>(3)?,
        ))
    }))?;
    let mut namespaces: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let (owner, namespace, generation, revision) = sql(row)?;
        if !safe_id(&owner)
            || !name(&namespace, 128)
            || !decimal(&generation)
            || u64::from_be_bytes(revision) == 0
        {
            return Err("Invalid private storage namespace".into());
        }
        owners.insert(owner.clone());
        *namespaces.entry(owner).or_default() += 1;
        if owners.len() > 4096 || namespaces.values().any(|n| *n > 64) {
            return Err("Private storage namespace quota exceeded".into());
        }
    }
    let bad:i64=sql(conn.query_row("SELECT count(*) FROM settings_vault.plugin_storage_records WHERE length(CAST(key AS BLOB)) NOT BETWEEN 1 AND 256 OR length(CAST(value AS BLOB))>262144 OR instr(key,char(0))>0 OR encoded_bytes != length(CAST(key AS BLOB))+length(CAST(value AS BLOB))",[],|r|r.get(0)))?;
    if bad != 0 {
        return Err("Private storage record bounds or accounting are invalid".into());
    }
    for owner in owners {
        let (keys,bytes):(i64,i64)=sql(conn.query_row("SELECT count(*),coalesce(sum(encoded_bytes),0) FROM settings_vault.plugin_storage_records WHERE plugin_id=?1",[&owner],|r|Ok((r.get(0)?,r.get(1)?))))?;
        let namespace_bytes:i64=sql(conn.query_row("SELECT coalesce(sum(length(CAST(namespace AS BLOB))),0) FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1",[&owner],|r|r.get(0)))?;
        let receipts: i64 = sql(conn.query_row(
            "SELECT count(*) FROM settings_vault.plugin_storage_receipts WHERE plugin_id=?1",
            [&owner],
            |r| r.get(0),
        ))?;
        if keys > 4096 || bytes + namespace_bytes > LOGICAL_LIMIT || receipts > 1024 {
            return Err("Private storage quota exceeded".into());
        }
    }
    let mut statement=sql(conn.prepare("SELECT operation_id,digest,generation,revision,created_ms FROM settings_vault.plugin_storage_receipts"))?;
    let rows = sql(statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, [u8; 8]>(3)?,
            r.get::<_, i64>(4)?,
        ))
    }))?;
    for row in rows {
        let (id, digest, generation, revision, time) = sql(row)?;
        if !uuid(&id)
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !decimal(&generation)
            || u64::from_be_bytes(revision) == 0
            || time < 0
        {
            return Err("Invalid private storage receipt".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod bound_tests {
    use super::*;
    // Dispatch a plugin storage RPC call
    fn call(storage: &Storage, method: &str, payload: Value) -> Value {
        let message = json!({"event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,"request_id":uuid::Uuid::new_v4().to_string(),"method":method,"payload":payload});
        let size = message.to_string().len();
        dispatch(
            storage.clone(),
            Arc::new(Admission::default()),
            Arc::new(AtomicBool::new(true)),
            "fixture".into(),
            message,
            false,
            size,
        )
    }
    // Write the value into its destination
    fn put(storage: &Storage, state: &Value, key: &str, value: Value) -> Value {
        call(
            storage,
            "commit",
            json!({"namespace":"n","generation":state["generation"],"expected_revision":state["revision"],"operation_id":uuid::Uuid::new_v4().to_string(),"mutations":[{"op":"put","key":key,"value":value}]}),
        )
    }
    #[test]
    // Verify exact value limit and key quota are checked on atomic candidate
    fn exact_value_limit_and_key_quota_are_checked_on_atomic_candidate() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let state = call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"].clone();
        let committed =
            put(&storage, &state, "k", json!("x".repeat(MAX_VALUE - 2)))["result"].clone();
        assert_eq!(committed["revision"], "1");
        assert_eq!(
            put(&storage, &committed, "k", json!("x".repeat(MAX_VALUE - 1)))["error"]["code"],
            "quota"
        );
        storage.transaction(|tx| {for number in 0..4095 {let key=format!("key-{number}");sql(tx.execute("INSERT INTO settings_vault.plugin_storage_records(plugin_id,namespace,key,value,encoded_bytes) VALUES('fixture','n',?1,'0',?2)",params![key,key.len() as i64+1]))?;} Ok(())}).unwrap();
        assert_eq!(
            put(&storage, &committed, "new", json!(0))["error"]["code"],
            "quota"
        );
        let result = call(
            &storage,
            "commit",
            json!({"namespace":"n","generation":committed["generation"],"expected_revision":"1","operation_id":uuid::Uuid::new_v4().to_string(),"mutations":[{"op":"delete","key":"key-0"},{"op":"put","key":"new","value":0}]}),
        );
        assert_eq!(result["result"]["revision"], "2");
        storage.run(|conn| validate_snapshot(conn)).unwrap();
    }
    #[test]
    // Verify typed database revisions retain the full unsigned range on the wire
    fn typed_revision_preserves_unsigned_wire_range_and_receipts() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(|conn| {
                sql(conn.execute(
                "INSERT INTO settings_vault.plugin_storage_namespaces VALUES('fixture','n','0',?1)",
                [(u64::MAX - 1).to_be_bytes()],
            ))?;
                Ok(())
            })
            .unwrap();
        let before = call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"].clone();
        assert_eq!(before["revision"], (u64::MAX - 1).to_string());
        let last = put(&storage, &before, "k", json!(1))["result"].clone();
        assert_eq!(last["revision"], u64::MAX.to_string());
        let receipt = call(
            &storage,
            "receipt",
            json!({"namespace":"n","generation":last["generation"],"operation_id":last["operation_id"]}),
        );
        assert_eq!(receipt["result"]["revision"], u64::MAX.to_string());
        assert_eq!(receipt["result"]["committed"], true);
        assert_eq!(
            put(&storage, &last, "k", json!(2))["error"]["code"],
            "quota"
        );
        assert_eq!(
            call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"]["value"],
            1
        );
        storage.run(|conn| validate_snapshot(conn)).unwrap();
    }
    #[test]
    // Verify restore epoch invalidates handles and old receipts without erasing values
    fn restore_epoch_invalidates_handles_and_old_receipts_without_erasing_values() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let before = call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"].clone();
        let result = put(&storage, &before, "k", json!("keep"))["result"].clone();
        storage
            .run(|conn| {
                sql(conn.execute(
                    "UPDATE database_metadata SET value=?1 WHERE key='storage_generation'",
                    [uuid::Uuid::new_v4().to_string()],
                ))
                .map(|_| ())
            })
            .unwrap();
        let after = call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"].clone();
        assert_eq!(after["value"], "keep");
        assert_ne!(after["generation"], before["generation"]);
        assert_eq!(after["revision"], "1");
        assert_eq!(
            put(&storage, &result, "k", json!("stale"))["error"]["code"],
            "conflict"
        );
        let receipt = call(
            &storage,
            "receipt",
            json!({"namespace":"n","generation":after["generation"],"operation_id":result["operation_id"]}),
        );
        assert_eq!(receipt["result"]["found"], false);
        let second = put(&storage, &after, "k", json!("new epoch"))["result"].clone();
        assert_eq!(second["revision"], "2");
        // A datastore-only restore keeps the Manager epoch and can restore the same revision.
        storage
            .run(|conn| {
                sql(conn.execute(
                    "UPDATE settings_vault.plugin_storage_epoch SET epoch=?1 WHERE singleton=1",
                    [uuid::Uuid::new_v4().to_string()],
                ))?;
                Ok(())
            })
            .unwrap();
        let final_state =
            call(&storage, "get", json!({"namespace":"n","key":"k"}))["result"].clone();
        assert_eq!(final_state["value"], "new epoch");
        assert_eq!(final_state["revision"], second["revision"]);
        assert_ne!(final_state["generation"], second["generation"]);
        assert_eq!(
            put(&storage, &second, "k", json!("stale"))["error"]["code"],
            "conflict"
        );
        let receipt = call(
            &storage,
            "receipt",
            json!({"namespace":"n","generation":final_state["generation"],"operation_id":second["operation_id"]}),
        );
        assert_eq!(receipt["result"]["found"], false);
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    // Receive for the current operation
    fn receive(
        socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    ) -> Value {
        loop {
            let frame = socket.read().unwrap();
            if let tungstenite::Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    #[test]
    // Verify registered vendor socket keeps base calls but cannot use private RPC
    fn registered_vendor_socket_keeps_base_calls_but_cannot_use_private_rpc() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::socket_host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.sessions.tokens.insert("token".into(), "fixture".into());
            hub.context_state
                .contexts
                .insert("opaque".into(), "fixture".into());
        }
        let (mut socket, _) =
            tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
        }
        socket
            .send(tungstenite::Message::text(
                json!({"event":"registerPlugin","uuid":"token"}).to_string(),
            ))
            .unwrap();
        assert_eq!(receive(&mut socket)["event"], "didReceiveGlobalSettings");
        // Runtime writes do not echo unsolicited settings events to their sender
        // A tagged getter is a socket-order barrier and verifies the persisted value
        for message in [
            json!({"event":"setGlobalSettings","payload":{"unknown":{"keep":true}}}),
            json!({"event":"getGlobalSettings","id":"globals-barrier"}),
        ] {
            socket
                .send(tungstenite::Message::text(message.to_string()))
                .unwrap();
        }
        let response = receive(&mut socket);
        assert_eq!(response["id"], "globals-barrier");
        assert_eq!(response["event"], "didReceiveGlobalSettings");
        assert_eq!(response["payload"]["settings"]["unknown"]["keep"], true);
        for message in [
            json!({"event":"setSettings","context":"opaque","payload":{"value":null,"unknown":42}}),
            json!({"event":"getSettings","context":"opaque","id":"action-barrier"}),
        ] {
            socket
                .send(tungstenite::Message::text(message.to_string()))
                .unwrap();
        }
        let response = receive(&mut socket);
        assert_eq!(response["id"], "action-barrier");
        assert_eq!(response["event"], "didReceiveSettings");
        assert_eq!(response["context"], "opaque");
        assert_eq!(response["payload"]["settings"]["unknown"], 42);
        let request_id = uuid::Uuid::new_v4().to_string();
        socket.send(tungstenite::Message::text(json!({"event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,"request_id":request_id,"method":"get","payload":{"namespace":"private","key":"missing"}}).to_string())).unwrap();
        socket
            .send(tungstenite::Message::text(
                json!({"event":"getGlobalSettings"}).to_string(),
            ))
            .unwrap();
        let response = receive(&mut socket);
        assert_eq!(response["event"], "didReceiveGlobalSettings");
        assert!(host.hub.lock().unwrap().health["fixture"]
            .recent_failures
            .iter()
            .any(|failure| failure.contains("not negotiated")));
        assert!(!host
            .storage()
            .run(|connection| connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='plugin_contexts')",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .map_err(|e| e.to_string()))
            .unwrap());
        let globals = host
            .storage()
            .read_json::<Value>(Domain::PluginGlobals, "fixture", 1024)
            .unwrap()
            .unwrap();
        assert_eq!(globals["unknown"]["keep"], true);
        socket.close(None).unwrap();
    }
}
