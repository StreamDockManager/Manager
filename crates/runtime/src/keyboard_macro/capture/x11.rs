//! Scoped XI2.1 physical observation and RECORD processed recording. No grabs,
//! XWayland fallback, evdev access, or shared keyboard settings are used
//! XTEST is identified by the server property, never by a device display name
//! Every physical keyboard uses its own checked XKB names. Mapping, hierarchy,
//! source-property changes, queue saturation, or connection failure invalidate the
//! session. XI2.1 delivers raw events during grabs, so the emergency chord stays
//! passive and cannot be silently unregistered like an application-level hotkey
//!
//! A generation-pinned KDE/logind lease protects each active transaction
//! RECORD supplies processed physical-slave key events including server repeats;
//! XI2 alone does not. Capability polling does not subscribe to keyboard events
//! Sources: https://www.x.org/releases/current/doc/inputproto/XI2proto.txt
//! https://www.x.org/releases/current/doc/kbproto/xkbproto.html
//! https://github.com/XQuartz/xorg-server/blob/master/include/xserver-properties.h
//! https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html
use super::{
    x11_state::{self, Device, SourceKind, State},
    CaptureError, CaptureHandle, CaptureReadiness, Purpose, Sink,
};
use crate::keyboard_macro::platform::{
    linux_session::native as session,
    x11_connection::{self, Connection as XConnection},
};
use std::{sync::atomic::Ordering, time::Duration};
use x11rb::{
    connection::Connection,
    protocol::{
        xinput::{self, DeviceType, XIEventMask},
        xkb,
        xproto::{AtomEnum, ConnectionExt},
        Event,
    },
};

// Report the wayland condition for X11
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
}
#[path = "x11_record.rs"]
mod recording;
// Report whether X11 is ready
pub(super) fn readiness() -> CaptureReadiness {
    let mut status = observer_readiness();
    if status.available {
        if let Err(reason) = recording::readiness() {
            status.available = false;
            status.reason = Some(reason);
        }
    }
    status
}
// Check X11 prerequisites for observing interference during playback
pub(super) fn observer_readiness() -> CaptureReadiness {
    if wayland() {
        return CaptureReadiness {
            available: false,
            implemented: false,
            reason: Some(
                crate::keyboard_macro::platform::wayland_limits::recording_unavailable_reason()
                    .into(),
            ),
        };
    }
    if std::env::var_os("DISPLAY").is_none() {
        return CaptureReadiness { available: false, implemented: false,
        reason: Some("Global keyboard recording needs an interactive desktop. Headless and CLI sessions support validation and simulation only.".into()) };
    }
    let status = session::readiness();
    CaptureReadiness {
        available: status.is_ok(),
        implemented: true,
        reason: status.err(),
    }
}
// Start the X11 operation
pub(super) fn start(
    purpose: Purpose,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<CaptureHandle, CaptureError> {
    if wayland() || std::env::var_os("DISPLAY").is_none() {
        return Err(CaptureError::UnsupportedPlatform);
    }
    // Acquire a generation-pinned session lease before subscribing to input
    // A later lock/unlock never rearms this active transaction
    let lease = session::acquire().map_err(|_| CaptureError::SessionStateUnavailable)?;
    let (mut sink, mut handle) = Sink::channel_with_stop(purpose, stop);
    let signal = sink.signal.clone();
    handle.thread = Some(
        std::thread::Builder::new()
            .name("keyboard-x11-observer".into())
            .spawn(move || {
                if sink.stopped() {
                    return;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&mut sink, lease)
                }));
                match result {
                    Ok(Ok(())) => (),
                    Ok(Err(error)) => signal.fail(error),
                    Err(_) => signal.fail_cleanup(CaptureError::WorkerFailed),
                }
                sink.finish();
                signal.completed.store(true, Ordering::Release);
            })
            .map_err(|_| CaptureError::WorkerFailed)?,
    );
    Ok(handle)
}
// Look up the X11 atom needed for capture state
fn atom(connection: &XConnection, name: &[u8]) -> Result<u32, CaptureError> {
    let atom = connection
        .intern_atom(true, name)
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .atom;
    if atom == 0 {
        Err(CaptureError::RawInputUnavailable)
    } else {
        Ok(atom)
    }
}
// Read the requested X11 window property
fn property(
    connection: &XConnection,
    device: u16,
    atom: u32,
    expected: AtomEnum,
    length: u32,
) -> Result<Option<Vec<u8>>, CaptureError> {
    let value = xinput::xi_get_property(
        connection,
        device,
        false,
        atom,
        AtomEnum::ANY.into(),
        0,
        length,
    )
    .map_err(|_| CaptureError::RawInputUnavailable)?
    .reply()
    .map_err(|_| CaptureError::RawInputUnavailable)?;
    if value.type_ == 0 {
        return Ok(None);
    }
    if value.type_ != u32::from(expected) || value.bytes_after != 0 {
        return Err(CaptureError::RawInputUnavailable);
    }
    value
        .items
        .as_data8()
        .cloned()
        .map(Some)
        .ok_or(CaptureError::RawInputUnavailable)
}
// Build the physical-key mapping used by capture
fn mapping(
    connection: &XConnection,
    device: u16,
) -> Result<[Option<super::PhysicalKey>; 256], CaptureError> {
    if device > u16::from(u8::MAX) {
        return Err(CaptureError::UnsupportedKey);
    }
    // Subscribe before reading names. Even a change-and-return remains queued
    xkb::select_events(
        connection,
        device,
        xkb::EventType::default(),
        xkb::EventType::NEW_KEYBOARD_NOTIFY
            | xkb::EventType::MAP_NOTIFY
            | xkb::EventType::NAMES_NOTIFY
            | xkb::EventType::COMPAT_MAP_NOTIFY
            | xkb::EventType::CONTROLS_NOTIFY,
        xkb::MapPart::from(0xffu16),
        xkb::MapPart::from(0xffu16),
        &xkb::SelectEventsAux::default(),
    )
    .map_err(|_| CaptureError::RawInputUnavailable)?
    .check()
    .map_err(|_| CaptureError::RawInputUnavailable)?;
    let reply = xkb::get_names(connection, device, xkb::NameDetail::KEY_NAMES)
        .map_err(|_| CaptureError::UnsupportedKey)?
        .reply()
        .map_err(|_| CaptureError::UnsupportedKey)?;
    if u16::from(reply.device_id) != device {
        return Err(CaptureError::UnsupportedKey);
    }
    let mut names = Vec::new();
    for (index, key) in reply
        .value_list
        .key_names
        .ok_or(CaptureError::UnsupportedKey)?
        .into_iter()
        .enumerate()
    {
        let code = usize::from(reply.first_key)
            .checked_add(index)
            .and_then(|code| u8::try_from(code).ok())
            .ok_or(CaptureError::UnsupportedKey)?;
        names.push((code, key.name));
    }
    x11_state::key_map(&names)
}
// Prepare the X11 operation
fn prepare(connection: &XConnection, root: u32) -> Result<(State, u16), CaptureError> {
    let version = xinput::xi_query_version(connection, 2, 1)
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?;
    if (version.major_version, version.minor_version) < (2, 1) {
        return Err(CaptureError::RawInputUnavailable);
    }
    if !xkb::use_extension(connection, 1, 0)
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .supported
    {
        return Err(CaptureError::RawInputUnavailable);
    }
    // Inventory/property changes must be subscribed before any inventory snapshot
    xinput::xi_select_events(
        connection,
        root,
        &[xinput::EventMask {
            deviceid: 0,
            mask: vec![
                XIEventMask::HIERARCHY | XIEventMask::DEVICE_CHANGED | XIEventMask::PROPERTY,
            ],
        }],
    )
    .map_err(|_| CaptureError::RawInputUnavailable)?
    .check()
    .map_err(|_| CaptureError::RawInputUnavailable)?;
    let inventory = xinput::xi_query_device(connection, 0u16)
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .infos;
    if inventory.len() > x11_state::MAX_DEVICES {
        return Err(CaptureError::RawInputUnavailable);
    }
    let masters: Vec<_> = inventory
        .iter()
        .filter(|device| device.enabled && device.type_ == DeviceType::MASTER_KEYBOARD)
        .collect();
    if masters.len() != 1 {
        return Err(CaptureError::RawInputUnavailable);
    }
    let master = masters[0].deviceid;
    let xtest = atom(connection, b"XTEST Device")?;
    let node = atom(connection, b"Device Node")?;
    let mut devices = Vec::new();
    for device in inventory.iter().filter(|device| {
        device.enabled
            && device
                .classes
                .iter()
                .any(|class| class.data.as_key().is_some())
    }) {
        if device.type_ == DeviceType::MASTER_KEYBOARD {
            continue;
        }
        if device.type_ != DeviceType::SLAVE_KEYBOARD || device.attachment != master {
            return Err(CaptureError::RawInputUnavailable);
        }
        let marker = property(connection, device.deviceid, xtest, AtomEnum::INTEGER, 1)?;
        let source_node = if marker.is_none() {
            property(connection, device.deviceid, node, AtomEnum::STRING, 64)?
        } else {
            None
        };
        let kind = x11_state::classify_source(marker.as_deref(), source_node.as_deref())?;
        let keys = if kind == SourceKind::Physical {
            mapping(connection, device.deviceid)?
        } else {
            [None; 256]
        };
        devices.push(Device::new(device.deviceid, kind, keys));
    }
    if !devices
        .iter()
        .any(|device| device.kind == SourceKind::Physical)
    {
        return Err(CaptureError::RawInputUnavailable);
    }
    xinput::xi_select_events(
        connection,
        root,
        &[xinput::EventMask {
            deviceid: master,
            mask: vec![XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE],
        }],
    )
    .map_err(|_| CaptureError::RawInputUnavailable)?
    .check()
    .map_err(|_| CaptureError::RawInputUnavailable)?;
    let keys = connection
        .query_keymap()
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .keys;
    if keys.iter().any(|byte| *byte != 0) {
        return Err(CaptureError::InitiallyHeldKeys);
    }
    Ok((State::new(devices)?, master))
}
// Drain pending X11 entries
fn drain(
    connection: &XConnection,
    state: &mut State,
    sink: &mut Sink,
    admitting: bool,
) -> Result<(), CaptureError> {
    for count in 0..=1024 {
        let Some(event) = connection
            .poll_for_event()
            .map_err(|_| CaptureError::HookDisabled)?
        else {
            return Ok(());
        };
        if count == 1024 {
            return Err(CaptureError::BufferOverflow);
        }
        let (event, down) = match event {
            Event::XinputRawKeyPress(event) => (event, true),
            Event::XinputRawKeyRelease(event) => (event, false),
            Event::XinputDeviceChanged(event)
                if event.reason == xinput::ChangeReason::SLAVE_SWITCH
                    && state.known_source(event.sourceid)
                    && (sink.purpose == Purpose::Recording
                        || state.synthetic_source(event.sourceid)) =>
            {
                continue
            }
            Event::XinputDeviceChanged(_) => return Err(CaptureError::DeviceChanged),
            Event::XinputHierarchy(_)
            | Event::XinputProperty(_)
            | Event::XkbMapNotify(_)
            | Event::XkbNamesNotify(_)
            | Event::XkbNewKeyboardNotify(_)
            | Event::XkbCompatMapNotify(_)
            | Event::XkbControlsNotify(_)
            | Event::MappingNotify(_) => {
                return Err(CaptureError::DeviceChanged);
            }
            Event::Error(_) => return Err(CaptureError::HookDisabled),
            _ => continue,
        };
        if event.response_type & 0x80 != 0 {
            return Err(CaptureError::InputMismatch);
        }
        if sink.purpose == Purpose::Recording {
            // Processed RECORD events supply authoritative repeats and timing
            // XI2 remains subscribed for device/map invalidation, not duplicate keys
            if !state.known_source(event.sourceid) {
                return Err(CaptureError::DeviceChanged);
            }
            if !admitting {
                return Err(CaptureError::InitiallyHeldKeys);
            }
            state.recording_raw(event.sourceid, event.detail, down, event.time)?;
            continue;
        }
        let repeat = event.flags.contains(xinput::KeyEventFlags::KEY_REPEAT);
        if let Some(key) = state.event(event.sourceid, event.detail, down, repeat)? {
            if !admitting {
                return Err(CaptureError::InitiallyHeldKeys);
            }
            sink.push(Some(key), down, repeat);
            if sink.stopped() {
                return Ok(());
            }
        }
    }
    Err(CaptureError::BufferOverflow)
}
// Drive the X11 operation
fn run(sink: &mut Sink, lease: session::Lease) -> Result<(), CaptureError> {
    lease
        .check()
        .map_err(|_| CaptureError::SessionStateUnavailable)?;
    if sink.purpose == Purpose::Recording {
        let countdown = std::time::Instant::now();
        while countdown.elapsed() < Duration::from_secs(3) {
            if sink.stopped() {
                return Ok(());
            }
            lease
                .check_current()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        sink.started = std::time::Instant::now();
    }
    let (connection, screen) =
        x11_connection::connect().map_err(|_| CaptureError::RawInputUnavailable)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or(CaptureError::RawInputUnavailable)?
        .root;
    let (mut state, master) = prepare(&connection, root)?;
    if sink.purpose == Purpose::Recording {
        return recording::run(&connection, state, master, sink, lease);
    }
    // A reply barrier followed by draining closes the initial snapshot race
    connection
        .get_input_focus()
        .map_err(|_| CaptureError::HookDisabled)?
        .reply()
        .map_err(|_| CaptureError::HookDisabled)?;
    drain(&connection, &mut state, sink, false)?;
    lease
        .check()
        .map_err(|_| CaptureError::SessionStateUnavailable)?;
    sink.signal.strict_health.store(true, Ordering::Release);
    sink.signal
        .worker_heartbeat
        .store(sink.signal.elapsed_ms(), Ordering::Release);
    sink.signal
        .hotkey_heartbeat
        .store(sink.signal.elapsed_ms(), Ordering::Release);
    sink.signal.ready.store(true, Ordering::Release);
    while !sink.expired() {
        let request = sink.signal.verification_requested.load(Ordering::Acquire);
        connection.stream().arm(Duration::from_millis(75));
        // This server acknowledgement checks connection health. XI2 subscriptions
        // have connection lifetime; they cannot be silently removed by a timeout
        connection
            .get_input_focus()
            .map_err(|_| CaptureError::HookDisabled)?
            .reply()
            .map_err(|_| CaptureError::HookDisabled)?;
        drain(&connection, &mut state, sink, true)?;
        if request != sink.signal.verified.load(Ordering::Acquire) {
            lease
                .check()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            // Input may have arrived while the D-Bus freshness barrier waited
            // Acknowledge only after a new X11 barrier and drain
            connection.stream().arm(Duration::from_millis(75));
            connection
                .get_input_focus()
                .map_err(|_| CaptureError::HookDisabled)?
                .reply()
                .map_err(|_| CaptureError::HookDisabled)?;
            drain(&connection, &mut state, sink, true)?;
            lease
                .check_current()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
        } else {
            lease
                .check_current()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
        }
        let now = sink.signal.elapsed_ms();
        sink.signal.worker_heartbeat.store(now, Ordering::Release);
        sink.signal.hotkey_heartbeat.store(now, Ordering::Release);
        sink.signal.verified.store(request, Ordering::Release);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Dropping this private connection removes all scoped selections. No global
    // settings, keyboard grabs, device nodes or input permissions were modified
    drop(connection);
    Ok(())
}
