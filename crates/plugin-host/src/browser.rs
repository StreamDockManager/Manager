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
// The VSD Vue starter uses the presence of a truthy fifth callback argument
// to choose the property inspector, even for a page shared with the runtime.
pub(super) enum BootstrapRole<'a> {
    Runtime,
    Inspector(&'a Value),
}

// Build the isolated browser plugin bootstrap
pub(super) fn bootstrap(port: u16, token: &str, info: &Value, role: BootstrapRole<'_>) -> String {
    let (event, action) = match role {
        BootstrapRole::Runtime => ("registerPlugin", None),
        BootstrapRole::Inspector(action) => ("registerPropertyInspector", Some(action)),
    };
    let mut arguments = vec![
        port.to_string(),
        token.to_owned(),
        event.to_owned(),
        info.to_string(),
    ];
    if let Some(action) = action {
        arguments.push(action.to_string());
    }
    let arguments = json!(arguments).to_string().replace('<', "\\u003c");
    let cancel = if let Some(action) = action {
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
/// VSD declares per-action defaults in the manifest. Seed only the missing
/// fields of a legacy browser binding/draft; saved or plugin-owned fields win.
pub(super) fn vsd_action_settings<'a>(
    plugin: &Plugin,
    action: &str,
    settings: &'a Value,
) -> std::borrow::Cow<'a, Value> {
    use std::borrow::Cow;
    if !legacy_vsd(plugin) {
        return Cow::Borrowed(settings);
    }
    let Some((defaults, existing)) = plugin
        .metadata
        .get(action)
        .and_then(|metadata| metadata["Settings"].as_object())
        .zip(settings.as_object())
    else {
        return Cow::Borrowed(settings);
    };
    if defaults.keys().all(|key| existing.contains_key(key)) {
        return Cow::Borrowed(settings);
    }
    let mut merged = existing.clone();
    for (key, value) in defaults {
        merged.entry(key).or_insert_with(|| value.clone());
    }
    let merged = Value::Object(merged);
    // Leave the previous behavior for over-limit manifests instead of putting
    // a larger settings payload in the queue or breaking an existing draft.
    if merged.to_string().len() > 256 * 1024 {
        return Cow::Borrowed(settings);
    }
    Cow::Owned(merged)
}

fn legacy_vsd(plugin: &Plugin) -> bool {
    plugin.info.dialect == "vsd"
        && plugin.info.kind == "browser"
        && plugin.info.capabilities.is_none()
}

/// Legacy browser actions can use title parameters to draw their own canvas,
/// even when Manager has no user-authored title override. Publish this once
/// after willAppear, separately from the authored/imported title dedup cache.
pub(super) fn vsd_initial_title(plugin: &Plugin, appearance: &Value) -> Option<Value> {
    if !legacy_vsd(plugin) {
        return None;
    }
    let action = appearance["action"].as_str()?;
    let state = usize::try_from(appearance["payload"]["state"].as_u64()?).ok()?;
    let state_data = plugin.metadata.get(action)?["States"].get(state)?;
    let mut title = json!({
        "fontFamily": "system-ui",
        "fontStyle": "Regular",
        "fontSize": 12,
        "fontUnderline": false,
        "showTitle": true,
        "titleAlignment": "bottom",
        "titleColor": "#FFFFFF"
    });
    if let Value::Object(values) = visual::manifest_title_parameters(state_data) {
        title.as_object_mut().unwrap().extend(values);
    }
    let text = state_data["Title"].as_str().unwrap_or_default();
    if text.len() > 4096 || title.to_string().len() > 8192 {
        return None;
    }
    Some(json!({
        "event": "titleParametersDidChange",
        "action": action,
        "context": appearance["context"],
        "device": appearance["device"],
        "payload": {
            "settings": appearance["payload"]["settings"],
            "coordinates": appearance["payload"]["coordinates"],
            "state": state,
            "title": text,
            "titleParameters": title
        }
    }))
}

pub(super) fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
        })
}

fn asset_mime(path: &Path) -> Option<&'static str> {
    if is_html(path) {
        return Some("text/html; charset=utf-8");
    }
    let extension = path.extension()?.to_str()?;
    Some(match extension {
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
        ext if ext.eq_ignore_ascii_case("ttf") => "font/ttf",
        ext if ext.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        ext if ext.eq_ignore_ascii_case("wav") => "audio/wav",
        ext if ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("oga") => "audio/ogg",
        ext if ext.eq_ignore_ascii_case("m4a") => "audio/mp4",
        _ => return None,
    })
}

// Media elements in WebKit can require a valid 206 for their initial bytes=0-1
// request. Support only one bounded byte range on already-approved audio files.
#[derive(Clone, Copy)]
enum AudioResponse {
    None,
    Full,
    Partial { start: u64, end: u64, length: u64 },
    Unsatisfiable { length: u64 },
}

fn audio_response(headers: &str, length: u64) -> AudioResponse {
    let mut ranges = headers.lines().skip(1).filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("Range").then_some(value.trim())
    });
    let Some(range) = ranges.next() else {
        return AudioResponse::Full;
    };
    let unsatisfiable = AudioResponse::Unsatisfiable { length };
    if ranges.next().is_some() || length == 0 {
        return unsatisfiable;
    }
    let Some((unit, range)) = range.split_once('=') else {
        return unsatisfiable;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return AudioResponse::Full;
    }
    let Some((start, end)) = range.split_once('-') else {
        return unsatisfiable;
    };
    let digit = |value: &str| {
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let (start, end) = if start.is_empty() {
        match digit(end) {
            Some(count) if count > 0 => (length.saturating_sub(count), length - 1),
            _ => return unsatisfiable,
        }
    } else {
        match (digit(start), digit(end), end.is_empty()) {
            (Some(start), _, true) if start < length => (start, length - 1),
            (Some(start), Some(end), false) if start < length && start <= end => {
                (start, end.min(length - 1))
            }
            _ => return unsatisfiable,
        }
    };
    AudioResponse::Partial { start, end, length }
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
    // Accepted sockets may inherit the listener's nonblocking mode on macOS
    // Use the bounded read/write timeouts instead of truncating a response on WouldBlock
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .is_err()
    {
        return;
    }
    let result = (|| -> Result<(Body, &str, AudioResponse), String> {
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
        let mime = asset_mime(&path).ok_or("file type not served")?;
        let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
            return Err("file size limit".into());
        }
        let audio = if mime.starts_with("audio/") {
            audio_response(headers, metadata.len())
        } else {
            AudioResponse::None
        };
        let body = if is_html(&path)
            && grant
                .runtime_entry
                .as_ref()
                .is_none_or(|entry| *entry == path)
        {
            Body::Html(read_html(file, &grant.bootstrap)?)
        } else if let AudioResponse::Partial { start, end, .. } = audio {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
            Body::Static(file, end - start + 1)
        } else {
            Body::Static(file, metadata.len())
        };
        Ok((body, mime, audio))
    })();
    match result {
        Ok((body, mime, audio)) => {
            let isolated = if scope.is_some() {
                "Content-Security-Policy: frame-ancestors 'none'; object-src 'none'; frame-src 'none'; base-uri 'self'; form-action 'self'\r\nCross-Origin-Resource-Policy: same-origin\r\nPermissions-Policy: camera=(), microphone=(), geolocation=(), display-capture=(), clipboard-read=(), clipboard-write=(), fullscreen=(), payment=(), usb=(), serial=(), hid=(), bluetooth=()\r\nReferrer-Policy: no-referrer\r\n"
            } else {
                ""
            };
            let (status, range_headers, body_bytes) = match audio {
                AudioResponse::None => ("200 OK", String::new(), body.len()),
                AudioResponse::Full => ("200 OK", "Accept-Ranges: bytes\r\n".into(), body.len()),
                AudioResponse::Partial { start, end, length } => (
                    "206 Partial Content",
                    format!(
                        "Accept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{length}\r\n"
                    ),
                    body.len(),
                ),
                AudioResponse::Unsatisfiable { length } => (
                    "416 Range Not Satisfiable",
                    format!("Accept-Ranges: bytes\r\nContent-Range: bytes */{length}\r\n"),
                    0,
                ),
            };
            if write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {body_bytes}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n{isolated}{range_headers}Connection: close\r\n\r\n").is_err()
                || matches!(audio, AudioResponse::Unsatisfiable { .. })
            {
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
            &json!({"name":"</script><script>evil()"}),
            BootstrapRole::Runtime,
        );
        assert_eq!(script.matches("</script>").count(), 1);
        assert!(script.contains("\\u003c"));
        assert!(!script.contains("stream-dock-manager-cancel-settings"));
        let inspector = bootstrap(
            1234,
            "token",
            &json!({}),
            BootstrapRole::Inspector(&json!({"context":"settings-draft:</script>"})),
        );
        assert_eq!(inspector.matches("</script>").count(), 1);
        assert!(inspector.contains("stream-dock-manager-cancel-settings"));
        assert!(inspector.contains("settings-draft:\\u003c/script>"));
    }
    #[test]
    fn shared_vsd_html_has_distinct_runtime_and_inspector_callback_arguments() {
        let info = json!({"application":{"version":"3.10"}});
        let details = json!({
            "action":"org.fixture.action",
            "context":"settings-draft:fixture",
            "payload":{"settings":{"label":"fixture"},"scope":"action"}
        });
        let callback_args = |script: &str| {
            let (_, arguments) = script.split_once("const args=").unwrap();
            let (arguments, _) = arguments.split_once(";const info=").unwrap();
            serde_json::from_str::<Vec<String>>(arguments).unwrap()
        };
        let runtime = bootstrap(1234, "runtime-token", &info, BootstrapRole::Runtime);
        let inspector = bootstrap(
            1234,
            "inspector-token",
            &info,
            BootstrapRole::Inspector(&details),
        );
        let runtime_args = callback_args(&runtime);
        let inspector_args = callback_args(&inspector);
        assert_eq!(
            runtime_args,
            vec![
                "1234".into(),
                "runtime-token".into(),
                "registerPlugin".into(),
                info.to_string()
            ]
        );
        assert_eq!(
            inspector_args,
            vec![
                "1234".into(),
                "inspector-token".into(),
                "registerPropertyInspector".into(),
                info.to_string(),
                details.to_string()
            ]
        );
        // A same-file starter treats a nonempty fifth string as the inspector.
        let uses_inspector = |args: &[String]| args.get(4).is_some_and(|arg| !arg.is_empty());
        assert!(!uses_inspector(&runtime_args));
        assert!(uses_inspector(&inspector_args));

        for script in [runtime, inspector] {
            let html = read_html(&b"<html><main id=app></main></html>"[..], &script).unwrap();
            let html = std::str::from_utf8(&html).unwrap();
            assert_eq!(html.matches("window.addEventListener('load'").count(), 1);
            assert_eq!(html.matches("connect(...args)").count(), 1);
            assert!(html.contains("{once:true}"));
            assert!(html.contains("window.connectOpenActionSocket||window.connectElgatoStreamDeckSocket||window.connectSocket"));
        }
    }

    #[test]
    fn legacy_vsd_canvas_actions_receive_manifest_defaults_and_initial_title_once() {
        let root = tempfile::tempdir().unwrap();
        let owner = "fixture.canvas";
        let action = "fixture.canvas.action";
        let package = root.path().join("fixture.canvas.sdPlugin");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("index.html"), "<html></html>").unwrap();
        std::fs::write(
            package.join("icon.svg"),
            "<svg xmlns='http://www.w3.org/2000/svg'/>",
        )
        .unwrap();
        std::fs::write(
            package.join("manifest.json"),
            json!({
                "UUID":owner,"Name":"Canvas fixture","Version":"1.0","SDKVersion":1,
                "CodePath":"index.html","StreamDockManager":{"ManifestDialect":"vsd"},
                "Actions":[{
                    "UUID":action,"Name":"Canvas clock","Controllers":["Keypad","Information"],
                    "Settings":{"theme":"theme1","zone":"system","enabled":true,"title":"Default",
                                "nullable":"seed","nested":{"x":1},"deep":0},
                    "States":[{"Image":"icon.svg","FontSize":"10","FontStyle":"Bold"}],
                    "PropertyInspectorPath":"index.html"
                }]
            })
            .to_string(),
        )
        .unwrap();
        let mut host = lifecycle_tests::host(root.path());
        host.enable(owner, true).unwrap();
        lifecycle_tests::demand(&mut host, owner);
        let overrides = json!({"title":"Saved","enabled":false,"nullable":null,"nested":{"y":2}});
        let expected = json!({
            "title":"Saved","enabled":false,"nullable":null,"nested":{"y":2},
            "theme":"theme1","zone":"system","deep":0
        });
        let active = || ActiveAction {
            plugin_id: owner.into(),
            action_id: action.into(),
            device_id: "device".into(),
            control_id: "key".into(),
            scope: "profile:page".into(),
            settings: overrides.clone(),
            surface: None,
            column: 1,
            row: 2,
            encoder: false,
        };
        host.synchronize(vec![active()], vec![json!({"id":"device"})]);
        host.synchronize(vec![active()], vec![json!({"id":"device"})]);
        let context = {
            let hub = host.hub.lock().unwrap();
            let events: Vec<_> = hub.sessions.queues[owner]
                .iter()
                .filter(|entry| entry["action"] == action)
                .collect();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0]["event"], "willAppear");
            assert_eq!(events[0]["payload"]["settings"], expected);
            assert_eq!(events[1]["event"], "titleParametersDidChange");
            assert_eq!(events[1]["context"], events[0]["context"]);
            assert_eq!(events[1]["payload"]["titleParameters"]["fontSize"], 10.0);
            assert_eq!(events[1]["payload"]["titleParameters"]["fontStyle"], "Bold");
            assert_eq!(
                events[1]["payload"]["titleParameters"]["titleColor"],
                "#FFFFFF"
            );
            assert_eq!(events[1]["payload"]["titleParameters"]["showTitle"], true);
            events[0]["context"].as_str().unwrap().to_owned()
        };
        assert!(!host
            .hide_title_parameters(owner, "profile:page", "device", "key", "")
            .unwrap());
        let authored = json!({"fontSize":18,"titleColor":"#000000"});
        assert!(host
            .update_title_parameters(
                owner,
                "profile:page",
                "device",
                "key",
                "Authored",
                authored.clone()
            )
            .unwrap());
        assert_eq!(
            host.hub.lock().unwrap().context_state.title_parameters[&context]["titleParameters"],
            authored
        );

        host.dispatch(
            owner,
            action,
            "device",
            "secondary",
            "profile:page",
            &hardware_core::Input::Pulse,
            &overrides,
        )
        .unwrap();
        {
            let hub = host.hub.lock().unwrap();
            let context = hub
                .lookup_binding_context(owner, "profile:page", "device", "secondary")
                .unwrap();
            let events: Vec<_> = hub.sessions.queues[owner]
                .iter()
                .filter(|entry| entry["context"] == context)
                .collect();
            let names: Vec<_> = events
                .iter()
                .map(|entry| entry["event"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                ["willAppear", "titleParametersDidChange", "keyDown", "keyUp"]
            );
            assert_eq!(events[0]["payload"]["settings"], expected);
            assert_eq!(events[2]["payload"]["settings"], expected);
        }
        let panel = host
            .settings_panel(owner, Some(action), &overrides)
            .unwrap();
        assert_eq!(host.panel_settings(&panel.token).unwrap(), expected);
        host.close_inspector(&panel.token);
        assert_eq!(host.action_defaults(owner, action).unwrap(), json!({}));
        assert!(Hub::load(host.storage())
            .unwrap()
            .context_state
            .settings
            .is_empty());

        let nonobject = json!([1]);
        let plugin = host.plugins.get_mut(owner).unwrap();
        assert_eq!(
            vsd_action_settings(plugin, action, &nonobject).as_ref(),
            &nonobject
        );
        plugin.info.dialect = "openaction".into();
        assert_eq!(
            vsd_action_settings(plugin, action, &overrides).as_ref(),
            &overrides
        );
        assert!(
            vsd_initial_title(plugin, &json!({"action":action,"payload":{"state":0}})).is_none()
        );
    }

    #[test]
    fn runtime_http_only_bootstraps_primary_html_and_serves_bounded_local_audio_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let main = root.join("plugin.HTML");
        std::fs::write(&main, "<main>entry</main>").unwrap();
        std::fs::write(root.join("other.HTM"), "<main>auxiliary</main>").unwrap();
        std::fs::write(root.join("sound.MP3"), b"0123456789").unwrap();
        std::fs::write(root.join("digits.TTF"), b"fixture font bytes").unwrap();
        std::fs::write(root.join("other.exe"), b"not served").unwrap();
        let mut hub = Hub::default();
        hub.sessions
            .tokens
            .insert("testtoken".into(), "fixture".into());
        hub.sessions.files.insert(
            "testtoken".into(),
            FileGrant {
                root,
                runtime_entry: Some(main),
                bootstrap: "<script>startFixtureOnce();</script>".into(),
                context: None,
                setup_runtime: false,
            },
        );
        let hub = Arc::new(Mutex::new(hub));
        let get = |target: &str, request_headers: &str| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let hub = hub.clone();
            let worker = std::thread::spawn(move || serve_file(listener.accept().unwrap().0, hub));
            let mut client = TcpStream::connect(address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write!(
                client,
                "GET /files/{target} HTTP/1.1\r\nHost: {address}\r\n{request_headers}\r\n"
            )
            .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            worker.join().unwrap();
            response
        };
        let sanitize_response = |response: String| {
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            (headers.to_owned(), body.to_owned())
        };
        let (headers, body) = sanitize_response(get("testtoken/plugin.HTML", ""));
        assert!(headers.contains("Content-Type: text/html; charset=utf-8"));
        assert_eq!(body.matches("startFixtureOnce()").count(), 1);
        let (headers, body) = sanitize_response(get("testtoken/other.HTM", ""));
        assert!(headers.contains("Content-Type: text/html; charset=utf-8"));
        assert_eq!(body, "<main>auxiliary</main>");
        let (headers, body) = sanitize_response(get("testtoken/digits.TTF", ""));
        assert!(headers.contains("Content-Type: font/ttf"));
        assert_eq!(body, "fixture font bytes");

        let (headers, body) = sanitize_response(get("testtoken/sound.MP3", ""));
        assert!(headers.starts_with("HTTP/1.1 200 OK"));
        assert!(headers.contains("Content-Type: audio/mpeg"));
        assert!(headers.contains("Accept-Ranges: bytes"));
        assert_eq!(body, "0123456789");
        for (request, expected_range, expected_body) in [
            ("bytes=0-1", "0-1/10", "01"),
            ("bytes=3-99", "3-9/10", "3456789"),
            ("bytes=7-", "7-9/10", "789"),
            ("bytes=-2", "8-9/10", "89"),
        ] {
            let (headers, body) =
                sanitize_response(get("testtoken/sound.MP3", &format!("Range: {request}\r\n")));
            assert!(headers.starts_with("HTTP/1.1 206 Partial Content"));
            assert!(headers.contains(&format!("Content-Range: bytes {expected_range}")));
            assert!(headers.contains(&format!("Content-Length: {}", expected_body.len())));
            assert_eq!(body, expected_body);
        }
        for invalid in [
            "Range: bytes=10-\r\n",
            "Range: bytes=0-1,3-4\r\n",
            "Range: bytes=6-4\r\n",
            "Range: bytes=-0\r\n",
            "Range: bytes=0-1\r\nrange: bytes=3-4\r\n",
        ] {
            let (headers, body) = sanitize_response(get("testtoken/sound.MP3", invalid));
            assert!(headers.starts_with("HTTP/1.1 416 Range Not Satisfiable"));
            assert!(headers.contains("Content-Range: bytes */10"));
            assert!(body.is_empty());
        }

        for target in [
            "testtoken/other.exe",
            "revoked/sound.MP3",
            "testtoken/%2e%2e/sound.MP3",
        ] {
            assert!(get(target, "Range: bytes=0-1\r\n").starts_with("HTTP/1.1 403 Forbidden"));
        }
        let (_, body) = sanitize_response(get("testtoken/plugin.HTML", "Range: bytes=0-1\r\n"));
        assert!(body.contains("startFixtureOnce()"));
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
