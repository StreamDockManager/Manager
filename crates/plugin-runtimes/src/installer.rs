use crate::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek},
    path::{Component, Path},
};
// Validate the runtime installation path
fn safe(name: &Path) -> Result<(), String> {
    if name.is_absolute()
        || name
            .components()
            .any(|p| !matches!(p, Component::Normal(_) | Component::CurDir))
        || name.to_string_lossy().contains(['\\', ':'])
    {
        return Err("Unsafe runtime archive path".into());
    }
    Ok(())
}
#[cfg(test)]
// Extract for the current operation
pub(crate) fn extract(
    bytes: &[u8],
    release: &RuntimeRelease,
    dest: &Path,
    progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
) -> Result<(), String> {
    extract_reader(std::io::Cursor::new(bytes), release, dest, progress)
}
// Extract reader
fn extract_reader<R: Read + Seek>(
    reader: R,
    release: &RuntimeRelease,
    dest: &Path,
    progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
) -> Result<(), String> {
    let mut found = false;
    let mut size = 0u64;
    let mut save =
        |path: &Path, link: bool, len: u64, reader: &mut dyn Read| -> Result<(), String> {
            progress(0, None)?;
            safe(path)?;
            if link {
                return Err("Links in runtime archives are not allowed".into());
            }
            size = size.checked_add(len).ok_or("Archive size overflow")?;
            if size > 512 * 1024 * 1024 {
                return Err("Runtime extraction exceeds 512 MiB".into());
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let executable = name == release.executable;
            let notice = matches!(
                name,
                "LICENSE" | "LICENSE.txt" | "COPYING" | "NOTICE" | "NOTICE.txt"
            );
            if executable || notice {
                if executable && found {
                    return Err("Duplicate runtime executable".into());
                }
                let target = if executable {
                    dest.join(&release.executable)
                } else {
                    dest.join(format!("notice-{}-{name}", size))
                };
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target)
                    .map_err(|e| e.to_string())?;
                let copied = std::io::copy(&mut reader.take(len + 1), &mut file)
                    .map_err(|e| e.to_string())?;
                if copied != len {
                    return Err("Archive entry size mismatch".into());
                }
                file.sync_all().map_err(|e| e.to_string())?;
                #[cfg(unix)]
                if executable {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o755))
                        .map_err(|e| e.to_string())?;
                }
                found |= executable;
            }
            Ok(())
        };
    if release.archive_name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
        crate::archives::validate_lzma_headers(&mut zip)?;
        if zip.len() > 10000 {
            return Err("Too many archive entries".into());
        }
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            let path = Path::new(entry.name().map_err(|e| e.to_string())?.as_ref()).to_path_buf();
            if entry.is_dir() {
                safe(&path)?;
                continue;
            }
            let link = entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000);
            save(&path, link, entry.size(), &mut entry)?;
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
        for (i, entry) in archive.entries().map_err(|e| e.to_string())?.enumerate() {
            if i > 10000 {
                return Err("Too many archive entries".into());
            }
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            let kind = entry.header().entry_type();
            // Node archives include npm symlinks. Validate their paths but never extract them
            safe(&path)?;
            if kind.is_symlink() || kind.is_hard_link() {
                let target = entry
                    .link_name()
                    .map_err(|e| e.to_string())?
                    .ok_or("Missing link target")?;
                if target.is_absolute() || target.to_string_lossy().contains(['\\', ':']) {
                    return Err("Unsafe archive link".into());
                }
                let mut depth = path.parent().map_or(0, |p| p.components().count());
                for component in target.components() {
                    match component {
                        Component::Normal(_) => depth += 1,
                        Component::CurDir => {}
                        Component::ParentDir if depth > 0 => depth -= 1,
                        _ => return Err("Escaping archive link".into()),
                    }
                }

                if path.file_name().and_then(|n| n.to_str()) == Some(&release.executable) {
                    return Err("Runtime executable is a link".into());
                }
                continue;
            }
            if !kind.is_file() && !kind.is_dir() {
                return Err("Unsupported runtime archive entry".into());
            }
            if kind.is_dir() {
                continue;
            }
            save(&path, false, entry.size(), &mut entry)?;
        }
    }
    if !found {
        return Err("Runtime executable missing from archive".into());
    }
    Ok(())
}
// Download identity
pub(crate) fn download_identity(release: &RuntimeRelease) -> String {
    format!(
        "runtime:{}:{}:{}:{}",
        release.kind.name(),
        release.channel,
        release.version,
        release.id
    )
}
// Install for the current operation
pub(crate) fn install(
    service: &RuntimeService,
    release: &RuntimeRelease,
    control: &dyn Fn() -> stream_dock_manager_downloads::Control,
    progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
) -> Result<RuntimeInstallation, String> {
    let version = semver::Version::parse(&release.version).map_err(|e| e.to_string())?;
    if !eligible(
        &service.status().lines,
        release.kind,
        &release.channel,
        &version,
        service.now(),
    ) {
        return Err("Runtime support expired; check for updates".into());
    }
    if !crate::providers::allowed(
        release.kind,
        &release.url.parse().map_err(|_| "Invalid runtime URL")?,
        true,
    ) {
        return Err("Runtime origin is not permitted".into());
    }
    let spec = stream_dock_manager_downloads::ArtifactSpec {
        identity: download_identity(release),
        url: release.url.clone(),
        sha256: String::new(),
        size: None,
        limit: 256 * 1024 * 1024,
    };
    let artifact =
        service
            .transport
            .download(release.kind, &spec, &service.downloads, control, progress)?;
    let root = service.root.join(release.kind.name());
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&root)
        .map_err(|e| e.to_string())?;
    let archive = std::fs::File::open(artifact.path()).map_err(|e| e.to_string())?;
    extract_reader(archive, release, staging.path(), progress)?;
    let executable = staging.path().join(&release.executable);
    let probed = probe_with_output(release.kind, &executable)?;
    let mut installed = probed.installation;
    if installed.version != release.version || installed.channel != release.channel {
        return Err(format!(
            "Downloaded executable {} ({}) does not match selected release {} ({}); version output: {:?}",
            installed.version, installed.channel, release.version, release.channel, probed.version_output
        ));
    }
    progress(artifact.size() as u32, Some(artifact.size() as u32))?;
    let target = root.join(&release.id);
    installed.id = release.id.clone();
    installed.source = "private".into();
    installed.verification = "official-https".into();
    installed.provenance = format!("{}; artifact {}", release.provenance, release.url);
    installed.executable = target.join(&release.executable).to_string_lossy().into();
    std::fs::write(
        staging.path().join("installation.json"),
        serde_json::to_vec_pretty(&installed).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if target.exists() {
        return Err("Runtime installation already exists".into());
    }
    std::fs::rename(staging.path(), &target).map_err(|e| e.to_string())?;
    Ok(installed)
}
// Inspect the runtime executable before accepting it
pub fn probe(kind: RuntimeKind, path: &Path) -> Result<RuntimeInstallation, String> {
    probe_with_output(kind, path).map(|probe| probe.installation)
}
struct Probe {
    installation: RuntimeInstallation,
    version_output: String,
}
// Inspect a runtime using its reported output
fn probe_with_output(kind: RuntimeKind, path: &Path) -> Result<Probe, String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("Select an absolute runtime executable path".into());
    }
    let mut command = background_command(path);
    command.args([
        "--eval",
        "console.log(process.version);console.log(process.release.lts || '')",
    ]);
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .env_remove("NODE_OPTIONS")
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = vec![];
        let _ = stdout.by_ref().take(16384).read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err("Runtime version probe failed".into());
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Runtime version probe timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let bytes = reader.join().map_err(|_| "Runtime version reader failed")?;
    let text = String::from_utf8_lossy(&bytes);
    let first = text.lines().next().unwrap_or("");
    if text.lines().nth(1).is_none_or(|s| s.trim().is_empty()) {
        return Err("Select an actual Node.js LTS build; Current releases are unsupported".into());
    }
    let value = first
        .strip_prefix('v')
        .ok_or("Selected executable is not Node.js")?;
    let version =
        semver::Version::parse(value).map_err(|_| "Runtime version could not be identified")?;
    if !version.pre.is_empty() {
        return Err("Prerelease runtime builds are unsupported".into());
    }
    let channel = "lts";
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let identity = format!(
        "{}:{version}:{channel}:{:?}:{}",
        path.display(),
        metadata.modified().ok(),
        metadata.len()
    );
    Ok(Probe {
        installation: RuntimeInstallation {
            id: format!(
                "external-{}-{}",
                kind.name(),
                hex::encode(Sha256::digest(identity.as_bytes()))
            ),
            kind,
            channel: channel.into(),
            line: kind.line(&version),
            version: version.to_string(),
            executable: path.to_string_lossy().into(),
            source: "external".into(),
            verification: "user-selected executable".into(),
            provenance: path.display().to_string(),
        },
        version_output: first.chars().take(1024).collect(),
    })
}
