//! Optional, exact-version compositor authority. Readiness never starts input capture
use serde::Deserialize;
use std::{
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::PathBuf,
    time::{Duration, Instant},
};
const SOURCE: &str = "efb50993780079460b0cbed1363e2166a2de1d9f";
const FRAME_LIMIT: usize = 65536;
// Report the selected condition for hyprland
pub(crate) fn selected() -> bool {
    crate::builtins::native::wayland() && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}
#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub ok: bool,
    pub protocol: u32,
    pub source: String,
    pub protected: bool,
    pub revoked: bool,
    pub interference: bool,
    pub target_alive: bool,
    pub target_focused: bool,
    pub label: String,
    pub held: Vec<u16>,
    pub events: Vec<(u16, bool, u64)>,
    pub seq: u64,
}
pub(crate) struct Client {
    stream: UnixStream,
    seq: Option<u64>,
}
impl Client {
    // Establish the hyprland connection
    pub fn connect() -> Result<Self, String> {
        if !selected() {
            return Err("The optional Hyprland provider is not selected".into());
        }
        let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .map_err(|_| "Hyprland instance unavailable")?;
        if instance.is_empty()
            || instance.len() > 128
            || !instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("Invalid Hyprland instance".into());
        }
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").ok_or("Runtime directory unavailable")?,
        );
        if !runtime.is_absolute() {
            return Err("Invalid runtime directory".into());
        }
        let uid = rustix::process::geteuid().as_raw();
        let path = runtime
            .join("hypr")
            .join(instance)
            .join("sdm-keyboard-v1.sock");
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "Optional Hyprland keyboard companion is not loaded. Build and load it explicitly; Manager never installs it.")?;
        if !meta.file_type().is_socket() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err("Hyprland companion socket ownership or permissions are invalid".into());
        }
        let stream = bounded_connect(&path)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| "Socket deadline unavailable")?;
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| "Socket deadline unavailable")?;
        let credentials = rustix::net::sockopt::socket_peercred(&stream)
            .map_err(|_| "Compositor peer credentials unavailable")?;
        if credentials.uid.as_raw() != uid {
            return Err("Compositor owner changed".into());
        }
        // Bind the auxiliary socket to the actual Wayland compositor process,
        // not a same-UID process merely using a plausible executable name
        let display = PathBuf::from(
            std::env::var_os("WAYLAND_DISPLAY").ok_or("Wayland display unavailable")?,
        );
        let display = if display.is_absolute() {
            display
        } else {
            runtime.join(display)
        };
        let display_stream = bounded_connect(&display)?;
        let display_peer = rustix::net::sockopt::socket_peercred(&display_stream)
            .map_err(|_| "Wayland peer credentials unavailable")?;
        if display_peer.pid != credentials.pid || display_peer.uid != credentials.uid {
            return Err("Keyboard companion does not belong to this Wayland compositor".into());
        }
        drop(display_stream);
        let exe = std::fs::read_link(format!("/proc/{}/exe", credentials.pid.as_raw_nonzero()))
            .map_err(|_| "Compositor executable unavailable")?;
        if exe.file_name().and_then(|s| s.to_str()) != Some("Hyprland") {
            return Err("Peer is not the Hyprland compositor".into());
        }
        let mut client = Self { stream, seq: None };
        client.request("hello", None)?.admitted()?;
        Ok(client)
    }
    // Submit the hyprland request
    pub fn request(&mut self, op: &str, mode: Option<&str>) -> Result<Snapshot, String> {
        let mut request = serde_json::to_vec(&serde_json::json!({"op":op,"mode":mode}))
            .map_err(|_| "Protocol encoding failed")?;
        request.push(b'\n');
        self.stream
            .write_all(&request)
            .map_err(|_| "Compositor request failed")?;
        let deadline = Instant::now() + Duration::from_millis(150);
        let mut frame = Vec::with_capacity(1024);
        loop {
            if frame.len() >= FRAME_LIMIT || Instant::now() > deadline {
                return Err("Compositor response exceeded its bound".into());
            }
            let mut byte = [0];
            self.stream
                .read_exact(&mut byte)
                .map_err(|_| "Compositor response unavailable")?;
            if byte[0] == b'\n' {
                break;
            }
            frame.push(byte[0]);
        }
        let reply: Snapshot =
            serde_json::from_slice(&frame).map_err(|_| "Invalid compositor response")?;
        reply.validate(self.seq)?;
        self.seq = Some(reply.seq);
        Ok(reply)
    }
    // Stop the hyprland operation
    pub fn stop(&mut self) -> Result<Snapshot, String> {
        self.request("stop", None)
    }
}
impl Drop for Client {
    // Release this hyprland owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
impl Snapshot {
    // Validate the supplied hyprland value
    fn validate(&self, previous_seq: Option<u64>) -> Result<(), String> {
        if !self.ok
            || self.protocol != 1
            || self.source != SOURCE
            || self.events.len() > 1024
            || self.held.len() > 768
            || self.label.len() > 2048
            || self
                .events
                .iter()
                .any(|(c, _, t)| *c > 767 || *t > 3_600_000)
            || self.held.iter().any(|c| *c > 767)
            || previous_seq.is_some_and(|s| self.seq < s)
        {
            return Err("Compositor protocol or lease changed".into());
        }
        Ok(())
    }
    // Require the selected Hyprland session to satisfy macro admission checks
    pub fn admitted(&self) -> Result<(), String> {
        if self.protected || self.revoked {
            Err("Hyprland session is protected or its lease was revoked".into())
        } else {
            Ok(())
        }
    }
}
// Report whether hyprland is ready
pub(crate) fn readiness() -> Result<(), String> {
    Client::connect().map(|_| ())
}
struct Target {
    client: Client,
    label: String,
}
// Capture target
pub(crate) fn capture_target() -> Result<Box<dyn super::test_target::TestTarget>, String> {
    let mut client = Client::connect()?;
    let state = client.request("start", Some("target"))?;
    state.admitted()?;
    if !state.target_alive || !state.target_focused {
        return Err("Select a native Wayland application window".into());
    }
    Ok(Box::new(Target {
        client,
        label: super::test_target::display_label("Hyprland", &state.label),
    }))
}
impl super::test_target::TestTarget for Target {
    // Return the display label for hyprland
    fn label(&self) -> &str {
        &self.label
    }
    // Reject an input target whose original process is no longer alive
    fn check_alive(&mut self) -> Result<(), String> {
        let s = self.client.request("alive", None)?;
        s.admitted()?;
        if s.target_alive {
            Ok(())
        } else {
            Err("Selected Wayland window was destroyed".into())
        }
    }
    // Reject an input target that no longer holds the required focus
    fn check_focused(&mut self) -> Result<(), String> {
        let s = self.client.request("focus", None)?;
        s.admitted()?;
        if s.target_alive && s.target_focused {
            Ok(())
        } else {
            Err("Selected Wayland target lost focus".into())
        }
    }
    // Finalize the pending hyprland operation
    fn finish(&mut self) -> Result<(), String> {
        self.client.stop().map(|_| ())
    }
}

// Produce bounded connect
fn bounded_connect(path: &std::path::Path) -> Result<UnixStream, String> {
    use rustix::{
        event::{poll, PollFd, PollFlags, Timespec},
        net,
    };
    let fd = net::socket_with(
        net::AddressFamily::UNIX,
        net::SocketType::STREAM,
        net::SocketFlags::NONBLOCK | net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| "Local socket unavailable")?;
    let address = net::SocketAddrUnix::new(path).map_err(|_| "Invalid local socket path")?;
    match net::connect(&fd, &address) {
        Ok(()) => (),
        Err(rustix::io::Errno::INPROGRESS | rustix::io::Errno::AGAIN) => {
            let mut descriptors = [PollFd::new(&fd, PollFlags::OUT)];
            if poll(
                &mut descriptors,
                Some(&Timespec {
                    tv_sec: 0,
                    tv_nsec: 100_000_000,
                }),
            )
            .map_err(|_| "Local connect failed")?
                == 0
            {
                return Err("Local connect timed out".into());
            }
            net::sockopt::socket_error(&fd)
                .map_err(|_| "Local connection failed")?
                .map_err(|_| "Local connection rejected")?;
        }
        Err(_) => return Err("Local connection unavailable".into()),
    }
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(false)
        .map_err(|_| "Local socket mode unavailable")?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Collect a hyprland sample
    fn sample() -> Snapshot {
        serde_json::from_value(serde_json::json!({"ok":true,"protocol":1,"source":SOURCE,"protected":false,"revoked":false,"interference":false,"target_alive":true,"target_focused":true,"label":"Editor","held":[],"events":[],"seq":2})).unwrap()
    }
    #[test]
    // Verify that wrong version source or regressed sequence is rejected
    fn wrong_version_source_or_regressed_sequence_is_rejected() {
        let mut s = sample();
        assert!(s.validate(Some(1)).is_ok());
        assert!(s.validate(Some(3)).is_err());
        s.protocol = 2;
        assert!(s.validate(None).is_err());
        s.protocol = 1;
        s.source = "other build".into();
        assert!(s.validate(None).is_err());
    }
    #[test]
    // Verify that physical input bounds and session revocation are independent
    fn physical_input_bounds_and_session_revocation_are_independent() {
        let mut s = sample();
        s.interference = true;
        assert!(s.admitted().is_ok()); // release observation remains live
        s.events.push((768, true, 0));
        assert!(s.validate(None).is_err());
        s.events.clear();
        s.events.resize(1025, (30, true, 0));
        assert!(s.validate(None).is_err());
        s.events.clear();
        s.revoked = true;
        assert!(s.admitted().is_err());
        s.revoked = false;
        s.protected = true;
        assert!(s.admitted().is_err());
    }
}
