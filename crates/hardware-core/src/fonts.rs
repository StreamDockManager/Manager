//! Host font families with bundled, portable Noto defaults. Saved profiles store
//! family names only; system font files remain on the computer that owns them.
use resvg::usvg::fontdb::{Database, Family, Query};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, OnceLock, RwLock},
};

pub const SANS: &str = "Noto Sans";
pub const MONO: &str = "Noto Sans Mono";
const PREFIX: &str = "system:";
struct Catalog {
    db: Arc<Database>,
    families: Vec<String>,
    generation: u64,
    system_loaded: bool,
}
// Check whether the font family name is acceptable
fn valid_family(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.trim() == name
        && !name.chars().any(char::is_control)
}
// Report whether the value satisfies the supported constraints
pub fn valid(font: &str) -> bool {
    matches!(font, "sans" | "mono") || font.strip_prefix(PREFIX).is_some_and(valid_family)
}
// Load for the current operation
fn load(system: bool) -> Catalog {
    let mut db = Database::new();
    if system {
        #[cfg(target_os = "macos")]
        load_macos(&mut db);
        #[cfg(not(target_os = "macos"))]
        db.load_system_fonts();
        #[cfg(target_os = "windows")]
        load_windows_registered(&mut db);
    }
    // Use our bundled versions consistently, even when another Noto version is installed
    let duplicates: Vec<_> = db
        .faces()
        .filter(|face| face.families.iter().any(|(n, _)| n == SANS || n == MONO))
        .map(|face| face.id)
        .collect();
    for id in duplicates {
        db.remove_face(id);
    }
    let mut names = BTreeMap::new();
    for face in db.faces() {
        if let Some((name, _)) = face.families.first().filter(|(n, _)| valid_family(n)) {
            names
                .entry(name.to_lowercase())
                .or_insert_with(|| name.clone());
        }
    }
    db.load_font_data(include_bytes!("../../../public/appearance/fonts/NotoSans.ttf").to_vec());
    db.load_font_data(include_bytes!("../../../public/appearance/fonts/NotoSansMono.ttf").to_vec());
    install_svg_aliases(&mut db);
    db.set_sans_serif_family(SANS);
    db.set_monospace_family(MONO);
    Catalog {
        db: Arc::new(db),
        families: names.into_values().collect(),
        generation: 1,
        system_loaded: system,
    }
}
// Build the available font catalog
fn catalog() -> &'static RwLock<Catalog> {
    static CATALOG: OnceLock<RwLock<Catalog>> = OnceLock::new();
    CATALOG.get_or_init(|| RwLock::new(load(false)))
}
// Ensure system fonts
fn ensure_system_fonts(refresh: bool) {
    let catalog = catalog();
    if !refresh && catalog.read().unwrap().system_loaded {
        return;
    }
    static LOADING: Mutex<()> = Mutex::new(());
    let _loading = LOADING.lock().unwrap();
    if refresh || !catalog.read().unwrap().system_loaded {
        let mut next = load(true);
        let mut current = catalog.write().unwrap();
        next.generation = current.generation.wrapping_add(1);
        *current = next;
    }
}
// List the available font families
pub fn families(refresh: bool) -> Vec<String> {
    ensure_system_fonts(refresh);
    catalog().read().unwrap().families.clone()
}
// Build a font database for the selected family
pub fn database_for(font: &str) -> Arc<Database> {
    if font.starts_with(PREFIX) {
        ensure_system_fonts(false);
    }
    database()
}

// Open the database used by this operation
pub fn database() -> Arc<Database> {
    catalog().read().unwrap().db.clone()
}
// Return the current generation identifier
pub fn generation() -> u64 {
    catalog().read().unwrap().generation
}
// Resolve the requested font family
fn requested(font: &str) -> &str {
    match font {
        "mono" => MONO,
        "sans" => SANS,
        _ => font
            .strip_prefix(PREFIX)
            .filter(|n| valid_family(n))
            .unwrap_or(SANS),
    }
}
// Resolve a font family from the supplied value
pub fn family<'a>(font: &'a str, db: &'a Database) -> &'a str {
    let name = requested(font);
    if db
        .query(&Query {
            families: &[Family::Name(name)],
            ..Default::default()
        })
        .is_some()
    {
        return name;
    }
    // CSS family names are case insensitive. Imported vendor profiles can use
    // different capitalization from the installed font's name table.
    let folded = name.to_lowercase();
    db.faces()
        .flat_map(|face| &face.families)
        .find(|(candidate, _)| candidate.to_lowercase() == folded)
        .map(|(candidate, _)| candidate.as_str())
        .unwrap_or(SANS)
}

// Map the font family to an SVG-compatible alias
fn svg_alias(name: &str) -> Option<String> {
    // svgtypes keeps CSS escapes literal rather than decoding them. Give these
    // otherwise valid families a private ASCII alias instead of falling back.
    name.contains(['"', '\\']).then(|| {
        use std::fmt::Write;
        let mut alias = String::from("sdm-font-");
        for byte in name.bytes() {
            write!(alias, "{byte:02x}").unwrap();
        }
        alias
    })
}
// Install SVG aliases
fn install_svg_aliases(db: &mut Database) {
    let aliases: Vec<_> = db
        .faces()
        .filter_map(|face| {
            let families: Vec<_> = face
                .families
                .iter()
                .filter_map(|(name, language)| svg_alias(name).map(|alias| (alias, *language)))
                .collect();
            if families.is_empty() {
                return None;
            }
            let mut alias = face.clone();
            alias.families = families;
            Some(alias)
        })
        .collect();
    for face in aliases {
        db.push_face_info(face);
    }
}
/// Quote the entire family (including punctuation), then escape the XML attribute
pub fn svg_family(font: &str, db: &Database) -> String {
    let family = family(font, db);
    let alias = svg_alias(family);
    let name = alias
        .as_deref()
        .unwrap_or(family)
        .replace('\\', "\\005c ")
        .replace('"', "\\0022 ");
    format!("\"{name}\", \"{SANS}\"")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
// Measure glyph advances for the requested text
pub fn advances(font: &str, text: &str, size: f64, db: &Database) -> HashMap<char, f64> {
    #[cfg(target_os = "macos")]
    if let Some(advances) = crate::native_text::advances(font, text, size, db) {
        return advances;
    }
    let id = db.query(&Query {
        families: &[Family::Name(family(font, db))],
        ..Default::default()
    });
    id.and_then(|id| {
        db.with_face_data(id, |bytes, index| {
            ttf_parser::Face::parse(bytes, index).ok().map(|face| {
                let scale = size / f64::from(face.units_per_em());
                text.chars()
                    .map(|ch| {
                        (
                            ch,
                            face.glyph_index(ch)
                                .and_then(|g| face.glyph_hor_advance(g))
                                .map(|n| f64::from(n) * scale)
                                .unwrap_or(size * 0.6),
                        )
                    })
                    .collect()
            })
        })
    })
    .flatten()
    .unwrap_or_default()
}

// CoreText also includes fonts activated outside the standard font directories
#[cfg(target_os = "macos")]
fn load_macos(db: &mut Database) {
    use std::{
        ffi::{c_void, CStr},
        os::unix::ffi::OsStringExt,
    };
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        // Declare the native CTFont Manager Copy Available Font URLs operation used by this adapter
        fn CTFontManagerCopyAvailableFontURLs() -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        // Declare the native CFArray Get Count operation used by this adapter
        fn CFArrayGetCount(array: *const c_void) -> isize;
        // Declare the native CFArray Get Value At Index operation used by this adapter
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        // Declare the native CFURLGet File System Representation operation used by this adapter
        fn CFURLGetFileSystemRepresentation(
            url: *const c_void,
            resolve: u8,
            buffer: *mut u8,
            size: isize,
        ) -> u8;
        // Declare the native CFRelease operation used by this adapter
        fn CFRelease(value: *const c_void);
    }
    // SAFETY: CoreText returns an owned CFArray of CFURLs. Values are borrowed
    // only while the array lives, and the output buffer is bounded and checked.
    unsafe {
        let urls = CTFontManagerCopyAvailableFontURLs();
        if urls.is_null() {
            db.load_system_fonts();
            return;
        }
        let mut paths = std::collections::BTreeSet::new();
        for index in 0..CFArrayGetCount(urls) {
            let url = CFArrayGetValueAtIndex(urls, index);
            let mut buffer = [0u8; 4096];
            if !url.is_null()
                && CFURLGetFileSystemRepresentation(
                    url,
                    1,
                    buffer.as_mut_ptr(),
                    buffer.len() as isize,
                ) != 0
            {
                if let Ok(path) = CStr::from_bytes_until_nul(&buffer) {
                    paths.insert(std::path::PathBuf::from(std::ffi::OsString::from_vec(
                        path.to_bytes().to_vec(),
                    )));
                }
            }
        }
        CFRelease(urls);
        for path in paths {
            let _ = db.load_font_file(path);
        }
    }
}

#[cfg(target_os = "windows")]
// Load windows registered
fn load_windows_registered(db: &mut Database) {
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::{Environment::ExpandEnvironmentStringsW, Registry::*},
    };
    // Directory discovery covers the usual system/per-user locations. Registry
    // entries additionally cover fonts installed at an arbitrary absolute path.
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts\0"
        .encode_utf16()
        .collect();
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut handle = std::ptr::null_mut();
        // SAFETY: terminated UTF-16 input, valid sized output buffers, read-only
        // registry access. Every successfully opened handle is closed below.
        unsafe {
            if RegOpenKeyExW(hive, key.as_ptr(), 0, KEY_QUERY_VALUE, &mut handle) != ERROR_SUCCESS {
                continue;
            }
            for index in 0..65536 {
                let mut name = vec![0u16; 16384];
                let mut name_len = name.len() as u32;
                let mut value = vec![0u16; 32769];
                // Leave a terminator even for malformed, unterminated REG_SZ data
                let mut bytes = ((value.len() - 1) * 2) as u32;
                let mut kind = 0;
                let status = RegEnumValueW(
                    handle,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null(),
                    &mut kind,
                    value.as_mut_ptr().cast(),
                    &mut bytes,
                );
                if status == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if status != ERROR_SUCCESS || ![REG_SZ, REG_EXPAND_SZ].contains(&kind) {
                    continue;
                }
                let end = value.iter().position(|v| *v == 0).unwrap_or(value.len());
                let mut path = String::from_utf16_lossy(&value[..end]);
                if kind == REG_EXPAND_SZ {
                    let mut expanded = vec![0u16; 32768];
                    let written = ExpandEnvironmentStringsW(
                        value.as_ptr(),
                        expanded.as_mut_ptr(),
                        expanded.len() as u32,
                    );
                    if written == 0 || written as usize > expanded.len() {
                        continue;
                    }
                    path = String::from_utf16_lossy(&expanded[..written as usize - 1]);
                }
                let path = std::path::Path::new(&path);
                let known = db.faces().any(
                    |f| matches!(&f.source, resvg::usvg::fontdb::Source::File(p) if p == path),
                );
                if path.is_absolute() && !known {
                    let _ = db.load_font_file(path);
                }
            }
            RegCloseKey(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Locate the bundled font resources
    fn bundled() -> Database {
        let mut db = Database::new();
        db.load_font_data(include_bytes!("../../../public/appearance/fonts/NotoSans.ttf").to_vec());
        db.load_font_data(
            include_bytes!("../../../public/appearance/fonts/NotoSansMono.ttf").to_vec(),
        );
        db
    }
    #[test]
    // Verify defaults legacy choices validation and missing font fallback are portable
    fn defaults_legacy_choices_validation_and_missing_font_fallback_are_portable() {
        let db = bundled();
        assert_eq!(crate::appearance::Appearance::default().font, "sans");
        assert_eq!(crate::plugin_display::ElementStyle::default().font, "sans");
        assert_eq!(family("sans", &db), SANS);
        assert_eq!(family("mono", &db), MONO);
        assert_eq!(family("system:noto sans mono", &db), MONO);
        assert_eq!(family("system:Missing fixture font", &db), SANS);
        for font in [
            "sans",
            "mono",
            "system:Arial",
            "system:日本語書体",
            "system:Family, With 'quotes' & More",
        ] {
            assert!(valid(font));
            let style = crate::appearance::Appearance {
                font: font.into(),
                ..Default::default()
            };
            style.validate().unwrap();
            let parsed: crate::appearance::Appearance =
                serde_json::from_value(serde_json::to_value(&style).unwrap()).unwrap();
            assert_eq!(parsed.font, font);
        }
        for font in [
            "",
            "Arial",
            "system:",
            "system: leading",
            "system:trailing ",
            "system:bad\nfont",
            "system:bad\0font",
        ] {
            assert!(!valid(font));
        }
        assert!(!valid(&format!("system:{}", "x".repeat(257))));
        assert_eq!(
            advances("system:Missing fixture font", "Wi 123", 24., &db),
            advances("sans", "Wi 123", 24., &db)
        );
    }
    #[test]
    // Verify family names are literal and cannot inject SVG or CSS
    fn family_names_are_literal_and_cannot_inject_svg_or_css() {
        let mut db = bundled();
        let mut face = db.faces().next().unwrap().clone();
        let name = "A \"Quote\", B & C <Font>";
        face.families[0].0 = name.into();
        db.push_face_info(face);
        install_svg_aliases(&mut db);
        let font = format!("system:{name}");
        assert_eq!(family(&font, &db), name);
        let attribute = svg_family(&font, &db);
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"80\" height=\"40\"><text y=\"30\" font-family=\"{attribute}\">Text</text></svg>");
        let document = roxmltree::Document::parse(&svg).unwrap();
        assert_eq!(document.descendants().filter(|n| n.is_element()).count(), 2);
        let options = resvg::usvg::Options {
            fontdb: Arc::new(db),
            ..Default::default()
        };
        resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    }
    #[test]
    // Verify escaped family names select the requested glyphs
    fn escaped_family_names_select_the_requested_glyphs() {
        let mut db = bundled();
        let mono = db
            .query(&Query {
                families: &[Family::Name(MONO)],
                ..Default::default()
            })
            .unwrap();
        let mut face = db.face(mono).unwrap().clone();
        let name = "Fixture \"Mono\" \\ Family";
        face.families = vec![(
            name.into(),
            resvg::usvg::fontdb::Language::English_UnitedStates,
        )];
        db.push_face_info(face);
        install_svg_aliases(&mut db);
        let render = |font: &str| {
            let family = svg_family(font, &db);
            let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"60\"><text x=\"5\" y=\"40\" font-size=\"28\" font-family=\"{family}\">Wi 123</text></svg>");
            let options = resvg::usvg::Options {
                fontdb: Arc::new(db.clone()),
                ..Default::default()
            };
            let tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut image = resvg::tiny_skia::Pixmap::new(200, 60).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut image.as_mut(),
            );
            image.take()
        };
        assert!(
            render(&format!("system:{name}")) == render("mono"),
            "escaped family must use mono glyphs, not fallback"
        );
    }
    #[test]
    #[ignore = "Requires installed host fonts; renders only, no hardware or font installation changes"]
    // Verify every advertised host family renders visible text
    fn every_advertised_host_family_renders_visible_text() {
        let names = families(false);
        assert!(!names.is_empty());
        let base = crate::render::solid_rgba(192, 112, [0; 4]).unwrap();
        let mut failures = Vec::new();
        for name in &names {
            let font = format!("system:{name}");
            let png = crate::appearance::compose(
                &base,
                192,
                112,
                "Wi ABC 123 !@",
                &crate::appearance::Appearance {
                    font: font.clone(),
                    size: 26,
                    ..Default::default()
                },
                "",
                0,
            )
            .unwrap();
            let title = image::load_from_memory(&png).unwrap().to_rgba8();
            let field = crate::plugin_display::text_image(
                "Wi ABC 123 !@",
                &crate::plugin_display::ElementStyle {
                    font,
                    layout: "fixture".into(),
                    key: "text".into(),
                    size: 26.,
                    ..Default::default()
                },
                192,
                112,
                112,
            )
            .unwrap();
            if !title.pixels().any(|pixel| pixel[3] != 0)
                || !field.pixels().any(|pixel| pixel[3] != 0)
            {
                failures.push(name.clone());
            }
        }
        assert!(
            failures.is_empty(),
            "Advertised families render blank text: {failures:?}"
        );
        eprintln!("Rendered {} host families through title and plugin/custom text paths; not hardware qualification", names.len());
    }
    #[test]
    #[ignore = "Requires installed host fonts; no hardware or font installation changes"]
    // Verify installed font changes real glyph pixels and refreshes the catalog
    fn installed_font_changes_real_glyph_pixels_and_refreshes_the_catalog() {
        let names = families(false);
        let selected = [
            "Georgia",
            "Times New Roman",
            "Arial",
            "Menlo",
            "DejaVu Serif",
            "Liberation Serif",
        ]
        .iter()
        .find_map(|wanted| names.iter().find(|name| name.eq_ignore_ascii_case(wanted)))
        .expect("Install a native test font to qualify system rendering");
        let font = format!("system:{selected}");
        let db = database();
        assert_eq!(family(&font, &db), selected);
        let base = crate::render::solid_rgba(192, 112, [0; 4]).unwrap();
        let appearance = crate::appearance::Appearance {
            font: font.clone(),
            size: 26,
            ..Default::default()
        };
        let system =
            crate::appearance::compose(&base, 192, 112, "Wi WWW 123", &appearance, "", 0).unwrap();
        let noto = crate::appearance::compose(
            &base,
            192,
            112,
            "Wi WWW 123",
            &crate::appearance::Appearance {
                font: "sans".into(),
                ..appearance
            },
            "",
            0,
        )
        .unwrap();
        assert_ne!(
            system, noto,
            "The selected font must change glyphs, not only its saved name"
        );
        let style = crate::plugin_display::ElementStyle {
            font,
            layout: "fixture".into(),
            key: "text".into(),
            size: 26.,
            ..Default::default()
        };
        let system =
            crate::plugin_display::text_image("Wi WWW 123", &style, 192, 112, 112).unwrap();
        let noto = crate::plugin_display::text_image(
            "Wi WWW 123",
            &crate::plugin_display::ElementStyle {
                font: "sans".into(),
                ..style
            },
            192,
            112,
            112,
        )
        .unwrap();
        assert_ne!(system, noto);
        let before = generation();
        assert!(families(true).contains(selected));
        assert!(generation() > before);
        eprintln!(
            "Qualified native system font rendering with {selected}; {} available families",
            names.len()
        );
    }
}
