//! PulseAudio/PipeWire's pactl subscription is read-only. Each event invalidates
//! the shared output observation; the sampler still reads authoritative values
use super::{Signal, Watch};
use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};
use stream_dock_manager_plugin_host::process_tree::{configure, poll_exit, ProcessTree};

pub(super) struct Subscription {
    child: Child,
    tree: ProcessTree,
    healthy: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}
impl Subscription {
    // Initialize Linux state from the supplied inputs
    pub fn new(signal: Arc<Signal>) -> Result<Self, String> {
        let mut command = stream_dock_manager_plugin_runtimes::background_command("pactl");
        command.arg("subscribe").env("LC_ALL", "C");
        Self::start(command, signal)
    }
    // Start the Linux operation
    fn start(mut command: std::process::Command, signal: Arc<Signal>) -> Result<Self, String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| "pactl subscription unavailable")?;
        let tree = match ProcessTree::attach(&child) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let healthy = Arc::new(AtomicBool::new(true));
        let mut value = Self {
            child,
            tree,
            healthy: healthy.clone(),
            reader: None,
        };
        let pipe = value
            .child
            .stdout
            .take()
            .ok_or("Audio notification output unavailable")?;
        value.reader = Some(std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut line = Vec::with_capacity(1024);
            while healthy.load(Ordering::SeqCst) {
                line.clear();
                match reader.by_ref().take(1025).read_until(b'\n', &mut line) {
                    Ok(count) if count > 0 && count <= 1024 => {
                        if [b" on sink ".as_slice(), b" on server ", b" on card "]
                            .iter()
                            .any(|part| line.windows(part.len()).any(|window| window == *part))
                        {
                            signal.invalidate(false);
                        }
                    }
                    _ => break,
                }
            }
            healthy.store(false, Ordering::SeqCst);
            signal.invalidate(false);
        }));
        Ok(value)
    }
}

impl Watch for Subscription {
    // Report whether the Linux observation is healthy
    fn healthy(&mut self) -> bool {
        if poll_exit(&mut self.child, Some(&self.tree)).is_some() {
            self.healthy.store(false, Ordering::SeqCst);
        }
        self.healthy.load(Ordering::SeqCst)
    }
}
impl Drop for Subscription {
    // Release this Linux owner when it leaves scope
    fn drop(&mut self) {
        self.healthy.store(false, Ordering::SeqCst);
        self.tree.terminate();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    // Start a shell-backed notification subscription for the Linux observer fixture
    fn script(source: &str, signal: Arc<Signal>) -> Subscription {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", source]);
        Subscription::start(command, signal).unwrap()
    }
    #[test]
    // Verify that notification reader is bounded and owned group teardown unblocks it
    fn notification_reader_is_bounded_and_owned_group_teardown_unblocks_it() {
        let signal = Arc::new(Signal::default());
        let observer = script(
            "printf \"Event 'change' on sink #1\\n\"; sleep 30",
            signal.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !signal.dirty.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let start = Instant::now();
        drop(observer);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    // Verify that oversized notification fails observation and reaps its owner
    fn oversized_notification_fails_observation_and_reaps_its_owner() {
        let signal = Arc::new(Signal::default());
        let mut observer = script("printf '%02048d' 0; sleep 30", signal);
        let deadline = Instant::now() + Duration::from_secs(2);
        while observer.healthy() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(observer);
    }
}
