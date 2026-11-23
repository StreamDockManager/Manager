//! Controlled HTTP fixtures: counts prove warm responses avoid payload transfer and reparse.
use super::*;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

struct Reply {
    bytes: Vec<u8>,
    delay: Duration,
}
impl Reply {
    fn response(status: &str, headers: &str, body: &[u8]) -> Self {
        let mut bytes = format!(
            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        Self {
            bytes,
            delay: Duration::ZERO,
        }
    }
    fn raw(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            delay: Duration::ZERO,
        }
    }
}
struct Server {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    requests: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/catalog.json", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (send, requests) = mpsc::channel();
        let worker = {
            let seen = seen.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                let mut replies = VecDeque::from(replies);
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    // On macOS an accepted stream inherits the listener's
                    // nonblocking mode. Read the complete request before replying.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n")
                        && request.len() < 16_384
                    {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => request.extend_from_slice(&buffer[..count]),
                        }
                    }
                    if !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        continue;
                    }
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&request).to_lowercase());
                    let _ = send.send(());
                    let reply = replies.pop_front().unwrap_or_else(|| {
                        Reply::response("500 Internal Server Error", "", b"unexpected request")
                    });
                    thread::sleep(reply.delay);
                    let _ = stream.write_all(&reply.bytes);
                }
            })
        };
        Self {
            url,
            seen,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn fixture() -> (tempfile::TempDir, Arc<UpdateService>, Client, AtomicBool) {
    let root = tempfile::tempdir().unwrap();
    let service = UpdateService::open(root.path().join("updates")).unwrap();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    (root, service, client, AtomicBool::new(false))
}
fn request(url: &str) -> CatalogRequest<'_> {
    CatalogRequest {
        source: "repository",
        repository: "owner/plugins",
        channel: "stable",
        url,
        generation: 0,
        token: None,
        kind: CatalogKind::Listing,
    }
}

#[test]
fn incomplete_connection_preserves_the_next_fixture_reply() {
    let server = Server::new(vec![Reply::response("200 OK", "", b"[]")]);
    let url = reqwest::Url::parse(&server.url).unwrap();
    let mut partial = std::net::TcpStream::connect(("127.0.0.1", url.port().unwrap())).unwrap();
    partial
        .write_all(b"GET /catalog.json HTTP/1.1\r\n")
        .unwrap();
    drop(partial);
    let (_root, service, client, cancel) = fixture();
    service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert_eq!(server.count(), 1);
}

#[test]
fn cold_and_warm_catalog_pair_transfers_and_parses_only_once() {
    let server = Server::new(vec![
        Reply::response(
            "200 OK",
            "ETag: \"revision-1\"\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
            b"[]",
        ),
        Reply::response("304 Not Modified", "", b""),
    ]);
    let (_root, service, client, cancel) = fixture();
    let cold = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    let warm = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert!(
        Arc::ptr_eq(&cold, &warm),
        "304 must reuse the parsed object"
    );
    assert!(matches!(&*cold, ParsedCatalog::Listing(items) if items.is_empty()));
    let requests = server.seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].contains("if-none-match:"));
    assert!(requests[1].contains("if-none-match: \"revision-1\""));
    let stats = service.catalog_http_stats.lock().unwrap();
    assert_eq!(stats.parses, 1);
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.transferred, 2);
}

#[test]
fn unusable_304_gets_one_unconditional_retry_only() {
    let (_root, service, client, cancel) = fixture();
    let server = Server::new(vec![
        Reply::response("304 Not Modified", "", b""),
        Reply::response("200 OK", "", b"[]"),
    ]);
    service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert_eq!(server.count(), 2);
    assert!(server
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|request| !request.contains("if-none-match:")));
    let broken = Server::new(vec![
        Reply::response("304 Not Modified", "", b""),
        Reply::response("304 Not Modified", "", b""),
    ]);
    assert!(service
        .catalog_fetch(&client, request(&broken.url), &cancel)
        .is_err());
    assert_eq!(broken.count(), 2, "304 retry must be bounded");
}

#[test]
fn cache_identity_separates_sources_repositories_channels_credentials_and_generations() {
    let server = Server::new(
        (0..7)
            .map(|_| Reply::response("200 OK", "ETag: \"revision\"\r\n", b"[]"))
            .collect(),
    );
    let (_root, service, client, cancel) = fixture();
    for index in 0..7 {
        let mut args = request(&server.url);
        match index {
            1 => args.source = "hosted",
            2 => args.repository = "other/plugins",
            3 => args.channel = "beta",
            4 => args.token = Some("fixture-token-a"),
            5 => args.token = Some("fixture-token-b"),
            6 => {
                service.store.lock().unwrap().catalog_generation = 1;
                args.generation = 1;
            }
            _ => {}
        }
        service.catalog_fetch(&client, args, &cancel).unwrap();
    }
    let requests = server.seen.lock().unwrap();
    assert_eq!(requests.len(), 7);
    assert!(requests
        .iter()
        .all(|request| !request.contains("if-none-match:")));
}

#[test]
fn malformed_oversized_and_chunked_payloads_do_not_poison_the_cache() {
    let (_root, service, client, cancel) = fixture();
    let cases = vec![
        Reply::response("200 OK", "ETag: \"bad\"\r\n", b"{broken"),
        Reply::raw(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 16777217\r\n\r\n".to_vec(),
        ),
        Reply::raw({
            let mut bytes = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            bytes.extend(vec![b' '; BODY_LIMIT + 1]);
            bytes
        }),
        Reply::raw({
            let body = vec![b' '; BODY_LIMIT + 1];
            let mut bytes = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", body.len()).into_bytes();
            bytes.extend(body);
            bytes.extend_from_slice(b"\r\n0\r\n\r\n");
            bytes
        }),
    ];
    for invalid in cases {
        let server = Server::new(vec![invalid, Reply::response("200 OK", "", b"[]")]);
        assert!(service
            .catalog_fetch(&client, request(&server.url), &cancel)
            .is_err());
        service
            .catalog_fetch(&client, request(&server.url), &cancel)
            .unwrap();
        assert!(!server.seen.lock().unwrap()[1].contains("if-none-match:"));
    }
}

#[test]
fn redirects_never_forward_fixture_credentials_to_another_origin() {
    let destination = Server::new(vec![]);
    let source = Server::new(vec![Reply::response(
        "302 Found",
        &format!("Location: {}\r\n", destination.url),
        b"",
    )]);
    let (_root, service, client, cancel) = fixture();
    let mut args = request(&source.url);
    args.token = Some("fixture-token-do-not-forward");
    assert!(service.catalog_fetch(&client, args, &cancel).is_err());
    assert_eq!(destination.count(), 0);
    assert_eq!(source.count(), 1);
}

#[test]
fn cancellation_and_changed_generation_discard_inflight_results() {
    for stale in [false, true] {
        let mut reply = Reply::response("200 OK", "ETag: \"late\"\r\n", b"[]");
        reply.delay = Duration::from_millis(150);
        let server = Server::new(vec![reply]);
        let (_root, service, client, _) = fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = {
            let service = service.clone();
            let cancel = cancel.clone();
            let url = server.url.clone();
            thread::spawn(move || service.catalog_fetch(&client, request(&url), &cancel))
        };
        server
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        if stale {
            service.store.lock().unwrap().catalog_generation = 1;
        } else {
            cancel.store(true, Ordering::SeqCst);
        }
        assert!(worker.join().unwrap().is_err());
        assert_eq!(service.catalog_http_stats.lock().unwrap().hits, 0);
    }
}

#[test]
fn retry_after_does_not_spin_or_hide_rate_limit_errors() {
    let server = Server::new(vec![Reply::response(
        "429 Too Many Requests",
        "Retry-After: 1\r\n",
        b"limited",
    )]);
    let (_root, service, client, cancel) = fixture();
    let start = Instant::now();
    assert!(service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .is_err());
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "rate limit should be reported rather than block the caller"
    );
    let until = service.store.lock().unwrap().plugins.retry_after.unwrap();
    assert!((now()..=now().saturating_add(2)).contains(&until));
    assert_eq!(
        server.count(),
        1,
        "rate limiting must not retry inside the transport"
    );
}

#[test]
fn hosted_catalog_pair_reuses_only_validated_matching_channel() {
    let body = br#"{"schema_version":2,"product":"plugins","repository":"owner/plugins","channel":"stable","plugins":[],"preview":false}"#;
    let server = Server::new(vec![
        Reply::response("200 OK", "ETag: \"hosted\"\r\n", body),
        Reply::response("304 Not Modified", "", b""),
    ]);
    let (_root, service, client, cancel) = fixture();
    let args = || CatalogRequest {
        source: "https://catalog.example/catalog.json",
        repository: "owner/plugins",
        channel: "stable",
        url: &server.url,
        generation: 0,
        token: None,
        kind: CatalogKind::Hosted,
    };
    let cold = service.catalog_fetch(&client, args(), &cancel).unwrap();
    let warm = service.catalog_fetch(&client, args(), &cancel).unwrap();
    assert!(matches!(&*warm, ParsedCatalog::Hosted(value) if value.plugins.is_empty()));
    assert!(Arc::ptr_eq(&cold, &warm));
    let stats = service.catalog_http_stats.lock().unwrap();
    assert_eq!(stats.transferred, body.len() as u64);
    assert_eq!(stats.parses, 1);
    assert_eq!(stats.hits, 1);
}

#[test]
fn later_request_supersedes_inflight_result_for_the_same_identity() {
    let mut first = Reply::response("200 OK", "ETag: \"old\"\r\n", b"[]");
    first.delay = Duration::from_millis(150);
    let server = Server::new(vec![
        first,
        Reply::response("200 OK", "ETag: \"new\"\r\n", b"[]"),
        Reply::response("304 Not Modified", "", b""),
    ]);
    let (_root, service, client, cancel) = fixture();
    let worker = {
        let service = service.clone();
        let client = client.clone();
        let url = server.url.clone();
        thread::spawn(move || {
            service.catalog_fetch(&client, request(&url), &AtomicBool::new(false))
        })
    };
    server
        .requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let current = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert!(worker.join().unwrap().is_err());
    let warm = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert!(Arc::ptr_eq(&current, &warm));
    assert!(server.seen.lock().unwrap()[2].contains("if-none-match: \"new\""));
}

#[test]
fn parsed_cache_has_entry_and_encoded_byte_budgets() {
    let mut cache = ConditionalCache::default();
    for index in 0..CACHE_ENTRIES + 5 {
        cache.insert(ConditionalEntry {
            key: index.to_string(),
            etag: "fixture".into(),
            value: Arc::new(ParsedCatalog::Listing(vec![])),
            bytes: 2,
        });
    }
    assert_eq!(cache.entries.len(), CACHE_ENTRIES);
    for index in 0..8 {
        cache.insert(ConditionalEntry {
            key: format!("large-{index}"),
            etag: "fixture".into(),
            value: Arc::new(ParsedCatalog::Listing(vec![])),
            bytes: BODY_LIMIT,
        });
    }
    assert!(cache.entries.iter().map(|entry| entry.bytes).sum::<usize>() <= CACHE_BYTES);
}

const REAL_LISTING: &[u8] = br#"[{"id":42,"draft":false,"prerelease":false,"tag_name":"plugins-v0.1.0","assets":[],"url":"https://api.github.com/repos/owner/plugins/releases/42","name":"Official plugins"}]"#;
const EMPTY_HOSTED: &[u8] = br#"{"schema_version":2,"product":"plugins","repository":"owner/plugins","channel":"stable","plugins":[],"preview":false}"#;

#[test]
fn production_source_policy_and_value_contracts_reject_mismatches() {
    let source = "https://api.github.com/repos/owner/plugins/releases";
    let mut args = CatalogRequest {
        source,
        repository: "owner/plugins",
        channel: "releases",
        url: "https://api.github.com/repos/owner/plugins/releases?per_page=100&page=1",
        generation: 0,
        token: None,
        kind: CatalogKind::Listing,
    };
    assert!(args.validate().is_ok());
    assert!(args.parse(REAL_LISTING).is_ok());
    for url in [
        "http://api.github.com/repos/owner/plugins/releases?per_page=100&page=1",
        "https://api.github.com/repos/owner/plugins/releases?per_page=100&page=11",
        "https://evil.example/repos/owner/plugins/releases?per_page=100&page=1",
        "https://secret@api.github.com/repos/owner/plugins/releases?per_page=100&page=1",
        "https://api.github.com/repos/owner/plugins/releases?per_page=100&page=1#fragment",
    ] {
        args.url = url;
        assert!(args.validate().is_err(), "{url}");
    }
    args.url = "https://api.github.com/repos/owner/plugins/releases?per_page=100&page=1";
    args.channel = "stable";
    assert!(args.validate().is_err());
    args.channel = "releases";
    args.source = "https://wrong.example/releases";
    assert!(args.validate().is_err());
    args.source = source;
    for field in ["id", "draft", "prerelease", "tag_name", "assets", "url"] {
        let mut value: Vec<serde_json::Value> = serde_json::from_slice(REAL_LISTING).unwrap();
        value[0].as_object_mut().unwrap().remove(field);
        assert!(
            args.validate_value(&ParsedCatalog::Listing(value)).is_err(),
            "{field}"
        );
    }
    let mut hosted = CatalogRequest {
        source: "https://catalog.example/catalog.json",
        repository: "owner/plugins",
        channel: "stable",
        url: "https://catalog.example/catalog.json",
        generation: 0,
        token: None,
        kind: CatalogKind::Hosted,
    };
    assert!(hosted.validate().is_ok());
    assert!(hosted.parse(EMPTY_HOSTED).is_ok());
    assert!(hosted
        .validate_value(&ParsedCatalog::Listing(vec![]))
        .is_err());
    hosted.token = Some("fixture-token");
    assert!(hosted.validate().is_err());
    hosted.token = None;
    hosted.channel = "development";
    assert!(hosted.validate().is_err());
    hosted.url = "https://catalog.example/development/catalog.json";
    assert!(hosted.validate().is_ok());
    assert!(hosted.parse(EMPTY_HOSTED).is_err());
}

#[test]
fn semantic_invalid_success_keeps_the_prior_validated_catalog() {
    let server = Server::new(vec![
        Reply::response("200 OK", "ETag: \"valid\"\r\n", REAL_LISTING),
        Reply::response("200 OK", "ETag: \"invalid\"\r\n", br#"[{"id":0}]"#),
        Reply::response("304 Not Modified", "", b""),
    ]);
    let (_root, service, client, cancel) = fixture();
    let prior = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert!(service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .is_err());
    let warm = service
        .catalog_fetch(&client, request(&server.url), &cancel)
        .unwrap();
    assert!(Arc::ptr_eq(&prior, &warm));
    assert!(server.seen.lock().unwrap()[2].contains("if-none-match: \"valid\""));
}

#[test]
fn damaged_cached_value_and_actual_eviction_make_304_retry_unconditional() {
    for corrupt in [true, false] {
        let server = Server::new(vec![
            Reply::response("200 OK", "ETag: \"before\"\r\n", REAL_LISTING),
            Reply::response("304 Not Modified", "", b""),
            Reply::response("200 OK", "ETag: \"after\"\r\n", REAL_LISTING),
        ]);
        let (_root, service, client, cancel) = fixture();
        service
            .catalog_fetch(&client, request(&server.url), &cancel)
            .unwrap();
        {
            let mut cache = service.catalog_http_cache.lock().unwrap();
            if corrupt {
                cache.entries[0].value =
                    Arc::new(ParsedCatalog::Listing(vec![serde_json::json!({"id":0})]));
            } else {
                for index in 0..CACHE_ENTRIES {
                    cache.insert(ConditionalEntry {
                        key: format!("evict-{index}"),
                        etag: "fixture".into(),
                        value: Arc::new(ParsedCatalog::Listing(vec![])),
                        bytes: 2,
                    });
                }
                assert!(cache
                    .entries
                    .iter()
                    .all(|entry| entry.key != request(&server.url).key()));
            }
        }
        service
            .catalog_fetch(&client, request(&server.url), &cancel)
            .unwrap();
        assert_eq!(server.count(), 3);
        assert!(server.seen.lock().unwrap()[1..]
            .iter()
            .all(|request| !request.contains("if-none-match:")));
        assert_eq!(service.catalog_http_stats.lock().unwrap().retries, 1);
    }
}

#[test]
#[ignore = "explicit paired loopback benchmark; no real TLS/network performance claim"]
fn paired_listing_and_hosted_conditional_benchmark() {
    let replies = |body: &[u8]| {
        let mut replies = vec![Reply::response("200 OK", "ETag: \"fixture\"\r\n", body)];
        for pair in 0..5 {
            for warm in if pair % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                replies.push(if warm {
                    Reply::response("304 Not Modified", "", b"")
                } else {
                    Reply::response("200 OK", "ETag: \"fixture\"\r\n", body)
                });
            }
        }
        replies
    };
    let listing = Server::new(replies(REAL_LISTING));
    let hosted = Server::new(replies(EMPTY_HOSTED));
    let (_warm_root, warm, client, cancel) = fixture();
    let (_cold_root, cold, _, _) = fixture();
    let fetch = |service: &UpdateService| {
        service
            .catalog_fetch(&client, request(&listing.url), &cancel)
            .unwrap();
        service
            .catalog_fetch(
                &client,
                CatalogRequest {
                    source: "https://catalog.example/catalog.json",
                    repository: "owner/plugins",
                    channel: "stable",
                    url: &hosted.url,
                    generation: 0,
                    token: None,
                    kind: CatalogKind::Hosted,
                },
                &cancel,
            )
            .unwrap();
    };
    let row = |service: &UpdateService,
               phase: &str,
               pair: usize,
               elapsed: Duration,
               before: (u64, u64, u64)| {
        let stats = service.catalog_http_stats.lock().unwrap();
        let cache = service.catalog_http_cache.lock().unwrap();
        println!(
            "{}",
            serde_json::json!({"phase":phase,"pair":pair,"elapsed_pair_us":elapsed.as_micros(),"transferred_bytes":stats.transferred-before.0,"parses":stats.parses-before.1,"hits":stats.hits-before.2,"listing_source_bytes":REAL_LISTING.len(),"hosted_source_bytes":EMPTY_HOSTED.len(),"cache_charged_bytes":cache.entries.iter().map(|entry| entry.bytes).sum::<usize>(),"transport":"http-loopback-no-tls"})
        );
    };
    let started = Instant::now();
    fetch(&warm);
    row(&warm, "warmup-cold", 0, started.elapsed(), (0, 0, 0));
    for pair in 0..5 {
        for conditional in if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let service = if conditional { &warm } else { &cold };
            if !conditional {
                service.catalog_http_cache.lock().unwrap().entries.clear();
            }
            let before = {
                let stats = service.catalog_http_stats.lock().unwrap();
                (stats.transferred, stats.parses, stats.hits)
            };
            let started = Instant::now();
            fetch(service);
            let elapsed = started.elapsed();
            row(
                service,
                if conditional {
                    "warm-conditional"
                } else {
                    "unconditional"
                },
                pair,
                elapsed,
                before,
            );
            let stats = service.catalog_http_stats.lock().unwrap();
            assert_eq!(stats.parses - before.1, if conditional { 0 } else { 2 });
            assert_eq!(
                stats.transferred - before.0,
                if conditional {
                    0
                } else {
                    (REAL_LISTING.len() + EMPTY_HOSTED.len()) as u64
                }
            );
        }
    }
}

#[derive(Default)]
struct TransportCredentials(Mutex<std::collections::BTreeMap<String, String>>);
impl crate::credentials::CredentialProvider for TransportCredentials {
    fn read(&self, account: &str) -> Result<String, crate::credentials::CredentialState> {
        self.0
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or(crate::credentials::CredentialState::Missing)
    }
    fn write(&self, account: &str, value: &str) -> Result<(), crate::credentials::CredentialState> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), crate::credentials::CredentialState> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[test]
fn saved_replaced_cleared_credentials_and_source_aba_protect_all_late_writes() {
    for mutation in ["save", "replace", "clear", "source-aba"] {
        for status in ["200 OK", "429 Too Many Requests"] {
            let root = tempfile::tempdir().unwrap();
            let downloads = Arc::new(
                stream_dock_manager_downloads::DownloadCache::open(root.path().join("partials"))
                    .unwrap(),
            );
            let service = UpdateService::open_with_credential_provider(
                root.path().join("updates"),
                downloads,
                Arc::new(TransportCredentials::default()),
            )
            .unwrap();
            let mut preferences = service.status().preferences;
            preferences.plugins_repository = "owner/plugins".into();
            preferences.plugins_catalog_source = Some("repository".into());
            preferences.plugins_catalog_url = None;
            preferences.plugin_channel = "stable".into();
            if mutation == "source-aba" {
                preferences.plugins_catalog_source = Some("hosted".into());
                preferences.plugins_catalog_url = Some("https://a.example/catalog.json".into());
            }
            service.preferences(preferences.clone()).unwrap();
            if ["replace", "clear"].contains(&mutation) {
                service
                    .save_plugin_github_token("owner/plugins", "fixture-original-token")
                    .unwrap();
            }
            let generation = service.store.lock().unwrap().catalog_generation;
            let token = service.plugin_github_token("owner/plugins").unwrap();
            let body = if mutation == "source-aba" {
                EMPTY_HOSTED
            } else {
                REAL_LISTING
            };
            let mut reply = Reply::response(status, "ETag: \"late\"\r\nRetry-After: 600\r\n", body);
            reply.delay = Duration::from_millis(250);
            let server = Server::new(vec![reply]);
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap();
            let worker = {
                let service = service.clone();
                let url = server.url.clone();
                thread::spawn(move || {
                    let mut args = request(&url);
                    args.generation = generation;
                    args.token = token.as_deref();
                    if mutation == "source-aba" {
                        args.kind = CatalogKind::Hosted;
                        args.source = "https://a.example/catalog.json";
                    }
                    service.catalog_fetch(&client, args, &AtomicBool::new(false))
                })
            };
            server
                .requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            match mutation {
                "save" => service
                    .save_plugin_github_token("owner/plugins", "fixture-first-token")
                    .unwrap(),
                "replace" => service
                    .save_plugin_github_token("owner/plugins", "fixture-replacement-token")
                    .unwrap(),
                "clear" => service.clear_plugin_github_token("owner/plugins").unwrap(),
                "source-aba" => {
                    let mut other = preferences.clone();
                    other.plugins_catalog_url = Some("https://b.example/catalog.json".into());
                    service.preferences(other).unwrap();
                    service.preferences(preferences).unwrap();
                }
                _ => unreachable!(),
            }
            let expected = {
                let mut store = service.store.lock().unwrap();
                assert!(store.catalog_generation > generation);
                store.plugins.state = "new-context".into();
                store.plugins.error = Some("new-context-status".into());
                store.plugins.retry_after = Some(17);
                store.manager.retry_after = Some(23);
                (
                    serde_json::to_value(&store.plugins).unwrap(),
                    serde_json::to_value(&store.manager).unwrap(),
                )
            };
            let error = match worker.join().unwrap() {
                Ok(_) => panic!("{mutation}/{status} unexpectedly accepted stale metadata"),
                Err(error) => error,
            };
            service.record_catalog_failure(generation, &error);
            assert!(
                service
                    .catalog_http_cache
                    .lock()
                    .unwrap()
                    .entries
                    .is_empty(),
                "{mutation}/{status}"
            );
            let store = service.store.lock().unwrap();
            assert_eq!(
                serde_json::to_value(&store.plugins).unwrap(),
                expected.0,
                "{mutation}/{status}"
            );
            assert_eq!(
                serde_json::to_value(&store.manager).unwrap(),
                expected.1,
                "{mutation}/{status}"
            );
        }
    }
}
