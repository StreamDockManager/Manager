//! Connection-local HID readiness with autonomous bounded-poll fallback
//!
//! Supported backends signal the existing input handle without a second report consumer
use std::time::Duration;

pub(super) struct PollPolicy {
    pub driver_timeout_ms: i32,
    pub idle_delay: Option<Duration>,
    pub reason: &'static str,
}

// Preserve the bounded path whenever native registration is unavailable
pub(super) fn policy(shared_handle: bool) -> PollPolicy {
    PollPolicy {
        driver_timeout_ms: if shared_handle { 0 } else { 5 },
        idle_delay: shared_handle.then_some(Duration::from_millis(5)),
        reason: "native HID readiness registration is unavailable",
    }
}

// Each connection owns one signal; no old callback can wake a replacement connection
#[derive(Default)]
pub(super) struct Signal {
    state: std::sync::Mutex<(u64, bool)>,
    changed: std::sync::Condvar,
}
impl Signal {
    // Snapshot before draining commands/reports so an intervening callback cannot be lost
    pub(super) fn generation(&self) -> u64 {
        self.state.lock().unwrap().0
    }
    // Coalesce producer notifications without touching the HID or output mutex
    pub(super) fn notify(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = state.0.wrapping_add(1);
        self.changed.notify_all();
    }
    // Publish sticky cancellation before waking an idle input worker
    pub(super) fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
    // Park only after an empty nonblocking read has released the shared HID mutex
    pub(super) fn wait(&self, observed: u64) {
        let mut state = self.state.lock().unwrap();
        while state.0 == observed && !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(super) struct Registration(std::sync::Arc<std::sync::Mutex<hidapi::HidDevice>>);
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Registration {
    // Register on the already-open handle without installing a second report consumer
    pub(super) fn install(
        hid: std::sync::Arc<std::sync::Mutex<hidapi::HidDevice>>,
        signal: std::sync::Arc<Signal>,
    ) -> Result<Self, hidapi::HidError> {
        hid.lock()
            .unwrap()
            .set_input_ready_callback(Some(Box::new(move || signal.notify())))?;
        Ok(Self(hid))
    }
}
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Drop for Registration {
    // Synchronous unregistration completes before either captured signal or HID handle can retire
    fn drop(&mut self) {
        let _ = self.0.lock().unwrap().set_input_ready_callback(None);
    }
}

// Exercise native cancellation and report ordering with isolated descriptors instead of hardware
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod candidate {
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    #[repr(C)]
    struct PollFd {
        fd: std::ffi::c_int,
        events: i16,
        revents: i16,
    }
    #[cfg(target_os = "macos")]
    type Count = u32;
    #[cfg(target_os = "linux")]
    type Count = std::ffi::c_ulong;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, count: Count, timeout: std::ffi::c_int) -> std::ffi::c_int;
    }
    const INPUT: i16 = 1;
    const FAILURE: i16 = 8 | 16 | 32;

    #[derive(Default)]
    struct State {
        stopped: bool,
        firmware: bool,
    }
    struct Shared {
        epoch: u64,
        state: Mutex<State>,
        notify: Mutex<UnixStream>,
    }
    #[derive(Clone)]
    struct Wake(Arc<Shared>);
    struct Waiter {
        shared: Arc<Shared>,
        receiver: UnixStream,
    }
    #[derive(Debug, PartialEq)]
    enum Event {
        Input,
        Firmware,
        Stopped,
    }

    // Create a connection-local native cancellation descriptor without touching hardware
    fn channel(epoch: u64) -> io::Result<(Wake, Waiter)> {
        let (tx, rx) = UnixStream::pair()?;
        tx.set_nonblocking(true)?;
        rx.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            epoch,
            state: Mutex::new(State::default()),
            notify: Mutex::new(tx),
        });
        Ok((
            Wake(shared.clone()),
            Waiter {
                shared,
                receiver: rx,
            },
        ))
    }
    impl Wake {
        // Publish sticky cancellation or a coalesced firmware request before waking the poll
        fn signal(&self, epoch: u64, stop: bool) {
            if epoch != self.0.epoch {
                return;
            }
            {
                let mut state = self.0.state.lock().unwrap();
                if stop {
                    state.stopped = true;
                } else {
                    state.firmware = true;
                }
            }
            // A full pipe already carries a wake, so no producer blocks behind its consumer
            let _ = self.0.notify.lock().unwrap().write(&[1]);
        }
    }
    impl Waiter {
        // Wait outside the output mutex and prioritize stop/firmware over the next report batch
        fn wait(&mut self, input: BorrowedFd<'_>) -> io::Result<Event> {
            loop {
                {
                    let mut state = self.shared.state.lock().unwrap();
                    if state.stopped {
                        return Ok(Event::Stopped);
                    }
                    if state.firmware {
                        state.firmware = false;
                        return Ok(Event::Firmware);
                    }
                }
                let mut fds = [
                    PollFd {
                        fd: input.as_raw_fd(),
                        events: INPUT,
                        revents: 0,
                    },
                    PollFd {
                        fd: self.receiver.as_raw_fd(),
                        events: INPUT,
                        revents: 0,
                    },
                ];
                // SAFETY: both descriptors remain borrowed/owned throughout this blocking call
                let result = unsafe { poll(fds.as_mut_ptr(), fds.len() as _, -1) };
                if result < 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e);
                }
                if fds[1].revents != 0 {
                    let mut bytes = [0; 128];
                    let _ = self.receiver.read(&mut bytes);
                    continue;
                }
                if fds[0].revents & FAILURE != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "input readiness source retired",
                    ));
                }
                if fds[0].revents & INPUT != 0 {
                    return Ok(Event::Input);
                }
            }
        }
    }
    #[test]
    // Sticky stop must interrupt a native wait and remain visible before subsequent waits
    fn native_cancel_before_and_during_wait() {
        use std::os::fd::AsFd;
        let (input, _writer) = UnixStream::pair().unwrap();
        let (wake, mut wait) = channel(7).unwrap();
        wake.signal(6, true);
        wake.signal(7, false);
        assert_eq!(wait.wait(input.as_fd()).unwrap(), Event::Firmware);
        let (finished, received) = std::sync::mpsc::channel();
        let thread =
            std::thread::spawn(move || finished.send(wait.wait(input.as_fd()).unwrap()).unwrap());
        assert!(received
            .recv_timeout(std::time::Duration::from_millis(20))
            .is_err());
        wake.signal(7, true);
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Event::Stopped
        );
        thread.join().unwrap();
        let (wake, mut wait) = channel(8).unwrap();
        let (input, _writer) = UnixStream::pair().unwrap();
        wake.signal(8, true);
        assert_eq!(wait.wait(input.as_fd()).unwrap(), Event::Stopped);
        assert_eq!(wait.wait(input.as_fd()).unwrap(), Event::Stopped);
    }
    #[test]
    // Keep queued input bytes intact while firmware work takes precedence between bounded batches
    fn firmware_priority_preserves_burst_and_disconnect() {
        use std::os::fd::AsFd;
        let (mut input, mut writer) = UnixStream::pair().unwrap();
        let (wake, mut wait) = channel(9).unwrap();
        writer.write_all(&[1, 2, 3]).unwrap();
        wake.signal(9, false);
        assert_eq!(wait.wait(input.as_fd()).unwrap(), Event::Firmware);
        for expected in [1, 2, 3] {
            assert_eq!(wait.wait(input.as_fd()).unwrap(), Event::Input);
            let mut byte = [0];
            input.read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], expected);
        }
        drop(writer);
        assert!(wait.wait(input.as_fd()).is_err());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // A notification between empty read and wait is retained, while stop is sticky
    fn real_worker_signal_closes_empty_read_and_stop_races() {
        let signal = std::sync::Arc::new(Signal::default());
        let observed = signal.generation();
        signal.notify();
        signal.wait(observed);
        let observed = signal.generation();
        let (done, receive) = std::sync::mpsc::channel();
        let worker = signal.clone();
        let thread = std::thread::spawn(move || {
            worker.wait(observed);
            done.send(()).unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(20)).is_err());
        signal.stop();
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        signal.wait(signal.generation());
        let replacement = Signal::default();
        let before = replacement.generation();
        signal.notify();
        assert_eq!(replacement.generation(), before);
    }

    #[test]
    // Unavailable native readiness retains the original bounded policy
    fn every_current_backend_retains_original_bounded_policy() {
        let shared = policy(true);
        let separate = policy(false);
        assert_eq!(shared.driver_timeout_ms, 0);
        assert_eq!(shared.idle_delay, Some(Duration::from_millis(5)));
        assert_eq!(separate.driver_timeout_ms, 5);
        assert_eq!(separate.idle_delay, None);
        assert!(shared.reason.contains("registration is unavailable"));
    }
}
