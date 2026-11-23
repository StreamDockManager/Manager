//! Backup timing only; SQLite and admission remain owned by the existing worker.
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_secs(30);
const MAX_DIRTY: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(crate) struct Schedule {
    first: Option<Instant>,
    last: Option<Instant>,
    retry: Option<Instant>,
}
impl Schedule {
    pub(crate) fn changed(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }
    // None means park until a job, admission transition or shutdown, never poll.
    pub(crate) fn deadline(&self, runnable: bool) -> Option<Instant> {
        if !runnable {
            return None;
        }
        let quiet = self.last? + QUIET;
        let ceiling = self.first? + MAX_DIRTY;
        let eligible = quiet.min(ceiling);
        Some(self.retry.map_or(eligible, |retry| eligible.max(retry)))
    }
    pub(crate) fn due(&self, now: Instant, runnable: bool, shutdown: bool) -> bool {
        runnable
            && self.first.is_some()
            && (shutdown || self.deadline(true).is_some_and(|at| at <= now))
    }
    pub(crate) fn attempted(&mut self, now: Instant, success: bool) {
        if success {
            self.first = None;
            self.last = None;
        }
        // Preserve the existing post-attempt backoff, including successful attempts.
        self.retry = Some(now + QUIET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clean_and_gated_workers_have_no_timer_wakes() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        let mut timers = 0;
        for second in 0..60 {
            timers += usize::from(
                schedule
                    .deadline(true)
                    .is_some_and(|at| at <= now + Duration::from_secs(second)),
            );
        }
        assert_eq!(
            timers, 0,
            "old fixed one-second policy scheduled 60 idle wakes"
        );
        schedule.changed(now);
        assert_eq!(schedule.deadline(false), None);
        assert!(!schedule.due(now + MAX_DIRTY, false, true));
        assert!(
            schedule.due(now + MAX_DIRTY, true, false),
            "reopening admission makes overdue work runnable"
        );
    }
    #[test]
    fn quiet_and_continuous_changes_keep_the_original_ceiling() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        schedule.changed(now);
        assert_eq!(schedule.deadline(true), Some(now + QUIET));
        assert!(!schedule.due(now + QUIET - Duration::from_nanos(1), true, false));
        assert!(schedule.due(now + QUIET, true, false));
        for second in (20..300).step_by(20) {
            schedule.changed(now + Duration::from_secs(second));
        }
        assert_eq!(schedule.deadline(true), Some(now + MAX_DIRTY));
    }
    #[test]
    fn retry_does_not_spin_and_shutdown_overrides_retry_only_when_authorized() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        schedule.changed(now);
        schedule.attempted(now + MAX_DIRTY, false);
        assert_eq!(schedule.deadline(true), Some(now + MAX_DIRTY + QUIET));
        assert!(!schedule.due(now + MAX_DIRTY, true, false));
        assert!(schedule.due(now + MAX_DIRTY, true, true));
        schedule.attempted(now + MAX_DIRTY + QUIET, true);
        assert_eq!(schedule.deadline(true), None);
        assert!(!schedule.due(now + MAX_DIRTY + QUIET, true, true));
    }
}
