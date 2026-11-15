//! Read-only package inspection and bounded directory copying. Never launches plugin code
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct CompatibilityCheck {
    pub code: String,
    pub level: String,
    pub message: String,
    pub remedy: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginPreflight {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: String,
    pub kind: String,
    pub installed: bool,
    pub checks: Vec<CompatibilityCheck>,
    pub can_install: bool,
    pub can_enable: bool,
    #[serde(default)]
    pub runtime: Option<stream_dock_manager_plugin_runtimes::RuntimeResolution>,
}
impl PluginPreflight {
    // Append a compatibility finding
    fn add(&mut self, code: &str, level: &str, message: impl Into<String>, remedy: &str) {
        if level == "blocker" {
            self.can_enable = false;
        }
        self.checks.push(CompatibilityCheck {
            code: code.into(),
            level: level.into(),
            message: message.into(),
            remedy: remedy.into(),
        });
    }
}
/// Apply the same installation-local resolver to package review and installed plugins
pub fn resolve_runtime(
    mut report: PluginPreflight,
    runtimes: &stream_dock_manager_plugin_runtimes::RuntimeService,
) -> PluginPreflight {
    if let Some(runtime) = &report.runtime {
        let resolution = runtimes.resolve(&report.id, &runtime.requirement);
        report.checks.retain(|c| c.code != "runtime_unavailable");
        if resolution.state != "ready" {
            report.checks.push(CompatibilityCheck {
                code: "runtime_unavailable".into(),
                level: "blocker".into(),
                message: resolution.message.clone(),
                remedy: "Select or install a supported runtime in Settings → Runtimes.".into(),
            });
        } else if resolution.overridden {
            report.checks.push(CompatibilityCheck {
                code: "runtime_override".into(),
                level: "warning".into(),
                message: "Using an explicitly acknowledged runtime version override".into(),
                remedy: "Verify the plugin's behavior after restart.".into(),
            });
        }
        report.runtime = Some(resolution);
        report.can_enable = !report.checks.iter().any(|c| c.level == "blocker");
    }
    report
}
// Inspect
pub fn inspect(path: &Path) -> PluginPreflight {
    inspect_with_identity(path, None, &|| Ok(()))
}
// Inspect installed
pub(super) fn inspect_installed(path: &Path, id: &str) -> PluginPreflight {
    inspect_with_identity(path, Some(id), &|| Ok(()))
}
/// Read-only setup inspection, with cancellation/deadline checks between bounded
/// file operations and inside each owned host-helper probe. No plugin is launched.
pub fn inspect_controlled(
    path: &Path,
    id: &str,
    control: &dyn Fn() -> Result<(), String>,
) -> Result<PluginPreflight, String> {
    control()?;
    let report = inspect_with_identity(path, Some(id), control);
    control()?;
    Ok(report)
}
// Check whether cancellation interrupted the inspection
fn interrupted(report: &mut PluginPreflight, control: &dyn Fn() -> Result<(), String>) -> bool {
    if let Err(error) = control() {
        report.can_install = false;
        report.add(
            "interrupted",
            "blocker",
            error,
            "Run the check again when ready.",
        );
        true
    } else {
        false
    }
}
// Inspect with identity
fn inspect_with_identity(
    path: &Path,
    installed_identity: Option<&str>,
    control: &dyn Fn() -> Result<(), String>,
) -> PluginPreflight {
    let mut report = PluginPreflight {
        id: String::new(),
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        version: "unknown".into(),
        source: path.display().to_string(),
        kind: "unknown".into(),
        installed: false,
        checks: vec![],
        can_install: true,
        can_enable: true,
        runtime: None,
    };
    if interrupted(&mut report, control) {
        return report;
    }
    let manifest = (|| -> Result<Vec<u8>, String> {
        let metadata =
            std::fs::symlink_metadata(path.join("manifest.json")).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
            return Err("Manifest is a symlink or exceeds 1 MiB".into());
        }
        std::fs::read(path.join("manifest.json")).map_err(|e| e.to_string())
    })();
    // Marketplace processing can wrap manifests in Elgato's binary container
    // Recognize the observed version-1 signature without interpreting its payload
    // Invalid JSON alone is never evidence that a package is protected
    if manifest
        .as_ref()
        .is_ok_and(|bytes| bytes.starts_with(b"ELGATO\x01\x00"))
    {
        report.kind = "protected_streamdeck".into();
        report.can_install = false;
        report.add(
            "protected_manifest",
            "blocker",
            "This plugin has an Elgato-protected manifest, not a readable JSON manifest.",
            "Use this Marketplace package in Elgato Stream Deck, or obtain an unprotected standalone build from its developer. Manager cannot read its version or import the protected package.",
        );
        return report;
    }
    let manifest = match manifest.and_then(|bytes| {
        serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())
    }) {
        Ok(m) => m,
        Err(error) => {
            report.can_install = false;
            report.add(
                "manifest",
                "blocker",
                format!("Cannot read manifest.json: {error}"),
                "Obtain a fresh standalone package with a valid JSON manifest from its developer. A parse error alone does not establish that a package is protected.",
            );
            return report;
        }
    };
    // OpenDeck merges host-specific overrides before validation. We do not implement
    // that dialect: never silently review/import only the base manifest instead.
    let override_path = path.join(format!("manifest.{}.json", std::env::consts::OS));
    if std::fs::symlink_metadata(&override_path).is_ok() {
        report.can_install = false;
        report.add("opendeck_manifest_override", "blocker",
            "This package uses an OpenDeck platform manifest override that Manager does not support.",
            "Obtain a standalone package with a complete manifest.json for this platform from its developer.");
    }
    if let Some(requirements) = manifest
        .get("StreamDockManager")
        .filter(|v| v.get("MinimumVersion").is_some() || v.get("RequiredCapabilities").is_some())
    {
        let minimum = requirements["MinimumVersion"]
            .as_str()
            .and_then(|v| semver::Version::parse(v).ok());
        let version = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let unsupported = requirements["RequiredCapabilities"]
            .as_array()
            .is_none_or(|caps| {
                caps.iter().any(|cap| {
                    cap.as_str()
                        .is_none_or(|c| !connection_contract::supports_capability(c))
                })
            });
        if minimum.is_none_or(|v| v > version) || unsupported {
            report.can_install = false;
            report.add(
                "manager_contract",
                "blocker",
                "Package requires a newer Manager or an unavailable host capability",
                "Review Manager updates before installing this package.",
            );
        }
    }
    report.id = manifest["UUID"].as_str().unwrap_or("").into();
    report.name = manifest["Name"].as_str().unwrap_or(&report.name).into();
    report.version = manifest["Version"].as_str().unwrap_or("unknown").into();
    if manifest::entry_point(&manifest).is_none()
        && ![
            "CodePath",
            "CodePathMac",
            "CodePathWin",
            "CodePathLinux",
            "CodePathLin",
        ]
        .iter()
        .any(|key| manifest[*key].is_string())
        && !manifest["CodePaths"]
            .as_object()
            .is_some_and(|paths| paths.values().any(Value::is_string))
    {
        report.kind = if manifest::dialect(&manifest) == "vsd" {
            "vendor_builtin"
        } else {
            "invalid"
        }
        .into();
        report.can_install = false;
        report.add(
            "vendor_builtin",
            "blocker",
            if report.kind == "vendor_builtin" {
                "Requires VSD Craft: this built-in has no standalone entry point"
            } else {
                "Plugin manifest has no standalone entry point"
            },
            "Use the equivalent Manager built-in action, or obtain a standalone plugin.",
        );
        return report;
    }
    if let Some(platforms) = manifest["OS"].as_array() {
        let declared: Vec<_> = platforms
            .iter()
            .filter_map(|item| item["Platform"].as_str())
            .collect();
        if !declared
            .iter()
            .any(|platform| *platform == sdk_platform() || (*platform == "win" && cfg!(windows)))
        {
            let host = match std::env::consts::OS {
                "macos" => "macOS",
                "windows" => "Windows",
                "linux" => "Linux",
                other => other,
            };
            let targets = declared
                .into_iter()
                .map(|platform| match platform {
                    "mac" | "macos" => "macOS",
                    "win" | "windows" => "Windows",
                    "linux" => "Linux",
                    other => other,
                })
                .collect::<Vec<_>>()
                .join(", ");
            let message = if targets.is_empty() {
                format!("Choose a plugin package that declares support for {host}.")
            } else {
                format!("This package targets {targets}. Choose a package for {host}.")
            };
            report.add("declared_platform", "blocker", message, "");
        }
    }
    let normalized = match manifest::normalize_with_identity(path, &manifest, installed_identity) {
        Ok(value) => value,
        Err(error) => {
            report.can_install = false;
            report.add(
                "manifest",
                "blocker",
                error,
                "Obtain a complete package with valid manifest references.",
            );
            return report;
        }
    };
    report.id = normalized.id.clone();
    report.name = normalized.name;
    report.version = normalized.version;
    let entry = normalized.entry_point.as_str();
    let file = match safe_file(path, entry) {
        Ok(p) => p,
        Err(error) => {
            report.can_install = false;
            report.add(
                "entry",
                "blocker",
                error,
                "Restore the missing entry point inside the package.",
            );
            return report;
        }
    };
    let inspection = match capabilities::inspect(path, &file, &manifest) {
        Ok(inspection) => inspection,
        Err(error) => {
            report.can_install = false;
            report.add(
                "descriptor",
                "blocker",
                error,
                "Obtain a current package with a matching supported capability descriptor.",
            );
            return report;
        }
    };
    if let Err(error) =
        manifest_schema::validate(&manifest, &report.id, &normalized.dialect, &inspection)
    {
        report.can_install = false;
        report.add(
            "manifest_schema",
            "blocker",
            error,
            "Ask the publisher for a complete manifest matching the declared Stream Deck software version. Manager does not rewrite vendor manifests.",
        );
        return report;
    }
    if normalized.dialect == "streamdeck"
        && matches!(inspection, capabilities::Inspection::VendorBaseline)
    {
        for (field, limitation, blocker) in manifest_schema::unsupported(&manifest) {
            report.add(
                "unsupported_manifest",
                if blocker { "blocker" } else { "warning" },
                format!("{field}: {limitation}"),
                if blocker { "Use a package that does not require this behavior, or request host support before enabling." } else { "Review this unsupported optional presentation or deferred feature before enabling; its declaration is preserved but not applied." },
            );
        }
    }
    report.kind = match file.extension().and_then(|s| s.to_str()) {
        Some("js" | "mjs" | "cjs") => "node",
        Some("html" | "htm") => "browser",
        _ => "native",
    }
    .into();
    match stream_dock_manager_plugin_runtimes::manifest_requirement(&manifest, &file) {
        Ok(Some(requirement)) => {
            report.kind = requirement.kind.name().into();
            report.runtime = Some(stream_dock_manager_plugin_runtimes::RuntimeResolution {
                recommended_line: None,
                requirement,
                selected: None,
                state: "missing".into(),
                message: "Select or install a compatible runtime in Settings → Runtimes".into(),
                overridden: false,
            });
            report.add(
                "runtime_unavailable",
                "blocker",
                "Runtime selection required",
                "Choose a private or existing compatible runtime.",
            );
        }
        Ok(None) => {}
        Err(error) => {
            report.can_install = false;
            report.add(
                "runtime_manifest",
                "blocker",
                error,
                "Ask the plugin author to fix the runtime declaration.",
            );
        }
    }
    if report.kind == "native" {
        inspect_dependencies(&file, &mut report, control);
        if interrupted(&mut report, control) {
            return report;
        }
        let mut header = [0u8; 4];
        if std::fs::File::open(&file)
            .and_then(|mut f| f.read_exact(&mut header))
            .is_ok()
        {
            let platform = if header == *b"\x7fELF" {
                Some("linux")
            } else if header.starts_with(b"MZ") {
                Some("windows")
            } else if [0xfeedfacfu32, 0xfeedface, 0xcafebabe, 0xcafebabf]
                .iter()
                .any(|magic| header == magic.to_le_bytes() || header == magic.to_be_bytes())
            {
                Some("macos")
            } else {
                None
            };
            if platform.is_some_and(|p| p != std::env::consts::OS) {
                report.add(
                    "binary_platform",
                    "blocker",
                    "Executable belongs to another operating system",
                    "Obtain the correct native package for this platform.",
                );
            }
        }

        match binary_architectures(&file) {
            Ok(arches) if arches.is_empty() => report.add(
                "architecture",
                "unknown",
                "Native executable format is not recognized",
                "Check interpreter availability or obtain a platform binary.",
            ),
            Ok(arches) if !arches.contains(&std::env::consts::ARCH) => {
                let level = if cfg!(target_os = "macos")
                    && arches.contains(&"x86_64")
                    && cfg!(target_arch = "aarch64")
                {
                    "warning"
                } else {
                    "blocker"
                };
                report.add(
                    "architecture",
                    level,
                    format!(
                        "Binary architecture: {}; host: {}",
                        arches.join(", "),
                        std::env::consts::ARCH
                    ),
                    "Install a compatible binary; Intel macOS binaries may require Rosetta.",
                );
            }
            Ok(arches) => report.add("architecture", "ok", arches.join(", "), ""),
            Err(error) => report.add(
                "architecture",
                "blocker",
                error,
                "Restore a valid executable.",
            ),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&file).is_ok_and(|m| m.permissions().mode() & 0o111 == 0) {
                report.add(
                    "executable",
                    "blocker",
                    "Entry point is not executable",
                    "Restore executable permissions in the source package.",
                );
            }
        }
        report.add(
            "native_dependencies",
            "unknown",
            "Dynamic and system library dependencies require runtime qualification",
            "Review package dependency instructions. Enable explicitly and inspect startup health.",
        );
    }
    for action in manifest["Actions"].as_array().unwrap() {
        if interrupted(&mut report, control) {
            return report;
        }
        if action_control_types(action).is_empty() {
            report.add(
                "controllers",
                "warning",
                "An action declares unsupported controller types",
                "Use actions matching supported buttons, dials, or touch surfaces.",
            );
        }
        for field in ["PropertyInspectorPath", "Icon"] {
            if let Some(relative) = action[field].as_str() {
                let exists = if field == "Icon"
                    && matches!(normalized.dialect.as_str(), "openaction" | "streamdeck")
                {
                    vendor_presentation::image_path(
                        path,
                        relative,
                        normalized.dialect == "openaction",
                    )
                    .is_some()
                } else {
                    safe_file(path, relative).is_ok_and(|file| file.is_file())
                };
                if !exists {
                    if field == "PropertyInspectorPath" {
                        report.can_install = false;
                    }
                    report.add(
                        "asset",
                        if field == "PropertyInspectorPath" {
                            "blocker"
                        } else {
                            "warning"
                        },
                        format!("Missing {field}: {relative}"),
                        "Restore the referenced file inside the package.",
                    );
                }
            }
        }
    }
    // OpenAction has no host software minimum field. Legacy Stream Deck
    // metadata in compatible packages must not be compared with Manager's own
    // version; platform and executable requirements are checked independently.
    if let Some(minimum) = manifest["Software"]["MinimumVersion"]
        .as_str()
        .filter(|_| normalized.dialect != "openaction")
    {
        let contract = connection_contract::vendor(&normalized.dialect)
            .expect("normalized manifest has a supported vendor dialect")
            .application_version;
        match manifest::compare_vendor_versions(minimum, contract) {
            Ok(std::cmp::Ordering::Greater) => report.add("vendor_contract", "blocker", format!("Requires {} software {minimum}; Manager implements the documented {contract} compatibility subset", normalized.dialect), "Use a plugin supporting the documented contract or request the missing host features."),
            Ok(_) => report.add("vendor_contract", "warning", format!("Vendor minimum {minimum} is evaluated against the {contract} compatibility subset, independently of Manager {}", env!("CARGO_PKG_VERSION")), "Review tested APIs and vendor-only dependency limitations."),
            Err(error) => { report.can_install = false; report.add("vendor_contract", "blocker", error, "Ask the publisher to correct Software.MinimumVersion."); }
        }
    }
    if let Some(platforms) = manifest["OS"].as_array() {
        for platform in platforms.iter().filter(|p| {
            p["Platform"]
                .as_str()
                .is_some_and(|s| s == sdk_platform() || s == "win" && cfg!(windows))
        }) {
            if interrupted(&mut report, control) {
                return report;
            }
            if let Some(minimum) = platform[if normalized.dialect == "openaction" {
                if platform["Version"].is_null() {
                    "MinimumVersion"
                } else {
                    "Version"
                }
            } else {
                "MinimumVersion"
            }]
            .as_str()
            {
                if manifest::vendor_version(minimum).is_err() {
                    report.can_install = false;
                    report.add(
                        "minimum_os",
                        "blocker",
                        "Invalid minimum operating-system version",
                        "Ask the publisher to fix the manifest.",
                    );
                    continue;
                }
                #[cfg(target_os = "macos")]
                if let Ok(version) =
                    probe_controlled("/usr/bin/sw_vers", &["-productVersion"], control)
                {
                    if manifest::compare_vendor_versions(version.trim(), minimum)
                        .is_ok_and(|o| o.is_lt())
                    {
                        report.add(
                            "minimum_os",
                            "blocker",
                            format!("Requires macOS {minimum}; this host is {}", version.trim()),
                            "Use a package supporting the installed operating system.",
                        );
                    }
                }
                #[cfg(windows)]
                if let Ok(version) = probe_controlled("cmd.exe", &["/C", "ver"], control) {
                    let parts: Vec<u64> = version
                        .split(|c: char| !c.is_ascii_digit())
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    let major = manifest::vendor_version(minimum).unwrap()[0];
                    if major >= 11 && parts.get(2).is_some_and(|build| *build < 22000) {
                        report.add(
                            "minimum_os",
                            "blocker",
                            format!("Requires Windows {minimum}"),
                            "Use a package supporting the installed Windows version.",
                        );
                    }
                }
            }
        }
    }
    if manifest.get("ApplicationsToMonitor").is_some() {
        if cfg!(target_os = "windows") {
            report.add(
                "application_monitoring_precision", "warning",
                "Application lifecycle uses one-second process snapshots; shorter-lived processes may not be observed",
                "Applications already running when monitoring begins are a baseline, not synthetic launch events.",
            );
        } else if !cfg!(target_os = "macos") {
            report.add(
                "application_monitoring_unavailable", "warning",
                "Vendor application lifecycle monitoring is unavailable on this platform",
                "The vendor manifest defines macOS bundle identifiers and Windows executable names only.",
            );
        }
    }
    for field in ["Profiles", "DataSources"] {
        if manifest.get(field).is_some()
            && !(field == "Profiles" && normalized.dialect == "streamdeck")
        {
            report.add(
                "unsupported_manifest",
                "warning",
                format!("{field} is outside the supported API subset"),
                "Features using this declaration may not work.",
            );
        }
    }
    report.add(
        "api",
        "unknown",
        "Dynamic SDK calls cannot be fully verified without running the plugin",
        "Review the compatibility contract and runtime health after enabling.",
    );
    report
}

#[cfg(target_os = "macos")]
// Inspect the architectures of a macOS native dependency
fn macos_dependency_architecture(architectures: &[&str], host: &str) -> Option<&'static str> {
    match host {
        "aarch64" if architectures.contains(&"aarch64") => Some("arm64"),
        "aarch64" | "x86_64" if architectures.contains(&"x86_64") => Some("x86_64"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
// Inspect the linked macOS native dependencies
fn macos_dependencies(output: &str) -> std::collections::BTreeSet<&str> {
    output
        .lines()
        // Each architecture has an unindented heading; only indented lines are libraries
        .filter(|line| line.starts_with(char::is_whitespace))
        .map(|line| {
            line.trim()
                .split(" (compatibility")
                .next()
                .unwrap_or("")
                .trim()
        })
        .filter(|dependency| !dependency.is_empty())
        .collect()
}

// Inspect dependencies
fn inspect_dependencies(
    file: &Path,
    report: &mut PluginPreflight,
    control: &dyn Fn() -> Result<(), String>,
) {
    if interrupted(report, control) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let architectures = binary_architectures(file).unwrap_or_default();
        let file_name = file.to_string_lossy();
        let mut args = vec![];
        if let Some(architecture) =
            macos_dependency_architecture(&architectures, std::env::consts::ARCH)
        {
            args.extend(["-arch", architecture]);
        }
        args.extend(["-L", file_name.as_ref()]);
        match probe_controlled("/usr/bin/otool", &args, control) {
            Ok(output) => {
                for dependency in macos_dependencies(&output) {
                    if interrupted(report, control) {
                        return;
                    }
                    if dependency.starts_with("/System/Library/")
                        || dependency.starts_with("/usr/lib/")
                    {
                        report.add(
                            "dependency",
                            "ok",
                            format!("System library: {dependency}"),
                            "",
                        );
                    } else if let Some(relative) = dependency
                        .strip_prefix("@loader_path/")
                        .or_else(|| dependency.strip_prefix("@executable_path/"))
                    {
                        let found = file
                            .parent()
                            .is_some_and(|parent| parent.join(relative).is_file());
                        report.add(
                            "dependency",
                            if found { "ok" } else { "blocker" },
                            format!("Bundled library: {dependency}"),
                            if found {
                                ""
                            } else {
                                "Restore the required bundled library."
                            },
                        );
                    } else if dependency.starts_with('/') {
                        let found = Path::new(dependency).is_file();
                        report.add(
                            "dependency",
                            if found { "ok" } else { "blocker" },
                            format!("Library: {dependency}"),
                            if found {
                                ""
                            } else {
                                "Install the library required by this plugin."
                            },
                        );
                    } else {
                        report.add(
                            "dependency",
                            "unknown",
                            format!("Loader-resolved library: {dependency}"),
                            "Check package runtime search paths and startup health.",
                        );
                    }
                }
            }
            Err(_) => report.add(
                "dependency_inspection",
                "unknown",
                "Native library inspection utility unavailable",
                "Install the platform developer tools or review package dependency instructions.",
            ),
        }
    }
    #[cfg(target_os = "linux")]
    {
        match probe_controlled("readelf", &["-d", &file.to_string_lossy()], control) {
            Ok(output) => {
                let libraries = probe_controlled("ldconfig", &["-p"], control).ok();
                for line in output.lines().filter(|line| line.contains("(NEEDED)")) {
                    if interrupted(report, control) {
                        return;
                    }
                    if let Some(name) = line
                        .split_once('[')
                        .and_then(|(_, s)| s.split_once(']').map(|(s, _)| s))
                    {
                        let bundled = file.parent().is_some_and(|p| {
                            p.join(name).is_file() || p.join("lib").join(name).is_file()
                        });
                        let system = libraries.as_ref().is_some_and(|list| {
                            list.lines()
                                .any(|line| line.split_whitespace().next() == Some(name))
                        });
                        report.add("dependency",if bundled||system{"ok"}else{"unknown"},format!("Required library: {name}"),if bundled||system{""}else{"Install the library, or verify the executable's runtime library paths."});
                    }
                }
            }
            Err(_) => report.add(
                "dependency_inspection",
                "unknown",
                "ELF dependency inspection unavailable",
                "Install binutils or review package dependency instructions.",
            ),
        }
    }
    #[cfg(windows)]
    {
        match probe_controlled("dumpbin", &["/dependents", &file.to_string_lossy()], control) {
            Ok(output)=>for name in output.lines().map(str::trim).filter(|line|line.to_ascii_lowercase().ends_with(".dll")) {
                let local=file.parent().is_some_and(|p|p.join(name).is_file());
                let system=std::env::var_os("SystemRoot").is_some_and(|p|PathBuf::from(p).join("System32").join(name).is_file());
                report.add("dependency",if local||system{"ok"}else{"unknown"},format!("Required DLL: {name}"),if local||system{""}else{"Install the plugin's runtime dependencies or verify DLL search paths."});
            },
            Err(_)=>report.add("dependency_inspection","unknown","PE dependency inspection utility unavailable","Review the package's required runtimes; Visual Studio dumpbin can inspect DLL imports."),
        }
    }
}
// Inspect source
pub fn inspect_source(path: &Path) -> Result<PluginPreflight, String> {
    if path.is_dir() {
        return Ok(inspect(path));
    }
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    extract_archive(path, temporary.path())?;
    let root = packages::select_root(temporary.path())?;
    let mut report = inspect(&root);
    report.source = path.display().to_string();
    Ok(report)
}

/// Bounded probes only for explicitly configured runtimes and host utilities, never package entries
pub fn probe(program: &str, args: &[&str]) -> Result<String, String> {
    probe_controlled(program, args, &|| Ok(()))
}
/// A helper probe owns its descendants and observes cancellation while waiting
/// for either process exit or output EOF. It never runs a plugin entry point.
pub fn probe_controlled(
    program: &str,
    args: &[&str],
    control: &dyn Fn() -> Result<(), String>,
) -> Result<String, String> {
    control()?;
    let mut command = stream_dock_manager_plugin_runtimes::background_command(program);
    command
        .env("LC_ALL", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::process_tree::configure(&mut command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let tree = match crate::process_tree::ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    struct OwnedProbe {
        child: std::process::Child,
        tree: crate::process_tree::ProcessTree,
    }
    impl Drop for OwnedProbe {
        // Release resources held by this owner
        fn drop(&mut self) {
            self.tree.terminate();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let mut owned = OwnedProbe { child, tree };
    let pipe = owned
        .child
        .stdout
        .take()
        .ok_or("Probe output unavailable")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut out = vec![];
        let result = pipe.take(65537).read_to_end(&mut out).map(|_| out);
        let _ = send.send(result);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut output = None;
    let mut status = None;
    loop {
        control()?;
        if std::time::Instant::now() >= deadline {
            return Err("Probe timed out".into());
        }
        if output.is_none() {
            match receive.try_recv() {
                Ok(result) => {
                    output = Some(result.map_err(|_| "Probe output could not be read")?);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("Probe output reader stopped".into())
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if output.as_ref().is_some_and(|bytes| bytes.len() > 65536) {
            return Err("Probe exceeded its output limit".into());
        }
        if status.is_none() {
            // Keep Unix PID/group ownership reserved until descendant cleanup
            status = crate::process_tree::poll_exit(&mut owned.child, Some(&owned.tree));
        }
        if let (Some(status), Some(bytes)) = (status, &output) {
            return if status.success() {
                Ok(String::from_utf8_lossy(bytes).into())
            } else {
                Err("Probe failed".into())
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
// Determine the architectures present in a native executable
fn binary_architectures(path: &Path) -> Result<Vec<&'static str>, String> {
    let mut b = vec![];
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(65536)
        .read_to_end(&mut b)
        .map_err(|e| e.to_string())?;
    let cpu = |v| match v {
        0x01000007 | 62 | 0x8664 => "x86_64",
        0x0100000c | 183 | 0xaa64 => "aarch64",
        7 | 3 | 0x14c => "x86",
        _ => "unknown",
    };
    let le = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    };
    let be = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
    };
    if b.starts_with(b"\x7fELF") {
        return Ok(b
            .get(18..20)
            .map(|s| {
                cpu(if b[5] == 1 {
                    u16::from_le_bytes(s.try_into().unwrap())
                } else {
                    u16::from_be_bytes(s.try_into().unwrap())
                } as u32)
            })
            .into_iter()
            .collect());
    }
    if b.starts_with(b"MZ") {
        let i = le(60).ok_or("Truncated PE header")? as usize;
        if b.get(i..i + 4) != Some(b"PE\0\0") {
            return Err("Invalid PE header".into());
        }
        return Ok(vec![cpu(u16::from_le_bytes(
            b.get(i + 4..i + 6)
                .ok_or("Truncated PE machine")?
                .try_into()
                .unwrap(),
        ) as u32)]);
    }
    if [Some(0xfeedfacf), Some(0xfeedface)].contains(&le(0)) {
        return Ok(le(4).map(cpu).into_iter().collect());
    }
    if [Some(0xcafebabe), Some(0xcafebabf)].contains(&be(0)) {
        let count = be(4).unwrap_or(0).min(64);
        let stride = if be(0) == Some(0xcafebabf) { 32 } else { 20 };
        return Ok((0..count)
            .filter_map(|i| be(8 + i as usize * stride).map(cpu))
            .collect());
    }
    Ok(vec![])
}
// Copy package
pub fn copy_package(source: &Path, target: &Path) -> Result<(), String> {
    // Copy for the current operation
    fn copy(
        source: &Path,
        target: &Path,
        files: &mut usize,
        total: &mut u64,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("Plugin directory nesting exceeds limit".into());
        }
        let m = std::fs::symlink_metadata(source).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() {
            return Err("Plugin symlinks are rejected".into());
        }
        if m.is_dir() {
            std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
            for e in std::fs::read_dir(source).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                copy(
                    &e.path(),
                    &target.join(e.file_name()),
                    files,
                    total,
                    depth + 1,
                )?;
            }
        } else if m.is_file() {
            *files += 1;
            *total = total.checked_add(m.len()).ok_or("Plugin size overflow")?;
            if *files > 4096 || m.len() > 64 * 1024 * 1024 || *total > 256 * 1024 * 1024 {
                return Err("Plugin exceeds package limits".into());
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let input = options.open(source).map_err(|e| e.to_string())?;
            if !input.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Special package file rejected".into());
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .map_err(|e| e.to_string())?;
            let actual = std::io::copy(&mut input.take(64 * 1024 * 1024 + 1), &mut output)
                .map_err(|e| e.to_string())?;
            if actual != m.len() {
                return Err("Package file changed or exceeds size limits".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    target,
                    std::fs::Permissions::from_mode(0o600 | (m.permissions().mode() & 0o111)),
                )
                .map_err(|e| e.to_string())?;
            }
        } else {
            return Err("Special files are rejected".into());
        }
        Ok(())
    }
    let before = packages::inventory(source)?;
    copy(source, target, &mut 0, &mut 0, 0)?;
    if packages::inventory(target)? != before {
        return Err("Source folder changed while copying".into());
    }
    Ok(())
}
// Discover for the current operation
pub fn discover(roots: Vec<PathBuf>, installed: &[PluginInfo]) -> Vec<PluginPreflight> {
    // Walk the package tree for inspection
    fn walk(root: &Path, depth: usize, visited: &mut usize, out: &mut Vec<PathBuf>) {
        if depth > 7 || *visited >= 10000 || out.len() >= 500 {
            return;
        }
        *visited += 1;
        if !std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            return;
        }
        if root.join("manifest.json").is_file() {
            out.push(root.into());
            return;
        }
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                walk(&e.path(), depth + 1, visited, out);
            }
        }
    }
    let roots = if roots.is_empty() {
        default_sources()
    } else {
        roots
    };
    let mut paths = vec![];
    let mut visited = 0;
    for root in roots {
        walk(&root, 0, &mut visited, &mut paths);
    }
    paths.sort();
    paths.dedup();
    let mut reports: Vec<_> = paths.iter().map(|p| inspect(p)).collect();
    for r in &mut reports {
        r.installed = installed.iter().any(|p| p.id == r.id);
    }
    reports.sort_by_key(|r| {
        (
            !is_user_source(&r.source),
            r.name.to_lowercase(),
            r.source.clone(),
        )
    });
    reports
}
// Choose the default platform source paths
pub fn default_sources() -> Vec<PathBuf> {
    let mut roots = vsd_sources();
    roots.extend(opendeck_sources());
    roots
}
// Select source paths for the requested platform
pub fn sources_for(source: &str) -> Result<Vec<PathBuf>, String> {
    match source {
        "all" => Ok(default_sources()),
        "vsd" => Ok(vsd_sources()),
        "opendeck" => Ok(opendeck_sources()),
        "streamdeck" => Err("Import from the Stream Deck installation is no longer supported; use folder or package import for compatible unencrypted plugins".into()),
        _ => Err("Unknown plugin import source".into()),
    }
}
// OpenDeck uses Tauri app_config_dir with identifier "opendeck", then plugins/
// https://github.com/nekename/OpenDeck/blob/main/src-tauri/src/shared.rs
// https://github.com/nekename/OpenDeck/blob/main/src-tauri/tauri.conf.json
fn opendeck_sources() -> Vec<PathBuf> {
    opendeck_source_paths(
        std::env::consts::OS,
        std::env::var_os("HOME").as_deref().map(Path::new),
        std::env::var_os("APPDATA").as_deref().map(Path::new),
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
    )
}
// Resolve platform entry points declared by OpenDeck
fn opendeck_source_paths(
    os: &str,
    home: Option<&Path>,
    appdata: Option<&Path>,
    xdg: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = vec![];
    match os {
        "macos" => {
            if let Some(home) = home {
                roots.push(home.join("Library/Application Support/opendeck/plugins"));
            }
        }
        "windows" => {
            if let Some(base) = appdata.filter(|p| !p.as_os_str().is_empty()) {
                roots.push(base.join("opendeck/plugins"));
            }
        }
        "linux" => {
            if let Some(base) = xdg.filter(|p| p.is_absolute()) {
                roots.push(base.join("opendeck/plugins"));
            } else if let Some(home) = home {
                roots.push(home.join(".config/opendeck/plugins"));
            }
            if let Some(home) = home {
                roots.push(home.join(".var/app/me.amankhanna.opendeck/config/opendeck/plugins"));
            }
        }
        _ => {}
    }
    roots.sort();
    roots.dedup();
    roots
}

// Resolve the platform source paths declared by VSD
fn vsd_sources() -> Vec<PathBuf> {
    let mut roots = vec![];
    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from(
            "/Applications/VSD Craft.app/Contents/Resources",
        ));
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(
                PathBuf::from(home).join("Library/Application Support/HotSpot/StreamDock/plugins"),
            );
        }
    }
    #[cfg(windows)]
    {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                roots.push(PathBuf::from(base).join("HotSpot/StreamDock/plugins"));
            }
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var) {
                for name in ["VSD Craft", "StreamDock"] {
                    roots.push(PathBuf::from(&base).join(name));
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            roots.push(home.join(".config/HotSpot/StreamDock/plugins"));
            roots.push(home.join(".local/share/HotSpot/StreamDock/plugins"));
        }
        roots.push(PathBuf::from("/opt/VSD Craft"));
    }
    roots
}

// Check whether the value is user source
fn is_user_source(source: &str) -> bool {
    let source = source.to_lowercase();
    source.contains("application support")
        || source.contains("appdata")
        || source.contains("/.config/")
        || source.contains("/.local/share/")
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the package representation
    fn package(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("manifest.json"),r#"{"UUID":"test.folder","Name":"Folder Plugin","Version":"1.0","CodePath":"index.html","Actions":[{"UUID":"test.folder.action","Name":"Action"}]}"#).unwrap();
        std::fs::write(
            path.join("index.html"),
            "<!doctype html><title>fixture</title>",
        )
        .unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
        manifest["SDKVersion"] = json!(1);
        manifest["StreamDockManager"] = json!({"ManifestDialect":"vsd"});
        crate::lifecycle_tests::current_descriptor(path, &mut manifest);
    }
    #[test]
    // Verify openaction codepaths only package uses selected entry in preflight
    fn openaction_codepaths_only_package_uses_selected_entry_in_preflight() {
        let root = tempfile::tempdir().unwrap();
        let manifest = json!({
            "UUID":"org.openaction.codepaths", "Name":"CodePaths", "Author":"Fixture", "Version":"1.0.0", "Icon":"icon",
            "CodePaths": {"x86_64-pc-windows-msvc":"plugin.html", "x86_64-apple-darwin":"plugin.html", "aarch64-apple-darwin":"plugin.html", "x86_64-unknown-linux-gnu":"plugin.html", "aarch64-unknown-linux-gnu":"plugin.html"},
            "OS":[{"Platform":"windows"},{"Platform":"mac"},{"Platform":"linux"}],
            "Actions":[{"UUID":"org.openaction.codepaths.action","Name":"Action","Icon":"icon","States":[{}]}]
        });
        std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(root.path().join("plugin.html"), "<!doctype html>").unwrap();
        std::fs::write(
            root.path().join("icon.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        )
        .unwrap();
        let report = inspect(root.path());
        assert!(report.can_install, "{:?}", report.checks);
        assert!(!report.checks.iter().any(|check| check.code == "platform"));
        assert_eq!(report.kind, "browser");
    }

    // Construct the icon package fixture
    fn icon_package(root: &Path) -> PathBuf {
        let path = root.join("me.amankhanna.oadiscord.sdPlugin");
        std::fs::create_dir_all(path.join("actions")).unwrap();
        std::fs::create_dir_all(path.join("pi")).unwrap();
        std::fs::write(path.join("plugin.html"), "<!doctype html>").unwrap();
        std::fs::write(path.join("pi/simple.html"), "<!doctype html>").unwrap();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"/>"#;
        std::fs::write(path.join("icon.svg"), svg).unwrap();
        let actions: Vec<_> = ["mute_0", "pushtotalk_0"]
            .into_iter()
            .map(|name| {
                std::fs::write(path.join(format!("actions/{name}.svg")), svg).unwrap();
                json!({
                    "UUID":format!("me.amankhanna.oadiscord.{name}"), "Name":name,
                    "Icon":format!("actions/{name}"), "PropertyInspectorPath":"pi/simple.html",
                    "Controllers":["Keypad"], "States":[{"Image":format!("actions/{name}")}]
                })
            })
            .collect();
        let manifest = json!({
            "Name":"Icon fixture", "Author":"Tests", "Version":"0.5.1", "Icon":"icon",
            "CodePath":"plugin.html",
            "OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}],
            "Actions":actions
        });
        std::fs::write(path.join("manifest.json"), manifest.to_string()).unwrap();
        path
    }

    #[test]
    // Verify openaction icon stems pass source and staged reviews without rewriting manifest
    fn openaction_icon_stems_pass_source_and_staged_reviews_without_rewriting_manifest() {
        let root = tempfile::tempdir().unwrap();
        let source = icon_package(root.path());
        let before = packages::inventory(&source).unwrap();
        let report = inspect(&source);
        assert!(
            report.can_install && report.can_enable,
            "{:?}",
            report.checks
        );
        assert!(
            !report.checks.iter().any(|check| check.code == "asset"),
            "{:?}",
            report.checks
        );
        let plugin = read_plugin(&source).unwrap();
        assert!(plugin
            .info
            .actions
            .iter()
            .all(|action| action.icon.is_some()));
        let prepared = PluginHost::prepare_install(root.path(), &source).unwrap();
        let staged = PluginHost::prepared_preflight(&prepared);
        assert!(
            staged.can_install && staged.can_enable,
            "{:?}",
            staged.checks
        );
        assert!(
            !staged.checks.iter().any(|check| check.code == "asset"),
            "{:?}",
            staged.checks
        );
        assert_eq!(packages::inventory(&source).unwrap(), before);
        assert_eq!(PluginHost::prepared_inventory(&prepared), before);
    }

    #[test]
    // Verify icon review keeps missing icons as warnings and inspectors as blockers
    fn icon_review_keeps_missing_icons_as_warnings_and_inspectors_as_blockers() {
        let root = tempfile::tempdir().unwrap();
        let source = icon_package(root.path());
        std::fs::remove_file(source.join("actions/mute_0.svg")).unwrap();
        let report = inspect(&source);
        assert!(report.can_install && report.can_enable);
        let assets: Vec<_> = report
            .checks
            .iter()
            .filter(|check| check.code == "asset")
            .collect();
        assert_eq!(assets.len(), 1, "{:?}", report.checks);
        assert_eq!(assets[0].message, "Missing Icon: actions/mute_0");
        assert_eq!(assets[0].level, "warning");
        // Inspector references remain exact: image suffix fallback is not valid here
        std::fs::rename(
            source.join("pi/simple.html"),
            source.join("pi/simple.html.svg"),
        )
        .unwrap();
        let report = inspect(&source);
        assert!(!report.can_install && !report.can_enable);
    }

    #[test]
    // Verify streamdeck icon stems use thumbnail resolution
    fn streamdeck_icon_stems_use_thumbnail_resolution() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = manifest_schema::fixture("org.vendor.icons", "plugin.html");
        manifest["Actions"][0]["Icon"] = json!("actions/mute_0");
        std::fs::create_dir(root.path().join("actions")).unwrap();
        std::fs::write(root.path().join("actions/mute_0@2x.png"), b"image fixture").unwrap();
        std::fs::write(root.path().join("plugin.html"), "<!doctype html>").unwrap();
        std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
        let report = inspect(root.path());
        assert!(report.can_install, "{:?}", report.checks);
        assert!(
            !report.checks.iter().any(|check| check.code == "asset"),
            "{:?}",
            report.checks
        );
    }

    #[test]
    // Verify unreadable vendor manifest does not claim encryption or bypass protection
    fn unreadable_vendor_manifest_does_not_claim_encryption_or_bypass_protection() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("manifest.json"), b"not a JSON manifest").unwrap();
        let report = inspect(root.path());
        assert!(!report.can_install);
        assert!(!report.can_enable);
        let check = report
            .checks
            .iter()
            .find(|check| check.code == "manifest")
            .unwrap();
        assert_eq!(report.kind, "unknown");
        assert!(check.message.contains("Cannot read manifest.json:"));
        assert!(check.remedy.contains("does not establish"));
    }
    #[test]
    // Verify protected marketplace manifests are identified without reading payloads
    fn protected_marketplace_manifests_are_identified_without_reading_payloads() {
        let root = tempfile::tempdir().unwrap();
        // Synthetic header only: no vendor payload or keys are needed
        let bytes = b"ELGATO\x01\x00\xffsynthetic protected data";
        std::fs::write(root.path().join("manifest.json"), bytes).unwrap();
        let report = inspect(root.path());
        assert_eq!(report.kind, "protected_streamdeck");
        assert_eq!(report.version, "unknown");
        assert!(!report.can_install);
        assert!(!report.can_enable);
        assert!(report.runtime.is_none());
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].code, "protected_manifest");
        assert!(report.checks[0].remedy.contains("Elgato Stream Deck"));
        assert!(!report.checks[0].message.contains("line 1"));
        assert_eq!(
            std::fs::read(root.path().join("manifest.json")).unwrap(),
            bytes
        );
        let install_root = root.path().join("manager");
        assert!(PluginHost::prepare_install(&install_root, root.path()).is_err());
    }

    #[test]
    // Verify arbitrary binary manifest is not classified as elgato protected
    fn arbitrary_binary_manifest_is_not_classified_as_elgato_protected() {
        for bytes in [b"ELGATO".as_slice(), b"ELGATO\x02\x00", b"\xff\xfeinvalid"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("manifest.json"), bytes).unwrap();
            let report = inspect(root.path());
            assert_ne!(report.kind, "protected_streamdeck");
            assert!(!report.can_install);
            assert_eq!(report.checks[0].code, "manifest");
        }
    }

    #[test]
    // Verify opendeck paths cover native and flatpak without relative XDG
    fn opendeck_paths_cover_native_and_flatpak_without_relative_xdg() {
        let home = Path::new("/home/test");
        assert_eq!(
            opendeck_source_paths("macos", Some(home), None, None),
            vec![home.join("Library/Application Support/opendeck/plugins")]
        );
        assert_eq!(
            opendeck_source_paths("windows", None, Some(Path::new("/roaming")), None),
            vec![PathBuf::from("/roaming/opendeck/plugins")]
        );
        let linux = opendeck_source_paths("linux", Some(home), None, Some(Path::new("/custom")));
        assert!(linux.contains(&PathBuf::from("/custom/opendeck/plugins")));
        assert!(
            linux.contains(&home.join(".var/app/me.amankhanna.opendeck/config/opendeck/plugins"))
        );
        assert!(!linux.contains(&home.join(".config/opendeck/plugins")));
        assert!(
            opendeck_source_paths("linux", Some(home), None, Some(Path::new("relative")))
                .contains(&home.join(".config/opendeck/plugins"))
        );
        assert!(opendeck_source_paths("windows", None, None, None).is_empty());
    }
    #[test]
    // Verify opendeck override is blocked without modifying source
    fn opendeck_override_is_blocked_without_modifying_source() {
        let root = tempfile::tempdir().unwrap();
        package(root.path());
        let override_path = root
            .path()
            .join(format!("manifest.{}.json", std::env::consts::OS));
        std::fs::write(&override_path, r#"{"CodePath":"other.html"}"#).unwrap();
        let before = packages::inventory(root.path()).unwrap();
        let report = inspect(root.path());
        assert!(!report.can_install);
        assert!(!report.can_enable);
        assert!(report
            .checks
            .iter()
            .any(|c| c.code == "opendeck_manifest_override"));
        assert_eq!(packages::inventory(root.path()).unwrap(), before);
    }
    #[test]
    // Verify opendeck openaction package discovery stages disabled and preserves source
    fn opendeck_openaction_package_discovery_stages_disabled_and_preserves_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root
            .path()
            .join("opendeck/plugins/org.openaction.fixture.sdPlugin");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("index.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        let manifest = json!({"Name":"OpenAction fixture","Author":"Tests","Version":"1.0.0","Icon":"icon","CodePath":"index.html","OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}],"Actions":[{"UUID":"org.openaction.fixture.action","Name":"Action","Icon":"icon","States":[{}]}]});
        std::fs::write(source.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(
            source.join("icon.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"/>"#,
        )
        .unwrap();
        let before = packages::inventory(&source).unwrap();
        let reports = discover(vec![root.path().join("opendeck/plugins")], &[]);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].can_install, "{:?}", reports[0].checks);
        assert_eq!(reports[0].kind, "browser");
        let mut host = crate::lifecycle_tests::host(&root.path().join("manager"));
        let prepared = PluginHost::prepare_install(&host.installation_root(), &source).unwrap();
        assert!(host.list().is_empty());
        let info = host.register_prepared(prepared).unwrap();
        assert!(!info.enabled);
        assert!(discover(vec![source.clone()], &[info])[0].installed);
        assert_eq!(packages::inventory(&source).unwrap(), before);
    }
    #[test]
    // Verify installed vendor sources are explicit and unknown sources rejected
    fn installed_vendor_sources_are_explicit_and_unknown_sources_rejected() {
        assert!(sources_for("unknown").is_err());
        assert_eq!(sources_for("all").unwrap(), default_sources());
        assert_eq!(sources_for("opendeck").unwrap(), opendeck_sources());
        assert!(sources_for("streamdeck")
            .unwrap_err()
            .contains("no longer supported"));
    }
    #[test]
    // Verify supported discovery and staging preserve source and require owned install
    fn supported_discovery_and_staging_preserve_source_and_require_owned_install() {
        let root = tempfile::tempdir().unwrap();
        let source = root
            .path()
            .join("External/Plugins/org.vendor.fixture.sdPlugin");
        std::fs::create_dir_all(&source).unwrap();
        let manifest = manifest_schema::fixture("org.vendor.fixture", "index.html");
        std::fs::write(source.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(
            source.join("index.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        let before = packages::inventory(&source).unwrap();
        let reports = discover(vec![root.path().join("External")], &[]);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].can_install);
        let mut host = crate::lifecycle_tests::host(&root.path().join("manager"));
        let prepared = PluginHost::prepare_install(&host.installation_root(), &source).unwrap();
        assert!(
            host.list().is_empty(),
            "staging must not install or enable the plugin"
        );
        let info = host.register_prepared(prepared).unwrap();
        assert!(!info.enabled);
        assert!(info.capabilities.is_none());
        assert_eq!(packages::inventory(&source).unwrap(), before);
        let reports = discover(vec![source.clone()], &[info]);
        assert!(reports[0].installed);
        let mut bad = manifest;
        bad["CodePath"] = json!("../escape.html");
        std::fs::write(source.join("manifest.json"), bad.to_string()).unwrap();
        assert!(!inspect(&source).can_install);
        assert!(PluginHost::prepare_install(&host.installation_root(), &source).is_err());
        assert_eq!(
            host.list().len(),
            1,
            "invalid import must preserve installed data"
        );
    }
    #[test]
    // Verify folder scan explains builtins missing entries and installed IDs
    fn folder_scan_explains_builtins_missing_entries_and_installed_ids() {
        let root = tempfile::tempdir().unwrap();
        let standalone = root.path().join("plugins/test.folder.sdPlugin");
        package(&standalone);
        let builtin = root.path().join("plugins/builtin.sdPlugin");
        std::fs::create_dir_all(&builtin).unwrap();
        std::fs::write(
            builtin.join("manifest.json"),
            r#"{"Name":"Built-in","Actions":[]}"#,
        )
        .unwrap();
        let installed = read_plugin(&standalone).unwrap().info;
        let reports = discover(vec![root.path().into()], &[installed]);
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().any(|r| r.installed && r.can_install));
        assert!(reports
            .iter()
            .any(|r| r.kind == "invalid" && !r.can_install));
        std::fs::remove_file(standalone.join("index.html")).unwrap();
        let report = inspect(&standalone);
        assert!(!report.can_install);
        assert!(report.checks.iter().any(|c| c.code == "manifest"));
    }
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "Reads the local VSD Craft installation and uses loopback; never executes plugins"]
    // Verify installed VSD folder can be copied independently without touching source
    fn installed_vsd_folder_can_be_copied_independently_without_touching_source() {
        let root = Path::new("/Applications/VSD Craft.app/Contents/Resources");
        if !root.exists() {
            return;
        }
        let reports = discover(vec![root.into()], &[]);
        assert!(reports.iter().any(|r| r.kind == "vendor_builtin"));
        let source = reports
            .iter()
            .find(|r| r.can_install && r.kind == "browser")
            .expect("No standalone browser package discovered");
        let manifest = std::fs::read(Path::new(&source.source).join("manifest.json")).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let mut host = crate::lifecycle_tests::socket_host(temporary.path());
        let prepared =
            PluginHost::prepare_install(&host.installation_root(), Path::new(&source.source))
                .unwrap();
        let info = host.register_prepared(prepared).unwrap();
        assert!(!info.enabled);
        assert!(host
            .installation_root()
            .join(&info.id)
            .join("manifest.json")
            .is_file());
        assert_eq!(
            std::fs::read(Path::new(&source.source).join("manifest.json")).unwrap(),
            manifest
        );
        host.shutdown();
    }
    #[test]
    // Verify directory copy preserves source and rejects symlinks
    fn directory_copy_preserves_source_and_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("original.sdPlugin");
        package(&source);
        let before = std::fs::read(source.join("manifest.json")).unwrap();
        let target = root.path().join("copy");
        copy_package(&source, &target).unwrap();
        assert_eq!(std::fs::read(source.join("manifest.json")).unwrap(), before);
        assert_eq!(std::fs::read(target.join("manifest.json")).unwrap(), before);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source.join("index.html"), source.join("link")).unwrap();
            assert!(copy_package(&source, &root.path().join("bad"))
                .unwrap_err()
                .contains("symlink"));
        }
    }
    #[test]
    #[cfg(target_os = "macos")]
    // Verify universal binary headings are not libraries and dependencies are deduplicated
    fn universal_binary_headings_are_not_libraries_and_dependencies_are_deduplicated() {
        let output = "/Applications/Audio Plugin/sdmicmute (architecture x86_64):\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n\t@loader_path/Audio Helper.dylib (compatibility version 1.0.0, current version 1.0.0)\n/Applications/Audio Plugin/sdmicmute (architecture arm64):\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n\t@loader_path/Audio Helper.dylib (compatibility version 1.0.0, current version 1.0.0)\n";
        assert_eq!(
            macos_dependencies(output),
            std::collections::BTreeSet::from([
                "/usr/lib/libSystem.B.dylib",
                "@loader_path/Audio Helper.dylib"
            ])
        );
        assert_eq!(macos_dependencies("/Applications/Audio Plugin/sdmicmute:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n"), std::collections::BTreeSet::from(["/usr/lib/libSystem.B.dylib"]));
    }
    #[test]
    #[cfg(target_os = "macos")]
    // Verify dependency checks use the native slice or intel for rosetta
    fn dependency_checks_use_the_native_slice_or_intel_for_rosetta() {
        assert_eq!(
            macos_dependency_architecture(&["x86_64", "aarch64"], "aarch64"),
            Some("arm64")
        );
        assert_eq!(
            macos_dependency_architecture(&["x86_64", "aarch64"], "x86_64"),
            Some("x86_64")
        );
        assert_eq!(
            macos_dependency_architecture(&["x86_64"], "aarch64"),
            Some("x86_64")
        );
        assert_eq!(macos_dependency_architecture(&["aarch64"], "x86_64"), None);
    }
    #[test]
    // Verify native headers distinguish architectures without executing code
    fn native_headers_distinguish_architectures_without_executing_code() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("binary");
        let mut bytes = vec![0; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&183u16.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(binary_architectures(&path).unwrap(), vec!["aarch64"]);
        bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(binary_architectures(&path).unwrap(), vec!["x86_64"]);
    }
}

#[cfg(test)]
mod manager_contract_tests {
    use super::*;
    #[test]
    // Verify controlled inspection stops between manifest actions without running package code
    fn controlled_inspection_stops_between_manifest_actions_without_running_package_code() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let actions: Vec<_> = (0..100)
            .map(|i| serde_json::json!({"UUID":format!("org.test.inspected.{i}"),"Name":"Fixture"}))
            .collect();
        let mut manifest = serde_json::json!({"UUID":"org.test.inspected","Name":"Fixture","Version":"1.0.0","CodePath":"plugin.html","SDKVersion":2,"Actions":actions,"StreamDockManager":{"MinimumVersion":"0.1.0","RequiredCapabilities":["plugin-protocol-v1"]}});
        std::fs::write(
            root.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("plugin.html"),
            "<!doctype html><title>Never executed</title>",
        )
        .unwrap();
        manifest["SDKVersion"] = json!(1);
        manifest["StreamDockManager"]["ManifestDialect"] = json!("vsd");
        crate::lifecycle_tests::current_descriptor(root.path(), &mut manifest);
        let calls = AtomicUsize::new(0);
        let result = inspect_controlled(root.path(), "org.test.inspected", &|| {
            if calls.fetch_add(1, Ordering::SeqCst) >= 8 {
                Err("fixture cancelled".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "fixture cancelled");
        assert!(calls.load(Ordering::SeqCst) < 15);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3);
    }
    #[test]
    // Verify standalone package requires declared manager contract
    fn standalone_package_requires_declared_manager_contract() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({"UUID":"org.test.future","Name":"Future","Version":"2.0.0","CodePath":"plugin.html","SDKVersion":2,"Actions":[{"UUID":"org.test.future.action","Name":"Action"}],"StreamDockManager":{"MinimumVersion":"99.0.0","RequiredCapabilities":["plugin-protocol-v1"]}});
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("plugin.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        let report = inspect(dir.path());
        assert!(!report.can_install);
        assert!(!report.can_enable);
        assert!(report.checks.iter().any(|c| c.code == "manager_contract"));
    }
}

#[cfg(all(test, unix))]
mod controlled_probe_tests {
    use super::*;
    #[test]
    // Verify cancellation before spawn never runs a helper
    fn cancellation_before_spawn_never_runs_a_helper() {
        let result = probe_controlled(
            "/definitely/not/a/program",
            &[],
            &|| Err("cancelled".into()),
        );
        assert_eq!(result.unwrap_err(), "cancelled");
    }
    #[test]
    // Verify root exit with inherited stdout cleans up descendants without waiting for eof
    fn root_exit_with_inherited_stdout_cleans_up_descendants_without_waiting_for_eof() {
        let started = std::time::Instant::now();
        assert_eq!(
            probe("/bin/sh", &["-c", "(sleep 30) & printf ready"]).unwrap(),
            "ready"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    // Verify cancelled running probe reaps its owned root
    fn cancelled_running_probe_reaps_its_owned_root() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("probe.pid");
        let marker_arg = marker.to_str().unwrap();
        let started = std::time::Instant::now();
        let result = probe_controlled(
            "/bin/sh",
            &[
                "-c",
                "printf '%s' $$ > \"$1\"; sleep 30",
                "probe",
                marker_arg,
            ],
            &|| {
                if marker.exists() {
                    Err("cancel requested".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.unwrap_err(), "cancel requested");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }
    #[test]
    // Verify excessive probe output is rejected
    fn excessive_probe_output_is_rejected() {
        assert!(probe("/usr/bin/head", &["-c", "70000", "/dev/zero"])
            .unwrap_err()
            .contains("output limit"));
    }
}
