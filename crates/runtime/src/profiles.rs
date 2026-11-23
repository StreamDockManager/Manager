use crate::actions::Action;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub const DEFAULT_PROFILE_NAME: &str = "Default";

const MAX_ARCHIVE_FILES: usize = 256;
// Compute a digest for reader
fn digest_reader(
    source: impl Read,
    limit: usize,
    control: &crate::media::PreparationControl,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut source = source.take(limit as u64 + 1);
    let mut hasher = Sha256::new();
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.check()?;
        let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count;
        if total > limit {
            return Err("Media content exceeds size limit".into());
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}
// Compute a digest for asset
fn digest_asset(path: &Path, control: &crate::media::PreparationControl) -> Result<String, String> {
    let stamp = SourceStamp::read(path)?;
    let source = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let digest = digest_reader(source, crate::media::MAX_ASSET, control)?;
    if SourceStamp::read(path)? != stamp {
        return Err("Animation changed during read; retry".into());
    }
    Ok(digest)
}
// Capture and hash the same bytes once. Parsing a later open can otherwise pair
// one revision's digest with another revision's frames on platforms whose file
// metadata cannot detect a same-length rewrite. The private temporary file also
// preserves that pairing for in-place mutations while keeping RAM bounded
fn snapshot_asset(
    path: &Path,
    control: &crate::media::PreparationControl,
) -> Result<(std::fs::File, String, SourceStamp), String> {
    use sha2::{Digest, Sha256};
    let stamp = SourceStamp::read(path)?;
    let mut input = std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(crate::media::MAX_ASSET as u64 + 1);
    let mut snapshot = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.check()?;
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count;
        if total > crate::media::MAX_ASSET {
            return Err("Media content exceeds size limit".into());
        }
        hash.update(&buffer[..count]);
        snapshot
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    if SourceStamp::read(path)? != stamp {
        return Err("Animation changed during read; retry".into());
    }
    control.check()?;
    snapshot.rewind().map_err(|e| e.to_string())?;
    Ok((snapshot, hex::encode(hash.finalize()), stamp))
}
// Report the same asset bytes condition for profiles
fn same_asset_bytes(left: &Path, right: &Path) -> bool {
    let compare = || -> Result<bool, String> {
        let left_stamp = SourceStamp::read(left)?;
        let right_stamp = SourceStamp::read(right)?;
        if left_stamp.len != right_stamp.len {
            return Ok(false);
        }
        let mut left_file = std::fs::File::open(left).map_err(|e| e.to_string())?;
        let mut right_file = std::fs::File::open(right).map_err(|e| e.to_string())?;
        let mut a = [0u8; 64 * 1024];
        let mut b = [0u8; 64 * 1024];
        let mut remaining = left_stamp.len;
        while remaining > 0 {
            let count = remaining.min(a.len() as u64) as usize;
            left_file
                .read_exact(&mut a[..count])
                .map_err(|e| e.to_string())?;
            right_file
                .read_exact(&mut b[..count])
                .map_err(|e| e.to_string())?;
            if a[..count] != b[..count] {
                return Ok(false);
            }
            remaining -= count as u64;
        }
        Ok(SourceStamp::read(left)? == left_stamp && SourceStamp::read(right)? == right_stamp)
    };
    compare().unwrap_or(false)
}

/// Selects a background without discarding the saved custom color or media
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundSource {
    Palette,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::live_label::LabelSegment>>")]
    pub live_label: Vec<crate::live_label::LabelSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dial_response: Option<crate::dial_response::DialResponse>,
    #[serde(
        default,
        skip_serializing_if = "crate::actions::ActivationPolicy::is_queue"
    )]
    #[ts(optional, as = "Option<crate::actions::ActivationPolicy>")]
    pub activation_policy: crate::actions::ActivationPolicy,
    pub title: String,
    pub color: String,
    /// Distinguishes an explicit color (including transparent) from the original default
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub custom_background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_source: Option<BackgroundSource>,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asset: Option<String>,
    #[serde(default)]
    pub media: crate::media::MediaOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub appearance: Option<Box<hardware_core::appearance::Appearance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_appearance: Option<Box<hardware_core::plugin_display::PluginAppearance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub composition: Option<Box<hardware_core::composition::DisplayComposition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub disabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub name: String,
    pub bindings: BTreeMap<String, Binding>,
}
impl Page {
    // Resolve a feedback surface to its binding source unless it has a direct or touch-panel binding
    pub(crate) fn surface_source<'a>(
        &self,
        manifest: &'a hardware_core::ModelManifest,
        control_id: &'a str,
    ) -> &'a str {
        if self.bindings.contains_key(control_id)
            || manifest
                .capabilities
                .touch_panel
                .as_ref()
                .is_some_and(|panel| panel.button_controls.iter().any(|id| id == control_id))
        {
            return control_id;
        }
        manifest
            .controls
            .iter()
            .find(|source| source.feedback_surface.as_deref() == Some(control_id))
            .map(|source| source.id.as_str())
            .unwrap_or(control_id)
    }
}
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct LinkMember {
    pub page_id: String,
    pub control_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<String,Vec<LinkMember>>>")]
    pub linked_controls: BTreeMap<String, Vec<LinkMember>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub palette: Option<stream_dock_manager_plugin_host::ProfilePalette>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::parameters::ProfileParameter>>")]
    pub parameters: Vec<crate::parameters::ProfileParameter>,
    pub id: String,
    pub name: String,
    pub model_id: String,
    pub active_page: String,
    pub pages: Vec<Page>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_asset: Option<String>,
    #[serde(default)]
    pub background_media: crate::media::MediaOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub minimum_manager: Option<String>,
    pub saved_macros: Vec<crate::builtins::SavedMacro>,
    pub schema_version: u32,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
    pub configuration_revision: u64,
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<crate::workspace::AppearancePreset>>")]
    pub styles: Vec<crate::workspace::AppearancePreset>,
    pub rules: Vec<crate::workspace::AppRule>,
    pub history: crate::workspace::HistoryIndex,
}

// Parse data
pub(crate) fn parse_data(value: serde_json::Value) -> Result<ProfileData, String> {
    if !value.is_object() {
        return Err("Workspace data must be a current JSON object".into());
    }
    // Report the compatibility floor before deserializing fields from a later release.
    if let Some(v) = value.get("minimum_manager").and_then(|v| v.as_str()) {
        if semver::Version::parse(v)
            .is_ok_and(|floor| floor > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap())
        {
            return Err(format!(
                "This workspace requires Manager {v} or newer; original file preserved"
            ));
        }
    }
    serde_json::from_value(value).map_err(|e| format!("Unsupported or invalid workspace data: {e}"))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileExportOptions {
    #[ts(optional)]
    pub profile_ids: Option<Vec<String>>,
    pub include_plugin_settings: bool,
}

impl ProfileData {
    // Update requirements
    pub(crate) fn update_requirements(&mut self) {
        let mut control_events = false;
        for action in self.saved_macros.iter().map(|m| &m.action).chain(
            self.profiles
                .iter()
                .flat_map(|p| &p.pages)
                .flat_map(|p| p.bindings.values())
                .map(|b| &b.action),
        ) {
            action.visit(&mut |a| {
                control_events |= matches!(a, Action::Events { .. });
            });
        }
        let new_activation = self
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .any(|b| b.activation_policy.interrupts());
        let linked = self.profiles.iter().any(|p| !p.linked_controls.is_empty());
        let labels = self
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .any(|b| !b.live_label.is_empty());
        let supported_baseline = semver::Version::new(0, 1, 1);
        let floor = (linked || new_activation || labels || control_events)
            .then(|| supported_baseline.clone());
        let models = hardware_registry::manifests();
        let hardware_floor = self
            .profiles
            .iter()
            .filter_map(|profile| {
                let model = models.iter().find(|m| m.model_id == profile.model_id)?;
                let mut used_lighting = false;
                for binding in profile.pages.iter().flat_map(|page| page.bindings.values()) {
                    binding.action.visit(&mut |action| {
                        if matches!(action, Action::Lighting { .. }) {
                            used_lighting = true;
                        }
                    });
                }
                profile
                    .pages
                    .iter()
                    .flat_map(|page| page.bindings.keys())
                    .filter_map(|id| model.control(id).ok()?.minimum_manager.as_deref())
                    .chain(
                        profile
                            .background_asset
                            .as_ref()
                            .and(model.capabilities.background_minimum_manager.as_deref()),
                    )
                    .chain(
                        used_lighting
                            .then_some(model.capabilities.lighting_minimum_manager.as_deref())
                            .flatten(),
                    )
                    .filter_map(|v| semver::Version::parse(v).ok())
                    .filter(|v| v >= &supported_baseline)
                    .max()
            })
            .max();
        let floor = floor.into_iter().chain(hardware_floor).max();
        if let Some(floor) = floor {
            if self.minimum_manager.as_ref().is_none_or(|current| {
                semver::Version::parse(current).is_ok_and(|version| version < floor)
            }) {
                self.minimum_manager = Some(floor.to_string());
            }
        }
    }
    // Delete profile
    pub fn delete_profile(&mut self, id: &str) -> Result<(), String> {
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == id)
            .ok_or("Unknown profile")?;
        let removed = self.profiles.remove(index);
        self.rules.retain(|rule| rule.profile_id != id);
        if self.active_profile == id {
            self.active_profile = self
                .profiles
                .iter()
                .find(|p| p.model_id == removed.model_id)
                .or_else(|| self.profiles.first())
                .map(|p| p.id.clone())
                .unwrap_or_default();
        }
        Ok(())
    }
    // Delete page
    pub fn delete_page(&mut self, profile_id: &str, page_id: &str) -> Result<(), String> {
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.id == profile_id)
            .ok_or("Unknown profile")?;
        let index = profile
            .pages
            .iter()
            .position(|p| p.id == page_id)
            .ok_or("Unknown page")?;
        if crate::keyboard_navigation::supported(&profile.model_id)
            && page_id == crate::keyboard_navigation::PAGE_ID
        {
            return Err("The keyboard Navigation page cannot be removed".into());
        }
        if profile.pages.len() <= 1 {
            return Err("Keep at least one page in each profile".into());
        }
        profile.pages.remove(index);
        profile.prune_links(|m| m.page_id != page_id);
        self.rules.retain(|rule| {
            rule.profile_id != profile_id || rule.page_id.as_deref() != Some(page_id)
        });
        if profile.active_page == page_id {
            profile.active_page = profile.pages[index.saturating_sub(1)].id.clone();
        }
        Ok(())
    }
}
impl Binding {
    // Report whether there is content
    pub fn has_content(&self) -> bool {
        self.background_source == Some(BackgroundSource::Palette)
            || !self.live_label.is_empty()
            || self.dial_response.is_some()
            || !self.activation_policy.is_queue()
            || !self.title.is_empty()
            || self.asset.is_some()
            || !matches!(self.action, Action::None)
            || self.appearance.is_some()
            || self.plugin_appearance.is_some()
            || self.composition.is_some()
            || self.feedback.as_deref().is_some_and(|value| value != "off")
            || self.disabled
            || (!["#242424", "#19202c"].contains(&self.color.to_ascii_lowercase().as_str())
                && hardware_core::appearance::background_color(&self.color)
                    .is_none_or(|color| color[3] != 0))
            || self.media.fit != "fill"
            || self.media.speed != 1.0
            || self.media.trim_start_ms != 0
            || self.media.trim_end_ms.is_some()
            || self.media.crop != [0.0, 0.0, 1.0, 1.0]
    }
    // Validate the supplied profiles value
    pub fn validate(&self) -> Result<(), String> {
        self.validate_with_draft(false)
    }
    #[cfg(test)]
    // Validate draft
    pub(crate) fn validate_draft(&self) -> Result<(), String> {
        self.validate_with_draft(true)
    }
    /// Visual-only editor input. Incomplete executable fields are intentionally
    /// allowed; this must never be used to save a binding or dispatch an action
    pub(crate) fn validate_visual_draft(&self) -> Result<(), String> {
        // Reject preview action trees exceeding the nesting or child-count bounds
        fn bounded(action: &crate::actions::Action, depth: usize) -> Result<(), String> {
            if depth > 8 {
                return Err("Preview action nesting exceeds 8".into());
            }
            for (index, child) in (0..).map_while(|index| action.child(index)).enumerate() {
                if index >= 100 {
                    return Err("Preview action exceeds 100 children".into());
                }
                bounded(child, depth + 1)?;
            }
            Ok(())
        }
        bounded(&self.action, 0)?;
        if serde_json::to_vec(&self.action)
            .map_err(|e| e.to_string())?
            .len()
            > 1024 * 1024
        {
            return Err("Preview action exceeds 1 MiB".into());
        }
        let mut visual = self.clone();
        visual.action = crate::actions::Action::None;
        visual.validate()
    }
    // Validate with draft
    fn validate_with_draft(&self, draft: bool) -> Result<(), String> {
        crate::live_label::validate(&self.live_label)?;
        if let Some(response) = self.dial_response {
            response.validate()?;
        }
        if let Some(composition) = &self.composition {
            composition.validate()?;
        }
        if let Some(appearance) = &self.plugin_appearance {
            appearance.validate()?;
        }
        if let Some(appearance) = &self.appearance {
            appearance.validate()?;
        }
        if self.feedback.as_ref().is_some_and(|f| {
            !["off", "volume", "mute", "application", "macro"].contains(&f.as_str())
        }) {
            return Err("Invalid feedback source".into());
        }
        self.media.validate()?;
        if draft {
            self.action.validate_draft(0)?;
        } else {
            self.action.validate(0)?;
        }
        if let Some(id) = &self.asset {
            validate_asset_id(id)?;
        }
        if self.title.len() > 160
            || hardware_core::appearance::background_color(&self.color).is_none()
        {
            return Err("Invalid binding appearance".into());
        }
        Ok(())
    }
}
impl Profile {
    // Validate the supplied profiles value
    pub fn validate(&self) -> Result<(), String> {
        self.validate_with_unresolved_controls(false)
    }
    fn validate_with_unresolved_controls(&self, allow_unresolved: bool) -> Result<(), String> {
        self.validate_links()?;
        if let Some(palette) = &self.palette {
            palette.validate()?;
        }
        if self.parameters.len() > 64 {
            return Err("Profile exceeds 64 parameters".into());
        }
        let mut parameter_names = HashSet::new();
        for p in &self.parameters {
            p.validate()?;
            if !parameter_names.insert(&p.name) {
                return Err("Duplicate parameter name".into());
            }
        }
        if self.id.is_empty()
            || self.name.trim().is_empty()
            || self.name.len() > 160
            || self.pages.is_empty()
        {
            return Err("profile needs an ID, name, and page".into());
        }
        let manifests = hardware_registry::manifests();
        self.background_media.validate()?;
        let manifest = manifests.iter().find(|m| m.model_id == self.model_id);
        if manifest.is_none() && !allow_unresolved {
            return Err("profile uses an unsupported model".into());
        }
        if let Some(asset) = &self.background_asset {
            validate_asset_id(asset)?;
            if manifest.is_some_and(|model| !model.capabilities.background) {
                return Err("Model has no background display".into());
            }
        }
        let mut ids = HashSet::new();
        for page in &self.pages {
            if page.id.is_empty() || !ids.insert(&page.id) {
                return Err("duplicate page ID".into());
            }
            for (id, binding) in &page.bindings {
                binding.validate()?;
                if binding.live_label.iter().any(|s|matches!(s,crate::live_label::LabelSegment::Parameter{name,..} if self.parameters.iter().any(|p|p.name==*name && p.sensitive))){
                    return Err("Choose a non-sensitive parameter for the live label.".into());
                }
                let Some(manifest) = manifest else { continue };
                let control = match manifest.control(id) {
                    Ok(control) => control,
                    Err(_) if allow_unresolved => continue,
                    Err(error) => {
                        return Err(format!(
                            "Unsupported profile control {}/{}: {error}",
                            self.model_id, id
                        ));
                    }
                };
                if binding.background_source == Some(BackgroundSource::Palette) {
                    let surface = manifest
                        .control(control.feedback_surface.as_deref().unwrap_or(id))
                        .map_err(|e| e.to_string())?;
                    if surface.image_width.is_none() || surface.image_height.is_none() {
                        return Err("Palette background requires an image surface".into());
                    }
                }
                if control.kind == "display" && !matches!(binding.action, Action::None) {
                    return Err(format!(
                        "Unsupported action on display-only control {}/{}",
                        self.model_id, id
                    ));
                }
                if control.kind == "encoder"
                    && !matches!(
                        binding.action,
                        Action::None | Action::Dial { .. } | Action::Events { .. }
                    )
                {
                    return Err(
                        "Encoder bindings require explicit left, right and press actions or event assignments".into(),
                    );
                }
                let mut dial_on_non_encoder = false;
                binding.action.visit(&mut |action| {
                    dial_on_non_encoder |=
                        control.kind != "encoder" && matches!(action, Action::Dial { .. });
                });
                if dial_on_non_encoder {
                    return Err("Dial actions require an encoder control".into());
                }
                binding.action.validate(0)?;
                if let Some(asset) = &binding.asset {
                    validate_asset_id(asset)?;
                }
                if binding.title.len() > 160
                    || hardware_core::appearance::background_color(&binding.color).is_none()
                {
                    return Err("invalid binding appearance".into());
                }
            }
        }
        if !ids.contains(&self.active_page) {
            return Err("active page does not exist".into());
        }
        Ok(())
    }
}
impl Default for ProfileData {
    // Construct the default profiles value
    fn default() -> Self {
        let actions = [
            (
                "Browser",
                "#c61e32",
                Action::OpenUrl {
                    url: "https://example.org".into(),
                },
            ),
            ("Mute", "#77202b", Action::Mute),
            (
                "Play / pause",
                "#404040",
                Action::Media {
                    command: "play-pause".into(),
                },
            ),
            ("Volume down", "#303030", Action::Volume { delta: -5 }),
            ("Volume up", "#303030", Action::Volume { delta: 5 }),
        ];
        let bindings = actions
            .into_iter()
            .enumerate()
            .map(|(i, (title, color, action))| {
                (
                    format!("key-{}", i + 1),
                    Binding {
                        live_label: Vec::new(),
                        dial_response: None,
                        title: title.into(),
                        color: format!("{color}00"),
                        custom_background: false,
                        background_source: None,
                        action,
                        asset: None,
                        media: Default::default(),
                        plugin_appearance: None,
                        composition: None,
                        appearance: None,
                        feedback: None,
                        activation_policy: Default::default(),
                        disabled: false,
                    },
                )
            })
            .collect();
        let profile = Profile {
            linked_controls: Default::default(),
            palette: None,
            parameters: vec![],
            background_asset: None,
            background_media: Default::default(),
            id: "default".into(),
            name: DEFAULT_PROFILE_NAME.into(),
            model_id: "n4-pro".into(),
            active_page: "main".into(),
            pages: vec![Page {
                id: "main".into(),
                name: "Main page".into(),
                bindings,
            }],
        };
        Self {
            minimum_manager: None,
            schema_version: 1,
            active_profile: "default".into(),
            profiles: vec![profile],
            configuration_revision: 1,

            saved_macros: vec![],

            styles: vec![],
            rules: vec![],
            history: Default::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl SourceStamp {
    // Read the requested profiles data
    pub(crate) fn read(path: &Path) -> Result<Self, String> {
        Self::read_bounded(path, crate::media::MAX_ASSET as u64)
    }
    // Read bounded
    pub(crate) fn read_bounded(path: &Path, maximum: u64) -> Result<Self, String> {
        let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
            return Err("Media source is not a bounded regular file".into());
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}
#[derive(Default)]
struct SourceCache {
    generation: u64,
    #[cfg(test)]
    clip_lookups: u64,
    clip_stamps: std::collections::HashMap<
        String,
        (SourceStamp, String, std::sync::Weak<crate::media::Clip>),
    >,
    // A stamp is only an invalidation detector. Content identity is the validated
    // SHA-256 below, never a path, logical asset id or mtime alone
    entries: std::collections::HashMap<String, (SourceStamp, String, bool)>,
    reads: u64,
    hashes: u64,
    clip_archive_reads: u64,
    clip_validation_reads: u64,
    clip_validation_hashes: u64,
}
type ClipCycleEntry = (u64, Result<Option<Arc<crate::media::Clip>>, String>);

struct DecodedFrame {
    digest: [u8; 32],
    pixels: image::DynamicImage,
    _reservation: crate::media_budget::Reservation,
}
/// Immutable validated clip handles live only for one render cycle. Explicit
/// invalidation still wins; a new cycle rechecks filesystem mutation metadata
#[derive(Default)]
pub(crate) struct ClipCycle {
    decoded: std::cell::RefCell<Option<DecodedFrame>>,
    clips: std::cell::RefCell<std::collections::HashMap<String, ClipCycleEntry>>,
}
impl ClipCycle {
    /// Borrow the decoded source only for this transform so its memory cannot
    /// escape the reservation. A declined reservation still permits uncached work.
    pub(crate) fn with_decoded<T>(
        &self,
        runtime: &crate::Runtime,
        clip: &crate::media::Clip,
        index: usize,
        edit: impl FnOnce(&image::DynamicImage) -> hardware_core::Result<T>,
    ) -> Result<T, String> {
        let digest = clip.frame_digests[index];
        let mut slot = self.decoded.borrow_mut();
        if let Some(hit) = slot.as_ref().filter(|entry| entry.digest == digest) {
            runtime.metrics.record("animation_source_decode_hits", 1.);
            return edit(&hit.pixels).map_err(|e| e.to_string());
        }
        slot.take();
        let pixels = hardware_core::render::decode_raster_pixels(&clip.frames[index])
            .map_err(|e| e.to_string())?;
        runtime.metrics.record("animation_source_decodes", 1.);
        let bytes = pixels.as_bytes().len();
        let Ok(reservation) = runtime.profiles.media_budget.reserve(bytes) else {
            return edit(&pixels).map_err(|e| e.to_string());
        };
        runtime
            .metrics
            .record("animation_decoded_source_bytes", bytes as f64);
        let entry = slot.insert(DecodedFrame {
            digest,
            pixels,
            _reservation: reservation,
        });
        edit(&entry.pixels).map_err(|e| e.to_string())
    }

    // Return the requested profiles value
    pub(crate) fn get(
        &self,
        store: &ProfileStore,
        id: &str,
    ) -> Result<Option<Arc<crate::media::Clip>>, String> {
        let generation = store.source_cache.lock().unwrap().generation;
        if let Some((saved, clip)) = self.clips.borrow().get(id) {
            if *saved == generation {
                return clip.clone();
            }
        }
        let clip = store.clip(id);
        self.clips
            .borrow_mut()
            .insert(id.into(), (generation, clip.clone()));
        clip
    }
}
pub struct ProfileStore {
    pub(crate) media_browse_cache: Mutex<crate::media_library::BrowseCache>,
    pub(crate) history_media_cache: Mutex<crate::workspace::HistoryMediaCache>,
    pub(crate) deleted_media: Mutex<crate::media_deletion::DeletedMedia>,
    pub(crate) validation_proofs: Mutex<std::collections::VecDeque<String>>,
    pub(crate) root: PathBuf,
    pub(crate) transaction: Mutex<()>,
    pub(crate) asset_leases: crate::asset_leases::AssetLeases,
    writer: Mutex<()>,
    pub(crate) storage: stream_dock_manager_storage::Storage,
    pub(crate) media_gate: crate::media::PreparationGate,
    source_content: Mutex<std::collections::HashMap<String, PathBuf>>,
    prepared_content: Mutex<std::collections::HashMap<String, PathBuf>>,
    pub(crate) media_budget: Arc<crate::media_budget::MediaBudget>,
    media_cache: Mutex<crate::media::Cache>,
    source_cache: Mutex<SourceCache>,
    pub(crate) render_cache: Mutex<crate::media::RenderCache>,
}
impl ProfileStore {
    // Open the requested profiles resource
    pub(crate) fn staging(directory: &tempfile::TempDir) -> Result<Self, String> {
        let storage = stream_dock_manager_storage::Storage::open_staging(directory.path().into())?;
        storage.add_snapshot_validator(crate::profile_database::validate_snapshot)?;
        Self::with_validated_storage(directory.path().join("profiles"), storage)
    }
    pub fn open(root: PathBuf) -> Result<Self, String> {
        let config_root = if root.file_name().is_some_and(|name| name == "profiles") {
            root.parent()
                .ok_or("Missing configuration root")?
                .to_path_buf()
        } else {
            root.clone()
        };
        let storage = stream_dock_manager_storage::Storage::open_validated(
            config_root,
            crate::application_data::validate_snapshot,
        )?;
        Self::with_validated_storage(root, storage)
    }
    // Borrow the profile store’s managed storage service
    pub fn storage(&self) -> &stream_dock_manager_storage::Storage {
        &self.storage
    }
    /// The caller registered profile snapshot validation, either alone for staging
    /// or as part of application_data::validate_snapshot for a durable workspace.
    pub(crate) fn with_validated_storage(
        root: PathBuf,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Self, String> {
        storage.run(|connection| crate::profile_database::validate_snapshot(connection))?;
        std::fs::create_dir_all(root.join("assets")).map_err(|e| e.to_string())?;
        crate::media_cleanup::recover(&root)
            .map_err(|error| format!("Media cleanup recovery failed; files preserved: {error}"))?;
        let deleted_media = crate::media_deletion::DeletedMedia::read(&storage)?;
        // The durable deletion index already detached these assets. Retry any
        // interrupted file removal without making deleted assignments live again
        let _ = deleted_media.purge(&root);
        let index: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
            std::fs::File::open(root.join("media-content.json"))
                .ok()
                .and_then(|file| serde_json::from_reader(file.take(4 * 1024 * 1024)).ok())
                .unwrap_or_default();
        let content = |name: &str| {
            index
                .get(name)
                .into_iter()
                .flatten()
                .take(8192)
                .filter_map(|(hash, name)| {
                    let path = root.join("assets").join(name);
                    let valid_hash = hash
                        .split('.')
                        .next()
                        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
                    (valid_hash
                        && !path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|id| deleted_media.assets.contains(id))
                        && Path::new(name).file_name().and_then(|v| v.to_str())
                            == Some(name.as_str())
                        && path.symlink_metadata().is_ok_and(|m| m.is_file()))
                    .then(|| (hash.clone(), path))
                })
                .collect()
        };
        let source_content = Mutex::new(content("sources"));
        let prepared_content = Mutex::new(content("prepared"));
        let media_budget = Arc::new(crate::media_budget::MediaBudget::default());
        Ok(Self {
            media_browse_cache: Default::default(),
            history_media_cache: Default::default(),
            deleted_media: Mutex::new(deleted_media),
            validation_proofs: Default::default(),
            root,
            transaction: Mutex::new(()),
            asset_leases: Default::default(),
            writer: Mutex::new(()),
            storage,
            media_gate: Default::default(),
            source_content,
            prepared_content,
            media_cache: Mutex::new(crate::media::Cache::with_budget(media_budget.clone())),
            source_cache: Default::default(),
            render_cache: Mutex::new(crate::media::RenderCache::with_budget(media_budget.clone())),
            media_budget,
        })
    }
    // Load the requested profiles state
    pub fn load(&self) -> Result<ProfileData, String> {
        self.load_for_edit(false).map(|(data, _)| data)
    }
    // Read the baseline record versions with the same workspace snapshot used by an edit
    pub(crate) fn load_for_edit(
        &self,
        with_stamp: bool,
    ) -> Result<(ProfileData, Option<crate::profile_database::WorkspaceStamp>), String> {
        use crate::profile_database::{load, WorkspaceStamp};
        let (stored, mut stamp) = self.storage.run(move |connection| {
            if !with_stamp {
                return Ok((load(connection)?, None));
            }
            let snapshot = connection
                .unchecked_transaction()
                .map_err(|e| e.to_string())?;
            let stored = load(&snapshot)?;
            let stamp = stored
                .as_ref()
                .map(|_| WorkspaceStamp::read(&snapshot))
                .transpose()?;
            snapshot.commit().map_err(|e| e.to_string())?;
            Ok((stored, stamp))
        })?;
        let mut data = match stored {
            Some(data) => data,
            None => {
                let data = self.prepared_data(&ProfileData::default())?;
                let records = crate::profile_database::PreparedRecords::new(&data)?;
                let cost = records.retained_bytes()?
                    + crate::profile_database::encoded_size(&data, 16 * 1024 * 1024)?;
                let _guard = self.writer.lock().unwrap();
                self.storage.transaction_with_cost(cost, move |tx| {
                    // Another caller may have initialized or edited the workspace
                    // after our first read. Never overwrite its committed state
                    if let Some(current) = crate::profile_database::load(tx)? {
                        return Ok(current);
                    }
                    records.commit(tx)?;
                    Ok(data)
                })?
            }
        };
        if data.schema_version != 1 {
            return Err("unsupported profile version; database preserved".into());
        }
        let normalized = crate::keyboard_navigation::normalize_for_load(&mut data)?;
        let detached = self.detach_deleted_media(&mut data);
        if normalized || detached {
            stamp = None;
        }
        if !self.proven(&data)? {
            validate_data(&data)?;
            self.remember_validation(&data)?;
        }
        Ok((data, stamp))
    }
    #[cfg(test)]
    // Read the requested profiles data
    pub(crate) fn read(&self, path: &Path) -> Result<ProfileData, String> {
        let bytes = stream_dock_manager_plugin_runtimes::bounded_io::read(
            path,
            16 * 1024 * 1024,
            "profile file too large",
        )?;
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }
        let version: Version = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if version.schema_version != 1 {
            return Err("unsupported profile version; original file preserved".into());
        }
        let mut data = parse_data(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?)?;
        self.detach_deleted_media(&mut data);
        if !self.proven(&data)? {
            validate_data(&data)?;
            self.remember_validation(&data)?;
        }
        Ok(data)
    }
    // Save the supplied profiles state
    pub fn save(&self, data: &ProfileData) -> Result<(), String> {
        let data = self.prepared_data(data)?;
        let records = crate::profile_database::PreparedRecords::new(&data)?;
        let cost = records.retained_bytes()?;
        let _guard = self.writer.lock().unwrap();
        self.storage.transaction_with_cost(cost, move |tx| {
            crate::profile_database::load(tx)?;
            records.commit(tx)
        })
    }
    // Export the selected profiles data
    pub fn export(&self, path: &Path) -> Result<(), String> {
        self.export_with_options(path, &ProfileExportOptions::default())
    }
    // Export with options
    pub fn export_with_options(
        &self,
        path: &Path,
        options: &ProfileExportOptions,
    ) -> Result<(), String> {
        self.export_with_password(path, options, None)
    }

    pub fn export_with_password(
        &self,
        path: &Path,
        options: &ProfileExportOptions,
        password: Option<&str>,
    ) -> Result<(), String> {
        stream_dock_manager_storage::vault::check_transfer_path(self.storage().root(), path)?;
        let _transaction = self.transaction.lock().unwrap();
        let data = select_profile_export(self.load()?, options)?;
        let _assets = self
            .asset_leases
            .pin(referenced_assets(&data), "Active profile export")?;
        drop(_transaction);
        if !options.include_plugin_settings {
            return self.export_archive(path, data, false);
        }
        let password = password
            .filter(|s| !s.is_empty())
            .ok_or("Enter an export password to include plugin settings.")?;
        let stage = tempfile::tempdir().map_err(|e| e.to_string())?;
        let archive = stage.path().join("archive.zip");
        self.export_archive(&archive, data, true)?;
        let mut data = zeroize::Zeroizing::new(Vec::new());
        std::fs::File::open(archive)
            .map_err(|e| e.to_string())?
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        use stream_dock_manager_storage::vault::transfer::{self, Kind};
        transfer::write(Kind::Profiles, file.as_file_mut(), &mut data, password)?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        atomic_replace(file.path(), path)
    }
    // Export archive
    pub(crate) fn export_archive(
        &self,
        path: &Path,
        mut data: ProfileData,
        include_plugin_settings: bool,
    ) -> Result<(), String> {
        stream_dock_manager_storage::vault::check_transfer_path(self.storage().root(), path)?;
        prepare_portable_export(&mut data, include_plugin_settings)?;
        validate_data(&data)?;
        let document = serde_json::to_value(&data).map_err(|e| e.to_string())?;
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary =
            tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipWriter::new(temporary.as_file_mut());
        zip.start_file(
            "profiles.json",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6)),
        )
        .map_err(|e| e.to_string())?;
        write_archive_json(&mut zip, &document)?;
        let mut contents: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut aliases = std::collections::BTreeMap::new();
        let mut expanded = 0u64;
        // profiles.json, export-notes.txt, and export-report.json are mandatory
        let mut entry_count = 3;
        for asset in referenced_assets(&data) {
            let extension = match self.asset_extension(&asset) {
                Ok(extension) => extension,
                Err(_) if !asset_required(&data, &asset) => continue,
                Err(error) => return Err(error),
            };
            let mut file = std::fs::File::open(
                self.root
                    .join("assets")
                    .join(format!("{asset}.{extension}")),
            )
            .map_err(|e| e.to_string())?;
            let size = file.metadata().map_err(|e| e.to_string())?.len();
            if size > crate::media::MAX_ASSET as u64 {
                return Err("Asset exceeds export size limit".into());
            }
            expanded += size;
            if expanded > 256 * 1024 * 1024 {
                return Err("Asset archive exceeds 256 MiB".into());
            }
            use sha2::{Digest, Sha256};
            let digest = {
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    digest.update(&buffer[..n]);
                }
                hex::encode(digest.finalize())
            };
            let content_key = format!("{digest}.{extension}");
            let original = contents.get(&content_key);
            // A repeated asset adds one aliases document the first time only
            if original.is_none() || aliases.is_empty() {
                entry_count += 1;
                if entry_count > MAX_ARCHIVE_FILES {
                    return Err("Profile archive exceeds the 256-file import limit".into());
                }
            }
            if let Some(original) = original {
                aliases.insert(asset, original.clone());
                continue;
            }
            let name = format!("assets/{asset}.{extension}");
            contents.insert(content_key, name.clone());
            use std::io::Seek;
            file.rewind().map_err(|e| e.to_string())?;
            zip.start_file(
                format!("assets/{asset}.{extension}"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(6)),
            )
            .map_err(|e| e.to_string())?;
            let copied = std::io::copy(
                &mut std::io::Read::by_ref(&mut file).take(crate::media::MAX_ASSET as u64 + 1),
                &mut zip,
            )
            .map_err(|e| e.to_string())?;
            if copied != size {
                return Err("Asset changed during export".into());
            }
        }
        crate::vendor_resources::export(
            &self.root,
            &data,
            &mut zip,
            &mut expanded,
            &mut entry_count,
        )?;
        if !aliases.is_empty() {
            zip.start_file(
                "asset-aliases.json",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(6)),
            )
            .map_err(|e| e.to_string())?;
            write_archive_json(&mut zip, &aliases)?;
        }
        zip.start_file(
            "export-notes.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6)),
        )
        .map_err(|e| e.to_string())?;
        let notes: &[u8] = if include_plugin_settings {
            b"Plugin action settings and associated resource files are included and may contain API keys, tokens, passwords, private information or local paths. Review every action and referenced macro before sharing.\n"
        } else {
            b"Plugin action settings are excluded. Commands, arguments, text, URLs, paths and profile parameter defaults may still contain keys, tokens, passwords or private information. Review every action and referenced macro before sharing.\n"
        };
        if let Some(required) = &data.minimum_manager {
            writeln!(zip, "Requires Stream Dock Manager {required} or newer.")
                .map_err(|error| error.to_string())?;
        }
        zip.write_all(notes).map_err(|e| e.to_string())?;
        zip.write_all(b"Keyboard recordings are included. They may contain sensitive keystrokes; exported workspaces and backups are not a password vault. Review recordings before sharing and before playback.\n").map_err(|e| e.to_string())?;
        let mut plugins = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for action in data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .map(|b| &b.action)
            .chain(data.saved_macros.iter().map(|m| &m.action))
        {
            portability_references(action, &mut plugins, &mut paths);
        }
        let included: HashSet<_> = data.profiles.iter().map(|p| p.id.as_str()).collect();
        let mut external_profiles = std::collections::BTreeSet::new();
        for action in data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .map(|b| &b.action)
            .chain(data.saved_macros.iter().map(|m| &m.action))
        {
            action.visit(&mut |a| {
                if let Action::Profile { profile_id } = a {
                    if !included.contains(profile_id.as_str()) {
                        external_profiles.insert(profile_id.clone());
                    }
                }
            });
        }
        zip.start_file(
            "export-report.json",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6)),
        )
        .map_err(|e| e.to_string())?;
        write_archive_json(
            &mut zip,
            &serde_json::json!({"plugins_required":plugins,"external_paths_to_resolve":paths,"plugin_settings_included":include_plugin_settings,"external_profiles_to_resolve":external_profiles}),
        )?;
        zip.finish().map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        atomic_replace(temporary.path(), path)?;
        Ok(())
    }
    // Import for repair
    pub(crate) fn import_for_repair(&self, path: &Path) -> Result<ProfileData, String> {
        self.read_portable_archive(path, false)
    }

    // Only the disposable review store may receive unresolved model/control
    // mappings. Live imports are validated again before assets or data transfer.
    pub(crate) fn read_portable_archive(
        &self,
        path: &Path,
        staged: bool,
    ) -> Result<ProfileData, String> {
        self.read_portable_with_report(path, staged)
            .map(|(data, _)| data)
    }
    pub(crate) fn read_portable_with_report(
        &self,
        path: &Path,
        staged: bool,
    ) -> Result<(ProfileData, bool), String> {
        let _preparation = self.media_gate.lock()?;
        let _reservation = self
            .media_budget
            .reserve(crate::media_budget::PREPARATION_BYTES)?;
        let (mut zip, source) = open_archive(path, "profiles.json")?;
        let omitted = settings_omitted(&mut zip)?;
        let mut document = serde_json::from_str(&source).map_err(|e| e.to_string())?;
        crate::vault::detach_portable_references(&mut document);
        let mut data = parse_data(document)?;
        if data.schema_version != 1 {
            return Err("Unsupported archive version".into());
        }
        data.rules.clear();
        data.history = Default::default();
        // Reject unsupported hardware/profile shapes before publishing any assets
        for profile in &data.profiles {
            profile.validate_with_unresolved_controls(staged)?;
        }
        Ok((self.import_archive_assets(data, zip)?, omitted))
    }
    // Import archive assets
    fn import_archive_assets(
        &self,
        mut data: ProfileData,
        mut zip: zip::ZipArchive<std::fs::File>,
    ) -> Result<ProfileData, String> {
        let aliases: BTreeMap<String, String> = {
            let mut source = vec![];
            match zip.by_name("asset-aliases.json") {
                Ok(entry) => {
                    entry
                        .take(65537)
                        .read_to_end(&mut source)
                        .map_err(|e| e.to_string())?;
                    if source.len() > 65536 {
                        return Err("Asset aliases exceed limit".into());
                    }
                    serde_json::from_slice(&source).map_err(|e| e.to_string())?
                }
                Err(_) => BTreeMap::new(),
            }
        };
        if aliases.len() > 256 {
            return Err("Too many asset aliases".into());
        }
        for (id, path) in &aliases {
            validate_asset_id(id)?;
            let parts: Vec<_> = path.split('/').collect();
            if parts.len() != 2
                || parts[0] != "assets"
                || !parts[1]
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| crate::media::ASSET_EXTENSIONS.contains(&ext))
            {
                return Err("Unsafe asset alias".into());
            }
            let target = parts[1].rsplit_once('.').unwrap().0;
            validate_asset_id(target)?;
            if !zip
                .file_names()
                .any(|name| name.is_ok_and(|name| name == path.as_str()))
            {
                return Err("Asset alias target missing".into());
            }
        }
        let resources = crate::vendor_resources::prepare_import(&data, &mut zip)?;
        let mut total = 0usize;
        let staging = tempfile::tempdir_in(self.root.join("assets")).map_err(|e| e.to_string())?;
        let mut assets = vec![];
        for asset in referenced_assets(&data) {
            validate_asset_id(&asset)?;
            let alias = aliases.get(&asset);
            let mut extensions = crate::media::ASSET_EXTENSIONS
                .into_iter()
                .filter(|extension| {
                    zip.file_names().any(|name| {
                        name.is_ok_and(|name| name == format!("assets/{asset}.{extension}"))
                    })
                });
            let direct = extensions.next();
            if extensions.next().is_some() || (direct.is_some() && alias.is_some()) {
                return Err("Duplicate asset formats in archive".into());
            }
            let extension = alias
                .and_then(|path| path.rsplit_once('.'))
                .and_then(|(_, extension)| {
                    crate::media::ASSET_EXTENSIONS
                        .into_iter()
                        .find(|candidate| *candidate == extension)
                })
                .or(direct)
                .unwrap_or("png");
            let archive_path = alias
                .cloned()
                .unwrap_or_else(|| format!("assets/{asset}.{extension}"));
            let entry = match zip.by_name(&archive_path) {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let mut staged =
                tempfile::NamedTempFile::new_in(staging.path()).map_err(|e| e.to_string())?;
            let count = std::io::copy(
                &mut entry.take(crate::media::MAX_ASSET as u64 + 1),
                staged.as_file_mut(),
            )
            .map_err(|e| e.to_string())? as usize;
            total += count;
            if count > crate::media::MAX_ASSET || total > 256 * 1024 * 1024 {
                return Err("asset archive exceeds size limits".into());
            }
            if extension == "sdmedia" {
                crate::media::Clip::from_reader_controlled(
                    std::io::BufReader::new(staged.as_file_mut()),
                    &Default::default(),
                )?;
            } else {
                let bytes =
                    crate::media_library::read_regular(staged.path(), crate::media::MAX_ASSET)?;
                let format = crate::media::detect(&bytes)?;
                if extension == "svg" && format != "svg"
                    || extension == "png" && !["png", "jpeg"].contains(&format)
                {
                    return Err("Archived media format does not match its entry".into());
                }
                if format != "svg" {
                    hardware_core::render::render(&bytes, 512, 512, 0, "png")
                        .map_err(|error| error.to_string())?;
                }
            }
            assets.push((asset, extension, staged));
        }
        crate::vendor_resources::publish_import(&self.root, &resources)?;
        for (mut asset, extension, staged) in assets {
            let existing = self.asset_extension(&asset).ok().map(|extension| {
                self.root
                    .join("assets")
                    .join(format!("{asset}.{extension}"))
            });
            if existing.is_some_and(|path| {
                SourceStamp::read(&path).is_ok() && !same_asset_bytes(&path, staged.path())
            }) {
                let replacement = uuid::Uuid::new_v4().to_string();
                for profile in &mut data.profiles {
                    if profile.background_asset.as_ref() == Some(&asset) {
                        profile.background_asset = Some(replacement.clone());
                    }
                    for binding in profile
                        .pages
                        .iter_mut()
                        .flat_map(|p| p.bindings.values_mut())
                    {
                        if binding.asset.as_ref() == Some(&asset) {
                            binding.asset = Some(replacement.clone());
                        }
                    }
                }
                asset = replacement;
            }
            let destination = self
                .root
                .join("assets")
                .join(format!("{asset}.{extension}"));
            let temporary = destination.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            std::fs::copy(staged.path(), &temporary).map_err(|e| e.to_string())?;
            atomic_replace(&temporary, &destination)?;
            // IDs are immutable: imports with conflicting contents were remapped above
            for other in crate::media::ASSET_EXTENSIONS
                .into_iter()
                .filter(|other| *other != extension)
            {
                let other = self.root.join("assets").join(format!("{asset}.{other}"));
                if other.exists() {
                    std::fs::remove_file(other).map_err(|error| error.to_string())?;
                }
            }
            self.forget_asset(&asset);
        }
        Ok(data)
    }
    // Discard the selected asset state
    pub(crate) fn forget_asset(&self, id: &str) {
        let mut sources = self.source_cache.lock().unwrap();
        sources.generation = sources.generation.wrapping_add(1);
        sources.clip_stamps.remove(id);
        sources.entries.remove(id);
        drop(sources);
        self.media_cache.lock().unwrap().remove(id);
    }
    // Import asset
    pub fn import_asset(&self, path: &Path) -> Result<String, String> {
        self.import_asset_controlled(path, &crate::media::PreparationControl::default())
    }
    // Import asset controlled
    pub fn import_asset_controlled(
        &self,
        path: &Path,
        control: &crate::media::PreparationControl,
    ) -> Result<String, String> {
        let _decode = self.media_gate.lock_cancel(&control.cancelled)?;
        self.import_asset_under_gate(path, control)
    }
    // Import asset pinned
    pub(crate) fn import_asset_pinned(
        &self,
        path: &Path,
        control: &crate::media::PreparationControl,
        reason: &str,
    ) -> Result<(String, crate::asset_leases::AssetLease), String> {
        let _decode = self.media_gate.lock_cancel(&control.cancelled)?;
        let id = self.import_asset_under_gate(path, control)?;
        let lease = self.asset_leases.pin([id.clone()], reason)?;
        Ok((id, lease))
    }
    // Publish prepared clip shared
    pub(crate) fn publish_prepared_clip_shared(
        &self,
        clip: Arc<crate::media::Clip>,
        source: &[u8],
        control: &crate::media::PreparationControl,
    ) -> Result<(String, crate::asset_leases::AssetLease), String> {
        let _decode = self.media_gate.lock_cancel(&control.cancelled)?;
        clip.validate_prepared(source, control)?;
        let mut staged =
            tempfile::NamedTempFile::new_in(self.root.join("assets")).map_err(|e| e.to_string())?;
        clip.write_archive(staged.as_file_mut(), source, control)?;
        self.publish_staged_clip(staged, clip, control)
    }
    // Publish staged clip
    fn publish_staged_clip(
        &self,
        staged: tempfile::NamedTempFile,
        clip: Arc<crate::media::Clip>,
        control: &crate::media::PreparationControl,
    ) -> Result<(String, crate::asset_leases::AssetLease), String> {
        control.check()?;
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        let hash = format!("{}.sdmedia", digest_asset(staged.path(), control)?);
        let id = uuid::Uuid::new_v4().to_string();
        let lease = self.asset_leases.pin([id.clone()], "Video preparation")?;
        let _guard = self.writer.lock().unwrap();
        control.check()?;
        let destination = self.root.join("assets").join(format!("{id}.sdmedia"));
        let existing = self.prepared_content.lock().unwrap().get(&hash).cloned();
        let existing = existing.filter(|path| same_asset_bytes(path, staged.path()));
        control.check()?;
        if let Some(existing) = existing {
            if std::fs::hard_link(&existing, &destination).is_err() {
                std::fs::copy(existing, &destination).map_err(|e| e.to_string())?;
            }
        } else {
            staged.persist(&destination).map_err(|e| e.to_string())?;
        }
        self.prepared_content
            .lock()
            .unwrap()
            .insert(hash, destination);
        self.save_content_index()?;
        // Publication is already durable. A full optional cache must not turn
        // it into a false failure or discard the caller's staged asset lease
        let _ = self.media_cache.lock().unwrap().insert(id.clone(), clip);
        Ok((id, lease))
    }
    // Import asset under gate
    fn import_asset_under_gate(
        &self,
        path: &Path,
        control: &crate::media::PreparationControl,
    ) -> Result<String, String> {
        control.check()?;
        let reservation = self
            .media_budget
            .reserve(crate::media_budget::PREPARATION_BYTES)?;
        let bytes = stream_dock_manager_storage::bounded_io::read_from(
            std::fs::File::open(path).map_err(|e| e.to_string())?,
            crate::media::MAX_SOURCE,
            "Media source exceeds 16 MiB",
        )?;
        use sha2::{Digest, Sha256};
        let source_hash = hex::encode(Sha256::digest(&bytes));
        let existing = self
            .source_content
            .lock()
            .unwrap()
            .get(&source_hash)
            .filter(|p| p.is_file())
            .cloned();
        if let Some(existing) = existing.filter(|path| self.matches_source(path, &source_hash)) {
            control.check()?;
            let _guard = self.writer.lock().unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let extension = existing
                .extension()
                .and_then(|e| e.to_str())
                .ok_or("Missing asset extension")?;
            let destination = self.root.join("assets").join(format!("{id}.{extension}"));
            if std::fs::hard_link(&existing, &destination).is_err() {
                std::fs::copy(existing, &destination).map_err(|e| e.to_string())?;
            }
            self.source_content
                .lock()
                .unwrap()
                .insert(source_hash, destination);
            self.save_content_index()?;
            return Ok(id);
        }
        let (extension, bytes, staged, clip) = match crate::media::detect(&bytes)? {
            "gif" | "mp4" => {
                let clip = Arc::new(crate::media::prepare_controlled(&bytes, control)?);
                reservation.track_clip(
                    &clip,
                    bytes.capacity() + crate::media_budget::PUBLICATION_WORK_BYTES,
                )?;
                let mut staged = tempfile::NamedTempFile::new_in(self.root.join("assets"))
                    .map_err(|e| e.to_string())?;
                clip.write_archive(staged.as_file_mut(), &bytes, control)?;
                staged.as_file().sync_all().map_err(|e| e.to_string())?;
                drop(bytes);
                ("sdmedia", Vec::new(), Some(staged), Some(clip))
            }
            "svg" => ("svg", bytes, None, None),
            _ => {
                // Validate with bounded decoding, but retain the source for non-destructive crops
                hardware_core::render::render(&bytes, 512, 512, 0, "png")
                    .map_err(|e| e.to_string())?;
                ("png", bytes, None, None)
            }
        };
        control.check()?;
        let id = uuid::Uuid::new_v4().to_string();
        let _guard = self.writer.lock().unwrap();
        let destination = self.root.join("assets").join(format!("{id}.{extension}"));
        let digest = if let Some(staged) = &staged {
            digest_asset(staged.path(), control)?
        } else {
            hex::encode(Sha256::digest(&bytes))
        };
        let hash = format!("{digest}.{extension}");
        let existing = self
            .prepared_content
            .lock()
            .unwrap()
            .get(&hash)
            .filter(|p| p.is_file())
            .cloned();
        let existing = existing
            .filter(|path| digest_asset(path, control).is_ok_and(|current| current == digest));
        control.check()?;
        if let Some(existing) = existing {
            if std::fs::hard_link(&existing, &destination).is_err() {
                std::fs::copy(existing, &destination).map_err(|e| e.to_string())?;
            }
        } else if let Some(staged) = staged {
            staged.persist(&destination).map_err(|e| e.to_string())?;
        } else {
            std::fs::write(&destination, &bytes).map_err(|e| e.to_string())?;
        }
        self.source_content
            .lock()
            .unwrap()
            .insert(source_hash, destination.clone());
        self.prepared_content
            .lock()
            .unwrap()
            .insert(hash, destination);
        self.save_content_index()?;
        if let Some(clip) = clip {
            let _ = self.media_cache.lock().unwrap().insert(id.clone(), clip);
        }
        Ok(id)
    }
    // Check for matching source
    fn matches_source(&self, path: &Path, expected: &str) -> bool {
        let result = (|| -> Result<bool, String> {
            let stamp = SourceStamp::read(path)?;
            let control = crate::media::PreparationControl::default();
            let digest = if path.extension().is_some_and(|e| e == "sdmedia") {
                let (mut file, _, _) = snapshot_asset(path, &control)?;
                let clip = crate::media::Clip::from_reader_controlled(
                    std::io::BufReader::new(&mut file),
                    &control,
                )?;
                let original = format!("original.{}", clip.source_format);
                drop(clip);
                file.rewind().map_err(|e| e.to_string())?;
                let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
                    .map_err(|e| e.to_string())?;
                let digest = digest_reader(
                    zip.by_name(&original).map_err(|e| e.to_string())?,
                    crate::media::MAX_SOURCE,
                    &control,
                )?;
                digest
            } else {
                digest_asset(path, &control)?
            };
            Ok(digest == expected && SourceStamp::read(path)? == stamp)
        })();
        result.unwrap_or(false)
    }
    // Save content index
    fn save_content_index(&self) -> Result<(), String> {
        let snapshot = |map: &Mutex<std::collections::HashMap<String, PathBuf>>| {
            let mut map = map.lock().unwrap();
            map.retain(|_, path| path.is_file());
            if map.len() > 8192 {
                map.clear()
            }
            map.iter()
                .filter_map(|(hash, path)| {
                    path.file_name()
                        .and_then(|v| v.to_str())
                        .map(|name| (hash.clone(), name.to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        };
        crate::workspace::write_json(
            &self.root.join("media-content.json"),
            &serde_json::json!({"sources":snapshot(&self.source_content),"prepared":snapshot(&self.prepared_content)}),
        )
    }
    // Read the identified asset with the configured media byte limit
    pub fn asset(&self, id: &str) -> Result<Vec<u8>, String> {
        let extension = self.asset_extension(id)?;
        stream_dock_manager_storage::bounded_io::read_from(
            std::fs::File::open(self.root.join("assets").join(format!("{id}.{extension}")))
                .map_err(|e| e.to_string())?,
            crate::media::MAX_ASSET,
            "Asset exceeds 96 MiB",
        )
    }

    // Require the managed assets path to be a regular nonsymlink directory
    fn checked_assets_directory(&self) -> Result<PathBuf, String> {
        let path = self.root.join("assets");
        let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Managed assets must be a regular directory".into());
        }
        Ok(path)
    }
    // Resolve exactly one supported asset extension while rejecting deleted or ambiguous media
    pub(crate) fn asset_extension(&self, id: &str) -> Result<&'static str, String> {
        if self.deleted_media.lock().unwrap().assets.contains(id) {
            return Err("This media was deleted. Choose another asset.".into());
        }
        validate_asset_id(id)?;
        let directory = self.checked_assets_directory()?;
        let mut extensions = crate::media::ASSET_EXTENSIONS
            .into_iter()
            .filter(|extension| directory.join(format!("{id}.{extension}")).is_file());
        match (extensions.next(), extensions.next()) {
            (Some(extension), None) => Ok(extension),
            _ => Err("Media asset is missing or ambiguous".into()),
        }
    }
    // Look up cached clip
    fn cached_clip(&self, id: &str) -> Result<Option<Arc<crate::media::Clip>>, String> {
        if self.asset_extension(id)? != "sdmedia" {
            return Ok(None);
        }
        let stamp = SourceStamp::read(&self.root.join("assets").join(format!("{id}.sdmedia")))?;
        let sources = self.source_cache.lock().unwrap();
        let Some((saved, _digest, protected)) = sources.clip_stamps.get(id) else {
            return Ok(None);
        };
        if saved != &stamp {
            return Ok(None);
        }
        let Some(clip) = self
            .media_cache
            .lock()
            .unwrap()
            .get(id)
            .or_else(|| protected.upgrade())
        else {
            return Ok(None);
        };
        #[cfg(unix)]
        {
            Ok(Some(clip))
        }
        #[cfg(not(unix))]
        {
            let expected = _digest.clone();
            let generation = sources.generation;
            // Verify once per render cycle on platforms without trustworthy
            // change timestamps. A cold/evicted clip needs only its decode read
            drop(sources);
            let digest = digest_asset(
                &self.root.join("assets").join(format!("{id}.sdmedia")),
                &crate::media::PreparationControl::default(),
            )?;
            let mut sources = self.source_cache.lock().unwrap();
            sources.clip_validation_reads += 1;
            sources.clip_validation_hashes += 1;
            if sources.generation != generation {
                return Ok(None);
            }
            Ok((digest == expected).then_some(clip))
        }
    }

    /// Content-addressed transient clips share the normal residency budget, without files
    pub(crate) fn vendor_gif(
        &self,
        id: &str,
        uri: &str,
        artwork: Option<&stream_dock_manager_plugin_host::artwork_cache::ArtworkFiles>,
        size: [u16; 2],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<crate::media::Clip>, crate::vendor_animation::PreparationFailure> {
        use crate::vendor_animation::PreparationFailure as Failure;
        if let Some(clip) = self.media_cache.lock().unwrap().get(id) {
            return Ok(clip);
        }
        let _permit = self.media_gate.try_lock().map_err(Failure::transient)?;
        if let Some(clip) = self.media_cache.lock().unwrap().get(id) {
            return Ok(clip);
        }
        let _reservation = self
            .media_budget
            .reserve(crate::media_budget::PREPARATION_BYTES)
            .map_err(Failure::transient)?;
        use base64::Engine;
        let encoded = uri
            .strip_prefix("data:image/gif;base64,")
            .ok_or_else(|| Failure::permanent("Unsupported vendor animation"))?;
        if encoded.len() > 24 * 1024 * 1024 {
            return Err(Failure::permanent("Vendor GIF exceeds source limit"));
        }
        let bytes = match artwork
            .map(|files| files.image_bytes(uri))
            .transpose()
            .map_err(Failure::transient)?
            .flatten()
        {
            Some(bytes) => bytes,
            None => base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(Failure::permanent)?,
        };
        if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            return Err(Failure::permanent("Invalid GIF source"));
        }
        // Preserve the source aspect for both crop-to-fill and contained rendering.
        // Scale only until the destination is covered; keep the old detail bound for overscan.
        let (w, h) =
            image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Gif)
                .into_dimensions()
                .map_err(Failure::permanent)?;
        let scale = (f64::from(size[0]) / f64::from(w)).max(f64::from(size[1]) / f64::from(h));
        let edge = (f64::from(w.max(h)) * scale).ceil() as u16;
        let edge = edge.clamp(1, size[0].max(size[1]).clamp(512, 1024));
        let clip = Arc::new(
            crate::media::gif_with_detail_checked(
                &bytes,
                edge,
                &Default::default(),
                cancelled,
                std::time::Duration::from_millis(500),
                512,
            )
            .map_err(Failure::permanent)?,
        );
        self.media_cache
            .lock()
            .unwrap()
            .insert(id.into(), clip.clone())
            .map_err(Failure::transient)?;
        Ok(clip)
    }
    // Reuse a cached clip or decode an sdmedia asset under the media gate
    pub fn clip(&self, id: &str) -> Result<Option<Arc<crate::media::Clip>>, String> {
        #[cfg(test)]
        {
            self.source_cache.lock().unwrap().clip_lookups += 1;
        }
        validate_asset_id(id)?;
        if let Some(clip) = self.cached_clip(id)? {
            return Ok(Some(clip));
        }
        if self.asset_extension(id)? != "sdmedia" {
            return Ok(None);
        }
        let _decode = self.media_gate.lock().unwrap();
        self.clip_under_gate(id)
    }
    /// Caller holds a regular or exclusive media permit. Never acquire another permit here
    pub(crate) fn clip_under_gate(
        &self,
        id: &str,
    ) -> Result<Option<Arc<crate::media::Clip>>, String> {
        validate_asset_id(id)?;
        if let Some(clip) = self.cached_clip(id)? {
            return Ok(Some(clip));
        }
        if self.asset_extension(id)? != "sdmedia" {
            return Ok(None);
        }
        let reservation = self
            .media_budget
            .reserve(crate::media_budget::CLIP_WORK_BYTES)?;
        let path = self.root.join("assets").join(format!("{id}.sdmedia"));
        let generation = self.source_cache.lock().unwrap().generation;
        let control = crate::media::PreparationControl::default();
        let (file, digest, stamp) = snapshot_asset(&path, &control)?;
        self.source_cache.lock().unwrap().clip_archive_reads += 1;
        let clip = Arc::new(crate::media::Clip::from_reader_controlled(
            std::io::BufReader::new(file),
            &control,
        )?);
        if SourceStamp::read(&path)? != stamp {
            return Err("Animation changed during read; retry".into());
        }
        reservation.track_clip(&clip, 0)?;
        let mut sources = self.source_cache.lock().unwrap();
        if sources.generation != generation {
            return Err("Animation invalidated during read; retry".into());
        }
        // The reservation already admitted this caller-owned clip. Cache
        // metadata pressure may decline reuse without failing the valid load
        let _ = self
            .media_cache
            .lock()
            .unwrap()
            .insert(id.into(), clip.clone());
        if sources.clip_stamps.len() >= 4096 {
            // A still-owned clip needs its original validation identity even
            // after its optional cache slot is gone. Clearing the whole index
            // would force protected playback to reallocate/reload under pressure
            sources
                .clip_stamps
                .retain(|_, (_, _, clip)| clip.strong_count() > 0);
            // Each remembered live Clip was admitted as a distinct allocation;
            // the shared coordinator bounds these owners. Keep an explicit
            // metadata ceiling as well, without retiring an existing owner
            if sources.clip_stamps.len() >= crate::media_budget::MAX_ALLOCATIONS {
                return Err("Too many live animation source identities".into());
            }
        }
        sources
            .clip_stamps
            .insert(id.into(), (stamp, digest, Arc::downgrade(&clip)));
        Ok(Some(clip))
    }
    /// A validated digest/classification is retained, never the original payload
    /// Unix mutation metadata permits a transformed-cache lookup before source I/O
    /// Other platforms fall back to bounded content verification, not mtime trust
    fn static_identity(&self, id: &str) -> Result<Option<(String, bool)>, String> {
        validate_asset_id(id)?;
        let path = self
            .checked_assets_directory()?
            .join(format!("{id}.{}", self.asset_extension(id)?));
        let stamp = SourceStamp::read(&path)?;
        #[cfg(unix)]
        {
            let sources = self.source_cache.lock().unwrap();
            if let Some((saved, digest, photographic)) = sources.entries.get(id) {
                if saved == &stamp {
                    return Ok(Some((digest.clone(), *photographic)));
                }
            }
        }
        #[cfg(not(unix))]
        let _ = stamp;
        Ok(None)
    }
    // Read and hash static media only while its file stamp and cache generation remain unchanged
    fn static_source(&self, id: &str) -> Result<(Vec<u8>, String, bool), String> {
        validate_asset_id(id)?;
        let path = self
            .checked_assets_directory()?
            .join(format!("{id}.{}", self.asset_extension(id)?));
        let stamp = SourceStamp::read(&path)?;
        let generation = self.source_cache.lock().unwrap().generation;
        let bytes = crate::media_library::read_regular(&path, crate::media::MAX_SOURCE)?;
        if SourceStamp::read(&path)? != stamp {
            return Err("Media changed during source read; retry".into());
        }
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(&bytes));
        let photographic = bytes.starts_with(&[0xff, 0xd8]);
        let mut sources = self.source_cache.lock().unwrap();
        sources.reads += 1;
        sources.hashes += 1;
        if sources.generation != generation {
            return Err("Media invalidated during source read; retry".into());
        }
        if sources.entries.len() >= 4096 {
            sources.entries.clear();
        }
        sources
            .entries
            .insert(id.into(), (stamp, digest.clone(), photographic));
        Ok((bytes, digest, photographic))
    }
    /// Resolve the selected source pixels without performing a transform. Cadence,
    /// speed and trim influence frame selection, never the transform identity
    pub(crate) fn media_pixel_identity_in_cycle(
        &self,
        id: &str,
        options: &crate::media::MediaOptions,
        elapsed: std::time::Duration,
        cycle: Option<&ClipCycle>,
    ) -> Result<String, String> {
        options.validate()?;
        let clip = match cycle {
            Some(cycle) => cycle.get(self, id)?,
            None => self.clip(id)?,
        };
        if let Some(clip) = clip {
            if options.trim_start_ms >= clip.duration_ms
                || options
                    .trim_end_ms
                    .is_some_and(|end| end > clip.duration_ms)
            {
                return Err("Trim boundaries exceed the clip duration".into());
            }
            let index = options.frame_index(&clip, elapsed);
            return Ok(format!(
                "{}:{}",
                clip.source_format,
                hex::encode(clip.frame_digests[index])
            ));
        }
        let (digest, photographic) = match self.static_identity(id)? {
            Some(value) => value,
            None => {
                let (_, digest, photographic) = self.static_source(id)?;
                (digest, photographic)
            }
        };
        Ok(format!("{photographic}:{digest}"))
    }
    // Report source-identity cache work counters without claiming retained source payload bytes
    pub fn source_work_stats(&self) -> serde_json::Value {
        let sources = self.source_cache.lock().unwrap();
        serde_json::json!({"source_reads":sources.reads,"source_hashes":sources.hashes,"source_identity_entries":sources.entries.len(),"clip_identity_entries":sources.clip_stamps.len(),"clip_archive_reads":sources.clip_archive_reads,"clip_validation_reads":sources.clip_validation_reads,"clip_validation_hashes":sources.clip_validation_hashes,"retained_source_payload_bytes":0})
    }
    // Collect media memory
    pub(crate) fn collect_media_memory(&self, memory: &mut crate::media_memory::MediaMemory) {
        self.media_cache.lock().unwrap().collect_memory(memory);
        self.render_cache.lock().unwrap().collect_memory(memory);
    }
    // Classify GIF or MP4 clips and JPEG static sources as photographic media
    pub(crate) fn asset_is_photographic(&self, id: &str) -> Result<bool, String> {
        if let Some(clip) = self.clip(id)? {
            return Ok(["gif", "mp4"].contains(&clip.source_format.as_str()));
        }
        if let Some((_, photographic)) = self.static_identity(id)? {
            return Ok(photographic);
        }
        Ok(self.static_source(id)?.2)
    }
    // Render still source preview
    pub(crate) fn render_still_source_preview(&self, id: &str) -> Result<Vec<u8>, String> {
        let _lease = self
            .asset_leases
            .pin([id.to_string()], "Photo editor source")?;
        let clip = self.clip(id)?;
        let (bytes, svg) = if let Some(clip) = &clip {
            if clip.frames.len() != 1 {
                return Err("Choose a still image to preview its source".into());
            }
            (std::borrow::Cow::Borrowed(&clip.frames[0][..]), false)
        } else {
            let svg = self.asset_extension(id)? == "svg";
            (std::borrow::Cow::Owned(self.static_source(id)?.0), svg)
        };
        let (width, height) = if svg {
            hardware_core::render::svg_media_dimensions(&bytes).map_err(|e| e.to_string())?
        } else {
            image::ImageReader::new(std::io::Cursor::new(&bytes[..]))
                .with_guessed_format()
                .and_then(|reader| reader.into_dimensions().map_err(std::io::Error::other))
                .map_err(|e| e.to_string())?
        };
        let scale = (640.0 / f64::from(width.max(height).max(1))).min(1.0);
        let dimension = |value| (f64::from(value) * scale).round().max(1.0) as u16;
        let pixels = hardware_core::render::edited_pixels(
            &bytes,
            dimension(width),
            dimension(height),
            0,
            [0.0, 0.0, 1.0, 1.0],
            false,
            None,
        )
        .map_err(|e| e.to_string())?;
        hardware_core::render::encode_pixels(pixels, "png").map_err(|e| e.to_string())
    }
    #[allow(clippy::too_many_arguments)]
    // Render media
    pub fn render_media(
        &self,
        id: &str,
        elapsed: std::time::Duration,
        width: u16,
        height: u16,
        rotation: u16,
        format: &str,
        options: &crate::media::MediaOptions,
    ) -> Result<Vec<u8>, String> {
        self.render_media_shared(id, elapsed, width, height, rotation, format, options)
            .map(|bytes| {
                {
                    let mut cache = self.render_cache.lock().unwrap();
                    cache.copied_bytes = cache.copied_bytes.saturating_add(bytes.len() as u64);
                }
                bytes.to_vec()
            })
    }
    /// Immutable internal render result. Copies are reserved for owned transport/UI adapters
    #[allow(clippy::too_many_arguments)]
    // Render media shared
    pub fn render_media_shared(
        &self,
        id: &str,
        elapsed: std::time::Duration,
        width: u16,
        height: u16,
        rotation: u16,
        format: &str,
        options: &crate::media::MediaOptions,
    ) -> Result<crate::media::SharedBytes, String> {
        self.render_media_shared_in_cycle(
            id, elapsed, width, height, rotation, format, options, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Render media shared in cycle
    pub(crate) fn render_media_shared_in_cycle(
        &self,
        id: &str,
        elapsed: std::time::Duration,
        width: u16,
        height: u16,
        rotation: u16,
        format: &str,
        options: &crate::media::MediaOptions,
        cycle: Option<&ClipCycle>,
    ) -> Result<crate::media::SharedBytes, String> {
        options.validate()?;
        let clip = match cycle {
            Some(cycle) => cycle.get(self, id)?,
            None => self.clip(id)?,
        };
        if let Some(clip) = &clip {
            if options.trim_start_ms >= clip.duration_ms
                || options
                    .trim_end_ms
                    .is_some_and(|end| end > clip.duration_ms)
            {
                return Err("Trim boundaries exceed the clip duration".into());
            }
        }
        let key_for = |content: &str| {
            format!(
                "content-v1:{content}:{width}:{height}:{rotation}:{format}:{}",
                options.pixel_key()
            )
        };
        let (source, content, checked_digest) = if let Some(clip) = &clip {
            let index = options.frame_index(clip, elapsed);
            (
                std::borrow::Cow::Borrowed(&**clip.frames[index]),
                hex::encode(clip.frame_digests[index]),
                None,
            )
        } else {
            let identity = self.static_identity(id)?;
            if let Some((digest, _)) = &identity {
                if let Some(bytes) = self
                    .render_cache
                    .lock()
                    .unwrap()
                    .get_shared(&key_for(digest))
                {
                    return Ok(bytes);
                }
            }
            // Cold/evicted transform: read and validate again, outside every cache lock
            let (source, digest, _) = self.static_source(id)?;
            (
                std::borrow::Cow::Owned(source),
                digest,
                identity.map(|(digest, _)| digest),
            )
        };
        let key = key_for(&content);
        let cached = {
            let mut cache = self.render_cache.lock().unwrap();
            // A racing renderer may have filled the same miss; don't count our
            // second lookup twice when the source identity remained unchanged
            if checked_digest.as_deref() == Some(content.as_str()) {
                cache.get_probe_shared(&key)
            } else {
                cache.get_shared(&key)
            }
        };
        if let Some(bytes) = cached {
            return Ok(bytes);
        }
        let pixels = hardware_core::render::edited_pixels(
            &source,
            width,
            height,
            rotation,
            options.crop,
            options.fit == "fit",
            clip.as_ref().map(|_| options.detail_bounds()),
        )
        .map_err(|e| e.to_string())?;
        let pixels = if options.letterbox.as_deref() == Some("black") {
            hardware_core::render::black_matte(pixels)
        } else {
            pixels
        };
        let bytes =
            hardware_core::render::encode_pixels(pixels, format).map_err(|e| e.to_string())?;
        self.render_cache.lock().unwrap().insert_shared(key, bytes)
    }
}
// Prepare portable export
pub(crate) fn prepare_portable_export(
    data: &mut ProfileData,
    include_plugin_settings: bool,
) -> Result<(), String> {
    data.update_requirements();
    data.rules.clear();
    data.history = Default::default();
    data.configuration_revision = 1;
    if !include_plugin_settings {
        for m in &mut data.saved_macros {
            redact_plugin_settings(&mut m.action);
        }
        for binding in data
            .profiles
            .iter_mut()
            .flat_map(|p| &mut p.pages)
            .flat_map(|p| p.bindings.values_mut())
        {
            redact_plugin_settings(&mut binding.action);
        }
    }
    // Local credential IDs are reserved installation metadata, never portable plugin settings
    fn strip(value: &mut serde_json::Value) {
        if value
            .as_object()
            .is_some_and(|m| m.len() == 1 && m.contains_key("$credential"))
        {
            *value = serde_json::Value::Null;
            return;
        }
        match value {
            serde_json::Value::Object(m) => {
                m.remove("$credential");
                for v in m.values_mut() {
                    strip(v);
                }
            }
            serde_json::Value::Array(a) => {
                for v in a {
                    strip(v);
                }
            }
            _ => {}
        }
    }
    for action in data.saved_macros.iter_mut().map(|m| &mut m.action).chain(
        data.profiles
            .iter_mut()
            .flat_map(|p| &mut p.pages)
            .flat_map(|p| p.bindings.values_mut())
            .map(|b| &mut b.action),
    ) {
        let result: Result<(), std::convert::Infallible> =
            action.try_visit_fields_mut(&mut |_, _| Ok(()), &mut |_, _, v| {
                strip(v);
                Ok(())
            });
        result.unwrap_or_else(|never| match never {});
    }
    let mut document = serde_json::to_value(&*data).map_err(|e| e.to_string())?;
    if crate::vault::detach_portable_references(&mut document) {
        *data = parse_data(document)?;
    }
    Ok(())
}
// Select profile export
pub(crate) fn select_profile_export(
    mut data: ProfileData,
    options: &ProfileExportOptions,
) -> Result<ProfileData, String> {
    if let Some(ids) = &options.profile_ids {
        let unique: HashSet<_> = ids.iter().collect();
        if ids.is_empty()
            || unique.len() != ids.len()
            || ids
                .iter()
                .any(|id| !data.profiles.iter().any(|p| &p.id == id))
        {
            return Err(
                "Choose existing profiles to export; the selection may have changed".into(),
            );
        }
        data.profiles.retain(|p| ids.contains(&p.id));
        if !ids.contains(&data.active_profile) {
            data.active_profile = data.profiles[0].id.clone();
        }
        data.styles.clear();
        let mut needed = HashSet::new();
        let mut scan = |a: &Action| {
            a.visit(&mut |a| {
                if let Action::Builtin { action } = a {
                    if let crate::builtins::BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                        needed.insert(macro_id.clone());
                    }
                }
            })
        };
        for binding in data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
        {
            scan(&binding.action);
        }
        loop {
            let before = needed.len();
            for m in &data.saved_macros {
                if needed.contains(&m.id) {
                    m.action.visit(&mut |a| {
                        if let Action::Builtin { action } = a {
                            if let crate::builtins::BuiltinAction::SavedMacro { macro_id, .. } =
                                &**action
                            {
                                needed.insert(macro_id.clone());
                            }
                        }
                    });
                }
            }
            if before == needed.len() {
                break;
            }
        }
        data.saved_macros.retain(|m| needed.contains(&m.id));
    }
    Ok(data)
}
// Write archive JSON
fn write_archive_json(writer: &mut impl Write, document: &impl Serialize) -> Result<(), String> {
    let mut serializer = serde_json::Serializer::with_formatter(
        &mut *writer,
        serde_json::ser::PrettyFormatter::with_indent(b"    "),
    );
    document
        .serialize(&mut serializer)
        .map_err(|e| e.to_string())?;
    writer.write_all(b"\n").map_err(|e| e.to_string())
}
// Open archive
pub(crate) fn settings_omitted(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<bool, String> {
    use zip::result::ZipError;
    let entry = match zip.by_name("export-report.json") {
        Ok(e) => e,
        Err(ZipError::FileNotFound) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    // Older archives had no settings-inclusion flag.
    #[derive(serde::Deserialize)]
    struct Report {
        plugin_settings_included: Option<bool>,
    }
    let report: Report =
        serde_json::from_reader(entry.take(16 * 1024 * 1024 + 1)).map_err(|e| e.to_string())?;
    Ok(report.plugin_settings_included == Some(false))
}

pub(crate) fn omitted_settings_warnings(
    profiles: &[Profile],
    macros: &[crate::builtins::SavedMacro],
) -> Vec<String> {
    let mut warnings = std::collections::BTreeSet::new();
    let mut check = |action: &Action, location: &str| {
        action.visit(&mut |action| {
            if let Action::Plugin { plugin_id, action_id, .. } = action {
                warnings.insert(format!(
                    "Configure {action_id} ({plugin_id}) in {location}. Its plugin settings were omitted."
                ));
            }
        });
    };
    for profile in profiles {
        for page in &profile.pages {
            for (control, binding) in &page.bindings {
                check(
                    &binding.action,
                    &format!("{} / {} / {control}", profile.name, page.name),
                );
            }
        }
    }
    for m in macros {
        check(&m.action, &format!("macro {}", m.name));
    }
    warnings.into_iter().collect()
}

pub(crate) fn open_archive(
    path: &Path,
    document: &str,
) -> Result<(zip::ZipArchive<std::fs::File>, String), String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    stream_dock_manager_plugin_runtimes::archives::validate_lzma_headers(&mut zip)?;
    if zip.len() > MAX_ARCHIVE_FILES {
        return Err("archive has too many files".into());
    }
    let mut names = HashSet::new();
    if zip.file_names().any(|name| {
        name.map(|name| !names.insert(name.into_owned()))
            .unwrap_or(true)
    }) {
        return Err("Duplicate file in archive".into());
    }
    let mut expanded = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        if entry.enclosed_name().is_none()
            || entry
                .name()
                .map_err(|e| e.to_string())?
                .contains(['\\', ':', '\0'])
            || entry
                .name()
                .map_err(|e| e.to_string())?
                .split('/')
                .any(|part| part == ".." || part == ".")
        {
            return Err("Unsafe archive path".into());
        }
        let limit = if entry.name().map_err(|e| e.to_string())? == document {
            16 * 1024 * 1024
        } else {
            crate::media::MAX_ASSET as u64
        };
        if entry.size() > limit {
            return Err("Archive entry exceeds size limit".into());
        }
        let actual = std::io::copy(&mut entry.take(limit + 1), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
        expanded = expanded
            .checked_add(actual)
            .ok_or("Archive size overflow")?;
        if actual > limit || expanded > 272 * 1024 * 1024 {
            return Err("Expanded archive exceeds size limit".into());
        }
    }
    let mut source = String::new();
    zip.by_name(document)
        .map_err(|_| format!("Archive is missing {document}"))?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > 16 * 1024 * 1024 {
        return Err("archive document exceeds 16 MiB".into());
    }
    Ok((zip, source))
}
// Validate asset ID
pub(crate) fn validate_asset_id(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "invalid asset ID")?
        .to_string()
        != id
    {
        return Err("noncanonical asset ID".into());
    }
    Ok(())
}
// Report the asset required condition for profiles
fn asset_required(data: &ProfileData, id: &str) -> bool {
    data.profiles
        .iter()
        .any(|p| p.background_asset.as_deref() == Some(id))
        || data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .any(|b| !b.disabled && b.asset.as_deref() == Some(id))
}
// Collect asset identifiers referenced by bindings and profile backgrounds
pub(crate) fn referenced_assets(data: &ProfileData) -> std::collections::BTreeSet<String> {
    data.profiles
        .iter()
        .flat_map(|p| &p.pages)
        .flat_map(|p| p.bindings.values())
        .filter_map(|b| b.asset.clone())
        .chain(
            data.profiles
                .iter()
                .filter_map(|p| p.background_asset.clone()),
        )
        .collect()
}
// Redact plugin settings
pub(crate) fn redact_plugin_settings(action: &mut Action) {
    // Clear resources
    fn clear_resources(action: &mut Action) {
        if let Action::Plugin { resources, .. } = action {
            resources.clear();
        }
        let mut index = 0;
        while let Some(child) = action.child_mut(index) {
            clear_resources(child);
            index += 1;
        }
    }
    clear_resources(action);
    let result: Result<(), std::convert::Infallible> =
        action.try_visit_fields_mut(&mut |_, _| Ok(()), &mut |_, _, settings| {
            if let Some(map) = settings.as_object_mut() {
                map.retain(|_, value| {
                    value.as_str().is_some_and(|value| {
                        crate::parameters::reference(value).is_some()
                            || crate::macro_inputs::reference(value).is_some()
                    }) || value.as_object().is_some_and(|m| {
                        m.len() == 1
                            && ["$parameter", "$input"].iter().any(|key| {
                                m.get(*key).and_then(serde_json::Value::as_str).is_some()
                            })
                    })
                });
                if map.is_empty() {
                    *settings = serde_json::Value::Null;
                }
            } else {
                *settings = serde_json::Value::Null;
            }
            Ok(())
        });
    result.unwrap_or_else(|never| match never {});
}
// Collect plugin dependencies and local filesystem paths referenced by an action
fn portability_references(
    action: &Action,
    plugins: &mut std::collections::BTreeSet<String>,
    paths: &mut std::collections::BTreeSet<String>,
) {
    let result: Result<(), std::convert::Infallible> = action.try_visit_fields(
        &mut |role, value| {
            if role.is_local_path() && !value.is_empty() {
                paths.insert(value.clone());
            }
            Ok(())
        },
        &mut |plugin_id, _, _| {
            plugins.insert(plugin_id.to_owned());
            Ok(())
        },
    );
    result.unwrap_or_else(|never| match never {});
}

// Validate data
pub(crate) fn validate_data(data: &ProfileData) -> Result<(), String> {
    if let Some(required) = &data.minimum_manager {
        let required =
            semver::Version::parse(required).map_err(|_| "Invalid minimum Manager version")?;
        if required > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap() {
            return Err(format!(
                "This workspace requires Manager {required} or newer; original file preserved"
            ));
        }
    }
    if data.schema_version != 1 {
        return Err("unsupported profile version; original file preserved".into());
    }
    if data.rules.len() > 100 {
        return Err("Workspace rule limit exceeded".into());
    }
    let mut style_ids = HashSet::new();
    if data.styles.len() > 1000 {
        return Err("Too many appearance presets".into());
    }
    for style in &data.styles {
        if style.id.is_empty()
            || !style_ids.insert(&style.id)
            || style.name.trim().is_empty()
            || style.name.len() > 160
        {
            return Err("Invalid appearance preset".into());
        }
        style.appearance.validate()?;
        if let Some(composition) = &style.composition {
            composition.validate()?;
        }
        if let Some(appearance) = &style.plugin_appearance {
            appearance.validate()?;
        }
        if style
            .color
            .as_deref()
            .is_some_and(|c| hardware_core::appearance::background_color(c).is_none())
        {
            return Err("Invalid preset background color".into());
        }
    }
    for action in data.saved_macros.iter().map(|saved| &saved.action).chain(
        data.profiles
            .iter()
            .flat_map(|profile| &profile.pages)
            .flat_map(|page| page.bindings.values())
            .map(|binding| &binding.action),
    ) {
        crate::keyboard_macro::validation::expanded_event_count(action, |id| {
            data.saved_macros
                .iter()
                .find(|saved| saved.id == id)
                .map(|saved| &saved.action)
        })?;
    }
    let mut macro_ids = HashSet::new();
    for m in &data.saved_macros {
        if m.id.is_empty() || m.name.trim().is_empty() || !macro_ids.insert(&m.id) {
            return Err("Invalid or duplicate saved macro".into());
        }
        m.action.validate_saved_macro()?;
        crate::macro_inputs::validate_definitions(&m.inputs)?;
        crate::macro_inputs::validate_action_calls(&m.action, &data.saved_macros, &m.inputs)?;
    }
    let mut rule_ids = HashSet::new();
    for rule in &data.rules {
        rule.validate()?;
        if rule.id.is_empty()
            || !rule_ids.insert(&rule.id)
            || (rule.application.trim().is_empty()
                && rule.title.is_none()
                && rule.schedule.is_none())
            || rule.application.len() > 4096
            || rule.device_key.is_empty()
            || rule.device_key.len() > 512
        {
            return Err("Invalid or duplicate application rule".into());
        }
    }
    let mut ids = HashSet::new();
    for profile in &data.profiles {
        profile.validate()?;
        for group in profile.linked_controls.values() {
            // Profile validation proved all members have the same effective assignment.
            let member = &group[0];
            let binding = &profile
                .pages
                .iter()
                .find(|p| p.id == member.page_id)
                .unwrap()
                .bindings[&member.control_id];
            crate::workspace::validate_assignment_targets(profile, &data.profiles, binding)?;
        }
        for binding in profile.pages.iter().flat_map(|page| page.bindings.values()) {
            crate::macro_inputs::validate_action_calls(&binding.action, &data.saved_macros, &[])?;
        }
        if !ids.insert(&profile.id) {
            return Err("duplicate profile ID".into());
        }
    }
    if !ids.contains(&data.active_profile)
        && !(data.profiles.is_empty() && data.active_profile.is_empty())
    {
        return Err("active profile missing".into());
    }
    Ok(())
}
pub(crate) use stream_dock_manager_storage::replace_file as atomic_replace;
impl ProfileStore {
    // Finalize media removal
    pub(crate) fn finish_media_removal(
        &self,
        removed: &std::collections::BTreeSet<String>,
        scan: &crate::media_library::MediaScan,
    ) -> Result<(), String> {
        for map in [&self.source_content, &self.prepared_content] {
            let mut map = map.lock().unwrap();
            map.retain(|_, path| {
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    return false;
                };
                if !removed.contains(id) {
                    return true;
                }
                if let Some(old) = scan.files.get(id) {
                    if let Some(other) = scan
                        .files
                        .values()
                        .find(|f| !removed.contains(&f.id) && f.identity == old.identity)
                    {
                        *path = other.path.clone();
                        return true;
                    }
                }
                false
            });
        }
        for id in removed {
            self.forget_asset(id);
        }
        self.render_cache.lock().unwrap().clear();
        self.save_content_index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that live source identity survives stamp index churn after cache removal
    fn live_source_identity_survives_stamp_index_churn_after_cache_removal() {
        let root = tempfile::tempdir().unwrap();
        let profiles = ProfileStore::open(root.path().join("app")).unwrap();
        let source = root.path().join("source.gif");
        std::fs::write(
            &source,
            crate::media::tests::isolated_gif(&crate::media::tests::sample_gif()),
        )
        .unwrap();
        let asset = profiles.import_asset(&source).unwrap();
        let playback = profiles
            .media_budget
            .protect(profiles.clip(&asset).unwrap().unwrap())
            .unwrap();
        profiles.media_cache.lock().unwrap().remove(&asset);
        let stamp = SourceStamp::read(
            &profiles
                .root
                .join("assets")
                .join(format!("{asset}.sdmedia")),
        )
        .unwrap();
        let mut held = Vec::new();
        for _ in 0..4096 {
            let clip =
                Arc::new(crate::media::Clip::new("gif", vec![vec![193; 8]], vec![40]).unwrap());
            held.push(profiles.media_budget.protect(clip.clone()).unwrap());
            profiles.source_cache.lock().unwrap().clip_stamps.insert(
                uuid::Uuid::new_v4().to_string(),
                (stamp.clone(), String::new(), Arc::downgrade(&clip)),
            );
        }
        std::fs::write(
            &source,
            crate::media::tests::isolated_gif(&crate::media::tests::sample_gif()),
        )
        .unwrap();
        let next = profiles.import_asset(&source).unwrap();
        assert!(profiles.clip(&next).unwrap().is_some());
        assert!(
            profiles.source_work_stats()["clip_identity_entries"]
                .as_u64()
                .unwrap()
                > 4096
        );
        let reads = profiles.source_work_stats()["clip_archive_reads"].clone();
        assert!(Arc::ptr_eq(
            &profiles.clip(&asset).unwrap().unwrap(),
            &playback.clip
        ));
        assert_eq!(profiles.source_work_stats()["clip_archive_reads"], reads);
        drop(held);
        std::fs::write(
            &source,
            crate::media::tests::isolated_gif(&crate::media::tests::sample_gif()),
        )
        .unwrap();
        let next = profiles.import_asset(&source).unwrap();
        assert!(profiles.clip(&next).unwrap().is_some());
        assert!(
            profiles.source_work_stats()["clip_identity_entries"]
                .as_u64()
                .unwrap()
                <= 3
        );
        assert!(Arc::ptr_eq(
            &profiles.clip(&asset).unwrap().unwrap(),
            &playback.clip
        ));
    }
    #[test]
    // Verify that durable publication and valid load succeed when optional cache metadata is full
    fn durable_publication_and_valid_load_succeed_when_optional_cache_metadata_is_full() {
        let root = tempfile::tempdir().unwrap();
        let profiles = ProfileStore::open(root.path().join("app")).unwrap();
        let mut protected = vec![];
        let mut slots = vec![];
        // Exactly exhaust the coordinator's per-slot key ceiling with live,
        // protected owners. Byte admission remains available for publication
        for count in [8192usize, 8192, 8192, 8192, 8192, 8192, 8192, 8184] {
            let frames = (0..count)
                .map(|i| (i as u64).to_le_bytes().to_vec())
                .collect();
            let clip = Arc::new(crate::media::Clip::new("gif", frames, vec![1; count]).unwrap());
            protected.push(profiles.media_budget.protect(clip.clone()).unwrap());
            slots.push(
                profiles
                    .media_budget
                    .admit(crate::media_budget::Value::Clip(clip))
                    .unwrap(),
            );
        }
        assert_eq!(
            profiles.media_budget.stats()["cache_slot_key_count"],
            65_536
        );
        let source = crate::media::tests::isolated_gif(&crate::media::tests::sample_gif());
        let archive = crate::media::prepare(&source)
            .unwrap()
            .archive(&source)
            .unwrap();
        let (asset, lease) = profiles
            .publish_prepared_clip_shared(
                Arc::new(crate::media::prepare(&source).unwrap()),
                &source,
                &Default::default(),
            )
            .unwrap();
        let path = profiles
            .root
            .join("assets")
            .join(format!("{asset}.sdmedia"));
        assert_eq!(std::fs::read(&path).unwrap(), archive);
        assert!(
            profiles.media_budget.stats()["rejections"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert!(profiles
            .asset_leases
            .lock()
            .live
            .values()
            .any(|(_, ids)| ids.contains(&asset)));
        let loaded = profiles.clip(&asset).unwrap().unwrap();
        assert_eq!(loaded.frames.len(), 2);
        // Ordinary imports already use the same optional-cache rule
        let original = root.path().join("source.gif");
        std::fs::write(&original, &source).unwrap();
        let imported = profiles.import_asset(&original).unwrap();
        assert!(profiles.clip(&imported).unwrap().is_some());
        drop((loaded, lease, slots, protected));
        profiles.media_budget.trim_pressure();
        assert_eq!(profiles.media_budget.stats()["combined_bytes"], 0);
    }
    #[test]
    // Verify that protected playback survives cache removal but never masks source replacement
    fn protected_playback_survives_cache_removal_but_never_masks_source_replacement() {
        let root = tempfile::tempdir().unwrap();
        let profiles = ProfileStore::open(root.path().join("app")).unwrap();
        let source = crate::media::tests::isolated_gif(&crate::media::tests::sample_gif());
        let path = root.path().join("source.gif");
        std::fs::write(&path, &source).unwrap();
        let asset = profiles.import_asset(&path).unwrap();
        let clip = profiles.clip(&asset).unwrap().unwrap();
        let old_frame = clip.frames[0].clone();
        let old_timing = clip.delays_us.clone();
        let mut playback = profiles.media_budget.protect(clip).unwrap();
        let reads = profiles.source_work_stats()["clip_archive_reads"]
            .as_u64()
            .unwrap();
        profiles.media_cache.lock().unwrap().remove(&asset);
        profiles.media_budget.trim_pressure();
        for _ in 0..10 {
            let validated = profiles.clip(&asset).unwrap().unwrap();
            assert!(Arc::ptr_eq(&validated, &playback.clip));
        }
        assert_eq!(profiles.source_work_stats()["clip_archive_reads"], reads);
        assert_eq!(profiles.media_budget.stats()["disposable_cache_bytes"], 0);
        let replacement = crate::media::Clip::new(
            "gif",
            playback.clip.frames.iter().map(|f| f.to_vec()).collect(),
            vec![77; playback.clip.frames.len()],
        )
        .unwrap();
        let archive = replacement.archive(&source).unwrap();
        let destination = profiles
            .root
            .join("assets")
            .join(format!("{asset}.sdmedia"));
        let staged = destination.with_extension("replacement");
        std::fs::write(&staged, archive).unwrap();
        atomic_replace(&staged, &destination).unwrap();
        let next = profiles.clip(&asset).unwrap().unwrap();
        assert!(!Arc::ptr_eq(&next, &playback.clip));
        assert_eq!(playback.clip.delays_us, old_timing);
        assert_eq!(next.delays_ms, vec![77; next.frames.len()]);
        assert_eq!(
            profiles.source_work_stats()["clip_archive_reads"],
            reads + 1
        );
        playback = profiles.media_budget.protect(next).unwrap();
        assert_eq!(&**playback.clip.frames[0], &**old_frame);
        profiles.media_cache.lock().unwrap().remove(&asset);
        drop((playback, old_frame, replacement));
        profiles.media_budget.trim_pressure();
        let stats = profiles.media_budget.stats();
        assert_eq!(stats["unique_live_bytes"], 0);
        assert_eq!(stats["tracked_allocations"], 0);
        assert_eq!(stats["cache_slots"], 0);
        assert_eq!(stats["reservations"], 0);
    }
    #[test]
    // Verify that immutable asset snapshot keeps hash and decoded timing paired after replacement
    fn immutable_asset_snapshot_keeps_hash_and_decoded_timing_paired_after_replacement() {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("master.sdmedia");
        let source = crate::media::tests::sample_gif();
        let prepared = crate::media::prepare(&source).unwrap();
        let frames = || {
            prepared
                .frames
                .iter()
                .map(|frame| frame.to_vec())
                .collect::<Vec<_>>()
        };
        let first =
            crate::media::Clip::new("gif", frames(), vec![50; prepared.frames.len()]).unwrap();
        let second =
            crate::media::Clip::new("gif", frames(), vec![60; prepared.frames.len()]).unwrap();
        // Equalize archive sizes with a valid ZIP comment, not assumptions about compressed metadata
        fn pad_zip_comment(bytes: &mut Vec<u8>, length: usize) {
            let end = bytes.len();
            assert_eq!(&bytes[end - 22..end - 18], b"PK\x05\x06");
            assert_eq!(&bytes[end - 2..], &[0, 0]);
            let padding = u16::try_from(length - end).unwrap();
            bytes[end - 2..].copy_from_slice(&padding.to_le_bytes());
            bytes.resize(length, b' ');
        }
        let mut first_bytes = first.archive(&source).unwrap();
        let mut second_bytes = second.archive(&source).unwrap();
        let length = first_bytes.len().max(second_bytes.len());
        pad_zip_comment(&mut first_bytes, length);
        pad_zip_comment(&mut second_bytes, length);
        let control = crate::media::PreparationControl::default();
        for (bytes, expected) in [(&first_bytes, &first), (&second_bytes, &second)] {
            assert_eq!(
                crate::media::Clip::from_archive_controlled(bytes, &control)
                    .unwrap()
                    .delays_us,
                expected.delays_us
            );
        }
        assert_eq!(first_bytes.len(), second_bytes.len());
        assert_ne!(first_bytes, second_bytes);
        std::fs::write(&path, &first_bytes).unwrap();
        let control = crate::media::PreparationControl::default();
        let (mut captured, hash, stamp) = snapshot_asset(&path, &control).unwrap();
        // Same-length rewrite with restored mtime models the metadata ambiguity
        // on Windows, while the captured-byte pairing is testable on every OS
        std::fs::write(&path, &second_bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(stamp.modified.unwrap())
            .unwrap();
        let loaded = crate::media::Clip::from_reader_controlled(
            std::io::BufReader::new(&mut captured),
            &control,
        )
        .unwrap();
        assert_eq!(loaded.delays_us, first.delays_us);
        assert_ne!(loaded.delays_us, second.delays_us);
        captured.rewind().unwrap();
        assert_eq!(
            digest_reader(&mut captured, crate::media::MAX_ASSET, &control).unwrap(),
            hash
        );
        assert_eq!(hash, hex::encode(Sha256::digest(&first_bytes)));
        assert_ne!(hash, hex::encode(Sha256::digest(&second_bytes)));
        assert_eq!(std::fs::read(&path).unwrap(), second_bytes);
    }

    #[test]
    // Verify that static identity does not retain large source and eviction revalidates
    fn static_identity_does_not_retain_large_source_and_eviction_revalidates() {
        use image::ImageEncoder;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("metadata-heavy.png");
        let pixels = image::RgbImage::from_pixel(16, 16, image::Rgb([11, 22, 33]));
        let mut exif = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        exif.resize(2 * 1024 * 1024, 0);
        let mut encoded = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut encoded);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(&pixels, 16, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(encoded.len() > 2 * 1024 * 1024);
        std::fs::write(&source, &encoded).unwrap();
        let store = ProfileStore::open(root.path().join("store")).unwrap();
        let id = store.import_asset(&source).unwrap();
        let render = || {
            store
                .render_media_shared(
                    &id,
                    std::time::Duration::ZERO,
                    16,
                    16,
                    0,
                    "png",
                    &Default::default(),
                )
                .unwrap()
        };
        let first = render();
        assert_eq!(store.source_work_stats()["source_reads"], 1);
        assert_eq!(
            store.source_work_stats()["retained_source_payload_bytes"],
            0
        );
        assert!(store.render_cache.lock().unwrap().resident_bytes() < 4096);
        let second = render();
        assert!(Arc::ptr_eq(&first, &second));
        #[cfg(unix)]
        assert_eq!(store.source_work_stats()["source_reads"], 1);
        let before = store.source_work_stats()["source_reads"].as_u64().unwrap();
        store.render_cache.lock().unwrap().clear();
        let after = render();
        assert_eq!(store.source_work_stats()["source_reads"], before + 1);
        assert_eq!(first, after);
        assert!(!Arc::ptr_eq(&first, &after));
        assert!(store.render_cache.lock().unwrap().resident_bytes() < 4096);
    }
    #[cfg(unix)]
    #[test]
    // Verify that warm source rejects symlinked file and asset directory
    fn warm_source_rejects_symlinked_file_and_asset_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("image.png");
        std::fs::write(
            &source,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    8,
                    8,
                    image::Rgb([5, 10, 15]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let store = ProfileStore::open(root.path().join("store")).unwrap();
        let id = store.import_asset(&source).unwrap();
        let render = || {
            store.render_media_shared(
                &id,
                std::time::Duration::ZERO,
                8,
                8,
                0,
                "png",
                &Default::default(),
            )
        };
        assert!(render().is_ok());
        let path = store.root.join("assets").join(format!("{id}.png"));
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&source, &path).unwrap();
        assert!(render().is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::copy(&source, &path).unwrap();
        assert!(render().is_ok());
        let moved = root.path().join("moved-assets");
        std::fs::rename(store.root.join("assets"), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, store.root.join("assets")).unwrap();
        assert!(render().is_err());
    }
    #[test]
    // Verify that shared render warm hit reuses digest source and allocation
    fn shared_render_warm_hit_reuses_digest_source_and_allocation() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("image.png");
        std::fs::write(
            &source,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    96,
                    96,
                    image::Rgb([10, 20, 30]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let store = ProfileStore::open(root.path().join("store")).unwrap();
        let id = store.import_asset(&source).unwrap();
        let render = || {
            store
                .render_media_shared(
                    &id,
                    std::time::Duration::ZERO,
                    96,
                    96,
                    0,
                    "png",
                    &Default::default(),
                )
                .unwrap()
        };
        let first = render();
        #[cfg(unix)]
        let before = store.source_work_stats();
        let second = render();
        assert!(Arc::ptr_eq(&first, &second));
        #[cfg(unix)]
        assert_eq!(before, store.source_work_stats());
        assert_eq!(store.render_cache.lock().unwrap().copied_bytes, 0);
        let path = store.root.join("assets").join(format!("{id}.png"));
        let replacement = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                96,
                96,
                image::Rgb([200, 100, 50]),
            )),
            "png",
        )
        .unwrap();
        let staged = path.with_extension("replacement");
        std::fs::write(&staged, &replacement).unwrap();
        atomic_replace(&staged, &path).unwrap();
        let changed = render();
        assert_ne!(first, changed);
        assert_eq!(
            image::load_from_memory(&changed)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [200, 100, 50]
        );
        store.forget_asset(&id);
        std::fs::remove_file(path).unwrap();
        assert!(store
            .render_media_shared(
                &id,
                std::time::Duration::ZERO,
                96,
                96,
                0,
                "png",
                &Default::default()
            )
            .is_err());
    }
    #[test]
    // Verify that shared render transform keys and workspace isolation
    fn shared_render_transform_keys_and_workspace_isolation() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("image.png");
        std::fs::write(
            &source,
            hardware_core::render::tile_rgba(96, 96, [10, 20, 30, 255], "A", 0, "png").unwrap(),
        )
        .unwrap();
        let store = ProfileStore::open(root.path().join("store")).unwrap();
        let id = store.import_asset(&source).unwrap();
        let base = store
            .render_media_shared(
                &id,
                std::time::Duration::ZERO,
                96,
                96,
                0,
                "png",
                &Default::default(),
            )
            .unwrap();
        let rotated = store
            .render_media_shared(
                &id,
                std::time::Duration::ZERO,
                96,
                96,
                180,
                "png",
                &Default::default(),
            )
            .unwrap();
        assert_ne!(base, rotated);
        assert!(store
            .render_media_shared(
                "../image",
                std::time::Duration::ZERO,
                96,
                96,
                0,
                "png",
                &Default::default()
            )
            .is_err());
    }
    #[test]
    // Verify that selected profile sharing includes only reachable macros and explicit settings
    fn selected_profile_sharing_includes_only_reachable_macros_and_explicit_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(dir.path().join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let mut other = data.profiles[0].clone();
        other.id = "other".into();
        other.name = "Other".into();
        let secret = Action::Plugin {
            user_desired_state: None,
            plugin_id: "example".into(),
            action_id: "action".into(),
            settings: serde_json::json!({"api_key":"private-key"}),
            resources: Default::default(),
        };
        data.saved_macros = vec![
            crate::builtins::SavedMacro {
                inputs: Default::default(),
                id: "used".into(),
                name: "Used".into(),
                action: Action::Macro {
                    actions: vec![secret.clone()],
                },
            },
            crate::builtins::SavedMacro {
                inputs: Default::default(),
                id: "unused".into(),
                name: "Unrelated".into(),
                action: secret.clone(),
            },
        ];
        let b = data.profiles[0].pages[0].bindings.get_mut("key-1").unwrap();
        b.action = Action::Macro {
            actions: vec![
                secret,
                Action::Builtin {
                    action: Box::new(crate::builtins::BuiltinAction::SavedMacro {
                        arguments: Default::default(),
                        macro_id: "used".into(),
                    }),
                },
                Action::Profile {
                    profile_id: "other".into(),
                },
            ],
        };
        let selected = data.profiles[0].id.clone();
        data.profiles.push(other);
        store.save(&data).unwrap();
        for include in [false, true] {
            let path = dir.path().join(format!("export-{include}.zip"));
            store
                .export_with_password(
                    &path,
                    &ProfileExportOptions {
                        profile_ids: Some(vec![selected.clone()]),
                        include_plugin_settings: include,
                    },
                    include.then_some("test transfer password"),
                )
                .unwrap();
            crate::importing::read_transfer(&path, Some("test transfer password"), |path| {
                let imported = store.import_for_repair(path)?;
                assert_eq!(imported.profiles.len(), 1);
                assert_eq!(imported.saved_macros.len(), 1);
                assert_eq!(imported.saved_macros[0].id, "used");
                assert_eq!(
                    serde_json::to_string(&imported)
                        .unwrap()
                        .contains("private-key"),
                    include
                );
                let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
                let report: serde_json::Value =
                    serde_json::from_reader(zip.by_name("export-report.json").unwrap()).unwrap();
                assert_eq!(report["plugin_settings_included"], include);
                assert_eq!(
                    report["external_profiles_to_resolve"],
                    serde_json::json!(["other"])
                );
                assert_eq!(report["plugins_required"], serde_json::json!(["example"]));
                Ok(())
            })
            .unwrap();
        }
        assert!(store
            .export_with_options(
                &dir.path().join("empty.zip"),
                &ProfileExportOptions {
                    profile_ids: Some(vec![]),
                    include_plugin_settings: false
                }
            )
            .is_err());
    }
    #[test]
    // Verify that render cycle retains validated clips and rechecks new cycles and invalidation
    fn render_cycle_retains_validated_clips_and_rechecks_new_cycles_and_invalidation() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().join("profiles")).unwrap();
        let source = directory.path().join("source.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let id = store.import_asset(&source).unwrap();
        let before = store.source_cache.lock().unwrap().clip_lookups;
        let cycle = ClipCycle::default();
        let first = cycle.get(&store, &id).unwrap().unwrap();
        let repeated = cycle.get(&store, &id).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &repeated));
        assert_eq!(cycle.clips.borrow().len(), 1);
        assert_eq!(store.source_cache.lock().unwrap().clip_lookups, before + 1);
        for _ in 0..4 {
            store
                .media_pixel_identity_in_cycle(
                    &id,
                    &Default::default(),
                    std::time::Duration::ZERO,
                    Some(&cycle),
                )
                .unwrap();
            store
                .render_media_shared_in_cycle(
                    &id,
                    std::time::Duration::ZERO,
                    32,
                    24,
                    0,
                    "png",
                    &Default::default(),
                    Some(&cycle),
                )
                .unwrap();
        }
        assert_eq!(
            store.source_cache.lock().unwrap().clip_lookups,
            before + 1,
            "layer identity and transform share the validated cycle handle"
        );
        let path = store.root.join("assets").join(format!("{id}.sdmedia"));
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[..4].fill(0);
        std::fs::write(&path, bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &cycle.get(&store, &id).unwrap().unwrap()
        ));
        assert!(ClipCycle::default().get(&store, &id).is_err());
        store.forget_asset(&id);
        assert!(cycle.get(&store, &id).is_err());
        assert!(
            !first.frames[0].is_empty(),
            "held frame ownership survives invalidation"
        );
    }
    #[test]
    // Verify that unsupported archive controls and boot backgrounds do not publish assets
    fn unsupported_archive_controls_and_boot_backgrounds_do_not_publish_assets() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(dir.path().join("profiles")).unwrap();
        let current = store.load().unwrap();
        for (model, id) in [
            ("n1", "key-18"),
            ("n3", "key-10"),
            ("n4", "touch-1"),
            ("293s", "key-16"),
            ("293s-v3", "key-18"),
            ("n4", "@background"),
        ] {
            let mut data = current.clone();
            let profile = &mut data.profiles[0];
            profile.model_id = model.into();
            let mut binding = profile.pages[0].bindings["key-1"].clone();
            binding.action = Action::Brightness { value: 50 };
            for page in &mut profile.pages {
                page.bindings.clear();
            }
            if id == "@background" {
                profile.background_asset = Some(uuid::Uuid::new_v4().to_string());
            } else {
                profile.pages[0].bindings.insert(id.into(), binding);
            }
            let path = dir.path().join("unsupported.zip");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&serde_json::to_vec(&data).unwrap()).unwrap();
            zip.finish().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let error = store.import_for_repair(&path).unwrap_err();
            if id == "@background" {
                assert!(error.contains("background"));
            } else {
                assert!(error.contains(model) && error.contains(id), "{error}");
            }
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert_eq!(
                std::fs::read_dir(store.root.join("assets"))
                    .unwrap()
                    .count(),
                0
            );
            assert_eq!(
                serde_json::to_value(store.load().unwrap()).unwrap(),
                serde_json::to_value(&current).unwrap()
            );
        }
    }
    #[test]
    // Verify that archive import rejects retired collections without rewriting source
    fn archive_import_rejects_retired_collections_without_rewriting_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(dir.path().join("profiles")).unwrap();
        let current = serde_json::to_value(store.load().unwrap()).unwrap();
        for key in [
            None,
            Some("variables"),
            Some("templates"),
            Some("unknown_field"),
        ] {
            let mut source = current.clone();
            if let Some(key) = key {
                source[key] = serde_json::json!([]);
            }
            let path = dir.path().join("import.zip");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&serde_json::to_vec(&source).unwrap())
                .unwrap();
            zip.finish().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let result = store.import_for_repair(&path);
            if let Some(key) = key {
                assert!(result.unwrap_err().contains(key));
            } else {
                assert_eq!(result.unwrap().profiles[0].id, current["profiles"][0]["id"]);
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                serde_json::to_value(store.load().unwrap()).unwrap(),
                current
            );
        }
    }
    #[test]
    // Verify that equal media keeps independent IDs and deduplicates after restart
    fn equal_media_keeps_independent_ids_and_deduplicates_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        std::fs::write(
            &source,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    64,
                    64,
                    image::Rgb([12, 34, 56]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let root = directory.path().join("profiles");
        let store = ProfileStore::open(root.clone()).unwrap();
        let first = store.import_asset(&source).unwrap();
        let a = root.join("assets").join(format!("{first}.png"));
        drop(store);
        let store = ProfileStore::open(root.clone()).unwrap();
        let second = store.import_asset(&source).unwrap();
        assert_ne!(first, second);
        assert_eq!(store.asset(&first).unwrap(), store.asset(&second).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(a).unwrap().ino(),
                std::fs::metadata(root.join("assets").join(format!("{second}.png")))
                    .unwrap()
                    .ino()
            );
        }
        #[cfg(not(unix))]
        let _ = a;
    }
    #[test]
    // Verify that exports respect import file count including aliases and preserve destination
    fn exports_respect_import_file_count_including_aliases_and_preserve_destination() {
        for deduplicated in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(directory.path().join("profiles")).unwrap();
            let mut data = store.load().unwrap();
            let binding = data.profiles[0].pages[0].bindings["key-1"].clone();
            data.profiles[0].pages.clear();
            data.profiles[0].active_page = "page-0".into();
            // 253 unique assets, or 252 unique assets plus an aliases document,
            // occupy the 253 slots left by the three mandatory documents
            for index in 0..253 {
                let asset = uuid::Uuid::new_v4().to_string();
                let color = if deduplicated && index == 252 {
                    0
                } else {
                    index
                };
                let bytes = hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                        2,
                        2,
                        image::Rgb([color as u8, 0, 0]),
                    )),
                    "png",
                )
                .unwrap();
                std::fs::write(
                    store.root.join("assets").join(format!("{asset}.png")),
                    bytes,
                )
                .unwrap();
                let mut binding = binding.clone();
                binding.asset = Some(asset);
                data.profiles[0].pages.push(Page {
                    id: format!("page-{index}"),
                    name: format!("Page {index}"),
                    bindings: BTreeMap::from([("key-1".into(), binding)]),
                });
            }
            store.save(&data).unwrap();
            let destination = directory.path().join("profiles.zip");
            store.export(&destination).unwrap();
            let (archive, source) = open_archive(&destination, "profiles.json").unwrap();
            assert_eq!(archive.len(), 256);
            drop(archive);
            let imported = parse_data(serde_json::from_str(&source).unwrap()).unwrap();
            assert_eq!(imported.profiles[0].pages.len(), 253);
            let accepted = std::fs::read(&destination).unwrap();

            let asset = uuid::Uuid::new_v4().to_string();
            std::fs::write(
                store.root.join("assets").join(format!("{asset}.png")),
                hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                        2,
                        2,
                        image::Rgb([255, 1, 0]),
                    )),
                    "png",
                )
                .unwrap(),
            )
            .unwrap();
            let mut binding = binding.clone();
            binding.asset = Some(asset);
            data.profiles[0].pages.push(Page {
                id: "one-too-many".into(),
                name: "One too many".into(),
                bindings: BTreeMap::from([("key-1".into(), binding)]),
            });
            store.save(&data).unwrap();
            assert!(store.export(&destination).unwrap_err().contains("256-file"));
            assert_eq!(std::fs::read(&destination).unwrap(), accepted);
        }
    }
    #[test]
    // Verify that future manager requirement preserves the configuration
    fn future_manager_requirement_preserves_the_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(directory.path().into()).unwrap();
        store.load().unwrap();
        let data = ProfileData {
            minimum_manager: Some("999.0.0".into()),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&data).unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, &bytes).unwrap();
        assert!(store.read(&path).unwrap_err().contains("requires Manager"));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    // Verify that n4 pro touch surfaces render independently of dials
    fn n4_pro_touch_surfaces_render_independently_of_dials() {
        let modules = hardware_registry::modules();
        let manifest = modules
            .iter()
            .find(|m| m.manifest().model_id == "n4-pro")
            .unwrap()
            .manifest();
        let mut data = ProfileData::default();
        let page = &mut data.profiles[0].pages[0];
        let mut dial = page.bindings["key-1"].clone();
        dial.asset = Some("dial-animation".into());
        page.bindings.insert("dial-1".into(), dial.clone());
        assert_eq!(page.surface_source(manifest, "touch-1"), "touch-1");
        page.bindings.insert(
            "touch-1".into(),
            Binding {
                live_label: Vec::new(),
                dial_response: None,
                title: String::new(),
                color: "#242424".into(),
                custom_background: false,
                background_source: None,
                action: Action::None,
                asset: None,
                media: Default::default(),
                plugin_appearance: None,
                composition: None,
                appearance: None,
                feedback: None,
                activation_policy: Default::default(),
                disabled: false,
            },
        );
        let source = page.surface_source(manifest, "touch-1");
        assert_eq!(source, "touch-1");
        assert!(page.bindings[source].asset.is_none());
        assert_eq!(page.bindings["dial-1"].asset, dial.asset);
        page.bindings.remove("touch-1");
        assert_eq!(page.surface_source(manifest, "touch-1"), "touch-1");
    }
    #[test]
    // Verify that n1 strip keeps existing button appearance bindings and explicit display overrides
    fn n1_strip_keeps_existing_button_appearance_bindings_and_explicit_display_overrides() {
        let modules = hardware_registry::modules();
        for model_id in ["n1", "n1-vsd"] {
            let manifest = modules
                .iter()
                .find(|m| m.manifest().model_id == model_id)
                .unwrap()
                .manifest();
            let mut data = ProfileData::default();
            let page = &mut data.profiles[0].pages[0];
            let mut existing = page.bindings["key-1"].clone();
            existing.title = "Existing top-button appearance".into();
            existing.color = "#2468ac".into();
            existing.asset = Some("existing-button-image".into());
            page.bindings.insert("key-16".into(), existing.clone());
            assert_eq!(page.surface_source(manifest, "display-1"), "key-16");
            let mut independent = existing.clone();
            independent.title = "Independent strip appearance".into();
            independent.action = Action::None;
            page.bindings.insert("display-1".into(), independent);
            assert_eq!(page.surface_source(manifest, "display-1"), "display-1");
            page.bindings.remove("display-1");
            assert_eq!(page.surface_source(manifest, "display-1"), "key-16");
            assert_eq!(
                serde_json::to_value(&page.bindings["key-16"]).unwrap(),
                serde_json::to_value(&existing).unwrap()
            );
        }
    }
    #[test]
    // Verify that starter backgrounds are transparent and saved solid colors are preserved
    fn starter_backgrounds_are_transparent_and_saved_solid_colors_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().into()).unwrap();
        let mut data = store.load().unwrap();
        assert!(data.profiles[0].pages[0].bindings.values().all(|binding| {
            hardware_core::appearance::background_color(&binding.color).unwrap()[3] == 0
        }));
        let binding = data.profiles[0].pages[0].bindings.get_mut("key-1").unwrap();
        binding.color = "#2468ac".into();
        store.save(&data).unwrap();
        assert_eq!(
            store.load().unwrap().profiles[0].pages[0].bindings["key-1"].color,
            "#2468ac"
        );
        let mut empty = data.profiles[0].pages[0].bindings["key-1"].clone();
        empty.title.clear();
        empty.action = Action::None;
        for color in ["#c61e3200", "#00000000", "#FFFFFF00"] {
            empty.color = color.into();
            assert!(!empty.has_content());
        }
        empty.color = "#2468ac80".into();
        assert!(empty.has_content());
    }
    #[test]
    // Verify that deletion preserves remaining selection and rejects last page
    fn deletion_preserves_remaining_selection_and_rejects_last_page() {
        let mut data = ProfileData::default();
        let mut copy = data.profiles[0].clone();
        copy.id = "copy".into();
        data.profiles.push(copy);
        data.delete_profile("copy").unwrap();
        assert_eq!(data.active_profile, "default");
        assert!(data.delete_profile("missing").is_err());
        assert_eq!(data.profiles.len(), 1);
        data.profiles[0].pages.push(Page {
            id: "extra".into(),
            name: "Extra".into(),
            bindings: BTreeMap::new(),
        });
        data.delete_page("default", "extra").unwrap();
        assert_eq!(data.profiles[0].active_page, "main");
        assert!(data.delete_page("default", "main").is_err());
        assert!(data.delete_page("default", "missing").is_err());
        assert!(data.delete_page("missing", "main").is_err());
        assert_eq!(data.profiles[0].pages.len(), 1);
        validate_data(&data).unwrap();
        data.delete_profile("default").unwrap();
        assert!(data.profiles.is_empty());
        assert!(data.active_profile.is_empty());
        validate_data(&data).unwrap();
        data.active_profile = "deleted".into();
        assert!(validate_data(&data).is_err());
    }
    #[test]
    // Verify that deleting profiles across models prefers a compatible fallback and allows none
    fn deleting_profiles_across_models_prefers_a_compatible_fallback_and_allows_none() {
        let mut data = ProfileData::default();
        let mut other = data.profiles[0].clone();
        other.id = "other-model".into();
        other.model_id = "mini".into();
        other.pages[0].bindings.clear();
        let mut copy = data.profiles[0].clone();
        copy.id = "copy".into();
        data.profiles.insert(0, other);
        data.profiles.push(copy);
        for (id, selected) in [
            ("default", "copy"),
            ("copy", "other-model"),
            ("other-model", ""),
        ] {
            data.delete_profile(id).unwrap();
            assert_eq!(data.active_profile, selected);
            validate_data(&data).unwrap();
        }
        assert!(data.profiles.is_empty());
    }
    #[test]
    // Verify that background data is rejected on startup only models
    fn background_data_is_rejected_on_startup_only_models() {
        let mut profile = ProfileData::default().profiles.remove(0);
        profile.model_id = "n4".into();
        profile
            .pages
            .iter_mut()
            .for_each(|page| page.bindings.clear());
        profile.background_asset = Some(uuid::Uuid::new_v4().to_string());
        assert!(profile.validate().is_err());
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4")
            .unwrap();
        assert!(!manifest.capabilities.background);
        assert!(manifest.capabilities.boot_logo.is_some());
        profile.model_id = "h1-pro".into();
        assert!(profile.validate().is_err());
    }
    #[test]
    // Verify that empty workspace survives restart archive history and profile creation
    fn empty_workspace_survives_restart_archive_history_and_profile_creation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("profiles");
        let store = ProfileStore::open(root.clone()).unwrap();
        let initial = store.load().unwrap();
        let original = initial.profiles[0].clone();
        let deleted = store
            .edit(initial.configuration_revision, "Delete profile", |data| {
                data.delete_profile(&original.id)
            })
            .unwrap();
        assert!(deleted.profiles.is_empty());
        let archive = directory.path().join("empty.zip");
        store.export(&archive).unwrap();
        assert!(store
            .import_for_repair(&archive)
            .unwrap()
            .profiles
            .is_empty());
        drop(store);
        let store = ProfileStore::open(root).unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.profiles.is_empty());
        assert!(loaded.active_profile.is_empty());
        let undone = store
            .history_action(loaded.configuration_revision, "undo")
            .unwrap();
        assert_eq!(undone.profiles[0].id, original.id);
        assert_eq!(undone.active_profile, original.id);
        let redone = store
            .history_action(undone.configuration_revision, "redo")
            .unwrap();
        assert!(redone.profiles.is_empty());
        let saved = store
            .edit(redone.configuration_revision, "Create profile", |data| {
                crate::workspace::apply_edits(
                    data,
                    vec![crate::workspace::Edit::SaveProfile {
                        profile: original.clone(),
                    }],
                )
            })
            .unwrap();
        assert_eq!(saved.active_profile, original.id);
        validate_data(&saved).unwrap();
    }
    #[test]
    // Verify that backup recovery and archive round trip
    fn backup_recovery_and_archive_round_trip() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let store = ProfileStore::open(root.clone()).unwrap();
        let data = store.load().unwrap();
        store.save(&data).unwrap();
        std::fs::write(root.join("profiles.json"), "broken").unwrap();
        assert_eq!(store.load().unwrap().active_profile, "default");
        let archive = root.join("export.zip");
        store.export(&archive).unwrap();
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
            for index in 0..zip.len() {
                assert_eq!(
                    zip.by_index(index).unwrap().compression(),
                    zip::CompressionMethod::Deflated
                );
            }
        }
        store.import_for_repair(&archive).unwrap();
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    // Verify that unstable control references are rejected
    fn unstable_control_references_are_rejected() {
        let mut data = ProfileData::default();
        let b = data.profiles[0].pages[0].bindings.remove("key-1").unwrap();
        data.profiles[0].pages[0]
            .bindings
            .insert("renamed-key".into(), b);
        assert!(validate_data(&data).is_err());
    }
    #[test]
    // Verify that exports include assets and redact nested plugin settings
    fn exports_include_assets_and_redact_nested_plugin_settings() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let store = ProfileStore::open(root.join("source")).unwrap();
        let mut data = ProfileData::default();
        let image = root.join("image.png");
        std::fs::write(
            &image,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    112,
                    112,
                    image::Rgb([12, 45, 80]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let asset = store.import_asset(&image).unwrap();
        let binding = data.profiles[0].pages[0].bindings.get_mut("key-1").unwrap();
        binding.asset = Some(asset.clone());
        binding.action = Action::Macro {
            actions: vec![Action::Plugin {
                user_desired_state: None,
                plugin_id: "fixture".into(),
                action_id: "action".into(),
                settings: serde_json::json!({"secret":"must-not-export"}),
                resources: Default::default(),
            }],
        };
        store.save(&data).unwrap();
        let archive = root.join("profiles.zip");
        store.export(&archive).unwrap();
        let imported = ProfileStore::open(root.join("destination")).unwrap();
        let data = imported.import_for_repair(&archive).unwrap();
        assert!(imported.asset(&asset).unwrap().len() > 10);
        assert!(!serde_json::to_string(&data)
            .unwrap()
            .contains("must-not-export"));
        assert!(imported.asset("../escape").is_err());
        drop(store);
        drop(imported);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    // Verify that configuration has one writer until owner exits
    fn configuration_has_one_writer_until_owner_exits() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let owner = ProfileStore::open(root.clone()).unwrap();
        assert!(ProfileStore::open(root.clone()).is_err());
        drop(owner);
        drop(ProfileStore::open(root.clone()).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    // Verify that animated assets and background survive portable export
    fn animated_assets_and_background_survive_portable_export() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let store = ProfileStore::open(root.join("source")).unwrap();
        let id = store.import_asset(&source).unwrap();
        let mut data = store.load().unwrap();
        data.profiles[0].background_asset = Some(id.clone());
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .asset = Some(id.clone());
        store.save(&data).unwrap();
        let archive = root.join("profile.zip");
        store.export(&archive).unwrap();
        let destination = ProfileStore::open(root.join("destination")).unwrap();
        let imported = destination.import_for_repair(&archive).unwrap();
        assert_eq!(imported.profiles[0].background_asset.as_ref(), Some(&id));
        let first = destination
            .render_media(
                &id,
                std::time::Duration::ZERO,
                112,
                112,
                0,
                "png",
                &Default::default(),
            )
            .unwrap();
        let next = destination
            .render_media(
                &id,
                std::time::Duration::from_millis(100),
                112,
                112,
                0,
                "png",
                &Default::default(),
            )
            .unwrap();
        assert_ne!(first, next);
        assert_eq!(destination.clip(&id).unwrap().unwrap().source_format, "gif");
        drop(store);
        drop(destination);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod expansion_hardware_tests {
    use super::*;
    #[test]
    // Preserve unsupported assignments through profile roundtrip and declare the export floor
    fn control_event_bindings_keep_their_slots_and_minimum_manager() {
        use crate::actions::{EventSlot, DEFAULT_HOLD_MS, DEFAULT_MULTI_PRESS_MS};
        let mut data = ProfileData::default();
        let profile = &mut data.profiles[0];
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        profile.model_id = "mini".into();
        for page in &mut profile.pages {
            page.bindings.clear();
        }
        let mut slots = vec![Action::None; EventSlot::COUNT];
        slots[EventSlot::SinglePress.index()] = Action::Mute;
        slots[EventSlot::HeldTurnLeft.index()] = Action::Volume { delta: -5 };
        binding.action = Action::Events {
            actions: slots,
            multi_press_ms: DEFAULT_MULTI_PRESS_MS,
            hold_ms: DEFAULT_HOLD_MS,
        };
        profile.pages[0].bindings.insert("key-1".into(), binding);

        data.minimum_manager = None;
        data.update_requirements();
        assert_eq!(data.minimum_manager.as_deref(), Some("0.1.1"));
        validate_data(&data).unwrap();
        let json = serde_json::to_value(data).unwrap();
        assert_eq!(json["schema_version"], 1);
        let loaded = parse_data(json.clone()).unwrap();
        validate_data(&loaded).unwrap();
        let binding = &loaded.profiles[0].pages[0].bindings["key-1"];
        assert!(matches!(
            binding.action.child(EventSlot::HeldTurnLeft.index()),
            Some(Action::Volume { delta: -5 })
        ));
        assert_eq!(serde_json::to_value(&loaded).unwrap(), json);
        let root = tempfile::tempdir().unwrap();
        let source = ProfileStore::open(root.path().join("export")).unwrap();
        let destination = ProfileStore::open(root.path().join("import")).unwrap();
        let archive = root.path().join("events.zip");
        source.export_archive(&archive, loaded, false).unwrap();
        let imported = destination.read_portable_archive(&archive, true).unwrap();
        assert_eq!(imported.minimum_manager.as_deref(), Some("0.1.1"));
        let action = &imported.profiles[0].pages[0].bindings["key-1"].action;
        assert!(matches!(
            action.child(EventSlot::SinglePress.index()),
            Some(Action::Mute)
        ));
        assert!(matches!(
            action.child(EventSlot::HeldTurnLeft.index()),
            Some(Action::Volume { delta: -5 })
        ));
        let (_, notes) = open_archive(&archive, "export-notes.txt").unwrap();
        assert!(notes.contains("Requires Stream Dock Manager 0.1.1 or newer."));
    }

    #[test]
    // Verify that baseline hardware features need no hint and retired controls are rejected
    fn baseline_hardware_features_need_no_hint_and_retired_controls_are_rejected() {
        let mut data = ProfileData::default();
        let mut binding = data.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = Action::None;
        data.profiles[0].model_id = "mini".into();
        for page in &mut data.profiles[0].pages {
            page.bindings.clear();
        }
        data.profiles[0].pages[0]
            .bindings
            .insert("key-1".into(), binding.clone());
        data.minimum_manager = None;
        data.update_requirements();
        assert_eq!(data.minimum_manager, None);
        data.profiles[0].pages[0]
            .bindings
            .insert("switch-1-left".into(), binding.clone());
        data.update_requirements();
        assert_eq!(data.minimum_manager, None);
        data.profiles[0].pages[0].bindings.remove("switch-1-left");
        data.minimum_manager = None;
        data.profiles[0].background_asset = Some("a".repeat(64));
        data.update_requirements();
        assert_eq!(data.minimum_manager, None);
        data.profiles[0].background_asset = None;
        data.minimum_manager = None;
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = Action::Lighting {
            brightness: 50,
            colors: vec![[1, 2, 3]; 12],
        };
        data.update_requirements();
        assert_eq!(data.minimum_manager, None);
        data.minimum_manager = Some("9.0.0".into());
        data.update_requirements();
        assert_eq!(data.minimum_manager.as_deref(), Some("9.0.0"));
        data.minimum_manager = None;
        data.profiles[0].model_id = "n3".into();
        data.profiles[0].pages[0].bindings.clear();
        data.profiles[0].pages[0]
            .bindings
            .insert("key-10".into(), binding);
        assert!(validate_data(&data).unwrap_err().contains("n3/key-10"));
        let restored: ProfileData =
            serde_json::from_value(serde_json::to_value(data).unwrap()).unwrap();
        assert!(validate_data(&restored).is_err());
        assert!(restored.profiles[0].pages[0]
            .bindings
            .contains_key("key-10"));
    }
}
