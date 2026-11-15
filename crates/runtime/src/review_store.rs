//! Bounded process-local review storage. Feature modules own validation and commits
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub(crate) trait ReviewSession {
    const LIMIT: usize;
    const FULL: &'static str;
    const LIFETIME: Option<Duration>;

    // Read the review creation time used to enforce its expiration
    fn created(&self) -> Instant;

    // Report the expired condition for review store
    fn expired(&self) -> bool {
        Self::LIFETIME.is_some_and(|lifetime| self.created().elapsed() >= lifetime)
    }
}

// Generate a fresh identifier for the new item
pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) struct ReviewStore<T> {
    entries: HashMap<String, T>,
}

impl<T> Default for ReviewStore<T> {
    // Construct the default review store value
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<T: ReviewSession> ReviewStore<T> {
    /// Preparation reclaims expired sessions, dropping their temporary files and leases
    /// Lookups deliberately do not: failed review/commit attempts retain their resources
    /// until cancellation or the next preparation, just as the individual features do
    pub(crate) fn check_capacity(&mut self) -> Result<(), String> {
        self.entries.retain(|_, session| !session.expired());
        self.check_limit()
    }

    // Check limit
    fn check_limit(&self) -> Result<(), String> {
        if self.entries.len() >= T::LIMIT {
            Err(T::FULL.into())
        } else {
            Ok(())
        }
    }

    // Insert an entry into review store
    pub(crate) fn insert(&mut self, id: String, session: T) -> Result<(), String> {
        self.check_limit()?;
        if self.entries.contains_key(&id) {
            return Err("Review identifier is already in use".into());
        }
        self.entries.insert(id, session);
        Ok(())
    }

    // Imports historically expire on the next preparation, not on lookup
    // Features with a commit deadline use active/active_mut instead
    pub(crate) fn get(&self, id: &str) -> Option<&T> {
        self.entries.get(id)
    }

    // Return mut
    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut T> {
        self.entries.get_mut(id)
    }

    // Report the active review store state
    pub(crate) fn active(&self, id: &str, missing: &str, expired: &str) -> Result<&T, String> {
        let session = self.get(id).ok_or_else(|| missing.to_string())?;
        if session.expired() {
            return Err(expired.into());
        }
        Ok(session)
    }

    // Borrow mutable active state for review store
    pub(crate) fn active_mut(
        &mut self,
        id: &str,
        missing: &str,
        expired: &str,
    ) -> Result<&mut T, String> {
        let session = self.get_mut(id).ok_or_else(|| missing.to_string())?;
        if session.expired() {
            return Err(expired.into());
        }
        Ok(session)
    }

    // Remove the selected review store entry
    pub(crate) fn remove(&mut self, id: &str) -> Option<T> {
        self.entries.remove(id)
    }

    // Check for key
    pub(crate) fn contains_key(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    // Report whether review store has no entries
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
impl<T> std::ops::Index<&String> for ReviewStore<T> {
    type Output = T;

    // Borrow the retained review entry indexed by its identifier
    fn index(&self, id: &String) -> &Self::Output {
        &self.entries[id]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Session {
        created: Instant,
        value: usize,
        drops: Arc<AtomicUsize>,
    }

    impl ReviewSession for Session {
        const LIMIT: usize = 2;
        const FULL: &'static str = "Close a review first";
        const LIFETIME: Option<Duration> = Some(Duration::from_secs(300));
        // Read the review creation time used to enforce its expiration
        fn created(&self) -> Instant {
            self.created
        }
    }
    impl Drop for Session {
        // Release this review store owner when it leaves scope
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    // Construct a review session with a shared drop counter for lifecycle tests
    fn session(drops: &Arc<AtomicUsize>) -> Session {
        Session {
            created: Instant::now(),
            value: 7,
            drops: drops.clone(),
        }
    }

    #[test]
    // Verify that capacity reclaims only expired sessions and drops their resources
    fn capacity_reclaims_only_expired_sessions_and_drops_their_resources() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut store = ReviewStore::default();
        store.insert("old".into(), session(&drops)).unwrap();
        store.insert("live".into(), session(&drops)).unwrap();
        assert_eq!(store.check_capacity().unwrap_err(), Session::FULL);
        assert!(store.insert("full".into(), session(&drops)).is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        store.get_mut("old").unwrap().created -= Duration::from_secs(300);
        store.check_capacity().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(!store.contains_key("old"));
        assert!(store.contains_key("live"));
        store.insert("new".into(), session(&drops)).unwrap();
        assert!(store.check_capacity().is_err());
    }

    #[test]
    // Verify that failed lookups retain expired sessions until cancel or prepare
    fn failed_lookups_retain_expired_sessions_until_cancel_or_prepare() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut store = ReviewStore::default();
        store.insert("id".into(), session(&drops)).unwrap();
        assert!(matches!(store.active("missing", "missing", "expired"), Err(e) if e == "missing"));
        store.active_mut("id", "missing", "expired").unwrap().value = 9;
        assert_eq!(store.active("id", "missing", "expired").unwrap().value, 9);
        store.get_mut("id").unwrap().created -= Duration::from_secs(300);
        assert!(matches!(store.active("id", "missing", "expired"), Err(e) if e == "expired"));
        assert!(matches!(store.active_mut("id", "missing", "expired"), Err(e) if e == "expired"));
        assert_eq!(store.get("id").unwrap().value, 9);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(store.remove("id"));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(store.remove("id").is_none());
        assert!(store.is_empty());
    }

    #[test]
    // Verify that duplicate identifiers do not replace a review
    fn duplicate_identifiers_do_not_replace_a_review() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut store = ReviewStore::default();
        let id = new_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_ne!(new_id(), id);
        store.insert(id.clone(), session(&drops)).unwrap();
        assert!(store.insert(id.clone(), session(&drops)).is_err());
        assert_eq!(store.get(&id).unwrap().value, 7);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    // Verify that expired reviews keep staging and leases until reclaimed
    fn expired_reviews_keep_staging_and_leases_until_reclaimed() {
        struct OwnedSession {
            created: Instant,
            _directory: tempfile::TempDir,
            _lease: crate::asset_leases::AssetLease,
        }
        impl ReviewSession for OwnedSession {
            const LIMIT: usize = 1;
            const FULL: &'static str = "Full";
            const LIFETIME: Option<Duration> = Some(Duration::from_secs(300));
            // Read the review creation time used to enforce its expiration
            fn created(&self) -> Instant {
                self.created
            }
        }
        let leases = crate::asset_leases::AssetLeases::default();
        let lease = leases.pin([new_id()], "Review test").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let mut store = ReviewStore::default();
        store
            .insert(
                "id".into(),
                OwnedSession {
                    created: Instant::now() - Duration::from_secs(300),
                    _directory: directory,
                    _lease: lease,
                },
            )
            .unwrap();
        assert!(store.active("id", "missing", "expired").is_err());
        assert!(path.is_dir());
        assert!(!leases.lock().live.is_empty());
        store.check_capacity().unwrap();
        assert!(!path.exists());
        assert!(leases.lock().live.is_empty());
    }

    #[test]
    // Verify that sessions without a lifetime remain until explicit removal
    fn sessions_without_a_lifetime_remain_until_explicit_removal() {
        struct Package(Instant);
        impl ReviewSession for Package {
            const LIMIT: usize = 1;
            const FULL: &'static str = "Full";
            const LIFETIME: Option<Duration> = None;
            // Read the review creation time used to enforce its expiration
            fn created(&self) -> Instant {
                self.0
            }
        }
        let mut store = ReviewStore::default();
        store
            .insert(
                "package".into(),
                Package(Instant::now() - Duration::from_secs(86400)),
            )
            .unwrap();
        assert!(store.check_capacity().is_err());
        assert!(store.active("package", "missing", "expired").is_ok());
    }
}
