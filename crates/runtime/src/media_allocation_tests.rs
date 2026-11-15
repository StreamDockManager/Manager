//! Observe the allocator freeing the payload, independently of cache counters
use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

struct ObservedAllocator;

thread_local! {
    static ALLOCATIONS: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
}

/// Count requested allocation bytes on this thread only, excluding setup and
/// unrelated runtime workers. This measures allocator traffic, not RSS
pub(crate) fn measure_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    struct Reset;
    impl Drop for Reset {
        // Release this media allocation tests owner when it leaves scope
        fn drop(&mut self) {
            ALLOCATIONS.with(|counter| counter.set(None));
        }
    }
    ALLOCATIONS.with(|counter| {
        assert!(counter.get().is_none(), "allocation probes cannot nest");
        counter.set(Some((0, 0)));
    });
    let _reset = Reset;
    let result = operation();
    let (count, bytes) = ALLOCATIONS.with(|counter| counter.take().unwrap());
    (result, count, bytes)
}

// Only one explicitly watched allocation is inspected. The allocator does not
// allocate, take locks, or retain an event history, including in other tests
static WATCHED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicUsize = AtomicUsize::new(0);
static PROBE: Mutex<()> = Mutex::new(());

#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

unsafe impl GlobalAlloc for ObservedAllocator {
    // Allocate memory while accounting for media allocation tests
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the caller's allocation contract unchanged.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let _ = ALLOCATIONS.try_with(|counter| {
                if let Some((count, bytes)) = counter.get() {
                    counter.set(Some((count + 1, bytes + layout.size())));
                }
            });
        }
        ptr
    }

    // Release memory while accounting for media allocation tests
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let watched = WATCHED.load(Ordering::Acquire);
        // An Arc<[u8]> payload starts inside its allocation; a boxed payload
        // starts at the allocation base. Cover both in the regression control
        let observed = watched != 0
            && watched >= ptr as usize
            && watched < (ptr as usize).saturating_add(layout.size())
            && WATCHED
                .compare_exchange(watched, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
        // SAFETY: delegate the original pointer/layout contract unchanged.
        unsafe { System.dealloc(ptr, layout) };
        if observed {
            FREED.store(layout.size(), Ordering::Release);
        }
    }
}

// Watch for media allocation tests changes
fn watch(payload: &[u8]) {
    assert!(!payload.is_empty());
    assert_eq!(WATCHED.load(Ordering::Acquire), 0);
    FREED.store(0, Ordering::Release);
    WATCHED.store(payload.as_ptr() as usize, Ordering::Release);
}

#[test]
// Verify that expired weak frames free payloads below pool capacity
fn expired_weak_frames_free_payloads_below_pool_capacity() {
    let _probe = PROBE.lock().unwrap();
    const PAYLOAD: usize = 50 * 1024;
    const CYCLES: usize = 128;
    let mut observations = Vec::new();
    for repetition in 0..5 {
        // Demonstrate the original allocation failure with the same payload:
        // dropping the final strong slice reference does not deallocate it
        let legacy: Arc<[u8]> = vec![1; PAYLOAD].into();
        let weak = Arc::downgrade(&legacy);
        watch(&legacy);
        drop(legacy);
        assert_eq!(FREED.load(Ordering::Acquire), 0);
        drop(weak);
        assert!(FREED.load(Ordering::Acquire) >= PAYLOAD);

        let mut pool = FramePool::default();
        let mut freed_payload_bytes = 0;
        let started = Instant::now();
        for cycle in 0..CYCLES {
            let mut source = vec![0; PAYLOAD];
            source[..8].copy_from_slice(&(cycle as u64).to_le_bytes());
            let (frames, _) = share_frames(vec![source.clone(), source], &mut pool);
            assert!(Arc::ptr_eq(&frames[0], &frames[1]));
            let renderer = frames[0].clone();
            watch(&renderer);
            drop(frames);
            assert_eq!(FREED.load(Ordering::Acquire), 0);
            drop(renderer);
            // Pool still owns every Weak and no sweep has run. Assert actual
            // deallocation, rather than using strong_count/resident_bytes
            assert_eq!(FREED.load(Ordering::Acquire), PAYLOAD);
            freed_payload_bytes += FREED.load(Ordering::Acquire);
            assert_eq!(pool.len(), cycle + 1);
        }
        assert!(pool.len() < MAX_SHARED_FRAMES);
        observations.push(serde_json::json!({
            "repetition": repetition,
            "cycles": CYCLES,
            "payload_bytes_per_cycle": PAYLOAD,
            "observed_deallocated_payload_bytes": freed_payload_bytes,
            "retained_weak_entries": pool.len(),
            "expired_weak_payload_bytes": 0,
            "elapsed_us": started.elapsed().as_micros(),
        }));
    }
    eprintln!(
        "FRAME_ALLOCATION_LIFETIME={}",
        serde_json::to_string(&observations).unwrap()
    );
}

#[test]
// Verify that retired render cache weak reference does not retain payload
fn retired_render_cache_weak_reference_does_not_retain_payload() {
    let _probe = PROBE.lock().unwrap();
    let mut cache = RenderCache::default();
    let renderer = cache
        .insert_shared("output".into(), vec![5; 65536])
        .unwrap();
    watch(&renderer);
    cache.remove("output");
    assert_eq!(cache.retired.len(), 1);
    assert_eq!(FREED.load(Ordering::Acquire), 0);
    assert_eq!(cache.in_flight_bytes(), 65536);
    drop(renderer);
    assert_eq!(FREED.load(Ordering::Acquire), 65536);
    assert_eq!(cache.retired.len(), 1, "no cache sweep has run");
    assert_eq!(cache.resident_bytes(), 0);
}

#[test]
// Verify that evicted clip payload is freed with pool and resident weak holders
fn evicted_clip_payload_is_freed_with_pool_and_resident_weak_holders() {
    let _probe = PROBE.lock().unwrap();
    let mut cache = Cache::default();
    let clip = Arc::new(Clip::new("gif", vec![vec![6; 65536]], vec![50]).unwrap());
    let renderer = clip.frames[0].clone();
    watch(&renderer);
    cache.insert("animation".into(), clip).unwrap();
    cache.remove("animation");
    assert_eq!(cache.resident.len(), 1);
    assert_eq!(FREED.load(Ordering::Acquire), 0);
    drop(renderer);
    assert_eq!(FREED.load(Ordering::Acquire), 65536);
    assert_eq!(cache.resident.len(), 1, "no resident sweep has run");
    assert_eq!(cache.resident_bytes(), 0);
}

#[test]
// Verify that rejected clip releases interned payload
fn rejected_clip_releases_interned_payload() {
    let _probe = PROBE.lock().unwrap();
    let source = vec![7; 65536];
    watch(&source);
    // Timing validation occurs after interning. Its Weak must not keep the
    // moved source allocation alive when construction fails
    assert!(Clip::new("gif", vec![source], vec![0]).is_err());
    assert_eq!(FREED.load(Ordering::Acquire), 65536);
}

#[test]
// Verify that combined budget keeps protected payload then frees it after playback and cache retire
fn combined_budget_keeps_protected_payload_then_frees_it_after_playback_and_cache_retire() {
    let _probe = PROBE.lock().unwrap();
    let budget = Arc::new(crate::media_budget::MediaBudget::default());
    let clip = Arc::new(Clip::new("gif", vec![vec![192; 65536]], vec![40]).unwrap());
    watch(&clip.frames[0]);
    let cached = budget
        .admit(crate::media_budget::Value::Clip(clip.clone()))
        .unwrap();
    let playback = budget.protect(clip.clone()).unwrap();
    drop(clip);
    budget.trim_pressure();
    assert_eq!(FREED.load(Ordering::Acquire), 0);
    assert_eq!(playback.clip.frames[0].len(), 65536);
    drop(playback);
    budget.trim_pressure();
    assert!(cached.clip().is_none());
    assert_eq!(FREED.load(Ordering::Acquire), 65536);
    assert_eq!(budget.stats()["tracked_allocations"], 0);
}
