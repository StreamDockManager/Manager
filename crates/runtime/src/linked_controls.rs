use crate::profiles::{Binding, LinkMember, Profile};
use std::collections::BTreeSet;

impl Profile {
    pub(super) fn prune_links(&mut self, mut keep: impl FnMut(&LinkMember) -> bool) {
        self.linked_controls.retain(|_, members| {
            members.retain(&mut keep);
            members.len() >= 2
        });
    }
    fn member_binding(&self, m: &LinkMember) -> Option<&Binding> {
        self.pages
            .iter()
            .find(|p| p.id == m.page_id)?
            .bindings
            .get(&m.control_id)
    }
    pub(super) fn validate_links(&self) -> Result<(), String> {
        if self.linked_controls.is_empty() {
            return Ok(());
        }
        let models = hardware_registry::manifests();
        let model = models
            .iter()
            .find(|m| m.model_id == self.model_id)
            .ok_or("Unknown linked-control model")?;
        let mut used = BTreeSet::new();
        for (id, members) in &self.linked_controls {
            if uuid::Uuid::parse_str(id).is_err() || !(2..=256).contains(&members.len()) {
                return Err("Linked controls need a stable group ID and 2–256 members.".into());
            }
            let first = &members[0];
            let reference = self
                .member_binding(first)
                .ok_or("Missing linked source assignment")?;
            let source = model
                .control(&first.control_id)
                .map_err(|e| e.to_string())?;
            let expected = serde_json::to_value(reference).map_err(|e| e.to_string())?;
            if contains_credential(&expected) {
                return Err("Detach this binding before assigning a local credential.".into());
            }
            for member in members {
                let control = model
                    .control(&member.control_id)
                    .map_err(|e| e.to_string())?;
                if !used.insert(member)
                    || crate::keyboard_navigation::protected_control(
                        &self.model_id,
                        &member.page_id,
                        &member.control_id,
                    )
                    || source.kind != control.kind
                    || source.semantics != control.semantics
                {
                    return Err(
                        "Linked members must be distinct compatible editable controls.".into(),
                    );
                }
                if Some(&expected)
                    != self
                        .member_binding(member)
                        .map(serde_json::to_value)
                        .transpose()
                        .map_err(|e| e.to_string())?
                        .as_ref()
                {
                    return Err(format!("Linked assignment differs at {} / {}. Detach it or apply the source assignment.", member.page_id, member.control_id));
                }
            }
        }
        Ok(())
    }
    pub(super) fn sync_link(&mut self, page: &str, control: &str) -> Result<(), String> {
        let Some(group) = self.linked_controls.values().find(|g| {
            g.iter()
                .any(|m| m.page_id == page && m.control_id == control)
        }) else {
            return Ok(());
        };
        let value = self
            .pages
            .iter()
            .find(|p| p.id == page)
            .and_then(|p| p.bindings.get(control))
            .cloned()
            .ok_or("Linked source missing")?;
        for m in group {
            self.pages
                .iter_mut()
                .find(|p| p.id == m.page_id)
                .ok_or("Linked page missing")?
                .bindings
                .insert(m.control_id.clone(), value.clone());
        }
        Ok(())
    }
    pub(super) fn link(&mut self, source: LinkMember, target: LinkMember) -> Result<(), String> {
        if source.page_id == target.page_id
            || self.linked_controls.values().any(|g| g.contains(&target))
        {
            return Err("Choose an independent control on another page.".into());
        }
        if self.member_binding(&source).is_none() {
            return Err("Save the source binding before linking.".into());
        }
        let id = self
            .linked_controls
            .iter()
            .find(|(_, g)| g.contains(&source))
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.linked_controls
            .entry(id)
            .or_insert_with(|| vec![source.clone()])
            .push(target);
        self.sync_link(&source.page_id, &source.control_id)?;
        self.validate_links()
    }
}

fn contains_credential(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("$credential") || map.values().any(contains_credential)
        }
        serde_json::Value::Array(list) => list.iter().any(contains_credential),
        _ => false,
    }
}
