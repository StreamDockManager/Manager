use super::*;
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
};
struct TestClock(AtomicI64);
impl Clock for TestClock {
    // Read the current time for this subsystem
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Offline;
impl Transport for Offline {
    // Download for the current operation
    fn download(
        &self,
        _: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        let _ = (spec, cache, control, progress);
        Err("Metadata-only fixture does not download artifacts".into())
    }

    // Retrieve the requested value
    fn get(
        &self,
        _: RuntimeKind,
        _: &str,
        _: bool,
        _: usize,
        _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        Err("offline fixture".into())
    }
}
// Construct the clock test fixture
fn clock(date: &str) -> Arc<TestClock> {
    Arc::new(TestClock(AtomicI64::new(
        chrono::DateTime::parse_from_rfc3339(&format!("{date}T00:00:00Z"))
            .unwrap()
            .timestamp(),
    )))
}
#[cfg(unix)]
// Construct the req test fixture
fn req(kind: RuntimeKind, version: Option<&str>) -> RuntimeRequirement {
    RuntimeRequirement {
        kind,
        channel: kind.channel().into(),
        version: version.map(str::to_string),
    }
}
#[test]
// Verify declarations require current explicit runtime
fn declarations_require_current_explicit_runtime() {
    for version in ["20", "24", "26"] {
        let req = manifest_requirement(
            &json!({"Nodejs":{"Version":version,"Debug":"enabled","GenerateProfilerOutput":false}}),
            Path::new("main.mjs"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(req.kind, RuntimeKind::Nodejs);
        assert_eq!(req.version.as_deref(), Some(version));
        assert_eq!(req.channel, "lts");
    }
    for node in [
        json!({"Version":"22"}),
        json!({"Version":"^24"}),
        json!({"Version":24}),
        json!({"Version":"24","Unknown":true}),
        json!({"Version":"24","GenerateProfilerOutput":true}),
    ] {
        assert!(manifest_requirement(&json!({"Nodejs":node}), Path::new("main.mjs")).is_err());
    }
    assert!(
        manifest_requirement(&json!({"Nodejs":{"Version":"24"}}), Path::new("main.ts")).is_err()
    );
    for extension in ["js", "mjs", "cjs"] {
        assert!(
            manifest_requirement(&json!({}), Path::new(&format!("main.{extension}")))
                .unwrap_err()
                .contains("explicit")
        );
    }
    assert!(manifest_requirement(&json!({}), Path::new("main.ts")).is_err());
    assert!(manifest_requirement(&json!({}), Path::new("main.tsx")).is_err());
    assert!(manifest_requirement(&json!({}), Path::new("main.html"))
        .unwrap()
        .is_none());
    let node =
        json!({"StreamDockManager":{"Runtime":{"Kind":"nodejs","Version":"^24","Channel":"lts"}}});
    assert_eq!(
        manifest_requirement(&node, Path::new("main.mjs"))
            .unwrap()
            .unwrap()
            .kind,
        RuntimeKind::Nodejs
    );
    for version in ["24", "24.22.1", "^24", ">=22, <27", ">=22 <27"] {
        let declared = json!({"StreamDockManager":{"Runtime":{"Kind":"nodejs","Version":version,"Channel":"lts"}}});
        assert_eq!(
            manifest_requirement(&declared, Path::new("main.js"))
                .unwrap()
                .unwrap()
                .version
                .as_deref(),
            Some(version)
        );
        for extension in ["ts", "mts", "cts", "tsx", "jsx"] {
            assert!(
                manifest_requirement(&declared, Path::new(&format!("main.{extension}"))).is_err()
            );
        }
    }
    assert!(manifest_requirement(
        &json!({"stream-dock-manager":{"Runtime":{"Kind":"nodejs","Version":"24"}}}),
        Path::new("main.js")
    )
    .is_err());
    let mut conflicting = node.clone();
    conflicting["Nodejs"] = json!({"Version":"^24"});
    assert!(manifest_requirement(&conflicting, Path::new("main.js")).is_err());
    for kind in ["deno", "bun"] {
        let manifest = json!({"StreamDockManager":{"Runtime":{"Kind":kind,"Version":"*"}}});
        assert!(manifest_requirement(&manifest, Path::new("main.mjs")).is_err());
        assert!(serde_json::from_value::<RuntimeKind>(json!(kind)).is_err());
    }
    let conflict = json!({"Nodejs":{"Version":"22"},"StreamDockManager":{"Runtime":{"Kind":"nodejs","Version":"24"}}});
    assert!(manifest_requirement(&conflict, Path::new("main.mjs")).is_err());
    assert!(manifest_requirement(&json!({"StreamDockManager":{"Runtime":{"Kind":"nodejs","Version":"24","Permissions":{"Net":[]}}}}),Path::new("main.mjs")).is_err());
}
#[test]
// Verify support dates do not promote current releases
fn support_dates_do_not_promote_current_releases() {
    let lines = fixture_lines();
    let now = clock("2026-10-23").now();
    assert!(!eligible(
        &lines,
        RuntimeKind::Nodejs,
        "lts",
        &"20.19.0".parse().unwrap(),
        now
    ));
    assert!(eligible(
        &lines,
        RuntimeKind::Nodejs,
        "lts",
        &"22.0.0".parse().unwrap(),
        now
    ));
    assert!(!eligible(
        &lines,
        RuntimeKind::Nodejs,
        "lts",
        &"26.0.0".parse().unwrap(),
        now
    ));
    assert!(!eligible(
        &lines,
        RuntimeKind::Nodejs,
        "lts",
        &"22.1.0-rc.1".parse().unwrap(),
        now
    ));
}
#[test]
// Verify offline and daily backoff use injected clock
fn offline_and_daily_backoff_use_injected_clock() {
    let root = tempfile::tempdir().unwrap();
    let c = clock("2026-10-23");
    let s = RuntimeService::with_dependencies(root.path().into(), c.clone(), Arc::new(Offline))
        .unwrap();
    assert!(s.due());
    s.check().unwrap();
    assert!(!s.due());
    assert!(s.status().stale);
    assert!(s.recommended(RuntimeKind::Nodejs).is_none());
    c.0.fetch_add(3601, Ordering::SeqCst);
    assert!(s.due());
    s.automatic(false).unwrap();
    assert!(!s.due());
    drop(s);
    let again =
        RuntimeService::with_dependencies(root.path().into(), c, Arc::new(Offline)).unwrap();
    assert!(!again.status().automatic);
}
#[test]
// Verify rejects unsafe archives
fn rejects_unsafe_archives() {
    let bytes = archive("../node", b"unsafe");
    let root = tempfile::tempdir().unwrap();
    assert!(crate::installer::extract(&bytes, &release(), root.path(), &|_, _| Ok(())).is_err());
}
// Construct the archive test fixture
fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    zip.start_file(
        name,
        zip::write::SimpleFileOptions::default().unix_permissions(0o755),
    )
    .unwrap();
    zip.write_all(bytes).unwrap();
    zip.finish().unwrap().into_inner()
}
// Release for the current operation
fn release() -> RuntimeRelease {
    RuntimeRelease {
        id: "nodejs-lts-24.22.1-fixture".into(),
        kind: RuntimeKind::Nodejs,
        channel: "lts".into(),
        line: "24".into(),
        version: "24.22.1".into(),
        platform: "fixture".into(),
        url: "https://nodejs.org/dist/v24.22.1/node.zip".into(),
        archive_name: "node.zip".into(),
        executable: RuntimeKind::Nodejs.executable().into(),
        verification: "fixture".into(),
        provenance: "fixture".into(),
    }
}
#[test]
// Verify restricts metadata and redirect origins
fn restricts_metadata_and_redirect_origins() {
    use crate::providers::allowed;
    for (kind, url) in [
        (RuntimeKind::Nodejs, "https://evil.test/dist/x"),
        (
            RuntimeKind::Nodejs,
            "https://github.com/attacker/bun/releases/download/a",
        ),
        (
            RuntimeKind::Nodejs,
            "https://dl.deno.land/release/v2.9.3/deno.zip",
        ),
        (RuntimeKind::Nodejs, "http://nodejs.org/dist/x"),
    ] {
        assert!(!allowed(kind, &url.parse().unwrap(), true));
    }
}
#[test]
// Verify defaults persist and unsupported selection fails
fn defaults_persist_and_unsupported_selection_fails() {
    let root = tempfile::tempdir().unwrap();
    let c = clock("2026-10-23");
    let s = seeded(
        root.path(),
        fixture_releases().pop().unwrap(),
        Arc::new(Offline),
        c.clone(),
    );
    let mut choice = s.recommended(RuntimeKind::Nodejs).unwrap();
    choice.channel = "stable".into();
    assert!(s.select(None, choice.clone(), None).is_err());
    choice.channel = "lts".into();
    choice.line = "2".into();
    assert!(s.select(None, choice, None).is_err());
    let choice = s.recommended(RuntimeKind::Nodejs).unwrap();
    s.select(None, choice, None).unwrap();
    drop(s);
    let again =
        RuntimeService::with_dependencies(root.path().into(), c, Arc::new(Offline)).unwrap();
    assert_eq!(again.recommended(RuntimeKind::Nodejs).unwrap().line, "24");
}
struct FixtureTransport {
    bytes: Vec<u8>,
    hold: AtomicBool,
}
impl Transport for FixtureTransport {
    // Download for the current operation
    fn download(
        &self,
        _: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        while self.hold.load(Ordering::SeqCst) {
            if control() != stream_dock_manager_downloads::Control::Continue {
                return Err("cancelled".into());
            }
            progress(0, None)?;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        struct Chunks<'a>(&'a [u8]);
        impl std::io::Read for Chunks<'_> {
            // Read for the current operation
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let size = out.len().min(self.0.len()).min(31);
                out[..size].copy_from_slice(&self.0[..size]);
                self.0 = &self.0[size..];
                Ok(size)
            }
        }
        cache.store_reader_from_origin(spec, &mut Chunks(&self.bytes), control, &|done, total| {
            progress(done as u32, total.map(|n| n as u32))
        })
    }

    // Retrieve the requested value
    fn get(
        &self,
        _: RuntimeKind,
        _: &str,
        _: bool,
        _: usize,
        _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        Err("Artifact fixture requires the streaming download method".into())
    }
}
// Construct the seeded test fixture
fn seeded(
    root: &Path,
    release: RuntimeRelease,
    t: Arc<dyn Transport>,
    c: Arc<dyn Clock>,
) -> Arc<RuntimeService> {
    let storage = stream_dock_manager_storage::Storage::open(root.into()).unwrap();
    storage.write_json(stream_dock_manager_storage::Domain::RuntimeState, "preferences", &json!({"lines":fixture_lines(),"releases":[release],"installations":[],"defaults":{},"plugins":{},"automatic":true,"last_check":c.now(),"next_check":c.now()+86400,"error":null}), 16*1024*1024).unwrap();
    drop(storage);
    RuntimeService::with_dependencies(root.into(), c, t).unwrap()
}
// Finish for the current operation
fn finish(s: &RuntimeService, id: &str) -> RuntimeJob {
    for _ in 0..300 {
        let j = s.status().jobs.into_iter().find(|j| j.id == id).unwrap();
        if j.state != "installing" {
            return j;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("job did not finish")
}
#[test]
// Verify duplicate jobs cancel and retry without selection changes
fn duplicate_jobs_cancel_and_retry_without_selection_changes() {
    let root = tempfile::tempdir().unwrap();
    let t = Arc::new(FixtureTransport {
        bytes: vec![],
        hold: AtomicBool::new(true),
    });
    let release = release();
    let id = release.id.clone();
    let s = seeded(root.path(), release, t.clone(), clock("2026-10-23"));
    let job = s.start_install(&id).unwrap();
    assert_eq!(job, s.start_install(&id).unwrap());
    s.cancel(&job).unwrap();
    assert_eq!(finish(&s, &job).state, "cancelled");
    assert!(s.status().installations.is_empty());
    assert!(s.status().defaults.is_empty());
    t.hold.store(false, Ordering::SeqCst);
    let retry = s.start_install(&id).unwrap();
    assert_eq!(finish(&s, &retry).state, "failed");
    assert!(s.status().installations.is_empty());
}
#[cfg(unix)]
#[test]
// Verify private install without publisher digest is atomic and offline
fn private_install_without_publisher_digest_is_atomic_and_offline() {
    let root = tempfile::tempdir().unwrap();
    let bytes = archive("node", b"#!/bin/sh\necho v24.22.1\necho Krypton\n");
    let t = Arc::new(FixtureTransport {
        bytes: bytes.clone(),
        hold: AtomicBool::new(false),
    });
    let c = clock("2026-10-23");
    let release = release();
    let id = release.id.clone();
    let s = seeded(root.path(), release, t, c.clone());
    let job = s.start_install(&id).unwrap();
    let completed = finish(&s, &job);
    assert_eq!(completed.state, "complete", "{completed:?}");
    assert_eq!(
        s.resolve("example", &req(RuntimeKind::Nodejs, Some("24")))
            .state,
        "ready"
    );
    drop(s);
    let again =
        RuntimeService::with_dependencies(root.path().into(), c, Arc::new(Offline)).unwrap();
    assert_eq!(
        again
            .resolve("example", &req(RuntimeKind::Nodejs, Some("24")))
            .state,
        "ready"
    );
    assert!(again.start_install(&id).is_err());
}
#[test]
#[ignore = "Explicit upstream download qualification; set STREAM_DOCK_MANAGER_QUALIFY_RUNTIMES=1"]
// Verify official download qualification
fn official_download_qualification() {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_QUALIFY_RUNTIMES").as_deref(),
        Ok("1")
    );
    let root = tempfile::tempdir().unwrap();
    let s = RuntimeService::open(root.path().into()).unwrap();
    s.check().unwrap();
    let kind = match std::env::var("STREAM_DOCK_MANAGER_QUALIFY_KIND").as_deref() {
        Ok("nodejs") | Err(_) => RuntimeKind::Nodejs,
        Ok(kind) => panic!("Unsupported runtime kind: {kind}"),
    };
    let release = s
        .status()
        .releases
        .into_iter()
        .filter(|r| r.kind == kind)
        .max_by_key(|r| semver::Version::parse(&r.version).unwrap())
        .expect("official metadata available");
    let job = s.start_install(&release.id).unwrap();
    for _ in 0..600 {
        let j = s.status().jobs.into_iter().find(|j| j.id == job).unwrap();
        if j.state != "installing" {
            assert_eq!(j.state, "complete", "{:?}", j.error);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("download timed out")
}

struct NodeMetadata;
impl Transport for NodeMetadata {
    // Download for the current operation
    fn download(
        &self,
        _: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        let _ = (spec, cache, control, progress);
        Err("Metadata-only fixture does not download artifacts".into())
    }

    // Retrieve the requested value
    fn get(
        &self,
        _: RuntimeKind,
        url: &str,
        _: bool,
        _: usize,
        _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        if url.ends_with("schedule.json") {
            return Ok(serde_json::to_vec(&node_schedule()).unwrap());
        }
        if url.ends_with("index.json") {
            return Ok(serde_json::to_vec(&json!([
                {"version":"v26.12.1","lts":false},
                {"version":"v24.20.0","lts":"Krypton"},
                {"version":"v24.22.1","lts":"Krypton"},
                {"version":"v22.23.4","lts":"Jod"},
                {"version":"v20.19.0","lts":"Iron"}
            ]))
            .unwrap());
        }
        Err("offline fixture".into())
    }
}
#[test]
// Verify discovery lists every actual release in unexpired lts lines
fn discovery_lists_every_actual_release_in_unexpired_lts_lines() {
    let now = clock("2026-11-01").now();
    let releases = crate::providers::discover_for_platform(
        &NodeMetadata,
        now,
        ("linux-x64".into(), "tar.gz".into()),
    )
    .unwrap()
    .releases;
    assert_eq!(releases.len(), 3);
    assert!(releases.iter().any(|r| r.version == "24.20.0"));
    assert!(releases.iter().any(|r| r.version == "24.22.1"));
    assert!(!releases.iter().any(|r| r.line == "26" || r.line == "20"));
    let native = crate::providers::discover(&NodeMetadata, now);
    match crate::providers::platform(RuntimeKind::Nodejs) {
        Ok(_) => assert_eq!(native.unwrap().releases.len(), releases.len()),
        Err(expected) => assert_eq!(native.unwrap_err(), expected),
    }
}
#[cfg(unix)]
#[test]
// Verify cached node eol and manifest override acknowledgement are enforced
fn cached_node_eol_and_manifest_override_acknowledgement_are_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let c = clock("2026-10-23");
    let service = seeded(
        root.path(),
        fixture_releases().pop().unwrap(),
        Arc::new(Offline),
        c.clone(),
    );
    let path = root.path().join("node");
    std::fs::write(&path, "#!/bin/sh\necho v24.22.1\necho Krypton\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut choice = service.recommended(RuntimeKind::Nodejs).unwrap();
    choice.external = Some(path.to_string_lossy().into());
    let r = req(RuntimeKind::Nodejs, Some("20"));
    assert!(service
        .select(Some("old-node"), choice.clone(), Some(&r))
        .is_err());
    choice.acknowledge_override = true;
    service.select(Some("old-node"), choice, Some(&r)).unwrap();
    assert!(service.resolve("old-node", &r).overridden);
    c.0.store(clock("2028-05-01").now(), Ordering::SeqCst);
    assert_eq!(service.resolve("old-node", &r).state, "unsupported");
}

#[cfg(unix)]
#[test]
// Verify interrupted publication is recovered without installing or selecting again
fn interrupted_publication_is_recovered_without_installing_or_selecting_again() {
    let root = tempfile::tempdir().unwrap();
    let bytes = archive("node", b"#!/bin/sh\necho v24.22.1\necho Krypton\n");
    let transport = Arc::new(FixtureTransport {
        bytes: bytes.clone(),
        hold: AtomicBool::new(false),
    });
    let c = clock("2026-10-23");
    let release = release();
    let id = release.id.clone();
    let service = seeded(root.path(), release, transport, c.clone());
    let job = service.start_install(&id).unwrap();
    let completed = finish(&service, &job);
    assert_eq!(completed.state, "complete", "{completed:?}");
    drop(service);
    let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
    let mut state: serde_json::Value = storage
        .read_json(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            16 * 1024 * 1024,
        )
        .unwrap()
        .unwrap();
    state["installations"] = json!([]);
    storage
        .write_json(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            &state,
            16 * 1024 * 1024,
        )
        .unwrap();
    drop(storage);
    let recovered =
        RuntimeService::with_dependencies(root.path().into(), c, Arc::new(Offline)).unwrap();
    assert_eq!(recovered.status().installations.len(), 1);
    assert!(recovered.status().defaults.is_empty());
}

#[test]
// Verify future lts recommendation waits for actual release metadata
fn future_lts_recommendation_waits_for_actual_release_metadata() {
    let root = tempfile::tempdir().unwrap();
    let clock = clock("2027-11-01");
    let service =
        RuntimeService::with_dependencies(root.path().into(), clock, Arc::new(NodeMetadata))
            .unwrap();
    assert!(service.recommended(RuntimeKind::Nodejs).is_none());
    service.check().unwrap();
    assert_eq!(service.recommended(RuntimeKind::Nodejs).unwrap().line, "24");
    assert!(!service
        .status()
        .lines
        .iter()
        .any(|l| l.kind == RuntimeKind::Nodejs && l.line == "27"));
}
#[cfg(unix)]
#[test]
// Verify external installation identity changes when its version changes
fn external_installation_identity_changes_when_its_version_changes() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node");
    std::fs::write(&path, "#!/bin/sh\necho v24.22.1\necho Krypton\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let before = probe(RuntimeKind::Nodejs, &path).unwrap();
    std::fs::write(&path, "#!/bin/sh\necho v24.22.2\necho Krypton\n").unwrap();
    let after = probe(RuntimeKind::Nodejs, &path).unwrap();
    assert_ne!(before.id, after.id);
    assert_eq!(before.executable, after.executable);
}

#[test]
// Verify recommendations require discovery without replacing saved choices
fn recommendations_require_discovery_without_replacing_saved_choices() {
    let fresh = tempfile::tempdir().unwrap();
    let service = RuntimeService::with_dependencies(
        fresh.path().into(),
        clock("2026-10-28"),
        Arc::new(Offline),
    )
    .unwrap();
    assert!(service.recommended(RuntimeKind::Nodejs).is_none());
    let existing = tempfile::tempdir().unwrap();
    let service = RuntimeService::with_dependencies(
        existing.path().into(),
        clock("2026-10-28"),
        Arc::new(Offline),
    )
    .unwrap();
    assert!(service.recommended(RuntimeKind::Nodejs).is_none());
}

// Construct the node schedule test fixture
fn node_schedule() -> serde_json::Value {
    json!({
        "v20": {"lts":"2023-10-24", "end":"2026-04-30"},
        "v22": {"lts":"2024-10-29", "end":"2027-04-30"},
        "v24": {"lts":"2025-10-28", "end":"2028-04-30"},
        "v26": {"lts":"2026-10-28", "end":"2029-04-30"},
        "v27": {"lts":"2027-10-28", "end":"2030-04-30"}
    })
}
// Construct the fixture lines test fixture
fn fixture_lines() -> Vec<RuntimeLine> {
    crate::providers::node_lines(&node_schedule())
}
// Construct the fixture releases test fixture
fn fixture_releases() -> Vec<RuntimeRelease> {
    let mut node = release();
    node.kind = RuntimeKind::Nodejs;
    node.channel = "lts".into();
    node.line = "24".into();
    node.version = "24.22.1".into();
    vec![node]
}

struct FutureMetadata;
impl Transport for FutureMetadata {
    // Download for the current operation
    fn download(
        &self,
        _: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        let _ = (spec, cache, control, progress);
        Err("Metadata-only fixture does not download artifacts".into())
    }

    // Retrieve the requested value
    fn get(
        &self,
        _: RuntimeKind,
        url: &str,
        _: bool,
        _: usize,
        _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        Ok(match url {
            "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json" => serde_json::to_vec(&json!({"v40":{"lts":"2030-01-01", "end":"2099-01-01"},"v41":{"lts":"2030-01-01","end":"2099-01-01"}})).unwrap(),
            "https://nodejs.org/dist/index.json" => serde_json::to_vec(&json!([{"version":"v40.1.0","lts":"Future"},{"version":"v41.0.0","lts":false}])).unwrap(),
            _ => panic!("Unexpected metadata/signature/checksum request: {url}"),
        })
    }
}
#[test]
// Verify future upstream lines need no manager policy or digest updates
fn future_upstream_lines_need_no_manager_policy_or_digest_updates() {
    if crate::providers::platform(RuntimeKind::Nodejs).is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let time = clock("2035-01-01");
    let service = RuntimeService::with_dependencies(
        root.path().into(),
        time.clone(),
        Arc::new(FutureMetadata),
    )
    .unwrap();
    let status = service.check().unwrap();
    assert!(status.error.is_none(), "{:?}", status.error);
    assert_eq!(status.releases.len(), 1);
    assert_eq!(service.recommended(RuntimeKind::Nodejs).unwrap().line, "40");
    assert!(status.releases.iter().any(|r| r.kind == RuntimeKind::Nodejs
        && r.line == "40"
        && r.verification == "official-https"));
    assert!(!status.lines.iter().any(|l| l.line == "41"));
    assert_eq!(status.next_check, time.now() + 86400);
    assert!(!service.due());
    drop(service);
    let offline =
        RuntimeService::with_dependencies(root.path().into(), time.clone(), Arc::new(Offline))
            .unwrap();
    assert_eq!(offline.status().releases.len(), 1);
    offline.check().unwrap();
    assert_eq!(offline.status().releases.len(), 1);
    assert_eq!(offline.status().last_check, status.last_check);
    assert_eq!(offline.status().next_check, time.now() + 3600);
}

#[cfg(unix)]
#[test]
// Verify future releases install without checksum requests and reject wrong executables
fn future_releases_install_without_checksum_requests_and_reject_wrong_executables() {
    struct RuntimeTransport {
        bytes: Vec<u8>,
        requests: std::sync::Mutex<Vec<String>>,
    }
    impl Transport for RuntimeTransport {
        // Download for the current operation
        fn download(
            &self,
            _: RuntimeKind,
            spec: &stream_dock_manager_downloads::ArtifactSpec,
            cache: &stream_dock_manager_downloads::DownloadCache,
            control: &dyn Fn() -> stream_dock_manager_downloads::Control,
            progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<stream_dock_manager_downloads::Artifact, String> {
            self.requests.lock().unwrap().push(spec.url.clone());
            struct Chunks<'a>(&'a [u8]);
            impl std::io::Read for Chunks<'_> {
                // Read for the current operation
                fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                    let size = out.len().min(self.0.len()).min(31);
                    out[..size].copy_from_slice(&self.0[..size]);
                    self.0 = &self.0[size..];
                    Ok(size)
                }
            }
            cache.store_reader_from_origin(
                spec,
                &mut Chunks(&self.bytes),
                control,
                &|done, total| progress(done as u32, total.map(|n| n as u32)),
            )
        }

        // Retrieve the requested value
        fn get(
            &self,
            kind: RuntimeKind,
            url: &str,
            artifact: bool,
            limit: usize,
            progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<Vec<u8>, String> {
            if artifact {
                return Err("Artifact requests must use streaming download".into());
            }
            FutureMetadata.get(kind, url, artifact, limit, progress)
        }
    }
    if crate::providers::platform(RuntimeKind::Nodejs).is_err() {
        return;
    }
    {
        let kind = RuntimeKind::Nodejs;
        for matches in [true, false] {
            let output = match (kind, matches) {
                (RuntimeKind::Nodejs, true) => "v40.1.0\nFuture",
                (RuntimeKind::Nodejs, false) => "v40.0.0\nFuture",
            };
            let script = format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n");
            let bytes = if kind == RuntimeKind::Nodejs {
                let encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                let mut tar = tar::Builder::new(encoder);
                let mut header = tar::Header::new_gnu();
                header.set_size(script.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(&mut header, "bin/node", script.as_bytes())
                    .unwrap();
                tar.into_inner().unwrap().finish().unwrap()
            } else {
                archive(kind.executable(), script.as_bytes())
            };
            let transport = Arc::new(RuntimeTransport {
                bytes,
                requests: Default::default(),
            });
            let root = tempfile::tempdir().unwrap();
            let service = RuntimeService::with_dependencies(
                root.path().into(),
                clock("2035-01-01"),
                transport.clone(),
            )
            .unwrap();
            let selected = service
                .check()
                .unwrap()
                .releases
                .into_iter()
                .find(|r| r.kind == kind)
                .unwrap();
            let job = service.start_install(&selected.id).unwrap();
            let completed = finish(&service, &job);
            assert_eq!(
                completed.state,
                if matches { "complete" } else { "failed" },
                "{kind:?}: {completed:?}"
            );
            assert_eq!(*transport.requests.lock().unwrap(), vec![selected.url]);
            assert_eq!(service.status().installations.len(), usize::from(matches));
            if !matches {
                assert!(completed
                    .error
                    .unwrap()
                    .contains("does not match selected release"));
            }
        }
    }
}

#[test]
// Verify retired runtime preferences are rejected without deleting files or state
fn retired_runtime_preferences_are_rejected_without_deleting_files_or_state() {
    let root = tempfile::tempdir().unwrap();
    let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
    let node = serde_json::to_value(release()).unwrap();
    let mut deno = node.clone();
    deno["kind"] = json!("deno");
    let mut bun = node.clone();
    bun["kind"] = json!("bun");
    let choice = json!({"kind":"nodejs","channel":"lts","line":"24","installation_id":null,"external":null,"acknowledge_override":false});
    let mut old_choice = choice.clone();
    old_choice["kind"] = json!("deno");
    storage
        .write_json(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            &json!({
                "lines":fixture_lines(),"releases":[node,deno,bun],"installations":[],
                "defaults":{"nodejs":choice,"deno":old_choice},"plugins":{"retired":old_choice},
                "automatic":false,"last_check":null,"next_check":0,"error":null
            }),
            16 * 1024 * 1024,
        )
        .unwrap();
    let before = storage
        .read_json::<serde_json::Value>(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            16 * 1024 * 1024,
        )
        .unwrap()
        .unwrap();
    drop(storage);
    let retired_files = root.path().join("deno");
    std::fs::create_dir(&retired_files).unwrap();
    std::fs::write(retired_files.join("keep"), "old installation").unwrap();
    let error = RuntimeService::with_dependencies(
        root.path().into(),
        clock("2026-11-01"),
        Arc::new(Offline),
    )
    .err()
    .unwrap();
    assert!(error.contains("runtime state"));
    let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
    assert_eq!(
        storage
            .read_json::<serde_json::Value>(
                stream_dock_manager_storage::Domain::RuntimeState,
                "preferences",
                16 * 1024 * 1024
            )
            .unwrap()
            .unwrap(),
        before
    );
    assert!(retired_files.join("keep").is_file());
}

#[cfg(unix)]
#[test]
// Verify selecting an older lts patch stays pinned after a newer install and restart
fn selecting_an_older_lts_patch_stays_pinned_after_a_newer_install_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let c = clock("2026-11-01");
    let storage = stream_dock_manager_storage::Storage::open(root.path().into()).unwrap();
    let mut installations = vec![];
    for version in ["24.20.0", "24.22.1"] {
        let executable = root.path().join(format!("node-{version}"));
        std::fs::write(&executable, "fixture").unwrap();
        installations.push(RuntimeInstallation {
            id: version.into(),
            kind: RuntimeKind::Nodejs,
            channel: "lts".into(),
            line: "24".into(),
            version: version.into(),
            executable: executable.to_string_lossy().into(),
            source: "private".into(),
            verification: "fixture".into(),
            provenance: "fixture".into(),
        });
    }
    storage.write_json(stream_dock_manager_storage::Domain::RuntimeState,"preferences",&json!({"lines":fixture_lines(),"releases":[],"installations":installations,"defaults":{},"plugins":{},"automatic":false,"last_check":null,"next_check":0,"error":null}),16*1024*1024).unwrap();
    drop(storage);
    let service =
        RuntimeService::with_dependencies(root.path().into(), c.clone(), Arc::new(Offline))
            .unwrap();
    let choice = RuntimeSelection {
        kind: RuntimeKind::Nodejs,
        channel: "lts".into(),
        line: "24".into(),
        installation_id: Some("24.20.0".into()),
        external: None,
        acknowledge_override: false,
    };
    service.select(None, choice.clone(), None).unwrap();
    service
        .select(
            Some("plugin"),
            choice,
            Some(&req(RuntimeKind::Nodejs, Some("24"))),
        )
        .unwrap();
    assert_eq!(
        service
            .resolve("default", &req(RuntimeKind::Nodejs, None))
            .selected
            .unwrap()
            .version,
        "24.20.0"
    );
    drop(service);
    let again =
        RuntimeService::with_dependencies(root.path().into(), c, Arc::new(Offline)).unwrap();
    assert_eq!(
        again
            .resolve("plugin", &req(RuntimeKind::Nodejs, Some("24")))
            .selected
            .unwrap()
            .version,
        "24.20.0"
    );
}

#[test]
// Verify discovery save failure releases checking and preserves metadata
fn discovery_save_failure_releases_checking_and_preserves_metadata() {
    let root = tempfile::tempdir().unwrap();
    let service = RuntimeService::with_dependencies(
        root.path().into(),
        clock("2026-11-01"),
        Arc::new(NodeMetadata),
    )
    .unwrap();
    let before = service.check().unwrap();
    service.storage().run(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER fail_metadata BEFORE INSERT ON runtime_state BEGIN SELECT RAISE(ABORT, 'injected metadata failure'); END;").map_err(|error| error.to_string())
    }).unwrap();
    assert!(service
        .check()
        .unwrap_err()
        .contains("injected metadata failure"));
    assert_eq!(
        serde_json::to_value(service.status().releases).unwrap(),
        serde_json::to_value(before.releases).unwrap()
    );
    // A stuck checking flag would return Ok without reaching the failing save
    assert!(service
        .check()
        .unwrap_err()
        .contains("injected metadata failure"));
    service
        .storage()
        .run(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_metadata;")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(service.check().unwrap().error.is_none());
}

#[test]
// Verify concurrent metadata checks share one discovery
fn concurrent_metadata_checks_share_one_discovery() {
    struct BlockingMetadata {
        started: std::sync::mpsc::Sender<()>,
        resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        requests: std::sync::atomic::AtomicUsize,
    }
    impl Transport for BlockingMetadata {
        // Download for the current operation
        fn download(
            &self,
            _: RuntimeKind,
            spec: &stream_dock_manager_downloads::ArtifactSpec,
            cache: &stream_dock_manager_downloads::DownloadCache,
            control: &dyn Fn() -> stream_dock_manager_downloads::Control,
            progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<stream_dock_manager_downloads::Artifact, String> {
            let _ = (spec, cache, control, progress);
            Err("Metadata-only fixture does not download artifacts".into())
        }

        // Retrieve the requested value
        fn get(
            &self,
            kind: RuntimeKind,
            url: &str,
            artifact: bool,
            limit: usize,
            progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<Vec<u8>, String> {
            if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.send(()).unwrap();
                self.resume.lock().unwrap().recv().unwrap();
            }
            NodeMetadata.get(kind, url, artifact, limit, progress)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (started, wait) = std::sync::mpsc::channel();
    let (resume, receiver) = std::sync::mpsc::channel();
    let transport = Arc::new(BlockingMetadata {
        started,
        resume: std::sync::Mutex::new(receiver),
        requests: Default::default(),
    });
    let service = Arc::new(
        RuntimeService::with_dependencies(
            root.path().into(),
            clock("2026-11-01"),
            transport.clone(),
        )
        .unwrap(),
    );
    let worker = service.clone();
    let thread = std::thread::spawn(move || worker.check());
    wait.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    service.check().unwrap();
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    resume.send(()).unwrap();
    assert!(thread.join().unwrap().unwrap().error.is_none());
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
}
