//! Scoped compositor physical edges, never application-generated repeat counts
use super::*;
use crate::keyboard_macro::{hyprland::Client, platform::linux_mapping};
use std::time::Duration;
// Report whether hyprland is ready
pub(super) fn readiness() -> CaptureReadiness {
    let result = crate::keyboard_macro::hyprland::readiness();
    CaptureReadiness {
        available: result.is_ok(),
        implemented: true,
        reason: result.err(),
    }
}
// Start the hyprland operation
pub(super) fn start(
    purpose: Purpose,
    stop: Arc<AtomicBool>,
) -> Result<CaptureHandle, CaptureError> {
    let (mut sink, mut handle) = Sink::channel_with_stop(purpose, stop);
    let signal = sink.signal.clone();
    handle.thread = Some(
        std::thread::Builder::new()
            .name("keyboard-hyprland-observer".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&mut sink)));
                match result {
                    Ok(Ok(())) => (),
                    Ok(Err(error)) => signal.fail_cleanup(error),
                    Err(_) => signal.fail_cleanup(CaptureError::WorkerFailed),
                }
                sink.finish();
                signal.completed.store(true, Ordering::Release);
            })
            .map_err(|_| CaptureError::WorkerFailed)?,
    );
    Ok(handle)
}
// Drive the hyprland operation
fn run(sink: &mut Sink) -> Result<(), CaptureError> {
    let mut client = Client::connect().map_err(|_| CaptureError::RawInputUnavailable)?;
    if sink.purpose == Purpose::Recording {
        let countdown = Instant::now();
        while countdown.elapsed() < Duration::from_secs(3) {
            if sink.stopped() {
                return Ok(());
            }
            client
                .request("hello", None)
                .and_then(|s| s.admitted())
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    if sink.stopped() {
        return Ok(());
    }
    let initial = client
        .request(
            "start",
            Some(if sink.purpose == Purpose::Recording {
                "record"
            } else {
                "observe"
            }),
        )
        .map_err(|_| CaptureError::RawInputUnavailable)?;
    initial
        .admitted()
        .map_err(|_| CaptureError::SessionStateUnavailable)?;
    if !initial.held.is_empty() {
        return Err(CaptureError::InitiallyHeldKeys);
    }
    sink.started = Instant::now();
    sink.signal.strict_health.store(true, Ordering::Release);
    let mut previous_time = 0;
    let result = (|| {
        while !sink.expired() {
            let request = sink.signal.verification_requested.load(Ordering::Acquire);
            let state = client
                .request("poll", None)
                .map_err(|_| CaptureError::WorkerUnresponsive)?;
            state
                .admitted()
                .map_err(|_| CaptureError::SessionStateUnavailable)?;
            if state.interference && sink.purpose == Purpose::PlaybackObserver {
                sink.signal.fail(CaptureError::PhysicalInterference);
            }
            for (code, pressed, elapsed) in state.events {
                if elapsed < previous_time {
                    return Err(CaptureError::InputMismatch);
                }
                previous_time = elapsed;
                sink.push_at(
                    linux_mapping::physical_from_evdev(code),
                    pressed,
                    false,
                    elapsed,
                    false,
                );
            }
            let now = sink.signal.elapsed_ms();
            sink.signal.worker_heartbeat.store(now, Ordering::Release);
            sink.signal.hotkey_heartbeat.store(now, Ordering::Release);
            sink.signal.verified.store(request, Ordering::Release);
            sink.signal.ready.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    })();
    // STOP unsubscribes before responding; disconnect is the emergency teardown
    // Completion cannot be reported before either acknowledged teardown or close
    let final_state = client.stop().map_err(|_| CaptureError::HookDisabled);
    if let Ok(state) = &final_state {
        if state.admitted().is_err() {
            drop(client);
            return Err(CaptureError::SessionStateUnavailable);
        }
        if result.is_ok() {
            for &(code, pressed, elapsed) in &state.events {
                if elapsed < previous_time {
                    drop(client);
                    return Err(CaptureError::InputMismatch);
                }
                previous_time = elapsed;
                sink.push_at(
                    linux_mapping::physical_from_evdev(code),
                    pressed,
                    false,
                    elapsed,
                    true,
                );
            }
        }
    }
    drop(client);
    result.and(final_state.map(|_| ()))
}
