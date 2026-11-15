//! Window presence is observable; a shared browser helper's per-plugin RSS is not
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use stream_dock_manager_runtime::{diagnostics::Diagnostic, Runtime};
use tauri::Manager;

// Collect for the current operation
pub fn collect(app: &tauri::AppHandle, runtime: &Runtime) -> Value {
    // Do not hold the plugin-host lock while querying native windows
    let owners = {
        let host = runtime.plugins.lock().unwrap();
        host.list()
            .iter()
            .filter_map(|plugin| {
                let mut labels: BTreeSet<String> = host
                    .view_labels(&plugin.id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let expected = host.browser_view(&plugin.id).ok().flatten();
                if let Some(view) = &expected {
                    labels.insert(view.label.clone());
                }
                (!labels.is_empty() || plugin.kind == "browser")
                    .then(|| (plugin.id.clone(), labels, expected.map(|view| view.label)))
            })
            .collect::<Vec<_>>()
    };
    let windows = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("plugin-") || label.starts_with("pi-"))
        .map(|(label, view)| (label, view.is_visible().ok()))
        .collect();
    let mut result = inventory(owners, windows);
    // Raw Wry inspectors are intentionally outside Tauri's webview registry. Count
    // their real native windows separately; never infer owners from window titles.
    result["isolated_inspector_windows"] = json!(app
        .windows()
        .keys()
        .filter(|label| label.starts_with("vendor-inspector-"))
        .count());
    result
}
// Collect the files and identity data for this inventory
fn inventory(
    owners: Vec<(String, BTreeSet<String>, Option<String>)>,
    windows: BTreeMap<String, Option<bool>>,
) -> Value {
    let mut attributed = BTreeSet::new();
    let plugins = owners
        .into_iter()
        .map(|(id, labels, expected)| {
            let observed: Vec<_> = labels
                .iter()
                .filter_map(|label| windows.get(label).map(|visible| (label, visible)))
                .collect();
            for (label, _) in &observed {
                attributed.insert((*label).clone());
            }
            json!({"plugin_id":id,"present_windows":observed.len(),
            "visible_windows":observed.iter().filter(|(_,visible)| **visible==Some(true)).count(),
            "visibility_unknown":observed.iter().filter(|(_,visible)| visible.is_none()).count(),
            "background_expected":expected.is_some(),
            "background_present":expected.is_some_and(|label| windows.contains_key(&label))})
        })
        .collect::<Vec<_>>();
    json!({"scope":"Point-in-time Tauri plugin window presence, not process count or per-plugin memory. Native isolated inspector ownership is not included. Lifecycle reconciliation can overlap sampling.",
        "plugins":plugins,"unattributed_windows":windows.keys().filter(|label| !attributed.contains(*label)).count()})
}
// Build rows describing the active browser views
pub fn rows(value: &Value) -> Vec<Diagnostic> {
    let mut rows = vec![Diagnostic { category:"Browser plugin windows".into(), status:"info".into(), value:format!("{} unattributed Tauri windows; {} isolated inspector windows. Presence only, not per-plugin RAM; shared helper processes may remain after a view closes.",value["unattributed_windows"],value["isolated_inspector_windows"]) }];
    for plugin in value["plugins"].as_array().into_iter().flatten() {
        rows.push(Diagnostic { category: format!("Browser windows: {}",plugin["plugin_id"].as_str().unwrap_or("unknown")), status:"info".into(), value:format!("{} present, {} visible, {} visibility unknown; background expected: {}, present: {}",plugin["present_windows"],plugin["visible_windows"],plugin["visibility_unknown"],plugin["background_expected"],plugin["background_present"]) });
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify exact ownership reports absence unknown visibility and unattributed windows
    fn exact_ownership_reports_absence_unknown_visibility_and_unattributed_windows() {
        let data = inventory(
            vec![
                (
                    "plugin-a".into(),
                    BTreeSet::from(["plugin-secret".into(), "pi-secret".into()]),
                    Some("plugin-secret".into()),
                ),
                (
                    "plugin-b".into(),
                    BTreeSet::from(["plugin-gone".into()]),
                    Some("plugin-gone".into()),
                ),
            ],
            BTreeMap::from([
                ("plugin-secret".into(), Some(false)),
                ("pi-secret".into(), None),
                ("plugin-unmapped".into(), Some(true)),
            ]),
        );
        assert_eq!(data["plugins"][0]["present_windows"], 2);
        assert_eq!(data["plugins"][0]["visible_windows"], 0);
        assert_eq!(data["plugins"][0]["visibility_unknown"], 1);
        assert_eq!(data["plugins"][1]["background_present"], false);
        assert_eq!(data["unattributed_windows"], 1);
        let encoded = data.to_string();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("plugin-unmapped"));
    }
}
