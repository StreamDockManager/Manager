//! The keyboard's permanent, palette-driven navigation page
use crate::actions::Action;
use crate::profiles::{BackgroundSource, Binding, Page, Profile, ProfileData};

pub(crate) const PAGE_ID: &str = "__keyboard_navigation";
pub(crate) const PAGE_NAME: &str = "Navigation";
const KEYS: [(&str, &str); 6] = [
    ("Insert", "insert"),
    ("Home", "home"),
    ("Page Up", "pageup"),
    ("Delete", "delete"),
    ("End", "end"),
    ("Page Down", "pagedown"),
];

// Report the supported condition for keyboard navigation
pub(crate) fn supported(model: &str) -> bool {
    // This layout is explicitly a two-row, three-column keyboard navigation block
    model == "k1-pro"
}
// Report the protected control condition for keyboard navigation
pub(crate) fn protected_control(model: &str, page: &str, control: &str) -> bool {
    supported(model) && page == PAGE_ID && (1..=6).any(|i| control == format!("key-{i}"))
}
// Create a transparent hotkey binding for the keyboard navigation page
fn binding(title: &str, key: &str) -> Binding {
    Binding {
        title: title.into(),
        color: "#00000000".into(),
        background_source: Some(BackgroundSource::Palette),
        action: Action::Hotkey {
            keys: vec![key.into()],
        },
        dial_response: None,
        activation_policy: Default::default(),
        custom_background: false,
        asset: None,
        media: Default::default(),
        appearance: None,
        plugin_appearance: None,
        composition: None,
        feedback: None,
        disabled: false,
    }
}
// Build the reserved navigation page from the maintained key definitions
pub(crate) fn page() -> Page {
    Page {
        id: PAGE_ID.into(),
        name: PAGE_NAME.into(),
        bindings: KEYS
            .iter()
            .enumerate()
            .map(|(i, (title, key))| (format!("key-{}", i + 1), binding(title, key)))
            .collect(),
    }
}
/// Visual customization is independent from the permanent key behavior
pub(crate) fn same_behavior(a: &Binding, b: &Binding) -> bool {
    serde_json::to_value((
        &a.action,
        &a.dial_response,
        &a.activation_policy,
        a.disabled,
    ))
    .ok()
        == serde_json::to_value((
            &b.action,
            &b.dial_response,
            &b.activation_policy,
            b.disabled,
        ))
        .ok()
}
// Report the canonical condition for keyboard navigation
pub(crate) fn canonical(page: &Page) -> bool {
    let expected = self::page();
    page.name == PAGE_NAME
        && expected.bindings.iter().all(|(key, binding)| {
            page.bindings
                .get(key)
                .is_some_and(|saved| same_behavior(saved, binding))
        })
}
/// Add rather than replace a user's first page. Existing active pages remain selected
/// A colliding user/imported ID is rejected, never discarded or silently redirected
pub(crate) fn normalize_profile(profile: &mut Profile) -> Result<(), String> {
    if !supported(&profile.model_id) {
        return Ok(());
    }
    if let Some(index) = profile.pages.iter().position(|p| p.id == PAGE_ID) {
        if !canonical(&profile.pages[index]) {
            return Err("Reserved keyboard Navigation page conflicts with custom content; rename that page before importing".into());
        }
        let page = profile.pages.remove(index);
        profile.pages.insert(0, page);
        return Ok(());
    }
    profile.pages.insert(0, page());
    if profile.active_page.is_empty() {
        profile.active_page = PAGE_ID.into();
    }
    Ok(())
}
// Normalize the keyboard navigation representation
pub(crate) fn normalize(data: &mut ProfileData) -> Result<(), String> {
    for profile in &mut data.profiles {
        normalize_profile(profile)?;
    }
    Ok(())
}
/// Edit transactions must not remove, move, rename or change this page's key behavior
/// Whole-profile removal/conversion is still permitted
pub(crate) fn validate_edit(before: &ProfileData, after: &ProfileData) -> Result<(), String> {
    for old in before.profiles.iter().filter(|p| supported(&p.model_id)) {
        let Some(new) = after
            .profiles
            .iter()
            .find(|p| p.id == old.id && p.model_id == old.model_id)
        else {
            continue;
        };
        if old
            .pages
            .first()
            .is_some_and(|p| p.id == PAGE_ID && canonical(p))
            && !new
                .pages
                .first()
                .is_some_and(|p| p.id == PAGE_ID && canonical(p))
        {
            return Err(
                "The keyboard Navigation page and its six navigation keys are protected".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct an isolated keyboard navigation fixture
    fn fixture() -> ProfileData {
        let mut data = ProfileData::default();
        data.profiles[0].model_id = "k1-pro".into();
        data
    }
    #[test]
    // Verify that navigation page is first preserves existing pages and is idempotent
    fn navigation_page_is_first_preserves_existing_pages_and_is_idempotent() {
        let mut data = fixture();
        let old = serde_json::to_value(&data.profiles[0].pages).unwrap();
        normalize(&mut data).unwrap();
        let p = &data.profiles[0];
        assert_eq!(p.pages[0].id, PAGE_ID);
        assert_eq!(serde_json::to_value(&p.pages[1..]).unwrap(), old);
        assert_eq!(p.active_page, "main");
        assert_eq!(p.pages[0].bindings.len(), 6);
        for (i, (title, key)) in KEYS.iter().enumerate() {
            let b = &p.pages[0].bindings[&format!("key-{}", i + 1)];
            assert_eq!(&b.title, title);
            assert!(matches!(&b.action, Action::Hotkey { keys } if keys == &vec![key.to_string()]));
            b.action.validate(0).unwrap();
            assert_eq!(b.background_source, Some(BackgroundSource::Palette));
        }
        let first = serde_json::to_value(&data).unwrap();
        normalize(&mut data).unwrap();
        assert_eq!(serde_json::to_value(&data).unwrap(), first);
        let mut normal = ProfileData::default();
        let before = serde_json::to_value(&normal).unwrap();
        normalize(&mut normal).unwrap();
        assert_eq!(serde_json::to_value(normal).unwrap(), before);
    }
    #[test]
    // Verify that conflicting reserved ID is rejected without rewriting content
    fn conflicting_reserved_id_is_rejected_without_rewriting_content() {
        let mut data = fixture();
        data.profiles[0].pages[0].id = PAGE_ID.into();
        data.profiles[0].active_page = PAGE_ID.into();
        let before = serde_json::to_value(&data).unwrap();
        assert!(normalize(&mut data)
            .unwrap_err()
            .contains("rename that page"));
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }
    #[test]
    // Verify that older replace import adds navigation without discarding custom page
    fn older_replace_import_adds_navigation_without_discarding_custom_page() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let data = fixture();
        runtime
            .save_profile(
                data.profiles[0].clone(),
                runtime.snapshot().configuration_revision,
            )
            .unwrap();
        let path = root.path().join("old-profile.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&data).unwrap()).unwrap();
        zip.finish().unwrap();
        let preview = runtime.prepare_profile_import(&path).unwrap();
        let preview = runtime
            .update_profile_import(&preview.id, "replace", vec![])
            .unwrap();
        assert!(preview.issues.is_empty(), "{:?}", preview.issues);
        let saved = runtime
            .commit_profile_import(&preview.id, preview.expected_revision)
            .unwrap();
        assert_eq!(saved.profiles[0].pages[0].id, PAGE_ID);
        assert_eq!(saved.profiles[0].pages.len(), 2);
        assert_eq!(
            saved.profiles[0].pages[1].bindings["key-1"].title,
            "Browser"
        );
        assert_eq!(saved.profiles[0].active_page, "main");
        runtime.shutdown();
    }
    #[test]
    // Verify that navigation keys follow palette background and ink
    fn navigation_keys_follow_palette_background_and_ink() {
        let model = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "k1-pro")
            .unwrap();
        for (surface, ink) in [("#112233", "#ffffff"), ("#eeddcc", "#000000")] {
            for (id, binding) in page().bindings {
                let themed = crate::Runtime::themed_button_binding(
                    model.control(&id).unwrap(),
                    &binding,
                    stream_dock_manager_plugin_host::DeviceThemePreferences {
                        sync: false,
                        transparent_buttons: true,
                    },
                    surface,
                    ink,
                )
                .unwrap();
                assert_eq!(themed.color, surface);
                assert_eq!(themed.appearance.unwrap().foreground, ink);
                assert_eq!(binding.color, "#00000000");
            }
        }
    }
    #[test]
    // Verify that navigation appearance persists but behavior stays protected
    fn navigation_appearance_persists_but_behavior_stays_protected() {
        use crate::workspace::{AssignmentScope, Edit};
        let root = tempfile::tempdir().unwrap();
        let store = crate::ProfileStore::open(root.path().into()).unwrap();
        let mut data = fixture();
        store.save(&mut data).unwrap();
        let data = store.load().unwrap();
        let original = data.profiles[0].pages[0].bindings["key-1"].clone();
        let mut custom = original.clone();
        custom.title = "My insert".into();
        custom.color = "#123456".into();
        custom.background_source = Some(BackgroundSource::Custom);
        custom.appearance = Some(Box::new(hardware_core::appearance::Appearance {
            position: "center".into(),
            size: 23,
            ..Default::default()
        }));
        let edited = store
            .edit(
                data.configuration_revision,
                "Navigation appearance",
                |data| {
                    crate::workspace::apply_edits(
                        data,
                        vec![Edit::AssignControl {
                            profile_id: "default".into(),
                            page_id: PAGE_ID.into(),
                            control_id: "key-1".into(),
                            scope: AssignmentScope::Page,
                            binding: custom.clone(),
                        }],
                    )
                },
            )
            .unwrap();
        assert!(canonical(&edited.profiles[0].pages[0]));
        for mutation in 0..4 {
            let mut invalid = custom.clone();
            match mutation {
                0 => {
                    invalid.action = Action::Hotkey {
                        keys: vec!["home".into()],
                    }
                }
                1 => invalid.disabled = true,
                2 => invalid.activation_policy = crate::actions::ActivationPolicy::IgnoreBusy,
                _ => {
                    invalid.dial_response = Some(crate::dial_response::DialResponse {
                        curve: crate::dial_response::DialCurve::Linear,
                        max_steps: 2,
                    })
                }
            }
            assert!(!same_behavior(&original, &invalid));
            assert!(store
                .edit(edited.configuration_revision, "Invalid", |data| {
                    crate::workspace::apply_edits(
                        data,
                        vec![Edit::Bindings {
                            profile_id: "default".into(),
                            page_id: PAGE_ID.into(),
                            bindings: [("key-1".into(), Some(invalid))].into(),
                        }],
                    )
                })
                .is_err());
        }
        let undone = store
            .history_action(edited.configuration_revision, "undo")
            .unwrap();
        assert_eq!(
            undone.profiles[0].pages[0].bindings["key-1"].title,
            "Insert"
        );
        store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        drop(store);
        let store = crate::ProfileStore::open(root.path().into()).unwrap();
        let mut loaded = store.load().unwrap();
        normalize(&mut loaded).unwrap();
        let saved = &loaded.profiles[0].pages[0].bindings["key-1"];
        assert_eq!(saved.title, "My insert");
        assert_eq!(saved.color, "#123456");
        assert_eq!(saved.appearance.as_ref().unwrap().size, 23);
        assert!(same_behavior(saved, &original));
    }

    #[test]
    // Verify that persisted keyboard page is protected and other pages remain editable
    fn persisted_keyboard_page_is_protected_and_other_pages_remain_editable() {
        use crate::workspace::{AssignmentScope, Edit};
        let root = tempfile::tempdir().unwrap();
        let store = crate::ProfileStore::open(root.path().into()).unwrap();
        store.save(&fixture()).unwrap();
        let data = store.load().unwrap();
        assert_eq!(data.profiles[0].pages[0].id, PAGE_ID);
        for edit in [
            Edit::DeletePage {
                profile_id: "default".into(),
                page_id: PAGE_ID.into(),
            },
            Edit::Bindings {
                profile_id: "default".into(),
                page_id: PAGE_ID.into(),
                bindings: [("key-1".into(), None)].into(),
            },
        ] {
            assert!(store
                .edit(data.configuration_revision, "Invalid", |data| {
                    crate::workspace::apply_edits(data, vec![edit])
                })
                .is_err());
        }
        for mutation in 0..3 {
            let mut p = data.profiles[0].clone();
            match mutation {
                0 => p.pages[0].name = "Renamed".into(),
                1 => p.pages.swap(0, 1),
                _ => {
                    p.pages.remove(0);
                }
            }
            assert!(store
                .edit(data.configuration_revision, "Invalid", |data| {
                    crate::workspace::apply_edits(data, vec![Edit::SaveProfile { profile: p }])
                })
                .is_err());
        }
        let custom = binding("Custom", "end");
        let changed = store
            .edit(data.configuration_revision, "All pages", |data| {
                crate::workspace::apply_edits(
                    data,
                    vec![Edit::AssignControl {
                        profile_id: "default".into(),
                        page_id: "main".into(),
                        control_id: "key-1".into(),
                        scope: AssignmentScope::Profile,
                        binding: custom,
                    }],
                )
            })
            .unwrap();
        assert_eq!(
            changed.profiles[0].pages[0].bindings["key-1"].title,
            "Insert"
        );
        assert_eq!(
            changed.profiles[0].pages[1].bindings["key-1"].title,
            "Custom"
        );
        let undone = store
            .history_action(changed.configuration_revision, "undo")
            .unwrap();
        assert_eq!(undone.profiles[0].pages[0].id, PAGE_ID);
        let redone = store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        assert_eq!(redone.profiles[0].pages[0].id, PAGE_ID);
        drop(store);
        let store = crate::ProfileStore::open(root.path().into()).unwrap();
        assert_eq!(store.load().unwrap().profiles[0].pages[0].id, PAGE_ID);
    }
}
