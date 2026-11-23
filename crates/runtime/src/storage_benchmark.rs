//! Run this fixture alone with the disposable installation-key wrapper, timing enabled,
//! and --ignored --nocapture --test-threads=1.
use crate::{profiles::ProfileStore, workspace::Edit, Runtime};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use stream_dock_manager_storage::timing;

fn measure(label: &str, mut run: impl FnMut(usize)) {
    let mut samples = BTreeMap::<String, Vec<f64>>::new();
    let mut calls = BTreeMap::new();
    for i in 0..20 {
        let before = timing::snapshot();
        let start = Instant::now();
        run(i);
        samples
            .entry("total".into())
            .or_default()
            .push(start.elapsed().as_secs_f64() * 1000.);
        for (phase, end) in timing::snapshot() {
            let previous = before.get(&phase).copied().unwrap_or_default();
            if end.calls > previous.calls {
                *calls.entry(phase).or_insert(0) += end.calls - previous.calls;
                samples
                    .entry(format!("{phase:?}"))
                    .or_default()
                    .push((end.elapsed - previous.elapsed).as_secs_f64() * 1000.);
            }
        }
    }
    for (phase, mut values) in samples {
        values.sort_by(f64::total_cmp);
        let n = values.len();
        println!(
            "{label} {phase} median={:.3} p95={:.3} ms",
            values[n.div_ceil(2) - 1],
            values[(n * 95).div_ceil(100) - 1]
        );
    }
    println!("{label} phase calls {calls:?}");
}

fn copies() -> u64 {
    timing::snapshot()
        .get(&timing::Phase::BackupCopy)
        .copied()
        .unwrap_or_default()
        .calls
}

// Observe the writer completing its first copy, before it validates the snapshot.
fn wait_for_copy(previous: u64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while copies() == previous {
        assert!(Instant::now() < deadline, "recovery copy did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "disposable storage latency fixture"]
fn storage_patch_workloads() {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_STORAGE_TIMING").as_deref(),
        Ok("1")
    );
    for count in [1, 32] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let store = ProfileStore::open(root.join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let template = data.profiles[0].clone();
        for i in 1..count {
            let mut profile = template.clone();
            profile.id = format!("fixture-{i}");
            data.profiles.push(profile);
        }
        store.save(&data).unwrap();
        drop(store);
        let runtime = Runtime::build(root.into(), false).unwrap();
        let storage = runtime.profiles.storage.clone();
        for _ in 0..3 {
            storage.backup_recovery().unwrap();
        }
        println!(
            "fixture profiles={count} workspace_bytes={} backups={}",
            serde_json::to_vec(&data).unwrap().len(),
            storage.backup_paths().unwrap().len()
        );
        let mut revision = runtime.profiles.load().unwrap().configuration_revision;
        let mut binding = template.pages[0].bindings["key-1"].clone();
        let mut profile = template.clone();
        let mut edit = |i: usize, rename: bool| {
            let operation = if rename {
                profile.name = format!("Profile {i}");
                Edit::SaveProfile {
                    profile: profile.clone(),
                }
            } else {
                binding.title = format!("Title {i}");
                profile.pages[0]
                    .bindings
                    .insert("key-1".into(), binding.clone());
                Edit::Bindings {
                    profile_id: profile.id.clone(),
                    page_id: profile.active_page.clone(),
                    bindings: BTreeMap::from([("key-1".into(), Some(binding.clone()))]),
                }
            };
            revision = runtime
                .apply_workspace_edits(revision, "Edit", vec![operation])
                .unwrap()
                .configuration_revision;
        };
        measure("review", |_| {
            runtime
                .plugins
                .lock()
                .unwrap()
                .service_review("fixture.demo")
                .unwrap();
        });
        measure("application_data", |_| {
            runtime.application_data().unwrap();
        });
        measure("binding", |i| edit(i, false));
        measure("profile", |i| edit(i, true));
        // Exercise the same writer-owned automatic backup repeatedly, then
        // independently observe a real debounce firing after the final edit.
        measure("queued_binding_and_backup", |i| {
            let previous = copies();
            std::thread::scope(|scope| {
                let job = scope.spawn(|| storage.backup_recovery().unwrap());
                wait_for_copy(previous);
                let start = Instant::now();
                edit(i + 20, false);
                println!("queued_save_us={}", start.elapsed().as_micros());
                job.join().unwrap();
            });
        });
        let previous = copies();
        wait_for_copy(previous);
        let start = Instant::now();
        edit(40, false);
        println!("timer_queued_save_us={}", start.elapsed().as_micros());
        runtime.shutdown();
    }
}
