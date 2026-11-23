//! Explicitly reviewed support snapshots. Never export raw diagnostic command output
use serde_json::{json, Value};
use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
const LIMIT: usize = 8 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(600);
struct Pending {
    id: String,
    document: String,
    created: Instant,
}
static PENDING: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();

// Stage for the current operation
pub fn stage(environment: Value, performance: Value) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let document = serde_json::to_string_pretty(&json!({
        "schema_version": 1, "report_id": id,
        "privacy": "No raw logs, profile contents, settings, hostnames, device serials, user file paths or arbitrary environment variables are collected. Plugin identifiers and runtime versions are included; review before sharing.",
        "environment": environment, "performance": performance,
        "resource_errors": crate::manager_log::resource_reports()
    })).map_err(|e| e.to_string())?;
    if document.len() > LIMIT {
        return Err("Diagnostic report exceeds 8 MiB".into());
    }
    *PENDING.get_or_init(Default::default).lock().unwrap() = Some(Pending {
        id,
        document: document.clone(),
        created: Instant::now(),
    });
    Ok(document)
}
// Read the export content that the user reviewed
pub fn reviewed(id: &str) -> Result<String, String> {
    let mut pending = PENDING.get_or_init(Default::default).lock().unwrap();
    if pending
        .as_ref()
        .is_some_and(|p| p.created.elapsed() > LIFETIME)
    {
        *pending = None;
    }
    pending
        .as_ref()
        .filter(|p| p.id == id)
        .map(|p| p.document.clone())
        .ok_or_else(|| {
            "Diagnostic preview expired or was replaced. Preview the report again before saving."
                .into()
        })
}
// Discard for the current operation
pub fn discard(id: &str) {
    let mut pending = PENDING.get_or_init(Default::default).lock().unwrap();
    if pending.as_ref().is_some_and(|p| p.id == id) {
        *pending = None;
    }
}
// Collect the runtime environment for diagnostics
pub fn environment() -> Value {
    let mut data = stream_dock_manager_runtime::diagnostics::support_environment();
    data["build"] = json!({"version": env!("CARGO_PKG_VERSION"), "profile": if cfg!(debug_assertions) { "debug" } else { "release" }, "commit": option_env!("SDM_BUILD_COMMIT")});
    data["webview"] = engine();
    data
}
// Identify the embedded browser engine
fn engine() -> Value {
    // Tauri queries the linked WebKitGTK library on Linux, the WebKit bundle on
    // macOS, and the available WebView2 runtime on Windows. Do not label the
    // Windows availability query as a particular loaded window's version
    let version = tauri::webview_version().ok().filter(|v| {
        v.len() <= 128
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ' '))
    });
    let source = if version.is_none() {
        "unavailable"
    } else if cfg!(target_os = "linux") {
        "loaded-library"
    } else if cfg!(target_os = "macos") {
        "system-webkit-bundle"
    } else {
        "available-runtime"
    };
    let mut info = json!({"engine": if cfg!(target_os="linux") { "WebKitGTK" } else if cfg!(target_os="macos") { "WKWebView" } else if cfg!(target_os="windows") { "WebView2" } else { "unknown" }, "version":version, "version_source":source});
    #[cfg(target_os = "linux")]
    {
        info["gtk_version"] = json!(format!(
            "{}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        ));
    }
    #[cfg(not(target_os = "linux"))]
    {
        info["gtk_version"] = Value::Null;
    }
    info
}
// List loaded native libraries relevant to diagnostics
pub fn loaded_rows() -> Vec<stream_dock_manager_runtime::diagnostics::Diagnostic> {
    let info = engine();
    let mut rows = vec![stream_dock_manager_runtime::diagnostics::Diagnostic {
        category: "WebView runtime".into(),
        value: format!(
            "{} / {}",
            info["engine"].as_str().unwrap_or("unknown"),
            info["version"]
                .as_str()
                .unwrap_or("runtime version unavailable")
        ),
        status: if info["version"].is_string() {
            "ok"
        } else {
            "info"
        }
        .into(),
    }];
    if let Some(version) = info["gtk_version"].as_str() {
        rows.push(stream_dock_manager_runtime::diagnostics::Diagnostic {
            category: "GTK runtime".into(),
            value: version.into(),
            status: "ok".into(),
        });
    }
    for (index, report) in crate::manager_log::resource_reports()
        .into_iter()
        .enumerate()
    {
        let failure = if matches!(
            report.reason,
            stream_dock_manager_plugin_host::manager_diagnostics::FailureReason::ResourceDecode
        ) {
            "Image decoding failed"
        } else {
            "Resource loading failed"
        };
        let context = serde_json::to_value(report.resource).unwrap_or_default();
        let field = |key: &str| context[key].as_str().unwrap_or("unknown");
        rows.push(stream_dock_manager_runtime::diagnostics::Diagnostic {
            category: format!("Frontend resource {}", index + 1),
            value: format!(
                "{failure} · Route {} · Component {} · Role {}",
                field("route"),
                field("owner"),
                field("role")
            ),
            status: "warning".into(),
        });
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify saved snapshot is exact and expired or replaced IDs fail
    fn saved_snapshot_is_exact_and_expired_or_replaced_ids_fail() {
        let first = stage(json!({"os":"fixture"}), json!({"metrics":[]})).unwrap();
        let id = serde_json::from_str::<Value>(&first).unwrap()["report_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(reviewed(&id).unwrap(), first);
        stage(json!({}), json!({})).unwrap();
        assert!(reviewed(&id).is_err());
        let mut p = PENDING.get().unwrap().lock().unwrap();
        let current = p.as_mut().unwrap();
        let id = current.id.clone();
        current.created = Instant::now() - LIFETIME - Duration::from_secs(1);
        drop(p);
        assert!(reviewed(&id).is_err());
        let document = stage(json!({}), json!({})).unwrap();
        let id = serde_json::from_str::<Value>(&document).unwrap()["report_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(stage(json!({"oversized": "x".repeat(LIMIT)}), json!({})).is_err());
        assert_eq!(reviewed(&id).unwrap(), document);
        discard(&id);
        assert!(reviewed(&id).is_err());
    }
}
