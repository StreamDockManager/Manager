use super::*;
// Map the host platform to the SDK platform identifier
pub(super) fn sdk_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        std::env::consts::OS
    }
}
/// Complete vendor registration shape. The compatibility version remains the
/// tested subset; it must not be raised merely to bypass a plugin's minimum.
pub(super) fn registration_info(
    hub: &Hub,
    id: &str,
    version: &str,
    dialect: &str,
    inspector: bool,
    browser_context: bool,
) -> Result<Value, String> {
    let contract =
        connection_contract::vendor(dialect).ok_or("Unsupported registration dialect")?;
    static PLATFORM_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    // Probe the operating-system version once rather than spawning a process for every registration
    let platform_version = PLATFORM_VERSION.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let probe =
            preflight::probe_controlled("/usr/bin/sw_vers", &["-productVersion"], &|| Ok(()));
        #[cfg(target_os = "windows")]
        let probe = preflight::probe_controlled("cmd.exe", &["/C", "ver"], &|| Ok(()));
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let probe = preflight::probe_controlled("uname", &["-r"], &|| Ok(()));
        probe.unwrap_or_default().trim().to_owned()
    });
    let mut devices: Vec<_> = hub.devices.values().cloned().collect();
    devices.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let theme = hub.theme_payload(id, inspector);
    let colors = &theme["palette"];
    let color = |key: &str, default: &str| colors[key].as_str().unwrap_or(default).to_owned();
    let mut info = json!({"application":{"platform":sdk_platform(),"platformVersion":platform_version,
        "version":contract.application_version,"font":if hub.ui_theme_style.font == "default" {"system-ui"} else {&hub.ui_theme_style.font}},
        "colors":{"buttonMouseOverBackgroundColor":color("surface-raised", "#333333"),
            "buttonPressedBackgroundColor":color("accent", "#4466cc"),
            "buttonPressedBorderColor":color("border", "#666666"),
            "buttonPressedTextColor":color("accent-ink", "#ffffff"),
            "highlightColor":color("accent", "#4466cc")},
        "devices":devices,"plugin":{"uuid":id,"version":version}});
    hub.registration_environment
        .apply(&mut info, browser_context)?;
    Ok(info)
}
// Build the isolated browser plugin bootstrap
pub(super) fn bootstrap(
    port: u16,
    token: &str,
    event: &str,
    info: &Value,
    action: &Value,
) -> String {
    let arguments = json!([
        port.to_string(),
        token,
        event,
        info.to_string(),
        action.to_string()
    ])
    .to_string()
    .replace('<', "\\u003c");
    let cancel = if event == "registerPropertyInspector" {
        let context = action["context"].to_string().replace('<', "\\u003c");
        // WebKit does not forward Escape from an iframe to its parent dialog
        // This carries no settings; the parent checks the frame, origin and token
        format!("window.addEventListener('keydown',event=>{{if(event.key==='Escape'&&!event.defaultPrevented&&!document.querySelector('dialog[open]')){{event.preventDefault();window.parent.postMessage({{event:'stream-dock-manager-cancel-settings',context:{context}}},'*');}}}});")
    } else {
        String::new()
    };
    let environment = include_str!("browser_environment.js")
        .trim()
        .trim_end_matches(';');
    format!("<script>{cancel}window.addEventListener('load',()=>{{const connect=window.connectOpenActionSocket||window.connectElgatoStreamDeckSocket||window.connectSocket;if(typeof connect==='function'){{const args={arguments};const info=({environment})(JSON.parse(args[3]),{{devicePixelRatio:window.devicePixelRatio}});args[3]=JSON.stringify(info);connect(...args);}}}},{{once:true}});</script>")
}
const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 32 * 1024;
enum Body {
    Html(Vec<u8>),
    Static(std::fs::File, u64),
}
impl Body {
    // Return the response body length in bytes
    fn len(&self) -> u64 {
        match self {
            Self::Html(bytes) => bytes.len() as u64,
            Self::Static(_, length) => *length,
        }
    }
}
// Read bounded HTML and append the host-owned bootstrap script
fn read_html(reader: impl Read, bootstrap: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err("file size limit".into());
    }
    bytes.extend_from_slice(bootstrap.as_bytes());
    Ok(bytes)
}
// Stream exactly the opened asset length without buffering the entire file
fn copy_static(reader: impl Read, mut writer: impl Write, length: u64) -> std::io::Result<()> {
    // Copy exactly the opened file's length. Growth cannot increase the body
    let mut reader = reader.take(length);
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    while reader.limit() > 0 {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "package asset changed",
            ));
        }
        writer.write_all(&buffer[..count])?;
    }
    Ok(())
}
// Serve file
pub(super) fn serve_file(stream: TcpStream, hub: Arc<Mutex<Hub>>) {
    serve_scoped_file(stream, hub, None);
}
// Serve inspector file
pub(super) fn serve_inspector_file(stream: TcpStream, hub: Arc<Mutex<Hub>>, token: &str) {
    serve_scoped_file(stream, hub, Some(token));
}
// Serve an asset only after validating its owner, path, grant and origin
fn serve_scoped_file(mut stream: TcpStream, hub: Arc<Mutex<Hub>>, scope: Option<&str>) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let result = (|| -> Result<(Body, &str), String> {
        let mut header = Vec::new();
        let mut buffer = [0; 1024];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !header.windows(4).any(|w| w == b"\r\n\r\n") {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or("HTTP request timed out")?;
            stream
                .set_read_timeout(Some(remaining.min(Duration::from_secs(2))))
                .map_err(|e| e.to_string())?;
            let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 || header.len() + count > 16384 {
                return Err("invalid HTTP request".into());
            }
            header.extend(&buffer[..count]);
        }
        let headers = std::str::from_utf8(&header).map_err(|e| e.to_string())?;
        let request = headers.lines().next().ok_or("empty request")?;
        let target = request
            .strip_prefix("GET ")
            .and_then(|s| s.split(' ').next())
            .ok_or("GET required")?;
        let target = target.split('?').next().unwrap_or(target);
        let mut parts = target
            .strip_prefix("/files/")
            .ok_or("Invalid file route")?
            .splitn(2, '/');
        let token = parts.next().ok_or("token missing")?;
        let relative = decode_path(parts.next().ok_or("path missing")?)?;
        let grant = {
            let hub = hub.lock().unwrap();
            if !hub.sessions.tokens.contains_key(token) {
                return Err("token revoked".into());
            }
            let port = stream.local_addr().map_err(|e| e.to_string())?.port();
            validate_file_origin(
                scope,
                token,
                hub.sessions
                    .inspector_origins
                    .get(token)
                    .map(|(port, _)| *port),
                port,
                headers,
            )?;
            if hub
                .sessions
                .inspector_origins
                .get(token)
                .is_some_and(|(_, alive)| !alive.load(Ordering::Acquire))
            {
                return Err("Inspector origin revoked".into());
            }
            hub.sessions
                .files
                .get(token)
                .cloned()
                .ok_or("unknown file grant")?
        };
        let path = safe_file(&grant.root, &relative).inspect_err(|_| {
            let mut hub = hub.lock().unwrap();
            if let Some(owner) = hub.sessions.tokens.get(token).cloned() {
                hub.issue(
                    &owner,
                    "missing_asset",
                    &format!(
                        "Missing package asset: {}",
                        relative.chars().take(240).collect::<String>()
                    ),
                );
            }
        })?;
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let mime = match extension {
            "html" | "htm" => "text/html; charset=utf-8",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "svg" => "image/svg+xml",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            _ => return Err("file type not served".into()),
        };
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
            return Err("file size limit".into());
        }
        let body = if extension == "html" || extension == "htm" {
            Body::Html(read_html(file, &grant.bootstrap)?)
        } else {
            Body::Static(file, metadata.len())
        };
        Ok((body, mime))
    })();
    match result {
        Ok((body, mime)) => {
            let isolated = if scope.is_some() {
                "Content-Security-Policy: frame-ancestors 'none'; object-src 'none'; frame-src 'none'; base-uri 'self'; form-action 'self'\r\nCross-Origin-Resource-Policy: same-origin\r\nPermissions-Policy: camera=(), microphone=(), geolocation=(), display-capture=(), clipboard-read=(), clipboard-write=(), fullscreen=(), payment=(), usb=(), serial=(), hid=(), bluetooth=()\r\nReferrer-Policy: no-referrer\r\n"
            } else {
                ""
            };
            if write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n{isolated}Connection: close\r\n\r\n",body.len()).is_err() {
                return;
            }
            match body {
                Body::Html(bytes) => {
                    let _ = stream.write_all(&bytes);
                }
                Body::Static(file, length) => {
                    let _ = copy_static(file, stream, length);
                }
            }
        }
        Err(_) => {
            let _ = stream.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
}
// Validate file origin
fn validate_file_origin(
    scope: Option<&str>,
    token: &str,
    assigned_port: Option<u16>,
    port: u16,
    headers: &str,
) -> Result<(), String> {
    if scope.is_some_and(|expected| expected != token)
        || assigned_port.is_some_and(|expected| expected != port)
    {
        return Err("File grant belongs to another origin".into());
    }
    if scope.is_some() {
        if assigned_port != Some(port) {
            return Err("Inspector origin revoked".into());
        }
        let expected = format!("127.0.0.1:{port}");
        let mut hosts = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.eq_ignore_ascii_case("host"));
        if hosts.next().map(|(_, value)| value.trim()) != Some(expected.as_str())
            || hosts.next().is_some()
        {
            return Err("Invalid inspector origin".into());
        }
        for (key, value) in headers.lines().filter_map(|line| line.split_once(':')) {
            if key.eq_ignore_ascii_case("origin") && value.trim() != format!("http://{expected}") {
                return Err("Cross-origin inspector request".into());
            }
        }
    }
    Ok(())
}
// Decode path
fn decode_path(input: &str) -> Result<String, String> {
    let mut output = vec![];
    let bytes = input.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'%' {
            let pair = bytes
                .get(offset + 1..offset + 3)
                .ok_or("invalid path escape")?;
            let hex = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            output.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
            offset += 3;
        } else {
            output.push(bytes[offset]);
            offset += 1;
        }
    }
    if output.contains(&0) || output.contains(&b'\\') {
        return Err("invalid path".into());
    }
    String::from_utf8(output).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify streamed static assets use fixed buffers and snapshot length
    fn streamed_static_assets_use_fixed_buffers_and_snapshot_length() {
        struct Source {
            read: u64,
            largest_buffer: usize,
        }
        impl Read for Source {
            // Read for the current operation
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.largest_buffer = self.largest_buffer.max(buffer.len());
                buffer.fill(b'x');
                self.read += buffer.len() as u64;
                Ok(buffer.len()) // Growing / never-ending source.
            }
        }
        for repetition in 0..5 {
            let mut source = Source {
                read: 0,
                largest_buffer: 0,
            };
            copy_static(&mut source, std::io::sink(), MAX_ASSET_BYTES).unwrap();
            assert_eq!(source.read, MAX_ASSET_BYTES);
            assert_eq!(source.largest_buffer, COPY_BUFFER_BYTES);
            eprintln!("STATIC_TRANSFER repetition={repetition} source_bytes={} working_buffer_bytes={COPY_BUFFER_BYTES}", source.read);
        }
        assert_eq!(
            copy_static(&b"short"[..], std::io::sink(), 6)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    #[test]
    // Verify HTML growth is bounded before bootstrap and length matches
    fn html_growth_is_bounded_before_bootstrap_and_length_matches() {
        struct Growing {
            read: u64,
        }
        impl Read for Growing {
            // Read for the current operation
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer.fill(b'x');
                self.read += buffer.len() as u64;
                Ok(buffer.len())
            }
        }
        let mut source = Growing { read: 0 };
        assert!(read_html(&mut source, "<script>bootstrap</script>").is_err());
        assert_eq!(source.read, MAX_ASSET_BYTES + 1);
        let bytes = read_html(&b"<html></html>"[..], "<script></script>").unwrap();
        assert_eq!(bytes, b"<html></html><script></script>");
        assert_eq!(Body::Html(bytes.clone()).len(), bytes.len() as u64);
    }
    #[test]
    // Verify bootstrap escapes script terminators
    fn bootstrap_escapes_script_terminators() {
        let script = bootstrap(
            1234,
            "token",
            "registerPlugin",
            &json!({"name":"</script><script>evil()"}),
            &json!({}),
        );
        assert_eq!(script.matches("</script>").count(), 1);
        assert!(script.contains("\\u003c"));
        assert!(!script.contains("stream-dock-manager-cancel-settings"));
        let inspector = bootstrap(
            1234,
            "token",
            "registerPropertyInspector",
            &json!({}),
            &json!({"context":"settings-draft:</script>"}),
        );
        assert_eq!(inspector.matches("</script>").count(), 1);
        assert!(inspector.contains("stream-dock-manager-cancel-settings"));
        assert!(inspector.contains("settings-draft:\\u003c/script>"));
    }
    #[test]
    // Verify encoded traversal is still checked by safe path
    fn encoded_traversal_is_still_checked_by_safe_path() {
        assert_eq!(decode_path("%2e%2e/secret").unwrap(), "../secret");
        assert!(decode_path("%00").is_err());
        assert!(decode_path("..%5csecret").is_err());
    }
}

#[cfg(test)]
mod isolated_origin_tests {
    use super::validate_file_origin as check;
    #[test]
    // Verify isolated file grants reject shared listener other sessions and rebinding
    fn isolated_file_grants_reject_shared_listener_other_sessions_and_rebinding() {
        let h = "GET /files/a/pi.html HTTP/1.1\r\nHost: 127.0.0.1:4001\r\n";
        assert!(check(Some("a"), "a", Some(4001), 4001, h).is_ok());
        assert!(check(None, "a", Some(4001), 4000, h).is_err());
        assert!(check(Some("b"), "a", Some(4001), 4001, h).is_err());
        assert!(check(Some("a"), "a", None, 4001, h).is_err());
        assert!(check(Some("a"), "a", Some(4001), 4001, "Host: hostile.example").is_err());
        assert!(check(
            Some("a"),
            "a",
            Some(4001),
            4001,
            &format!("{h}Origin: http://127.0.0.1:4002\r\n")
        )
        .is_err());
        assert!(check(
            Some("a"),
            "a",
            Some(4001),
            4001,
            &format!("{h}Host: 127.0.0.1:4001\r\n")
        )
        .is_err());
    }
}
