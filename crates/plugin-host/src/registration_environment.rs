//! Trusted host presentation facts and desktop integrations. Never persisted or supplied over plugin RPC
use super::*;

#[derive(Clone, Debug, Default)]
pub struct RegistrationEnvironment {
    language: Option<String>,
    display_scale: Option<f64>,
    native_plugin_notifications: bool,
}
impl RegistrationEnvironment {
    /// Supply the host application's actual UI language and native display scale
    /// This is not the user's OS locale or the plugin's own HTML language
    /// `None` means unavailable, never a nominal English/1x fallback.
    pub fn new(locale: Option<&str>, display_scale: Option<f64>) -> Result<Self, String> {
        if display_scale.is_some_and(|scale| !scale.is_finite() || scale <= 0.0 || scale > 16.0) {
            return Err("Invalid registration display scale".into());
        }
        let language = locale.map(language_from_locale).transpose()?.flatten();
        Ok(Self {
            language,
            display_scale,
            native_plugin_notifications: false,
        })
    }

    /// The native desktop wires a presenter during startup. This advertises the
    /// supported protocol before workers can launch; no RPC can deliver until
    /// the presenter is attached and the exact permission is granted.
    pub fn with_native_plugin_notifications(mut self) -> Self {
        self.native_plugin_notifications = true;
        self
    }

    pub(super) fn allows_plugin_notifications(&self, browser: bool, declared: bool) -> bool {
        self.native_plugin_notifications && !browser && declared
    }

    // Apply for the current operation
    pub(crate) fn apply(&self, info: &mut Value, browser_context: bool) -> Result<(), String> {
        if let Some(language) = &self.language {
            info["application"]["language"] = json!(language);
        } else {
            return Err("Plugin registration requires a supported application language; no language was fabricated".into());
        }
        if let Some(scale) = self.display_scale {
            info["devicePixelRatio"] = json!(scale);
        } else if !browser_context {
            return Err("Plugin registration requires an actual application display scale; headless display context is unavailable".into());
        }
        Ok(())
    }
}

/// Vendor language identifiers are a closed upstream vocabulary. Unknown locales
/// stay unknown; locale-independent POSIX C/POSIX are not claims of English.
fn language_from_locale(locale: &str) -> Result<Option<String>, String> {
    if locale.is_empty()
        || locale.len() > 64
        || !locale
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.@".contains(&byte))
    {
        return Err("Invalid registration locale".into());
    }
    let locale = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_ascii_lowercase();
    let parts: Vec<_> = locale.split('-').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err("Invalid registration locale".into());
    }
    let language = match parts[0] {
        "de" | "en" | "es" | "fr" | "ja" | "ko" => Some(parts[0]),
        "zh" if parts
            .iter()
            .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo")) =>
        {
            Some("zh_TW")
        }
        "zh" if parts
            .iter()
            .any(|part| matches!(*part, "hans" | "cn" | "sg")) =>
        {
            Some("zh_CN")
        }
        _ => None,
    };
    Ok(language.map(str::to_owned))
}
impl PluginHost {
    /// Only the trusted native host supplies this context. A plugin/inspector
    /// cannot change another connection's locale or main-window display scale.
    pub fn set_registration_environment(&self, mut environment: RegistrationEnvironment) {
        let mut hub = self.hub.lock().unwrap();
        // Window scale/locale changes do not remove a desktop integration.
        environment.native_plugin_notifications |=
            hub.registration_environment.native_plugin_notifications;
        hub.registration_environment = environment;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify provided locale and scale are preserved and validated
    fn provided_locale_and_scale_are_preserved_and_validated() {
        for (locale, language) in [
            ("de-DE", "de"),
            ("en_US.UTF-8", "en"),
            ("zh-Hant-TW", "zh_TW"),
            ("zh_CN", "zh_CN"),
        ] {
            let environment = RegistrationEnvironment::new(Some(locale), Some(1.75)).unwrap();
            let mut info = json!({"application":{}});
            environment.apply(&mut info, false).unwrap();
            assert_eq!(info["application"]["language"], language);
            assert_eq!(info["devicePixelRatio"], 1.75);
        }
        for scale in [f64::NAN, f64::INFINITY, 0.0, -1.0, 17.0] {
            assert!(RegistrationEnvironment::new(Some("en-US"), Some(scale)).is_err());
        }
        for locale in ["", "en\nUS", "en--US", "</script>"] {
            assert!(RegistrationEnvironment::new(Some(locale), Some(1.0)).is_err());
        }
    }
    #[test]
    // Verify unknown native context fails and browser context is only staged
    fn unknown_native_context_fails_and_browser_context_is_only_staged() {
        for environment in [
            RegistrationEnvironment::default(),
            RegistrationEnvironment::new(Some("en"), None).unwrap(),
            RegistrationEnvironment::new(Some("C.UTF-8"), Some(2.0)).unwrap(),
            RegistrationEnvironment::new(Some("unsupported"), Some(2.0)).unwrap(),
        ] {
            assert!(environment
                .apply(&mut json!({"application":{}}), false)
                .is_err());
        }
        let mut staged = json!({"application":{}});
        assert!(RegistrationEnvironment::default()
            .apply(&mut staged, true)
            .is_err());
        RegistrationEnvironment::new(Some("en"), None)
            .unwrap()
            .apply(&mut staged, true)
            .unwrap();
        assert_eq!(staged["application"]["language"], "en");
        assert!(staged.get("devicePixelRatio").is_none());
    }
    #[test]
    // Verify missing native display fails before process or run state is created
    fn missing_native_display_fails_before_process_or_run_state_is_created() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.no-display");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.no-display", true).unwrap();
        host.visible_plugins.insert("fixture.no-display".into());
        host.reconcile_demand(HashMap::from([(
            "fixture.no-display".into(),
            vec![PluginDemand {
                device_id: "synthetic-display".into(),
                profile_id: "fixture".into(),
            }],
        )]));
        host.set_registration_environment(RegistrationEnvironment::new(Some("en"), None).unwrap());
        // Exercise the native branch without executing any package file
        host.plugins
            .get_mut("fixture.no-display")
            .unwrap()
            .info
            .kind = "native".into();
        let error = host.launch("fixture.no-display", true, false).unwrap_err();
        assert!(error.contains("actual application display scale"));
        let plugin = &host.plugins["fixture.no-display"];
        assert!(plugin.started.is_none());
        assert!(plugin.info.runtime.run_id.is_none());
        assert!(plugin.child.is_none());
    }

    #[test]
    fn desktop_notification_contract_is_explicit_native_and_survives_display_refresh() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::host(root.path());
        let environment = || RegistrationEnvironment::new(Some("en"), Some(2.0)).unwrap();
        assert!(!environment().allows_plugin_notifications(false, true));
        host.set_registration_environment(environment().with_native_plugin_notifications());
        host.set_registration_environment(environment());
        let hub = host.hub.lock().unwrap();
        let current = &hub.registration_environment;
        assert!(current.allows_plugin_notifications(false, true));
        assert!(!current.allows_plugin_notifications(true, true));
        assert!(!current.allows_plugin_notifications(false, false));
    }

    #[test]
    // Verify independent hosts do not share presentation context
    fn independent_hosts_do_not_share_presentation_context() {
        let root = tempfile::tempdir().unwrap();
        let first = lifecycle_tests::host(&root.path().join("first"));
        let second = lifecycle_tests::host(&root.path().join("second"));
        first.set_registration_environment(
            RegistrationEnvironment::new(Some("fr-CA"), Some(2.0)).unwrap(),
        );
        second.set_registration_environment(
            RegistrationEnvironment::new(Some("ja-JP"), Some(1.25)).unwrap(),
        );
        let a = browser::registration_info(
            &first.hub.lock().unwrap(),
            "a",
            "1.0",
            "streamdeck",
            false,
            false,
        )
        .unwrap();
        let b = browser::registration_info(
            &second.hub.lock().unwrap(),
            "b",
            "1.0",
            "streamdeck",
            false,
            false,
        )
        .unwrap();
        assert_eq!(a["application"]["language"], "fr");
        assert_eq!(a["devicePixelRatio"], 2.0);
        assert_eq!(b["application"]["language"], "ja");
        assert_eq!(b["devicePixelRatio"], 1.25);
        assert_eq!(a["application"]["platform"], browser::sdk_platform());
        #[cfg(target_os = "linux")]
        assert_eq!(a["application"]["platform"], "linux");
    }
}
