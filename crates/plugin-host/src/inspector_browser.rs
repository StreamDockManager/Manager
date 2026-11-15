//! Revocable per-inspector HTTP origins for isolated native vendor windows
use super::*;

#[derive(Clone)]
pub struct InspectorBrowser {
    pub(crate) hub: Arc<Mutex<Hub>>,
    pub(crate) token: String,
    pub(crate) owner: String,
    pub(crate) url: String,
    pub(crate) alive: Arc<AtomicBool>,
}
impl InspectorBrowser {
    // Read the token identifying this inspector session
    pub fn token(&self) -> &str {
        &self.token
    }
    // Read the plugin that owns this inspector session
    pub fn owner(&self) -> &str {
        &self.owner
    }
    // Resolve the URL for this inspector session
    pub fn url(&self) -> &str {
        &self.url
    }
    // Check whether the value is active
    pub fn is_active(&self) -> bool {
        let hub = self.hub.lock().unwrap();
        self.alive.load(Ordering::Acquire) && active(&hub, &self.owner, &self.token)
    }
    /// Closing a native tree immediately disables its endpoint, including while
    /// the editor is still finishing pending vendor settings writes.
    pub fn revoke(&self) {
        let mut hub = self.hub.lock().unwrap();
        self.alive.store(false, Ordering::Release);
        hub.sessions.resource_selections.remove(&self.token);
        if let Some(wake) = hub
            .sessions
            .queues
            .get(&format!("pi:{}", self.token))
            .and_then(|queue| queue.wake.as_ref())
        {
            let _ = wake.wake();
        }
    }
}
// Check whether this inspector session is still active
fn active(hub: &Hub, owner: &str, token: &str) -> bool {
    hub.sessions.tokens.get(token).map(String::as_str) == Some(owner)
        && hub
            .sessions
            .files
            .get(token)
            .and_then(|grant| grant.context.as_ref())
            .is_some_and(|context| {
                hub.configuration_actions.contains_key(context)
                    && hub.context_state.contexts.get(context).map(String::as_str) == Some(owner)
            })
        && !hub.capabilities.contains_key(owner)
}
// Check whether the request origin belongs to the inspector session
pub(super) fn origin_allowed(hub: &Hub, token: &str, origin: Option<&str>) -> bool {
    hub.sessions
        .inspector_origins
        .get(token)
        .is_none_or(|(port, alive)| {
            alive.load(Ordering::Acquire)
                && origin == Some(format!("http://127.0.0.1:{port}").as_str())
        })
}
impl Hub {
    // Revoke native inspectors
    pub(super) fn revoke_native_inspectors(&mut self, owner: &str) {
        let tokens: Vec<_> = self
            .sessions
            .inspector_origins
            .keys()
            .filter(|token| self.sessions.tokens.get(*token).map(String::as_str) == Some(owner))
            .cloned()
            .collect();
        for token in tokens {
            if let Some((_, alive)) = self.sessions.inspector_origins.get(&token) {
                alive.store(false, Ordering::Release);
            }
            self.sessions.resource_selections.remove(&token);
            if let Some(wake) = self
                .sessions
                .queues
                .get(&format!("pi:{token}"))
                .and_then(|queue| queue.wake.as_ref())
            {
                let _ = wake.wake();
            }
        }
    }
}
// Choose the default inspector window dimensions
fn default_window_size(dialect: &str, manifest: &Value) -> [f64; 2] {
    if dialect == "streamdeck" {
        if let Some(values) = manifest["DefaultWindowSize"]
            .as_array()
            .filter(|values| values.len() == 2)
        {
            if let (Some(width), Some(height)) = (values[0].as_f64(), values[1].as_f64()) {
                if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                    return [width, height];
                }
            }
        }
    }
    [500.0, 650.0]
}
impl PluginHost {
    // Resolve the configured inspector window dimensions
    pub fn inspector_default_window_size(&self, owner: &str) -> Result<[f64; 2], String> {
        let plugin = self.plugins.get(owner).ok_or("Plugin not installed")?;
        Ok(default_window_size(&plugin.info.dialect, &plugin.manifest))
    }
    // Build an isolated property inspector session
    pub fn isolate_inspector(
        &self,
        owner: &str,
        panel: &SettingsPanel,
    ) -> Result<InspectorBrowser, String> {
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let prefix = format!("http://127.0.0.1:{}/files/{}/", self.port, panel.token);
        let relative = panel
            .url
            .strip_prefix(&prefix)
            .ok_or("Invalid inspector URL")?;
        let mut hub = self.hub.lock().unwrap();
        if !active(&hub, owner, &panel.token) {
            return Err("Vendor inspector expired".into());
        }
        if hub.sessions.inspector_origins.contains_key(&panel.token) {
            return Err("Inspector already isolated".into());
        }
        // The native window registry imposes a tighter one-root limit. This
        // host bound also protects callers that never create a window.
        let live: HashSet<_> = hub.sessions.tokens.keys().cloned().collect();
        hub.sessions
            .inspector_origins
            .retain(|token, _| live.contains(token));
        if hub.sessions.inspector_origins.len() >= 8 {
            return Err("Too many isolated inspectors".into());
        }
        let alive = Arc::new(AtomicBool::new(true));
        hub.sessions
            .inspector_origins
            .insert(panel.token.clone(), (port, alive.clone()));
        drop(hub);
        let browser = InspectorBrowser {
            hub: self.hub.clone(),
            token: panel.token.clone(),
            owner: owner.into(),
            url: format!("http://127.0.0.1:{port}/files/{}/{relative}", panel.token),
            alive,
        };
        let server = browser.clone();
        std::thread::Builder::new()
            .name("inspector-files".into())
            .spawn(move || {
                let admission = server_admission::Admission::default();
                while server.is_active() {
                    match listener.accept() {
                        Ok((stream, peer)) if peer.ip().is_loopback() => {
                            let Some(permit) = admission.files.acquire() else {
                                continue;
                            };
                            let request = server.clone();
                            let _ = std::thread::Builder::new()
                                .name("inspector-file".into())
                                .spawn(move || {
                                    let _permit = permit;
                                    if request.is_active() {
                                        browser::serve_inspector_file(
                                            stream,
                                            request.hub,
                                            &request.token,
                                        );
                                    }
                                });
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => break,
                    }
                }
                // Keep the inactive origin tombstone until the token is retired
                // Otherwise the shared listener could serve it again during close
                server.revoke();
            })
            .map_err(|e| {
                browser.revoke();

                e.to_string()
            })?;
        Ok(browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the isolated test fixture
    fn fixture() -> InspectorBrowser {
        let mut hub = Hub::default();
        hub.sessions.tokens.insert("token".into(), "plugin".into());
        hub.sessions.files.insert(
            "token".into(),
            FileGrant {
                root: PathBuf::new(),
                bootstrap: String::new(),
                context: Some("context".into()),
                setup_runtime: false,
            },
        );
        hub.context_state
            .contexts
            .insert("context".into(), "plugin".into());
        hub.configuration_actions
            .insert("context".into(), "action".into());
        InspectorBrowser {
            hub: Arc::new(Mutex::new(hub)),
            token: "token".into(),
            owner: "plugin".into(),
            url: "http://127.0.0.1:1/files/token/pi.html".into(),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
    #[test]
    // Verify elgato popup size uses published default without inferring VSD
    fn elgato_popup_size_uses_published_default_without_inferring_vsd() {
        assert_eq!(
            default_window_size("streamdeck", &json!({})),
            [500.0, 650.0]
        );
        assert_eq!(
            default_window_size("streamdeck", &json!({"DefaultWindowSize":[800,900]})),
            [800.0, 900.0]
        );
        assert_eq!(
            default_window_size("vsd", &json!({"DefaultWindowSize":[800,900]})),
            [500.0, 650.0]
        );
        for invalid in [
            json!([0, 900]),
            json!([-1, 900]),
            json!([800]),
            json!(["800", 900]),
        ] {
            assert_eq!(
                default_window_size("streamdeck", &json!({"DefaultWindowSize":invalid})),
                [500.0, 650.0]
            );
        }
    }
    #[test]
    // Verify inspector lease tracks owner context and revocation
    fn inspector_lease_tracks_owner_context_and_revocation() {
        let lease = fixture();
        assert!(lease.is_active());
        let mut other = lease.clone();
        other.owner = "other".into();
        assert!(!other.is_active());
        lease
            .hub
            .lock()
            .unwrap()
            .context_state
            .contexts
            .insert("context".into(), "other".into());
        assert!(!lease.is_active());
        let lease = fixture();
        let child = lease.clone();
        lease.revoke();
        assert!(!child.is_active());
        let lease = fixture();
        lease.hub.lock().unwrap().sessions.tokens.remove("token");
        assert!(!lease.is_active());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use tungstenite::{client::IntoClientRequest, Message};
    #[test]
    // Verify isolated WebSocket origin and passive revocation are enforced
    fn isolated_websocket_origin_and_passive_revocation_are_enforced() {
        for origin in [
            None,
            Some("http://127.0.0.1:4101"),
            Some("http://127.0.0.1:4102"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let mut hub = Hub::default();
            hub.sessions
                .tokens
                .insert("isolated".into(), "plugin".into());
            hub.sessions.files.insert(
                "isolated".into(),
                FileGrant {
                    root: PathBuf::new(),
                    bootstrap: String::new(),
                    context: Some("context".into()),
                    setup_runtime: false,
                },
            );
            hub.context_state
                .contexts
                .insert("context".into(), "plugin".into());
            hub.configuration_actions
                .insert("context".into(), "action".into());
            let alive = Arc::new(AtomicBool::new(true));
            hub.sessions
                .inspector_origins
                .insert("isolated".into(), (4101, alive.clone()));
            let hub = Arc::new(Mutex::new(hub));
            let worker = hub.clone();
            let server = std::thread::spawn(move || {
                serve(
                    listener.accept().unwrap().0,
                    worker,
                    Arc::new(AtomicBool::new(false)),
                )
            });
            let mut request = format!("ws://{address}").into_client_request().unwrap();
            if let Some(origin) = origin {
                request
                    .headers_mut()
                    .insert("Origin", origin.parse().unwrap());
            }
            let stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (mut socket, _) = tungstenite::client(request, stream).unwrap();
            socket
                .send(Message::text(
                    json!({"event":"registerPropertyInspector","uuid":"isolated"}).to_string(),
                ))
                .unwrap();
            if origin == Some("http://127.0.0.1:4101") {
                let first = socket.read().expect("Matching origin registers");
                assert!(first.to_text().unwrap().contains("didReceiveSettings"));
                let lease = InspectorBrowser {
                    hub: hub.clone(),
                    token: "isolated".into(),
                    owner: "plugin".into(),
                    url: String::new(),
                    alive,
                };
                lease.revoke();
                hub.lock()
                    .unwrap()
                    .sessions
                    .queues
                    .entry("pi:isolated".into())
                    .or_default()
                    .push_back(json!({"event":"never-after-revoke"}));
                loop {
                    match socket.read() {
                        Ok(message) => assert!(!message
                            .to_text()
                            .unwrap_or("")
                            .contains("never-after-revoke")),
                        Err(tungstenite::Error::Io(error))
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                            ) =>
                        {
                            panic!("Passive revoked session did not close")
                        }
                        Err(_) => break,
                    }
                }
                assert!(
                    hub.lock()
                        .unwrap()
                        .sessions
                        .inspector_origins
                        .contains_key("isolated"),
                    "Revoked token keeps origin tombstone"
                );
            } else {
                assert!(
                    matches!(socket.read(), Err(_) | Ok(Message::Close(_))),
                    "Missing/foreign browser Origin must not register"
                );
            }
            server.join().unwrap();
        }
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    #[test]
    // Verify isolated HTTP rejects cross session shared listener and inflight revocation
    fn isolated_http_rejects_cross_session_shared_listener_and_inflight_revocation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("pi.html"),
            "<!doctype html><title>Private fixture</title>",
        )
        .unwrap();
        for case in [
            "own",
            "other-token",
            "shared",
            "revoked-header",
            "foreign-host",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let alive = Arc::new(AtomicBool::new(true));
            let mut hub = Hub::default();
            for token in ["token", "other"] {
                hub.sessions.tokens.insert(token.into(), "plugin".into());
                hub.sessions.files.insert(
                    token.into(),
                    FileGrant {
                        root: root.path().canonicalize().unwrap(),
                        bootstrap: String::new(),
                        context: Some("context".into()),
                        setup_runtime: false,
                    },
                );
                hub.sessions.inspector_origins.insert(
                    token.into(),
                    (
                        if case == "shared" {
                            port.wrapping_add(1)
                        } else {
                            port
                        },
                        alive.clone(),
                    ),
                );
            }
            let hub = Arc::new(Mutex::new(hub));
            let server = std::thread::spawn(move || {
                let stream = listener.accept().unwrap().0;
                if case == "shared" {
                    browser::serve_file(stream, hub);
                } else {
                    browser::serve_inspector_file(stream, hub, "token");
                }
            });
            let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let target = if case == "other-token" {
                "other"
            } else {
                "token"
            };
            let hostname = if case == "foreign-host" {
                "hostile.example".into()
            } else {
                format!("127.0.0.1:{port}")
            };
            write!(
                client,
                "GET /files/{target}/pi.html HTTP/1.1\r\nHost: {hostname}\r\n"
            )
            .unwrap();
            if case == "revoked-header" {
                alive.store(false, Ordering::Release);
            }
            client.write_all(b"\r\n").unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            if case == "own" {
                assert!(response.starts_with("HTTP/1.1 200"));
                assert!(response.contains("Private fixture"));
                assert!(response.contains("Permissions-Policy: camera=()"));
                assert!(response.contains("frame-ancestors 'none'"));
            } else {
                assert!(response.starts_with("HTTP/1.1 403"), "{case}: {response}");
                assert!(!response.contains("Private fixture"));
            }
            server.join().unwrap();
        }
    }
}
