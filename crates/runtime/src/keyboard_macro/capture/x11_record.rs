//! Scoped RECORD device events, never client-delivered events or application data
//!
//! Xorg AccessXRepeatKeyExpire -> XkbProcessKeyboardEvent -> ProcessOtherEvent
//! invokes DeviceEventCallback; RecordADeviceEvent exports XI1 events with the
//! originating device ID. This includes repeats omitted from XI2 raw input
//! Core/master duplicates and XTEST sources are discarded. No sequence-number
//! repeat hack is used: only a second processed down on the same held slave key
//!
//! The sole recorded request is our own control client's GetInputFocus, a
//! content-free heartbeat/barrier. Each bounded drain ends at that marker, so
//! shutdown does not wait indefinitely for the next user key
//! https://github.com/XQuartz/xorg-server/blob/master/record/record.c
//! https://github.com/XQuartz/xorg-server/blob/master/xkb/xkbAccessX.c
use super::*;
use x11rb::{cookie::RecordEnableContextCookie, protocol::record, x11_utils::TryParse};

// Report whether X11 record is ready
pub(super) fn readiness() -> Result<(), String> {
    let (connection, _) = x11_connection::connect()?;
    let version = record::query_version(&connection, 1, 13)
        .map_err(|_| "X11 RECORD is unavailable")?
        .reply()
        .map_err(|_| "X11 RECORD is unavailable")?;
    if (version.major_version, version.minor_version) < (1, 13) {
        return Err("X11 recording requires RECORD 1.13".into());
    }
    Ok(())
}

// Drive the X11 record operation
pub(super) fn run(
    control: &XConnection,
    mut state: State,
    master: u16,
    sink: &mut Sink,
    lease: session::Lease,
) -> Result<(), CaptureError> {
    if master >= 128 || !state.supports_record_ids() {
        return Err(CaptureError::RawInputUnavailable);
    }
    let extension = control
        .query_extension(b"XInputExtension")
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?;
    if !extension.present {
        return Err(CaptureError::RawInputUnavailable);
    }
    let first = extension
        .first_event
        .checked_add(xinput::DEVICE_KEY_PRESS_EVENT)
        .ok_or(CaptureError::RawInputUnavailable)?;
    let last = extension
        .first_event
        .checked_add(xinput::DEVICE_KEY_RELEASE_EVENT)
        .filter(|last| *last < 128)
        .ok_or(CaptureError::RawInputUnavailable)?;
    let (data, _) = x11_connection::connect().map_err(|_| CaptureError::RawInputUnavailable)?;
    record::query_version(&data, 1, 13)
        .map_err(|_| CaptureError::RawInputUnavailable)?
        .reply()
        .map_err(|_| CaptureError::RawInputUnavailable)?;
    let context = control
        .generate_id()
        .map_err(|_| CaptureError::RawInputUnavailable)?;
    let client = control.setup().resource_id_base;
    record::create_context(
        control,
        context,
        record::ElementHeader::default(),
        &[client],
        &[record::Range {
            device_events: record::Range8 { first, last },
            core_requests: record::Range8 {
                first: 43,
                last: 43,
            },
            ..Default::default()
        }],
    )
    .map_err(|_| CaptureError::RawInputUnavailable)?
    .check()
    .map_err(|_| CaptureError::RawInputUnavailable)?;
    let mut replies =
        record::enable_context(&data, context).map_err(|_| CaptureError::RawInputUnavailable)?;
    let start = replies
        .next()
        .ok_or(CaptureError::HookDisabled)?
        .map_err(|_| CaptureError::HookDisabled)?;
    if start.category != 4 || !start.data.is_empty() {
        return Err(CaptureError::InputMismatch);
    }
    let mut last_time = 0;
    // Subscribe before the held-key snapshot. Drain to our explicit request
    // marker before ready; any input in the startup interval rejects startup
    control.stream().arm(Duration::from_millis(75));
    let keys = control
        .query_keymap()
        .map_err(|_| CaptureError::HookDisabled)?
        .reply()
        .map_err(|_| CaptureError::HookDisabled)?
        .keys;
    if keys.iter().any(|byte| *byte != 0) {
        return Err(CaptureError::InitiallyHeldKeys);
    }
    drain(control, &mut state, sink, false)?;
    record_drain(
        control,
        &data,
        &mut replies,
        &mut state,
        master,
        first,
        client,
        start.server_time,
        &mut last_time,
        sink,
        false,
        &lease,
    )?;
    lease
        .check()
        .map_err(|_| CaptureError::SessionStateUnavailable)?;
    sink.signal.strict_health.store(true, Ordering::Release);
    let now = sink.signal.elapsed_ms();
    sink.signal.worker_heartbeat.store(now, Ordering::Release);
    sink.signal.hotkey_heartbeat.store(now, Ordering::Release);
    sink.signal.ready.store(true, Ordering::Release);
    while !sink.expired() {
        let request = sink.signal.verification_requested.load(Ordering::Acquire);
        control.stream().arm(Duration::from_millis(75));
        drain(control, &mut state, sink, true)?;
        lease
            .check_current()
            .map_err(|_| CaptureError::SessionStateUnavailable)?;
        record_drain(
            control,
            &data,
            &mut replies,
            &mut state,
            master,
            first,
            client,
            start.server_time,
            &mut last_time,
            sink,
            true,
            &lease,
        )?;
        if request != sink.signal.verified.load(Ordering::Acquire) {
            lease
                .check()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            // Events can arrive during the session barrier; include them before ACK
            record_drain(
                control,
                &data,
                &mut replies,
                &mut state,
                master,
                first,
                client,
                start.server_time,
                &mut last_time,
                sink,
                true,
                &lease,
            )?;
        }
        lease
            .check_current()
            .map_err(|_| CaptureError::SessionStateUnavailable)?;
        let now = sink.signal.elapsed_ms();
        sink.signal.worker_heartbeat.store(now, Ordering::Release);
        sink.signal.hotkey_heartbeat.store(now, Ordering::Release);
        sink.signal.verified.store(request, Ordering::Release);
        std::thread::sleep(Duration::from_millis(5));
    }
    control.stream().arm(Duration::from_millis(75));
    record::disable_context(control, context)
        .map_err(|_| CaptureError::HookDisabled)?
        .check()
        .map_err(|_| CaptureError::HookDisabled)?;
    // Stop completes at native server detach, not at UI-click time. RECORD
    // queues before EndOfData belong to that capture; preserve them even though
    // the stop flag is already set. The control barrier also supplies raw edge
    // provenance for every event recorded before DisableContext took effect
    control
        .get_input_focus()
        .map_err(|_| CaptureError::HookDisabled)?
        .reply()
        .map_err(|_| CaptureError::HookDisabled)?;
    drain(control, &mut state, sink, true)?;
    data.stream().arm(Duration::from_millis(75));
    let mut events = 0;
    let mut ended = false;
    for _ in 0..=1024 {
        let Some(reply) = replies.next() else {
            ended = true;
            break;
        };
        let reply = reply.map_err(|_| CaptureError::HookDisabled)?;
        lease
            .check_current()
            .map_err(|_| CaptureError::SessionStateUnavailable)?;
        process_data(
            &reply,
            &mut state,
            master,
            first,
            start.server_time,
            &mut last_time,
            sink,
            true,
            true,
            &lease,
            &mut events,
        )?;
    }
    if !ended {
        return Err(CaptureError::BufferOverflow);
    }
    drop(replies);
    drop(data);
    record::free_context(control, context)
        .map_err(|_| CaptureError::HookDisabled)?
        .check()
        .map_err(|_| CaptureError::HookDisabled)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
// Record drain
fn record_drain(
    control: &XConnection,
    data: &XConnection,
    replies: &mut RecordEnableContextCookie<'_, XConnection>,
    state: &mut State,
    master: u16,
    press: u8,
    client: u32,
    origin: u32,
    last_time: &mut u32,
    sink: &mut Sink,
    admitting: bool,
    lease: &session::Lease,
) -> Result<(), CaptureError> {
    control.stream().arm(Duration::from_millis(75));
    data.stream().arm(Duration::from_millis(75));
    control
        .get_input_focus()
        .map_err(|_| CaptureError::HookDisabled)?
        .reply()
        .map_err(|_| CaptureError::HookDisabled)?;
    // The control reply is a barrier: all raw events preceding the recorded
    // heartbeat are now available, even if RECORD replies arrived first
    drain(control, state, sink, admitting)?;
    let mut events = 0;
    for _ in 0..=1024 {
        lease
            .check_current()
            .map_err(|_| CaptureError::SessionStateUnavailable)?;
        let reply = replies
            .next()
            .ok_or(CaptureError::HookDisabled)?
            .map_err(|_| CaptureError::HookDisabled)?;
        if reply.client_swapped || reply.element_header != record::ElementHeader::default() {
            return Err(CaptureError::InputMismatch);
        }
        if reply.category == 1 && reply.xid_base == client {
            if reply.data.len() == 4 && reply.data[0] == 43 {
                state.check_record_lag(reply.server_time)?;
                return Ok(());
            }
            return Err(CaptureError::InputMismatch);
        }
        process_data(
            &reply,
            state,
            master,
            press,
            origin,
            last_time,
            sink,
            admitting,
            true,
            lease,
            &mut events,
        )?;
    }
    Err(CaptureError::BufferOverflow)
}

#[allow(clippy::too_many_arguments)]
// Decode recorded X11 input and forward observations to the capture sink
fn process_data(
    reply: &record::EnableContextReply,
    state: &mut State,
    master: u16,
    press: u8,
    origin: u32,
    last_time: &mut u32,
    sink: &mut Sink,
    admitting: bool,
    draining: bool,
    lease: &session::Lease,
    events: &mut usize,
) -> Result<(), CaptureError> {
    validate_device_packet(reply)?;
    for bytes in reply.data.as_chunks::<32>().0 {
        *events += 1;
        if *events > 1024 {
            return Err(CaptureError::BufferOverflow);
        }
        let event = xinput::DeviceKeyPressEvent::try_parse(bytes)
            .map_err(|_| CaptureError::InputMismatch)?
            .0;
        if event.response_type != press && event.response_type != press + 1 {
            return Err(CaptureError::InputMismatch);
        }
        // Xorg RecordSendProtocolEvents filters each of the converted XI1
        // events separately against pDeviceEventSet, so the unselected
        // DeviceValuator companion is not serialized even with MORE_EVENTS
        // The flag is not part of the 7-bit slave ID
        let source = u16::from(event.device_id & 0x7f);
        if source == master {
            continue;
        }
        let elapsed = event.time.wrapping_sub(origin);
        if elapsed < *last_time {
            return Err(CaptureError::InputMismatch);
        }
        if u64::from(elapsed) > super::super::MAX_DURATION_MS {
            return Err(CaptureError::SessionExpired);
        }
        *last_time = elapsed;
        let down = event.response_type == press;
        if let Some((key, repeat)) = state.recorded_event(source, event.detail, down, event.time)? {
            if !admitting {
                return Err(CaptureError::InitiallyHeldKeys);
            }
            lease
                .check_current()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            // An emergency chord is the explicit event cutoff, unlike ordinary
            // asynchronous Stop which retires at the server detach boundary
            if sink.signal.error() != Some(CaptureError::EmergencyStop) {
                sink.push_at(Some(key), down, repeat, u64::from(elapsed), draining);
            }
        }
    }
    Ok(())
}

// Validate device packet
fn validate_device_packet(reply: &record::EnableContextReply) -> Result<(), CaptureError> {
    if reply.client_swapped
        || reply.element_header != record::ElementHeader::default()
        || reply.category != 0
        || reply.xid_base != 0
        || !reply.data.len().is_multiple_of(32)
        || reply.data.len() > 32 * 1024
    {
        Err(CaptureError::InputMismatch)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that record device packets are scoped native order fixed size and bounded
    fn record_device_packets_are_scoped_native_order_fixed_size_and_bounded() {
        let valid = record::EnableContextReply {
            data: vec![0; 32],
            ..Default::default()
        };
        assert!(validate_device_packet(&valid).is_ok());
        for invalid in [
            record::EnableContextReply {
                client_swapped: true,
                ..valid.clone()
            },
            record::EnableContextReply {
                xid_base: 1,
                ..valid.clone()
            },
            record::EnableContextReply {
                category: 1,
                ..valid.clone()
            },
            record::EnableContextReply {
                element_header: 1u8,
                ..valid.clone()
            },
            record::EnableContextReply {
                data: vec![0; 31],
                ..valid.clone()
            },
            record::EnableContextReply {
                data: vec![0; 32 * 1025],
                ..valid
            },
        ] {
            assert_eq!(
                validate_device_packet(&invalid),
                Err(CaptureError::InputMismatch)
            );
        }
    }
}
