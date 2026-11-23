//! Bounded admission to the device actor, including requests selected for dispatch
//!
//! Snapshot notifications describe authoritative current state and coalesce only
//! between admitted commands. Admission freezes preceding notifications as bounded
//! FIFO barriers, so a later restore cannot cross a benchmark start or upload
//! Per-device notifications occupy
//! two words in an actor-registered live identity, not an event backlog. Explicit
//! commands and reply-bearing requests are never coalesced
use crate::devices::Request;
use hardware_core::DeviceCommand;
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    sync::{mpsc, Arc, Condvar, Mutex, Weak},
    time::{Duration, Instant},
};

const MAX_REQUESTS: usize = 256;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const CONTROL_REQUESTS: usize = 32;
const CONTROL_BYTES: usize = 64 * 1024;
const RELEASE_REQUESTS: usize = 8;
const RELEASE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
enum Class {
    Normal,
    Control,
    Release,
    Signal,
}
struct Pending {
    request: Request,
    sequence: u64,
    bytes: usize,
    class: Class,
}
#[derive(Default)]
struct Signals {
    restore: Option<u64>,
    lost: Option<u64>,
}
#[derive(Default)]
struct State {
    pending: VecDeque<Pending>,
    wake: Option<u64>,
    media_attention: bool,
    restore: Option<u64>,
    identities: HashMap<(String, u64), Signals>,
    shutdown: Option<mpsc::Sender<()>>,
    sequence: u64,
    normal_count: usize,
    normal_bytes: usize,
    control_count: usize,
    control_bytes: usize,
    release_count: usize,
    release_bytes: usize,
    in_flight_count: usize,
    in_flight_bytes: usize,
    rejected: u64,
    coalesced: u64,
    senders: usize,
    closed: bool,
    closing: bool,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}
pub(crate) struct Sender(Arc<Shared>);
pub(crate) struct Receiver(Arc<Shared>);

#[derive(Serialize)]
pub(crate) struct Stats {
    pending_count: usize,
    pending_bytes: usize,
    in_flight_count: usize,
    in_flight_bytes: usize,
    request_limit: usize,
    byte_limit: usize,
    reserved_control_requests: usize,
    reserved_control_bytes: usize,
    reserved_release_requests: usize,
    reserved_release_bytes: usize,
    lifecycle_identities: usize,
    lifecycle_identity_bytes: usize,
    rejected: u64,
    coalesced: u64,
}

// Create shared device-mailbox state with one sender and one receiver
pub(crate) fn channel() -> (Sender, Receiver) {
    let shared = Arc::new(Shared::default());
    shared.state.lock().unwrap().senders = 1;
    (Sender(shared.clone()), Receiver(shared))
}

impl Clone for Sender {
    // Copy the device mailbox value for another owner
    fn clone(&self) -> Self {
        self.0.state.lock().unwrap().senders += 1;
        Self(self.0.clone())
    }
}
impl Drop for Sender {
    // Release this device mailbox owner when it leaves scope
    fn drop(&mut self) {
        self.0.state.lock().unwrap().senders -= 1;
        self.0.ready.notify_all();
    }
}

/// Registration is owned by exactly one physical connection and retires on all
/// failed-open/disconnect paths. Old-epoch notifications cannot target a successor
pub(crate) struct Registration {
    shared: Weak<Shared>,
    identity: (String, u64),
}
impl Drop for Registration {
    // Release this device mailbox owner when it leaves scope
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared
                .state
                .lock()
                .unwrap()
                .identities
                .remove(&self.identity);
        }
    }
}

impl Sender {
    // Register a device mailbox consumer
    pub fn register(&self, device: &str, epoch: u64) -> Registration {
        let identity = (device.to_owned(), epoch);
        self.0
            .state
            .lock()
            .unwrap()
            .identities
            .insert(identity.clone(), Signals::default());
        Registration {
            shared: Arc::downgrade(&self.0),
            identity,
        }
    }

    // Send the device mailbox message
    pub fn send(&self, request: Request) -> Result<(), String> {
        let mut state = self.0.state.lock().unwrap();
        let error = if state.closed || state.closing {
            Some("Device service is stopping or unavailable")
        } else {
            None
        };
        if let Some(error) = error {
            state.rejected = state.rejected.saturating_add(1);
            drop(state);
            reject(request, error);
            return Err(error.into());
        }
        state.sequence = state.sequence.wrapping_add(1).max(1);
        let sequence = state.sequence;
        let replaced = match request {
            Request::Wake => state.wake.replace(sequence).is_some(),
            Request::Restore => state.restore.replace(sequence).is_some(),
            Request::RestoreDevice(device, epoch) => state
                .identities
                .get_mut(&(device, epoch))
                .is_some_and(|signals| signals.restore.replace(sequence).is_some()),
            Request::Lost(device, epoch) => state
                .identities
                .get_mut(&(device, epoch))
                .is_some_and(|signals| signals.lost.replace(sequence).is_some()),
            Request::Shutdown(reply) => {
                state.closing = true;
                state.shutdown = Some(reply);
                false
            }
            request => {
                let bytes = request_bytes(&request);
                let class = if matches!(&request, Request::BenchmarkStop { .. }) {
                    Class::Release
                } else if reserved_control(&request) {
                    Class::Control
                } else {
                    Class::Normal
                };
                let (count, used, count_limit, byte_limit) = match class {
                    Class::Release => (
                        state.release_count,
                        state.release_bytes,
                        RELEASE_REQUESTS,
                        RELEASE_BYTES,
                    ),
                    Class::Control => (
                        state.control_count,
                        state.control_bytes,
                        CONTROL_REQUESTS,
                        CONTROL_BYTES,
                    ),
                    _ => (
                        state.normal_count,
                        state.normal_bytes,
                        MAX_REQUESTS,
                        MAX_BYTES,
                    ),
                };
                if count >= count_limit || bytes > byte_limit.saturating_sub(used) {
                    state.rejected = state.rejected.saturating_add(1);
                    drop(state);
                    let error = "Device request queue is full; retry after pending output settles";
                    reject(request, error);
                    return Err(error.into());
                }
                if !matches!(class, Class::Release) {
                    let (notification_count, notification_bytes) = notification_budget(&state);
                    let own_count = usize::from(matches!(class, Class::Normal));
                    let own_bytes = if own_count == 1 { bytes } else { 0 };
                    if notification_count.saturating_add(own_count)
                        > MAX_REQUESTS.saturating_sub(state.normal_count)
                        || notification_bytes.saturating_add(own_bytes)
                            > MAX_BYTES.saturating_sub(state.normal_bytes)
                    {
                        state.rejected = state.rejected.saturating_add(1);
                        drop(state);
                        let error =
                            "Device request queue is full; retry after pending output settles";
                        reject(request, error);
                        return Err(error.into());
                    }
                    freeze_notifications(&mut state);
                }
                match class {
                    Class::Release => {
                        state.release_count += 1;
                        state.release_bytes += bytes;
                    }
                    Class::Control => {
                        state.control_count += 1;
                        state.control_bytes += bytes;
                    }
                    _ => {
                        state.normal_count += 1;
                        state.normal_bytes += bytes;
                    }
                }
                state.pending.push_back(Pending {
                    request,
                    sequence,
                    bytes,
                    class,
                });
                false
            }
        };
        if replaced {
            state.coalesced = state.coalesced.saturating_add(1);
        }
        drop(state);
        self.0.ready.notify_one();
        Ok(())
    }

    // Snapshot queued and in-flight mailbox work, including coalesced notification slots
    pub fn stats(&self) -> Stats {
        let state = self.0.state.lock().unwrap();
        let mut pending_count = state.pending.len();
        let mut pending_bytes: usize = state.pending.iter().map(|p| p.bytes).sum();
        for present in [
            state.wake.is_some(),
            state.restore.is_some(),
            state.shutdown.is_some(),
        ] {
            if present {
                pending_count += 1;
                pending_bytes += size_of::<Request>();
            }
        }
        for ((device, _), signals) in &state.identities {
            let count =
                usize::from(signals.restore.is_some()) + usize::from(signals.lost.is_some());
            pending_count += count;
            pending_bytes += count * (size_of::<Request>() + device.len());
        }
        Stats {
            pending_count,
            pending_bytes,
            in_flight_count: state.in_flight_count,
            in_flight_bytes: state.in_flight_bytes,
            request_limit: MAX_REQUESTS,
            byte_limit: MAX_BYTES,
            reserved_control_requests: CONTROL_REQUESTS,
            reserved_control_bytes: CONTROL_BYTES,
            reserved_release_requests: RELEASE_REQUESTS,
            reserved_release_bytes: RELEASE_BYTES,
            lifecycle_identities: state.identities.len(),
            lifecycle_identity_bytes: state
                .identities
                .keys()
                .map(|(id, _)| id.capacity() + size_of::<Signals>() + size_of::<(String, u64)>())
                .sum(),
            rejected: state.rejected,
            coalesced: state.coalesced,
        }
    }
}

// Charge reserved notification slots for their request and device-identity storage
fn notification_budget(state: &State) -> (usize, usize) {
    let mut count = usize::from(state.wake.is_some()) + usize::from(state.restore.is_some());
    let mut bytes = count * size_of::<Request>();
    for ((device, _), signals) in &state.identities {
        let present = usize::from(signals.restore.is_some()) + usize::from(signals.lost.is_some());
        count = count.saturating_add(present);
        bytes = bytes.saturating_add(present * (size_of::<Request>() + device.len()));
    }
    (count, bytes)
}

/// Called only after reserving every barrier and the new explicit request
fn freeze_notifications(state: &mut State) {
    let mut barriers = Vec::new();
    if let Some(sequence) = state.wake.take() {
        barriers.push((sequence, Request::Wake));
    }
    if let Some(sequence) = state.restore.take() {
        barriers.push((sequence, Request::Restore));
    }
    for ((device, epoch), signals) in &mut state.identities {
        if let Some(sequence) = signals.restore.take() {
            barriers.push((sequence, Request::RestoreDevice(device.clone(), *epoch)));
        }
        if let Some(sequence) = signals.lost.take() {
            barriers.push((sequence, Request::Lost(device.clone(), *epoch)));
        }
    }
    barriers.sort_unstable_by_key(|(sequence, _)| *sequence);
    for (sequence, request) in barriers {
        let bytes = request_bytes(&request);
        state.normal_count += 1;
        state.normal_bytes += bytes;
        state.pending.push_back(Pending {
            request,
            sequence,
            bytes,
            class: Class::Normal,
        });
    }
}

struct Permit {
    shared: Arc<Shared>,
    bytes: usize,
    class: Class,
}
impl Drop for Permit {
    // Release this device mailbox owner when it leaves scope
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.in_flight_count -= 1;
        state.in_flight_bytes -= self.bytes;
        match self.class {
            Class::Normal => {
                state.normal_count -= 1;
                state.normal_bytes -= self.bytes;
            }
            Class::Control => {
                state.control_count -= 1;
                state.control_bytes -= self.bytes;
            }
            Class::Release => {
                state.release_count -= 1;
                state.release_bytes -= self.bytes;
            }
            Class::Signal => {}
        }
    }
}
pub(crate) struct Received {
    pub request: Option<Request>,
    _permit: Permit,
}
impl Drop for Received {
    // Release this device mailbox owner when it leaves scope
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            reject(
                request,
                "Device service stopped before dispatching the request",
            );
        }
    }
}

enum Signal {
    Wake,
    Restore,
    Device(String, u64, bool),
}
impl Receiver {
    // Transfer a selected request into in-flight accounting with a release receipt
    fn selected(
        &self,
        state: &mut State,
        request: Request,
        bytes: usize,
        class: Class,
    ) -> Received {
        state.in_flight_count += 1;
        state.in_flight_bytes += bytes;
        Received {
            request: Some(request),
            _permit: Permit {
                shared: self.0.clone(),
                bytes,
                class,
            },
        }
    }

    // Take shutdown
    pub fn take_shutdown(&self) -> Option<Received> {
        let mut state = self.0.state.lock().unwrap();
        let reply = state.shutdown.take()?;
        Some(self.selected(
            &mut state,
            Request::Shutdown(reply),
            size_of::<Request>(),
            Class::Signal,
        ))
    }

    // Remove the next device mailbox entry
    fn pop(&self, state: &mut State) -> Option<Received> {
        if let Some(reply) = state.shutdown.take() {
            return Some(self.selected(
                state,
                Request::Shutdown(reply),
                size_of::<Request>(),
                Class::Signal,
            ));
        }
        // Cancellation may overtake visual/stat work. Its own reserve cannot be
        // exhausted by those classes, and later dispatch observes the ended lease
        if let Some(index) = state
            .pending
            .iter()
            .position(|pending| matches!(pending.class, Class::Release))
        {
            let pending = state.pending.remove(index).unwrap();
            if let Request::BenchmarkStop {
                device,
                epoch,
                run_id,
                ..
            } = &pending.request
            {
                // A timed-out start enqueues a stop for cleanup. Never let that
                // priority stop pass an unstarted lease and then activate it later
                while let Some(index) = state.pending.iter().position(|queued| {
                    matches!(&queued.request, Request::BenchmarkStart { device: queued_device, epoch: queued_epoch, run_id: queued_run, .. }
                        if queued_device == device && queued_epoch == epoch && queued_run == run_id)
                }) {
                    let cancelled = state.pending.remove(index).unwrap();
                    state.control_count -= 1;
                    state.control_bytes -= cancelled.bytes;
                    // This is a small Start control request. Its unbounded reply
                    // channel send cannot wait for a consumer or take Runtime locks
                    reject(cancelled.request, "Benchmark start cancelled before dispatch");
                }
            }
            return Some(self.selected(state, pending.request, pending.bytes, pending.class));
        }
        let mut first = state.pending.front().map_or(u64::MAX, |p| p.sequence);
        let mut signal = None;
        for (sequence, kind) in [(state.wake, Signal::Wake), (state.restore, Signal::Restore)] {
            if let Some(sequence) = sequence.filter(|sequence| *sequence < first) {
                first = sequence;
                signal = Some(kind);
            }
        }
        let mut identity = None;
        for ((device, epoch), signals) in &state.identities {
            for (sequence, lost) in [(signals.restore, false), (signals.lost, true)] {
                if let Some(sequence) = sequence.filter(|sequence| *sequence < first) {
                    first = sequence;
                    identity = Some((device, *epoch, lost));
                }
            }
        }
        if let Some((device, epoch, lost)) = identity {
            signal = Some(Signal::Device(device.clone(), epoch, lost));
        }
        let request = match signal {
            Some(Signal::Wake) => {
                state.wake = None;
                Request::Wake
            }
            Some(Signal::Restore) => {
                state.restore = None;
                Request::Restore
            }
            Some(Signal::Device(device, epoch, lost)) => {
                let signals = state.identities.get_mut(&(device.clone(), epoch)).unwrap();
                if lost {
                    signals.lost = None;
                    Request::Lost(device, epoch)
                } else {
                    signals.restore = None;
                    Request::RestoreDevice(device, epoch)
                }
            }
            None => {
                let pending = state.pending.pop_front()?;
                return Some(self.selected(state, pending.request, pending.bytes, pending.class));
            }
        };
        let bytes = request_bytes(&request);
        Some(self.selected(state, request, bytes, Class::Signal))
    }

    // Attempt iter
    pub fn try_iter(&self) -> impl Iterator<Item = Received> + '_ {
        std::iter::from_fn(|| self.pop(&mut self.0.state.lock().unwrap()))
    }

    // Wake only maintenance scheduling, without admitting a request or repainting devices
    pub(crate) fn media_waker(&self) -> impl Fn() + Send + Sync + 'static {
        let shared = Arc::downgrade(&self.0);
        move || {
            if let Some(shared) = shared.upgrade() {
                let mut state = shared.state.lock().unwrap();
                if !state.closing && !state.closed && !state.media_attention {
                    state.media_attention = true;
                    shared.ready.notify_one();
                }
            }
        }
    }

    // Acknowledge a pending maintenance wake before reading the authoritative budget
    pub(crate) fn take_media_attention(&self) -> bool {
        std::mem::take(&mut self.0.state.lock().unwrap().media_attention)
    }

    // Wait for queued work until timeout, closure or disappearance of all senders
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Received, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.0.state.lock().unwrap();
        loop {
            if let Some(request) = self.pop(&mut state) {
                return Ok(request);
            }
            if state.closed || state.senders == 0 {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
            if state.media_attention {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            (state, _) = self.0.ready.wait_timeout(state, remaining).unwrap();
        }
    }
}
impl Drop for Receiver {
    // Release this device mailbox owner when it leaves scope
    fn drop(&mut self) {
        let (pending, shutdown) = {
            let mut state = self.0.state.lock().unwrap();
            state.closed = true;
            state.wake = None;
            state.restore = None;
            state.identities.clear();
            let pending = std::mem::take(&mut state.pending);
            for item in &pending {
                match item.class {
                    Class::Normal => {
                        state.normal_count -= 1;
                        state.normal_bytes -= item.bytes;
                    }
                    Class::Control => {
                        state.control_count -= 1;
                        state.control_bytes -= item.bytes;
                    }
                    Class::Release => {
                        state.release_count -= 1;
                        state.release_bytes -= item.bytes;
                    }
                    Class::Signal => {}
                }
            }
            (pending, state.shutdown.take())
        };
        for item in pending {
            reject(
                item.request,
                "Device service stopped before dispatching the request",
            );
        }
        drop(shutdown);
        self.0.ready.notify_all();
    }
}

// Report the reserved control condition for device mailbox
fn reserved_control(request: &Request) -> bool {
    matches!(
        request,
        Request::BenchmarkStart { .. }
            | Request::BenchmarkStats { .. }
            | Request::BenchmarkOutcomes { .. }
            | Request::BenchmarkStop { .. }
    ) || matches!(request, Request::Command(_, _, command) if !matches!(command, DeviceCommand::Image { .. } | DeviceCommand::Background { .. } | DeviceCommand::BootLogo { .. }))
}
// Charge the owned heap capacities of the device command’s payload
fn command_bytes(command: &DeviceCommand) -> usize {
    match command {
        DeviceCommand::Image { control_id, data } => {
            control_id.capacity().saturating_add(data.capacity())
        }
        DeviceCommand::Background { data, .. } | DeviceCommand::BootLogo { data } => {
            data.capacity()
        }
        DeviceCommand::Lighting { colors, .. } => {
            colors.capacity().saturating_mul(size_of::<[u8; 3]>())
        }
        DeviceCommand::Vibration { configuration, .. } => configuration
            .keys()
            .map(|key| key.capacity().saturating_add(96))
            .sum(),
        _ => 0,
    }
}
// Request bytes
fn request_bytes(request: &Request) -> usize {
    let owned = match request {
        Request::Command(device, _, command) => {
            device.capacity().saturating_add(command_bytes(command))
        }
        Request::Keyboard {
            device,
            contract_key,
            transaction,
            ..
        }
        | Request::BootLogo {
            device,
            contract_key,
            transaction,
            ..
        } => device
            .capacity()
            .saturating_add(contract_key.capacity())
            .saturating_add(
                transaction
                    .reports
                    .capacity()
                    .saturating_mul(size_of::<Vec<u8>>()),
            )
            .saturating_add(transaction.reports.iter().map(Vec::capacity).sum::<usize>())
            .saturating_add(
                transaction
                    .coalesce_key
                    .as_ref()
                    .map_or(0, String::capacity),
            ),
        Request::BenchmarkFrame {
            device, commands, ..
        } => device
            .capacity()
            .saturating_add(
                commands
                    .capacity()
                    .saturating_mul(size_of::<DeviceCommand>()),
            )
            .saturating_add(commands.iter().map(command_bytes).sum::<usize>()),
        Request::RefreshPluginBindings {
            device, sources, ..
        } => device
            .capacity()
            .saturating_add(sources.capacity().saturating_mul(size_of::<String>()))
            .saturating_add(sources.iter().map(String::capacity).sum::<usize>()),
        Request::BenchmarkStart { device, .. }
        | Request::BenchmarkStats { device, .. }
        | Request::BenchmarkOutcomes { device, .. }
        | Request::BenchmarkStop { device, .. }
        | Request::RestoreDevice(device, _)
        | Request::Lost(device, _) => device.capacity(),
        Request::Playback {
            device,
            control,
            asset,
            format,
            ..
        } => device
            .capacity()
            .saturating_add(control.as_ref().map_or(0, String::capacity))
            .saturating_add(asset.capacity())
            .saturating_add(format.capacity()),
        _ => 0,
    };
    size_of::<Request>().saturating_add(owned)
}
// Reject the invalid device mailbox request
fn reject(request: Request, error: &str) {
    match request {
        Request::Keyboard { reply, .. } => {
            let _ = reply.send(Err(crate::keyboard::TransferError::NotSubmitted(
                error.into(),
            )));
        }
        Request::BootLogo { reply, .. }
        | Request::BenchmarkStart { reply, .. }
        | Request::BenchmarkStop { reply, .. } => {
            let _ = reply.send(Err(error.into()));
        }
        Request::BenchmarkFrame { reply, .. } | Request::BenchmarkStats { reply, .. } => {
            let _ = reply.send(Err(error.into()));
        }
        Request::BenchmarkOutcomes { reply, .. } => {
            let _ = reply.send(Err(error.into()));
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "device_mailbox_tests.rs"]
mod tests;

#[cfg(test)]
mod media_tests {
    use super::*;

    #[test]
    // A pre-wait notification persists and cannot consume a mailbox command slot
    fn media_attention_is_coalesced_and_keeps_request_order() {
        let (sender, receiver) = channel();
        let wake = receiver.media_waker();
        wake();
        wake();
        assert_eq!(sender.stats().pending_count, 0);
        sender.send(Request::Restore).unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::ZERO).unwrap().request,
            Some(Request::Restore)
        ));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(receiver.take_media_attention());
        assert!(!receiver.take_media_attention());
        drop(receiver);
        wake();
    }

    #[test]
    // A blocked receiver wakes on maintenance without waiting for its ordinary deadline
    fn media_attention_interrupts_an_idle_mailbox() {
        let (_sender, receiver) = channel();
        let wake = receiver.media_waker();
        let (started, starting) = mpsc::sync_channel(0);
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            let result = receiver.recv_timeout(Duration::from_secs(3));
            finished
                .send((
                    matches!(result, Err(mpsc::RecvTimeoutError::Timeout)),
                    receiver.take_media_attention(),
                ))
                .unwrap();
        });
        starting.recv().unwrap();
        wake();
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            (true, true)
        );
        thread.join().unwrap();
    }
}
