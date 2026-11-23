//! Explicit native template RPC; ordinary plugin settings are never resolved here.
use super::*;
use crate::vault_rpc::{SessionScope, VaultRead, VaultSetupSlot};

pub const PROTOCOL: &str = "plugin-text-references-v1";
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_WIRE_BYTES: usize = 512 * 1024;

pub fn eligible_slot(slot: &VaultSetupSlot) -> bool {
    slot.mode == "direct"
        && matches!(
            (slot.kind.as_str(), slot.entry_kind.as_str()),
            ("value", "text") | ("location", "location")
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    action: &'a str,
    text: &'a str,
}

pub(super) fn dispatch(
    reader: Option<&dyn VaultRead>,
    scope: Option<&SessionScope>,
    alive: Option<&AtomicBool>,
    message: &Value,
    length: usize,
    admitted: Result<(), &'static str>,
    mut send: impl FnMut(String) -> bool,
) -> bool {
    let request_id = message["request_id"].as_str().unwrap_or_default();
    if !uuid::Uuid::parse_str(request_id).is_ok_and(|id| id.to_string() == request_id) {
        return false;
    }
    let reply = |result: Result<&str, &'static str>| match result {
        Ok(text) => json!({
            "event":"streamDockManagerRpcResponse",
            "protocol_id":PROTOCOL, "request_id":request_id,
            "result":{"text":text}
        })
        .to_string(),
        Err(code) => json!({
            "event":"streamDockManagerRpcResponse",
            "protocol_id":PROTOCOL, "request_id":request_id,
            "error":{"code":code,"message":"Text formatting unavailable"}
        })
        .to_string(),
    };
    let mut sent = None;
    let mut publish = |formatted: &str| {
        let wire = (formatted.len() <= MAX_OUTPUT_BYTES)
            .then(|| reply(Ok(formatted)))
            .filter(|wire| wire.len() <= MAX_WIRE_BYTES && wire.len() <= MAX_MESSAGE_BYTES);
        let accepted = wire.is_some();
        if alive.is_none_or(|flag| !flag.load(Ordering::Acquire)) {
            sent = Some(false);
            return false;
        }
        let delivered = send(wire.unwrap_or_else(|| reply(Err("quota"))));
        sent = Some(delivered);
        // A quota error was delivered, but no Vault value was published.
        accepted && delivered
    };
    let result = (|| {
        admitted?;
        if length > MAX_WIRE_BYTES || length > MAX_MESSAGE_BYTES {
            return Err("quota");
        }
        if message["method"] != "format-text"
            || !message.as_object().is_some_and(|m| {
                m.len() == 5
                    && ["event", "protocol_id", "request_id", "method", "payload"]
                        .iter()
                        .all(|key| m.contains_key(*key))
            })
        {
            return Err("invalid_request");
        }
        let scope = scope
            .filter(|scope| scope.slots.iter().any(eligible_slot))
            .ok_or("permission_denied")?;
        let alive = alive
            .filter(|flag| flag.load(Ordering::Acquire))
            .ok_or("permission_denied")?;
        let request = Request::deserialize(&message["payload"]).map_err(|_| "invalid_request")?;
        if !permissions::identifier(request.action, 256) {
            return Err("invalid_request");
        }
        if request.text.len() > MAX_INPUT_BYTES {
            return Err("quota");
        }
        // No Vault-formatting work for absent or unfinished relevant placeholders.
        if !request
            .text
            .split_once("${vault.")
            .is_some_and(|(_, tail)| tail.contains('}'))
        {
            if !alive.load(Ordering::Acquire) {
                return Err("cancelled");
            }
            return publish(request.text).then_some(()).ok_or("cancelled");
        }
        let slots: Vec<_> = scope
            .slots
            .iter()
            .filter(|slot| slot.action == request.action && eligible_slot(slot))
            .collect();
        reader
            .ok_or("operation_unavailable")?
            .format_text_and_publish(
                &scope.installation,
                &slots,
                request.text,
                &mut |formatted| {
                    if !alive.load(Ordering::Acquire) {
                        return false;
                    }
                    publish(formatted)
                },
            )
    })();
    match (sent, result) {
        (Some(delivered), _) => delivered,
        (None, Err(code)) => send(reply(Err(code))),
        (None, Ok(())) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Reader<'a> {
        calls: &'a AtomicUsize,
        reply: &'a str,
    }

    impl VaultRead for Reader<'_> {
        fn read_and_publish(
            &self,
            _: &vault_rpc::VaultInstallation,
            _: &VaultSetupSlot,
            _: &mut dyn FnMut(&Value, &str) -> bool,
        ) -> Result<(), &'static str> {
            panic!("formatter must not route through direct secret sharing");
        }

        fn format_text_and_publish(
            &self,
            owner: &vault_rpc::VaultInstallation,
            slots: &[&VaultSetupSlot],
            _: &str,
            publish: &mut dyn FnMut(&str) -> bool,
        ) -> Result<(), &'static str> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(owner.owner, "fixture");
            assert!(slots
                .iter()
                .all(|slot| slot.action == "fixture.show" && slot.kind == "value"));
            publish(self.reply).then_some(()).ok_or("cancelled")
        }
    }

    #[test]
    fn requires_live_scope_and_bounds_utf8() {
        let scope = SessionScope {
            installation: vault_rpc::VaultInstallation {
                owner: "fixture".into(),
                token: "install".into(),
                inventory_sha256: "inventory".into(),
                storage_generation: "generation".into(),
            },
            slots: [
                ("fixture.show", "value", "text"),
                ("fixture.show", "secret", "api-key"),
                ("fixture.other", "location", "location"),
            ]
            .into_iter()
            .map(|(action, kind, entry_kind)| VaultSetupSlot {
                action: action.into(),
                kind: kind.into(),
                entry_kind: entry_kind.into(),
                mode: "direct".into(),
                ..VaultSetupSlot::default()
            })
            .collect(),
            writers: vec![],
        };
        let calls = AtomicUsize::new(0);
        let send = |scope: Option<&SessionScope>, live, text: &str, reply| {
            let request = json!({
                "event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,
                "request_id":"10000000-0000-4000-8000-000000000002",
                "method":"format-text","payload":{"action":"fixture.show","text":text}
            });
            let mut result = Value::Null;
            assert!(dispatch(
                Some(&Reader {
                    calls: &calls,
                    reply
                }),
                scope,
                Some(&AtomicBool::new(live)),
                &request,
                request.to_string().len(),
                Ok(()),
                |wire| {
                    assert!(result.is_null(), "exactly one reply");
                    result = serde_json::from_str(&wire).unwrap();
                    true
                }
            ));
            result
        };
        assert_eq!(
            send(None, true, "plain", "")["error"]["code"],
            "permission_denied"
        );
        assert_eq!(
            send(Some(&scope), false, "plain", "")["error"]["code"],
            "permission_denied"
        );
        let input = "🌤".repeat(MAX_INPUT_BYTES / 4);
        assert_eq!(
            send(Some(&scope), true, &input, "")["result"]["text"],
            input
        );
        assert_eq!(
            send(Some(&scope), true, &(input + "a"), "")["error"]["code"],
            "quota"
        );
        let unfinished = "keep ${vault.unclosed";
        assert_eq!(
            send(Some(&scope), true, unfinished, "")["result"]["text"],
            unfinished
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "ordinary text requires no formatter"
        );
        assert_eq!(
            send(Some(&scope), true, "${vault.fixture}", "result")["result"]["text"],
            "result"
        );
        let over_wire = "\0".repeat(MAX_OUTPUT_BYTES);
        assert_eq!(
            send(Some(&scope), true, "${vault.fixture}", &over_wire)["error"]["code"],
            "quota"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
