//! Independently implemented CRT family from revision-pinned vendor protocol facts
//! See hardware/protocol-evidence.md. This does not alter the existing N4 Pro codec
use hardware_core::*;
use image::{DynamicImage, ImageFormat};
use serde::Deserialize;
use std::{collections::HashSet, io::Cursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireEncoding {
    /// Preserve the host-selected source format: PNG stays lossless, JPEG stays JPEG
    SourceFormat,
    Jpeg,
    Png,
    BgrRows,
    BgrColumns,
    Rgb565Le,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Surface {
    pub control_id: String,
    pub encoding: WireEncoding,
    /// Clockwise degrees. Vendor Pillow positive angles are counterclockwise
    pub rotation: u16,
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    /// Soft targets never authorize destructive PNG quantization
    /// SourceFormat overrides apply to JPEG only; PNG keeps its 29k/20k targets
    #[serde(default)]
    pub static_target: Option<usize>,
    #[serde(default)]
    pub animated_target: Option<usize>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mapping {
    Button {
        code: u8,
        control_id: String,
    },
    Pulse {
        code: u8,
        control_id: String,
    },
    Rotate {
        code: u8,
        control_id: String,
        delta: i32,
    },
    Swipe {
        code: u8,
        control_id: String,
        direction: String,
    },
}
impl Mapping {
    // Resolve the address used by this operation
    fn address(&self) -> (u8, &str, &str) {
        match self {
            Self::Button { code, control_id } => (*code, control_id, "press"),
            Self::Pulse { code, control_id } => (*code, control_id, "pulse"),
            Self::Rotate {
                code, control_id, ..
            } => (*code, control_id, "rotate"),
            Self::Swipe {
                code, control_id, ..
            } => (*code, control_id, "swipe"),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Codec {
    pub schema_version: u32,
    pub report_size: usize,
    // External hardware package v1 defaults remain supported; owned models declare these explicitly
    #[serde(default)]
    pub report_id: u8,
    pub mode: Option<u8>,
    /// Select software mode before DIS on models that require it before other commands
    #[serde(default)]
    pub mode_before_wake: bool,
    /// Repeat mode selection and initial brightness on ordinary wake commands
    /// Models with connection-only setup opt out; initialize always performs it
    #[serde(default = "reinitialize_on_wake")]
    pub reinitialize_on_wake: bool,
    /// The manufacturer SDK requires OK; independent firmware readers only require ACK
    #[serde(default = "require_input_ok")]
    pub require_input_ok: bool,
    /// Optional LIG immediately after DIS during initialization and, unless
    /// reinitialize_on_wake is false, ordinary wake commands.
    /// This is model-owned initialization, before the runtime restores brightness
    pub wake_brightness: Option<u8>,
    pub inputs: Vec<Mapping>,
    pub surfaces: Vec<Surface>,
    pub background: Option<Surface>,
    pub boot: Option<Surface>,
    /// Explicit per-event accepted wire states; VSD variants retain their policy
    pub button_states: Vec<u8>,
    pub rotate_states: Vec<u8>,
    pub swipe_states: Vec<u8>,
}
// Require input ok
fn require_input_ok() -> bool {
    true
}
// Configure whether waking requires protocol reinitialization
fn reinitialize_on_wake() -> bool {
    true
}
impl Codec {
    // Parse for the current operation
    pub fn parse(source: &str, manifest: &ModelManifest) -> Result<Self> {
        let codec: Self = toml::from_str(source).map_err(|e| Error::Descriptor(e.to_string()))?;
        if codec.schema_version != 1
            || ![513, 1025].contains(&codec.report_size)
            || ![0, 4].contains(&codec.report_id)
            || codec.mode.is_some_and(|m| m > 2)
            || (codec.mode_before_wake && codec.mode.is_none())
            || codec.wake_brightness.is_some_and(|value| value > 100)
            || codec.inputs.len() > 256
            || [
                &codec.button_states,
                &codec.rotate_states,
                &codec.swipe_states,
            ]
            .iter()
            .any(|states| states.is_empty() || states.iter().any(|s| *s > 2))
        {
            return Err(Error::Descriptor("unsupported CRT configuration".into()));
        }
        let mut codes = HashSet::new();
        for mapping in &codec.inputs {
            let (code, id, semantic) = mapping.address();
            let control = manifest.control(id)?;
            if !codes.insert(code)
                || !control.semantics.iter().any(|s| s == semantic)
                || matches!(mapping, Mapping::Rotate { delta, .. } if ![-1,1].contains(delta))
                || matches!(mapping, Mapping::Swipe { direction, .. } if !["left","right"].contains(&direction.as_str()))
            {
                return Err(Error::Descriptor("invalid CRT input mapping".into()));
            }
        }
        let mut surfaces = HashSet::new();
        for surface in &codec.surfaces {
            let control = manifest.control(&surface.control_id)?;
            if control.image_slot.is_none()
                || !surfaces.insert(&surface.control_id)
                || ![0, 90, 180, 270].contains(&surface.rotation)
            {
                return Err(Error::Descriptor("invalid CRT image surface".into()));
            }
        }
        if manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .count()
            != surfaces.len()
            || codec.background.is_some() != manifest.capabilities.background
            || codec.boot.is_some() != manifest.capabilities.boot_logo.is_some()
            || codec
                .background
                .iter()
                .chain(codec.boot.iter())
                .any(|s| ![0, 90, 180, 270].contains(&s.rotation))
        {
            return Err(Error::Descriptor("CRT surface/capability mismatch".into()));
        }
        Ok(codec)
    }
    /// HIDAPI report length includes the ID even for unnumbered (zero) reports
    /// The pinned vendor chunkSize subtracts an additional byte for numbered HID,
    /// and setKeyBitmap subtracts one more; retain its trailing pad for ID 4.
    pub fn payload_size(&self) -> usize {
        self.report_size - if self.report_id == 0 { 1 } else { 2 }
    }
    // Construct the command representation
    pub fn command(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.len() + 5 > self.payload_size() {
            return Err(Error::Protocol("CRT command too long".into()));
        }
        let mut report = self.packet(&[])?;
        report[1..6].copy_from_slice(b"CRT\0\0");
        report[6..6 + bytes.len()].copy_from_slice(bytes);
        Ok(report)
    }
    // Construct the device protocol packet
    fn packet(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.len() > self.payload_size() {
            return Err(Error::Protocol("CRT report too long".into()));
        }
        let mut report = vec![0; self.report_size];
        report[0] = self.report_id;
        report[1..1 + bytes.len()].copy_from_slice(bytes);
        Ok(report)
    }
    // Transfer the prepared protocol bytes to the device
    fn transfer(
        &self,
        header: &[u8],
        data: &[u8],
        commit: bool,
        key: &str,
    ) -> Result<DeviceTransaction> {
        if data.is_empty() || data.len() > 16 * 1024 * 1024 {
            return Err(Error::Protocol("invalid CRT image size".into()));
        }
        let mut reports = vec![self.command(header)?];
        for bytes in data.chunks(self.payload_size()) {
            reports.push(self.packet(bytes)?);
        }
        if commit {
            reports.push(self.command(b"STP")?);
        }
        Ok(DeviceTransaction {
            reports,
            coalesce_key: Some(key.into()),
        })
    }
}
/// Decode bounded PNG/JPEG source assets into model-owned wire geometry/formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingPurpose {
    Static,
    Animated,
    Boot,
    Background,
}

// Encode surface
pub fn encode_surface(data: &[u8], size: [u16; 2], surface: &Surface) -> Result<Vec<u8>> {
    encode_surface_for(data, size, surface, EncodingPurpose::Static)
}

// Encode surface for
pub fn encode_surface_for(
    data: &[u8],
    size: [u16; 2],
    surface: &Surface,
    purpose: EncodingPurpose,
) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > 16 * 1024 * 1024 || size.iter().any(|n| *n == 0 || *n > 4096)
    {
        return Err(Error::Protocol("invalid CRT image source".into()));
    }
    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return Err(Error::Protocol("CRT source must be PNG/JPEG".into()));
    }
    let encoding = match (surface.encoding, reader.format()) {
        (WireEncoding::SourceFormat, Some(ImageFormat::Png)) => WireEncoding::Png,
        (WireEncoding::SourceFormat, Some(ImageFormat::Jpeg)) => WireEncoding::Jpeg,
        (encoding, _) => encoding,
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    if purpose == EncodingPurpose::Boot && [image.width(), image.height()] != size.map(u32::from) {
        return Err(Error::Protocol(
            "startup logo must exactly match its input canvas".into(),
        ));
    }
    let image = image.resize_exact(
        size[0].into(),
        size[1].into(),
        image::imageops::FilterType::Lanczos3,
    );
    let mut image = match surface.rotation {
        0 => image,
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("invalid CRT rotation".into())),
    };
    if surface.flip_x {
        image = image.fliph();
    }
    if surface.flip_y {
        image = image.flipv();
    }
    if matches!(encoding, WireEncoding::Png) {
        // These are soft targets: keep RGBA pixels and alpha intact even if
        // lossless compression cannot meet the transport-size preference.
        let target = if purpose == EncodingPurpose::Animated {
            if surface.encoding == WireEncoding::SourceFormat {
                20_000
            } else {
                surface.animated_target.unwrap_or(20_000)
            }
        } else {
            if surface.encoding == WireEncoding::SourceFormat {
                29_000
            } else {
                surface.static_target.unwrap_or(29_000)
            }
        };
        let pixels = DynamicImage::ImageRgba8(image.to_rgba8());
        let initial = png_output::encode(&pixels)?;
        if initial.len() <= target {
            return Ok(initial);
        }
        let mut best = Vec::new();
        pixels
            .write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
                &mut best,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            ))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        return Ok(if best.len() < initial.len() {
            best
        } else {
            initial
        });
    }
    let rgba = image.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y);
        image::Rgb([0, 1, 2].map(|i| ((u16::from(p[i]) * u16::from(p[3]) + 127) / 255) as u8))
    });
    if matches!(encoding, WireEncoding::Jpeg) {
        let target = match purpose {
            EncodingPurpose::Static => surface.static_target.unwrap_or(9_000),
            EncodingPurpose::Animated => surface.animated_target.unwrap_or(3_000),
            EncodingPurpose::Boot => 522_240,
            EncodingPurpose::Background => usize::MAX,
        };
        let pixels = DynamicImage::ImageRgb8(rgb);
        let mut quality = if purpose == EncodingPurpose::Background {
            60
        } else {
            100
        };
        loop {
            let mut out = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                .encode_image(&pixels)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            if out.len() <= target || quality <= 2 || purpose == EncodingPurpose::Boot {
                if purpose == EncodingPurpose::Boot && out.len() > 522_240 {
                    return Err(Error::Protocol("startup JPEG exceeds 522240 bytes".into()));
                }
                return Ok(out);
            }
            quality -= 2;
        }
    }
    if encoding == WireEncoding::Rgb565Le {
        return Ok(rgb
            .pixels()
            .flat_map(|p| {
                let value = (u16::from(p[0] >> 3) << 11)
                    | (u16::from(p[1] >> 2) << 5)
                    | u16::from(p[2] >> 3);
                value.to_le_bytes()
            })
            .collect());
    }
    let mut out = Vec::with_capacity(rgb.len());
    let mut append = |x, y| {
        let p = rgb.get_pixel(x, y);
        out.extend([p[2], p[1], p[0]]);
    };
    match encoding {
        WireEncoding::BgrColumns => {
            for x in 0..rgb.width() {
                for y in 0..rgb.height() {
                    append(x, y);
                }
            }
        }
        WireEncoding::BgrRows => {
            for y in 0..rgb.height() {
                for x in 0..rgb.width() {
                    append(x, y);
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(out)
}
pub struct Session {
    manifest: ModelManifest,
    codec: Codec,
    image_output_disabled_reason: Option<String>,
}
impl Session {
    // Construct the value and initialize its state
    pub fn new(manifest: &'static ModelManifest, codec: &'static Codec) -> Self {
        Self {
            manifest: manifest.clone(),
            codec: codec.clone(),
            image_output_disabled_reason: None,
        }
    }
    // Construct a session using the resolved device contract
    pub fn new_resolved(contract: &ResolvedDeviceContract, codec: &Codec) -> Self {
        let mut codec = codec.clone();
        if let Some(transform) = &contract.key_transform {
            for surface in &mut codec.surfaces {
                surface.rotation = transform.rotation;
                surface.flip_x = transform.flip_x;
                surface.flip_y = transform.flip_y;
            }
        }
        if let Some(surface) = &mut codec.boot {
            if let Some(transform) = &contract.boot_transform {
                surface.rotation = transform.rotation;
                surface.flip_x = transform.flip_x;
                surface.flip_y = transform.flip_y;
            }
            if let Some(encoding) = &contract.boot_encoding {
                surface.encoding = match encoding {
                    WireImageEncoding::Jpeg => WireEncoding::Jpeg,
                    WireImageEncoding::Png => WireEncoding::Png,
                    WireImageEncoding::BgrRows => WireEncoding::BgrRows,
                    WireImageEncoding::BgrColumns => WireEncoding::BgrColumns,
                    WireImageEncoding::Rgb565Le => WireEncoding::Rgb565Le,
                };
            }
        }
        Self {
            manifest: contract.manifest.clone(),
            codec,
            image_output_disabled_reason: contract.image_output_disabled_reason.clone(),
        }
    }
    // Wake for the current operation
    fn wake(&self, initialize: bool) -> Result<DeviceTransaction> {
        let mut tx = DeviceTransaction::single(self.codec.command(b"DIS")?);
        if initialize || self.codec.reinitialize_on_wake {
            if let Some(value) = self.codec.wake_brightness {
                tx.reports
                    .push(self.codec.command(&[b'L', b'I', b'G', 0, 0, value])?);
            }
            if let Some(mode) = self.codec.mode {
                let report = self.codec.command(&[b'M', b'O', b'D', 0, 0, b'1' + mode])?;
                if self.codec.mode_before_wake {
                    tx.reports.insert(0, report);
                } else {
                    tx.reports.push(report);
                }
            }
        }
        Ok(tx)
    }
    // Build the protocol transfer for an image upload
    fn image_upload(
        &self,
        control_id: String,
        data: Vec<u8>,
        grouped: bool,
        purpose: EncodingPurpose,
    ) -> Result<FrameUpload> {
        if let Some(reason) = &self.image_output_disabled_reason {
            return Err(Error::Unsupported(reason.clone()));
        }
        let control = self.manifest.control(&control_id)?;
        let surface = self
            .codec
            .surfaces
            .iter()
            .find(|s| s.control_id == control_id)
            .ok_or_else(|| Error::Unsupported("control has no image display".into()))?;
        let data = encode_surface_for(
            &data,
            [control.image_width.unwrap(), control.image_height.unwrap()],
            surface,
            purpose,
        )?;
        let mut header = b"BAT".to_vec();
        header.extend((data.len() as u32).to_be_bytes());
        header.push(control.image_slot.unwrap());
        // Touch and display-only uploads keep their independent refresh boundary
        let grouped = grouped && control.kind == "button";
        Ok(FrameUpload {
            transaction: self.codec.transfer(&header, &data, !grouped, &control_id)?,
            embedded_commit_count: usize::from(!grouped),
            commit: if grouped {
                Some(FrameCommit {
                    group: format!("{}-keys-stp-v1", self.manifest.model_id),
                    reports: vec![self.codec.command(b"STP")?],
                })
            } else {
                None
            },
        })
    }
}
impl HardwareSession for Session {
    // Check support for frame encoding
    fn supports_frame_encoding(&self, _: FrameEncoding) -> bool {
        true
    }
    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        self.encode_frame_with_purpose(commands, encoding, ImagePurpose::Static)
    }
    // Encode frame with purpose
    fn encode_frame_with_purpose(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
        purpose: ImagePurpose,
    ) -> Result<Vec<FrameUpload>> {
        commands
            .into_iter()
            .map(|command| match command {
                DeviceCommand::Image { control_id, data } => self.image_upload(
                    control_id,
                    data,
                    encoding == FrameEncoding::GroupedKeyUpdates,
                    if purpose == ImagePurpose::Animated {
                        EncodingPurpose::Animated
                    } else {
                        EncodingPurpose::Static
                    },
                ),
                command => self.encode(command).map(FrameUpload::from),
            })
            .collect()
    }
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
        Ok(vec![
            self.wake(true)?,
            self.encode(DeviceCommand::Clear)?,
            DeviceTransaction::single(self.codec.command(b"STP")?),
        ])
    }
    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>> {
        let data = if self.codec.report_id == 0 {
            super::normalize(report)
        } else if report.first() == Some(&self.codec.report_id) {
            &report[1..]
        } else {
            return Ok(vec![]);
        };
        if data.get(..3) != Some(b"ACK") {
            return Ok(vec![]);
        }
        if data.len() < 11 {
            return Err(Error::Protocol("truncated CRT input".into()));
        }
        if self.codec.require_input_ok && data.get(5..7) != Some(b"OK") {
            return Ok(vec![]);
        }
        let (code, state) = (data[9], data[10]);
        if ![0, 1, 2].contains(&state) {
            return Ok(vec![]);
        }
        let Some(mapping) = self.codec.inputs.iter().find(|m| m.address().0 == code) else {
            return Ok(vec![]);
        };
        let states = match mapping {
            Mapping::Button { .. } => &self.codec.button_states,
            Mapping::Pulse { .. } => &self.codec.button_states,
            Mapping::Rotate { .. } => &self.codec.rotate_states,
            Mapping::Swipe { .. } => &self.codec.swipe_states,
        };
        if !states.contains(&state) {
            return Ok(vec![]);
        }
        let input = match mapping {
            Mapping::Button { .. } => Input::Button {
                pressed: state == 1,
            },
            Mapping::Pulse { .. } if state == 1 => Input::Pulse,
            Mapping::Pulse { .. } => return Ok(vec![]),
            Mapping::Rotate { delta, .. } => Input::Rotate { delta: *delta },
            Mapping::Swipe { direction, .. } => Input::Swipe {
                direction: direction.clone(),
            },
        };
        Ok(vec![ControlEvent {
            control_id: mapping.address().1.into(),
            input,
        }])
    }
    // Encode for the current operation
    fn encode(&mut self, command: DeviceCommand) -> Result<DeviceTransaction> {
        let one = |bytes: &[u8]| self.codec.command(bytes).map(DeviceTransaction::single);
        match command {
            DeviceCommand::Keyboard(_)
            | DeviceCommand::KeyboardFactoryReset
            | DeviceCommand::KeyboardSettingsQuery => {
                Err(Error::Unsupported("keyboard settings".into()))
            }
            DeviceCommand::Vibration { .. } => {
                Err(Error::Unsupported("vibration configuration".into()))
            }
            DeviceCommand::Wake => self.wake(false),
            DeviceCommand::Sleep => one(b"HAN"),
            DeviceCommand::Heartbeat => one(b"CONNECT"),
            DeviceCommand::Clear => one(b"CLE\0\0\0\xff"),
            DeviceCommand::Brightness(value) if value <= 100 => {
                one(&[b'L', b'I', b'G', 0, 0, value])
            }
            DeviceCommand::Brightness(_) => {
                Err(Error::Protocol("brightness must be 0..100".into()))
            }
            DeviceCommand::Lighting { brightness, colors } => {
                if !self.manifest.capabilities.lighting
                    || colors.len() != self.manifest.capabilities.lighting_channels.len()
                {
                    return Err(Error::Unsupported(
                        "lighting channels do not match this model".into(),
                    ));
                }
                if brightness > 100 {
                    return Err(Error::Protocol("lighting must be 0..100".into()));
                }
                let mut bytes = b"SETLB".to_vec();
                for color in colors {
                    bytes.extend(color);
                }
                Ok(DeviceTransaction {
                    reports: vec![
                        self.codec
                            .command(&[b'L', b'B', b'L', b'I', b'G', brightness])?,
                        self.codec.command(&bytes)?,
                    ],
                    coalesce_key: Some("lighting".into()),
                })
            }
            DeviceCommand::Image { control_id, data } => self
                .image_upload(control_id, data, false, EncodingPurpose::Static)
                .map(|upload| upload.transaction),
            DeviceCommand::Background {
                data,
                x,
                y,
                width,
                height,
            } => {
                if let Some(reason) = &self.image_output_disabled_reason {
                    return Err(Error::Unsupported(reason.clone()));
                }
                if !self.manifest.capabilities.background {
                    return Err(Error::Unsupported(
                        "live background unavailable; use an explicit startup logo upload".into(),
                    ));
                }
                let surface = self.codec.background.as_ref().ok_or_else(|| {
                    Error::Unsupported("background unavailable for this model/firmware".into())
                })?;
                let size = self.manifest.capabilities.background_size.unwrap();
                if [x, y] != [0, 0] || [width, height] != size {
                    return Err(Error::Unsupported(
                        "this adapter requires a complete background frame".into(),
                    ));
                }
                let data = encode_surface_for(&data, size, surface, EncodingPurpose::Background)?;
                let mut header = b"BGPIC".to_vec();
                header.extend((data.len() as u32).to_be_bytes());
                for value in [0, 0, width, height] {
                    header.extend(value.to_be_bytes());
                }
                header.extend([0, 0]);
                self.codec.transfer(&header, &data, false, "background")
            }
            DeviceCommand::BootLogo { data } => {
                let logo = self
                    .manifest
                    .capabilities
                    .boot_logo
                    .as_ref()
                    .ok_or_else(|| Error::Unsupported("startup logo unavailable".into()))?;
                let surface =
                    self.codec.boot.as_ref().ok_or_else(|| {
                        Error::Unsupported("startup logo surface unavailable".into())
                    })?;
                let data = encode_surface_for(&data, logo.size, surface, EncodingPurpose::Boot)?;
                let bytes_per_pixel = match surface.encoding {
                    WireEncoding::BgrRows | WireEncoding::BgrColumns => Some(3),
                    WireEncoding::Rgb565Le => Some(2),
                    _ => None,
                };
                if bytes_per_pixel.is_some_and(|n| {
                    data.len() != usize::from(logo.size[0]) * usize::from(logo.size[1]) * n
                }) {
                    return Err(Error::Protocol("invalid raw startup logo length".into()));
                }
                let mut header = b"LOG".to_vec();
                header.extend((data.len() as u32).to_be_bytes());
                header.push(1);
                let mut transaction = self.codec.transfer(&header, &data, false, "boot-logo")?;
                transaction.coalesce_key = None;
                Ok(transaction)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the protocol codec used by this test
    fn codec(report_size: usize, report_id: u8) -> Codec {
        Codec {
            schema_version: 1,
            report_size,
            report_id,
            mode: None,
            mode_before_wake: false,
            reinitialize_on_wake: true,
            require_input_ok: true,
            wake_brightness: None,
            inputs: vec![],
            surfaces: vec![],
            background: None,
            boot: None,
            button_states: vec![0, 1],
            rotate_states: vec![0],
            swipe_states: vec![0],
        }
    }
    // Expose the underlying error for error chaining
    fn source() -> Vec<u8> {
        let mut data = Cursor::new(vec![]);
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
            let n = (x + 3 * y) as u8 * 30;
            image::Rgba([n + 1, n + 2, n + 3, 255])
        }))
        .write_to(&mut data, ImageFormat::Png)
        .unwrap();
        data.into_inner()
    }
    // Resolve the surface value for this operation
    fn surface(encoding: WireEncoding, rotation: u16, flip_x: bool) -> Surface {
        Surface {
            control_id: "test".into(),
            encoding,
            rotation,
            flip_x,
            flip_y: false,
            static_target: None,
            animated_target: None,
        }
    }
    #[test]
    // Verify boot requires exact canvas and rgb565 is little endian
    fn boot_requires_exact_canvas_and_rgb565_is_little_endian() {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image::RgbImage::from_fn(3, 1, |x, _| {
            image::Rgb(match x {
                0 => [255, 0, 0],
                1 => [0, 255, 0],
                _ => [0, 0, 255],
            })
        }))
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
        let policy = surface(WireEncoding::Rgb565Le, 0, false);
        assert_eq!(
            encode_surface_for(bytes.get_ref(), [3, 1], &policy, EncodingPurpose::Boot).unwrap(),
            [0, 248, 224, 7, 31, 0]
        );
        assert!(
            encode_surface_for(bytes.get_ref(), [1, 3], &policy, EncodingPurpose::Boot).is_err()
        );
        // Ordinary key output may resize; persistent writes must not
        assert_eq!(
            encode_surface(bytes.get_ref(), [1, 3], &policy)
                .unwrap()
                .len(),
            6
        );
    }
    #[test]
    // Verify lossless soft targets never quantize or drop alpha
    fn lossless_soft_targets_never_quantize_or_drop_alpha() {
        let mut policy = surface(WireEncoding::Png, 0, false);
        policy.static_target = Some(1);
        policy.animated_target = Some(1);
        let mut input = Cursor::new(Vec::new());
        let original = image::RgbaImage::from_fn(32, 31, |x, y| {
            image::Rgba([
                (x * 7 + y * 11) as u8,
                (x * 13 + y * 17) as u8,
                (x * 19 + y * 23) as u8,
                (x * 29 + y * 31) as u8,
            ])
        });
        DynamicImage::ImageRgba8(original.clone())
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        for purpose in [EncodingPurpose::Static, EncodingPurpose::Animated] {
            let output = encode_surface_for(input.get_ref(), [32, 31], &policy, purpose).unwrap();
            assert!(output.len() > 1);
            assert_eq!(
                image::load_from_memory(&output).unwrap().to_rgba8(),
                original
            );
        }
    }
    #[test]
    // Verify source format preserves lossless PNG and selects JPEG budget
    fn source_format_preserves_lossless_png_and_selects_jpeg_budget() {
        let mut policy = surface(WireEncoding::SourceFormat, 0, false);
        policy.animated_target = Some(2040); // XL JPEG-only transport preference.
        for alpha in [false, true] {
            let original = image::RgbaImage::from_fn(80, 80, |x, y| {
                image::Rgba([
                    x as u8 * 3,
                    y as u8 * 3,
                    (x ^ y) as u8,
                    if alpha { (x + y) as u8 } else { 255 },
                ])
            });
            let mut input = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(original.clone())
                .write_to(&mut input, ImageFormat::Png)
                .unwrap();
            for purpose in [EncodingPurpose::Static, EncodingPurpose::Animated] {
                let bytes =
                    encode_surface_for(input.get_ref(), [80, 80], &policy, purpose).unwrap();
                assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
                assert_eq!(
                    image::load_from_memory(&bytes).unwrap().to_rgba8(),
                    original
                );
            }
        }
        let pixels = DynamicImage::ImageRgb8(image::RgbImage::from_fn(80, 80, |x, y| {
            let n = (x.wrapping_mul(1664525) ^ y.wrapping_mul(1013904223)).wrapping_mul(2654435761);
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        }));
        let mut input = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut input, 100)
            .encode_image(&pixels)
            .unwrap();
        let animated =
            encode_surface_for(&input, [80, 80], &policy, EncodingPurpose::Animated).unwrap();
        assert_eq!(image::guess_format(&animated).unwrap(), ImageFormat::Jpeg);
        assert!(animated.len() <= 2040);
        let still = encode_surface_for(&input, [80, 80], &policy, EncodingPurpose::Static).unwrap();
        assert_eq!(image::guess_format(&still).unwrap(), ImageFormat::Jpeg);
        assert!(still.len() > animated.len());
        assert!(encode_surface(b"not an image", [80, 80], &policy).is_err());
    }
    #[test]
    // Verify JPEG purpose targets are distinct and boot limit is hard
    fn jpeg_purpose_targets_are_distinct_and_boot_limit_is_hard() {
        let original = image::RgbImage::from_fn(256, 256, |x, y| {
            let n = (x.wrapping_mul(1664525) ^ y.wrapping_mul(1013904223)).wrapping_mul(2654435761);
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        });
        let mut input = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(original)
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        let policy = surface(WireEncoding::Jpeg, 0, false);
        let still = encode_surface_for(
            input.get_ref(),
            [256, 256],
            &policy,
            EncodingPurpose::Static,
        )
        .unwrap();
        let animated = encode_surface_for(
            input.get_ref(),
            [256, 256],
            &policy,
            EncodingPurpose::Animated,
        )
        .unwrap();
        assert!(animated.len() <= still.len());
        assert_ne!(still, animated);
        let large = image::RgbImage::from_fn(800, 480, |x, y| {
            let n = (x.wrapping_mul(1664525) ^ y.wrapping_mul(1013904223)).wrapping_mul(2654435761);
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        });
        let mut input = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(large)
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        assert!(
            encode_surface_for(input.get_ref(), [800, 480], &policy, EncodingPurpose::Boot)
                .is_err()
        );
    }
    #[test]
    // Verify every report boundary preserves payload and numbered report padding
    fn every_report_boundary_preserves_payload_and_numbered_report_padding() {
        for (size, id, payload) in [(513, 0, 512), (1025, 0, 1024), (1025, 4, 1023)] {
            let codec = codec(size, id);
            assert_eq!(codec.payload_size(), payload);
            for length in [payload - 1, payload, payload + 1, 2 * payload + 7] {
                let data: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
                let tx = codec.transfer(b"BAT", &data, true, "test").unwrap();
                assert_eq!(tx.reports.len(), length.div_ceil(payload) + 2);
                assert!(tx.reports.iter().all(|p| p.len() == size && p[0] == id));
                let reconstructed: Vec<_> = tx.reports[1..tx.reports.len() - 1]
                    .iter()
                    .flat_map(|r| r[1..1 + payload].iter().copied())
                    .take(length)
                    .collect();
                assert_eq!(data, reconstructed);
                if id != 0 {
                    assert!(tx.reports.iter().all(|p| p[size - 1] == 0));
                }
                assert_eq!(&tx.reports.last().unwrap()[6..9], b"STP");
            }
            assert!(codec.command(&vec![0; payload]).is_err());
            assert!(codec.transfer(b"LOG", &[], false, "test").is_err());
        }
    }
    #[test]
    // Verify asymmetric RGBA rotation flip and raw storage order follow vendor transforms
    fn asymmetric_rgba_rotation_flip_and_raw_storage_order_follow_vendor_transforms() {
        // A B C / D E F -> counterclockwise rotation then horizontal flip:
        // F C / E B / D A. Assert pixel order, channel order and swapped dimensions
        let bytes =
            encode_surface(&source(), [3, 2], &surface(WireEncoding::Png, 270, true)).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 3));
        assert_eq!(decoded.color(), image::ColorType::Rgba8);
        assert_eq!(
            decoded
                .to_rgba8()
                .pixels()
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [151, 61, 121, 31, 91, 1]
        );
        // Raw 293: row-major BGR; raw 293s: horizontal flip then column-major BGR
        let rows = encode_surface(
            &source(),
            [3, 2],
            &surface(WireEncoding::BgrRows, 180, false),
        )
        .unwrap();
        let expected_rows: Vec<u8> = [150, 120, 90, 60, 30, 0]
            .into_iter()
            .flat_map(|n| [n + 3, n + 2, n + 1])
            .collect();
        assert_eq!(rows, expected_rows);
        let columns = encode_surface(
            &source(),
            [3, 2],
            &surface(WireEncoding::BgrColumns, 0, true),
        )
        .unwrap();
        let expected_columns: Vec<u8> = [60, 150, 30, 120, 0, 90]
            .into_iter()
            .flat_map(|n| [n + 3, n + 2, n + 1])
            .collect();
        assert_eq!(columns, expected_columns);
    }
    #[test]
    // Verify raw output flattens alpha and invalid sources do not allocate wire images
    fn raw_output_flattens_alpha_and_invalid_sources_do_not_allocate_wire_images() {
        let mut input = Cursor::new(vec![]);
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                image::Rgba([255, 127, 0, 128])
            } else {
                image::Rgba([255, 255, 255, 0])
            }
        }))
        .write_to(&mut input, ImageFormat::Png)
        .unwrap();
        assert_eq!(
            encode_surface(
                input.get_ref(),
                [2, 1],
                &surface(WireEncoding::BgrRows, 0, false)
            )
            .unwrap(),
            [0, 64, 128, 0, 0, 0]
        );
        assert!(encode_surface(&[], [1, 1], &surface(WireEncoding::Jpeg, 0, false)).is_err());
        assert!(encode_surface(&source(), [0, 1], &surface(WireEncoding::Jpeg, 0, false)).is_err());
        assert!(
            encode_surface(&source(), [4097, 1], &surface(WireEncoding::Jpeg, 0, false)).is_err()
        );
        assert!(
            encode_surface(&source(), [1, 1], &surface(WireEncoding::Jpeg, 45, false)).is_err()
        );
        assert!(encode_surface(b"<svg/>", [1, 1], &surface(WireEncoding::Png, 0, false)).is_err());
    }
}
