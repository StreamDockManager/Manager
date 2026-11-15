#![allow(dead_code)]
use stream_dock_manager_plugin_host::{PluginDemand, PluginHost};
mod runtime_metadata;
pub use runtime_metadata::ProtocolMetadata;

/// Build an offline disposable host with discovered Node 24 metadata. The
/// production resolver still probes the explicitly selected executable.
pub fn protocol_host(root: &std::path::Path) -> Result<PluginHost, String> {
    let runtimes = stream_dock_manager_plugin_runtimes::RuntimeService::with_dependencies(
        root.join("runtimes"),
        std::sync::Arc::new(ProtocolMetadata),
        std::sync::Arc::new(ProtocolMetadata),
    )?;
    if let Some(error) = runtimes.check()?.error {
        return Err(error);
    }
    let storage = runtimes.storage().clone();
    let host = PluginHost::with_storage(root.join("plugins"), runtimes, storage)?;
    // Synthetic test display metadata, not a claim about this machine
    host.set_registration_environment(
        stream_dock_manager_plugin_host::RegistrationEnvironment::new(Some("en"), Some(1.0))?,
    );
    Ok(host)
}

// Construct demand for the action under test
pub fn demand(host: &mut PluginHost, id: &str, device: &str) {
    host.reconcile_demand(std::collections::HashMap::from([(
        id.into(),
        vec![PluginDemand {
            device_id: device.into(),
            profile_id: "test-profile".into(),
        }],
    )]));
    host.poll();
}
/// Test harnesses select an explicitly supplied executable through the production resolver
/// They never install a runtime or override a version without a harness option
pub fn runtime(host: &PluginHost, id: &str, allow_override: bool) -> Result<(), String> {
    let Some(resolution) = host.preflight(id)?.runtime else {
        return Ok(());
    };
    let requirement = resolution.requirement;
    let Some(path) = std::env::var_os(requirement.kind.env()) else {
        return Err("Supply an explicit test runtime executable".into());
    };
    let installation =
        stream_dock_manager_plugin_runtimes::probe(requirement.kind, std::path::Path::new(&path))?;
    host.runtimes.select(
        Some(id),
        stream_dock_manager_plugin_runtimes::RuntimeSelection {
            kind: requirement.kind,
            channel: requirement.channel.clone(),
            line: installation.line,
            installation_id: None,
            external: Some(path.to_string_lossy().into()),
            acknowledge_override: allow_override,
        },
        Some(&requirement),
    )
}
