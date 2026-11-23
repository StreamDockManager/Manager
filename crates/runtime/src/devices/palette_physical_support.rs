//! Test-only exclusive transport and neutral cleanup support for palette qualification.
use super::*;

#[cfg(target_os = "macos")]
pub(super) struct PhysicalTransport {
    pub(super) device: Arc<Mutex<hidapi::HidDevice>>,
    pub(super) writes: Arc<std::sync::atomic::AtomicUsize>,
}
#[cfg(target_os = "macos")]
impl Transport for PhysicalTransport {
    fn write(&mut self, report: &[u8]) -> Result<()> {
        let written = self
            .device
            .lock()
            .unwrap()
            .write(report)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        if written != report.len() {
            return Err(Error::Protocol("Short physical HID write".into()));
        }
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}
#[cfg(target_os = "macos")]
pub(super) struct BlankOnDrop {
    pub(super) model: String,
    pub(super) device: Arc<Mutex<hidapi::HidDevice>>,
    pub(super) session: Box<dyn HardwareSession>,
    pub(super) commands: Vec<DeviceCommand>,
}
#[cfg(target_os = "macos")]
impl Drop for BlankOnDrop {
    fn drop(&mut self) {
        let result = (|| -> Result<usize> {
            let mut count = 0;
            for command in std::mem::take(&mut self.commands) {
                let transaction = self.session.encode(command)?;
                for report in transaction.reports {
                    let written = self
                        .device
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .write(&report)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    if written != report.len() {
                        return Err(Error::Protocol("Short cleanup HID write".into()));
                    }
                    count += 1;
                }
            }
            Ok(count)
        })();
        eprintln!(
            "PALETTE_PHYSICAL_CLEANUP {}",
            serde_json::json!({"model":self.model,"blank_reports":result.as_ref().ok(),"success":result.is_ok(),"prior_pixels_restored":false,"visual_confirmation":false})
        );
    }
}
pub(super) fn blank_scene(manifest: &ModelManifest) -> Vec<DeviceCommand> {
    let image = |width: u16, height: u16| {
        hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                width.into(),
                height.into(),
                image::Rgb([0, 0, 0]),
            )),
            "png",
        )
        .unwrap()
    };
    let mut commands: Vec<_> = manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some())
        .map(|c| DeviceCommand::Image {
            control_id: c.id.clone(),
            data: image(
                c.image_width.unwrap_or(c.width),
                c.image_height.unwrap_or(c.height),
            ),
        })
        .collect();
    if let Some([width, height]) = manifest.capabilities.background_size {
        commands.push(DeviceCommand::Background {
            data: image(width, height),
            x: 0,
            y: 0,
            width,
            height,
        });
    }
    commands
}

#[cfg(target_os = "macos")]
pub(super) struct PhysicalModel {
    pub(super) model: &'static str,
    pub(super) contract: ResolvedDeviceContract,
    pub(super) device: Arc<Mutex<hidapi::HidDevice>>,
}
#[cfg(target_os = "macos")]
pub(super) fn open_physical_models() -> Vec<PhysicalModel> {
    assert!(
        std::env::var("STREAM_DOCK_MANAGER_PHYSICAL_PALETTE_PROFILE").as_deref()
            == Ok("authorized-temporary-display")
    );
    open_models(&["n4-pro", "k1-pro"], false)
}
#[cfg(target_os = "macos")]
pub(super) fn open_models(models: &[&'static str], query: bool) -> Vec<PhysicalModel> {
    let process_list = std::process::Command::new("/bin/ps")
        .args(["-axo", "comm="])
        .output()
        .unwrap();
    assert!(
        process_list.status.success(),
        "Cannot verify no competing Manager"
    );
    assert!(
        !String::from_utf8_lossy(&process_list.stdout)
            .lines()
            .any(|line| line.ends_with("/stream-dock-manager")
                || line.ends_with("/Stream Dock Manager")),
        "A Manager instance is running; do not interrupt it"
    );
    let api = hidapi::HidApi::new().expect("Read-only HID enumeration must succeed");
    api.set_open_exclusive(true);
    assert!(api.get_open_exclusive());
    let modules = hardware_registry::modules();
    let mut owned = Vec::new();
    // Establish requested exclusive handles before any display write. No shared-open fallback.
    for &model in models {
        let module = modules
            .iter()
            .find(|m| m.manifest().model_id == model)
            .unwrap();
        let matches: Vec<_> = api
            .device_list()
            .filter_map(|info| {
                let identity = super::identity(info);
                module
                    .manifest()
                    .variants
                    .iter()
                    .find(|v| v.available && v.identity.matches(&identity))
                    .map(|v| (info, v.id.clone()))
            })
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "Expected exactly one eligible interface for {model}"
        );
        let (info, variant) = &matches[0];
        let device = api
            .open_path(info.path())
            .expect("Exclusive HID open failed; do not change permissions or stop another owner");
        assert!(device.is_open_exclusive().unwrap());
        let device = Arc::new(Mutex::new(device));
        let firmware = if query && module.manifest().capabilities.firmware_query {
            super::query_firmware(
                &device,
                &module
                    .manifest()
                    .capabilities
                    .firmware_report
                    .clone()
                    .unwrap_or_default(),
            )
            .ok()
            .flatten()
        } else {
            None
        };
        let contract = module
            .resolve_contract(variant, firmware.as_deref())
            .unwrap();
        assert!(
            contract.image_output_disabled_reason.is_none(),
            "Resolved contract forbids image output for {model}"
        );
        eprintln!(
            "PALETTE_PHYSICAL_OPEN {}",
            serde_json::json!({"model":model,"variant":variant,"exclusive":true,"firmware":firmware,"usb_speed":"not queried"})
        );
        owned.push(PhysicalModel {
            model,
            contract,
            device,
        });
    }
    owned
}
