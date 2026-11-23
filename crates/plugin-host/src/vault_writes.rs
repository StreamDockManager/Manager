//! Native connection ingress for a write-only private Vault slot.
use super::*;
use crate::permissions::{ActionScope, PermissionScope};
pub const PROTOCOL: &str = "plugin-vault-writes-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretWrite {
    pub operation: String,
    pub slot: String,
    pub field: String,
    pub entry_kind: String,
    pub action: Option<String>,
    pub expected_revision: Option<String>,
    pub value: String,
}
impl Drop for SecretWrite {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.value.zeroize();
    }
}
pub(super) fn declarations(d: Option<&permissions::PermissionDeclaration>) -> Vec<ActionScope> {
    let mut out = Vec::new();
    for (action, g) in d.into_iter().flat_map(|d| &d.actions) {
        for (required, scope) in g
            .required
            .iter()
            .map(|s| (true, s))
            .chain(g.optional.iter().map(|s| (false, s)))
        {
            if matches!(scope, PermissionScope::WriteSecret { .. }) {
                out.push(ActionScope {
                    action: action.clone(),
                    required,
                    scope: scope.clone(),
                });
            }
        }
    }
    out
}
pub(super) fn dispatch(
    reader: Option<&dyn vault_rpc::VaultRead>,
    session: Option<&vault_rpc::SessionScope>,
    lease: Option<&AtomicBool>,
    mut msg: Value,
    length: usize,
    admitted: Result<(), &'static str>,
    send: impl FnOnce(Value) -> bool,
) -> bool {
    let result = (|| {
        admitted?;
        if length > 96 * 1024
            || msg["method"] != "write-secret"
            || !msg.as_object().is_some_and(|o| {
                o.len() == 5
                    && ["event", "protocol_id", "request_id", "method", "payload"]
                        .iter()
                        .all(|k| o.contains_key(*k))
            })
        {
            return Err("invalid_request");
        }
        let session = session.ok_or("permission_denied")?;
        let lease = lease.ok_or("permission_denied")?;
        if !lease.load(Ordering::Acquire) {
            return Err("cancelled");
        }
        let req: SecretWrite =
            serde_json::from_value(msg["payload"].take()).map_err(|_| "invalid_request")?;
        if !permissions::identifier(&req.slot, 128)
            || !permissions::identifier(&req.field, 128)
            || req.value.is_empty()
            || req.value.len() > 65536
            || !matches!(req.operation.as_str(), "create" | "rotate")
            || (req.operation == "create" && req.expected_revision.is_some())
            || (req.operation == "rotate"
                && !req
                    .expected_revision
                    .as_ref()
                    .is_some_and(|s| s.parse::<u64>().is_ok_and(|v| v.to_string() == *s && v > 0)))
        {
            return Err("invalid_request");
        }
        let mut options=session.writers.iter().filter(|s|req.action.as_ref().is_none_or(|a|a==&s.action) && matches!(&s.scope,PermissionScope::WriteSecret{field,entry_kind,..} if field==&req.field && entry_kind==&req.entry_kind));
        let grant = options.next().ok_or("not_declared")?;
        if options.next().is_some() {
            return Err("invalid_request");
        }
        reader.ok_or("operation_unavailable")?.write_private(
            &session.installation,
            grant,
            &req,
            lease,
        )
    })();
    let mut reply = json!({"event":"streamDockManagerRpcResponse","protocol_id":PROTOCOL,"request_id":msg["request_id"].as_str().filter(|id|uuid::Uuid::parse_str(id).is_ok_and(|u|u.to_string()==*id)).unwrap_or("")});
    match result {
        Ok((entry, revision)) => {
            if lease.is_none_or(|l| !l.load(Ordering::Acquire)) {
                return false;
            }
            reply["result"] = json!({"entry_ref":entry,"revision":revision});
        }
        Err(code) => reply["error"] = json!({"code":code,"message":"Vault write unavailable"}),
    }
    send(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_foreign_identity_and_redacts_replies() {
        let alive = AtomicBool::new(true);
        let session = vault_rpc::SessionScope {
            installation: vault_rpc::VaultInstallation {
                owner: "test.owner".into(),
                token: "installed".into(),
                inventory_sha256: "verified".into(),
                storage_generation: "epoch".into(),
            },
            slots: vec![],
            writers: vec![ActionScope {
                action: "test.press".into(),
                required: true,
                scope: PermissionScope::WriteSecret {
                    field: "credential".into(),
                    entry_kind: "api-key".into(),
                    purpose: "Store key".into(),
                },
            }],
        };
        let msg = json!({"event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,"request_id":"12345678-1234-4234-8234-123456789abc","method":"write-secret","payload":{"slot":"token","field":"credential","entry_kind":"api-key","operation":"create","value":"synthetic-private"}});
        for (key, value, expected) in [
            ("owner", "other", "invalid_request"),
            ("entry_id", "copied", "invalid_request"),
            ("field", "unlisted", "not_declared"),
            ("operation", "rotate", "invalid_request"),
            ("action", "other", "not_declared"),
        ] {
            let mut request = msg.clone();
            request["payload"][key] = json!(value);
            assert!(dispatch(
                None,
                Some(&session),
                Some(&alive),
                request,
                512,
                Ok(()),
                |r| {
                    assert_eq!(r["error"]["code"], expected);
                    assert!(!r.to_string().contains("synthetic-private"));
                    true
                }
            ));
        }
    }
}
