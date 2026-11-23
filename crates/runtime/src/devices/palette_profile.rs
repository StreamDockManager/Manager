//! Explicitly authorized transient display qualification through the real draft renderer
//! Headless lease harness: process CPU/wake counters are measured, not window/optical behavior.
use super::palette_physical_support::{
    blank_scene, open_physical_models, BlankOnDrop, PhysicalTransport,
};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static RENDER_COUNTS: std::cell::Cell<Option<[u64; 2]>> = const { std::cell::Cell::new(None) };
}
// Called only from cfg(test, macOS) render-cache hooks. No keys or pixel data are collected.
pub(crate) fn record_render_cache(hit: bool) {
    RENDER_COUNTS.with(|cell| {
        if let Some(mut counts) = cell.get() {
            counts[0] += 1;
            counts[1] += u64::from(!hit);
            cell.set(Some(counts));
        }
    });
}

#[repr(C)]
#[derive(Default)]
struct RusageV2 {
    uuid: [u8; 16],
    // proc_pid_rusage CPU times are Mach ticks; convert using mach_timebase_info.
    user_ticks: u64,
    system_ticks: u64,
    package_idle_wakeups: u64,
    interrupt_wakeups: u64,
    pageins: u64,
    wired_bytes: u64,
    rss_bytes: u64,
    footprint_bytes: u64,
    start_abstime: u64,
    exit_abstime: u64,
    remainder: [u64; 8],
}
#[derive(serde::Serialize)]
struct ResourceDelta {
    user_ticks: u64,
    system_ticks: u64,
    package_idle_wakeups: u64,
    interrupt_wakeups: u64,
    rss_before: u64,
    rss_after: u64,
    footprint_before: u64,
    footprint_after: u64,
}
fn resources() -> Option<RusageV2> {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut *mut c_void) -> c_int;
    }
    const _: () = assert!(std::mem::size_of::<RusageV2>() == 160);
    const _: () = assert!(std::mem::offset_of!(RusageV2, footprint_bytes) == 72);
    let mut value = RusageV2::default();
    let result = unsafe {
        proc_pid_rusage(
            std::process::id() as c_int,
            2,
            (&mut value as *mut RusageV2).cast(),
        )
    };
    (result == 0 && value.start_abstime != 0 && value.exit_abstime == 0).then_some(value)
}
fn resource_delta(before: Option<RusageV2>, after: Option<RusageV2>) -> Option<ResourceDelta> {
    let (a, b) = (before?, after?);
    if a.start_abstime != b.start_abstime {
        return None;
    }
    Some(ResourceDelta {
        user_ticks: b.user_ticks.checked_sub(a.user_ticks)?,
        system_ticks: b.system_ticks.checked_sub(a.system_ticks)?,
        package_idle_wakeups: b.package_idle_wakeups.checked_sub(a.package_idle_wakeups)?,
        interrupt_wakeups: b.interrupt_wakeups.checked_sub(a.interrupt_wakeups)?,
        rss_before: a.rss_bytes,
        rss_after: b.rss_bytes,
        footprint_before: a.footprint_bytes,
        footprint_after: b.footprint_bytes,
    })
}

// Drain completion receipts before reporting transport delivery rather than optical success
fn settle(runtime: &Runtime, connections: &mut [Connection]) {
    for c in connections {
        assert!(c.worker.wait_idle(Duration::from_secs(10)));
        c.collect_outcomes(runtime);
        assert!(c.delivery.values().all(|d| d.completed && !d.pending));
    }
}

// Visit the existing renderer without bypassing its real wall-clock sampling policy
fn visit(runtime: &Runtime, connections: &mut [Connection]) {
    for c in connections {
        c.collect_outcomes(runtime);
        refresh_draft_preview(runtime, c);
    }
}

// Capture bounded cumulative counters so each paired interval can report exact differences
fn counts(connections: &[Connection], writes: &[Arc<AtomicUsize>]) -> Vec<[u64; 4]> {
    connections
        .iter()
        .zip(writes)
        .map(|(c, w)| {
            [
                c.static_palette.visits,
                c.static_palette.captures,
                c.static_palette.skipped,
                w.load(Ordering::Relaxed) as u64,
            ]
        })
        .collect()
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "physical static-palette qualification; requires explicit transient-display authorization"]
#[expect(
    clippy::assertions_on_constants,
    reason = "Reject physical execution in release mode while still allowing other release tests to compile"
)]
fn physical_static_palette_profile() {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_PHYSICAL_PALETTE_PROFILE").as_deref(),
        Ok("authorized-temporary-display")
    );
    assert!(cfg!(debug_assertions), "The candidate remains debug-only");
    let owned = open_physical_models();
    assert_eq!(owned.len(), 2);
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let modules = hardware_registry::modules();
    // Cleanup guards precede connections so workers join before the final neutral display writes
    let mut cleanup = Vec::new();
    let mut connections = Vec::new();
    let mut writes = Vec::new();
    for physical in owned {
        let module = modules
            .iter()
            .find(|m| m.manifest().model_id == physical.model)
            .unwrap();
        cleanup.push(BlankOnDrop {
            model: physical.model.into(),
            device: physical.device.clone(),
            session: module.session_for(&physical.contract).unwrap(),
            commands: blank_scene(&physical.contract.manifest),
        });
        let (mut c, _) = super::frame_policy_tests::connection_for_model(&runtime, physical.model);
        let old = c.device_id.clone();
        c.device_id = format!("palette-qualification-{}", physical.model);
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == old).unwrap();
            device.id = c.device_id.clone();
            device.persistence_key = c.device_id.clone();
            device.manifest = physical.contract.manifest.clone();
            device.variant_id = physical.contract.variant_id.clone();
            let mut profile = state.profiles[0].clone();
            profile.id = format!("palette-profile-{}", physical.model);
            profile.model_id = physical.model.into();
            profile.background_asset = None;
            for page in &mut profile.pages {
                page.bindings.clear();
            }
            let mut session = state.session(&old);
            session.profile_id = profile.id.clone();
            session.page_id = profile.active_page.clone();
            state.sessions.insert(c.device_id.clone(), session);
            state.profiles.push(profile);
        }
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(c.device_id.clone(), c.epoch);
        c.manifest = physical.contract.manifest.clone();
        c.contract_identity = physical.contract.identity();
        assert!(
            c.image_admission().unwrap()(),
            "Actual interface contract must pass normal admission"
        );
        c.session = Arc::new(Mutex::new(module.session_for(&physical.contract).unwrap()));
        c.encoding = FrameEncoding::IndividualUpdates;
        let count = Arc::new(AtomicUsize::new(0));
        c.worker = OutputWorker::spawn_with_policy(
            Box::new(PhysicalTransport {
                device: physical.device,
                writes: count.clone(),
            }),
            20,
            c.policy,
        )
        .unwrap();
        c.scene_revision = c.worker.scene_revision();
        connections.push(c);
        writes.push(count);
    }
    assert_ne!(connections[0].device_id, connections[1].device_id);
    assert_eq!(runtime.snapshot().devices.len(), 2);
    let candidate = crate::hardware_preview::PaletteDraft {
        mode: "dark".into(),
        id: "default".into(),
        custom: None,
    };
    let smoke = std::env::var("STREAM_DOCK_MANAGER_PHYSICAL_PALETTE_SMOKE").as_deref() == Ok("1");
    let pairs = if smoke { 1 } else { 5 };
    let phase_duration = Duration::from_secs(if smoke { 2 } else { 60 });
    for pair in 0..pairs {
        // Reverse order on alternate pairs to reduce simple thermal/order confounding
        let order = if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for enabled in order {
            runtime.cancel_plugin_palette_preview(None);
            visit(&runtime, &mut connections);
            settle(&runtime, &mut connections);
            for c in &mut connections {
                c.static_palette = StaticPaletteGate::default();
            }
            let mut token = runtime.preview_plugin_palette(candidate.clone()).unwrap();
            visit(&runtime, &mut connections);
            settle(&runtime, &mut connections);
            assert!(connections
                .iter()
                .all(|c| !enabled || c.static_palette.scene.is_some()));
            let before = counts(&connections, &writes);
            RENDER_COUNTS.with(|c| c.set(Some([0, 0])));
            let resource_before = resources();
            let mut latency_ns = Vec::with_capacity(1201);
            let start = Instant::now();
            let mut next_tick = start;
            let mut renew = start + Duration::from_secs(2);
            while start.elapsed() < phase_duration {
                if Instant::now() >= renew {
                    token = runtime.preview_plugin_palette(candidate.clone()).unwrap();
                    renew = Instant::now() + Duration::from_secs(2);
                }
                // Historical baseline is a test-only forced recapture, never a runtime setting
                if !enabled {
                    for connection in &mut connections {
                        connection.static_palette.scene = None;
                    }
                }
                let visiting = Instant::now();
                visit(&runtime, &mut connections);
                if latency_ns.len() < 2048 {
                    latency_ns.push(visiting.elapsed().as_nanos());
                }
                next_tick += Duration::from_millis(50);
                std::thread::sleep(next_tick.saturating_duration_since(Instant::now()));
            }
            settle(&runtime, &mut connections);
            let resource_after = resources();
            let render_counts = RENDER_COUNTS.with(|c| c.replace(None)).unwrap();
            let after = counts(&connections, &writes);
            let delta: Vec<_> = after
                .iter()
                .zip(&before)
                .map(|(a, b)| std::array::from_fn::<_, 4, _>(|i| a[i] - b[i]))
                .collect();
            if enabled {
                assert!(delta.iter().all(|d| d[1] == 0 && d[2] > 0));
            }
            assert!(
                delta.iter().all(|d| d[3] == 0),
                "Unchanged static scenes must not upload repeatedly"
            );
            eprintln!(
                "PALETTE_PHYSICAL {}",
                serde_json::json!({"smoke":smoke,"pair":pair,"enabled":enabled,"duration_ms":start.elapsed().as_millis(),"models":["n4-pro","k1-pro"],"counter_order":["visits","captures","skips","reports"],"delta":delta,"window_state":"headless lease harness","resources":resource_delta(resource_before,resource_after),"render_cache_request_miss_counts":render_counts,"visit_latency_ns":latency_ns,"optical_confirmation":false})
            );
            // A stale owner must not cancel the newer lease
            let newer = runtime.preview_plugin_palette(candidate.clone()).unwrap();
            runtime.cancel_plugin_palette_preview(Some(&token));
            assert!(connections
                .iter()
                .all(|c| runtime.hardware_preview_token(&c.device_id).is_some()));
            runtime.cancel_plugin_palette_preview(Some(&newer));
            visit(&runtime, &mut connections);
            settle(&runtime, &mut connections);
            assert!(connections
                .iter()
                .all(|c| runtime.hardware_preview_token(&c.device_id).is_none()));
        }
    }
    // Lease expiry is observed through the same renderer without relying on an unrelated UI event
    runtime.preview_plugin_palette(candidate).unwrap();
    visit(&runtime, &mut connections);
    settle(&runtime, &mut connections);
    std::thread::sleep(Duration::from_secs(6));
    visit(&runtime, &mut connections);
    settle(&runtime, &mut connections);
    assert!(connections
        .iter()
        .all(|c| runtime.hardware_preview_token(&c.device_id).is_none()));
    drop(connections);
    drop(cleanup);
    runtime.shutdown();
}
