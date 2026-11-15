//! One demand-owned audio observer and authoritative reader. Native callbacks
//! only set invalidation bits and try a bounded, nonblocking wake channel
use crate::{feedback::AudioState, Runtime};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
#[cfg(target_os = "windows")]
mod windows;
const FALLBACK: Duration = Duration::from_millis(250);
const SAFETY: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(30);

pub(crate) struct Signal {
    send: mpsc::SyncSender<()>,
    receive: Mutex<Option<mpsc::Receiver<()>>>,
    demand: AtomicU64,
    dirty: AtomicBool,
    rebind: AtomicBool,
    stopped: AtomicBool,
}
impl Default for Signal {
    // Construct the default audio observer value
    fn default() -> Self {
        let (send, receive) = mpsc::sync_channel(1);
        Self {
            send,
            receive: Mutex::new(Some(receive)),
            demand: AtomicU64::new(1),
            dirty: AtomicBool::new(false),
            rebind: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }
}
impl Signal {
    // Wake the audio observer worker
    fn wake(&self) {
        let _ = self.send.try_send(());
    }
    // Advance the audio demand generation and wake the observer
    pub(crate) fn demand_changed(&self) {
        self.demand.fetch_add(1, Ordering::SeqCst);
        self.wake();
    }
    // Invalidate cached audio observer state
    pub(crate) fn invalidate(&self, rebind: bool) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if rebind {
            self.rebind.store(true, Ordering::SeqCst);
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.wake();
    }
    // Stop the audio observer operation
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake();
    }
}
pub(crate) trait Watch {
    // Report whether the audio observer observation is healthy
    fn healthy(&mut self) -> bool;
}
type Subscribe = dyn Fn(Arc<Signal>) -> Result<Box<dyn Watch>, String> + Send + Sync;
pub(crate) struct Backend {
    pub read: Arc<dyn Fn() -> AudioState + Send + Sync>,
    pub subscribe: Arc<Subscribe>,
}
// Subscribe to audio observer updates
fn subscribe(signal: Arc<Signal>) -> Result<Box<dyn Watch>, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::Subscription::new(signal)?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::Subscription::new(signal)?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::Subscription::new(signal)?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = signal;
        Err("Native audio notifications unavailable".into())
    }
}
// Start the audio observer operation
pub(crate) fn start(runtime: Weak<Runtime>) {
    start_with(
        runtime,
        Backend {
            read: Arc::new(crate::feedback::read_audio),
            subscribe: Arc::new(subscribe),
        },
    );
}
// Report the needed condition for audio observer
fn needed(runtime: &Runtime) -> bool {
    if runtime
        .plugins
        .lock()
        .unwrap()
        .telemetry_topics()
        .contains("audio")
    {
        return true;
    }
    let state = runtime.state.lock().unwrap();
    state
        .devices
        .iter()
        .filter(|device| device.connected)
        .any(|device| {
            let session = state.session(&device.id);
            state
                .profiles
                .iter()
                .find(|profile| profile.id == session.profile_id)
                .and_then(|profile| profile.pages.iter().find(|page| page.id == session.page_id))
                .is_some_and(|page| {
                    page.bindings.values().any(|binding| {
                        matches!(binding.feedback.as_deref(), Some("volume" | "mute"))
                    })
                })
        })
}
// Start with
pub(crate) fn start_with(runtime: Weak<Runtime>, backend: Backend) {
    let Some(owner) = runtime.upgrade() else {
        return;
    };
    let signal = owner.audio_signal.clone();
    let Some(receiver) = signal.receive.lock().unwrap().take() else {
        return;
    };
    drop(owner);
    std::thread::spawn(move || {
        let mut watch: Option<Box<dyn Watch>> = None;
        let mut demand_generation = 0;
        let mut demand = false;
        let mut last_read: Option<Instant> = None;
        let mut retry = Instant::now();
        let mut pending = false;
        loop {
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.is_stopping() || signal.stopped.load(Ordering::SeqCst) {
                break;
            }
            let generation = signal.demand.load(Ordering::SeqCst);
            if generation != demand_generation {
                let next = needed(&runtime);
                if !demand && next {
                    retry = Instant::now();
                }
                demand = next;
                demand_generation = generation;
                pending = true; // Initial subscribers get a fresh, shared reading
            }
            pending |= signal.dirty.swap(false, Ordering::SeqCst);
            if !demand {
                watch = None;
                last_read = None;
                pending = false;
                signal.rebind.store(false, Ordering::SeqCst);
                runtime.feedback_state.lock().unwrap().audio_observer = false;
                drop(runtime);
                if receiver.recv().is_err() {
                    break;
                }
                continue;
            }
            let now = Instant::now();
            let rebind = signal.rebind.swap(false, Ordering::SeqCst);
            if watch.as_mut().is_some_and(|watch| !watch.healthy()) {
                watch = None;
                retry = now + RETRY;
                pending = true;
            }
            if rebind || (watch.is_none() && now >= retry) {
                drop(watch.take()); // Teardown occurs on this worker, outside locks
                watch = (backend.subscribe)(signal.clone()).ok();
                retry = Instant::now() + RETRY;
                pending = true;
            }
            let mut observing = watch.as_mut().is_some_and(|watch| watch.healthy());
            let interval = if observing { SAFETY } else { FALLBACK };
            let due = last_read.is_none_or(|at| at.elapsed() >= interval)
                || (pending && last_read.is_none_or(|at| at.elapsed() >= FALLBACK));
            if due {
                pending = false;
                let audio = (backend.read)();
                if audio.error.is_some() && observing {
                    // Read failure invalidates notification confidence too;
                    // retry registration later while ordinary polling recovers
                    watch = None;
                    retry = Instant::now() + RETRY;
                    observing = false;
                }
                crate::feedback::publish_audio(&runtime, audio, observing);
                last_read = Some(Instant::now());
            }
            let delay = if pending || !observing {
                FALLBACK
            } else {
                interval
            };
            let wait = delay.saturating_sub(last_read.map_or(Duration::ZERO, |at| at.elapsed()));
            drop(runtime); // An idle observer must not retain the application
            if pending && !wait.is_zero() {
                // Leave notifications coalesced in the single-slot channel until
                // the next permitted read. A notification storm must not spin
                // the worker or exceed the existing four-read/second ceiling
                std::thread::sleep(wait);
                continue;
            }
            match receiver.recv_timeout(wait) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(watch);
    });
}
