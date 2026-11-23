//! Current per-process accounting is separate from RSS history and alert inputs
use super::{footprint, webkit};
use crate::process_resources::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
pub struct MemoryAccounting {
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub primary_metric: String,
    pub platform: String,
    pub groups: Vec<MemoryGroup>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct MemoryGroup {
    pub kind: String,
    pub complete: bool,
    pub note: String,
    pub rows: Vec<MemoryProcess>,
    #[ts(type = "number | null")]
    pub resident_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub footprint_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct MemoryProcess {
    pub pid: u32,
    #[ts(type = "number")]
    pub created_seconds: u64,
    pub role: String,
    #[ts(type = "number | null")]
    pub resident_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub footprint_bytes: Option<u64>,
    pub note: String,
}

// Sum measured rows without converting failed discovery or unavailable readings into zero
fn group(
    kind: &str,
    rows: Vec<MemoryProcess>,
    discovered: bool,
    mac: bool,
    note: &str,
) -> MemoryGroup {
    let sum = |footprint: bool| {
        let values: Vec<_> = rows
            .iter()
            .filter_map(|r| {
                if footprint {
                    r.footprint_bytes
                } else {
                    r.resident_bytes
                }
            })
            .collect();
        if values.is_empty() && (!rows.is_empty() || !discovered || (footprint && !mac)) {
            None
        } else {
            Some(values.into_iter().fold(0u64, u64::saturating_add))
        }
    };
    MemoryGroup {
        kind: kind.into(),
        complete: discovered
            && rows
                .iter()
                .all(|r| r.resident_bytes.is_some() && (!mac || r.footprint_bytes.is_some())),
        note: note.into(),
        resident_bytes: sum(false),
        footprint_bytes: sum(true),
        rows,
    }
}

// Read each already-attributed PID once and reuse its result across both scope selections
pub(super) fn collect(
    snapshot: &Snapshot,
    combined: &BTreeSet<u32>,
    manager: &BTreeSet<u32>,
    helpers: &webkit::Discovery,
    plugins_complete: bool,
) -> (MemoryAccounting, MemoryAccounting) {
    collect_with(
        snapshot,
        combined,
        manager,
        helpers,
        plugins_complete,
        cfg!(target_os = "macos"),
        |pid, created| {
            footprint::read_with_identity(pid, created, helpers.identities.get(&pid).copied())
        },
    )
}

// Inject metric reads so scope and failure tests never depend on unrelated live processes
fn collect_with(
    snapshot: &Snapshot,
    combined: &BTreeSet<u32>,
    manager: &BTreeSet<u32>,
    helpers: &webkit::Discovery,
    plugins_complete: bool,
    mac: bool,
    mut read: impl FnMut(u32, u64) -> Result<u64, &'static str>,
) -> (MemoryAccounting, MemoryAccounting) {
    let root = std::process::id();
    let mut rows = HashMap::new();
    for &pid in combined {
        let p = snapshot.processes.get(&pid);
        let result = p.map_or(Err("Process is no longer present in the sample"), |p| {
            read(pid, p.created)
        });
        rows.insert(
            pid,
            MemoryProcess {
                pid,
                created_seconds: p.map_or(0, |p| p.created),
                role: if pid == root {
                    "Manager"
                } else if helpers.pids.contains(&pid) {
                    "WebKit helper"
                } else if !manager.contains(&pid) {
                    "Native plugin process"
                } else {
                    "Other helper"
                }
                .into(),
                resident_bytes: if mac && result.is_err() {
                    None
                } else {
                    p.map(|p| p.memory)
                },
                footprint_bytes: result.as_ref().ok().copied(),
                note: result.err().unwrap_or("").into(),
            },
        );
    }
    let build = |include_plugins: bool| {
        let select = |kind: &str| {
            rows.values()
                .filter(|r| match kind {
                    "manager" => r.pid == root,
                    "webkit" => helpers.pids.contains(&r.pid),
                    "native_plugins" => !manager.contains(&r.pid),
                    _ => {
                        r.pid != root && manager.contains(&r.pid) && !helpers.pids.contains(&r.pid)
                    }
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut groups = vec![
            group(
                "manager",
                select("manager"),
                rows.contains_key(&root),
                mac,
                "Single Manager process",
            ),
            group(
                "webkit",
                select("webkit"),
                helpers.complete,
                mac,
                helpers.note,
            ),
            group(
                "other",
                select("other"),
                true,
                mac,
                "Other attributed Manager descendants",
            ),
        ];
        if include_plugins {
            groups.push(group(
                "native_plugins",
                select("native_plugins"),
                plugins_complete,
                mac,
                "Attributed native plugin processes",
            ));
        }
        for g in &mut groups {
            g.rows.sort_by_key(|r| r.pid);
        }
        MemoryAccounting {
            platform: std::env::consts::OS.into(),
            timestamp_ms: snapshot.timestamp_ms,
            primary_metric: if mac {
                "physical_footprint"
            } else {
                "resident"
            }
            .into(),
            groups,
        }
    };
    (build(false), build(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Keep footprint and resident totals distinct even when footprint is larger
    #[test]
    fn metrics_and_partial_subtotals_remain_distinct() {
        let row = MemoryProcess {
            pid: 1,
            created_seconds: 1,
            role: "Manager".into(),
            resident_bytes: Some(100),
            footprint_bytes: Some(300),
            note: String::new(),
        };
        let full = group("manager", vec![row.clone()], true, true, "");
        assert_eq!(full.resident_bytes, Some(100));
        assert_eq!(full.footprint_bytes, Some(300));
        assert!(full.complete);
        let mut missing = row;
        missing.pid = 2;
        missing.footprint_bytes = None;
        let partial = group(
            "webkit",
            vec![full.rows[0].clone(), missing],
            true,
            true,
            "",
        );
        assert!(!partial.complete);
        assert_eq!(partial.footprint_bytes, Some(300));
        assert_eq!(
            group("webkit", vec![], false, true, "").footprint_bytes,
            None
        );
        assert_eq!(
            group("webkit", vec![], true, true, "").footprint_bytes,
            Some(0)
        );
        assert_eq!(
            group("webkit", vec![], true, false, "").footprint_bytes,
            None
        );
    }
    // Sample shared PIDs once while preserving separate Manager, helper and plugin scopes
    #[test]
    fn one_pass_scopes_and_failed_identity() {
        let root = std::process::id();
        let web = root + 1;
        let plugin = root + 2;
        let now = std::time::Instant::now();
        let snapshot = Snapshot {
            processes: [root, web, plugin]
                .into_iter()
                .map(|pid| {
                    (
                        pid,
                        crate::process_resources::Process {
                            parent: None,
                            created: 10,
                            memory: 100,
                            cpu: 0.0,
                        },
                    )
                })
                .collect(),
            at: now,
            previous_at: None,
            timestamp_ms: 99,
        };
        let combined = [root, web, plugin].into_iter().collect();
        let manager = [root, web].into_iter().collect();
        let helpers = webkit::Discovery {
            identities: HashMap::new(),
            pids: [web].into_iter().collect(),
            complete: true,
            note: "verified",
        };
        let mut calls = Vec::new();
        let (own, all) = collect_with(
            &snapshot,
            &combined,
            &manager,
            &helpers,
            true,
            true,
            |pid, _| {
                calls.push(pid);
                if pid == web {
                    Err("identity_mismatch")
                } else {
                    Ok(300)
                }
            },
        );
        assert_eq!(calls.len(), 3);
        assert_eq!(own.groups.iter().flat_map(|g| &g.rows).count(), 2);
        assert_eq!(all.groups.iter().flat_map(|g| &g.rows).count(), 3);
        assert!(!own.groups[1].complete);
        assert_eq!(own.groups[1].rows[0].resident_bytes, None);
        assert_eq!(own.groups[1].rows[0].footprint_bytes, None);
        assert_eq!(all.groups[3].footprint_bytes, Some(300));
        assert_eq!(own.timestamp_ms, 99);
    }
}
