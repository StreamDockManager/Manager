//! All-or-nothing publication of reference files prepared from the editor snapshot.
pub(crate) fn write(
    directory: &std::path::Path,
    names: &[String],
    content: &[String],
) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    if names.is_empty()
        || names.len() > 128
        || names.len() != content.len()
        || content.iter().map(String::len).sum::<usize>() > 80 * 1024 * 1024
    {
        return Err("Reference exceeds the export limits.".into());
    }
    let dir = tempfile::Builder::new()
        .prefix("Manager reference ")
        .tempdir_in(directory)
        .map_err(|e| e.to_string())?;
    for (name, encoded) in names.iter().zip(content) {
        if !name
            .strip_prefix("page-")
            .and_then(|s| s.strip_suffix(".png"))
            .is_some_and(|n| !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("Invalid reference filename.".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "Invalid reference data.")?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("Invalid reference format.".into());
        }
        use std::io::Write;
        let mut file =
            std::fs::File::create_new(dir.path().join(name)).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(dir.keep().to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    #[test]
    fn writes_selected_pngs_to_a_new_directory() {
        let root = tempfile::tempdir().unwrap();
        let png = include_bytes!("../icons/32x32.png");
        let files = ["page-01.png".into(), "page-03.png".into()];
        let folder = write(
            root.path(),
            &files,
            &[STANDARD.encode(png), STANDARD.encode(png)],
        )
        .unwrap();
        for name in files {
            assert_eq!(
                std::fs::read(std::path::Path::new(&folder).join(name)).unwrap(),
                png
            );
        }
    }

    #[test]
    fn partial_and_duplicate_outputs_are_removed_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let image = STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        for (name, encoded) in [
            ("../escape.png", image.clone()),
            ("page-01.png", image.clone()),
            ("page-02.png", "invalid".into()),
            ("page-02.png", STANDARD.encode(b"%PDF-1.7")),
        ] {
            assert!(write(
                root.path(),
                &["page-01.png".into(), name.into()],
                &[image.clone(), encoded]
            )
            .is_err());
            assert_eq!(root.path().read_dir().unwrap().count(), 0);
        }
        assert!(write(
            root.path(),
            &["reference.pdf".into()],
            &[STANDARD.encode(b"%PDF-1.7")]
        )
        .is_err());
        assert_eq!(root.path().read_dir().unwrap().count(), 0);
        assert!(write(
            &root.path().join("missing"),
            &["page-01.png".into()],
            &[image]
        )
        .is_err());
    }
}
