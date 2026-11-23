use super::*;
use crate::{export_review::ExportReviewRequest, workspace::Edit};
use serde_json::json;
use std::io::{Read, Write};

// Construct a saved-macro call fixture without arguments
fn call(id: &str) -> Action {
    Action::Builtin {
        action: Box::new(BuiltinAction::SavedMacro {
            arguments: Default::default(),
            macro_id: id.into(),
        }),
    }
}
// Build a named saved-macro fixture around the supplied action
fn saved(id: &str, action: Action) -> SavedMacro {
    SavedMacro {
        inputs: Default::default(),
        id: id.into(),
        name: id.into(),
        action,
    }
}
// Return the underlying tests source
fn source(marker: &Path) -> ProfileData {
    ProfileData {
        saved_macros: vec![
            saved(
                "Root",
                Action::Macro {
                    actions: vec![
                        Action::Toggle {
                            on: Box::new(call("Child")),
                            off: Box::new(Action::None),
                        },
                        Action::Command {
                            program: "/usr/bin/touch".into(),
                            args: vec![marker.display().to_string()],
                        },
                    ],
                },
            ),
            saved(
                "Child",
                Action::Macro {
                    actions: vec![Action::Plugin {
                        user_desired_state: None,
                        plugin_id: "missing.plugin".into(),
                        action_id: "missing.action".into(),
                        settings: json!({"token":"secret-token", "password":"secret-password", "value":"${parameter.name}"}),
                        resources: Default::default(),
                    }],
                },
            ),
            saved("Unrelated", Action::None),
        ],
        ..Default::default()
    }
}
// Construct the tests archive
fn archive(path: &Path, data: &ProfileData) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(data).unwrap()).unwrap();
    zip.finish().unwrap();
}
// Serialize a test value into JSON for fixture comparisons
fn value(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap()
}
// Collect plugin settings nested inside the supplied saved macros
fn plugin_settings(data: &[SavedMacro]) -> Vec<serde_json::Value> {
    let mut settings = vec![];
    for macro_ in data {
        macro_.action.visit(&mut |action| {
            if let Action::Plugin { settings: v, .. } = action {
                settings.push(v.clone());
            }
        });
    }
    settings
}

#[test]
fn portable_vault_tokens_require_relinking_even_on_the_original_installation() {
    let root = tempfile::tempdir().unwrap();
    let store = profiles::ProfileStore::open(root.path().join("local")).unwrap();
    let vault = uuid::Uuid::new_v4();
    let entry = uuid::Uuid::new_v4();
    let token = format!("${{vault.{vault}.{entry}}}");
    let text = format!("before {token} and {token} after ${{run.answer}}");
    let action = Action::Builtin {
        action: Box::new(BuiltinAction::TypeText {
            text: text.clone(),
            template: true,
        }),
    };
    let mut original = ProfileData::default();
    original.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = action.clone();
    original.saved_macros.push(saved("portable", action));
    store.save(&original).unwrap();
    let path = root.path().join("portable.zip");
    store.export(&path).unwrap();
    let (_, document) = profiles::open_archive(&path, "profiles.json").unwrap();
    let placeholder = "before ${vault.unlinked} and ${vault.unlinked} after ${run.answer}";
    assert!(document.contains(placeholder));
    assert!(!document.contains(&vault.to_string()));
    assert!(!document.contains(&entry.to_string()));
    assert!(value(&store.load().unwrap()).to_string().contains(&text));

    // Even an archive written outside Manager is detached at import.
    archive(&path, &original);
    let imported = store.import_for_repair(&path).unwrap();
    assert!(!value(&imported).to_string().contains(&token));
    assert!(value(&imported.saved_macros)
        .to_string()
        .contains(placeholder));
    let runtime = Runtime::build(root.path().join("independent"), false).unwrap();
    let review = runtime.prepare_macro_import(&path).unwrap();
    let snapshot = runtime
        .commit_macro_import(&review.id, review.expected_revision, false)
        .unwrap();
    assert!(value(&snapshot.saved_macros)
        .to_string()
        .contains(placeholder));
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets, never HID"]
// Verify that export is macro only includes dependencies and redacts nested settings by default
fn export_is_macro_only_includes_dependencies_and_redacts_nested_settings_by_default() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let marker = root.path().join("must-not-execute");
    let original = source(&marker);
    runtime.profiles.save(&original).unwrap();
    let path = root.path().join("macros.zip");
    for include in [false, true] {
        let review = runtime
            .prepare_export_review(ExportReviewRequest {
                kind: "macros".into(),
                ids: vec!["Root".into()],
                include_plugin_settings: include,
            })
            .unwrap();
        runtime
            .write_export_transfer(
                &review.id,
                review.revision,
                &review.archive_sha256,
                &path,
                include.then_some("macro transfer password"),
            )
            .unwrap();
        let (mut zip, document) = crate::importing::read_transfer(
            &path,
            include.then_some("macro transfer password"),
            |p| profiles::open_archive(p, "profiles.json"),
        )
        .unwrap();
        let data = profiles::parse_data(serde_json::from_str(&document).unwrap()).unwrap();
        assert_eq!(
            data.saved_macros
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["Root", "Child"]
        );
        assert!(data.profiles.is_empty() && data.styles.is_empty() && data.rules.is_empty());
        assert!(data.active_profile.is_empty());
        assert_eq!(
            value(&data.history),
            value(&crate::workspace::HistoryIndex::default())
        );
        assert_eq!(
            plugin_settings(&data.saved_macros)[0]
                .get("token")
                .is_some(),
            include
        );
        if !include {
            assert_eq!(
                plugin_settings(&data.saved_macros),
                vec![json!({"value":"${parameter.name}"})]
            );
        }
        assert_eq!(zip.len(), 3); // document, privacy notice and portability report
        let mut notice = String::new();
        zip.by_name("export-notes.txt")
            .unwrap()
            .read_to_string(&mut notice)
            .unwrap();
        assert!(notice.contains("passwords") && notice.contains("referenced macro"));
    }
    assert_eq!(value(&runtime.profiles.load().unwrap()), value(&original));
    for ids in [
        vec![],
        vec!["missing".into()],
        vec!["Root".into(), "Root".into()],
    ] {
        assert!(runtime
            .prepare_export_review(ExportReviewRequest {
                kind: "macros".into(),
                ids,
                include_plugin_settings: false,
            })
            .is_err());
    }
    assert!(!marker.exists());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets, never HID"]
// Verify that import is reviewed copied remapped redacted and one undoable edit
fn import_is_reviewed_copied_remapped_redacted_and_one_undoable_edit() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let marker = root.path().join("must-not-execute");
    let mut data = source(&marker);
    data.saved_macros.pop();
    let path = root.path().join("shared-profile.zip");
    archive(&path, &data); // profile ZIPs can supply macros without importing profiles
    let before = runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Existing macro",
            vec![Edit::SaveMacro {
                value: saved("Root", Action::None),
            }],
        )
        .unwrap();
    let preview = runtime.prepare_macro_import(&path).unwrap();
    assert_eq!(preview.macros[0].imported_name, "Root (imported)");
    assert!(
        preview.includes_plugin_settings
            && preview
                .warnings
                .iter()
                .any(|w| w.contains("missing.plugin"))
    );
    assert_eq!(
        value(&runtime.snapshot().saved_macros),
        value(&before.saved_macros)
    );
    let after = runtime
        .commit_macro_import(&preview.id, preview.expected_revision, false)
        .unwrap();
    assert_eq!(value(&after.profiles), value(&before.profiles));
    assert_eq!(value(&after.sessions), value(&before.sessions));
    assert_eq!(after.saved_macros.len(), 3);
    assert_eq!(
        value(&after.saved_macros[0]),
        value(&before.saved_macros[0])
    );
    assert!(after.saved_macros[1..]
        .iter()
        .all(|m| !["Root", "Child"].contains(&m.id.as_str())));
    assert_eq!(
        referenced_macros(&after.saved_macros[1].action),
        HashSet::from([after.saved_macros[2].id.clone()])
    );
    assert_eq!(
        plugin_settings(&after.saved_macros),
        vec![json!({"value":"${parameter.name}"})]
    );
    assert!(runtime
        .commit_macro_import(&preview.id, after.configuration_revision, true)
        .is_err());
    let undone = runtime
        .history_action("undo", after.configuration_revision)
        .unwrap();
    assert_eq!(value(&undone.saved_macros), value(&before.saved_macros));
    let redone = runtime
        .history_action("redo", undone.configuration_revision)
        .unwrap();
    assert_eq!(value(&redone.saved_macros), value(&after.saved_macros));
    assert_eq!(
        value(&runtime.profiles.load().unwrap().saved_macros),
        value(&after.saved_macros)
    );
    assert!(!marker.exists());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets, never HID"]
// Verify that stale import can be rechecked settings require opt in and cancel and expiry remove access
fn stale_import_can_be_rechecked_settings_require_opt_in_and_cancel_and_expiry_remove_access() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("macros.zip");
    archive(&path, &source(&root.path().join("never-run")));
    let preview = runtime.prepare_macro_import(&path).unwrap();
    let changed = runtime
        .apply_edits(
            preview.expected_revision,
            "Concurrent macro",
            vec![Edit::SaveMacro {
                value: saved("Child", Action::None),
            }],
        )
        .unwrap();
    assert!(runtime
        .commit_macro_import(&preview.id, preview.expected_revision, true)
        .unwrap_err()
        .contains("Recheck"));
    assert_eq!(runtime.snapshot().saved_macros.len(), 1);
    let preview = runtime.recheck_macro_import(&preview.id).unwrap();
    assert_eq!(preview.expected_revision, changed.configuration_revision);
    assert_eq!(preview.macros[1].imported_name, "Child (imported)");
    let after = runtime
        .commit_macro_import(&preview.id, preview.expected_revision, true)
        .unwrap();
    assert_eq!(
        plugin_settings(&after.saved_macros)[0]["password"],
        "secret-password"
    );
    let stage = runtime.prepare_macro_import(&path).unwrap();
    runtime.cancel_macro_import(&stage.id);
    assert!(runtime.recheck_macro_import(&stage.id).is_err());
    let stage = runtime.prepare_macro_import(&path).unwrap();
    runtime
        .macro_imports
        .lock()
        .unwrap()
        .get_mut(&stage.id)
        .unwrap()
        .created = Instant::now() - LIFETIME;
    assert!(runtime
        .commit_macro_import(&stage.id, stage.expected_revision, false)
        .unwrap_err()
        .contains("expired"));
    runtime.shutdown();
}

#[test]
// Verify that macro archives reject unsupported versions missing references cycles and duplicate IDs
fn macro_archives_reject_unsupported_versions_missing_references_cycles_and_duplicate_ids() {
    let valid = macro_only(source(Path::new("/not-executed")));
    validate_macros(&valid).unwrap();
    let mut invalid = valid.clone();
    invalid.schema_version = 999;
    assert!(validate_macros(&invalid).is_err());
    let mut invalid = valid.clone();
    invalid.minimum_manager = Some("999.0.0".into());
    assert!(validate_macros(&invalid).is_err());
    let mut invalid = valid.clone();
    invalid.saved_macros.clear();
    assert!(validate_macros(&invalid).is_err());
    let mut invalid = valid.clone();
    invalid.saved_macros.remove(1);
    assert!(validate_macros(&invalid)
        .unwrap_err()
        .contains("missing saved macro"));
    let mut invalid = valid.clone();
    invalid.saved_macros.push(invalid.saved_macros[0].clone());
    assert!(validate_macros(&invalid).unwrap_err().contains("duplicate"));
    let mut invalid = valid.clone();
    invalid.saved_macros[1].action = call("Root");
    assert!(validate_macros(&invalid).unwrap_err().contains("cycle"));
    let mut invalid = valid.clone();
    invalid.saved_macros[0].name = " ".into();
    assert!(validate_macros(&invalid).is_err());
    let mut invalid = valid.clone();
    invalid.saved_macros[0].name = "x".repeat(641);
    assert!(validate_macros(&invalid).is_err());
    let mut invalid = valid;
    invalid.saved_macros = (0..1001)
        .map(|n| saved(&n.to_string(), Action::None))
        .collect();
    assert!(validate_macros(&invalid).is_err());
}

#[test]
// Verify that repeated shared macro graphs are bounded and still check expansion depth and cycles
fn repeated_shared_macro_graphs_are_bounded_and_still_check_expansion_depth_and_cycles() {
    let mut macros = vec![saved("0", Action::None)];
    for n in 1..5 {
        macros.push(saved(
            &n.to_string(),
            Action::Macro {
                actions: vec![call(&(n - 1).to_string()); 50],
            },
        ));
    }
    // Without memoization this expands over six million repeated references
    crate::builtins::validate_macro_graph(&macros).unwrap();
    macros.push(saved(
        "5",
        Action::Macro {
            actions: vec![call("4")],
        },
    ));
    assert!(crate::builtins::validate_macro_graph(&macros)
        .unwrap_err()
        .contains("nesting"));
    macros.pop();
    macros[0].action = call("4");
    assert!(crate::builtins::validate_macro_graph(&macros).is_err());
}

#[test]
#[ignore = "Requires local loopback sockets, never HID"]
// Verify that staging rejects bad archives without mutating or extracting and limits live sessions
fn staging_rejects_bad_archives_without_mutating_or_extracting_and_limits_live_sessions() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("untrusted.zip");
    let before = value(&runtime.profiles.load().unwrap());
    for (name, document) in [
        ("profiles.json", "invalid JSON".to_string()),
        ("missing.json", "{}".to_string()),
        (
            "../profiles.json",
            serde_json::to_string(&source(Path::new("/not-run"))).unwrap(),
        ),
        ("profiles.json", " ".repeat(16 * 1024 * 1024 + 1)),
    ] {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        zip.finish().unwrap();
        assert!(runtime.prepare_macro_import(&path).is_err());
        assert!(runtime.macro_imports.lock().unwrap().is_empty());
        assert_eq!(value(&runtime.profiles.load().unwrap()), before);
    }
    archive(&path, &source(Path::new("/not-run")));
    let mut stages = vec![];
    for _ in 0..8 {
        stages.push(runtime.prepare_macro_import(&path).unwrap());
    }
    assert!(runtime
        .prepare_macro_import(&path)
        .unwrap_err()
        .contains("Close an existing"));
    runtime.cancel_macro_import(&stages[0].id);
    assert!(runtime.prepare_macro_import(&path).is_ok());
    assert_eq!(value(&runtime.profiles.load().unwrap()), before);
    runtime.shutdown();
}

#[test]
// Verify that resource files follow macro review cancel redaction and commit
fn resource_files_follow_macro_review_cancel_redaction_and_commit() {
    let root = tempfile::tempdir().unwrap();
    let source_store = profiles::ProfileStore::open(root.path().join("source")).unwrap();
    let source = root.path().join("document.txt");
    std::fs::write(&source, b"macro resource").unwrap();
    let resources = crate::vendor_resources::stage(
        &source_store.root,
        &std::collections::BTreeMap::from([("file".into(), source.to_string_lossy().into_owned())]),
    )
    .unwrap();
    let data = ProfileData {
        saved_macros: vec![saved(
            "resource",
            Action::Plugin {
                user_desired_state: None,
                plugin_id: "example".into(),
                action_id: "example.run".into(),
                settings: json!({}),
                resources: resources.clone(),
            },
        )],
        ..Default::default()
    };
    let file = root.path().join("macro.zip");
    source_store.export_archive(&file, data, true).unwrap();
    let runtime = Runtime::build(root.path().join("target"), false).unwrap();
    let preview = runtime.prepare_macro_import(&file).unwrap();
    assert!(preview.includes_plugin_settings);
    assert!(!runtime.profiles.root.join("plugin-resources").exists());
    runtime.cancel_macro_import(&preview.id);
    assert!(!runtime.profiles.root.join("plugin-resources").exists());
    let preview = runtime.prepare_macro_import(&file).unwrap();
    runtime
        .commit_macro_import(&preview.id, preview.expected_revision, false)
        .unwrap();
    assert!(!runtime.profiles.root.join("plugin-resources").exists());
    let preview = runtime.prepare_macro_import(&file).unwrap();
    let snapshot = runtime
        .commit_macro_import(&preview.id, preview.expected_revision, true)
        .unwrap();
    assert!(snapshot.saved_macros.iter().any(|m| matches!(&m.action, Action::Plugin { resources: retained, .. } if retained == &resources)));
    let paths = crate::vendor_resources::paths(&runtime.profiles.root, &resources).unwrap();
    assert_eq!(std::fs::read(&paths["file"]).unwrap(), b"macro resource");
    runtime.shutdown();
}
