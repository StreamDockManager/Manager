//! Recovery-only operations for the installation-encrypted plugin settings store
use super::ConfigurationRecovery;
use serde::Serialize;

#[derive(Serialize, ts_rs::TS)]
pub struct ConfigurationRecoveryKeystoreBackup {
    pub id: String,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub modified_ms: u64,
}

#[derive(Serialize, ts_rs::TS)]
pub struct ConfigurationRecoveryKeystoreStatus {
    pub state: String,
    pub reason: Option<String>,
    pub revision: String,
    pub backups: Vec<ConfigurationRecoveryKeystoreBackup>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct ConfigurationRecoveryKeystoreReview {
    pub review_token: String,
    pub warnings: Vec<String>,
}

pub(super) struct Review {
    revision: String,
    token: String,
    operation: Operation,
}

enum Operation {
    Create,
    Reset,
    Restore(String),
}

impl ConfigurationRecovery {
    pub fn keystore_status(&self) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
        self.editor.refresh()?;
        let revision = self.editor.revision()?;
        let (state, reason) = self.editor.plugin_settings_status()?;
        let backups = self
            .editor
            .plugin_settings_backups()?
            .into_iter()
            .map(|backup| ConfigurationRecoveryKeystoreBackup {
                id: backup.id,
                bytes: backup.bytes,
                modified_ms: backup.modified_ms,
            })
            .collect();
        Ok(ConfigurationRecoveryKeystoreStatus {
            state,
            reason,
            revision,
            backups,
        })
    }

    pub fn retry_keystore(
        &self,
        revision: &str,
    ) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
        *self
            .keystore_review
            .lock()
            .map_err(|_| "Recovery lock failed")? = None;
        self.editor.retry_plugin_settings(revision)?;
        self.keystore_status()
    }

    pub fn review_keystore(
        &self,
        revision: &str,
        operation: &str,
        backup_id: Option<&str>,
    ) -> Result<ConfigurationRecoveryKeystoreReview, String> {
        let mut review = self
            .keystore_review
            .lock()
            .map_err(|_| "Recovery lock failed")?;
        *review = None;
        let (operation, token, warnings) = match (operation, backup_id) {
            ("create", None) => (
                Operation::Create,
                self.editor.preview_plugin_settings_create(revision)?.token,
                vec![
                    "Create starts with an empty encrypted keystore. It does not restore settings from the missing file.".into(),
                    "The existing configuration and your personal Vault are preserved. You may need to re-enter protected settings or approve access again.".into(),
                ],
            ),
            ("reset", None) => (
                Operation::Reset,
                self.editor.preview_plugin_settings_reset(revision)?.token,
                vec![
                    "Reset starts with empty protected plugin settings. Plugins may need credentials and other settings entered again.".into(),
                    "The original database and keystore files are preserved. Profiles and the separate user Vault are not reset.".into(),
                    "Some plugins may need permission or Vault access approved again.".into(),
                ],
            ),
            ("restore", Some(id)) => (
                Operation::Restore(id.into()),
                self.editor.preview_plugin_settings_restore(revision, id)?.token,
                vec![
                    "Restore uses the original operating-system credential or installation key file. The encrypted backup cannot replace that key.".into(),
                    "The backup must match this configuration database. The original files are preserved and the separate user Vault is not changed.".into(),
                ],
            ),
            _ => return Err("Choose a keystore recovery operation and its matching backup".into()),
        };
        *review = Some(Review {
            revision: revision.into(),
            token: token.clone(),
            operation,
        });
        Ok(ConfigurationRecoveryKeystoreReview {
            review_token: token,
            warnings,
        })
    }

    pub fn apply_keystore(
        &self,
        revision: &str,
        review_token: &str,
    ) -> Result<ConfigurationRecoveryKeystoreStatus, String> {
        let review = self
            .keystore_review
            .lock()
            .map_err(|_| "Recovery lock failed")?
            .take()
            .filter(|review| review.revision == revision && review.token == review_token)
            .ok_or("Review the keystore operation again before applying it")?;
        let preserved = match review.operation {
            Operation::Create => self.editor.create_plugin_settings(revision, review_token)?,
            Operation::Reset => self.editor.reset_plugin_settings(revision, review_token)?,
            Operation::Restore(id) => {
                self.editor
                    .restore_plugin_settings(revision, &id, review_token)?
            }
        };
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? = Some(preserved.display().to_string());
        self.keystore_status()
    }
}
