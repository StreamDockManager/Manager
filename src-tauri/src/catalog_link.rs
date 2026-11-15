use crate::editor_lifecycle::{self, EditorWork};
use std::sync::{atomic::Ordering, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_deep_link::DeepLinkExt;

#[derive(Default)]
pub struct PendingCatalogLink(Mutex<Option<(String, String)>>);

// Parse for the current operation
fn parse(url: &str) -> Option<(String, String)> {
    let (channel, id) = url
        .strip_prefix("stream-dock-manager://catalog/")?
        .split_once('/')?;
    if !matches!(channel, "stable" | "development")
        || id.len() >= 160
        || !id.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_alphanumeric()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
    {
        return None;
    }
    Some((channel.into(), id.into()))
}

// Initialize this desktop integration
pub fn init(app: &AppHandle) -> bool {
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        if let Some(link) = event
            .urls()
            .iter()
            .filter_map(|url| parse(url.as_str()))
            .next_back()
        {
            *handle.state::<PendingCatalogLink>().0.lock().unwrap() = Some(link);
            if handle
                .try_state::<std::sync::Arc<stream_dock_manager_runtime::Runtime>>()
                .is_some()
            {
                editor_lifecycle::reveal_with_activation(&handle, None);
            }
        }
    });
    let link = app
        .deep_link()
        .get_current()
        .ok()
        .flatten()
        .and_then(|urls| {
            urls.iter()
                .filter_map(|url| parse(url.as_str()))
                .next_back()
        });
    let state = app.state::<PendingCatalogLink>();
    let mut pending = state.0.lock().unwrap();
    if pending.is_none() {
        *pending = link;
    }
    pending.is_some()
}

// Notify for the current operation
pub fn notify(app: &AppHandle) {
    if app
        .state::<crate::commands::EditorReadiness>()
        .0
        .load(Ordering::SeqCst)
        && app
            .try_state::<PendingCatalogLink>()
            .is_some_and(|state| state.0.lock().unwrap().is_some())
    {
        let _ = app.emit_to("main", "manager-catalog-link", ());
    }
}
// Handle the manager catalog link desktop command

#[tauri::command]
pub fn manager_catalog_link(_editor_work: EditorWork, app: AppHandle) -> Option<(String, String)> {
    if !app
        .state::<crate::commands::EditorReadiness>()
        .0
        .load(Ordering::SeqCst)
    {
        return None;
    }
    app.state::<PendingCatalogLink>().0.lock().unwrap().clone()
}
// Handle the acknowledge manager catalog link desktop command

#[tauri::command]
pub fn acknowledge_manager_catalog_link(
    _editor_work: EditorWork,
    app: AppHandle,
    channel: String,
    plugin_id: String,
) {
    let state = app.state::<PendingCatalogLink>();
    let mut link = state.0.lock().unwrap();
    if link.as_ref().is_some_and(|(saved_channel, saved_id)| {
        saved_channel == &channel && saved_id == &plugin_id
    }) {
        *link = None;
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    // Verify only accepts catalog ID and channel
    fn only_accepts_catalog_id_and_channel() {
        assert!(parse(
            "stream-dock-manager://catalog/stable/app.streamdockmanager.plugins.weather"
        )
        .is_some());
        for url in [
            "https://catalog/stable/app.streamdockmanager.plugins.weather",
            "stream-dock-manager://catalog/development/app.streamdockmanager.plugins.weather?url=https://example.com",
            "stream-dock-manager://catalog/beta/app.streamdockmanager.plugins.weather",
            "stream-dock-manager://catalog/stable/org..weather",
            "stream-dock-manager://catalog/stable/app.streamdockmanager.plugins.weather#install",
            "stream-dock-manager://catalog/stable/app.streamdockmanager.plugins.%77eather",
        ] {
            assert!(parse(url).is_none(), "{url}");
        }
    }
}
