//! Read-only unit observations and exact-variant manufacturer release metadata
//! Firmware images and updater programs are never downloaded or executed here
use crate::Runtime;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::io::Read;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex,
    },
    time::Duration,
};

pub type QueryReply = mpsc::Sender<Result<Option<String>, String>>;
#[derive(Clone)]
pub(crate) struct QuerySender {
    sender: mpsc::SyncSender<QueryReply>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}
impl QuerySender {
    // Couple a successful firmware enqueue to the connection's input-worker wake
    pub(crate) fn new(
        sender: mpsc::SyncSender<QueryReply>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { sender, wake }
    }
    // Publish the request first so a woken worker always sees queued work
    fn try_send(&self, reply: QueryReply) -> Result<(), mpsc::TrySendError<QueryReply>> {
        self.sender.try_send(reply)?;
        (self.wake)();
        Ok(())
    }
}
#[derive(Default)]
pub struct Firmware {
    pub(crate) queries: Mutex<HashMap<String, (u64, QuerySender)>>,
    observations: Mutex<HashMap<String, (u64, FirmwareStatus)>>,
    release_checks: Mutex<HashMap<String, (u64, u64)>>,
    next_check: AtomicU64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct FirmwareStatus {
    pub state: String,
    pub installed: Option<String>,
    pub installed_source: Option<String>,
    pub latest: Option<String>,
    pub latest_state: String,
    pub latest_message: String,
    pub latest_source: Option<String>,
    pub latest_checked: Option<String>,
    pub latest_stale: bool,
    pub last_checked: Option<String>,
    pub instructions: Option<String>,
    pub instructions_shared: bool,
    pub message: String,
    pub stale: bool,
}
/// MiraBox's documented decimal firmware numbering, including leading zeros
/// Never parse USB releases or treat a capability minimum as a release record
pub fn mirabox_version(raw: &str) -> Option<(u32, u32)> {
    let (major, minor) = raw.split_once('.')?;
    if major.is_empty()
        || major.len() > 4
        || minor.is_empty()
        || minor.len() > 6
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}
/// The pinned Python wrapper selects the first nonempty NUL-delimited string
/// Fail closed on input events, invalid UTF-8, control bytes and unknown versions
pub fn parse_input_report(bytes: &[u8]) -> Option<String> {
    let value = bytes.split(|b| *b == 0).find(|part| !part.is_empty())?;
    if value.len() > 63 {
        return None;
    }
    let raw = std::str::from_utf8(value).ok()?;
    // The SDK returns the complete version, e.g. V4.VSD N4 Pro.02.010.
    // Decimal-only recognition used to discard these valid device responses
    // Retail protocols can return a dotted 3/4-component version. Display it
    // without interpreting it as MiraBox's two-component release numbering
    if version_parts(raw).is_none() {
        let parts: Vec<_> = raw.split('.').collect();
        if !(3..=4).contains(&parts.len())
            || parts
                .iter()
                .any(|p| p.is_empty() || p.len() > 6 || !p.bytes().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
    }
    Some(raw.into())
}

/// Execute exactly the declared read through the connection's sole input owner
/// Nonzero HID report IDs are framing, not part of the version text
pub(crate) fn read_firmware_report(
    report: &hardware_core::FirmwareReport,
    windows: bool,
    read: impl FnOnce(hardware_core::FirmwareReportKind, &mut [u8]) -> Result<usize, String>,
) -> Result<Option<String>, String> {
    if windows && !report.windows_supported {
        return Err("Firmware detection for this device is not supported on Windows by the source protocol.".into());
    }
    if !(2..=1025).contains(&report.length) {
        return Err("Invalid firmware report length".into());
    }
    let mut bytes = vec![0; report.length as usize];
    bytes[0] = report.report_id;
    let size = read(report.kind, &mut bytes)?;
    if size > bytes.len() {
        return Err("Firmware response exceeds the requested report length".into());
    }
    let bytes = &bytes[..size];
    let payload = if report.report_id != 0 {
        if bytes.first() != Some(&report.report_id) {
            return Ok(None);
        }
        &bytes[1..]
    } else {
        bytes
    };
    Ok(parse_input_report(payload))
}

const RELEASE_URL: &str =
    "https://cdn1.key123.vip/StreamDock/firmware/firmware-version-mirabox.json";
#[cfg(test)]
const METADATA_LIMIT: u64 = 256 * 1024;
struct ReleaseTarget {
    #[cfg(test)]
    key: &'static str,
    prefix: &'static str,
}
// Release target
fn release_target(model: &str, variant: &str) -> Option<ReleaseTarget> {
    match (model, variant) {
        ("n4-pro", "vsdinside") => Some(ReleaseTarget {
            #[cfg(test)]
            key: "VSD_N4Pro",
            prefix: "V4.VSD N4 Pro",
        }),
        ("n4-pro", "mirabox") => Some(ReleaseTarget {
            #[cfg(test)]
            key: "MBox-N4Pro",
            prefix: "V4.N4 Pro",
        }),
        ("k1-pro", "usb-6603-1015") => Some(ReleaseTarget {
            #[cfg(test)]
            key: "K1Pro",
            prefix: "V3.K1_PRO",
        }),
        ("k1-pro", "usb-6603-1019") => Some(ReleaseTarget {
            #[cfg(test)]
            key: "K1ProEU",
            prefix: "V3.K1_PRO_EU",
        }),
        // No E-specific release is established by the manufacturer catalog
        _ => None,
    }
}
// Parse a bounded vendor firmware prefix and numeric version pair
fn version_parts(raw: &str) -> Option<(&str, (u32, u32))> {
    if raw.len() > 63
        || raw.is_empty()
        || raw.trim() != raw
        || !raw
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b" ._-".contains(&c))
    {
        return None;
    }
    if let Some(version) = mirabox_version(raw) {
        return Some(("", version));
    }
    let (prefix, last) = raw.rsplit_once('.')?;
    let (prefix, first) = prefix.rsplit_once('.')?;
    // Model-qualified strings are displayed intact. Their prefix must match
    // exactly before two releases can be compared
    if !prefix.starts_with('V') || !prefix.contains('.') {
        return None;
    }
    Some((prefix, mirabox_version(&format!("{first}.{last}"))?))
}
// Read bounded
#[cfg(test)]
fn read_bounded(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > METADATA_LIMIT {
        return Err("Firmware metadata exceeds size limit".into());
    }
    Ok(bytes)
}
// Release from bytes
#[cfg(test)]
fn release_from_bytes(bytes: &[u8], target: &ReleaseTarget) -> Result<Option<String>, String> {
    // Some manufacturer CDN responses are gzip encoded even with identity requested
    let decoded = if bytes.starts_with(&[0x1f, 0x8b]) {
        read_bounded(flate2::read::GzDecoder::new(bytes))?
    } else {
        read_bounded(bytes)?
    };
    let catalog: serde_json::Value =
        serde_json::from_slice(&decoded).map_err(|_| "Invalid firmware release metadata")?;
    let releases = catalog
        .get("CurrentVersion")
        .and_then(|v| v.as_object())
        .ok_or("Invalid firmware release catalog")?;
    let Some(entry) = releases.get(target.key) else {
        return Ok(None);
    };
    let raw = entry
        .get("Version")
        .and_then(|v| v.as_str())
        .ok_or("Missing firmware release version")?;
    let (prefix, _) = version_parts(raw).ok_or("Invalid firmware release version")?;
    if prefix != target.prefix {
        return Err("Firmware release does not match the device variant".into());
    }
    Ok(Some(raw.into()))
}
// Fetch release
#[cfg(test)]
fn fetch_release(target: &ReleaseTarget) -> Result<Option<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("StreamDockManager/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(RELEASE_URL)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Manufacturer firmware lookup failed: {e}"))?;
    release_from_bytes(&read_bounded(response)?, target)
}
// Explain firmware version comparison only when the observed prefix and freshness are compatible
fn comparison_message(
    status: &FirmwareStatus,
    target: &ReleaseTarget,
    latest: &str,
) -> &'static str {
    match (status.installed.as_deref().and_then(version_parts), version_parts(latest)) {
        (Some((prefix, installed)), Some((published_prefix, published)))
            if prefix == target.prefix && prefix == published_prefix && !status.stale => {
                match installed.cmp(&published) {
                    std::cmp::Ordering::Less => "A newer firmware release is available. Use the manufacturer's updater to install it.",
                    std::cmp::Ordering::Equal => "Installed firmware matches the manufacturer's published release.",
                    std::cmp::Ordering::Greater => "Installed firmware is newer than the manufacturer's published release.",
                }
            }
        _ => "Manufacturer release found. The installed version cannot be compared reliably.",
    }
}
impl Runtime {
    // Encode the observed firmware and connection generation into a cache identity
    pub(crate) fn firmware_observation_key(&self, device_id: &str) -> String {
        self.firmware
            .observations
            .lock()
            .unwrap()
            .get(device_id)
            .map(|(epoch, status)| {
                format!(
                    "{epoch}:{}:{:?}:{:?}:{}",
                    status.state, status.installed, status.last_checked, status.stale
                )
            })
            .unwrap_or_default()
    }
    // Explain why a vibration variant fails its firmware prerequisite
    pub(crate) fn vibration_firmware_error(
        &self,
        device_id: &str,
        capability: &hardware_core::VibrationCapabilities,
        variant: &str,
    ) -> Option<String> {
        let minimum = capability.firmware_requirements.get(variant)?;
        let unknown = || {
            Some("Vibration is disabled until the connected device firmware is known. Check firmware first.".into())
        };
        let epochs = self.connection_epochs.lock().unwrap();
        let Some(epoch) = epochs.get(device_id) else {
            return unknown();
        };
        let observations = self.firmware.observations.lock().unwrap();
        let Some((_, status)) = observations
            .get(device_id)
            .filter(|(e, s)| e == epoch && !s.stale)
        else {
            return unknown();
        };
        let Some((prefix, installed)) = status.installed.as_deref().and_then(version_parts) else {
            return unknown();
        };
        let expected_prefix = match variant {
            "mirabox" => "V4.N4 Pro",
            "vsdinside" => "V4.VSD N4 Pro",
            _ => return unknown(),
        };
        if !prefix.is_empty() && prefix != expected_prefix {
            return unknown();
        }
        let Some(required) = mirabox_version(minimum) else {
            return unknown();
        };
        (installed < required).then(|| {
            format!("Vibration requires firmware {minimum} or later. Check for firmware updates.")
        })
    }
    // Discard the selected firmware connection state
    pub(crate) fn forget_firmware_connection(&self, device_id: &str) {
        self.firmware.queries.lock().unwrap().remove(device_id);
        self.firmware.observations.lock().unwrap().remove(device_id);
        self.firmware
            .release_checks
            .lock()
            .unwrap()
            .remove(device_id);
    }
    // Read firmware status for the currently connected device generation
    pub fn firmware_status(&self, device_id: &str) -> Result<FirmwareStatus, String> {
        let snapshot = self.snapshot();
        let device = snapshot
            .devices
            .iter()
            .find(|d| d.id == device_id && d.connected)
            .ok_or("Device disconnected")?;
        let epoch = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .copied()
            .unwrap_or(0);
        if !device.simulated {
            if let Some((observed_epoch, status)) =
                self.firmware.observations.lock().unwrap().get(device_id)
            {
                if *observed_epoch == epoch {
                    return Ok(status.clone());
                }
            }
        }
        let supported = device.manifest.capabilities.firmware_query
            && (!cfg!(target_os = "windows")
                || device
                    .manifest
                    .capabilities
                    .firmware_report
                    .as_ref()
                    .is_none_or(|r| r.windows_supported));
        // Simulators cannot open physical guidance; canonical registry matching rejects invented identities
        let guidance = (!device.simulated)
            .then(|| hardware_registry::firmware_instructions(&device.model_id, &device.variant_id))
            .flatten();
        let instructions_shared = guidance.as_ref().is_some_and(|(_, shared)| *shared);
        let instructions = guidance.map(|(url, _)| url);
        let target = release_target(&device.model_id, &device.variant_id);
        Ok(FirmwareStatus {
            state: if supported {
                "not_checked"
            } else {
                "unsupported"
            }
            .into(),
            installed: None,
            installed_source: None,
            latest: None,
            last_checked: None,
            instructions,
            instructions_shared,
            latest_state: if target.is_some() || (device.simulated && supported) {
                "not_checked"
            } else {
                "unsupported"
            }
            .into(),
            latest_message: if device.simulated {
                ""
            } else if target.is_some() {
                "Check for updates to read the manufacturer's release catalog."
            } else {
                "No verified firmware release catalog entry is available for this device variant."
            }
            .into(),
            latest_source: target.map(|_| RELEASE_URL.into()),
            latest_checked: None,
            latest_stale: false,
            message: if device.simulated {
                "Simulator fixture: firmware is unmeasured; no device or network queries are made."
            } else if supported {
                "Installed firmware has not been read yet."
            } else {
                "Firmware detection is unsupported for this device on this platform."
            }
            .into(),
            stale: false,
        })
    }
    // Observe firmware
    pub(crate) fn observe_firmware(
        &self,
        device_id: &str,
        epoch: u64,
        result: Result<Option<String>, String>,
    ) {
        if self.connection_epochs.lock().unwrap().get(device_id) != Some(&epoch) {
            return;
        }
        let Ok(mut status) = self.firmware_status(device_id) else {
            return;
        };
        status.last_checked = Some(chrono::Utc::now().to_rfc3339());
        match result {
            Ok(Some(raw)) => {
                status.state = "installed_known".into();
                status.stale = false;
                status.message = "Installed firmware read from the connected device.".into();
                status.installed = Some(raw.clone());
                status.installed_source = Some(
                    if self
                        .snapshot()
                        .devices
                        .iter()
                        .find(|d| d.id == device_id)
                        .and_then(|d| d.manifest.capabilities.firmware_report.as_ref())
                        .is_some_and(|r| r.kind == hardware_core::FirmwareReportKind::Feature)
                    {
                        "mirajazz-hid-get-feature-report-v1"
                    } else {
                        "vendor-sdk-hid-get-input-report-v1"
                    }
                    .into(),
                );
            }
            Ok(None) => {
                status.state = "installed_unknown".into();
                status.stale = status.installed.is_some();
                status.message = "The device did not return a recognizable firmware version. Retry the check; USB release is a separate value.".into();
            }
            Err(error) => {
                status.state = "failed".into();
                status.stale = status.installed.is_some();
                status.message = error;
            }
        }
        {
            let epochs = self.connection_epochs.lock().unwrap();
            if epochs.get(device_id) != Some(&epoch) {
                return;
            }
            let mut state = self.state.lock().unwrap();
            if let Some(device) = state.devices.iter_mut().find(|d| d.id == device_id) {
                device.firmware = if status.state == "installed_known" && !status.stale {
                    status.installed.clone().unwrap_or_default()
                } else {
                    String::new()
                };
                if let Ok(contract) = crate::device_contract::resolve(device) {
                    device.manifest = contract.manifest;
                }
            }
        }
        let target = self
            .snapshot()
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .and_then(|d| release_target(&d.model_id, &d.variant_id));
        let epochs = self.connection_epochs.lock().unwrap();
        if epochs.get(device_id) == Some(&epoch) {
            let mut observations = self.firmware.observations.lock().unwrap();
            // A HID observation must not replace metadata completed while the
            // device response was being processed
            if let Some((_, current)) = observations.get(device_id).filter(|(e, _)| *e == epoch) {
                status.latest = current.latest.clone();
                status.latest_state = current.latest_state.clone();
                status.latest_message = current.latest_message.clone();
                status.latest_source = current.latest_source.clone();
                status.latest_checked = current.latest_checked.clone();
                status.latest_stale = current.latest_stale;
            }
            if status.latest_state == "available" && !status.latest_stale {
                if let (Some(latest), Some(target)) = (status.latest.as_deref(), target) {
                    status.latest_message = comparison_message(&status, &target, latest).into();
                }
            }
            observations.insert(device_id.into(), (epoch, status));
        }
        drop(epochs);
        // The actor re-resolves its owned encoder before restoring surfaces
        let _ = self
            .device_commands
            .send(crate::devices::Request::RestoreDevice(
                device_id.into(),
                epoch,
            ));
    }
    // Check firmware
    pub fn check_firmware(&self, device_id: &str) -> Result<FirmwareStatus, String> {
        self.check_firmware_query(
            device_id,
            None::<fn(&ReleaseTarget) -> Result<Option<String>, String>>,
        )
    }
    // Check firmware with
    #[cfg(test)]
    fn check_firmware_with(
        &self,
        device_id: &str,
        fetch: impl FnOnce(&ReleaseTarget) -> Result<Option<String>, String>,
    ) -> Result<FirmwareStatus, String> {
        self.check_firmware_query(device_id, Some(fetch))
    }
    // Read the installed version independently of catalog discovery and its uncertainty rules
    fn check_firmware_query(
        &self,
        device_id: &str,
        fetch: Option<impl FnOnce(&ReleaseTarget) -> Result<Option<String>, String>>,
    ) -> Result<FirmwareStatus, String> {
        let status = self.firmware_status(device_id)?;
        let snapshot = self.snapshot();
        let device = snapshot
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("Device disconnected")?;
        if device.simulated || status.state == "unsupported" {
            return Ok(status);
        }
        let epoch = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .copied()
            .ok_or("Device disconnected")?;
        let check = self.firmware.next_check.fetch_add(1, Ordering::Relaxed);
        self.firmware
            .release_checks
            .lock()
            .unwrap()
            .insert(device_id.into(), (epoch, check));
        let query = self
            .firmware
            .queries
            .lock()
            .unwrap()
            .get(device_id)
            .filter(|(e, _)| *e == epoch)
            .map(|(_, q)| q.clone());
        let result = query
            .ok_or_else(|| "Firmware query unavailable".to_string())
            .and_then(|query| {
                let (reply, response) = mpsc::channel();
                query
                    .try_send(reply)
                    .map_err(|_| "Firmware query is busy or the device disconnected".to_string())?;
                response
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|_| "Firmware query timed out".to_string())
                    .and_then(|r| r)
            });
        if self.connection_epochs.lock().unwrap().get(device_id) != Some(&epoch) {
            return Err("Device connection changed".into());
        }
        self.observe_firmware(device_id, epoch, result);
        // Update discovery is independent of whether the installed read succeeded
        if let (Some(target), Some(fetch)) =
            (release_target(&device.model_id, &device.variant_id), fetch)
        {
            let result = fetch(&target);
            self.observe_release(device_id, epoch, check, &target, result);
        }
        if self.connection_epochs.lock().unwrap().get(device_id) != Some(&epoch) {
            return Err("Device connection changed".into());
        }
        self.firmware_status(device_id)
    }
    // Observe release
    fn observe_release(
        &self,
        device_id: &str,
        epoch: u64,
        check: u64,
        target: &ReleaseTarget,
        result: Result<Option<String>, String>,
    ) {
        if self.connection_epochs.lock().unwrap().get(device_id) != Some(&epoch) {
            return;
        }
        let checks = self.firmware.release_checks.lock().unwrap();
        if checks.get(device_id) != Some(&(epoch, check)) {
            return;
        }
        let Ok(fallback) = self.firmware_status(device_id) else {
            return;
        };
        let epochs = self.connection_epochs.lock().unwrap();
        if epochs.get(device_id) != Some(&epoch) {
            return;
        }
        let mut observations = self.firmware.observations.lock().unwrap();
        let (_, status) = observations
            .entry(device_id.into())
            .or_insert((epoch, fallback));
        // Use the most recent installed observation, including responses that
        // arrived while the HTTPS lookup was in flight
        status.latest_checked = Some(chrono::Utc::now().to_rfc3339());
        match result {
            Ok(Some(latest)) => {
                status.latest_state = "available".into();
                status.latest_stale = false;
                status.latest_message = comparison_message(status, target, &latest).into();
                status.latest = Some(latest);
            }
            Ok(None) => {
                status.latest = None;
                status.latest_state = "not_listed".into();
                status.latest_stale = false;
                status.latest_message =
                    "The manufacturer catalog has no release for this exact device variant.".into();
            }
            Err(error) => {
                status.latest_state = "failed".into();
                status.latest_stale = status.latest.is_some();
                status.latest_message = format!(
                    "{error}. Retry the check or open the manufacturer's update instructions."
                );
            }
        }
    }
    // Open firmware instructions
    pub fn open_firmware_instructions(&self, device_id: &str) -> Result<(), String> {
        let status = self.firmware_status(device_id)?;
        if status
            .instructions
            .as_deref()
            .is_some_and(|url| !hardware_core::approved_firmware_instructions(url))
        {
            return Err("Firmware instructions URL is not approved".into());
        }
        crate::actions::open(
            status
                .instructions
                .as_deref()
                .ok_or("No approved manufacturer instructions for this variant")?,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Simulators never inherit physical guidance and real known variants keep explicit shared provenance
    #[test]
    fn instructions_follow_hardware_source_and_absence_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:instructions";
        runtime.simulate_device(true, id).unwrap();
        assert!(runtime.firmware_status(id).unwrap().instructions.is_none());
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.model_id = "k1-pro".into();
            device.variant_id = "usb-5548-1025".into();
        }
        let status = runtime.firmware_status(id).unwrap();
        assert!(status.instructions_shared);
        assert_eq!(
            status.instructions,
            hardware_registry::firmware_instructions("n4-pro", "vsdinside").map(|(url, _)| url)
        );
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.model_id = "n4-pro".into();
            device.variant_id = "mirabox".into();
        }
        let status = runtime.firmware_status(id).unwrap();
        assert!(!status.instructions_shared);
        assert_eq!(
            status.instructions,
            hardware_registry::firmware_instructions("n4-pro", "mirabox").map(|(url, _)| url)
        );
        runtime
            .state
            .lock()
            .unwrap()
            .devices
            .iter_mut()
            .find(|d| d.id == id)
            .unwrap()
            .model_id = "unknown".into();
        assert!(runtime.firmware_status(id).unwrap().instructions.is_none());
        runtime.shutdown();
    }
    #[test]
    // Verify that queried h1 firmware selects geometry and failed observations revoke it
    fn queried_h1_firmware_selects_geometry_and_failed_observations_revoke_it() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:h1-pro:contract";
        runtime.simulate_device(true, id).unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.variant_id = device.manifest.variants[0].id.clone();
        }
        let epoch = runtime.connection_epochs.lock().unwrap()[id];
        for raw in ["V25.H1 Pro.01.000", "V3.H1Pro.00.001"] {
            let observed = parse_input_report(format!("\0{raw}\0").as_bytes()).unwrap();
            runtime.observe_firmware(id, epoch, Ok(Some(observed)));
            let device = runtime
                .snapshot()
                .devices
                .into_iter()
                .find(|d| d.id == id)
                .unwrap();
            assert!(device
                .manifest
                .capabilities
                .image_output_unavailable_reason
                .is_none());
            let contract = crate::device_contract::resolve(&device).unwrap();
            assert_eq!(contract.key_transform.unwrap().rotation, 270);
            assert_eq!(
                contract.manifest.control("key-1").unwrap().image_width,
                Some(64)
            );
        }
        // A recognizable trailing model/family token never qualifies an
        // unknown leading family, even when the generic parser accepts it
        let raw = "V4.H1Pro.V3.00.001";
        let observed = parse_input_report(format!("\0{raw}\0").as_bytes()).unwrap();
        runtime.observe_firmware(id, epoch, Ok(Some(observed)));
        assert!(runtime
            .snapshot()
            .devices
            .iter()
            .find(|device| device.id == id)
            .unwrap()
            .manifest
            .capabilities
            .image_output_unavailable_reason
            .is_some());
        runtime.observe_firmware(id, epoch, Err("synthetic query failure".into()));
        let device = runtime
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert!(device.firmware.is_empty());
        assert!(device
            .manifest
            .capabilities
            .image_output_unavailable_reason
            .is_some());
        runtime.shutdown();
    }
    #[test]
    // Verify that vibration requires current compatible firmware observation
    fn vibration_requires_current_compatible_firmware_observation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:vibration-contract";
        runtime.simulate_device(true, id).unwrap();
        let capability = {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.variant_id = "mirabox".into();
            device.manifest.capabilities.vibration.clone().unwrap()
        };
        let epoch = runtime.connection_epochs.lock().unwrap()[id];
        assert!(runtime
            .vibration_firmware_error(id, &capability, "mirabox")
            .is_some());
        runtime.observe_firmware(id, epoch, Ok(Some("V3.H1Pro.99.999".into())));
        assert!(runtime
            .vibration_firmware_error(id, &capability, "mirabox")
            .is_some());
        runtime.observe_firmware(id, epoch, Ok(Some("V4.N4 Pro.02.010".into())));
        assert!(runtime
            .vibration_firmware_error(id, &capability, "mirabox")
            .is_none());
        runtime.observe_firmware(id, epoch, Ok(None));
        assert!(runtime
            .vibration_firmware_error(id, &capability, "mirabox")
            .is_some());
        runtime.shutdown();
    }
    #[test]
    // Verify that firmware reads use model report kind ID length and platform
    fn firmware_reads_use_model_report_kind_id_length_and_platform() {
        use hardware_core::{FirmwareReport, FirmwareReportKind};
        for (kind, id, length, raw) in [
            (FirmwareReportKind::Input, 0, 513, "V4.N4 Pro.02.010"),
            (FirmwareReportKind::Input, 4, 513, "V3.K1_PRO.03.009"),
            (FirmwareReportKind::Feature, 1, 20, "1.0.0.0"),
        ] {
            let contract = FirmwareReport {
                kind,
                report_id: id,
                length,
                windows_supported: id != 1,
            };
            let version = read_firmware_report(&contract, false, |actual_kind, bytes| {
                assert_eq!(actual_kind, kind);
                assert_eq!(bytes.len(), length as usize);
                assert_eq!(bytes[0], id);
                assert!(bytes[1..].iter().all(|b| *b == 0));
                bytes[1..1 + raw.len()].copy_from_slice(raw.as_bytes());
                Ok(raw.len() + 2)
            })
            .unwrap();
            assert_eq!(version.as_deref(), Some(raw));
            if id != 0 {
                assert!(read_firmware_report(&contract, false, |_, bytes| {
                    bytes[0] = id + 1;
                    Ok(1)
                })
                .unwrap()
                .is_none());
            }
            assert!(read_firmware_report(&contract, false, |_, _| Ok(0))
                .unwrap()
                .is_none());
            assert!(
                read_firmware_report(&contract, false, |_, bytes| Ok(bytes.len() + 1)).is_err()
            );
            assert!(
                read_firmware_report(&contract, false, |_, _| Err("Disconnected".into())).is_err()
            );
        }
        let retail = FirmwareReport {
            kind: FirmwareReportKind::Feature,
            report_id: 1,
            length: 20,
            windows_supported: false,
        };
        assert!(
            read_firmware_report(&retail, true, |_, _| panic!("No unsupported Windows query"))
                .is_err()
        );
        let invalid = FirmwareReport {
            length: 0,
            ..FirmwareReport::default()
        };
        assert!(read_firmware_report(&invalid, false, |_, _| panic!("No invalid read")).is_err());
    }

    #[test]
    // Verify that firmware read coverage is explicit for every registered model
    fn firmware_read_coverage_is_explicit_for_every_registered_model() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        for model in [
            "293", "293-v3", "293s", "293s-v3", "h1-pro", "k1-pro", "m18", "m3", "mini", "n1",
            "n1-vsd", "n3", "n3-vsd", "n4", "n4-pro", "xl",
        ] {
            let id = format!("simulator:{model}:firmware-matrix");
            runtime.simulate_device(true, &id).unwrap();
            let snapshot = runtime.snapshot();
            let device = snapshot.devices.iter().find(|d| d.id == id).unwrap();
            assert!(device.manifest.capabilities.firmware_query, "{model}");
            let report = device
                .manifest
                .capabilities
                .firmware_report
                .clone()
                .unwrap_or_default();
            assert_eq!(
                report.report_id,
                if model == "k1-pro" {
                    4
                } else if model.ends_with("-vsd") {
                    1
                } else {
                    0
                }
            );
            assert_eq!(
                report.kind == hardware_core::FirmwareReportKind::Feature,
                model.ends_with("-vsd")
            );
            let result = runtime
                .check_firmware_with(&id, |_| panic!("Simulator cannot fetch metadata"))
                .unwrap();
            assert!(result.installed.is_none());
        }
        runtime.shutdown();
    }

    #[test]
    // Verify that k1 release metadata preserves exact region and rejects other brands
    fn k1_release_metadata_preserves_exact_region_and_rejects_other_brands() {
        let us = release_target("k1-pro", "usb-6603-1015").unwrap();
        let eu = release_target("k1-pro", "usb-6603-1019").unwrap();
        let bytes = br#"{"CurrentVersion":{"K1Pro":{"Version":"V3.K1_PRO.03.009"},"K1ProEU":{"Version":"V3.K1_PRO_EU.03.009"}}}"#;
        assert_eq!(
            release_from_bytes(bytes, &us).unwrap().as_deref(),
            Some("V3.K1_PRO.03.009")
        );
        assert_eq!(
            release_from_bytes(bytes, &eu).unwrap().as_deref(),
            Some("V3.K1_PRO_EU.03.009")
        );
        assert!(release_target("k1-pro", "usb-5548-1025").is_none());
        assert!(release_from_bytes(
            br#"{"CurrentVersion":{"K1Pro":{"Version":"V3.K1_PRO_EU.03.009"}}}"#,
            &us
        )
        .is_err());
    }

    #[test]
    // Verify that decimal versions and unrecognized reports fail closed
    fn decimal_versions_and_unrecognized_reports_fail_closed() {
        assert!(mirabox_version("02.006") < mirabox_version("02.010"));
        assert_eq!(mirabox_version("02.006"), mirabox_version("2.6"));
        for value in [
            "",
            "0x0206",
            "USB release 0206",
            "02.006.1",
            "unknown",
            "2.-1",
            "2.6\n",
        ] {
            assert!(mirabox_version(value).is_none());
        }
        assert_eq!(
            parse_input_report(b"\x00\x0002.006\x00padding"),
            Some("02.006".into())
        );
        assert!(parse_input_report(b"ACK\0\0OK\0\0\x01\x01").is_none());
        assert!(parse_input_report(b"\xff\0").is_none());
        for raw in ["V4.VSD N4 Pro.02.010", "V4.N4 Pro.00.005"] {
            assert_eq!(
                parse_input_report(format!("\0{raw}\0").as_bytes()),
                Some(raw.into())
            );
        }
        for raw in [
            "ACK.02.010",
            "V4.N4 Pro.02.010\n",
            "<svg>.02.010",
            "V4.N4 Pro.two.010",
        ] {
            assert!(parse_input_report(raw.as_bytes()).is_none());
        }
        assert!(parse_input_report(&[b'1'; 512]).is_none());
        assert!(parse_input_report(format!("\0V4.{}.02.010", "A".repeat(64)).as_bytes()).is_none());
    }
    #[test]
    // Verify that manufacturer metadata is exact bounded and gzip compatible
    fn manufacturer_metadata_is_exact_bounded_and_gzip_compatible() {
        let target = release_target("n4-pro", "vsdinside").unwrap();
        let bytes = br#"{"CurrentVersion":{"VSD_N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":"https://invalid.example/never-request-this.img"},"MBox-N4Pro":{"Version":"V4.N4 Pro.02.011"}}}"#;
        assert_eq!(
            release_from_bytes(bytes, &target).unwrap().as_deref(),
            Some("V4.VSD N4 Pro.02.010")
        );
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        gzip.write_all(bytes).unwrap();
        assert_eq!(
            release_from_bytes(&gzip.finish().unwrap(), &target).unwrap(),
            release_from_bytes(bytes, &target).unwrap()
        );
        assert!(release_from_bytes(
            br#"{"CurrentVersion":{"VSD_N4Pro":{"Version":"V4.N4 Pro.02.010"}}}"#,
            &target
        )
        .is_err());
        assert!(release_from_bytes(br#"{"CurrentVersion":{}}"#, &target)
            .unwrap()
            .is_none());
        assert!(release_from_bytes(b"not json", &target).is_err());
        assert!(read_bounded(&vec![0; METADATA_LIMIT as usize + 1][..]).is_err());
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&vec![b' '; METADATA_LIMIT as usize + 1])
            .unwrap();
        assert!(release_from_bytes(&gzip.finish().unwrap(), &target).is_err());
        assert!(release_target("n4-pro", "mirabox-e").is_none());
        assert!(release_target("other", "vsdinside").is_none());
    }
    #[test]
    // Verify that checks recover preserve failed observations and reject old connections
    fn checks_recover_preserve_failed_observations_and_reject_old_connections() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:firmware";
        runtime.simulate_device(true, id).unwrap();
        runtime
            .check_firmware_with(id, |_| panic!("Simulators must never fetch releases"))
            .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false; // Controlled transport below; no HID interface is opened
            device.variant_id = "vsdinside".into();
        }
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(id.into(), 11);
        let lookup_only = runtime
            .check_firmware_with(id, |_| Ok(Some("V4.VSD N4 Pro.02.010".into())))
            .unwrap();
        assert_eq!(lookup_only.state, "failed");
        assert!(lookup_only.latest.is_some());
        assert!(lookup_only.latest_message.contains("cannot be compared"));
        let answer = |result: Result<Option<String>, String>| {
            let (query, requests) = mpsc::sync_channel::<QueryReply>(1);
            runtime.firmware.queries.lock().unwrap().insert(
                id.into(),
                (11, QuerySender::new(query, std::sync::Arc::new(|| {}))),
            );
            std::thread::spawn(move || {
                requests
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .send(result)
                    .unwrap()
            })
        };
        let worker = answer(Ok(Some("V4.VSD N4 Pro.02.006".into())));
        let status = runtime
            .check_firmware_with(id, |_| Ok(Some("V4.VSD N4 Pro.02.010".into())))
            .unwrap();
        worker.join().unwrap();
        assert_eq!(status.state, "installed_known");
        assert!(status.latest_message.starts_with("A newer firmware"));
        assert!(status.latest_checked.is_some());
        let worker = answer(Err("HID unavailable".into()));
        let failed = runtime
            .check_firmware_with(id, |_| Err("Offline".into()))
            .unwrap();
        worker.join().unwrap();
        assert_eq!(failed.installed, status.installed);
        assert_eq!(failed.latest, status.latest);
        assert!(failed.stale && failed.latest_stale);
        let worker = answer(Ok(Some("V4.VSD N4 Pro.02.010".into())));
        let recovered = runtime
            .check_firmware_with(id, |_| Ok(Some("V4.VSD N4 Pro.02.010".into())))
            .unwrap();
        worker.join().unwrap();
        assert!(!recovered.stale && !recovered.latest_stale);
        assert_eq!(recovered.state, "installed_known");
        assert!(recovered
            .latest_message
            .starts_with("Installed firmware matches"));
        runtime
            .firmware
            .release_checks
            .lock()
            .unwrap()
            .insert(id.into(), (11, 99));
        let target = release_target("n4-pro", "vsdinside").unwrap();
        runtime.observe_release(id, 11, 98, &target, Ok(Some("V4.VSD N4 Pro.99.999".into())));
        assert_eq!(
            runtime.firmware_status(id).unwrap().latest,
            recovered.latest
        );
        runtime.observe_firmware(id, 11, Ok(None));
        assert!(runtime.firmware_status(id).unwrap().stale);
        assert_eq!(
            runtime.firmware_status(id).unwrap().latest,
            recovered.latest
        );
        assert!(runtime
            .firmware_status(id)
            .unwrap()
            .latest_message
            .contains("cannot be compared"));
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(id.into(), 12);
        runtime.observe_release(id, 11, 99, &target, Ok(Some("V4.VSD N4 Pro.99.999".into())));
        assert!(runtime.firmware_status(id).unwrap().latest.is_none());
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Read-only manufacturer network qualification; no HID or firmware downloads"]
    // Verify that live manufacturer catalog contains exact supported releases
    fn live_manufacturer_catalog_contains_exact_supported_releases() {
        for (model, variant) in [
            ("n4-pro", "vsdinside"),
            ("n4-pro", "mirabox"),
            ("k1-pro", "usb-6603-1015"),
            ("k1-pro", "usb-6603-1019"),
        ] {
            let target = release_target(model, variant).unwrap();
            let latest = fetch_release(&target).unwrap().unwrap();
            assert_eq!(version_parts(&latest).unwrap().0, target.prefix);
            eprintln!("Manufacturer metadata: {variant}: {latest}");
        }
    }
}

#[cfg(test)]
mod query_wake_tests {
    use super::*;
    // Firmware wakes happen after publication and never consume or duplicate a reply
    #[test]
    fn enqueue_publishes_before_waking_and_busy_requests_do_not_wake() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = wakes.clone();
        let query = QuerySender::new(
            sender,
            std::sync::Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let (reply, result) = mpsc::channel();
        query.try_send(reply).unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        let (extra, _) = mpsc::channel();
        assert!(matches!(
            query.try_send(extra),
            Err(mpsc::TrySendError::Full(_))
        ));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        receiver
            .recv()
            .unwrap()
            .send(Ok(Some("fixture".into())))
            .unwrap();
        assert_eq!(result.recv().unwrap(), Ok(Some("fixture".into())));
        drop(receiver);
        let (extra, _) = mpsc::channel();
        assert!(query.try_send(extra).is_err());
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }
}
