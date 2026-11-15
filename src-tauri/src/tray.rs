use crate::commands::EditorReadiness;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use stream_dock_manager_runtime::{Runtime, Snapshot};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

#[derive(Debug, PartialEq, Eq)]
struct DeviceEntry {
    id: String,
    label: String,
    sleeping: bool,
    supports_sleep: bool,
    epoch: u64,
    profiles: Vec<ProfileEntry>,
}
#[derive(Debug, PartialEq, Eq)]
struct ProfileEntry {
    id: String,
    label: String,
    active: bool,
    pages: Vec<(String, String, bool)>,
}

// Build the device entries for the tray menu
fn devices(snapshot: &Snapshot) -> Vec<DeviceEntry> {
    let mut connected: Vec<_> = snapshot.devices.iter().filter(|d| d.connected).collect();
    connected.sort_by_key(|d| (d.name.to_lowercase(), &d.id));
    let mut names = std::collections::BTreeMap::new();
    let mut entries: Vec<_> = connected
        .into_iter()
        .map(|device| {
            let label = if device.simulated && !device.name.contains("Simulator") {
                format!("{} (Simulator)", device.name)
            } else {
                device.name.clone()
            };
            *names.entry(label.clone()).or_insert(0) += 1;
            DeviceEntry {
                epoch: 0,
                profiles: snapshot
                    .profiles
                    .iter()
                    .filter(|profile| profile.model_id == device.model_id)
                    .map(|profile| {
                        let session = snapshot.session(&device.id);
                        ProfileEntry {
                            id: profile.id.clone(),
                            label: profile.name.clone(),
                            active: session.profile_id == profile.id,
                            pages: profile
                                .pages
                                .iter()
                                .map(|page| {
                                    (
                                        page.id.clone(),
                                        page.name.clone(),
                                        session.profile_id == profile.id
                                            && session.page_id == page.id,
                                    )
                                })
                                .collect(),
                        }
                    })
                    .collect(),
                id: device.id.clone(),
                label,
                sleeping: snapshot.session(&device.id).output.sleeping,
                supports_sleep: device.manifest.capabilities.sleep,
            }
        })
        .collect();
    let mut numbered = std::collections::BTreeMap::new();
    for entry in &mut entries {
        if names[&entry.label] > 1 {
            let index = numbered.entry(entry.label.clone()).or_insert(0);
            *index += 1;
            entry.label = format!("{} ({index})", entry.label);
        }
    }
    entries
}

// Read the devices currently represented in the tray
fn current_devices(runtime: &Runtime) -> Vec<DeviceEntry> {
    let mut entries = devices(&runtime.snapshot());
    for entry in &mut entries {
        entry.epoch = runtime.connection_generation(&entry.id).unwrap_or_default();
        if let Ok(output) = runtime.effective_device_output(&entry.id) {
            entry.sleeping = output.sleeping;
        }
    }
    entries
}

struct TrayMenu {
    menu: Menu<tauri::Wry>,
    devices: Mutex<Option<(Vec<DeviceEntry>, bool)>>,
    refresh_pending: AtomicBool,
    pending_editor: Mutex<Option<String>>,
}

impl TrayMenu {
    // Update for the current operation
    fn update(&self, app: &AppHandle, devices: Vec<DeviceEntry>) -> tauri::Result<()> {
        let mut previous = self.devices.lock().unwrap();
        let recovery = app.state::<Arc<Runtime>>().recovery_status().active;
        if previous
            .as_ref()
            .is_some_and(|(old, active)| old == &devices && *active == recovery)
        {
            return Ok(());
        }
        let mut items = vec![
            MenuItemKind::MenuItem(MenuItem::with_id(
                app,
                "show",
                "Open Stream Dock Manager",
                true,
                None::<&str>,
            )?),
            MenuItemKind::Predefined(PredefinedMenuItem::separator(app)?),
        ];
        if devices.is_empty() {
            items.push(MenuItemKind::MenuItem(MenuItem::with_id(
                app,
                "tray:no-devices",
                "No devices connected",
                false,
                None::<&str>,
            )?));
        }
        for device in &devices {
            let open = MenuItem::with_id(
                app,
                format!("tray:open:{}", device.id),
                "Open device editor",
                true,
                None::<&str>,
            )?;
            // The ID records the requested state so a stale menu click cannot invert a newer state
            let (action, label) = if !device.supports_sleep {
                ("sleep", "Sleep unavailable")
            } else if device.sleeping {
                ("wake", "Wake device")
            } else {
                ("sleep", "Sleep device")
            };
            let sleep = MenuItem::with_id(
                app,
                format!("tray:{action}:{}", device.id),
                label,
                device.supports_sleep,
                None::<&str>,
            )?;
            let status = if device.sleeping { "Sleeping" } else { "Awake" };
            let submenu = Submenu::with_id_and_items(
                app,
                format!("tray:device:{}", device.id),
                format!("{} · {status}", device.label.replace('&', "&&")),
                true,
                &[&open, &sleep],
            )?;
            submenu.append(&PredefinedMenuItem::separator(app)?)?;
            for profile in &device.profiles {
                let profile_menu = Submenu::new(app, profile.label.replace('&', "&&"), true)?;
                let select_id = |page: Option<&str>| {
                    format!(
                        "tray:select:{}",
                        serde_json::to_string(&(&device.id, device.epoch, &profile.id, page))
                            .unwrap()
                    )
                };
                profile_menu.append(&CheckMenuItem::with_id(
                    app,
                    select_id(None),
                    "Use profile",
                    true,
                    profile.active,
                    None::<&str>,
                )?)?;
                for (id, name, active) in &profile.pages {
                    profile_menu.append(&CheckMenuItem::with_id(
                        app,
                        select_id(Some(id)),
                        name.replace('&', "&&"),
                        true,
                        *active,
                        None::<&str>,
                    )?)?;
                }
                submenu.append(&profile_menu)?;
            }
            items.push(MenuItemKind::Submenu(submenu));
        }
        items.push(MenuItemKind::Predefined(PredefinedMenuItem::separator(
            app,
        )?));
        items.push(MenuItemKind::MenuItem(MenuItem::with_id(
            app,
            if recovery {
                "recovery-exit"
            } else {
                "recovery-restart"
            },
            if recovery {
                "Exit recovery mode"
            } else {
                "Restart in recovery mode"
            },
            true,
            None::<&str>,
        )?));
        items.push(MenuItemKind::MenuItem(MenuItem::with_id(
            app,
            "quit",
            "Quit",
            true,
            None::<&str>,
        )?));
        // Keep the attached menu object and mutate its contents for Linux tray compatibility
        for item in self.menu.items()? {
            self.menu.remove(&item)?;
        }
        for item in items {
            self.menu.append(&item)?;
        }
        *previous = Some((devices, recovery));
        Ok(())
    }
}

// Install for the current operation
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let state = TrayMenu {
        menu: Menu::new(app)?,
        devices: Mutex::new(None),
        refresh_pending: AtomicBool::new(false),
        pending_editor: Mutex::new(None),
    };
    state.update(app, current_devices(&app.state::<Arc<Runtime>>()))?;
    let menu = state.menu.clone();
    app.manage(state);
    TrayIconBuilder::with_id("stream-dock-manager")
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(false)
        .tooltip("Stream Dock Manager")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .build(app)?;
    Ok(())
}

// Refresh for the current operation
pub fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayMenu>() else {
        return;
    };
    if state.refresh_pending.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        let state = handle.state::<TrayMenu>();
        state.refresh_pending.store(false, Ordering::SeqCst);
        let entries = current_devices(&handle.state::<Arc<Runtime>>());
        if let Err(error) = state.update(&handle, entries) {
            crate::manager_log::warning("Could not update the device tray menu");
            eprintln!("Could not update the device tray menu: {error}");
        }
    }) {
        state.refresh_pending.store(false, Ordering::SeqCst);
        crate::manager_log::warning("Could not schedule the device tray menu update");
        eprintln!("Could not schedule the device tray menu update: {error}");
    }
}

// Check whether this operation needs refresh
pub(crate) fn needs_refresh(kind: &str) -> bool {
    matches!(
        kind,
        "devices" | "device-output" | "device-power" | "profiles" | "sessions" | "plugins"
    )
}

// Show manager
fn show_manager(app: &AppHandle) {
    crate::editor_lifecycle::reveal(app);
}

// Retain an early click until the frontend has installed its native event listeners
pub fn deliver_editor_request(app: &AppHandle) {
    if !app.state::<EditorReadiness>().0.load(Ordering::SeqCst) {
        return;
    }
    let Some(state) = app.try_state::<TrayMenu>() else {
        return;
    };
    let mut pending = state.pending_editor.lock().unwrap();
    if let (Some(id), Some(window)) = (pending.as_ref(), app.get_webview_window("main")) {
        if window.emit("tray-open-device", id).is_ok() {
            *pending = None;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DeviceAction<'a> {
    Open(&'a str),
    Sleep(&'a str, bool),
}
// Handle the selected device tray action
fn device_action(id: &str) -> Option<DeviceAction<'_>> {
    for (prefix, sleeping) in [("tray:sleep:", true), ("tray:wake:", false)] {
        if let Some(id) = id.strip_prefix(prefix).filter(|id| !id.is_empty()) {
            return Some(DeviceAction::Sleep(id, sleeping));
        }
    }
    id.strip_prefix("tray:open:")
        .filter(|id| !id.is_empty())
        .map(DeviceAction::Open)
}

// Show error
fn show_error(app: &AppHandle, message: String) {
    app.dialog()
        .message(message)
        .title("Stream Dock Manager")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

// Handle menu event
fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    if let Some(selection) = event.id.as_ref().strip_prefix("tray:select:") {
        if let Ok((device, epoch, profile, page)) =
            serde_json::from_str::<(String, u64, String, Option<String>)>(selection)
        {
            let handle = app.clone();
            let runtime = app.state::<Arc<Runtime>>().inner().clone();
            std::thread::spawn(move || {
                if let Err(error) =
                    runtime.select_from_tray(&device, epoch, &profile, page.as_deref())
                {
                    show_error(
                        &handle,
                        format!("Could not change the device selection: {error}"),
                    );
                }
                refresh(&handle);
            });
        }
        return;
    }
    match event.id.as_ref() {
        "show" => show_manager(app),
        "recovery-restart" => {
            crate::editor_lifecycle::request_restart(app, true);
        }
        "recovery-exit" => crate::editor_lifecycle::request_restart(app, false),
        "quit" => crate::editor_lifecycle::request_quit(app),
        id => match device_action(id) {
            Some(DeviceAction::Open(id)) => {
                *app.state::<TrayMenu>().pending_editor.lock().unwrap() = Some(id.into());
                show_manager(app);
                deliver_editor_request(app);
            }
            Some(DeviceAction::Sleep(id, sleeping)) => {
                let id = id.to_owned();
                let handle = app.clone();
                let runtime = app.state::<Arc<Runtime>>().inner().clone();
                std::thread::spawn(move || {
                    if let Err(error) = runtime.set_device_sleep(&id, sleeping) {
                        crate::manager_log::warning("Could not change device sleep from the tray");
                        let action = if sleeping { "sleep" } else { "wake" };
                        show_error(&handle, format!("Could not {action} the device: {error}"));
                    }
                    refresh(&handle);
                });
            }
            None => {}
        },
    }
}

#[cfg(test)]
mod tests;
