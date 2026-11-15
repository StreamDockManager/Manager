use super::*;
use serde_json::{json, Value};
use std::sync::mpsc;

// Construct the store test fixture
fn store() -> (tempfile::TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("config")).unwrap();
    (dir, storage)
}

#[test]
// Verify SQLite full rolls back records and revisions without resetting the database
fn sqlite_full_rolls_back_records_and_revisions_without_resetting_the_database() {
    let (_directory, storage) = store();
    storage
        .write_json(Domain::LocalPreferences, "kept", &json!({"value":42}), 1024)
        .unwrap();
    let revision_before = storage
        .run(|connection| revision(connection, Domain::LocalPreferences, "kept"))
        .unwrap();
    storage
        .run(|connection| {
            let pages: u64 = connection
                .query_row("PRAGMA page_count", [], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            connection
                .pragma_update(None, "max_page_count", pages)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let failure = storage
        .transaction(|transaction| {
            write_json(
                transaction,
                Domain::LocalPreferences,
                "kept",
                &json!({"value":99}),
                1024,
            )?;
            write_json(
                transaction,
                Domain::LocalPreferences,
                "large",
                &"x".repeat(2 * 1024 * 1024),
                4 * 1024 * 1024,
            )
        })
        .unwrap_err();
    assert!(failure.contains("full"), "{failure}");
    assert_eq!(
        storage
            .run(|connection| revision(connection, Domain::LocalPreferences, "kept"))
            .unwrap(),
        revision_before
    );
    let root = storage.root().to_owned();
    drop(storage);
    let restored = Storage::open(root).unwrap();
    assert_eq!(
        restored
            .read_json::<Value>(Domain::LocalPreferences, "kept", 1024)
            .unwrap(),
        Some(json!({"value":42}))
    );
    assert!(restored
        .read_json::<Value>(Domain::LocalPreferences, "large", 4 * 1024 * 1024)
        .unwrap()
        .is_none());
}

#[test]
// Verify SQLite busy has a bounded failure and accepts writes after release
fn sqlite_busy_has_a_bounded_failure_and_accepts_writes_after_release() {
    let (_directory, storage) = store();
    storage
        .write_json(Domain::LocalPreferences, "kept", &42, 64)
        .unwrap();
    let outside = rusqlite::Connection::open(storage.root().join("manager.sqlite3")).unwrap();
    outside.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = std::time::Instant::now();
    let error = storage
        .write_json(Domain::LocalPreferences, "kept", &99, 64)
        .unwrap_err();
    assert!(
        error.contains("locked") || error.contains("busy"),
        "{error}"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    outside.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        storage
            .read_json::<u64>(Domain::LocalPreferences, "kept", 64)
            .unwrap(),
        Some(42)
    );
    storage
        .write_json(Domain::LocalPreferences, "kept", &100, 64)
        .unwrap();
    assert_eq!(
        storage
            .read_json::<u64>(Domain::LocalPreferences, "kept", 64)
            .unwrap(),
        Some(100)
    );
}

#[test]
// Verify persists separate domains and unknown values
fn persists_separate_domains_and_unknown_values() {
    let (_dir, storage) = store();
    let value = json!({"unknown":{"nested":[false,0,null,"☃"]},"empty":""});
    storage
        .write_json(Domain::Profiles, "one", &value, 4096)
        .unwrap();
    storage
        .write_json(Domain::PluginGlobals, "one", &json!({"other":true}), 4096)
        .unwrap();
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "one", 4096)
            .unwrap(),
        Some(value.clone())
    );
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "missing", 4096)
            .unwrap(),
        None
    );
    let root = storage.root().to_owned();
    drop(storage);
    let storage = Storage::open(root).unwrap();
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "one", 4096)
            .unwrap(),
        Some(value)
    );
    assert_eq!(
        storage
            .list_json::<Value>(Domain::Profiles, 10, 4096)
            .unwrap()
            .len(),
        1
    );
}

#[test]
// Verify root lock is shared by clones and matches legacy owner
fn root_lock_is_shared_by_clones_and_matches_legacy_owner() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    let clone = storage.clone();
    drop(storage);
    assert!(Storage::open(root.clone())
        .unwrap_err()
        .contains("owns this configuration"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("profiles/writer.lock"))
        .unwrap();
    assert!(lock.try_lock().is_err());
    drop(clone);
    lock.try_lock().unwrap();
    assert!(Storage::open(root.clone()).is_err());
    lock.unlock().unwrap();
    drop(lock);
    Storage::open(root).unwrap();
}

#[test]
// Verify legacy data is preserved without import
fn legacy_data_is_preserved_without_import() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("profiles")).unwrap();
    let path = dir.path().join("profiles/profiles.json");
    std::fs::write(&path, b"legacy source").unwrap();
    let storage = Storage::open(dir.path().into()).unwrap();
    assert!(storage
        .read_json::<Value>(Domain::Profiles, "profiles", 4096)
        .unwrap()
        .is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"legacy source");
    assert!(dir.path().join(DATABASE_NAME).is_file());
}

#[test]
// Verify missing previously initialized database is fresh and preserves other files
fn missing_previously_initialized_database_is_fresh_and_preserves_other_files() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    storage
        .write_json(Domain::ManagerPreferences, "prior", &true, 64)
        .unwrap();
    let backup = storage.backup(root.join("saved.sqlite3")).unwrap();
    drop(storage);
    std::fs::remove_file(root.join(DATABASE_NAME)).unwrap();
    std::fs::write(root.join("desktop-settings.json"), b"legacy source").unwrap();
    for name in [
        "manager.sqlite3-journal",
        "manager.sqlite3-wal",
        "manager.sqlite3-shm",
    ] {
        std::fs::write(root.join(name), name).unwrap();
    }

    let storage = Storage::open(root.clone()).unwrap();
    assert!(storage
        .read_json::<bool>(Domain::ManagerPreferences, "prior", 64)
        .unwrap()
        .is_none());
    let preserved = std::fs::read_dir(&root)
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

    for name in [
        "manager.sqlite3-journal",
        "manager.sqlite3-wal",
        "manager.sqlite3-shm",
    ] {
        assert_eq!(
            std::fs::read(preserved.join(name)).unwrap(),
            name.as_bytes()
        );
    }
    assert!(backup.path.is_file());
}

#[test]
// Verify corrupt and foreign databases are preserved
fn corrupt_and_foreign_databases_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DATABASE_NAME);
    std::fs::write(&path, b"invalid sqlite contents").unwrap();
    assert!(Storage::open(dir.path().into()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid sqlite contents");
    std::fs::remove_file(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute("CREATE TABLE other (value TEXT)", []).unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(Storage::open(dir.path().into())
        .unwrap_err()
        .contains("different application"));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
// Verify newer schema is not modified or downgraded
fn newer_schema_is_not_modified_or_downgraded() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    storage
        .run(|conn| {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .map_err(|e| e.to_string())
        })
        .unwrap();
    drop(storage);
    let before = std::fs::read(root.join(DATABASE_NAME)).unwrap();
    assert!(Storage::open(root.clone())
        .unwrap_err()
        .contains("Unsupported"));
    assert_eq!(std::fs::read(root.join(DATABASE_NAME)).unwrap(), before);
}

#[test]
// Verify connection enforces durability and foreign keys
fn connection_enforces_durability_and_foreign_keys() {
    let (_dir, storage) = store();
    storage
        .run(|conn| {
            assert_eq!(
                conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "delete"
            );
            assert_eq!(
                conn.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                3
            );
            assert_eq!(
                conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(conn
                .execute(
                    "INSERT INTO plugin_storage_records VALUES('missing','missing','key','null',4)",
                    []
                )
                .is_err());
            Ok(())
        })
        .unwrap();
    assert!(storage.stats().unwrap().sqlite_version.starts_with('3'));
}

#[test]
// Verify transaction failure preserves every domain and revision
fn transaction_failure_preserves_every_domain_and_revision() {
    let (_dir, storage) = store();
    storage
        .write_json(Domain::Profiles, "profile", &json!(1), 100)
        .unwrap();
    let failure: Result<(), String> = storage.transaction(|tx| {
        write_json(tx, Domain::Profiles, "profile", &json!(2), 100)?;
        write_json(tx, Domain::PluginGlobals, "plugin", &json!(3), 100)?;
        Err("injected failure".into())
    });
    assert_eq!(failure.unwrap_err(), "injected failure");
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "profile", 100)
            .unwrap(),
        Some(json!(1))
    );
    assert_eq!(
        storage
            .read_json::<Value>(Domain::PluginGlobals, "plugin", 100)
            .unwrap(),
        None
    );
    assert_eq!(
        storage
            .run(|conn| revision(conn, Domain::Profiles, "profile"))
            .unwrap(),
        Some(1)
    );
}

#[test]
// Verify panicking transaction rolls back and worker survives
fn panicking_transaction_rolls_back_and_worker_survives() {
    let (_dir, storage) = store();
    let result: Result<(), String> = storage.transaction(|tx| {
        write_json(tx, Domain::Profiles, "profile", &json!(2), 100)?;
        panic!("deliberate test failure")
    });
    assert!(result.is_err());
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "profile", 100)
            .unwrap(),
        None
    );
    storage
        .write_json(Domain::Profiles, "profile", &json!(3), 100)
        .unwrap();
}

#[test]
// Verify bounds apply to serialization reads lists and admission
fn bounds_apply_to_serialization_reads_lists_and_admission() {
    let (_dir, storage) = store();
    assert!(storage
        .write_json(Domain::Profiles, "one", &"☃☃", 7)
        .is_err());
    storage
        .write_json(Domain::Profiles, "one", &"☃☃", 8)
        .unwrap();
    assert!(storage
        .read_json::<Value>(Domain::Profiles, "one", 7)
        .is_err());
    assert!(storage
        .list_json::<Value>(Domain::Profiles, 0, 100)
        .is_err());
    assert!(storage.list_json::<Value>(Domain::Profiles, 1, 10).is_err());
    assert!(storage
        .run_with_cost(MAX_ADMITTED_BYTES + 1, |_| Ok(()))
        .is_err());
    assert!(storage
        .write_json(Domain::Profiles, "", &json!(null), 100)
        .is_err());
}

#[test]
// Verify busy admission is bounded and releases after completion
fn busy_admission_is_bounded_and_releases_after_completion() {
    let (_dir, storage) = store();
    let other = storage.clone();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        other.run_with_cost(MAX_ADMITTED_BYTES, move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    entered_rx.recv().unwrap();
    assert!(storage
        .run_with_cost(1, |_| Ok(()))
        .unwrap_err()
        .contains("busy"));
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    storage.run_with_cost(1, |_| Ok(())).unwrap();
}

#[test]
// Verify maintenance fences old epochs and blocks all normal commands
fn maintenance_fences_old_epochs_and_blocks_all_normal_commands() {
    let (_dir, storage) = store();
    let old = storage.generation();
    let maintenance = storage.maintenance().unwrap();
    assert!(storage
        .write_json(Domain::Profiles, "one", &json!(1), 100)
        .is_err());
    assert!(storage
        .read_json::<Value>(Domain::Profiles, "one", 100)
        .is_err());
    maintenance.inspect(schema::validate).unwrap();
    drop(maintenance);
    assert!(storage.run_at_generation(old, 0, |_| Ok(())).is_err());
    storage
        .write_json(Domain::Profiles, "one", &json!(2), 100)
        .unwrap();
}

#[test]
// Verify backups are consistent and restore waits for cache rebuild
fn backups_are_consistent_and_restore_waits_for_cache_rebuild() {
    let (dir, storage) = store();
    storage
        .write_json(Domain::Profiles, "one", &json!({"revision":1}), 100)
        .unwrap();
    let backup = storage.backup(dir.path().join("export.sqlite3")).unwrap();
    storage
        .write_json(Domain::Profiles, "one", &json!({"revision":2}), 100)
        .unwrap();
    assert_eq!(
        storage
            .read_snapshot(&backup.path, |conn| read_json::<Value>(
                conn,
                Domain::Profiles,
                "one",
                100
            ))
            .unwrap(),
        Some(json!({"revision":1}))
    );
    let old_generation = storage.stats().unwrap().storage_generation;
    let maintenance = storage.maintenance().unwrap();
    let before = maintenance
        .restore(backup.path, |conn| {
            let value = read_json::<Value>(conn, Domain::Profiles, "one", 100)?.unwrap();
            if value["revision"] != 1 {
                return Err("invalid profile".into());
            }
            Ok(())
        })
        .unwrap();
    assert!(before.path.exists());
    assert_eq!(
        maintenance
            .inspect(|conn| read_json::<Value>(conn, Domain::Profiles, "one", 100))
            .unwrap(),
        Some(json!({"revision":1}))
    );
    assert!(storage.run(|_| Ok(())).is_err());
    drop(maintenance);
    assert_ne!(storage.stats().unwrap().storage_generation, old_generation);
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "one", 100)
            .unwrap(),
        Some(json!({"revision":1}))
    );
    assert_eq!(storage.backup_paths().unwrap(), vec![before.path]);
}

#[test]
// Verify rejected restore keeps current database and backup cannot replace live file
fn rejected_restore_keeps_current_database_and_backup_cannot_replace_live_file() {
    let (dir, storage) = store();
    storage
        .write_json(Domain::Profiles, "one", &json!(1), 100)
        .unwrap();
    let snapshot = storage.backup(dir.path().join("export.sqlite3")).unwrap();
    storage
        .write_json(Domain::Profiles, "one", &json!(2), 100)
        .unwrap();
    let maintenance = storage.maintenance().unwrap();
    assert!(maintenance
        .restore(snapshot.path, |_| Err("domain validation rejected".into()))
        .is_err());
    drop(maintenance);
    assert_eq!(
        storage
            .read_json::<Value>(Domain::Profiles, "one", 100)
            .unwrap(),
        Some(json!(2))
    );
    assert!(storage.backup(storage.root().join(DATABASE_NAME)).is_err());
}

#[test]
// Verify recursive access is rejected instead of deadlocking
fn recursive_access_is_rejected_instead_of_deadlocking() {
    let (_dir, storage) = store();
    let nested = storage.clone();
    assert!(storage
        .run(move |_| nested.run(|_| Ok(())))
        .unwrap_err()
        .contains("Recursive"));
}

#[test]
// Verify backup validation precedes publication and retention is bounded
fn backup_validation_precedes_publication_and_retention_is_bounded() {
    let (dir, storage) = store();
    storage
        .add_snapshot_validator(|conn| {
            if read_json::<Value>(conn, Domain::Profiles, "one", 100)? == Some(json!("invalid")) {
                return Err("invalid profile fixture".into());
            }
            Ok(())
        })
        .unwrap();
    storage
        .write_json(Domain::Profiles, "one", &json!(1), 100)
        .unwrap();
    let destination = dir.path().join("snapshot.sqlite3");
    let original = storage.backup(destination.clone()).unwrap();
    assert_eq!(original.revision, "1");
    let bytes = std::fs::read(&destination).unwrap();
    storage
        .write_json(Domain::Profiles, "one", &json!("invalid"), 100)
        .unwrap();
    assert!(storage.backup(destination.clone()).is_err());
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
    for value in 2..37 {
        storage
            .write_json(Domain::Profiles, "one", &json!(value), 100)
            .unwrap();
        let point = storage.backup_recovery().unwrap();
        assert!(point.path.exists());
        assert!(storage.backup_paths().unwrap().len() <= 32);
    }
    assert_eq!(storage.stats().unwrap().recovery_snapshots.len(), 32);
}

#[test]
// Verify final shutdown creates a recovery point for unbacked commits
fn final_shutdown_creates_a_recovery_point_for_unbacked_commits() {
    let (_dir, storage) = store();
    storage
        .write_json(
            Domain::ManagerPreferences,
            "desktop",
            &json!({"theme":"dark"}),
            100,
        )
        .unwrap();
    let root = storage.root().to_owned();
    assert!(storage.backup_paths().unwrap().is_empty());
    drop(storage);
    let reopened = Storage::open(root).unwrap();
    let paths = reopened.backup_paths().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(
        reopened
            .read_snapshot(&paths[0], |conn| read_json::<Value>(
                conn,
                Domain::ManagerPreferences,
                "desktop",
                100
            ))
            .unwrap(),
        Some(json!({"theme":"dark"}))
    );
}

#[test]
// Verify subprocess helper
fn subprocess_helper() {
    let Some(root) = std::env::var_os("SDM_STORAGE_TEST_ROOT") else {
        return;
    };
    if std::env::var_os("SDM_STORAGE_TEST_LOCK").is_some() {
        assert!(Storage::open(PathBuf::from(root))
            .unwrap_err()
            .contains("owns this configuration"));
        return;
    }
    let storage = Storage::open(PathBuf::from(root)).unwrap();
    let _: Result<(), String> = storage.run(|conn| {
        conn.execute_batch("PRAGMA cache_size=1; BEGIN IMMEDIATE;")
            .unwrap();
        for n in 0..100 {
            write_json(
                conn,
                Domain::Profiles,
                &format!("row-{n}"),
                &"x".repeat(4096),
                8192,
            )
            .unwrap();
        }
        std::process::exit(41)
    });
    unreachable!();
}

#[test]
// Verify other process is excluded and interrupted transaction recovers
fn other_process_is_excluded_and_interrupted_transaction_recovers() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    let executable = std::env::current_exe().unwrap();
    let exclusion = std::process::Command::new(&executable)
        .args(["--exact", "tests::subprocess_helper"])
        .env("SDM_STORAGE_TEST_ROOT", &root)
        .env("SDM_STORAGE_TEST_LOCK", "1")
        .output()
        .unwrap();
    assert!(
        exclusion.status.success(),
        "{}",
        String::from_utf8_lossy(&exclusion.stderr)
    );
    storage
        .write_json(Domain::Profiles, "preserved", &json!(true), 100)
        .unwrap();
    drop(storage);
    let interrupted = std::process::Command::new(&executable)
        .args(["--exact", "tests::subprocess_helper"])
        .env("SDM_STORAGE_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert_eq!(interrupted.status.code(), Some(41));
    assert!(root.join("manager.sqlite3-journal").exists());
    let storage = Storage::open(root).unwrap();
    assert_eq!(
        storage
            .list_json::<Value>(Domain::Profiles, 1000, 1024 * 1024)
            .unwrap(),
        vec![("preserved".into(), json!(true))]
    );
}

#[test]
// Verify unsupported exact schema and contract are rejected without touching files
fn unsupported_exact_schema_and_contract_are_rejected_without_touching_files() {
    for modification in [
        "CREATE TABLE obsolete(value TEXT)",
        "CREATE INDEX obsolete ON profiles(revision)",
        "DROP TABLE plugin_storage_receipts",
        "DELETE FROM database_metadata WHERE key='contract'",
        "UPDATE database_metadata SET value='other' WHERE key='contract'",
        "PRAGMA user_version=2",
        "PRAGMA application_id=1",
    ] {
        let (_dir, storage) = store();
        let root = storage.root().to_owned();
        let sql = modification.to_owned();
        storage
            .run(move |connection| connection.execute_batch(&sql).map_err(|e| e.to_string()))
            .unwrap();
        drop(storage);
        let original = std::fs::read(root.join(DATABASE_NAME)).unwrap();
        assert!(Storage::open(root.clone()).is_err(), "{modification}");
        assert_eq!(
            std::fs::read(root.join(DATABASE_NAME)).unwrap(),
            original,
            "{modification}"
        );
        assert!(!root.join(format!("{DATABASE_NAME}-journal")).exists());
    }
}

#[test]
// Verify rejected wal and domain payload preserve primary and sidecars
fn rejected_wal_and_domain_payload_preserve_primary_and_sidecars() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    drop(storage);
    let connection = rusqlite::Connection::open(root.join(DATABASE_NAME)).unwrap();
    connection.execute_batch("PRAGMA journal_mode=WAL; UPDATE database_metadata SET value='unsupported' WHERE key='contract';").unwrap();
    let names = [
        DATABASE_NAME.to_owned(),
        format!("{DATABASE_NAME}-wal"),
        format!("{DATABASE_NAME}-shm"),
    ];
    let before: Vec<_> = names
        .iter()
        .map(|name| std::fs::read(root.join(name)).unwrap())
        .collect();
    assert!(Storage::open(root.clone()).is_err());
    for (name, bytes) in names.iter().zip(before) {
        assert_eq!(std::fs::read(root.join(name)).unwrap(), bytes, "{name}");
    }
    drop(connection);

    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    drop(storage);
    let before = std::fs::read(root.join(DATABASE_NAME)).unwrap();
    assert!(Storage::open_validated(root.clone(), |_| Err("unsupported payload".into())).is_err());
    assert_eq!(std::fs::read(root.join(DATABASE_NAME)).unwrap(), before);
    assert!(!root.join(format!("{DATABASE_NAME}-journal")).exists());
}

#[test]
// Verify missing primary with recovery evidence initializes without import
fn missing_primary_with_recovery_evidence_initializes_without_import() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("recovery")).unwrap();
    let evidence = root.path().join("recovery/prior.sqlite3");
    std::fs::write(&evidence, b"preserve recovery evidence").unwrap();
    assert!(Storage::open(root.path().into()).is_ok());
    assert!(root.path().join(DATABASE_NAME).is_file());
    assert_eq!(
        std::fs::read(evidence).unwrap(),
        b"preserve recovery evidence"
    );
}

#[test]
// Verify external change during preflight is detected before original open
fn external_change_during_preflight_is_detected_before_original_open() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    drop(storage);
    let primary = root.join(DATABASE_NAME);
    let changed = primary.clone();
    let result = Storage::open_validated(root, move |_| {
        std::fs::write(&changed, b"external writer changed the primary").map_err(|e| e.to_string())
    });
    assert!(result.unwrap_err().contains("changed during validation"));
    assert_eq!(
        std::fs::read(primary).unwrap(),
        b"external writer changed the primary"
    );
}

#[test]
// Verify owner unlocks after lifetime even with an inherited description
fn owner_unlocks_after_lifetime_even_with_an_inherited_description() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        // Prove the close-only behavior this guard fixes on the current platform
        let path = root.path().join("close-only-lock");
        let original = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        original.try_lock().unwrap();
        let duplicate = original.try_clone().unwrap();
        drop(original);
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert!(contender.try_lock().is_err());
        drop(duplicate);
        contender.try_lock().unwrap();
        contender.unlock().unwrap();
    }
    let (_, owner) = super::own_root(root.path()).unwrap();
    // try_clone shares the file description just as fork inheritance does,
    // without forking a multithreaded test process or changing descriptor flags.
    let inherited = owner.0.try_clone().unwrap();
    assert!(super::own_root(root.path()).is_err());
    drop(owner);
    let (_, next) = super::own_root(root.path()).unwrap();
    assert!(super::own_root(root.path()).is_err());
    drop(inherited);
    assert!(super::own_root(root.path()).is_err());
    drop(next);
    super::own_root(root.path()).unwrap();
}

#[test]
// Verify worker shutdown closes database and releases ownership before returning
fn worker_shutdown_closes_database_and_releases_ownership_before_returning() {
    let root = tempfile::tempdir().unwrap();
    for iteration in 0..16 {
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .write_json(Domain::ManagerPreferences, "iteration", &iteration, 64)
            .unwrap();
        assert!(Storage::open(root.path().into()).is_err());
        drop(storage);
        let reopened = Storage::open(root.path().into()).unwrap();
        assert_eq!(
            reopened
                .read_json::<i32>(Domain::ManagerPreferences, "iteration", 64)
                .unwrap(),
            Some(iteration)
        );
        drop(reopened);
    }
}

#[test]
// Verify startup failure and panic finish releasing ownership before return
fn startup_failure_and_panic_finish_releasing_ownership_before_return() {
    let root = tempfile::tempdir().unwrap();
    drop(Storage::open(root.path().into()).unwrap());
    assert!(
        Storage::open_validated(root.path().into(), |_| Err("fixture rejection".into())).is_err()
    );
    drop(Storage::open(root.path().into()).unwrap());
    assert!(
        Storage::open_validated(root.path().into(), |_| panic!("fixture startup panic")).is_err()
    );
    drop(Storage::open(root.path().into()).unwrap());
}

#[test]
// Verify repeated missing primary quarantines never clobber prior evidence
fn repeated_missing_primary_quarantines_never_clobber_prior_evidence() {
    let root = tempfile::tempdir().unwrap();
    let name = "manager.sqlite3-wal";
    for payload in [b"first".as_slice(), b"second".as_slice()] {
        std::fs::write(root.path().join(name), payload).unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        assert!(!root.path().join(name).exists());
        drop(storage);
        std::fs::remove_file(root.path().join(DATABASE_NAME)).unwrap();
    }
    let mut preserved: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("missing-primary-preserved-")
        })
        .map(|entry| std::fs::read(entry.path().join(name)).unwrap())
        .collect();
    preserved.sort();
    assert_eq!(preserved, [b"first".to_vec(), b"second".to_vec()]);
}

#[cfg(unix)]
#[test]
// Verify missing primary preserves orphan symlink without touching target
fn missing_primary_preserves_orphan_symlink_without_touching_target() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("journal");
    std::fs::write(&target, b"untouched target").unwrap();
    symlink(&target, root.path().join("manager.sqlite3-journal")).unwrap();
    let storage = Storage::open(root.path().into()).unwrap();
    let preserved = std::fs::read_dir(root.path())
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
        std::fs::read_link(preserved.join("manager.sqlite3-journal")).unwrap(),
        target
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched target");
    drop(storage);
}

#[test]
// Verify pending recovery with existing primary remains fenced
fn pending_recovery_with_existing_primary_remains_fenced() {
    let (_dir, storage) = store();
    let root = storage.root().to_owned();
    drop(storage);
    std::fs::write(root.join(crate::schema::RECOVERY_PENDING), b"pending").unwrap();
    let original = std::fs::read(root.join(DATABASE_NAME)).unwrap();
    assert!(Storage::open(root.clone())
        .unwrap_err()
        .contains("recovery did not finish"));
    assert_eq!(std::fs::read(root.join(DATABASE_NAME)).unwrap(), original);
}

#[test]
// Verify independent backup inventory reads without primary and across generations
fn independent_backup_inventory_reads_without_primary_and_across_generations() {
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::open(root.path().into()).unwrap();
    storage
        .write_json(Domain::ManagerPreferences, "prior", &true, 64)
        .unwrap();
    std::fs::create_dir_all(root.path().join("recovery")).unwrap();
    let backup = root.path().join("recovery/prior.sqlite3");
    storage.backup(backup.clone()).unwrap();
    drop(storage);
    std::fs::remove_file(root.path().join(DATABASE_NAME)).unwrap();
    let inspect = || {
        crate::inspect_recovery_backups(root.path(), 10, |name, _, conn| {
            Ok((
                name.to_owned(),
                crate::read_json::<bool>(conn, Domain::ManagerPreferences, "prior", 64)?,
            ))
        })
        .unwrap()
    };
    let (before, warnings) = inspect();
    assert!(warnings.is_empty());
    assert!(before.contains(&("prior.sqlite3".into(), Some(true))));
    assert!(!root.path().join(DATABASE_NAME).exists());
    let fresh = Storage::open(root.path().into()).unwrap();
    assert_eq!(inspect().0, before);
    drop(fresh);
    let editor = crate::RecoveryEditor::open(root.path().into()).unwrap();
    assert!(editor
        .inspect_backups(10, |_, _, _| Ok(()))
        .unwrap()
        .0
        .is_empty());
    std::fs::write(root.path().join("recovery/prior.sqlite3-wal"), b"orphan").unwrap();
    let (found, warnings) = inspect();
    assert!(!found.iter().any(|(name, _)| name == "prior.sqlite3"));
    assert!(warnings.iter().any(|w| w.contains("sidecars")));
    assert_eq!(
        std::fs::read(root.path().join("recovery/prior.sqlite3-wal")).unwrap(),
        b"orphan"
    );
}

#[test]
// Verify fresh primary retention counts prior generation backups
fn fresh_primary_retention_counts_prior_generation_backups() {
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::open(root.path().into()).unwrap();
    storage
        .write_json(Domain::ManagerPreferences, "old", &true, 64)
        .unwrap();
    let mut originals = Vec::new();
    for index in 0..5 {
        let path = root.path().join(format!("recovery/old-{index}.sqlite3"));
        storage.backup(path.clone()).unwrap();
        originals.push((path.clone(), std::fs::read(path).unwrap()));
    }
    drop(storage);
    // Capture five old-generation files after shutdown; new-generation snapshots
    // must include these files in the same recovery-directory budget.
    let source = originals[0].1.clone();
    for (path, _) in &originals {
        std::fs::write(path, &source).unwrap();
    }
    std::fs::remove_file(root.path().join(DATABASE_NAME)).unwrap();
    let fresh = Storage::open(root.path().into()).unwrap();
    fresh
        .write_json(Domain::ManagerPreferences, "new", &true, 64)
        .unwrap();
    for _ in 0..35 {
        fresh.backup_recovery().unwrap();
    }
    drop(fresh);
    assert!(originals.iter().all(|(path, _)| !path.exists()));
    let count = std::fs::read_dir(root.path().join("recovery"))
        .unwrap()
        .count();
    assert!(count <= 32);
}

#[test]
// Verify recovery retention detects either budget limit
fn recovery_retention_detects_either_budget_limit() {
    use crate::retention::{exceeds, BYTES_LIMIT};
    assert_eq!(BYTES_LIMIT, 1_073_741_824);
    assert!(!exceeds(32, BYTES_LIMIT));
    assert!(exceeds(33, 1));
    assert!(exceeds(2, BYTES_LIMIT + 1));
    assert!(exceeds(1, BYTES_LIMIT + 1));
    assert!(!exceeds(0, 0));
}

#[test]
// Verify successful snapshot reports incomplete cleanup as a warning
fn successful_snapshot_reports_incomplete_cleanup_as_a_warning() {
    let (_root, storage) = store();
    let mut deep = storage.root().join("recovery");
    for _ in 0..66 {
        deep.push("nested");
    }
    std::fs::create_dir_all(deep).unwrap();
    let point = storage.backup_recovery().unwrap();
    assert!(point.path.exists());
    assert!(storage
        .stats()
        .unwrap()
        .last_backup_error
        .unwrap()
        .contains("cleanup deferred"));
}
