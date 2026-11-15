//! Icon-only SVG normalization. Media rendering keeps the original artwork
use std::borrow::Cow;

/// Older plugin icons paint a canvas-sized rectangle before their glyph. Remove
/// that backplate at the icon boundary, retaining vector details and source files.
pub fn transparent_svg(source: &[u8]) -> Cow<'_, [u8]> {
    let unchanged = || Cow::Borrowed(source);
    let Ok(svg) = std::str::from_utf8(source) else {
        return unchanged();
    };
    if !svg.contains("<rect") {
        return unchanged();
    }
    let Ok(document) = roxmltree::Document::parse(svg) else {
        return unchanged();
    };
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return unchanged();
    }
    let number = |value: &str| {
        value
            .trim()
            .trim_end_matches("px")
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
    };
    let bounds: Option<[f64; 4]> = if let Some(view_box) = root.attribute("viewBox") {
        view_box
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|part| !part.is_empty())
            .map(number)
            .collect::<Option<Vec<_>>>()
            .and_then(|values| values.try_into().ok())
    } else {
        root.attribute("width")
            .and_then(number)
            .zip(root.attribute("height").and_then(number))
            .map(|(w, h)| [0.0, 0.0, w, h])
    };
    let Some([x, y, width, height]) = bounds.filter(|b| b[2] > 0.0 && b[3] > 0.0) else {
        return unchanged();
    };
    let mut shapes = root.children().filter(|node| {
        node.is_element()
            && !["defs", "title", "desc", "metadata", "style"].contains(&node.tag_name().name())
    });
    let Some(rect) = shapes.next() else {
        return unchanged();
    };
    if rect.tag_name().name() != "rect"
        || rect.attribute("transform").is_some()
        || shapes.next().is_none()
    {
        return unchanged();
    }
    let dimension = |name, full| {
        let value = rect.attribute(name)?;
        if value.trim() == "100%" {
            Some(full)
        } else {
            number(value)
        }
    };
    let coordinate = |name| rect.attribute(name).map_or(Some(0.0), number);
    if ![
        (coordinate("x").unwrap_or(f64::NAN), x),
        (coordinate("y").unwrap_or(f64::NAN), y),
        (dimension("width", width).unwrap_or(0.0), width),
        (dimension("height", height).unwrap_or(0.0), height),
    ]
    .iter()
    .all(|(a, b)| (a - b).abs() < 0.000001)
    {
        return unchanged();
    }
    let range = rect.range();
    let mut result = Vec::with_capacity(source.len() - range.len());
    result.extend_from_slice(&source[..range.start]);
    result.extend_from_slice(&source[range.end..]);
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify icon backplates are removed without changing glyph geometry
    fn icon_backplates_are_removed_without_changing_glyph_geometry() {
        for fill in ["#121212", "#ffffff", "#0f141d", "#ff0000"] {
            for size in [
                "width=\"32\" height=\"32\"",
                "width=\"100%\" height=\"100%\"",
            ] {
                let svg = format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0,0,32,32"><title>Run</title><defs/><rect {size} fill="{fill}"/><g fill="none" stroke="white"><rect x="12" y="12" width="8" height="8"/><path d="M8 8h16"/></g></svg>"#
                );
                let clean = transparent_svg(svg.as_bytes());
                let doc = roxmltree::Document::parse(std::str::from_utf8(&clean).unwrap()).unwrap();
                assert_eq!(
                    doc.descendants().filter(|n| n.has_tag_name("rect")).count(),
                    1
                );
                let bytes = crate::render::render(&clean, 64, 64, 0, "png").unwrap();
                let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
                assert_eq!(image.get_pixel(0, 0)[3], 0);
                assert!(image.pixels().any(|p| p[3] > 0));
            }
        }
    }
    #[test]
    // Verify original media and foreground shapes are not removed
    fn original_media_and_foreground_shapes_are_not_removed() {
        for svg in [
            r#"<svg viewBox="0 0 32 32"><rect width="32" height="32"/></svg>"#,
            r#"<svg viewBox="0 0 32 32"><rect width="16" height="16"/><path d="M0 0h32"/></svg>"#,
            r#"<svg viewBox="0 0 32 32"><g><rect width="32" height="32"/></g><path d="M0 0h32"/></svg>"#,
            r#"<svg viewBox="0 0 32 32"><rect transform="translate(16 0)" width="32" height="32"/><path d="M0 0h32"/></svg>"#,
            "invalid SVG",
        ] {
            assert_eq!(&*transparent_svg(svg.as_bytes()), svg.as_bytes());
        }
        let png = crate::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                32,
                32,
                image::Rgb([20, 50, 90]),
            )),
            "png",
        )
        .unwrap();
        assert_eq!(&*transparent_svg(&png), png.as_slice());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32"/><circle cx="16" cy="16" r="4"/></svg>"#;
        assert_ne!(&*transparent_svg(svg), svg);
        // The general media renderer deliberately retains an SVG's background
        let rendered = crate::render::render(svg, 32, 32, 0, "png").unwrap();
        assert_eq!(
            image::load_from_memory(&rendered)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            255
        );
    }
}
