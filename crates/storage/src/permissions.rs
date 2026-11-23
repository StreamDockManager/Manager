//! Local Manager-service decisions. Vault identities and values live in the separate keystore.
use crate::Storage;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet};

// The schema admits 128 actions with at most 64 scopes each.
const MAX_DECISIONS: usize = 128 * 64;

const MIGRATION: &str = "CREATE TABLE IF NOT EXISTS plugin_permissions_v1(
    plugin TEXT NOT NULL, installation TEXT NOT NULL, inventory TEXT NOT NULL, generation TEXT NOT NULL,
    action TEXT NOT NULL, scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 2048),
    allowed INTEGER NOT NULL CHECK(allowed IN (0,1)), revision INTEGER NOT NULL CHECK(revision>0),
    ever_allowed INTEGER NOT NULL DEFAULT 1 CHECK(ever_allowed IN (0,1)),
    PRIMARY KEY(plugin,installation,inventory,generation,action,scope)) STRICT;";

#[derive(Clone)]
pub struct PermissionOwner {
    pub plugin: String,
    pub installation: String,
    pub inventory: String,
    pub generation: String,
}

#[derive(Default)]
pub struct Decisions {
    pub revision: u64,
    pub scopes: BTreeMap<(String, String), bool>,
    pub revoked: BTreeSet<(String, String)>,
}

pub(crate) fn installed(db: &Connection) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='plugin_permissions_v1')",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn migrate(db: &Connection) -> Result<(), String> {
    db.execute_batch(MIGRATION).map_err(|e| e.to_string())?;
    if !tracks_prior_grant(db)? {
        // Earlier explicit negative decisions were revocations. Fresh denials are distinguished below.
        db.execute_batch(
            "ALTER TABLE plugin_permissions_v1 ADD COLUMN ever_allowed
             INTEGER NOT NULL DEFAULT 1 CHECK(ever_allowed IN (0,1))",
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn tracks_prior_grant(db: &Connection) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('plugin_permissions_v1')
                       WHERE name='ever_allowed')",
        [],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn discard(db: &Connection) -> Result<(), String> {
    if installed(db)? {
        db.execute_batch("PRAGMA secure_delete=ON; DROP TABLE plugin_permissions_v1; VACUUM;")
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove installation decisions in the same transaction as package removal.
pub fn forget(tx: &rusqlite::Transaction<'_>, owner: &str) -> Result<(), String> {
    if installed(tx)? {
        tx.execute("DELETE FROM plugin_permissions_v1 WHERE plugin=?1", [owner])
            .map_err(|_| "operation_unavailable")?;
    }
    Ok(())
}

fn read(db: &Connection, owner: &PermissionOwner) -> Result<Decisions, String> {
    let generation = crate::persisted_generation(db).map_err(|_| "operation_unavailable")?;
    if generation != owner.generation {
        return Err("stale_revision".into());
    }
    let mut result = Decisions::default();
    if !installed(db)? {
        return Ok(result);
    }
    let prior = if tracks_prior_grant(db)? {
        "ever_allowed"
    } else {
        "1"
    };
    let sql = format!(
        "SELECT action,scope,allowed,revision,{prior} FROM plugin_permissions_v1
         WHERE plugin=?1 AND installation=?2 AND inventory=?3 AND generation=?4 LIMIT 8193"
    );
    let mut statement = db.prepare(&sql).map_err(|_| "operation_unavailable")?;
    let rows = statement
        .query_map(
            params![
                owner.plugin,
                owner.installation,
                owner.inventory,
                owner.generation
            ],
            |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                    r.get::<_, bool>(2)?,
                    r.get(3)?,
                    r.get::<_, bool>(4)?,
                ))
            },
        )
        .map_err(|_| "operation_unavailable")?;
    for row in rows {
        let (selector, allowed, revision, was_allowed) =
            row.map_err(|_| "operation_unavailable")?;
        if !allowed && was_allowed {
            result.revoked.insert(selector.clone());
        }
        result.scopes.insert(selector, allowed);
        result.revision = result.revision.max(revision);
    }
    if result.scopes.len() > MAX_DECISIONS {
        return Err("operation_unavailable".into());
    }
    Ok(result)
}

impl Storage {
    pub fn permission_decisions(&self, owner: PermissionOwner) -> Result<Decisions, String> {
        self.run(move |db| read(db, &owner))
    }

    /// Compare and commit the complete reviewed change on the owning writer.
    pub fn set_permission_decisions(
        &self,
        owner: PermissionOwner,
        expected: u64,
        changes: Vec<(String, String, bool)>,
    ) -> Result<u64, String> {
        self.transaction(move |tx| write_decisions(tx, &owner, expected, changes))
    }
}

pub fn verify_revision(
    tx: &rusqlite::Transaction<'_>,
    owner: &PermissionOwner,
    expected: u64,
) -> Result<(), String> {
    if read(tx, owner)?.revision != expected {
        return Err("stale_revision".into());
    }
    Ok(())
}

pub fn write_decisions(
    tx: &rusqlite::Transaction<'_>,
    owner: &PermissionOwner,
    expected: u64,
    changes: Vec<(String, String, bool)>,
) -> Result<u64, String> {
    if !(1..=MAX_DECISIONS).contains(&changes.len())
        || changes.iter().any(|(action, scope, _)| {
            action.is_empty() || action.len() > 256 || scope.is_empty() || scope.len() > 2048
        })
    {
        return Err("invalid_request".into());
    }
    verify_revision(tx, owner, expected)?;
    migrate(tx)?;
    let revision: u64 = tx
        .query_row(
            "SELECT CAST(value AS INTEGER)+1 FROM database_metadata WHERE key='commit_sequence'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "operation_unavailable")?;
    for (action, scope, allowed) in changes {
        tx.execute(
            "INSERT INTO plugin_permissions_v1
             (plugin,installation,inventory,generation,action,scope,allowed,revision,ever_allowed)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?7)
             ON CONFLICT(plugin,installation,inventory,generation,action,scope) DO UPDATE SET
                 allowed=excluded.allowed, revision=excluded.revision,
                 ever_allowed=MAX(ever_allowed,excluded.allowed)",
            params![
                owner.plugin,
                owner.installation,
                owner.inventory,
                owner.generation,
                action,
                scope,
                allowed,
                revision
            ],
        )
        .map_err(|_| "operation_unavailable")?;
    }
    Ok(revision)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decisions_require_exact_installation_revision_and_never_enter_backups() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("data")).unwrap();
        let owner = PermissionOwner {
            plugin: "sample".into(),
            installation: "install".into(),
            inventory: "digest".into(),
            generation: store.stats().unwrap().storage_generation,
        };
        let change = vec![("demo.action".into(), "clipboard-fixture".into(), true)];
        let revision = store
            .set_permission_decisions(owner.clone(), 0, change.clone())
            .unwrap();
        assert!(store
            .set_permission_decisions(owner.clone(), 0, change.clone())
            .is_err());
        assert_eq!(
            store
                .permission_decisions(owner.clone())
                .unwrap()
                .scopes
                .values()
                .next(),
            Some(&true)
        );
        let path = root.path().join("backup.sqlite3");
        store.backup(path.clone()).unwrap();
        let backup = Connection::open(&path).unwrap();
        assert!(!installed(&backup).unwrap());
        assert!(!std::fs::read(path)
            .unwrap()
            .windows(17)
            .any(|s| s == b"clipboard-fixture"));
        let replacement = PermissionOwner {
            installation: "replacement".into(),
            ..owner.clone()
        };
        assert!(store
            .permission_decisions(replacement)
            .unwrap()
            .scopes
            .is_empty());
        drop(store);
        let store = Storage::open(root.path().join("data")).unwrap();
        assert_eq!(
            store.permission_decisions(owner.clone()).unwrap().revision,
            revision
        );
        store
            .set_permission_decisions(
                owner.clone(),
                revision,
                vec![("demo.action".into(), "clipboard-fixture".into(), false)],
            )
            .unwrap();
        // The preceding ledger recorded revocations before it tracked explicit denials.
        store
            .transaction(|tx| {
                tx.execute_batch("ALTER TABLE plugin_permissions_v1 DROP COLUMN ever_allowed")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let current = store.permission_decisions(owner.clone()).unwrap();
        assert!(current
            .revoked
            .contains(&("demo.action".into(), "clipboard-fixture".into())));
        store
            .set_permission_decisions(
                owner.clone(),
                current.revision,
                vec![("demo.action".into(), "scope-never-granted".into(), false)],
            )
            .unwrap();
        assert!(!store
            .permission_decisions(owner.clone())
            .unwrap()
            .revoked
            .contains(&("demo.action".into(), "scope-never-granted".into())));
        assert_eq!(
            store
                .permission_decisions(owner)
                .unwrap()
                .scopes
                .values()
                .next(),
            Some(&false)
        );
    }
}
