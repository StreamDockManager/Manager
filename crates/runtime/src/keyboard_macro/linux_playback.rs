//! XKB-name/XTest and authorized RemoteDesktop physical-key adapters. Neither
//! route is enabled without physical interference, protected-session, stop and
//! repeat guarantees. Wayland never falls back to X11/XWayland
use super::*;
use crate::keyboard_macro::capture::NativePlaybackObserver;
use x11rb::{
    connection::Connection,
    protocol::{
        xinput, xkb,
        xproto::{self, AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask},
        xtest, Event,
    },
};

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Code {
    X11(u8),
    Evdev(u16),
}
struct X11State {
    connection: x11_connection::Connection,
    root: u32,
    names: Vec<(u8, [u8; 4])>,
    active_atom: u32,
    target: Option<u32>,
    changed: bool,
    admission_revoked: bool,
    route_revoked: bool,
    master: u16,
    source: u16,
    master_repeat: repeat::X11RepeatControls,
    source_repeat: repeat::X11RepeatControls,
    modifier_masks: [u8; 256],
    held: [bool; 256],
}
impl X11State {
    // Establish the Linux playback connection
    fn connect() -> Result<Self, String> {
        let (connection, screen) =
            x11_connection::connect().map_err(|_| "X11 keyboard connection is unavailable")?;
        let root = connection
            .setup()
            .roots
            .get(screen)
            .ok_or("X11 screen is unavailable")?
            .root;
        if !xkb::use_extension(&connection, 1, 0)
            .map_err(|_| "XKB is unavailable")?
            .reply()
            .map_err(|_| "XKB is unavailable")?
            .supported
        {
            return Err("XKB physical names are unavailable".into());
        }
        xtest::get_version(&connection, 2, 2)
            .map_err(|_| "XTest is unavailable")?
            .reply()
            .map_err(|_| "XTest is unavailable")?;
        let version = xinput::xi_query_version(&connection, 2, 1)
            .map_err(|_| "XI2 is unavailable")?
            .reply()
            .map_err(|_| "XI2 is unavailable")?;
        if (version.major_version, version.minor_version) < (2, 1) {
            return Err("XI2.1 device observation is unavailable".into());
        }
        xinput::xi_select_events(
            &connection,
            root,
            &[xinput::EventMask {
                deviceid: 0,
                mask: vec![
                    xinput::XIEventMask::HIERARCHY
                        | xinput::XIEventMask::DEVICE_CHANGED
                        | xinput::XIEventMask::PROPERTY,
                ],
            }],
        )
        .map_err(|_| "X11 device observer is unavailable")?
        .check()
        .map_err(|_| "X11 device observer is unavailable")?;
        let (master, source) = output_devices(&connection)?;
        for device in [master, source] {
            xkb::select_events(
                &connection,
                device,
                xkb::EventType::default(),
                xkb::EventType::NEW_KEYBOARD_NOTIFY
                    | xkb::EventType::MAP_NOTIFY
                    | xkb::EventType::NAMES_NOTIFY
                    | xkb::EventType::COMPAT_MAP_NOTIFY
                    | xkb::EventType::CONTROLS_NOTIFY
                    | xkb::EventType::STATE_NOTIFY,
                xkb::MapPart::from(0xffu16),
                xkb::MapPart::from(0xffu16),
                &xkb::SelectEventsAux::default(),
            )
            .map_err(|_| "XKB observer is unavailable")?
            .check()
            .map_err(|_| "XKB observer is unavailable")?;
        }
        let master_repeat = repeat_controls(&connection, master)?;
        let source_repeat = repeat_controls(&connection, source)?;
        neutral_keyboard_state(&connection, master)?;
        neutral_keyboard_state(&connection, source)?;
        let names = xkb::get_names(&connection, master, xkb::NameDetail::KEY_NAMES)
            .map_err(|_| "XKB names are unavailable")?
            .reply()
            .map_err(|_| "XKB names are unavailable")?;
        if u16::from(names.device_id) != master {
            return Err("XKB master identity changed".into());
        }
        let first = names.first_key;
        let names = names
            .value_list
            .key_names
            .ok_or("XKB key names are unavailable")?
            .into_iter()
            .enumerate()
            .map(|(index, key)| {
                let code = usize::from(first)
                    .checked_add(index)
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or("XKB keycode range is invalid")?;
                Ok((code, key.name))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let source_names = xkb::get_names(&connection, source, xkb::NameDetail::KEY_NAMES)
            .map_err(|_| "XKB XTEST source names unavailable")?
            .reply()
            .map_err(|_| "XKB XTEST source names unavailable")?;
        if u16::from(source_names.device_id) != source
            || source_names.first_key != first
            || source_names
                .value_list
                .key_names
                .as_ref()
                .map(|values| values.iter().map(|k| k.name).collect::<Vec<_>>())
                != Some(names.iter().map(|(_, name)| *name).collect())
        {
            return Err("XKB XTEST source/master mappings differ".into());
        }
        let active_atom = connection
            .intern_atom(true, b"_NET_ACTIVE_WINDOW")
            .map_err(|_| "X11 target identity is unavailable")?
            .reply()
            .map_err(|_| "X11 target identity is unavailable")?
            .atom;
        if active_atom == 0 {
            return Err("This window manager has no verifiable active-window identity".into());
        }
        Ok(Self {
            connection,
            root,
            names,
            active_atom,
            target: None,
            changed: false,
            admission_revoked: false,
            route_revoked: false,
            master,
            source,
            master_repeat,
            source_repeat,
            modifier_masks: [0; 256],
            held: [false; 256],
        })
    }
    // Read current target
    fn current_target(&self) -> Result<u32, String> {
        self.connection
            .stream()
            .arm(std::time::Duration::from_millis(75));
        let reply = self
            .connection
            .get_property(false, self.root, self.active_atom, AtomEnum::WINDOW, 0, 1)
            .map_err(|_| "X11 target is unavailable")?
            .reply()
            .map_err(|_| "X11 target is unavailable")?;
        let target = reply
            .value32()
            .and_then(|mut values| values.next())
            .filter(|value| *value != 0)
            .ok_or("X11 target is unavailable")?;
        Ok(target)
    }
    // Create a focus watcher for the selected playback target
    fn watch_target(&mut self) -> Result<(), String> {
        self.connection
            .stream()
            .arm(std::time::Duration::from_millis(250));
        self.connection
            .change_window_attributes(
                self.root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(|_| "X11 target observer is unavailable")?
            .check()
            .map_err(|_| "X11 target observer is unavailable")?;
        let target = self.current_target()?;
        self.connection
            .change_window_attributes(
                target,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(|_| "X11 target observer is unavailable")?
            .check()
            .map_err(|_| "X11 target observer is unavailable")?;
        self.target = Some(target);
        self.check_target()
    }
    // Check target
    fn check_target(&mut self) -> Result<(), String> {
        self.check_state(true)
    }
    // Check state
    fn check_state(&mut self, enforce_target: bool) -> Result<(), String> {
        self.connection
            .stream()
            .arm(std::time::Duration::from_millis(75));
        // Roundtrip first so queued server-side control changes precede the drain
        // This is revocation, not atomic isolation from future server changes
        if self.route_revoked {
            return Err("X11 output device binding was revoked".into());
        }
        if output_devices(&self.connection) != Ok((self.master, self.source)) {
            self.route_revoked = true;
            return Err("X11 output device binding changed or became unavailable".into());
        }
        if enforce_target {
            // Only our admitted momentary SetMods keys may alter depressed
            // modifiers. Latches, locks and groups remain neutral throughout
            let expected = self
                .held
                .iter()
                .enumerate()
                .filter(|(_, held)| **held)
                .fold(0, |mask, (code, _)| mask | self.modifier_masks[code]);
            if keyboard_state(&self.connection, self.master, expected).is_err()
                || keyboard_state(&self.connection, self.source, expected).is_err()
            {
                self.admission_revoked = true;
            }
        }
        for count in 0..=256 {
            let Some(event) = self.connection.poll_for_event().map_err(|_| {
                self.route_revoked = true;
                "X11 target observer stopped"
            })?
            else {
                break;
            };
            if count == 256 {
                self.route_revoked = true;
                return Err("X11 target observer queue overflowed".into());
            }
            match event {
                Event::PropertyNotify(event)
                    if event.window == self.root && event.atom == self.active_atom =>
                {
                    self.changed = true
                }
                Event::DestroyNotify(event) if Some(event.window) == self.target => {
                    self.changed = true
                }
                Event::XkbMapNotify(_)
                | Event::XkbNamesNotify(_)
                | Event::XkbCompatMapNotify(_)
                | Event::XkbControlsNotify(_)
                | Event::MappingNotify(_) => self.admission_revoked = true,
                Event::XkbStateNotify(event) => {
                    // Depressed-modifier transitions caused by our own keys
                    // are expected. Sticky lock/latch/group changes are not,
                    // even if another client restores them before the query
                    if u16::from(event.latched_mods) != 0
                        || u16::from(event.locked_mods) != 0
                        || u8::from(event.group) != 0
                        || event.base_group != 0
                        || event.latched_group != 0
                        || u8::from(event.locked_group) != 0
                    {
                        self.admission_revoked = true;
                    }
                }
                Event::XinputDeviceChanged(event)
                    if event.reason == xinput::ChangeReason::SLAVE_SWITCH
                        && event.deviceid == self.master
                        && event.sourceid == self.source => {}
                Event::XkbNewKeyboardNotify(_)
                | Event::XinputHierarchy(_)
                | Event::XinputDeviceChanged(_)
                | Event::XinputProperty(_) => {
                    self.route_revoked = true;
                    return Err("Keyboard macro output device identity changed".into());
                }
                Event::Error(_) => {
                    self.route_revoked = true;
                    return Err("X11 target observer failed".into());
                }
                _ => (),
            }
        }
        if enforce_target {
            if let Some(target) = self.target {
                self.changed |= self.current_target()? != target;
            }
        }
        output_admission(self.admission_revoked, self.changed, !enforce_target)
    }
}

/// Cleanup uses the original resolved keycode on the still-pinned output route
/// Mapping/repeat/focus changes revoke new presses, not that exact release
/// Route identity and physical ownership remain separately mandatory
fn output_admission(
    policy_changed: bool,
    target_changed: bool,
    cleanup: bool,
) -> Result<(), String> {
    if !cleanup && policy_changed {
        Err("Keyboard macro stopped because the XKB mapping or controls changed".into())
    } else if !cleanup && target_changed {
        Err("Keyboard macro stopped because the target changed".into())
    } else {
        Ok(())
    }
}
// Core XTest chooses PickKeyboard(client), the keyboard paired with this
// connection's ClientPointer. A device name is not identity. Do not create a
// private master or change another client's pointer/focus to make this work
fn output_devices(connection: &x11_connection::Connection) -> Result<(u16, u16), String> {
    connection
        .get_input_focus()
        .map_err(|_| "X11 binding is unavailable")?
        .reply()
        .map_err(|_| "X11 binding is unavailable")?;
    let pointer = xinput::xi_get_client_pointer(connection, 0)
        .map_err(|_| "X11 client pointer is unavailable")?
        .reply()
        .map_err(|_| "X11 client pointer is unavailable")?;
    if !pointer.set {
        return Err("X11 client pointer is unknown".into());
    }
    let inventory = xinput::xi_query_device(connection, 0u16)
        .map_err(|_| "X11 devices are unavailable")?
        .reply()
        .map_err(|_| "X11 devices are unavailable")?
        .infos;
    if inventory.len() > 64 {
        return Err("X11 device inventory exceeds supported bounds".into());
    }
    let master = inventory
        .iter()
        .find(|d| {
            d.enabled
                && d.type_ == xinput::DeviceType::MASTER_POINTER
                && d.deviceid == pointer.deviceid
        })
        .ok_or("X11 client pointer is not an enabled master")?
        .attachment;
    if inventory
        .iter()
        .filter(|d| d.enabled && d.type_ == xinput::DeviceType::MASTER_KEYBOARD)
        .count()
        != 1
        || !inventory.iter().any(|d| {
            d.enabled
                && d.deviceid == master
                && d.type_ == xinput::DeviceType::MASTER_KEYBOARD
                && d.attachment == pointer.deviceid
        })
        || master > 255
    {
        return Err("X11 output master is ambiguous".into());
    }
    let atom = connection
        .intern_atom(true, b"XTEST Device")
        .map_err(|_| "XTEST identity unavailable")?
        .reply()
        .map_err(|_| "XTEST identity unavailable")?
        .atom;
    if atom == 0 {
        return Err("XTEST identity unavailable".into());
    }
    let mut sources = Vec::new();
    for device in inventory.iter().filter(|d| {
        d.enabled && d.type_ == xinput::DeviceType::SLAVE_KEYBOARD && d.attachment == master
    }) {
        let property = xinput::xi_get_property(
            connection,
            device.deviceid,
            false,
            atom,
            AtomEnum::ANY.into(),
            0,
            1,
        )
        .map_err(|_| "XTEST identity unavailable")?
        .reply()
        .map_err(|_| "XTEST identity unavailable")?;
        if property.type_ == 0 {
            continue;
        }
        if property.type_ != u32::from(AtomEnum::INTEGER)
            || property.bytes_after != 0
            || property.items.as_data8().map(Vec::as_slice) != Some(&[1])
        {
            return Err("XTEST device marker is invalid".into());
        }
        sources.push(device.deviceid);
    }
    match sources.as_slice() {
        [source] if *source <= 255 => Ok((master, *source)),
        _ => Err("XTEST output source is ambiguous".into()),
    }
}
// Require an unmodified keyboard state before playback
fn neutral_keyboard_state(
    connection: &x11_connection::Connection,
    device: u16,
) -> Result<(), String> {
    keyboard_state(connection, device, 0)
}
// Require the current XKB modifier and group state to match the owned keys
fn keyboard_state(
    connection: &x11_connection::Connection,
    device: u16,
    expected: u8,
) -> Result<(), String> {
    let state = xkb::get_state(connection, device)
        .map_err(|_| "XKB modifier/group state unavailable")?
        .reply()
        .map_err(|_| "XKB modifier/group state unavailable")?;
    if u16::from(state.device_id) != device || !expected_modifier_state(&state, expected) {
        return Err("XKB modifier/group state does not match owned keys".into());
    }
    Ok(())
}
#[cfg(test)]
// Report the neutral state condition for Linux playback
fn neutral_state(state: &xkb::GetStateReply) -> bool {
    expected_modifier_state(state, 0)
}
// Report the expected modifier state condition for Linux playback
fn expected_modifier_state(state: &xkb::GetStateReply, expected: u8) -> bool {
    [
        state.mods,
        state.base_mods,
        state.compat_state,
        state.grab_mods,
        state.compat_grab_mods,
        state.lookup_mods,
        state.compat_lookup_mods,
    ]
    .iter()
    .all(|value| u16::from(*value) == u16::from(expected))
        && u16::from(state.latched_mods) == 0
        && u16::from(state.locked_mods) == 0
        && u8::from(state.group) == 0
        && u8::from(state.locked_group) == 0
        && state.base_group == 0
        && state.latched_group == 0
}
// Read the XKB controls governing key repetition
fn repeat_controls(
    connection: &x11_connection::Connection,
    device: u16,
) -> Result<repeat::X11RepeatControls, String> {
    let controls = xkb::get_controls(connection, device)
        .map_err(|_| "XKB controls unavailable")?
        .reply()
        .map_err(|_| "XKB controls unavailable")?;
    // Do not approximate accessibility transforms, latching or timeout-driven
    // changes. This subset allows only RepeatKeys among boolean controls
    if u16::from(controls.device_id) != device
        || u32::from(controls.enabled_controls) & !u32::from(xkb::BoolCtrl::REPEAT_KEYS) != 0
    {
        return Err("XKB keyboard transformations are unsupported".into());
    }
    Ok(repeat::X11RepeatControls {
        repeat_keys_enabled: u32::from(controls.enabled_controls)
            & u32::from(xkb::BoolCtrl::REPEAT_KEYS)
            != 0,
        per_key_repeat: controls.per_key_repeat,
    })
}
// Admit only uniform momentary modifier actions across every level/group
// XKB's action mask is server-resolved (including UseModMapMods/virtual mods)
// ClearLocks is harmless only because locks/latches are required neutral and
// any observed change revokes admission. StickyKeys and all transformations
// are separately rejected by repeat_controls. Never change shared controls
// https://www.x.org/releases/current/doc/kbproto/xkbproto.html
fn ordinary_key(
    connection: &x11_connection::Connection,
    device: u16,
    code: u8,
) -> Result<u8, String> {
    let map = xkb::get_map(
        connection,
        device,
        xkb::MapPart::default(),
        xkb::MapPart::KEY_ACTIONS | xkb::MapPart::KEY_BEHAVIORS,
        0,
        0,
        0,
        0,
        code,
        1,
        code,
        1,
        xkb::VMod::default(),
        0,
        0,
        0,
        0,
        0,
        0,
    )
    .map_err(|_| "XKB key behavior unavailable")?
    .reply()
    .map_err(|_| "XKB key behavior unavailable")?;
    if u16::from(map.device_id) != device
        || code < map.min_key_code
        || code > map.max_key_code
        || map.total_key_behaviors != 0
    {
        return Err("XKB nondefault key behaviors are unsupported".into());
    }
    let actions = map.map.key_actions.ok_or("XKB key actions unavailable")?;
    if actions.acts_rtrn_acts.len() != usize::from(map.total_actions) {
        return Err("XKB action response is incomplete".into());
    }
    momentary_modifier_mask(&actions.acts_rtrn_acts)
}
// Collect the modifier bits associated with momentary physical modifiers
fn momentary_modifier_mask(actions: &[xkb::Action]) -> Result<u8, String> {
    let mut expected = None;
    for action in actions {
        let action = action.as_setmods();
        let mask = if action.type_ == xkb::SAType::NO_ACTION {
            0
        } else if action.type_ == xkb::SAType::SET_MODS
            && u8::from(action.flags)
                & !(u8::from(xkb::SA::CLEAR_LOCKS) | u8::from(xkb::SA::USE_MOD_MAP_MODS))
                == 0
        {
            u8::try_from(u16::from(action.mask)).map_err(|_| "Invalid XKB modifier mask")?
        } else {
            return Err("XKB latch, lock, group and other stateful actions are unsupported".into());
        };
        if expected.is_some_and(|value| value != mask) {
            return Err("XKB key changes modifier action across levels/groups".into());
        }
        expected = Some(mask);
    }
    Ok(expected.unwrap_or(0))
}
pub(crate) struct NativeBackend {
    owner: String,
    press_guard: Option<crate::keyboard_macro::playback::PressGuard>,
    wayland: bool,
    x11: Option<X11State>,
    portal_generation: usize,
    observer: Option<NativePlaybackObserver>,
    target: Option<Box<dyn crate::keyboard_macro::test_target::TestTarget>>,
    allow_focus_changes: bool,
    session: Option<linux_session::native::Lease>,
    repeat_mode: KeyboardMacroRepeatMode,
}
impl NativeBackend {
    // Initialize Linux playback state from the supplied inputs
    pub(crate) fn new(owner: &str) -> Result<Self, String> {
        Ok(Self {
            owner: owner.into(),
            press_guard: None,
            wayland: crate::builtins::native::wayland(),
            x11: None,
            portal_generation: 0,
            observer: None,
            target: None,
            allow_focus_changes: false,
            session: None,
            repeat_mode: KeyboardMacroRepeatMode::Exact,
        })
    }
}
impl Backend for NativeBackend {
    type Code = Code;
    // Set press guard
    fn set_press_guard(&mut self, guard: crate::keyboard_macro::playback::PressGuard) {
        self.press_guard = Some(guard);
    }
    // Check Linux playback prerequisites before proceeding
    fn preflight(
        &mut self,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
    ) -> Result<(), String> {
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }

        let readiness = crate::keyboard_macro::capture::playback_observer_readiness();
        if !readiness.available {
            return Err(readiness
                .reason
                .unwrap_or_else(|| "Linux physical-input observation is unavailable".into()));
        }
        if self.wayland {
            if playback.repeat_mode != KeyboardMacroRepeatMode::System {
                return Err("Hyprland requires explicit System repeat mode; clients generate repeat independently".into());
            }
            if !crate::wayland::authorized() {
                return Err(
                    "Wayland keyboard permission is unavailable; authorize separately in Settings"
                        .into(),
                );
            }
            self.portal_generation = crate::wayland::keyboard_generation();
        } else {
            let mut state = X11State::connect()?;
            let mut checked = [false; 256];
            repeat::preflight_x11(
                recording,
                playback.repeat_mode,
                &state.master_repeat,
                &state.source_repeat,
                |key| {
                    let code = linux_mapping::resolve_xkb(key, &state.names)?;
                    if !checked[usize::from(code)] {
                        let master_mask = ordinary_key(&state.connection, state.master, code)?;
                        let source_mask = ordinary_key(&state.connection, state.source, code)?;
                        if master_mask != source_mask {
                            return Err("XKB source/master modifier actions differ".into());
                        }
                        state.modifier_masks[usize::from(code)] = master_mask;
                        checked[usize::from(code)] = true;
                    }
                    Ok(code)
                },
            )?;
            state.check_target()?;
            self.x11 = Some(state);
        }
        self.repeat_mode = playback.repeat_mode;
        Ok(())
    }
    // Resolve the supplied Linux playback request
    fn resolve(&mut self, key: PhysicalKey) -> Result<Code, String> {
        if matches!(
            key,
            PhysicalKey::CapsLock
                | PhysicalKey::NumLock
                | PhysicalKey::ScrollLock
                | PhysicalKey::Pause
                | PhysicalKey::PrintScreen
        ) {
            return Err("Lock and special-key state preservation is not qualified".into());
        }
        if self.wayland {
            linux_mapping::evdev_code(key)
                .map(Code::Evdev)
                .ok_or_else(|| "Unsupported Linux physical key".into())
        } else {
            linux_mapping::resolve_xkb(
                key,
                &self
                    .x11
                    .as_ref()
                    .ok_or("X11 physical mapping is not prepared")?
                    .names,
            )
            .map(Code::X11)
        }
    }
    // Begin tracking the Linux playback operation
    fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String> {
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }
        if !self.wayland {
            self.session = Some(linux_session::native::acquire()?);
        }
        self.observer = Some(NativePlaybackObserver::start().map_err(|error| error.to_string())?);
        self.allow_focus_changes = policy == KeyboardMacroFocusPolicy::AllowFocusChanges;
        self.target = Some(if self.wayland {
            crate::keyboard_macro::hyprland::capture_target()?
        } else {
            crate::keyboard_macro::test_target::capture_x11_playback()?
        });
        if policy == KeyboardMacroFocusPolicy::StayOnTarget && !self.wayland {
            self.x11
                .as_mut()
                .ok_or("X11 target observer is unavailable")?
                .watch_target()?;
        }
        self.check()
    }
    // Check the current Linux playback conditions
    fn check(&mut self) -> Result<(), String> {
        self.observer
            .as_ref()
            .ok_or("Linux physical-input observer is unavailable")?
            .check()
            .map_err(|error| error.to_string())?;
        if self.wayland {
            if self.allow_focus_changes {
                if let Some(target) = self.target.as_mut() {
                    target.finish()?;
                }
                self.target = None;
                self.target = Some(crate::keyboard_macro::hyprland::capture_target()?);
            }
            self.target
                .as_mut()
                .ok_or("Wayland target unavailable")?
                .check_focused()?;
            if !crate::wayland::authorized()
                || self.portal_generation != crate::wayland::keyboard_generation()
            {
                return Err("Wayland keyboard authorization changed".into());
            }
        } else {
            if self.allow_focus_changes {
                self.target = Some(crate::keyboard_macro::test_target::capture_x11_playback()?);
            }
            self.target
                .as_mut()
                .ok_or("X11 target identity unavailable")?
                .check_focused()?;
            self.x11
                .as_mut()
                .ok_or("X11 keyboard connection is unavailable")?
                .check_target()?;
        }
        Ok(())
    }
    // Finalize the pending Linux playback operation
    fn finish(&mut self) -> Result<(), String> {
        let capture = self.observer.as_mut().map_or(Ok(()), |observer| {
            observer.stop().map_err(|error| error.to_string())
        });
        self.observer = None;
        let target = self
            .target
            .as_mut()
            .map_or(Ok(()), |target| target.finish());
        self.target = None;
        self.session = None;
        self.x11 = None;
        capture.and(target)
    }
    // Deliver the Linux playback event to its consumer
    fn emit(&mut self, code: &Code, kind: KeyboardMacroEventKind) -> Result<(), String> {
        if repeat::skip_recorded_repeat(self.repeat_mode, kind) {
            // The generic scheduler already waited and verified cancellation,
            // ownership and safety for this event. The server supplies repeat;
            // do not call macro_key_event or modify owned pressed-key state
            return Ok(());
        }
        if kind == KeyboardMacroEventKind::Repeat {
            return Err("The Linux backend cannot faithfully inject explicit repeat events".into());
        }
        // Press admission has a single blocking verification point: check()
        // The generic player checks cancellation AFTER check() and BEFORE emit
        // Do not insert another blocking pre-press verification after that
        // barrier. Cleanup is independent and must still verify the pinned route
        let physical = match code {
            Code::Evdev(code) => linux_mapping::physical_from_evdev(*code),
            Code::X11(code) => self.x11.as_ref().and_then(|state| {
                PhysicalKey::ALL
                    .iter()
                    .copied()
                    .find(|key| linux_mapping::resolve_xkb(*key, &state.names).ok() == Some(*code))
            }),
        };
        if kind == KeyboardMacroEventKind::KeyUp {
            self.observer
                .as_ref()
                .ok_or("Keyboard cleanup observer is unavailable")?
                .prepare_release(physical.ok_or("Keyboard cleanup mapping is unavailable")?)?;
            // The physical-release wait may take time. Revalidate the original
            // route afterwards, never reuse a pre-wait device binding sample
            if let Some(state) = self.x11.as_mut() {
                state.check_state(false)?;
            }
        }
        let owned_code = match code {
            Code::X11(code) => u16::from(*code),
            Code::Evdev(code) => *code,
        };
        let result =
            crate::builtins::native::macro_key_event(&self.owner, owned_code, kind, || {
                if kind != KeyboardMacroEventKind::KeyUp {
                    self.observer
                        .as_ref()
                        .ok_or("Keyboard input observer unavailable")?
                        .admission_ready()?;
                }
                if kind != KeyboardMacroEventKind::KeyUp && !self.wayland {
                    self.session
                        .as_ref()
                        .ok_or("Keyboard session lease unavailable")?
                        .check_current()?;
                }
                if kind == KeyboardMacroEventKind::KeyUp {
                    self.observer
                        .as_ref()
                        .ok_or("Keyboard cleanup observer is unavailable")?
                        .release_ready(
                            physical.ok_or("Keyboard cleanup mapping is unavailable")?,
                        )?;
                }
                let pressed = kind == KeyboardMacroEventKind::KeyDown;
                match code {
                    Code::Evdev(code) if self.wayland => {
                        let observer = self
                            .observer
                            .as_ref()
                            .ok_or("Keyboard input observer unavailable")?;
                        let guard = if pressed {
                            let observed = observer.admission_guard();
                            let canceled = self
                                .press_guard
                                .clone()
                                .ok_or("Queued keyboard cancellation guard unavailable")?;
                            std::sync::Arc::new(move || {
                                canceled()?;
                                observed()
                            })
                        } else {
                            observer.release_guard(
                                physical.ok_or("Keyboard cleanup mapping unavailable")?,
                            )
                        };
                        crate::wayland::physical_key_state(
                            *code,
                            pressed,
                            self.portal_generation,
                            guard,
                        )
                    }
                    Code::X11(code) if !self.wayland => {
                        let state = self
                            .x11
                            .as_ref()
                            .ok_or("X11 keyboard connection is unavailable")?;
                        state
                            .connection
                            .stream()
                            .arm(std::time::Duration::from_millis(75));
                        xtest::fake_input(
                            &state.connection,
                            if pressed {
                                xproto::KEY_PRESS_EVENT
                            } else {
                                xproto::KEY_RELEASE_EVENT
                            },
                            *code,
                            0,
                            0,
                            0,
                            0,
                            0,
                        )
                        .map_err(|_| "X11 physical keyboard output failed")?
                        .check()
                        .map_err(|_| "X11 physical keyboard output failed".into())
                    }
                    _ => Err("Keyboard macro physical mapping belongs to another backend".into()),
                }
            });
        if result.is_ok() {
            if let (Some(state), Code::X11(code)) = (self.x11.as_mut(), code) {
                state.held[usize::from(*code)] = kind == KeyboardMacroEventKind::KeyDown;
            }
        }
        result
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    // Verify that accepts uniform momentary modifier masks only
    fn accepts_uniform_momentary_modifier_masks_only() {
        let shift: xkb::Action = xkb::SASetMods {
            type_: xkb::SAType::SET_MODS,
            flags: xkb::SA::CLEAR_LOCKS | xkb::SA::USE_MOD_MAP_MODS,
            mask: 1u8.into(),
            ..Default::default()
        }
        .into();
        let control: xkb::Action = xkb::SASetMods {
            type_: xkb::SAType::SET_MODS,
            mask: 4u8.into(),
            ..Default::default()
        }
        .into();
        let lock: xkb::Action = xkb::SASetMods {
            type_: xkb::SAType::LOCK_MODS,
            mask: 1u8.into(),
            ..Default::default()
        }
        .into();
        assert_eq!(momentary_modifier_mask(&[shift, shift]), Ok(1));
        assert!(momentary_modifier_mask(&[shift, control]).is_err());
        assert!(momentary_modifier_mask(&[lock]).is_err());
        assert_eq!(momentary_modifier_mask(&[]), Ok(0));
        let state = xkb::GetStateReply {
            mods: 5u8.into(),
            base_mods: 5u8.into(),
            compat_state: 5u8.into(),
            grab_mods: 5u8.into(),
            compat_grab_mods: 5u8.into(),
            lookup_mods: 5u8.into(),
            compat_lookup_mods: 5u8.into(),
            ..Default::default()
        };
        assert!(expected_modifier_state(&state, 5));
        assert!(!expected_modifier_state(&state, 1));
        assert!(!expected_modifier_state(
            &xkb::GetStateReply {
                latched_mods: 1u8.into(),
                ..state
            },
            5
        ));
    }
    #[test]
    // Verify that subset rejects pending or active modifier group state
    fn subset_rejects_pending_or_active_modifier_group_state() {
        let neutral = xkb::GetStateReply::default();
        assert!(neutral_state(&neutral));
        for state in [
            xkb::GetStateReply {
                base_mods: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                latched_mods: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                locked_mods: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                mods: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                latched_group: 1,
                ..Default::default()
            },
            xkb::GetStateReply {
                base_group: -1,
                ..Default::default()
            },
            xkb::GetStateReply {
                locked_group: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                group: 1u8.into(),
                ..Default::default()
            },
            xkb::GetStateReply {
                compat_lookup_mods: 1u8.into(),
                ..Default::default()
            },
        ] {
            assert!(!neutral_state(&state));
        }
    }
    #[test]
    // Verify that cleanup retains original route after admission policy revocation
    fn cleanup_retains_original_route_after_admission_policy_revocation() {
        for (mapping, focus) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(output_admission(mapping, focus, true).is_ok());
            assert_eq!(
                output_admission(mapping, focus, false).is_ok(),
                !mapping && !focus
            );
        }
    }
}
