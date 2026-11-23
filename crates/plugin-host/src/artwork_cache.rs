//! Disk-backed plugin artwork. The Manager configuration writer owns this directory.
//! Each active visual or render snapshot leases the exact bytes it was given.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use stream_dock_manager_storage::cache_index::{self, Kind, Limits, Record, Removal, Table};

pub const MAX_BYTES: u64 = 2_000_000_000;
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const MIN_BYTES: usize = 64 * 1024;
const MAX_URI: usize = 24 * 1024 * 1024;

struct Artwork {
    file: Mutex<File>,
    bytes: usize,
    base64_header: Option<String>,
    // The private token is meaningful only in snapshots which own this file.
    token: String,
    digest: [u8; 32],
    payload_digest: [u8; 32],
    table: Table,
    index: Weak<Mutex<Index>>,
}
impl Artwork {
    fn read_bytes(&self) -> Result<Vec<u8>, String> {
        let mut file = self.file.lock().unwrap();
        file.rewind().map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.bytes);
        (&mut *file)
            .take(self.bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() != self.bytes || Sha256::digest(&bytes)[..] != self.payload_digest {
            return Err("Cached artwork changed during use".into());
        }
        Ok(bytes)
    }

    fn read(&self) -> Result<String, String> {
        let bytes = self.read_bytes()?;
        if let Some(header) = &self.base64_header {
            let mut uri = String::with_capacity(header.len() + 1 + bytes.len().div_ceil(3) * 4);
            uri.push_str(header);
            uri.push(',');
            STANDARD.encode_string(bytes, &mut uri);
            Ok(uri)
        } else {
            String::from_utf8(bytes).map_err(|e| e.to_string())
        }
    }
}
impl Drop for Artwork {
    fn drop(&mut self) {
        // Count time displayed, including periods with only warm frame-cache hits.
        if let Some(index) = self.index.upgrade() {
            let mut index = index.lock().unwrap();
            if index
                .active
                .get(&self.digest)
                .is_some_and(|w| w.strong_count() == 0)
            {
                index.active.remove(&self.digest);
                let _ = self
                    .table
                    .touch(&format!("{}.art", hex::encode(self.digest)), false);
            }
        }
    }
}

#[derive(Default)]
pub struct ArtworkReport {
    pub bytes: u64,
    pub entries: usize,
    pub protected_entries: usize,
    pub removed_entries: usize,
    pub removed_bytes: u64,
    pub errors: Vec<String>,
}
#[derive(Default)]
struct Index {
    active: HashMap<[u8; 32], Weak<Artwork>>,
}
pub struct ArtworkCache {
    directory: PathBuf,
    budget: u64,
    namespace: uuid::Uuid,
    index: Arc<Mutex<Index>>,
    table: Table,
}
impl ArtworkCache {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            table: Table::new(
                directory.parent().expect("artwork cache parent"),
                Kind::Artwork,
            ),
            directory,
            budget: MAX_BYTES,
            namespace: uuid::Uuid::new_v4(),
            index: Default::default(),
        }
    }

    fn directory(&self) -> Result<(), String> {
        let parent = self
            .directory
            .parent()
            .ok_or("Missing cached-media parent")?;
        if parent.canonicalize().map_err(|e| e.to_string())? != parent {
            return Err("Unsafe cached-media parent".into());
        }
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        if let Err(e) = builder.create(&self.directory) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.to_string());
            }
        }
        let meta = self
            .directory
            .symlink_metadata()
            .map_err(|e| e.to_string())?;
        if !meta.is_dir() || linked(&meta) {
            return Err("Unsafe cached-media directory".into());
        }
        Ok(())
    }

    /// None inspects, false prunes by age and LRU, true clears idle entries.
    pub fn maintain(&self, clear: Option<bool>) -> Result<ArtworkReport, String> {
        let index = self.index.lock().unwrap();
        self.directory()?;
        self.table.import_once(|| {
            let mut records = Vec::new();
            for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !cache_index::safe_relative(name) {
                    continue;
                }
                let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
                let mut r = Record::file(name.into(), meta.len());
                r.owned = meta.is_file()
                    && !linked(&meta)
                    && (digest_name(name).is_some() || pending(name));
                r.used = cache_index::timestamp(meta.modified().map_err(|e| e.to_string())?);
                r.hint = pending(name);
                records.push(r);
            }
            Ok(records)
        })?;
        self.table.recover_activity(|name| {
            Ok(digest_name(name).is_some_and(|digest| index.active.contains_key(&digest)))
        })?;
        let report = if let Some(clear) = clear {
            self.table.purge(
                Limits {
                    bytes: self.budget,
                    entries: None,
                    age: Some(MAX_AGE),
                },
                clear,
                |name| {
                    if digest_name(name)
                        .is_some_and(|d| index.active.get(&d).is_some_and(|w| w.strong_count() > 0))
                    {
                        return Ok(Removal::Busy);
                    }
                    if digest_name(name).is_none() && !pending(name) {
                        return Ok(Removal::Protected);
                    }
                    let path = self.directory.join(name);
                    match path.symlink_metadata() {
                        Ok(m) if m.is_file() && !linked(&m) => fs::remove_file(&path)
                            .map(|_| Removal::Removed)
                            .map_err(|e| e.to_string()),
                        Ok(_) => Ok(Removal::Protected),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal::Missing),
                        Err(e) => Err(e.to_string()),
                    }
                },
            )?
        } else {
            cache_index::Report {
                stats: self.table.stats()?,
                ..Default::default()
            }
        };
        Ok(ArtworkReport {
            bytes: report.stats.bytes,
            entries: report.stats.entries,
            protected_entries: report.stats.protected + report.stats.active,
            removed_entries: report.removed_entries,
            removed_bytes: report.removed_bytes,
            errors: report.errors,
        })
    }

    fn lease(&self, uri: &str, header: &str) -> Result<Arc<Artwork>, String> {
        let digest: [u8; 32] = Sha256::digest(uri.as_bytes()).into();
        let mut index = self.index.lock().unwrap();
        if let Some(live) = index.active.get(&digest).and_then(Weak::upgrade) {
            return Ok(live);
        }
        let base64_header = header.ends_with(";base64").then_some(header);
        let payload = if base64_header.is_some() {
            Cow::Owned(
                STANDARD
                    .decode(&uri.as_bytes()[header.len() + 1..])
                    .map_err(|e| e.to_string())?,
            )
        } else {
            Cow::Borrowed(uri.as_bytes())
        };
        self.directory()?;
        let name = format!("{}.art", hex::encode(digest));
        let path = self.directory.join(&name);
        let mut existing = None;
        match path.symlink_metadata() {
            Ok(m) if m.is_file() && !linked(&m) => {
                let mut options = OpenOptions::new();
                options.read(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
                }
                let mut file = options.open(&path).map_err(|e| e.to_string())?;
                let m = file.metadata().map_err(|e| e.to_string())?;
                if !m.is_file() || linked(&m) {
                    return Err("Unsafe cached artwork".into());
                }
                if m.len() == payload.len() as u64 {
                    // The inbound bytes are already available. Verify the previous file without
                    // allocating a second copy, including when recovering a truncated write.
                    let mut buf = [0u8; 8192];
                    let identical = payload.chunks(buf.len()).all(|chunk| {
                        file.read_exact(&mut buf[..chunk.len()]).is_ok()
                            && &buf[..chunk.len()] == chunk
                    });
                    if identical {
                        existing = Some(file);
                    }
                }
            }
            Ok(_) => return Err("Unsafe cached artwork".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        let mut record = Record::file(name, payload.len() as u64);
        record.active = true;
        // Register the final path before publication so interrupted writes can be reconciled.
        self.table.record(&record)?;
        let file = if let Some(file) = existing {
            file
        } else {
            let mut tmp = tempfile::Builder::new()
                .prefix("artwork-pending-")
                .rand_bytes(12)
                .suffix(".tmp")
                .tempfile_in(&self.directory)
                .map_err(|e| e.to_string())?;
            let mut pending = Record::file(
                tmp.path().file_name().unwrap().to_string_lossy().into(),
                payload.len() as u64,
            );
            pending.hint = true;
            self.table.record(&pending)?;
            tmp.write_all(&payload).map_err(|e| e.to_string())?;
            let file = tmp.persist(&path).map_err(|e| e.to_string())?;
            self.table.forget(&pending.path)?;
            file
        };
        // Keep the MIME prefix so GIF detection still works before opening artwork.
        let art = Arc::new(Artwork {
            file: Mutex::new(file),
            bytes: payload.len(),
            base64_header: base64_header.map(str::to_owned),
            token: format!("{header},@{}/{}", self.namespace, hex::encode(digest)),
            digest,
            payload_digest: Sha256::digest(&payload).into(),
            table: self.table.clone(),
            index: Arc::downgrade(&self.index),
        });
        if index.active.len() >= 256 {
            index.active.retain(|_, w| w.strong_count() != 0);
        }
        index.active.insert(digest, Arc::downgrade(&art));
        Ok(art)
    }
}
fn digest_name(name: &str) -> Option<[u8; 32]> {
    let stem = name.strip_suffix(".art")?;
    if stem.len() != 64
        || !stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    hex::decode(stem).ok()?.try_into().ok()
}
fn pending(name: &str) -> bool {
    name.strip_prefix("artwork-pending-")
        .and_then(|s| s.strip_suffix(".tmp"))
        .is_some_and(|s| s.len() == 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}
fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.file_type().is_symlink() || (metadata.is_file() && metadata.nlink() != 1)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(any(unix, windows)))]
    {
        metadata.file_type().is_symlink()
    }
}

#[derive(Clone, Default)]
pub struct ArtworkFiles(HashMap<String, Arc<Artwork>>);
impl ArtworkFiles {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Canonical inline PNG/JPEG pixels already have their final MIME and bytes.
    /// Acquire them on ingress before the package/theme loader decodes and re-encodes.
    pub(super) fn inbound_raster(&mut self, cache: &ArtworkCache, source: &str) -> Option<Value> {
        let (header, data) = source.split_once(',')?;
        if source.len() < MIN_BYTES || data.len() > (16 * 1024 * 1024 / 3) * 4 {
            return None;
        }
        if !((header == "data:image/png;base64" && data.starts_with("iVBORw0KGgo"))
            || (header == "data:image/jpeg;base64" && data.starts_with("/9j/")))
        {
            return None;
        }
        let art = cache.lease(source, header).ok()?;
        let token = art.token.clone();
        self.0.insert(token.clone(), art);
        Some(Value::String(token))
    }
    /// Resolve only snapshot-owned binary artwork tokens; ordinary data URIs go to the renderer.
    pub fn image_bytes(&self, token: &str) -> Result<Option<Vec<u8>>, String> {
        self.0
            .get(token)
            .filter(|art| art.base64_header.is_some())
            .map(|art| art.read_bytes())
            .transpose()
    }

    pub fn stored_digest(&self, token: &str) -> Option<[u8; 32]> {
        self.0.get(token).map(|art| art.digest)
    }
    pub fn source<'a>(&self, uri: &'a str) -> Result<Cow<'a, str>, String> {
        match self.0.get(uri) {
            Some(art) => Ok(Cow::Owned(art.read()?)),
            None => Ok(Cow::Borrowed(uri)),
        }
    }
    pub fn expand(&self, value: &mut Value) -> Result<(), String> {
        if self.is_empty() {
            return Ok(());
        }
        visit(value, &mut |s| {
            if s.len() < 256 {
                if let Some(art) = self.0.get(s.as_str()) {
                    *s = art.read()?;
                }
            }
            Ok(())
        })
    }
    pub(super) fn compact(&mut self, cache: &ArtworkCache, visual: &mut Value) {
        let mut next = Self::default();
        let mut field = |value: &mut Value, image_field: bool| {
            visit(value, &mut |s| {
                if s.len() < 256 {
                    if let Some(art) = self.0.get(s.as_str()) {
                        next.0.insert(s.clone(), art.clone());
                    }
                } else if (MIN_BYTES..=MAX_URI).contains(&s.len())
                    && (s.starts_with("data:image/") || image_field && s.starts_with("data:"))
                {
                    if let Some((header, _)) = s.split_once(',').filter(|(h, _)| h.len() <= 128) {
                        // Keep the source inline if cache storage is unavailable.
                        if let Ok(art) = cache.lease(s, header) {
                            *s = art.token.clone();
                            next.0.insert(s.clone(), art);
                        }
                    }
                }
                Ok(())
            })
            .expect("compaction preserves inline sources on cache errors");
        };
        for key in [
            "image",
            "imageReference",
            "encoderBackground",
            "feedbackReferences",
        ] {
            if let Some(image) = visual.get_mut(key) {
                field(image, true);
            }
        }
        if let Some(feedback) = visual.get_mut("feedback") {
            field(feedback, false);
        }
        if let Some(states) = visual
            .get_mut("stateOverrides")
            .and_then(Value::as_object_mut)
        {
            for state in states.values_mut() {
                if let Some(image) = state.get_mut("image") {
                    field(image, true);
                }
            }
        }
        if let Some(items) = visual
            .get_mut("layout")
            .and_then(|v| v.get_mut("items"))
            .and_then(Value::as_array_mut)
        {
            for item in items.iter_mut().filter(|v| v["type"] == "pixmap") {
                if let Some(image) = item.get_mut("value") {
                    field(image, true);
                }
            }
        }
        *self = next;
    }
}
fn visit(
    value: &mut Value,
    f: &mut impl FnMut(&mut String) -> Result<(), String>,
) -> Result<(), String> {
    match value {
        Value::String(s) => f(s)?,
        Value::Object(map) => {
            for v in map.values_mut() {
                visit(v, f)?;
            }
        }
        Value::Array(seq) => {
            for v in seq {
                visit(v, f)?;
            }
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uri(fill: char) -> String {
        format!(
            "data:image/png;base64,{}",
            fill.to_string().repeat(MIN_BYTES)
        )
    }
    fn path(cache: &ArtworkCache, uri: &str) -> PathBuf {
        cache.directory.join(format!(
            "{}.art",
            hex::encode(Sha256::digest(uri.as_bytes()))
        ))
    }
    fn add(cache: &ArtworkCache, uri: &str) -> (Value, ArtworkFiles) {
        let mut visual =
            json!({"image":uri,"imageReference":uri,"stateOverrides":{"0":{"image":uri}}});
        let mut files = ArtworkFiles::default();
        files.compact(cache, &mut visual);
        (visual, files)
    }
    #[test]
    fn artwork_lru_admission_expiry_and_active_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let (a, b, c) = (uri('a'), uri('b'), uri('c'));
        let mut cache = ArtworkCache::new(temp.path().canonicalize().unwrap().join("cached-media"));
        let payload_size = MIN_BYTES * 3 / 4;
        cache.budget = (payload_size * 2) as u64;
        let (first, files) = add(&cache, &a);
        let token = first["image"].as_str().unwrap();
        assert_eq!(first["imageReference"], token);
        assert_eq!(first["stateOverrides"]["0"]["image"], token);
        assert!(first.to_string().len() < 600);
        assert_eq!(files.0.len(), 1);
        let (_, more) = add(&cache, &b);
        assert_eq!(cache.maintain(Some(true)).unwrap().protected_entries, 2);
        let (_, full) = add(&cache, &c);
        assert!(!full.is_empty());
        assert!(cache.maintain(None).unwrap().bytes > cache.budget);
        drop(full);
        drop(files);
        drop(more);
        // Access A again. B is now the oldest candidate, within the seven-day window.
        let age = |uri: &str, ago: Duration| {
            let mut r = Record::file(
                path(&cache, uri)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                payload_size as u64,
            );
            r.used = cache_index::now_ms() - ago.as_millis() as i64;
            cache.table.record(&r).unwrap();
        };
        age(&b, Duration::from_secs(3600));
        let (again, active) = add(&cache, &a);
        assert_eq!(again["image"], token);
        let (_, new) = add(&cache, &c);
        assert!(!new.is_empty());
        assert_eq!(cache.maintain(Some(false)).unwrap().removed_entries, 1);
        assert!(!path(&cache, &b).exists());
        assert_eq!(cache.maintain(None).unwrap().bytes, cache.budget);
        assert_eq!(active.source(token).unwrap(), a);
        drop(active);
        drop(new);
        age(&a, MAX_AGE + Duration::from_secs(10));
        let report = cache.maintain(Some(false)).unwrap();
        assert_eq!(
            (report.removed_entries, report.removed_bytes),
            (1, payload_size as u64)
        );
        assert!(path(&cache, &c).is_file());
    }

    #[test]
    fn reusable_artwork_survives_restart_and_corrupt_content_is_repaired() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().canonicalize().unwrap().join("cached-media");
        let a = uri('a');
        let cache = ArtworkCache::new(dir.clone());
        drop(add(&cache, &a));
        let file = path(&cache, &a);
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            file.metadata().unwrap().ino()
        };
        drop(cache);
        let reopened = ArtworkCache::new(dir);
        let (v, files) = add(&reopened, &a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(file.metadata().unwrap().ino(), inode);
        }
        assert_eq!(files.source(v["image"].as_str().unwrap()).unwrap(), a);
        drop(files);
        fs::write(&file, vec![b'x'; MIN_BYTES * 3 / 4]).unwrap();
        let (v, files) = add(&reopened, &a);
        assert_eq!(files.source(v["image"].as_str().unwrap()).unwrap(), a);
        drop(files);
        fs::write(reopened.directory.join("user-file"), b"keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            reopened.directory.join("user-file"),
            reopened.directory.join(format!("{}.art", "1".repeat(64))),
        )
        .unwrap();
        let report = reopened.maintain(Some(true)).unwrap();
        assert_eq!(report.removed_entries, 1);
        assert_eq!(
            fs::read(reopened.directory.join("user-file")).unwrap(),
            b"keep"
        );
    }
}
