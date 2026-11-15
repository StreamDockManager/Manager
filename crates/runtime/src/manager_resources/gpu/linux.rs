//! https://docs.kernel.org/gpu/drm-usage-stats.html
use super::Counter;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};
// Parse the supplied Linux representation
fn parse(text: &str) -> BTreeMap<String, Counter> {
    let fields: BTreeMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once(':').map(|(k, v)| (k.trim(), v.trim())))
        .collect();
    let Some(client) = fields
        .get("drm-client-id")
        .filter(|v| v.parse::<u64>().is_ok())
    else {
        return BTreeMap::new();
    };
    let Some(driver) = fields.get("drm-driver") else {
        return BTreeMap::new();
    };
    let device = format!("{driver}:{}", fields.get("drm-pdev").unwrap_or(&"global"));
    let mut result = BTreeMap::new();
    for (key, value) in &fields {
        let Some(engine) = key
            .strip_prefix("drm-engine-")
            .filter(|s| !s.starts_with("capacity-"))
        else {
            continue;
        };
        let mut parts = value.split_whitespace();
        let Some(nanos) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        if parts.next() != Some("ns") {
            continue;
        }
        let capacity = fields
            .get(format!("drm-engine-capacity-{engine}").as_str())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1);
        if capacity == 0 {
            continue;
        }
        result.insert(
            format!("{device}:{client}:{engine}"),
            Counter {
                engine: format!("{device}:{engine}"),
                nanos,
                capacity,
            },
        );
    }
    result
}
// Keep ownership until a scope is selected: a client shared with another
// process must remain visible in either scope, but count only once in their union
#[derive(Default)]
pub(super) struct Snapshot {
    processes: BTreeMap<u32, BTreeMap<String, Counter>>,
}
impl Snapshot {
    // Collect the Linux values
    pub(super) fn collect(pids: &BTreeSet<u32>) -> Self {
        Self::collect_with(pids, process_counters)
    }

    // Collect with
    pub(super) fn collect_with(
        pids: &BTreeSet<u32>,
        mut collect: impl FnMut(u32) -> BTreeMap<String, Counter>,
    ) -> Self {
        Self {
            processes: pids.iter().map(|pid| (*pid, collect(*pid))).collect(),
        }
    }

    // Merge cached GPU counters only for the selected process IDs
    pub(super) fn counters(&self, pids: &BTreeSet<u32>) -> BTreeMap<String, Counter> {
        let mut result = BTreeMap::new();
        for pid in pids {
            if let Some(counters) = self.processes.get(pid) {
                merge(&mut result, counters.clone());
            }
        }
        result
    }
}

// Merge the supplied Linux values
fn merge(result: &mut BTreeMap<String, Counter>, counters: BTreeMap<String, Counter>) {
    for (key, counter) in counters {
        // Duplicated/shared file descriptors refer to one DRM client
        result
            .entry(key)
            .and_modify(|old| old.nanos = old.nanos.max(counter.nanos))
            .or_insert(counter);
    }
}

// Read bounded Linux fdinfo entries and collect recognized GPU engine counters
fn process_counters(pid: u32) -> BTreeMap<String, Counter> {
    let mut result = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fdinfo")) else {
        return result;
    };
    for entry in entries.take(1024).flatten() {
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut text = String::new();
        if file.take(65536).read_to_string(&mut text).is_err() {
            continue;
        }
        merge(&mut result, parse(&text));
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that overlapping scopes scan each PID once and deduplicate shared clients
    fn overlapping_scopes_scan_each_pid_once_and_deduplicate_shared_clients() {
        let manager = BTreeSet::from([10]);
        let combined = BTreeSet::from([10, 20]);
        let mut calls = Vec::new();
        let snapshot = Snapshot::collect_with(&combined, |pid| {
            calls.push(pid);
            let mut counters = parse(&format!(
                "drm-driver: i915\ndrm-client-id: 1\ndrm-engine-render: {} ns\n",
                if pid == 10 { 100 } else { 120 }
            ));
            if pid == 20 {
                merge(
                    &mut counters,
                    parse("drm-driver: i915\ndrm-client-id: 2\ndrm-engine-render: 50 ns\n"),
                );
            }
            counters
        });
        let owned = snapshot.counters(&manager);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned.values().next().unwrap().nanos, 100);
        let all = snapshot.counters(&combined);
        assert_eq!(all.len(), 2);
        assert_eq!(all.values().map(|counter| counter.nanos).sum::<u64>(), 170);
        assert_eq!(snapshot.counters(&BTreeSet::from([20])).len(), 2);
        assert!(snapshot.counters(&BTreeSet::from([30])).is_empty());
        assert!(snapshot.counters(&BTreeSet::new()).is_empty());
        assert_eq!(calls, [10, 20]);
    }

    #[test]
    // Verify that duplicated descriptors retain highest counter
    fn duplicated_descriptors_retain_highest_counter() {
        let mut result = parse("drm-driver: i915\ndrm-client-id: 1\ndrm-engine-render: 100 ns\n");
        merge(
            &mut result,
            parse("drm-driver: i915\ndrm-client-id: 1\ndrm-engine-render: 90 ns\n"),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result.values().next().unwrap().nanos, 100);
    }

    #[test]
    // Verify that fdinfo scopes clients to devices and parses capacity and units
    fn fdinfo_scopes_clients_to_devices_and_parses_capacity_and_units() {
        let parsed=parse("drm-driver: i915\ndrm-pdev: 0000:00:02.0\ndrm-client-id: 12\ndrm-engine-render: 200000000 ns\ndrm-engine-capacity-render: 2\ndrm-engine-copy: invalid ns\ndrm-engine-video: 50 cycles\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.values().next().unwrap().capacity, 2);
        assert_eq!(parsed.values().next().unwrap().nanos, 200000000);
        assert!(parse("drm-engine-render: 50 ns").is_empty());
    }
}
