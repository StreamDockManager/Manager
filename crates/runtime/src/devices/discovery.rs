//! Discovery deadlines are independent of repaint and plugin mailbox traffic
use std::time::{Duration, Instant};

pub(super) struct Schedule {
    next: Instant,
    retry: Duration,
    notifications: bool,
    enabled: bool,
}
impl Schedule {
    // Start with an immediate scan and conservative polling until observation is installed
    pub(super) fn new(now: Instant) -> Self {
        Self {
            next: now,
            retry: Duration::from_secs(1),
            notifications: false,
            enabled: false,
        }
    }
    // Use slower safety scans only after native registration succeeds
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn observe(&mut self, available: bool) {
        if self.notifications && !available {
            self.next = self.next.min(Instant::now() + Duration::from_secs(1));
        }
        self.notifications = available;
    }
    // Reconcile admission changes and coalesced native topology notifications
    pub(super) fn update(&mut self, now: Instant, enabled: bool, changed: bool) -> bool {
        if enabled && (!self.enabled || changed) {
            self.request(now);
        }
        self.enabled = enabled;
        enabled && now >= self.next
    }
    // Explicit reconnect requests also reset transient failure backoff
    pub(super) fn request(&mut self, now: Instant) {
        self.next = now;
        self.retry = Duration::from_secs(1);
    }
    // Worker failures retry after a bounded delay rather than spinning on repeated successful opens
    pub(super) fn retry_connection(&mut self, now: Instant) {
        self.next = now + Duration::from_secs(1);
    }
    // Retry failures without requiring replug while bounding repeated enumeration cost
    pub(super) fn complete(&mut self, now: Instant, failed: bool) {
        let interval = if !self.notifications {
            Duration::from_secs(1)
        } else if failed {
            let delay = self.retry;
            self.retry = (delay * 2).min(Duration::from_secs(30));
            delay
        } else {
            self.retry = Duration::from_secs(1);
            Duration::from_secs(30)
        };
        self.next = now + interval;
    }
    // Disabled devices never force an expired scan deadline into a busy loop
    pub(super) fn remaining(&self, now: Instant, enabled: bool) -> Duration {
        if !enabled {
            Duration::from_secs(if self.notifications { 5 } else { 1 })
        } else {
            self.next.saturating_duration_since(now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Initial discovery and notification bursts produce one reconciliation per consumed signal
    fn notifications_and_initial_scan() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        s.observe(true);
        assert!(s.update(now, true, false));
        s.complete(now, false);
        assert!(!s.update(now + Duration::from_secs(1), true, false));
        assert_eq!(s.remaining(now, true), Duration::from_secs(30));
        assert!(s.update(now, true, true));
        s.complete(now, false);
        assert!(!s.update(now, true, false));
    }
    #[test]
    // Failed opens retry exponentially up to the safety interval and success resets backoff
    fn retries_are_bounded_and_recover_without_events() {
        let mut now = Instant::now();
        let mut s = Schedule::new(now);
        s.observe(true);
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            assert!(s.update(now, true, false));
            s.complete(now, true);
            assert_eq!(s.remaining(now, true), Duration::from_secs(seconds));
            now += Duration::from_secs(seconds);
        }
        s.complete(now, false);
        s.request(now);
        s.complete(now, true);
        assert_eq!(s.remaining(now, true), Duration::from_secs(1));
    }
    #[test]
    // Permission resume and explicit wake bypass the slow deadline without spinning while blocked
    fn permission_and_wake_resume() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        s.observe(true);
        assert!(!s.update(now, false, true));
        assert_eq!(s.remaining(now, false), Duration::from_secs(5));
        assert!(s.update(now, true, false));
        s.complete(now, false);
        assert!(!s.update(now, false, false));
        assert!(s.update(now, true, false));
        s.complete(now, false);
        s.request(now);
        assert!(s.update(now, true, false));
    }
    #[test]
    // Retired workers reopen without a hotplug event and cannot create a tight retry loop
    fn retired_worker_and_observer_failure() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        s.observe(true);
        assert!(s.update(now, true, false));
        s.complete(now, false);
        s.retry_connection(now);
        assert!(!s.update(now, true, false));
        assert!(s.update(now + Duration::from_secs(1), true, false));
        s.complete(now, false);
        s.observe(false);
        assert!(s.remaining(Instant::now(), true) <= Duration::from_secs(1));
    }
    #[test]
    // Other platforms and failed native registration preserve the existing one-second fallback
    fn unavailable_observer_keeps_polling() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        s.complete(now, false);
        assert_eq!(s.remaining(now, true), Duration::from_secs(1));
        s.complete(now, true);
        assert_eq!(s.remaining(now, true), Duration::from_secs(1));
    }
}
