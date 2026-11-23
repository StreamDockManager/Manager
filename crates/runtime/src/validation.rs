//! Revision-bound edit validation; unchanged entities retain their previous proof
use crate::ProfileData;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
struct Index {
    hashes: BTreeMap<String, String>,
    incoming: BTreeMap<String, BTreeSet<String>>,
    document_entities_bytes: usize,
}
// Hash the validation value
fn hash(value: &impl serde::Serialize) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )))
}
impl Index {
    // Add an entry to validation
    fn add(&mut self, id: String, value: &impl serde::Serialize) -> Result<usize, String> {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let len = bytes.len();
        self.hashes
            .insert(id.clone(), hex::encode(Sha256::digest(bytes)));
        let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
        // Scan the available validation entries
        fn scan(index: &mut Index, source: &str, value: &Value) {
            match value {
                Value::Object(map) => {
                    for (key, value) in map {
                        let prefix = match key.as_str() {
                            "profile_id" => Some("profile"),
                            "page_id" => Some("page"),
                            "macro_id" => Some("macro"),
                            "plugin_id" => Some("plugin"),
                            "asset" | "background_asset" => Some("media"),
                            "variable" | "variable_name" => Some("variable"),
                            _ => None,
                        };
                        if let (Some(prefix), Some(target)) = (prefix, value.as_str()) {
                            index
                                .incoming
                                .entry(format!("{prefix}:{target}"))
                                .or_default()
                                .insert(source.into());
                        }
                        scan(index, source, value)
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        scan(index, source, value)
                    }
                }
                _ => {}
            }
        }
        scan(self, &id, &value);
        Ok(len)
    }
    // Build validation from the configured inputs
    fn build(data: &ProfileData) -> Result<Self, String> {
        let mut index = Self::default();
        for p in &data.profiles {
            index.document_entities_bytes += index.add(format!("profile:{}", p.id), p)?;
            for page in &p.pages {
                index.add(format!("page:{}", page.id), page)?;
                index
                    .incoming
                    .entry(format!("page:{}", page.id))
                    .or_default()
                    .insert(format!("profile:{}", p.id));
            }
        }
        for value in &data.saved_macros {
            index.document_entities_bytes += index.add(format!("macro:{}", value.id), value)?;
        }
        for value in &data.styles {
            index.document_entities_bytes += index.add(format!("style:{}", value.id), value)?;
        }
        for value in &data.rules {
            index.document_entities_bytes += index.add(format!("rule:{}", value.id), value)?;
        }
        // Empty-array brackets belong to the header; only inter-item commas are extra
        index.document_entities_bytes += [
            data.profiles.len(),
            data.saved_macros.len(),
            data.styles.len(),
            data.rules.len(),
        ]
        .into_iter()
        .map(|len| len.saturating_sub(1))
        .sum::<usize>();
        Ok(index)
    }
}
pub struct EditValidation {
    pub revision: u64,
    pub affected: BTreeSet<String>,
    pub(crate) document_entities_bytes: usize,
    pub(crate) changed: BTreeSet<String>,
}
impl EditValidation {
    // Report whether current
    pub fn is_current(&self, revision: u64) -> bool {
        self.revision == revision
    }
}
// Reject duplicate workspace identities in the supplied sequence
fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err("Duplicate workspace identity".into());
        }
    }
    Ok(())
}
// Validate edit
pub fn validate_edit(before: &ProfileData, after: &ProfileData) -> Result<EditValidation, String> {
    unique(after.profiles.iter().map(|p| p.id.as_str()))?;
    unique(after.saved_macros.iter().map(|v| v.id.as_str()))?;
    unique(after.styles.iter().map(|v| v.id.as_str()))?;
    unique(after.rules.iter().map(|v| v.id.as_str()))?;
    if after.rules.len() > 100 || after.styles.len() > 1000 {
        return Err("Workspace entity limit exceeded".into());
    }
    let old = Index::build(before)?;
    let next = Index::build(after)?;
    let changed: BTreeSet<_> = old
        .hashes
        .keys()
        .chain(next.hashes.keys())
        .filter(|key| old.hashes.get(*key) != next.hashes.get(*key))
        .cloned()
        .collect();
    let mut affected = changed.clone();
    let mut queue: Vec<_> = affected.iter().cloned().collect();
    while let Some(entity) = queue.pop() {
        for source in old
            .incoming
            .get(&entity)
            .into_iter()
            .flatten()
            .chain(next.incoming.get(&entity).into_iter().flatten())
        {
            if affected.insert(source.clone()) {
                queue.push(source.clone())
            }
        }
    }
    let mut partial = after.clone();
    partial.profiles.retain(|p| {
        p.id == after.active_profile || affected.contains(&format!("profile:{}", p.id))
    });
    partial
        .saved_macros
        .retain(|v| affected.contains(&format!("macro:{}", v.id)));
    partial
        .styles
        .retain(|v| affected.contains(&format!("style:{}", v.id)));
    partial
        .rules
        .retain(|v| affected.contains(&format!("rule:{}", v.id)));
    crate::profiles::validate_data(&partial)?;
    Ok(EditValidation {
        revision: after.configuration_revision,
        affected,
        document_entities_bytes: next.document_entities_bytes,
        changed,
    })
}
impl crate::ProfileStore {
    // Check whether this exact profile-data hash already has a retained validation proof
    pub(crate) fn proven(&self, data: &ProfileData) -> Result<bool, String> {
        let hash = hash(data)?;
        Ok(self.validation_proofs.lock().unwrap().contains(&hash))
    }
    // Remember successful validation by content hash while retaining at most sixteen proofs
    pub(crate) fn remember_validation(&self, data: &ProfileData) -> Result<(), String> {
        let hash = hash(data)?;
        let mut proofs = self.validation_proofs.lock().unwrap();
        if !proofs.contains(&hash) {
            proofs.push_back(hash)
        }
        while proofs.len() > 16 {
            proofs.pop_front();
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that affected edits match full validation and results are revision bound
    fn affected_edits_match_full_validation_and_results_are_revision_bound() {
        let before = ProfileData::default();
        let mut after = before.clone();
        after.profiles[0].name = "Changed".into();
        let result = validate_edit(&before, &after).unwrap();
        crate::profiles::validate_data(&after).unwrap();
        assert!(result.affected.contains("profile:default"));
        assert!(result.is_current(after.configuration_revision));
        assert!(!result.is_current(after.configuration_revision + 1));
        after.profiles[0].name = String::new();
        assert!(validate_edit(&before, &after).is_err());
        assert!(crate::profiles::validate_data(&after).is_err());
    }
}
