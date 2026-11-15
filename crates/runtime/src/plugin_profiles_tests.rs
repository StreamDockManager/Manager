use super::*;
use serde_json::json;
// Construct an isolated plugin profiles tests fixture
fn fixture() -> (tempfile::TempDir, std::sync::Arc<Runtime>, String) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("org.test.suggestions.sdPlugin");
    std::fs::create_dir_all(source.join("profiles")).unwrap();
    std::fs::write(source.join("index.html"), "fixture").unwrap();
    std::fs::write(source.join("manifest.json"),json!({"UUID":"org.test.suggestions","Name":"Suggestions","Version":"0.1.0","SDKVersion":1,"CodePath":"index.html","Actions":[{"UUID":"org.test.suggestions.run","Name":"Run","Controllers":["Keypad","Encoder"]}]}).to_string()).unwrap();
    crate::test_plugin::modernize(&source);
    let document = json!({"schema_version":1,"configuration_revision":0,"saved_macros":[],"rules":[],"history":crate::workspace::HistoryIndex::default(),"active_profile":"suggestion","profiles":[{"id":"suggestion","name":"Suggested","model_id":"n4-pro","active_page":"page","pages":[{"id":"page","name":"Suggested page","bindings":{"key-1":{"title":"Run suggestion","color":"#00000000","action":{"kind":"plugin","plugin_id":"org.test.suggestions","action_id":"org.test.suggestions.run","settings":{}}},"key-2":{"title":"Navigate","color":"#00000000","action":{"kind":"page","page_id":"page"}}}}]}],"plugin_configuration":{"settings":{"nested":{"endpoint":"example"}},"action_defaults":{"org.test.suggestions.run":{"step":2}}}});
    std::fs::write(
        source.join("profiles/n4-pro.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let runtime = Runtime::build(directory.path().join("manager"), false).unwrap();
    let review = runtime.prepare_plugin_package(&source).unwrap();
    assert_eq!(
        review
            .suggested_profiles
            .as_ref()
            .unwrap()
            .suggestions
            .len(),
        1
    );
    runtime
        .commit_plugin_package(&review.id, false, false)
        .unwrap();
    (directory, runtime, "org.test.suggestions".into())
}

#[test]
// Verify that pending setup skips a removed candidate and keeps installed suggestions
fn pending_setup_skips_a_removed_candidate_and_keeps_installed_suggestions() {
    let (_directory, runtime, id) = fixture();
    let catalogs = runtime
        .pending_plugin_profile_setup(vec!["org.test.removed".into(), id.clone()])
        .unwrap();
    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].plugin_id, id);
}

#[test]
// Verify that profile discovery does not count unrelated directory entries as suggestions
fn profile_discovery_does_not_count_unrelated_directory_entries_as_suggestions() {
    let names = (0..100)
        .map(|index| format!("aaa-{index:03}.txt"))
        .chain(std::iter::once("n4-pro.json".to_owned()));
    assert_eq!(
        suggestion_keys(names).keys().collect::<Vec<_>>(),
        vec!["profiles/n4-pro.json"]
    );
    let (_directory, runtime, id) = fixture();
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    let profiles = root.join("profiles");
    let document = std::fs::read(profiles.join("n4-pro.json")).unwrap();
    std::fs::remove_dir_all(&profiles).unwrap();
    std::fs::create_dir(&profiles).unwrap();
    for index in 0..100 {
        std::fs::write(
            profiles.join(format!("aaa-{index:03}.txt")),
            "Package author notes",
        )
        .unwrap();
    }
    std::fs::write(profiles.join("n4-pro.json"), document).unwrap();
    let catalog = runtime.list_plugin_profiles(&id).unwrap();
    assert_eq!(catalog.suggestions.len(), 1);
    assert!(
        catalog.suggestions[0].error.is_none(),
        "{:?}",
        catalog.suggestions[0].error
    );
}
// Prepare the plugin profiles tests operation
fn prepare(runtime: &Runtime, id: &str) -> ImportPreview {
    let catalog = runtime.list_plugin_profiles(id).unwrap();
    assert!(
        catalog
            .suggestions
            .iter()
            .all(|suggestion| suggestion.error.is_none()),
        "{:?}",
        catalog.warnings
    );
    runtime
        .prepare_plugin_profile_import(id, "profiles/n4-pro.json", None, None)
        .unwrap()
}
#[test]
// Verify that discovery cancel import and whole undo redo
fn discovery_cancel_import_and_whole_undo_redo() {
    let (_directory, runtime, id) = fixture();
    let before = runtime.snapshot();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    placement.target_model = "n4-pro".into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    assert!(report.plugin.as_ref().unwrap().ready);
    assert_eq!(runtime.snapshot().profiles.len(), before.profiles.len());
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    assert_eq!(after.profiles.len(), before.profiles.len() + 1);
    assert_eq!(
        runtime
            .plugins
            .lock()
            .unwrap()
            .configuration(&id)
            .unwrap()
            .settings["nested"]["endpoint"],
        "example"
    );
    let profile = after.profiles.last().unwrap();
    assert!(
        matches!(&profile.pages[0].bindings["key-2"].action,Action::Page{page_id} if *page_id==profile.pages[0].id)
    );
    let undone = runtime
        .history_action("undo", after.configuration_revision)
        .unwrap();
    assert_eq!(undone.profiles.len(), before.profiles.len());
    assert!(runtime
        .plugins
        .lock()
        .unwrap()
        .configuration(&id)
        .unwrap()
        .settings
        .as_object()
        .unwrap()
        .is_empty());
    let redone = runtime
        .history_action("redo", undone.configuration_revision)
        .unwrap();
    assert_eq!(redone.profiles.len(), after.profiles.len());
    let report = prepare(&runtime, &id);
    runtime.cancel_profile_import(&report.id);
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .is_err());
    runtime.shutdown();
}
#[test]
// Verify that clearing plugin data prevents history from restoring deleted settings
fn clearing_plugin_data_prevents_history_from_restoring_deleted_settings() {
    let (_directory, runtime, id) = fixture();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    placement.target_model = "n4-pro".into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let imported = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    let undone = runtime
        .history_action("undo", imported.configuration_revision)
        .unwrap();
    {
        let mut host = runtime.plugins.lock().unwrap();
        let review = host
            .stored_data()
            .unwrap()
            .into_iter()
            .find(|row| row.plugin_id == id)
            .unwrap();
        host.purge_stored_data(&id, &review.review_token).unwrap();
    }
    let error = runtime
        .history_action("redo", undone.configuration_revision)
        .unwrap_err();
    assert!(error.contains("cannot restore deleted settings"), "{error}");
    assert_eq!(
        runtime.snapshot().configuration_revision,
        undone.configuration_revision
    );
    assert_eq!(runtime.snapshot().profiles.len(), undone.profiles.len());
    let configuration = runtime.plugins.lock().unwrap().configuration(&id).unwrap();
    assert!(configuration.settings.as_object().unwrap().is_empty());
    assert!(configuration.action_defaults.is_empty());
    runtime.shutdown();
}

#[test]
// Verify that settings conflicts stale package and revision are reviewable
fn settings_conflicts_stale_package_and_revision_are_reviewable() {
    let (_directory, runtime, id) = fixture();
    let mut report = prepare(&runtime, &id);
    let mut placement = report.plugin.as_ref().unwrap().placement.clone();
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let before = runtime.plugins.lock().unwrap().configuration(&id).unwrap();
    let mut changed = before.clone();
    changed
        .settings
        .as_object_mut()
        .unwrap()
        .insert("nested".into(), json!({"endpoint":"local","token":"keep"}));
    runtime
        .plugins
        .lock()
        .unwrap()
        .configuration_transaction_sql(&id, &before, &changed, 0, |_| Ok(()))
        .unwrap();
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap_err()
        .contains("settings changed"));
    let report = runtime
        .update_plugin_profile_import(&report.id, report.plugin.unwrap().placement)
        .unwrap();
    assert!(report
        .plugin
        .as_ref()
        .unwrap()
        .configuration
        .iter()
        .any(|r| r.current == Some(json!("local")) && !r.use_suggested));
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    std::fs::write(root.join("profiles/n4-pro.json"), "invalid").unwrap();
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap_err()
        .contains("suggestions changed"));
    assert!(runtime.list_plugin_profiles(&id).unwrap().suggestions[0]
        .error
        .is_some());
    runtime.shutdown();
}
#[test]
// Verify that overlaps keep existing and duplicate destinations are blocked
fn overlaps_keep_existing_and_duplicate_destinations_are_blocked() {
    let (_directory, runtime, id) = fixture();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    placement.bindings[1].target_control = placement.bindings[0].target_control.clone();
    let invalid = runtime
        .update_plugin_profile_import(&report.id, placement.clone())
        .unwrap();
    assert!(!invalid.plugin.unwrap().ready);
    placement.bindings[1].target_control = OMIT.into();
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    let target = after.profiles.last().unwrap();
    let report = runtime
        .prepare_plugin_profile_import(
            &id,
            "profiles/n4-pro.json",
            Some(target.id.clone()),
            Some(target.pages[0].id.clone()),
        )
        .unwrap();
    assert!(report
        .plugin
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .any(|r| r.occupied && !r.replace));
    assert_eq!(
        runtime
            .commit_profile_import(&report.id, report.expected_revision)
            .unwrap()
            .profiles
            .last()
            .unwrap()
            .pages[0]
            .bindings["key-1"]
            .title,
        "Run suggestion"
    );
    runtime.shutdown();
}
#[test]
// Verify that prompts track suggestions independently of executable bytes
fn prompts_track_suggestions_independently_of_executable_bytes() {
    let (_directory, runtime, id) = fixture();
    let pending = runtime
        .pending_plugin_profile_setup(vec![id.clone()])
        .unwrap();
    assert_eq!(pending.len(), 1);
    runtime
        .acknowledge_plugin_profile_setup(&id, &pending[0].fingerprint)
        .unwrap();
    assert!(runtime
        .pending_plugin_profile_setup(vec![id.clone()])
        .unwrap()
        .is_empty());
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    std::fs::write(root.join("index.html"), "changed code").unwrap();
    crate::test_plugin::modernize(&root);
    assert!(runtime
        .pending_plugin_profile_setup(vec![id.clone()])
        .unwrap()
        .is_empty());
    let suggestion = root.join("profiles/n4-pro.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&suggestion).unwrap()).unwrap();
    document["profiles"][0]["name"] = json!("Updated suggestion");
    std::fs::write(suggestion, serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(
        runtime
            .pending_plugin_profile_setup(vec![id])
            .unwrap()
            .len(),
        1
    );
    runtime.shutdown();
}
#[test]
// Verify that failed transaction preserves both stores on restart
fn failed_transaction_preserves_both_stores_on_restart() {
    let (directory, runtime, id) = fixture();
    let before = serde_json::to_value(runtime.profiles.load().unwrap()).unwrap();
    let current = runtime.plugins.lock().unwrap().configuration(&id).unwrap();
    let mut changed = current.clone();
    changed
        .settings
        .as_object_mut()
        .unwrap()
        .insert("temporary".into(), json!(true));
    let prepared = runtime
        .profiles
        .prepare_import_transaction(
            runtime.snapshot().configuration_revision,
            "fault injection",
            None,
            0,
            |data| {
                data.profiles[0].name = "must roll back".into();
                Ok(())
            },
        )
        .unwrap();
    let result: Result<(), String> = runtime
        .plugins
        .lock()
        .unwrap()
        .configuration_transaction_sql(
            &id,
            &current,
            &changed,
            prepared.retained_bytes().unwrap(),
            move |tx| {
                prepared.commit(tx)?;
                Err("injected failure after workspace and plugin writes".into())
            },
        );
    assert!(result.is_err());
    assert_eq!(
        serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        runtime
            .plugins
            .lock()
            .unwrap()
            .configuration(&id)
            .unwrap()
            .settings,
        current.settings
    );
    runtime.shutdown();
    drop(runtime);
    let recovered = Runtime::build(directory.path().join("manager"), false).unwrap();
    assert_eq!(
        serde_json::to_value(recovered.profiles.load().unwrap()).unwrap(),
        before
    );
    assert!(!recovered
        .plugins
        .lock()
        .unwrap()
        .configuration(&id)
        .unwrap()
        .settings
        .as_object()
        .unwrap()
        .contains_key("temporary"));
    assert!(!directory
        .path()
        .join("manager/profiles/profiles.json")
        .exists());
    recovered.shutdown();
}

#[test]
// Verify that save failure rolls back settings and retains the import draft
fn save_failure_rolls_back_settings_and_retains_the_import_draft() {
    let (_directory, runtime, id) = fixture();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let before = runtime.snapshot();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .is_err());
    assert!(runtime
        .plugins
        .lock()
        .unwrap()
        .configuration(&id)
        .unwrap()
        .settings
        .as_object()
        .unwrap()
        .is_empty());
    assert_eq!(
        runtime.snapshot().configuration_revision,
        before.configuration_revision
    );
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=OFF")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .is_ok());
    runtime.shutdown();
}
#[test]
// Verify that cross model mapping requires omitting incompatible dials
fn cross_model_mapping_requires_omitting_incompatible_dials() {
    let (_directory, runtime, id) = fixture();
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    let file = root.join("profiles/n4-pro.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut binding = document["profiles"][0]["pages"][0]["bindings"]["key-1"].clone();
    let action = binding["action"].clone();
    binding["action"] = json!({"kind":"dial","actions":[action.clone(), action, {"kind":"none"}]});
    document["profiles"][0]["pages"][0]["bindings"]["dial-1"] = binding;
    std::fs::write(file, serde_json::to_vec(&document).unwrap()).unwrap();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    placement.target_model = "xl".into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement.clone())
        .unwrap();
    assert!(!report.plugin.unwrap().ready);
    placement
        .bindings
        .iter_mut()
        .find(|b| b.source_control == "dial-1")
        .unwrap()
        .target_control = OMIT.into();
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    assert!(report.plugin.unwrap().ready);
    runtime.shutdown();
}
#[test]
// Verify that undo survives restart and rejects intervening configuration changes
fn undo_survives_restart_and_rejects_intervening_configuration_changes() {
    let (directory, runtime, id) = fixture();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    runtime.shutdown();
    drop(runtime);
    let runtime = Runtime::build(directory.path().join("manager"), false).unwrap();
    let before = runtime.plugins.lock().unwrap().configuration(&id).unwrap();
    let mut changed = before.clone();
    changed.settings["nested"]["endpoint"] = json!("edited later");
    runtime
        .plugins
        .lock()
        .unwrap()
        .configuration_transaction_sql(&id, &before, &changed, 0, |_| Ok(()))
        .unwrap();
    assert!(runtime
        .history_action("undo", after.configuration_revision)
        .unwrap_err()
        .contains("settings changed"));
    assert_eq!(
        runtime.snapshot().configuration_revision,
        after.configuration_revision
    );
    runtime
        .plugins
        .lock()
        .unwrap()
        .configuration_transaction_sql(&id, &changed, &before, 0, |_| Ok(()))
        .unwrap();
    assert!(runtime
        .history_action("undo", after.configuration_revision)
        .is_ok());
    runtime.shutdown();
}
#[test]
// Verify that root profiles are unsupported and bad optional profiles do not block installation
fn root_profiles_are_unsupported_and_bad_optional_profiles_do_not_block_installation() {
    let (_directory, runtime, id) = fixture();
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    std::fs::rename(root.join("profiles/n4-pro.json"), root.join("profile.json")).unwrap();
    assert!(runtime
        .prepare_plugin_profile_import(&id, "profile.json", None, None)
        .is_err());
    std::fs::write(root.join("profiles/xl.json"), "invalid JSON").unwrap();
    let catalog = runtime.list_plugin_profiles(&id).unwrap();
    assert_eq!(catalog.suggestions.len(), 1);
    assert!(root.join("profile.json").is_file());
    assert_eq!(catalog.warnings.len(), 1);
    assert!(runtime.snapshot().plugins.iter().any(|p| p.id == id));
    runtime.shutdown();
}
#[test]
// Verify that bundled media is staged with fresh IDs and missing media is unavailable
fn bundled_media_is_staged_with_fresh_ids_and_missing_media_is_unavailable() {
    let (directory, runtime, id) = fixture();
    let image = directory.path().join("sample.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 30, 40, 255]))
        .save(&image)
        .unwrap();
    let asset = runtime.profiles.import_asset(&image).unwrap();
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    std::fs::create_dir_all(root.join("profiles/assets")).unwrap();
    std::fs::write(
        root.join(format!("profiles/assets/{asset}.png")),
        runtime.profiles.asset(&asset).unwrap(),
    )
    .unwrap();
    let file = root.join("profiles/n4-pro.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    document["profiles"][0]["pages"][0]["bindings"]["key-1"]["asset"] = json!(asset);
    std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    let imported = after.profiles.last().unwrap().pages[0].bindings["key-1"]
        .asset
        .as_ref()
        .unwrap();
    assert_ne!(imported, &asset);
    assert_eq!(
        runtime.profiles.asset(imported).unwrap(),
        runtime.profiles.asset(&asset).unwrap()
    );
    std::fs::remove_file(root.join(format!("profiles/assets/{asset}.png"))).unwrap();
    assert!(runtime.list_plugin_profiles(&id).unwrap().suggestions[0]
        .error
        .is_some());
    runtime.shutdown();
}
#[test]
#[ignore = "Requires explicitly built local official plugin artifacts; imports disabled packages and never executes them"]
// Verify that packaged official suggestions validate and apply without plugin execution
fn packaged_official_suggestions_validate_and_apply_without_plugin_execution() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(directory.path().join("manager"), false).unwrap();
    let manager = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let base = manager.join("../StreamDockManager-Plugins/dist/packages");
    let packages = if cfg!(target_os = "macos") {
        base.join("darwin").join(if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "amd64"
        })
    } else {
        base.join(std::env::consts::OS)
    };
    let catalog_name = if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    };
    let metadata: Value = serde_json::from_slice(
        &std::fs::read(manager.join(format!(
            "../StreamDockManager-Plugins/dist/catalog-{catalog_name}.json"
        )))
        .unwrap(),
    )
    .unwrap();
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let archives: Vec<_> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| package["architectures"] == json!([arch]))
        .map(|package| {
            packages.join(
                std::path::Path::new(package["path"].as_str().unwrap())
                    .file_name()
                    .unwrap(),
            )
        })
        .collect();
    let source: Value = serde_json::from_slice(
        &std::fs::read(manager.join("../StreamDockManager-Plugins/catalog/metadata.json")).unwrap(),
    )
    .unwrap();
    let expected: std::collections::BTreeSet<_> = source
        .as_object()
        .unwrap()
        .keys()
        .map(|slug| format!("app.streamdockmanager.plugins.{slug}"))
        .collect();
    let actual: std::collections::BTreeSet<_> = archives
        .iter()
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        actual, expected,
        "Build every current official plugin package first"
    );
    for archive in archives {
        let review = runtime.prepare_plugin_package(&archive).unwrap();
        let catalog = review.suggested_profiles.as_ref().unwrap();
        assert_eq!(
            catalog.suggestions.len(),
            hardware_registry::manifests().len()
        );
        assert!(
            catalog.warnings.is_empty(),
            "{}: {:?}",
            catalog.name,
            catalog.warnings
        );
        let id = review.package.id.clone();
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        for suggestion in &catalog.suggestions {
            let report = runtime
                .prepare_plugin_profile_import(&id, &suggestion.key, None, None)
                .unwrap();
            let mut placement = report.plugin.unwrap().placement;
            placement.target_profile = NEW.into();
            placement.target_model = suggestion.model_id.clone();
            for page in &mut placement.pages {
                page.target_page = NEW.into();
            }
            let report = runtime
                .update_plugin_profile_import(&report.id, placement)
                .unwrap();
            assert!(
                report.plugin.as_ref().unwrap().ready,
                "{} / {}: {:?}",
                catalog.name,
                suggestion.model_id,
                report.impact
            );
            if suggestion.model_id == "n4-pro" {
                let after = runtime
                    .commit_profile_import(&report.id, report.expected_revision)
                    .unwrap();
                runtime
                    .history_action("undo", after.configuration_revision)
                    .unwrap();
            } else {
                runtime.cancel_profile_import(&report.id);
            }
        }
    }
    assert!(runtime
        .snapshot()
        .plugins
        .iter()
        .all(|plugin| !plugin.enabled));
    runtime.shutdown();
}

#[test]
// Verify that references to suggestions skipped by collisions require explicit resolution
fn references_to_suggestions_skipped_by_collisions_require_explicit_resolution() {
    let (_directory, runtime, id) = fixture();
    let report = prepare(&runtime, &id);
    let mut placement = report.plugin.unwrap().placement;
    placement.target_profile = NEW.into();
    for page in &mut placement.pages {
        page.target_page = NEW.into();
    }
    placement
        .bindings
        .iter_mut()
        .find(|binding| binding.source_control == "key-2")
        .unwrap()
        .target_control = OMIT.into();
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    let current = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    let profile = current.profiles.last().unwrap();
    let root = runtime
        .plugins
        .lock()
        .unwrap()
        .plugin_directory(&id)
        .unwrap();
    let file = root.join("profiles/n4-pro.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    document["profiles"][0]["pages"][0]["bindings"]["key-2"]["action"] =
        serde_json::to_value(Action::Builtin {
            action: Box::new(crate::builtins::BuiltinAction::TriggerBinding {
                device_id: String::new(),
                profile_id: "suggestion".into(),
                page_id: "page".into(),
                control_id: "key-1".into(),
            }),
        })
        .unwrap();
    std::fs::write(file, serde_json::to_vec(&document).unwrap()).unwrap();
    let report = runtime
        .prepare_plugin_profile_import(
            &id,
            "profiles/n4-pro.json",
            Some(profile.id.clone()),
            Some(profile.pages[0].id.clone()),
        )
        .unwrap();
    assert!(!report.plugin.as_ref().unwrap().ready);
    let mut placement = report.plugin.unwrap().placement;
    placement
        .bindings
        .iter_mut()
        .find(|binding| binding.source_control == "key-1")
        .unwrap()
        .replace = true;
    let report = runtime
        .update_plugin_profile_import(&report.id, placement)
        .unwrap();
    assert!(report.plugin.unwrap().ready);
    runtime.shutdown();
}

#[test]
// Stage a Default profile without persistence and create it only when the suggestion is accepted
fn empty_workspace_suggestion_creates_default_only_on_commit() {
    let (_directory, runtime, id) = fixture();
    runtime.simulate(true).unwrap();
    let before = runtime.snapshot();
    let empty = runtime
        .delete_profile(&before.profiles[0].id, before.configuration_revision)
        .unwrap();
    assert!(empty.profiles.is_empty());
    let report = prepare(&runtime, &id);
    assert_eq!(
        report.plugin.as_ref().unwrap().placement.profile_name,
        crate::profiles::DEFAULT_PROFILE_NAME
    );
    assert!(runtime.profiles.load().unwrap().profiles.is_empty());
    runtime.cancel_profile_import(&report.id);
    assert!(runtime.profiles.load().unwrap().profiles.is_empty());
    let report = prepare(&runtime, &id);
    assert!(report.plugin.as_ref().unwrap().ready);
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    assert_eq!(after.profiles.len(), 1);
    let profile = &after.profiles[0];
    assert_eq!(profile.name, "Default");
    assert_eq!(after.active_profile, profile.id);
    assert_eq!(profile.pages[0].bindings["key-1"].title, "Run suggestion");
    assert_eq!(runtime.profiles.load().unwrap().profiles[0].name, "Default");
    runtime.shutdown();
}

#[test]
// Preserve an existing user profile name when importing suggested bindings into it
fn suggestion_preserves_existing_user_profile_name() {
    let (_directory, runtime, id) = fixture();
    let snapshot = runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    profile.name = "Everyday".into();
    profile.pages[0].bindings.clear();
    runtime
        .save_profile(profile.clone(), snapshot.configuration_revision)
        .unwrap();
    let report = prepare(&runtime, &id);
    assert_eq!(
        report.plugin.as_ref().unwrap().placement.target_profile,
        profile.id
    );
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    assert_eq!(after.profiles.len(), 1);
    assert_eq!(after.profiles[0].name, "Everyday");
    runtime.shutdown();
}

#[test]
// Import into the chosen device without changing another active device profile
fn chosen_device_without_profile_receives_default_suggestion() {
    let (_directory, runtime, id) = fixture();
    runtime.simulate(true).unwrap();
    runtime.simulate_device(true, "simulator:n3").unwrap();
    let snapshot = runtime.snapshot();
    let other = snapshot
        .profiles
        .iter()
        .find(|p| p.model_id == "n3")
        .unwrap()
        .clone();
    let chosen = snapshot
        .profiles
        .iter()
        .find(|p| p.model_id == "n4-pro")
        .unwrap();
    runtime
        .delete_profile(&chosen.id, snapshot.configuration_revision)
        .unwrap();
    let report = runtime
        .prepare_plugin_profile_import_for_device(
            &id,
            "profiles/n4-pro.json",
            None,
            None,
            Some("simulator:n4-pro".into()),
        )
        .unwrap();
    let placement = &report.plugin.as_ref().unwrap().placement;
    assert_eq!(placement.target_model, "n4-pro");
    assert_eq!(placement.profile_name, "Default");
    assert_eq!(runtime.profiles.load().unwrap().profiles.len(), 1);
    let after = runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .unwrap();
    assert_eq!(after.profiles.len(), 2);
    assert_eq!(
        serde_json::to_value(after.profiles.iter().find(|p| p.id == other.id).unwrap()).unwrap(),
        serde_json::to_value(&other).unwrap()
    );
    let created = after
        .profiles
        .iter()
        .find(|p| p.model_id == "n4-pro")
        .unwrap();
    assert_eq!(created.name, "Default");
    assert_eq!(after.session("simulator:n4-pro").profile_id, created.id);
    assert_eq!(after.session("simulator:n3").profile_id, other.id);
    assert!(runtime
        .prepare_plugin_profile_import_for_device(
            &id,
            "profiles/n4-pro.json",
            Some(other.id),
            None,
            Some("simulator:n4-pro".into())
        )
        .is_err());
    runtime.shutdown();
}

#[test]
// Reject stale explicit destinations and devices disconnected after review without persisting an import
fn suggested_import_rejects_missing_profile_and_disconnected_device() {
    let (_directory, runtime, id) = fixture();
    assert!(runtime
        .prepare_plugin_profile_import(&id, "profiles/n4-pro.json", Some("missing".into()), None)
        .is_err());
    runtime.simulate(true).unwrap();
    let snapshot = runtime.snapshot();
    runtime
        .delete_profile(&snapshot.profiles[0].id, snapshot.configuration_revision)
        .unwrap();
    let report = runtime
        .prepare_plugin_profile_import_for_device(
            &id,
            "profiles/n4-pro.json",
            None,
            None,
            Some("simulator:n4-pro".into()),
        )
        .unwrap();
    runtime.simulate(false).unwrap();
    assert!(runtime
        .commit_profile_import(&report.id, report.expected_revision)
        .is_err());
    assert!(runtime.profiles.load().unwrap().profiles.is_empty());
    runtime.shutdown();
}

#[test]
// Resolve the chosen device session through its stable persistence key rather than its connection ID
fn suggestion_uses_selected_device_persistent_session() {
    let (_directory, runtime, id) = fixture();
    runtime.simulate(true).unwrap();
    runtime
        .simulate_device(true, "simulator:n4-pro:other")
        .unwrap();
    let snapshot = runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    profile.id = "chosen-profile".into();
    profile.name = "My controls".into();
    let after = runtime
        .save_profile(profile.clone(), snapshot.configuration_revision)
        .unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .devices
            .iter_mut()
            .find(|d| d.id == "simulator:n4-pro")
            .unwrap()
            .persistence_key = "stable-device".into();
        state.sessions.insert(
            "stable-device".into(),
            crate::workspace::DeviceSession {
                profile_id: profile.id.clone(),
                page_id: profile.active_page.clone(),
                ..Default::default()
            },
        );
    }
    assert_ne!(
        runtime
            .snapshot()
            .session("simulator:n4-pro:other")
            .profile_id,
        profile.id
    );
    let report = runtime
        .prepare_plugin_profile_import_for_device(
            &id,
            "profiles/n4-pro.json",
            None,
            None,
            Some("simulator:n4-pro".into()),
        )
        .unwrap();
    assert_eq!(
        report.plugin.as_ref().unwrap().placement.target_profile,
        profile.id
    );
    assert_eq!(
        runtime.snapshot().configuration_revision,
        after.configuration_revision
    );
    runtime.cancel_profile_import(&report.id);
    runtime.shutdown();
}
