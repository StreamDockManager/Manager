//! Vault values used as operands rather than presentation strings.
//! Unavailable values must not select a different slot, file or comparison.
use super::Run;
use sha2::{Digest, Sha256};
use stream_dock_manager_plugin_host::text_references::{expand, parts, Part};
use zeroize::Zeroizing;

const OPERAND_UNAVAILABLE: &str = "The saved Vault value is unavailable for this action";
const CLIPBOARD_PREFIX: &str = "clipboard-vault:";

pub(super) fn is_clipboard_key(key: &str) -> bool {
    key.strip_prefix(CLIPBOARD_PREFIX).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

impl Run {
    // Require the identical authored field so parameter or macro input injection
    // cannot introduce new references or change the selector around a reference
    fn has_saved_vault_operand(&self, source: &str, effective: &str) -> Result<bool, String> {
        if !source.contains("${vault.") && !effective.contains("${vault.") {
            return Ok(false);
        }
        if source != effective {
            return Err("Write the Vault reference directly in the saved field".into());
        }
        if self.simulation.is_some() || self.debug.is_some() {
            return Err("Vault operands require a live native action".into());
        }
        self.check()?;
        Ok(true)
    }

    pub(super) fn vault_text_operand(
        &self,
        source: &str,
        effective: &str,
        limit: usize,
    ) -> Result<Option<Zeroizing<String>>, String> {
        if !self.has_saved_vault_operand(source, effective)? {
            return Ok(None);
        }
        if source.len() > 256 * 1024
            || parts(source).any(|part| match part {
                Part::Reference { name, .. } => name.contains("${vault."),
                Part::Unclosed(rest) => rest.contains("${vault."),
                Part::Literal(_) => false,
            })
        {
            return Err(OPERAND_UNAVAILABLE.into());
        }
        let (lease, text) = self
            .runtime
            .vault
            .with_builtin_references(false, |resolve| {
                expand(source, limit, |name| {
                    name.strip_prefix("vault.").map(resolve).transpose()
                })
                .map(Zeroizing::new)
            })
            .map_err(|_| OPERAND_UNAVAILABLE)?;
        self.check()?;
        self.runtime
            .vault
            .check_builtin_reference(lease)
            .map_err(|_| OPERAND_UNAVAILABLE)?;
        Ok(Some(text))
    }

    pub(super) fn vault_path_operand(
        &self,
        source: &str,
        effective: &str,
    ) -> Result<Option<Zeroizing<String>>, String> {
        if !self.has_saved_vault_operand(source, effective)? {
            return Ok(None);
        }
        let mut segments = parts(source);
        let reference = match (segments.next(), segments.next()) {
            (Some(Part::Reference { name, .. }), None) => name.strip_prefix("vault."),
            _ => None,
        }
        .ok_or("A file condition requires one complete Vault path reference")?;
        let (lease, path) = self
            .runtime
            .vault
            .with_unprotected_builtin_paths(|resolve| resolve(reference).map(Zeroizing::new))
            .map_err(|_| OPERAND_UNAVAILABLE)?;
        if !local_absolute_path(&path) {
            return Err(
                "The saved Vault path must identify an absolute local file or folder".into(),
            );
        }
        self.check()?;
        self.runtime
            .vault
            .check_builtin_reference(lease)
            .map_err(|_| OPERAND_UNAVAILABLE)?;
        Ok(Some(path))
    }

    pub(super) fn clipboard_slot_key(&self, source: &str, name: &str) -> Result<String, String> {
        let Some(expanded) = self.vault_text_operand(source, name, 4096)? else {
            return Ok(format!("clipboard:{name}"));
        };
        if expanded.is_empty() || expanded.chars().any(char::is_control) {
            return Err("The saved Vault clipboard slot name is invalid".into());
        }
        // Keep the resolved Vault value out of preference keys and separate
        // dynamic keys from pre-existing literal slots. Distinct templates
        // resolving to the same name identify the same dynamic slot.
        Ok(format!(
            "{CLIPBOARD_PREFIX}{}",
            hex::encode(Sha256::digest(expanded.as_bytes()))
        ))
    }
}

fn local_absolute_path(value: &str) -> bool {
    use std::path::{Component, Path};
    let path = Path::new(value);
    if value.len() > 4096
        || value.contains('\0')
        || !path.is_absolute()
        || value.starts_with("//")
        || path.components().any(|part| part == Component::ParentDir)
    {
        return false;
    }
    #[cfg(windows)]
    if !matches!(
        path.components().next(),
        Some(Component::Prefix(part)) if matches!(part.kind(), std::path::Prefix::Disk(_))
    ) {
        return false;
    }
    true
}
