/// The actual first-party UI language, derived from the shipped editor entrypoint
/// There is currently no application language preference or translation layer
/// OS and browser locale preferences must not be reported as Manager's language
pub fn application_language() -> &'static str {
    include_str!("../../../index.html")
        .split_once("<html lang=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(language, _)| language)
        .expect("Editor entrypoint declares its application language")
}

use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct AutostartStatus {
    pub supported: bool,
    pub enabled: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopSettings {
    pub notifications: bool,
    /// Changed only by the dedicated native-consent handler
    pub plugin_links: bool,
    /// Independently opted-in OpenAction scheme; never enabled by ordinary settings saves
    pub openaction_links: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub openaction_link_previous_handler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_link_previous_handler: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub show_manager_shortcut: Option<bool>,
    pub launch_to_tray: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub onboarding: Option<crate::onboarding::SetupProgress>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct PluginLinkStatus {
    pub enabled: bool,
    pub supported: bool,
    pub registered: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum EditorRestoreRoute {
    Editor,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct EditorRestoreDescriptor {
    pub route: EditorRestoreRoute,
    pub device_id: String,
    pub control_id: String,
    pub selected_controls: Vec<String>,
    pub dirty_draft_key: Option<String>,
    pub focus_id: Option<String>,
    pub scroll_x: f64,
    pub scroll_y: f64,
}
impl EditorRestoreDescriptor {
    // Validate the supplied desktop value
    pub fn validate(&self) -> Result<(), String> {
        // Report the bounded condition for desktop
        fn bounded(value: &str, limit: usize) -> bool {
            value.len() <= limit && !value.contains('\0')
        }
        if !bounded(&self.device_id, 256)
            || !bounded(&self.control_id, 128)
            || self.selected_controls.len() > 128
            || self.selected_controls.iter().any(|id| !bounded(id, 128))
            || self
                .dirty_draft_key
                .as_deref()
                .is_some_and(|id| !bounded(id, 512))
            || self.focus_id.as_deref().is_some_and(|id| !bounded(id, 256))
            || ![self.scroll_x, self.scroll_y]
                .into_iter()
                .all(|position| position.is_finite() && (0.0..=10_000_000.0).contains(&position))
            || self
                .selected_controls
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.selected_controls.len()
        {
            return Err("Editor restore state is invalid or exceeds its limits".into());
        }
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > 32 * 1024
        {
            return Err("Editor restore state exceeds 32 KiB".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct EditorRestoreState {
    #[ts(type = "number")]
    pub generation: u64,
    pub restore: Option<EditorRestoreDescriptor>,
}
// Report the autostart status condition for desktop
pub fn autostart_status() -> bool {
    #[cfg(target_os = "linux")]
    {
        autostart_path().is_ok_and(|path| path.exists())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
#[cfg(target_os = "linux")]
// Resolve the Linux autostart desktop-file path under the user’s XDG configuration
fn autostart_path() -> Result<std::path::PathBuf, String> {
    directories::BaseDirs::new()
        .map(|dirs| {
            dirs.config_dir()
                .join("autostart/app.streamdockmanager.manager.desktop")
        })
        .ok_or("XDG config directory unavailable".into())
}
// Set autostart
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let path = autostart_path()?;
        if enabled {
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe = exe.to_str().ok_or("executable path is not UTF-8")?;
            if exe.contains(['\n', '\r', '"', '%', '`', '$']) {
                return Err(
                    "executable path cannot be represented safely in a desktop entry".into(),
                );
            }
            let source = format!(
                "# Managed by Stream Dock Manager\n[Desktop Entry]\nType=Application\nName=Stream Dock Manager\nExec=\"{exe}\" --autostart\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
            );
            std::fs::write(path, source).map_err(|e| e.to_string())?;
        } else if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = enabled;
        Err("XDG autostart is available on Linux".into())
    }
}

/// Explicit launcher requests can reveal the editor; legacy background invocations
/// remain supported. Login registration and window visibility are independent
pub fn start_hidden(settings: &DesktopSettings, args: &[String]) -> bool {
    if args.iter().any(|arg| arg == "--show") {
        return false;
    }
    settings.launch_to_tray || args.iter().any(|arg| arg == "--background")
}
// Report the background activation condition for desktop
pub fn background_activation(args: &[String]) -> bool {
    !args.iter().any(|arg| arg == "--show")
        && args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--autostart" | "--background"))
}
/// Desktop preferences are the pilot typed record; the serialized v1 shape stays unchanged
pub(crate) const SETTINGS_RECORD: stream_dock_manager_storage::JsonRecord<DesktopSettings> =
    stream_dock_manager_storage::JsonRecord {
        domain: stream_dock_manager_storage::Domain::ManagerPreferences,
        key: "desktop",
        max_bytes: 1024 * 1024,
        decode: |text| {
            // Check the container without normalizing it through Value: the typed
            // decoder must still reject duplicate fields, including nested ones
            if !text.trim_start().starts_with('{') {
                return Err(
                    "Invalid stored object in manager_preferences/desktop; database preserved"
                        .into(),
                );
            }
            serde_json::from_str(text).map_err(|error| {
                format!("Unsupported desktop preferences; original preserved: {error}")
            })
        },
    };

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that vendor link routing is default off and generic settings cannot enable it
    fn vendor_link_routing_is_default_off_and_generic_settings_cannot_enable_it() {
        assert!(!DesktopSettings::default().plugin_links);
        assert!(!DesktopSettings::default().openaction_links);
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let mut settings = runtime.desktop_settings();
        settings.plugin_links = true;
        settings.openaction_links = true;
        settings.openaction_link_previous_handler = Some("/Applications/Unapproved.app".into());
        settings.plugin_link_previous_handler = Some("/Applications/Unapproved.app".into());
        runtime.save_desktop_settings(settings).unwrap();
        assert!(!runtime.desktop_settings().plugin_links);
        assert!(!runtime.desktop_settings().openaction_links);
        assert!(runtime
            .desktop_settings()
            .openaction_link_previous_handler
            .is_none());
        assert!(runtime
            .desktop_settings()
            .plugin_link_previous_handler
            .is_none());
        assert!(!runtime
            .dispatch_plugin_link("streamdeck://plugins/message/example.plugin/message")
            .unwrap());
        assert!(!runtime
            .dispatch_plugin_link("openaction://plugins/message/example.plugin/message")
            .unwrap());
        runtime
            .set_openaction_link_settings(true, Some("previous-openaction".into()))
            .unwrap();
        assert!(!runtime.desktop_settings().plugin_links);
        assert!(runtime.desktop_settings().openaction_links);
        runtime
            .set_plugin_link_settings(true, Some("previous-streamdeck".into()))
            .unwrap();
        runtime.set_openaction_link_settings(false, None).unwrap();
        assert!(runtime.desktop_settings().plugin_links);
        assert_eq!(
            runtime
                .desktop_settings()
                .plugin_link_previous_handler
                .as_deref(),
            Some("previous-streamdeck")
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that typed record round trip and restore share shape and domain validation
    fn typed_record_round_trip_and_restore_share_shape_and_domain_validation() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        let settings = DesktopSettings {
            notifications: true,
            ..Default::default()
        };
        SETTINGS_RECORD.write(&storage, &settings).unwrap();
        assert!(
            SETTINGS_RECORD
                .read(&storage)
                .unwrap()
                .unwrap()
                .notifications
        );
        assert!(
            storage
                .run(|conn| SETTINGS_RECORD.read_from(conn))
                .unwrap()
                .unwrap()
                .notifications
        );
        for invalid in [
            serde_json::json!([]),
            serde_json::json!({"notifications":"yes"}),
            serde_json::json!({"retired":true}),
        ] {
            storage
                .write_json(
                    SETTINGS_RECORD.domain,
                    SETTINGS_RECORD.key,
                    &invalid,
                    SETTINGS_RECORD.max_bytes,
                )
                .unwrap();
            let normal = SETTINGS_RECORD.read(&storage).unwrap_err();
            let restore = storage
                .run(|conn| SETTINGS_RECORD.read_from(conn))
                .unwrap_err();
            assert_eq!(normal, restore);
        }
    }

    #[test]
    // Verify that typed record rejects duplicate fields in normal reads and restore
    fn typed_record_rejects_duplicate_fields_in_normal_reads_and_restore() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        SETTINGS_RECORD
            .write(&storage, &DesktopSettings::default())
            .unwrap();
        for raw in [
            r#"{"notifications":true,"notifications":false}"#,
            r#"{"onboarding":{"step":1,"step":2}}"#,
        ] {
            storage
                .transaction(move |tx| {
                    tx.execute(
                        "UPDATE manager_preferences SET payload=?1 WHERE key='desktop'",
                        [raw],
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(())
                })
                .unwrap();
            let normal = SETTINGS_RECORD.read(&storage).unwrap_err();
            let restore = storage
                .run(|conn| SETTINGS_RECORD.read_from(conn))
                .unwrap_err();
            assert!(normal.contains("duplicate field"), "{normal}");
            assert_eq!(normal, restore);
        }
    }

    #[test]
    // Verify that editor restore rejects oversize escaped strings duplicate IDs and invalid scroll
    fn editor_restore_rejects_oversize_escaped_strings_duplicate_ids_and_invalid_scroll() {
        let mut value = EditorRestoreDescriptor {
            route: EditorRestoreRoute::Editor,
            device_id: "device".into(),
            control_id: "button".into(),
            selected_controls: vec!["button".into()],
            dirty_draft_key: Some("draft".into()),
            focus_id: None,
            scroll_x: 0.0,
            scroll_y: 20.0,
        };
        value.validate().unwrap();
        value.scroll_x = f64::NAN;
        assert!(value.validate().is_err());
        value.scroll_x = 0.0;
        value.selected_controls.push("button".into());
        assert!(value.validate().is_err());
        value.selected_controls.pop();
        value.device_id = "é".repeat(129);
        assert!(value.validate().is_err());
        value.device_id.clear();
        value.selected_controls = (0..128)
            .map(|index| format!("{index:03}{}", "\u{1}".repeat(125)))
            .collect();
        assert!(value.validate().unwrap_err().contains("32 KiB"));
    }
    #[test]
    // Verify that old preferences keep visible startup and launch to tray is independent of login
    fn old_preferences_keep_visible_startup_and_launch_to_tray_is_independent_of_login() {
        let old: DesktopSettings = serde_json::from_str(r#"{"notifications":true}"#).unwrap();
        assert!(!start_hidden(&old, &[]));
        assert!(!start_hidden(&old, &["--autostart".into()]));
        assert!(start_hidden(&old, &["--background".into()]));
        let hidden = DesktopSettings {
            launch_to_tray: true,
            ..Default::default()
        };
        assert!(start_hidden(&hidden, &[]));
        assert!(start_hidden(&hidden, &["--autostart".into()]));
        assert!(!start_hidden(
            &hidden,
            &["--show".into(), "--background".into()]
        ));
        assert!(background_activation(&["app".into(), "--autostart".into()]));
        assert!(!background_activation(&["app".into()]));
        assert!(!background_activation(&[
            "--autostart".into(),
            "--show".into()
        ]));
    }
    #[test]
    // Verify that retired unload preference is rejected
    fn retired_unload_preference_is_rejected() {
        for enabled in [false, true] {
            let error = serde_json::from_value::<DesktopSettings>(serde_json::json!({
                "notifications": true, "launch_to_tray": true, "low_memory_tray": enabled
            }))
            .unwrap_err();
            assert!(error.to_string().contains("low_memory_tray"));
        }
    }

    #[test]
    #[ignore = "Disposable configuration and loopback host only; never registers OS startup"]
    // Verify that window preferences persist without changing setup history or registering login
    fn window_preferences_persist_without_changing_setup_history_or_registering_login() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let before = runtime.snapshot();
        let progress = runtime.desktop_settings().onboarding.unwrap();
        runtime
            .save_desktop_settings(DesktopSettings {
                launch_to_tray: true,
                notifications: true,
                show_manager_shortcut: None,
                onboarding: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(runtime.desktop_settings().onboarding).unwrap(),
            serde_json::to_value(Some(progress.clone())).unwrap()
        );
        runtime
            .save_setup_progress(crate::onboarding::SetupProgress {
                step: 2,
                ..progress
            })
            .unwrap();
        assert!(runtime.desktop_settings().launch_to_tray);
        assert!(runtime.desktop_settings().notifications);
        assert_eq!(
            before.configuration_revision,
            runtime.snapshot().configuration_revision
        );
        assert_eq!(
            before.history.undo.len(),
            runtime.snapshot().history.undo.len()
        );
        let settings: DesktopSettings = runtime
            .profiles
            .storage
            .read_json(
                stream_dock_manager_storage::Domain::ManagerPreferences,
                "desktop",
                65536,
            )
            .unwrap()
            .unwrap();
        assert!(settings.launch_to_tray);
        assert!(settings.notifications);
        assert_eq!(settings.onboarding.unwrap().step, 2);
        runtime.shutdown();
    }
}

#[cfg(test)]
mod registration_language_tests {
    #[test]
    // Verify that application language matches every first party entrypoint
    fn application_language_matches_every_first_party_entrypoint() {
        let language = super::application_language();
        assert!(!language.is_empty());
        for html in [
            include_str!("../../../action-popup.html"),
            include_str!("../../../plugin-logs.html"),
        ] {
            assert!(html.contains(&format!("<html lang=\"{language}\"")));
        }
    }
}

/// Scheme-specific foreground suppression; message content remains opaque to plugins
pub fn plugin_link_hidden(url: &str) -> bool {
    let key = if url.starts_with("openaction://plugins/message/") {
        "openaction=hidden"
    } else if url.starts_with("streamdeck://plugins/message/") {
        "streamdeck=hidden"
    } else {
        return false;
    };
    url.split('#')
        .next()
        .unwrap_or(url)
        .split_once('?')
        .is_some_and(|(_, query)| query.split('&').any(|pair| pair == key))
}

#[cfg(test)]
mod plugin_link_tests {
    use super::*;
    #[test]
    // Verify that foreground suppression is scheme specific and query only
    fn foreground_suppression_is_scheme_specific_and_query_only() {
        assert!(plugin_link_hidden(
            "openaction://plugins/message/p/a?x=1&openaction=hidden"
        ));
        assert!(plugin_link_hidden(
            "streamdeck://plugins/message/p/a?streamdeck=hidden"
        ));
        for url in [
            "openaction://plugins/message/p/openaction=hidden",
            "openaction://plugins/message/p/a?streamdeck=hidden",
            "openaction://plugins/message/p/a#?openaction=hidden",
        ] {
            assert!(!plugin_link_hidden(url));
        }
    }
}
