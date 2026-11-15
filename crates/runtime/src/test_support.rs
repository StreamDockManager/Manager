//! Opt-in support for disposable tests and qualification hosts. Never attaches hardware
use crate::Runtime;
use std::{path::PathBuf, sync::Arc};

/// Start the runtime services needed by a headless test without desktop automation or downloads
pub fn headless(root: PathBuf) -> Result<Arc<Runtime>, String> {
    headless_with_recovery(root, false)
}

/// Preserve startup recovery selection in isolated native lifecycle fixtures
pub fn headless_with_recovery(root: PathBuf, recovery: bool) -> Result<Arc<Runtime>, String> {
    let runtime = Runtime::build_credentials_recovery(root, false, Default::default(), recovery)?;
    crate::plugin_monitor::start(Arc::downgrade(&runtime));
    Ok(runtime)
}

/// Drive the production plugin synchronization path when no device actor is running
pub fn synchronize_plugins(runtime: &Runtime) {
    crate::devices::sync_plugins(runtime);
}

/// Pause dequeue, retaining the production admission snapshot for each genuine wire request
pub fn hold_vendor_profile_switches(
    runtime: &Arc<Runtime>,
) -> Result<crate::vendor_profiles::VendorProfileSwitchHold, String> {
    runtime.hold_vendor_profile_switches()
}
/// Drive actual authenticated host effects in an isolated profile-only qualification host
/// Other effects are not acted on; this never accepts fabricated wire/effect inputs
pub fn pump_vendor_profile_switches(runtime: &Arc<Runtime>) -> usize {
    let effects = runtime.plugins.lock().unwrap().drain_effects();
    let mut count = 0;
    for effect in effects {
        if let stream_dock_manager_plugin_host::HostEffect::SwitchProfile(effect) = effect {
            runtime.queue_vendor_profile_switch(effect);
            count += 1;
        }
    }
    count
}

/// Exercise production image encoding with validation enabled for performance fixtures
pub fn encode_image(
    png: Vec<u8>,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
) -> Result<Vec<u8>, String> {
    crate::encoding::choose_shared_quality(
        crate::media::shared_bytes(png),
        supports_jpeg,
        policy,
        photographic,
        false,
        90,
    )
    .map(|bytes| bytes.to_vec())
}

#[cfg(test)]
mod tests {
    #[test]
    // Verify that isolated hosts preserve explicit recovery selection
    fn isolated_hosts_preserve_explicit_recovery_selection() {
        for recovery in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let runtime =
                super::headless_with_recovery(root.path().join("manager"), recovery).unwrap();
            assert_eq!(runtime.recovery_status().active, recovery);
            runtime.shutdown();
        }
    }
}
