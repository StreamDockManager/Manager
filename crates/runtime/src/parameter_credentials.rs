use crate::{
    credentials::{self, CredentialState},
    parameters::ParameterOverrides,
    Profile, Runtime, Snapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialTarget {
    pub profile_id: String,
    pub name: String,
    pub device_id: Option<String>,
}
#[derive(Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialWrite {
    pub target: CredentialTarget,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub value: String,
}
// Read the selected parameter override from its explicit device or installation scope
fn selected<'a>(overrides: &'a ParameterOverrides, target: &CredentialTarget) -> Option<&'a Value> {
    let profiles = target
        .device_id
        .as_ref()
        .and_then(|device| overrides.devices.get(device))
        .or_else(|| {
            target
                .device_id
                .is_none()
                .then_some(&overrides.installation)
        });
    profiles
        .and_then(|profiles| profiles.get(&target.profile_id))
        .and_then(|values| values.get(&target.name))
}
// Borrow mutable scope state for parameter credentials
fn scope_mut<'a>(
    overrides: &'a mut ParameterOverrides,
    target: &CredentialTarget,
) -> &'a mut BTreeMap<String, Value> {
    let profiles = if let Some(device) = &target.device_id {
        overrides.devices.entry(device.clone()).or_default()
    } else {
        &mut overrides.installation
    };
    profiles.entry(target.profile_id.clone()).or_default()
}
// Validate the supplied parameter credentials value
fn validate(
    snapshot: &Snapshot,
    target: &CredentialTarget,
    revision: Option<u64>,
) -> Result<(), String> {
    if revision.is_some_and(|revision| snapshot.configuration_revision != revision) {
        return Err("Profile changed. Reload before changing a credential.".into());
    }
    let definition = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == target.profile_id)
        .and_then(|profile| {
            profile
                .parameters
                .iter()
                .find(|parameter| parameter.name == target.name)
        })
        .ok_or("Unknown credential parameter")?;
    if !definition.sensitive || definition.r#type != "string" || definition.default.is_some() {
        return Err("Choose a sensitive string parameter without a portable default".into());
    }
    if target
        .device_id
        .as_ref()
        .is_some_and(|device| device.is_empty() || device.len() > 512)
    {
        return Err("Invalid credential device scope".into());
    }
    Ok(())
}
impl Runtime {
    // Validate sensitive definition changes
    pub(crate) fn validate_sensitive_definition_changes(
        &self,
        before: &Snapshot,
        after: &crate::ProfileData,
    ) -> Result<(), String> {
        let stored = self.stored_parameter_overrides()?;
        for profile in &before.profiles {
            for parameter in profile.parameters.iter().filter(|p| p.sensitive) {
                let changed_to_plain = after
                    .profiles
                    .iter()
                    .find(|p| p.id == profile.id)
                    .and_then(|p| p.parameters.iter().find(|p| p.name == parameter.name))
                    .is_some_and(|p| !p.sensitive);
                let has_value = std::iter::once(&stored.installation)
                    .chain(stored.devices.values())
                    .any(|profiles| {
                        profiles
                            .get(&profile.id)
                            .is_some_and(|values| values.contains_key(&parameter.name))
                    });
                if changed_to_plain && has_value {
                    return Err("Clear credential overrides in every configured scope before changing a sensitive parameter to an ordinary value".into());
                }
            }
        }
        Ok(())
    }
    // Validate the credential target and report keyring availability without returning the secret
    pub fn credential_status(&self, target: &CredentialTarget) -> Result<CredentialState, String> {
        validate(&self.snapshot(), target, None)?;
        let overrides = self.stored_parameter_overrides()?;
        let Some(value) = selected(&overrides, target) else {
            return Ok(CredentialState::Missing);
        };
        let Some(account) = credentials::reference(value) else {
            return Ok(CredentialState::NeedsReentry);
        };
        Ok(match self.credentials.provider.read(account) {
            Ok(_) => CredentialState::Configured,
            Err(state) => state,
        })
    }
    // Write credential
    pub fn write_credential(&self, write: CredentialWrite) -> Result<CredentialState, String> {
        self.write_credential_checked(write)
    }
    // Write credential checked
    fn write_credential_checked(&self, write: CredentialWrite) -> Result<CredentialState, String> {
        if write.value.is_empty() || write.value.len() > 64 * 1024 {
            return Err("Enter a credential between 1 byte and 64 KiB".into());
        }
        let _change = self.credentials.changes.lock().unwrap();
        validate(
            &self.snapshot(),
            &write.target,
            Some(write.expected_revision),
        )?;
        let mut overrides = self.stored_parameter_overrides()?;
        let old = selected(&overrides, &write.target)
            .and_then(credentials::reference)
            .map(str::to_owned);
        let account = format!("parameter-{}", uuid::Uuid::new_v4());
        self.credentials
            .provider
            .write(&account, &write.value)
            .map_err(|_| {
                "The OS credential store is locked or unavailable; the previous value was retained"
            })?;
        let result = (|| {
            let snapshot = self.state.lock().unwrap();
            validate(&snapshot, &write.target, Some(write.expected_revision))?;
            scope_mut(&mut overrides, &write.target)
                .insert(write.target.name.clone(), json!({"$credential":account}));
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::ParameterOverrides,
                &overrides,
            )
        })();
        if let Err(error) = result {
            let _ = self.credentials.provider.delete(&account);
            return Err(error);
        }
        self.credentials.remember(&write.value);
        if let Some(old) = old {
            let _ = self.credentials.provider.delete(&old);
        }
        crate::devices::sync_plugins(self);
        Ok(CredentialState::Configured)
    }
    // Clear credential
    pub fn clear_credential(
        &self,
        target: &CredentialTarget,
        expected_revision: u64,
    ) -> Result<CredentialState, String> {
        let _change = self.credentials.changes.lock().unwrap();
        let snapshot = self.state.lock().unwrap();
        validate(&snapshot, target, Some(expected_revision))?;
        let mut overrides = self.stored_parameter_overrides()?;
        let old = selected(&overrides, target)
            .and_then(credentials::reference)
            .map(str::to_owned);
        scope_mut(&mut overrides, target).remove(&target.name);
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::ParameterOverrides,
            &overrides,
        )?;
        drop(snapshot);
        if let Some(account) = old {
            self.credentials.provider.delete(&account).map_err(|_| {
                "Credential reference was cleared; OS credential removal is unavailable"
            })?;
        }
        crate::devices::sync_plugins(self);
        Ok(CredentialState::Missing)
    }
    // Resolve sensitive override
    pub(crate) fn resolve_sensitive_override(
        &self,
        profile: &Profile,
        overrides: &mut ParameterOverrides,
        device: &str,
        name: &str,
    ) -> Result<(), String> {
        if !profile
            .parameters
            .iter()
            .any(|parameter| parameter.name == name && parameter.sensitive)
        {
            return Ok(());
        }
        let device_value = overrides
            .devices
            .get_mut(device)
            .and_then(|profiles| profiles.get_mut(&profile.id))
            .and_then(|values| values.get_mut(name));
        let value = if let Some(value) = device_value {
            Some(value)
        } else {
            overrides
                .installation
                .get_mut(&profile.id)
                .and_then(|values| values.get_mut(name))
        };
        if let Some(value) = value {
            let account = credentials::reference(value)
                .ok_or("Migrate or replace the plaintext override for this sensitive parameter")?;
            *value = Value::String(self.credentials.read(account)?);
        }
        Ok(())
    }
    // Preserve existing credential references and reject secret changes through ordinary JSON overrides
    pub(crate) fn preserve_credentials(
        &self,
        next: &mut ParameterOverrides,
        snapshot: &Snapshot,
    ) -> Result<(), String> {
        let old = self.stored_parameter_overrides()?;
        for (device, profiles) in std::iter::once((None, &old.installation)).chain(
            old.devices
                .iter()
                .map(|(device, profiles)| (Some(device), profiles)),
        ) {
            for (profile_id, values) in profiles {
                if let Some(profile) = snapshot
                    .profiles
                    .iter()
                    .find(|profile| profile.id == *profile_id)
                {
                    for parameter in profile
                        .parameters
                        .iter()
                        .filter(|parameter| parameter.sensitive)
                    {
                        if let Some(value) = values.get(&parameter.name) {
                            let target = CredentialTarget {
                                profile_id: profile_id.clone(),
                                name: parameter.name.clone(),
                                device_id: device.cloned(),
                            };
                            if selected(next, &target)
                                .is_some_and(|next| !next.is_null() && next != value)
                            {
                                return Err(
                                    "Use credential controls to change sensitive values".into()
                                );
                            }
                            scope_mut(next, &target).insert(parameter.name.clone(), value.clone());
                        }
                    }
                }
            }
        }
        // JSON override commands cannot introduce a secret or point at another OS entry
        for (device, profiles) in std::iter::once((None, &next.installation)).chain(
            next.devices
                .iter()
                .map(|(device, profiles)| (Some(device), profiles)),
        ) {
            for (profile_id, values) in profiles {
                if let Some(profile) = snapshot
                    .profiles
                    .iter()
                    .find(|profile| profile.id == *profile_id)
                {
                    for parameter in profile
                        .parameters
                        .iter()
                        .filter(|parameter| parameter.sensitive)
                    {
                        let target = CredentialTarget {
                            profile_id: profile_id.clone(),
                            name: parameter.name.clone(),
                            device_id: device.cloned(),
                        };
                        if values.get(&parameter.name) != selected(&old, &target) {
                            return Err("Sensitive values require the OS credential store".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
