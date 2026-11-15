//! Bounded, read-time thumbnails for source-defined application presentation
use super::*;
use base64::Engine;

// Check support for OS
pub(super) fn supports_os(action: &Value, platform: &str) -> bool {
    action["OS"].as_array().is_none_or(|platforms| {
        platforms
            .iter()
            .any(|value| value.as_str() == Some(platform))
    })
}

/// Resolve manifest icon references without reading or decoding their contents
/// Keep compatibility review and catalogue thumbnails on the same vendor rules
pub(super) fn image_path(root: &Path, source: &str, prefer_svg: bool) -> Option<PathBuf> {
    let candidates = if prefer_svg && Path::new(source).extension().is_none() {
        vec![
            format!("{source}.svg"),
            format!("{source}@2x.png"),
            format!("{source}.png"),
        ]
    } else if Path::new(source).extension().is_none() {
        vec![
            format!("{source}@2x.png"),
            format!("{source}.png"),
            format!("{source}@2x.svg"),
            format!("{source}.svg"),
            format!("{source}@2x.gif"),
            format!("{source}.gif"),
        ]
    } else {
        vec![source.into()]
    };
    candidates.iter().find_map(|candidate| {
        safe_file(root, candidate)
            .ok()
            .filter(|path| path.is_file())
    })
}

pub(super) struct Thumbnails<'a> {
    root: &'a Path,
    cache: HashMap<String, Option<String>>,
    attempts: usize,
    source_bytes: usize,
    output_bytes: usize,
    prefer_svg: bool,
}
impl<'a> Thumbnails<'a> {
    // Construct the value and initialize its state
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            cache: Default::default(),
            attempts: 0,
            source_bytes: 0,
            output_bytes: 0,
            prefer_svg: false,
        }
    }
    // Translate the presentation into the OpenAction representation
    pub(super) fn openaction(&mut self) {
        self.prefer_svg = true;
    }
    // Resolve the image representation used by this operation
    pub(super) fn image(&mut self, source: Option<&str>) -> Option<String> {
        let source = source?;
        if !self.cache.contains_key(source) {
            if self.attempts >= 32 {
                return None;
            }
            self.attempts += 1;
            let image = self.decode(source);
            self.cache.insert(source.into(), image);
        }
        let image = self.cache.get(source)?.as_ref()?;
        if self.output_bytes + image.len() > 512 * 1024 {
            return None;
        }
        self.output_bytes += image.len();
        Some(image.clone())
    }
    // Decode for the current operation
    fn decode(&mut self, source: &str) -> Option<String> {
        let path = image_path(self.root, source, self.prefer_svg)?;
        let metadata = std::fs::metadata(&path).ok()?;
        let bytes = usize::try_from(metadata.len()).ok()?;
        if !metadata.is_file() || bytes > 256 * 1024 || self.source_bytes + bytes > 4 * 1024 * 1024
        {
            return None;
        }
        self.source_bytes += bytes;
        let mut source = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(256 * 1024 + 1)
            .read_to_end(&mut source)
            .ok()?;
        if source.len() > 256 * 1024 {
            return None;
        }
        // Existing renderer bounds raster allocations and disables SVG href resolution
        let png = hardware_core::render::render(&source, 72, 72, 0, "png").ok()?;
        if png.len() > 32 * 1024 {
            return None;
        }
        Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ))
    }
}

// Resolve the plugin associated with the saved binding
pub(super) fn bound_plugin(plugin: &mut PluginInfo, remaining: &mut usize) {
    if plugin.capabilities.is_some()
        || !matches!(plugin.dialect.as_str(), "streamdeck" | "openaction")
    {
        return;
    }
    let mut keep = |value: &mut Option<String>| {
        if let Some(image) = value {
            if image.len() > *remaining {
                *value = None;
            } else {
                *remaining -= image.len();
            }
        }
    };
    keep(&mut plugin.icon);
    for action in &mut plugin.actions {
        keep(&mut action.icon);
        keep(&mut action.encoder_icon);
        if let Some(images) = &mut action.multi_action_images {
            for image in images {
                keep(image);
            }
        }
    }
}

#[cfg(test)]
// Resolve the catalog entry associated with the binding
pub(super) fn bound_catalogue(plugins: &mut [PluginInfo]) {
    let mut remaining = 2 * 1024 * 1024;
    for plugin in plugins {
        bound_plugin(plugin, &mut remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify icon paths resolve vendor suffixes and preserve precedence
    fn icon_paths_resolve_vendor_suffixes_and_preserve_precedence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("actions")).unwrap();
        for (prefer_svg, suffixes) in [
            (true, vec![".svg", "@2x.png", ".png"]),
            (
                false,
                vec!["@2x.png", ".png", "@2x.svg", ".svg", "@2x.gif", ".gif"],
            ),
        ] {
            for suffix in &suffixes {
                std::fs::write(
                    root.path().join(format!("actions/icon{suffix}")),
                    b"fixture",
                )
                .unwrap();
            }
            for suffix in suffixes {
                let expected = root.path().join(format!("actions/icon{suffix}"));
                assert_eq!(
                    image_path(root.path(), "actions/icon", prefer_svg),
                    Some(expected.canonicalize().unwrap())
                );
                assert_eq!(
                    image_path(root.path(), &format!("actions/icon{suffix}"), prefer_svg),
                    Some(expected.canonicalize().unwrap())
                );
                std::fs::remove_file(expected).unwrap();
            }
            assert!(image_path(root.path(), "actions/icon", prefer_svg).is_none());
        }
    }

    #[test]
    // Verify icon paths keep extensions exact and reject directories and traversal
    fn icon_paths_keep_extensions_exact_and_reject_directories_and_traversal() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(package.join("actions/directory.svg")).unwrap();
        std::fs::write(package.join("actions/icon.svg"), b"fixture").unwrap();
        std::fs::write(root.path().join("outside.svg"), b"fixture").unwrap();
        for prefer_svg in [false, true] {
            for source in [
                "actions/icon.png",
                "actions/directory",
                "actions/directory.svg",
                "../outside",
                "../outside.svg",
            ] {
                assert!(
                    image_path(&package, source, prefer_svg).is_none(),
                    "{source}"
                );
            }
            assert!(image_path(
                &package,
                root.path().join("outside.svg").to_str().unwrap(),
                prefer_svg
            )
            .is_none());
        }
    }

    #[test]
    #[cfg(unix)]
    // Verify icon paths reject suffix candidates that escape through symlinks
    fn icon_paths_reject_suffix_candidates_that_escape_through_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(package.join("actions")).unwrap();
        std::fs::write(root.path().join("outside.svg"), b"fixture").unwrap();
        std::os::unix::fs::symlink(
            root.path().join("outside.svg"),
            package.join("actions/icon.svg"),
        )
        .unwrap();
        for prefer_svg in [false, true] {
            assert!(image_path(&package, "actions/icon", prefer_svg).is_none());
            assert!(image_path(&package, "actions/icon.svg", prefer_svg).is_none());
        }
    }

    #[test]
    // Verify vendor thumbnails are rasterized once and have source decode and output budgets
    fn vendor_thumbnails_are_rasterized_once_and_have_source_decode_and_output_budgets() {
        let root = tempfile::tempdir().unwrap();
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="#ff0000"/></svg>"##;
        std::fs::write(root.path().join("icon.svg"), svg).unwrap();
        std::fs::write(root.path().join("large.svg"), "x".repeat(256 * 1024 + 1)).unwrap();
        let mut cache = Thumbnails::new(root.path());
        let image = cache.image(Some("icon")).unwrap();
        assert!(image.starts_with("data:image/png;base64,"));
        assert!(image.len() < 32 * 1024);
        assert_eq!(cache.image(Some("icon")), Some(image.clone()));
        assert_eq!(cache.attempts, 1);
        assert!(cache.image(Some("large")).is_none());
        assert!(cache.image(Some("../outside")).is_none());
        cache.output_bytes = 512 * 1024;
        assert!(cache.image(Some("icon")).is_none());
        cache.output_bytes = 0;
        cache.attempts = 32;
        assert!(cache.image(Some("new")).is_none());
        cache.attempts = 0;
        cache.source_bytes = 4 * 1024 * 1024;
        std::fs::write(root.path().join("second.svg"), svg).unwrap();
        assert!(cache.image(Some("second")).is_none());
    }
}
