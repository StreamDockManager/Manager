//! Admission before spawning threads. Registered WebSocket sessions leave the
//! handshake budget; their lifetime remains owned by authenticated host grants.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub(super) const HANDSHAKES: usize = 32;
pub(super) const FILE_TRANSFERS: usize = 8;

pub(super) struct Budget {
    active: AtomicUsize,
    limit: usize,
}
impl Budget {
    // Construct the value and initialize its state
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            limit,
        })
    }
    // Acquire for the current operation
    pub(super) fn acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.limit {
                return None;
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => active = current,
            }
        }
        Some(Permit(self.clone()))
    }
}
pub(super) struct Permit(Arc<Budget>);
impl Drop for Permit {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(super) struct Admission {
    pub(super) handshakes: Arc<Budget>,
    pub(super) files: Arc<Budget>,
}
impl Default for Admission {
    // Construct the default value
    fn default() -> Self {
        Self {
            handshakes: Budget::new(HANDSHAKES),
            files: Budget::new(FILE_TRANSFERS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify independent budgets plateau and release even during unwind
    fn independent_budgets_plateau_and_release_even_during_unwind() {
        let admission = Admission::default();
        for repetition in 0..5 {
            let files: Vec<_> = (0..FILE_TRANSFERS)
                .map(|_| admission.files.acquire().unwrap())
                .collect();
            assert!(admission.files.acquire().is_none());
            let handshakes: Vec<_> = (0..HANDSHAKES)
                .map(|_| admission.handshakes.acquire().unwrap())
                .collect();
            assert!(admission.handshakes.acquire().is_none());
            assert_eq!(
                admission.files.active.load(Ordering::Acquire),
                FILE_TRANSFERS
            );
            assert_eq!(
                admission.handshakes.active.load(Ordering::Acquire),
                HANDSHAKES
            );
            drop(handshakes);
            // Slow static transfers cannot consume the next registration's slot
            assert!(admission.handshakes.acquire().is_some());
            drop(files);
            assert_eq!(admission.handshakes.active.load(Ordering::Acquire), 0);
            assert_eq!(admission.files.active.load(Ordering::Acquire), 0);
            eprintln!("SERVER_ADMISSION repetition={repetition} pending_max={HANDSHAKES} transfers_max={FILE_TRANSFERS} retired=0");
        }
        let _ = std::panic::catch_unwind(|| {
            let _permit = admission.files.acquire().unwrap();
            panic!("fixture failure");
        });
        assert_eq!(admission.files.active.load(Ordering::Acquire), 0);
    }
}
