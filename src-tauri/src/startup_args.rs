// Read whether startup requested configuration recovery
pub(crate) fn recovery_mode(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    args.into_iter()
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--recovery-mode")
}

use std::{ffi::OsString, path::PathBuf};

/// Leave existing desktop/OS activation arguments to their current handlers
/// Only an explicit data-directory override changes the shared default root
pub(crate) fn configuration_root(
    args: impl IntoIterator<Item = OsString>,
) -> Result<PathBuf, String> {
    configuration_override(args)?
        .map_or_else(stream_dock_manager_runtime::Runtime::default_root, Ok)
}

// Resolve the configuration path supplied at startup
fn configuration_override(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter();
    let mut root = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--config" {
            let value = args.next().ok_or("--config requires a directory path")?;
            if value.to_string_lossy().starts_with('-') {
                return Err("--config requires a directory path; use --config=<path> if the path starts with '-'".into());
            }
            value
        } else if arg.as_encoded_bytes().starts_with(b"--config=") {
            let value = arg
                .to_str()
                .ok_or("Use --config <path> for a non-Unicode directory path")?;
            OsString::from(&value["--config=".len()..])
        } else {
            continue;
        };
        if value.is_empty() {
            return Err("--config requires a non-empty directory path".into());
        }
        if root.replace(PathBuf::from(value)).is_some() {
            return Err("Pass --config only once".into());
        }
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parse for the current operation
    fn parse(args: &[&str]) -> Result<Option<PathBuf>, String> {
        configuration_override(args.iter().map(OsString::from))
    }

    #[test]
    // Verify desktop override preserves paths and activation arguments
    fn desktop_override_preserves_paths_and_activation_arguments() {
        assert_eq!(parse(&["--autostart", "--show"]).unwrap(), None);
        assert_eq!(
            parse(&[
                "--background",
                "--config",
                "directory with spaces",
                "--show"
            ])
            .unwrap(),
            Some(PathBuf::from("directory with spaces"))
        );
        assert_eq!(
            parse(&["--config=directory with spaces"]).unwrap(),
            Some(PathBuf::from("directory with spaces"))
        );
        assert_eq!(parse(&["--", "--config", "ignored"]).unwrap(), None);
        assert_eq!(
            configuration_root([OsString::from("--config=chosen-root")]).unwrap(),
            PathBuf::from("chosen-root")
        );
    }

    #[test]
    // Verify recovery flag obeys end of options
    fn recovery_flag_obeys_end_of_options() {
        assert!(recovery_mode([OsString::from("--recovery-mode")]));
        assert!(!recovery_mode([
            OsString::from("--"),
            OsString::from("--recovery-mode")
        ]));
    }

    #[test]
    // Verify malformed override cannot silently open the default root
    fn malformed_override_cannot_silently_open_the_default_root() {
        for args in [
            vec!["--config"],
            vec!["--config", ""],
            vec!["--config="],
            vec!["--config", "--background"],
            vec!["--config=one", "--config", "two"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    // Verify separate path argument preserves non unicode bytes
    fn separate_path_argument_preserves_non_unicode_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"/tmp/config-\xff".to_vec());
        assert_eq!(
            configuration_override([OsString::from("--config"), path.clone()]).unwrap(),
            Some(PathBuf::from(path))
        );
        assert!(
            configuration_override([OsString::from_vec(b"--config=/tmp/\xff".to_vec())]).is_err()
        );
    }
}
