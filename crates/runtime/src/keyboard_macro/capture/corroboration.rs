//! Independent, bounded Raw Input/low-level-hook agreement. A hook handle is not
//! a continuity proof. A missing or reordered observation fails the whole draft
use super::CaptureError;

pub(super) const AGREEMENT_MS: u64 = 250;
const CAPACITY: usize = 128;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Observation {
    pub scan: u16,
    pub extended: bool,
    pub down: bool,
    pub elapsed_ms: u64,
}
impl Observation {
    // Report the same key event condition for corroboration
    fn same_key_event(self, other: Self) -> bool {
        self.scan == other.scan && self.extended == other.extended && self.down == other.down
    }
}
struct Pending {
    values: [Option<Observation>; CAPACITY],
    head: usize,
    len: usize,
}
impl Default for Pending {
    // Construct the default corroboration value
    fn default() -> Self {
        Self {
            values: [None; CAPACITY],
            head: 0,
            len: 0,
        }
    }
}
impl Pending {
    // Inspect the oldest queued observation without consuming it
    fn front(&self) -> Option<Observation> {
        self.values[self.head]
    }
    // Append a corroboration entry
    fn push(&mut self, observation: Observation) -> Result<(), CaptureError> {
        if self.len == CAPACITY {
            return Err(CaptureError::BufferOverflow);
        }
        self.values[(self.head + self.len) % CAPACITY] = Some(observation);
        self.len += 1;
        Ok(())
    }
    // Remove the next corroboration entry
    fn pop(&mut self) -> Option<Observation> {
        if self.len == 0 {
            return None;
        }
        let value = self.values[self.head].take();
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        value
    }
}
#[derive(Default)]
pub(super) struct Corroboration {
    hooks: Pending,
    raw: Pending,
}
impl Corroboration {
    // Queue a low-level hook observation for corroboration
    pub fn hook(&mut self, observation: Observation) -> Result<(), CaptureError> {
        self.hooks.push(observation)
    }
    // Queue a raw-input observation for corroboration
    pub fn raw(&mut self, observation: Observation) -> Result<(), CaptureError> {
        self.raw.push(observation)
    }
    // Report whether corroboration has no entries
    pub fn is_empty(&self) -> bool {
        self.hooks.len == 0 && self.raw.len == 0
    }
    // Match hook and raw observations within the agreement window or report failure
    pub fn next(&mut self, now_ms: u64) -> Result<Option<Observation>, CaptureError> {
        match (self.hooks.front(), self.raw.front()) {
            (Some(hook), Some(raw)) => {
                if !hook.same_key_event(raw)
                    || hook.elapsed_ms.abs_diff(raw.elapsed_ms) > AGREEMENT_MS
                {
                    return Err(CaptureError::InputMismatch);
                }
                self.hooks.pop();
                self.raw.pop();
                Ok(Some(hook))
            }
            (Some(value), None) | (None, Some(value)) => {
                if now_ms.saturating_sub(value.elapsed_ms) > AGREEMENT_MS {
                    Err(CaptureError::InputMismatch)
                } else {
                    Ok(None)
                }
            }
            (None, None) => Ok(None),
        }
    }
    // Finalize the pending corroboration operation
    pub fn finish(&self) -> Result<(), CaptureError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(CaptureError::InputMismatch)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Construct the scan-code observation used by the test
    fn observation(scan: u16, elapsed_ms: u64) -> Observation {
        Observation {
            scan,
            extended: false,
            down: true,
            elapsed_ms,
        }
    }
    #[test]
    // Verify that neither source alone can commit a recording
    fn neither_source_alone_can_commit_a_recording() {
        for raw_first in [false, true] {
            let mut value = Corroboration::default();
            if raw_first {
                value.raw(observation(1, 10)).unwrap();
            } else {
                value.hook(observation(1, 10)).unwrap();
            }
            assert!(value.next(20).unwrap().is_none());
            assert_eq!(value.finish(), Err(CaptureError::InputMismatch));
            assert!(matches!(value.next(261), Err(CaptureError::InputMismatch)));
        }
    }
    #[test]
    // Verify that agreement preserves hook timestamp and order
    fn agreement_preserves_hook_timestamp_and_order() {
        let mut value = Corroboration::default();
        value.raw(observation(1, 20)).unwrap();
        value.hook(observation(1, 10)).unwrap();
        assert_eq!(value.next(30).unwrap().unwrap().elapsed_ms, 10);
        assert!(value.finish().is_ok());
    }
    #[test]
    // Verify that mismatches and delayed matches are not resynchronized
    fn mismatches_and_delayed_matches_are_not_resynchronized() {
        let mut value = Corroboration::default();
        value.hook(observation(1, 0)).unwrap();
        value.raw(observation(2, 0)).unwrap();
        assert!(matches!(value.next(0), Err(CaptureError::InputMismatch)));
        let mut value = Corroboration::default();
        value.hook(observation(1, 0)).unwrap();
        value.raw(observation(1, AGREEMENT_MS + 1)).unwrap();
        assert!(matches!(
            value.next(AGREEMENT_MS + 1),
            Err(CaptureError::InputMismatch)
        ));
    }
    #[test]
    // Verify that queue is bounded and wraparound is lossless
    fn queue_is_bounded_and_wraparound_is_lossless() {
        let mut value = Corroboration::default();
        for _ in 0..CAPACITY {
            value.hook(observation(1, 0)).unwrap();
        }
        assert_eq!(
            value.hook(observation(1, 0)),
            Err(CaptureError::BufferOverflow)
        );
        for index in 0..CAPACITY * 3 {
            value.raw(observation(1, 0)).unwrap();
            assert!(value.next(0).unwrap().is_some());
            if index < CAPACITY * 2 {
                value.hook(observation(1, 0)).unwrap();
            }
        }
        assert!(value.finish().is_ok());
    }
}
