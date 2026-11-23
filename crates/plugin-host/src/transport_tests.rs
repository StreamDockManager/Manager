use super::*;

#[test]
// Verify in flight responses after context retirement are discarded without disconnect
fn in_flight_responses_after_context_retirement_are_discarded_without_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let hub = Arc::new(Mutex::new(Hub::default()));
    {
        let mut state = hub.lock().unwrap();
        state
            .sessions
            .tokens
            .insert("token".into(), "fixture".into());
        state
            .context_state
            .contexts
            .insert("gone".into(), "fixture".into());
        state
            .context_state
            .contexts
            .insert("barrier".into(), "fixture".into());
    }
    let worker = hub.clone();
    let server = std::thread::spawn(move || {
        serve(
            listener.accept().unwrap().0,
            worker,
            Arc::new(AtomicBool::new(false)),
        );
    });
    let (mut client, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = client.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin","uuid":"token"}).to_string(),
        ))
        .unwrap();
    assert!(client.read().is_ok());
    // Reproduce the real lifecycle boundary: the host revokes ownership while
    // a preceding render/settings response is still in the socket pipeline.
    assert_eq!(
        hub.lock().unwrap().retire_context("gone").as_deref(),
        Some("fixture")
    );
    for event in ["setTitle", "setState", "setFeedback", "setSettings"] {
        client.send(tungstenite::Message::text(json!({"event":event,"context":"gone","payload":{"title":"late","state":1,"count":99}}).to_string())).unwrap();
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"getSettings","context":"barrier"}).to_string(),
        ))
        .unwrap();
    loop {
        let message = client.read().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["event"] == "didReceiveSettings" && value["context"] == "barrier" {
            break;
        }
    }
    {
        let mut state = hub.lock().unwrap();
        assert!(!state.context_state.settings.contains_key("gone"));
        assert!(!state.visual_state.visuals.contains_key("gone"));
        assert!(state.effects.is_empty());
        assert!(state
            .health
            .get("fixture")
            .is_none_or(|health| health.session.issues == 0));
        // Reappearance restores ordinary validation and application
        state
            .context_state
            .contexts
            .insert("gone".into(), "fixture".into());
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"setSettings","context":"gone","payload":{"count":7}}).to_string(),
        ))
        .unwrap();
    client
        .send(tungstenite::Message::text(
            json!({"event":"getSettings","context":"gone","id":"reappearance-barrier"}).to_string(),
        ))
        .unwrap();
    loop {
        let message = client.read().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["event"] == "didReceiveSettings" && value["context"] == "gone" {
            assert_eq!(value["id"], "reappearance-barrier");
            assert_eq!(value["payload"]["settings"]["count"], 7);
            break;
        }
    }
    // A retired name cannot bypass a different current owner's authorization
    hub.lock()
        .unwrap()
        .context_state
        .contexts
        .insert("gone".into(), "other".into());
    client
        .send(tungstenite::Message::text(
            json!({"event":"setSettings","context":"gone","payload":{"count":99}}).to_string(),
        ))
        .unwrap();
    assert!(matches!(
        client.read(),
        Ok(tungstenite::Message::Close(_)) | Err(_)
    ));
    server.join().unwrap();
    let mut state = hub.lock().unwrap();
    assert_eq!(state.context_state.settings["gone"]["count"], 7);
    assert!(!state
        .sessions
        .retired_contexts
        .contains("token", "gone", std::time::Instant::now()));
}

#[test]
// Verify retired context history never authorizes unknown or replacement sessions
fn retired_context_history_never_authorizes_unknown_or_replacement_sessions() {
    let mut hub = Hub::default();
    hub.sessions.tokens.insert("old".into(), "fixture".into());
    hub.sessions.connected.insert("old".into());
    // Browser plugin tokens also own file grants; only inspector grants have
    // a context and must be excluded from plugin retirement history.
    hub.sessions.files.insert(
        "old".into(),
        FileGrant {
            root: PathBuf::new(),
            runtime_entry: None,
            bootstrap: String::new(),
            context: None,
            setup_runtime: false,
        },
    );
    hub.sessions
        .tokens
        .insert("inspector".into(), "fixture".into());
    hub.sessions.connected.insert("inspector".into());
    hub.sessions.files.insert(
        "inspector".into(),
        FileGrant {
            root: PathBuf::new(),
            runtime_entry: None,
            bootstrap: String::new(),
            context: Some("known".into()),
            setup_runtime: false,
        },
    );
    hub.context_state
        .contexts
        .insert("known".into(), "fixture".into());
    hub.retire_context("known");
    let message = json!({"event":"setSettings","context":"known","payload":{}});
    assert!(hub.discard_retired_response("old", &message));
    assert!(!hub.discard_retired_response("inspector", &message));
    assert!(!hub.discard_retired_response("new", &message));
    assert!(!hub.discard_retired_response("old", &json!({"context":"unknown"})));
    hub.context_state
        .contexts
        .insert("known".into(), "other".into());
    assert!(!hub.discard_retired_response("old", &message));
    assert!(validate_message(&message, "fixture", &hub.context_state.contexts).is_err());
}

#[test]
// Verify outgoing admission charges selected messages and reclaims every cycle
fn outgoing_admission_charges_selected_messages_and_reclaims_every_cycle() {
    for repetition in 0..5 {
        let mut queue = transport::Outgoing::default();
        let payload = json!({"event":"sendToPlugin", "payload":"é".repeat(250_000)});
        let bytes = payload.to_string().len();
        let admitted = transport::OUTGOING_BYTES / bytes;
        for _ in 0..admitted {
            queue.push_batch(vec![payload.clone()]).unwrap();
        }
        let before = queue.retained_bytes();
        let selected = queue.take_batch().unwrap();
        assert!(!selected.is_empty());
        assert!(selected.len() <= 32);
        assert_eq!(queue.retained_bytes(), before);
        assert_eq!(queue.retained_messages(), admitted);
        assert!(queue.push_batch(vec![payload.clone()]).is_err());
        // Clearing pending work cannot release buffers owned by the sender
        queue.clear();
        assert_eq!(queue.retained_bytes(), selected.len() * bytes);
        drop(selected);
        assert_eq!(queue.retained_bytes(), 0);
        assert_eq!(queue.retained_messages(), 0);
        queue.push_batch(vec![payload]).unwrap();
        let selected = queue.take_batch().unwrap();
        drop(queue); // Selected permits can outlive a retired queue safely.
        drop(selected);
        eprintln!(
            "OUTGOING_BUDGET repetition={repetition} peak_utf8_bytes={before} retired_bytes=0"
        );
    }
}

#[test]
// Verify outgoing command groups are atomic and nonreturning producers fail explicitly
fn outgoing_command_groups_are_atomic_and_nonreturning_producers_fail_explicitly() {
    let mut queue = transport::Outgoing::default();
    for _ in 0..transport::OUTGOING_MESSAGES - 1 {
        queue.push_back(json!({"event":"keyDown"}));
    }
    let before = queue.retained_bytes();
    assert!(queue
        .push_batch(vec![json!({"event":"keyDown"}), json!({"event":"keyUp"})])
        .is_err());
    assert_eq!(queue.len(), transport::OUTGOING_MESSAGES - 1);
    assert_eq!(queue.retained_bytes(), before);
    queue.push_back(json!({"event":"willDisappear"}));
    assert_eq!(queue.len(), transport::OUTGOING_MESSAGES);
    queue.push_back(json!({"event":"didReceiveSettings"}));
    assert!(queue.take_batch().is_err());
    assert_eq!(queue.retained_bytes(), 0);
    assert_eq!(queue.len(), 0);
}

#[test]
// Verify outgoing utf8 envelope limit and retained message limit cover inflight work
fn outgoing_utf8_envelope_limit_and_retained_message_limit_cover_inflight_work() {
    let mut queue = transport::Outgoing::default();
    assert!(queue
        .push_batch(vec![json!({"payload":"é".repeat(MAX_MESSAGE_BYTES / 2)})])
        .is_err());
    assert_eq!(queue.retained_bytes(), 0);
    for _ in 0..transport::OUTGOING_MESSAGES {
        queue.push_back(json!({"event":"keyUp"}));
    }
    let selected = queue.take_batch().unwrap();
    assert_eq!(selected.len(), 32);
    assert!(queue.push_batch(vec![json!({"event":"keyDown"})]).is_err());
    let before = queue.retained_bytes();
    queue.retain(|_| false);
    assert!(queue.retained_bytes() < before);
    assert_eq!(queue.retained_messages(), selected.len());
    drop(selected);
    assert_eq!(queue.retained_bytes(), 0);
}

#[test]
// Verify overflow disconnects and releases owner instead of silently losing lifecycle
fn overflow_disconnects_and_releases_owner_instead_of_silently_losing_lifecycle() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let hub = Arc::new(Mutex::new(Hub::default()));
    hub.lock()
        .unwrap()
        .sessions
        .tokens
        .insert("token".into(), "fixture".into());
    let worker = hub.clone();
    let server = std::thread::spawn(move || {
        serve(
            listener.accept().unwrap().0,
            worker,
            Arc::new(AtomicBool::new(false)),
        );
    });
    let (mut client, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = client.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin","uuid":"token"}).to_string(),
        ))
        .unwrap();
    assert!(client.read().is_ok());
    {
        let mut state = hub.lock().unwrap();
        let queue = state.sessions.queues.get_mut("fixture").unwrap();
        for _ in 0..transport::OUTGOING_MESSAGES + 1 {
            queue.push_back(json!({"event":"willDisappear", "context":"fixture:context"}));
        }
    }
    assert!(matches!(
        client.read(),
        Ok(tungstenite::Message::Close(_)) | Err(_)
    ));
    server.join().unwrap();
    let state = hub.lock().unwrap();
    assert!(!state.sessions.connected.contains("token"));
    assert!(!state.sessions.queues.contains_key("fixture"));
    assert!(state
        .effects
        .iter()
        .any(|event| matches!(event, HostEffect::DesktopInput(effect) if effect.payload["op"] == "releaseOwner")));
}

#[test]
// Verify accepted nonblocking socket waits for delayed handshake
fn accepted_nonblocking_socket_waits_for_delayed_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let stream = listener.accept().unwrap().0;
    stream.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        serve(
            stream,
            Arc::new(Mutex::new(Hub::default())),
            Arc::new(AtomicBool::new(false)),
        );
    });
    std::thread::sleep(Duration::from_millis(30));
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (mut socket, _) = tungstenite::client("ws://localhost", peer).unwrap();
    socket.close(None).unwrap();
    server.join().unwrap();
}

#[test]
// Verify trickled WebSocket headers cannot hold admission past the handshake deadline
fn trickled_websocket_headers_cannot_hold_admission_past_the_handshake_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let admission = Arc::new(server_admission::Admission::default());
    let server = spawn_connection(
        listener.accept().unwrap().0,
        Arc::new(Mutex::new(Hub::default())),
        Arc::new(AtomicBool::new(false)),
        admission.clone(),
        None,
    )
    .unwrap();
    peer.write_all(b"GET / HTTP/1.1\r\nX-Slow: ").unwrap();
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(5500) && !server.is_finished() {
        if peer.write_all(b"a").is_err() {
            break;
        }
        // Stay below Tungstenite's separate 64-small-packet attack heuristic
        std::thread::sleep(Duration::from_millis(200));
    }
    let retired = server.is_finished();
    drop(peer);
    server.join().unwrap();
    assert!(retired, "trickled reads renewed the handshake deadline");
    let permits: Vec<_> = (0..server_admission::HANDSHAKES)
        .map(|_| admission.handshakes.acquire().unwrap())
        .collect();
    assert_eq!(permits.len(), server_admission::HANDSHAKES);
}

#[test]
// Verify quiet peer receives queued messages in order without sending input
fn quiet_peer_receives_queued_messages_in_order_without_sending_input() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let hub = Arc::new(Mutex::new(Hub::default()));
    hub.lock()
        .unwrap()
        .sessions
        .tokens
        .insert("token".into(), "fixture".into());
    let stop = Arc::new(AtomicBool::new(false));
    let server_hub = hub.clone();
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        serve(listener.accept().unwrap().0, server_hub, server_stop);
    });
    let (mut client, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = client.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin","uuid":"token"}).to_string(),
        ))
        .unwrap();
    let initial: Value = serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(initial["event"], "didReceiveGlobalSettings");
    let mut latencies = vec![];
    for sequence in 0..5 {
        // Let a quiet connection enter its blocking read/wait
        std::thread::sleep(Duration::from_millis(20));
        let start = std::time::Instant::now();
        hub.lock()
            .unwrap()
            .sessions
            .queues
            .entry("fixture".into())
            .or_default()
            .push_back(json!({"event":"keyDown","sequence":sequence}));
        let received: Value =
            serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap();
        latencies.push(start.elapsed().as_micros());
        assert_eq!(received["sequence"], sequence);
    }
    eprintln!("QUIET_PEER_QUEUE_LATENCY_US={latencies:?}");
    // One producer wake must drain several count/byte-bounded batches even
    // when the peer sends nothing else to wake the host between those batches.
    {
        let mut state = hub.lock().unwrap();
        let queue = state.sessions.queues.get_mut("fixture").unwrap();
        for sequence in 100..196 {
            queue.push_back(
                json!({"event":"keyDown","sequence":sequence,"payload":"x".repeat(40 * 1024)}),
            );
        }
    }
    for sequence in 100..196 {
        let received: Value =
            serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(received["sequence"], sequence);
    }
    {
        let mut state = hub.lock().unwrap();
        state.sessions.tokens.remove("token");
        state.sessions.queues.remove("fixture");
    }
    assert!(
        matches!(client.read(), Ok(tungstenite::Message::Close(_)) | Err(_)),
        "revocation must wake a quiet peer"
    );
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
}

#[test]
// Verify WebSocket budget counts utf8 message and fragment bytes
fn websocket_budget_counts_utf8_message_and_fragment_bytes() {
    use tungstenite::protocol::{
        frame::{
            coding::{Data, OpCode},
            Frame,
        },
        Role, WebSocketConfig,
    };
    // Use an in-memory unmasked server stream to exercise aggregate fragment
    // enforcement separately from each individual frame's size.
    for size in [
        MAX_MESSAGE_BYTES - 1,
        MAX_MESSAGE_BYTES,
        MAX_MESSAGE_BYTES + 1,
    ] {
        let mut bytes = vec![];
        let text = format!(
            "{}{}",
            "é".repeat(size / 2),
            if size % 2 == 1 { "x" } else { "" }
        );
        let split = (size / 4) * 2;
        Frame::message(
            text.as_bytes()[..split].to_vec(),
            OpCode::Data(Data::Text),
            false,
        )
        .format(&mut bytes)
        .unwrap();
        Frame::message(
            text.as_bytes()[split..].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )
        .format(&mut bytes)
        .unwrap();
        let mut socket = tungstenite::WebSocket::from_raw_socket(
            std::io::Cursor::new(bytes),
            Role::Client,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(MAX_MESSAGE_BYTES))
                    .max_frame_size(Some(MAX_MESSAGE_BYTES)),
            ),
        );
        assert_eq!(socket.read().is_ok(), size <= MAX_MESSAGE_BYTES, "{size}");
        // Independently exercise the frame cap with a larger aggregate budget
        let mut single = vec![];
        Frame::message(text.into_bytes(), OpCode::Data(Data::Text), true)
            .format(&mut single)
            .unwrap();
        let mut socket = tungstenite::WebSocket::from_raw_socket(
            std::io::Cursor::new(single),
            Role::Client,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(2 * MAX_MESSAGE_BYTES))
                    .max_frame_size(Some(MAX_MESSAGE_BYTES)),
            ),
        );
        assert_eq!(
            socket.read().is_ok(),
            size <= MAX_MESSAGE_BYTES,
            "single frame {size}"
        );
    }
}

#[test]
// Verify slow reader receives each accepted message once in order
fn slow_reader_receives_each_accepted_message_once_in_order() {
    use tungstenite::protocol::Role;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let stream = listener.accept().unwrap().0;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let size: libc::c_int = 4096;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &size as *const _ as *const _,
                    std::mem::size_of_val(&size) as _,
                )
            },
            0
        );
    }
    let (received, complete) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let ws = tungstenite::WebSocket::from_raw_socket(stream, Role::Server, None);
        let mut socket = transport::Socket::new(ws).unwrap();
        for index in 0..12 {
            socket
                .send(tungstenite::Message::text(format!(
                    "{index}:{}",
                    "x".repeat(256 * 1024)
                )))
                .unwrap();
        }
        // send() drains the websocket into the kernel, not into the peer application.
        // Keep this connection alive until the slow reader has checked every frame;
        // closing it early can reset unread buffered data on Windows.
        complete.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut peer = tungstenite::WebSocket::from_raw_socket(peer, Role::Client, None);
    for index in 0..12 {
        std::thread::sleep(Duration::from_millis(10));
        let message = peer.read().unwrap();
        let (sequence, body) = message.to_text().unwrap().split_once(':').unwrap();
        assert_eq!(sequence, index.to_string());
        assert_eq!(body.len(), 256 * 1024);
        assert!(body.bytes().all(|byte| byte == b'x'));
    }
    received.send(()).unwrap();
    server.join().unwrap();
}

#[test]
// Verify outgoing queue removal wakes its waiter
fn outgoing_queue_removal_wakes_its_waiter() {
    let mut poll = mio::Poll::new().unwrap();
    let wake = Arc::new(mio::Waker::new(poll.registry(), mio::Token(12)).unwrap());
    let mut queue = transport::Outgoing::default();
    // The socket owns its Waker while the queue holds another reference. Keep
    // that owner alive: closing the final Linux eventfd removes its readiness.
    queue.wake = Some(wake.clone());
    drop(queue);
    let mut events = mio::Events::with_capacity(1);
    poll.poll(&mut events, Some(Duration::from_millis(100)))
        .unwrap();
    assert!(events.iter().any(|event| event.token() == mio::Token(12)));
}

#[test]
// Verify rejected duplicate registration preserves live session
fn rejected_duplicate_registration_preserves_live_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let hub = Arc::new(Mutex::new(Hub::default()));
    hub.lock()
        .unwrap()
        .sessions
        .tokens
        .insert("token".into(), "fixture".into());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_hub = hub.clone();
    let worker_stop = stop.clone();
    let server = std::thread::spawn(move || {
        let first_hub = worker_hub.clone();
        let first_stop = worker_stop.clone();
        let first_stream = listener.accept().unwrap().0;
        let first = std::thread::spawn(move || serve(first_stream, first_hub, first_stop));
        serve(listener.accept().unwrap().0, worker_hub, worker_stop);
        first
    });
    let registration = json!({"event":"registerPlugin","uuid":"token"}).to_string();
    let (mut live, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = live.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }
    live.send(tungstenite::Message::text(registration.clone()))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(live.read().unwrap().to_text().unwrap()).unwrap()["event"],
        "didReceiveGlobalSettings"
    );
    let (mut duplicate, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = duplicate.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }
    duplicate
        .send(tungstenite::Message::text(registration))
        .unwrap();
    assert!(matches!(
        duplicate.read(),
        Ok(tungstenite::Message::Close(_)) | Err(_)
    ));
    let first = server.join().unwrap();
    let connected = hub.lock().unwrap().sessions.connected.contains("token");
    hub.lock()
        .unwrap()
        .sessions
        .queues
        .get_mut("fixture")
        .unwrap()
        .push_back(json!({"event":"keyDown","sequence":9}));
    let received: Value = serde_json::from_str(live.read().unwrap().to_text().unwrap()).unwrap();
    live.close(None).unwrap();
    first.join().unwrap();
    assert!(
        connected,
        "a rejected duplicate must not retire the live session"
    );
    assert_eq!(received["sequence"], 9);
}

#[test]
// Verify retired registration cannot claim replacement queue wakeup
fn retired_registration_cannot_claim_replacement_queue_wakeup() {
    let old_poll = mio::Poll::new().unwrap();
    let mut replacement_poll = mio::Poll::new().unwrap();
    let old_wake = Arc::new(mio::Waker::new(old_poll.registry(), mio::Token(1)).unwrap());
    let replacement_wake =
        Arc::new(mio::Waker::new(replacement_poll.registry(), mio::Token(2)).unwrap());
    let hub = Arc::new(Mutex::new(Hub::default()));
    let mut state = hub.lock().unwrap();
    state.sessions.tokens.insert("old".into(), "fixture".into());
    let worker_hub = hub.clone();
    let (started, waiting) = std::sync::mpsc::channel();
    let old = std::thread::spawn(move || {
        started.send(()).unwrap();
        transport::register(
            &mut worker_hub.lock().unwrap(),
            &json!({"event":"registerPlugin","uuid":"old"}),
            old_wake,
        )
    });
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    // Hold registration behind the same lock used by restart/token retirement
    state.sessions.tokens.remove("old");
    state.sessions.queues.remove("fixture");
    state
        .sessions
        .tokens
        .insert("replacement".into(), "fixture".into());
    let accepted = transport::register(
        &mut state,
        &json!({"event":"registerPlugin","uuid":"replacement"}),
        replacement_wake.clone(),
    )
    .unwrap();
    assert_eq!(accepted.token, "replacement");
    let initial = state.sessions.queues["fixture"].len();
    drop(state);
    assert!(old.join().unwrap().is_none());
    let mut state = hub.lock().unwrap();
    assert_eq!(
        state.sessions.queues["fixture"].len(),
        initial,
        "retired registration must not enqueue initial messages"
    );
    assert!(state.sessions.connected.contains("replacement"));
    assert!(!state.sessions.connected.contains("old"));
    assert!(Arc::ptr_eq(
        state.sessions.queues["fixture"].wake.as_ref().unwrap(),
        &replacement_wake
    ));
    let mut events = mio::Events::with_capacity(1);
    replacement_poll
        .poll(&mut events, Some(Duration::ZERO))
        .unwrap();
    state
        .sessions
        .queues
        .get_mut("fixture")
        .unwrap()
        .push_back(json!({"event":"keyDown"}));
    replacement_poll
        .poll(&mut events, Some(Duration::from_millis(100)))
        .unwrap();
    assert!(events.iter().any(|event| event.token() == mio::Token(2)));
}

#[test]
// Verify duplicate inspector registration keeps original queue owner
fn duplicate_inspector_registration_keeps_original_queue_owner() {
    let live_poll = mio::Poll::new().unwrap();
    let duplicate_poll = mio::Poll::new().unwrap();
    let live_wake = Arc::new(mio::Waker::new(live_poll.registry(), mio::Token(1)).unwrap());
    let duplicate_wake =
        Arc::new(mio::Waker::new(duplicate_poll.registry(), mio::Token(2)).unwrap());
    let mut hub = Hub::default();
    hub.sessions.tokens.insert("panel".into(), "fixture".into());
    hub.sessions.files.insert(
        "panel".into(),
        FileGrant {
            root: PathBuf::new(),
            runtime_entry: None,
            bootstrap: String::new(),
            context: Some("context".into()),
            setup_runtime: false,
        },
    );
    let message = json!({"event":"registerPropertyInspector","uuid":"panel"});
    assert!(transport::register(&mut hub, &message, live_wake.clone()).is_some());
    let initial = hub.sessions.queues["pi:panel"].len();
    assert!(transport::register(&mut hub, &message, duplicate_wake).is_none());
    assert!(hub.configuration_sessions.contains("panel"));
    assert_eq!(hub.sessions.queues["pi:panel"].len(), initial);
    assert!(Arc::ptr_eq(
        hub.sessions.queues["pi:panel"].wake.as_ref().unwrap(),
        &live_wake
    ));
}

#[test]
// Verify connection flood is admitted before thread creation and retires
fn connection_flood_is_admitted_before_thread_creation_and_retires() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let admission = Arc::new(server_admission::Admission::default());
    let hub = Arc::new(Mutex::new(Hub::default()));
    let stop = Arc::new(AtomicBool::new(false));
    for repetition in 0..5 {
        let mut peers = Vec::new();
        let mut threads = Vec::new();
        for _ in 0..server_admission::HANDSHAKES * 2 {
            peers.push(TcpStream::connect(address).unwrap());
            if let Some(thread) = spawn_connection(
                listener.accept().unwrap().0,
                hub.clone(),
                stop.clone(),
                admission.clone(),
                None,
            ) {
                threads.push(thread);
            }
        }
        assert_eq!(threads.len(), server_admission::HANDSHAKES);
        assert!(admission.handshakes.acquire().is_none());
        drop(peers);
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(admission.handshakes.acquire().is_some());
        eprintln!("SERVER_FLOOD repetition={repetition} attempted=64 admitted_threads=32 retired_threads=32");
    }
}

#[test]
// Verify saturated file transfers do not consume registered session capacity
fn saturated_file_transfers_do_not_consume_registered_session_capacity() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let admission = Arc::new(server_admission::Admission::default());
    let _files: Vec<_> = (0..server_admission::FILE_TRANSFERS)
        .map(|_| admission.files.acquire().unwrap())
        .collect();
    let hub = Arc::new(Mutex::new(Hub::default()));
    hub.lock()
        .unwrap()
        .sessions
        .tokens
        .insert("fixture-token".into(), "fixture".into());
    let server_hub = hub.clone();
    let server_admission = admission.clone();
    let server = std::thread::spawn(move || {
        spawn_connection(
            listener.accept().unwrap().0,
            server_hub,
            Arc::new(AtomicBool::new(false)),
            server_admission,
            None,
        )
        .unwrap()
        .join()
        .unwrap();
    });
    let (mut client, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    client
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin", "uuid":"fixture-token"}).to_string(),
        ))
        .unwrap();
    let reply: Value = serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(reply["event"], "didReceiveGlobalSettings");
    let pending: Vec<_> = (0..server_admission::HANDSHAKES)
        .map(|_| admission.handshakes.acquire().unwrap())
        .collect();
    assert!(admission.handshakes.acquire().is_none());
    hub.lock()
        .unwrap()
        .sessions
        .queues
        .get_mut("fixture")
        .unwrap()
        .push_back(json!({"event":"keyUp", "context":"fixture:button"}));
    let reply: Value = serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(reply["event"], "keyUp");
    client.close(None).unwrap();
    drop(client);
    drop(pending);
    server.join().unwrap();
}

#[test]
// Verify ordinary inbound policy has one context and inspector decision
fn ordinary_inbound_policy_has_one_context_and_inspector_decision() {
    let contexts = HashMap::from([("owned".into(), "plugin".into())]);
    let cases = [
        ("desktopInput", true, false),
        ("setTitle", true, false),
        ("setImage", true, false),
        ("setState", true, false),
        ("setFeedback", true, false),
        ("setFeedbackLayout", true, false),
        ("showAlert", true, false),
        ("showOk", true, false),
        ("sendToPropertyInspector", true, false),
        ("setSettings", true, true),
        ("getSettings", true, true),
        ("sendToPlugin", true, true),
        ("subscribeTelemetry", false, false),
        ("setDynamicItems", false, false),
        ("setGlobalSettings", false, true),
        ("getGlobalSettings", false, true),
        ("logMessage", false, true),
        ("openUrl", false, true),
    ];
    for (event, context_required, inspector_allowed) in cases {
        let policy = InboundPolicy::classify(event).unwrap();
        assert_eq!(policy.inspector_allowed(), inspector_allowed, "{event}");
        assert_eq!(policy.context_required(), context_required, "{event}");
        for context in [Value::Null, json!(7), json!("owned"), json!("spoofed")] {
            let result = validate_message(
                &json!({"event":event, "context":context, "payload":{"pluginDefined":[1,null]}}),
                "plugin",
                &contexts,
            );
            let expected = if !context_required || context == "owned" {
                Ok(())
            } else if context.as_str().is_none() {
                Err(InboundError::MissingContext)
            } else {
                Err(InboundError::UnownedContext)
            };
            assert_eq!(result, expected, "{event}: {context}");
        }
    }
    // These have their own authenticated admission and must never enter the
    // ordinary policy (nor gain inspector access from it).
    for event in [
        "actionResult",
        "streamDockManagerRpcRequest",
        "networkPayload",
        "unknown",
    ] {
        assert_eq!(InboundPolicy::classify(event), None);
        assert_eq!(
            validate_message(&json!({"event":event}), "plugin", &contexts),
            Err(InboundError::UnsupportedEvent)
        );
    }
    for message in [json!({}), json!({"event":3})] {
        assert_eq!(
            validate_message(&message, "plugin", &contexts),
            Err(InboundError::MissingEvent)
        );
    }
}

#[test]
// Verify inbound rejection dispositions and diagnostics are stable
fn inbound_rejection_dispositions_and_diagnostics_are_stable() {
    for (error, text, ordinary, retired) in [
        (
            InboundError::MissingEvent,
            "event missing",
            InboundDisposition::Disconnect,
            InboundDisposition::Disconnect,
        ),
        (
            InboundError::UnsupportedEvent,
            "unsupported plugin event",
            InboundDisposition::Reject,
            InboundDisposition::Reject,
        ),
        (
            InboundError::UnownedContext,
            "context is not owned by plugin",
            InboundDisposition::Disconnect,
            InboundDisposition::Ignore,
        ),
        (
            InboundError::MissingContext,
            "context required",
            InboundDisposition::Disconnect,
            InboundDisposition::Disconnect,
        ),
    ] {
        assert_eq!(error.to_string(), text);
        assert_eq!(error.disposition(false), ordinary);
        assert_eq!(error.disposition(true), retired);
    }
}

#[test]
// Verify unsupported event keeps connection but malformed context disconnects
fn unsupported_event_keeps_connection_but_malformed_context_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let hub = Arc::new(Mutex::new(Hub::default()));
    {
        let mut state = hub.lock().unwrap();
        state
            .sessions
            .tokens
            .insert("token".into(), "fixture".into());
        state
            .context_state
            .contexts
            .insert("owned".into(), "fixture".into());
    }
    let worker = hub.clone();
    let server = std::thread::spawn(move || {
        serve(
            listener.accept().unwrap().0,
            worker,
            Arc::new(AtomicBool::new(false)),
        );
    });
    let (mut client, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = client.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    client
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin","uuid":"token"}).to_string(),
        ))
        .unwrap();
    assert!(client.read().is_ok());
    client
        .send(tungstenite::Message::text(
            json!({"event":"futureUnsupportedOperation"}).to_string(),
        ))
        .unwrap();
    client
        .send(tungstenite::Message::text(
            json!({"event":"getSettings","context":"owned"}).to_string(),
        ))
        .unwrap();
    loop {
        let message = client.read().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["event"] == "didReceiveSettings" {
            break;
        }
    }
    assert_eq!(
        hub.lock().unwrap().health["fixture"].unsupported_apis,
        ["futureUnsupportedOperation"]
    );
    client
        .send(tungstenite::Message::text(
            json!({"event":"getSettings","context":{}}).to_string(),
        ))
        .unwrap();
    assert!(matches!(
        client.read(),
        Ok(tungstenite::Message::Close(_)) | Err(_)
    ));
    server.join().unwrap();
    assert_eq!(
        hub.lock().unwrap().health["fixture"]
            .recent_failures
            .last()
            .map(String::as_str),
        Some("context required")
    );
}
