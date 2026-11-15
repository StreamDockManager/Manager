//! A diagnostic union of cache-tracked encoded allocations. This is not a heap
//! estimate: decoding workspaces, GPU memory, strings and allocator overhead are
//! excluded. Sampling independent caches is approximate during concurrent work
use crate::media::{SharedBytes, WeakBytes};
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
pub(crate) struct MediaMemory {
    allocations: HashMap<usize, (usize, bool)>,
}
impl MediaMemory {
    // Track each allocation address once and merge whether any owner is cached
    fn allocation(&mut self, address: usize, bytes: usize, cached: bool) {
        self.allocations
            .entry(address)
            .and_modify(|entry| entry.1 |= cached)
            .or_insert((bytes, cached));
    }
    // Look up cached frame
    pub(crate) fn cached_frame(&mut self, frame: &SharedBytes) {
        self.allocation(Arc::as_ptr(frame) as usize, frame.len(), true);
    }
    // Charge a weakly referenced frame only while a strong owner still exists
    pub(crate) fn live_frame(&mut self, frame: &WeakBytes, bytes: usize) {
        if frame.strong_count() > 0 {
            self.allocation(frame.as_ptr() as usize, bytes, false);
        }
    }
    // Look up cached index
    pub(crate) fn cached_index(&mut self, index: &Arc<Vec<u64>>) {
        self.allocation(
            Arc::as_ptr(index) as usize,
            index.capacity() * size_of::<u64>(),
            true,
        );
    }
    // Charge a weakly referenced timing index only while a strong owner still exists
    pub(crate) fn live_index(&mut self, index: &std::sync::Weak<Vec<u64>>, bytes: usize) {
        if index.strong_count() > 0 {
            self.allocation(index.as_ptr() as usize, bytes, false);
        }
    }
    // Summarize unique live allocations and the subset retained by caches
    pub(crate) fn summary(&self) -> serde_json::Value {
        let live: usize = self.allocations.values().map(|(bytes, _)| bytes).sum();
        let cached: usize = self
            .allocations
            .values()
            .filter(|(_, cached)| *cached)
            .map(|(bytes, _)| bytes)
            .sum();
        serde_json::json!({
            "tracked_unique_allocations": self.allocations.len(),
            "tracked_live_payload_bytes": live,
            "cached_payload_bytes": cached,
            "retired_live_payload_bytes": live - cached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that multiple clip caches and retired frames share one payload charge
    fn multiple_clip_caches_and_retired_frames_share_one_payload_charge() {
        let clip = Arc::new(crate::media::Clip::new("gif", vec![vec![9; 1024]], vec![40]).unwrap());
        let mut first = crate::media::Cache::default();
        let mut second = crate::media::Cache::default();
        first.insert("first".into(), clip.clone()).unwrap();
        second.insert("second".into(), clip.clone()).unwrap();
        let held = clip.frames[0].clone();
        drop(clip);
        let sample = |first: &mut crate::media::Cache, second: &mut crate::media::Cache| {
            let mut memory = MediaMemory::default();
            first.collect_memory(&mut memory);
            second.collect_memory(&mut memory);
            memory.summary()
        };
        assert_eq!(
            sample(&mut first, &mut second)["tracked_live_payload_bytes"],
            1024
        );
        first.remove("first");
        assert_eq!(
            sample(&mut first, &mut second)["cached_payload_bytes"],
            1024
        );
        assert_eq!(
            sample(&mut first, &mut second)["retired_live_payload_bytes"],
            0
        );
        second.remove("second");
        assert_eq!(
            sample(&mut first, &mut second)["retired_live_payload_bytes"],
            1024
        );
        drop(held);
        assert_eq!(
            sample(&mut first, &mut second)["tracked_live_payload_bytes"],
            0
        );
    }

    #[test]
    // Verify that shared cache payloads are counted once and retired only after last cache owner
    fn shared_cache_payloads_are_counted_once_and_retired_only_after_last_cache_owner() {
        let first: SharedBytes = Arc::new(vec![1; 100].into_boxed_slice());
        let second: SharedBytes = Arc::new(vec![2; 200].into_boxed_slice());
        let mut memory = MediaMemory::default();
        memory.cached_frame(&first);
        memory.live_frame(&Arc::downgrade(&first), first.len());
        memory.cached_frame(&first);
        memory.live_frame(&Arc::downgrade(&second), second.len());
        assert_eq!(memory.summary()["tracked_live_payload_bytes"], 300);
        assert_eq!(memory.summary()["cached_payload_bytes"], 100);
        assert_eq!(memory.summary()["retired_live_payload_bytes"], 200);
        let retired = Arc::downgrade(&second);
        drop(second);
        let mut after = MediaMemory::default();
        after.live_frame(&retired, 200);
        assert_eq!(after.summary()["tracked_live_payload_bytes"], 0);
    }
}
