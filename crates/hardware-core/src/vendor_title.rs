//! Vendor state title defaults. These are transient display inputs, not persisted styles
use crate::{Error, Result};
use serde_json::Value;

// Escape text before embedding it in markup
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// Construct the SVG representation
fn svg(
    title: &str,
    parameters: &Value,
    width: u16,
    height: u16,
) -> (String, std::sync::Arc<resvg::usvg::fontdb::Database>) {
    let font = parameters["fontFamily"]
        .as_str()
        .map(|family| format!("system:{family}"))
        .filter(|font| crate::fonts::valid(font))
        .unwrap_or_else(|| "sans".into());
    let db = crate::fonts::database_for(&font);
    let family = crate::fonts::svg_family(&font, &db);
    let size = parameters["fontSize"]
        .as_f64()
        .filter(|size| size.is_finite() && *size > 0.)
        .unwrap_or(16.)
        .clamp(1., 1024.);
    let color = parameters["titleColor"]
        .as_str()
        .filter(|color| crate::appearance::background_color(color).is_some())
        .unwrap_or("#ffffff");
    let style = parameters["fontStyle"].as_str().unwrap_or("Regular");
    let weight = if matches!(style, "Bold" | "Bold Italic") {
        "bold"
    } else {
        "normal"
    };
    let style = if matches!(style, "Italic" | "Bold Italic") {
        "italic"
    } else {
        "normal"
    };
    let decoration = if parameters["fontUnderline"] == true {
        "underline"
    } else {
        "none"
    };
    let outline = parameters["outlineThickness"]
        .as_f64()
        .filter(|v| v.is_finite())
        .unwrap_or(0.)
        .clamp(0., 16.);
    let title: String = title.chars().take(4096).collect();
    let lines: Vec<_> = title.split('\n').take(128).collect();
    let line_height = size * 1.2;
    let block_height = line_height * lines.len() as f64;
    let top = match parameters["titleAlignment"].as_str().unwrap_or("middle") {
        "top" => 0.,
        "bottom" => (f64::from(height) - block_height).max(0.),
        _ => ((f64::from(height) - block_height) / 2.).max(0.),
    };
    let mut body = String::new();
    if parameters["showTitle"] != false {
        for (index, line) in lines.iter().enumerate() {
            body.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-family=\"{family}\" font-size=\"{size}\" font-weight=\"{weight}\" font-style=\"{style}\" text-decoration=\"{decoration}\" stroke=\"#000000\" stroke-width=\"{outline}\" paint-order=\"stroke fill\" fill=\"{}\">{}</text>",
                f64::from(width) / 2., top + size + index as f64 * line_height, escape(color), escape(line)
            ));
        }
    }
    (format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">{body}</svg>"), db)
}

// Draw for the current operation
pub(crate) fn draw(
    title: &str,
    parameters: &Value,
    width: u16,
    height: u16,
) -> Result<image::RgbaImage> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(Error::Protocol("invalid title surface dimensions".into()));
    }
    let (svg, fontdb) = svg(title, parameters, width, height);
    let mut options = resvg::usvg::Options {
        fontdb,
        ..Default::default()
    };
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    #[cfg(target_os = "macos")]
    let (svg, _) = crate::native_text::prepare_svg(
        &svg,
        &format!(
            "system:{}",
            parameters["fontFamily"]
                .as_str()
                .unwrap_or(crate::fonts::SANS)
        ),
        &mut options,
    )?;
    let tree = resvg::usvg::Tree::from_str(&svg, &options)
        .map_err(|error| Error::Protocol(error.to_string()))?;
    let mut pixels = resvg::tiny_skia::Pixmap::new(width.into(), height.into())
        .ok_or_else(|| Error::Protocol("invalid title bounds".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    let bytes = pixels
        .encode_png()
        .map_err(|error| Error::Protocol(error.to_string()))?;
    image::load_from_memory(&bytes)
        .map(|image| image.into_rgba8())
        .map_err(|error| Error::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    // Verify vendor title defaults control visibility alignment color and font attributes
    fn vendor_title_defaults_control_visibility_alignment_color_and_font_attributes() {
        let mut parameters = json!({"fontFamily":"Noto Sans", "fontSize":12, "fontStyle":"Bold Italic", "fontUnderline":true,"titleColor":"#ff0000", "titleAlignment":"top"});
        let (source, _) = svg("A<&", &parameters, 72, 72);
        assert!(source.contains("font-weight=\"bold\""));
        assert!(source.contains("font-style=\"italic\""));
        assert!(source.contains("text-decoration=\"underline\""));
        assert!(source.contains("font-size=\"12\""));
        assert!(source.contains("A&lt;&amp;"));
        let bounds = |image: &image::RgbaImage| {
            image
                .enumerate_pixels()
                .filter(|(_, _, p)| p[3] != 0)
                .map(|(_, y, _)| y)
                .fold((u32::MAX, 0), |(min, max), y| (min.min(y), max.max(y)))
        };
        let top = draw("Title", &parameters, 72, 72).unwrap();
        assert!(top
            .pixels()
            .any(|p| p[3] > 0 && p[0] > 0 && p[1] == 0 && p[2] == 0));
        parameters["titleAlignment"] = json!("bottom");
        let bottom = draw("Title", &parameters, 72, 72).unwrap();
        assert!(bounds(&bottom).0 > bounds(&top).1);
        parameters["showTitle"] = json!(false);
        assert!(draw("Title", &parameters, 72, 72)
            .unwrap()
            .pixels()
            .all(|p| p[3] == 0));
    }
}
