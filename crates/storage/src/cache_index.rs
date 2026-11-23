//! Rebuildable file-cache inventory, separate from configuration and its backups.
//! Cache owners serialize publication and deletion with their own file/lease locks.
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Artwork,
    Transfers,
    UpdateMetadata,
    UpdateDownloads,
    Firmware,
}
impl Kind {
    fn overlapping_leases(self) -> bool {
        matches!(self, Self::UpdateMetadata | Self::UpdateDownloads)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Artwork => "artwork_cache",
            Self::Transfers => "transfer_cache",
            Self::UpdateMetadata => "update_metadata_cache",
            Self::UpdateDownloads => "update_download_cache",
            Self::Firmware => "firmware_catalog_cache",
        }
    }
}

// Coalesce short bursts of overlapping leases; no work remains pending after touch.
const RECENCY_WINDOW_MS: i64 = 100;

struct OpenIndex {
    connection: Connection,
    // One completed touch, never a pending write. An intervening SQL change
    // makes the next touch exact, including a record or explicit timestamp reset.
    last_touch: Option<(Kind, String, u64)>,
}
type Database = Mutex<Option<OpenIndex>>;
// Tables in one data directory share a connection and its page cache.
// Weak handles let temporary or closed Manager instances release both.
static DATABASES: OnceLock<Mutex<HashMap<PathBuf, Weak<Database>>>> = OnceLock::new();
#[derive(Clone)]
pub struct Table {
    db: Arc<Database>,
    path: PathBuf,
    kind: Kind,
}
pub fn now_ms() -> i64 {
    timestamp(SystemTime::now())
}
pub fn timestamp(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub struct Record {
    pub incomplete: bool,
    pub path: String,
    pub bytes: u64,
    pub used: i64,
    pub owned: bool,
    pub active: bool,
    pub hint: bool,
    pub scope: String,
}
impl Record {
    pub fn file(path: String, bytes: u64) -> Self {
        Self {
            incomplete: false,
            path,
            bytes,
            used: now_ms(),
            owned: true,
            active: false,
            hint: false,
            scope: String::new(),
        }
    }
}
#[derive(Default)]
pub struct Stats {
    pub incomplete: bool,
    pub entries: usize,
    pub bytes: u64,
    pub active: usize,
    pub protected: usize,
}
#[derive(Default)]
pub struct Report {
    pub stats: Stats,
    pub removed_entries: usize,
    pub removed_bytes: u64,
    pub errors: Vec<String>,
}
pub enum Removal {
    Removed,
    Missing,
    Busy,
    Protected,
}
pub struct Limits {
    pub bytes: u64,
    pub entries: Option<usize>,
    pub age: Option<Duration>,
}
impl Table {
    pub fn new(db_root: &Path, kind: Kind) -> Self {
        let path = crate::database_path(db_root, "cache.sqlite3");
        let mut handles = DATABASES.get_or_init(Default::default).lock().unwrap();
        let db = handles
            .get(&path)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                handles.retain(|_, db| db.strong_count() > 0);
                let db = Arc::new(Mutex::new(None));
                handles.insert(path.clone(), Arc::downgrade(&db));
                db
            });
        Self { db, path, kind }
    }
    fn connect<T>(
        &self,
        op: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        self.with_index(|index| op(&mut index.connection))
    }
    fn with_index<T>(
        &self,
        op: impl FnOnce(&mut OpenIndex) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let mut db = self.db.lock().map_err(|e| e.to_string())?;
        if db.is_none() {
            let parent = self.path.parent().ok_or("Missing cache index root")?;
            let root = parent.parent().ok_or("Missing application data root")?;
            // A standalone cache has no authority to move an existing database family
            if crate::has_legacy_database_layout(root)? {
                return Err("The application must relocate its existing databases before opening the cache index".into());
            }
            let directory = crate::database_layout::ensure_directory(root)?;
            if directory != parent || parent.canonicalize().map_err(|e| e.to_string())? != parent {
                return Err("Unsafe cache index root".into());
            }
            for extension in ["sqlite3", "sqlite3-wal", "sqlite3-shm", "sqlite3-journal"] {
                let path = self.path.with_extension(extension);
                match path.symlink_metadata() {
                    Ok(m) if !private_regular(&m) => return Err("Unsafe cache index file".into()),
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
                    _ => (),
                }
            }
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.path) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.to_string()),
            }
            let mut c = Connection::open_with_flags(
                &self.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|e| e.to_string())?;
            c.busy_timeout(Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
            let version: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if version > 1 {
                return Err(format!("Unsupported cache.sqlite3 version {version}"));
            }
            c.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-256;",
            )
            .map_err(|e| e.to_string())?;
            if version == 0 {
                let tx = c
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|e| e.to_string())?;
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS cache_imports (cache_type TEXT PRIMARY KEY);",
                )
                .map_err(|e| e.to_string())?;
                for kind in [
                    Kind::Artwork,
                    Kind::Transfers,
                    Kind::UpdateMetadata,
                    Kind::UpdateDownloads,
                    Kind::Firmware,
                ] {
                    let t = kind.name();
                    let schema=format!("CREATE TABLE IF NOT EXISTS {t} (
                            cache_key TEXT PRIMARY KEY,
                            relative_path TEXT NOT NULL UNIQUE,
                            size_bytes INTEGER NOT NULL CHECK(size_bytes>=0),
                            last_used_ms INTEGER NOT NULL,
                            can_remove INTEGER NOT NULL DEFAULT 0,
                            owned INTEGER NOT NULL,
                            active INTEGER NOT NULL,
                            source_scope TEXT NOT NULL,
                            incomplete INTEGER NOT NULL DEFAULT 0
                        );
                        CREATE INDEX IF NOT EXISTS {t}_lru ON {t}(last_used_ms,cache_key);
                        CREATE INDEX IF NOT EXISTS {t}_hints ON {t}(can_remove,last_used_ms) WHERE owned=1;
                        CREATE INDEX IF NOT EXISTS {t}_active ON {t}(cache_key) WHERE active=1;");
                    tx.execute_batch(&schema).map_err(|e| e.to_string())?;
                }
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS transfer_scope ON transfer_cache(source_scope);
                                  PRAGMA user_version=1;",
                )
                .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
            }
            *db = Some(OpenIndex {
                connection: c,
                last_touch: None,
            });
        }
        op(db.as_mut().unwrap()).map_err(|e| format!("Cache index: {e}"))
    }
    fn insert(&self, c: &Connection, r: &Record, import: bool) -> rusqlite::Result<()> {
        if !safe_relative(&r.path) {
            return Err(rusqlite::Error::InvalidPath(PathBuf::from(&r.path)));
        }
        let conflict = if import {
            "DO NOTHING"
        } else {
            "DO UPDATE SET size_bytes=excluded.size_bytes, last_used_ms=excluded.last_used_ms,
                can_remove=excluded.can_remove, owned=excluded.owned, active=excluded.active,
                source_scope=excluded.source_scope, incomplete=excluded.incomplete"
        };
        let sql = format!(
            "INSERT INTO {} (cache_key, relative_path, size_bytes, last_used_ms,
                  can_remove, owned, active, source_scope, incomplete)
                VALUES(?1,?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(cache_key) {conflict}",
            self.kind.name()
        );
        c.execute(
            &sql,
            params![
                r.path,
                r.bytes,
                r.used,
                r.hint,
                r.owned,
                r.active,
                r.scope,
                r.incomplete
            ],
        )?;
        Ok(())
    }
    pub fn import_once(
        &self,
        load: impl FnOnce() -> Result<Vec<Record>, String>,
    ) -> Result<(), String> {
        let name = self.kind.name();
        if self.connect(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM cache_imports WHERE cache_type=?1)",
                [name],
                |r| r.get::<_, bool>(0),
            )
        })? {
            return Ok(());
        }
        let records = load()?;
        self.connect(|c| {
            let tx = c.transaction()?;
            for record in records {
                self.insert(&tx, &record, true)?;
            }
            tx.execute("INSERT OR IGNORE INTO cache_imports VALUES(?1)", [name])?;
            tx.commit()
        })
    }
    /// A terminated process may have left active flags behind. The owner holds its
    /// acquisition lock while revalidating only the rows that claim a live lease.
    pub fn recover_activity(
        &self,
        mut live: impl FnMut(&str) -> Result<bool, String>,
    ) -> Result<(), String> {
        let keys = self.connect(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT cache_key FROM {} WHERE active=1",
                self.kind.name()
            ))?;
            let keys = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(keys)
        })?;
        for key in keys {
            if !live(&key)? {
                // A crashed owner never published the final, exact release time.
                // Allow for a skipped active touch before making the row age-eligible.
                self.connect(|c| {
                    c.execute(
                        &format!(
                            "UPDATE {} SET active=0,
                                last_used_ms=max(last_used_ms,min(?2,last_used_ms+?3))
                             WHERE cache_key=?1 AND active=1",
                            self.kind.name()
                        ),
                        params![
                            key,
                            now_ms(),
                            if self.kind.overlapping_leases() {
                                RECENCY_WINDOW_MS
                            } else {
                                0
                            }
                        ],
                    )
                })?;
            }
        }
        Ok(())
    }
    pub fn record(&self, r: &Record) -> Result<(), String> {
        self.connect(|c| self.insert(c, r, false))
    }
    pub fn touch(&self, path: &str, active: bool) -> Result<bool, String> {
        self.touch_at(path, active, now_ms())
    }

    fn touch_at(&self, path: &str, active: bool, now: i64) -> Result<bool, String> {
        // Age-limited caches publish exact inactive timestamps (including the
        // final lease release). Firmware has no strict-age retention policy.
        self.with_index(|index| {
            let c = &index.connection;
            let name = self.kind.name();
            let repeat = index
                .last_touch
                .as_ref()
                .is_some_and(|(kind, key, changes)| {
                    *kind == self.kind && key == path && *changes == c.total_changes()
                });
            let eligible =
                (active && self.kind.overlapping_leases()) || matches!(self.kind, Kind::Firmware);
            let coalesce = repeat && eligible;
            let sql = format!(
                "UPDATE {name} SET last_used_ms=?2, active=?3 WHERE cache_key=?1
                AND (NOT ?4 OR active!=?3 OR last_used_ms>?2 OR last_used_ms<=?5)"
            );
            let changed = c.execute(
                &sql,
                params![
                    path,
                    now,
                    active,
                    coalesce,
                    now.saturating_sub(RECENCY_WINDOW_MS)
                ],
            )?;
            if changed != 0 {
                // Long keys stay on the exact path. Keep only one bounded hint
                // per connection for the overlapping same-entry lease workload.
                index.last_touch = (eligible && path.len() <= 4096)
                    .then(|| (self.kind, path.to_owned(), c.total_changes()));
                return Ok(true);
            }
            if !coalesce {
                return Ok(false);
            }
            c.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {name} WHERE cache_key=?1)"),
                [path],
                |r| r.get(0),
            )
        })
    }

    pub fn set_used(&self, path: &str, used: i64) -> Result<(), String> {
        self.connect(|c| {
            c.execute(
                &format!(
                    "UPDATE {} SET last_used_ms=?2 WHERE cache_key=?1",
                    self.kind.name()
                ),
                params![path, used],
            )?;
            Ok(())
        })
    }
    pub fn hint(&self, path: &str) -> Result<(), String> {
        self.connect(|c| {
            c.execute(
                &format!(
                    "UPDATE {} SET can_remove=1 WHERE cache_key=?1 AND owned=1",
                    self.kind.name()
                ),
                [path],
            )
            .map(|_| ())
        })
    }
    pub fn hinted(&self, path: &str) -> Result<bool, String> {
        self.connect(|c| {
            Ok(c.query_row(
                &format!(
                    "SELECT can_remove FROM {} WHERE cache_key=?1",
                    self.kind.name()
                ),
                [path],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
        })
    }
    pub fn scope_keys(&self, scope: &str) -> Result<Vec<String>, String> {
        self.connect(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT cache_key FROM {} WHERE source_scope=?1 AND owned=1",
                self.kind.name()
            ))?;
            let rows = stmt.query_map([scope], |r| r.get(0))?;
            rows.collect()
        })
    }
    pub fn forget(&self, path: &str) -> Result<(), String> {
        self.connect(|c| {
            c.execute(
                &format!("DELETE FROM {} WHERE cache_key=?1", self.kind.name()),
                [path],
            )
            .map(|_| ())
        })
    }
    pub fn stats(&self) -> Result<Stats, String> {
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes),0),
                    COUNT(*) FILTER (WHERE owned=1 AND active=1),
                    COUNT(*) FILTER (WHERE owned=0), COALESCE(MAX(incomplete),0)
                FROM {}",
            self.kind.name()
        );
        self.connect(|c| {
            c.query_row(&sql, [], |row| {
                Ok(Stats {
                    entries: row.get(0)?,
                    bytes: row.get(1)?,
                    active: row.get(2)?,
                    protected: row.get(3)?,
                    incomplete: row.get(4)?,
                })
            })
        })
    }
    pub fn purge(
        &self,
        limits: Limits,
        clear: bool,
        remove: impl FnMut(&str) -> Result<Removal, String>,
    ) -> Result<Report, String> {
        self.purge_at(limits, clear, now_ms(), remove)
    }

    fn purge_at(
        &self,
        limits: Limits,
        clear: bool,
        now: i64,
        mut remove: impl FnMut(&str) -> Result<Removal, String>,
    ) -> Result<Report, String> {
        let mut usage = self.stats()?;
        let over =
            |s: &Stats| s.bytes > limits.bytes || limits.entries.is_some_and(|n| s.entries > n);
        let allowance = if matches!(self.kind, Kind::Firmware) {
            RECENCY_WINDOW_MS
        } else {
            0
        };
        let cutoff = limits
            .age
            .map(|d| {
                now.saturating_sub(d.as_millis().min(i64::MAX as u128) as i64)
                    .saturating_sub(allowance)
            })
            .unwrap_or(i64::MIN);
        let rows = self.connect(|c| {
            let filter = if clear || over(&usage) {
                "1"
            } else {
                "can_remove=1 OR last_used_ms<=:cutoff"
            };
            let sql = format!(
                "SELECT relative_path,size_bytes,can_remove,last_used_ms
                FROM {} WHERE owned=1 AND active=0 AND ({filter})
                ORDER BY can_remove DESC,last_used_ms,cache_key",
                self.kind.name()
            );
            let mut stmt = c.prepare(&sql)?;
            if let Some(index) = stmt.parameter_index(":cutoff")? {
                stmt.raw_bind_parameter(index, cutoff)?;
            }
            let rows = stmt
                .raw_query()
                .mapped(|row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let mut report = Report::default();
        for (path, bytes, hinted, used) in rows {
            if !clear && !hinted && used > cutoff && !over(&usage) {
                break;
            }
            if !safe_relative(&path) {
                return Err("Invalid relative path in cache index".into());
            }
            // Owners hold their acquisition/registry lock over purge; the callback
            // checks the file lease and its ownership before unlinking. Recheck
            // the indexed candidate too, since a release may have refreshed it.
            let current = self.connect(|c| {
                let sql = format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE cache_key=?1
                     AND owned=1 AND active=0 AND size_bytes=?2 AND can_remove=?3 AND last_used_ms=?4)",
                    self.kind.name()
                );
                c.query_row(&sql, params![path, bytes, hinted, used], |r| r.get::<_, bool>(0))
            })?;
            if !current {
                continue;
            }
            match remove(&path) {
                Ok(result @ (Removal::Removed | Removal::Missing)) => {
                    self.forget(&path)?;
                    usage.entries -= 1;
                    usage.bytes = usage.bytes.saturating_sub(bytes);
                    if matches!(result, Removal::Removed) {
                        report.removed_entries += 1;
                        report.removed_bytes += bytes;
                    }
                }
                Ok(Removal::Protected) => {
                    self.connect(|c| {
                        c.execute(
                            &format!(
                                "UPDATE {} SET owned=0,active=0 WHERE relative_path=?1",
                                self.kind.name()
                            ),
                            [&path],
                        )
                    })?;
                }
                Ok(Removal::Busy) => (),
                Err(e) => {
                    self.hint(&path)?;
                    report.errors.push(e);
                }
            }
        }
        report.stats = self.stats()?;
        Ok(report)
    }
}
fn private_regular(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}
pub fn safe_relative(s: &str) -> bool {
    !s.is_empty()
        && !s.contains(['\\', '\0', ':'])
        && s.split('/').all(|v| !matches!(v, "" | "." | ".."))
        && Path::new(s)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits(age_ms: u64) -> Limits {
        Limits {
            bytes: u64::MAX,
            entries: None,
            age: Some(Duration::from_millis(age_ms)),
        }
    }

    #[test]
    fn touch_burst_keeps_transitions_exact_and_never_recreates_a_row() {
        let directory = tempfile::tempdir().unwrap();
        let table = Table::new(
            &directory.path().canonicalize().unwrap(),
            Kind::UpdateMetadata,
        );
        let state = || {
            table
                .connect(|c| {
                    c.query_row(
            "SELECT last_used_ms,active FROM update_metadata_cache WHERE cache_key='one'",
            [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)),
        )
                })
                .unwrap()
        };
        let changes = || table.connect(|c| Ok(c.total_changes())).unwrap();
        table.record(&Record::file("one".into(), 7)).unwrap();
        let baseline = changes();
        for t in 1000..1100 {
            assert!(table.touch_at("one", true, t).unwrap());
        }
        assert_eq!(changes() - baseline, 1);
        assert_eq!(state(), (1000, true));
        assert!(table.touch_at("one", true, 1100).unwrap());
        assert_eq!(state(), (1100, true));
        assert!(table.touch_at("one", true, 1090).unwrap());
        assert_eq!(state(), (1090, true));
        assert!(table.touch_at("one", false, 1091).unwrap());
        assert_eq!(state(), (1091, false));
        table.set_used("one", 10).unwrap();
        assert_eq!(state(), (10, false));
        // A skipped active touch before a crash cannot expire early.
        table.touch_at("one", true, 2000).unwrap();
        table.touch_at("one", true, 2099).unwrap();
        table.recover_activity(|_| Ok(false)).unwrap();
        assert_eq!(state(), (2100, false));
        assert_eq!(
            table
                .purge_at(limits(1000), false, 3099, |_| panic!())
                .unwrap()
                .stats
                .entries,
            1
        );
        table.forget("one").unwrap();
        assert!(!table.touch_at("one", false, 3100).unwrap());
        assert_eq!(table.stats().unwrap().entries, 0);
    }

    #[test]
    fn age_expiry_allows_for_skipped_inactive_touches_and_clear_is_immediate() {
        let directory = tempfile::tempdir().unwrap();
        let table = Table::new(&directory.path().canonicalize().unwrap(), Kind::Firmware);
        for key in ["old", "busy", "protected", "active"] {
            let mut record = Record::file(key.into(), 10);
            record.used = 1;
            record.owned = key != "protected";
            record.active = key == "active";
            table.record(&record).unwrap();
        }
        table.set_used("old", 999).unwrap();
        let before = table.connect(|c| Ok(c.total_changes())).unwrap();
        assert!(table.touch_at("old", false, 1000).unwrap());
        assert_eq!(
            table
                .connect(|c| c.query_row(
                    "SELECT last_used_ms FROM firmware_catalog_cache WHERE cache_key='old'",
                    [],
                    |row| row.get::<_, i64>(0),
                ))
                .unwrap(),
            1000
        );
        for t in 1001..1100 {
            assert!(table.touch_at("old", false, t).unwrap());
        }
        assert_eq!(
            table.connect(|c| Ok(c.total_changes())).unwrap() - before,
            1
        );
        let remove = |key: &str| match key {
            "old" => Ok(Removal::Removed),
            "busy" => Ok(Removal::Busy),
            _ => panic!("protected candidate"),
        };
        let first = table.purge_at(limits(500), false, 1599, remove).unwrap();
        assert_eq!(first.removed_entries, 0);
        let expired = table.purge_at(limits(500), false, 1600, remove).unwrap();
        assert_eq!(expired.removed_entries, 1);
        table.record(&Record::file("old".into(), 10)).unwrap();
        let cleared = table.purge_at(limits(500), true, 1601, remove).unwrap();
        assert_eq!(cleared.removed_entries, 1);
        assert_eq!(
            (
                cleared.stats.entries,
                cleared.stats.active,
                cleared.stats.protected
            ),
            (3, 1, 1)
        );
    }

    #[test]
    fn purge_rechecks_refreshed_and_acquired_candidates_before_removal() {
        let directory = tempfile::tempdir().unwrap();
        let table = Table::new(&directory.path().canonicalize().unwrap(), Kind::Artwork);
        for key in ["first", "refreshed", "leased"] {
            let mut r = Record::file(key.into(), 5);
            r.used = 10;
            table.record(&r).unwrap();
        }
        let result = table
            .purge_at(limits(500), false, 600, |key| {
                assert_eq!(key, "first");
                table.touch_at("refreshed", false, 600).unwrap();
                table.touch_at("leased", true, 600).unwrap();
                Ok(Removal::Removed)
            })
            .unwrap();
        assert_eq!(
            (
                result.removed_entries,
                result.stats.active,
                result.stats.entries
            ),
            (1, 1, 2)
        );
    }

    #[test]
    #[ignore = "disposable SQL write-count benchmark"]
    fn overlapping_lease_touch_burst() {
        let directory = tempfile::tempdir().unwrap();
        let table = Table::new(
            &directory.path().canonicalize().unwrap(),
            Kind::UpdateMetadata,
        );
        table.record(&Record::file("metadata".into(), 8)).unwrap();
        let changes = || table.connect(|c| Ok(c.total_changes())).unwrap();
        let before = changes();
        for _ in 0..100 {
            assert!(table.touch("metadata", true).unwrap());
        }
        assert!(table.touch("metadata", false).unwrap());
        println!("101 touches, {} SQL rows changed", changes() - before);
    }
    #[test]
    fn reopened_index_preserves_hints_lru_and_imports_each_namespace_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let table = Table::new(&root, Kind::Artwork);
        for (key, used) in [("old", 10), ("middle", 20), ("recent", 30), ("discard", 40)] {
            let mut entry = Record::file(key.into(), 10);
            entry.used = used;
            table.record(&entry).unwrap();
        }
        table.hint("discard").unwrap();
        table
            .import_once(|| Ok(vec![Record::file("discard".into(), 99)]))
            .unwrap();
        let metadata = Table::new(&root, Kind::UpdateMetadata);
        metadata.record(&Record::file("old".into(), 17)).unwrap();
        drop(metadata);
        drop(table);
        let reopened = Table::new(&root, Kind::Artwork);
        reopened
            .import_once(|| panic!("re-enumerated legacy files"))
            .unwrap();
        let mut candidates = Vec::new();
        let report = reopened
            .purge(
                Limits {
                    bytes: 20,
                    entries: None,
                    age: None,
                },
                false,
                |key| {
                    candidates.push(key.to_owned());
                    if key == "old" {
                        Ok(Removal::Busy)
                    } else {
                        Ok(Removal::Removed)
                    }
                },
            )
            .unwrap();
        assert_eq!(candidates, ["discard", "old", "middle"]);
        assert_eq!((report.removed_entries, report.stats.bytes), (2, 20));
        assert_eq!(
            Table::new(&root, Kind::UpdateMetadata)
                .stats()
                .unwrap()
                .bytes,
            17
        );
        assert!(reopened
            .record(&Record::file("../outside".into(), 0))
            .is_err());
    }
    #[test]
    fn failed_removals_are_retried_without_dropping_the_deletion_hint() {
        let dir = tempfile::tempdir().unwrap();
        let table = Table::new(&dir.path().canonicalize().unwrap(), Kind::Transfers);
        table.record(&Record::file("partial".into(), 60)).unwrap();
        table.hint("partial").unwrap();
        let limits = || Limits {
            bytes: 1000,
            entries: None,
            age: None,
        };
        let failed = table
            .purge(limits(), false, |_| Err("busy filesystem".into()))
            .unwrap();
        assert_eq!(failed.errors.len(), 1);
        assert_eq!(failed.stats.bytes, 60);
        assert!(table.hinted("partial").unwrap());
        let retry = table
            .purge(limits(), false, |_| Ok(Removal::Missing))
            .unwrap();
        assert_eq!((retry.stats.bytes, retry.removed_entries), (0, 0));
    }
    #[test]
    fn standalone_cache_refuses_to_create_a_second_index_before_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let legacy = root.join("cache.sqlite3");
        let conn = Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE old_data(x);").unwrap();
        drop(conn);
        let before = std::fs::read(&legacy).unwrap();
        assert!(Table::new(&root, Kind::Artwork)
            .stats()
            .err()
            .unwrap()
            .contains("relocate"));
        assert!(!crate::database_path(&root, "cache.sqlite3").exists());
        assert_eq!(std::fs::read(&legacy).unwrap(), before);
    }

    #[test]
    fn relocation_preserves_committed_uncheckpointed_cache_wal() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let source_root = source.path().canonicalize().unwrap();
        let target_root = target.path().canonicalize().unwrap();
        let table = Table::new(&source_root, Kind::Artwork);
        table
            .record(&Record::file("checkpointed".into(), 5))
            .unwrap();
        drop(table);

        let path = crate::database_path(&source_root, "cache.sqlite3");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        writer
            .execute(
                "INSERT INTO artwork_cache VALUES('pending','pending',13,1,0,1,0,'',0)",
                [],
            )
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(path.with_extension("sqlite3-wal").metadata().unwrap().len() > 32);

        // Copy the committed file family while the fixture connection stays idle
        for name in ["cache.sqlite3", "cache.sqlite3-wal", "cache.sqlite3-shm"] {
            std::fs::copy(path.with_file_name(name), target_root.join(name)).unwrap();
        }
        drop(writer);
        let table = Table::new(&target_root, Kind::Artwork);
        assert!(table.stats().is_err());
        crate::prepare_database_layout(&target_root).unwrap();
        for name in ["cache.sqlite3", "cache.sqlite3-wal", "cache.sqlite3-shm"] {
            assert!(!target_root.join(name).exists());
            assert!(crate::database_path(&target_root, name).exists());
        }
        let recovered = table.stats().unwrap();
        assert_eq!((recovered.entries, recovered.bytes), (2, 18));
    }

    #[test]
    fn future_database_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = crate::database_layout::ensure_directory(&root)
            .unwrap()
            .join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version=99; CREATE TABLE future_data(x);")
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(Table::new(&root, Kind::Artwork)
            .stats()
            .err()
            .unwrap()
            .contains("version 99"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
