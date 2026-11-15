//! Serialized native input with per-binding ownership and correlated replies
use crate::Runtime;
use enigo::Enigo;
use serde_json::{json, Value};
#[cfg(not(windows))]
use std::process::Command;
use std::{
    collections::HashMap,
    io::Write,
    process::Stdio,
    sync::{mpsc, OnceLock, Weak},
    time::{Duration, Instant},
};
struct Request {
    runtime: Weak<Runtime>,
    owner: String,
    context: String,
    session: String,
    payload: Value,
}
struct Cleanup {
    owner: String,
    context: Option<String>,
    shutdown: bool,
    done: Option<mpsc::Sender<()>>,
}
struct Worker {
    requests: mpsc::SyncSender<Request>,
    cleanup: mpsc::Sender<Cleanup>,
}
static WORKER: OnceLock<Worker> = OnceLock::new();
#[derive(Default)]
struct OwnedKeys(HashMap<String, Vec<String>>);
impl OwnedKeys {
    // Acquire the owned desktop input resource
    fn acquire(&mut self, owner: &str, keys: Vec<String>) -> Vec<String> {
        if self.0.contains_key(owner) {
            return vec![];
        }
        let mut unique = vec![];
        for key in keys {
            if !unique.contains(&key) {
                unique.push(key);
            }
        }
        let newly = unique
            .iter()
            .filter(|key| !self.0.values().any(|keys| keys.contains(key)))
            .cloned()
            .collect();
        self.0.insert(owner.into(), unique);
        newly
    }
    // Release the owned desktop input resource
    fn release(&mut self, owner: &str) -> Vec<String> {
        self.0
            .remove(owner)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .filter(|key| !self.0.values().any(|keys| keys.contains(key)))
            .collect()
    }
}
struct Target {
    identity: Option<String>,
    changed: bool,
    at: Instant,
}
impl Target {
    // Update desktop input from the new observation
    fn observe(&mut self, current: &Option<String>) {
        if current != &self.identity {
            self.changed = true;
        }
    }
    // Report the matches condition for desktop input
    fn matches(&self, current: &Option<String>) -> bool {
        self.identity.is_some() && !self.changed && current == &self.identity
    }
}
// Normalize key
fn normalize_key(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "control" => "ctrl".into(),
        "super" | "meta" => "cmd".into(),
        other => other.into(),
    }
}
// Release the owned desktop input resource
fn release(owner: &str, held: &mut OwnedKeys, _keyboard: &mut Option<Enigo>) -> Result<(), String> {
    held.release(owner);
    crate::builtins::plugin_release(owner);
    Ok(())
}
// Press the requested desktop input input
fn press(
    owner: &str,
    keys: Vec<String>,
    held: &mut OwnedKeys,
    _keyboard: &mut Option<Enigo>,
) -> Result<(), String> {
    crate::builtins::plugin_hold(owner, &keys)?;
    held.acquire(owner, keys);
    Ok(())
}
#[derive(Default)]
struct WorkerState {
    keyboard: Option<Enigo>,
    held: OwnedKeys,
    targets: HashMap<String, Target>,
}
impl WorkerState {
    // Clean up resources retained by desktop input
    fn cleanup(&mut self, cleanup: &Cleanup) {
        let prefix = format!("{}:", cleanup.owner);
        let matches = |key: &String| {
            cleanup.shutdown
                || (key.starts_with(&prefix)
                    && cleanup
                        .context
                        .as_ref()
                        .is_none_or(|context| key.ends_with(&format!(":{context}"))))
        };
        let owners: Vec<_> = self
            .held
            .0
            .keys()
            .filter(|key| matches(key))
            .cloned()
            .collect();
        for owner in owners {
            let _ = release(&owner, &mut self.held, &mut self.keyboard);
        }
        self.targets.retain(|key, _| !matches(key));
    }
    // Execute the requested desktop input operation
    fn execute(&mut self, request: &Request) -> Result<String, String> {
        let op = request.payload["op"].as_str().unwrap_or("");
        let owner = format!("{}:{}:{}", request.owner, request.session, request.context);
        match op {
            "captureTarget" => {
                self.targets.insert(
                    owner,
                    Target {
                        identity: target(),
                        changed: false,
                        at: Instant::now(),
                    },
                );
                Ok("Insertion target captured".into())
            }
            "clearTarget" => {
                self.targets.remove(&owner);
                Ok("Insertion cancelled".into())
            }
            "copy" | "paste" => {
                let text = request.payload["text"]
                    .as_str()
                    .ok_or("transcript missing")?;
                if text.len() > 256 * 1024 {
                    return Err("transcript exceeds limit".into());
                }
                crate::builtins::copy_text(text)?;
                if op == "copy" {
                    self.targets.remove(&owner);
                    return Ok("Transcript copied".into());
                }
                let expected = self.targets.remove(&owner);
                if !expected
                    .as_ref()
                    .is_some_and(|expected| expected.matches(&target()))
                {
                    return Ok("Target changed or unavailable · transcript copied".into());
                }
                crate::builtins::paste_text(
                    text,
                    false,
                    expected
                        .as_ref()
                        .and_then(|target| target.identity.as_deref()),
                )?;
                Ok("Transcript inserted".into())
            }
            "release" => {
                self.targets.remove(&owner);
                release(&owner, &mut self.held, &mut self.keyboard)?;
                Ok("Released".into())
            }
            "shortcut" | "press" => {
                let raw: Vec<String> = serde_json::from_value(request.payload["keys"].clone())
                    .map_err(|_| "keys must be strings")?;
                if raw.is_empty() || raw.len() > 12 {
                    return Err("shortcut requires 1–12 keys".into());
                }
                let keys: Vec<_> = raw.iter().map(|key| normalize_key(key)).collect();
                for key in &keys {
                    crate::actions::parse_key(key)?;
                }
                if op == "shortcut" {
                    crate::builtins::send_shortcut(&keys)?;
                    Ok("Command sent".into())
                } else {
                    press(&owner, keys, &mut self.held, &mut self.keyboard)?;
                    Ok("Shortcut held".into())
                }
            }
            _ => Err("unsupported desktop input operation".into()),
        }
    }
}
// Drain queued work until the owning worker shuts down
fn worker_loop(rx: mpsc::Receiver<Request>, cleanup_rx: mpsc::Receiver<Cleanup>) {
    let mut state = WorkerState::default();
    loop {
        while let Ok(cleanup) = cleanup_rx.try_recv() {
            state.cleanup(&cleanup);
            if let Some(done) = cleanup.done {
                let _ = done.send(());
            }
            if cleanup.shutdown {
                return;
            }
        }
        let request = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(request) => Some(request),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        if !state.targets.is_empty() {
            let current = target();
            for target in state.targets.values_mut() {
                target.observe(&current);
            }
            state
                .targets
                .retain(|_, target| target.at.elapsed() < Duration::from_secs(600));
        }
        let Some(request) = request else { continue };
        let Some(runtime) = request.runtime.upgrade() else {
            continue;
        };
        if !runtime.plugins.lock().unwrap().desktop_input_allowed(
            &request.owner,
            &request.context,
            &request.session,
        ) {
            state.cleanup(&Cleanup {
                owner: format!("{}:{}", request.owner, request.session),
                context: None,
                shutdown: false,
                done: None,
            });
            continue;
        }
        let payload = match state.execute(&request) {
            Ok(outcome) => json!({"request_id":request.payload["request_id"],"outcome":outcome}),
            Err(error) => {
                json!({"request_id":request.payload["request_id"],"outcome":error,"error":true})
            }
        };
        runtime.plugins.lock().unwrap().desktop_input_result(
            &request.owner,
            &request.context,
            payload,
        );
    }
}
// Initialize the bounded input worker and its separate cleanup channel once
fn sender() -> &'static Worker {
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(128);
        let (cleanup_tx, cleanup_rx) = mpsc::channel();
        std::thread::spawn(move || worker_loop(rx, cleanup_rx));
        Worker {
            requests: tx,
            cleanup: cleanup_tx,
        }
    })
}

// Copy the selected desktop input data
pub(crate) fn copy(text: &str) -> Result<(), String> {
    let mut command = {
        #[cfg(target_os = "macos")]
        {
            Command::new("pbcopy")
        }
        #[cfg(target_os = "windows")]
        {
            let mut c = crate::native_process::command("powershell");
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$input | Set-Clipboard",
            ]);
            c
        }
        #[cfg(target_os = "linux")]
        {
            if crate::builtins::on_wayland() {
                Command::new("wl-copy")
            } else {
                let mut c = Command::new("xclip");
                c.args(["-selection", "clipboard"]);
                c
            }
        }
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("clipboard input missing")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    if child.wait().map_err(|e| e.to_string())?.success() {
        Ok(())
    } else {
        Err("clipboard backend failed; check installed clipboard tools".into())
    }
}
// Query a stable foreground insertion target through the platform-specific command
pub(crate) fn target() -> Option<String> {
    let mut command = {
        #[cfg(target_os = "macos")]
        {
            let mut c = Command::new("osascript");
            c.args(["-e","tell application \"System Events\" to tell first application process whose frontmost is true to return (unix id as text) & \":\" & (name of front window as text)"]);
            c
        }
        #[cfg(target_os = "windows")]
        {
            let mut c = crate::native_process::command("powershell");
            c.args(["-NoProfile","-NonInteractive","-Command","Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class W{[DllImport(\"user32.dll\")]public static extern IntPtr GetForegroundWindow();}';[W]::GetForegroundWindow().ToInt64()"]);
            c
        }
        #[cfg(target_os = "linux")]
        {
            if crate::builtins::on_wayland() {
                return None;
            }
            let mut c = Command::new("xdotool");
            c.arg("getactivewindow");
            c
        }
    };
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !value.is_empty() && value != "0").then_some(value)
}
// Submit the desktop input work item
pub fn submit(
    runtime: &std::sync::Arc<Runtime>,
    effect: &stream_dock_manager_plugin_host::DesktopInputEffect,
) {
    let owner = effect.plugin.clone();
    let context = effect.context.clone();
    let session = effect.session.clone().unwrap_or_default();
    let cleanup_owner = if session.is_empty() {
        owner.clone()
    } else {
        format!("{owner}:{session}")
    };
    let op = effect.payload["op"].as_str().unwrap_or("");
    if op == "releaseOwner" {
        let _ = sender().cleanup.send(Cleanup {
            owner: cleanup_owner,
            context: None,
            shutdown: false,
            done: None,
        });
        return;
    }
    if op == "release" && session.is_empty() {
        let _ = sender().cleanup.send(Cleanup {
            owner,
            context: Some(context),
            shutdown: false,
            done: None,
        });
        return;
    }
    // Do not retain requests submitted during a recorded transaction for later
    // playback. Release/owner cleanup keeps its separate, unbounded control lane
    if matches!(op, "shortcut" | "press" | "paste")
        && crate::builtins::native::keyboard_macro_busy()
    {
        runtime.plugins.lock().unwrap().desktop_input_result(
            &owner,
            &context,
            json!({"request_id":effect.payload["request_id"],"outcome":"Keyboard input is busy recording or playing a macro","error":true}),
        );
        return;
    }
    let request = Request {
        runtime: std::sync::Arc::downgrade(runtime),
        owner: owner.clone(),
        context: context.clone(),
        session,
        payload: effect.payload.clone(),
    };
    if sender().requests.try_send(request).is_err() {
        if op == "release" {
            let _ = sender().cleanup.send(Cleanup {
                owner: cleanup_owner,
                context: Some(context),
                shutdown: false,
                done: None,
            });
        } else {
            runtime.plugins.lock().unwrap().desktop_input_result(&owner,&context,json!({"request_id":effect.payload["request_id"],"outcome":"Desktop input queue is busy","error":true}));
        }
    }
}
// Shut down the desktop input worker
pub fn shutdown() {
    if let Some(worker) = WORKER.get() {
        let (tx, rx) = mpsc::channel();
        let _ = worker.cleanup.send(Cleanup {
            owner: String::new(),
            context: None,
            shutdown: true,
            done: Some(tx),
        });
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that owners release only their keys
    fn owners_release_only_their_keys() {
        let mut keys = OwnedKeys::default();
        assert_eq!(
            keys.acquire("a", vec!["ctrl".into(), "m".into()]),
            ["ctrl", "m"]
        );
        assert_eq!(keys.acquire("b", vec!["ctrl".into(), "d".into()]), ["d"]);
        assert_eq!(keys.release("a"), ["m"]);
        assert_eq!(keys.release("b"), ["d", "ctrl"]);
        assert!(keys.release("b").is_empty());
    }
    #[test]
    // Verify that changed targets never resume insertion
    fn changed_targets_never_resume_insertion() {
        let mut target = Target {
            identity: Some("window-a".into()),
            changed: false,
            at: Instant::now(),
        };
        assert!(target.matches(&Some("window-a".into())));
        target.observe(&Some("window-b".into()));
        target.observe(&Some("window-a".into()));
        assert!(!target.matches(&Some("window-a".into())));
    }
}
