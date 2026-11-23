//! Brief, bounded authorization history for responses already in transit when
//! a context disappears. These records authorize discarding, never execution.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_CONTEXTS: usize = 1024;
const MAX_BYTES: usize = 256 * 1024;
const GRACE: Duration = Duration::from_secs(30);

struct Retired {
    session: String,
    context: String,
    expires: Instant,
}
impl Retired {
    // Read the byte charge of retained data
    fn bytes(&self) -> usize {
        self.session.len() + self.context.len()
    }
}

#[derive(Default)]
pub(super) struct RetiredContexts {
    entries: VecDeque<Retired>,
    bytes: usize,
}
impl RetiredContexts {
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.entries.front().map(|entry| entry.expires)
    }

    // Remove retained entries whose deadlines have elapsed
    pub(super) fn expire(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.expires <= now)
        {
            self.bytes -= self.entries.pop_front().unwrap().bytes();
        }
    }
    // Insert for the current operation
    pub(super) fn insert(&mut self, session: &str, context: &str, now: Instant) {
        self.expire(now);
        let bytes = session.len().saturating_add(context.len());
        if bytes > MAX_BYTES {
            return;
        }
        self.entries.retain(|entry| {
            let keep = entry.session != session || entry.context != context;
            if !keep {
                self.bytes -= entry.bytes();
            }
            keep
        });
        while self.entries.len() >= MAX_CONTEXTS || self.bytes + bytes > MAX_BYTES {
            self.bytes -= self.entries.pop_front().unwrap().bytes();
        }
        self.entries.push_back(Retired {
            session: session.into(),
            context: context.into(),
            expires: now + GRACE,
        });
        self.bytes += bytes;
    }
    // Check whether the retained set contains the context
    pub(super) fn contains(&mut self, session: &str, context: &str, now: Instant) -> bool {
        self.expire(now);
        self.entries
            .iter()
            .any(|entry| entry.session == session && entry.context == context)
    }
    // Clear session
    pub(super) fn clear_session(&mut self, session: &str) {
        self.entries.retain(|entry| {
            if entry.session == session {
                self.bytes -= entry.bytes();
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify retirement history is exact expiring and bounded
    fn retirement_history_is_exact_expiring_and_bounded() {
        let mut history = RetiredContexts::default();
        let now = Instant::now();
        history.insert("session", "context", now);
        assert!(history.contains("session", "context", now));
        assert!(!history.contains("replacement", "context", now));
        assert!(!history.contains("session", "unknown", now));
        assert!(!history.contains("session", "context", now + GRACE));
        assert_eq!(history.bytes, 0);
        for index in 0..MAX_CONTEXTS * 2 {
            history.insert("session", &index.to_string(), now);
        }
        assert_eq!(history.entries.len(), MAX_CONTEXTS);
        for index in 0..MAX_CONTEXTS {
            history.insert("session", &format!("{index}{}", "é".repeat(1000)), now);
            assert!(history.bytes <= MAX_BYTES);
        }
        history.insert("session", &"x".repeat(MAX_BYTES), now);
        assert!(history.bytes <= MAX_BYTES);
        history.clear_session("session");
        assert!(history.entries.is_empty());
        assert_eq!(history.bytes, 0);
    }
}
