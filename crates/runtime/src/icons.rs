//! Iconify's public catalog, with a persistent local cache and no network on offline calls
use hardware_core::appearance::{icon_id, IconAsset, ICONS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, OnceLock};
use std::{collections::BTreeMap, io::Read, path::PathBuf, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct IconCollection {
    pub prefix: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub license_url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct IconResults {
    pub icons: Vec<String>,
    pub collections: Vec<IconCollection>,
    pub more: bool,
    pub offline: bool,
    pub message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalIconMetadata {
    id: String,
    // Search does not retain or render vectors. load()/save() validate the SVG
    // before use; serde skips its JSON value without allocating its payload
    #[serde(rename = "svg")]
    _svg: serde::de::IgnoredAny,
    collection: String,
    author: String,
    license: String,
    license_url: String,
}
impl LocalIconMetadata {
    // Report the valid condition for icons
    fn valid(&self) -> bool {
        icon_id(&self.id)
            && [
                &self.collection,
                &self.author,
                &self.license,
                &self.license_url,
            ]
            .iter()
            .all(|value| value.len() <= 1024)
    }
}
// Reuse a bounded HTTP client that refuses redirects for icon requests
fn client() -> Result<reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("Stream-Dock-Manager/0.1.0 (Iconify icon selector)")
                .build()
                .map_err(|e| e.to_string())
        })
        .clone()
}
// Return the requested icons value
fn get(path: &str, query: &[(&str, String)], limit: u64) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&format!("https://api.iconify.design/{path}"))
        .map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
    let response = client()?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    if response.content_length().is_some_and(|n| n > limit) {
        return Err("Iconify response is too large".into());
    }
    let mut text = String::new();
    response
        .take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > limit {
        return Err("Iconify response is too large".into());
    }
    Ok(text)
}
// Decode bounded collection labels and metadata from the Iconify response
fn collection(prefix: &str, v: &serde_json::Value) -> IconCollection {
    let text = |key: &str| {
        v.pointer(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(1024)
            .collect()
    };
    IconCollection {
        prefix: prefix.into(),
        name: text("/name"),
        author: text("/author/name"),
        license: text("/license/title"),
        license_url: text("/license/url"),
    }
}
#[derive(Clone)]
pub struct IconStore {
    root: PathBuf,
    collections: Arc<Mutex<BTreeMap<String, IconCollection>>>,
}
impl IconStore {
    // Initialize icons state from the supplied inputs
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            collections: Default::default(),
        }
    }
    // Validate the icon ID and hash it into a local cache filename
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if !icon_id(id) {
            return Err("Invalid Iconify icon name".into());
        }
        Ok(self.root.join(format!(
            "{}.json",
            hex::encode(Sha256::digest(id.as_bytes()))
        )))
    }
    // Read a bounded cached icon only if its identity and content validate
    fn cached(&self, id: &str) -> Option<IconAsset> {
        let path = self.path(id).ok()?;
        let mut json = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(300 * 1024)
            .read_to_string(&mut json)
            .ok()?;
        let asset: IconAsset = serde_json::from_str(&json).ok()?;
        (asset.id == id && asset.validate().is_ok()).then_some(asset)
    }
    // Save the supplied icons state
    pub fn save(&self, asset: &IconAsset) -> Result<(), String> {
        asset.validate()?;
        let path = self.path(&asset.id)?;
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        serde_json::to_writer(tmp.as_file_mut(), asset).map_err(|e| e.to_string())?;
        tmp.as_file().sync_all().map_err(|e| e.to_string())?;
        tmp.persist(path).map_err(|e| e.to_string())?;
        Ok(())
    }
    // Source-equivalent eager inventory retained only for test fixtures and the
    // allocation comparison; production search never loads the full inventory
    #[cfg(test)]
    // Collect valid locally cached icons without trusting arbitrary cache entries
    fn local(&self) -> Vec<IconAsset> {
        let mut assets = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Ok(file) = std::fs::File::open(entry.path()) else {
                    continue;
                };
                let mut json = String::new();
                if file.take(300 * 1024).read_to_string(&mut json).is_ok() {
                    if let Ok(asset) = serde_json::from_str::<IconAsset>(&json) {
                        if asset.validate().is_ok() {
                            assets.insert(asset.id.clone(), asset);
                        }
                    }
                }
            }
        }
        for (name, svg) in ICONS {
            let id = format!("lucide:{name}");
            assets.entry(id.clone()).or_insert_with(|| IconAsset {
                id,
                svg: svg.to_string(),
                collection: "Lucide".into(),
                author: "Lucide Contributors".into(),
                license: "ISC".into(),
                license_url: "https://lucide.dev/license".into(),
            });
        }
        assets.into_values().collect()
    }
    // Filter and paginate local icon IDs by query words and collection prefix
    fn local_page(&self, query: &str, prefix: &str, start: u32) -> (Vec<String>, bool) {
        let query = query.trim().to_lowercase();
        let prefix = format!("{prefix}:");
        let limit = start as usize + 65;
        let mut ids = std::collections::BTreeSet::new();
        let mut include = |id: String| {
            if query.split_whitespace().all(|word| id.contains(word))
                && (prefix == ":" || id.starts_with(&prefix))
            {
                ids.insert(id);
                if ids.len() > limit {
                    ids.pop_last();
                }
            }
        };
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Ok(file) = std::fs::File::open(&path) else {
                    continue;
                };
                if file
                    .metadata()
                    .ok()
                    .is_none_or(|meta| !meta.is_file() || meta.len() > 300 * 1024)
                {
                    continue;
                }
                let source = std::io::BufReader::new(file.take(300 * 1024 + 1));
                let Ok(meta) = serde_json::from_reader::<_, LocalIconMetadata>(source) else {
                    continue;
                };
                if meta.valid() && self.path(&meta.id).ok().as_ref() == Some(&path) {
                    include(meta.id);
                }
            }
        }
        for (name, _) in ICONS {
            include(format!("lucide:{name}"));
        }
        let more = ids.len() > start as usize + 64;
        (
            ids.into_iter().skip(start as usize).take(64).collect(),
            more,
        )
    }
    // Search the icons collection
    pub fn search(
        &self,
        query: &str,
        prefix: &str,
        start: u32,
        offline: bool,
    ) -> Result<IconResults, String> {
        self.search_using(query, prefix, start, offline, get)
    }
    // Search remote icons when allowed and fall back to saved and bundled collections
    fn search_using(
        &self,
        query: &str,
        prefix: &str,
        start: u32,
        offline: bool,
        fetch: impl Fn(&str, &[(&str, String)], u64) -> Result<String, String>,
    ) -> Result<IconResults, String> {
        if query.len() > 160
            || prefix.len() > 100
            || start > 100_000
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("Invalid icon search".into());
        }
        let fallback = |message: String| {
            let (icons, more) = self.local_page(query, prefix, start);
            IconResults {
                icons,
                collections: vec![],
                more,
                offline: true,
                message,
            }
        };
        if offline {
            return Ok(fallback(
                "Offline mode: saved icons and bundled Lucide icons.".into(),
            ));
        }
        if query.trim().is_empty() && prefix.is_empty() {
            return Ok(fallback(
                "Search Iconify's full library, or choose an offline icon below.".into(),
            ));
        }
        let remote = || -> Result<IconResults, String> {
            let (icons, collections, more) = if query.trim().is_empty() {
                let data: serde_json::Value = serde_json::from_str(&fetch(
                    "collection",
                    &[("prefix", prefix.into()), ("info", "1".into())],
                    4 * 1024 * 1024,
                )?)
                .map_err(|e| e.to_string())?;
                if data["prefix"].as_str() != Some(prefix) {
                    return Err("Invalid Iconify collection response".into());
                }
                let mut names = std::collections::BTreeSet::new();
                for list in std::iter::once(&data["uncategorized"]).chain(
                    data["categories"]
                        .as_object()
                        .into_iter()
                        .flat_map(|m| m.values()),
                ) {
                    for name in list
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                    {
                        let id = format!("{prefix}:{name}");
                        if icon_id(&id) {
                            names.insert(id);
                        }
                    }
                }
                let more = names.len() > start as usize + 64;
                (
                    names.into_iter().skip(start as usize).take(64).collect(),
                    vec![collection(prefix, &data["info"])],
                    more,
                )
            } else {
                let params = [
                    ("query", query.trim().into()),
                    ("prefix", prefix.into()),
                    ("limit", "64".into()),
                    ("start", start.to_string()),
                ];
                let data: serde_json::Value =
                    serde_json::from_str(&fetch("search", &params, 2 * 1024 * 1024)?)
                        .map_err(|e| e.to_string())?;
                let ids = data["icons"]
                    .as_array()
                    .ok_or("Invalid Iconify search response")?;
                let icons: Vec<String> = ids
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|id| icon_id(id))
                    .take(64)
                    .map(str::to_owned)
                    .collect();
                let collections: Vec<IconCollection> = data["collections"]
                    .as_object()
                    .map(|map| map.iter().map(|(p, v)| collection(p, v)).collect())
                    .unwrap_or_default();
                (icons, collections, ids.len() >= 64)
            };
            for meta in &collections {
                self.collections
                    .lock()
                    .unwrap()
                    .insert(meta.prefix.clone(), meta.clone());
            }
            Ok(IconResults {
                icons,
                collections,
                more,
                offline: false,
                message: String::new(),
            })
        };
        Ok(remote().unwrap_or_else(|_| fallback("Unable to reach Iconify. Showing saved icons and the bundled offline collection. Retry when connected.".into())))
    }
    // Load the requested icons state
    pub fn load(&self, id: &str, offline: bool) -> Result<IconAsset, String> {
        self.path(id)?;
        if let Some(asset) = self.cached(id) {
            return Ok(asset);
        }
        if let Some((name, svg)) = ICONS
            .iter()
            .find(|(name, _)| id == format!("lucide:{name}"))
        {
            return Ok(IconAsset {
                id: format!("lucide:{name}"),
                svg: svg.to_string(),
                collection: "Lucide".into(),
                author: "Lucide Contributors".into(),
                license: "ISC".into(),
                license_url: "https://lucide.dev/license".into(),
            });
        }
        if offline {
            return Err("This icon is not saved offline yet".into());
        }
        let (prefix, name) = id.split_once(':').ok_or("Invalid icon name")?;
        let svg = get(
            &format!("{prefix}/{name}.svg"),
            &[("height", "auto".into())],
            256 * 1024,
        )?;
        // Fetch attribution with the vector so selected icons retain their license when shared
        let cached_meta = self.collections.lock().unwrap().get(prefix).cloned();
        let meta = if let Some(meta) = cached_meta {
            meta
        } else {
            let info: serde_json::Value = serde_json::from_str(&get(
                "collections",
                &[("prefix", prefix.into())],
                512 * 1024,
            )?)
            .map_err(|e| e.to_string())?;
            let meta = collection(prefix, &info[prefix]);
            self.collections
                .lock()
                .unwrap()
                .insert(prefix.into(), meta.clone());
            meta
        };
        let asset = IconAsset {
            id: id.into(),
            svg,
            collection: meta.name,
            author: meta.author,
            license: meta.license,
            license_url: meta.license_url,
        };
        asset.validate()?;
        Ok(asset)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that local search pages sorted metadata and preserves load validation
    fn local_search_pages_sorted_metadata_and_preserves_load_validation() {
        let dir = tempfile::tempdir().unwrap();
        let store = IconStore::new(dir.path().into());
        let mut asset = store.load("lucide:music", true).unwrap();
        for index in (0..150).rev() {
            asset.id = format!("fixture:icon-{index:03}");
            store.save(&asset).unwrap();
        }
        let mut all = Vec::new();
        for start in [0, 64, 128, 192] {
            let result = store
                .search_using("icon", "fixture", start, true, |_, _, _| {
                    panic!("offline HTTP")
                })
                .unwrap();
            assert!(result.icons.len() <= 64);
            assert_eq!(result.more, start < 128);
            all.extend(result.icons);
        }
        assert_eq!(
            all,
            (0..150)
                .map(|i| format!("fixture:icon-{i:03}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store.search("icon 149", "fixture", 0, true).unwrap().icons,
            ["fixture:icon-149"]
        );
        let selected = store.load(&all[149], true).unwrap();
        assert_eq!(selected.license, asset.license);
        // Search may describe a corrupted cache entry, but it can never render
        // that vector: the existing load validator still rejects it
        asset.svg = "<svg><script>alert(1)</script></svg>".into();
        std::fs::write(
            store.path(&asset.id).unwrap(),
            serde_json::to_vec(&asset).unwrap(),
        )
        .unwrap();
        assert!(store.load(&asset.id, true).is_err());
        std::fs::write(dir.path().join("malformed.json"), b"not json").unwrap();
        assert_eq!(
            store.search("icon 149", "fixture", 0, true).unwrap().icons,
            ["fixture:icon-149"]
        );
    }
    #[test]
    // Verify that local search five repeats do not allocate the SVG inventory
    fn local_search_five_repeats_do_not_allocate_the_svg_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let store = IconStore::new(dir.path().into());
        let mut asset = store.load("lucide:music", true).unwrap();
        asset.svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}</svg>",
            " ".repeat(64 * 1024)
        );
        for index in 0..300 {
            asset.id = format!("fixture:icon-{index:03}");
            store.save(&asset).unwrap();
        }
        let mut observations = Vec::new();
        for repeat in 1..=5 {
            let started = std::time::Instant::now();
            let ((baseline, payload), baseline_allocations, baseline_bytes) =
                crate::media::allocation_tests::measure_allocations(|| {
                    let assets = store.local();
                    let payload = assets.iter().map(|a| a.svg.len()).sum::<usize>();
                    (
                        assets
                            .into_iter()
                            .filter(|a| a.id.starts_with("fixture:"))
                            .map(|a| a.id)
                            .collect::<Vec<_>>(),
                        payload,
                    )
                });
            let baseline_ms = started.elapsed().as_secs_f64() * 1000.;
            let started = std::time::Instant::now();
            let (page, candidate_allocations, candidate_bytes) =
                crate::media::allocation_tests::measure_allocations(|| {
                    store.search("", "fixture", 64, true).unwrap()
                });
            let candidate_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(page.icons, baseline[64..128]);
            assert!(page.more);
            assert!(
                candidate_bytes < baseline_bytes / 4,
                "metadata search allocated SVG-scale buffers"
            );
            let loaded: Vec<_> = page
                .icons
                .iter()
                .map(|id| store.load(id, true).unwrap())
                .collect();
            observations.push(serde_json::json!({"repeat":repeat,"library_icons":300,"page_icons":loaded.len(),"baseline_inventory_svg_bytes":payload,"candidate_search_svg_bytes":0,"candidate_page_svg_bytes":loaded.iter().map(|a|a.svg.len()).sum::<usize>(),"baseline_requested_allocation_bytes":baseline_bytes,"candidate_requested_allocation_bytes":candidate_bytes,"baseline_allocations":baseline_allocations,"candidate_allocations":candidate_allocations,"baseline_ms":baseline_ms,"candidate_ms":candidate_ms,"ordering_matches":true}));
        }
        eprintln!(
            "ICON_METADATA_ALLOCATION={}",
            serde_json::to_string(&observations).unwrap()
        );
    }
    #[test]
    // Verify that offline never calls HTTP and failed or invalid API uses bundled icons
    fn offline_never_calls_http_and_failed_or_invalid_api_uses_bundled_icons() {
        let dir = tempfile::tempdir().unwrap();
        let store = IconStore::new(dir.path().into());
        assert!(
            store
                .search_using("music", "", 0, true, |_, _, _| panic!(
                    "Offline search made an HTTP request"
                ))
                .unwrap()
                .offline
        );
        for response in [
            Err("No connection".into()),
            Ok("not json".into()),
            Ok("{}".into()),
        ] {
            let result = store
                .search_using("music", "", 0, false, |_, _, _| response.clone())
                .unwrap();
            assert!(result.offline);
            assert_eq!(result.icons, ["lucide:music"]);
        }
    }
    #[test]
    // Verify that collection browsing deduplicates categories and keeps license
    fn collection_browsing_deduplicates_categories_and_keeps_license() {
        let dir = tempfile::tempdir().unwrap();
        let store = IconStore::new(dir.path().into());
        let result = store.search_using("", "example", 0, false, |path,_,_| {
            assert_eq!(path, "collection");
            Ok(r#"{"prefix":"example","uncategorized":["home"],"categories":{"General":["home","music"]},"aliases":{"house":"home"},"hidden":["old"],"info":{"name":"Example","license":{"title":"ISC"}}}"#.into())
        }).unwrap();
        assert!(!result.offline);
        assert_eq!(result.icons, ["example:home", "example:music"]);
        assert_eq!(result.collections[0].license, "ISC");
    }
    #[test]
    // Verify that offline and cache survive reopen without network
    fn offline_and_cache_survive_reopen_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = IconStore::new(dir.path().into());
        assert_eq!(store.local().len(), 24);
        for icon in store.local() {
            icon.validate().unwrap();
        }
        let mut a = store.local().remove(0);
        a.id = "example:portable-icon".into();
        store.save(&a).unwrap();
        let reopened = IconStore::new(dir.path().into());
        assert_eq!(reopened.load(&a.id, true).unwrap().svg, a.svg);
        assert_eq!(
            reopened.search("portable", "", 0, true).unwrap().icons,
            vec![a.id]
        );
        assert!(reopened.load("unknown:icon", true).is_err());
        assert!(reopened.load("../../secret:icon", true).is_err());
        assert!(reopened.search("", "../", 0, true).is_err());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    // Verify that icon vectors and licenses survive profile export without plugin settings
    fn icon_vectors_and_licenses_survive_profile_export_without_plugin_settings() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::ProfileStore::open(root.path().join("source")).unwrap();
        let mut data = store.load().unwrap();
        let mut asset = IconStore::new(root.path().join("icons")).local().remove(0);
        asset.id = "test:shared-icon".into();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .appearance = Some(Box::new(hardware_core::appearance::Appearance {
            icon: asset.id.clone(),
            icon_asset: Some(asset.clone()),
            ..Default::default()
        }));
        store.save(&data).unwrap();
        let zip = root.path().join("shared.zip");
        store.export(&zip).unwrap();
        let destination = crate::ProfileStore::open(root.path().join("destination")).unwrap();
        let imported = destination.import_for_repair(&zip).unwrap();
        let a = imported.profiles[0].pages[0].bindings["key-1"]
            .appearance
            .as_ref()
            .unwrap();
        a.validate().unwrap();
        assert_eq!(a.icon_asset.as_ref().unwrap().svg, asset.svg);
        assert_eq!(a.icon_asset.as_ref().unwrap().license, asset.license);
    }
    #[test]
    #[ignore = "explicit live Iconify API verification"]
    // Verify that live iconify search load and offline reopen
    fn live_iconify_search_load_and_offline_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = IconStore::new(root.path().into());
        let result = store.search("music", "mdi", 0, false).unwrap();
        assert!(!result.offline, "{}", result.message);
        assert!(result.icons.contains(&"mdi:music".into()));
        let collection = store.search("", "mdi", 0, false).unwrap();
        assert!(!collection.offline, "{}", collection.message);
        assert_eq!(collection.icons.len(), 64);
        assert!(collection.more);
        for id in ["logos:github-icon", "noto:rocket", "fluent-emoji-flat:star"] {
            store.load(id, false).unwrap().validate().unwrap();
        }
        let icon = store.load("mdi:music", false).unwrap();
        assert!(!icon.license.is_empty());
        store.save(&icon).unwrap();
        assert_eq!(
            IconStore::new(root.path().into())
                .load(&icon.id, true)
                .unwrap()
                .svg,
            icon.svg
        );
    }
}
