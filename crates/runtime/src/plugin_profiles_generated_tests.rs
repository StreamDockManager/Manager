//! Opt-in producer integration. The caller supplies freshly generated profiles in
//! inert, disabled packages. No bundled code or hardware is ever started here
use super::*;
use std::path::{Component, PathBuf};
use stream_dock_manager_plugin_host::RuntimeState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    packages: Vec<CorpusPackage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusPackage {
    path: String,
    profiles: usize,
    #[serde(default)]
    archive: Option<String>,
}

use crate::test_support::open_disposable;

// Assert inactive
fn assert_inactive(runtime: &Runtime) {
    for plugin in runtime.snapshot().plugins {
        assert!(!plugin.enabled, "{} unexpectedly enabled", plugin.id);
        assert!(
            !matches!(plugin.runtime.state, RuntimeState::Running),
            "{} unexpectedly running",
            plugin.id
        );
    }
}

#[test]
#[ignore = "requires STREAM_DOCK_MANAGER_PROFILE_CORPUS pointing to fresh, inert producer packages"]
// Verify that generated suggestions import cancel and reopen
fn generated_suggestions_import_cancel_and_reopen() {
    let index = PathBuf::from(
        std::env::var_os("STREAM_DOCK_MANAGER_PROFILE_CORPUS")
            .expect("Set STREAM_DOCK_MANAGER_PROFILE_CORPUS to an explicit corpus.json"),
    );
    let corpus: Corpus = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    assert!(!corpus.packages.is_empty(), "Generated corpus is empty");
    let base = index.parent().unwrap();
    let mut total = 0;
    for package in &corpus.packages {
        assert_eq!(Path::new(&package.path).components().count(), 1);
        assert!(matches!(
            Path::new(&package.path).components().next(),
            Some(Component::Normal(_))
        ));
        assert!(package.profiles > 0, "{} has no profiles", package.path);
        let source = base.join(&package.path);
        let disposable = tempfile::tempdir().unwrap();
        let config = disposable.path().join("manager");
        let runtime = open_disposable(config.clone());
        let review = runtime
            .prepare_plugin_package(&source)
            .unwrap_or_else(|error| panic!("{} package review: {error}", package.path));
        let catalog = review.suggested_profiles.as_ref().unwrap();
        assert_eq!(
            catalog.suggestions.len(),
            package.profiles,
            "{}",
            package.path
        );
        assert!(
            catalog.warnings.is_empty(),
            "{}: {:?}",
            package.path,
            catalog.warnings
        );
        for suggestion in &catalog.suggestions {
            assert!(
                suggestion.error.is_none(),
                "{} / {}: {:?}",
                package.path,
                suggestion.key,
                suggestion.error
            );
        }
        let id = review.package.id.clone();
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        let plugin = runtime
            .snapshot()
            .plugins
            .into_iter()
            .find(|plugin| plugin.id == id)
            .unwrap();
        let catalog = runtime.list_plugin_profiles(&id).unwrap();
        let mut originals = Vec::new();
        for suggestion in &catalog.suggestions {
            let context = format!("{} / {}", package.path, suggestion.key);
            let original = std::fs::read(source.join(&suggestion.key)).unwrap();
            let (data, _, _) = document_bytes(&original, &suggestion.key, &plugin)
                .unwrap_or_else(|error| panic!("{context}: {error}"));
            crate::profiles::validate_data(&data).unwrap();
            // The integration must keep exercising the strict current parser,
            // not migrate producer omissions behind the fixture's back
            for field in ["configuration_revision", "saved_macros", "rules", "history"] {
                let mut missing: Value = serde_json::from_slice(&original).unwrap();
                assert!(
                    missing.as_object_mut().unwrap().remove(field).is_some(),
                    "{context}: missing {field}"
                );
                assert!(
                    document_bytes(
                        &serde_json::to_vec(&missing).unwrap(),
                        &suggestion.key,
                        &plugin
                    )
                    .is_err(),
                    "{context}: accepted missing {field}"
                );
            }
            let before = serde_json::to_value(runtime.profiles.load().unwrap()).unwrap();
            let cancel = runtime
                .prepare_plugin_profile_import(&id, &suggestion.key, None, None)
                .unwrap_or_else(|error| panic!("{context} prepare/cancel: {error}"));
            assert_eq!(
                serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
                before,
                "{context}: staging wrote workspace"
            );
            runtime.cancel_profile_import(&cancel.id);
            assert!(
                runtime
                    .commit_profile_import(&cancel.id, cancel.expected_revision)
                    .is_err(),
                "{context}: cancelled import committed"
            );
            assert_eq!(
                serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
                before,
                "{context}: cancel changed workspace"
            );

            let report = runtime
                .prepare_plugin_profile_import(&id, &suggestion.key, None, None)
                .unwrap();
            let mut placement = report.plugin.unwrap().placement;
            placement.target_profile = NEW.into();
            placement.target_model = suggestion.model_id.clone();
            for page in &mut placement.pages {
                page.target_page = NEW.into();
            }
            let prepared = runtime
                .update_plugin_profile_import(&report.id, placement)
                .unwrap_or_else(|error| panic!("{context} placement: {error}"));
            let preview = prepared.plugin.as_ref().unwrap();
            assert!(preview.ready, "{context}: {:?}", preview.rows);
            assert_eq!(
                serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
                before,
                "{context}: placement wrote workspace"
            );
            let previous_profiles = runtime.snapshot().profiles;
            let previous_count = previous_profiles.len();
            let imported = runtime
                .commit_profile_import(&prepared.id, prepared.expected_revision)
                .unwrap_or_else(|error| panic!("{context} commit: {error}"));
            assert_eq!(imported.profiles.len(), previous_count + 1, "{context}");
            let profile = imported
                .profiles
                .iter()
                .find(|p| !previous_profiles.iter().any(|existing| existing.id == p.id))
                .unwrap();
            assert_eq!(profile.model_id, suggestion.model_id, "{context}");
            // K1 Pro always carries its host-owned Navigation page in addition
            // to the producer's layout. Check its identity and content separately.
            let (navigation, imported_pages): (Vec<_>, Vec<_>) = profile
                .pages
                .iter()
                .partition(|p| p.id == crate::keyboard_navigation::PAGE_ID);
            assert_eq!(
                navigation.len(),
                usize::from(crate::keyboard_navigation::supported(&profile.model_id)),
                "{context}"
            );
            if let Some(page) = navigation.first() {
                assert_eq!(
                    serde_json::to_value(page).unwrap(),
                    serde_json::to_value(crate::keyboard_navigation::page()).unwrap(),
                    "{context}"
                );
            }
            assert_eq!(imported_pages.len(), suggestion.page_count, "{context}");
            assert_eq!(
                imported_pages
                    .iter()
                    .map(|page| page.bindings.len())
                    .sum::<usize>(),
                suggestion.binding_count,
                "{context}"
            );
            crate::profiles::validate_data(&runtime.profiles.load().unwrap()).unwrap();
            assert_inactive(&runtime);
            originals.push((suggestion.key.clone(), original));
            total += 1;
        }
        if let Some(archive) = &package.archive {
            assert!(Path::new(archive)
                .components()
                .all(|part| matches!(part, Component::Normal(_))));
            let path = base.join(archive);
            let original = std::fs::read(&path).unwrap();
            let before = serde_json::to_value(runtime.profiles.load().unwrap()).unwrap();
            let cancel = runtime
                .prepare_profile_import(&path)
                .unwrap_or_else(|error| panic!("{} portable archive: {error}", package.path));
            assert!(
                cancel.issues.is_empty(),
                "{} portable archive: {:?}",
                package.path,
                cancel.issues
            );
            assert_eq!(
                serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
                before
            );
            runtime.cancel_profile_import(&cancel.id);
            assert!(runtime
                .commit_profile_import(&cancel.id, cancel.expected_revision)
                .is_err());
            assert_eq!(
                serde_json::to_value(runtime.profiles.load().unwrap()).unwrap(),
                before
            );
            let prepared = runtime.prepare_profile_import(&path).unwrap();
            assert!(prepared.issues.is_empty());
            let previous_count = runtime.snapshot().profiles.len();
            let imported = runtime
                .commit_profile_import(&prepared.id, prepared.expected_revision)
                .unwrap();
            assert_eq!(
                imported.profiles.len(),
                previous_count + prepared.profiles.len()
            );
            crate::profiles::validate_data(&runtime.profiles.load().unwrap()).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_inactive(&runtime);
            println!("{}: portable archive imported and cancelled", package.path);
        }
        let saved = serde_json::to_value(runtime.profiles.load().unwrap()).unwrap();
        let settings =
            serde_json::to_value(runtime.plugins.lock().unwrap().configuration(&id).unwrap())
                .unwrap();
        runtime.shutdown();
        drop(runtime);
        let reopened = open_disposable(config);
        assert_eq!(
            serde_json::to_value(reopened.profiles.load().unwrap()).unwrap(),
            saved,
            "{}: reopened workspace changed",
            package.path
        );
        assert_eq!(
            serde_json::to_value(reopened.plugins.lock().unwrap().configuration(&id).unwrap())
                .unwrap(),
            settings,
            "{}: reopened settings changed",
            package.path
        );
        let rediscovered = reopened.list_plugin_profiles(&id).unwrap();
        assert_eq!(rediscovered.suggestions.len(), package.profiles);
        assert!(rediscovered
            .suggestions
            .iter()
            .all(|suggestion| suggestion.error.is_none()));
        assert_inactive(&reopened);
        reopened.shutdown();
        drop(reopened);
        for (key, original) in originals {
            assert_eq!(
                std::fs::read(source.join(key)).unwrap(),
                original,
                "{}: source changed",
                package.path
            );
        }
        println!(
            "{}: {} suggestions imported, cancelled and reopened",
            package.path, package.profiles
        );
    }
    println!(
        "Verified {total} generated/authored suggestions from {} inert packages",
        corpus.packages.len()
    );
}
