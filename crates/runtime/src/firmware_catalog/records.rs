//! Interpret vendor manifests without assuming that discovery establishes compatibility

use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{cmp::Ordering, collections::HashMap};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    Mirabox,
    VsdCraft,
    Test,
    Generic,
}

impl SourceId {
    /// Return the stable source identifier used in selection keys
    pub fn id(self) -> &'static str {
        match self {
            Self::Mirabox => "mirabox",
            Self::VsdCraft => "vsd_craft",
            Self::Test => "test",
            Self::Generic => "generic",
        }
    }

    /// Keep nonstandard catalogs out of ordinary comparisons
    pub fn standard(self) -> bool {
        matches!(self, Self::Mirabox | Self::VsdCraft)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Exact,
    Alias,
    Ambiguous,
    Unmatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VersionComparison {
    Newer,
    Equal,
    Older,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumStatus {
    Missing,
    Malformed,
    Valid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct FirmwareCandidate {
    pub id: String,
    pub source: SourceId,
    pub source_url: String,
    pub snapshot_id: String,
    pub model_key: String,
    pub variant_key: Option<String>,
    pub version: Option<String>,
    pub download_url: Option<String>,
    pub file_url: Option<String>,
    pub artifact_url: Option<String>,
    pub md5: Option<String>,
    pub checksum: ChecksumStatus,
    pub matching: MatchKind,
    pub comparison: VersionComparison,
    pub equivalent_to: Option<String>,
    pub reasons: Vec<String>,
    #[ts(type = "unknown")]
    pub raw: Value,
}

pub(crate) struct ParsedCatalog {
    pub records: Vec<FirmwareCandidate>,
    pub issues: Vec<String>,
}

/// Admit only the observed firmware origins, directories and passive file types
pub(crate) fn approved_artifact_url(raw: &str) -> Option<Url> {
    let path = raw.strip_prefix("https://cdn1.key123.vip/")?;
    let filename = path
        .strip_prefix("StreamDock/firmware/download/")
        .or_else(|| path.strip_prefix("VSDCraft/firmware/download/"))?;
    // Check the raw final segment before the URL parser can normalize a traversal
    if filename.is_empty()
        || filename.len() > 512
        || filename.contains(['/', '\\', '?', '#'])
        || filename.chars().any(char::is_control)
        || ![".img", ".bin", ".fwpkg"]
            .iter()
            .any(|ext| filename.ends_with(ext))
    {
        return None;
    }
    // Only encoded spaces have been observed in filenames; reject encoded separators,
    // encoded dots, and double-escaped path characters rather than decoding them
    let unescaped = filename.replace("%20", " ");
    if unescaped.contains('%') {
        return None;
    }
    Url::parse(raw).ok()
}

/// Preserve a JSON string verbatim and diagnose any nonstring value
fn raw_string(row: &Map<String, Value>, field: &str, reasons: &mut Vec<String>) -> Option<String> {
    match row.get(field) {
        Some(Value::String(string)) => Some(string.clone()),
        Some(_) => {
            reasons.push(format!("invalid_{field}"));
            None
        }
        None => None,
    }
}

/// Normalize one independently addressable record while retaining source fields
fn candidate(
    source: SourceId,
    source_url: &str,
    snapshot_id: &str,
    model_key: &str,
    variant_key: Option<&str>,
    row: &Map<String, Value>,
) -> Option<FirmwareCandidate> {
    let mut reasons = Vec::new();
    let version = raw_string(row, "Version", &mut reasons);
    let download_url = raw_string(row, "downloadURL", &mut reasons);
    let file_url = raw_string(row, "fileURL", &mut reasons);
    let md5 = raw_string(row, "md5", &mut reasons);
    let nonempty = |value: &Option<String>| value.as_ref().is_some_and(|v| !v.trim().is_empty());
    if !nonempty(&version) && !nonempty(&download_url) && !nonempty(&file_url) {
        return None;
    }

    let checksum = match md5.as_deref() {
        Some(v) if v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()) => {
            ChecksumStatus::Valid
        }
        Some("") | None if !row.get("md5").is_some_and(|v| !v.is_string()) => {
            ChecksumStatus::Missing
        }
        _ => ChecksumStatus::Malformed,
    };
    let artifact_url = if reasons
        .iter()
        .any(|r| matches!(r.as_str(), "invalid_downloadURL" | "invalid_fileURL"))
    {
        None
    } else if nonempty(&download_url) && nonempty(&file_url) && download_url != file_url {
        reasons.push("artifact_urls_disagree".into());
        None
    } else {
        let target = [&download_url, &file_url]
            .into_iter()
            .find(|url| nonempty(url))
            .and_then(Option::as_deref);
        match target.and_then(approved_artifact_url) {
            Some(url) => Some(url.to_string()),
            None => {
                reasons.push(
                    if target.is_some() {
                        "artifact_url_disallowed"
                    } else {
                        "artifact_url_missing"
                    }
                    .into(),
                );
                None
            }
        }
    };
    if !nonempty(&version) {
        reasons.push("version_missing".into());
    }
    if checksum == ChecksumStatus::Malformed {
        reasons.push("checksum_malformed".into());
    }
    if row.contains_key("MiniVersion") || row.contains_key("Prohibited") {
        reasons.push("unsupported_constraints".into());
    }
    if row.keys().any(|key| {
        !matches!(
            key.as_str(),
            "Version"
                | "downloadURL"
                | "fileURL"
                | "md5"
                | "MiniVersion"
                | "Prohibited"
                | "ReleaseNotes"
        )
    }) {
        reasons.push("unknown_record_fields".into());
    }

    // A serialized tuple keeps model and variant boundaries unambiguous even when vendor keys
    // contain delimiters; source and raw manifest digest pin the immutable selected record
    let key = serde_json::to_vec(&(source.id(), snapshot_id, model_key, variant_key)).ok()?;
    let id = hex::encode(Sha256::digest(key));
    Some(FirmwareCandidate {
        id,
        source,
        source_url: source_url.into(),
        snapshot_id: snapshot_id.into(),
        model_key: model_key.into(),
        variant_key: variant_key.map(str::to_owned),
        version,
        download_url,
        file_url,
        artifact_url,
        md5,
        checksum,
        matching: MatchKind::Unmatched,
        comparison: VersionComparison::Unavailable,
        equivalent_to: None,
        reasons,
        raw: Value::Object(row.clone()),
    })
}

/// Reject malformed manifest roots while preserving usable flat and nested siblings
pub(crate) fn parse(
    source: SourceId,
    source_url: &str,
    snapshot_id: &str,
    bytes: &[u8],
) -> Result<ParsedCatalog, String> {
    let root: UniqueValue =
        serde_json::from_slice(bytes).map_err(|_| "Malformed firmware manifest JSON")?;
    let models = root
        .0
        .as_object()
        .filter(|root| root.len() == 1)
        .and_then(|root| root.get("CurrentVersion"))
        .and_then(Value::as_object)
        .ok_or("Unsupported firmware manifest structure")?;
    let mut parsed = ParsedCatalog {
        records: Vec::new(),
        issues: Vec::new(),
    };
    for (model, item) in models {
        let Some(obj) = item.as_object() else {
            parsed
                .issues
                .push(format!("{model}: record is not an object"));
            continue;
        };
        let mut flat = obj.clone();
        if let Some(variants) = flat.remove("variants") {
            if let Some(variants) = variants.as_object() {
                // Only known flat display/artifact fields can safely coexist with
                // nested records without influencing their branch eligibility
                let unresolved_parent = flat.keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "Version" | "downloadURL" | "fileURL" | "md5" | "ReleaseNotes"
                    )
                });
                for (variant, item) in variants {
                    let Some(row) = item.as_object() else {
                        parsed
                            .issues
                            .push(format!("{model}/{variant}: record is not an object"));
                        continue;
                    };
                    let prior = parsed.records.len();
                    push_record(
                        &mut parsed,
                        source,
                        source_url,
                        snapshot_id,
                        model,
                        Some(variant),
                        row,
                    );
                    if unresolved_parent && parsed.records.len() > prior {
                        parsed.records[prior]
                            .reasons
                            .push("unsupported_model_metadata".into());
                    }
                }
            } else {
                parsed
                    .issues
                    .push(format!("{model}: variants is not an object"));
            }
        }
        push_record(
            &mut parsed,
            source,
            source_url,
            snapshot_id,
            model,
            None,
            &flat,
        );
    }
    Ok(parsed)
}

/// Keep unusable records out of discovery and report nonempty malformed entries
fn push_record(
    parsed: &mut ParsedCatalog,
    source: SourceId,
    source_url: &str,
    snapshot_id: &str,
    model: &str,
    variant: Option<&str>,
    row: &Map<String, Value>,
) {
    if let Some(record) = candidate(source, source_url, snapshot_id, model, variant, row) {
        parsed.records.push(record);
    } else if row
        .values()
        .any(|v| !matches!(v, Value::Null) && !v.as_str().is_some_and(|s| s.trim().is_empty()))
    {
        parsed.issues.push(format!(
            "{model}{}: record has no usable version or artifact",
            variant.map(|v| format!("/{v}")).unwrap_or_default()
        ));
    }
}

/// Deserialize JSON objects without silently accepting duplicate selection fields
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(Value::Array(values)))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, UniqueValue(value))) =
                    map.next_entry::<String, UniqueValue>()?
                {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Match only explicitly recorded source-specific keys against Manager's hardware variants
fn identity(
    model: &str,
    variant: &str,
    row: &FirmwareCandidate,
) -> (MatchKind, Option<&'static str>) {
    use MatchKind::{Alias, Ambiguous, Exact, Unmatched};
    use SourceId::{Mirabox, Test};
    let precise = match (model, variant, row.source, row.model_key.as_str()) {
        ("n4-pro", "vsdinside", Mirabox, "VSD_N4Pro") => Some((Exact, "V4.VSD N4 Pro")),
        ("n4-pro", "vsdinside", Mirabox, "VSD-N4Pro")
        | ("n4-pro", "vsdinside", Test, "VSDN4Pro") => Some((Alias, "V4.VSD N4 Pro")),
        ("n4-pro", "mirabox", Mirabox, "MBox-N4Pro") => Some((Exact, "V4.N4 Pro")),
        ("k1-pro", "usb-6603-1015", Mirabox, "K1Pro") => Some((Exact, "V3.K1_PRO")),
        ("k1-pro", "usb-6603-1019", Mirabox, "K1ProEU") => Some((Exact, "V3.K1_PRO_EU")),
        _ => None,
    };
    if let Some((matching, branch)) = precise {
        if row.variant_key.is_none() {
            return (matching, Some(branch));
        }
        // A familiar catalog model object does not identify newly introduced branches
        return (Ambiguous, None);
    }

    // Related catalog entries can be inspected without treating a name or USB ID
    // as evidence for a generation, region, or installed firmware branch
    let nearby = matches!(
        (model, variant, row.model_key.as_str()),
        ("n4-pro", "mirabox", "MBox-N4Pro")
            | ("n4-pro", "mirabox-e", "MBox-N4ProE")
            | ("k1-pro", "usb-6603-1015", "K1Pro")
            | ("k1-pro", "usb-6603-1019", "K1ProEU")
            | ("k1-pro", "usb-5548-1025", "VSDK1Pro")
            | ("n1", _, "MBox-N1" | "MBox-N1E")
            | ("n1-vsd", _, "VSDN1")
            | ("n4", _, "MBox-N4" | "MBox-N4 E")
            | ("m18", _, "M18" | "M18E" | "M18V3_C" | "VSD_M18")
            | ("m3", _, "M3" | "M3E")
            | ("xl", _, "XL" | "VSDXL")
    );
    (if nearby { Ambiguous } else { Unmatched }, None)
}

/// Separate trailing numeric vendor components while preserving the raw prefix
fn qualified_version(label: &str) -> Option<(&str, (u32, u32))> {
    if label.len() > 63
        || label.trim() != label
        || !label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b" ._-".contains(&c))
    {
        return None;
    }
    let (prefix, last) = label.rsplit_once('.')?;
    let (prefix, first) = prefix.rsplit_once('.')?;
    if prefix.split_once('.')?.1.is_empty()
        || !numeric_component(first, 4)
        || !numeric_component(last, 6)
    {
        return None;
    }
    Some((prefix, (first.parse().ok()?, last.parse().ok()?)))
}

/// Exclude Unicode, signs, empty fields and unbounded integers from comparison
fn numeric_component(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_digit())
}

/// Recognize a contradiction only when a filename repeats the version's exact branch
fn filename_contradicts_version(row: &FirmwareCandidate) -> bool {
    let Some((branch, declared)) = row.version.as_deref().and_then(qualified_version) else {
        return false;
    };
    let Some(file) = row
        .artifact_url
        .as_deref()
        .and_then(|url| url.rsplit('/').next())
    else {
        return false;
    };
    let Some((stem, _)) = file.rsplit_once('.') else {
        return false;
    };
    let stem = stem.replace("%20", " ");
    let stem = stem
        .strip_prefix("d13x_")
        .or_else(|| stem.strip_prefix("d21x_"))
        .unwrap_or(&stem);
    qualified_version(stem).is_some_and(|(file_branch, named)| {
        // The VSD K1 feed omits the V in its declared label but not its filename
        file_branch.trim_start_matches('V') == branch.trim_start_matches('V') && named != declared
    })
}

/// Keep an ordinary candidate independent of experimental catalog records
fn comparison_scope(source: SourceId) -> bool {
    source == SourceId::Test
}

/// Evaluate the confirmed device identity and mark conflicting or equivalent provenance
pub(crate) fn evaluate(
    model: &str,
    variant: &str,
    installed: Option<&str>,
    records: Vec<FirmwareCandidate>,
) -> Vec<FirmwareCandidate> {
    let mut relevant: Vec<_> = records
        .into_iter()
        .filter_map(|mut row| {
            let (matching, branch) = identity(model, variant, &row);
            if matching == MatchKind::Unmatched {
                return None;
            }
            row.matching = matching;
            if filename_contradicts_version(&row) {
                row.reasons.push("version_filename_conflict".into());
            }
            if matching == MatchKind::Ambiguous {
                row.reasons.push("unresolved_device_variant".into());
            }
            if let Some(branch) = branch {
                if row.source.standard() && row.reasons.is_empty() {
                    let installed = installed.and_then(qualified_version);
                    let offered = row.version.as_deref().and_then(qualified_version);
                    match (installed, offered) {
                        (Some((a, current)), Some((b, candidate)))
                            if a == branch && b == branch =>
                        {
                            row.comparison = match candidate.cmp(&current) {
                                Ordering::Greater => VersionComparison::Newer,
                                Ordering::Equal => VersionComparison::Equal,
                                Ordering::Less => VersionComparison::Older,
                            };
                        }
                        _ => row
                            .reasons
                            .push("version_family_or_format_unsupported".into()),
                    }
                }
            }
            Some(row)
        })
        .collect();
    reconcile(&mut relevant);
    relevant
}

#[derive(Default)]
struct Agreement<T> {
    first: Option<T>,
    conflict: bool,
}

impl<T: Eq> Agreement<T> {
    /// Detect a disagreement without comparing every pair of records
    fn add(&mut self, value: T) {
        if let Some(first) = &self.first {
            self.conflict |= *first != value;
        } else {
            self.first = Some(value);
        }
    }
}

/// Preserve all raw source references while withholding disputed comparisons
fn reconcile(rows: &mut [FirmwareCandidate]) {
    let mut url_labels = HashMap::<_, Agreement<String>>::new();
    let mut url_checksums = HashMap::<_, Agreement<String>>::new();
    let mut label_targets = HashMap::<_, Agreement<String>>::new();
    for row in rows.iter() {
        if let Some(url) = &row.artifact_url {
            let scope = comparison_scope(row.source);
            if let Some(label) = row.version.as_ref().filter(|v| !v.trim().is_empty()) {
                url_labels
                    .entry((scope, url.clone()))
                    .or_default()
                    .add(label.clone());
                label_targets
                    .entry((scope, row.variant_key.clone(), label.clone()))
                    .or_default()
                    .add(url.clone());
            }
            if row.checksum == ChecksumStatus::Valid {
                if let Some(md5) = &row.md5 {
                    url_checksums
                        .entry((scope, url.clone()))
                        .or_default()
                        .add(md5.to_ascii_lowercase());
                }
            }
        }
    }

    // Prefer a canonical key and a standard source for duplicate display only
    rows.sort_by_key(|row| {
        let priority = match row.source {
            SourceId::Mirabox => 0,
            SourceId::VsdCraft => 1,
            SourceId::Test => 2,
            SourceId::Generic => 3,
        };
        (
            priority,
            row.matching != MatchKind::Exact,
            row.checksum != ChecksumStatus::Valid,
        )
    });
    let mut equivalents = HashMap::new();
    for row in rows {
        let Some(url) = &row.artifact_url else {
            continue;
        };
        let scope = comparison_scope(row.source);
        let url_key = (scope, url.clone());
        if url_labels.get(&url_key).is_some_and(|v| v.conflict) {
            row.reasons.push("target_version_conflict".into());
        }
        if url_checksums.get(&url_key).is_some_and(|v| v.conflict) {
            row.reasons.push("target_checksum_conflict".into());
        }
        if let Some(label) = &row.version {
            let label_key = (scope, row.variant_key.clone(), label.clone());
            if label_targets.get(&label_key).is_some_and(|v| v.conflict) {
                row.reasons.push("version_target_conflict".into());
            }
        }
        if !row.reasons.is_empty() {
            row.comparison = VersionComparison::Unavailable;
        }

        if matches!(row.matching, MatchKind::Exact | MatchKind::Alias)
            && row.reasons.is_empty()
            && row.version.as_ref().is_some_and(|v| !v.is_empty())
            && row.checksum == ChecksumStatus::Valid
        {
            let equivalent_key = (
                scope,
                row.variant_key.clone(),
                url.clone(),
                row.version.clone(),
                row.md5.as_ref().map(|m| m.to_ascii_lowercase()),
            );
            if let Some(original) = equivalents.get(&equivalent_key) {
                row.equivalent_to = Some(String::clone(original));
            } else {
                equivalents.insert(equivalent_key, row.id.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Captured 2026-11-17 catalog excerpts; no fixture downloads firmware binaries
    const VSD_N4_URL: &str =
        "https://cdn1.key123.vip/StreamDock/firmware/download/VSD%20N4%20Pro%2002.010.img";
    const VSD_N4_MD5: &str = "60d9c5ca6919d5b004498af9822e3b8f";

    // Parse a fixture with digest-derived snapshot identity
    fn fixture(source: SourceId, models: Value) -> ParsedCatalog {
        let bytes = serde_json::to_vec(&json!({"CurrentVersion":models})).unwrap();
        let id = hex::encode(Sha256::digest(&bytes));
        parse(source, "https://cdn1.key123.vip/fixture", &id, &bytes).unwrap()
    }

    // Preserve aliases and source references without letting the test source recommend
    #[test]
    fn vsd_n4_pro_aliases_are_checked_only_in_their_observed_source_scope() {
        let row = json!({"Version":"V4.VSD N4 Pro.02.010", "downloadURL":VSD_N4_URL, "fileURL":VSD_N4_URL, "md5":VSD_N4_MD5});
        let mut standard = fixture(
            SourceId::Mirabox,
            json!({
                "VSD_N4Pro":row.clone(), "VSD-N4Pro":row.clone(), "VSDN4Pro":row.clone()
            }),
        )
        .records;
        standard.extend(fixture(SourceId::Test, json!({"VSDN4Pro":row})).records);
        let rows = evaluate(
            "n4-pro",
            "vsdinside",
            Some("V4.VSD N4 Pro.02.009"),
            standard.clone(),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].model_key, "VSD_N4Pro");
        assert_eq!(rows[0].matching, MatchKind::Exact);
        assert_eq!(rows[0].comparison, VersionComparison::Newer);
        assert_eq!(rows[1].matching, MatchKind::Alias);
        assert_eq!(rows[1].equivalent_to.as_ref(), Some(&rows[0].id));
        assert_eq!(rows[2].matching, MatchKind::Alias);
        assert_eq!(rows[2].source, SourceId::Test);
        assert_eq!(rows[2].comparison, VersionComparison::Unavailable);
        assert!(evaluate("n4-pro", "mirabox", Some("V4.N4 Pro.02.010"), standard).is_empty());
    }

    // Keep valid siblings and independent flat/nested paths even with malformed entries
    #[test]
    fn parse_preserves_flat_and_variants_and_rejects_ambiguous_json_members() {
        let parsed = fixture(
            SourceId::Test,
            json!({
               "bad":false, "placeholder":{"Version":"","downloadURL":"","md5":""},
               "MBox-N4Pro":{
                  "Version":"V4.N4 Pro.02.010",
                  "MiniVersion":"unknown parent condition",
                  "variants":{
                    "V4.N4 Pro_HJDF":{
                      "Version":"V4.N4 Pro_HJDF.02.015","Prohibited":[],
                      "downloadURL":"https://cdn1.key123.vip/StreamDock/firmware/download/V4.N4 Pro_HJDF.02.015.img",
                      "Other":{"revision":"X"}
                    },
                    "V4.N4 Pro_HXWY":{"Version":"V4.N4 Pro_HXWY.02.015"},
                    "broken":4
                  }
               },
               "K1W":{"Version":"","downloadURL":"https://cdn1.key123.vip/StreamDock/firmware/download/V3_D.030.01.014.img"}
            }),
        );
        assert_eq!(parsed.records.len(), 4);
        assert_eq!(parsed.issues.len(), 2);
        let rows = evaluate(
            "n4-pro",
            "mirabox",
            Some("V4.N4 Pro.02.010"),
            parsed.records.clone(),
        );
        assert_eq!(rows.len(), 3);
        let branch = rows
            .iter()
            .find(|r| r.variant_key.as_deref() == Some("V4.N4 Pro_HJDF"))
            .unwrap();
        assert_eq!(branch.raw["Other"]["revision"], "X");
        assert!(branch
            .reasons
            .iter()
            .any(|r| r == "unsupported_model_metadata"));
        assert!(branch
            .reasons
            .iter()
            .any(|r| r == "unsupported_constraints"));
        assert_eq!(branch.matching, MatchKind::Ambiguous);
        assert!(rows
            .iter()
            .all(|r| r.comparison == VersionComparison::Unavailable));
        assert!(parsed
            .records
            .iter()
            .any(|r| r.version.as_deref() == Some("") && r.artifact_url.is_some()));
        assert!(parse(
            SourceId::Test,
            "source",
            "digest",
            br#"{"CurrentVersion":{"x":{"Version":"1","Version":"2"}}}"#
        )
        .is_err());
        assert!(parse(
            SourceId::Test,
            "source",
            "digest",
            br#"{"CurrentVersion":[]}"#
        )
        .is_err());
        assert!(parse(SourceId::Test, "source", "digest", b"<html>error").is_err());
    }

    // Retain contradictory source labels and filenames without repairing either
    #[test]
    fn captured_inconsistencies_never_become_comparable() {
        const N1: &str =
            "https://cdn1.key123.vip/StreamDock/firmware/download/d13x_N1-HID_01.011.img";
        let mut rows = fixture(
            SourceId::Mirabox,
            json!({"MBox-N1":{"Version":"V3.N1.00.001","downloadURL":N1}}),
        )
        .records;
        rows.extend(
            fixture(
                SourceId::Generic,
                json!({"MBox-N1":{"Version":"V3.N1.01.001","downloadURL":N1}}),
            )
            .records,
        );
        let actual = evaluate("n1", "usb-6603-1011", Some("V3.N1.00.000"), rows);
        assert_eq!(actual.len(), 2);
        assert!(actual.iter().all(|r| r
            .reasons
            .iter()
            .any(|reason| reason == "target_version_conflict")));
        assert!(actual
            .iter()
            .all(|r| r.comparison == VersionComparison::Unavailable));
        let rows = fixture(SourceId::VsdCraft,json!({"VSDK1Pro":{
            "Version":"3.VSD_K1_PRO.01.001",
            "downloadURL":"https://cdn1.key123.vip/VSDCraft/firmware/download/V3.VSD_K1_PRO.03.004.img"
        }})).records;
        let actual = evaluate(
            "k1-pro",
            "usb-5548-1025",
            Some("V3.VSD_K1_PRO.03.009"),
            rows,
        );
        assert_eq!(actual[0].version.as_deref(), Some("3.VSD_K1_PRO.01.001"));
        assert!(actual[0]
            .reasons
            .iter()
            .any(|r| r == "version_filename_conflict"));
        let rows = fixture(SourceId::Test,json!({"VSD_M18":{"variants":{"V3.VSDM18_HJDF":{
            "Version":"V3.VSDM18_HJDF.02.001",
            "downloadURL":"https://cdn1.key123.vip/StreamDock/firmware/download/V3.VSDM18_HJDF.02.021.img",
            "MiniVersion":"V3.VSDM18_HJDF.02.016","Prohibited":[]
        }}}})).records;
        let actual = evaluate("m18", "unknown", Some("V3.VSDM18_HJDF.02.020"), rows);
        assert_eq!(actual[0].version.as_deref(), Some("V3.VSDM18_HJDF.02.001"));
        assert!(actual[0]
            .reasons
            .iter()
            .any(|r| r == "version_filename_conflict"));
        assert_eq!(actual[0].comparison, VersionComparison::Unavailable);
    }

    // Isolate experimental targets and block conflicts within ordinary provenance
    #[test]
    fn same_label_distinct_targets_are_not_merged_and_test_is_isolated() {
        let differing = "https://cdn1.key123.vip/StreamDock/firmware/download/other.img";
        let rows = fixture(
            SourceId::Mirabox,
            json!({
                "VSD_N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":VSD_N4_URL},
                "VSD-N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":differing}
            }),
        )
        .records;
        let result = evaluate("n4-pro", "vsdinside", Some("V4.VSD N4 Pro.02.009"), rows);
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .all(|r| r.equivalent_to.is_none() && r.comparison == VersionComparison::Unavailable));
        assert!(result.iter().all(|r| r
            .reasons
            .iter()
            .any(|reason| reason == "version_target_conflict")));
        let mut rows = fixture(
            SourceId::Mirabox,
            json!({"VSD_N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":VSD_N4_URL}}),
        )
        .records;
        rows.extend(
            fixture(
                SourceId::Test,
                json!({"VSDN4Pro":{"Version":"V4.VSD N4 Pro.03.001","downloadURL":VSD_N4_URL}}),
            )
            .records,
        );
        let result = evaluate("n4-pro", "vsdinside", Some("V4.VSD N4 Pro.02.010"), rows);
        assert_eq!(result[0].comparison, VersionComparison::Equal);
        assert_eq!(result[1].comparison, VersionComparison::Unavailable);
    }

    // Pin normalized requests to the exact vendor origin and opaque file types
    #[test]
    fn url_and_checksum_validation_keep_raw_values() {
        let raw = "https://cdn1.key123.vip/StreamDock/firmware/download/a b.img";
        assert_eq!(
            approved_artifact_url(raw).unwrap().path(),
            "/StreamDock/firmware/download/a%20b.img"
        );
        for bad in [
            "http://cdn1.key123.vip/StreamDock/firmware/download/a.img",
            "https://cdn1.key123.vip@other.example/StreamDock/firmware/download/a.img",
            "https://cdn1.key123.vip:443/StreamDock/firmware/download/a.img",
            "https://cdn1.key123.vip/StreamDock/firmware/download/../a.img",
            "https://cdn1.key123.vip/StreamDock/firmware/download/a%2Fb.img",
            "https://cdn1.key123.vip/StreamDock/firmware/download/a%252Fb.img",
            "https://cdn1.key123.vip/StreamDock/firmware/download/a.img?key=value",
            "https://cdn1.key123.vip/StreamDock/firmware/download/program.exe",
        ] {
            assert!(approved_artifact_url(bad).is_none(), "{bad}");
        }
        let rows=fixture(SourceId::Mirabox,json!({"K1Pro":{"Version":"V3.K1_PRO.03.009","downloadURL":raw,"fileURL":VSD_N4_URL,"md5":42}})).records;
        assert_eq!(rows[0].download_url.as_deref(), Some(raw));
        assert_eq!(rows[0].checksum, ChecksumStatus::Malformed);
        assert_eq!(rows[0].raw["md5"], 42);
        assert_eq!(rows[0].artifact_url, None);
    }
}
