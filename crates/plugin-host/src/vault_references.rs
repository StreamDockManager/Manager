//! Metadata and recipe selectors. Only the native Vault decides which entries a caller can see.
use crate::{permissions::PermissionScope, vault_rpc::VaultInstallation};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: &str = "plugin-vault-references-v1";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceScope {
    pub owner: VaultInstallation,
    pub action: String,
    pub purpose: String,
    pub declaration: PermissionScope,
    pub permission_revision: u64,
}
#[derive(Clone, Serialize)]
pub struct EntryName {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Copy)]
pub struct ReviewVersion {
    pub epoch: uuid::Uuid,
    pub revision: u64,
}
#[derive(Serialize)]
pub struct EntryPage {
    pub entries: Vec<EntryName>,
    pub next: Option<usize>,
}
/// Implementations must decline concurrent Vault transitions. These calls can hold
/// the host's permission lock while binding publication to a live connection.
pub trait References: Send + Sync {
    /// Publish an ordinary use/browse decision only while this trusted Vault review is live.
    fn approve(
        &self,
        expected: ReviewVersion,
        commit: &mut dyn FnMut() -> Result<u64, String>,
    ) -> Result<u64, String> {
        let current = self.version()?;
        if current.epoch != expected.epoch || current.revision != expected.revision {
            return Err("stale_revision".into());
        }
        commit()
    }

    /// Trusted status projection. Never wait for a Vault lock held by an installer.
    fn secret_grant(&self, _scope: &ReferenceScope) -> (bool, bool) {
        (false, false)
    }
    fn resolve(&self, entry: &RecipeEntry) -> Result<RecipeValue, &'static str>;
    fn version(&self) -> Result<ReviewVersion, &'static str>;
    fn entries(
        &self,
        scope: &ReferenceScope,
        approved_only: bool,
        start: usize,
        version: Option<ReviewVersion>,
    ) -> Result<EntryPage, &'static str>;
    fn select(
        &self,
        scope: &ReferenceScope,
        id: &str,
        version: ReviewVersion,
        live: &dyn Fn() -> bool,
    ) -> Result<EntryName, &'static str>;
}

#[derive(Clone)]
pub struct RecipeEntry {
    pub scope: ReferenceScope,
    pub id: String,
    pub version: ReviewVersion,
}
pub enum RecipeValue {
    Authorization(zeroize::Zeroizing<String>),
}
