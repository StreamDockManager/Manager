//! Values safe to expose as text on physical displays; no plugin or renderer grants.
use super::*;
use std::collections::HashMap;
use stream_dock_manager_plugin_host::text_references::{Part, parts};

pub(super) type DisplayTextCache = HashMap<String, String>;

impl Vault {
    /// Resolve only while unlocked, without prolonging the Vault session or bypassing
    /// reveal protection. Cache bounded nonsecret values only for the live Vault revision.
    pub(crate) fn with_display_text<T>(
        &self,
        render: impl FnOnce(&dyn Fn(&str, usize) -> String) -> T,
    ) -> T {
        let state = self.current_state();
        render(&|source, limit| {
            let mut output = String::new();
            let mut remaining = limit;
            for part in parts(source) {
                let replacement;
                let text = match part {
                    Part::Reference { name, .. } if name.starts_with("vault.") => {
                        replacement = state
                            .session
                            .as_ref()
                            .map(|session| session.display_value(&name[6..]))
                            .unwrap_or_default();
                        &replacement
                    }
                    // A partially typed Vault reference is editor data, not visible copy.
                    Part::Unclosed(text) if text.starts_with("${vault.") => "",
                    Part::Reference { token, .. }
                    | Part::Literal(token)
                    | Part::Unclosed(token) => token,
                };
                let mut length = text.len().min(remaining);
                while !text.is_char_boundary(length) {
                    length -= 1;
                }
                output.push_str(&text[..length]);
                remaining -= length;
                if length < text.len() || remaining == 0 {
                    break;
                }
            }
            output
        })
    }
}

impl Session {
    fn display_value(&self, id: &str) -> String {
        let Ok(reference) = TypedReference::parse(id) else {
            return String::new();
        };
        if reference.vault != self.id {
            return String::new();
        }
        let mut cache = self.display_text.borrow_mut();
        if let Some(value) = cache.get(id) {
            return value.clone();
        }
        let result = self
            .vault
            .rows(|rows| {
                let Some(entry) = rows::header(rows, reference.entry)?
                    .filter(|e| e.owner.is_none() && !e.protected)
                else {
                    return Ok(String::new());
                };
                let eligible = matches!(
                    (entry.kind, reference.field),
                    (
                        VaultKind::Text | VaultKind::Integer | VaultKind::Boolean,
                        None
                    ) | (VaultKind::Location, _)
                );
                if !eligible {
                    return Ok(String::new());
                }
                let value = rows::value(rows, entry.id)?;
                entry.kind.validate(&value.0)?;
                let output = match (entry.kind, reference.field) {
                    (VaultKind::Text, None) => value
                        .0
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(4096)
                        .collect(),
                    (VaultKind::Location, Some(field)) => match field.project(&value.0)? {
                        Value::String(text) => text,
                        value => value.to_string(),
                    },
                    (VaultKind::Location, None) => {
                        format!("{},{}", value.0["latitude"], value.0["longitude"])
                    }
                    _ => value.0.to_string(),
                };
                Ok(output)
            })
            .unwrap_or_default();
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(id.to_owned(), result.clone());
        result
    }
}
