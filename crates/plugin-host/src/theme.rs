use super::*;
use base64::Engine;
#[derive(serde::Serialize, ts_rs::TS)]
pub struct AppearancePreferences {
    pub configured: bool,
    pub preference: String,
    pub system_dark: bool,
    pub style: UiThemeStyle,
    pub custom_palettes: Vec<CustomPalette>,
    pub device_theme: DeviceThemePreferences,
    pub profile_palette: Option<ProfilePalette>,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePaletteTarget {
    #[default]
    Global,
    Device,
    Manager,
    Both,
}

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilePalette {
    pub target: ProfilePaletteTarget,
    pub light_palette: String,
    pub dark_palette: String,
}

impl Default for ProfilePalette {
    // Construct the default value
    fn default() -> Self {
        Self {
            target: ProfilePaletteTarget::Global,
            light_palette: "default".into(),
            dark_palette: "default".into(),
        }
    }
}

impl ProfilePalette {
    // Report whether this profile palette controls device colors
    pub fn syncs_device(&self) -> bool {
        matches!(
            self.target,
            ProfilePaletteTarget::Device | ProfilePaletteTarget::Both
        )
    }
    // Report whether this profile palette controls Manager colors
    pub fn syncs_manager(&self) -> bool {
        matches!(
            self.target,
            ProfilePaletteTarget::Manager | ProfilePaletteTarget::Both
        )
    }
    // Validate for the current operation
    pub fn validate(&self) -> Result<(), String> {
        for (mode, id) in [("light", &self.light_palette), ("dark", &self.dark_palette)] {
            // Custom palettes are installation-owned. Preserve portable references;
            // a missing custom palette resolves to Default on the receiving computer.
            if id != "default"
                && palettes()["managerSchemes"][id]["mode"] != mode
                && uuid::Uuid::parse_str(id).is_err()
            {
                return Err(format!("Invalid {mode} profile palette"));
            }
        }
        Ok(())
    }
    // Select the light or dark palette identifier
    fn id(&self, mode: &str) -> &str {
        if mode == "light" {
            &self.light_palette
        } else {
            &self.dark_palette
        }
    }
}
const PALETTE_LIMIT: usize = 64;
const PALETTE_RECORD_LIMIT: usize = 8 * 1024 * 1024;
pub(super) const THEME_RPC: &str = "plugin-theme-v1";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CustomPalette {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub colors: std::collections::BTreeMap<String, String>,
    pub wallpaper: Option<PaletteWallpaper>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct PaletteWallpaper {
    pub asset_id: String,
    pub format: String,
    pub animated: bool,
    pub options: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub svg: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceThemePreferences {
    pub sync: bool,
    pub transparent_buttons: bool,
}

impl Default for DeviceThemePreferences {
    // Construct the default value
    fn default() -> Self {
        Self {
            sync: true,
            transparent_buttons: true,
        }
    }
}

impl CustomPalette {
    // Validate for the current operation
    fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.id).is_err()
            || self.name.trim().is_empty()
            || self.name.len() > 96
            || self.name.chars().any(char::is_control)
            || !["light", "dark"].contains(&self.mode.as_str())
            || self.colors.iter().any(|(key, color)| {
                !palettes()["themes"][&self.mode][key]
                    .as_str()
                    .is_some_and(|value| value.starts_with('#'))
                    && !["danger-text", "toast-error", "meter"].contains(&key.as_str())
                    || !valid_color(color)
            })
        {
            return Err("Enter a palette name and valid six-digit hex colors".into());
        }
        if let Some(wallpaper) = &self.wallpaper {
            if uuid::Uuid::parse_str(&wallpaper.asset_id).is_err()
                || !["png", "jpeg", "svg", "gif", "mp4"].contains(&wallpaper.format.as_str())
                || wallpaper.svg.as_ref().is_some_and(|svg| {
                    wallpaper.format != "svg"
                        || svg.len() > 64 * 1024
                        || hardware_core::render::svg_media_dimensions(svg.as_bytes()).is_err()
                })
            {
                return Err("Invalid palette wallpaper".into());
            }
        }
        Ok(())
    }
}

// Check for a six-digit hexadecimal RGB palette color
fn valid_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

// Load custom palettes
pub(super) fn load_custom_palettes(
    conn: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<Vec<CustomPalette>, String> {
    let palettes: Vec<CustomPalette> = stream_dock_manager_storage::read_json(
        conn,
        stream_dock_manager_storage::Domain::PluginMetadata,
        "custom_palettes",
        PALETTE_RECORD_LIMIT,
    )?
    .unwrap_or_default();
    let mut ids = HashSet::new();
    if palettes.len() > PALETTE_LIMIT || palettes.iter().any(|palette| !ids.insert(&palette.id)) {
        return Err("Invalid custom palette inventory".into());
    }
    for palette in &palettes {
        palette.validate()?;
    }
    Ok(palettes)
}

// Collect media referenced by configured palettes
pub fn palette_media_references(
    conn: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<Vec<(String, String)>, String> {
    // Read only the reference fields. Full palette validation runs when the host loads
    let palettes: Vec<CustomPalette> = stream_dock_manager_storage::read_json(
        conn,
        stream_dock_manager_storage::Domain::PluginMetadata,
        "custom_palettes",
        PALETTE_RECORD_LIMIT,
    )?
    .unwrap_or_default();
    Ok(palettes
        .into_iter()
        .filter_map(|palette| Some((palette.wallpaper?.asset_id, palette.name)))
        .collect())
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiThemeStyle {
    pub light_palette: String,
    pub dark_palette: String,
    pub font: String,
    pub text_scale: u16,
}
impl Default for UiThemeStyle {
    // Construct the default value
    fn default() -> Self {
        Self {
            light_palette: "default".into(),
            dark_palette: "default".into(),
            font: "default".into(),
            text_scale: 100,
        }
    }
}
impl UiThemeStyle {
    // Validate for the current operation
    pub(super) fn validate(&self, custom: &[CustomPalette]) -> Result<(), String> {
        let matches_mode = |id: &str, mode: &str| {
            id == "default"
                || palettes()["managerSchemes"][id]["mode"] == mode
                || custom
                    .iter()
                    .any(|palette| palette.id == id && palette.mode == mode)
        };
        if !matches_mode(&self.light_palette, "light")
            || !matches_mode(&self.dark_palette, "dark")
            || !(self.font == "default" || hardware_core::fonts::valid(&self.font))
            || !(90..=150).contains(&self.text_scale)
            || !self.text_scale.is_multiple_of(5)
        {
            return Err("Invalid interface theme settings".into());
        }
        Ok(())
    }
}
// Read the available Manager palettes
fn palettes() -> &'static Value {
    static PALETTE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    PALETTE.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/palette.v1.json"))
            .expect("checked-in palette")
    })
}
// Read the available device palettes
fn device_palettes() -> &'static Value {
    static PALETTES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    PALETTES.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/device-palettes.v1.json"))
            .expect("checked-in device palettes")
    })
}

// Select a device palette by mode and identifier
pub(super) fn device_palette(mode: &str, palette: &str) -> &'static Value {
    let palettes = &device_palettes()["palettes"][mode];
    palettes.get(palette).unwrap_or(&palettes["default"])
}

// Select the palette variant for this appearance
pub(super) fn variant(root: &Path, source: &str, mode: &str, palette: &str) -> Option<String> {
    if source.contains(['\\', ':'])
        || !Path::new(source)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let (stem, extension) = source.rsplit_once('.')?;
    if !matches!(extension, "svg" | "json") {
        return None;
    }
    let stem = stem
        .strip_suffix("-light")
        .or_else(|| stem.strip_suffix("-dark"))
        .unwrap_or(stem);
    let name = format!("palettes/{mode}/{palette}/{stem}.{extension}");
    safe_file(root, &name).ok()?;
    Some(name)
}
// Resolve the themed icon for the requested appearance
pub(super) fn theme_icon(root: &Path, asset: &ThemedAsset, theme: &str) -> Option<String> {
    let source = if theme == "light" {
        &asset.light
    } else {
        &asset.dark
    };
    let path = safe_file(root, source).ok()?;
    if std::fs::metadata(&path).ok()?.len() > 256 * 1024 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let mime = if source.ends_with(".svg") {
        "image/svg+xml"
    } else if source.ends_with(".jpg") || source.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD
            .encode(hardware_core::icons::transparent_svg(&data))
    ))
}
/// Revision-based wakeup avoids both lost preference changes and idle subprocess probes
#[derive(Default)]
pub(super) struct ObserverSignal {
    revision: Mutex<u64>,
    changed: std::sync::Condvar,
}
impl ObserverSignal {
    // Wake for the current operation
    pub fn wake(&self) {
        let mut revision = self.revision.lock().unwrap();
        *revision = revision.wrapping_add(1);
        self.changed.notify_all();
    }
    // Wait for the current operation
    fn wait(&self, revision: u64, demand: bool, stop: &AtomicBool) {
        let current = self.revision.lock().unwrap();
        let unchanged = |value: &mut u64| *value == revision && !stop.load(Ordering::SeqCst);
        if demand {
            drop(
                self.changed
                    .wait_timeout_while(current, Duration::from_secs(2), unchanged)
                    .unwrap(),
            );
        } else {
            drop(self.changed.wait_while(current, unchanged).unwrap());
        }
    }
}
// Refresh system
pub(super) fn refresh_system(hub: &Arc<Mutex<Hub>>, probe: impl FnOnce() -> Option<bool>) {
    if !hub.lock().unwrap().follows_system_theme() {
        return;
    }
    // OS tools may be slow; never retain the hub lock during a probe
    if let Some(dark) = probe() {
        let mut h = hub.lock().unwrap();
        if h.follows_system_theme() && h.system_dark != dark {
            h.system_dark = dark;
            h.notify_themes();
            let _ = h.persist();
        }
    }
}
// Observe
pub(super) fn observe(hub: Arc<Mutex<Hub>>, stop: Arc<AtomicBool>) {
    let signal = hub.lock().unwrap().theme_observer.clone();
    while !stop.load(Ordering::SeqCst) {
        let revision = *signal.revision.lock().unwrap();
        let demand = hub.lock().unwrap().follows_system_theme();
        signal.wait(revision, demand, &stop);
        if !stop.load(Ordering::SeqCst) {
            refresh_system(&hub, system_appearance);
        }
    }
}
impl Hub {
    // Check whether UI appearance or synchronized palettes depend on the system theme
    pub(super) fn follows_system_theme(&self) -> bool {
        !matches!(self.ui_appearance.as_str(), "light" | "dark")
            || self
                .profile_palettes
                .values()
                .any(|palette| palette.syncs_device() || palette.syncs_manager())
    }

    // Resolve the global light or dark UI mode
    fn global_ui_theme(&self) -> &'static str {
        match self.ui_appearance.as_str() {
            "light" => "light",
            "dark" => "dark",
            _ if self.system_dark => "dark",
            _ => "light",
        }
    }

    // Find the profile palette that controls Manager colors
    fn manager_profile_palette(&self) -> Option<&ProfilePalette> {
        self.profile_palettes
            .get(&self.manager_theme_device)
            .filter(|palette| palette.syncs_manager())
    }

    // Build the effective UI theme
    fn resolved_ui_theme(&self) -> &'static str {
        if self.manager_profile_palette().is_some() {
            if self.system_dark {
                "dark"
            } else {
                "light"
            }
        } else {
            self.global_ui_theme()
        }
    }

    // Choose an available palette identifier
    fn available_palette_id<'a>(&self, mode: &str, id: &'a str) -> &'a str {
        if id == "default"
            || palettes()["managerSchemes"][id]["mode"] == mode
            || self
                .custom_palettes
                .iter()
                .any(|palette| palette.id == id && palette.mode == mode)
        {
            id
        } else {
            "default"
        }
    }

    // Resolve the palette identifier used by Manager
    fn manager_palette_id(&self, mode: &str) -> &str {
        self.manager_profile_palette().map_or_else(
            || self.palette_id(mode),
            |palette| self.available_palette_id(mode, palette.id(mode)),
        )
    }

    // Resolve the appearance mode for the selected device
    pub(super) fn device_mode_for(&self, device: &str) -> &'static str {
        if self
            .profile_palettes
            .get(device)
            .is_some_and(ProfilePalette::syncs_device)
        {
            if self.system_dark {
                "dark"
            } else {
                "light"
            }
        } else {
            self.device_mode()
        }
    }

    // Resolve the palette identifier for the selected device
    pub(super) fn device_palette_id_for(&self, mode: &str, device: &str) -> &str {
        self.profile_palettes
            .get(device)
            .filter(|palette| palette.syncs_device())
            .map_or_else(
                || self.device_palette_id(mode),
                |palette| self.available_palette_id(mode, palette.id(mode)),
            )
    }

    // Resolve colors for the selected device
    pub(super) fn device_colors_for(&self, mode: &str, device: &str) -> Value {
        self.colors_for_palette(mode, self.device_palette_id_for(mode, device))
    }

    // Resolve wallpaper for the selected device
    pub(super) fn device_wallpaper_for(
        &self,
        mode: &str,
        device: &str,
    ) -> Option<&PaletteWallpaper> {
        self.custom_palette(self.device_palette_id_for(mode, device))?
            .wallpaper
            .as_ref()
    }

    // Build theme settings for the selected device
    pub(super) fn device_theme_for(&self, device: &str) -> DeviceThemePreferences {
        DeviceThemePreferences {
            sync: self.device_theme.sync
                || self
                    .profile_palettes
                    .get(device)
                    .is_some_and(ProfilePalette::syncs_device),
            ..self.device_theme
        }
    }

    // Build the effective theme for the selected device
    pub(super) fn resolved_device_theme_for(&self, id: &str, device: &str) -> &'static str {
        match self.appearance.get(id).map(String::as_str) {
            Some("light") => "light",
            Some("dark") => "dark",
            _ => self.device_mode_for(device),
        }
    }

    // Read the configured palette identifier for the appearance mode
    pub(super) fn palette_id(&self, mode: &str) -> &str {
        if mode == "light" {
            &self.ui_theme_style.light_palette
        } else {
            &self.ui_theme_style.dark_palette
        }
    }

    // Read the configured device palette identifier
    pub(super) fn device_palette_id(&self, mode: &str) -> &str {
        if self.device_theme.sync {
            self.palette_id(mode)
        } else {
            "default"
        }
    }

    // Find the custom palette with this identifier
    fn custom_palette(&self, id: &str) -> Option<&CustomPalette> {
        self.custom_palettes.iter().find(|palette| palette.id == id)
    }

    // Resolve the current device colors
    pub(super) fn device_colors(&self, mode: &str) -> Value {
        let id = self.device_palette_id(mode);
        self.colors_for_palette(mode, id)
    }

    // Combine built-in colors with the selected custom palette
    fn colors_for_palette(&self, mode: &str, id: &str) -> Value {
        let mut colors = device_palette(mode, id).clone();
        if let Some(custom) = self.custom_palette(id) {
            for key in ["surface", "ink", "muted", "accent", "meter", "border"] {
                if let Some(color) = custom.colors.get(key).or_else(|| {
                    if key == "meter" {
                        custom.colors.get("accent")
                    } else {
                        None
                    }
                }) {
                    colors[key] = json!(color);
                }
            }
        }
        colors
    }

    // Resolve the global device appearance mode
    pub(super) fn device_mode(&self) -> &'static str {
        if self.device_theme.sync {
            self.global_ui_theme()
        } else {
            "dark"
        }
    }

    // Resolve the global device wallpaper
    pub(super) fn device_wallpaper(&self, mode: &str) -> Option<&PaletteWallpaper> {
        self.custom_palette(self.device_palette_id(mode))?
            .wallpaper
            .as_ref()
    }

    // Build the effective global device theme
    pub(super) fn resolved_device_theme(&self, id: &str) -> &'static str {
        match self.appearance.get(id).map(String::as_str) {
            Some("light") => "light",
            Some("dark") => "dark",
            _ => self.device_mode(),
        }
    }

    // Capture the theme exposed to a plugin
    pub(super) fn plugin_theme_snapshot(&self, id: &str) -> Value {
        let mode = self.resolved_device_theme(id);
        let mut theme = self.device_colors(mode);
        if let Some(wallpaper) = self.device_wallpaper(mode) {
            theme["wallpaper"] = wallpaper.svg.as_ref().map_or_else(
                || json!({"kind":"media","asset_id":wallpaper.asset_id,"format":wallpaper.format,"animated":wallpaper.animated}),
                |svg| json!(svg),
            );
        }
        json!({"version":1,"resolved":mode,"revision":self.theme_revision,"theme":theme})
    }

    // Handle a theme RPC request and return its correlated response
    pub(super) fn theme_rpc(&mut self, id: &str, message: &Value, inspector: bool) -> Value {
        let request = message["request_id"]
            .as_str()
            .filter(|id| id.len() <= 36)
            .unwrap_or("");
        let mut response = json!({"event":"streamDockManagerRpcResponse","protocol_id":THEME_RPC,"request_id":request});
        let error = if inspector {
            Some(("unauthorized", "Theme RPC requires an authenticated plugin"))
        } else if uuid::Uuid::parse_str(request).is_err()
            || message["method"] != "get"
            || !message["payload"]
                .as_object()
                .is_some_and(|payload| payload.is_empty())
            || !message.as_object().is_some_and(|fields| {
                fields.keys().all(|key| {
                    ["event", "protocol_id", "method", "request_id", "payload"]
                        .contains(&key.as_str())
                })
            })
        {
            Some(("invalid_request", "Use get with an empty payload"))
        } else {
            let snapshot = self.plugin_theme_snapshot(id);
            self.sessions
                .queues
                .entry(id.into())
                .or_default()
                .display_theme = Some(snapshot.clone());
            response["result"] = snapshot;
            None
        };
        if let Some((code, message)) = error {
            response["error"] = json!({"code":code,"message":message});
        }
        response
    }

    // Queue display theme
    fn queue_display_theme(&mut self, id: &str) {
        let snapshot = self.plugin_theme_snapshot(id);
        let Some(queue) = self.sessions.queues.get_mut(id) else {
            return;
        };
        let Some(previous) = &queue.display_theme else {
            return;
        };
        if previous["theme"] == snapshot["theme"] && previous["resolved"] == snapshot["resolved"] {
            return;
        }
        queue.display_theme = Some(snapshot.clone());
        queue.retain(|event| event["event"] != "didReceivePluginTheme");
        queue.push_back(json!({"event":"didReceivePluginTheme","payload":snapshot}));
    }

    // Build the effective theme for the requested scope
    pub(super) fn resolved_theme(&self, id: &str) -> &'static str {
        let preference = self
            .appearance
            .get(id)
            .map(String::as_str)
            .unwrap_or("follow-ui");
        match preference {
            "light" => "light",
            "dark" => "dark",
            _ => self.resolved_ui_theme(),
        }
    }
    // Build the theme notification payload
    pub(super) fn theme_payload(&self, id: &str, inspector: bool) -> Value {
        let resolved = if inspector {
            self.resolved_theme(id)
        } else {
            self.resolved_device_theme(id)
        };
        let data = palettes();
        let mut colors = data["themes"][resolved].clone();
        if inspector {
            // Inspector SDKs accept literal colors rather than CSS references
            for token in ["danger-text", "toast-error"] {
                colors[token] = colors["accent-ink"].clone();
            }
            let scheme = self.manager_palette_id(resolved);
            if let (Some(colors), Some(overrides)) = (
                colors.as_object_mut(),
                data["managerSchemes"][scheme]["tokens"].as_object(),
            ) {
                colors.extend(overrides.clone());
            }
            if let Some(custom) = self.custom_palette(scheme) {
                for (key, value) in &custom.colors {
                    colors[key] = json!(value);
                }
            }
        }
        json!({"version":1,"resolved":resolved,"palette":colors})
    }
    // Queue theme
    pub(super) fn queue_theme(&mut self, id: &str, token: &str, inspector: bool) {
        // Inspector CSS retains this event. Runtime authoring subscribes to
        // plugin-theme-v1 and receives didReceivePluginTheme only.
        if !inspector || !self.capabilities.contains_key(id) {
            return;
        }
        let payload = self.theme_payload(id, inspector);
        let queue = self
            .sessions
            .queues
            .entry(if inspector {
                format!("pi:{token}")
            } else {
                id.into()
            })
            .or_default();
        if queue.theme.as_ref() == Some(&payload) {
            return;
        }
        queue.theme = Some(payload.clone());
        queue.retain(|event| event["event"] != "didReceiveTheme");
        queue.push_back(json!({"event":"didReceiveTheme","payload":payload}));
    }
    // Notify themes
    pub(super) fn notify_themes(&mut self) {
        self.notify_display_theme();
        let modern: Vec<_> = self
            .sessions
            .queues
            .iter()
            .filter(|(_, queue)| queue.display_theme.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in modern {
            self.queue_display_theme(&id);
        }
        self.notify_inspector_themes();
    }
    // Notify display theme
    fn notify_display_theme(&mut self) {
        self.theme_revision = self.theme_revision.wrapping_add(1);
        self.visual_state.render_generation = self.visual_state.render_generation.wrapping_add(1);
        visual::refresh_themes(self);
        if let Some(wake) = &self.wake {
            wake();
        }
    }
    // Notify inspector themes
    fn notify_inspector_themes(&mut self) {
        let panels: Vec<_> = self
            .sessions
            .files
            .iter()
            .filter(|(_, grant)| grant.context.is_some())
            .filter_map(|(token, _)| {
                self.sessions
                    .tokens
                    .get(token)
                    .map(|id| (id.clone(), token.clone()))
            })
            .collect();
        for (id, token) in panels {
            self.queue_theme(&id, &token, true);
        }
    }
}
impl PluginHost {
    /// Session-derived overrides are transient; profile storage remains runtime-owned
    pub fn set_profile_palettes(&self, palettes: HashMap<String, ProfilePalette>) {
        let mut hub = self.hub.lock().unwrap();
        if hub.profile_palettes == palettes {
            return;
        }
        hub.profile_palettes = palettes;
        hub.notify_themes();
        hub.theme_observer.wake();
    }

    // Set manager theme device
    pub fn set_manager_theme_device(&self, device: &str) {
        let mut hub = self.hub.lock().unwrap();
        if hub.manager_theme_device != device {
            hub.manager_theme_device = device.into();
            hub.notify_inspector_themes();
        }
    }

    // Reset appearance with
    pub fn reset_appearance_with<T: Send + 'static>(
        &mut self,
        id: &str,
        edit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        if !self.plugins.contains_key(id) {
            return Err("Plugin not installed".into());
        }
        let mut hub = self.hub.lock().unwrap();
        let previous = hub.appearance.remove(id);
        match hub.persist_with(edit) {
            Ok(result) => {
                if previous.is_some() {
                    hub.notify_themes();
                }
                Ok(result)
            }
            Err(error) => {
                if let Some(previous) = previous {
                    hub.appearance.insert(id.into(), previous);
                }
                Err(error)
            }
        }
    }

    // Read the colors used to render the device display
    pub fn device_display_colors(
        &self,
        device: &str,
        plugin: Option<&str>,
    ) -> (DeviceThemePreferences, Value) {
        let hub = self.hub.lock().unwrap();
        let mode = plugin.map_or_else(
            || hub.device_mode_for(device),
            |id| hub.resolved_device_theme_for(id, device),
        );
        (
            hub.device_theme_for(device),
            hub.device_colors_for(mode, device),
        )
    }

    // Read the configured appearance preferences
    pub fn appearance_preferences(&self) -> AppearancePreferences {
        let h = self.hub.lock().unwrap();
        AppearancePreferences {
            configured: !h.ui_appearance.is_empty(),
            preference: if h.ui_appearance.is_empty() {
                "system".into()
            } else {
                h.ui_appearance.clone()
            },
            system_dark: h.system_dark,
            style: h.ui_theme_style.clone(),
            custom_palettes: h
                .custom_palettes
                .iter()
                .cloned()
                .map(|mut palette| {
                    if let Some(wallpaper) = &mut palette.wallpaper {
                        wallpaper.svg = None;
                    }
                    palette
                })
                .collect(),
            device_theme: h.device_theme,
            profile_palette: h.manager_profile_palette().cloned(),
        }
    }
    // Set UI theme style
    pub fn set_ui_theme_style(
        &mut self,
        style: UiThemeStyle,
        preference: Option<&str>,
    ) -> Result<bool, String> {
        if preference.is_some_and(|mode| !["system", "light", "dark"].contains(&mode)) {
            return Err("invalid UI appearance".into());
        }
        let mut h = self.hub.lock().unwrap();
        style.validate(&h.custom_palettes)?;
        let mode_changed = preference.is_some_and(|mode| mode != h.ui_appearance);
        if h.ui_theme_style == style && !mode_changed {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut h.ui_theme_style, style);
        let previous_mode = h.ui_appearance.clone();
        if let Some(mode) = preference {
            h.ui_appearance = mode.into();
        }
        if let Err(error) = h.persist() {
            h.ui_theme_style = previous;
            h.ui_appearance = previous_mode;
            return Err(error);
        }
        if mode_changed {
            h.notify_themes();
            h.theme_observer.wake();
        } else if h.ui_theme_style.light_palette != previous.light_palette
            || h.ui_theme_style.dark_palette != previous.dark_palette
        {
            h.notify_themes();
        }
        Ok(mode_changed)
    }
    // Set device theme preferences
    pub fn set_device_theme_preferences(
        &mut self,
        preferences: DeviceThemePreferences,
    ) -> Result<(), String> {
        let mut h = self.hub.lock().unwrap();
        if h.device_theme == preferences {
            return Ok(());
        }
        let before = std::mem::replace(&mut h.device_theme, preferences);
        if let Err(error) = h.persist() {
            h.device_theme = before;
            return Err(error);
        }
        h.notify_themes();
        Ok(())
    }

    // Update custom palette
    pub fn update_custom_palette(
        &mut self,
        mut palette: Option<CustomPalette>,
        delete: Option<String>,
    ) -> Result<(), String> {
        let mut h = self.hub.lock().unwrap();
        let mut palettes = h.custom_palettes.clone();
        let mut style = h.ui_theme_style.clone();
        match (&mut palette, delete) {
            (Some(palette), None) => {
                palette.name = palette.name.trim().into();
                if palette.id.is_empty() {
                    palette.id = uuid::Uuid::new_v4().to_string();
                    if palettes.len() >= PALETTE_LIMIT {
                        return Err("Palette library is full".into());
                    }
                    palette.validate()?;
                    palettes.push(palette.clone());
                } else {
                    palette.validate()?;
                    let previous = palettes
                        .iter_mut()
                        .find(|item| item.id == palette.id)
                        .ok_or("Palette no longer exists")?;
                    if previous.mode != palette.mode {
                        return Err("Duplicate the palette to change its mode".into());
                    }
                    *previous = palette.clone();
                }
            }
            (None, Some(id)) => {
                let index = palettes
                    .iter()
                    .position(|palette| palette.id == id)
                    .ok_or("Palette no longer exists")?;
                palettes.remove(index);
                for selection in [&mut style.light_palette, &mut style.dark_palette] {
                    if *selection == id {
                        *selection = "default".into();
                    }
                }
            }
            _ => return Err("Choose one palette operation".into()),
        }
        let previous = std::mem::replace(&mut h.custom_palettes, palettes);
        let previous_style = std::mem::replace(&mut h.ui_theme_style, style);
        if let Err(error) = h.persist() {
            h.custom_palettes = previous;
            h.ui_theme_style = previous_style;
            return Err(error);
        }
        h.notify_themes();
        Ok(())
    }

    // Set appearance
    pub fn set_appearance(
        &mut self,
        id: Option<&str>,
        preference: &str,
        system_dark: Option<bool>,
    ) -> Result<(), String> {
        if let Some(id) = id {
            if !self
                .plugins
                .get(id)
                .is_some_and(|p| p.info.capabilities.is_some())
            {
                return Err("plugin does not support appearance".into());
            }
            if !["follow-ui", "light", "dark"].contains(&preference) {
                return Err("invalid plugin appearance".into());
            }
        } else if !["system", "light", "dark"].contains(&preference) {
            return Err("invalid UI appearance".into());
        }
        let mut h = self.hub.lock().unwrap();
        let before = (h.appearance.clone(), h.ui_appearance.clone(), h.system_dark);
        if let Some(id) = id {
            h.appearance.insert(id.into(), preference.into());
        } else {
            h.ui_appearance = preference.into();
        }
        if let Some(dark) = system_dark {
            h.system_dark = dark;
        }
        if let Err(error) = h.persist() {
            (h.appearance, h.ui_appearance, h.system_dark) = before;
            return Err(error);
        }
        if before != (h.appearance.clone(), h.ui_appearance.clone(), h.system_dark) {
            h.notify_themes();
            h.theme_observer.wake();
        }
        Ok(())
    }
}

// Probe the system’s current dark-mode preference
pub(super) fn system_appearance() -> Option<bool> {
    #[cfg(not(target_os = "windows"))]
    use std::process::Command;
    use std::process::Stdio;
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "Dark")
    }
    #[cfg(target_os = "windows")]
    {
        let output = stream_dock_manager_plugin_runtimes::background_command("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).contains("0x0"))
    }
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).contains("dark"));
        }
        let home = std::env::var_os("HOME")?;
        let config =
            std::fs::read_to_string(PathBuf::from(home).join(".config/kdeglobals")).ok()?;
        Some(
            config.lines().any(|line| {
                line.starts_with("ColorScheme=") && line.to_lowercase().contains("dark")
            }),
        )
    }
}

impl PluginHost {
    // Read the revision used to invalidate themed displays
    pub fn display_theme_revision(&self) -> u64 {
        self.hub.lock().unwrap().theme_revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify profile palettes isolate targets devices and follow system
    fn profile_palettes_isolate_targets_devices_and_follow_system() {
        let mut hub = Hub {
            ui_appearance: "light".into(),
            system_dark: true,
            manager_theme_device: "dock-a".into(),
            ..Default::default()
        };
        let palette = ProfilePalette {
            target: ProfilePaletteTarget::Manager,
            light_palette: "sage".into(),
            dark_palette: "midnight".into(),
        };
        hub.profile_palettes
            .insert("dock-a".into(), palette.clone());
        assert!(hub.follows_system_theme());
        assert_eq!(hub.resolved_ui_theme(), "dark");
        assert_eq!(hub.manager_palette_id("dark"), "midnight");
        assert_eq!(hub.device_mode_for("dock-a"), "light");
        assert_eq!(hub.device_palette_id_for("light", "dock-a"), "default");
        hub.profile_palettes.get_mut("dock-a").unwrap().target = ProfilePaletteTarget::Device;
        assert_eq!(hub.resolved_ui_theme(), "light");
        assert_eq!(hub.device_mode_for("dock-a"), "dark");
        assert_eq!(
            hub.device_colors_for("dark", "dock-a"),
            *device_palette("dark", "midnight")
        );
        assert_eq!(hub.device_mode_for("dock-b"), "light");
        assert_eq!(hub.device_palette_id_for("dark", "dock-b"), "default");
        hub.device_theme.sync = false;
        assert!(hub.device_theme_for("dock-a").sync);
        assert!(!hub.device_theme_for("dock-b").sync);
        hub.profile_palettes.get_mut("dock-a").unwrap().target = ProfilePaletteTarget::Both;
        hub.system_dark = false;
        assert_eq!(hub.resolved_ui_theme(), "light");
        assert_eq!(hub.device_mode_for("dock-a"), "light");
        assert_eq!(hub.device_palette_id_for("light", "dock-a"), "sage");
        hub.manager_theme_device = "dock-b".into();
        assert_eq!(hub.manager_palette_id("light"), "default");
        hub.profile_palettes.get_mut("dock-a").unwrap().target = ProfilePaletteTarget::Global;
        assert!(!hub.follows_system_theme());
        assert_eq!(hub.device_mode_for("dock-a"), "dark");
        assert_eq!(hub.device_palette_id_for("light", "dock-a"), "default");
    }

    #[test]
    // Verify profile palette validation and missing library fallback
    fn profile_palette_validation_and_missing_library_fallback() {
        let mut palette = ProfilePalette {
            target: ProfilePaletteTarget::Both,
            light_palette: "sage".into(),
            dark_palette: "midnight".into(),
        };
        palette.validate().unwrap();
        palette.light_palette = "midnight".into();
        assert!(palette.validate().is_err());
        palette.light_palette = uuid::Uuid::new_v4().to_string();
        palette.validate().unwrap();
        let mut hub = Hub {
            manager_theme_device: "dock".into(),
            ..Default::default()
        };
        hub.profile_palettes.insert("dock".into(), palette.clone());
        assert_eq!(hub.device_palette_id_for("light", "dock"), "default");
        assert_eq!(hub.manager_palette_id("light"), "default");
        let custom = CustomPalette {
            id: palette.light_palette.clone(),
            name: "Local".into(),
            mode: "light".into(),
            colors: [("surface".into(), "#123456".into())].into(),
            wallpaper: None,
        };
        hub.custom_palettes.push(custom);
        assert_eq!(hub.device_colors_for("light", "dock")["surface"], "#123456");
        hub.custom_palettes.clear();
        assert_eq!(hub.device_palette_id_for("light", "dock"), "default");
        assert_eq!(hub.profile_palettes["dock"], palette);
    }

    #[test]
    // Verify system observer probes only on demand and suppresses unchanged results
    fn system_observer_probes_only_on_demand_and_suppresses_unchanged_results() {
        let hub = Arc::new(Mutex::new(Hub {
            ui_appearance: "dark".into(),
            ..Default::default()
        }));
        refresh_system(&hub, || panic!("explicit themes must not probe"));
        hub.lock().unwrap().ui_appearance = "system".into();
        let before = hub.lock().unwrap().theme_revision;
        refresh_system(&hub, || None);
        assert_eq!(hub.lock().unwrap().theme_revision, before);
        let dark = hub.lock().unwrap().system_dark;
        refresh_system(&hub, || Some(dark));
        assert_eq!(hub.lock().unwrap().theme_revision, before);
        refresh_system(&hub, || Some(!dark));
        assert_eq!(hub.lock().unwrap().theme_revision, before + 1);
        // A manual preference selected while the subprocess is running wins
        refresh_system(&hub, || {
            hub.lock().unwrap().ui_appearance = "light".into();
            Some(dark)
        });
        assert_eq!(hub.lock().unwrap().theme_revision, before + 1);
    }
    #[test]
    // Verify theme RPC negotiates live snapshots and rejects inspectors
    fn theme_rpc_negotiates_live_snapshots_and_rejects_inspectors() {
        let mut hub = Hub::default();
        let request =
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"method":"get","payload":{}});
        assert_eq!(
            hub.theme_rpc("fixture", &request, true)["error"]["code"],
            "unauthorized"
        );
        let mut invalid = request.clone();
        invalid["payload"]["plugin_id"] = json!("someone-else");
        assert_eq!(
            hub.theme_rpc("fixture", &invalid, false)["error"]["code"],
            "invalid_request"
        );
        let initial = hub.theme_rpc("fixture", &request, false);
        assert_eq!(initial["protocol_id"], THEME_RPC);
        assert_eq!(initial["result"]["version"], 1);
        assert!(initial["result"]["theme"]["wallpaper"]
            .as_str()
            .unwrap()
            .contains("<svg"));
        hub.notify_themes();
        assert!(hub
            .sessions
            .queues
            .get_mut("fixture")
            .unwrap()
            .take_batch()
            .unwrap()
            .is_empty());
        hub.appearance.insert("fixture".into(), "dark".into());
        hub.ui_theme_style.dark_palette = "midnight".into();
        hub.notify_themes();
        let update = hub
            .sessions
            .queues
            .get_mut("fixture")
            .unwrap()
            .take_batch()
            .unwrap();
        assert_eq!(update.len(), 1);
        assert_eq!(update[0].value["event"], "didReceivePluginTheme");
        assert_eq!(update[0].value["payload"]["theme"]["surface"], "#172138");
    }

    #[test]
    // Verify runtime has no duplicate theme event and inspectors keep CSS updates
    fn runtime_has_no_duplicate_theme_event_and_inspectors_keep_css_updates() {
        let mut hub = Hub {
            ui_appearance: "system".into(),
            system_dark: false,
            ..Default::default()
        };
        hub.capabilities
            .insert("follow".into(), Capabilities::default());
        hub.queue_theme("follow", "", false);
        assert!(!hub.sessions.queues.contains_key("follow"));
        hub.queue_theme("follow", "panel", true);
        assert_eq!(hub.sessions.queues["pi:panel"].len(), 1);
        hub.sessions.queues.get_mut("pi:panel").unwrap().clear();
        hub.queue_theme("follow", "panel", true);
        assert!(hub.sessions.queues["pi:panel"].is_empty());
        hub.system_dark = true;
        hub.queue_theme("follow", "panel", true);
        assert_eq!(hub.sessions.queues["pi:panel"].len(), 1);
        assert_eq!(
            hub.sessions.queues["pi:panel"].iter().next().unwrap()["event"],
            "didReceiveTheme"
        );
    }
    #[test]
    // Verify observer wait wakes on preference changes and shutdown
    fn observer_wait_wakes_on_preference_changes_and_shutdown() {
        let signal = Arc::new(ObserverSignal::default());
        let stop = Arc::new(AtomicBool::new(false));
        signal.wake();
        signal.wait(0, false, &stop); // A change before waiting must not be lost.
        let worker_signal = signal.clone();
        let worker_stop = stop.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            worker_signal.wait(1, false, &worker_stop);
            sent.send(()).unwrap();
        });
        stop.store(true, Ordering::SeqCst);
        signal.wake();
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }
    #[test]
    // Verify invalid stored preferences fail without resetting data
    fn invalid_stored_preferences_fail_without_resetting_data() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        storage
            .write_json(
                stream_dock_manager_storage::Domain::PluginMetadata,
                "appearance",
                &json!([]),
                1024,
            )
            .unwrap();
        assert!(Hub::load(&storage).is_err());
        assert_eq!(
            storage
                .read_json::<Value>(
                    stream_dock_manager_storage::Domain::PluginMetadata,
                    "appearance",
                    1024
                )
                .unwrap(),
            Some(json!([]))
        );
    }
    #[test]
    // Verify manual and system changes respect plugin overrides
    fn manual_and_system_changes_respect_plugin_overrides() {
        let mut hub = Hub {
            ui_appearance: "system".into(),
            system_dark: true,
            ..Hub::default()
        };
        assert_eq!(hub.resolved_theme("fixture"), "dark");
        hub.ui_appearance = "light".into();
        assert_eq!(hub.resolved_theme("fixture"), "light");
        hub.system_dark = false;
        hub.appearance.insert("fixture".into(), "dark".into());
        assert_eq!(hub.resolved_theme("fixture"), "dark");
        hub.appearance.insert("fixture".into(), "follow-ui".into());
        assert_eq!(hub.resolved_theme("fixture"), "light");
        hub.ui_appearance = "system".into();
        hub.system_dark = true;
        assert_eq!(hub.resolved_theme("fixture"), "dark");
    }
    #[test]
    // Verify curated style persists and uses the resolved inspector mode
    fn curated_style_persists_and_uses_the_resolved_inspector_mode() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        let mut hub = Hub::load(&storage).unwrap();
        assert!(hub.ui_theme_style == UiThemeStyle::default());
        hub.ui_theme_style = UiThemeStyle {
            light_palette: "sea-glass".into(),
            dark_palette: "hot-pink".into(),
            font: "system:Absent Fixture".into(),
            text_scale: 135,
        };
        hub.ui_theme_style.validate(&[]).unwrap();
        hub.ui_appearance = "light".into();
        hub.appearance.insert("pinned".into(), "dark".into());
        hub.persist().unwrap();
        let restored = Hub::load(&storage).unwrap();
        assert!(restored.ui_theme_style == hub.ui_theme_style);
        assert_eq!(
            restored.theme_payload("following", true)["palette"]["accent"],
            "#14666c"
        );
        assert_eq!(
            restored.theme_payload("pinned", true)["palette"]["accent"],
            "#d42883"
        );
        assert_eq!(
            restored.theme_payload("following", false)["palette"]["accent"],
            "#b51e36"
        );
        let invalid = UiThemeStyle {
            light_palette: "hot-pink".into(),
            ..restored.ui_theme_style.clone()
        };
        assert!(invalid.validate(&[]).is_err());
        hub.ui_theme_style.dark_palette = "pine".into();
        hub.persist().unwrap();
        assert!(Hub::load(&storage).is_err());
    }
    #[test]
    // Verify host preferences and unknown settings survive restart without saving drafts
    fn host_preferences_and_unknown_settings_survive_restart_without_saving_drafts() {
        let root = tempfile::tempdir().unwrap();
        let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
        let mut hub = Hub::load(&storage).unwrap();
        hub.appearance.insert("fixture".into(), "light".into());
        hub.ui_appearance = "dark".into();
        hub.globals
            .insert("saved".into(), json!({"unknown_legacy_key":42}));
        hub.context_state
            .settings
            .insert("draft".into(), json!({"do_not_save":true}));
        hub.context_state.drafts.insert("draft".into());
        hub.persist().unwrap();
        let restored = Hub::load(&storage).unwrap();
        assert_eq!(restored.resolved_theme("fixture"), "light");
        assert_eq!(restored.resolved_theme("other"), "dark");
        assert_eq!(restored.globals["saved"]["unknown_legacy_key"], 42);
        assert!(!restored.context_state.settings.contains_key("draft"));
    }
}
