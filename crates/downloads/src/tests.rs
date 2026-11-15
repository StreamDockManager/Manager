use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};
type RequestLog = Arc<Mutex<Vec<String>>>;
// Extract the named header from a fixture request
fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then(|| value.trim()))
}
// Start the isolated test server
fn server(responses: Vec<Vec<u8>>) -> (String, RequestLog, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(vec![]));
    let log = requests.clone();
    let task = thread::spawn(move || {
        for response in responses {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("Expected fixture request: {error}"),
                }
            };
            // Accepted sockets may inherit the listener's nonblocking mode
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = vec![];
            let mut b = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                if stream.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                request.push(b[0]);
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8(request).unwrap());
            let _ = stream.write_all(&response);
        }
    });
    (url, requests, task)
}
// Construct the fixture response
fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
// Construct the artifact specification used by the test
fn spec(url: &str, bytes: &[u8]) -> ArtifactSpec {
    ArtifactSpec {
        identity: "fixture/release/platform".into(),
        url: url.into(),
        sha256: hex::encode(Sha256::digest(bytes)),
        size: Some(bytes.len() as u64),
        limit: bytes.len() as u64,
    }
}
// Construct the policy used by this operation
fn policy(_: &Url, _: bool) -> Result<(), String> {
    Ok(())
}
// Download for the current operation
fn download(cache: &DownloadCache, spec: &ArtifactSpec) -> Result<Artifact, String> {
    cache.download(
        spec,
        NetworkPolicy {
            headers: None,
            validate: &policy,
            timeout: Duration::from_secs(5),
        },
        &|| Control::Continue,
        &|_, _| Ok(()),
    )
}
#[test]
// Verify authentication headers are recomputed for metadata and artifact redirects
fn authentication_headers_are_recomputed_for_metadata_and_artifact_redirects() {
    for artifact in [false, true] {
        let (url, requests, task) = server(vec![
            response(
                "302 Found",
                "Location: /download\r\nContent-Length: 0\r\n",
                b"",
            ),
            response("200 OK", "Content-Length: 4\r\n", b"data"),
        ]);
        let headers = |current: &Url| {
            let mut values = header::HeaderMap::new();
            if current.path() == "/" {
                values.insert(
                    header::AUTHORIZATION,
                    header::HeaderValue::from_static("Bearer fixture-secret"),
                );
            }
            Ok(values)
        };
        let network = NetworkPolicy {
            headers: Some(&headers),
            validate: &policy,
            timeout: Duration::from_secs(5),
        };
        if artifact {
            let root = tempfile::tempdir().unwrap();
            let cache = DownloadCache::open(root.path().into()).unwrap();
            cache
                .download(
                    &spec(&url, b"data"),
                    network,
                    &|| Control::Continue,
                    &|_, _| Ok(()),
                )
                .unwrap();
        } else {
            assert_eq!(
                metadata(&url, network, 100, &|_, _| Ok(())).unwrap(),
                b"data"
            );
        }
        task.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(
            request_header(&requests[0], "authorization"),
            Some("Bearer fixture-secret")
        );
        assert_eq!(request_header(&requests[1], "authorization"), None);
    }
}
#[test]
// Verify interrupted prefix survives restart rehashes and resumes an exact range
fn interrupted_prefix_survives_restart_rehashes_and_resumes_an_exact_range() {
    let bytes = vec![b'x'; 150000];
    let offset = 50000;
    let first = response(
        "200 OK",
        &format!("Content-Length: {}\r\nETag: \"stable\"\r\n", bytes.len()),
        &bytes[..offset],
    );
    let second = response(
        "206 Partial Content",
        &format!(
            "Content-Length: {}\r\nContent-Range: bytes {offset}-{}/{}\r\nETag: \"stable\"\r\n",
            bytes.len() - offset,
            bytes.len() - 1,
            bytes.len()
        ),
        &bytes[offset..],
    );
    let (url, requests, task) = server(vec![first, second]);
    let root = tempfile::tempdir().unwrap();
    let spec = spec(&url, &bytes);
    let cache = DownloadCache::open(root.path().join("partials")).unwrap();
    assert!(download(&cache, &spec).is_err());
    let dir = cache.root.join(spec.key());
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["completed"], offset);
    // Simulate a crash after body writes but before the next journal commit
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("body.part"))
        .unwrap()
        .write_all(b"unjournaled tail")
        .unwrap();
    std::fs::write(dir.join(".checkpoint-orphan"), b"unfinished metadata").unwrap();
    drop(cache);
    let cache = DownloadCache::open(root.path().join("partials")).unwrap();
    let artifact = download(&cache, &spec).unwrap();
    assert_eq!(std::fs::read(artifact.path()).unwrap(), bytes);
    assert_eq!(artifact.size(), bytes.len() as u64);
    assert!(!dir.join(".checkpoint-orphan").exists());
    assert!(download(&cache, &spec)
        .err()
        .unwrap()
        .contains("active download owner"));
    drop(artifact);
    // Complete cache hits are rehashed and need no third network request
    assert!(download(&cache, &spec).is_ok());
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(request_header(&requests[0], "range"), None);
    assert_eq!(
        request_header(&requests[1], "range"),
        Some(format!("bytes={offset}-").as_str()),
        "requests: {requests:#?}"
    );
    assert_eq!(request_header(&requests[1], "if-range"), Some("\"stable\""));
}
#[test]
// Verify ignored ranges restart and changed prefixes never append unsafe bytes
fn ignored_ranges_restart_and_changed_prefixes_never_append_unsafe_bytes() {
    for corrupt in [false, true] {
        let bytes = b"the exact immutable artifact";
        let first = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", bytes.len()),
            &bytes[..8],
        );
        let full = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", bytes.len()),
            bytes,
        );
        let (url, requests, task) = server(vec![first, full]);
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path().into()).unwrap();
        let spec = spec(&url, bytes);
        assert!(download(&cache, &spec).is_err());
        if corrupt {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(cache.root.join(spec.key()).join("body.part"))
                .unwrap();
            file.write_all(b"altered!").unwrap();
        }
        let artifact = download(&cache, &spec).unwrap();
        assert_eq!(std::fs::read(artifact.path()).unwrap(), bytes);
        task.join().unwrap();
        let request = requests.lock().unwrap()[1].to_ascii_lowercase();
        assert_eq!(request.contains("range: bytes="), !corrupt);
    }
}
#[test]
// Verify invalid ranges and checksums are discarded and cancel drops retained bytes
fn invalid_ranges_and_checksums_are_discarded_and_cancel_drops_retained_bytes() {
    let bytes = b"verified artifact bytes";
    for failure in ["range", "hash", "cancel"] {
        let first = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", bytes.len()),
            &bytes[..4],
        );
        let second = response(
            "206 Partial Content",
            &format!(
                "Content-Length: {}\r\nContent-Range: bytes 5-{}/{}\r\n",
                bytes.len() - 4,
                bytes.len() - 1,
                bytes.len()
            ),
            &bytes[4..],
        );
        let responses = if failure == "range" {
            vec![first, second]
        } else if failure == "cancel" {
            vec![first]
        } else {
            vec![response(
                "200 OK",
                &format!("Content-Length: {}\r\n", bytes.len()),
                bytes,
            )]
        };
        let (url, _, task) = server(responses);
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path().into()).unwrap();
        let mut spec = spec(&url, bytes);
        if failure == "hash" {
            spec.sha256 = "0".repeat(64);
        }
        assert!(download(&cache, &spec).is_err());
        if failure == "range" {
            assert!(download(&cache, &spec)
                .err()
                .unwrap()
                .contains("Content-Range"));
        }
        if failure == "cancel" {
            assert!(cache
                .download(
                    &spec,
                    NetworkPolicy {
                        headers: None,
                        validate: &policy,
                        timeout: Duration::from_secs(5)
                    },
                    &|| Control::Cancel,
                    &|_, _| Ok(())
                )
                .err()
                .unwrap()
                .contains("cancelled"));
        }
        assert!(!cache.root.join(spec.key()).join("body.part").exists());
        assert!(!cache.root.join(spec.key()).join("meta.json").exists());
        task.join().unwrap();
    }
}
#[test]
// Verify redirects are authorized before following and unexpected encodings fail closed
fn redirects_are_authorized_before_following_and_unexpected_encodings_fail_closed() {
    let bytes = b"artifact";
    for encoding in [false, true] {
        let response = if encoding {
            response(
                "200 OK",
                "Content-Encoding: gzip\r\nContent-Length: 8\r\n",
                bytes,
            )
        } else {
            response(
                "302 Found",
                "Location: http://untrusted.invalid/private\r\nContent-Length: 0\r\n",
                b"",
            )
        };
        let (url, requests, task) = server(vec![response]);
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path().into()).unwrap();
        let spec = spec(&url, bytes);
        let allowed = |candidate: &Url, redirect: bool| {
            if !redirect && candidate.as_str() == Url::parse(&url).unwrap().as_str() {
                Ok(())
            } else {
                Err("Rejected redirect".into())
            }
        };
        let error = cache
            .download(
                &spec,
                NetworkPolicy {
                    headers: None,
                    validate: &allowed,
                    timeout: Duration::from_secs(5),
                },
                &|| Control::Continue,
                &|_, _| Ok(()),
            )
            .err()
            .unwrap();
        task.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(error.contains("redirect") || error.contains("Encoded"));
    }
}
#[test]
// Verify cache retention is bounded and live reservations are not evicted
fn cache_retention_is_bounded_and_live_reservations_are_not_evicted() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    for i in 0..70 {
        let mut spec = spec("https://fixture.invalid/artifact", b"x");
        spec.identity = i.to_string();
        drop(storage::acquire(&cache.root, &spec).unwrap());
    }
    assert_eq!(cache.stats().unwrap().entries, 32);
    let live = storage::acquire(&cache.root, &spec("https://fixture.invalid/live", b"x")).unwrap();
    let report = cache.clear().unwrap();
    assert_eq!(report.active_entries, 1);
    assert_eq!(report.stats.entries, 1);
    assert!(live.body.exists());
    drop(live);
    assert_eq!(cache.clear().unwrap().removed_entries, 1);
}
#[cfg(unix)]
#[test]
// Verify cache symlinks preserve external files
fn cache_symlinks_preserve_external_files() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().join("cache")).unwrap();
    let spec = spec("https://fixture.invalid/artifact", b"x");
    let dir = cache.root.join(spec.key());
    std::fs::create_dir_all(&dir).unwrap();
    let external = root.path().join("external");
    std::fs::write(&external, b"preserve").unwrap();
    std::os::unix::fs::symlink(&external, dir.join("body.part")).unwrap();
    assert!(storage::acquire(&cache.root, &spec).is_err());
    assert_eq!(std::fs::read(external).unwrap(), b"preserve");
}
#[test]
// Verify cached artifacts still require the current origin policy and scope discard respects live owners
fn cached_artifacts_still_require_the_current_origin_policy_and_scope_discard_respects_live_owners()
{
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let one = spec("https://fixture.invalid/one", b"one");
    let artifact = cache
        .store_reader_from_origin(
            &one,
            &mut std::io::Cursor::new(b"one"),
            &|| Control::Continue,
            &|_, _| Ok(()),
        )
        .unwrap();
    drop(artifact);
    let rejected = |_: &Url, _: bool| Err("Origin no longer permitted".into());
    assert!(cache
        .download(
            &one,
            NetworkPolicy {
                headers: None,
                validate: &rejected,
                timeout: Duration::from_secs(5)
            },
            &|| Control::Continue,
            &|_, _| Ok(())
        )
        .err()
        .unwrap()
        .contains("Origin no longer permitted"));
    let two = spec("https://fixture.invalid/two", b"two");
    let active = cache
        .store_reader_from_origin(
            &two,
            &mut std::io::Cursor::new(b"two"),
            &|| Control::Continue,
            &|_, _| Ok(()),
        )
        .unwrap();
    cache.discard_identity(&one.identity).unwrap();
    assert!(!cache.root.join(one.key()).exists());
    assert_eq!(std::fs::read(active.path()).unwrap(), b"two");
    drop(active);
    cache.discard_identity(&two.identity).unwrap();
    assert!(!cache.root.join(two.key()).exists());
}
#[test]
// Verify pause retains a checkpoint and metadata reads are bounded and validate redirects
fn pause_retains_a_checkpoint_and_metadata_reads_are_bounded_and_validate_redirects() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let bytes = vec![b'p'; 150000];
    let offset = 65536;
    let ranged = response(
        "206 Partial Content",
        &format!(
            "Content-Length: {}\r\nContent-Range: bytes {offset}-{}/{}\r\n",
            bytes.len() - offset,
            bytes.len() - 1,
            bytes.len()
        ),
        &bytes[offset..],
    );
    let (url, requests, task) = server(vec![ranged]);
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let spec = spec(&url, &bytes);
    let pause = AtomicBool::new(false);
    let error = cache
        .store_reader_from_origin(
            &spec,
            &mut std::io::Cursor::new(&bytes),
            &|| {
                if pause.load(Ordering::SeqCst) {
                    Control::Pause
                } else {
                    Control::Continue
                }
            },
            &|done, _| {
                if done > 0 {
                    pause.store(true, Ordering::SeqCst);
                    return Err("pause requested".into());
                }
                Ok(())
            },
        )
        .err()
        .unwrap();
    assert!(error.contains("pause"));
    assert!(cache.root.join(spec.key()).join("meta.json").exists());
    let artifact = download(&cache, &spec).unwrap();
    assert_eq!(std::fs::read(artifact.path()).unwrap(), bytes);
    task.join().unwrap();
    assert_eq!(
        request_header(&requests.lock().unwrap()[0], "range"),
        Some(format!("bytes={offset}-").as_str())
    );
    let (url, requests, task) = server(vec![
        response("302 Found", "Location: /next\r\nContent-Length: 0\r\n", b""),
        response("200 OK", "Content-Length: 4\r\n", b"meta"),
    ]);
    let calls = Mutex::new(vec![]);
    let check = |_: &Url, redirect: bool| {
        calls.lock().unwrap().push(redirect);
        Ok(())
    };
    let value = metadata(
        &url,
        NetworkPolicy {
            headers: None,
            validate: &check,
            timeout: Duration::from_secs(5),
        },
        10,
        &|_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(value, b"meta");
    assert_eq!(*calls.lock().unwrap(), vec![false, true]);
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(metadata(
        "not a network request",
        NetworkPolicy {
            headers: None,
            validate: &policy,
            timeout: Duration::from_secs(5)
        },
        8 * 1024 * 1024 + 1,
        &|_, _| Ok(())
    )
    .err()
    .unwrap()
    .contains("bounds"));
}

#[test]
// Verify explicit cancel discards a stalled body and releases its owner within the read deadline
fn explicit_cancel_discards_a_stalled_body_and_releases_its_owner_within_the_read_deadline() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let prefix_read = Arc::new(AtomicBool::new(false));
    let observed = prefix_read.clone();
    let task = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nprefix")
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !observed.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        thread::sleep(Duration::from_millis(100));
        signal.store(true, Ordering::SeqCst);
        // Keep the body open until the client closes it. Cancellation must not
        // depend on another body chunk arriving from the server.
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap_or(0), 0);
    });
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let spec = spec(&url, &[b'x'; 100]);
    let started = std::time::Instant::now();
    let error = cache
        .download(
            &spec,
            NetworkPolicy {
                headers: None,
                validate: &policy,
                timeout: Duration::from_secs(30),
            },
            &|| {
                if cancel.load(Ordering::SeqCst) {
                    Control::Cancel
                } else {
                    Control::Continue
                }
            },
            &|done, _| {
                if done > 0 {
                    prefix_read.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        )
        .err()
        .unwrap();
    assert!(error.contains("cancelled"), "{error}");
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(!cache.root.join(spec.key()).join("body.part").exists());
    // The failed worker no longer holds the filesystem lease
    assert!(storage::acquire(&cache.root, &spec).is_ok());
    task.join().unwrap();
}

#[test]
// Verify changed validators never append even when the range matches
fn changed_validators_never_append_even_when_the_range_matches() {
    let bytes = b"the immutable release";
    let (url, _, task) = server(vec![
        response(
            "200 OK",
            &format!("Content-Length: {}\r\nETag: \"one\"\r\n", bytes.len()),
            &bytes[..4],
        ),
        response(
            "206 Partial Content",
            &format!(
                "Content-Length: {}\r\nContent-Range: bytes 4-{}/{}\r\nETag: \"two\"\r\n",
                bytes.len() - 4,
                bytes.len() - 1,
                bytes.len()
            ),
            &bytes[4..],
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let spec = spec(&url, bytes);
    assert!(download(&cache, &spec).is_err());
    assert!(download(&cache, &spec)
        .err()
        .unwrap()
        .contains("validator changed"));
    assert!(!cache.root.join(spec.key()).join("body.part").exists());
    task.join().unwrap();
}

// Run a fixture download with an explicit trusted origin
fn origin_download(cache: &DownloadCache, spec: &ArtifactSpec) -> Result<Artifact, String> {
    cache.download_from_origin(
        spec,
        NetworkPolicy {
            headers: None,
            validate: &policy,
            timeout: Duration::from_secs(5),
        },
        &|| Control::Continue,
        &|_, _| Ok(()),
    )
}
#[test]
// Verify publisher digest remains required by the default download paths
fn publisher_digest_remains_required_by_the_default_download_paths() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let mut request = spec("https://example.org/runtime", b"data");
    request.sha256.clear();
    assert!(download(&cache, &request).is_err());
    request.sha256 = "invalid".into();
    assert!(cache
        .store_reader_from_origin(
            &request,
            &mut &b"data"[..],
            &|| Control::Continue,
            &|_, _| Ok(())
        )
        .is_err());
    request.sha256.clear();
    let stored = cache
        .store_reader_from_origin(
            &request,
            &mut &b"data"[..],
            &|| Control::Continue,
            &|_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(std::fs::read(stored.path()).unwrap(), b"data");
}
#[test]
// Verify origin download resumes only with a matching strong etag
fn origin_download_resumes_only_with_a_matching_strong_etag() {
    for resumed_etag in [Some("\"v1\""), Some("\"v2\""), None] {
        let second_headers = format!(
            "Content-Length: 2\r\nContent-Range: bytes 2-3/4\r\n{}",
            resumed_etag
                .map(|e| format!("ETag: {e}\r\n"))
                .unwrap_or_default()
        );
        let (url, requests, task) = server(vec![
            response("200 OK", "Content-Length: 4\r\nETag: \"v1\"\r\n", b"da"),
            response("206 Partial Content", &second_headers, b"ta"),
        ]);
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path().into()).unwrap();
        let mut request = spec(&url, b"data");
        request.sha256.clear();
        assert!(origin_download(&cache, &request).is_err());
        let result = origin_download(&cache, &request);
        if resumed_etag == Some("\"v1\"") {
            assert_eq!(std::fs::read(result.unwrap().path()).unwrap(), b"data");
        } else {
            assert!(result.is_err());
        }
        task.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(request_header(&requests[1], "range"), Some("bytes=2-"));
        assert_eq!(request_header(&requests[1], "if-range"), Some("\"v1\""));
    }
}
#[test]
// Verify origin download restarts unvalidated partials and rehashes cached bytes
fn origin_download_restarts_unvalidated_partials_and_rehashes_cached_bytes() {
    let (url, requests, task) = server(vec![
        response("200 OK", "Content-Length: 4\r\n", b"da"),
        response("200 OK", "Content-Length: 4\r\n", b"data"),
        response("200 OK", "Content-Length: 4\r\n", b"data"),
    ]);
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let mut request = spec(&url, b"data");
    request.sha256.clear();
    assert!(origin_download(&cache, &request).is_err());
    let stored = origin_download(&cache, &request).unwrap();
    assert_eq!(std::fs::read(stored.path()).unwrap(), b"data");
    let path = stored.path().to_owned();
    drop(stored);
    // A complete file is reused only after checking its local checkpoint hash
    drop(origin_download(&cache, &request).unwrap());
    std::fs::write(path, b"oops").unwrap();
    let stored = origin_download(&cache, &request).unwrap();
    assert_eq!(std::fs::read(stored.path()).unwrap(), b"data");
    task.join().unwrap();
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| request_header(r, "range").is_none()));
}

// Count the cache entries retained after the operation
fn retained(cache: &DownloadCache, name: &str, bytes: &[u8]) -> ArtifactSpec {
    let mut spec = spec("https://fixture.invalid/artifact", bytes);
    spec.identity = name.into();
    drop(
        cache
            .store_reader_from_origin(
                &spec,
                &mut std::io::Cursor::new(bytes),
                &|| Control::Continue,
                &|_, _| Ok(()),
            )
            .unwrap(),
    );
    spec
}
// Touch for the current operation
fn touch(cache: &DownloadCache, spec: &ArtifactSpec, timestamp: u64) {
    let path = cache.root.join(spec.key()).join("meta.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["touched"] = timestamp.into();
    std::fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
}
#[test]
// Verify retention uses actual bytes and lru not declared reservations
fn retention_uses_actual_bytes_and_lru_not_declared_reservations() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let oldest = retained(&cache, "oldest", b"old");
    let newest = retained(&cache, "newest", b"new");
    touch(&cache, &oldest, 100);
    touch(&cache, &newest, 200);
    let total = cache.stats().unwrap();
    let actual: u64 = [oldest.key(), newest.key()]
        .iter()
        .flat_map(|key| std::fs::read_dir(root.path().join(key)).unwrap())
        .map(|f| f.unwrap().metadata().unwrap().len())
        .sum();
    assert_eq!(total.bytes, actual);
    assert!(total.bytes > 6); // Metadata occupies physical bytes too.
    let limited = DownloadCache::open_with_limits(
        root.path().into(),
        CacheLimits {
            max_age: Duration::MAX,
            max_bytes: total.bytes - 1,
            max_entries: 32,
        },
    )
    .unwrap();
    let report = limited.prune().unwrap();
    assert_eq!(report.removed_entries, 1);
    assert!(!root.path().join(oldest.key()).exists());
    assert!(root.path().join(newest.key()).exists());
    assert_eq!(report.removed_bytes + report.stats.bytes, total.bytes);
}
#[test]
// Verify retention expires at age boundary and refreshes on reuse
fn retention_expires_at_age_boundary_and_refreshes_on_reuse() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let old = retained(&cache, "old", b"x");
    let reused = retained(&cache, "reused", b"y");
    touch(&cache, &old, 1);
    touch(&cache, &reused, 1);
    drop(storage::acquire(&cache.root, &reused).unwrap());
    assert!(!root.path().join(old.key()).exists());
    assert!(root.path().join(reused.key()).exists());
    let zero_age = DownloadCache::open_with_limits(
        root.path().into(),
        CacheLimits {
            max_age: Duration::ZERO,
            ..CacheLimits::default()
        },
    )
    .unwrap();
    assert_eq!(zero_age.prune().unwrap().removed_entries, 1);
}
#[test]
// Verify clear preserves artifact consumers and reports unknown or malformed entries
fn clear_preserves_artifact_consumers_and_reports_unknown_or_malformed_entries() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let bad = retained(&cache, "bad", b"bad");
    std::fs::write(root.path().join(bad.key()).join("meta.json"), b"not json").unwrap();
    let unknown = retained(&cache, "unknown", b"unknown");
    std::fs::write(
        root.path().join(unknown.key()).join("personal.txt"),
        b"preserve",
    )
    .unwrap();
    let spec = spec("https://fixture.invalid/live", b"live");
    let live = cache
        .store_reader_from_origin(
            &spec,
            &mut std::io::Cursor::new(b"live"),
            &|| Control::Continue,
            &|_, _| Ok(()),
        )
        .unwrap();
    let report = cache.clear().unwrap();
    assert_eq!(report.active_entries, 1);
    assert_eq!(report.skipped_entries, 2);
    assert_eq!(report.stats.active_entries, 1);
    assert_eq!(std::fs::read(live.path()).unwrap(), b"live");
    assert!(root.path().join(bad.key()).exists());
    assert!(root.path().join(unknown.key()).exists());
    drop(live);
    assert_eq!(cache.clear().unwrap().removed_entries, 1);
}
#[test]
// Verify concurrent acquisition and clear never unlink live entries
fn concurrent_acquisition_and_clear_never_unlink_live_entries() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..40 {
                cache.clear().unwrap();
            }
        });
        for i in 0..40 {
            let mut spec = spec("https://fixture.invalid/live", b"live");
            spec.identity = i.to_string();
            let artifact = cache
                .store_reader_from_origin(
                    &spec,
                    &mut std::io::Cursor::new(b"live"),
                    &|| Control::Continue,
                    &|_, _| Ok(()),
                )
                .unwrap();
            thread::yield_now();
            assert_eq!(std::fs::read(artifact.path()).unwrap(), b"live");
        }
    });
}
#[cfg(unix)]
#[test]
// Verify maintenance never follows symlink entries or bodies
fn maintenance_never_follows_symlink_entries_or_bodies() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().join("cache")).unwrap();
    let spec = retained(&cache, "linked-body", b"body");
    let external = root.path().join("external");
    std::fs::write(&external, b"preserve").unwrap();
    let body = cache.root.join(spec.key()).join("body.part");
    std::fs::remove_file(&body).unwrap();
    std::os::unix::fs::symlink(&external, body).unwrap();
    std::os::unix::fs::symlink(root.path(), cache.root.join("a".repeat(64))).unwrap();
    assert_eq!(cache.clear().unwrap().skipped_entries, 2);
    assert_eq!(std::fs::read(external).unwrap(), b"preserve");
}

#[test]
// Verify stats include preserved files and flag unmeasurable contents
fn stats_include_preserved_files_and_flag_unmeasurable_contents() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    std::fs::write(root.path().join("unrecognized.txt"), b"five!").unwrap();
    let directory = root.path().join("unknown");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("file"), b"123").unwrap();
    let stats = cache.stats().unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.skipped_entries, 2);
    assert_eq!(stats.bytes, 8);
    assert!(!stats.usage_incomplete);
    std::fs::create_dir(directory.join("nested")).unwrap();
    let report = cache.clear().unwrap();
    assert_eq!(report.removed_entries, 0);
    assert!(report.stats.usage_incomplete);
    assert_eq!(report.stats.bytes, 8);
}

#[test]
// Verify zero limits preserve active consumers until they release the lease
fn zero_limits_preserve_active_consumers_until_they_release_the_lease() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open_with_limits(
        root.path().into(),
        CacheLimits {
            max_age: Duration::ZERO,
            max_bytes: 0,
            max_entries: 0,
        },
    )
    .unwrap();
    let spec = spec("https://fixture.invalid/zero-limits", b"live");
    let artifact = cache
        .store_reader_from_origin(
            &spec,
            &mut std::io::Cursor::new(b"live"),
            &|| Control::Continue,
            &|_, _| Ok(()),
        )
        .unwrap();
    let report = cache.prune().unwrap();
    assert_eq!(report.active_entries, 1);
    assert_eq!(report.removed_entries, 0);
    assert_eq!(std::fs::read(artifact.path()).unwrap(), b"live");
    drop(artifact);
    let report = cache.prune().unwrap();
    assert_eq!(report.removed_entries, 1);
    assert_eq!(report.stats.entries, 0);
    assert_eq!(report.stats.bytes, 0);
}

#[cfg(unix)]
#[test]
// Verify maintenance rejects replaced root and ancestor links
fn maintenance_rejects_replaced_root_and_ancestor_links() {
    for ancestor in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let cache = DownloadCache::open(parent.join("cache")).unwrap();
        let victim = root.path().join("victim");
        std::fs::create_dir_all(victim.join("cache")).unwrap();
        std::fs::write(victim.join("keep"), b"preserve").unwrap();
        let replace = if ancestor { parent } else { cache.root.clone() };
        std::fs::rename(&replace, root.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&victim, replace).unwrap();
        assert!(cache.clear().is_err());
        assert!(cache.prune().is_err());
        assert!(cache.stats().is_err());
        assert_eq!(std::fs::read(victim.join("keep")).unwrap(), b"preserve");
        assert!(!victim.join("registry.lock").exists());
        assert!(!victim.join("cache/registry.lock").exists());
    }
}

#[test]
// Verify discarded empty leases are reclaimed without evicting retained artifacts
fn discarded_empty_leases_are_reclaimed_without_evicting_retained_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let kept = retained(&cache, "keep", b"keep");
    for i in 0..70 {
        let mut spec = spec("https://fixture.invalid/cancel", b"cancel");
        spec.identity = format!("cancel-{i}");
        let entry = storage::acquire(&cache.root, &spec).unwrap();
        entry.discard().unwrap();
        drop(entry);
    }
    let report = cache.prune().unwrap();
    assert_eq!(report.stats.entries, 1);
    assert_eq!(report.stats.skipped_entries, 0);
    assert_eq!(
        std::fs::read(cache.root.join(kept.key()).join("body.part")).unwrap(),
        b"keep"
    );
}

#[test]
// Verify empty lease cleanup protects active owners and preserves orphan files
fn empty_lease_cleanup_protects_active_owners_and_preserves_orphan_files() {
    let root = tempfile::tempdir().unwrap();
    let cache = DownloadCache::open(root.path().into()).unwrap();
    let live = storage::acquire(
        &cache.root,
        &spec("https://fixture.invalid/cancel", b"cancel"),
    )
    .unwrap();
    live.discard().unwrap();
    let report = cache.clear().unwrap();
    assert_eq!(report.active_entries, 1);
    assert_eq!(report.skipped_entries, 0);
    assert_eq!(report.removed_entries, 0);
    drop(live);
    assert_eq!(cache.prune().unwrap().removed_entries, 1);
    for (i, name) in [
        "body.part",
        ".checkpoint-orphan",
        "unknown.txt",
        "meta.json",
    ]
    .iter()
    .enumerate()
    {
        let path = cache.root.join(format!("{i:064x}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("lease.lock"), []).unwrap();
        std::fs::write(path.join(name), b"preserve").unwrap();
    }
    let report = cache.clear().unwrap();
    assert_eq!(report.removed_entries, 0);
    assert_eq!(report.skipped_entries, 4);
    assert_eq!(report.stats.entries, 4);
}
