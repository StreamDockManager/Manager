//! Bounded, bytes-only conversion of distributed V2 archives into an untrusted plan
//! The API accepts bytes only and receives no filesystem, runtime or persistence handles
//! It runs in process and is not an operating-system sandbox
use hardware_core::plugin_display::VendorTitleStyle;
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{Cursor, Read},
};

#[derive(Debug)]
pub(crate) struct ConversionPlan {
    pub name: String,
    pub device_model: String,
    pub device_type: u32,
    pub rows: u8,
    pub columns: u8,
    pub actions: Vec<VendorAction>,
}
#[derive(Debug)]
pub(crate) struct VendorAction {
    pub column: u8,
    pub row: u8,
    pub uuid: String,
    pub name: String,
    pub settings: Value,
    pub current_state: usize,
    pub linked_title: bool,
    pub states: Vec<VendorTitleStyle>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Root {
    device: Device,
    name: String,
    pages: Pages,
    version: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Device {
    model: String,
    #[serde(rename = "UUID")]
    uuid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Pages {
    current: String,
    default: String,
    pages: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Page {
    controllers: Vec<Controller>,
    icon: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Controller {
    #[serde(rename = "Type")]
    kind: String,
    actions: BTreeMap<String, Action>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Action {
    #[serde(rename = "ActionID")]
    action_id: String,
    linked_title: bool,
    name: String,
    settings: Value,
    state: usize,
    states: Vec<State>,
    #[serde(rename = "UUID")]
    uuid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct State {
    font_family: String,
    font_size: f64,
    font_style: String,
    font_underline: bool,
    outline_thickness: f64,
    show_title: bool,
    title_alignment: String,
    title_color: String,
}
impl From<State> for VendorTitleStyle {
    // Convert the input into the vendor profile archive representation
    fn from(s: State) -> Self {
        Self {
            font_family: s.font_family,
            font_size: s.font_size,
            font_style: s.font_style,
            font_underline: s.font_underline,
            outline_thickness: s.outline_thickness,
            show_title: s.show_title,
            title_alignment: s.title_alignment,
            title_color: s.title_color,
        }
    }
}
// serde_json::Value normally overwrites duplicate members. Reject them at every depth,
// including opaque plugin settings, before typed deserialization can lose evidence
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    // Decode vendor profile archive from the incoming serialized value
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            // Describe the duplicate-key-rejecting JSON format expected by the visitor
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unique-key JSON")
            }
            // Visit map
            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = m.next_entry::<String, Unique>()? {
                    if out.insert(k, v.0).is_some() {
                        return Err(de::Error::custom("duplicate JSON member"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            // Visit seq
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut out = vec![];
                while let Some(v) = a.next_element::<Unique>()? {
                    out.push(v.0);
                }
                Ok(Unique(Value::Array(out)))
            }
            // Visit bool
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            // Visit i64
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            // Visit u64
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            // Visit f64
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            // Visit str
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            // Visit unit
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}
// Decode JSON through the unique-key validator before converting it to the requested type
fn json<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, String> {
    let value: Unique = serde_json::from_slice(b).map_err(|e| e.to_string())?;
    serde_json::from_value(value.0).map_err(|e| e.to_string())
}
// Require a canonical hyphenated profile UUID while allowing hexadecimal letter case differences
fn uuid(s: &str) -> Result<uuid::Uuid, String> {
    let id = uuid::Uuid::parse_str(s).map_err(|_| "Invalid profile UUID")?;
    if s.len() != 36 || !s.eq_ignore_ascii_case(&id.hyphenated().to_string()) {
        return Err("Noncanonical profile UUID".into());
    }
    Ok(id)
}
/// Observed V2 mapping, verified against all twelve pinned page UUID/path pairs
/// This is not a claim about V3 or arbitrary future profile versions
fn page_path(id: &str) -> Result<String, String> {
    let id = uuid(id)?;
    let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
    let mut out = String::new();
    let mut bits = 0u32;
    let mut n = 0;
    for byte in id.as_bytes() {
        bits = (bits << 8) | u32::from(*byte);
        n += 8;
        while n >= 5 {
            n -= 5;
            let c = alphabet[((bits >> n) & 31) as usize];
            out.push(match c {
                b'U' => 'V',
                b'V' => 'W',
                _ => c as char,
            });
        }
    }
    if n > 0 {
        let c = alphabet[((bits << (5 - n)) & 31) as usize];
        out.push(match c {
            b'U' => 'V',
            b'V' => 'W',
            _ => c as char,
        });
    }
    out.push('Z');
    Ok(out)
}
// Map a supported vendor device model to its profile type and key-grid dimensions
pub(crate) fn geometry(model: &str) -> Result<(u32, u8, u8), String> {
    match model {
        "20GAA9901" => Ok((0, 5, 3)),
        "20GAI9901" => Ok((1, 3, 2)),
        "20GAT9901" => Ok((2, 8, 4)),
        "VSD2/WiFi" => Ok((3, 8, 8)),
        "20GBD9901" => Ok((7, 4, 2)),
        "20GBJ9901" => Ok((9, 4, 2)),
        _ => Err("Unsupported distributed profile device model".into()),
    }
}
// Validate supported page-controller structure and extract its keypad actions
fn controllers(page: Page, device: u32) -> Result<BTreeMap<String, Action>, String> {
    if !page.icon.is_empty() || !page.name.is_empty() {
        return Err("Unsupported page name/icon".into());
    }
    let kinds: &[&str] = match device {
        7 => &["Keypad", "Encoder"],
        9 => &["Keypad", "Neo"],
        _ => &["Keypad"],
    };
    if page.controllers.len() != kinds.len() {
        return Err("Unsupported page controllers".into());
    }
    let mut actions = BTreeMap::new();
    for (controller, kind) in page.controllers.into_iter().zip(kinds) {
        if controller.kind != *kind || (*kind != "Keypad" && !controller.actions.is_empty()) {
            return Err("Unsupported page controller content".into());
        }
        if *kind == "Keypad" {
            actions = controller.actions;
        }
    }
    Ok(actions)
}
// Decode bounded bytes into an inert plan without consulting host state or allocating live IDs
pub(crate) fn convert(bytes: &[u8]) -> Result<ConversionPlan, String> {
    const LIMIT: u64 = 256 * 1024;
    if bytes.len() > 1024 * 1024 {
        return Err("Unsupported vendor profile archive".into());
    }
    // Bounded single-disk ZIP shape, excluding ZIP64, comments, trailers and
    // duplicate entries hidden by a reader's filename index
    let end = bytes
        .get(bytes.len().saturating_sub(22)..)
        .ok_or("Missing ZIP end record")?;
    if end.len() != 22 || &end[..8] != b"PK\x05\x06\0\0\0\0" || end[20..] != [0, 0] {
        return Err("Unsupported ZIP end record".into());
    }
    // Directory entries are optional ZIP bookkeeping, not profile content. The
    // same three required manifest files may have any subset of six known dirs
    let entry_count = usize::from(u16::from_le_bytes(end[10..12].try_into().unwrap()));
    if end[8..10] != end[10..12] || !(3..=9).contains(&entry_count) {
        return Err("Unsupported profile ZIP entry count".into());
    }
    let central_size = u32::from_le_bytes(end[12..16].try_into().unwrap()) as usize;
    let central_start = u32::from_le_bytes(end[16..20].try_into().unwrap()) as usize;
    if central_start.checked_add(central_size) != Some(bytes.len() - 22) {
        return Err("Invalid ZIP central directory bounds".into());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if zip.len() != entry_count || zip.has_duplicate_names() {
        return Err("Expected the supported V2 profile entries".into());
    }
    zip.validate_header_consistency()
        .map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut total = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if name.len() > 240
            || name.contains('\\')
            || name.starts_with('/')
            || name.split('/').any(|p| p == "." || p == "..")
            || !seen.insert(name.clone())
            || entry.encrypted()
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
            || entry.unix_mode().is_some_and(|m| {
                m & 0o170000 != 0
                    && m & 0o170000 != if entry.is_dir() { 0o040000 } else { 0o100000 }
            })
            || entry.size() > LIMIT
            || entry.size()
                > entry
                    .compressed_size()
                    .saturating_mul(200)
                    .saturating_add(1024)
        {
            return Err("Unsafe or unsupported profile ZIP entry".into());
        }
        total += entry.size();
        if total > LIMIT {
            return Err("Profile archive exceeds expanded limit".into());
        }
        if entry.is_dir() {
            if entry.size() != 0 {
                return Err("Nonempty ZIP directory".into());
            }
            dirs.insert(name);
        } else {
            let mut data = Vec::new();
            entry
                .take(LIMIT + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() as u64 > LIMIT {
                return Err("Profile entry exceeds limit".into());
            }
            files.insert(name, data);
        }
    }
    let roots: Vec<_> = files
        .keys()
        .filter(|s| s.split('/').count() == 2 && s.ends_with("/manifest.json"))
        .cloned()
        .collect();
    if roots.len() != 1 {
        return Err("Missing unique profile root".into());
    }
    let prefix = roots[0].strip_suffix("manifest.json").unwrap();
    uuid(
        prefix
            .strip_suffix(".sdProfile/")
            .ok_or("Invalid profile root")?,
    )?;
    let root: Root = json(&files[&roots[0]])?;
    let (device_type, columns, rows) = geometry(&root.device.model)?;
    if root.version != "2.0"
        || !root.device.uuid.is_empty()
        || root.name.is_empty()
        || root.name.len() > 256
        || root.name.chars().any(char::is_control)
        || root.pages.pages != vec![root.pages.current.clone()]
        || uuid(&root.pages.current)? == uuid(&root.pages.default)?
    {
        return Err("Unsupported V2 profile metadata or declared geometry mismatch".into());
    }
    let current = page_path(&root.pages.current)?;
    let default = page_path(&root.pages.default)?;
    let expected_dirs: BTreeSet<_> = [
        prefix.to_string(),
        format!("{prefix}Profiles/"),
        format!("{prefix}Profiles/{current}/"),
        format!("{prefix}Profiles/{current}/Images/"),
        format!("{prefix}Profiles/{default}/"),
        format!("{prefix}Profiles/{default}/Images/"),
    ]
    .into_iter()
    .collect();
    let current_file = format!("{prefix}Profiles/{current}/manifest.json");
    let default_file = format!("{prefix}Profiles/{default}/manifest.json");
    if !dirs.is_subset(&expected_dirs)
        || files.len() != 3
        || !files.contains_key(&current_file)
        || !files.contains_key(&default_file)
    {
        return Err("Unlisted profile pages or resources".into());
    }
    if !controllers(json(&files[&default_file])?, device_type)?.is_empty() {
        return Err("Default page must be empty".into());
    }
    let mut actions = vec![];
    let mut ids = BTreeSet::new();
    for (position, a) in controllers(json(&files[&current_file])?, device_type)? {
        let (column, row) = position
            .split_once(',')
            .ok_or("Invalid action coordinates")?;
        let column: u8 = column.parse().map_err(|_| "Invalid column")?;
        let row: u8 = row.parse().map_err(|_| "Invalid row")?;
        if position != format!("{column},{row}") || column >= columns || row >= rows {
            return Err("Profile coordinate outside declared geometry".into());
        }
        // ActionID is source-instance evidence,
        // never a live runtime context. Independent per-state title editing is unqualified
        if !ids.insert(uuid(&a.action_id)?)
            || !a.settings.is_object()
            || a.states.is_empty()
            || a.states.len() > 64
            || a.state >= a.states.len()
            || a.name.len() > 256
            || !a.linked_title
        {
            return Err("Invalid profile action state/settings/identity".into());
        }
        let states: Vec<VendorTitleStyle> = a.states.into_iter().map(Into::into).collect();
        for state in &states {
            state.validate()?;
        }
        actions.push(VendorAction {
            column,
            row,
            uuid: a.uuid,
            name: a.name,
            settings: a.settings,
            current_state: a.state,
            linked_title: a.linked_title,
            states,
        });
    }
    Ok(ConversionPlan {
        name: root.name,
        device_model: root.device.model,
        device_type,
        columns,
        rows,
        actions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_plugin_host::PluginInfo;

    // Exercise conversion and trusted acceptance together for existing archive regressions
    fn parse(bytes: &[u8], owner: &PluginInfo, device: u32) -> Result<ConversionPlan, String> {
        crate::vendor_profiles::validate_conversion(convert(bytes)?, owner, device)
            .map(|validated| validated.test_plan())
    }
    use std::io::Write;
    const BASE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/plugins/vendor/elgato/lights-out-profiles/"
    );
    // Construct a disabled vendor plugin owner from the fixture manifest action metadata
    fn owner() -> PluginInfo {
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(format!("{BASE}manifest.json")).unwrap())
                .unwrap();
        serde_json::from_value(serde_json::json!({"id":"com.elgato.lightsout","name":"Lights Out","version":"1.0","dialect":"streamdeck","enabled":false,"kind":"native","status":"disabled","health":{},
            "actions":manifest["Actions"].as_array().unwrap().iter().map(|a|serde_json::json!({"id":a["UUID"],"name":a["Name"],"control_types":["button"],"supported_in_multi_actions":true,"supported_in_key_logic_actions":true,"visible_in_actions_list":true,"user_title_enabled":true,"state_names":a["States"].as_array().unwrap().iter().map(|_|"").collect::<Vec<_>>() })).collect::<Vec<_>>() })).unwrap()
    }
    // Return the underlying vendor profile archive source
    fn source() -> Vec<(String, Vec<u8>)> {
        let b = std::fs::read(format!("{BASE}sd-mini.streamDeckProfile")).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(b)).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut e = zip.by_index(i).unwrap();
                let n = e.name().unwrap().into_owned();
                let mut b = vec![];
                e.read_to_end(&mut b).unwrap();
                (n, b)
            })
            .collect()
    }
    // Pack the selected vendor profile archive content
    fn pack(entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(vec![]));
        for (name, b) in entries {
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            if name.ends_with('/') {
                z.add_directory(name, o).unwrap();
            } else {
                z.start_file(name, o).unwrap();
                z.write_all(&b).unwrap();
            }
        }
        z.finish().unwrap().into_inner()
    }
    // Apply a mutation to vendor profile archive
    fn mutate(part: &str, change: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut entries = source();
        let (_, b) = entries
            .iter_mut()
            .find(|(n, b)| {
                n.ends_with("manifest.json")
                    && if part == "root" {
                        n.split('/').count() == 2
                    } else {
                        String::from_utf8_lossy(b).contains("ActionID")
                    }
            })
            .unwrap();
        let mut v: Value = serde_json::from_slice(b).unwrap();
        change(&mut v);
        *b = serde_json::to_vec(&v).unwrap();
        pack(entries)
    }
    #[test]
    // Verify that authentic archives match declared geometry and appearance
    fn authentic_archives_match_declared_geometry_and_appearance() {
        for (file, device, columns, rows) in [
            ("sd", 0, 5, 3),
            ("sd-mini", 1, 3, 2),
            ("sdxl", 2, 8, 4),
            ("sd-mobile", 3, 8, 8),
            ("sdp", 7, 4, 2),
            ("sd-neo", 9, 4, 2),
        ] {
            let b = std::fs::read(format!("{BASE}{file}.streamDeckProfile")).unwrap();
            let p = parse(&b, &owner(), device).unwrap();
            assert_eq!((p.columns, p.rows), (columns, rows));
            assert_eq!(p.actions.len(), usize::from(columns * rows));
            assert_eq!(p.name, "Lights Out");
            assert_eq!(p.device_type, device);
            assert!(!p.device_model.is_empty());
            for a in p.actions {
                assert!(a.linked_title);
                assert_eq!(a.current_state, 0);
                assert!(a.uuid.starts_with("com.elgato.lightsout."));
                assert_eq!(a.states[0].font_size, 9.0);
                assert_eq!(a.states[0].title_color, "#ffffff");
            }
            assert!(parse(&b, &owner(), 99).is_err());
        }
    }
    #[test]
    // Verify that supports optional directory records without changing manifest content
    fn supports_optional_directory_records_without_changing_manifest_content() {
        // Repacked derivatives of pinned originals, not new vendor-schema evidence
        for (file, device) in [
            ("sd", 0),
            ("sd-mini", 1),
            ("sdxl", 2),
            ("sd-mobile", 3),
            ("sdp", 7),
            ("sd-neo", 9),
        ] {
            let bytes = std::fs::read(format!("{BASE}{file}.streamDeckProfile")).unwrap();
            let original = parse(&bytes, &owner(), device).unwrap();
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let entries: Vec<_> = (0..zip.len())
                .map(|i| {
                    let mut entry = zip.by_index(i).unwrap();
                    let name = entry.name().unwrap().into_owned();
                    let mut data = vec![];
                    entry.read_to_end(&mut data).unwrap();
                    (name, data)
                })
                .collect();
            // Every subset of known directory records must be equivalent
            for mask in 0u8..64 {
                let mut directory = 0;
                let selected = entries
                    .iter()
                    .filter(|(name, _)| {
                        if !name.ends_with('/') {
                            return true;
                        }
                        let keep = mask & (1 << directory) != 0;
                        directory += 1;
                        keep
                    })
                    .cloned()
                    .collect();
                let parsed = parse(&pack(selected), &owner(), device).unwrap();
                assert_eq!(parsed.name, original.name);
                assert_eq!(parsed.actions.len(), original.actions.len());
                for (a, b) in parsed.actions.iter().zip(&original.actions) {
                    assert_eq!(
                        (a.column, a.row, &a.uuid, &a.settings, a.current_state),
                        (b.column, b.row, &b.uuid, &b.settings, b.current_state)
                    );
                    assert_eq!(
                        serde_json::to_value(&a.states).unwrap(),
                        serde_json::to_value(&b.states).unwrap()
                    );
                }
            }
        }
    }
    #[test]
    // Verify that optional directories do not admit extra content or invalid counts
    fn optional_directories_do_not_admit_extra_content_or_invalid_counts() {
        let files: Vec<_> = source()
            .into_iter()
            .filter(|(n, _)| !n.ends_with('/'))
            .collect();
        let mut unknown_dir = files.clone();
        unknown_dir.push(("unlisted/".into(), vec![]));
        assert!(parse(&pack(unknown_dir), &owner(), 1).is_err());
        let mut unknown_file = files.clone();
        unknown_file.push(("unlisted.txt".into(), vec![1]));
        assert!(parse(&pack(unknown_file), &owner(), 1).is_err());
        let mut missing = files.clone();
        missing.pop();
        assert!(parse(&pack(missing), &owner(), 1).is_err());
        let mut mismatched_count = pack(files);
        let end = mismatched_count.len() - 22;
        mismatched_count[end + 8..end + 10].copy_from_slice(&4u16.to_le_bytes());
        assert!(parse(&mismatched_count, &owner(), 1).is_err());
    }
    #[test]
    // Verify that rejects unknown fields versions pages and controller content
    fn rejects_unknown_fields_versions_pages_and_controller_content() {
        for b in [
            mutate("root", |v| v["Version"] = "3.0".into()),
            mutate("root", |v| v["Unknown"] = true.into()),
            mutate("root", |v| {
                v["Pages"]["Pages"] =
                    serde_json::json!([v["Pages"]["Current"], v["Pages"]["Default"]])
            }),
            mutate("root", |v| {
                v["Pages"]["Current"] = v["Pages"]["Default"].clone()
            }),
            mutate("page", |v| v["Controllers"][0]["Type"] = "Encoder".into()),
            mutate("page", |v| {
                v["Controllers"][0]["Actions"]["0,0"]["States"][0]["Image"] = "x".into()
            }),
            mutate("page", |v| {
                v["Controllers"][0]["Actions"]["0,0"]["States"][0]["TitleAlignment"] = "left".into()
            }),
            mutate("page", |v| {
                v["Controllers"][0]["Actions"]["0,0"]["State"] = 99.into()
            }),
            mutate("page", |v| {
                v["Controllers"][0]["Actions"]["0,0"]["UUID"] =
                    "com.elgato.streamdeck.builtin".into()
            }),
            mutate("page", |v| {
                let a = v["Controllers"][0]["Actions"]["0,0"].take();
                v["Controllers"][0]["Actions"]
                    .as_object_mut()
                    .unwrap()
                    .remove("0,0");
                v["Controllers"][0]["Actions"]["9,9"] = a;
            }),
        ] {
            assert!(parse(&b, &owner(), 1).is_err());
        }
    }
    #[test]
    // Verify that rejects duplicate JSON members even in settings
    fn rejects_duplicate_json_members_even_in_settings() {
        assert!(json::<Value>(br#"{"x":1,"x":2}"#).is_err());
        assert!(json::<Value>(br#"{"Settings":{"x":1,"x":2}}"#).is_err());
        let mut e = source();
        let (_, b) = e
            .iter_mut()
            .find(|(n, _)| n.split('/').count() == 2 && n.ends_with("manifest.json"))
            .unwrap();
        *b = String::from_utf8(b.clone())
            .unwrap()
            .replacen("{", "{\"Version\":\"2.0\",", 1)
            .into_bytes();
        assert!(parse(&pack(e), &owner(), 1).is_err());
    }
    #[test]
    // Verify that rejects resources traversal wrong owner and nonempty default
    fn rejects_resources_traversal_wrong_owner_and_nonempty_default() {
        let mut e = source();
        e[0].0 = "../escape/".into();
        assert!(parse(&pack(e), &owner(), 1).is_err());
        let mut e = source();
        e.push(("resource.png".into(), vec![1]));
        assert!(parse(&pack(e), &owner(), 1).is_err());
        let mut e = source();
        let current = e
            .iter()
            .find(|(_, b)| String::from_utf8_lossy(b).contains("ActionID"))
            .unwrap()
            .1
            .clone();
        let (_, default) = e
            .iter_mut()
            .find(|(n, b)| {
                n.ends_with("manifest.json")
                    && String::from_utf8_lossy(b).contains("Controllers")
                    && !String::from_utf8_lossy(b).contains("ActionID")
            })
            .unwrap();
        *default = current;
        assert!(parse(&pack(e), &owner(), 1).is_err());
        let mut o = owner();
        o.actions.clear();
        assert!(parse(&pack(source()), &o, 1).is_err());
    }
    #[test]
    // Verify that rejects zip duplicates symlinks bombs and trailers
    fn rejects_zip_duplicates_symlinks_bombs_and_trailers() {
        let mut bytes = pack(source());
        bytes.push(0);
        assert!(parse(&bytes, &owner(), 1).is_err());
        let mut entries = source();
        let (_, data) = entries
            .iter_mut()
            .find(|(n, _)| n.ends_with("manifest.json"))
            .unwrap();
        *data = vec![b' '; 256 * 1024 + 1];
        assert!(parse(&pack(entries), &owner(), 1).is_err());
        let mut bytes = pack(source());
        let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        // Mark an otherwise normal entry as a Unix symlink in the central record
        bytes[central + 5] = 3;
        bytes[central + 38..central + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
        assert!(parse(&bytes, &owner(), 1).is_err());
        let entries = source();
        let paths: Vec<_> = entries
            .iter()
            .filter(|(n, _)| n.ends_with("/manifest.json") && n.split('/').count() > 2)
            .map(|(n, _)| n.clone())
            .collect();
        let bytes = pack(entries);
        let from = paths[0].as_bytes();
        let to = paths[1].as_bytes();
        assert_eq!(from.len(), to.len());
        let mut duplicated = bytes.clone();
        for start in 0..=bytes.len() - from.len() {
            if &bytes[start..start + from.len()] == from {
                duplicated[start..start + from.len()].copy_from_slice(to);
            }
        }
        assert!(parse(&duplicated, &owner(), 1).is_err());
    }
    #[test]
    // Verify that preserves settings per state style and initial state without source IDs
    fn preserves_settings_per_state_style_and_initial_state_without_source_ids() {
        let bytes = mutate("page", |v| {
            let a = &mut v["Controllers"][0]["Actions"]["0,1"];
            a["State"] = 1.into();
            a["Settings"] = serde_json::json!({"level":3,"nested":{"kept":true}});
            a["States"][1]["ShowTitle"] = false.into();
        });
        let p = parse(&bytes, &owner(), 1).unwrap();
        let a = p
            .actions
            .iter()
            .find(|a| a.column == 0 && a.row == 1)
            .unwrap();
        assert_eq!(a.current_state, 1);
        assert!(a.linked_title);
        assert!(!a.states[1].show_title);
        assert!(a.states[0].show_title);
        assert_eq!(a.settings["nested"]["kept"], true);
        assert_eq!(a.uuid, "com.elgato.lightsout.gamepiece");
    }
    #[test]
    // Verify that rejects unimplemented action name and unlinked title edits
    fn rejects_unimplemented_action_name_and_unlinked_title_edits() {
        for (key, value) in [
            ("Name", serde_json::json!("Changed name")),
            ("LinkedTitle", serde_json::json!(false)),
        ] {
            let bytes = mutate("page", |v| {
                v["Controllers"][0]["Actions"]["0,1"][key] = value;
            });
            assert!(parse(&bytes, &owner(), 1).is_err());
        }
    }
    #[test]
    // Keep conversion independent of installed plugins while rejecting unauthorized plans at admission
    fn conversion_is_inert_and_trusted_admission_rejects_forged_plans() {
        let bytes = pack(source());
        let info = owner();
        let plan = convert(&bytes).unwrap();
        assert_eq!(plan.name, parse(&bytes, &info, 1).unwrap().name);
        let mut wrong_owner = owner();
        wrong_owner.actions.clear();
        assert!(crate::vendor_profiles::validate_conversion(plan, &wrong_owner, 1).is_err());
        for mutation in 0..7 {
            let mut plan = convert(&bytes).unwrap();
            match mutation {
                0 => plan.actions[0].uuid = "unowned.action".into(),
                1 => plan.actions[0].current_state = usize::MAX,
                2 => plan.actions[0].name = "forged name".into(),
                3 => plan.columns = 255,
                4 => plan.actions[0].settings = serde_json::json!([]),
                5 => plan.actions[0].settings = serde_json::json!({"data": "x".repeat(256 * 1024)}),
                _ => {
                    plan.actions[1].column = plan.actions[0].column;
                    plan.actions[1].row = plan.actions[0].row;
                }
            }
            assert!(
                crate::vendor_profiles::validate_conversion(plan, &info, 1).is_err(),
                "mutation {mutation}"
            );
        }
    }
}
