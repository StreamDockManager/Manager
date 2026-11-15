//! Deliberately scoped saved-macro repairs. Never executes actions or grants plugin access
use super::*;

// List saved macros as independently repairable recovery records
pub(super) fn inventory(conn: &Connection) -> Result<Vec<Record>, String> {
    Ok(
        stream_dock_manager_storage::list_json::<Value>(conn, Domain::SavedMacros, 4096, LIMIT)?
            .into_iter()
            .map(|(key, value)| Record {
                id: format!("macro:{key}"),
                key,
                kind: "macro",
                value,
            })
            .collect(),
    )
}

// Validate a repaired macro’s identity, name, input definitions and action contract
pub(super) fn issues(record: &Record, value: &Value) -> Vec<String> {
    let validate = || -> Result<(), String> {
        let saved: crate::builtins::SavedMacro =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        if saved.id != record.key || saved.id.is_empty() {
            return Err("$.id: saved macro identity cannot be changed".into());
        }
        if saved.name.trim().is_empty() {
            return Err("$.name: macro name must not be empty".into());
        }
        saved.action.validate(0)?;
        crate::macro_inputs::validate_definitions(&saved.inputs)
    };
    validate().err().into_iter().collect()
}

// Report the references condition for saved items
fn references(value: &Value, id: &str) -> bool {
    match value {
        // Conservative even for malformed actions: never leave a known reference dangling
        Value::Object(map) => {
            map.get("macro_id").and_then(Value::as_str) == Some(id)
                || map.values().any(|v| references(v, id))
        }
        Value::Array(values) => values.iter().any(|v| references(v, id)),
        _ => false,
    }
}

// Apply the reviewed saved items change
pub(super) fn apply(
    conn: &Connection,
    record: &Record,
    value: Option<&Value>,
) -> Result<(), String> {
    if let Some(value) = value {
        let failures = issues(record, value);
        if !failures.is_empty() {
            return Err(failures.join("\n"));
        }
        write(conn, Domain::SavedMacros, &record.key, value)?;
        // Validate the macro graph and all readable callers against the updated signature
        // Unrelated malformed profiles remain available for a separate repair
        let macros = inventory(conn)?
            .into_iter()
            .map(|row| {
                serde_json::from_value::<crate::builtins::SavedMacro>(row.value).map_err(|e| {
                    format!("Repair malformed saved macros before validating references: {e}")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for saved in &macros {
            crate::macro_inputs::validate_action_calls(&saved.action, &macros, &saved.inputs)?;
            crate::keyboard_macro::validation::expanded_event_count(&saved.action, |id| {
                macros.iter().find(|m| m.id == id).map(|m| &m.action)
            })?;
        }
        for (_, value) in
            stream_dock_manager_storage::list_json::<Value>(conn, Domain::Profiles, 4096, LIMIT)?
        {
            if let Ok(profile) = serde_json::from_value::<crate::profiles::Profile>(value) {
                for binding in profile.pages.iter().flat_map(|p| p.bindings.values()) {
                    crate::macro_inputs::validate_action_calls(&binding.action, &macros, &[])?;
                    crate::keyboard_macro::validation::expanded_event_count(
                        &binding.action,
                        |id| macros.iter().find(|m| m.id == id).map(|m| &m.action),
                    )?;
                }
            }
        }
    } else {
        for domain in [Domain::Profiles, Domain::SavedMacros] {
            for (key, value) in
                stream_dock_manager_storage::list_json::<Value>(conn, domain, 4096, LIMIT)?
            {
                if domain == Domain::SavedMacros && key == record.key {
                    continue;
                }
                if references(&value, &record.key) {
                    return Err(
                        "This macro is still referenced. Repair its callers before deleting it."
                            .into(),
                    );
                }
            }
        }
        if read(conn, Domain::LocalPreferences, "drafts")?
            .as_ref()
            .is_some_and(|v| references(v, &record.key))
        {
            return Err(
                "This macro is referenced by a saved draft. Repair or delete the draft first."
                    .into(),
            );
        }
        stream_dock_manager_storage::delete(conn, Domain::SavedMacros, &record.key)?;
    }
    let mut header =
        read(conn, Domain::Workspace, "current")?.ok_or("Workspace metadata is missing")?;
    let revision = header["document"]["configuration_revision"]
        .as_u64()
        .ok_or("Invalid configuration revision")?;
    header["document"]["configuration_revision"] = Value::from(
        revision
            .checked_add(1)
            .ok_or("Configuration revision exhausted")?,
    );
    write(conn, Domain::Workspace, "current", &header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::Storage;
    // Construct an isolated saved items fixture
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().into()).unwrap();
        store
            .transaction(|conn| {
                crate::profile_database::save(conn, &crate::profiles::ProfileData::default())
            })
            .unwrap();
        let saved = crate::builtins::SavedMacro {
            id: "example".into(),
            name: "Example".into(),
            inputs: vec![],
            action: crate::actions::Action::Builtin {
                action: Box::new(crate::builtins::BuiltinAction::TypeText {
                    text: "hello".into(),
                    template: false,
                }),
            },
        };
        store
            .write_json(Domain::SavedMacros, "example", &saved, LIMIT)
            .unwrap();
        root
    }
    #[test]
    // Verify that boolean setting repairs through revision guarded public API
    fn boolean_setting_repairs_through_revision_guarded_public_api() {
        let root = fixture();
        {
            let store = Storage::open(root.path().into()).unwrap();
            store
                .write_json(
                    Domain::ManagerPreferences,
                    "offline_icons",
                    &serde_json::json!("invalid"),
                    LIMIT,
                )
                .unwrap();
        }
        let recovery = ConfigurationRecovery::open(root.path().into(), "test".into()).unwrap();
        let doc = recovery
            .document("setting:manager_preferences:offline_icons")
            .unwrap();
        assert!(!doc.issues.is_empty());
        assert!(
            recovery
                .validate(&doc.id, &doc.revision, "true")
                .unwrap()
                .valid
        );
        assert_eq!(recovery.document(&doc.id).unwrap().json, doc.json);
        let report = recovery.save(&doc.id, &doc.revision, "true").unwrap();
        assert!(report.last_backup.is_some());
        assert!(recovery.save(&doc.id, &doc.revision, "false").is_err());
        let doc = recovery.document(&doc.id).unwrap();
        assert_eq!(doc.json, "true");
        recovery.delete(&doc.id, &doc.revision).unwrap();
        assert!(recovery.document(&doc.id).is_err());
    }
    #[test]
    // Verify that macro validation is nonmutating identity locked and save revision guarded
    fn macro_validation_is_nonmutating_identity_locked_and_save_revision_guarded() {
        let root = fixture();
        let recovery = ConfigurationRecovery::open(root.path().into(), "test".into()).unwrap();
        let doc = recovery.document("macro:example").unwrap();
        let mut value: Value = serde_json::from_str(&doc.json).unwrap();
        value["id"] = "other".into();
        assert!(
            !recovery
                .validate(&doc.id, &doc.revision, &value.to_string())
                .unwrap()
                .valid
        );
        value["id"] = "example".into();
        value["name"] = "Repaired".into();
        assert!(
            recovery
                .validate(&doc.id, &doc.revision, &value.to_string())
                .unwrap()
                .valid
        );
        assert_eq!(recovery.document(&doc.id).unwrap().json, doc.json);
        let report = recovery
            .save(&doc.id, &doc.revision, &value.to_string())
            .unwrap();
        assert!(report.last_backup.is_some());
        assert!(recovery
            .save(&doc.id, &doc.revision, &value.to_string())
            .is_err());
        assert!(recovery
            .document(&doc.id)
            .unwrap()
            .json
            .contains("Repaired"));
    }
    #[test]
    // Verify that recursive macro repair is rejected without mutating saved record
    fn recursive_macro_repair_is_rejected_without_mutating_saved_record() {
        let root = fixture();
        let recovery = ConfigurationRecovery::open(root.path().into(), "test".into()).unwrap();
        let doc = recovery.document("macro:example").unwrap();
        let mut saved: crate::builtins::SavedMacro = serde_json::from_str(&doc.json).unwrap();
        saved.action = crate::actions::Action::Builtin {
            action: Box::new(crate::builtins::BuiltinAction::SavedMacro {
                macro_id: saved.id.clone(),
                arguments: Default::default(),
            }),
        };
        let json = serde_json::to_string(&saved).unwrap();
        assert!(recovery.save(&doc.id, &doc.revision, &json).is_err());
        assert_eq!(recovery.document(&doc.id).unwrap().json, doc.json);
    }
    #[test]
    // Verify that macro delete rejects draft references and unreferenced delete is backed up
    fn macro_delete_rejects_draft_references_and_unreferenced_delete_is_backed_up() {
        let root = fixture();
        {
            let store = Storage::open(root.path().into()).unwrap();
            store.write_json(Domain::LocalPreferences, "drafts", &serde_json::json!({"draft":{"key":"k","base":null,"value":{"macro_id":"example"}}}), LIMIT).unwrap();
        }
        let recovery = ConfigurationRecovery::open(root.path().into(), "test".into()).unwrap();
        let doc = recovery.document("macro:example").unwrap();
        assert!(recovery
            .delete(&doc.id, &doc.revision)
            .unwrap_err()
            .contains("draft"));
        assert!(recovery.document(&doc.id).is_ok());
        let draft = recovery.document("draft:draft").unwrap();
        recovery.delete(&draft.id, &draft.revision).unwrap();
        let doc = recovery.document("macro:example").unwrap();
        let report = recovery.delete(&doc.id, &doc.revision).unwrap();
        assert!(report.last_backup.is_some());
        assert!(recovery.document(&doc.id).is_err());
    }
}
