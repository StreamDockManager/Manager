use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Fixture {
    reads: AtomicUsize,
    registrations: AtomicUsize,
    drops: AtomicUsize,
    unhealthy: AtomicBool,
    fail_read: AtomicBool,
    times: Mutex<Vec<Instant>>,
}
struct Lease(Arc<Fixture>);
impl Watch for Lease {
    // Report whether the tests observation is healthy
    fn healthy(&mut self) -> bool {
        !self.0.unhealthy.load(Ordering::SeqCst)
    }
}
impl Drop for Lease {
    // Release this tests owner when it leaves scope
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Fixture {
    // Build an instrumented audio backend with controllable availability and failures
    fn backend(self: &Arc<Self>, available: bool) -> Backend {
        let source = self.clone();
        let observer = self.clone();
        Backend {
            read: Arc::new(move || {
                source.reads.fetch_add(1, Ordering::SeqCst);
                source.times.lock().unwrap().push(Instant::now());
                if source.fail_read.load(Ordering::SeqCst) {
                    return AudioState {
                        error: Some("Fixture unavailable".into()),
                        ..Default::default()
                    };
                }
                AudioState {
                    volume: Some(42),
                    muted: Some(false),
                    error: None,
                }
            }),
            subscribe: Arc::new(move |_| {
                observer.registrations.fetch_add(1, Ordering::SeqCst);
                if available {
                    Ok(Box::new(Lease(observer.clone())))
                } else {
                    Err("Fixture unsupported".into())
                }
            }),
        }
    }
}
// Wait for the tests condition or result
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "Observer fixture timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
// Toggle the fixture binding’s volume feedback and notify the audio observer
fn demand(runtime: &Runtime, enabled: bool) {
    let mut state = runtime.state.lock().unwrap();
    state.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .feedback = enabled.then(|| "volume".into());
    drop(state);
    runtime.audio_signal.demand_changed();
}
// Create a temporary runtime with a simulated device and no audio demand
fn runtime() -> (tempfile::TempDir, Arc<Runtime>) {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime.simulate_device(true, "simulator:observer").unwrap();
    demand(&runtime, false);
    (root, runtime)
}
#[test]
// Verify that callback storm coalesces without blocking or allocating an event queue
fn callback_storm_coalesces_without_blocking_or_allocating_an_event_queue() {
    let signal = Signal::default();
    for _ in 0..100_000 {
        signal.invalidate(false);
    }
    signal.invalidate(true);
    assert!(signal.dirty.load(Ordering::SeqCst));
    assert!(signal.rebind.load(Ordering::SeqCst));
    let receiver = signal.receive.lock().unwrap().take().unwrap();
    assert!(receiver.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
    signal.stop();
    signal.dirty.store(false, Ordering::SeqCst);
    signal.invalidate(false);
    assert!(!signal.dirty.load(Ordering::SeqCst));
}
#[test]
#[ignore = "Disposable loopback host; controlled observer, no native audio or HID"]
// Verify that shared observer is demand owned coalesces and releases on last consumer
fn shared_observer_is_demand_owned_coalesces_and_releases_on_last_consumer() {
    let (_root, runtime) = runtime();
    let source = Arc::new(Fixture::default());
    start_with(Arc::downgrade(&runtime), source.backend(true));
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    assert_eq!(source.registrations.load(Ordering::SeqCst), 0);
    demand(&runtime, true);
    wait(|| source.reads.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    // A newly interested consumer forces a shared reading without a second lease
    runtime.audio_signal.demand_changed();
    wait(|| source.reads.load(Ordering::SeqCst) == 2);
    for _ in 0..100_000 {
        runtime.audio_signal.invalidate(false);
    }
    wait(|| source.reads.load(Ordering::SeqCst) >= 3);
    assert_eq!(source.registrations.load(Ordering::SeqCst), 1);
    assert!(source
        .times
        .lock()
        .unwrap()
        .windows(2)
        .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_millis(245)));
    demand(&runtime, false);
    wait(|| source.drops.load(Ordering::SeqCst) == 1);
    let before = source.reads.load(Ordering::SeqCst);
    runtime.audio_signal.invalidate(true);
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(source.reads.load(Ordering::SeqCst), before);
    demand(&runtime, true);
    wait(|| source.registrations.load(Ordering::SeqCst) == 2);
    let weak = Arc::downgrade(&runtime);
    drop(runtime);
    wait(|| weak.upgrade().is_none());
    wait(|| source.drops.load(Ordering::SeqCst) == 2);
}
#[test]
#[ignore = "Disposable loopback host; controlled observer, no native audio or HID"]
// Verify that unavailable and failed notifications restore polling without registration spin
fn unavailable_and_failed_notifications_restore_polling_without_registration_spin() {
    for available in [false, true] {
        let (_root, runtime) = runtime();
        let source = Arc::new(Fixture::default());
        start_with(Arc::downgrade(&runtime), source.backend(available));
        demand(&runtime, true);
        wait(|| source.reads.load(Ordering::SeqCst) == 1);
        if available {
            source.unhealthy.store(true, Ordering::SeqCst);
            runtime.audio_signal.invalidate(false);
        }
        wait(|| source.reads.load(Ordering::SeqCst) >= 4);
        assert_eq!(source.registrations.load(Ordering::SeqCst), 1);
        assert!(!runtime.feedback_state.lock().unwrap().audio_observer);
        runtime.shutdown();
    }
}
#[test]
#[ignore = "Disposable loopback host; controlled observer, no native audio or HID"]
// Verify that authoritative read failure releases observation and recovery uses fallback
fn authoritative_read_failure_releases_observation_and_recovery_uses_fallback() {
    let (_root, runtime) = runtime();
    let source = Arc::new(Fixture::default());
    start_with(Arc::downgrade(&runtime), source.backend(true));
    demand(&runtime, true);
    wait(|| runtime.feedback_state.lock().unwrap().audio_observer);
    source.fail_read.store(true, Ordering::SeqCst);
    runtime.audio_signal.invalidate(false);
    wait(|| source.drops.load(Ordering::SeqCst) == 1);
    wait(|| runtime.feedback_state.lock().unwrap().audio.error.is_some());
    source.fail_read.store(false, Ordering::SeqCst);
    wait(|| runtime.feedback_state.lock().unwrap().audio.volume == Some(42));
    assert!(!runtime.feedback_state.lock().unwrap().audio_observer);
    assert_eq!(source.registrations.load(Ordering::SeqCst), 1);
    runtime.shutdown();
}
#[test]
#[cfg(target_os = "macos")]
#[ignore = "Explicit read-only native Core Audio registration/teardown; does not change volume or qualify callbacks"]
// Verify that macOS native registration and teardown
fn macos_native_registration_and_teardown() {
    for _ in 0..10 {
        let signal = Arc::new(Signal::default());
        let mut lease = macos::Subscription::new(signal.clone()).unwrap();
        assert!(lease.healthy());
        drop(lease);
        signal.stop();
    }
}

#[test]
#[ignore = "Disposable loopback host; controlled observer, no native audio or HID"]
// Verify that healthy observation still takes authoritative safety reads
fn healthy_observation_still_takes_authoritative_safety_reads() {
    let (_root, runtime) = runtime();
    let source = Arc::new(Fixture::default());
    start_with(Arc::downgrade(&runtime), source.backend(true));
    demand(&runtime, true);
    wait(|| runtime.feedback_state.lock().unwrap().sampled_at.is_some());
    let initial = runtime.feedback_state.lock().unwrap().sampled_at;
    let deadline = Instant::now() + Duration::from_secs(7);
    while runtime.feedback_state.lock().unwrap().sampled_at == initial {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let samples = source.times.lock().unwrap();
    assert!(samples[1].duration_since(samples[0]) >= SAFETY);
    assert_ne!(runtime.feedback_state.lock().unwrap().sampled_at, initial);
    assert_eq!(source.registrations.load(Ordering::SeqCst), 1);
    runtime.shutdown();
}
