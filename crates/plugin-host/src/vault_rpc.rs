//! Exact installed-package declarations and authenticated direct Vault replies
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub const VALUES: &str = "plugin-vault-values-v1";
pub const SECRETS: &str = "plugin-vault-secret-sharing-v1";

#[derive(
    Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct VaultSetupSlot {
    pub id: String,
    pub kind: String,
    pub entry_kind: String,
    pub mode: String,
    pub required: bool,
    pub purpose: String,
    pub field: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recipient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub precision: Option<String>,
}

impl VaultSetupSlot {
    pub fn is_direct_secret(&self) -> bool {
        self.kind == "secret" && self.mode == "direct"
    }
    pub fn requires_direct_approval(&self) -> bool {
        self.mode == "direct" && matches!(self.kind.as_str(), "secret" | "location")
    }
    pub fn broker_scope(&self) -> Option<permissions::PermissionScope> {
        if self.mode != "broker" {
            return None;
        }
        let recipe = self.recipe.as_deref()?;
        let operation = self.operation.as_deref()?;
        let origin = self.origin.as_deref()?;
        let valid = match (
            self.kind.as_str(),
            self.entry_kind.as_str(),
            recipe,
            operation,
            origin,
        ) {
            (
                "secret",
                "api-key" | "bearer-token",
                "clipboard-summary",
                "authorize-request",
                "https://api.example.test",
            ) => self.fields.is_none() && self.precision.is_none() && self.recipient.is_none(),
            _ => false,
        };
        valid.then(|| permissions::PermissionScope::Access {
            mode: "use-entry".into(),
            field: self.field.clone(),
            operation: operation.into(),
            origin: origin.into(),
            recipe: recipe.into(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    version: u8,
    slots: Vec<VaultSetupSlot>,
}

use super::permissions::identifier;

// Parse canonical typed slots without honoring caller-created declarations
pub fn setup_slots(manifest: &Value) -> Result<Vec<VaultSetupSlot>, String> {
    let Some(value) = manifest["StreamDockManager"].get("SetupSlots") else {
        return Ok(vec![]);
    };
    let declaration: Declaration =
        serde_json::from_value(value.clone()).map_err(|_| "Invalid Vault setup declaration")?;
    if declaration.version != 1 || declaration.slots.is_empty() || declaration.slots.len() > 64 {
        return Err("Unsupported Vault setup declaration".into());
    }
    let actions: BTreeSet<_> = manifest["Actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|action| action["UUID"].as_str())
        .collect();
    let mut seen = BTreeSet::new();
    let mut shapes = BTreeMap::new();
    let mut fields = BTreeSet::new();
    for slot in &declaration.slots {
        let direct = slot.mode == "direct"
            && slot.origin.is_none()
            && slot.recipe.is_none()
            && slot.operation.is_none()
            && slot.fields.is_none()
            && slot.recipient.is_none()
            && slot.precision.is_none();
        let known_type = slot.broker_scope().is_some()
            || direct
                && match slot.kind.as_str() {
                    "value" => slot.entry_kind == "text",
                    "location" => slot.entry_kind == "location",
                    "secret" => matches!(
                        slot.entry_kind.as_str(),
                        "api-key" | "bearer-token" | "credential"
                    ),
                    _ => false,
                };
        let shape = (
            slot.kind.as_str(),
            slot.entry_kind.as_str(),
            slot.field.as_str(),
            slot.required,
            &slot.mode,
            &slot.origin,
            &slot.recipe,
            &slot.operation,
            &slot.fields,
            &slot.recipient,
            &slot.precision,
        );
        if !known_type
            || !identifier(&slot.id, 128)
            || !identifier(&slot.field, 128)
            || !identifier(&slot.action, 256)
            || !actions.contains(slot.action.as_str())
            || slot.purpose.trim().is_empty()
            || slot.purpose.len() > 512
            || slot.purpose.chars().any(char::is_control)
            || !seen.insert((slot.id.as_str(), slot.action.as_str()))
            || !fields.insert((slot.field.as_str(), slot.action.as_str()))
            || shapes
                .insert(slot.id.as_str(), shape)
                .is_some_and(|old| old != shape)
        {
            return Err("Invalid or conflicting Vault setup slot".into());
        }
    }
    let mut slots = declaration.slots;
    slots.sort_by(|a, b| (&a.id, &a.action).cmp(&(&b.id, &b.action)));
    super::permissions::validate_slots(
        &slots,
        super::permissions::declarations(manifest)?.as_ref(),
    )?;
    Ok(slots)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultInstallation {
    pub owner: String,
    pub token: String,
    pub inventory_sha256: String,
    pub storage_generation: String,
}

impl VaultInstallation {
    pub fn permission_owner(&self) -> stream_dock_manager_storage::permissions::PermissionOwner {
        stream_dock_manager_storage::permissions::PermissionOwner {
            plugin: self.owner.clone(),
            installation: self.token.clone(),
            inventory: self.inventory_sha256.clone(),
            generation: self.storage_generation.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct SessionScope {
    pub(super) writers: Vec<permissions::ActionScope>,
    pub(super) installation: VaultInstallation,
    pub(super) slots: Vec<VaultSetupSlot>,
}

pub trait VaultRead: Send + Sync {
    fn writer_ready(
        &self,
        _owner: &VaultInstallation,
        _action: &str,
        _scope: &permissions::PermissionScope,
    ) -> bool {
        false
    }
    fn write_private(
        &self,
        _owner: &VaultInstallation,
        _scope: &permissions::ActionScope,
        _request: &vault_writes::SecretWrite,
        _alive: &AtomicBool,
    ) -> Result<(String, String), &'static str> {
        Err("operation_unavailable")
    }

    fn required_ready(
        &self,
        _installation: &VaultInstallation,
        _slots: &[&VaultSetupSlot],
    ) -> bool {
        false
    }

    /// Format only against this live connection's declared, direct nonsecret scopes.
    fn format_text_and_publish(
        &self,
        _installation: &VaultInstallation,
        _slots: &[&VaultSetupSlot],
        _text: &str,
        _publish: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str> {
        Err("operation_unavailable")
    }

    // Keep admission and publication serialized against lock, revoke, and entry changes
    fn read_and_publish(
        &self,
        installation: &VaultInstallation,
        slot: &VaultSetupSlot,
        publish: &mut dyn FnMut(&Value, &str) -> bool,
    ) -> Result<(), &'static str>;
}

impl PluginHost {
    // Evaluate the currently running installation without holding the socket hub during Vault I/O.
    pub(super) fn check_action_permissions(&self, id: &str, action: &str) -> Result<(), String> {
        let plugin = self.plugins.get(id).ok_or("Plugin not installed")?;
        let group = plugin
            .info
            .permissions
            .as_ref()
            .and_then(|d| d.actions.get(action));
        let public_slots = &plugin.manifest["StreamDockManager"]["SetupSlots"]["slots"];
        if group.is_none_or(|p| p.required.is_empty())
            && !public_slots.as_array().is_some_and(|slots| {
                slots
                    .iter()
                    .any(|s| s["required"] == true && s["action"] == action)
            })
        {
            return Ok(());
        }
        let slots = setup_slots(&plugin.manifest)?;
        let required: Vec<_> = slots
            .iter()
            .filter(|s| s.required && s.action == action)
            .collect();
        let writers: Vec<_> = group
            .into_iter()
            .flat_map(|g| &g.required)
            .filter(|s| matches!(s, permissions::PermissionScope::WriteSecret { .. }))
            .collect();
        if !writers.is_empty() {
            let (reader, owner) = {
                let h = self.hub.lock().unwrap();
                (
                    h.vault_reader.clone(),
                    h.vault_sessions
                        .get(&plugin.token)
                        .map(|s| s.installation.clone()),
                )
            };
            let owner = owner.or_else(|| self.verified_vault_installation(id));
            if !reader.zip(owner).is_some_and(|(reader, owner)| {
                writers
                    .iter()
                    .all(|s| reader.writer_ready(&owner, action, s))
            }) {
                return Err("Review this action's private Vault writes in Manager".into());
            }
        }
        let ordinary: Vec<_> = group
            .into_iter()
            .flat_map(|g| &g.required)
            .filter(|scope| {
                !matches!(scope, permissions::PermissionScope::WriteSecret { .. })
                    && !required.iter().any(|slot| scope.matches_direct_slot(slot))
            })
            .collect();
        if !ordinary.is_empty() && !self.ordinary_required_ready(id, action, &ordinary) {
            return Err("A required Manager permission is unavailable for this action".into());
        }
        if required.is_empty() {
            return Ok(());
        }
        let (reader, active) = {
            let hub = self.hub.lock().unwrap();
            (
                hub.vault_reader.clone(),
                hub.vault_sessions
                    .get(&plugin.token)
                    .map(|s| s.installation.clone()),
            )
        };
        let installation = active.or_else(|| self.verified_vault_installation(id));
        if reader
            .zip(installation)
            .is_some_and(|(reader, owner)| reader.required_ready(&owner, &required))
        {
            Ok(())
        } else {
            Err("Review this action's required Vault access in Manager".into())
        }
    }

    // Attach the runtime resolver without granting any entry to a package
    pub fn set_vault_reader(&mut self, reader: Arc<dyn VaultRead>) {
        self.hub.lock().unwrap().vault_reader = Some(reader);
    }

    // Return the current installed identity used to invalidate staged approvals
    pub fn current_vault_installation_token(&self, id: &str) -> Option<String> {
        self.hub.lock().unwrap().installations.get(id).cloned()
    }

    // Validate the actual staged inventory before issuing an encrypted grant
    pub fn prepared_vault_installation(
        &self,
        prepared: &PreparedPlugin,
    ) -> Result<VaultInstallation, String> {
        Self::verify_prepared_files(prepared, &prepared.inventory)?;
        Ok(VaultInstallation {
            owner: prepared.id.clone(),
            token: uuid::Uuid::new_v4().to_string(),
            inventory_sha256: hash_inventory(&prepared.inventory)?,
            storage_generation: vault_generation(self.storage())?,
        })
    }
}

// Use the durable restore epoch rather than a process-local counter
fn vault_generation(store: &stream_dock_manager_storage::Storage) -> Result<String, String> {
    store
        .persisted_generation()
        .map_err(|_| "Vault installation generation unavailable".into())
}

impl PluginHost {
    // Bind a native launch to actual installed files and the authoritative install token
    pub(super) fn vault_launch_scope(&self, id: &str) -> Option<SessionScope> {
        let plugin = self.plugins.get(id)?;
        let slots = setup_slots(&plugin.manifest).ok()?;
        let installation = self.verified_vault_installation(id)?;
        let writers = vault_writes::declarations(plugin.info.permissions.as_ref());
        if (slots.is_empty() && writers.is_empty()) || plugin.info.kind == "browser" {
            return None;
        }
        Some(SessionScope {
            writers,
            installation,
            slots,
        })
    }

    // Retain the negotiated managed mode even if the owner grant is locked or revoked
    pub fn vault_managed(&self, id: &str) -> bool {
        self.hub
            .lock()
            .unwrap()
            .provenance
            .get(id)
            .is_some_and(|p| p["vault_mode"] == "managed")
    }

    // A Settings review requires the byte inventory captured by the owning installer.
    pub fn installed_setup_slot(
        &self,
        id: &str,
        action: &str,
        field: &str,
    ) -> Result<(VaultInstallation, VaultSetupSlot), String> {
        let plugin = self.plugins.get(id).ok_or("plugin_unavailable")?;
        let slot = setup_slots(&plugin.manifest)?
            .into_iter()
            .find(|s| s.action == action && s.field == field && s.requires_direct_approval())
            .ok_or("not_declared")?;
        let identity = self
            .installation_inventory(id)
            .ok_or("operation_unavailable")?;
        if self
            .provenance(id)
            .and_then(|p| p["vault_reviewed_inventory"].as_str().map(str::to_owned))
            .as_deref()
            != Some(&identity.inventory_sha256)
        {
            return Err("Package contents need a new install review".into());
        }
        Ok((identity, slot))
    }

    pub fn select_managed_vault(&mut self, id: &str) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let previous = hub
            .provenance
            .get(id)
            .cloned()
            .ok_or("Package review unavailable")?;
        if previous["vault_mode"] == "managed" {
            return Ok(());
        }
        let mut current = previous.clone();
        current["vault_mode"] = json!("managed");
        hub.provenance.insert(id.into(), current);
        if let Err(error) = hub.persist() {
            hub.provenance.insert(id.into(), previous);
            return Err(error);
        }
        Ok(())
    }

    // Confirm the current install token and byte inventory against the encrypted grant
    pub fn verified_vault_installation(&self, id: &str) -> Option<VaultInstallation> {
        self.vault_managed(id).then_some(())?;
        self.installation_inventory(id)
    }

    pub fn reviewed_vault_owner(&self, id: &str) -> Option<VaultInstallation> {
        let installation = self.installation_inventory(id)?;
        let source = self.provenance(id)?;
        (source["vault_mode"] == "managed"
            || source["permission_reviewed_inventory"] == installation.inventory_sha256)
            .then_some(installation)
    }
    /// Exact managed owners for the trusted Vault management views, including disabled installs.
    pub fn verified_vault_installations(&self) -> Vec<VaultInstallation> {
        self.plugins
            .keys()
            .filter_map(|id| self.reviewed_vault_owner(id))
            .collect()
    }

    pub(super) fn installation_inventory(&self, id: &str) -> Option<VaultInstallation> {
        let plugin = self.plugins.get(id)?;
        if plugin.path != self.root.join(id) || plugin.info.kind == "browser" {
            return None;
        }
        let token = self.current_vault_installation_token(id)?;
        let inventory_sha256 = hash_inventory(&packages::inventory(&plugin.path).ok()?).ok()?;
        let storage_generation = vault_generation(self.storage()).ok()?;
        Some(VaultInstallation {
            owner: id.into(),
            token,
            inventory_sha256,
            storage_generation,
        })
    }

    // Remove unavailable feature advertisements from portable and vendor sessions
    pub(super) fn filter_vault_features(registration: &mut Value, managed: bool) {
        if !managed {
            if let Some(capabilities) = registration["capabilities"].as_array_mut() {
                capabilities.retain(|v| !matches!(v.as_str(), Some(VALUES | SECRETS)));
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    slot: String,
    field: String,
    action: String,
}

#[derive(Serialize)]
struct Reply<'a> {
    event: &'static str,
    protocol_id: &'a str,
    request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ReadResult<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct ReadResult<'a> {
    entry_kind: &'a str,
    value: &'a Value,
    revision: &'a str,
}

#[derive(Serialize)]
struct RpcError {
    code: &'static str,
    message: &'static str,
}

#[derive(Default)]
pub(super) struct ReadBudget {
    // Only UUIDs and authenticated owners are retained. Reconnect does not reset the rate limit.
    recent: VecDeque<(std::time::Instant, String, uuid::Uuid)>,
}

impl ReadBudget {
    pub(super) fn admit(&mut self, owner: &str, request: &Value) -> Result<(), &'static str> {
        let id = request["request_id"]
            .as_str()
            .and_then(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .filter(|id| id.to_string() == s)
            })
            .ok_or("invalid_request")?;
        let now = std::time::Instant::now();
        while self
            .recent
            .front()
            .is_some_and(|(time, _, _)| now.duration_since(*time) >= Duration::from_secs(30))
        {
            self.recent.pop_front();
        }
        let mut global = 0;
        let mut per_owner = 0;
        for (time, source, seen) in self.recent.iter().rev() {
            if source == owner && *seen == id {
                return Err("invalid_request");
            }
            if now.duration_since(*time) < Duration::from_secs(1) {
                global += 1;
                if source == owner {
                    per_owner += 1;
                }
            }
        }
        if global >= 128 || per_owner >= 32 || self.recent.len() >= 4096 {
            return Err("rate_limited");
        }
        self.recent.push_back((now, owner.to_owned(), id));
        Ok(())
    }
}

// Read and send one scoped reply only on its authenticated live native connection
pub(super) fn dispatch(
    reader: Option<&dyn VaultRead>,
    scope: Option<&SessionScope>,
    alive: Option<&AtomicBool>,
    message: &Value,
    length: usize,
    admitted: Result<(), &'static str>,
    mut send: impl FnMut(String) -> bool,
) -> bool {
    let protocol = message["protocol_id"].as_str().unwrap_or_default();
    let request_id = message["request_id"].as_str().unwrap_or_default();
    if !uuid::Uuid::parse_str(request_id).is_ok_and(|id| id.to_string() == request_id) {
        return false;
    }
    let mut sent = None;
    let result = (|| {
        if length > 4096
            || !message.as_object().is_some_and(|m| {
                m.len() == 5
                    && ["event", "protocol_id", "request_id", "method", "payload"]
                        .iter()
                        .all(|k| m.contains_key(*k))
            })
        {
            return Err("invalid_request");
        }
        let kind = match (protocol, message["method"].as_str()) {
            (VALUES, Some("read-location")) => "location",
            (VALUES, Some("read-value")) => "value",
            (SECRETS, Some("read-secret")) => "secret",
            _ => return Err("not_declared"),
        };
        admitted?;
        let scope = scope.ok_or("permission_denied")?;
        let alive = alive.ok_or("permission_denied")?;
        if !alive.load(Ordering::Acquire) {
            return Err("permission_denied");
        }
        let request: Request =
            serde_json::from_value(message["payload"].clone()).map_err(|_| "invalid_request")?;
        if !identifier(&request.slot, 128)
            || !identifier(&request.field, 128)
            || !identifier(&request.action, 256)
        {
            return Err("invalid_request");
        }
        let slot = scope
            .slots
            .iter()
            .find(|slot| {
                slot.id == request.slot
                    && slot.field == request.field
                    && slot.action == request.action
                    && slot.mode == "direct"
                    && slot.kind == kind
            })
            .ok_or("not_declared")?;
        reader.ok_or("operation_unavailable")?.read_and_publish(
            &scope.installation,
            slot,
            &mut |value, revision| {
                if alive.load(Ordering::Acquire) {
                    sent = Some(send(
                        serde_json::to_string(&Reply {
                            event: "streamDockManagerRpcResponse",
                            protocol_id: protocol,
                            request_id,
                            result: Some(ReadResult {
                                entry_kind: &slot.entry_kind,
                                value,
                                revision,
                            }),
                            error: None,
                        })
                        .expect("serializable Vault response"),
                    ));
                }
                sent.unwrap_or(false)
            },
        )?;
        Ok(())
    })();
    if let Some(sent) = sent {
        return sent;
    }
    let code = match result {
        Ok(()) => return false,
        Err(code) => code,
    };
    send(
        serde_json::to_string(&Reply {
            event: "streamDockManagerRpcResponse",
            protocol_id: protocol,
            request_id,
            result: None,
            error: Some(RpcError {
                code,
                message: "Vault request unavailable",
            }),
        })
        .expect("serializable Vault error"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vault_admission_bounds_duplicate_requests_across_connections() {
        let mut budget = ReadBudget::default();
        let request = || json!({"request_id":uuid::Uuid::new_v4().to_string()});
        let original = request();
        assert!(budget.admit("one", &original).is_ok());
        assert_eq!(budget.admit("one", &original), Err("invalid_request"));
        assert!(budget.admit("two", &original).is_ok());
        for _ in 1..32 {
            assert!(budget.admit("one", &request()).is_ok());
        }
        assert_eq!(budget.admit("one", &request()), Err("rate_limited"));
    }

    // Distinguish a scoped live native read from forbidden or mismatched requests
    #[test]
    fn exact_owner_scope_and_live_response() {
        struct Reader;
        impl VaultRead for Reader {
            fn read_and_publish(
                &self,
                install: &VaultInstallation,
                slot: &VaultSetupSlot,
                publish: &mut dyn FnMut(&Value, &str) -> bool,
            ) -> Result<(), &'static str> {
                assert_eq!(install.owner, "fixture");
                assert_eq!(slot.field, "credential");
                publish(&json!("synthetic-fixture"), "7");
                Ok(())
            }
        }
        let manifest = json!({
            "Actions": [{"UUID":"fixture.action"}],
            "StreamDockManager": {"Permissions": {"version":1,"actions":{"fixture.action":{"purpose":"fixture","required":[{"id":"vault.read-secret","field":"credential","entry_kind":"api-key","purpose":"fixture"}]}}},"SetupSlots": {"version":1,"slots":[
                {"id":"credential","field":"credential","action":"fixture.action","kind":"secret",
                    "entry_kind":"api-key","mode":"direct","purpose":"fixture","required":true}
            ]}}
        });
        let scope = SessionScope {
            writers: Vec::new(),
            installation: VaultInstallation {
                owner: "fixture".into(),
                token: "unique-install".into(),
                inventory_sha256: "digest".into(),
                storage_generation: "epoch".into(),
            },
            slots: setup_slots(&manifest).unwrap(),
        };
        let mut request = json!({"event":"streamDockManagerRpcRequest","protocol_id":SECRETS,"request_id":"12345678-1234-4234-8234-123456789abc","method":"read-secret",
            "payload":{"slot":"credential","field":"credential","action":"fixture.action"}});
        let alive = AtomicBool::new(true);
        let mut reply = Value::Null;
        let mut send = |wire: String| {
            reply = serde_json::from_str(&wire).unwrap();
            true
        };
        assert!(dispatch(
            Some(&Reader),
            None,
            Some(&alive),
            &request,
            256,
            Ok(()),
            &mut send
        ));
        assert_eq!(reply["error"]["code"], "permission_denied");
        request["payload"]["action"] = json!("foreign.action");
        assert!(dispatch(
            Some(&Reader),
            Some(&scope),
            Some(&alive),
            &request,
            256,
            Ok(()),
            |wire| {
                reply = serde_json::from_str(&wire).unwrap();
                true
            }
        ));
        assert_eq!(reply["error"]["code"], "not_declared");
        request["payload"]["action"] = json!("fixture.action");
        assert!(dispatch(
            Some(&Reader),
            Some(&scope),
            Some(&alive),
            &request,
            256,
            Ok(()),
            |wire| {
                reply = serde_json::from_str(&wire).unwrap();
                true
            }
        ));
        assert_eq!(
            reply["result"],
            json!({"entry_kind":"api-key","value":"synthetic-fixture","revision":"7"})
        );
    }
}
