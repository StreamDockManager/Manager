use crate::*;
use serde_json::Value;
use std::collections::BTreeSet;
// Identify the current runtime download platform
pub(crate) fn platform(_kind: RuntimeKind) -> Result<(String, String), String> {
    let cpu = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return Err("Unsupported process architecture".into()),
    };
    let os = std::env::consts::OS;
    if !matches!(
        (os, cpu),
        ("windows", "x64") | ("linux", "x64") | ("macos", "x64") | ("macos", "arm64")
    ) {
        return Err(
            "Runtime platform is not qualified; use a compatible external executable".into(),
        );
    }
    if cfg!(target_env = "musl") {
        return Err("Managed runtimes require a glibc build".into());
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            // Declare the native gnu_get_libc_version operation used by this adapter
            fn gnu_get_libc_version() -> *const std::os::raw::c_char;
        }
        let value = unsafe { std::ffi::CStr::from_ptr(gnu_get_libc_version()) }
            .to_str()
            .map_err(|_| "Could not identify glibc version")?;
        let parts: Vec<u32> = value
            .split('.')
            .take(2)
            .filter_map(|v| v.parse().ok())
            .collect();
        let minimum = (2, 28);
        if parts.len() != 2 || (parts[0], parts[1]) < minimum {
            return Err(format!(
                "{} requires glibc {}.{} or newer",
                _kind.name(),
                minimum.0,
                minimum.1
            ));
        }
    }
    Ok((
        format!(
            "{}-{cpu}",
            match os {
                "macos" => "darwin",
                "windows" => "win",
                _ => "linux",
            }
        ),
        if os == "windows" { "zip" } else { "tar.gz" }.into(),
    ))
}

// Fetch bounded runtime metadata and decode it as UTF-8
fn text(t: &dyn Transport, kind: RuntimeKind, url: &str) -> Result<String, String> {
    String::from_utf8(t.get(kind, url, false, 8 * 1024 * 1024, &|_, _| Ok(()))?)
        .map_err(|e| e.to_string())
}
// Fetch and parse runtime metadata as JSON
fn json(t: &dyn Transport, kind: RuntimeKind, url: &str) -> Result<Value, String> {
    serde_json::from_str(&text(t, kind, url)?).map_err(|e| e.to_string())
}
#[derive(Debug)]
pub(crate) struct NodeDiscovery {
    pub lines: Vec<RuntimeLine>,
    pub releases: Vec<RuntimeRelease>,
}
// Discover for the current operation
pub(crate) fn discover(t: &dyn Transport, now: i64) -> Result<NodeDiscovery, String> {
    discover_for_platform(t, now, platform(RuntimeKind::Nodejs)?)
}
// Keep metadata selection independently testable on hosts whose managed
// downloads are intentionally unqualified. Production discovery always passes
// through the platform admission above.
pub(crate) fn discover_for_platform(
    t: &dyn Transport,
    now: i64,
    (platform, extension): (String, String),
) -> Result<NodeDiscovery, String> {
    let kind = RuntimeKind::Nodejs;
    let mut result = vec![];
    let schedule = json(
        t,
        kind,
        "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json",
    )?;
    let lines = node_lines(&schedule);
    if lines.is_empty() {
        return Err("Invalid upstream Node support schedule".into());
    }
    let index = json(t, kind, "https://nodejs.org/dist/index.json")?;
    let mut versions = BTreeSet::new();
    for row in index.as_array().ok_or("Invalid Node release index")? {
        if !row["lts"].is_string() {
            continue;
        }
        let Ok(v) = semver::Version::parse(
            row["version"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches('v'),
        ) else {
            continue;
        };
        if !eligible(&lines, kind, "lts", &v, now) {
            continue;
        }
        versions.insert(v);
    }
    for v in versions.into_iter().rev() {
        let name = format!("node-v{v}-{platform}.{extension}");
        result.push(release(
            kind,
            &v,
            &platform,
            &name,
            format!("https://nodejs.org/dist/v{v}/{name}"),
            "https://nodejs.org/dist/index.json",
        ));
    }
    Ok(NodeDiscovery {
        lines,
        releases: result,
    })
}
// Release for the current operation
fn release(
    kind: RuntimeKind,
    v: &semver::Version,
    platform: &str,
    name: &str,
    url: String,
    provenance: &str,
) -> RuntimeRelease {
    RuntimeRelease {
        id: format!("{}-{}-{v}-{platform}", kind.name(), kind.channel()),
        kind,
        channel: kind.channel().into(),
        line: kind.line(v),
        version: v.to_string(),
        platform: platform.into(),
        url,
        archive_name: name.into(),
        executable: kind.executable().into(),
        verification: "official-https".into(),
        provenance: provenance.into(),
    }
}
// Check whether this runtime URL is permitted
pub(crate) fn allowed(kind: RuntimeKind, url: &reqwest::Url, artifact: bool) -> bool {
    if url.scheme() != "https"
        || url.port().is_some_and(|p| p != 443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    let path = url.path();
    match (kind, artifact) {
        (RuntimeKind::Nodejs, true) => host == "nodejs.org" && path.starts_with("/dist/"),
        (RuntimeKind::Nodejs, false) => {
            (host == "nodejs.org" && path.starts_with("/dist/"))
                || (host == "raw.githubusercontent.com"
                    && path == "/nodejs/Release/main/schedule.json")
        }
    }
}

// Read the Node.js release-line metadata
pub(crate) fn node_lines(schedule: &Value) -> Vec<RuntimeLine> {
    schedule
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(major, row)| {
            Some(RuntimeLine {
                kind: RuntimeKind::Nodejs,
                channel: "lts".into(),
                line: major.strip_prefix('v')?.parse::<u64>().ok()?.to_string(),
                start: row["lts"].as_str()?.into(),
                end: Some(row["end"].as_str()?.into()),
                policy: "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json"
                    .into(),
            })
        })
        .collect()
}

// Check whether this runtime version is supported
pub fn supported(line: &RuntimeLine, now: i64) -> bool {
    let date = chrono::DateTime::from_timestamp(now, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string();
    date >= line.start && line.end.as_ref().is_none_or(|end| date < *end)
}

// Check whether the candidate is eligible for this operation
pub fn eligible(
    lines: &[RuntimeLine],
    kind: RuntimeKind,
    channel: &str,
    version: &semver::Version,
    now: i64,
) -> bool {
    version.pre.is_empty()
        && lines.iter().any(|l| {
            l.kind == kind
                && l.channel == channel
                && l.line == kind.line(version)
                && supported(l, now)
        })
}
