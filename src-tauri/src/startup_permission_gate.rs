//! Development-only acknowledgements never grant OS access; an input skip permits only a HID attempt.
use std::{collections::BTreeSet, sync::Mutex};
use stream_dock_manager_runtime::startup_permissions::StartupPermissionReport;

static SKIPPED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

// Decorate a passive report while preserving a prior development-only input acknowledgement
pub(crate) fn report(runtime: &stream_dock_manager_runtime::Runtime) -> StartupPermissionReport {
    let mut skipped = SKIPPED.lock().unwrap();
    let development = tauri::is_dev();
    let report = runtime.startup_permissions_with_input_monitoring_skip(
        false,
        development && skipped.contains("input_monitoring"),
    );
    apply(report, development, &mut skipped, None)
        .expect("passive permission report cannot reject a skip")
}

// Validate each explicit acknowledgement before allowing a final HID admission attempt
pub(crate) fn complete(
    runtime: &stream_dock_manager_runtime::Runtime,
    skip: Option<&str>,
) -> Result<StartupPermissionReport, String> {
    let mut skipped = SKIPPED.lock().unwrap();
    let development = tauri::is_dev();
    apply(
        stream_dock_manager_runtime::startup_permissions::check(),
        development,
        &mut skipped,
        skip,
    )?;
    let report = runtime.startup_permissions_with_input_monitoring_skip(
        skip.is_none(),
        development && skipped.contains("input_monitoring"),
    );
    apply(report, development, &mut skipped, None)
}

fn apply(
    mut report: StartupPermissionReport,
    development: bool,
    skipped: &mut BTreeSet<String>,
    skip: Option<&str>,
) -> Result<StartupPermissionReport, String> {
    if let Some(id) = skip {
        if !development {
            return Err(
                "Skipping startup permission checks is available only in development mode".into(),
            );
        }
        if !report.permissions.iter().any(|p| p.id == id && p.required) {
            return Err("Unknown or optional startup permission".into());
        }
        skipped.insert(id.into());
    }
    report.development_mode = development;
    report.skipped_permissions = if development {
        report
            .permissions
            .iter()
            .filter(|p| skipped.contains(&p.id))
            .map(|p| p.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    report.setup_ready = report
        .permissions
        .iter()
        .all(|p| !p.required || p.status == "granted" || (development && skipped.contains(&p.id)));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_runtime::startup_permissions::StartupPermission;
    fn missing() -> StartupPermissionReport {
        StartupPermissionReport {
            platform: "macos".into(),
            ready: false,
            setup_ready: false,
            development_mode: false,
            skipped_permissions: vec![],
            device_connections_enabled: false,
            permissions: ["input_monitoring", "accessibility"]
                .into_iter()
                .map(|id| StartupPermission {
                    id: id.into(),
                    title: id.into(),
                    description: String::new(),
                    instructions: String::new(),
                    status: "missing".into(),
                    required: true,
                    settings_available: true,
                })
                .collect(),
        }
    }
    #[test]
    fn per_item_skips_never_grant_real_capabilities_and_reset_with_session() {
        let mut skips = BTreeSet::new();
        let one = apply(missing(), true, &mut skips, Some("accessibility")).unwrap();
        assert!(!one.setup_ready);
        let both = apply(missing(), true, &mut skips, Some("input_monitoring")).unwrap();
        assert!(both.setup_ready);
        assert!(!both.ready && !both.device_connections_enabled);
        assert!(both.permissions.iter().all(|p| p.status == "missing"));
        assert!(
            apply(missing(), true, &mut skips, None)
                .unwrap()
                .setup_ready
        );
        assert!(
            !apply(missing(), true, &mut BTreeSet::new(), None)
                .unwrap()
                .setup_ready
        );
        assert!(apply(missing(), true, &mut skips, Some("unknown")).is_err());
    }
    #[test]
    fn production_rejects_requests_and_ignores_development_session_state() {
        let mut skips = BTreeSet::from(["input_monitoring".into(), "accessibility".into()]);
        assert!(apply(missing(), false, &mut skips, Some("accessibility")).is_err());
        let report = apply(missing(), false, &mut skips, None).unwrap();
        assert!(!report.setup_ready && !report.ready && !report.development_mode);
        assert!(report.skipped_permissions.is_empty());
        assert!(!report.device_connections_enabled);
    }
}
