//! Portable, ordered display layers. Live values are resolved by exact source identity
use crate::plugin_display::{self, ElementStyle};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaySource {
    pub kind: String,
    pub plugin_id: String,
    pub action_id: String,
    pub layout: String,
    pub key: String,
    pub element_kind: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisplayLayer {
    pub id: String,
    pub label: String,
    pub source: DisplaySource,
    pub style: ElementStyle,
    #[serde(default)]
    pub text: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisplayComposition {
    pub layers: Vec<DisplayLayer>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DisplaySourceDescriptor {
    pub label: String,
    pub source: DisplaySource,
    pub style: ElementStyle,
    pub value: Value,
    pub available: bool,
    pub placeholder: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<serde_json::Value>")]
    pub properties: Value,
}
impl DisplayComposition {
    // Validate for the current operation
    pub fn validate(&self) -> Result<(), String> {
        if self.layers.len() > 64
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024
        {
            return Err("Display composition is too large".into());
        }
        let mut ids = HashSet::new();
        for layer in &self.layers {
            if layer.id.is_empty()
                || layer.id.len() > 256
                || !ids.insert(&layer.id)
                || layer.label.len() > 160
                || layer.text.len() > 4096
            {
                return Err("Invalid display layer".into());
            }
            layer.style.validate()?;
            let source = &layer.source;
            if ![
                "title",
                "icon",
                "media",
                "feedback",
                "plugin",
                "text",
                "custom_icon",
            ]
            .contains(&source.kind.as_str())
                || [
                    &source.plugin_id,
                    &source.action_id,
                    &source.layout,
                    &source.key,
                    &source.element_kind,
                ]
                .iter()
                .any(|s| s.len() > 1024)
            {
                return Err("Invalid display source".into());
            }
            if source.kind == "plugin"
                && (source.plugin_id.is_empty()
                    || source.action_id.is_empty()
                    || source.layout.is_empty()
                    || source.key.is_empty()
                    || source.element_kind != layer.style.kind)
            {
                return Err("Invalid plugin display source".into());
            }
            let expected = match source.kind.as_str() {
                "icon" | "media" | "custom_icon" => Some("pixmap"),
                "title" | "feedback" | "text" => Some("text"),
                _ => None,
            };
            if expected.is_some_and(|kind| kind != layer.style.kind) {
                return Err("Display source and layer type differ".into());
            }
        }
        Ok(())
    }
    // Resolve the visual value for this operation
    pub fn visual(
        &self,
        sources: &[DisplaySourceDescriptor],
        color: &str,
        state: Option<u32>,
    ) -> Result<Value, String> {
        self.validate()?;
        let mut items = vec![];
        // Media selected after designing a canvas still belongs behind its content
        // An explicit media layer owns its placement and visibility instead
        if !self.layers.iter().any(|layer| layer.source.kind == "media") {
            if let Some(media) = sources
                .iter()
                .find(|s| s.source.kind == "media" && s.available)
            {
                items.push(json!({"key":"$binding-media","type":"pixmap","rect":[0,0,200,100],"value":media.value}));
            }
        }
        for layer in &self.layers {
            if !layer.style.visible {
                continue;
            }
            let mut value = match layer.source.kind.as_str() {
                "text" => json!(layer.text),
                "custom_icon" => Value::Null,
                _ => match sources
                    .iter()
                    .find(|s| s.source == layer.source && s.available)
                {
                    Some(s) => s.value.clone(),
                    None => continue,
                },
            };
            let style = &layer.style;
            if style.kind == "pixmap" {
                if let Some(asset) = state
                    .and_then(|s| style.state_icons.get(&s.to_string()))
                    .or(style.icon_asset.as_ref())
                {
                    value = json!(crate::appearance::icon_uri(asset, &style.foreground));
                }
                if !value.is_string() {
                    continue;
                }
            }
            let mut item = sources
                .iter()
                .find(|s| s.source == layer.source)
                .map(|s| s.properties.clone())
                .filter(Value::is_object)
                .unwrap_or(json!({}));
            item["key"] = json!(layer.id);
            item["type"] = json!(style.kind);
            item["rect"] = json!([
                style.rect[0] * 2.,
                style.rect[1],
                style.rect[2] * 2.,
                style.rect[3]
            ]);
            item["value"] = value;
            item["__manager_style"] = json!(style);
            items.push(item);
        }
        Ok(
            json!({"background":color,"layout":{"id":"manager-composition-v1","background":color,"items":items},"feedback":{}}),
        )
    }
}
// Collect the visual sources used by this composition
pub fn sources(
    title: &str,
    icon: Option<&str>,
    media: Option<&str>,
    feedback: &str,
    visual: Option<&Value>,
    placeholder: bool,
) -> Vec<DisplaySourceDescriptor> {
    let mut result = vec![];
    for (kind, label, value, rect, size) in [
        (
            "media",
            "Background media",
            media.unwrap_or(""),
            [0., 0., 100., 100.],
            12.,
        ),
        (
            "icon",
            "Binding icon",
            icon.unwrap_or(""),
            [29., 20., 42., 42.],
            12.,
        ),
        ("title", "Binding title", title, [2., 74., 96., 24.], 18.),
        (
            "feedback",
            "Runtime feedback",
            feedback,
            [2., 76., 96., 22.],
            18.,
        ),
    ] {
        let image = kind == "media" || kind == "icon";
        result.push(DisplaySourceDescriptor {
            label: label.into(),
            source: DisplaySource {
                kind: kind.into(),
                ..Default::default()
            },
            style: ElementStyle {
                layout: "$binding".into(),
                key: kind.into(),
                kind: if image { "pixmap" } else { "text" }.into(),
                rect,
                size,
                ..Default::default()
            },
            value: json!(value),
            available: !value.is_empty(),
            placeholder: false,
            properties: json!({}),
        });
    }
    if let Some(visual) = visual {
        let values = plugin_display::element_values(visual);
        let properties = plugin_display::render_items(visual);
        for element in plugin_display::elements(visual) {
            let value = values
                .iter()
                .find(|(key, kind, _)| key == &element.style.key && kind == &element.style.kind)
                .map(|(_, _, v)| v.clone())
                .unwrap_or(Value::Null);
            result.push(DisplaySourceDescriptor {
                label: element.label,
                source: DisplaySource {
                    kind: "plugin".into(),
                    plugin_id: visual["plugin_id"].as_str().unwrap_or("").into(),
                    action_id: visual["action_id"].as_str().unwrap_or("").into(),
                    layout: element.style.layout.clone(),
                    key: element.style.key.clone(),
                    element_kind: element.style.kind.clone(),
                },
                style: element.style.clone(),
                value,
                available: true,
                placeholder,
                properties: properties
                    .iter()
                    .find(|i| i["key"] == element.style.key && i["type"] == element.style.kind)
                    .cloned()
                    .unwrap_or(json!({})),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    // Resolve the text value for this operation
    fn text(id: &str, source: DisplaySource) -> DisplayLayer {
        DisplayLayer {
            id: id.into(),
            label: id.into(),
            source,
            style: ElementStyle {
                layout: "test".into(),
                key: id.into(),
                ..Default::default()
            },
            text: "Static label".into(),
        }
    }
    #[test]
    // Verify live sources are exact and values never replace styles
    fn live_sources_are_exact_and_values_never_replace_styles() {
        let visual = json!({"plugin_id":"weather","action_id":"conditions","layout":{"id":"weather-v1","items":[{"key":"temperature","label":"Temperature","type":"text","rect":[0,0,200,100]}]},"feedback":{"temperature":"54.5 °F"}});
        let source = sources("", None, None, "", Some(&visual), false)
            .pop()
            .unwrap();
        let mut layer = text("temp", source.source.clone());
        layer.style.size = 32.;
        let composition = DisplayComposition {
            layers: vec![layer],
        };
        let rendered = composition
            .visual(std::slice::from_ref(&source), "#00000000", None)
            .unwrap();
        assert_eq!(rendered["layout"]["items"][0]["value"], "54.5 °F");
        assert_eq!(
            rendered["layout"]["items"][0]["__manager_style"]["size"],
            32.
        );
        let mut other = source.clone();
        other.source.action_id = "forecast".into();
        assert!(
            composition.visual(&[other], "#00000000", None).unwrap()["layout"]["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(composition.layers[0].text, "Static label");
    }
    #[test]
    // Verify ordering visibility validation and pixel rendering share one compositor
    fn ordering_visibility_validation_and_pixel_rendering_share_one_compositor() {
        let mut back = text(
            "back",
            DisplaySource {
                kind: "text".into(),
                ..Default::default()
            },
        );
        back.text = "A".into();
        let mut front = back.clone();
        front.id = "front".into();
        front.style.key = "front".into();
        front.text = "B".into();
        let mut composition = DisplayComposition {
            layers: vec![back, front],
        };
        let scene = composition.visual(&[], "#00000000", None).unwrap();
        assert_eq!(scene["layout"]["items"][1]["value"], "B");
        let png = crate::render::visual(&scene, 112, 112, 0).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        composition.layers[1].style.visible = false;
        assert_eq!(
            composition.visual(&[], "#00000000", None).unwrap()["layout"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        composition.layers[1].style.rect[2] = 0.;
        assert!(composition.validate().is_err());
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    #[test]
    // Verify live bar keeps its range and rendering properties
    fn live_bar_keeps_its_range_and_rendering_properties() {
        let original = json!({"plugin_id":"monitor","action_id":"value","layout":{"id":"meter","background":"#000000","items":[{"key":"level","type":"bar","rect":[0,0,200,100],"range":{"min":-30,"max":70},"bar_fill_c":"#ff0000","bar_bg_c":"#000000"}]},"feedback":{"level":-10}});
        let descriptor = sources("", None, None, "", Some(&original), false)
            .pop()
            .unwrap();
        let composition = DisplayComposition {
            layers: vec![DisplayLayer {
                id: "meter".into(),
                label: "Meter".into(),
                source: descriptor.source.clone(),
                style: descriptor.style.clone(),
                text: String::new(),
            }],
        };
        let scene = composition.visual(&[descriptor], "#000000", None).unwrap();
        assert_eq!(
            scene["layout"]["items"][0]["range"],
            json!({"min":-30,"max":70})
        );
        assert_eq!(
            crate::render::visual(&original, 112, 112, 0).unwrap(),
            crate::render::visual(&scene, 112, 112, 0).unwrap()
        );
    }
}
