//! Canonical public declarations. Local entry identities and grants never belong in manifests.
use super::vault_rpc::VaultSetupSlot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(tag = "id", deny_unknown_fields)]
pub enum PermissionScope {
    #[serde(rename = "ui.popup")]
    Popup { mode: String },
    #[serde(rename = "ui.notification")]
    Notification { mode: String },
    #[serde(rename = "notifications.create")]
    CreateNotification { mode: String },
    #[serde(rename = "clipboard.read-text")]
    ClipboardRead { mode: String },
    #[serde(rename = "clipboard.write-text")]
    ClipboardWrite { mode: String },
    #[serde(rename = "network.https")]
    Https {
        origin: String,
        recipe: String,
        data_class: String,
    },
    #[serde(rename = "network.http")]
    Http {
        origin: String,
        recipe: String,
        data_class: String,
    },
    #[serde(rename = "vault.read-location")]
    ReadLocation {
        field: String,
        entry_kind: String,
        purpose: String,
    },
    #[serde(rename = "vault.read-secret")]
    ReadSecret {
        field: String,
        entry_kind: String,
        purpose: String,
    },
    #[serde(rename = "vault.write-secret")]
    WriteSecret {
        field: String,
        entry_kind: String,
        purpose: String,
    },
    #[serde(rename = "vault.access")]
    Access {
        mode: String,
        field: String,
        operation: String,
        recipe: String,
        origin: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionPermissions {
    pub purpose: String,
    #[serde(default)]
    pub required: Vec<PermissionScope>,
    #[serde(default)]
    pub optional: Vec<PermissionScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionDeclaration {
    pub version: u8,
    pub actions: BTreeMap<String, ActionPermissions>,
}

pub(crate) fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn purpose(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

/// Parse an exact HTTP origin without DNS. Localhost always connects to IPv4 loopback.
pub fn loopback_http_address(origin: &str) -> Option<std::net::SocketAddr> {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let authority = origin.strip_prefix("http://")?;
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']')?;
        (
            &authority[..=end],
            authority[end + 1..]
                .strip_prefix(':')
                .or_else(|| (end + 1 == authority.len()).then_some("80"))?,
        )
    } else {
        authority.split_once(':').unwrap_or((authority, "80"))
    };
    let number = port.parse::<u16>().ok()?;
    if number == 0 || number.to_string() != port {
        return None;
    }
    let ip = match host {
        "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        "[::1]" => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        s => {
            let v = s.parse::<Ipv4Addr>().ok()?;
            if v.to_string() != s {
                return None;
            }
            IpAddr::V4(v)
        }
    };
    ip.is_loopback().then(|| SocketAddr::new(ip, number))
}
pub const WEBHOOK_ORIGIN: &str = "https://127.0.0.1:39481";

// Provider recipes accept their registry origin or its explicit default HTTPS port.
fn canonical_origin(origin: &mut String, recipe: &str) -> bool {
    if matches!(recipe, "manager-webhook" | "plugin-webhook") {
        return origin == WEBHOOK_ORIGIN;
    }
    let host = match recipe {
        "clipboard-summary" => "api.example.test",
        _ => return false,
    };
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https")
        || !authority
            .strip_suffix(":443")
            .unwrap_or(authority)
            .eq_ignore_ascii_case(host)
    {
        return false;
    }
    *origin = format!("https://{host}");
    true
}

impl PermissionScope {
    fn validate(&mut self) -> bool {
        match self {
            Self::Popup { mode } => mode == "text-result",
            Self::Notification { mode } => mode == "completion-only",
            Self::CreateNotification { mode } => mode == "plugin-message",
            Self::ClipboardRead { mode } => matches!(mode.as_str(), "direct" | "manager-recipe"),
            Self::ClipboardWrite { mode } => mode == "explicit-copy",
            Self::Http {
                origin,
                recipe,
                data_class,
            } => {
                matches!(recipe.as_str(), "plugin-webhook" | "manager-webhook")
                    && data_class == "example.ping"
                    && loopback_http_address(origin).is_some()
            }
            Self::Https {
                origin,
                recipe,
                data_class,
            } => {
                matches!(
                    (recipe.as_str(), data_class.as_str()),
                    ("clipboard-summary", "clipboard.text")
                        | ("manager-webhook" | "plugin-webhook", "example.ping")
                ) && canonical_origin(origin, recipe)
            }
            Self::Access {
                mode,
                field,
                operation,
                origin,
                recipe,
            } => {
                matches!(mode.as_str(), "browse-metadata" | "use-entry")
                    && identifier(field, 128)
                    && (recipe == "clipboard-summary" && operation == "authorize-request")
                    && canonical_origin(origin, recipe)
            }
            Self::ReadLocation {
                field,
                entry_kind,
                purpose: why,
            } => identifier(field, 128) && entry_kind == "location" && purpose(why),
            Self::ReadSecret {
                field,
                entry_kind,
                purpose: why,
            }
            | Self::WriteSecret {
                field,
                entry_kind,
                purpose: why,
            } => {
                identifier(field, 128)
                    && matches!(
                        entry_kind.as_str(),
                        "api-key" | "bearer-token" | "credential"
                    )
                    && purpose(why)
            }
        }
    }
    pub fn matches_direct_slot(&self, slot: &VaultSetupSlot) -> bool {
        matches!(self, Self::ReadLocation {field,entry_kind,purpose} if slot.kind=="location" && slot.mode=="direct" && field==&slot.field && entry_kind==&slot.entry_kind && purpose==&slot.purpose)
            || matches!(self, Self::ReadSecret { field, entry_kind, purpose }
            if slot.kind == "secret" && slot.mode == "direct" && field == &slot.field
                && entry_kind == &slot.entry_kind && purpose == &slot.purpose)
    }
}

pub fn declarations(manifest: &Value) -> Result<Option<PermissionDeclaration>, String> {
    let Some(value) = manifest["StreamDockManager"].get("Permissions") else {
        return Ok(None);
    };
    let mut declaration: PermissionDeclaration = serde_json::from_value(value.clone())
        .map_err(|_| "Invalid Manager permission declaration")?;
    let actions: BTreeSet<_> = manifest["Actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|action| action["UUID"].as_str())
        .collect();
    if declaration.version != 1 || declaration.actions.is_empty() || declaration.actions.len() > 128
    {
        return Err("Unsupported Manager permission declaration".into());
    }
    for (action, group) in &mut declaration.actions {
        let count = group.required.len() + group.optional.len();
        if !identifier(action, 256)
            || !actions.contains(action.as_str())
            || !purpose(&group.purpose)
            || !(1..=64).contains(&count)
        {
            return Err("Invalid Manager permission action".into());
        }
        for scopes in [&mut group.required, &mut group.optional] {
            for scope in scopes.iter_mut() {
                if !scope.validate() {
                    return Err("Unsupported Manager permission scope".into());
                }
            }
            scopes.sort();
            scopes.dedup();
        }
        if group
            .required
            .iter()
            .any(|scope| group.optional.binary_search(scope).is_ok())
        {
            return Err("A permission cannot be both required and optional".into());
        }
    }
    Ok(Some(declaration))
}

pub fn validate_slots(
    slots: &[VaultSetupSlot],
    declarations: Option<&PermissionDeclaration>,
) -> Result<(), String> {
    for slot in slots {
        if slot.mode == "broker" {
            let scope = slot.broker_scope().ok_or("Invalid Vault broker setup")?;
            let group = declarations
                .and_then(|d| d.actions.get(&slot.action))
                .ok_or("Missing Vault broker permission")?;
            let scopes = if slot.required {
                &group.required
            } else {
                &group.optional
            };
            if group.purpose != slot.purpose || !scopes.contains(&scope) {
                return Err("Vault broker setup requires an exact use-entry declaration for the action and purpose".into());
            }
            continue;
        }
        if !slot.requires_direct_approval() {
            continue;
        }
        let declared = declarations
            .and_then(|d| d.actions.get(&slot.action))
            .is_some_and(|p| {
                let scopes = if slot.required {
                    &p.required
                } else {
                    &p.optional
                };
                scopes.iter().any(|scope| scope.matches_direct_slot(slot))
            });
        if !declared {
            return Err("Direct Vault setup requires a matching read permission".into());
        }
    }
    Ok(())
}

impl super::manifest::NormalizedManifest {
    pub fn unavailable_required_permission(&self) -> bool {
        self.missing_required_services(&[])
    }
    pub fn missing_required_services(&self, accepted: &[ActionScope]) -> bool {
        self.permissions.as_ref().is_some_and(|d| {
            d.actions.iter().any(|(action, group)| {
                group.required.iter().any(|scope| {
                    !accepted
                        .iter()
                        .any(|a| a.required && a.action == *action && a.scope == *scope)
                        && !self.setup_slots.iter().flatten().any(|slot| {
                            slot.action == *action
                                && slot.required
                                && scope.matches_direct_slot(slot)
                        })
                })
            })
        })
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
pub struct ActionScope {
    pub action: String,
    pub required: bool,
    pub scope: PermissionScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PermissionDiff {
    pub added: Vec<ActionScope>,
    pub removed: Vec<ActionScope>,
    pub unchanged: Vec<ActionScope>,
}

pub fn scope_diff(
    current: Option<&PermissionDeclaration>,
    candidate: Option<&PermissionDeclaration>,
) -> Option<PermissionDiff> {
    if current.is_none() && candidate.is_none() {
        return None;
    }
    fn scopes(declaration: Option<&PermissionDeclaration>) -> BTreeSet<ActionScope> {
        declaration
            .into_iter()
            .flat_map(|d| &d.actions)
            .flat_map(|(action, group)| {
                [(true, &group.required), (false, &group.optional)]
                    .into_iter()
                    .flat_map(move |(required, scopes)| {
                        scopes.iter().cloned().map(move |scope| ActionScope {
                            action: action.clone(),
                            required,
                            scope,
                        })
                    })
            })
            .collect()
    }
    let current = scopes(current);
    let candidate = scopes(candidate);
    Some(PermissionDiff {
        added: candidate.difference(&current).cloned().collect(),
        removed: current.difference(&candidate).cloned().collect(),
        unchanged: current.intersection(&candidate).cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(required: Value, optional: Value) -> Value {
        json!({"Actions":[{"UUID":"fixture.action"}],"StreamDockManager":{"Permissions":{
            "version":1,"actions":{"fixture.action":{"purpose":"Review fixture data","required":required,"optional":optional}}
        }}})
    }

    fn https(origin: &str) -> Value {
        json!({"id":"network.https","origin":origin,"recipe":"clipboard-summary","data_class":"clipboard.text"})
    }

    #[test]
    fn http_webhooks_require_a_literal_loopback_origin_and_separate_grant() {
        for origin in [
            "http://localhost",
            "http://127.0.0.1:39480",
            "http://[::1]:3000",
        ] {
            let mut scope = PermissionScope::Http {
                origin: origin.into(),
                recipe: "manager-webhook".into(),
                data_class: "example.ping".into(),
            };
            assert!(scope.validate());
        }
        for origin in [
            "http://example.com",
            "http://192.168.1.2",
            "http://0.0.0.0",
            "http://localhost.example.org",
            "https://127.0.0.1:39480",
            "http://127.0.0.1:0",
            "http://[::1]/path",
            "http://u@localhost:3000",
            "http://127.0.0.1:80?x",
            "http://127.1",
            "http://[::ffff:127.0.0.1]",
            "http://localhost:65536",
        ] {
            assert!(loopback_http_address(origin).is_none(), "{origin}");
        }
        let mut wrong = PermissionScope::Http {
            origin: "http://localhost".into(),
            recipe: "clipboard-summary".into(),
            data_class: "clipboard.text".into(),
        };
        assert!(!wrong.validate());
    }

    #[test]
    fn validates_and_canonicalizes_exact_action_scopes() {
        assert_eq!(declarations(&json!({"Actions":[]})).unwrap(), None);
        let a = https("HTTPS://API.EXAMPLE.TEST:443");
        let b = https("https://api.example.test");
        let normalized = declarations(&manifest(json!([a, b]), json!([])))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&normalized.actions["fixture.action"].required).unwrap(),
            json!([b])
        );
        assert!(declarations(&manifest(json!([a]), json!([b]))).is_err());
        for origin in [
            "http://api.example.test",
            "https://api.example.test/",
            "https://api.example.test:444",
            "https://user@api.example.test",
            "https://api.example.test?x",
            "https://api.example.test#x",
            "https://*.example.test",
            "https://api.example.test.",
            "https://127.0.0.1",
        ] {
            assert!(declarations(&manifest(json!([https(origin)]), json!([]))).is_err());
        }
        for scope in [
            json!({"id":"ui.popup","mode":"html"}),
            json!({"id":"vault.read-secret","field":"token"}),
            json!({"id":"ui.popup","mode":"text-result","owner":"fixture"}),
            json!({"id":"storage.read-secret"}),
        ] {
            assert!(declarations(&manifest(json!([]), json!([scope]))).is_err());
        }
        let mut unknown = manifest(json!([a]), json!([]));
        unknown["Actions"] = json!([]);
        assert!(declarations(&unknown).is_err());
    }

    #[test]
    fn diff_reports_widening_removal_and_unchanged_exact_scopes() {
        let read = json!({"id":"clipboard.read-text","mode":"manager-recipe"});
        let popup = json!({"id":"ui.popup","mode":"text-result"});
        let current = declarations(&manifest(
            json!([read]),
            json!([popup, https("HTTPS://API.EXAMPLE.TEST:443")]),
        ))
        .unwrap();
        let incoming = declarations(&manifest(
            json!([popup]),
            json!([https("https://api.example.test")]),
        ))
        .unwrap();
        let diff = scope_diff(current.as_ref(), incoming.as_ref()).unwrap();
        assert_eq!(
            (diff.added.len(), diff.removed.len(), diff.unchanged.len()),
            (1, 2, 1)
        );
        assert!(diff.added[0].required);
        assert!(!diff.unchanged[0].required);
    }

    #[test]
    fn direct_read_slot_requires_a_separate_matching_declaration() {
        let slot = VaultSetupSlot {
            id: "key".into(),
            kind: "secret".into(),
            entry_kind: "api-key".into(),
            mode: "direct".into(),
            required: false,
            purpose: "Use reviewed fixture".into(),
            field: "key".into(),
            action: "fixture.action".into(),
            ..Default::default()
        };
        let scope = json!({"id":"vault.read-secret","field":"key","entry_kind":"api-key","purpose":slot.purpose});
        let slots = std::slice::from_ref(&slot);
        assert!(validate_slots(slots, None).is_err());
        let decl = declarations(&manifest(json!([]), json!([scope])))
            .unwrap()
            .unwrap();
        validate_slots(slots, Some(&decl)).unwrap();
        for altered in [
            VaultSetupSlot {
                required: true,
                ..slot.clone()
            },
            VaultSetupSlot {
                action: "fixture.other".into(),
                ..slot.clone()
            },
            VaultSetupSlot {
                purpose: "New use".into(),
                ..slot.clone()
            },
            VaultSetupSlot {
                field: "other".into(),
                ..slot.clone()
            },
        ] {
            assert!(validate_slots(&[altered], Some(&decl)).is_err());
        }
    }
}
