//! Individual package reviews share validated staging and recoverable replacement
use crate::{
    updates::{compatible, stable_version, verify_file, CatalogSelection, PackageCandidate},
    vault::{VaultAssignment, VaultEntry, VaultEntryInput, VaultSetupSelection},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use stream_dock_manager_plugin_host::{PluginHost, PreparedPlugin};

#[derive(Serialize, ts_rs::TS)]
pub struct PluginReloadResult {
    pub snapshot: crate::Snapshot,
    pub added: usize,
    pub removed: usize,
}

#[derive(Serialize, ts_rs::TS)]
pub struct PluginSelectionFailure {
    pub source: String,
    pub status: String,
    pub message: String,
}

#[derive(Serialize, ts_rs::TS)]
pub struct PluginSelectionReview {
    pub reviews: Vec<PluginPackageReview>,
    pub results: Vec<PluginSelectionFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginPackageReview {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional=nullable,as="Option<Vec<stream_dock_manager_plugin_host::vault_rpc::VaultSetupSlot>>")]
    pub retained_slots: Vec<stream_dock_manager_plugin_host::vault_rpc::VaultSetupSlot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional=nullable,as="Option<Vec<stream_dock_manager_plugin_host::permissions::ActionScope>>")]
    pub retained_services: Vec<stream_dock_manager_plugin_host::permissions::ActionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub reviewable_services: Option<Vec<stream_dock_manager_plugin_host::permissions::ActionScope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission_diff: Option<stream_dock_manager_plugin_host::permissions::PermissionDiff>,
    #[serde(default)]
    #[ts(optional)]
    pub suggested_profiles: Option<crate::plugin_profiles::PluginProfileCatalog>,
    pub id: String,
    pub trusted: bool,
    pub package: PackageCandidate,
    pub installed_version: Option<String>,
    pub channel: String,
    pub channel_change: bool,
    pub compatible: bool,
    pub force_reinstall_available: bool,
    pub reason: Option<String>,
    pub manifest: stream_dock_manager_plugin_host::manifest::NormalizedManifest,
    pub source: stream_dock_manager_plugin_host::PackageSource,
    pub compatibility: stream_dock_manager_plugin_host::preflight::PluginPreflight,
    pub can_import: bool,
    pub can_run: bool,
    pub version_change: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct VaultSetupChoices {
    pub ticket: String,
    pub entries: Vec<VaultEntry>,
}

struct StagedPicker {
    ticket: String,
    slot: String,
    action: String,
    epoch: uuid::Uuid,
    opened: std::time::Instant,
}

// Require confirmation for an older version or the same version from the same channel
fn catalog_requires_reinstall(
    version: &str,
    installed: &str,
    channel_change: bool,
) -> Result<bool, String> {
    Ok(stable_version(version)? < stable_version(installed)?
        || version == installed && !channel_change)
}

pub struct StagedPackage {
    service_review: stream_dock_manager_plugin_host::permission_rpc::InstallReview,
    publisher: Option<String>,
    continuity: Option<crate::vault::Continuity>,
    picker: Option<StagedPicker>,
    pub review: PluginPackageReview,
    prepared: Option<PreparedPlugin>,
    selection: Option<CatalogSelection>,
    created: std::time::Instant,
    vault_expected_installation: Option<String>,
    force_reinstall_origin: Option<serde_json::Value>,
    vault_editor: Option<u64>,
    vault_selections: std::collections::BTreeMap<(String, String), VaultAssignment>,
    direct_read_opt_ins: std::collections::BTreeMap<(String, String), uuid::Uuid>,
    vault_created_values: std::collections::BTreeSet<(String, String)>,
}
impl crate::review_store::ReviewSession for StagedPackage {
    const LIMIT: usize = 4;
    const FULL: &'static str = "Close an existing package review first";
    const LIFETIME: Option<std::time::Duration> = None;
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
impl crate::Runtime {
    // Prepare plugin package
    pub fn prepare_plugin_package(&self, path: &Path) -> Result<PluginPackageReview, String> {
        let _source_lease = self.updates.cache_lease(path)?;
        let root = self.plugins.lock().unwrap().installation_root();
        let prepared = PluginHost::prepare_install(&root, path)?;
        let info = PluginHost::prepared_info(&prepared)?;
        let descriptor = info.capabilities.as_ref();
        let source = PluginHost::prepared_source(&prepared);
        let package = PackageCandidate {
            files: PluginHost::prepared_inventory(&prepared),
            os: if cfg!(target_os = "macos") {
                "macos"
            } else {
                std::env::consts::OS
            }
            .into(),
            id: info.id,
            version: info.version,
            path: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            sha256: source.sha256.clone(),
            size: path.metadata().map_err(|e| e.to_string())?.len(),
            architectures: vec![if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            }
            .into()],
            minimum_manager: descriptor
                .map(|c| c.minimum_manager.clone())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "0.1.0".into()),
            required_capabilities: descriptor
                .map(|c| c.required_capabilities.clone())
                .unwrap_or_default(),
        };
        self.stage_package(prepared, package, None)
    }
    // Prepare catalog download
    pub fn prepare_catalog_download(&self, job: &str) -> Result<PluginPackageReview, String> {
        let selection = self
            .updates
            .downloaded_selection(job)
            .ok_or("Download has no immutable package selection")?;
        self.updates.selection_current(&selection)?;
        let package = selection.package();
        compatible(&package)?;
        let (path, _source_lease) = self.updates.leased_completed_download(job)?;
        verify_file(&path, &package.sha256, package.size)?;
        let root = self.plugins.lock().unwrap().installation_root();
        let prepared = PluginHost::prepare_install(&root, &path)?;
        PluginHost::verify_prepared_files(&prepared, &package.files)?;
        let info = PluginHost::prepared_info(&prepared)?;
        if info.id != package.id
            || info.version != package.version
            || info.capabilities.as_ref() != Some(&selection.plugin.capabilities)
        {
            return Err("Manifest identity/version/capabilities differ from catalog".into());
        }
        self.stage_package(prepared, package, Some(selection))
    }
    // Stage package
    fn stage_package(
        &self,
        prepared: PreparedPlugin,
        package: PackageCandidate,
        selection: Option<CatalogSelection>,
    ) -> Result<PluginPackageReview, String> {
        let manifest = PluginHost::prepared_manifest(&prepared);
        let publisher = selection
            .as_ref()
            .filter(|_| manifest.permissions.is_some() || manifest.setup_slots.is_some())
            .and_then(|s| self.updates.scoped_publisher(s));
        let host = self.plugins.lock().unwrap();
        let installed = host.list().into_iter().find(|p| p.id == package.id);
        let provenance = host.provenance(&package.id);
        let vault_expected_installation = host.current_vault_installation_token(&package.id);
        let channel = selection
            .as_ref()
            .map(|s| s.channel.clone())
            .unwrap_or_else(|| "local".into());
        let channel_change =
            selection.is_some() && provenance.as_ref().is_some_and(|p| p["channel"] != channel);
        let compatibility = stream_dock_manager_plugin_host::preflight::resolve_runtime(
            PluginHost::prepared_preflight(&prepared),
            &self.runtimes,
        );
        let mut reason = if selection.is_some() {
            compatible(&package).err()
        } else if !compatibility.can_install {
            Some("Package cannot be imported".into())
        } else {
            None
        };
        let mut catalog_reinstall = false;
        if let (Some(old), Some(selection)) = (&installed, &selection) {
            if provenance
                .as_ref()
                .is_none_or(|p| p["repository"] != selection.repository)
            {
                reason = Some("Installed package is manually managed. Import a local ZIP to replace it explicitly.".into());
            } else {
                catalog_reinstall =
                    catalog_requires_reinstall(&package.version, &old.version, channel_change)?;
            }
        }
        let reviewable_services = host.reviewable_services(&prepared);
        let service_review = host.service_review(&package.id)?;
        if PluginHost::prepared_manifest(&prepared).missing_required_services(&reviewable_services)
        {
            reason.get_or_insert_with(|| "A required Manager permission is unavailable".into());
        }
        let uses_vault_references = installed.as_ref().and_then(|p|p.permissions.as_ref()).is_some_and(|d|
            d.actions.values().any(|g| g.required.iter().chain(&g.optional).any(|s| matches!(s, stream_dock_manager_plugin_host::permissions::PermissionScope::Access{..}))));
        let prior_vault = publisher.as_ref().and_then(|_| {
            host.verified_vault_installation(&package.id).or_else(|| {
                uses_vault_references
                    .then(|| host.reviewed_vault_owner(&package.id))
                    .flatten()
            })
        });
        let prior_decisions = publisher
            .as_ref()
            .and_then(|_| host.installed_permissions(&package.id).ok());
        drop(host);
        let version_change = installed
            .as_ref()
            .and_then(|old| {
                if old.dialect == "openaction" {
                    semver::Version::parse(&package.version)
                        .ok()
                        .zip(semver::Version::parse(&old.version).ok())
                        .map(|(new, old)| new.cmp(&old))
                } else {
                    stream_dock_manager_plugin_host::manifest::compare_vendor_versions(
                        &package.version,
                        &old.version,
                    )
                    .ok()
                }
            })
            .map(|order| {
                match order {
                    std::cmp::Ordering::Less => "downgrade",
                    std::cmp::Ordering::Equal => "same_version",
                    std::cmp::Ordering::Greater => "upgrade",
                }
                .into()
            });
        let manifest = PluginHost::prepared_manifest(&prepared);
        let permission_diff = stream_dock_manager_plugin_host::permissions::scope_diff(
            installed.as_ref().and_then(|p| p.permissions.as_ref()),
            manifest.permissions.as_ref(),
        );
        let continuous = publisher
            .as_ref()
            .zip(provenance.as_ref())
            .zip(selection.as_ref())
            .is_some_and(|((proof, old), current)| {
                old["verified_publisher"].as_str() == Some(proof)
                    && old["repository"] == current.repository
                    && old["catalog_url"] == serde_json::json!(current.catalog_url)
                    && !channel_change
                    && version_change.as_deref() == Some("upgrade")
            });
        let mut retained_services: Vec<_> = if continuous {
            permission_diff
                .as_ref()
                .into_iter()
                .flat_map(|d| &d.unchanged)
                .filter(|item| {
                    let prior = installed
                        .as_ref()
                        .and_then(|i| i.permissions.as_ref())
                        .and_then(|d| d.actions.get(&item.action));
                    let next = manifest
                        .permissions
                        .as_ref()
                        .and_then(|d| d.actions.get(&item.action));
                    prior.zip(next).is_some_and(|(a, b)| a.purpose == b.purpose)
                        && reviewable_services.contains(item)
                        && prior_decisions
                            .as_ref()
                            .and_then(|v| v["scopes"].as_array())
                            .is_some_and(|rows| {
                                rows.iter().any(|r| {
                                    r["action"] == item.action
                                        && r["scope"] == serde_json::json!(item.scope)
                                        && r["granted"] == true
                                        && r["available"] == true
                                })
                            })
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let continuity = if continuous {
            prior_vault.and_then(|prior| {
                let same_purpose = |action: &str| {
                    let old = installed
                        .as_ref()
                        .and_then(|p| p.permissions.as_ref())
                        .and_then(|d| d.actions.get(action));
                    let new = manifest
                        .permissions
                        .as_ref()
                        .and_then(|d| d.actions.get(action));
                    old.map(|g| &g.purpose) == new.map(|g| &g.purpose)
                };
                let unchanged: Vec<_> = permission_diff
                    .as_ref()
                    .into_iter()
                    .flat_map(|d| &d.unchanged)
                    .filter(|i| same_purpose(&i.action))
                    .cloned()
                    .collect();
                let slots: Vec<_> = manifest
                    .setup_slots
                    .iter()
                    .flatten()
                    .filter(|s| {
                        same_purpose(&s.action)
                            && (!s.requires_direct_approval()
                                || unchanged.iter().any(|a| {
                                    a.action == s.action && a.scope.matches_direct_slot(s)
                                }))
                    })
                    .cloned()
                    .collect();
                let revision = prior_decisions.as_ref().and_then(|d|d["revision"].as_str()).and_then(|rev|rev.parse().ok());
                match self.vault.review_continuity(prior, &slots, &unchanged, revision) {
                    Ok(review) => Some(review),
                    Err(_) if matches!(self.vault.status().state, crate::vault::VaultState::Uninitialized) => None,
                    Err(_) => {
                        reason = Some("Unlock Vault and review the update again to verify its existing access.".into());
                        None
                    }
                }
            })
        } else {
            None
        };
        if let Some(c) = &continuity {
            retained_services.extend(c.writers.iter().cloned())
        }
        let review = PluginPackageReview {
            retained_slots: continuity
                .as_ref()
                .map(|c| c.slots.clone())
                .unwrap_or_default(),
            retained_services,
            reviewable_services: (!reviewable_services.is_empty()).then_some(reviewable_services),
            permission_diff,
            suggested_profiles: Some(crate::plugin_profiles::discover(
                PluginHost::prepared_directory(&prepared),
                &PluginHost::prepared_info(&prepared)?,
            )),
            manifest,
            source: PluginHost::prepared_source(&prepared),
            can_import: reason.is_none(),
            can_run: compatibility.can_enable,
            compatibility,
            version_change,
            id: crate::review_store::new_id(),
            trusted: selection.is_some(),
            package,
            installed_version: installed.map(|p| p.version),
            channel,
            channel_change,
            compatible: reason.is_none(),
            force_reinstall_available: catalog_reinstall && reason.is_none(),
            reason,
        };
        let mut reviews = self.plugin_packages.lock().unwrap();
        reviews.check_capacity()?;
        reviews.insert(
            review.id.clone(),
            StagedPackage {
                publisher,
                continuity,
                service_review,
                picker: None,
                review: review.clone(),
                prepared: Some(prepared),
                selection,
                created: std::time::Instant::now(),
                vault_expected_installation,
                force_reinstall_origin: provenance.filter(|_| review.force_reinstall_available),
                vault_editor: None,
                vault_selections: Default::default(),
                direct_read_opt_ins: Default::default(),
                vault_created_values: Default::default(),
            },
        )?;
        Ok(review)
    }
    /// A trusted package review can opt in to a single action's direct read.
    /// Turning it off also retires any staged entry approval for that action.
    pub fn vault_setup_permission(
        &self,
        id: &str,
        slot_id: &str,
        action: &str,
        editor: u64,
        allow: bool,
    ) -> Result<(), String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let slot = setup_slot(stage, slot_id, action, editor)?;
        if !slot.requires_direct_approval() {
            return Err("not_declared".into());
        }
        let key = (slot.action, slot.id);
        if allow {
            self.check_setup_candidate(stage)?;
            let epoch = self.vault.session_epoch()?;
            stage.direct_read_opt_ins.insert(key, epoch);
        } else {
            stage.direct_read_opt_ins.remove(&key);
            stage.vault_selections.remove(&key);
        }
        Ok(())
    }

    // Preview only compatible Vault entry metadata for a staged package
    pub fn vault_setup_slot_entries(
        &self,
        id: &str,
        slot_id: &str,
        action: &str,
        editor: u64,
    ) -> Result<VaultSetupChoices, String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let slot = setup_slot(stage, slot_id, action, editor)?;
        self.check_setup_candidate(stage)?;
        let (epoch, entries) = self.vault.setup_entries(&slot)?;
        let ticket = crate::review_store::new_id();
        stage.picker = Some(StagedPicker {
            ticket: ticket.clone(),
            action: slot.action,
            slot: slot.id,
            epoch,
            opened: std::time::Instant::now(),
        });
        Ok(VaultSetupChoices { ticket, entries })
    }

    fn check_setup_candidate(&self, stage: &StagedPackage) -> Result<(), String> {
        if stage.vault_expected_installation
            != self
                .plugins
                .lock()
                .unwrap()
                .current_vault_installation_token(&stage.review.package.id)
        {
            return Err("Vault setup review expired".into());
        }
        let prepared = stage
            .prepared
            .as_ref()
            .ok_or("Vault setup review expired")?;
        PluginHost::verify_prepared_files(prepared, &stage.review.package.files)
    }

    pub fn cancel_vault_picker(&self, id: &str, editor: u64, ticket: &str) {
        let mut reviews = self.plugin_packages.lock().unwrap();
        if let Some(stage) = reviews.get_mut(id) {
            if stage.vault_editor == Some(editor)
                && stage.picker.as_ref().is_some_and(|p| p.ticket == ticket)
            {
                stage.picker = None;
            }
        }
    }

    // Select an exact eligible entry after the user approves the disclosed use
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the installation, logical slot and reviewed Vault identity explicit"
    )]
    pub fn vault_setup_select(
        &self,
        id: &str,
        slot_id: &str,
        action: &str,
        editor: u64,
        ticket: &str,
        entry_id: &str,
        revision: &str,
        allow_direct_read: bool,
    ) -> Result<VaultSetupSelection, String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let slot = setup_slot(stage, slot_id, action, editor)?;
        let epoch = picker_epoch(stage, &slot, ticket)?;
        stage.picker = None;
        self.check_setup_candidate(stage)?;
        let key = (slot.action.clone(), slot.id.clone());
        if slot.requires_direct_approval() && stage.direct_read_opt_ins.get(&key) != Some(&epoch) {
            return Err("explicit_package_permission_required".into());
        }
        let (selected, mut assignment) =
            self.vault
                .setup_selection(&slot, entry_id, revision, allow_direct_read)?;
        if assignment.epoch != epoch {
            return Err("vault_setup_expired".into());
        }
        assignment.make_private = stage
            .vault_created_values
            .contains(&(slot.id.clone(), entry_id.into()));
        stage.vault_selections.insert(key, assignment);
        Ok(selected)
    }

    // Create a compatible user-owned entry without authorizing the staged plugin
    pub fn vault_setup_create_entry(
        &self,
        id: &str,
        slot_id: &str,
        action: &str,
        editor: u64,
        ticket: &str,
        input: VaultEntryInput,
    ) -> Result<VaultEntry, String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let slot = setup_slot(stage, slot_id, action, editor)?;
        let epoch = picker_epoch(stage, &slot, ticket)?;
        self.check_setup_candidate(stage)?;
        let entry = self.vault.setup_create(&slot, epoch, input)?;
        if slot.kind == "value" {
            stage
                .vault_created_values
                .insert((slot.id, entry.id.clone()));
        }
        Ok(entry)
    }

    // Discard the staged review without discarding any verified catalog archive
    pub fn cancel_plugin_package(&self, id: &str) {
        self.plugin_packages.lock().unwrap().remove(id);
    }
    // Commit plugin package
    pub fn commit_plugin_package(
        &self,
        id: &str,
        allow_channel_change: bool,
        replace: bool,
    ) -> Result<crate::Snapshot, String> {
        self.commit_plugin_package_from_editor(id, allow_channel_change, replace, None)
    }

    // Commit an exact package review while carrying the trusted editor owner
    pub fn commit_plugin_package_from_editor(
        &self,
        id: &str,
        allow_channel_change: bool,
        replace: bool,
        editor: Option<u64>,
    ) -> Result<crate::Snapshot, String> {
        self.commit_plugin_package_permissions(id, allow_channel_change, replace, editor, &[])
    }
    pub fn commit_plugin_package_permissions(
        &self,
        id: &str,
        allow_channel_change: bool,
        replace: bool,
        editor: Option<u64>,
        approvals: &[stream_dock_manager_plugin_host::permissions::ActionScope],
    ) -> Result<crate::Snapshot, String> {
        self.commit_reviewed_plugin_package(
            id,
            allow_channel_change,
            replace,
            editor,
            approvals,
            false,
        )
    }

    // Commit a reviewed package with a separate opt-in for a catalog downgrade or reinstall
    pub fn commit_reviewed_plugin_package(
        &self,
        id: &str,
        allow_channel_change: bool,
        replace: bool,
        editor: Option<u64>,
        approvals: &[stream_dock_manager_plugin_host::permissions::ActionScope],
        force_reinstall: bool,
    ) -> Result<crate::Snapshot, String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let review = &stage.review;
        if !review.compatible {
            return Err(review
                .reason
                .clone()
                .unwrap_or_else(|| "Incompatible package".into()));
        }
        if stage.selection.is_some() {
            compatible(&review.package)?;
        }
        if let Some(proof) = &stage.publisher {
            let selection = stage
                .selection
                .as_ref()
                .ok_or("Package source changed during review")?;
            if self.updates.scoped_publisher(selection).as_ref() != Some(proof) {
                return Err(
                    "Publisher evidence changed during review. Review the package again.".into(),
                );
            }
        }
        let mut host = self.plugins.lock().unwrap();
        let old = host.list().into_iter().find(|p| p.id == review.package.id);
        if let Some(selection) = &stage.selection {
            self.updates.selection_current(selection)?;
            if force_reinstall
                && (!review.force_reinstall_available
                    || old.as_ref().map(|old| old.version.as_str())
                        != review.installed_version.as_deref()
                    || host.provenance(&review.package.id) != stage.force_reinstall_origin
                    || host.current_vault_installation_token(&review.package.id)
                        != stage.vault_expected_installation)
            {
                return Err("Installed package changed; review this package again".into());
            }
            if let Some(old) = &old {
                let provenance = host
                    .provenance(&old.id)
                    .ok_or("Installed package is manually managed")?;
                if provenance["repository"] != selection.repository {
                    return Err("Installed package origin differs".into());
                }
                if provenance["channel"] != selection.channel && !allow_channel_change {
                    return Err("Acknowledge the channel change".into());
                }
                if catalog_requires_reinstall(
                    &review.package.version,
                    &old.version,
                    provenance["channel"] != selection.channel,
                )? && !force_reinstall
                {
                    return Err("Already installed or downgrade blocked".into());
                }
            }
        } else if old.is_some() && !replace {
            return Err("Confirm replacing the installed package".into());
        }
        if let Some(prepared) = &stage.prepared {
            PluginHost::verify_prepared_files(prepared, &review.package.files)?;
        }
        let writers:Vec<_>=approvals.iter().filter(|s|matches!(s.scope,stream_dock_manager_plugin_host::permissions::PermissionScope::WriteSecret{..})).cloned().collect();
        let approved_vault_access = approvals.iter().any(|v| {
            matches!(
                v.scope,
                stream_dock_manager_plugin_host::permissions::PermissionScope::Access { .. }
            )
        });
        let write_epoch = (!writers.is_empty() || approved_vault_access)
            .then(|| self.vault.session_epoch())
            .transpose()?;
        let scoped = review.manifest.permissions.is_some();
        if !approvals.is_empty()
            && (editor.is_none() || stage.created.elapsed() >= VAULT_REVIEW_AGE)
        {
            return Err("Package permission review expired".into());
        }
        if scoped || !approvals.is_empty() {
            host.stage_service_grants(
                stage.prepared.as_mut().ok_or("Prepare the package again")?,
                &stage.service_review,
                approvals,
            )?;
        }
        let slots = review.manifest.setup_slots.as_deref().unwrap_or_default();
        let managed = !stage.vault_selections.is_empty()
            || !review.retained_slots.is_empty()
            || slots.iter().any(|slot| slot.required);
        if managed
            && (editor.is_none()
                || (!stage.vault_selections.is_empty() && stage.vault_editor != editor)
                || stage.created.elapsed() >= VAULT_REVIEW_AGE
                || slots.iter().any(|slot| {
                    slot.required
                        && !review.retained_slots.contains(slot)
                        && !stage
                            .vault_selections
                            .contains_key(&(slot.action.clone(), slot.id.clone()))
                })
                || stage.vault_expected_installation
                    != host.current_vault_installation_token(&review.package.id))
        {
            return Err("Vault setup needs an active reviewed selection for this package".into());
        }

        let replacement = old
            .as_ref()
            .map(|old| host.reserve_replacement(&old.id))
            .transpose()?;
        // Waiting belongs outside the host mutex: input and other plugins keep working
        let waiter = if let Some(old) = &old {
            host.stop_for_replacement(&old.id)?
        } else {
            None
        };
        drop(host);
        if old.is_some() {
            self.emit("plugins", None, None, None);
        }
        if let Some(waiter) = waiter {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !waiter.load(std::sync::atomic::Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err(
                        "Plugin process cleanup did not finish; review replacement again".into(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let mut host = self.plugins.lock().unwrap();
        let mut prepared = stage
            .prepared
            .take()
            .ok_or("Prepare the package again before retrying")?;
        if scoped {
            host.stage_service_grants(&mut prepared, &stage.service_review, approvals)?;
        }
        if let Some(selection) = &stage.selection {
            self.updates.selection_current(selection)?;
            PluginHost::write_prepared_metadata(
                &mut prepared,
                ".stream-dock-manager-provenance.json",
                &serde_json::json!({"verified_publisher":stage.publisher,"catalog_url":selection.catalog_url,"repository":selection.repository,"release_tag":selection.artifact.release_tag,"channel":selection.channel,"version":selection.plugin.version,"build":selection.artifact.release_tag,"package_hash":selection.artifact.sha256,"artifact":selection.artifact}),
            )?;
        }
        if stage.selection.is_none() {
            PluginHost::write_prepared_metadata(
                &mut prepared,
                ".stream-dock-manager-provenance.json",
                &serde_json::json!({"source":review.source,"dialect":review.manifest.dialect,"version":review.manifest.version,"trust":"unverified_local"}),
            )?;
        }
        let installed_id = review.package.id.clone();
        let result = if managed || write_epoch.is_some() || stage.continuity.is_some() {
            if stage.created.elapsed() >= VAULT_REVIEW_AGE
                || stage.vault_expected_installation
                    != host.current_vault_installation_token(&installed_id)
            {
                return Err("Vault setup review expired".into());
            }
            let owner = host.prepared_vault_installation(&prepared)?;
            self.vault.grant_setup_with_writers(
                &owner,
                slots,
                &stage.vault_selections,
                &writers,
                write_epoch,
                stage.continuity.as_ref(),
                || host.replace_prepared_with_vault(prepared, &owner),
            )
        } else {
            host.replace_prepared(prepared)
        };
        drop(host);
        reviews.remove(id);
        drop(reviews);
        drop(replacement);
        let snapshot = self.plugins_changed();
        if result.is_ok() && old.is_none() {
            self.clear_plugin_profile_acknowledgement(&installed_id)?;
        }
        result.map(|_| snapshot)
    }
}

const VAULT_REVIEW_AGE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

fn picker_epoch(
    stage: &StagedPackage,
    slot: &stream_dock_manager_plugin_host::vault_rpc::VaultSetupSlot,
    ticket: &str,
) -> Result<uuid::Uuid, String> {
    stage
        .picker
        .as_ref()
        .filter(|p| {
            p.ticket == ticket
                && p.slot == slot.id
                && p.action == slot.action
                && p.opened.elapsed() < std::time::Duration::from_secs(300)
        })
        .map(|p| p.epoch)
        .ok_or_else(|| "vault_setup_expired".into())
}

// Bind setup to one live editor document and one immutable native staged review
fn setup_slot(
    stage: &mut StagedPackage,
    slot_id: &str,
    action: &str,
    editor: u64,
) -> Result<stream_dock_manager_plugin_host::vault_rpc::VaultSetupSlot, String> {
    if !stage.review.can_import
        || stage.prepared.is_none()
        || stage.created.elapsed() >= VAULT_REVIEW_AGE
        || stage
            .vault_editor
            .is_some_and(|generation| generation != editor)
    {
        return Err("Vault setup review expired".into());
    }
    let slot = stage
        .review
        .manifest
        .setup_slots
        .as_ref()
        .into_iter()
        .flatten()
        .find(|slot| slot.id == slot_id && slot.action == action)
        .ok_or("Vault setup slot is not declared")?
        .clone();
    stage.vault_editor = Some(editor);
    Ok(slot)
}

/// Explicit local development installs preserve enablement without starting plugins
pub fn install_local_packages(
    config_root: &Path,
    paths: Vec<PathBuf>,
) -> Result<Vec<stream_dock_manager_plugin_host::PluginInfo>, String> {
    let _writer = crate::ProfileStore::open(config_root.join("profiles"))?;
    PluginHost::install_local_archives_with_storage(
        config_root.join("plugins"),
        paths,
        _writer.storage().clone(),
    )
}

/// Remove selected local packages without starting plugins or opening devices
pub fn uninstall_local_packages(
    config_root: &Path,
    ids: Vec<String>,
) -> Result<Vec<String>, String> {
    let _writer = crate::ProfileStore::open(config_root.join("profiles"))?;
    PluginHost::uninstall_local_packages_with_storage(
        config_root.join("plugins"),
        ids,
        _writer.storage().clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use stream_dock_manager_plugin_host::RuntimeState;
    #[test]
    // Verify that offline uninstall obeys writer lock and preserves profiles
    fn offline_uninstall_obeys_writer_lock_and_preserves_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("manager");
        let source = vendor(dir.path(), "0.1.0");
        install_local_packages(&config, vec![source.clone()]).unwrap();
        let store = crate::ProfileStore::open(config.join("profiles")).unwrap();
        store.load().unwrap();
        let profiles = serde_json::to_value(store.load().unwrap()).unwrap();
        let ids = vec!["org.vendor.fixture".into()];
        assert!(uninstall_local_packages(&config, ids.clone())
            .unwrap_err()
            .contains("Another Stream Dock Manager process"));
        assert!(config
            .join("plugins/org.vendor.fixture/manifest.json")
            .is_file());
        drop(store);
        assert_eq!(
            uninstall_local_packages(&config, ids).unwrap(),
            ["org.vendor.fixture"]
        );
        // The temporary uninstall host may still be retiring workers holding its
        // storage Arc. Wait only for that specific lock, without weakening the
        // assertion that an actively held writer prevents uninstall above
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let restored = loop {
            match crate::ProfileStore::open(config.join("profiles")) {
                Ok(store) => break store,
                Err(error)
                    if error.contains("Another Stream Dock Manager process")
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("Could not reopen after uninstall: {error}"),
            }
        };
        assert_eq!(
            serde_json::to_value(restored.load().unwrap()).unwrap(),
            profiles
        );
        assert!(source.join("manifest.json").is_file());
        assert!(!config.join("plugins/org.vendor.fixture").exists());
    }
    #[test]
    #[ignore = "requires explicitly downloaded pinned plugin samples"]
    // Verify that pinned vendor packages use review commit cancel without execution
    fn pinned_vendor_packages_use_review_commit_cancel_without_execution() {
        let manager = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sources: serde_json::Value = serde_json::from_slice(
            &std::fs::read(manager.join("tests/fixtures/plugins/sources.json")).unwrap(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        for source in sources["sources"].as_array().unwrap() {
            let path = manager
                .join("artifacts/plugin-samples")
                .join(source["id"].as_str().unwrap())
                .join(
                    source["archive"]
                        .as_str()
                        .or(source["package_path"].as_str())
                        .unwrap(),
                );
            let cancelled = runtime.prepare_plugin_package(&path).unwrap();
            assert!(cancelled.can_import && !cancelled.trusted);
            runtime.cancel_plugin_package(&cancelled.id);
            assert!(runtime
                .commit_plugin_package(&cancelled.id, false, false)
                .is_err());
            let review = runtime.prepare_plugin_package(&path).unwrap();
            assert_eq!(review.source.sha256, cancelled.source.sha256);
            let installed = runtime
                .commit_plugin_package(&review.id, false, false)
                .unwrap();
            let plugin = installed
                .plugins
                .iter()
                .find(|p| p.id == review.manifest.id)
                .unwrap();
            assert_eq!(plugin.version, review.manifest.version);
            assert!(!plugin.enabled);
            assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
            if source["id"] == "elgato-layouts" {
                assert_eq!(plugin.version, "0.1.0.0");
                assert!(!review.can_run);
                assert_eq!(
                    review
                        .compatibility
                        .runtime
                        .unwrap()
                        .requirement
                        .version
                        .as_deref(),
                    Some(">=22, <27")
                );
            }
        }
        runtime.shutdown();
    }
    // Write a vendor browser package fixture with the requested version
    fn vendor(root: &Path, version: &str) -> PathBuf {
        let source = root.join("org.vendor.fixture.sdPlugin");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("index.html"), "reviewed bytes").unwrap();
        crate::test_plugin::write_manifest(
            &source,
            json!({"UUID":"org.vendor.fixture","Name":"Vendor fixture","Author":"Vendor","Version":version,"SDKVersion":1,"CodePath":"index.html","UnknownOptional":{"keep":true},"Actions":[{"UUID":"org.vendor.fixture.action","Name":"Action","Controllers":["Keypad"]}]}),
        );
        source
    }
    #[test]
    // Verify that review commit cancel and replacement use exact staged bytes and preserve preferences
    fn review_commit_cancel_and_replacement_use_exact_staged_bytes_and_preserve_preferences() {
        let root = tempfile::tempdir().unwrap();
        let source = vendor(root.path(), "0.1.0.0");
        let manifest = std::fs::read(source.join("manifest.json")).unwrap();
        let original_entry = std::fs::read_to_string(source.join("index.html")).unwrap();
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(review.can_import && review.can_run && !review.trusted);
        assert_eq!(review.manifest.version, "0.1.0.0");
        assert_eq!(review.manifest.publisher.as_deref(), Some("Vendor"));
        assert_eq!(review.package.files.len(), 3);
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        std::fs::write(source.join("index.html"), "source changed after review").unwrap();
        let installed = runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        assert!(!installed.plugins[0].enabled);
        let target = runtime
            .plugins
            .lock()
            .unwrap()
            .installation_root()
            .join("org.vendor.fixture");
        assert_eq!(
            std::fs::read(target.join("manifest.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            original_entry
        );
        let origin = runtime
            .plugins
            .lock()
            .unwrap()
            .provenance("org.vendor.fixture")
            .unwrap();
        assert!(origin.get("repository").is_none());
        assert_eq!(origin["trust"], "unverified_local");
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("org.vendor.fixture", true)
            .unwrap();
        assert_eq!(
            runtime.snapshot().plugins[0].runtime.state,
            RuntimeState::Idle
        );
        vendor(root.path(), "1.4.1");
        let replacement_entry = std::fs::read_to_string(source.join("index.html")).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert_eq!(review.installed_version.as_deref(), Some("0.1.0.0"));
        assert_eq!(review.version_change.as_deref(), Some("upgrade"));
        assert!(runtime
            .commit_plugin_package(&review.id, false, false)
            .is_err());
        let snapshot = runtime
            .commit_plugin_package(&review.id, false, true)
            .unwrap();
        assert!(snapshot.plugins[0].enabled);
        assert_eq!(snapshot.plugins[0].runtime.state, RuntimeState::Idle);
        let review = runtime.prepare_plugin_package(&source).unwrap();
        runtime.cancel_plugin_package(&review.id);
        assert!(runtime
            .commit_plugin_package(&review.id, false, true)
            .is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            replacement_entry
        );
        assert!(
            crate::updates::stable_version("0.1.0.0").is_err(),
            "official catalog SemVer stays strict"
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that changed staging fails without replacement and missing runtime is importable
    fn changed_staging_fails_without_replacement_and_missing_runtime_is_importable() {
        let root = tempfile::tempdir().unwrap();
        let source = vendor(root.path(), "1.0.0");
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        let plugin_root = runtime.plugins.lock().unwrap().installation_root();
        let staging = std::fs::read_dir(&plugin_root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".install-")
            })
            .unwrap();
        std::fs::write(
            staging.join("unpacked/org.vendor.fixture.sdPlugin/index.html"),
            "tampered",
        )
        .unwrap();
        assert!(runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap_err()
            .contains("hash mismatch"));
        assert!(runtime.snapshot().plugins.is_empty());
        runtime.cancel_plugin_package(&review.id);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.join("manifest.json")).unwrap()).unwrap();
        manifest["CodePath"] = json!("main.js");
        manifest["StreamDockManager"]["Runtime"] =
            json!({"Kind":"nodejs","Version":">=22, <27","Channel":"lts"});
        std::fs::write(source.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(
            source.join("main.js"),
            "throw Error('import must never execute')",
        )
        .unwrap();
        crate::test_plugin::write_manifest(&source, manifest);
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(review.can_import && !review.can_run);
        assert_eq!(
            review
                .compatibility
                .runtime
                .unwrap()
                .requirement
                .version
                .as_deref(),
            Some(">=22, <27")
        );
        let snapshot = runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        assert!(!snapshot.plugins[0].enabled);
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        runtime.shutdown();
    }
}
