//! Process-owned pins for immutable managed assets. Pins never expire while an owner is alive
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
#[derive(Debug, Default)]
pub(crate) struct AssetPins {
    pub generation: u64,
    pub live: BTreeMap<String, (String, BTreeSet<String>)>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct AssetLeases(Arc<Mutex<AssetPins>>);
#[derive(Clone, Debug)]
pub(crate) struct AssetLease(Arc<Registration>);
#[derive(Debug)]
struct Registration {
    pool: AssetLeases,
    id: String,
}
impl Drop for Registration {
    // Remove the final registration and invalidate cleanup plans when assets become unpinned
    fn drop(&mut self) {
        let mut state = self.pool.0.lock().unwrap();
        if let Some((_, ids)) = state.live.remove(&self.id) {
            if ids
                .iter()
                .any(|id| !state.live.values().any(|(_, live)| live.contains(id)))
            {
                state.generation = state.generation.wrapping_add(1);
            }
        }
    }
}
impl AssetLeases {
    // Treat live owners or an unavailable lock as a reason to retain resources
    pub(crate) fn has_live_or_busy(&self) -> bool {
        crate::editor_retirement::unavailable_or(&self.0, |pins| !pins.live.is_empty())
    }

    // Pin validated asset IDs until the last lease owner releases them
    pub fn pin(
        &self,
        ids: impl IntoIterator<Item = String>,
        reason: &str,
    ) -> Result<AssetLease, String> {
        let ids: BTreeSet<_> = ids.into_iter().collect();
        if ids.len() > 8192 || reason.len() > 160 {
            return Err("Asset lease exceeds bounds".into());
        }
        for id in &ids {
            crate::profiles::validate_asset_id(id)?;
        }
        let mut state = self.0.lock().unwrap();
        if state.live.len() >= 256
            || state.live.values().map(|(_, ids)| ids.len()).sum::<usize>() + ids.len() > 32768
        {
            return Err("Too many active media owners; close an unfinished media operation".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        if ids
            .iter()
            .any(|id| !state.live.values().any(|(_, live)| live.contains(id)))
        {
            state.generation = state.generation.wrapping_add(1);
        }
        state.live.insert(id.clone(), (reason.into(), ids));
        Ok(AssetLease(Arc::new(Registration {
            pool: self.clone(),
            id,
        })))
    }
    // Acquire the asset leases lock
    pub fn lock(&self) -> std::sync::MutexGuard<'_, AssetPins> {
        self.0.lock().unwrap()
    }
}
impl AssetLease {
    // Return the registration ID identifying this asset lease
    pub fn id(&self) -> &str {
        &self.0.id
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that pins follow the last owner and invalidate cleanup generation
    fn pins_follow_the_last_owner_and_invalidate_cleanup_generation() {
        let pool = AssetLeases::default();
        let id = uuid::Uuid::new_v4().to_string();
        let lease = pool.pin([id.clone(), id.clone()], "Staged review").unwrap();
        let generation = pool.lock().generation;
        assert_eq!(pool.lock().live[lease.id()].1.len(), 1);
        let copy = lease.clone();
        drop(lease);
        assert_eq!(pool.lock().generation, generation);
        drop(copy);
        assert!(pool.lock().live.is_empty());
        assert_ne!(pool.lock().generation, generation);
        assert!(pool.pin(["../unmanaged".into()], "Invalid").is_err());
    }
}
