//! Manager-owned diagnostic metadata. Never forward error text, settings, paths,
//! URLs, environment variables, plugin output, or arbitrary JSON to this sink
use serde::{Deserialize, Serialize};

pub const TARGET: &str = "stream_dock_manager::diagnostics";

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEvent {
    Starting,
    Registered,
    StartupFailed,
    UnexpectedExit,
    RetryScheduled,
    Blocked,
    Enabled,
    Disabled,
    InterruptedStartup,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    PermissionDenied,
    MissingFile,
    MissingRuntime,
    Incompatible,
    InvalidData,
    Timeout,
    ConnectionClosed,
    ProcessExit,
    Network,
    ModuleLoad,
    Reference,
    Type,
    Syntax,
    Range,
    Assertion,
    Unwrap,
    PoisonedLock,
    ResourceLoad,
    ResourceDecode,
    Unknown,
}

/// Classify locally, then discard the original text. Scrubbing free-form messages
/// cannot reliably remove arbitrary user content or credentials.
pub fn classify(message: &str) -> FailureReason {
    let text = message
        .chars()
        .take(8192)
        .collect::<String>()
        .to_ascii_lowercase();
    if text.contains("permission") || text.contains("access denied") {
        FailureReason::PermissionDenied
    } else if text.contains("no such file") || text.contains("not found") {
        FailureReason::MissingFile
    } else if text.contains("runtime unavailable")
        || text.contains("runtime is not")
        || text.contains("missing runtime")
    {
        FailureReason::MissingRuntime
    } else if text.contains("timed out") || text.contains("timeout") {
        FailureReason::Timeout
    } else if text.contains("connection closed") {
        FailureReason::ConnectionClosed
    } else if text.contains("process exited") {
        FailureReason::ProcessExit
    } else if text.contains("incompatible") || text.contains("compatibility") {
        FailureReason::Incompatible
    } else if text.contains("invalid") || text.contains("malformed") {
        FailureReason::InvalidData
    } else if text.contains("assertion") {
        FailureReason::Assertion
    } else if text.contains("unwrap") {
        FailureReason::Unwrap
    } else if text.contains("poison") {
        FailureReason::PoisonedLock
    } else if text.contains("out of bounds") {
        FailureReason::Range
    } else {
        FailureReason::Unknown
    }
}

#[derive(Serialize)]
pub struct PluginRecord<'a> {
    pub event: PluginEvent,
    pub plugin_id: &'a str,
    pub version: &'a str,
    pub run_id: Option<&'a str>,
    pub reason: Option<FailureReason>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub attempt: u32,
    pub recovery: bool,
}

// Construct the plugin diagnostic representation
pub fn plugin(mut record: PluginRecord<'_>) {
    // These are public package identities, never display names or host paths
    if !crate::safe_id(record.plugin_id) {
        record.plugin_id = "invalid-package-id";
    }
    if record.version.len() > 80
        || (semver::Version::parse(record.version).is_err()
            && crate::manifest::vendor_version(record.version).is_err())
    {
        record.version = "unknown";
    }
    record.run_id = record.run_id.filter(|id| uuid::Uuid::parse_str(id).is_ok());
    let message = serde_json::to_string(&record).unwrap_or_default();
    match record.event {
        PluginEvent::StartupFailed | PluginEvent::UnexpectedExit | PluginEvent::Blocked => {
            log::error!(target: TARGET, "{message}")
        }
        PluginEvent::InterruptedStartup => log::warn!(target: TARGET, "{message}"),
        _ => log::info!(target: TARGET, "{message}"),
    }
    #[cfg(test)]
    CAPTURE.with(|events| events.borrow_mut().push(message));
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryEvent {
    EnteredExplicit,
    InterruptedStartup,
    KeptActive,
    StartedNormally,
    RestartRecoveryRequested,
    RestartNormalRequested,
    RestartCancelled,
}
// Construct the recovery diagnostic representation
pub fn recovery(event: RecoveryEvent, interrupted: usize) {
    log::info!(target: TARGET, "{}", serde_json::json!({"event":"recovery_transition", "transition":event, "interrupted_plugins":interrupted}));
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrontendEvent {
    RuntimeError,
    UnhandledRejection,
    ConsoleError,
    ResourceError,
    CspViolation,
    StartupFailed,
    RouteFailed,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FrontendComponent {
    Main,
    RouteShell,
    RouteEditor,
    RoutePlugins,
    RouteSettings,
    RouteDiagnostics,
    PluginLogs,
    ActionPopup,
    Alpine,
    Acl,
    AppScript,
    External,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrontendFrame {
    pub component: FrontendComponent,
    pub line: u32,
    pub column: u32,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceRoute {
    Editor,
    ActionLibrary,
    Macros,
    Plugins,
    Settings,
    Diagnostics,
    Unknown,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceOwner {
    SdActionLibrary,
    SdActionWorkspace,
    SdAppShell,
    SdDeviceOutputDialog,
    SdDevicePreview,
    SdEditor,
    SdInspector,
    SdMacros,
    SdMediaLibrary,
    SdMediaOptions,
    SdPluginActionGroup,
    SdPluginCatalogDialog,
    SdPlugins,
    SdSettings,
    SdVideoEditor,
    Unknown,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceRole {
    BindingIcon,
    ControlSurface,
    BackgroundPreview,
    MediaPreview,
    Thumbnail,
    InterfaceIcon,
    PluginIcon,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceContext {
    pub route: ResourceRoute,
    pub owner: ResourceOwner,
    pub role: ResourceRole,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrontendStartupStage {
    ModuleImport,
    NativeHandshake,
    WorkspaceLoad,
    ShellMount,
    InitialRoute,
    EditorServices,
    VaultUnlock,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StartupLoaderCode {
    AclAssetLoadFailed,
    AclComponentLoadFailed,
    AclManifestFetchFailed,
    AclManifestHttpError,
    AclManifestInvalid,
    #[serde(rename = "ACL_MANIFEST_INVALID_JSON")]
    AclManifestInvalidJson,
    AclManifestTimeout,
    AclManifestTooLarge,
    AclTemplateFetchFailed,
    AclTemplateHttpError,
    AclTemplateTimeout,
    AclTemplateTooLarge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrontendStartupContext {
    pub stage: FrontendStartupStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub loader_code: Option<StartupLoaderCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrontendDiagnostic {
    pub event: FrontendEvent,
    pub reason: FailureReason,
    pub frames: Vec<FrontendFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resource: Option<ResourceContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub startup: Option<FrontendStartupContext>,
}
impl FrontendDiagnostic {
    // Validate for the current operation
    pub fn validate(&self) -> Result<(), String> {
        if self.resource.is_some() && !matches!(self.event, FrontendEvent::ResourceError) {
            return Err("Resource context requires a resource event".into());
        }
        if self.startup.is_some() && !matches!(self.event, FrontendEvent::StartupFailed) {
            return Err("Startup context requires a startup event".into());
        }
        if self.startup.is_some_and(|startup| {
            startup.loader_code.is_some()
                && !matches!(startup.stage, FrontendStartupStage::ShellMount)
        }) {
            return Err("Startup loader code requires the shell mount stage".into());
        }
        if self.frames.len() > 12
            || self
                .frames
                .iter()
                .any(|frame| frame.line > 10_000_000 || frame.column > 10_000_000)
        {
            return Err("Frontend diagnostic exceeds its bounds".into());
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! { static CAPTURE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) }; }
#[cfg(test)]
// Take events
pub(crate) fn take_events() -> Vec<String> {
    CAPTURE.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify arbitrary errors and payloads cannot reach structured logs
    fn arbitrary_errors_and_payloads_cannot_reach_structured_logs() {
        let secret = "Bearer private-token https://person:password@example.com/a?q=secret /Users/private/Profile";
        assert!(matches!(
            classify(&format!("Permission denied {secret}")),
            FailureReason::PermissionDenied
        ));
        assert!(!serde_json::to_string(&classify(secret))
            .unwrap()
            .contains(secret));
        assert!(serde_json::from_value::<FrontendDiagnostic>(serde_json::json!({"event":"runtime_error","reason":"unknown","frames":[],"message":secret})).is_err());
        assert!(serde_json::from_value::<FrontendDiagnostic>(serde_json::json!({"event":"runtime_error","reason":"unknown","frames":[{"component":secret,"line":1,"column":1}]})).is_err());
        let report = FrontendDiagnostic {
            event: FrontendEvent::RuntimeError,
            reason: FailureReason::Unknown,
            resource: None,
            startup: None,
            frames: vec![
                FrontendFrame {
                    component: FrontendComponent::Main,
                    line: 1,
                    column: 1
                };
                13
            ],
        };
        assert!(report.validate().is_err());
        plugin(PluginRecord {
            event: PluginEvent::StartupFailed,
            plugin_id: secret,
            version: secret,
            run_id: Some(secret),
            reason: Some(classify(secret)),
            exit_code: Some(1),
            signal: None,
            attempt: 0,
            recovery: false,
        });
        let events = take_events().join("\n");
        assert!(!events.contains("private-token"));
        assert!(!events.contains("password"));
        assert!(!events.contains("/Users"));
        assert!(events.contains("invalid-package-id"));
    }
}
