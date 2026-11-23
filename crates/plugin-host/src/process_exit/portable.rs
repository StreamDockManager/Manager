//! One bounded, cancellable observer. Snapshots retain owned OS objects until a
//! wait completes; removing a generation cannot close a handle being waited on.
use std::{
    collections::HashMap,
    process::Child,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;
struct Entry {
    object: platform::Object,
    ready: AtomicBool,
}
pub(crate) struct Registry {
    signal: platform::Signal,
    entries: Mutex<(usize, HashMap<usize, Arc<Entry>>)>,
    stop: AtomicBool,
}
pub(crate) struct Watch {
    registry: Arc<Registry>,
    id: usize,
    entry: Arc<Entry>,
}
impl Registry {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Option<(Arc<Self>, JoinHandle<()>)> {
        let registry = Arc::new(Self {
            signal: platform::Signal::new()?,
            entries: Mutex::new((0, HashMap::new())),
            stop: AtomicBool::new(false),
        });
        let owner = registry.clone();
        let thread = std::thread::Builder::new().name("plugin-exit-events".into()).spawn(move || {
            while !owner.stop.load(Ordering::Acquire) {
                let snapshot: Vec<_> = owner.entries.lock().unwrap().1.iter()
                    .filter(|(_, entry)| !entry.ready.load(Ordering::Acquire))
                    .map(|(id, entry)| (*id, entry.clone())).collect();
                let objects: Vec<_> = snapshot.iter().map(|(_, entry)| &entry.object).collect();
                match owner.signal.wait(&objects) {
                    Ok(indices) => {
                        let mut changed = false;
                        {
                            let entries = owner.entries.lock().unwrap();
                            for index in indices {
                                let (id, entry) = &snapshot[index];
                                if entries.1.get(id).is_some_and(|current| Arc::ptr_eq(current, entry)) {
                                    entry.ready.store(true, Ordering::Release);
                                    changed = true;
                                }
                            }
                        }
                        if changed { wake(); }
                    }
                    Err(error) => {
                        log::warn!("Plugin exit observer failed; using bounded lifecycle polling: {error}");
                        owner.stop.store(true, Ordering::Release);
                        wake();
                    }
                }
            }
        }).ok()?;
        Some((registry, thread))
    }
    pub fn register(self: &Arc<Self>, child: &Child) -> Option<Watch> {
        let mut entries = self.entries.lock().unwrap();
        if self.stop.load(Ordering::Acquire) || entries.1.len() >= platform::CAPACITY {
            return None;
        }
        let id = entries.0.checked_add(1)?;
        // Caller retains the unreaped Child throughout registration. Linux PID
        // cannot be reused; Windows duplicates that Child's exact kernel handle.
        let entry = Arc::new(Entry {
            object: platform::Object::new(child)?,
            ready: AtomicBool::new(false),
        });
        entries.0 = id;
        entries.1.insert(id, entry.clone());
        self.signal.notify();
        Some(Watch {
            registry: self.clone(),
            id,
            entry,
        })
    }
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.signal.notify();
    }
}
impl Watch {
    pub fn healthy(&self) -> bool {
        !self.registry.stop.load(Ordering::Acquire)
    }
    pub fn ready(&self) -> bool {
        self.entry.ready.load(Ordering::Acquire)
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.registry.entries.lock().unwrap().1.remove(&self.id);
        self.registry.signal.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    fn child() -> Child {
        #[cfg(target_os = "windows")]
        {
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .spawn()
                .unwrap()
        }
        #[cfg(target_os = "linux")]
        {
            Command::new("/bin/sleep").arg("30").spawn().unwrap()
        }
    }
    #[test]
    fn cancellation_capacity_and_shutdown_preserve_child_ownership() {
        let (registry, thread) = Registry::new(Arc::new(|| {})).unwrap();
        let mut child = child();
        let mut watches = Vec::new();
        for _ in 0..platform::CAPACITY {
            watches.push(registry.register(&child).unwrap());
        }
        assert!(registry.register(&child).is_none());
        watches.pop();
        let replacement = registry.register(&child).unwrap();
        assert!(!replacement.ready());
        registry.shutdown();
        thread.join().unwrap();
        assert!(!replacement.healthy());
        assert!(child.try_wait().unwrap().is_none());
        drop(watches);
        drop(replacement);
        assert!(registry.entries.lock().unwrap().1.is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
    }
    #[test]
    fn exit_wakes_and_owner_retains_authoritative_status() {
        let (registry, thread) = Registry::new(Arc::new(|| {})).unwrap();
        for _ in 0..16 {
            let mut child = child();
            let stale = registry.register(&child).unwrap();
            drop(stale);
            let watch = registry.register(&child).unwrap();
            child.kill().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !watch.ready() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(watch.ready());
            drop(watch);
            assert!(!child.wait().unwrap().success());
        }
        registry.shutdown();
        thread.join().unwrap();
    }
}

#[cfg(test)]
mod early_exit_tests {
    use super::*;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    #[test]
    fn early_exit_keeps_exact_status_and_shutdown_rejects_registration() {
        let (registry, thread) = Registry::new(Arc::new(|| {})).unwrap();
        for _ in 0..16 {
            #[cfg(target_os = "windows")]
            let mut child = Command::new("cmd.exe")
                .args(["/D", "/C", "exit 7"])
                .spawn()
                .unwrap();
            #[cfg(target_os = "linux")]
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap();
            // Deliberately allow exit before registering without reaping it.
            std::thread::sleep(Duration::from_millis(10));
            let watch = registry.register(&child).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !watch.ready() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(watch.ready());
            drop(watch);
            assert_eq!(child.wait().unwrap().code(), Some(7));
            assert!(registry.entries.lock().unwrap().1.is_empty());
        }
        registry.shutdown();
        thread.join().unwrap();
        #[cfg(target_os = "windows")]
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/C", "exit 0"])
            .spawn()
            .unwrap();
        #[cfg(target_os = "linux")]
        let mut child = Command::new("/bin/true").spawn().unwrap();
        assert!(registry.register(&child).is_none());
        child.wait().unwrap();
    }
}
