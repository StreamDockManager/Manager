//! Resolve user-selected applications without launching them or changing permissions
use std::path::Path;

// Resolve the supplied automation application request
pub fn resolve(path: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("Choose an application or executable".into());
    }
    let path = Path::new(path);
    if !path.exists() {
        return Err("The selected application no longer exists".into());
    }
    #[cfg(target_os = "macos")]
    {
        // Accept the .app itself or a binary selected from Contents/MacOS
        if let Some(bundle) = path.ancestors().find(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
                && p.is_dir()
        }) {
            return objc2::rc::autoreleasepool(|_| {
                use objc2_foundation::{NSBundle, NSString};
                NSBundle::bundleWithPath(&NSString::from_str(&bundle.to_string_lossy()))
                    .and_then(|bundle| bundle.bundleIdentifier())
                    .map(|id| id.to_string())
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| "The selected application has no bundle identifier".into())
            });
        }
    }
    if !path.is_file() {
        return Err("Choose an application or executable file".into());
    }
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve application: {e}"))?;
    let value = resolved.to_string_lossy().into_owned();
    // QueryFullProcessImageNameW returns a DOS path, not Rust's extended-length prefix
    #[cfg(target_os = "windows")]
    let value = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    #[test]
    // Verify that rejects missing paths and directories
    fn rejects_missing_paths_and_directories() {
        assert!(super::resolve("").is_err());
        assert!(super::resolve("/missing-sdm-application-3782").is_err());
        assert!(super::resolve(std::env::temp_dir().to_str().unwrap()).is_err());
    }

    #[test]
    // Verify that resolves an existing executable without launching it
    fn resolves_an_existing_executable_without_launching_it() {
        let executable = std::env::current_exe().unwrap();
        assert!(!super::resolve(executable.to_str().unwrap())
            .unwrap()
            .is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    // Verify that resolves bundle and nested executable to identifier
    fn resolves_bundle_and_nested_executable_to_identifier() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("Fixture.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.manager.automation</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        let executable = macos.join("fixture");
        std::fs::write(&executable, "not executable").unwrap();
        assert_eq!(
            super::resolve(bundle.to_str().unwrap()).unwrap(),
            "test.manager.automation"
        );
        assert_eq!(
            super::resolve(executable.to_str().unwrap()).unwrap(),
            "test.manager.automation"
        );
    }
}
