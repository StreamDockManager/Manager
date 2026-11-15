//! Durable references and transient resolved plugin settings must never share storage
use super::*;

/// Mask original text once, without matching credentials inside the replacement
pub fn redact_known_values(value: &str, secrets: &[String]) -> String {
    redact_prepared(value, &redaction_patterns(secrets))
}
// Collect secret-reference patterns for diagnostic redaction
pub(super) fn redaction_patterns(secrets: &[String]) -> Vec<String> {
    let mut patterns = vec![];
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        patterns.push(secret.clone());
        let encoded = secret
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>();
        patterns.push(encoded.replace("%20", "+"));
        patterns.push(encoded);
        if let Ok(encoded) = serde_json::to_string(secret) {
            patterns.push(encoded[1..encoded.len() - 1].to_string());
        }
    }
    // Treat an existing marker as an already-covered range. This makes repeated
    // sanitization idempotent even when a credential occurs inside its spelling.
    patterns.push("[credential]".into());
    patterns.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    patterns.dedup();
    patterns
}
// Redact sensitive values from prepared plugin data
pub(super) fn redact_prepared(value: &str, patterns: &[String]) -> String {
    if patterns.len() == 1 {
        return value.into();
    }
    let mut result = String::with_capacity(value.len());
    let mut covered_until = 0;
    let mut masking = false;
    for (offset, character) in value.char_indices() {
        if let Some(pattern) = patterns
            .iter()
            .find(|pattern| value[offset..].starts_with(pattern.as_str()))
        {
            covered_until = covered_until.max(offset + pattern.len());
        }
        if offset < covered_until {
            if !masking {
                result.push_str("[credential]");
            }
            masking = true;
        } else {
            masking = false;
            result.push(character);
        }
    }
    result
}

// Recognize a complete parameter or input reference expression
fn reference(value: &Value) -> bool {
    let valid = |name: &str| {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    value.as_str().is_some_and(|value| {
        ["${parameter.", "${input."].iter().any(|prefix| {
            value
                .strip_prefix(prefix)
                .and_then(|s| s.strip_suffix('}'))
                .is_some_and(valid)
        })
    }) || value.as_object().is_some_and(|map| {
        map.len() == 1
            && ["$parameter", "$input"]
                .iter()
                .any(|key| map.get(*key).and_then(Value::as_str).is_some_and(valid))
    })
}
// Keep managed references while applying edited settings
pub fn preserve_references(source: &Value, incoming: &Value) -> Value {
    let mut result = incoming.clone();
    if let Some(source) = source.as_object() {
        if source.values().any(reference) && !result.is_object() {
            result = json!({});
        }
        if let Some(result) = result.as_object_mut() {
            for (key, value) in source {
                if reference(value) {
                    result.insert(key.clone(), value.clone());
                }
            }
        }
    }
    result
}
impl Hub {
    // Refresh log secrets
    pub(super) fn refresh_log_secrets(&self) {
        // Collect for the current operation
        fn collect(value: &Value, secrets: &mut Vec<String>) {
            if let Some(items) = value.as_array() {
                for item in items {
                    collect(item, secrets);
                }
            }
            if let Some(map) = value.as_object() {
                for (key, value) in map {
                    let key = key.to_ascii_lowercase();
                    if [
                        "password",
                        "secret",
                        "token",
                        "api_key",
                        "api-key",
                        "authorization",
                        "cookie",
                        "webhook",
                    ]
                    .iter()
                    .any(|part| key.contains(part))
                    {
                        if let Some(value) = value
                            .as_str()
                            .filter(|s| !s.is_empty() && !s.starts_with("${"))
                        {
                            secrets.push(value.into());
                            if let Some(token) = value.strip_prefix("Bearer ") {
                                secrets.push(token.into());
                            }
                        }
                    }
                    collect(value, secrets);
                }
            }
        }
        if let Some(logs) = &self.logs {
            let mut secrets = self.secret_redactions.clone();
            secrets.extend(self.sessions.tokens.keys().cloned());
            for value in self
                .globals
                .values()
                .chain(self.context_state.settings.values())
                .chain(self.context_state.execution.values())
            {
                collect(value, &mut secrets);
            }
            secrets.sort();
            secrets.dedup();
            logs.set_secrets(secrets);
        }
    }

    // Prepare settings for durable storage
    pub(super) fn durable_settings(&self, context: &str, incoming: &Value) -> Value {
        let mut result = incoming.clone();
        if let Some(source) = self
            .context_state
            .reference_sources
            .get(context)
            .or_else(|| self.context_state.configured.get(context))
            .or_else(|| self.context_state.settings.get(context))
            .and_then(Value::as_object)
        {
            if source.values().any(reference) && !result.is_object() {
                result = json!({});
            }
            if let Some(result) = result.as_object_mut() {
                for (key, value) in source {
                    if reference(value) {
                        result.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        self.redact_payload(&mut result);
        result
    }
    // Resolve settings for plugin execution
    pub(super) fn execution_settings(&self, context: &str, fallback: &Value) -> Value {
        let mut result = self
            .context_state
            .settings
            .get(context)
            .unwrap_or(fallback)
            .clone();
        if let (Some(result), Some(source), Some(resolved)) = (
            result.as_object_mut(),
            self.context_state
                .reference_sources
                .get(context)
                .and_then(Value::as_object),
            self.context_state
                .execution
                .get(context)
                .and_then(Value::as_object),
        ) {
            for (key, value) in source {
                if reference(value) {
                    if let Some(value) = resolved.get(key) {
                        result.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        result
    }
    // Remove sensitive settings values from an outbound diagnostic payload
    pub(super) fn redact_payload(&self, value: &mut Value) {
        if reference(value) {
            return;
        }
        match value {
            Value::String(value) => {
                *value = redact_known_values(value, &self.secret_redactions);
            }
            Value::Array(items) => {
                for value in items {
                    self.redact_payload(value);
                }
            }
            Value::Object(items) => {
                for value in items.values_mut() {
                    self.redact_payload(value);
                }
            }
            _ => {}
        }
    }
}
impl PluginHost {
    #[allow(clippy::too_many_arguments)]
    // Set execution settings
    pub fn set_execution_settings(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        durable: &Value,
        resolved: &Value,
        secrets: Vec<String>,
    ) {
        let mut hub = self.hub.lock().unwrap();
        let context = hub.binding_context(id, scope, device, control);
        hub.secret_redactions = secrets;
        hub.refresh_log_secrets();
        let previous = hub.execution_settings(&context, durable);
        let mut resolved = resolved.clone();
        if let (Some(next), Some(previous), Some(source), Some(durable)) = (
            resolved.as_object_mut(),
            hub.context_state
                .execution
                .get(&context)
                .and_then(Value::as_object),
            hub.context_state
                .reference_sources
                .get(&context)
                .and_then(Value::as_object),
            durable.as_object(),
        ) {
            for (key, value) in next {
                let unresolved_input = value
                    .as_str()
                    .is_some_and(|v| v.starts_with("${input.") && reference(value))
                    || value
                        .as_object()
                        .is_some_and(|v| v.len() == 1 && v.contains_key("$input"));
                if unresolved_input && source.get(key) == durable.get(key) {
                    if let Some(previous) = previous.get(key) {
                        *value = previous.clone();
                    }
                }
            }
        }
        hub.context_state
            .execution
            .insert(context.clone(), resolved);
        hub.context_state
            .reference_sources
            .insert(context.clone(), durable.clone());
        let current = hub
            .context_state
            .settings
            .get(&context)
            .unwrap_or(durable)
            .clone();
        let safe = hub.durable_settings(&context, &current);
        let changed = current != safe;
        hub.context_state.settings.insert(context.clone(), safe);
        if changed {
            let _ = hub.persist();
        }
        let next = hub.execution_settings(&context, durable);
        if previous != next {
            if let Some(mut event) = hub.context_state.visible.get(&context).cloned() {
                event["event"] = json!("didReceiveSettings");
                if let Some(visible) = hub.context_state.visible.get_mut(&context) {
                    visible["payload"]["settings"] = next.clone();
                }
                event["payload"] = json!({"settings": next});
                hub.visual_state.render_generation =
                    hub.visual_state.render_generation.wrapping_add(1);
                let queue = hub.sessions.queues.entry(id.into()).or_default();
                queue.push_back(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify redactions cover overlapping matches and keep markers idempotent
    fn redactions_cover_overlapping_matches_and_keep_markers_idempotent() {
        let secrets = vec!["abcdef".into(), "xa".into(), "aa".into(), "a".into()];
        let masked = redact_known_values("xabcdef / aaa / 🔒", &secrets);
        assert_eq!(masked, "[credential] / [credential] / 🔒");
        assert_eq!(redact_known_values(&masked, &secrets), masked);
    }
    #[test]
    // Verify plugin echoes keep named references and never persist resolved values
    fn plugin_echoes_keep_named_references_and_never_persist_resolved_values() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        let mut hub = Hub::load(&storage).unwrap();
        let durable = json!({"token":{"$parameter":"api_key"},"counter":0,"unknown":{"keep":true}});
        hub.context_state
            .reference_sources
            .insert("context".into(), durable.clone());
        hub.context_state
            .settings
            .insert("context".into(), durable.clone());
        hub.context_state.execution.insert(
            "context".into(),
            json!({"token":"secret-token","counter":0}),
        );
        hub.secret_redactions.push("secret-token".into());
        assert_eq!(
            hub.execution_settings("context", &durable)["token"],
            "secret-token"
        );
        let clean = hub.durable_settings("context", &json!({"token":"secret-token","counter":2,"echo":"contains secret-token","unknown":{"keep":true}}));
        assert_eq!(clean["token"], durable["token"]);
        assert_eq!(clean["counter"], 2);
        assert_eq!(clean["echo"], "contains [credential]");
        assert_eq!(clean["unknown"], durable["unknown"]);
        hub.context_state.settings.insert("context".into(), clean);
        assert_eq!(hub.execution_settings("context", &durable)["counter"], 2);
        assert!(!serde_json::to_string(&hub.context_state.settings)
            .unwrap()
            .contains("secret-token"));
        assert_eq!(
            hub.durable_settings("context", &json!({}))["token"],
            durable["token"]
        );
        hub.persist().unwrap();
        let restored = Hub::load(&storage).unwrap();
        assert!(
            restored.context_state.settings.is_empty(),
            "runtime contexts are projections, never a second durable copy"
        );
        assert!(!serde_json::to_string(&restored.context_state.settings)
            .unwrap()
            .contains("secret-token"));
        assert!(restored.context_state.execution.is_empty());
    }
}
