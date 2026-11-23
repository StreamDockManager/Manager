//! Recovery-only desktop commands. No normal Runtime exists in this window
use std::sync::{Arc, Mutex};
use stream_dock_manager_runtime::configuration_recovery::{
    ConfigurationRecovery, ConfigurationRecoveryBackupProfiles, ConfigurationRecoveryBackups,
    ConfigurationRecoveryDocument, ConfigurationRecoveryKeystoreReview,
    ConfigurationRecoveryKeystoreStatus, ConfigurationRecoveryReport,
    ConfigurationRecoveryRestorePreview, ConfigurationRecoveryValidation,
    ConfigurationRecoveryVersions,
};
use tauri::{Emitter, Manager, WebviewWindow};

/// Serialize repair operations with restart admission. A queued write cannot
/// start after validation hands ownership back to normal startup.
pub struct RecoverySession {
    service: ConfigurationRecovery,
    root: std::path::PathBuf,
    restarting: Mutex<bool>,
}
impl RecoverySession {
    // Run for the current operation
    fn run<T>(
        &self,
        operation: impl FnOnce(&ConfigurationRecovery) -> Result<T, String>,
    ) -> Result<T, String> {
        let restarting = self
            .restarting
            .lock()
            .map_err(|_| "Recovery operation lock failed")?;
        if *restarting {
            return Err("Manager is restarting".into());
        }
        operation(&self.service)
    }
    // Prepare restart
    fn prepare_restart(&self, recovery: bool) -> Result<(), String> {
        let mut restarting = self
            .restarting
            .lock()
            .map_err(|_| "Recovery operation lock failed")?;
        if *restarting {
            return Err("Manager is already restarting".into());
        }
        if !self.service.report()?.ready {
            return Err(
                "Resolve the remaining configuration issues before reloading Manager".into(),
            );
        }
        self.service.request_restart(recovery)?;
        *restarting = true;
        Ok(())
    }
}

// Validate authorization for the recovery operation
fn authorize<R: tauri::Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    if window.label() != "main"
        || window
            .app_handle()
            .try_state::<Arc<stream_dock_manager_runtime::Runtime>>()
            .is_some()
    {
        return Err("Configuration recovery is only available in the recovery window".into());
    }
    Ok(())
}

// Open for the current operation
pub fn open(app: &tauri::AppHandle, root: std::path::PathBuf, error: String) -> Result<(), String> {
    let recovery = ConfigurationRecovery::open(root.clone(), error)?;
    open_window(app, root, recovery)
}

// Open requested
pub fn open_requested(app: &tauri::AppHandle, root: std::path::PathBuf) -> Result<(), String> {
    let recovery = ConfigurationRecovery::open_requested(root.clone(), String::new())?;
    open_window(app, root, recovery)
}

// Open window
fn open_window(
    app: &tauri::AppHandle,
    root: std::path::PathBuf,
    recovery: ConfigurationRecovery,
) -> Result<(), String> {
    app.manage(Arc::new(RecoverySession {
        service: recovery,
        root,
        restarting: Mutex::new(false),
    }));
    tauri::WebviewWindowBuilder::new(
        app,
        "main",
        tauri::WebviewUrl::App("config-recovery.html".into()),
    )
    .title("Stream Dock Manager — Configuration recovery")
    .inner_size(1120.0, 800.0)
    .min_inner_size(600.0, 480.0)
    .disable_drag_drop_handler()
    .center()
    .build()
    .map_err(|error| error.to_string())?;
    Ok(())
}

// Reveal for the current operation
pub fn reveal(app: &tauri::AppHandle) {
    if app.try_state::<Arc<RecoverySession>>().is_some() {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }
}
// Handle the configuration recovery status desktop command

#[tauri::command]
pub async fn configuration_recovery_status<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
) -> Result<ConfigurationRecoveryReport, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recovery.run(|service| service.report()))
        .await
        .map_err(|error| error.to_string())?
}
// Handle the configuration recovery backups desktop command

#[tauri::command]
pub async fn configuration_recovery_backups<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
) -> Result<ConfigurationRecoveryBackups, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recovery.run(|service| service.backup_catalog()))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn configuration_recovery_keystore_status<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recovery.run(|service| service.keystore_status()))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn configuration_recovery_retry_keystore<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    revision: String,
) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.retry_keystore(&revision))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn configuration_recovery_review_keystore<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    revision: String,
    operation: String,
    backup_id: Option<String>,
) -> Result<ConfigurationRecoveryKeystoreReview, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.review_keystore(&revision, &operation, backup_id.as_deref()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn configuration_recovery_apply_keystore<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    revision: String,
    review_token: String,
) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.apply_keystore(&revision, &review_token))
    })
    .await
    .map_err(|error| error.to_string())?
}

// Handle the configuration recovery backup profiles desktop command

#[tauri::command]
pub async fn configuration_recovery_backup_profiles<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    backup_id: String,
    revision: String,
) -> Result<ConfigurationRecoveryBackupProfiles, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.backup_profiles(&backup_id, &revision))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery transfer profiles desktop command

#[tauri::command]
pub async fn configuration_recovery_transfer_profiles<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    backup_id: String,
    revision: String,
    profile_ids: Vec<String>,
) -> Result<ConfigurationRecoveryReport, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery
            .run(|service| service.transfer_backup_profiles(&backup_id, &revision, &profile_ids))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery preview restore desktop command

#[tauri::command]
pub async fn configuration_recovery_preview_restore<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    backup_id: String,
    revision: String,
) -> Result<ConfigurationRecoveryRestorePreview, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.preview_backup_restore(&backup_id, &revision))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery restore backup desktop command

#[tauri::command]
pub async fn configuration_recovery_restore_backup<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    backup_id: String,
    revision: String,
    review_token: String,
) -> Result<ConfigurationRecoveryReport, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.restore_backup(&backup_id, &revision, &review_token))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery document desktop command

#[tauri::command]
pub async fn configuration_recovery_document<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
) -> Result<ConfigurationRecoveryDocument, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recovery.run(|service| service.document(&id)))
        .await
        .map_err(|error| error.to_string())?
}
// Handle the configuration recovery versions desktop command

#[tauri::command]
pub async fn configuration_recovery_versions<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
    revision: String,
) -> Result<ConfigurationRecoveryVersions, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.versions(&id, &revision))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery version desktop command

#[tauri::command]
pub async fn configuration_recovery_version<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
    revision: String,
    version_id: String,
) -> Result<ConfigurationRecoveryDocument, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.version(&id, &revision, &version_id))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery validate desktop command

#[tauri::command]
pub async fn configuration_recovery_validate<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
    revision: String,
    json: String,
) -> Result<ConfigurationRecoveryValidation, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.validate(&id, &revision, &json))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery save desktop command

#[tauri::command]
pub async fn configuration_recovery_save<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
    revision: String,
    json: String,
) -> Result<ConfigurationRecoveryReport, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.save(&id, &revision, &json))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery delete desktop command

#[tauri::command]
pub async fn configuration_recovery_delete<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
    id: String,
    revision: String,
) -> Result<ConfigurationRecoveryReport, String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|service| service.delete(&id, &revision))
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the configuration recovery leave desktop command

#[tauri::command]
pub async fn configuration_recovery_leave<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    session: tauri::State<'_, Arc<RecoverySession>>,
    app: tauri::AppHandle<R>,
    recovery: bool,
) -> Result<(), String> {
    authorize(&window)?;
    let session = session.inner().clone();
    let operation = session.clone();
    tauri::async_runtime::spawn_blocking(move || operation.prepare_restart(recovery))
        .await
        .map_err(|error| error.to_string())??;
    // The dev launcher keeps Vite alive and relaunches with the original args
    // Persisted one-shot intent overrides those args on both restart paths
    match crate::dev_restart::request() {
        Ok(true) => app.exit(0),
        Ok(false) => app.restart(),
        Err(error) => {
            *session
                .restarting
                .lock()
                .map_err(|_| "Recovery operation lock failed")? = false;
            return Err(error);
        }
    }
    Ok(())
}
// Handle the configuration recovery open data desktop command

#[tauri::command]
pub async fn configuration_recovery_open_data<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    recovery: tauri::State<'_, Arc<RecoverySession>>,
) -> Result<(), String> {
    authorize(&window)?;
    let recovery = recovery.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recovery.run(|_| crate::manager_log::open_folder(&recovery.root))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Native close is a navigation choice, not an implicit exit or mode change
pub fn close_requested<R: tauri::Runtime>(window: &tauri::Window<R>, event: &tauri::WindowEvent) {
    if window.label() != "main"
        || window
            .app_handle()
            .try_state::<Arc<RecoverySession>>()
            .is_none()
    {
        return;
    }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.emit("configuration-recovery-close-requested", ());
    }
}

/// Unlike closing the recovery page, quitting never changes startup mode
fn exit_requires_confirmation(approved: bool, code: Option<i32>) -> bool {
    !approved && code != Some(tauri::RESTART_EXIT_CODE)
}

// Exit requested
pub fn exit_requested<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: &tauri::RunEvent) {
    let Some(session) = app.try_state::<Arc<RecoverySession>>() else {
        return;
    };
    if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
        // Native menu Quit may carry Some(0). Only our serialized leave/quit
        // handlers authorize ordinary exits. Never block the UI on an active save.
        let approved = session
            .restarting
            .try_lock()
            .map(|flag| *flag)
            .unwrap_or(false);
        if exit_requires_confirmation(approved, *code) {
            api.prevent_exit();
            let _ = app.emit_to("main", "configuration-recovery-quit-requested", ());
        }
    }
}
// Handle the configuration recovery quit desktop command

#[tauri::command]
pub async fn configuration_recovery_quit<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    session: tauri::State<'_, Arc<RecoverySession>>,
    app: tauri::AppHandle<R>,
) -> Result<(), String> {
    authorize(&window)?;
    let session = session.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Finish any already-admitted save before exiting. No queued operation
        // can start after this point, and originals/commits remain unambiguous.
        let mut exiting = session
            .restarting
            .lock()
            .map_err(|_| "Recovery operation lock failed")?;
        if *exiting {
            return Err("Manager is already exiting".to_owned());
        }
        *exiting = true;
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())??;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tauri::test::MockRuntime;

    // Construct the isolated test fixture
    fn fixture() -> (tempfile::TempDir, Arc<RecoverySession>) {
        let directory = tempfile::tempdir().unwrap();
        let store =
            stream_dock_manager_runtime::ProfileStore::open(directory.path().join("profiles"))
                .unwrap();
        let data = store.load().unwrap();
        store
            .edit(data.configuration_revision, "Rename profile", |data| {
                data.profiles[0].name = "Changed".into();
                Ok(())
            })
            .unwrap();
        drop(store);
        let service = ConfigurationRecovery::open(
            directory.path().to_path_buf(),
            "Fixture startup error".into(),
        )
        .unwrap();
        let root = directory.path().to_path_buf();
        (
            directory,
            Arc::new(RecoverySession {
                service,
                root,
                restarting: Mutex::new(false),
            }),
        )
    }

    // Dispatch the authorized recovery command
    fn invoke(
        window: &tauri::WebviewWindow<MockRuntime>,
        name: &str,
        body: Value,
    ) -> Result<Value, Value> {
        tauri::test::get_ipc_response(
            window,
            tauri::webview::InvokeRequest {
                cmd: name.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(target_os = "windows") {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }

    #[test]
    // Verify recovery commands work without runtime and remain first party only
    fn recovery_commands_work_without_runtime_and_remain_first_party_only() {
        let (_directory, session) = fixture();
        let app = tauri::test::mock_builder()
            .manage(session)
            .invoke_handler(tauri::generate_handler![
                configuration_recovery_status,
                configuration_recovery_keystore_status,
                configuration_recovery_retry_keystore,
                configuration_recovery_review_keystore,
                configuration_recovery_apply_keystore,
                configuration_recovery_backups,
                configuration_recovery_preview_restore,
                configuration_recovery_restore_backup,
                configuration_recovery_backup_profiles,
                configuration_recovery_transfer_profiles,
                configuration_recovery_document,
                configuration_recovery_versions,
                configuration_recovery_version,
                configuration_recovery_validate,
                configuration_recovery_leave,
                configuration_recovery_open_data,
                configuration_recovery_quit
            ])
            .build(crate::desktop_context())
            .unwrap();
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let plugin = tauri::WebviewWindowBuilder::new(&app, "plugin-fixture", Default::default())
            .build()
            .unwrap();
        assert!(app
            .try_state::<Arc<stream_dock_manager_runtime::Runtime>>()
            .is_none());
        let report = invoke(&main, "configuration_recovery_status", json!({})).unwrap();
        assert_eq!(report["ready"], true, "{report}");
        assert!(invoke(&plugin, "configuration_recovery_status", json!({}))
            .unwrap_err()
            .to_string()
            .contains("not allowed"));
        for (command, body) in [
            ("configuration_recovery_keystore_status", json!({})),
            (
                "configuration_recovery_retry_keystore",
                json!({"revision":"stale"}),
            ),
            (
                "configuration_recovery_review_keystore",
                json!({"revision":"stale","operation":"reset"}),
            ),
            (
                "configuration_recovery_review_keystore",
                json!({"revision":"stale","operation":"create"}),
            ),
            (
                "configuration_recovery_apply_keystore",
                json!({"revision":"stale","reviewToken":"unreviewed"}),
            ),
            ("configuration_recovery_backups", json!({})),
            (
                "configuration_recovery_preview_restore",
                json!({"backupId":"unknown","revision":"stale"}),
            ),
            (
                "configuration_recovery_restore_backup",
                json!({"backupId":"unknown","revision":"stale","reviewToken":"unreviewed"}),
            ),
            (
                "configuration_recovery_backup_profiles",
                json!({"backupId":"unknown","revision":"stale"}),
            ),
            (
                "configuration_recovery_transfer_profiles",
                json!({"backupId":"unknown","revision":"stale","profileIds":[]}),
            ),
            ("configuration_recovery_leave", json!({"recovery": true})),
            ("configuration_recovery_open_data", json!({})),
            ("configuration_recovery_quit", json!({})),
            (
                "configuration_recovery_versions",
                json!({"id":"profile:any","revision":"current"}),
            ),
            (
                "configuration_recovery_version",
                json!({"id":"profile:any","revision":"current","versionId":"any"}),
            ),
        ] {
            assert!(invoke(&plugin, command, body)
                .unwrap_err()
                .to_string()
                .contains("not allowed"));
        }
        let keystore = invoke(&main, "configuration_recovery_keystore_status", json!({})).unwrap();
        assert_eq!(keystore["state"], "ready");
        assert!(invoke(
            &main,
            "configuration_recovery_review_keystore",
            json!({"revision":keystore["revision"],"operation":"create"}),
        )
        .is_err());
        assert!(invoke(
            &main,
            "configuration_recovery_apply_keystore",
            json!({"revision":keystore["revision"],"reviewToken":"unreviewed"})
        )
        .is_err());
        let backups = invoke(&main, "configuration_recovery_backups", json!({})).unwrap();
        let backup_revision = backups["revision"].as_str().unwrap();
        for command in [
            "configuration_recovery_backup_profiles",
            "configuration_recovery_transfer_profiles",
        ] {
            assert!(invoke(&main, command, json!({
                "backupId":"../../arbitrary.sqlite3", "revision":backup_revision, "profileIds":["profile"]
            })).is_err());
        }
        assert_eq!(
            invoke(&main, "configuration_recovery_status", json!({})).unwrap()["ready"],
            true
        );
        let report = invoke(&main, "configuration_recovery_status", json!({})).unwrap();
        let id = report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "profile")
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let current = invoke(&main, "configuration_recovery_document", json!({"id":id})).unwrap();
        let revision = &current["revision"];
        let versions = invoke(
            &main,
            "configuration_recovery_versions",
            json!({"id":id,"revision":revision}),
        )
        .unwrap();
        let previous_id = &versions["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|version| version["source"] == "history")
            .unwrap()["id"];
        let previous = invoke(
            &main,
            "configuration_recovery_version",
            json!({"id":id,"revision":revision,"versionId":previous_id}),
        )
        .unwrap();
        assert_eq!(previous["revision"], *revision);
        assert_ne!(previous["json"], current["json"]);
        for body in [
            json!({"id":id,"revision":revision,"versionId":"../../arbitrary-file"}),
            json!({"id":"profile:other","revision":revision,"versionId":previous_id}),
            json!({"id":id,"revision":"stale","versionId":previous_id}),
        ] {
            assert!(invoke(&main, "configuration_recovery_version", body).is_err());
        }
        assert_eq!(
            invoke(&main, "configuration_recovery_document", json!({"id":id})).unwrap(),
            current
        );
        assert!(invoke(
            &main,
            "configuration_recovery_validate",
            json!({"id":"missing","revision":"stale","json":"{}"})
        )
        .is_err());
        assert_eq!(
            invoke(&main, "configuration_recovery_status", json!({})).unwrap()["ready"],
            true
        );
    }

    #[test]
    // Verify corrupt primary can restore reviewed backup without starting runtime
    fn corrupt_primary_can_restore_reviewed_backup_without_starting_runtime() {
        let (directory, session) = fixture();
        drop(session);
        let root = directory.path().to_path_buf();
        let primary = stream_dock_manager_storage::database_path(
            &root,
            stream_dock_manager_storage::DATABASE_NAME,
        );
        std::fs::write(&primary, b"corrupted primary evidence").unwrap();
        let service =
            ConfigurationRecovery::open(root.clone(), "Corrupted primary".into()).unwrap();
        let session = Arc::new(RecoverySession {
            service,
            root,
            restarting: Mutex::new(false),
        });
        let app = tauri::test::mock_builder()
            .manage(session)
            .invoke_handler(tauri::generate_handler![
                configuration_recovery_status,
                configuration_recovery_backups,
                configuration_recovery_preview_restore,
                configuration_recovery_restore_backup,
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let status = invoke(&main, "configuration_recovery_status", json!({})).unwrap();
        assert_eq!(status["primary_readable"], false);
        assert_eq!(status["ready"], false);
        let catalog = invoke(&main, "configuration_recovery_backups", json!({})).unwrap();
        let revision = &catalog["revision"];
        let preview = catalog["backups"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|backup| {
                let preview = invoke(
                    &main,
                    "configuration_recovery_preview_restore",
                    json!({"backupId":backup["id"],"revision":revision}),
                )
                .unwrap();
                (preview["valid"] == true).then_some(preview)
            })
            .expect("valid full backup");
        assert_eq!(
            std::fs::read(&primary).unwrap(),
            b"corrupted primary evidence"
        );
        assert!(invoke(&main, "configuration_recovery_restore_backup", json!({"backupId":preview["backup_id"],"revision":revision,"reviewToken":"not-reviewed"})).is_err());
        assert_eq!(
            std::fs::read(&primary).unwrap(),
            b"corrupted primary evidence"
        );
        let preview = invoke(
            &main,
            "configuration_recovery_preview_restore",
            json!({"backupId":preview["backup_id"],"revision":revision}),
        )
        .unwrap();
        let restored = invoke(&main, "configuration_recovery_restore_backup", json!({"backupId":preview["backup_id"],"revision":revision,"reviewToken":preview["review_token"]})).unwrap();
        assert_eq!(restored["primary_readable"], true);
        assert_eq!(restored["ready"], true);
        assert!(app
            .try_state::<Arc<stream_dock_manager_runtime::Runtime>>()
            .is_none());
    }

    #[test]
    // Verify menu quit codes cannot bypass unsaved guard
    fn menu_quit_codes_cannot_bypass_unsaved_guard() {
        for code in [None, Some(0), Some(1)] {
            assert!(exit_requires_confirmation(false, code));
            assert!(!exit_requires_confirmation(true, code));
        }
        // Tauri's restart event cannot be prevented and is requested only after
        // the recovery leave operation has persisted its chosen startup mode.
        assert!(!exit_requires_confirmation(
            false,
            Some(tauri::RESTART_EXIT_CODE)
        ));
    }

    #[test]
    // Verify restart fences queued recovery operations
    fn restart_fences_queued_recovery_operations() {
        let (_directory, session) = fixture();
        session.prepare_restart(false).unwrap();
        assert!(session.run(|_| Ok(())).unwrap_err().contains("restarting"));
        assert!(session
            .prepare_restart(false)
            .unwrap_err()
            .contains("already restarting"));
    }
}
