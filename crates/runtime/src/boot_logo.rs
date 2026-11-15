//! Explicit, reviewed startup-image uploads. These drafts never enter portable profiles
use crate::{DeviceInfo, Runtime};
use base64::Engine;
use hardware_core::OutputStatus;
use image::ImageDecoder;
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use std::{sync::mpsc, time::Instant};

#[derive(Clone, serde::Serialize, ts_rs::TS)]
pub struct BootLogoPreview {
    pub id: String,
    pub device_id: String,
    pub file_name: String,
    pub size: [u16; 2],
    pub fit: String,
    pub image: String,
    pub simulated: bool,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct BootLogoResult {
    pub device_id: String,
    pub sent: bool,
    pub message: String,
}
struct Stage {
    target: DeviceInfo,
    epoch: Option<u64>,
    contract_key: String,
    transaction: Arc<hardware_core::DeviceTransaction>,
    created: Instant,
    applying: bool,
}
#[derive(Default)]
pub(crate) struct BootLogos {
    preparing: Mutex<()>,
    stages: Mutex<HashMap<String, Stage>>,
}
impl BootLogos {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        use crate::editor_retirement::unavailable_or;
        unavailable_or(&self.preparing, |_| false)
            || unavailable_or(&self.stages, |stages| !stages.is_empty())
    }
}
const LIFETIME: Duration = Duration::from_secs(15 * 60);

// Resolve the boot-logo device and its current connection generation
fn target(runtime: &Runtime, id: &str) -> Result<(DeviceInfo, Option<u64>), String> {
    let device = runtime
        .state
        .lock()
        .unwrap()
        .devices
        .iter()
        .find(|device| device.id == id)
        .cloned()
        .ok_or("Device is no longer available")?;
    if !device.connected {
        return Err("Connect the device before preparing its startup logo".into());
    }
    if device.manifest.capabilities.boot_logo.is_none() {
        return Err("This device does not support startup logo uploads".into());
    }
    let epoch = Some(
        *runtime
            .connection_epochs
            .lock()
            .unwrap()
            .get(id)
            .ok_or("Physical connection changed or is unavailable")?,
    );
    Ok((device, epoch))
}

// Decode a bounded PNG or JPEG and fit it to the boot-logo dimensions
fn raster(path: &Path, size: [u16; 2], fit: &str) -> Result<Vec<u8>, String> {
    if !["contain", "cover"].contains(&fit) {
        return Err("Choose Fit entire image or Fill and crop".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    const LIMIT: usize = 32 * 1024 * 1024;
    if !metadata.is_file() || metadata.len() > LIMIT as u64 {
        return Err("Choose a PNG or JPEG image smaller than 32 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err("Startup image exceeds 32 MiB".into());
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let decoder =
            image::codecs::png::PngDecoder::new(Cursor::new(&bytes)).map_err(|e| e.to_string())?;
        if decoder.is_apng().map_err(|e| e.to_string())? {
            return Err("Startup logos require a static PNG or JPEG image".into());
        }
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("Startup logos require a static PNG or JPEG image".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut source = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    source.apply_orientation(orientation);
    let [width, height] = size.map(u32::from);
    let resized = if fit == "cover" {
        source.resize_to_fill(width, height, image::imageops::FilterType::Lanczos3)
    } else {
        source.resize(width, height, image::imageops::FilterType::Lanczos3)
    };
    let mut output = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 255]));
    image::imageops::overlay(
        &mut output,
        &resized,
        i64::from((width - resized.width()) / 2),
        i64::from((height - resized.height()) / 2),
    );
    // The image crate's integer alpha blend can round an opaque destination to
    // alpha 254. The startup surface has no transparency channel
    for pixel in output.pixels_mut() {
        pixel.0[3] = 255;
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(output)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

impl Runtime {
    // Prepare boot logo
    pub fn prepare_boot_logo(
        &self,
        device_id: &str,
        path: &Path,
        fit: &str,
    ) -> Result<BootLogoPreview, String> {
        let _preparation = self
            .boot_logos
            .preparing
            .try_lock()
            .map_err(|_| "Another startup logo is being prepared")?;
        {
            let mut stages = self.boot_logos.stages.lock().unwrap();
            stages.retain(|_, stage| stage.applying || stage.created.elapsed() < LIFETIME);
            if stages.len() >= 4 {
                return Err(
                    "Close an existing startup-logo preview before preparing another".into(),
                );
            }
        }
        let (device, epoch) = target(self, device_id)?;
        let size = device
            .manifest
            .capabilities
            .boot_logo
            .as_ref()
            .unwrap()
            .size;
        let png = raster(path, size, fit)?;
        let module = hardware_registry::modules()
            .into_iter()
            .find(|module| module.manifest().model_id == device.model_id)
            .ok_or("Hardware module is unavailable")?;
        let contract = crate::device_contract::resolve(&device)?;
        let contract_key = self.device_contract_key(&device)?;
        let transaction = module
            .session_for(&contract)
            .map_err(|e| e.to_string())?
            .encode(hardware_core::DeviceCommand::BootLogo { data: png.clone() })
            .map_err(|e| e.to_string())?;
        if png.len() > 8 * 1024 * 1024
            || transaction.reports.iter().map(Vec::len).sum::<usize>() > 4 * 1024 * 1024
        {
            return Err("Prepared startup logo exceeds the device upload limit".into());
        }
        let (current, current_epoch) = target(self, device_id)?;
        if current.persistence_key != device.persistence_key
            || current.model_id != device.model_id
            || current_epoch != epoch
            || self.device_contract_key(&current)? != contract_key
        {
            return Err("Device changed while preparing its logo. Reopen the preview.".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let preview = BootLogoPreview {
            id: id.clone(),
            device_id: device_id.into(),
            size,
            fit: fit.into(),
            simulated: device.simulated,
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            image: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            ),
        };
        self.boot_logos.stages.lock().unwrap().insert(
            id,
            Stage {
                target: device,
                epoch,
                contract_key,
                transaction: Arc::new(transaction),
                created: Instant::now(),
                applying: false,
            },
        );
        Ok(preview)
    }
    // Cancel boot logo
    pub fn cancel_boot_logo(&self, id: &str) -> Result<(), String> {
        let mut stages = self.boot_logos.stages.lock().unwrap();
        if stages.get(id).is_some_and(|stage| stage.applying) {
            return Err("Wait for the startup logo upload to finish".into());
        }
        stages.remove(id);
        Ok(())
    }
    // Commit boot logo
    pub fn commit_boot_logo(&self, id: &str) -> Result<BootLogoResult, String> {
        let (device, epoch, contract_key, transaction) = {
            let mut stages = self.boot_logos.stages.lock().unwrap();
            let stage = stages
                .get_mut(id)
                .ok_or("Startup-logo preview expired. Choose the image again.")?;
            if stage.applying {
                return Err("Startup logo is already being uploaded".into());
            }
            if stage.created.elapsed() >= LIFETIME {
                return Err("Startup-logo preview expired. Choose the image again.".into());
            }
            stage.applying = true;
            (
                stage.target.clone(),
                stage.epoch,
                stage.contract_key.clone(),
                stage.transaction.clone(),
            )
        };
        let result = (|| {
            let (current, current_epoch) = target(self, &device.id)?;
            if current.persistence_key != device.persistence_key
                || current.model_id != device.model_id
                || current_epoch != epoch
                || self.device_contract_key(&current)? != contract_key
            {
                return Err("Device changed. Reopen the startup-logo preview.".into());
            }
            if !device.simulated {
                let epoch = epoch.ok_or("Device connection changed")?;
                let (reply, receive) = mpsc::channel();
                self.device_commands
                    .send(crate::devices::Request::BootLogo {
                        device: device.id.clone(),
                        epoch,
                        contract_key,
                        transaction: (*transaction).clone(),
                        reply,
                    })
                    .map_err(|_| "Device service is unavailable")?;
                receive
                    .recv_timeout(Duration::from_secs(35))
                    .map_err(|_| "Startup logo transfer status unknown. Do not retry until the device state has been checked.")??;
            }
            Ok(BootLogoResult {
                device_id: device.id,
                sent: !device.simulated,
                message: if !device.simulated {
                    "Startup logo host transfer completed. Firmware persistence has not been verified.".into()
                } else {
                    "Simulator preview updated.".into()
                },
            })
        })();
        // Commit is single-use even on a timeout, disconnect or write error:
        // the device may already have received persistent bytes. No implicit retry
        self.boot_logos.stages.lock().unwrap().remove(id);
        result
    }
}

pub(crate) struct PendingUpload {
    pub receipt: mpsc::Receiver<OutputStatus>,
    pub reply: Option<mpsc::Sender<Result<(), String>>>,
    pub deadline: Instant,
}
impl PendingUpload {
    // Poll boot logo for pending work
    pub fn poll(&mut self, failure: Option<String>) -> bool {
        crate::transfer_receipt::poll(
            &self.receipt,
            &mut self.reply,
            self.deadline,
            |status| {
                match status {
                OutputStatus::Completed => Ok(()),
                status => Err(failure.map(|error| format!("Startup logo transfer status unknown: {error}. Check the device before retrying.")).unwrap_or_else(|| {
                    format!("Startup logo transfer status unknown ({status:?}); check the device before preparing another upload")
                })),
            }
            },
            || {
                "Startup logo transfer status unknown: device connection changed during upload. Do not retry automatically.".into()
            },
            || {
                "Startup logo transfer status unknown. Wait for the current transfer and check the device before preparing another upload.".into()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that editor retirement waits for logo preparation and explicit preview release
    fn editor_retirement_waits_for_logo_preparation_and_explicit_preview_release() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("manager"), false).unwrap();
        runtime.simulate(true).unwrap();
        {
            let _preparing = runtime.boot_logos.preparing.lock().unwrap();
            assert!(runtime.boot_logos.editor_retirement_blocked());
        }
        let preview = runtime
            .prepare_boot_logo("simulator:n4-pro", &source(root.path()), "contain")
            .unwrap();
        assert!(runtime.boot_logos.editor_retirement_blocked());
        runtime.cancel_boot_logo(&preview.id).unwrap();
        assert!(!runtime.boot_logos.editor_retirement_blocked());
        runtime.shutdown();
    }

    // Return the underlying boot logo source
    fn source(root: &Path) -> std::path::PathBuf {
        let path = root.join("logo.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 128]))
            .save(&path)
            .unwrap();
        path
    }
    #[test]
    // Verify that startup raster fits crops and flattens transparency
    fn startup_raster_fits_crops_and_flattens_transparency() {
        let dir = tempfile::tempdir().unwrap();
        let path = source(dir.path());
        let fit = image::load_from_memory(&raster(&path, [80, 48], "contain").unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(fit.dimensions(), (80, 48));
        assert_eq!(fit.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(fit.get_pixel(40, 24).0, [128, 0, 0, 255]);
        let crop = image::load_from_memory(&raster(&path, [80, 48], "cover").unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(crop.get_pixel(0, 0).0, [128, 0, 0, 255]);
        assert!(raster(&path, [80, 48], "invalid").is_err());
        std::fs::write(&path, b"not an image").unwrap();
        assert!(raster(&path, [80, 48], "contain").is_err());
    }
    #[test]
    // Verify that startup draft cancel simulator and changed device are isolated
    fn startup_draft_cancel_simulator_and_changed_device_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = crate::test_support::headless(dir.path().join("manager")).unwrap();
        runtime.simulate(true).unwrap();
        let before = serde_json::to_value(runtime.snapshot()).unwrap();
        let path = source(dir.path());
        let preview = runtime
            .prepare_boot_logo("simulator:n4-pro", &path, "contain")
            .unwrap();
        assert_eq!(preview.size, [800, 480]);
        assert!(preview.simulated);
        assert_eq!(serde_json::to_value(runtime.snapshot()).unwrap(), before);
        runtime.cancel_boot_logo(&preview.id).unwrap();
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        let preview = runtime
            .prepare_boot_logo("simulator:n4-pro", &path, "cover")
            .unwrap();
        // Apply uses immutable staged bytes, not a reread of the source file
        std::fs::remove_file(&path).unwrap();
        assert!(!runtime.commit_boot_logo(&preview.id).unwrap().sent);
        assert_eq!(serde_json::to_value(runtime.snapshot()).unwrap(), before);
        let path = source(dir.path());
        let preview = runtime
            .prepare_boot_logo("simulator:n4-pro", &path, "contain")
            .unwrap();
        runtime.simulate(false).unwrap();
        runtime.simulate(true).unwrap();
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        runtime.simulate(false).unwrap();
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        runtime.cancel_boot_logo(&preview.id).unwrap();
        runtime.simulate_device(true, "simulator:h1-pro").unwrap();
        assert!(runtime
            .prepare_boot_logo("simulator:h1-pro", &path, "contain")
            .is_err());
    }
    #[test]
    // Verify that startup draft is bound to variant firmware observation and is single use
    fn startup_draft_is_bound_to_variant_firmware_observation_and_is_single_use() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = crate::test_support::headless(dir.path().join("manager")).unwrap();
        let id = "simulator:n3";
        runtime.simulate_device(true, id).unwrap();
        let epoch = runtime.connection_epochs.lock().unwrap()[id];
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.variant_id = "usb-6602-1000".into();
        }
        let preview = runtime
            .prepare_boot_logo(id, &source(dir.path()), "contain")
            .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.variant_id = "usb-6603-1002".into();
        }
        assert!(runtime
            .commit_boot_logo(&preview.id)
            .err()
            .unwrap()
            .contains("changed"));
        assert!(!runtime
            .boot_logos
            .stages
            .lock()
            .unwrap()
            .contains_key(&preview.id));
        let preview = runtime
            .prepare_boot_logo(id, &source(dir.path()), "contain")
            .unwrap();
        runtime.observe_firmware(id, epoch, Ok(Some("V25.N3.01.000".into())));
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        let preview = runtime
            .prepare_boot_logo(id, &source(dir.path()), "contain")
            .unwrap();
        // Same firmware, new observation: the user must review a fresh draft
        runtime.observe_firmware(id, epoch, Ok(Some("V25.N3.01.000".into())));
        assert!(runtime.commit_boot_logo(&preview.id).is_err());
        runtime.shutdown();
    }
    #[test]
    // Verify that persistent receipt failures are uncertain and never retried
    fn persistent_receipt_failures_are_uncertain_and_never_retried() {
        for status in [
            OutputStatus::Failed,
            OutputStatus::Invalidated,
            OutputStatus::Replaced,
        ] {
            let (send, receipt) = mpsc::channel();
            let (reply, receive) = mpsc::channel();
            let mut pending = PendingUpload {
                receipt,
                reply: Some(reply),
                deadline: Instant::now() + Duration::from_secs(1),
            };
            send.send(status).unwrap();
            assert!(pending.poll(None));
            assert!(receive
                .recv()
                .unwrap()
                .unwrap_err()
                .contains("status unknown"));
            assert!(pending.reply.is_none());
        }
    }
    #[test]
    // Verify that transfer timeout retains ownership until receipt arrives
    fn transfer_timeout_retains_ownership_until_receipt_arrives() {
        let (send, receipt) = mpsc::channel();
        let (reply, receive) = mpsc::channel();
        let mut pending = PendingUpload {
            receipt,
            reply: Some(reply),
            deadline: Instant::now(),
        };
        assert!(!pending.poll(None));
        assert!(receive.recv().unwrap().is_err());
        assert!(!pending.poll(None));
        send.send(OutputStatus::Completed).unwrap();
        assert!(pending.poll(None));
    }
}
