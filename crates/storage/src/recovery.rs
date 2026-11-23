//! Explicit cold restore also works when ordinary startup cannot read the primary
//! No normal open path invokes this operation or selects a backup automatically
use crate::{files, maintenance, schema, Connection, Snapshot, Storage};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct RecoverySource {
    pub path: PathBuf,
    pub connection: Connection,
    _directory: tempfile::TempDir,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub restored: Snapshot,
    pub preserved_directory: PathBuf,
    pub prior_sources: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl Storage {
    /// Explicitly restore an owned, offline root. `validate` must validate all
    /// application domains. `reconcile` can preserve permanent deletion metadata
    /// from readable prior sources, and returns warnings for unavailable evidence.
    /// Both callbacks use only their connections; no running services are started
    pub fn recover<F, G>(
        config_root: PathBuf,
        candidate: PathBuf,
        validate: F,
        reconcile: G,
    ) -> Result<RecoveryReport, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[RecoverySource]) -> Result<Vec<String>, String>,
    {
        // Keep ownership declared before all connections, so both normal return
        // and unwinding close SQLite handles before the explicit lock release.
        let (root, _owner) = crate::own_root(&config_root)?;
        crate::database_layout::prepare_owned(&root)?;
        recover_locked(&root, candidate, None, validate, reconcile)
    }
}

/// Caller retains exclusive root ownership throughout this operation
pub(crate) fn recover_locked<F, G>(
    root: &Path,
    candidate: PathBuf,
    expected_source: Option<&str>,
    validate: F,
    reconcile: G,
) -> Result<RecoveryReport, String>
where
    F: Fn(&Connection) -> Result<(), String>,
    G: FnOnce(&Connection, &[RecoverySource]) -> Result<Vec<String>, String>,
{
    recover_locked_inner(
        root,
        candidate,
        expected_source,
        validate,
        false,
        |db, sources| Ok((reconcile(db, sources)?, None)),
    )
}

pub(crate) fn recover_locked_inner<F, G>(
    root: &Path,
    candidate: PathBuf,
    expected_source: Option<&str>,
    validate: F,
    replaces_datastore: bool,
    reconcile: G,
) -> Result<RecoveryReport, String>
where
    F: Fn(&Connection) -> Result<(), String>,
    G: FnOnce(
        &Connection,
        &[RecoverySource],
    ) -> Result<(Vec<String>, Option<tempfile::NamedTempFile>), String>,
{
    if crate::installation_key::is_reset_marker(root)? {
        return Err("Complete the interrupted plugin settings reset before restoring a configuration backup".into());
    }
    if candidate.canonicalize().map_err(|e| e.to_string())?
        == crate::database_path(root, schema::DATABASE_NAME)
    {
        return Err("Select a backup, not the live configuration database".into());
    }
    let fingerprint = || -> Result<schema::Fingerprint, String> {
        let mut result = recovery_fingerprint(root)?;
        if replaces_datastore {
            result.extend(schema::fingerprint(&crate::database_path(
                root,
                crate::installation_key::NAME,
            ))?);
        }
        Ok(result)
    };
    let expected = fingerprint()?;
    check_source(&candidate, expected_source)?;
    let source = maintenance::open_snapshot(&candidate)?;
    check_source(&candidate, expected_source)?;
    let temporary = tempfile::NamedTempFile::new_in(crate::database_directory(root))
        .map_err(|e| e.to_string())?;
    let mut replacement = Connection::open(temporary.path()).map_err(|e| e.to_string())?;
    maintenance::copy_database(&source, &mut replacement)?;
    drop(source);
    schema::configure(&replacement)?;
    schema::validate(&replacement)?;
    validate(&replacement)?;
    crate::permissions::discard(&replacement)?;

    let recovery = crate::database_layout::ensure_recovery_directory(root)?;
    let preserved = recovery.join(format!("quarantine-{}", uuid::Uuid::new_v4()));
    files::ensure_directory(&preserved)?;
    // Copy all original evidence before opening the previous database. A
    // read-only SQLite connection can otherwise attempt hot-journal recovery.
    for filename in evidence_names() {
        preserve_regular(
            &schema::live_evidence_path(root, filename),
            &preserved.join(filename),
        )?;
    }
    if replaces_datastore {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let name = format!("{}{suffix}", crate::installation_key::NAME);
            preserve_regular(&crate::database_path(root, &name), &preserved.join(&name))?;
        }
    }
    files::sync_directory(&preserved)?;
    files::sync_directory(&recovery)?;
    let (sources, mut warnings) = prior_sources(root);
    let (reconciled_warnings, datastore) = reconcile(&replacement, &sources)?;
    if datastore.is_some() != replaces_datastore {
        return Err("Recovery did not stage the expected encrypted datastore".into());
    }
    warnings.extend(reconciled_warnings);
    if !replacement.is_autocommit() {
        return Err(
            "Recovery reconciliation left an uncommitted transaction; primary preserved".into(),
        );
    }
    replacement
        .execute(
            "UPDATE database_metadata SET value=?1 WHERE key='storage_generation'",
            [uuid::Uuid::new_v4().to_string()],
        )
        .map_err(|e| e.to_string())?;
    let changed = replacement.execute("UPDATE database_metadata SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='commit_sequence' AND CAST(value AS INTEGER)<9223372036854775807",[]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("Restored commit sequence is unavailable or exhausted".into());
    }
    schema::validate(&replacement)?;
    validate(&replacement)?;
    let prior_sources = sources.iter().map(|source| source.path.clone()).collect();
    drop(sources);
    replacement
        .close()
        .map_err(|(_, error)| error.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;

    check_source(&candidate, expected_source)?;
    if let Some(datastore) = &datastore {
        datastore.as_file().sync_all().map_err(|e| e.to_string())?;
    }
    if fingerprint()? != expected {
        return Err("Configuration changed during restore preparation; originals preserved. Reload recovery.".into());
    }

    // Fence normal startup across the multi-file replacement. If any removal,
    // rename, sync or final validation fails, the marker remains for explicit
    // recovery and the original byte-for-byte evidence remains in quarantine.
    let mut pending = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    serde_json::to_writer(
        pending.as_file_mut(),
        &serde_json::json!({"version":1,"preserved_directory":preserved,"candidate":candidate}),
    )
    .map_err(|e| e.to_string())?;
    pending.as_file_mut().flush().map_err(|e| e.to_string())?;
    pending.as_file().sync_all().map_err(|e| e.to_string())?;
    files::atomic_replace(pending.path(), &root.join(schema::RECOVERY_PENDING))?;
    for suffix in ["-journal", "-wal", "-shm"] {
        let path = crate::database_path(root, &format!("{}{suffix}", schema::DATABASE_NAME));
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Recovery remains fenced; raw evidence preserved: {error}"
                ));
            }
        }
    }
    files::sync_directory(&crate::database_directory(root))?;
    // Replace the encrypted datastore first. Both the prior and new Manager
    // database use the same key identity when the datastore already existed,
    // making an interrupted portable restore retriable under the fence.
    if let Some(datastore) = &datastore {
        files::atomic_replace(
            datastore.path(),
            &crate::database_path(root, crate::installation_key::NAME),
        )?;
        files::sync_directory(&crate::database_directory(root))?;
    }
    files::atomic_replace(
        temporary.path(),
        &crate::database_path(root, schema::DATABASE_NAME),
    )?;
    let published = schema::live_snapshot_copy(root)?;
    validate(&published)?;
    drop(published);
    let restored =
        maintenance::snapshot_metadata(crate::database_path(root, schema::DATABASE_NAME))?;
    fs::remove_file(root.join(schema::RECOVERY_PENDING)).map_err(|e| e.to_string())?;
    files::sync_directory(root)?;
    Ok(RecoveryReport {
        restored,
        preserved_directory: preserved,
        prior_sources,
        warnings,
    })
}

// List filenames that can contain recovery evidence
fn evidence_names() -> [&'static str; 5] {
    [
        schema::DATABASE_NAME,
        "manager.sqlite3-journal",
        "manager.sqlite3-wal",
        "manager.sqlite3-shm",
        schema::RECOVERY_PENDING,
    ]
}

// Preserve an existing regular recovery file
fn preserve_regular(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = match source.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Recovery evidence is not a regular file and was preserved: {}",
            source.display()
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(destination).map_err(|e| e.to_string())?;
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_json, write_json, Domain};

    // Inspect a potential database recovery candidate
    fn candidate(root: &Path) -> Snapshot {
        let storage = Storage::open(root.to_owned()).unwrap();
        storage
            .write_json(Domain::ManagerPreferences, "setting", &true, 32)
            .unwrap();
        storage.backup(root.join("candidate.sqlite3")).unwrap()
    }
    // Validate for the current operation
    fn validate(connection: &Connection) -> Result<(), String> {
        let _: Option<bool> = read_json(connection, Domain::ManagerPreferences, "setting", 32)?;
        Ok(())
    }

    #[test]
    // Verify explicit recovery preserves corrupt primary sidecars and legacy evidence
    fn explicit_recovery_preserves_corrupt_primary_sidecars_and_legacy_evidence() {
        let source = tempfile::tempdir().unwrap();
        let snapshot = candidate(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();

        fs::write(
            crate::database_path(target.path(), schema::DATABASE_NAME),
            b"corrupt database evidence",
        )
        .unwrap();
        assert!(Storage::open(target.path().to_owned()).is_err());
        fs::write(
            crate::database_path(target.path(), "manager.sqlite3-journal"),
            b"journal evidence",
        )
        .unwrap();
        fs::write(
            target.path().join("profiles/profiles.json"),
            b"legacy evidence",
        )
        .unwrap();
        let report = Storage::recover(target.path().to_owned(), snapshot.path, validate, |_, _| {
            Ok(vec![])
        })
        .unwrap();
        assert!(!report.warnings.is_empty());
        assert_ne!(
            report.restored.storage_generation,
            snapshot.storage_generation
        );
        assert_eq!(
            fs::read(report.preserved_directory.join(schema::DATABASE_NAME)).unwrap(),
            b"corrupt database evidence"
        );
        assert_eq!(
            fs::read(report.preserved_directory.join("manager.sqlite3-journal")).unwrap(),
            b"journal evidence"
        );
        assert_eq!(
            fs::read(target.path().join("profiles/profiles.json")).unwrap(),
            b"legacy evidence"
        );
        assert!(!crate::database_path(target.path(), "manager.sqlite3-journal").exists());
        assert!(!target.path().join(schema::RECOVERY_PENDING).exists());
        let restored = Storage::open(target.path().to_owned()).unwrap();
        assert_eq!(
            restored
                .read_json::<bool>(Domain::ManagerPreferences, "setting", 32)
                .unwrap(),
            Some(true)
        );
    }

    #[test]
    // Verify interrupted recovery with missing primary is preserved before fresh start
    fn interrupted_recovery_with_missing_primary_is_preserved_before_fresh_start() {
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        fs::write(
            target.path().join(schema::RECOVERY_PENDING),
            b"prior recovery evidence",
        )
        .unwrap();
        let storage = Storage::open(target.path().to_owned()).unwrap();
        assert_eq!(
            storage
                .read_json::<bool>(Domain::ManagerPreferences, "setting", 32)
                .unwrap(),
            None
        );
        let preserved = fs::read_dir(target.path())
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("missing-primary-preserved-")
            })
            .unwrap()
            .path();
        assert_eq!(
            fs::read(preserved.join(schema::RECOVERY_PENDING)).unwrap(),
            b"prior recovery evidence"
        );
        assert!(!target.path().join(schema::RECOVERY_PENDING).exists());
        drop(storage);
        assert!(Storage::open(target.path().to_owned()).is_ok());
    }

    #[test]
    // Verify rejected candidate or reconciliation never replaces primary
    fn rejected_candidate_or_reconciliation_never_replaces_primary() {
        let source = tempfile::tempdir().unwrap();
        let snapshot = candidate(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let primary = crate::database_path(target.path(), schema::DATABASE_NAME);
        fs::write(&primary, b"original corrupt evidence").unwrap();
        let bad_candidate = source.path().join("invalid.sqlite3");
        fs::write(&bad_candidate, b"invalid candidate").unwrap();
        assert!(Storage::recover(
            target.path().to_owned(),
            bad_candidate,
            validate,
            |_, _| Ok(vec![])
        )
        .is_err());
        assert_eq!(fs::read(&primary).unwrap(), b"original corrupt evidence");
        assert!(Storage::recover(
            target.path().to_owned(),
            snapshot.path,
            validate,
            |connection, _| {
                write_json(
                    connection,
                    Domain::ManagerPreferences,
                    "setting",
                    &"invalid",
                    32,
                )?;
                Ok(vec![])
            }
        )
        .is_err());
        assert_eq!(fs::read(&primary).unwrap(), b"original corrupt evidence");
        assert!(!target.path().join(schema::RECOVERY_PENDING).exists());
    }

    #[test]
    // Verify recovery uses prior sources and excludes a live owner
    fn recovery_uses_prior_sources_and_excludes_a_live_owner() {
        let source = tempfile::tempdir().unwrap();
        let snapshot = candidate(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let live = Storage::open(target.path().to_owned()).unwrap();
        live.write_json(Domain::ManagerPreferences, "protected", &42u64, 32)
            .unwrap();
        assert!(Storage::recover(
            target.path().to_owned(),
            snapshot.path.clone(),
            validate,
            |_, _| Ok(vec![])
        )
        .is_err());
        drop(live);
        let report = Storage::recover(
            target.path().to_owned(),
            snapshot.path,
            validate,
            |connection, sources| {
                let mut protected = 0;
                for source in sources {
                    protected = protected.max(
                        read_json::<u64>(
                            &source.connection,
                            Domain::ManagerPreferences,
                            "protected",
                            32,
                        )?
                        .unwrap_or(0),
                    );
                }
                write_json(
                    connection,
                    Domain::ManagerPreferences,
                    "protected",
                    &protected,
                    32,
                )?;
                Ok(vec![])
            },
        )
        .unwrap();
        assert!(report.prior_sources.contains(&crate::database_path(
            &target.path().canonicalize().unwrap(),
            schema::DATABASE_NAME
        )));
        let restored = Storage::open(target.path().to_owned()).unwrap();
        assert_eq!(
            restored
                .read_json::<u64>(Domain::ManagerPreferences, "protected", 32)
                .unwrap(),
            Some(42)
        );
    }

    #[test]
    // Verify maintenance validates after mutating reconciliation
    fn maintenance_validates_after_mutating_reconciliation() {
        let source = tempfile::tempdir().unwrap();
        let snapshot = candidate(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let live = Storage::open(target.path().to_owned()).unwrap();
        live.write_json(Domain::ManagerPreferences, "setting", &false, 32)
            .unwrap();
        live.add_snapshot_validator(validate).unwrap();
        {
            let maintenance = live.maintenance().unwrap();
            assert!(maintenance
                .restore(snapshot.path, |connection| write_json(
                    connection,
                    Domain::ManagerPreferences,
                    "setting",
                    &"invalid",
                    32
                ))
                .is_err());
        }
        assert_eq!(
            live.read_json::<bool>(Domain::ManagerPreferences, "setting", 32)
                .unwrap(),
            Some(false)
        );
    }
}

// Find prior database generations available for recovery
fn prior_sources(root: &Path) -> (Vec<RecoverySource>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut sources = Vec::new();
    // The immutable raw copies are never opened by SQLite, which may touch
    // journal/shared-memory files even while opening a read-only connection.
    let old_primary = crate::database_path(root, schema::DATABASE_NAME);
    match schema::live_snapshot_copy(root) {
            Ok(connection) => sources.push(RecoverySource {path:old_primary,connection: connection.connection, _directory: connection.directory}),
            Err(error) => warnings.push(format!("The prior primary could not be read. Raw evidence was preserved; deletion metadata newer than readable recovery snapshots may be unavailable: {error}")),
        }
    match crate::inspect_recovery_backups(root, 1024, |name, _, source| {
        let mut connection = schema::ScratchDatabase::new()?;
        maintenance::copy_database(source, &mut connection)?;
        Ok(RecoverySource {
            path: crate::recovery_directory(root).join(name),
            connection: connection.connection,
            _directory: connection.directory,
        })
    }) {
        Ok((backups, issues)) => {
            sources.extend(backups);
            warnings.extend(issues);
        }
        Err(error) => warnings.push(format!(
            "Recovery inventory could not be fully read; files were preserved: {error}"
        )),
    }
    (sources, warnings)
}

// Inspect a full restore without replacing the current database
pub(crate) fn preview_restore<F, G>(
    root: &Path,
    candidate: &Path,
    validate: F,
    reconcile: G,
) -> Result<RestorePreview, String>
where
    F: Fn(&Connection) -> Result<(), String>,
    G: FnOnce(&Connection, &[RecoverySource]) -> Result<Vec<String>, String>,
{
    if crate::installation_key::is_reset_marker(root)? {
        return Err("Complete the interrupted plugin settings reset before restoring a configuration backup".into());
    }
    let source_fingerprint = source_identity(candidate)?;
    let source = maintenance::open_snapshot(candidate)?;
    check_source(candidate, Some(&source_fingerprint))?;
    let mut replacement = schema::ScratchDatabase::new()?;
    maintenance::copy_database(&source, &mut replacement)?;
    drop(source);
    schema::configure(&replacement)?;
    schema::validate(&replacement)?;
    validate(&replacement)?;
    let (sources, mut warnings) = prior_sources(root);
    warnings.extend(reconcile(&replacement, &sources)?);
    if !replacement.is_autocommit() {
        return Err(
            "Recovery reconciliation left an uncommitted transaction; primary preserved".into(),
        );
    }
    schema::validate(&replacement)?;
    validate(&replacement)?;
    check_source(candidate, Some(&source_fingerprint))?;
    Ok(RestorePreview {
        source_fingerprint,
        warnings,
    })
}

// Compute the identity used to detect changed recovery data
fn recovery_fingerprint(root: &Path) -> Result<schema::Fingerprint, String> {
    let mut result = schema::fingerprint(&crate::database_path(root, schema::DATABASE_NAME))?;
    result.extend(schema::fingerprint(&root.join(schema::RECOVERY_PENDING))?);
    Ok(result)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestorePreview {
    pub source_fingerprint: String,
    pub warnings: Vec<String>,
}
// Read the identity of the selected recovery source
fn source_identity(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_vec(&schema::fingerprint(path)?).map_err(|e| e.to_string())?;
    Ok(hex::encode(Sha256::digest(encoded)))
}
// Check source
fn check_source(path: &Path, expected: Option<&str>) -> Result<(), String> {
    if let Some(expected) = expected {
        if source_identity(path)? != expected {
            return Err("Backup changed since review; preview it again before restoring".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod cold_editor_tests {
    use super::*;
    use crate::{Domain, RecoveryEditor};

    // Create a backup of the current database
    fn backup(root: &Path) -> PathBuf {
        let storage = Storage::open(root.to_owned()).unwrap();
        storage
            .write_json(Domain::ManagerPreferences, "value", &true, 32)
            .unwrap();
        storage.backup(root.join("candidate.sqlite3")).unwrap().path
    }
    // Validate for the current operation
    fn validate(conn: &Connection) -> Result<(), String> {
        let _: Option<bool> = crate::read_json(conn, Domain::ManagerPreferences, "value", 32)?;
        Ok(())
    }

    #[test]
    // Verify corrupt editor preview is readonly and restore preserves source and evidence
    fn corrupt_editor_preview_is_readonly_and_restore_preserves_source_and_evidence() {
        let source = tempfile::tempdir().unwrap();
        let candidate = backup(source.path());
        let candidate_bytes = fs::read(&candidate).unwrap();
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let primary = crate::database_path(target.path(), schema::DATABASE_NAME);
        fs::write(&primary, b"corrupt primary").unwrap();
        fs::write(
            crate::database_path(target.path(), "manager.sqlite3-wal"),
            b"orphan wal",
        )
        .unwrap();
        let editor = RecoveryEditor::open_for_recovery(target.path().into()).unwrap();
        assert!(editor.unavailable_reason().unwrap().is_some());
        assert!(editor.read(|_, _| Ok(())).is_err());
        assert!(Storage::open(target.path().into()).is_err());
        let revision = editor.revision().unwrap();
        let preview = editor
            .preview_restore(&revision, candidate.clone(), validate, |_, _| Ok(vec![]))
            .unwrap();
        assert!(!crate::recovery_directory(target.path()).exists());
        assert_eq!(fs::read(&primary).unwrap(), b"corrupt primary");
        let report = editor
            .restore_reviewed(
                &revision,
                candidate.clone(),
                &preview.source_fingerprint,
                validate,
                |_, _| Ok(vec![]),
            )
            .unwrap();
        assert_eq!(
            fs::read(report.preserved_directory.join(schema::DATABASE_NAME)).unwrap(),
            b"corrupt primary"
        );
        assert_eq!(
            fs::read(report.preserved_directory.join("manager.sqlite3-wal")).unwrap(),
            b"orphan wal"
        );
        assert_eq!(fs::read(&candidate).unwrap(), candidate_bytes);
        assert_eq!(editor.unavailable_reason().unwrap(), None);
        assert!(Storage::open(target.path().into()).is_err());
        assert!(editor
            .restore_reviewed(
                &revision,
                candidate.clone(),
                &preview.source_fingerprint,
                validate,
                |_, _| Ok(vec![])
            )
            .is_err());
        drop(editor);
        assert!(Storage::open(target.path().into()).is_ok());
    }

    #[test]
    // Verify reviewed source change and failed reconciliation never replace primary
    fn reviewed_source_change_and_failed_reconciliation_never_replace_primary() {
        let source = tempfile::tempdir().unwrap();
        let candidate = backup(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let primary = crate::database_path(target.path(), schema::DATABASE_NAME);
        fs::write(&primary, b"corrupt original").unwrap();
        let editor = RecoveryEditor::open_for_recovery(target.path().into()).unwrap();
        let revision = editor.revision().unwrap();
        let preview = editor
            .preview_restore(&revision, candidate.clone(), validate, |_, _| Ok(vec![]))
            .unwrap();
        let mut changed = fs::OpenOptions::new()
            .append(true)
            .open(&candidate)
            .unwrap();
        changed.write_all(b"changed").unwrap();
        drop(changed);
        assert!(editor
            .restore_reviewed(
                &revision,
                candidate.clone(),
                &preview.source_fingerprint,
                validate,
                |_, _| Ok(vec![])
            )
            .unwrap_err()
            .contains("changed since review"));
        assert_eq!(fs::read(&primary).unwrap(), b"corrupt original");
        let revision = editor.revision().unwrap();
        assert!(editor
            .restore(&revision, candidate, validate, |_, _| Err(
                "reconciliation rejected".into()
            ))
            .is_err());
        assert_eq!(fs::read(&primary).unwrap(), b"corrupt original");
        assert!(!target.path().join(schema::RECOVERY_PENDING).exists());
    }

    #[test]
    // Verify fenced primary is unavailable until explicit restore
    fn fenced_primary_is_unavailable_until_explicit_restore() {
        let source = tempfile::tempdir().unwrap();
        let candidate = backup(source.path());
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        let storage = Storage::open(target.path().into()).unwrap();
        drop(storage);
        fs::write(
            target.path().join(schema::RECOVERY_PENDING),
            b"interrupted evidence",
        )
        .unwrap();
        assert!(RecoveryEditor::open(target.path().into()).is_err());
        let editor = RecoveryEditor::open_for_recovery(target.path().into()).unwrap();
        assert!(editor.unavailable_reason().unwrap().is_some());
        let revision = editor.revision().unwrap();
        let report = editor
            .restore(&revision, candidate, validate, |_, _| Ok(vec![]))
            .unwrap();
        assert_eq!(
            fs::read(report.preserved_directory.join(schema::RECOVERY_PENDING)).unwrap(),
            b"interrupted evidence"
        );
        assert!(!target.path().join(schema::RECOVERY_PENDING).exists());
        assert!(editor.unavailable_reason().unwrap().is_none());
    }
    #[test]
    // Verify post publication validation failure keeps fence and exclusive owner
    fn post_publication_validation_failure_keeps_fence_and_exclusive_owner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let source = tempfile::tempdir().unwrap();
        let candidate = backup(source.path());
        let bytes = fs::read(&candidate).unwrap();
        let target = tempfile::tempdir().unwrap();
        crate::prepare_database_layout(target.path()).unwrap();
        fs::write(
            crate::database_path(target.path(), schema::DATABASE_NAME),
            b"original evidence",
        )
        .unwrap();
        let editor = RecoveryEditor::open_for_recovery(target.path().into()).unwrap();
        let revision = editor.revision().unwrap();
        let calls = AtomicUsize::new(0);
        let result = editor.restore(
            &revision,
            candidate.clone(),
            |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                    Err("publish validation rejected".into())
                } else {
                    Ok(())
                }
            },
            |_, _| Ok(vec![]),
        );
        assert!(result.is_err());
        assert!(target.path().join(schema::RECOVERY_PENDING).is_file());
        assert!(editor.unavailable_reason().unwrap().is_some());
        assert!(editor.read(|_, _| Ok(())).is_err());
        assert!(Storage::open(target.path().into()).is_err());
        assert_eq!(fs::read(candidate).unwrap(), bytes);
        let quarantine = fs::read_dir(crate::recovery_directory(target.path()))
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("quarantine-"))
            .unwrap()
            .path();
        assert_eq!(
            fs::read(quarantine.join(schema::DATABASE_NAME)).unwrap(),
            b"original evidence"
        );
        drop(editor);
        assert!(Storage::open(target.path().into())
            .unwrap_err()
            .contains("recovery did not finish"));
    }
}
