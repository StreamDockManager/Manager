use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stream_dock_manager_plugin_host::manager_diagnostics::{
    self as diagnostics, FrontendDiagnostic,
};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

const MAX_FILE_SIZE: u128 = 5 * 1024 * 1024;
static SESSION: Mutex<Option<SessionFile>> = Mutex::new(None);
static PANICKED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Initializing,
    RuntimeReady,
    Recovery,
    FrontendReady,
    EditorClosed,
    CleanExit,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    id: String,
    started_ms: u64,
    stage: Stage,
    clean: bool,
}
struct SessionFile {
    path: PathBuf,
    record: SessionRecord,
}

// Format the current timestamp for logging
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
// Record the lifecycle event in the Manager log
fn event(name: &'static str, fields: serde_json::Value) {
    tauri_plugin_log::log::info!(target: diagnostics::TARGET, "{}", serde_json::json!({"event":name,"details":fields}));
}
impl SessionFile {
    // Begin for the current operation
    fn begin(directory: &Path) -> Result<(Self, Option<SessionRecord>), String> {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let path = directory.join("manager-session.json");
        let previous = if path.metadata().is_ok_and(|metadata| metadata.len() <= 2048) {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<SessionRecord>(&bytes).ok())
                .filter(|record| uuid::Uuid::parse_str(&record.id).is_ok())
        } else {
            None
        };
        let session = Self {
            path,
            record: SessionRecord {
                id: uuid::Uuid::new_v4().to_string(),
                started_ms: timestamp(),
                stage: Stage::Initializing,
                clean: false,
            },
        };
        session.save()?;
        Ok((session, previous.filter(|record| !record.clean)))
    }
    // Save for the current operation
    fn save(&self) -> Result<(), String> {
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut file, &self.record).map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        file.persist(&self.path)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

// Validate or create the managed directory
pub fn directory(config_root: &Path) -> PathBuf {
    config_root.join("logs")
}

// Initialize this desktop integration
pub fn init<R: tauri::Runtime>(app: &tauri::AppHandle<R>, config_root: &Path) {
    let directory = directory(config_root);
    let plugin = tauri_plugin_log::Builder::new()
        .targets([Target::new(TargetKind::Folder {
            path: directory.clone(),
            file_name: Some("manager".into()),
        })])
        .level(tauri_plugin_log::log::LevelFilter::Info)
        .filter(|record| {
            matches!(
                record.target(),
                "stream_dock_manager::desktop" | diagnostics::TARGET
            )
        })
        .max_file_size(MAX_FILE_SIZE)
        .rotation_strategy(RotationStrategy::KeepSome(3))
        .build();
    if app.plugin(plugin).is_err() {
        eprintln!("Could not initialize the Manager log");
    }
    install_panic_hook();
    match SessionFile::begin(&directory) {
        Ok((session, previous)) => {
            event(
                "session_started",
                serde_json::json!({"session":session.record.id,"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH}),
            );
            if let Some(previous) = previous {
                tauri_plugin_log::log::warn!(target: diagnostics::TARGET, "{}", serde_json::json!({"event":"previous_unclean_exit","session":previous.id,"started_ms":previous.started_ms,"last_stage":previous.stage,"cause":"unknown"}));
            }
            *SESSION.lock().unwrap_or_else(|error| error.into_inner()) = Some(session);
        }
        Err(error) => event(
            "session_journal_unavailable",
            serde_json::json!({"reason":diagnostics::classify(&error)}),
        ),
    }
}

// Stage for the current operation
pub fn stage(stage: Stage) {
    let mut guard = SESSION.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(session) = guard.as_mut() {
        if session.record.clean {
            return;
        }
        session.record.stage = stage;
        if let Err(error) = session.save() {
            event(
                "session_journal_write_failed",
                serde_json::json!({"reason":diagnostics::classify(&error)}),
            );
        }
        event(
            "session_stage",
            serde_json::json!({"session":session.record.id,"stage":stage}),
        );
    }
}
// Record the application startup failure
pub fn startup_failed(error: &str) {
    tauri_plugin_log::log::error!(target: diagnostics::TARGET, "{}", serde_json::json!({"event":"manager_startup_failed","reason":diagnostics::classify(error)}));
    tauri_plugin_log::log::logger().flush();
}
// Finish for the current operation
pub fn finish() {
    let mut guard = SESSION.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(session) = guard.as_mut() {
        if session.record.clean {
            return;
        }
        session.record.stage = Stage::CleanExit;
        session.record.clean = true;
        if let Err(error) = session.save() {
            event(
                "session_journal_write_failed",
                serde_json::json!({"reason":diagnostics::classify(&error)}),
            );
        }
        event(
            "session_finished",
            serde_json::json!({"session":session.record.id}),
        );
        tauri_plugin_log::log::logger().flush();
    }
}

// Source names here come from compiled Rust panic locations, not user values
fn code_location(file: &str) -> String {
    let file = file.replace('\\', "/");
    for marker in ["src-tauri/", "crates/", "hardware/", "library/"] {
        if let Some((_, suffix)) = file.rsplit_once(marker) {
            return format!("{marker}{}", suffix.chars().take(240).collect::<String>());
        }
    }
    "dependency".into()
}
// Resolve stack frames for the diagnostic record
fn stack_symbols(stack: &str) -> Vec<String> {
    stack
        .lines()
        .filter_map(|line| {
            let (number, symbol) = line.trim().split_once(':')?;
            number.parse::<u32>().ok()?;
            let symbol = symbol.trim();
            if symbol.len() > 240
                || !symbol
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_:<>., &$#{}()-".contains(&c))
            {
                return None;
            }
            if [
                "stream_dock_manager::",
                "stream_dock_manager_",
                "core::",
                "std::",
                "alloc::",
                "tauri::",
                "tao::",
                "wry::",
                "tokio::",
            ]
            .iter()
            .any(|prefix| symbol.starts_with(prefix))
            {
                Some(symbol.to_owned())
            } else {
                None
            }
        })
        .take(24)
        .collect()
}
// Install panic hook
fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !PANICKED.swap(true, Ordering::SeqCst) {
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("");
            let location = info.location().map(|location| serde_json::json!({"source":code_location(location.file()),"line":location.line(),"column":location.column()}));
            let frames = stack_symbols(&std::backtrace::Backtrace::force_capture().to_string());
            tauri_plugin_log::log::error!(target: diagnostics::TARGET, "{}", serde_json::json!({"event":"native_panic","reason":diagnostics::classify(message),"location":location,"frames":frames}));
            tauri_plugin_log::log::logger().flush();
        }
        previous(info);
    }));
}

struct Budget {
    start: Instant,
    count: u32,
    dropped: u32,
}
impl Budget {
    // Admit for the current operation
    fn admit(&mut self, now: Instant) -> bool {
        if now.duration_since(self.start) >= Duration::from_secs(60) {
            self.start = now;
            self.count = 0;
            self.dropped = 0;
        }
        self.count = self.count.saturating_add(1);
        if self.count > 30 {
            self.dropped = self.dropped.saturating_add(1);
            false
        } else {
            true
        }
    }
}
static FRONTEND_BUDGETS: OnceLock<Mutex<[Budget; 3]>> = OnceLock::new();
// Record a frontend diagnostic message
pub fn frontend(window: &str, report: FrontendDiagnostic) -> Result<(), String> {
    report.validate()?;
    let (index, source) = if window == "main" {
        (0, "editor")
    } else if window.starts_with("log-reader-") {
        (1, "log_reader")
    } else if window.starts_with("action-popup-") {
        (2, "action_popup")
    } else {
        return Err("Frontend diagnostics are restricted to Manager windows".into());
    };
    let budgets = FRONTEND_BUDGETS.get_or_init(|| {
        Mutex::new(std::array::from_fn(|_| Budget {
            start: Instant::now(),
            count: 0,
            dropped: 0,
        }))
    });
    let mut budgets = budgets.lock().unwrap_or_else(|error| error.into_inner());
    if !budgets[index].admit(Instant::now()) {
        if budgets[index].dropped == 1 {
            event(
                "frontend_reports_rate_limited",
                serde_json::json!({"window":source}),
            );
        }
        return Ok(());
    }
    drop(budgets);
    tauri_plugin_log::log::error!(target: diagnostics::TARGET, "{}", serde_json::json!({"event":"frontend_error","window":source,"report":report}));
    Ok(())
}

// Existing operation descriptions are compile-time strings, never raw errors
pub fn failure(operation: &'static str) {
    tauri_plugin_log::log::error!(target: "stream_dock_manager::desktop", "{operation}");
}
// Record a warning in the Manager log
pub fn warning(operation: &'static str) {
    tauri_plugin_log::log::warn!(target: "stream_dock_manager::desktop", "{operation}");
}
// Record why editor closure was blocked
pub fn editor_close_blocked(active: usize, blockers: &[&'static str]) {
    tauri_plugin_log::log::warn!(target: diagnostics::TARGET, "{}", serde_json::json!({
        "event": "editor_close_blocked",
        "active_operations": active,
        "resources": blockers,
    }));
}
// Open folder
pub fn open_folder(path: &Path) -> Result<(), String> {
    tauri_plugin_log::log::logger().flush();
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    stream_dock_manager_runtime::actions::open(
        path.to_str().ok_or("Manager log path is not UTF-8")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify logs and sessions use selected application data root
    fn logs_and_sessions_use_selected_application_data_root() {
        const KEY: &str = "STREAM_DOCK_MANAGER_TEST_LOG_ROOT";
        if let Some(root) = std::env::var_os(KEY) {
            let root = crate::startup_args::configuration_root([
                std::ffi::OsString::from("--config"),
                root,
            ])
            .unwrap();
            let app = tauri::test::mock_builder()
                .build(crate::desktop_context())
                .unwrap();
            init(app.handle(), &root);
            stage(Stage::RuntimeReady);
            startup_failed("timeout");
            finish();
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("custom application data");
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "manager_log::tests::logs_and_sessions_use_selected_application_data_root",
                "--nocapture",
            ])
            .env(KEY, &root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let log = std::fs::read_to_string(root.join("logs/manager.log")).unwrap();
        assert!(log.contains("session_started"));
        assert!(log.contains("manager_startup_failed"));
        assert!(log.contains("session_finished"));
        let journal: SessionRecord =
            serde_json::from_slice(&std::fs::read(root.join("logs/manager-session.json")).unwrap())
                .unwrap();
        assert!(journal.clean);
        assert!(matches!(journal.stage, Stage::CleanExit));
        assert!(!root.join("manager.log").exists());
    }
    #[test]
    // Verify unclean sessions are reported once and clean shutdowns are not crashes
    fn unclean_sessions_are_reported_once_and_clean_shutdowns_are_not_crashes() {
        let directory = tempfile::tempdir().unwrap();
        let (mut first, previous) = SessionFile::begin(directory.path()).unwrap();
        assert!(previous.is_none());
        first.record.stage = Stage::Recovery;
        first.save().unwrap();
        let (mut second, previous) = SessionFile::begin(directory.path()).unwrap();
        let previous = previous.unwrap();
        assert_eq!(previous.id, first.record.id);
        assert!(matches!(previous.stage, Stage::Recovery));
        second.record.clean = true;
        second.save().unwrap();
        assert!(SessionFile::begin(directory.path()).unwrap().1.is_none());
        std::fs::write(
            directory.path().join("manager-session.json"),
            b"{\"secret\":\"do not log me\"}",
        )
        .unwrap();
        assert!(SessionFile::begin(directory.path()).unwrap().1.is_none());
    }
    #[test]
    // Verify panic details keep code locations without local paths or payloads
    fn panic_details_keep_code_locations_without_local_paths_or_payloads() {
        assert_eq!(
            code_location("/Users/private/work/stream-dock-manager/src-tauri/src/main.rs"),
            "src-tauri/src/main.rs"
        );
        assert_eq!(
            code_location("C:\\Users\\private\\source\\crates\\runtime\\src\\lib.rs"),
            "crates/runtime/src/lib.rs"
        );
        assert_eq!(code_location("/Users/private/unknown.rs"), "dependency");
        let frames = stack_symbols(" 0: stream_dock_manager::startup\n at /Users/private/secret.rs:12\n 1: https://user:token@example.org\n 2: std::panicking::begin_panic\n 3: secret=private-token");
        assert_eq!(
            frames,
            [
                "stream_dock_manager::startup",
                "std::panicking::begin_panic"
            ]
        );
        assert_eq!(MAX_FILE_SIZE, 5 * 1024 * 1024);
    }
    #[test]
    // Verify panic hook flushes a private data free crash record in a child process
    fn panic_hook_flushes_a_private_data_free_crash_record_in_a_child_process() {
        use tauri_plugin_log::log;
        const KEY: &str = "STREAM_DOCK_MANAGER_TEST_PANIC_LOG";
        if let Some(path) = std::env::var_os(KEY) {
            struct Sink(Mutex<std::fs::File>);
            impl log::Log for Sink {
                // Report whether this feature is enabled
                fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                    true
                }
                // Write the diagnostic record to the Manager log
                fn log(&self, record: &log::Record<'_>) {
                    writeln!(self.0.lock().unwrap(), "{}", record.args()).unwrap();
                }
                // Flush for the current operation
                fn flush(&self) {
                    self.0.lock().unwrap().sync_all().unwrap();
                }
            }
            log::set_boxed_logger(Box::new(Sink(Mutex::new(
                std::fs::File::create(path).unwrap(),
            ))))
            .unwrap();
            log::set_max_level(log::LevelFilter::Info);
            install_panic_hook();
            panic!("assertion failed with Bearer private-token https://person:password@example.org /Users/private");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("panic.log");
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "manager_log::tests::panic_hook_flushes_a_private_data_free_crash_record_in_a_child_process", "--nocapture"])
            .env(KEY, &path).output().unwrap();
        assert!(!result.status.success());
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(record["event"], "native_panic");
        assert_eq!(record["reason"], "assertion");
        assert_eq!(record["location"]["source"], "src-tauri/src/manager_log.rs");
        assert!(!record["frames"].as_array().unwrap().is_empty());
        let record = record.to_string();
        for private in ["private-token", "person", "password", "https:", "/Users"] {
            assert!(!record.contains(private));
        }
    }
    #[test]
    // Verify frontend floods are bounded and recover after the window
    fn frontend_floods_are_bounded_and_recover_after_the_window() {
        let now = Instant::now();
        let mut budget = Budget {
            start: now,
            count: 0,
            dropped: 0,
        };
        for _ in 0..30 {
            assert!(budget.admit(now));
        }
        for _ in 0..1000 {
            assert!(!budget.admit(now));
        }
        assert!(budget.admit(now + Duration::from_secs(61)));
    }
}
