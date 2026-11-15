//! Shared media residency policy, not a process/decoder heap limit
//!
//! Counts unique encoded payloads, owned Clip arrays/strings, index capacities,
//! and explicit pending-buffer reservations. Decoder scratch, GPU/OS buffers,
//! command-owned copies and allocator/container overhead are separate bounds
//! Owner locks may acquire budget -> slot; slot methods release their leaf lock
//! before touching the budget. Eviction never calls back into an owner lock
use crate::media::{Clip, SharedBytes, WeakBytes};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

const MIB: usize = 1024 * 1024;
pub(crate) const HARD_BYTES: usize = 384 * MIB;
const CACHE_HIGH: usize = 128 * MIB;
const CACHE_LOW: usize = 96 * MIB;
const IDLE_BYTES: usize = 32 * MIB;
const IDLE_AGE: Duration = Duration::from_secs(30);
pub(crate) const MAX_ALLOCATIONS: usize = 65_536;
const MAX_SLOTS: usize = 8192;
const MAX_SLOT_KEYS: usize = 65_536;
const MAX_RESERVATIONS: usize = 128;
/// Encoded collection plus one bounded frame's growth and metadata staging
/// Native collectors compact each completed frame before retaining the next
/// Decoder pixels/workspaces remain separately bounded by their existing limits
pub(crate) const CLIP_WORK_BYTES: usize = crate::media::MAX_CLIP + 16 * MIB;
pub(crate) const PUBLICATION_WORK_BYTES: usize = 8 * MIB;
pub(crate) const PREPARATION_BYTES: usize = 2 * crate::media::MAX_SOURCE + CLIP_WORK_BYTES;

#[derive(Clone, Copy)]
struct Limits {
    hard: usize,
    high: usize,
    low: usize,
    idle: usize,
}
impl Default for Limits {
    // Construct the default media budget value
    fn default() -> Self {
        Self {
            hard: HARD_BYTES,
            high: CACHE_HIGH,
            low: CACHE_LOW,
            idle: IDLE_BYTES,
        }
    }
}
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Key(u8, usize);
#[derive(Clone)]
enum Allocation {
    Frame(WeakBytes, usize),
    Clip(Weak<Clip>, usize),
    Index(Weak<Vec<u64>>, usize),
}
impl Allocation {
    // Return the key identifying media budget
    fn key(&self) -> Key {
        match self {
            Self::Frame(value, _) => Key(0, value.as_ptr() as usize),
            Self::Clip(value, _) => Key(1, value.as_ptr() as usize),
            Self::Index(value, _) => Key(2, value.as_ptr() as usize),
        }
    }
    // Read the stored logical charge for a tracked frame, clip or index allocation
    fn bytes(&self) -> usize {
        match self {
            Self::Frame(_, n) | Self::Clip(_, n) | Self::Index(_, n) => *n,
        }
    }
    // Report the live condition for media budget
    fn live(&self) -> bool {
        match self {
            Self::Frame(value, _) => value.strong_count() > 0,
            Self::Clip(value, _) => value.strong_count() > 0,
            Self::Index(value, _) => value.strong_count() > 0,
        }
    }
}
pub(crate) enum Value {
    Clip(Arc<Clip>),
    Frame(SharedBytes),
    Index(Arc<Vec<u64>>),
}
impl Value {
    // Enumerate shared allocations referenced by this cache value without duplicating ownership
    fn allocations(&self) -> Vec<Allocation> {
        match self {
            Self::Clip(clip) => {
                let mut result: Vec<_> = clip
                    .frames
                    .iter()
                    .map(|frame| Allocation::Frame(Arc::downgrade(frame), frame.len()))
                    .collect();
                result.push(Allocation::Clip(
                    Arc::downgrade(clip),
                    clip.metadata_capacity_bytes(),
                ));
                result
            }
            Self::Frame(frame) => vec![Allocation::Frame(Arc::downgrade(frame), frame.len())],
            Self::Index(index) => vec![Allocation::Index(
                Arc::downgrade(index),
                index.capacity() * size_of::<u64>(),
            )],
        }
    }
}
pub(crate) struct Slot {
    value: Mutex<Option<Value>>,
    budget: Weak<MediaBudget>,
    id: u64,
}
impl Slot {
    // Refresh the media budget entry’s activity marker
    fn touch(&self) {
        if let Some(budget) = self.budget.upgrade() {
            budget.touch(self.id);
        }
    }
    // Borrow a cached clip and refresh its recency when present
    pub(crate) fn clip(&self) -> Option<Arc<Clip>> {
        let value = self.peek_clip();
        if value.is_some() {
            self.touch();
        }
        value
    }
    // Borrow a cached clip without changing its recency
    pub(crate) fn peek_clip(&self) -> Option<Arc<Clip>> {
        match self.value.lock().unwrap().as_ref() {
            Some(Value::Clip(value)) => Some(value.clone()),
            _ => None,
        }
    }
    // Borrow a cached frame and refresh its recency when present
    pub(crate) fn frame(&self) -> Option<SharedBytes> {
        let value = self.peek_frame();
        if value.is_some() {
            self.touch();
        }
        value
    }
    // Borrow a cached frame without changing its recency
    pub(crate) fn peek_frame(&self) -> Option<SharedBytes> {
        match self.value.lock().unwrap().as_ref() {
            Some(Value::Frame(value)) => Some(value.clone()),
            _ => None,
        }
    }
    // Borrow a cached timing index and refresh its recency when present
    pub(crate) fn index(&self) -> Option<Arc<Vec<u64>>> {
        let value = self.peek_index();
        if value.is_some() {
            self.touch();
        }
        value
    }
    // Borrow a cached timing index without changing its recency
    pub(crate) fn peek_index(&self) -> Option<Arc<Vec<u64>>> {
        match self.value.lock().unwrap().as_ref() {
            Some(Value::Index(value)) => Some(value.clone()),
            _ => None,
        }
    }
    // Report the live condition for media budget
    pub(crate) fn live(&self) -> bool {
        self.value.lock().unwrap().is_some()
    }
    // Evict a retained media budget entry
    pub(crate) fn evict(&self) {
        let value = self.value.lock().unwrap().take();
        drop(value);
    }
}
struct Entry {
    id: u64,
    slot: Weak<Slot>,
    keys: Vec<Key>,
    used: Instant,
}
#[derive(Default)]
struct State {
    allocations: HashMap<Key, Allocation>,
    entries: VecDeque<Entry>,
    protected: HashMap<Key, usize>,
    reservations: HashMap<u64, usize>,
    sequence: u64,
    evictions: u64,
    rejections: u64,
    peak: usize,
    pressure: bool,
}
impl State {
    // Prune obsolete media budget entries
    fn prune(&mut self) {
        self.allocations.retain(|_, value| value.live());
        self.entries
            .retain(|entry| entry.slot.upgrade().is_some_and(|slot| slot.live()));
        // Weak control blocks and the coordinator's own high-water containers
        // retire too; do not retain a large empty index after session churn
        if self.allocations.capacity() > 1024
            && self.allocations.len() * 4 < self.allocations.capacity()
        {
            self.allocations.shrink_to(self.allocations.len().max(256));
        }
        if self.entries.capacity() > 256 && self.entries.len() * 4 < self.entries.capacity() {
            self.entries.shrink_to(self.entries.len().max(64));
        }
    }
    // Sum each uniquely tracked live allocation’s logical byte charge
    fn live_bytes(&self) -> usize {
        self.allocations.values().map(Allocation::bytes).sum()
    }
    // Sum preparation bytes reserved by all active admissions
    fn reserved_bytes(&self) -> usize {
        self.reservations.values().sum()
    }
    // Collect cached allocation keys not protected by playback ownership
    fn disposable_keys(&self) -> HashSet<Key> {
        self.entries
            .iter()
            .flat_map(|entry| entry.keys.iter().copied())
            .filter(|key| !self.protected.contains_key(key))
            .collect()
    }
    // Sum disposable cached allocations without charging shared keys twice
    fn disposable_bytes(&self) -> usize {
        self.disposable_keys()
            .iter()
            .filter_map(|key| self.allocations.get(key))
            .map(Allocation::bytes)
            .sum()
    }
    // Project disposable bytes after adding incoming unprotected allocations
    fn projected_disposable(&self, incoming: &HashMap<Key, &Allocation>) -> usize {
        let mut keys = self.disposable_keys();
        keys.extend(
            incoming
                .keys()
                .filter(|key| !self.protected.contains_key(key))
                .copied(),
        );
        keys.iter()
            .filter_map(|key| {
                self.allocations
                    .get(key)
                    .or_else(|| incoming.get(key).copied())
            })
            .map(Allocation::bytes)
            .sum()
    }
    // Evict one
    fn evict_one(&mut self, older_than: Option<Instant>) -> bool {
        let index = self.entries.iter().position(|entry| {
            older_than.is_none_or(|cutoff| entry.used <= cutoff)
                && entry
                    .keys
                    .iter()
                    .any(|key| !self.protected.contains_key(key))
        });
        let Some(index) = index else {
            return false;
        };
        let entry = self.entries.remove(index).unwrap();
        if let Some(slot) = entry.slot.upgrade() {
            slot.evict();
            self.evictions += 1;
        }
        self.prune();
        true
    }
    // Construct room
    fn make_room(
        &mut self,
        mut limits: Limits,
        incoming: &[Allocation],
        reservation: Option<(u64, usize)>,
        cache: bool,
        retired_reservation: usize,
    ) -> Result<(), String> {
        self.prune();
        let incoming: HashMap<_, _> = incoming
            .iter()
            .map(|allocation| (allocation.key(), allocation))
            .collect();
        let old_reservation = reservation
            .and_then(|(id, _)| self.reservations.get(&id))
            .copied()
            .unwrap_or(0);
        let new_reservation = reservation.map_or(0, |(_, bytes)| bytes);
        let additional = || {
            incoming
                .iter()
                .filter(|(key, _)| !self.allocations.contains_key(key))
                .map(|(_, value)| value.bytes())
                .sum::<usize>()
        };
        let projected = self
            .live_bytes()
            .saturating_add(self.reserved_bytes())
            .saturating_sub(old_reservation)
            .saturating_add(new_reservation)
            .saturating_add(additional());
        // Growing preparation work leaves one maximum-sized hardware surface
        // available for frame production. Conversions/shrinks may keep their
        // already admitted occupancy, including concurrently in-flight output
        if reservation.is_some()
            && projected
                > self
                    .live_bytes()
                    .saturating_add(self.reserved_bytes())
                    .saturating_add(retired_reservation)
        {
            limits.hard = limits.hard.saturating_sub((limits.hard / 8).min(8 * MIB));
        }
        let soft = cache && self.projected_disposable(&incoming) > limits.high;
        let hard_pressure = projected > limits.hard;
        let hard_target = if hard_pressure {
            limits
                .hard
                .saturating_sub(limits.high.saturating_sub(limits.low))
        } else {
            limits.hard
        };
        loop {
            let additional_count = incoming
                .keys()
                .filter(|key| !self.allocations.contains_key(key))
                .count();
            let additional_bytes: usize = incoming
                .iter()
                .filter(|(key, _)| !self.allocations.contains_key(key))
                .map(|(_, value)| value.bytes())
                .sum();
            let projected = self
                .live_bytes()
                .saturating_add(self.reserved_bytes())
                .saturating_sub(old_reservation)
                .saturating_add(new_reservation)
                .saturating_add(additional_bytes);
            let capacity_ok = self.allocations.len() + additional_count <= MAX_ALLOCATIONS
                && (!cache
                    || (self.entries.len() < MAX_SLOTS
                        && self
                            .entries
                            .iter()
                            .map(|entry| entry.keys.len())
                            .sum::<usize>()
                            + incoming.len()
                            <= MAX_SLOT_KEYS));
            let soft_ok = !soft || self.projected_disposable(&incoming) <= limits.low;
            if projected <= hard_target && capacity_ok && soft_ok {
                return Ok(());
            }
            if !self.evict_one(None) {
                if projected <= limits.hard && capacity_ok {
                    return Ok(());
                }
                self.rejections += 1;
                return Err("Media memory budget is held by active playback, in-flight frames or pending preparation; finish or cancel unused work and retry".into());
            }
        }
    }
    // Track the media budget resource
    fn track(&mut self, allocations: Vec<Allocation>) {
        for allocation in allocations {
            self.allocations.insert(allocation.key(), allocation);
        }
        self.peak = self.peak.max(self.live_bytes() + self.reserved_bytes());
    }
}
pub(crate) struct MediaBudget {
    state: Mutex<State>,
    limits: Limits,
}
impl Default for MediaBudget {
    // Construct the default media budget value
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            limits: Limits::default(),
        }
    }
}
impl MediaBudget {
    // Tighten cache eviction targets when memory pressure is active
    fn limits(&self, state: &State) -> Limits {
        if state.pressure {
            Limits {
                high: self.limits.high.min(16 * MIB),
                low: self.limits.low.min(8 * MIB),
                idle: 0,
                ..self.limits
            }
        } else {
            self.limits
        }
    }
    // Admit a media budget request after its checks
    pub(crate) fn admit(self: &Arc<Self>, value: Value) -> Result<Arc<Slot>, String> {
        let allocations = value.allocations();
        let mut state = self.state.lock().unwrap();
        let limits = self.limits(&state);
        state.make_room(limits, &allocations, None, true, 0)?;
        state.sequence += 1;
        let id = state.sequence;
        let slot = Arc::new(Slot {
            value: Mutex::new(Some(value)),
            budget: Arc::downgrade(self),
            id,
        });
        let keys = allocations
            .iter()
            .map(Allocation::key)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        state.track(allocations);
        state.entries.push_back(Entry {
            id,
            slot: Arc::downgrade(&slot),
            keys,
            used: Instant::now(),
        });
        Ok(slot)
    }
    // Refresh the media budget entry’s activity marker
    fn touch(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state.entries.iter().position(|entry| entry.id == id) {
            let mut entry = state.entries.remove(index).unwrap();
            entry.used = Instant::now();
            state.entries.push_back(entry);
        }
    }
    // Reserve capacity for media budget
    pub(crate) fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Reservation, String> {
        let mut state = self.state.lock().unwrap();
        if state.reservations.len() >= MAX_RESERVATIONS {
            state.rejections += 1;
            return Err("Too many pending media reservations".into());
        }
        state.sequence += 1;
        let id = state.sequence;
        let limits = self.limits(&state);
        state.make_room(limits, &[], Some((id, bytes)), false, 0)?;
        state.reservations.insert(id, bytes);
        state.peak = state.peak.max(state.live_bytes() + state.reserved_bytes());
        Ok(Reservation {
            budget: self.clone(),
            id,
        })
    }
    // Protect the active media budget resource
    pub(crate) fn protect(self: &Arc<Self>, clip: Arc<Clip>) -> Result<ProtectedClip, String> {
        let allocations = Value::Clip(clip.clone()).allocations();
        let mut state = self.state.lock().unwrap();
        let limits = self.limits(&state);
        state.make_room(limits, &allocations, None, false, 0)?;
        let keys: HashSet<_> = allocations.iter().map(Allocation::key).collect();
        state.track(allocations);
        for key in &keys {
            *state.protected.entry(*key).or_default() += 1;
        }
        Ok(ProtectedClip {
            clip,
            _protection: Protection {
                budget: self.clone(),
                keys,
            },
        })
    }
    // Trim idle
    pub(crate) fn trim_idle(&self) {
        self.trim(false, None);
    }
    // Trim pressure
    pub(crate) fn trim_pressure(&self) {
        self.trim(true, None);
    }
    // Maintain the media budget background state
    pub(crate) fn maintain(&self) {
        if let Some(cutoff) = Instant::now().checked_sub(IDLE_AGE) {
            self.trim(false, Some(cutoff));
        }
    }
    /// Available-memory watermarks are deliberately hysteretic. This is a
    /// cross-platform pressure hint, not a claim about process RSS or OS jetsam
    pub(crate) fn observe_available_memory(&self, available: u64, total: u64) {
        if total == 0 {
            return;
        }
        let changed = {
            let mut state = self.state.lock().unwrap();
            let next = if state.pressure {
                available <= (total / 10).max(512 * MIB as u64)
            } else {
                available < (total / 20).max(256 * MIB as u64)
            };
            let entered = next && !state.pressure;
            state.pressure = next;
            entered
        };
        if changed {
            self.trim_pressure();
        }
    }
    // Trim retained media budget entries
    fn trim(&self, pressure: bool, cutoff: Option<Instant>) {
        let mut state = self.state.lock().unwrap();
        state.prune();
        let target = if pressure {
            0
        } else {
            self.limits(&state).idle
        };
        while state.disposable_bytes() > target && state.evict_one(cutoff) {}
    }
    // Prune stale ownership and report live, protected, disposable and reserved memory charges
    pub(crate) fn stats(&self) -> serde_json::Value {
        let mut state = self.state.lock().unwrap();
        state.prune();
        let protected: usize = state
            .protected
            .keys()
            .filter_map(|key| state.allocations.get(key))
            .map(Allocation::bytes)
            .sum();
        serde_json::json!({"unique_live_bytes":state.live_bytes(),"reserved_preparation_bytes":state.reserved_bytes(),"combined_bytes":state.live_bytes()+state.reserved_bytes(),"protected_playback_bytes":protected,"disposable_cache_bytes":state.disposable_bytes(),"hard_limit_bytes":self.limits.hard,"preparation_admission_limit_bytes":self.limits.hard-(self.limits.hard/8).min(8*MIB),"cache_high_bytes":self.limits.high,"cache_low_bytes":self.limits.low,"effective_cache_high_bytes":self.limits(&state).high,"effective_cache_low_bytes":self.limits(&state).low,"idle_target_bytes":self.limits.idle,"memory_pressure":state.pressure,"tracked_allocations":state.allocations.len(),"allocation_record_capacity":state.allocations.capacity(),"cache_slot_key_count":state.entries.iter().map(|entry|entry.keys.len()).sum::<usize>(),"cache_slots":state.entries.len(),"reservations":state.reservations.len(),"evictions":state.evictions,"rejections":state.rejections,"peak_combined_bytes":state.peak})
    }
}
pub(crate) struct Reservation {
    budget: Arc<MediaBudget>,
    id: u64,
}
impl Reservation {
    // Resize the media budget value
    pub(crate) fn resize(&self, bytes: usize) -> Result<(), String> {
        let mut state = self.budget.state.lock().unwrap();
        let limits = self.budget.limits(&state);
        state.make_room(limits, &[], Some((self.id, bytes)), false, 0)?;
        state.reservations.insert(self.id, bytes);
        state.peak = state.peak.max(state.live_bytes() + state.reserved_bytes());
        Ok(())
    }
    /// Transfer validated batch ownership into its job without double charging
    /// or leaving a cancellation window with uncharged encoded buffers
    pub(crate) fn resize_from(&self, temporary: &Reservation, bytes: usize) -> Result<(), String> {
        if !Arc::ptr_eq(&self.budget, &temporary.budget) || self.id == temporary.id {
            return Err("Invalid media reservation transfer".into());
        }
        let mut state = self.budget.state.lock().unwrap();
        let previous = state
            .reservations
            .remove(&temporary.id)
            .ok_or("Media reservation expired")?;
        let limits = self.budget.limits(&state);
        if let Err(error) = state.make_room(limits, &[], Some((self.id, bytes)), false, previous) {
            state.reservations.insert(temporary.id, previous);
            return Err(error);
        }
        state.reservations.insert(self.id, bytes);
        state.peak = state.peak.max(state.live_bytes() + state.reserved_bytes());
        Ok(())
    }
    /// Atomically replace raw pending storage with the unique immutable payloads
    /// it became. Keep source/other temporary capacity charged until its drop
    pub(crate) fn track_clip(&self, clip: &Arc<Clip>, remaining: usize) -> Result<(), String> {
        let allocations = Value::Clip(clip.clone()).allocations();
        let mut state = self.budget.state.lock().unwrap();
        let limits = self.budget.limits(&state);
        state.make_room(limits, &allocations, Some((self.id, remaining)), false, 0)?;
        state.reservations.insert(self.id, remaining);
        state.track(allocations);
        Ok(())
    }
    // Track index
    pub(crate) fn track_index(&self, index: &Arc<Vec<u64>>) -> Result<(), String> {
        let allocations = Value::Index(index.clone()).allocations();
        let mut state = self.budget.state.lock().unwrap();
        let limits = self.budget.limits(&state);
        state.make_room(limits, &allocations, Some((self.id, 0)), false, 0)?;
        state.reservations.insert(self.id, 0);
        state.track(allocations);
        Ok(())
    }
}
impl Drop for Reservation {
    // Release this media budget owner when it leaves scope
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.reservations.remove(&self.id);
        state.prune();
    }
}
pub(crate) struct ProtectedClip {
    pub(crate) clip: Arc<Clip>,
    _protection: Protection,
}
struct Protection {
    budget: Arc<MediaBudget>,
    keys: HashSet<Key>,
}
impl Drop for Protection {
    // Release this media budget owner when it leaves scope
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        for key in &self.keys {
            if let Some(count) = state.protected.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    state.protected.remove(key);
                }
            }
        }
        state.prune();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Build an isolated media budget with test-selected admission and eviction limits
    fn budget(hard: usize, high: usize, low: usize, idle: usize) -> Arc<MediaBudget> {
        Arc::new(MediaBudget {
            state: Mutex::new(State::default()),
            limits: Limits {
                hard,
                high,
                low,
                idle,
            },
        })
    }
    // Create a single-frame clip fixture with a controlled byte charge
    fn clip(seed: u8, bytes: usize) -> Arc<Clip> {
        Arc::new(Clip::new("gif", vec![vec![seed; bytes]], vec![40]).unwrap())
    }
    // Read a numeric media-budget diagnostic for test assertions
    fn number(budget: &MediaBudget, key: &str) -> usize {
        budget.stats()[key].as_u64().unwrap() as usize
    }
    #[test]
    // Verify that shared allocations protected playback and in flight frames have distinct owners
    fn shared_allocations_protected_playback_and_in_flight_frames_have_distinct_owners() {
        let budget = budget(2 * MIB, MIB, MIB / 2, MIB / 8);
        let clip = clip(151, MIB / 2);
        let expected = clip.frames[0].len() + clip.metadata_capacity_bytes();
        let cached = budget.admit(Value::Clip(clip.clone())).unwrap();
        let same = budget.admit(Value::Clip(clip.clone())).unwrap();
        let held = budget.protect(clip.clone()).unwrap();
        let in_flight = clip.frames[0].clone();
        drop(clip);
        assert_eq!(number(&budget, "unique_live_bytes"), expected);
        assert_eq!(number(&budget, "protected_playback_bytes"), expected);
        budget.trim_pressure();
        assert!(cached.clip().is_some());
        assert!(same.clip().is_some());
        assert_eq!(held.clip.frames[0][0], 151);
        drop(held);
        budget.trim_pressure();
        assert!(cached.clip().is_none());
        assert_eq!(number(&budget, "unique_live_bytes"), MIB / 2);
        assert_eq!(number(&budget, "protected_playback_bytes"), 0);
        drop(in_flight);
        assert_eq!(number(&budget, "unique_live_bytes"), 0);
        assert_eq!(number(&budget, "tracked_allocations"), 0);
        assert_eq!(number(&budget, "cache_slots"), 0);
    }
    #[test]
    // Verify that failed reservation growth and batch transfer preserve existing ownership
    fn failed_reservation_growth_and_batch_transfer_preserve_existing_ownership() {
        let budget = budget(MIB, MIB / 2, MIB / 4, 0);
        let held = budget.protect(clip(152, 600 * 1024)).unwrap();
        let job = budget.reserve(100 * 1024).unwrap();
        let batch = budget.reserve(100 * 1024).unwrap();
        assert!(job.resize_from(&batch, 600 * 1024).is_err());
        assert_eq!(number(&budget, "reserved_preparation_bytes"), 200 * 1024);
        assert_eq!(number(&budget, "reservations"), 2);
        job.resize_from(&batch, 200 * 1024).unwrap();
        drop(batch);
        assert_eq!(number(&budget, "reservations"), 1);
        assert_eq!(number(&budget, "reserved_preparation_bytes"), 200 * 1024);
        let prepared = clip(153, 180 * 1024);
        job.track_clip(&prepared, 20 * 1024).unwrap();
        assert_eq!(number(&budget, "reserved_preparation_bytes"), 20 * 1024);
        assert!(number(&budget, "combined_bytes") < MIB);
        let too_large = clip(154, 500 * 1024);
        assert!(job.track_clip(&too_large, 0).is_err());
        assert_eq!(number(&budget, "reserved_preparation_bytes"), 20 * 1024);
        assert_eq!(held.clip.frames[0][0], 152);
        drop((too_large, prepared, job, held));
        assert_eq!(number(&budget, "combined_bytes"), 0);
        assert_eq!(number(&budget, "reservations"), 0);
    }
    #[test]
    // Verify that preparation growth keeps render headroom and shrink succeeds with output in flight
    fn preparation_growth_keeps_render_headroom_and_shrink_succeeds_with_output_in_flight() {
        let budget = budget(MIB, MIB / 2, MIB / 4, 0);
        let reservation = budget.reserve(7 * MIB / 8).unwrap();
        assert!(budget.reserve(1).is_err());
        let frame = Arc::new(vec![161; MIB / 8].into_boxed_slice());
        let slot = budget.admit(Value::Frame(frame.clone())).unwrap();
        assert_eq!(number(&budget, "combined_bytes"), MIB);
        reservation.resize(7 * MIB / 8).unwrap();
        reservation.resize(6 * MIB / 8).unwrap();
        assert_eq!(frame[0], 161);
        drop((slot, frame, reservation));
        assert_eq!(number(&budget, "combined_bytes"), 0);
    }
    #[test]
    // Verify that pending reservations block admission and cancel releases capacity
    fn pending_reservations_block_admission_and_cancel_releases_capacity() {
        let budget = budget(MIB, MIB / 2, MIB / 4, 0);
        let first = budget.reserve(700 * 1024).unwrap();
        assert!(budget.reserve(400 * 1024).is_err());
        let frame = Arc::new(vec![155; 400 * 1024].into_boxed_slice());
        assert!(budget.admit(Value::Frame(frame.clone())).is_err());
        drop(first);
        let slot = budget.admit(Value::Frame(frame.clone())).unwrap();
        assert_eq!(slot.frame().unwrap()[0], 155);
        drop((slot, frame));
        assert_eq!(number(&budget, "combined_bytes"), 0);
    }
    #[test]
    // Verify that cached already tracked allocations still trigger disposable hysteresis
    fn cached_already_tracked_allocations_still_trigger_disposable_hysteresis() {
        let budget = budget(4 * MIB, MIB, MIB / 2, 0);
        let first = budget
            .admit(Value::Frame(Arc::new(
                vec![156; 700 * 1024].into_boxed_slice(),
            )))
            .unwrap();
        let pending = budget.reserve(700 * 1024).unwrap();
        let second = Arc::new(vec![1u64; 80 * 1024]);
        pending.track_index(&second).unwrap();
        let second_slot = budget.admit(Value::Index(second.clone())).unwrap();
        assert!(first.frame().is_none());
        assert!(second_slot.index().is_some());
        assert_eq!(
            number(&budget, "disposable_cache_bytes"),
            second.capacity() * 8
        );
    }
    #[test]
    // Verify that idle age and system pressure watermarks do not evict active playback
    fn idle_age_and_system_pressure_watermarks_do_not_evict_active_playback() {
        let budget = budget(2 * MIB, MIB, MIB / 2, MIB / 8);
        let active = budget.protect(clip(157, MIB / 4)).unwrap();
        let active_slot = budget.admit(Value::Clip(active.clip.clone())).unwrap();
        let recent = budget
            .admit(Value::Frame(Arc::new(
                vec![158; MIB / 2].into_boxed_slice(),
            )))
            .unwrap();
        budget.maintain();
        assert!(recent.frame().is_some());
        for entry in &mut budget.state.lock().unwrap().entries {
            entry.used = Instant::now() - Duration::from_secs(31);
        }
        budget.maintain();
        assert!(recent.frame().is_none());
        let other = budget
            .admit(Value::Frame(Arc::new(
                vec![159; MIB / 4].into_boxed_slice(),
            )))
            .unwrap();
        budget.observe_available_memory(200 * MIB as u64, 8 * 1024 * MIB as u64);
        assert_eq!(budget.stats()["memory_pressure"], true);
        assert!(other.frame().is_none());
        assert!(active_slot.clip().is_some());
        budget.observe_available_memory(600 * MIB as u64, 8 * 1024 * MIB as u64);
        assert_eq!(budget.stats()["memory_pressure"], true);
        budget.observe_available_memory(900 * MIB as u64, 8 * 1024 * MIB as u64);
        assert_eq!(budget.stats()["memory_pressure"], false);
        assert_eq!(active.clip.frames[0][0], 157);
    }
    #[test]
    // Verify that weak records and high water capacity retire and reservation count is bounded
    fn weak_records_and_high_water_capacity_retire_and_reservation_count_is_bounded() {
        let budget = budget(4 * MIB, 3 * MIB, 2 * MIB, 0);
        let mut slots = vec![];
        for _ in 0..2000 {
            slots.push(
                budget
                    .admit(Value::Frame(Arc::new(vec![160; 16].into_boxed_slice())))
                    .unwrap(),
            );
        }
        assert_eq!(number(&budget, "tracked_allocations"), 2000);
        assert!(number(&budget, "allocation_record_capacity") > 1024);
        drop(slots);
        assert_eq!(number(&budget, "tracked_allocations"), 0);
        assert_eq!(number(&budget, "cache_slots"), 0);
        assert_eq!(number(&budget, "cache_slot_key_count"), 0);
        assert!(number(&budget, "allocation_record_capacity") <= 1024);
        let reservations: Vec<_> = (0..MAX_RESERVATIONS)
            .map(|_| budget.reserve(0).unwrap())
            .collect();
        assert!(budget.reserve(0).is_err());
        drop(reservations);
        assert_eq!(number(&budget, "reservations"), 0);
    }
    #[test]
    // Verify that concurrent cache reads pressure and reservations complete without lock inversion
    fn concurrent_cache_reads_pressure_and_reservations_complete_without_lock_inversion() {
        let budget = budget(8 * MIB, 4 * MIB, 2 * MIB, MIB);
        let threads: Vec<_> = (0..4)
            .map(|index| {
                let budget = budget.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        let held = budget.protect(clip(170 + index, 16 * 1024)).unwrap();
                        let slot = budget.admit(Value::Clip(held.clip.clone())).unwrap();
                        let reservation = budget.reserve(32 * 1024).unwrap();
                        let _ = slot.clip();
                        budget.trim_pressure();
                        assert_eq!(held.clip.frames[0][0], 170 + index);
                        drop((slot, held, reservation));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        budget.trim_pressure();
        assert_eq!(number(&budget, "combined_bytes"), 0);
    }
}
