//! R06 retained-allocation workload. Large structural evidence is opt-in
use std::sync::Arc;

// Construct the fixture clip fixture for media budget tests
fn fixture_clip(group: u64) -> Arc<crate::media::Clip> {
    let frames = (0u64..64)
        .map(|frame| {
            let mut bytes = vec![7; 1024 * 1024];
            bytes[..8].copy_from_slice(&group.to_le_bytes());
            bytes[8..16].copy_from_slice(&frame.to_le_bytes());
            bytes
        })
        .collect();
    Arc::new(crate::media::Clip::new("gif", frames, vec![40; 64]).unwrap())
}

#[test]
#[ignore = "five-repeat release combined media retention/admission evidence"]
// Verify that combined cache pending and active playback plateau
fn combined_cache_pending_and_active_playback_plateau() {
    use crate::media_budget::{MediaBudget, Value, HARD_BYTES};
    use std::time::Instant;
    for repetition in 0..5 {
        let budget = Arc::new(MediaBudget::default());
        let mut clips = crate::media::Cache::with_budget(budget.clone());
        let mut active = vec![];
        let mut admission_us = 0u128;
        for index in 0..4 {
            let reservation = budget.reserve(66 * 1024 * 1024).unwrap();
            let clip = fixture_clip(index);
            let started = Instant::now();
            reservation.track_clip(&clip, 0).unwrap();
            active.push(budget.protect(clip.clone()).unwrap());
            clips.insert(format!("clip-{index}"), clip).unwrap();
            admission_us += started.elapsed().as_micros();
        }
        let mut editor_slots = vec![];
        for index in 10..12 {
            let reservation = budget.reserve(66 * 1024 * 1024).unwrap();
            let clip = fixture_clip(index);
            let started = Instant::now();
            reservation.track_clip(&clip, 0).unwrap();
            editor_slots.push(budget.admit(Value::Clip(clip)).unwrap());
            admission_us += started.elapsed().as_micros();
        }
        let mut rendered = crate::media::RenderCache::with_budget(budget.clone());
        for index in 0..31 {
            let frame = vec![index; 2 * 1024 * 1024];
            let started = Instant::now();
            rendered.insert(format!("render-{index}"), frame);
            admission_us += started.elapsed().as_micros();
        }
        let mut index_slots = vec![];
        for value in 0..2 {
            let reservation = budget.reserve(8_000_000).unwrap();
            let index = Arc::new(vec![value; 1_000_000]);
            let started = Instant::now();
            reservation.track_index(&index).unwrap();
            index_slots.push(budget.admit(Value::Index(index)).unwrap());
            admission_us += started.elapsed().as_micros();
        }
        let before_pending = budget.stats();
        let first = budget.reserve(80 * 1024 * 1024).unwrap();
        let pending = vec![3u8; 80 * 1024 * 1024];
        assert!(budget.reserve(80 * 1024 * 1024).is_err());
        let saturated = budget.stats();
        assert!(saturated["combined_bytes"].as_u64().unwrap() <= HARD_BYTES as u64);
        assert!(saturated["peak_combined_bytes"].as_u64().unwrap() <= HARD_BYTES as u64);
        assert_eq!(saturated["reserved_preparation_bytes"], pending.capacity());
        for (index, playback) in active.iter().enumerate() {
            assert_eq!(&playback.clip.frames[0][..8], &(index as u64).to_le_bytes());
        }
        let keys: Vec<_> = (0..4).map(|index| format!("clip-{index}")).collect();
        let started = Instant::now();
        let (_, hit_allocations, hit_requested_bytes) =
            crate::media::allocation_tests::measure_allocations(|| {
                for _ in 0..1000 {
                    for (index, key) in keys.iter().enumerate() {
                        let clip = clips.get(key).unwrap();
                        assert!(Arc::ptr_eq(&clip, &active[index].clip));
                    }
                }
            });
        let protected_hit_us = started.elapsed().as_micros();
        assert_eq!(hit_allocations, 0);
        assert_eq!(hit_requested_bytes, 0);
        drop(pending);
        drop(first);
        let after_cancel = budget.stats();
        let native = budget
            .reserve(crate::media_budget::PREPARATION_BYTES)
            .unwrap();
        let active_with_native_workspace = budget.stats();
        assert!(
            active_with_native_workspace["combined_bytes"]
                .as_u64()
                .unwrap()
                <= active_with_native_workspace["preparation_admission_limit_bytes"]
                    .as_u64()
                    .unwrap()
        );
        drop(native);
        drop(active);
        budget.trim_idle();
        let idle = budget.stats();
        assert!(idle["combined_bytes"].as_u64().unwrap() <= 32 * 1024 * 1024);
        drop((clips, rendered, editor_slots, index_slots));
        budget.trim_pressure();
        let retired = budget.stats();
        assert_eq!(retired["combined_bytes"], 0);
        assert_eq!(retired["tracked_allocations"], 0);
        assert_eq!(retired["cache_slots"], 0);
        assert_eq!(retired["reservations"], 0);
        println!(
            "R06_COMBINED_CANDIDATE={}",
            serde_json::json!({"repetition":repetition,"before_pending":before_pending,"saturated":saturated,"after_cancel":after_cancel,"active_with_native_workspace":active_with_native_workspace,"idle":idle,"retired":retired,"admission_us":admission_us,"protected_4000_hits_us":protected_hit_us,"protected_hit_allocation_count":hit_allocations,"protected_hit_requested_bytes":hit_requested_bytes})
        );
    }
}
