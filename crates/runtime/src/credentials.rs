//! OS-held credentials. Public commands expose status, never stored values
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Configured,
    Missing,
    Locked,
    NeedsReentry,
}

pub trait CredentialProvider: Send + Sync {
    // Read the requested credentials data
    fn read(&self, account: &str) -> Result<String, CredentialState>;
    // Write the supplied credentials data
    fn write(&self, account: &str, value: &str) -> Result<(), CredentialState>;
    // Remove the selected credentials entry
    fn delete(&self, account: &str) -> Result<(), CredentialState>;
}
pub struct OsCredentials;
impl OsCredentials {
    // Open the account’s keyring entry under the Manager service namespace
    fn entry(account: &str) -> Result<keyring::Entry, CredentialState> {
        keyring::Entry::new("app.streamdockmanager.manager", account)
            .map_err(|_| CredentialState::Locked)
    }
    // Map keyring failures to public credential availability states
    fn error(error: keyring::Error) -> CredentialState {
        if matches!(error, keyring::Error::NoEntry) {
            CredentialState::Missing
        } else {
            CredentialState::Locked
        }
    }
}
impl CredentialProvider for OsCredentials {
    // Read the requested credentials data
    fn read(&self, account: &str) -> Result<String, CredentialState> {
        Self::entry(account)?.get_password().map_err(Self::error)
    }
    // Write the supplied credentials data
    fn write(&self, account: &str, value: &str) -> Result<(), CredentialState> {
        Self::entry(account)?
            .set_password(value)
            .map_err(Self::error)
    }
    // Remove the selected credentials entry
    fn delete(&self, account: &str) -> Result<(), CredentialState> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(Self::error(error)),
        }
    }
}

pub struct Credentials {
    pub(crate) provider: Arc<dyn CredentialProvider>,
    pub(crate) changes: Mutex<()>,
    redactions: Mutex<Vec<String>>,
}
impl Default for Credentials {
    // Construct the default credentials value
    fn default() -> Self {
        Self::with_provider(Arc::new(OsCredentials))
    }
}
impl Credentials {
    // Construct using provider
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            provider,
            changes: Mutex::new(()),
            redactions: Mutex::new(vec![]),
        }
    }
    // Read the requested credentials data
    pub(crate) fn read(&self, account: &str) -> Result<String, String> {
        let value = self.provider.read(account).map_err(|state| {
            match state {
                CredentialState::Missing => {
                    "Credential is missing. Configure it in Profile parameters."
                }
                _ => "The OS credential store is locked or unavailable. Unlock it and try again.",
            }
            .to_owned()
        })?;
        self.remember(&value);
        Ok(value)
    }
    // Remember secret values for later redaction, longest matches first
    pub(crate) fn remember(&self, value: &str) {
        if value.is_empty() {
            return;
        }
        let mut redactions = self.redactions.lock().unwrap();
        if !redactions.iter().any(|known| known == value) {
            redactions.push(value.into());
            // Longest first avoids exposing a suffix when credentials overlap
            redactions.sort_by_key(|value| std::cmp::Reverse(value.len()));
        }
    }
    // Copy the currently registered secret values for diagnostic redaction
    pub(crate) fn redactions(&self) -> Vec<String> {
        self.redactions.lock().unwrap().clone()
    }
    // Redact event
    pub(crate) fn redact_event(&self, kind: &str, value: String) -> String {
        match kind {
            // These are first-party identifiers/selectors, never resolved fields
            "action-ui-close" | "action-show-manager" | "action-run" => value,
            "action-ui" => {
                let Ok(mut data) = serde_json::from_str::<serde_json::Value>(&value) else {
                    return self.redact_text(value);
                };
                for key in ["title", "text", "labels"] {
                    if let Some(field) = data.get_mut(key) {
                        let wrapped = serde_json::json!({"value":field});
                        *field = serde_json::from_str::<serde_json::Value>(
                            &self.redact(wrapped.to_string()),
                        )
                        .unwrap()["value"]
                            .take();
                    }
                }
                data.to_string()
            }
            "action-notification" => self.redact(value),
            _ => self.redact_text(value),
        }
    }
    // Redact text
    fn redact_text(&self, value: String) -> String {
        stream_dock_manager_plugin_host::redact_known_values(
            &value,
            &self.redactions.lock().unwrap(),
        )
    }
    // Redact sensitive credentials fields
    pub(crate) fn redact(&self, value: String) -> String {
        let secrets = self.redactions.lock().unwrap();
        if secrets.is_empty() {
            return value;
        }
        // Redact known secret values from a string
        fn text(value: String, secrets: &[String]) -> String {
            stream_dock_manager_plugin_host::redact_known_values(&value, secrets)
        }
        // Redact string values recursively while retaining JSON keys and structure
        fn fields(value: &mut serde_json::Value, secrets: &[String]) {
            match value {
                serde_json::Value::String(value) => *value = text(std::mem::take(value), secrets),
                serde_json::Value::Array(values) => {
                    for value in values {
                        fields(value, secrets);
                    }
                }
                serde_json::Value::Object(values) => {
                    for value in values.values_mut() {
                        fields(value, secrets);
                    }
                }
                _ => {}
            }
        }
        // Structured runtime messages retain their contract keys and JSON framing,
        // even when a credential is a single quote or another low-entropy value
        if let Ok(mut structured) = serde_json::from_str::<serde_json::Value>(&value) {
            if structured.is_object() || structured.is_array() {
                fields(&mut structured, &secrets);
                return structured.to_string();
            }
        }
        text(value, &secrets)
    }
}

// Recognize a single supported credential reference without resolving its secret
pub(crate) fn reference(value: &serde_json::Value) -> Option<&str> {
    value
        .as_object()
        .filter(|map| map.len() == 1)
        .and_then(|map| map.get("$credential"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            id.strip_prefix("parameter-")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        })
}

#[cfg(test)]
mod redaction_tests {
    #[test]
    // Verify that runtime messages mask both plain and JSON encoded credentials
    fn runtime_messages_mask_both_plain_and_json_encoded_credentials() {
        let credentials = super::Credentials::default();
        let secret = "quote\" slash\\ newline\nend";
        credentials.remember(secret);
        assert_eq!(credentials.redact(secret.into()), "[credential]");
        let message = serde_json::json!({"value":secret}).to_string();
        let masked: serde_json::Value = serde_json::from_str(&credentials.redact(message)).unwrap();
        assert_eq!(masked["value"], "[credential]");
    }
    #[test]
    // Verify that low entropy credentials cannot corrupt structured event keys
    fn low_entropy_credentials_cannot_corrupt_structured_event_keys() {
        let credentials = super::Credentials::default();
        credentials.remember("\"");
        let message = serde_json::json!({"outcome":"completed","title":"a\"b"}).to_string();
        let masked: serde_json::Value = serde_json::from_str(&credentials.redact(message)).unwrap();
        assert_eq!(masked["outcome"], "completed");
        assert_eq!(masked["title"], "a[credential]b");
    }
    #[test]
    // Verify that event redaction preserves popup correlation and masks opaque log keys
    fn event_redaction_preserves_popup_correlation_and_masks_opaque_log_keys() {
        let credentials = super::Credentials::default();
        credentials.remember("a");
        let message =
            serde_json::json!({"id":"a-uuid","kind":"path","title":"a","labels":["a"]}).to_string();
        let masked: serde_json::Value =
            serde_json::from_str(&credentials.redact_event("action-ui", message)).unwrap();
        assert_eq!(masked["id"], "a-uuid");
        assert_eq!(masked["kind"], "path");
        assert_eq!(masked["title"], "[credential]");
        assert_eq!(masked["labels"][0], "[credential]");
        assert_eq!(
            credentials.redact_event("action-ui-close", "a-uuid".into()),
            "a-uuid"
        );
        credentials.remember("secret-key");
        assert!(!credentials
            .redact_event("plugin-log", r#"{"secret-key":"value"}"#.into())
            .contains("secret-key"));
    }
}
