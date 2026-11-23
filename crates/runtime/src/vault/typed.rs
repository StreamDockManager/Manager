//! A Vault reference names two canonical IDs and at most one schema-defined field.
use super::*;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocationField {
    Latitude,
    Longitude,
    Name,
}
pub(crate) struct TypedReference {
    pub vault: uuid::Uuid,
    pub entry: uuid::Uuid,
    pub field: Option<LocationField>,
}
impl TypedReference {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let text = if text.starts_with("${") {
            text.strip_prefix("${vault.")
                .and_then(|s| s.strip_suffix('}'))
                .ok_or("invalid_vault_reference")?
        } else {
            text
        };
        let mut parts = text.split('.');
        let mut id = || {
            let raw = parts.next().ok_or("invalid_vault_reference")?;
            uuid::Uuid::parse_str(raw)
                .ok()
                .filter(|id| id.to_string() == raw)
                .ok_or("invalid_vault_reference")
        };
        let vault = id()?;
        let entry = id()?;
        let field = match parts.next() {
            None => None,
            Some("latitude") => Some(LocationField::Latitude),
            Some("longitude") => Some(LocationField::Longitude),
            Some("name") => Some(LocationField::Name),
            _ => return Err("invalid_vault_reference"),
        };
        if parts.next().is_some() {
            return Err("invalid_vault_reference");
        }
        Ok(Self {
            vault,
            entry,
            field,
        })
    }
}
impl LocationField {
    pub(super) fn project(self, location: &Value) -> Result<Value, &'static str> {
        match self {
            Self::Name => location["name"].as_str().map(|s| Value::String(s.into())),
            Self::Latitude | Self::Longitude => {
                let field = if self == Self::Latitude {
                    "latitude"
                } else {
                    "longitude"
                };
                location[field]
                    .as_f64()
                    .map(|n| serde_json::json!((n * 100.).round() / 100.))
            }
        }
        .ok_or("invalid_vault_reference")
    }
}
impl Vault {
    pub fn json_choices(&self) -> Result<Vec<VaultEntry>, String> {
        self.row_operation(|s, rows| {
            let mut choices = Vec::new();
            for entry in rows::headers(rows)?.iter().filter(|e| e.owner.is_none()) {
                if entry.kind == VaultKind::Json && !entry.protected {
                    choices.push(entry.metadata(s.id));
                }
                if entry.kind == VaultKind::Location {
                    for field in ["name", "latitude", "longitude"] {
                        let mut choice = entry.metadata(s.id);
                        choice.id = format!("{}.{field}", choice.id);
                        choice.label = format!("{} · {field}", choice.label);
                        choices.push(choice);
                    }
                }
            }
            choices.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok(choices)
        })
    }
    pub(crate) fn matches_json(&self, reference: &str, input: &str) -> Result<bool, String> {
        let reference = TypedReference::parse(reference)?;
        if input.len() > 256 * 1024 {
            return Err("Clipboard JSON exceeds the size limit".into());
        }
        let actual = ValueBuffer(
            serde_json::from_str(input).map_err(|_| "Clipboard text is not valid JSON")?,
        );
        self.row_operation(|s, rows| {
            if reference.vault != s.id {
                return Err("ineligible_vault_reference".into());
            }
            let entry = rows::header(rows, reference.entry)?
                .filter(|entry| entry.owner.is_none())
                .ok_or("ineligible_vault_reference")?;
            match (entry.kind, reference.field) {
                (VaultKind::Json, None) if !entry.protected => {
                    Ok(rows::value(rows, entry.id)?.0 == actual.0)
                }
                (VaultKind::Location, Some(field)) => {
                    let value = rows::value(rows, entry.id)?;
                    entry.kind.validate(&value.0)?;
                    let projected = ValueBuffer(field.project(&value.0)?);
                    Ok(projected.0 == actual.0)
                }
                _ => Err("ineligible_vault_reference".into()),
            }
        })
    }
}
