//! Per-binding display styling. Source values remain owned by the plugin
use crate::{appearance::IconAsset, Error, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginAppearance {
    pub plugin_id: String,
    pub action_id: String,
    pub elements: Vec<ElementStyle>,
    /// Source-backed per-state title appearance from a bundled vendor profile
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vendor_profile: Option<VendorProfileAppearance>,
}
/// Current owned representation, distinct from the external archive format
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VendorProfileAppearance {
    pub linked_title: bool,
    pub initial_state: u32,
    pub states: Vec<VendorTitleStyle>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VendorTitleStyle {
    pub font_family: String,
    pub font_size: f64,
    pub font_style: String,
    pub font_underline: bool,
    pub outline_thickness: f64,
    pub show_title: bool,
    pub title_alignment: String,
    pub title_color: String,
}
impl VendorTitleStyle {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.font_family.len() > 256
            || self.font_family.chars().any(char::is_control)
            || (!self.font_family.is_empty()
                && !crate::fonts::valid(&format!("system:{}", self.font_family)))
            || !self.font_size.is_finite()
            || !(1.0..=1024.0).contains(&self.font_size)
            || !["", "Regular", "Bold", "Italic", "Bold Italic"].contains(&self.font_style.as_str())
            || !self.outline_thickness.is_finite()
            || !(0.0..=16.0).contains(&self.outline_thickness)
            || !["top", "middle", "bottom"].contains(&self.title_alignment.as_str())
            || crate::appearance::background_color(&self.title_color).is_none()
        {
            return Err("Unsupported vendor profile title appearance".into());
        }
        Ok(())
    }
    // Resolve the display parameters for this binding
    pub fn parameters(&self) -> Value {
        json!({"fontFamily":self.font_family,"fontSize":self.font_size,"fontStyle":self.font_style,
            "fontUnderline":self.font_underline,"outlineThickness":self.outline_thickness,
            "showTitle":self.show_title,"titleAlignment":self.title_alignment,"titleColor":self.title_color})
    }
}
impl VendorProfileAppearance {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.linked_title
            || self.states.is_empty()
            || self.states.len() > 64
            || self.initial_state as usize >= self.states.len()
        {
            return Err("Invalid vendor profile states".into());
        }
        for state in &self.states {
            state.validate()?;
        }
        Ok(())
    }
    // Resolve the display parameters for this binding
    pub fn parameters(&self, state: u32) -> Option<Value> {
        self.states
            .get(state as usize)
            .map(VendorTitleStyle::parameters)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ElementStyle {
    pub layout: String,
    pub key: String,
    pub kind: String,
    /// X, Y, width and height, as percentages of the surface
    pub rect: [f64; 4],
    pub visible: bool,
    pub font: String,
    pub font_weight: u16,
    pub font_italic: bool,
    pub size: f64,
    pub foreground: String,
    pub align: String,
    pub vertical_align: String,
    pub wrap: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_asset: Option<IconAsset>,
    pub state_icons: BTreeMap<String, IconAsset>,
}
impl Default for ElementStyle {
    // Construct the default value
    fn default() -> Self {
        Self {
            layout: String::new(),
            key: String::new(),
            kind: "text".into(),
            rect: [0., 0., 100., 100.],
            visible: true,
            font: "sans".into(),
            font_weight: 400,
            font_italic: false,
            size: 12.,
            foreground: "#ffffff".into(),
            align: "center".into(),
            vertical_align: "center".into(),
            wrap: true,
            icon_asset: None,
            state_icons: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DisplayElement {
    pub label: String,
    pub value: String,
    pub style: ElementStyle,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DisplayState {
    pub index: u32,
    pub label: String,
}

// Remove background layers
pub fn remove_background_layers(visual: &mut Value) {
    if let Some(visual) = visual.as_object_mut() {
        visual.remove("encoderBackground");
    }
    if let Some(items) = visual
        .get_mut("layout")
        .and_then(|layout| layout.get_mut("items"))
        .and_then(Value::as_array_mut)
    {
        items.retain(|item| {
            !(item["type"] == "pixmap" && item["streamDockManager"]["role"] == "background")
        });
    }
}

impl ElementStyle {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.layout.is_empty()
            || self.layout.len() > 1024
            || self.key.is_empty()
            || self.key.len() > 256
            || !["text", "pixmap", "bar", "gbar", "gauge", "sparkline"]
                .contains(&self.kind.as_str())
            || !self
                .rect
                .iter()
                .all(|n| n.is_finite() && (0.0..=100.0).contains(n))
            || self.rect[2] == 0.
            || self.rect[3] == 0.
            || !self.size.is_finite()
            || !(1.0..=100.0).contains(&self.size)
            || !crate::fonts::valid(&self.font)
            || !matches!(self.font_weight, 400 | 700)
            || !["left", "center", "right"].contains(&self.align.as_str())
            || !["top", "center", "bottom"].contains(&self.vertical_align.as_str())
            || crate::appearance::background_color(&self.foreground).is_none()
            || self.state_icons.len() > 64
        {
            return Err("Invalid plugin display style".into());
        }
        for (state, asset) in &self.state_icons {
            if state.parse::<u32>().is_err() || state.len() > 10 {
                return Err("Invalid plugin display state".into());
            }
            asset.validate()?;
        }
        if let Some(asset) = &self.icon_asset {
            asset.validate()?;
        }
        Ok(())
    }
}
impl PluginAppearance {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.plugin_id.is_empty()
            || self.action_id.is_empty()
            || self.plugin_id.len() > 256
            || self.action_id.len() > 256
            || self.elements.len() > 64
        {
            return Err("Invalid plugin display identity".into());
        }
        if let Some(profile) = &self.vendor_profile {
            profile.validate()?;
        }
        let mut keys = HashSet::new();
        for element in &self.elements {
            element.validate()?;
            if !keys.insert((&element.layout, &element.key, &element.kind)) {
                return Err("Duplicate plugin display element".into());
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err("Plugin display style exceeds 4 MiB".into());
        }
        Ok(())
    }
}
// Compute the identity of this display configuration
fn identity(visual: &Value) -> String {
    if visual["layout"].is_object() {
        visual["layout"]["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| visual["layoutReference"].as_str())
            .unwrap_or("$layout")
            .into()
    } else {
        "$title-image".into()
    }
}
// Read the display items from the layout
fn items(visual: &Value) -> Vec<Value> {
    if let Some(items) = visual["layout"]["items"].as_array() {
        items
            .iter()
            .take(64)
            .map(|item| {
                let mut item = item.clone();
                let key = item["key"].as_str().unwrap_or("");
                if let Some(value) = visual["feedback"].get(key) {
                    if let Some(properties) = value.as_object() {
                        for (key, value) in properties {
                            item[key] = value.clone();
                        }
                    } else {
                        item["value"] = value.clone();
                    }
                }
                item
            })
            .collect()
    } else {
        let mut items = vec![];
        if visual["image"].is_string() {
            items.push(json!({"key":"image","label":"Plugin image","type":"pixmap","rect":[0,0,200,100],"value":visual["image"]}));
        }
        if visual["title"].as_str().is_some_and(|s| !s.is_empty()) || !visual["image"].is_string() {
            items.push(json!({"key":"title","label":"Plugin title","type":"text","rect":[0,0,200,100],"value":visual["title"],"__manager_original_title":true,"__vendor_title_parameters":visual["titleParameters"]}));
        }
        items
    }
}
// Read the groups from the display layout
fn groups(visual: &Value) -> Vec<Value> {
    visual["layout"]["streamDockManager"]["text_groups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .take(8)
                .filter(|g| {
                    g["key"]
                        .as_str()
                        .is_some_and(|key| !key.is_empty() && key.len() <= 256)
                        && g["source"] == "title"
                        && g["keys"].as_array().is_some_and(|keys| {
                            !keys.is_empty()
                                && keys.len() <= 64
                                && keys.iter().all(|key| key.is_string())
                        })
                        && g["rect"].as_array().is_some_and(|rect| {
                            rect.len() == 4
                                && rect
                                    .iter()
                                    .all(|n| n.as_f64().is_some_and(|n| n.is_finite() && n >= 0.))
                                && rect[2].as_f64().is_some_and(|n| n > 0.)
                                && rect[3].as_f64().is_some_and(|n| n > 0.)
                        })
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
// Resolve one item within a display group
fn group_item(group: &Value, visual: &Value) -> Value {
    json!({"key":group["key"],"label":group["label"],"type":"text","rect":group["rect"],"font":{"size":8},"value":visual["title"]})
}
// Render items
pub fn render_items(visual: &Value) -> Vec<Value> {
    let groups = groups(visual);
    let grouped: HashSet<&str> = groups
        .iter()
        .flat_map(|g| {
            g["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect();
    let mut values: Vec<_> = items(visual)
        .into_iter()
        .filter(|i| !grouped.contains(i["key"].as_str().unwrap_or("")))
        .collect();
    values.extend(groups.iter().map(|g| group_item(g, visual)));
    values
}
// Resolve the values supplied to layout elements
pub fn element_values(visual: &Value) -> Vec<(String, String, Value)> {
    render_items(visual)
        .into_iter()
        .map(|i| {
            (
                i["key"].as_str().unwrap_or("").into(),
                i["type"].as_str().unwrap_or("").into(),
                i["value"].clone(),
            )
        })
        .collect()
}
/// Only scalar fields in rendered semantic items may supply authored labels.
pub fn scalar_field(visual: &Value, key: &str) -> Option<Value> {
    items(visual)
        .into_iter()
        .find(|item| {
            item["key"].as_str() == Some(key)
                && matches!(
                    item["type"].as_str(),
                    Some("text" | "bar" | "gbar" | "gauge")
                )
        })
        .and_then(|item| {
            let v = &item["value"];
            (v.is_boolean() || v.is_number() || v.is_string()).then(|| v.clone())
        })
}
// Read the elements from the display layout
pub fn elements(visual: &Value) -> Vec<DisplayElement> {
    let layout = identity(visual);
    let groups = groups(visual);
    let grouped: HashSet<&str> = groups
        .iter()
        .flat_map(|g| {
            g["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect();
    let mut items: Vec<_> = items(visual)
        .into_iter()
        .filter(|i| !grouped.contains(i["key"].as_str().unwrap_or("")))
        .collect();
    items.extend(groups.iter().map(|g| group_item(g, visual)));
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            let kind = item["type"].as_str()?;
            if !["text", "pixmap", "bar", "gbar", "gauge", "sparkline"].contains(&kind) {
                return None;
            }
            let rect = item["rect"].as_array()?;
            if rect.len() != 4 {
                return None;
            }
            let key = item["key"].as_str()?.to_string();
            if key.is_empty() || !seen.insert((key.clone(), kind.to_string())) {
                return None;
            }
            let style = ElementStyle {
                layout: layout.clone(),
                key: key.clone(),
                kind: kind.into(),
                rect: std::array::from_fn(|i| {
                    (rect[i].as_f64().unwrap_or(0.) / if i % 2 == 0 { 2. } else { 1. })
                        .clamp(0., 100.)
                }),
                size: item["font"]["size"].as_f64().unwrap_or(16.).clamp(1., 100.),
                foreground: item["color"]
                    .as_str()
                    .filter(|c| crate::appearance::background_color(c).is_some())
                    .unwrap_or("#edf2fc")
                    .into(),
                visible: rect[2].as_f64().unwrap_or(0.) > 0. && rect[3].as_f64().unwrap_or(0.) > 0.,
                ..Default::default()
            };
            let mut style = style;
            style.rect[2] = style.rect[2].max(0.5);
            style.rect[3] = style.rect[3].max(0.5);
            Some(DisplayElement {
                label: item["label"]
                    .as_str()
                    .unwrap_or(&key)
                    .chars()
                    .take(160)
                    .collect(),
                value: if kind == "pixmap" {
                    "Live plugin image".into()
                } else if let Some(value) = item["value"].as_str() {
                    value.chars().take(4096).collect()
                } else if item["value"].is_null() {
                    String::new()
                } else {
                    item["value"].to_string().chars().take(4096).collect()
                },
                style,
            })
        })
        .take(64)
        .collect()
}
/// Merge overrides only after feedback properties, so an update cannot erase a user style
pub fn styled_visual(visual: &Value, appearance: &PluginAppearance) -> Result<Value> {
    appearance.validate().map_err(Error::Protocol)?;
    if visual["plugin_id"] != appearance.plugin_id || visual["action_id"] != appearance.action_id {
        return Ok(visual.clone());
    }
    let projected;
    let visual = if let Some(profile) = &appearance.vendor_profile {
        projected = {
            let mut value = visual.clone();
            let state = value["state"]
                .as_u64()
                .unwrap_or(u64::from(profile.initial_state));
            if let Some(parameters) = u32::try_from(state)
                .ok()
                .and_then(|state| profile.parameters(state))
            {
                value["titleParameters"] = parameters;
            }
            value
        };
        &projected
    } else {
        visual
    };
    let layout = identity(visual);
    let styles: Vec<_> = appearance
        .elements
        .iter()
        .filter(|s| s.layout == layout)
        .collect();
    if styles.is_empty() {
        return Ok(visual.clone());
    }
    let mut items = items(visual);
    if !styles.iter().any(|s| {
        items
            .iter()
            .any(|i| i["key"] == s.key && i["type"] == s.kind)
            || groups(visual)
                .iter()
                .any(|g| g["key"] == s.key && s.kind == "text")
    }) {
        return Ok(visual.clone());
    }
    for group in groups(visual) {
        if styles
            .iter()
            .any(|s| s.key == group["key"] && s.kind == "text")
        {
            items.retain(|i| !group["keys"].as_array().unwrap().contains(&i["key"]));
            items.push(group_item(&group, visual));
        }
    }
    for item in &mut items {
        let Some(style) = styles
            .iter()
            .find(|s| item["key"] == s.key && item["type"] == s.kind)
        else {
            continue;
        };
        item["rect"] = json!([
            style.rect[0] * 2.,
            style.rect[1],
            if style.visible {
                style.rect[2] * 2.
            } else {
                0.
            },
            style.rect[3]
        ]);
        item["__manager_style"] =
            serde_json::to_value(style).map_err(|e| Error::Protocol(e.to_string()))?;
        if style.kind == "pixmap" {
            let state = visual["state"].as_u64().map(|s| s.to_string());
            if let Some(asset) = state
                .as_ref()
                .and_then(|s| style.state_icons.get(s))
                .or(style.icon_asset.as_ref())
            {
                item["value"] = json!(format!(
                    "data:image/svg+xml;base64,{}",
                    base64::engine::general_purpose::STANDARD
                        .encode(asset.svg.replace("currentColor", &style.foreground))
                ));
            }
        }
    }
    let mut result = visual.clone();
    if !result["layout"].is_object() {
        result["layout"] = json!({"background":visual["background"].as_str().unwrap_or("#415a82")});
    }
    result["layout"]["items"] = json!(items);
    result["feedback"] = json!({});
    Ok(result)
}
/// Draw one customized field with the same host/bundled font used for its metrics
pub(crate) fn text_image(
    text: &str,
    style: &ElementStyle,
    width: u32,
    height: u32,
    surface_height: u16,
) -> Result<image::RgbaImage> {
    text_image_fitted(text, style, width, height, surface_height, false)
}
// Render text fitted to the target image dimensions
pub(crate) fn text_image_fitted(
    text: &str,
    style: &ElementStyle,
    width: u32,
    height: u32,
    surface_height: u16,
    fit: bool,
) -> Result<image::RgbaImage> {
    text_image_details(text, style, width, height, surface_height, fit).map(|(pixels, _)| pixels)
}
// Render text and return its layout details
fn text_image_details(
    text: &str,
    style: &ElementStyle,
    width: u32,
    height: u32,
    surface_height: u16,
    fit: bool,
) -> Result<(image::RgbaImage, bool)> {
    style.validate().map_err(Error::Protocol)?;
    let db = crate::appearance::fonts(&style.font);
    let mut size = (f64::from(surface_height) * style.size / 100.).max(1.);
    let text: String = text.chars().take(4096).collect();
    let mut advances = crate::fonts::styled_advances(
        &style.font,
        &text,
        size,
        &db,
        style.font_weight,
        style.font_italic,
    );
    if fit {
        let longest = text
            .split('\n')
            .map(|line| {
                line.chars()
                    .map(|ch| advances.get(&ch).copied().unwrap_or(size * 0.6))
                    .sum::<f64>()
            })
            .fold(0.0_f64, f64::max);
        let lines = text.split('\n').count().max(1) as f64;
        let scale = (f64::from(width) * 0.84 / longest.max(1.))
            .min(f64::from(height) * 0.96 / (size * 1.2 * lines))
            .min(1.);
        let fitted_size = (size * scale).max(1.);
        // Ordinary values already fit. Reuse their metrics instead of parsing
        // the same font and allocating the same advance map a second time.
        if fitted_size != size {
            size = fitted_size;
            advances = crate::fonts::styled_advances(
                &style.font,
                &text,
                size,
                &db,
                style.font_weight,
                style.font_italic,
            );
        }
    }
    let advance = |ch| advances.get(&ch).copied().unwrap_or(size * 0.6);
    let mut lines = Vec::new();
    let mut truncated = false;
    let max_lines = ((f64::from(height) / (size * 1.2)).floor() as usize).clamp(1, 1024);
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut length = 0.;
        for ch in paragraph.chars() {
            let next = advance(ch);
            if style.wrap && !line.is_empty() && length + next > f64::from(width) {
                if let Some(index) = line.rfind(char::is_whitespace) {
                    let rest = line[index..].trim_start().to_owned();
                    lines.push(line[..index].to_owned());
                    line = rest;
                    length = line.chars().map(advance).sum();
                } else {
                    lines.push(std::mem::take(&mut line));
                    length = 0.;
                }
                if lines.len() >= max_lines {
                    truncated = true;
                    break;
                }
            }
            line.push(ch);
            length += next;
        }
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }
        lines.push(line);
    }
    lines.truncate(max_lines);
    let line_height = size * 1.2;
    let block = line_height * lines.len() as f64;
    let top = match style.vertical_align.as_str() {
        "top" => 0.,
        "bottom" => (f64::from(height) - block).max(0.),
        _ => ((f64::from(height) - block) / 2.).max(0.),
    };
    let (x, anchor) = match style.align.as_str() {
        "left" => (if fit { f64::from(width) * 0.08 } else { 0. }, "start"),
        "right" => (f64::from(width) * if fit { 0.92 } else { 1. }, "end"),
        _ => (f64::from(width) / 2., "middle"),
    };
    let family = crate::fonts::svg_family(&style.font, &db);
    let weight = if style.font_weight == 700 {
        "bold"
    } else {
        "normal"
    };
    let slant = if style.font_italic {
        "italic"
    } else {
        "normal"
    };
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        body.push_str(&format!("<text x=\"{x}\" y=\"{}\" text-anchor=\"{anchor}\" font-family=\"{family}\" font-weight=\"{weight}\" font-style=\"{slant}\" font-size=\"{size}\" fill=\"{}\">{}</text>",top+size+i as f64*line_height,style.foreground,line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")));
    }
    let options = resvg::usvg::Options {
        fontdb: db,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    let mut options = options;
    let source = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">{body}</svg>"
    );
    #[cfg(target_os = "macos")]
    let (source, native_clipped) =
        crate::native_text::prepare_svg(&source, &style.font, &mut options)?;
    #[cfg(not(target_os = "macos"))]
    let native_clipped = false;
    let tree = resvg::usvg::Tree::from_str(&source, &options)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let clipped = truncated
        || native_clipped
        || crate::appearance::glyphs_clipped(tree.root(), width, height);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| Error::Protocol("Invalid text bounds".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok((crate::render::pixmap_rgba(pixmap)?, clipped))
}

/// Advisory checks use the same wrapped glyph layout as composition output
pub fn text_checks(
    background: &image::RgbaImage,
    text: &str,
    style: &ElementStyle,
    surface_height: u16,
    animated: bool,
) -> Result<Vec<String>> {
    if text.is_empty() {
        return Ok(vec![]);
    }
    let (mask, clipped) = text_image_details(
        text,
        style,
        background.width(),
        background.height(),
        surface_height,
        false,
    )?;
    let mut warnings = Vec::new();
    if clipped {
        warnings.push("Some label glyphs are clipped by this layer's bounds.".into());
    }
    if crate::appearance::low_contrast(
        background,
        mask.as_raw().as_chunks::<4>().0.iter().map(|p| p[3]),
        &style.foreground,
    ) {
        warnings.push(crate::appearance::contrast_warning(animated));
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify only explicit background roles are removed
    fn only_explicit_background_roles_are_removed() {
        let mut visual = json!({"layout":{"items":[
            {"key":"background","type":"pixmap"},
            {"key":"backdrop","type":"pixmap"},
            {"key":"caption-background","type":"pixmap"},
            {"key":"authored","type":"pixmap","streamDockManager":{"role":"background"}},
            {"key":"caption","type":"text","streamDockManager":{"role":"background"}}
        ]}});
        remove_background_layers(&mut visual);
        let keys: Vec<_> = visual["layout"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            ["background", "backdrop", "caption-background", "caption"]
        );
    }

    // Resolve the visual value for this operation
    fn visual() -> Value {
        json!({"plugin_id":"test","action_id":"counter","state":0,"layout":{"id":"counter","background":"#00000000","items":[{"key":"value","type":"text","rect":[0,0,200,60]},{"key":"icon","type":"pixmap","rect":[0,60,60,40]}]},"feedback":{"value":{"value":"Live 23%","rect":[0,0,200,60]},"icon":""}})
    }
    // Resolve the visual style for this display
    fn style(visual: &Value) -> PluginAppearance {
        PluginAppearance {
            vendor_profile: None,
            plugin_id: "test".into(),
            action_id: "counter".into(),
            elements: vec![elements(visual)[0].style.clone()],
        }
    }
    // Resolve the asset value for this operation
    fn asset(id: &str, color: &str) -> IconAsset {
        IconAsset {
            id: id.into(),
            svg: format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 40 20\"><rect width=\"40\" height=\"20\" fill=\"{color}\"/></svg>"
            ),
            collection: "Test".into(),
            author: "Test".into(),
            license: "MIT".into(),
            license_url: String::new(),
        }
    }
    #[test]
    // Verify merges after feedback preserves source and updates
    fn merges_after_feedback_preserves_source_and_updates() {
        let mut visual = visual();
        let original = visual.clone();
        let mut appearance = style(&visual);
        appearance.elements[0].rect = [10., 20., 80., 50.];
        appearance.elements[0].size = 14.;
        appearance.elements[0].font = "mono".into();
        let first = styled_visual(&visual, &appearance).unwrap();
        assert_eq!(visual, original);
        assert_eq!(
            first["layout"]["items"][0]["rect"],
            json!([20., 20., 160., 50.])
        );
        assert_eq!(first["layout"]["items"][0]["value"], "Live 23%");
        visual["feedback"]["value"] = json!({"value":"Live 87%","rect":[0,0,200,60]});
        let second = styled_visual(&visual, &appearance).unwrap();
        assert_eq!(second["layout"]["items"][0]["value"], "Live 87%");
        assert_eq!(
            first["layout"]["items"][0]["__manager_style"],
            second["layout"]["items"][0]["__manager_style"]
        );
        assert_ne!(
            crate::render::visual(&first, 112, 112, 0).unwrap(),
            crate::render::visual(&second, 112, 112, 0).unwrap()
        );
    }
    #[test]
    // Verify state icons fall back without mutating text or source
    fn state_icons_fall_back_without_mutating_text_or_source() {
        let mut visual = visual();
        let mut appearance = style(&visual);
        appearance.elements = vec![elements(&visual)[1].style.clone()];
        appearance.elements[0].icon_asset = Some(asset("test:default", "#ff0000"));
        appearance.elements[0]
            .state_icons
            .insert("1".into(), asset("test:odd", "#00ff00"));
        let first = styled_visual(&visual, &appearance).unwrap();
        visual["state"] = json!(1);
        let second = styled_visual(&visual, &appearance).unwrap();
        assert_ne!(
            first["layout"]["items"][1]["value"],
            second["layout"]["items"][1]["value"]
        );
        assert_eq!(second["layout"]["items"][0]["value"], "Live 23%");
        visual["state"] = json!(2);
        assert_eq!(
            styled_visual(&visual, &appearance).unwrap()["layout"]["items"][1]["value"],
            first["layout"]["items"][1]["value"]
        );
        let bytes = crate::render::visual(&second, 112, 112, 90).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0)[3], 0);
    }
    #[test]
    // Verify full text group replaces rows only when customized
    fn full_text_group_replaces_rows_only_when_customized() {
        let mut visual = visual();
        visual["title"] = json!("Full original text that was truncated by the plugin");
        visual["layout"]["items"] = json!([{"key":"line0","type":"text","rect":[0,0,200,30]},{"key":"line1","type":"text","rect":[0,30,200,30]}]);
        visual["layout"]["streamDockManager"] = json!({"text_groups":[{"key":"live_text","source":"title","label":"Live text","keys":["line0","line1"],"rect":[0,0,200,60]}]});
        let fields = elements(&visual);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value, visual["title"]);
        let appearance = style(&visual);
        let styled = styled_visual(&visual, &appearance).unwrap();
        assert_eq!(styled["layout"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(styled["layout"]["items"][0]["value"], visual["title"]);
        assert_eq!(visual["layout"]["items"].as_array().unwrap().len(), 2);
    }
    #[test]
    // Verify unrelated identity layout and type leave visual unchanged
    fn unrelated_identity_layout_and_type_leave_visual_unchanged() {
        let visual = visual();
        let mut appearance = style(&visual);
        appearance.action_id = "other".into();
        assert_eq!(styled_visual(&visual, &appearance).unwrap(), visual);
        appearance.action_id = "counter".into();
        appearance.elements[0].layout = "other".into();
        assert_eq!(styled_visual(&visual, &appearance).unwrap(), visual);
        appearance.elements[0].layout = "counter".into();
        appearance.elements[0].kind = "pixmap".into();
        assert_eq!(styled_visual(&visual, &appearance).unwrap(), visual);
    }
    #[test]
    // Verify title and image are editable independently
    fn title_and_image_are_editable_independently() {
        let visual = json!({"plugin_id":"test","action_id":"counter","title":"Plugin text","image":"","background":"#00000000"});
        assert_eq!(elements(&visual).len(), 2);
        let mut appearance = style(&visual);
        appearance.elements = vec![elements(&visual)[1].style.clone()];
        appearance.elements[0].size = 15.;
        let styled = styled_visual(&visual, &appearance).unwrap();
        assert_eq!(styled["layout"]["items"][1]["value"], "Plugin text");
    }
    #[test]
    // Verify bundled fonts wrap escape and clip to field
    fn bundled_fonts_wrap_escape_and_clip_to_field() {
        let style = ElementStyle {
            layout: "test".into(),
            key: "text".into(),
            size: 14.,
            align: "left".into(),
            vertical_align: "top".into(),
            ..Default::default()
        };
        let wrapped = text_image(
            "Wide <&>\nUnicode café and a long label",
            &style,
            70,
            80,
            112,
        )
        .unwrap();
        assert_eq!(wrapped.dimensions(), (70, 80));
        assert!(wrapped.pixels().any(|p| p[3] > 0));
        let mut single = style.clone();
        single.wrap = false;
        assert_ne!(
            wrapped,
            text_image(
                "Wide <&>\nUnicode café and a long label",
                &single,
                70,
                80,
                112
            )
            .unwrap()
        );
    }
    #[test]
    // Verify custom icons keep aspect ratio and alpha
    fn custom_icons_keep_aspect_ratio_and_alpha() {
        let icon = asset("test:shape", "#ff0000");
        let image = crate::render::contained_pixels(icon.svg.as_bytes(), 80, 80).unwrap();
        assert_eq!(image.get_pixel(40, 0)[3], 0);
        assert_eq!(image.get_pixel(40, 40)[3], 255);
    }
    #[test]
    // Verify invalid and duplicate styles are rejected and assets round trip
    fn invalid_and_duplicate_styles_are_rejected_and_assets_round_trip() {
        let mut appearance = style(&visual());
        appearance.elements[0].icon_asset = Some(asset("test:portable", "currentColor"));
        let copy: PluginAppearance =
            serde_json::from_slice(&serde_json::to_vec(&appearance).unwrap()).unwrap();
        copy.validate().unwrap();
        assert!(copy.elements[0]
            .icon_asset
            .as_ref()
            .unwrap()
            .svg
            .contains("currentColor"));
        appearance.elements.push(appearance.elements[0].clone());
        assert!(appearance.validate().is_err());
        appearance.elements.pop();
        appearance.elements[0].rect[0] = f64::NAN;
        assert!(appearance.validate().is_err());
    }
}

#[cfg(test)]
mod text_regressions {
    use super::*;
    #[test]
    // Verify small fonts keep long live lines and invalid optional groups do not hide fields
    fn small_fonts_keep_long_live_lines_and_invalid_optional_groups_do_not_hide_fields() {
        let style = ElementStyle {
            layout: "test".into(),
            key: "text".into(),
            size: 1.,
            align: "left".into(),
            vertical_align: "top".into(),
            ..Default::default()
        };
        let short = text_image(&"i".repeat(160), &style, 512, 16, 112).unwrap();
        let long = text_image(&"i".repeat(220), &style, 512, 16, 112).unwrap();
        assert!(
            short != long,
            "Live lines must not inherit the static label's 160-character limit"
        );
        let visual = json!({"title":"Full title","layout":{"id":"test","items":[{"key":"value","type":"text","rect":[0,0,200,100]}],"streamDockManager":{"text_groups":[{"key":"group","source":"title","keys":["value"],"rect":[]}]}},"feedback":{"value":"Still editable"}});
        let fields = elements(&visual);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value, "Still editable");
    }
}

#[cfg(test)]
mod adaptive_text_tests {
    use super::*;
    #[test]
    // Verify fitted labels keep padding and respect unicode multiline
    fn fitted_labels_keep_padding_and_respect_unicode_multiline() {
        for width in [64, 72, 176] {
            for align in ["left", "center", "right"] {
                let style = ElementStyle {
                    layout: "test".into(),
                    key: "label".into(),
                    size: 30.,
                    align: align.into(),
                    ..Default::default()
                };
                for text in [
                    "WWWW long label 12345",
                    "Café résumé",
                    "One\nLonger second line",
                ] {
                    let pixels = text_image_fitted(text, &style, width, 64, 64, true).unwrap();
                    let bounds = pixels
                        .enumerate_pixels()
                        .filter(|(_, _, p)| p[3] > 0)
                        .map(|(x, _, _)| x)
                        .fold((u32::MAX, 0), |(lo, hi), x| (lo.min(x), hi.max(x)));
                    assert!(bounds.0 >= 3 && bounds.1 < width - 3, "{text}: {bounds:?}");
                    assert_eq!(style.size, 30.);
                }
            }
        }
    }
}

#[cfg(test)]
mod element_font_tests {
    use super::*;
    #[test]
    // Load existing element styles and preserve the real native font faces through serialization
    fn element_font_defaults_validation_and_pixels() {
        let old = serde_json::json!({"layout":"fixture","key":"text"});
        let defaults: ElementStyle = serde_json::from_value(old).unwrap();
        assert_eq!(defaults.font_weight, 400);
        assert!(!defaults.font_italic);
        let mut outputs = std::collections::BTreeSet::new();
        for weight in [400, 700] {
            for italic in [false, true] {
                let style = ElementStyle {
                    font_weight: weight,
                    font_italic: italic,
                    size: 24.,
                    ..defaults.clone()
                };
                style.validate().unwrap();
                let value = serde_json::to_value(&style).unwrap();
                let restored: ElementStyle = serde_json::from_value(value.clone()).unwrap();
                assert_eq!(serde_json::to_value(&restored).unwrap(), value);
                outputs.insert(
                    text_image("Wide label", &restored, 112, 112, 112)
                        .unwrap()
                        .into_raw(),
                );
            }
        }
        assert_eq!(outputs.len(), 4);
        for weight in [0, 100, 650, 900] {
            assert!(ElementStyle {
                font_weight: weight,
                ..defaults.clone()
            }
            .validate()
            .is_err());
        }
    }
}
