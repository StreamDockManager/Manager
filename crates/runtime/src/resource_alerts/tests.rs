use super::*;
// Collect a tests sample
fn sample(cpu: Option<f64>, memory: u64) -> PluginResources {
    PluginResources {
        state: MeasurementState::Available,
        source: MeasurementSource::ProcessTreeRss,
        root_pid: Some(123),
        cpu_percent: cpu,
        resident_bytes: Some(memory * 1024 * 1024),
        ..Default::default()
    }
}
// Construct enabled CPU and memory thresholds for resource-alert tests
fn prefs() -> ResourceAlertPreferences {
    ResourceAlertPreferences {
        enabled: true,
        cpu_percent: Some(80.0),
        memory_mib: Some(512),
    }
}
#[test]
#[ignore = "Requires a disposable loopback host; preferences only, no plugin execution"]
// Verify that native preferences survive restart without workspace edits history or exports
fn native_preferences_survive_restart_without_workspace_edits_history_or_exports() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let id = "org.test.resource.alerts";
    assert!(runtime.resource_alert_settings(id).is_err());
    let source = root.path().join("source").join(id);
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("manifest.json"), serde_json::to_vec(&serde_json::json!({"UUID":id,"Name":"Alerts fixture","Version":"1.0.0.0","Author":"Stream Dock Manager contributors","Description":"Disposable resource-alert fixture","Icon":"icon","OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],"Software":{"MinimumVersion":"6.9"},"CodePath":"runtime.html","SDKVersion":2,"Actions":[{"UUID":format!("{id}.action"),"Name":"Action","Icon":"icon","States":[{"Image":"icon"}]}]})).unwrap()).unwrap();
    std::fs::write(
        source.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    let review = runtime.prepare_plugin_package(&source).unwrap();
    runtime
        .commit_plugin_package(&review.id, false, true)
        .unwrap();
    let before = runtime.snapshot();
    let mut value = runtime.resource_alert_settings(id).unwrap();
    assert!(!value.preferences.enabled);
    value.preferences.enabled = true;
    value.preferences.cpu_percent = Some(25.0);
    value.preferences.memory_mib = None;
    let saved = runtime.save_resource_alert_settings(value.clone()).unwrap();
    assert!(runtime.save_resource_alert_settings(value).is_err());
    assert_eq!(
        runtime.snapshot().configuration_revision,
        before.configuration_revision
    );
    assert_eq!(
        serde_json::to_value(runtime.snapshot().history).unwrap(),
        serde_json::to_value(before.history).unwrap()
    );
    let archive = root.path().join("portable.zip");
    runtime.profiles.export(&archive).unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
    assert!((0..zip.len()).all(|index| !zip
        .by_index(index)
        .unwrap()
        .name()
        .unwrap()
        .contains("resource-alert")));
    assert!(runtime
        .plugins
        .lock()
        .unwrap()
        .list()
        .iter()
        .all(|p| !p.enabled));
    runtime.shutdown();
    drop(runtime);
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let reloaded = runtime.resource_alert_settings(id).unwrap();
    assert_eq!(reloaded.revision, saved.revision);
    assert_eq!(reloaded.preferences, saved.preferences);
    runtime.shutdown();
}
// Observe a fixture resource sample at the requested elapsed time and run identity
fn at(
    incident: &mut Incident,
    base: Instant,
    t: u64,
    run: &str,
    uptime: u64,
    resources: &PluginResources,
) -> Option<String> {
    incident.observe(
        &prefs(),
        run,
        uptime,
        base + Duration::from_secs(t),
        resources,
    )
}
#[test]
// Verify that startup grace and sustained threshold emit one warning per incident
fn startup_grace_and_sustained_threshold_emit_one_warning_per_incident() {
    let base = Instant::now();
    let mut incident = Incident::default();
    let high = sample(Some(90.0), 100);
    for t in (0..60).step_by(5) {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
    let warning = at(&mut incident, base, 60, "one", 60_000, &high).unwrap();
    assert!(warning.contains("CPU 90.0%"));
    assert!(!warning.contains("resident memory"));
    for t in (65..=600).step_by(5) {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
}
#[test]
// Verify that recovery rearms but five minute cooldown still applies
fn recovery_rearms_but_five_minute_cooldown_still_applies() {
    let base = Instant::now();
    let mut incident = Incident::default();
    let high = sample(Some(90.0), 100);
    let low = sample(Some(10.0), 100);
    for t in (0..60).step_by(5) {
        at(&mut incident, base, t, "one", t * 1000, &high);
    }
    assert!(at(&mut incident, base, 60, "one", 60_000, &high).is_some());
    assert!(at(&mut incident, base, 65, "one", 65_000, &low).is_none());
    for t in (70..360).step_by(5) {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
    assert!(at(&mut incident, base, 360, "one", 360_000, &high).is_some());
}
#[test]
// Verify that gaps and unavailable measurements reset sustained timing without false rearming
fn gaps_and_unavailable_measurements_reset_sustained_timing_without_false_rearming() {
    let base = Instant::now();
    let mut incident = Incident::default();
    let high = sample(Some(90.0), 100);
    for t in [30, 35, 40, 45, 50] {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
    // A fifteen-second sampling gap cannot count toward the sustained threshold
    for t in [65, 70, 75, 80, 85, 90] {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
    assert!(at(&mut incident, base, 95, "one", 95_000, &high).is_some());
    let mut unavailable = high.clone();
    unavailable.state = MeasurementState::Stale;
    assert!(at(&mut incident, base, 100, "one", 100_000, &unavailable).is_none());
    for t in (105..=600).step_by(5) {
        assert!(at(&mut incident, base, t, "one", t * 1000, &high).is_none());
    }
}
#[test]
// Verify that process instance changes restart grace and timing but preserve cooldown
fn process_instance_changes_restart_grace_and_timing_but_preserve_cooldown() {
    let base = Instant::now();
    let mut incident = Incident::default();
    let high = sample(Some(90.0), 100);
    for t in (0..60).step_by(5) {
        at(&mut incident, base, t, "one", t * 1000, &high);
    }
    assert!(at(&mut incident, base, 60, "one", 60_000, &high).is_some());
    for t in (65..360).step_by(5) {
        assert!(at(&mut incident, base, t, "two", (t - 65) * 1000, &high).is_none());
    }
    assert!(at(&mut incident, base, 360, "two", 295_000, &high).is_some());
}
#[test]
// Verify that thresholds are sustained independently and shared webviews are unavailable
fn thresholds_are_sustained_independently_and_shared_webviews_are_unavailable() {
    let base = Instant::now();
    let mut incident = Incident::default();
    let cpu = sample(Some(90.0), 100);
    let memory = sample(Some(10.0), 600);
    for t in [30, 35, 40, 45] {
        assert!(at(&mut incident, base, t, "one", t * 1000, &cpu).is_none());
    }
    for t in [50, 55, 60, 65, 70, 75] {
        assert!(at(&mut incident, base, t, "one", t * 1000, &memory).is_none());
    }
    let warning = at(&mut incident, base, 80, "one", 80_000, &memory).unwrap();
    assert!(warning.contains("resident memory 600 MiB"));
    assert!(!warning.contains("CPU 10"));
    let mut incident = Incident::default();
    let mut shared = sample(Some(100.0), 1000);
    shared.source = MeasurementSource::SharedWebview;
    for t in (0..=600).step_by(5) {
        assert!(at(&mut incident, base, t, "one", t * 1000, &shared).is_none());
    }
}
#[test]
// Verify that preferences are opt in revision checked preserve disabled values and keep invalid files
fn preferences_are_opt_in_revision_checked_preserve_disabled_values_and_keep_invalid_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resource-alerts.json");
    let mut alerts = ResourceAlerts::open(path.clone());
    let mut settings = alerts.settings("plugin").unwrap();
    assert!(!settings.preferences.enabled);
    assert!(!path.exists());
    assert!(alerts
        .observe(
            "plugin",
            "run",
            60_000,
            Instant::now(),
            &sample(Some(100.0), 1000)
        )
        .is_none());
    assert!(!alerts.has_incidents());
    settings.preferences.cpu_percent = Some(5.0);
    settings.preferences.memory_mib = Some(128);
    let saved = alerts.save(settings.clone()).unwrap();
    assert!(alerts.save(settings).is_err());
    let mut enabled = saved;
    enabled.preferences.enabled = true;
    let mut disabled = alerts.save(enabled).unwrap();
    disabled.preferences.enabled = false;
    let disabled = alerts.save(disabled).unwrap();
    assert_eq!(disabled.preferences.cpu_percent, Some(5.0));
    assert_eq!(disabled.preferences.memory_mib, Some(128));
    let mut invalid = disabled.clone();
    invalid.preferences.cpu_percent = Some(f64::NAN);
    assert!(alerts.save(invalid).is_err());
    alerts
        .storage
        .write_json(
            stream_dock_manager_storage::Domain::LocalPreferences,
            "resource-alerts",
            &serde_json::json!(["invalid preferences"]),
            1024,
        )
        .unwrap();
    assert!(alerts.settings("plugin").is_err());
    assert!(alerts.save(disabled).is_err());
    assert_eq!(
        alerts
            .storage
            .read_json::<serde_json::Value>(
                stream_dock_manager_storage::Domain::LocalPreferences,
                "resource-alerts",
                1024
            )
            .unwrap(),
        Some(serde_json::json!(["invalid preferences"]))
    );
    assert!(alerts
        .observe(
            "plugin",
            "run",
            60_000,
            Instant::now(),
            &sample(Some(100.0), 1000)
        )
        .is_none());
}
#[test]
// Verify that ending a run clears timing and empty disabled tracking adds no incidents
fn ending_a_run_clears_timing_and_empty_disabled_tracking_adds_no_incidents() {
    let root = tempfile::tempdir().unwrap();
    let mut alerts = ResourceAlerts::open(root.path().join("alerts.json"));
    let mut settings = alerts.settings("plugin").unwrap();
    settings.preferences.enabled = true;
    alerts.save(settings).unwrap();
    let base = Instant::now();
    let high = sample(Some(90.0), 100);
    for t in [30, 35, 40, 45, 50] {
        alerts.observe(
            "plugin",
            "one",
            t * 1000,
            base + Duration::from_secs(t),
            &high,
        );
    }
    alerts.retain_runs(&HashSet::new(), base + Duration::from_secs(55));
    assert!(!alerts.has_incidents());
    assert!(alerts
        .observe("plugin", "two", 0, base + Duration::from_secs(60), &high)
        .is_none());
}
