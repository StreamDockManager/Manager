use super::*;

// Construct the service used by the test
fn service() -> (tempfile::TempDir, Arc<LogCaptureService>) {
    let root = tempfile::tempdir().unwrap();
    let service = LogCaptureService::open(root.path().join("preferences.json")).unwrap();
    (root, service)
}
#[test]
// Verify network debug is opt in retains history and follows capture lifecycle
fn network_debug_is_opt_in_retains_history_and_follows_capture_lifecycle() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    assert!(!logs.debug_network("plugin"));
    logs.record("plugin", "run", "stdout", "network", "not enabled");
    let mut preferences = logs.preferences();
    preferences
        .plugins
        .entry("plugin".into())
        .or_default()
        .debug_network = true;
    logs.save_preferences(preferences.clone()).unwrap();
    assert!(logs.debug_network("plugin"));
    logs.record("plugin", "run", "stdout", "network", "HTTP GET request");
    preferences.plugins.get_mut("plugin").unwrap().debug_network = false;
    logs.save_preferences(preferences.clone()).unwrap();
    logs.record("plugin", "run", "stdout", "network", "no longer enabled");
    let page = logs.page(&capture.id, None, false).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].source, "network");
    assert_eq!(page.entries[0].message, "HTTP GET request");
    preferences.plugins.get_mut("plugin").unwrap().debug_network = true;
    logs.save_preferences(preferences).unwrap();
    logs.stop(&capture.id).unwrap();
    assert!(!logs.debug_network("plugin"));
    logs.resume(&capture.id).unwrap();
    assert!(logs.debug_network("plugin"));
}
#[test]
// Verify destroying a reader window releases its leases without stopping capture
fn destroying_a_reader_window_releases_its_leases_without_stopping_capture() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    let main = logs.subscribe_owned(&capture.id, "main").unwrap();
    let popup = logs.subscribe_owned(&capture.id, "log-reader-1").unwrap();
    logs.unsubscribe_owner("log-reader-1");
    assert!(logs.read(&popup, None, false).is_err());
    assert!(logs.list()[0].capturing);
    logs.record("plugin", "run", "stdout", "rpc", "after popup close");
    assert_eq!(
        logs.read(&main, None, false).unwrap().entries[0].message,
        "after popup close"
    );
    // Closed window subscriptions cannot exhaust the reader limit on reopen
    for _ in 0..32 {
        logs.subscribe_owned(&capture.id, "log-reader-2").unwrap();
        logs.unsubscribe_owner("log-reader-2");
    }
}
#[test]
// Verify capture lifetime is independent of readers and stop resume appends
fn capture_lifetime_is_independent_of_readers_and_stop_resume_appends() {
    let (_root, logs) = service();
    logs.record("plugin", "run1", "stdout", "rpc", "discarded");
    let capture = logs.start("plugin", "Plugin").unwrap();
    let lease = logs.subscribe(&capture.id).unwrap();
    logs.record("plugin", "run1", "stdout", "rpc", "one");
    let page = logs.read(&lease, None, false).unwrap();
    assert_eq!(page.entries[0].message, "one");
    logs.unsubscribe(&lease.id);
    assert!(logs.read(&lease, None, false).is_err());
    logs.record("plugin", "run1", "stderr", "process", "hidden error");
    logs.stop(&capture.id).unwrap();
    logs.record("plugin", "run1", "stdout", "rpc", "not captured");
    assert!(!logs.start("plugin", "Plugin").unwrap().capturing);
    logs.resume(&capture.id).unwrap();
    logs.record("plugin", "run2", "stdout", "rpc", "two");
    logs.stop(&capture.id).unwrap();
    let page = logs.page(&capture.id, Some(page.next), true).unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        ["hidden error", "Capture resumed", "two"]
    );
    assert_eq!(page.entries[2].run_id, "run2");
    assert!(page
        .entries
        .windows(2)
        .all(|p| p[1].sequence == p[0].sequence + 1));
}
#[test]
// Verify pages and exports cover the full capture without duplicates
fn pages_and_exports_cover_the_full_capture_without_duplicates() {
    let (root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    for i in 0..1000 {
        logs.record(
            "plugin",
            "run",
            if i % 2 == 0 { "stdout" } else { "stderr" },
            "rpc",
            &format!("entry {i}"),
        );
    }
    logs.stop(&capture.id).unwrap();
    let mut page = logs.page(&capture.id, None, false).unwrap();
    assert_eq!(page.entries.last().unwrap().sequence, 1000);
    let mut all = page.entries.clone();
    while let Some(cursor) = page.earlier {
        page = logs.page(&capture.id, Some(cursor), false).unwrap();
        let mut entries = page.entries.clone();
        entries.extend(all);
        all = entries;
    }
    assert_eq!(all.len(), 1000);
    for (i, e) in all.iter().enumerate() {
        assert_eq!(e.sequence, i as u64 + 1);
    }
    let destination = root.path().join("export.jsonl");
    logs.export(&capture.id, &destination).unwrap();
    let exported = std::fs::read_to_string(destination).unwrap();
    assert_eq!(exported.lines().count(), 1000);
    assert!(exported.contains("entry 0") && exported.contains("entry 999"));
    let mut bad = page.next;
    bad.generation += 1;
    assert!(logs.page(&capture.id, Some(bad), true).is_err());
}
#[test]
// Verify redaction precedes continuations and stream preferences keep history
fn redaction_precedes_continuations_and_stream_preferences_keep_history() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    logs.set_secrets(vec!["secret-token".into()]);
    let message = format!("{}secret-token{}", "🔒".repeat(2047), "z".repeat(10000));
    logs.record("plugin", "run", "stderr", "rpc", &message);
    let mut preferences = logs.preferences();
    preferences.plugins.insert(
        "plugin".into(),
        LogPluginPreferences {
            stdout: false,
            ..Default::default()
        },
    );
    logs.save_preferences(preferences.clone()).unwrap();
    logs.record("plugin", "run", "stdout", "rpc", "discarded");
    preferences.enabled = false;
    logs.save_preferences(preferences.clone()).unwrap();
    let page = logs.page(&capture.id, None, false).unwrap();
    assert!(page.entries.len() > 1 && page.entries[1].continuation);
    let joined: String = page.entries.iter().map(|e| e.message.as_str()).collect();
    assert!(!joined.contains("secret-token") && !joined.contains("discarded"));
    assert_eq!(
        joined,
        crate::redact_known_values(&message, &["secret-token".into()])
    );
    preferences.enabled = true;
    logs.save_preferences(preferences).unwrap();
    assert!(!logs.list()[0].capturing);
    logs.clear(&capture.id).unwrap();
    assert!(logs.list().is_empty());
}
#[test]
// Verify queue overload is explicit and keeps accepted data
fn queue_overload_is_explicit_and_keeps_accepted_data() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    // One producer operation exceeds its bounded queue before the worker can drain
    logs.record("plugin", "run", "stdout", "rpc", &"x".repeat(QUEUE * 2));
    let state = logs.list().remove(0);
    assert!(state.incomplete && !state.capturing);
    assert!(state.error.unwrap().contains("overloaded"));
    logs.stop(&capture.id).unwrap();
    assert!(logs.list()[0].bytes > 0);
    assert!(logs.resume(&capture.id).is_err());
}
#[test]
// Verify raw pipes preserve stream utf8 and secrets across read boundaries
fn raw_pipes_preserve_stream_utf8_and_secrets_across_read_boundaries() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    logs.set_secrets(vec!["secret-token".into()]);
    let input = format!("{}secret-token🔒\n", "a".repeat(FRAME - 5));
    logs.drain_pipe(
        "plugin",
        "run",
        "stderr",
        std::io::Cursor::new(input.as_bytes()),
    );
    logs.stop(&capture.id).unwrap();
    let page = logs.page(&capture.id, None, false).unwrap();
    let text: String = page.entries.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        text,
        crate::redact_known_values(&input, &["secret-token".into()])
    );
    assert!(page
        .entries
        .iter()
        .all(|e| e.stream == "stderr" && e.source == "process"));
}
#[test]
// Verify subthreshold batches wait for the timer or a flush barrier
fn subthreshold_batches_wait_for_the_timer_or_a_flush_barrier() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("capture");
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .open(&path)
        .unwrap();
    let mut state = Shared::default();
    state.captures.insert(
        "plugin".into(),
        Pending {
            status: LogCaptureStatus {
                id: "capture".into(),
                plugin_id: "plugin".into(),
                name: "Plugin".into(),
                capturing: true,
                waiting: false,
                incomplete: false,
                error: None,
                generation: 1,
                bytes: 0,
                sequence: 0,
            },
            entries: VecDeque::new(),
            queued_bytes: 0,
        },
    );
    enqueue(
        state.captures.get_mut("plugin").unwrap(),
        "stdout",
        "rpc",
        "run",
        "small message",
        false,
    );
    let state = Mutex::new(state);
    let mut files = BTreeMap::from([("plugin".into(), Disk::new(file))]);
    flush(&state, &mut files, false);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert!(!files["plugin"].staged.is_empty());
    flush(&state, &mut files, true);
    assert!(std::fs::read_to_string(path)
        .unwrap()
        .contains("small message"));
    assert_eq!(state.lock().unwrap().writer_calls, 1);
}
#[test]
// Verify captures are independent and logging preference corruption cannot block plugins
fn captures_are_independent_and_logging_preference_corruption_cannot_block_plugins() {
    let (_root, logs) = service();
    let first = logs.start("one", "One").unwrap();
    let second = logs.start("two", "Two").unwrap();
    logs.record("one", "run", "stdout", "rpc", "first");
    logs.stop(&first.id).unwrap();
    logs.record("two", "run", "stderr", "rpc", "second");
    assert_eq!(
        logs.page(&second.id, None, false).unwrap().entries[0].message,
        "second"
    );
    assert!(
        logs.list()
            .iter()
            .find(|c| c.id == second.id)
            .unwrap()
            .capturing
    );
    logs.mark_incomplete(
        "two",
        "SDK logging queue overloaded; capture is incomplete.",
    );
    let second = logs.page(&second.id, None, false).unwrap();
    assert!(second.status.incomplete && !second.status.capturing);
    assert_eq!(second.entries[0].message, "second");
    logs.storage
        .write_json(
            stream_dock_manager_storage::Domain::LoggingPreferences,
            "preferences",
            &serde_json::json!([]),
            1024,
        )
        .unwrap();
    assert!(LogCaptureService::with_storage(logs.storage.clone()).is_err());
    logs.save_preferences(LogPreferences::default()).unwrap();
    assert!(LogCaptureService::with_storage(logs.storage.clone()).is_ok());
}
#[test]
#[cfg(unix)]
// Verify capture directory is private independently of the user umask
fn capture_directory_is_private_independently_of_the_user_umask() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let directory = private_directory(&root.path().join("logs")).unwrap();
    assert_eq!(
        std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
#[test]
// Verify captures use application logs and only remove their own session
fn captures_use_application_logs_and_only_remove_their_own_session() {
    let (root, logs) = service();
    let directory = root.path().join("logs");
    assert!(!directory.exists(), "Capture storage is created on demand");
    std::fs::create_dir_all(directory.join("plugin-captures-previous")).unwrap();
    std::fs::write(directory.join("manager.log"), "Manager history").unwrap();
    std::fs::write(
        directory.join("plugin-captures-previous/retained"),
        "Previous capture",
    )
    .unwrap();
    let capture = logs.start("plugin", "Plugin").unwrap();
    logs.record("plugin", "run", "stderr", "process", "Captured failure");
    logs.stop(&capture.id).unwrap();
    let session = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join(&capture.id).is_file())
        .unwrap();
    assert!(session
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("plugin-captures-"));
    assert!(std::fs::read_to_string(session.join(&capture.id))
        .unwrap()
        .contains("Captured failure"));
    let export = root.path().join("export.jsonl");
    logs.export(&capture.id, &export).unwrap();
    assert!(std::fs::read_to_string(export)
        .unwrap()
        .contains("Captured failure"));
    logs.shutdown();
    assert!(!session.exists());
    assert_eq!(
        std::fs::read_to_string(directory.join("manager.log")).unwrap(),
        "Manager history"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("plugin-captures-previous/retained")).unwrap(),
        "Previous capture"
    );
}
#[test]
// Verify unusable application log directory reports failure without falling back to temp
fn unusable_application_log_directory_reports_failure_without_falling_back_to_temp() {
    let (root, logs) = service();
    let directory = root.path().join("logs");
    std::fs::write(&directory, "User file").unwrap();
    assert!(logs.start("plugin", "Plugin").is_err());
    assert!(logs.list().is_empty());
    assert_eq!(std::fs::read_to_string(&directory).unwrap(), "User file");
    std::fs::remove_file(&directory).unwrap();
    assert!(logs.start("plugin", "Plugin").is_ok());
}
#[test]
// Verify limits and write failures stop capture without evicting files
fn limits_and_write_failures_stop_capture_without_evicting_files() {
    for global in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(root.path().join("capture"))
            .unwrap();
        let mut shared = Shared::default();
        let bytes = if global {
            PLUGIN_LIMIT / 2
        } else {
            PLUGIN_LIMIT
        };
        shared.captures.insert(
            "plugin".into(),
            Pending {
                status: LogCaptureStatus {
                    id: "capture".into(),
                    plugin_id: "plugin".into(),
                    name: "Plugin".into(),
                    capturing: true,
                    waiting: false,
                    incomplete: false,
                    error: None,
                    generation: 1,
                    bytes,
                    sequence: 0,
                },
                entries: VecDeque::new(),
                queued_bytes: 0,
            },
        );
        if global {
            let mut other = shared.captures["plugin"].status.clone();
            other.id = "other".into();
            other.plugin_id = "other".into();
            other.bytes = SESSION_LIMIT - PLUGIN_LIMIT / 2;
            other.capturing = false;
            shared.captures.insert(
                "other".into(),
                Pending {
                    status: other,
                    entries: VecDeque::new(),
                    queued_bytes: 0,
                },
            );
        }
        enqueue(
            shared.captures.get_mut("plugin").unwrap(),
            "stdout",
            "rpc",
            "run",
            "entry",
            false,
        );
        let shared = Mutex::new(shared);
        let mut files = BTreeMap::from([("plugin".into(), Disk::new(file))]);
        flush(&shared, &mut files, true);
        let shared = shared.lock().unwrap();
        assert!(shared.captures["plugin"].status.incomplete);
        assert!(!shared.captures["plugin"].status.capturing);
        if global {
            assert!(shared.captures["plugin"]
                .status
                .error
                .as_ref()
                .unwrap()
                .contains("1 GiB"));
            assert!(!shared.captures["other"].status.incomplete);
        }

        assert!(root.path().join("capture").exists());
    }
    // A read-only descriptor injects a real write error on every platform
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("capture"), "retained\n").unwrap();
    let file = File::open(root.path().join("capture")).unwrap();
    let (_preferences, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    logs.record("plugin", "run", "stdout", "rpc", "new entry");
    let mut files = BTreeMap::from([("plugin".into(), Disk::new(file))]);
    flush(&logs.shared, &mut files, true);
    assert!(logs.list()[0]
        .error
        .as_ref()
        .unwrap()
        .contains("Cannot write"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("capture")).unwrap(),
        "retained\n"
    );
    assert!(logs.resume(&capture.id).is_err());
}

#[test]
#[ignore = "Explicit bounded resource probe; no process execution or hardware"]
#[cfg(unix)]
// Verify capture resource probe
fn capture_resource_probe() {
    // Read the storage usage recorded by this fixture
    fn usage() -> (f64, u64) {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
        let cpu = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.;
        let multiplier = if cfg!(target_os = "macos") { 1 } else { 1024 };
        (cpu, usage.ru_maxrss as u64 * multiplier)
    }
    for mode in ["off", "hidden", "visible"] {
        let (_root, logs) = service();
        let capture = (mode != "off").then(|| logs.start("plugin", "Plugin").unwrap());
        let lease =
            (mode == "visible").then(|| logs.subscribe(&capture.as_ref().unwrap().id).unwrap());
        let (cpu, _) = usage();
        let started = std::time::Instant::now();
        let mut displays = 0;
        for action in 0..2000 {
            logs.record(
                "plugin",
                "run",
                if action % 10 == 0 { "stderr" } else { "stdout" },
                "rpc",
                &format!("Action {action} completed · {}", "x".repeat(128)),
            );
            if action % 200 == 0 {
                hardware_core::feedback::draw(&serde_json::json!({"items":[{"key":"value","type":"text","rect":[0,0,200,100],"font":{"size":30,"fit":true}}]}), &serde_json::json!({"value":action.to_string()}), 64,64,0).unwrap();
                displays += 1;
                if let Some(lease) = &lease {
                    logs.read(lease, None, false).unwrap();
                }
            }
        }
        if let Some(capture) = &capture {
            logs.stop(&capture.id).unwrap();
            assert!(!logs.list()[0].incomplete);
        }
        let state = logs.shared.lock().unwrap();
        let (after_cpu, resident) = usage();
        println!(
            "{}",
            serde_json::json!({"mode":mode,"actions":2000,"display_renders":displays,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,"cpu_ms":after_cpu-cpu,"process_peak_resident_bytes":resident,"writer_calls":state.writer_calls,"reader_calls":state.reader_calls,"stored_bytes":state.captures.values().map(|p|p.status.bytes).sum::<u64>()})
        );
        if mode != "visible" {
            assert_eq!(state.reader_calls, 0);
        }
        assert!(state.writer_calls < 100);
    }
}

#[test]
// Verify network bodies are redacted on disk loaded lazily and require a current reader
fn network_bodies_are_redacted_on_disk_loaded_lazily_and_require_a_current_reader() {
    use base64::Engine;
    let (root, logs) = service();
    logs.set_secrets(vec!["known-secret".into()]);
    let mut preferences = logs.preferences();
    preferences
        .plugins
        .entry("plugin".into())
        .or_default()
        .debug_network = true;
    logs.save_preferences(preferences.clone()).unwrap();
    let capture = logs.start("plugin", "Plugin").unwrap();
    let binary = base64::engine::general_purpose::STANDARD.encode(b"\0known-secret\xff");
    logs.record_network(
        "plugin",
        "run",
        "stdout",
        "POST service.test/endpoint · HTTP 200",
        LogNetworkDetail {
            request_id: "1".into(),
            request_headers: None,
            response_headers: None,
            request: LogNetworkBody {
                text: "{\"value\":\"known-secret\",\"other\":\"visible\"}".into(),
                encoding: "utf8".into(),
                complete: true,
                ..Default::default()
            },
            response: LogNetworkBody {
                text: binary,
                encoding: "base64".into(),
                complete: true,
                ..Default::default()
            },
        },
    );
    let lease = logs.subscribe(&capture.id).unwrap();
    let page = logs.read(&lease, None, false).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].network_request_id.as_deref(), Some("1"));
    assert!(!serde_json::to_string(&page).unwrap().contains("visible"));
    let cursor = page.entries[0].cursor.as_ref().unwrap();
    preferences.plugins.get_mut("plugin").unwrap().debug_network = false;
    logs.save_preferences(preferences).unwrap();
    let detail = logs.network_detail(&lease, cursor).unwrap();
    assert!(detail.request.text.contains("visible"));
    assert!(!detail.request.text.contains("known-secret"));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(detail.response.text)
            .unwrap(),
        b"\0[credential]\xff"
    );
    let mut forged = cursor.clone();
    forged.offset += 1;
    assert!(logs.network_detail(&lease, &forged).is_err());
    forged = cursor.clone();
    forged.capture_id = "another-capture".into();
    assert!(logs.network_detail(&lease, &forged).is_err());
    let export = root.path().join("network.jsonl");
    logs.export(&capture.id, &export).unwrap();
    let exported = std::fs::read_to_string(export).unwrap();
    assert!(exported.contains("visible") && !exported.contains("known-secret"));
    logs.unsubscribe(&lease.id);
    assert!(logs.network_detail(&lease, cursor).is_err());
}
#[test]
// Verify oversized network bodies are explicitly omitted and do not expand log pages
fn oversized_network_bodies_are_explicitly_omitted_and_do_not_expand_log_pages() {
    let (_root, logs) = service();
    let mut preferences = logs.preferences();
    preferences
        .plugins
        .entry("plugin".into())
        .or_default()
        .debug_network = true;
    logs.save_preferences(preferences).unwrap();
    let capture = logs.start("plugin", "Plugin").unwrap();
    logs.record_network(
        "plugin",
        "run",
        "stdout",
        "Request 2",
        LogNetworkDetail {
            request_id: "2".into(),
            request_headers: None,
            response_headers: None,
            request: LogNetworkBody {
                text: "x".repeat(65537),
                encoding: "utf8".into(),
                complete: true,
                bytes: 65537,
                ..Default::default()
            },
            response: LogNetworkBody::default(),
        },
    );
    let lease = logs.subscribe(&capture.id).unwrap();
    let page = logs.read(&lease, None, false).unwrap();
    assert!(serde_json::to_string(&page).unwrap().len() < 2048);
    let detail = logs
        .network_detail(&lease, page.entries[0].cursor.as_ref().unwrap())
        .unwrap();
    assert!(detail.request.truncated && detail.request.text.is_empty());
    assert_eq!(detail.request.bytes, 65537);
}

#[test]
// Verify network headers are redacted before disk and do not expand log pages
fn network_headers_are_redacted_before_disk_and_do_not_expand_log_pages() {
    let (root, logs) = service();
    logs.set_secrets(vec!["known-header-secret".into()]);
    let mut preferences = logs.preferences();
    preferences
        .plugins
        .entry("plugin".into())
        .or_default()
        .debug_network = true;
    logs.save_preferences(preferences).unwrap();
    let capture = logs.start("plugin", "Plugin").unwrap();
    let fields = LogNetworkHeaders {
        entries: vec![
            LogNetworkHeader {
                name: "aUtHoRiZaTiOn".into(),
                values: vec!["Bearer raw-authorization".into()],
            },
            LogNetworkHeader {
                name: "Set-Cookie".into(),
                values: vec!["session=raw-cookie".into(), "other=second-cookie".into()],
            },
            LogNetworkHeader {
                name: "X-API-Key".into(),
                values: vec!["raw-api-key".into()],
            },
            LogNetworkHeader {
                name: "X-Trace".into(),
                values: vec!["first".into(), "second known-header-secret".into()],
            },
            LogNetworkHeader {
                name: "Access-Control-Allow-Credentials".into(),
                values: vec!["true".into()],
            },
            LogNetworkHeader {
                name: "X-Oversized".into(),
                values: vec!["header-private".repeat(8192)],
            },
        ],
        truncated: false,
    };
    logs.record_network(
        "plugin",
        "run",
        "stdout",
        "GET service.test",
        LogNetworkDetail {
            request_id: "1".into(),
            request_headers: Some(fields.clone()),
            response_headers: Some(fields),
            ..Default::default()
        },
    );
    let lease = logs.subscribe(&capture.id).unwrap();
    let page = logs.read(&lease, None, false).unwrap();
    assert!(!serde_json::to_string(&page).unwrap().contains("X-Trace"));
    let detail = logs
        .network_detail(&lease, page.entries[0].cursor.as_ref().unwrap())
        .unwrap();
    for headers in [
        detail.request_headers.unwrap(),
        detail.response_headers.unwrap(),
    ] {
        assert!(headers.truncated);
        assert!(headers
            .entries
            .iter()
            .any(|header| header.name == "X-Trace"
                && header.values == ["first", "second [credential]"]));
        assert!(headers
            .entries
            .iter()
            .any(|header| header.name == "Access-Control-Allow-Credentials"
                && header.values == ["true"]));
    }
    let export = root.path().join("headers.jsonl");
    logs.export(&capture.id, &export).unwrap();
    let text = std::fs::read_to_string(export).unwrap();
    for private in [
        "raw-authorization",
        "raw-cookie",
        "second-cookie",
        "raw-api-key",
        "known-header-secret",
        "header-private",
    ] {
        assert!(!text.contains(private), "{private}");
    }
}
#[test]
// Verify body only network details from older sdks remain readable
fn body_only_network_details_from_older_sdks_remain_readable() {
    let detail: LogNetworkDetail =
        serde_json::from_str(r#"{"request_id":"old","request":{},"response":{}}"#).unwrap();
    assert!(detail.request_headers.is_none() && detail.response_headers.is_none());
}

#[test]
// Verify editor retirement capture query preserves stopped capture and independent reader
fn editor_retirement_capture_query_preserves_stopped_capture_and_independent_reader() {
    let (_root, logs) = service();
    let capture = logs.start("plugin", "Plugin").unwrap();
    let reader = logs
        .subscribe_owned(&capture.id, "auxiliary-log-reader")
        .unwrap();
    assert!(logs.list().iter().any(|capture| capture.capturing));
    logs.stop(&capture.id).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while logs.list().iter().any(|capture| capture.capturing) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(logs.read(&reader, None, false).is_ok());
    logs.resume(&capture.id).unwrap();
    assert!(logs.list().iter().any(|capture| capture.capturing));
}
