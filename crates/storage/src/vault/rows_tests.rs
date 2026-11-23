use super::{data_key, rows::RowKind, EncryptedVault, FILE_NAME};
use serde_json::{json, Value};

#[test]
fn encrypted_row_transactions_isolate_records_and_rollback_together() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FILE_NAME);
    let id = uuid::Uuid::new_v4();
    let key = data_key().unwrap();
    let good = "fixture-row-key-good";
    let other = "fixture-row-key-other";
    let stage = EncryptedVault::stage_rows(root.path(), id, &key, |rows| {
        rows.put(RowKind::Entry, good, "", &"fixture-label-2233")?;
        rows.put(RowKind::EntryValue, good, "", &json!("fixture-secret-3344"))?;
        rows.put(RowKind::EntryValue, other, "", &json!({"held": true}))?;
        rows.put(
            RowKind::Grant,
            "grant-good",
            "fixture-owner-a",
            &"fixture-grant",
        )?;
        rows.put(
            RowKind::Grant,
            "grant-other",
            "fixture-owner-b",
            &json!({"foreign": true}),
        )?;
        rows.set_revision(4)
    })
    .unwrap();
    stage.persist_noclobber(&path).unwrap();
    let vault = EncryptedVault::open(&path, id, &key).unwrap();
    assert_eq!(
        vault
            .rows(|rows| rows.get::<String>(RowKind::EntryValue, good))
            .unwrap(),
        Some("fixture-secret-3344".into())
    );
    assert!(vault
        .rows(|rows| rows.get::<String>(RowKind::EntryValue, other))
        .is_err());
    assert_eq!(
        vault
            .rows(|rows| rows.list::<String>(RowKind::Grant, Some("fixture-owner-a")))
            .unwrap(),
        vec![("grant-good".into(), "fixture-grant".into())],
    );
    assert!(vault
        .rows(|rows| rows.list::<String>(RowKind::Grant, None))
        .is_err());
    let failed: Result<(), String> = vault.rows(|rows| {
        rows.put(RowKind::Entry, good, "", &"rolled-back-label")?;
        rows.delete(RowKind::EntryValue, other)?;
        rows.set_revision(19)?;
        Err("rollback-fixture".into())
    });
    assert_eq!(failed.unwrap_err(), "rollback-fixture");
    vault
        .rows(|rows| {
            assert_eq!(rows.revision()?, 4);
            assert_eq!(
                rows.get::<Value>(RowKind::EntryValue, other)?,
                Some(json!({"held": true}))
            );
            assert_eq!(
                rows.get::<String>(RowKind::Entry, good)?.as_deref(),
                Some("fixture-label-2233")
            );
            rows.put(RowKind::EntryValue, good, "", &json!("fixture-secret-4455"))?;
            rows.set_revision(5)
        })
        .unwrap();
    drop(vault);
    assert!(EncryptedVault::open(&path, id, &data_key().unwrap()).is_err());
    let vault = EncryptedVault::open(&path, id, &key).unwrap();
    vault
        .rows(|rows| {
            assert_eq!(rows.revision()?, 5);
            assert_eq!(
                rows.get::<Value>(RowKind::EntryValue, other)?,
                Some(json!({"held": true}))
            );
            assert_eq!(
                rows.get::<String>(RowKind::EntryValue, good)?.as_deref(),
                Some("fixture-secret-4455")
            );
            Ok(())
        })
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    for needle in [
        good,
        other,
        "fixture-label-2233",
        "fixture-secret-3344",
        "fixture-secret-4455",
    ] {
        assert!(!bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes()));
    }
}

#[test]
fn encrypted_singleton_requires_explicit_reset_without_changing_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FILE_NAME);
    let id = uuid::Uuid::new_v4();
    let key = data_key().unwrap();
    let fixture = tempfile::NamedTempFile::new_in(root.path()).unwrap();
    fixture.persist_noclobber(&path).unwrap();
    let db = super::Connection::open(&path).unwrap();
    super::pages::key_database(&db, &key).unwrap();
    db.execute_batch(
        "CREATE TABLE vault(singleton INTEGER PRIMARY KEY, format INTEGER, id TEXT, payload BLOB)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO vault VALUES(1, 1, ?1, x'010203')",
        [id.to_string()],
    )
    .unwrap();
    drop(db);
    super::pages::Header::create(&path, id).unwrap();
    let (header, _) = super::pages::lookup(&path).unwrap().unwrap();
    let database_bytes = std::fs::read(&path).unwrap();
    let header_bytes = std::fs::read(&header).unwrap();
    assert_eq!(super::identity(&path).unwrap(), id);
    assert_eq!(
        EncryptedVault::open(&path, id, &key).err().as_deref(),
        Some("vault_unsupported_format"),
    );
    assert_eq!(std::fs::read(&path).unwrap(), database_bytes);
    assert_eq!(std::fs::read(&header).unwrap(), header_bytes);
    assert!(!root.path().join(super::RESET_INTENT).exists());
}

#[test]
fn authenticated_row_store_rejects_unexpected_schema() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FILE_NAME);
    let id = uuid::Uuid::new_v4();
    let key = data_key().unwrap();
    EncryptedVault::stage_rows(root.path(), id, &key, |_| Ok(()))
        .unwrap()
        .persist_noclobber(&path)
        .unwrap();
    let db = EncryptedVault::open(&path, id, &key).unwrap();
    db.db
        .execute_batch("ALTER TABLE vault_rows ADD COLUMN fixture TEXT")
        .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        EncryptedVault::open(&path, id, &key).err().as_deref(),
        Some("vault_recovery_required"),
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
