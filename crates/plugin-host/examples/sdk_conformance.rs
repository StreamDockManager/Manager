//! Disposable SDK extension probe. Desktop requests are acknowledged as data only;
//! no native input backend, user profile or physical device is invoked.
use hardware_core::Input;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stream_dock_manager_plugin_host::{ActiveAction, HostEffect, PluginHost};
mod support;
// Wait for the current operation
fn wait(host: &mut PluginHost, condition: impl Fn(&PluginHost) -> bool) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        host.poll();
        if condition(host) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("SDK conformance condition timed out".into())
}
// Read the title observed by the conformance host
fn title(host: &mut PluginHost, expected: &str) -> Result<(), String> {
    wait(host, |h| {
        h.surface("sdk-device", "key-a", "main")
            .is_some_and(|v| v["title"] == expected)
    })
    .map_err(|error| {
        format!(
            "{error}; expected {expected}; surface {:?}; plugins {:?}",
            host.surface("sdk-device", "key-a", "main"),
            host.list()
        )
    })
}

// Construct the fixture HTTP response test fixture
fn fixture_http_response(mut stream: TcpStream) -> std::io::Result<()> {
    // Winsock accept inherits the listener's nonblocking mode. This responder
    // reads a complete request synchronously; WouldBlock is not an HTTP EOF.
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let remaining = || {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Fixture HTTP request deadline exceeded",
            ))
        } else {
            Ok(timeout)
        }
    };
    let mut request = Vec::new();
    let mut byte = [0];
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        stream.set_read_timeout(Some(remaining()?))?;
        stream.read_exact(&mut byte)?;
        request.push(byte[0]);
    }
    if !request.ends_with(b"\r\n\r\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Fixture HTTP headers exceed 8192 bytes",
        ));
    }
    let body = r#"{"ok":true,"token":"fixture-private","echo":"cookie-private"}"#;
    let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: session=cookie-private; Secure\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(response.as_bytes())
}

// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let archive = std::env::args()
        .nth(1)
        .ok_or("Pass a test-only SDK probe ZIP")?;
    let temporary = tempfile::tempdir()?;
    let mut host = support::protocol_host(temporary.path())?;
    // Establish a real light -> dark transition below, independent of the OS
    // theme; unchanged resolved themes intentionally do not emit notifications.
    host.set_appearance(None, "light", None)?;
    let plugin = host.install(std::path::Path::new(&archive))?;
    let id = plugin.id.clone();
    let action = plugin.actions[0].id.clone();
    let capture = host.logs.start(&id, &plugin.name)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let endpoint = format!("http://{}/fixture?q=private-query", listener.local_addr()?);
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
            if let Ok((stream, _)) = listener.accept() {
                if let Err(error) = fixture_http_response(stream) {
                    eprintln!("SDK fixture HTTP response failed: {error}");
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    assert!(!plugin.enabled);
    support::runtime(&host, &id, false)?;
    // This probe registers on profile demand before its action first appears.
    host.set_sleep_when_hidden(&id, false)?;
    host.enable(&id, true)?;
    let launch_started = Instant::now();
    support::demand(&mut host, &id, "sdk-device");
    eprintln!("{id}: launch preparation {:?}", launch_started.elapsed());
    wait(&mut host, |h| h.list()[0].status == "Running").map_err(|error| {
        format!(
            "{error}; registration; plugins {:?}; effects {:?}",
            host.list(),
            host.drain_effects()
        )
    })?;
    eprintln!("{id}: registration {:?}", launch_started.elapsed());
    let settings =
        json!({"opaque":{"retain":true},"api_key":"fixture-private","fixture_endpoint":endpoint});
    let active = |settings| ActiveAction {
        plugin_id: id.clone(),
        action_id: action.clone(),
        device_id: "sdk-device".into(),
        control_id: "key-a".into(),
        scope: "main".into(),
        settings,
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    };
    let devices = vec![json!({"id":"sdk-device","name":"Synthetic SDK device","type":7})];
    host.synchronize(vec![active(settings.clone())], devices.clone());
    title(&mut host, "global:saved")?;
    for (phase, (enabled, expected)) in [(false, 0), (true, 1), (false, 1), (true, 2)]
        .into_iter()
        .enumerate()
    {
        let mut preferences = host.logs.preferences();
        preferences
            .plugins
            .entry(id.clone())
            .or_default()
            .debug_network = enabled;
        host.logs.save_preferences(preferences)?;
        host.refresh_log_configuration();
        let receipt = host.dispatch_tracked(
            &id,
            &action,
            "sdk-device",
            "key-a",
            "main",
            &Input::Button { pressed: true },
            &settings,
        )?;
        receipt.wait(Duration::from_secs(5), || Ok(()))?;
        let output = temporary.path().join("network-capture.jsonl");
        host.logs.export(&capture.id, &output)?;
        let until = Instant::now() + Duration::from_secs(5);
        let (text, values) = loop {
            host.logs.export(&capture.id, &output)?;
            let text = std::fs::read_to_string(&output)?;
            let values = text
                .lines()
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<Result<Vec<_>, _>>()?;
            // Action receipts have priority over the independent log queue
            // This FIFO marker proves the preceding trace reached the host
            let barriers = values
                .iter()
                .filter(|value| value["message"] == "SDK network request finished")
                .count();
            if barriers > phase {
                break (text, values);
            }
            if Instant::now() >= until {
                return Err(format!("{id}: logging delivery marker missing").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            values
                .iter()
                .filter(|value| value["source"] == "network")
                .count(),
            expected,
            "{id}: live network toggle {enabled}"
        );
        for secret in ["fixture-private", "cookie-private", "private-query"] {
            assert!(
                !text.contains(secret),
                "{id}: captured fixture credential {secret}"
            );
        }
        assert!(values.iter().any(|value| value["source"] == "rpc"
            && value["stream"] == "stdout"
            && value["message"]
                .as_str()
                .is_some_and(|text| text.contains("SDK stdout"))));
        assert!(values.iter().any(|value| value["source"] == "rpc"
            && value["stream"] == "stderr"
            && value["message"]
                .as_str()
                .is_some_and(|text| text.contains("SDK stderr"))));
    }
    assert_eq!(
        host.effective_settings(&id, "main", "sdk-device", "key-a", &json!({}))["opaque"],
        settings["opaque"]
    );
    wait(&mut host, |h| h.telemetry_topics().contains("cpu"))
        .map_err(|error| format!("{error}; telemetry subscription; plugins {:?}", host.list()))?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    // Node's Windows wall clock is anchored to GetSystemTimeAsFileTime, while
    // Rust uses GetSystemTimePreciseAsFileTime. Give this synthetic observation
    // a real age margin so clock quantization cannot make fresh input future.
    // Keep the SDK's future/stale rejection and the five-second lifetime intact
    let observed_at = now.checked_sub(100).ok_or("Fixture clock predates epoch")?;
    host.publish_telemetry(
        json!({"audio_observer":false,"cpu":{"value":42,"observed_at":observed_at,"max_age_ms":5000,"error":null}}),
    );
    title(&mut host, "telemetry:42")?;
    host.set_appearance(Some(&id), "dark", None)?;
    title(&mut host, "theme:dark")?;
    host.dispatch(
        &id,
        &action,
        "sdk-device",
        "key-a",
        "main",
        &Input::Rotate { delta: -3 },
        &settings,
    )?;
    title(&mut host, "dial:-3")?;
    host.dispatch(
        &id,
        &action,
        "sdk-device",
        "key-a",
        "main",
        &Input::Touch {
            phase: "tap".into(),
            x: 4,
            y: 5,
        },
        &settings,
    )?;
    title(&mut host, "touch")?;
    // The fixed v1 1 MiB UTF-8 envelope cap also applies without negotiation
    // Legal boundary messages must leave the connection usable; one byte over
    // must be rejected locally before reaching the host.
    // Existing contexts use saved settings, so update the active profile rather
    // than relying on dispatch's fallback settings to replace persisted values.
    let boundary_settings = json!({"fixture_message_limit":true});
    host.synchronize(vec![active(boundary_settings.clone())], devices.clone());
    host.dispatch(
        &id,
        &action,
        "sdk-device",
        "key-a",
        "main",
        &Input::Button { pressed: true },
        &boundary_settings,
    )?;
    title(&mut host, "message-limit:ok")?;
    host.synchronize(vec![active(settings.clone())], devices);
    for failed in [false, true] {
        host.drain_effects();
        let receipt = host.dispatch_tracked(
            &id,
            &action,
            "sdk-device",
            "key-a",
            "main",
            &Input::Button { pressed: false },
            &settings,
        )?;
        let until = Instant::now() + Duration::from_secs(5);
        let request = loop {
            host.poll();
            if let Some(event) = host
                .drain_effects()
                .into_iter()
                .find_map(|effect| match effect {
                    HostEffect::DesktopInput(input) => Some(input),
                    _ => None,
                })
            {
                break event;
            }
            if Instant::now() >= until {
                return Err("desktop input request missing".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            receipt.wait(Duration::from_millis(30), || Ok(())).is_err(),
            "completed before desktop acknowledgement"
        );
        host.desktop_input_result(
            &id,
            &request.context,
            json!({"request_id":request.payload["request_id"],"error":failed}),
        );
        assert_eq!(
            receipt.wait(Duration::from_secs(5), || Ok(())).is_err(),
            failed
        );
    }
    host.enable(&id, false)?;
    host.logs.stop(&capture.id)?;
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    println!("{id}: settings, globals, telemetry, theme, dial, touch, UTF-8 message boundaries, deferred desktop success/failure, typed capture logging, live network toggles/redaction/export and teardown passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::Shutdown;

    // Construct the conformance test connection
    fn connection() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    // Read response
    fn read_response(client: &mut TcpStream) -> String {
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    }

    // Assert waiting for headers
    fn assert_waiting_for_headers(client: &mut TcpStream) {
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let error = client.peek(&mut [0]).unwrap_err();
        assert!(matches!(
            error.kind(),
            ErrorKind::WouldBlock | ErrorKind::TimedOut
        ));
    }

    #[test]
    // Verify fixture HTTP accepts immediate complete headers
    fn fixture_http_accepts_immediate_complete_headers() {
        let (mut client, server) = connection();
        let worker = std::thread::spawn(move || fixture_http_response(server));
        client
            .write_all(b"GET /fixture HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let response = read_response(&mut client);
        worker.join().unwrap().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(
            response.ends_with(r#"{"ok":true,"token":"fixture-private","echo":"cookie-private"}"#)
        );
    }

    #[test]
    // Verify fixture HTTP waits for delayed fragmented headers
    fn fixture_http_waits_for_delayed_fragmented_headers() {
        let (mut client, server) = connection();
        let worker = std::thread::spawn(move || fixture_http_response(server));
        // A connected client is allowed to send its headers later. The server
        // must neither reply nor close before the terminating empty line.
        assert_waiting_for_headers(&mut client);
        client
            .write_all(b"GET /fixture HTTP/1.1\r\nHost: localhost\r\n")
            .unwrap();
        assert_waiting_for_headers(&mut client);
        client.write_all(b"\r\n").unwrap();
        assert!(read_response(&mut client).starts_with("HTTP/1.1 200 OK\r\n"));
        worker.join().unwrap().unwrap();
    }

    #[test]
    // Verify fixture HTTP rejects incomplete headers without a response
    fn fixture_http_rejects_incomplete_headers_without_a_response() {
        let (mut client, server) = connection();
        let worker = std::thread::spawn(move || fixture_http_response(server));
        client.write_all(b"GET /fixture HTTP/1.1\r\n").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        assert!(read_response(&mut client).is_empty());
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    // Verify fixture HTTP rejects headers over the existing limit
    fn fixture_http_rejects_headers_over_the_existing_limit() {
        let (mut client, server) = connection();
        let worker = std::thread::spawn(move || fixture_http_response(server));
        client.write_all(&[b'x'; 8192]).unwrap();
        assert!(read_response(&mut client).is_empty());
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
