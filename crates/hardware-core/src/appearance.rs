//! Portable overlays with bundled fonts and self-contained, validated icon vectors
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub font: String,
    /// Font size as a percentage of surface height
    pub size: u8,
    pub foreground: String,
    pub position: String,
    pub align: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_asset: Option<IconAsset>,
    /// Alternate icon selected after each successful binding execution
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub toggle_icon: Option<Box<ToggleIcon>>,
    pub icon_size: u8,
    pub icon_position: String,
    pub show_title: bool,
}
impl Default for Appearance {
    // Construct the default value
    fn default() -> Self {
        Self {
            font: "sans".into(),
            size: 18,
            foreground: "#ffffff".into(),
            position: "bottom".into(),
            align: "center".into(),
            icon: String::new(),
            icon_asset: None,
            toggle_icon: None,
            icon_size: 42,
            icon_position: "center".into(),
            show_title: true,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToggleIcon {
    pub icon: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_asset: Option<IconAsset>,
}
/// The vector and attribution travel with bindings and presets, independently of the disk cache
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IconAsset {
    pub id: String,
    pub svg: String,
    pub collection: String,
    pub author: String,
    pub license: String,
    pub license_url: String,
}
// Check that an icon identifier has safe, bounded collection and name components
pub fn icon_id(id: &str) -> bool {
    let Some((prefix, name)) = id.split_once(':') else {
        return false;
    };
    [prefix, name].iter().all(|part| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}
impl IconAsset {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !icon_id(&self.id)
            || self.svg.len() > 256 * 1024
            || [
                &self.collection,
                &self.author,
                &self.license,
                &self.license_url,
            ]
            .iter()
            .any(|s| s.len() > 1024)
        {
            return Err("Invalid icon data".into());
        }
        validate_static_svg(&self.svg)?;
        let tree = icon_tree(&self.svg, "#ffffff")?;
        check_svg_dimensions(&tree)
    }
}

// Validate static SVG
pub fn validate_static_svg(svg: &str) -> std::result::Result<(), String> {
    if svg.len() > 256 * 1024 {
        return Err("SVG exceeds 256 KiB".into());
    }
    let doc = roxmltree::Document::parse(svg).map_err(|_| "Invalid SVG")?;
    if doc.root_element().tag_name().name() != "svg" || doc.descendants().count() > 8192 {
        return Err("Invalid SVG".into());
    }
    // Only static vector graphics. Never allow images, executable content or external references,
    // including in imported profiles. The renderer also disables both image resolvers.
    for node in doc.descendants().filter(|n| n.is_element()) {
        let tag = node.tag_name().name();
        if ![
            "svg",
            "g",
            "defs",
            "path",
            "rect",
            "circle",
            "ellipse",
            "line",
            "polyline",
            "polygon",
            "linearGradient",
            "radialGradient",
            "stop",
            "clipPath",
            "mask",
            "pattern",
            "use",
            "title",
            "desc",
            "filter",
            "feBlend",
            "feColorMatrix",
            "feComponentTransfer",
            "feComposite",
            "feConvolveMatrix",
            "feDiffuseLighting",
            "feDisplacementMap",
            "feDistantLight",
            "feDropShadow",
            "feFlood",
            "feFuncA",
            "feFuncB",
            "feFuncG",
            "feFuncR",
            "feGaussianBlur",
            "feMerge",
            "feMergeNode",
            "feMorphology",
            "feOffset",
            "fePointLight",
            "feSpecularLighting",
            "feSpotLight",
            "feTile",
            "feTurbulence",
        ]
        .contains(&tag)
        {
            return Err("SVG contains unsupported SVG content".into());
        }
        for attr in node.attributes() {
            let value = attr.value().to_ascii_lowercase();
            if attr.name().to_ascii_lowercase().starts_with("on")
                || attr.name() == "style"
                || (attr.name() == "href" && !value.starts_with('#'))
                || (value.contains("url(")
                    && !(value.starts_with("url(#")
                        && value.ends_with(')')
                        && value[5..value.len() - 1]
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')))
                || value.contains("javascript:")
                || value.contains("data:")
                || value.contains("http:")
                || value.contains("https:")
            {
                return Err("SVG contains an external or active resource".into());
            }
        }
    }
    Ok(())
}

// Check SVG dimensions
pub(crate) fn check_svg_dimensions(tree: &resvg::usvg::Tree) -> std::result::Result<(), String> {
    if tree.size().width() > 4096.0 || tree.size().height() > 4096.0 {
        return Err("SVG dimensions are too large".into());
    }
    Ok(())
}

// Parse the SVG tree for the selected icon
fn icon_tree(svg: &str, color: &str) -> std::result::Result<resvg::usvg::Tree, String> {
    let mut options = resvg::usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    let svg = svg.replace("currentColor", color);
    resvg::usvg::Tree::from_data(&crate::icons::transparent_svg(svg.as_bytes()), &options)
        .map_err(|e| e.to_string())
}
pub const ICONS: &[(&str, &str)] = &[
    (
        "volume-2",
        include_str!("../../../public/appearance/icons/volume-2.svg"),
    ),
    (
        "volume-x",
        include_str!("../../../public/appearance/icons/volume-x.svg"),
    ),
    (
        "play",
        include_str!("../../../public/appearance/icons/play.svg"),
    ),
    (
        "pause",
        include_str!("../../../public/appearance/icons/pause.svg"),
    ),
    (
        "music",
        include_str!("../../../public/appearance/icons/music.svg"),
    ),
    (
        "mic",
        include_str!("../../../public/appearance/icons/mic.svg"),
    ),
    (
        "mic-off",
        include_str!("../../../public/appearance/icons/mic-off.svg"),
    ),
    (
        "monitor",
        include_str!("../../../public/appearance/icons/monitor.svg"),
    ),
    (
        "globe",
        include_str!("../../../public/appearance/icons/globe.svg"),
    ),
    (
        "folder",
        include_str!("../../../public/appearance/icons/folder.svg"),
    ),
    (
        "terminal",
        include_str!("../../../public/appearance/icons/terminal.svg"),
    ),
    (
        "keyboard",
        include_str!("../../../public/appearance/icons/keyboard.svg"),
    ),
    (
        "sun",
        include_str!("../../../public/appearance/icons/sun.svg"),
    ),
    (
        "moon",
        include_str!("../../../public/appearance/icons/moon.svg"),
    ),
    (
        "power",
        include_str!("../../../public/appearance/icons/power.svg"),
    ),
    (
        "settings",
        include_str!("../../../public/appearance/icons/settings.svg"),
    ),
    (
        "heart",
        include_str!("../../../public/appearance/icons/heart.svg"),
    ),
    (
        "star",
        include_str!("../../../public/appearance/icons/star.svg"),
    ),
    (
        "clock",
        include_str!("../../../public/appearance/icons/clock.svg"),
    ),
    (
        "calendar",
        include_str!("../../../public/appearance/icons/calendar.svg"),
    ),
    (
        "check",
        include_str!("../../../public/appearance/icons/check.svg"),
    ),
    (
        "circle-alert",
        include_str!("../../../public/appearance/icons/circle-alert.svg"),
    ),
    (
        "zap",
        include_str!("../../../public/appearance/icons/zap.svg"),
    ),
    (
        "home",
        include_str!("../../../public/appearance/icons/home.svg"),
    ),
];
// Check for a six-digit hexadecimal RGB color
pub fn color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}
/// Binding backgrounds can carry alpha; foregrounds remain six-digit RGB
pub fn background_color(value: &str) -> Option<[u8; 4]> {
    if ![7, 9].contains(&value.len())
        || !value.starts_with('#')
        || !value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let mut rgba = [0, 0, 0, 255];
    for (index, channel) in rgba.iter_mut().enumerate().take((value.len() - 1) / 2) {
        *channel = u8::from_str_radix(&value[1 + index * 2..3 + index * 2], 16).ok()?;
    }
    Some(rgba)
}
impl Appearance {
    // Validate for the current operation
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !crate::fonts::valid(&self.font)
            || !(6..=60).contains(&self.size)
            || !(10..=90).contains(&self.icon_size)
            || !color(&self.foreground)
            || !["top", "center", "bottom"].contains(&self.position.as_str())
            || !["left", "center", "right"].contains(&self.align.as_str())
            || !["top", "center", "bottom"].contains(&self.icon_position.as_str())
            || (!self.icon.is_empty()
                && !ICONS.iter().any(|(id, _)| *id == self.icon)
                && !self.icon_asset.as_ref().is_some_and(|a| a.id == self.icon))
        {
            return Err("Invalid appearance settings".into());
        }
        if let Some(asset) = &self.icon_asset {
            if asset.id != self.icon {
                return Err("Icon identity does not match its vector".into());
            }
            asset.validate()?;
        }
        if let Some(toggle) = &self.toggle_icon {
            if self.icon.is_empty() || toggle.icon.is_empty() {
                return Err("Choose both an initial and a toggled icon".into());
            }
            let alternate = Self {
                icon: toggle.icon.clone(),
                icon_asset: toggle.icon_asset.clone(),
                toggle_icon: None,
                ..self.clone()
            };
            alternate.validate()?;
        }
        Ok(())
    }
    // Apply icon state while preserving the surrounding appearance
    pub fn with_icon_state(&self, toggled: bool) -> Self {
        let mut appearance = self.clone();
        if let Some(icon) = self.toggle_icon.as_ref().filter(|_| toggled) {
            appearance.icon = icon.icon.clone();
            appearance.icon_asset = icon.icon_asset.clone();
        }
        appearance
    }
}
// Escape text before embedding it in markup
fn escape(value: &str) -> String {
    value
        .chars()
        .take(160)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
// Resolve the fonts used by the appearance
pub(crate) fn fonts(font: &str) -> Arc<resvg::usvg::fontdb::Database> {
    crate::fonts::database_for(font)
}
/// Base is always in logical orientation. Rotate only after all overlays are composed
pub fn compose(
    base: &[u8],
    width: u16,
    height: u16,
    title: &str,
    appearance: &Appearance,
    feedback: &str,
    rotation: u16,
) -> Result<Vec<u8>> {
    let base = crate::render::layer_pixels(base, width, height)?;
    crate::render::encode_pixels(
        compose_pixels(base, width, height, title, appearance, feedback, rotation)?,
        "png",
    )
}
/// Keep the bounded logical surface in pixels until the final output encoding
fn title_markup(
    width: u16,
    height: u16,
    title: &str,
    appearance: &Appearance,
    feedback: &str,
) -> String {
    let w = f64::from(width);
    let h = f64::from(height);
    let requested_size = (h * f64::from(appearance.size) / 100.0).max(6.0);
    let padding = (w * 0.08).max(4.0).min(w / 4.0);
    let db = fonts(&appearance.font);
    let title: String = title.chars().take(160).collect();
    let advances = crate::fonts::advances(&appearance.font, &title, requested_size, &db);
    let lines: Vec<_> = title.split('\n').collect();
    let longest = lines
        .iter()
        .map(|line| {
            line.chars()
                .map(|ch| advances.get(&ch).copied().unwrap_or(requested_size * 0.6))
                .sum::<f64>()
        })
        .fold(0.0_f64, f64::max);
    let available_height = (h
        - 8.0
        - if feedback.is_empty() {
            0.0
        } else {
            requested_size + 8.0
        })
    .max(1.0);
    // The saved size remains a ceiling, never a value rewritten by rendering
    let size = requested_size
        * ((w - padding * 2.0) / longest.max(1.0))
            .min(available_height / (requested_size * 1.2 * lines.len() as f64))
            .min(1.0);
    let block = size * 1.2 * lines.len() as f64;
    let top = match appearance.position.as_str() {
        "top" => 3.0,
        "bottom" => {
            h - if feedback.is_empty() {
                5.0
            } else {
                requested_size + 8.0
            } - block
        }
        _ => (h - block) / 2.0,
    };
    let (x, anchor) = match appearance.align.as_str() {
        "left" => (padding, "start"),
        "right" => (w - padding, "end"),
        _ => (w / 2.0, "middle"),
    };
    let family = crate::fonts::svg_family(&appearance.font, &db);
    lines.iter().enumerate().map(|(index, line)| {
        let y = top + size + index as f64 * size * 1.2;
        format!("<text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\" font-family=\"{family}\" font-size=\"{size}\" fill=\"{}\">{}</text>", appearance.foreground, escape(line))
    }).collect()
}

/// Uses the very same SVG glyph shaping and placement as the device overlay
/// Background pixels are the upright draft with the title removed
pub fn text_warnings(
    background: &image::RgbaImage,
    title: &str,
    appearance: &Appearance,
    feedback: &str,
    animated: bool,
) -> Result<Vec<String>> {
    if !appearance.show_title || title.is_empty() {
        return Ok(vec![]);
    }
    let (width, height) = background.dimensions();
    let body = title_markup(width as u16, height as u16, title, appearance, feedback);
    let options = resvg::usvg::Options {
        fontdb: fonts(&appearance.font),
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    let mut options = options;
    let source = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">{body}</svg>");
    #[cfg(target_os = "macos")]
    let (source, native_clipped) =
        crate::native_text::prepare_svg(&source, &appearance.font, &mut options)?;
    #[cfg(not(target_os = "macos"))]
    let native_clipped = false;
    let tree = resvg::usvg::Tree::from_str(&source, &options)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut warnings = vec![];
    if native_clipped || glyphs_clipped(tree.root(), width, height) {
        warnings.push("Some title glyphs are clipped at the edge of this surface.".into());
    }
    let mut mask = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| Error::Protocol("Invalid text check dimensions".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut mask.as_mut(),
    );
    if low_contrast(
        background,
        mask.pixels().iter().map(|p| p.alpha()),
        &appearance.foreground,
    ) {
        warnings.push(contrast_warning(animated));
    }
    Ok(warnings)
}

// Check whether rendered glyphs extend beyond their bounds
pub(crate) fn glyphs_clipped(group: &resvg::usvg::Group, width: u32, height: u32) -> bool {
    use resvg::usvg::Node;
    group.children().iter().any(|node| match node {
        Node::Group(group) => glyphs_clipped(group, width, height),
        Node::Text(text) => glyphs_clipped(text.flattened(), width, height),
        node => {
            // usvg 0.50 represents geometry without painted bounds as None.
            // Empty paths do not clip; keep checking every visible node
            node.abs_bounding_box().is_some_and(|bounds| {
                bounds.left() < 0.
                    || bounds.top() < 0.
                    || bounds.right() > width as f32
                    || bounds.bottom() > height as f32
            })
        }
    })
}

// Describe insufficient text contrast when applicable
pub fn contrast_warning(animated: bool) -> String {
    if animated {
        "Text contrast is below 4.5:1 in part of this displayed frame sample."
    } else {
        "Text contrast is below 4.5:1 in part of this preview."
    }
    .into()
}
// Check whether the foreground and background have insufficient contrast
pub fn low_contrast(
    background: &image::RgbaImage,
    mask: impl Iterator<Item = u8>,
    foreground: &str,
) -> bool {
    let ink = background_color(foreground).unwrap_or([255, 255, 255, 255]);
    // Calculate relative luminance for the color
    fn luminance(rgb: [f64; 3]) -> f64 {
        rgb.into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(c, w)| {
                w * if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            })
            .sum()
    }
    mask.zip(background.pixels()).any(|(alpha, pixel)| {
        // Ignore antialiased edges; compare the ink, including its own opacity
        if alpha < ink[3].max(1) / 2 || alpha == 0 {
            return false;
        }
        let bg = [0, 1, 2].map(|i| f64::from(pixel[i]) * f64::from(pixel[3]) / (255. * 255.));
        let a = f64::from(ink[3]) / 255.;
        let fg = [0, 1, 2].map(|i| f64::from(ink[i]) / 255. * a + bg[i] * (1. - a));
        let under = luminance(bg);
        let over = luminance(fg);
        (over.max(under) + 0.05) / (over.min(under) + 0.05) < 4.5
    })
}

// Composite the appearance layers into output pixels
pub fn compose_pixels(
    base_image: image::RgbaImage,
    width: u16,
    height: u16,
    title: &str,
    appearance: &Appearance,
    feedback: &str,
    rotation: u16,
) -> Result<image::DynamicImage> {
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || base_image.dimensions() != (width.into(), height.into())
    {
        return Err(Error::Protocol("Invalid overlay dimensions".into()));
    }
    appearance.validate().map_err(Error::Protocol)?;
    let w = f64::from(width);
    let h = f64::from(height);
    let size = (h * f64::from(appearance.size) / 100.0).max(6.0);
    let db = fonts(&appearance.font);
    let family = crate::fonts::svg_family(&appearance.font, &db);
    let mut body = String::new();
    if let Some((_, icon)) = ICONS.iter().find(|(id, _)| *id == appearance.icon) {
        let s = w.min(h) * f64::from(appearance.icon_size) / 100.0;
        let iy = match appearance.icon_position.as_str() {
            "top" => 3.0,
            "bottom" => h - s - 3.0,
            _ => (h - s) / 2.0,
        };
        let content = icon
            .split_once('>')
            .map(|(_, s)| s)
            .unwrap_or("")
            .rsplit_once("</svg>")
            .map(|(s, _)| s)
            .unwrap_or("");
        body.push_str(&format!("<g transform=\"translate({},{iy}) scale({})\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{content}</g>", (w-s)/2.0, s/24.0, appearance.foreground));
    }
    if appearance.show_title {
        body.push_str(&title_markup(width, height, title, appearance, feedback));
    }
    if !feedback.is_empty() {
        if base_image.pixels().all(|pixel| pixel[3] == 255) {
            body.push_str(&format!("<rect x=\"0\" y=\"{}\" width=\"{w}\" height=\"{}\" fill=\"#000000\" fill-opacity=\"0.8\"/>", h-size-8.0, size+8.0));
        }
        let advances = crate::fonts::advances(&appearance.font, feedback, size, &db);
        let length: f64 = feedback
            .chars()
            .take(160)
            .map(|ch| advances.get(&ch).copied().unwrap_or(size * 0.6))
            .sum();
        let size = size * (w * 0.84 / length.max(1.)).min(1.);
        body.push_str(&format!("<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-family=\"{family}\" font-size=\"{size}\" fill=\"{}\">{}</text>", w/2.0, h-4.0, appearance.foreground, escape(feedback)));
    }
    let mut options = resvg::usvg::Options {
        fontdb: db,
        ..Default::default()
    };
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    let source = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\">{body}</svg>"
    );
    #[cfg(target_os = "macos")]
    let (source, _) = crate::native_text::prepare_svg(&source, &appearance.font, &mut options)?;
    let tree = resvg::usvg::Tree::from_str(&source, &options)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width.into(), height.into())
        .ok_or_else(|| Error::Protocol("Invalid overlay dimensions".into()))?;
    if let Some(asset) = &appearance.icon_asset {
        let icon = icon_tree(&asset.svg, &appearance.foreground).map_err(Error::Protocol)?;
        let s = width.min(height) as f32 * f32::from(appearance.icon_size) / 100.0;
        let scale = (s / icon.size().width()).min(s / icon.size().height());
        let iw = icon.size().width() * scale;
        let ih = icon.size().height() * scale;
        let iy = match appearance.icon_position.as_str() {
            "top" => 3.0,
            "bottom" => height as f32 - ih - 3.0,
            _ => (height as f32 - ih) / 2.0,
        };
        resvg::render(
            &icon,
            resvg::tiny_skia::Transform::from_row(
                scale,
                0.0,
                0.0,
                scale,
                (width as f32 - iw) / 2.0,
                iy,
            ),
            &mut pixmap.as_mut(),
        );
    }
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    // This is precisely tiny-skia's PNG conversion, including alpha rounding,
    // consuming its existing allocation instead of encoding and decoding it.
    let overlay =
        image::RgbaImage::from_raw(width.into(), height.into(), pixmap.take_demultiplied())
            .ok_or_else(|| Error::Protocol("Invalid overlay dimensions".into()))?;
    let mut image = image::DynamicImage::ImageRgba8(base_image);
    image::imageops::overlay(&mut image, &overlay, 0, 0);
    let image = match rotation {
        0 => image,
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("Invalid rotation".into())),
    };
    Ok(image)
}

/// Self-contained icon source for display layers
pub fn icon_uri(asset: &IconAsset, color: &str) -> String {
    use base64::Engine;
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(asset.svg.replace("currentColor", color))
    )
}
// Resolve the icon URI for the appearance
pub fn appearance_icon_uri(appearance: &Appearance) -> Option<String> {
    use base64::Engine;
    let svg = appearance
        .icon_asset
        .as_ref()
        .map(|asset| asset.svg.as_str())
        .or_else(|| {
            ICONS
                .iter()
                .find(|(id, _)| *id == appearance.icon)
                .map(|(_, svg)| *svg)
        })?;
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD
            .encode(svg.replace("currentColor", &appearance.foreground))
    ))
}

#[cfg(test)]
mod icon_tests {
    #[test]
    // Verify SVG clipping ignores empty bounds and detects visible overflow
    fn svg_clipping_ignores_empty_bounds_and_detects_visible_overflow() {
        for (body, clipped) in [
            (r#"<path d=""/>"#, false),
            (r#"<rect x="1" y="1" width="4" height="4"/>"#, false),
            (r#"<rect x="-2" y="1" width="4" height="4"/>"#, true),
        ] {
            let source = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{body}</svg>"#
            );
            let tree = resvg::usvg::Tree::from_str(&source, &Default::default()).unwrap();
            assert_eq!(super::glyphs_clipped(tree.root(), 10, 10), clipped);
        }
    }
    use super::*;
    use std::io::Cursor;
    #[test]
    // Verify title contrast uses ink pixels and the 4 5 threshold with animation sample label
    fn title_contrast_uses_ink_pixels_and_the_4_5_threshold_with_animation_sample_label() {
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]));
        assert!(!low_contrast(&white, [255].into_iter(), "#767676"));
        assert!(low_contrast(&white, [255].into_iter(), "#777777"));
        assert!(!low_contrast(&white, [0].into_iter(), "#ffffff"));
        let background = image::RgbaImage::from_pixel(72, 72, image::Rgba([0, 0, 0, 255]));
        let warnings = text_warnings(
            &background,
            "Hi",
            &Appearance {
                foreground: "#000000".into(),
                ..Default::default()
            },
            "",
            true,
        )
        .unwrap();
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("displayed frame sample")));
    }
    #[test]
    // Verify direct overlay pixels match PNG alpha rounding
    fn direct_overlay_pixels_match_png_alpha_rounding() {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(256, 3).unwrap();
        for (index, pixel) in pixmap.pixels_mut().iter_mut().enumerate() {
            let alpha = (index % 256) as u8;
            *pixel = resvg::tiny_skia::PremultipliedColorU8::from_rgba(
                alpha / 3,
                alpha / 2,
                alpha,
                alpha,
            )
            .unwrap();
        }
        let legacy = image::load_from_memory(&pixmap.encode_png().unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(legacy.as_raw(), &pixmap.take_demultiplied());
        assert!(compose_pixels(
            image::RgbaImage::new(2, 1),
            1,
            1,
            "",
            &Default::default(),
            "",
            0
        )
        .is_err());
        assert!(compose_pixels(
            image::RgbaImage::new(0, 0),
            0,
            0,
            "",
            &Default::default(),
            "",
            0
        )
        .is_err());
    }

    // Resolve the asset value for this operation
    fn asset(svg: &str) -> IconAsset {
        IconAsset {
            id: "test:vector".into(),
            svg: svg.into(),
            collection: "Test".into(),
            author: "Test".into(),
            license: "ISC".into(),
            license_url: String::new(),
        }
    }
    #[test]
    // Verify saved icons and alternate icons keep the key background visible
    fn saved_icons_and_alternate_icons_keep_the_key_background_visible() {
        let icon = asset(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" fill="#121212"/><circle cx="16" cy="16" r="8" fill="currentColor"/></svg>"##,
        );
        let appearance = Appearance {
            icon: icon.id.clone(),
            icon_asset: Some(icon.clone()),
            show_title: false,
            toggle_icon: Some(Box::new(ToggleIcon {
                icon: icon.id.clone(),
                icon_asset: Some(icon),
            })),
            ..Default::default()
        };
        for toggled in [false, true] {
            let selected = appearance.with_icon_state(toggled);
            for background in [[36, 104, 172, 255], [0; 4]] {
                let base = crate::render::solid_rgba(64, 64, background).unwrap();
                let composed = compose(&base, 64, 64, "", &selected, "", 0).unwrap();
                let image = image::load_from_memory(&composed).unwrap().to_rgba8();
                assert_eq!(image.get_pixel(19, 19).0, background);
                assert_eq!(image.get_pixel(32, 32).0, [255; 4]);
            }
        }
    }
    #[test]
    // Verify imported icons reject executable external and oversize vectors
    fn imported_icons_reject_executable_external_and_oversize_vectors() {
        let good = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 12\"><path fill=\"currentColor\" d=\"M0 0h24v12H0z\"/></svg>";
        asset(good).validate().unwrap();
        for svg in [
            good.replace("<path", "<script"),
            good.replace("<path", "<image"),
            good.replace("<path", "<foreignObject"),
            good.replace("<path", "<path onclick=\"alert(1)\""),
            good.replace("<path", "<path style=\"fill:url(https://example.com)\""),
            good.replace("<path", "<use href=\"file:///etc/passwd\""),
            good.replace("currentColor", "url(https://example.com/icon.svg)"),
            good.replace("currentColor", "url(#safe) url(https://example.com)"),
            "x".repeat(256 * 1024 + 1),
        ] {
            assert!(asset(&svg).validate().is_err(), "Accepted: {svg:.100}");
        }
        let mut a = Appearance {
            icon: "test:other".into(),
            icon_asset: Some(asset(good)),
            ..Default::default()
        };
        assert!(a.validate().is_err());
        a.icon = "test:vector".into();
        a.validate().unwrap();
    }
    #[test]
    // Verify portable icon renders with color and preserves non square ratio
    fn portable_icon_renders_with_color_and_preserves_non_square_ratio() {
        let icon = asset(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"12\"><path fill=\"currentColor\" d=\"M0 0h24v12H0z\"/></svg>",
        );
        let a = Appearance {
            icon: icon.id.clone(),
            icon_asset: Some(icon),
            foreground: "#ff0000".into(),
            show_title: false,
            icon_size: 50,
            ..Default::default()
        };
        let mut base = Cursor::new(vec![]);
        image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 0, 255]))
            .write_to(&mut base, image::ImageFormat::Png)
            .unwrap();
        let bytes = compose(base.get_ref(), 100, 100, "", &a, "", 0).unwrap();
        let out = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(out.get_pixel(50, 50).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(50, 30).0, [0, 0, 0, 255]);
        assert_eq!(out.get_pixel(20, 50).0, [0, 0, 0, 255]);
    }
    #[test]
    // Verify alternate icons are portable validated and do not mutate the initial icon
    fn alternate_icons_are_portable_validated_and_do_not_mutate_the_initial_icon() {
        let vector = asset(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><circle cx=\"12\" cy=\"12\" r=\"8\"/></svg>",
        );
        let appearance = Appearance {
            icon: "play".into(),
            toggle_icon: Some(Box::new(ToggleIcon {
                icon: vector.id.clone(),
                icon_asset: Some(vector),
            })),
            ..Default::default()
        };
        appearance.validate().unwrap();
        let restored: Appearance =
            serde_json::from_str(&serde_json::to_string(&appearance).unwrap()).unwrap();
        assert_eq!(restored.with_icon_state(true).icon, "test:vector");
        assert!(restored.with_icon_state(true).icon_asset.is_some());
        assert_eq!(restored.with_icon_state(false).icon, "play");
        let mut bad = restored.clone();
        bad.toggle_icon
            .as_mut()
            .unwrap()
            .icon_asset
            .as_mut()
            .unwrap()
            .svg = "<svg><script/></svg>".into();
        assert!(bad.validate().is_err());
        bad = restored;
        bad.toggle_icon.as_mut().unwrap().icon.clear();
        assert!(bad.validate().is_err());
        let legacy: Appearance = serde_json::from_str("{}").unwrap();
        assert!(legacy.toggle_icon.is_none());
    }
}

#[cfg(test)]
mod adaptive_title_tests {
    use super::*;

    #[test]
    // Verify fitted multiline title checks match rendered lines and contrast
    fn fitted_multiline_title_checks_match_rendered_lines_and_contrast() {
        let title = "WIDE TITLE\nSECOND LINE";
        let appearance = Appearance {
            size: 60,
            ..Default::default()
        };
        let background = image::RgbaImage::from_pixel(72, 72, image::Rgba([0, 0, 0, 255]));
        let warnings = text_warnings(&background, title, &appearance, "", false).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let pixels = compose_pixels(
            image::RgbaImage::new(72, 72),
            72,
            72,
            title,
            &appearance,
            "",
            0,
        )
        .unwrap()
        .to_rgba8();
        // Verify actual ink occupies two separated lines, not merely that the
        // warning was suppressed. The auto-fitted text keeps all four margins.
        let mut in_line = false;
        let mut lines = 0;
        for mut row in pixels.rows() {
            let has_ink = row.any(|pixel| pixel[3] > 16);
            if has_ink && !in_line {
                lines += 1;
            }
            in_line = has_ink;
        }
        assert_eq!(lines, 2);
        assert!(pixels
            .enumerate_pixels()
            .all(|(x, y, pixel)| { pixel[3] == 0 || (x > 0 && x < 71 && y > 0 && y < 71) }));
        assert_eq!(appearance.size, 60);

        let low_contrast_appearance = Appearance {
            foreground: "#000000".into(),
            ..appearance
        };
        for animated in [false, true] {
            assert_eq!(
                text_warnings(&background, title, &low_contrast_appearance, "", animated).unwrap(),
                vec![contrast_warning(animated)]
            );
        }
    }

    #[test]
    // Verify titles shrink without changing saved size and keep side padding
    fn titles_shrink_without_changing_saved_size_and_keep_side_padding() {
        for (width, height) in [(64, 64), (72, 72), (176, 112)] {
            for align in ["left", "center", "right"] {
                let appearance = Appearance {
                    size: 30,
                    align: align.into(),
                    ..Default::default()
                };
                for title in [
                    "WWW very long label 12345",
                    "Café résumé",
                    "First line\nLonger second line",
                ] {
                    let base = image::RgbaImage::new(width.into(), height.into());
                    let pixels =
                        compose_pixels(base, width, height, title, &appearance, "", 0).unwrap();
                    if let Ok(directory) = std::env::var("SDM_TEXT_FIT_ARTIFACTS") {
                        std::fs::create_dir_all(&directory).unwrap();
                        let name = if title.contains('\n') {
                            "multiline"
                        } else if title.starts_with("Café") {
                            "unicode"
                        } else {
                            "long"
                        };
                        pixels
                            .save(format!("{directory}/{width}-{align}-{name}.png"))
                            .unwrap();
                    }
                    let bounds = pixels
                        .to_rgba8()
                        .enumerate_pixels()
                        .filter(|(_, _, p)| p[3] > 0)
                        .map(|(x, _, _)| x)
                        .fold((u32::MAX, 0), |(lo, hi), x| (lo.min(x), hi.max(x)));
                    assert!(
                        bounds.0 >= 3 && bounds.1 < u32::from(width) - 3,
                        "{title}: {bounds:?}"
                    );
                    assert_eq!(appearance.size, 30);
                }
            }
        }
        let appearance = Appearance::default();
        let source = title_markup(100, 100, "Hi", &appearance, "");
        assert!(source.contains("font-size=\"18\""));
    }
}
