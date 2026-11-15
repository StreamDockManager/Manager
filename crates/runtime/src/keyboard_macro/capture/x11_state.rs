//! Pure XI2 admission and state checks. Device names and keysyms are never identity
use super::{CaptureError, PhysicalKey, KEY_SLOTS};
use crate::keyboard_macro::platform::linux_mapping;

pub(super) const MAX_DEVICES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    Xtest,
    Physical,
}

/// Xorg's XTEST Device property is INTEGER/8, exactly one true value. A missing
/// property alone does not establish a physical source: require a driver node
/// The path is inspected only as metadata, never opened, retained, or logged
pub(super) fn classify_source(
    xtest: Option<&[u8]>,
    node: Option<&[u8]>,
) -> Result<SourceKind, CaptureError> {
    match xtest {
        Some([1]) => return Ok(SourceKind::Xtest),
        Some(_) => return Err(CaptureError::RawInputUnavailable),
        None => (),
    }
    let node = node.ok_or(CaptureError::RawInputUnavailable)?;
    let suffix = node
        .strip_prefix(b"/dev/input/event")
        .filter(|suffix| !suffix.is_empty() && suffix.len() <= 10)
        .ok_or(CaptureError::RawInputUnavailable)?;
    if !suffix.iter().all(u8::is_ascii_digit) {
        return Err(CaptureError::RawInputUnavailable);
    }
    Ok(SourceKind::Physical)
}

// Read the X11 key map used to resolve physical keys
pub(super) fn key_map(names: &[(u8, [u8; 4])]) -> Result<[Option<PhysicalKey>; 256], CaptureError> {
    let mut result = [None; 256];
    let mut seen_codes = [false; 256];
    for (code, _) in names {
        if *code < 8 || std::mem::replace(&mut seen_codes[usize::from(*code)], true) {
            return Err(CaptureError::UnsupportedKey);
        }
    }
    for key in PhysicalKey::ALL {
        let Some(name) = linux_mapping::xkb_name(*key) else {
            continue;
        };
        let mut candidates = names.iter().filter(|(_, candidate)| *candidate == name);
        if let Some((code, _)) = candidates.next() {
            if candidates.next().is_some() {
                return Err(CaptureError::UnsupportedKey);
            }
            result[usize::from(*code)] = Some(*key);
        }
    }
    Ok(result)
}

pub(super) struct Device {
    pub id: u16,
    pub kind: SourceKind,
    pub keys: [Option<PhysicalKey>; 256],
    held: [bool; KEY_SLOTS],
}
impl Device {
    // Initialize X11 state state from the supplied inputs
    pub fn new(id: u16, kind: SourceKind, keys: [Option<PhysicalKey>; 256]) -> Self {
        Self {
            id,
            kind,
            keys,
            held: [false; KEY_SLOTS],
        }
    }
}

pub(super) struct State {
    devices: Vec<Device>,
    pending_raw: std::collections::VecDeque<(u16, u8, bool, u32)>,
}
impl State {
    // Initialize X11 state state from the supplied inputs
    pub fn new(devices: Vec<Device>) -> Result<Self, CaptureError> {
        if devices.is_empty() || devices.len() > MAX_DEVICES {
            return Err(CaptureError::RawInputUnavailable);
        }
        for (index, device) in devices.iter().enumerate() {
            if device.id < 2 || devices[..index].iter().any(|prior| prior.id == device.id) {
                return Err(CaptureError::RawInputUnavailable);
            }
        }
        Ok(Self {
            devices,
            pending_raw: Default::default(),
        })
    }
    // Report the synthetic source condition for X11 state
    pub fn synthetic_source(&self, source: u16) -> bool {
        self.devices
            .iter()
            .any(|device| device.id == source && device.kind == SourceKind::Xtest)
    }
    // Report the known source condition for X11 state
    pub fn known_source(&self, source: u16) -> bool {
        self.devices.iter().any(|device| device.id == source)
    }
    // Translate raw X11 recording input into physical key observations
    pub fn recording_raw(
        &mut self,
        source: u16,
        code: u32,
        down: bool,
        time: u32,
    ) -> Result<(), CaptureError> {
        let device = self
            .devices
            .iter()
            .find(|device| device.id == source)
            .ok_or(CaptureError::DeviceChanged)?;
        if device.kind == SourceKind::Xtest {
            return Ok(());
        }
        let code = u8::try_from(code).map_err(|_| CaptureError::UnsupportedKey)?;
        if device.keys[usize::from(code)].is_none() {
            return Err(CaptureError::UnsupportedKey);
        }
        if self.pending_raw.len() >= 1024 {
            return Err(CaptureError::BufferOverflow);
        }
        self.pending_raw.push_back((source, code, down, time));
        Ok(())
    }
    /// RECORD device events include the physical slave ID. Normal edges must
    /// agree with XI2 raw provenance, rejecting XKB redirected/synthetic edges
    /// Server repeats may have no raw event, but must belong to an already held
    /// physical key on that exact slave. Never merge two keyboard owners
    pub fn recorded_event(
        &mut self,
        source: u16,
        code: u8,
        down: bool,
        time: u32,
    ) -> Result<Option<(PhysicalKey, bool)>, CaptureError> {
        let device = self
            .devices
            .iter()
            .find(|d| d.id == source)
            .ok_or(CaptureError::DeviceChanged)?;
        if device.kind == SourceKind::Xtest {
            return Ok(None);
        }
        let key = device.keys[usize::from(code)].ok_or(CaptureError::UnsupportedKey)?;
        let repeat = down && device.held[key as usize];
        let matched = self
            .pending_raw
            .iter()
            .position(|raw| *raw == (source, code, down, time));
        if let Some(index) = matched {
            self.pending_raw.remove(index);
        } else if !repeat {
            return Err(CaptureError::InputMismatch);
        }
        self.event(source, u32::from(code), down, repeat)
            .map(|value| value.map(|key| (key, repeat)))
    }
    // Check record lag
    pub fn check_record_lag(&self, server_time: u32) -> Result<(), CaptureError> {
        if self.pending_raw.iter().any(|(_, _, _, time)| {
            let age = server_time.wrapping_sub(*time);
            age > 100 && age < (u32::MAX / 2)
        }) {
            Err(CaptureError::InputMismatch)
        } else {
            Ok(())
        }
    }
    // Report support for record IDs
    pub fn supports_record_ids(&self) -> bool {
        self.devices.iter().all(|device| device.id < 128)
    }
    // Handle the incoming X11 state event
    pub fn event(
        &mut self,
        source: u16,
        code: u32,
        down: bool,
        repeat: bool,
    ) -> Result<Option<PhysicalKey>, CaptureError> {
        let index = self
            .devices
            .iter()
            .position(|device| device.id == source)
            .ok_or(CaptureError::DeviceChanged)?;
        if self.devices[index].kind == SourceKind::Xtest {
            return Ok(None);
        }
        let key = usize::try_from(code)
            .ok()
            .and_then(|code| self.devices[index].keys.get(code).copied().flatten())
            .ok_or(CaptureError::UnsupportedKey)?;
        let slot = key as usize;
        let held = self.devices[index].held[slot];
        if repeat && (!down || !held) {
            return Err(CaptureError::InputMismatch);
        }
        if down && !repeat && held || !down && !held {
            return Err(CaptureError::InputMismatch);
        }
        // The recording schema cannot represent two physical keyboards owning
        // the same canonical key. Do not misclassify that overlap as auto-repeat
        if down
            && !repeat
            && self
                .devices
                .iter()
                .enumerate()
                .any(|(other, device)| other != index && device.held[slot])
        {
            return Err(CaptureError::InputMismatch);
        }
        self.devices[index].held[slot] = down;
        Ok(Some(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Resolve the physical key for the observed X11 key code
    fn physical(id: u16) -> Device {
        Device::new(
            id,
            SourceKind::Physical,
            key_map(&[(44, *b"AC01"), (61, *b"RCTL")]).unwrap(),
        )
    }
    #[test]
    // Verify that record edges require raw provenance but server repeats do not
    fn record_edges_require_raw_provenance_but_server_repeats_do_not() {
        let mut state = State::new(vec![
            physical(9),
            physical(10),
            Device::new(3, SourceKind::Xtest, [None; 256]),
        ])
        .unwrap();
        assert!(state.supports_record_ids());
        assert!(state.synthetic_source(3));
        assert!(!state.synthetic_source(9));
        assert_eq!(
            state.recorded_event(9, 44, true, 100),
            Err(CaptureError::InputMismatch)
        );
        state.recording_raw(9, 44, true, 100).unwrap();
        assert_eq!(
            state.recorded_event(9, 44, true, 100),
            Ok(Some((PhysicalKey::KeyA, false)))
        );
        assert_eq!(
            state.recorded_event(9, 44, true, 200),
            Ok(Some((PhysicalKey::KeyA, true)))
        );
        assert_eq!(
            state.recorded_event(10, 44, true, 200),
            Err(CaptureError::InputMismatch)
        );
        state.recording_raw(9, 44, false, 250).unwrap();
        assert_eq!(
            state.recorded_event(9, 44, false, 250),
            Ok(Some((PhysicalKey::KeyA, false)))
        );
        assert_eq!(
            state.recorded_event(9, 44, true, 260),
            Err(CaptureError::InputMismatch)
        );
        assert_eq!(state.recorded_event(3, 255, true, 260), Ok(None));
        assert!(state.check_record_lag(500).is_ok());
        assert!(!State::new(vec![physical(128)])
            .unwrap()
            .supports_record_ids());
    }
    #[test]
    // Verify that record rejects transformed edges stale unmatched raw and overflow
    fn record_rejects_transformed_edges_stale_unmatched_raw_and_overflow() {
        let mut state = State::new(vec![physical(9)]).unwrap();
        state.recording_raw(9, 44, true, u32::MAX - 20).unwrap();
        assert!(state.check_record_lag(10).is_ok());
        assert_eq!(
            state.check_record_lag(100),
            Err(CaptureError::InputMismatch)
        );
        assert_eq!(
            state.recorded_event(9, 61, true, u32::MAX - 20),
            Err(CaptureError::InputMismatch)
        );
        for _ in 1..1024 {
            state.recording_raw(9, 44, true, 100).unwrap();
        }
        assert_eq!(
            state.recording_raw(9, 44, true, 101),
            Err(CaptureError::BufferOverflow)
        );
    }
    #[test]
    // Verify that synthetic property is exact and unknown sources are not ignored
    fn synthetic_property_is_exact_and_unknown_sources_are_not_ignored() {
        assert_eq!(classify_source(Some(&[1]), None), Ok(SourceKind::Xtest));
        assert_eq!(
            classify_source(None, Some(b"/dev/input/event12")),
            Ok(SourceKind::Physical)
        );
        for marker in [&[][..], &[0], &[1, 1]] {
            assert!(classify_source(Some(marker), Some(b"/dev/input/event1")).is_err());
        }
        for node in [
            None,
            Some(&b"/dev/input/event"[..]),
            Some(&b"/dev/input/event1\0"[..]),
            Some(&b"/tmp/event1"[..]),
        ] {
            assert!(classify_source(None, node).is_err());
        }
        let mut state = State::new(vec![physical(9)]).unwrap();
        assert!(state.known_source(9));
        assert!(!state.known_source(10));
        assert_eq!(
            state.event(10, 44, true, false),
            Err(CaptureError::DeviceChanged)
        );
    }
    #[test]
    // Verify that physical mapping uses names and rejects aliases
    fn physical_mapping_uses_names_and_rejects_aliases() {
        let map = key_map(&[(250, *b"AC01"), (19, *b"RCTL")]).unwrap();
        assert_eq!(map[250], Some(PhysicalKey::KeyA));
        assert_eq!(map[19], Some(PhysicalKey::ControlRight));
        assert!(key_map(&[(44, *b"AC01"), (45, *b"AC01")]).is_err());
        assert!(key_map(&[(44, *b"AC01"), (44, *b"RCTL")]).is_err());
        assert!(key_map(&[(1, *b"AC01")]).is_err());
    }
    #[test]
    // Verify that repeat is explicit and overlapping keyboards fail closed
    fn repeat_is_explicit_and_overlapping_keyboards_fail_closed() {
        let mut state = State::new(vec![physical(9), physical(10)]).unwrap();
        assert_eq!(state.event(9, 44, true, false), Ok(Some(PhysicalKey::KeyA)));
        assert_eq!(state.event(9, 44, true, true), Ok(Some(PhysicalKey::KeyA)));
        assert_eq!(
            state.event(10, 44, true, false),
            Err(CaptureError::InputMismatch)
        );
        assert_eq!(
            state.event(9, 44, false, false),
            Ok(Some(PhysicalKey::KeyA))
        );
        assert_eq!(
            state.event(9, 44, true, true),
            Err(CaptureError::InputMismatch)
        );
        assert_eq!(
            state.event(9, 44, false, false),
            Err(CaptureError::InputMismatch)
        );
        assert_eq!(
            state.event(9, 700, true, false),
            Err(CaptureError::UnsupportedKey)
        );
    }
    #[test]
    // Verify that xtest is filtered before key decode and device inventory is bounded
    fn xtest_is_filtered_before_key_decode_and_device_inventory_is_bounded() {
        let mut state = State::new(vec![
            physical(9),
            Device::new(3, SourceKind::Xtest, [None; 256]),
        ])
        .unwrap();
        assert_eq!(state.event(3, u32::MAX, true, false), Ok(None));
        assert!(State::new(vec![physical(9), physical(9)]).is_err());
        assert!(State::new((2..35).map(physical).collect()).is_err());
    }
}
