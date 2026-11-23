//! CoreText bridge for nonportable macOS fonts and missing-script fallback
//! The generated text SVG is the only input; no external image source is enabled
use crate::{Error, Result};
use resvg::usvg::{
    fontdb::{Database, Family, Query},
    Options,
};
use std::{collections::HashMap, ffi::c_void, sync::Arc};

type Ref = *const c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    origin: Point,
    size: Size,
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    // Declare the native CFRelease operation used by this adapter
    fn CFRelease(value: Ref);
    #[cfg(test)]
    // Declare the native CFEqual operation used by this adapter
    fn CFEqual(first: Ref, second: Ref) -> u8;
    // Declare the native CFData Create operation used by this adapter
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    // Declare the native CFURLCreate From File System Representation operation used by this adapter
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Ref,
        buffer: *const u8,
        length: isize,
        directory: u8,
    ) -> Ref;
    // Declare the native CFString Create With Bytes operation used by this adapter
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    // Declare the native CFDictionary Create operation used by this adapter
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    // Declare the native CFAttributed String Create operation used by this adapter
    fn CFAttributedStringCreate(allocator: Ref, string: Ref, attributes: Ref) -> Ref;
    // Declare the native CFNumber Create operation used by this adapter
    fn CFNumberCreate(allocator: Ref, kind: isize, value: *const c_void) -> Ref;
    static kCFBooleanTrue: Ref;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontAttributeName: Ref;
    static kCTFontNameAttribute: Ref;
    static kCTFontURLAttribute: Ref;
    // Declare the native CTFont Descriptor Create With Attributes operation used by this adapter
    fn CTFontDescriptorCreateWithAttributes(attributes: Ref) -> Ref;
    // Declare the native CTFont Create With Font Descriptor operation used by this adapter
    fn CTFontCreateWithFontDescriptor(descriptor: Ref, size: f64, matrix: Ref) -> Ref;
    static kCTForegroundColorFromContextAttributeName: Ref;
    static kCTUnderlineStyleAttributeName: Ref;
    // Declare the native CTFont Create With Name operation used by this adapter
    fn CTFontCreateWithName(name: Ref, size: f64, matrix: Ref) -> Ref;
    #[cfg(test)]
    // Declare the native CTFont Copy Post Script Name operation used by this adapter
    fn CTFontCopyPostScriptName(font: Ref) -> Ref;
    // Declare the native CTFont Create With Graphics Font operation used by this adapter
    fn CTFontCreateWithGraphicsFont(font: Ref, size: f64, matrix: Ref, attributes: Ref) -> Ref;
    // Declare the native CTFont Create Copy With Symbolic Traits operation used by this adapter
    fn CTFontCreateCopyWithSymbolicTraits(
        font: Ref,
        size: f64,
        matrix: Ref,
        traits: u32,
        mask: u32,
    ) -> Ref;
    // Declare the native CTLine Create With Attributed String operation used by this adapter
    fn CTLineCreateWithAttributedString(string: Ref) -> Ref;
    // Declare the native CTLine Get Typographic Bounds operation used by this adapter
    fn CTLineGetTypographicBounds(
        line: Ref,
        ascent: *mut f64,
        descent: *mut f64,
        leading: *mut f64,
    ) -> f64;
    // Declare the native CTLine Get Image Bounds operation used by this adapter
    fn CTLineGetImageBounds(line: Ref, context: Ref) -> Rect;
    // Declare the native CTLine Draw operation used by this adapter
    fn CTLineDraw(line: Ref, context: Ref);
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // Declare the native CGColor Space Create Device RGB operation used by this adapter
    fn CGColorSpaceCreateDeviceRGB() -> Ref;
    // Declare the native CGData Provider Create With CFData operation used by this adapter
    fn CGDataProviderCreateWithCFData(data: Ref) -> Ref;
    // Declare the native CGFont Create With Data Provider operation used by this adapter
    fn CGFontCreateWithDataProvider(provider: Ref) -> Ref;
    // Declare the native CGBitmap Context Create operation used by this adapter
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits: usize,
        row: usize,
        space: Ref,
        info: u32,
    ) -> Ref;
    // Declare the native CGContext Set RGBFill Color operation used by this adapter
    fn CGContextSetRGBFillColor(context: Ref, red: f64, green: f64, blue: f64, alpha: f64);
    // Declare the native CGContext Set RGBStroke Color operation used by this adapter
    fn CGContextSetRGBStrokeColor(context: Ref, red: f64, green: f64, blue: f64, alpha: f64);
    // Declare the native CGContext Set Line Width operation used by this adapter
    fn CGContextSetLineWidth(context: Ref, width: f64);
    // Declare the native CGContext Set Text Drawing Mode operation used by this adapter
    fn CGContextSetTextDrawingMode(context: Ref, mode: i32);
    // Declare the native CGContext Set Text Position operation used by this adapter
    fn CGContextSetTextPosition(context: Ref, x: f64, y: f64);
    // Declare the native CGContext Flush operation used by this adapter
    fn CGContextFlush(context: Ref);
}
struct Owned(Ref);
impl Owned {
    // Construct the value and initialize its state
    fn new(value: Ref) -> Result<Self> {
        if value.is_null() {
            Err(Error::Protocol(
                "Could not create native font renderer".into(),
            ))
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Owned {
    // Release resources held by this owner
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
// Convert the string into the representation required by this API
fn string(value: &str) -> Result<Owned> {
    // SAFETY: UTF-8 pointer is valid for its stated length; CoreFoundation copies it.
    unsafe {
        Owned::new(CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as isize,
            0x08000100,
            0,
        ))
    }
}
// Check whether native text rendering is required
fn needed(font: &str, text: &str, db: &Database) -> bool {
    let name = crate::fonts::family(font, db);
    db.query(&Query {
        families: &[Family::Name(name)],
        ..Default::default()
    })
    .and_then(|id| {
        db.with_face_data(id, |bytes, index| {
            ttf_parser::Face::parse(bytes, index)
                .ok()
                .is_none_or(|face| {
                    [b"glyf", b"CFF ", b"CFF2"].iter().all(|tag| {
                        face.raw_face()
                            .table(ttf_parser::Tag::from_bytes(tag))
                            .is_none()
                    }) || text.chars().any(|ch| {
                        !ch.is_control() && !ch.is_whitespace() && face.glyph_index(ch).is_none()
                    })
                })
        })
    })
    .unwrap_or(false)
}
// Create font
fn create_font(family: &str, size: f64, db: &Database) -> Result<Owned> {
    let id = db.query(&Query {
        families: &[Family::Name(family)],
        ..Default::default()
    });
    let face = id.and_then(|id| db.face(id));
    let name = string(face.map(|f| f.post_script_name.as_str()).unwrap_or(family))?;
    unsafe {
        if let Some(
            resvg::usvg::fontdb::Source::File(path)
            | resvg::usvg::fontdb::Source::SharedFile(path, _),
        ) = face.map(|f| &f.source)
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            let url = Owned::new(CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                0,
            ))?;
            let keys = [kCTFontNameAttribute, kCTFontURLAttribute];
            let values = [name.0, url.0];
            let attrs = Owned::new(CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            ))?;
            let descriptor = Owned::new(CTFontDescriptorCreateWithAttributes(attrs.0))?;
            return Owned::new(CTFontCreateWithFontDescriptor(
                descriptor.0,
                size,
                std::ptr::null(),
            ));
        }
        if let Some(id) = id {
            // Embedded portable fonts are not necessarily registered with macOS
            // Build the exact font from its owned bytes before asking CoreText
            // to supply missing-script/emoji fallback glyphs.
            if let Some(result) = db.with_face_data(id, |bytes, _| -> Result<Owned> {
                let data = Owned::new(CFDataCreate(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                ))?;
                let provider = Owned::new(CGDataProviderCreateWithCFData(data.0))?;
                let graphics = Owned::new(CGFontCreateWithDataProvider(provider.0))?;
                Owned::new(CTFontCreateWithGraphicsFont(
                    graphics.0,
                    size,
                    std::ptr::null(),
                    std::ptr::null(),
                ))
            }) {
                return result;
            }
        }
        Owned::new(CTFontCreateWithName(name.0, size, std::ptr::null()))
    }
}
// Construct the line representation
fn line(
    text: &str,
    family: &str,
    size: f64,
    bold: bool,
    italic: bool,
    underline: bool,
    db: &Database,
) -> Result<Owned> {
    let font = styled_font(family, size, bold, italic, db)?;
    attributed_line(text, &font, underline)
}
// Reuse one styled native font for both glyph metrics and complete line rendering
fn styled_font(family: &str, size: f64, bold: bool, italic: bool, db: &Database) -> Result<Owned> {
    let font = create_font(family, size, db)?;
    let traits = u32::from(italic) | (u32::from(bold) << 1);
    let styled = if traits != 0 {
        unsafe {
            Owned::new(CTFontCreateCopyWithSymbolicTraits(
                font.0,
                size,
                std::ptr::null(),
                traits,
                traits,
            ))
            .ok()
        }
    } else {
        None
    };
    Ok(styled.unwrap_or(font))
}
// Create a native text line with the supplied attributes
fn attributed_line(text: &str, font: &Owned, underline: bool) -> Result<Owned> {
    let text = string(text)?;
    // SAFETY: CFType callbacks retain each attribute value. The resulting line
    // owns its attributed string and fonts independently of these temporaries.
    unsafe {
        let value = i32::from(underline);
        let underline = Owned::new(CFNumberCreate(
            std::ptr::null(),
            3,
            (&value as *const i32).cast(),
        ))?;
        let keys = [
            kCTFontAttributeName,
            kCTForegroundColorFromContextAttributeName,
            kCTUnderlineStyleAttributeName,
        ];
        let values = [font.0, kCFBooleanTrue, underline.0];
        let attributes = Owned::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        ))?;
        let attributed = Owned::new(CFAttributedStringCreate(
            std::ptr::null(),
            text.0,
            attributes.0,
        ))?;
        Owned::new(CTLineCreateWithAttributedString(attributed.0))
    }
}
// Measure the rendered text width
fn width(line: &Owned) -> f64 {
    unsafe {
        CTLineGetTypographicBounds(
            line.0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}
// Measure regular glyph advances for native text regression tests
#[cfg(test)]
pub(crate) fn advances(
    font: &str,
    text: &str,
    size: f64,
    db: &Database,
) -> Option<HashMap<char, f64>> {
    styled_advances(font, text, size, db, false, false)
}
// Match CoreText measurements to the symbolic traits used by the rendered line
pub(crate) fn styled_advances(
    font: &str,
    text: &str,
    size: f64,
    db: &Database,
    bold: bool,
    italic: bool,
) -> Option<HashMap<char, f64>> {
    if !needed(font, text, db) {
        return None;
    }
    let family = crate::fonts::family(font, db);
    let font = styled_font(family, size, bold, italic, db).ok()?;
    let mut result = HashMap::new();
    for ch in text.chars() {
        if let std::collections::hash_map::Entry::Vacant(entry) = result.entry(ch) {
            entry.insert(width(&attributed_line(&ch.to_string(), &font, false).ok()?));
        }
    }
    Some(result)
}

/// Replace generated plain text nodes only. Each bitmap is available solely by
/// its exact private name in this parse; arbitrary files, URLs and data stay off.
pub(crate) fn prepare_svg(
    svg: &str,
    font: &str,
    options: &mut Options<'_>,
) -> Result<(String, bool)> {
    let doc = roxmltree::Document::parse(svg).map_err(|e| Error::Protocol(e.to_string()))?;
    if !doc
        .descendants()
        .filter(|node| node.has_tag_name("text"))
        .any(|node| needed(font, node.text().unwrap_or(""), &options.fontdb))
    {
        return Ok((svg.into(), false));
    }
    let root = doc.root_element();
    let number = |node: roxmltree::Node<'_, '_>, name: &str, default: f64| {
        node.attribute(name)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .unwrap_or(default)
    };
    let width = number(root, "width", 0.);
    let height = number(root, "height", 0.);
    if !(1.0..=4096.).contains(&width) || !(1.0..=4096.).contains(&height) {
        return Err(Error::Protocol("Invalid native text surface".into()));
    }
    let family = crate::fonts::family(font, &options.fontdb).to_owned();
    let mut images = HashMap::new();
    let mut replacements = vec![];
    let mut clipped = false;
    for (index, node) in doc
        .descendants()
        .filter(|n| n.has_tag_name("text"))
        .enumerate()
    {
        let text = node.text().unwrap_or("");
        if !needed(font, text, &options.fontdb) {
            continue;
        }
        let size = number(node, "font-size", 16.).clamp(1., 4096.);
        let underline = node.attribute("text-decoration") == Some("underline");
        let line = line(
            text,
            &family,
            size,
            node.attribute("font-weight") == Some("bold"),
            node.attribute("font-style") == Some("italic"),
            underline,
            &options.fontdb,
        )?;
        let advance = width_of(&line);
        let x = number(node, "x", 0.)
            - match node.attribute("text-anchor") {
                Some("middle") => advance / 2.,
                Some("end") => advance,
                _ => 0.,
            };
        let y = number(node, "y", 0.);
        let outline = number(node, "stroke-width", 0.).clamp(0., 16.);
        let mut bounds = unsafe { CTLineGetImageBounds(line.0, std::ptr::null()) };
        if bounds.size.width <= 0. || bounds.size.height <= 0. {
            let (mut ascent, mut descent, mut leading) = (0., 0., 0.);
            unsafe {
                CTLineGetTypographicBounds(line.0, &mut ascent, &mut descent, &mut leading);
            }
            bounds = Rect {
                origin: Point { x: 0., y: -descent },
                size: Size {
                    width: advance,
                    height: ascent + descent,
                },
            };
        }
        let pad = outline + 2.;
        let left = (x + bounds.origin.x - pad).floor();
        let right = (x + bounds.origin.x + bounds.size.width + pad).ceil();
        let top = (y - bounds.origin.y - bounds.size.height - pad).floor();
        let bottom = (y - bounds.origin.y + pad + if underline { size * 0.2 } else { 0. }).ceil();
        clipped |=
            left + pad < 0. || top + pad < 0. || right - pad > width || bottom - pad > height;
        let left = left.max(0.);
        let top = top.max(0.);
        let right = right.min(width);
        let bottom = bottom.min(height);
        let w = (right - left).max(0.) as u32;
        let h = (bottom - top).max(0.) as u32;
        if w == 0 || h == 0 || text.is_empty() {
            replacements.push((node.range(), String::new()));
            continue;
        }
        let mut pixels = vec![0u8; w as usize * h as usize * 4];
        let color =
            crate::appearance::background_color(node.attribute("fill").unwrap_or("#ffffff"))
                .unwrap_or([255; 4]);
        // SAFETY: buffer outlives context, row stride and allocation match; every
        // created object is released. Big-endian premultiplied-last is RGBA.
        unsafe {
            let space = Owned::new(CGColorSpaceCreateDeviceRGB())?;
            let context = Owned::new(CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                w as usize,
                h as usize,
                8,
                w as usize * 4,
                space.0,
                (4 << 12) | 1,
            ))?;
            CGContextSetRGBFillColor(
                context.0,
                color[0] as f64 / 255.,
                color[1] as f64 / 255.,
                color[2] as f64 / 255.,
                color[3] as f64 / 255.,
            );
            if outline > 0. {
                CGContextSetRGBStrokeColor(context.0, 0., 0., 0., 1.);
                CGContextSetLineWidth(context.0, outline);
                // Match the generated SVG's stroke-then-fill paint order
                CGContextSetTextDrawingMode(context.0, 1);
                CGContextSetTextPosition(context.0, x - left, bottom - y);
                CTLineDraw(line.0, context.0);
                CGContextSetTextDrawingMode(context.0, 0);
            }
            CGContextSetTextPosition(context.0, x - left, bottom - y);
            CTLineDraw(line.0, context.0);
            CGContextFlush(context.0);
        }
        let pixmap = resvg::tiny_skia::Pixmap::from_vec(
            pixels,
            resvg::tiny_skia::IntSize::from_wh(w, h).unwrap(),
        )
        .ok_or_else(|| Error::Protocol("Invalid native glyph pixels".into()))?;
        let png = pixmap
            .encode_png()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let name = format!("sdm-native-text:{index}");
        images.insert(name.clone(), Arc::new(png));
        replacements.push((
            node.range(),
            format!(
                "<image x=\"{left}\" y=\"{top}\" width=\"{w}\" height=\"{h}\" href=\"{name}\"/>"
            ),
        ));
    }
    let mut result = svg.to_string();
    for (range, replacement) in replacements.into_iter().rev() {
        result.replace_range(range, &replacement);
    }
    options.image_href_resolver.resolve_string =
        Box::new(move |name, _| images.get(name).cloned().map(resvg::usvg::ImageKind::PNG));
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    Ok((result, clipped))
}
// Measure the width of the supplied text
fn width_of(line: &Owned) -> f64 {
    width(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Uses installed macOS fonts without modifying the host"]
    // Verify native bitmap and hvgl samples
    fn native_bitmap_and_hvgl_samples() {
        crate::fonts::families(false);
        let output =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/font-audit");
        std::fs::create_dir_all(&output).unwrap();
        for name in ["PingFang SC", "GB18030 Bitmap", "Inter", "Georgia"] {
            for size in [12, 16, 24, 32] {
                let font = format!("system:{name}");
                let db = crate::fonts::database_for(&font);
                let mut options = Options {
                    fontdb: db,
                    ..Default::default()
                };
                let source = format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"300\" height=\"70\"><text x=\"10\" y=\"45\" font-size=\"{size}\" fill=\"#ff8000\">Abc 汉字 !</text></svg>"
                );
                let (source, _) = prepare_svg(&source, &font, &mut options).unwrap();
                let tree = resvg::usvg::Tree::from_str(&source, &options).unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(300, 70).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                let ink = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
                assert!(ink > 0, "{name} at {size} must render visible glyphs");
                eprintln!("{name} size{size}: {ink} ink pixels");
                pixmap
                    .save_png(output.join(format!("{}-{size}.png", name.replace(' ', "-"))))
                    .unwrap();
            }
        }
    }
    #[test]
    #[ignore = "Uses installed macOS fonts without modifying the host"]
    // Verify native font identity clipping styles and resource isolation
    fn native_font_identity_clipping_styles_and_resource_isolation() {
        let families = crate::fonts::families(false);
        let db = crate::fonts::database();
        for family in families {
            let id = format!("system:{family}");
            if !needed(&id, "", &db) {
                continue;
            }
            let face = db
                .query(&Query {
                    families: &[Family::Name(&family)],
                    ..Default::default()
                })
                .and_then(|id| db.face(id))
                .unwrap();
            let font = create_font(&family, 24., &db).unwrap();
            let expected = string(&face.post_script_name).unwrap();
            let actual = unsafe { Owned::new(CTFontCopyPostScriptName(font.0)).unwrap() };
            assert_ne!(
                unsafe { CFEqual(expected.0, actual.0) },
                0,
                "CoreText must not substitute another font for {family}"
            );
        }
        let metrics = advances("system:PingFang SC", "Wi  ", 24., &db).unwrap();
        assert!(metrics[&' '] > 0., "Whitespace must retain its advance");
        assert_eq!(
            metrics.len(),
            3,
            "Repeated characters share measured metrics"
        );
        let mut options = Options {
            fontdb: db.clone(),
            ..Default::default()
        };
        let mixed = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"60\"><text y=\"20\">Latin</text><text y=\"40\">汉字</text></svg>";
        let (prepared, _) = prepare_svg(mixed, "sans", &mut options).unwrap();
        assert!(prepared.contains("<text y=\"20\">Latin</text>"));
        assert!(prepared.contains("sdm-native-text:1"));
        let render = |x: f64, style: &str| {
            let mut options = Options {
                fontdb: db.clone(),
                ..Default::default()
            };
            let source = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"60\"><text x=\"{x}\" y=\"40\" font-size=\"28\" fill=\"#ff8000\" {style}>Abc 汉字</text></svg>"
            );
            let (source, clipped) =
                prepare_svg(&source, "system:PingFang SC", &mut options).unwrap();
            assert!(
                (options.image_href_resolver.resolve_string)("/etc/passwd", &options).is_none()
            );
            assert!((options.image_href_resolver.resolve_string)(
                "sdm-native-text:missing",
                &options
            )
            .is_none());
            assert!((options.image_href_resolver.resolve_data)(
                "image/png",
                Arc::new(vec![]),
                &options
            )
            .is_none());
            let tree = resvg::usvg::Tree::from_str(&source, &options).unwrap();
            let mut image = resvg::tiny_skia::Pixmap::new(160, 60).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut image.as_mut(),
            );
            (image, clipped)
        };
        let (plain, clipped) = render(5., "");
        assert!(!clipped);
        assert!(render(-15., "").1);
        let (styled, _) = render(
            5.,
            "font-weight=\"bold\" font-style=\"italic\" text-decoration=\"underline\" stroke=\"#000000\" stroke-width=\"2\"",
        );
        assert_ne!(plain.data(), styled.data());
        assert!(styled
            .pixels()
            .iter()
            .any(|p| p.alpha() > 0 && p.red() == 0));
    }
}
