//! Disposable profile-effect admission tests. No plugin process or hardware runs
use super::*;

const OWNER: &str = "fixture.profile-effects";

// Construct the isolated test fixture
fn fixture(socket: bool) -> (tempfile::TempDir, PluginHost, String) {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("package"), OWNER);
    let mut host = if socket {
        lifecycle_tests::socket_host(root.path())
    } else {
        lifecycle_tests::host(root.path())
    };
    let token = uuid::Uuid::new_v4().to_string();
    let plugin = host.plugins.get_mut(OWNER).unwrap();
    plugin.token = token.clone();
    plugin.info.dialect = "streamdeck".into();
    plugin.info.capabilities = None;
    {
        let mut hub = host.hub.lock().unwrap();
        hub.enabled.insert(OWNER.into());
        hub.capabilities.remove(OWNER);
        hub.manifest_dialects
            .insert(OWNER.into(), "streamdeck".into());
        hub.sessions.tokens.insert(token.clone(), OWNER.into());
        hub.devices
            .insert("device".into(), json!({"id":"device","type":0}));
        if !socket {
            hub.sessions.connected.insert(token.clone());
        }
    }
    (root, host, token)
}

#[test]
// Verify profile commit guard holds session retirement lock and rechecks authority
fn profile_commit_guard_holds_session_retirement_lock_and_rechecks_authority() {
    let (_root, mut host, token) = fixture(false);
    let value = host
        .with_profile_switch_session(OWNER, &token, || {
            assert!(matches!(
                host.hub.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            Ok(42)
        })
        .unwrap();
    assert_eq!(value, 42);
    assert!(host.hub.try_lock().is_ok(), "guard releases after commit");
    assert!(host
        .with_profile_switch_session(OWNER, "wrong", || -> Result<(), String> {
            panic!("wrong session must never enter commit")
        })
        .is_err());
    assert!(host
        .with_profile_switch_session("another-owner", &token, || -> Result<(), String> {
            panic!("wrong owner must never enter commit")
        })
        .is_err());
    host.hub.lock().unwrap().enabled.remove(OWNER);
    assert!(host
        .with_profile_switch_session(OWNER, &token, || -> Result<(), String> {
            panic!("disabled owner must never enter commit")
        })
        .is_err());
    {
        let mut hub = host.hub.lock().unwrap();
        hub.enabled.insert(OWNER.into());
        hub.sessions.connected.remove(&token);
    }
    assert!(host
        .with_profile_switch_session(OWNER, &token, || -> Result<(), String> {
            panic!("retired session must never enter commit")
        })
        .is_err());
    host.hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .insert(token.clone());
    host.plugins.get_mut(OWNER).unwrap().info.dialect = "vsd".into();
    assert!(host
        .with_profile_switch_session(OWNER, &token, || -> Result<(), String> {
            panic!("unsupported dialect must never enter commit")
        })
        .is_err());
}

#[test]
// Verify socket profile switches reject unknown devices throttle coalesce and bound
fn socket_profile_switches_reject_unknown_devices_throttle_coalesce_and_bound() {
    use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
    // Wait until the fixture host has processed preceding events
    fn barrier(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, marker: u8) {
        socket.send(Message::Ping(vec![marker].into())).unwrap();
        loop {
            match socket.read().unwrap() {
                Message::Pong(bytes) if bytes.as_ref() == [marker] => break,
                Message::Close(_) => panic!("profile fixture connection closed"),
                _ => {}
            }
        }
    }
    // Process the request through this subsystem
    fn request(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, device: &str, name: &str) {
        socket
            .send(Message::text(
                json!({"event":"switchToProfile","device":device,"payload":{"profile":name}})
                    .to_string(),
            ))
            .unwrap();
    }
    // Report the pending work tracked by this owner
    fn pending(host: &PluginHost) -> Vec<ProfileSwitchEffect> {
        host.hub
            .lock()
            .unwrap()
            .effects
            .iter()
            .filter_map(|effect| match effect {
                HostEffect::SwitchProfile(effect) => Some(effect.clone()),
                _ => None,
            })
            .collect()
    }
    let (_root, mut host, token) = fixture(true);
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    socket
        .send(Message::text(
            json!({"event":"registerPlugin","uuid":token}).to_string(),
        ))
        .unwrap();
    barrier(&mut socket, 0);
    request(&mut socket, "not-connected", "ignored");
    barrier(&mut socket, 1);
    assert!(pending(&host).is_empty());
    assert!(!host
        .hub
        .lock()
        .unwrap()
        .profile_switch_requests
        .contains_key(OWNER));

    request(&mut socket, "device", "first");
    barrier(&mut socket, 2);
    assert_eq!(pending(&host).len(), 1);
    assert_eq!(pending(&host)[0].session, token);
    assert_eq!(pending(&host)[0].plugin, OWNER);
    // A future fixture timestamp makes this a throttle-branch test, without
    // depending on delivery within a 250 ms scheduling window.
    host.hub.lock().unwrap().profile_switch_requests.insert(
        OWNER.into(),
        std::time::Instant::now() + Duration::from_secs(3600),
    );
    request(&mut socket, "device", "too-soon");
    barrier(&mut socket, 3);
    assert_eq!(pending(&host)[0].profile.as_deref(), Some("first"));

    host.hub
        .lock()
        .unwrap()
        .profile_switch_requests
        .remove(OWNER);
    request(&mut socket, "device", "replacement");
    barrier(&mut socket, 4);
    assert_eq!(pending(&host).len(), 1);
    assert_eq!(pending(&host)[0].profile.as_deref(), Some("replacement"));

    {
        let mut hub = host.hub.lock().unwrap();
        hub.effects.clear();
        hub.profile_switch_requests.clear();
        for index in 0..16 {
            hub.effects
                .push_back(HostEffect::SwitchProfile(ProfileSwitchEffect {
                    plugin: format!("other-{index}"),
                    session: "fixture".into(),
                    device: format!("device-{index}"),
                    profile: Some("profile".into()),
                    page: None,
                }));
        }
    }
    request(&mut socket, "device", "over-capacity");
    barrier(&mut socket, 5);
    assert_eq!(pending(&host).len(), 16);
    assert!(pending(&host).iter().all(|effect| effect.plugin != OWNER));
    let _ = socket.close(None);
    host.shutdown();
}
