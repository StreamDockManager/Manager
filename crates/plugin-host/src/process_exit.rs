//! Non-reaping exit readiness. Plugin/stop-worker retain exclusive Child ownership.
//! macOS uses kqueue, Linux pidfds, and Windows owned process handles.
//! Unsupported APIs/capacity or observer failure retain bounded polling.
#[cfg(target_os = "macos")]
mod native {
    use std::{
        collections::HashMap,
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread::JoinHandle,
    };
    struct Entry {
        pid: u32,
        ready: Arc<AtomicBool>,
    }
    pub(crate) struct Registry {
        fd: OwnedFd,
        entries: Mutex<(usize, HashMap<usize, Entry>)>,
        stop: AtomicBool,
    }
    pub(crate) struct Watch {
        registry: Arc<Registry>,
        id: usize,
        ready: Arc<AtomicBool>,
    }
    fn event(ident: usize, filter: i16, flags: u16, fflags: u32, id: usize) -> libc::kevent {
        libc::kevent {
            ident,
            filter,
            flags,
            fflags,
            data: 0,
            udata: id as *mut _,
        }
    }
    impl Registry {
        fn change(&self, event: libc::kevent) -> io::Result<()> {
            let result = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Option<(Arc<Self>, JoinHandle<()>)> {
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                log::warn!(
                    "Native plugin exit observer unavailable: {}",
                    io::Error::last_os_error()
                );
                return None;
            }
            let registry = Arc::new(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                entries: Mutex::new((0, HashMap::new())),
                stop: AtomicBool::new(false),
            });
            // Do not leak the observer descriptor into subsequently launched plugins.
            if unsafe { libc::fcntl(registry.fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
            {
                return None;
            }
            if registry
                .change(event(
                    0,
                    libc::EVFILT_USER,
                    libc::EV_ADD | libc::EV_CLEAR,
                    0,
                    0,
                ))
                .is_err()
            {
                return None;
            }
            let owner = registry.clone();
            let thread = std::thread::Builder::new()
                .name("plugin-exit-events".into())
                .spawn(move || {
                    while !owner.stop.load(Ordering::Acquire) {
                        let mut events: [libc::kevent; 32] = unsafe { std::mem::zeroed() };
                        let count = unsafe {
                            libc::kevent(
                                owner.fd.as_raw_fd(),
                                std::ptr::null(),
                                0,
                                events.as_mut_ptr(),
                                events.len() as i32,
                                std::ptr::null(),
                            )
                        };
                        if count < 0 {
                            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                                continue;
                            }
                            // Fail open to lifecycle polling, never silently lose exits.
                            owner.stop.store(true, Ordering::Release);
                            wake();
                            break;
                        }
                        let mut changed = false;
                        {
                            let entries = owner.entries.lock().unwrap();
                            for event in &events[..count as usize] {
                                if event.filter == libc::EVFILT_PROC {
                                    if let Some(entry) = entries.1.get(&(event.udata as usize)) {
                                        if entry.pid as usize == event.ident {
                                            entry.ready.store(true, Ordering::Release);
                                            changed = true;
                                        }
                                    }
                                }
                            }
                        }
                        if changed {
                            wake();
                        }
                    }
                })
                .ok()?;
            Some((registry, thread))
        }
        pub fn register(self: &Arc<Self>, child: &std::process::Child) -> Option<Watch> {
            let pid = child.id();
            if self.stop.load(Ordering::Acquire) {
                return None;
            }
            let mut entries = self.entries.lock().unwrap();
            let id = entries.0.checked_add(1)?;
            entries.0 = id;
            let ready = Arc::new(AtomicBool::new(false));
            // Caller still owns the unreaped Child, so PID cannot be reused here.
            self.change(event(
                pid as usize,
                libc::EVFILT_PROC,
                libc::EV_ADD | libc::EV_ONESHOT,
                libc::NOTE_EXIT,
                id,
            ))
            .ok()?;
            entries.1.insert(
                id,
                Entry {
                    pid,
                    ready: ready.clone(),
                },
            );
            Some(Watch {
                registry: self.clone(),
                id,
                ready,
            })
        }
        pub fn shutdown(&self) {
            self.stop.store(true, Ordering::Release);
            let _ = self.change(event(0, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0));
        }
    }
    impl Watch {
        pub fn healthy(&self) -> bool {
            !self.registry.stop.load(Ordering::Acquire)
        }
        pub fn ready(&self) -> bool {
            self.ready.load(Ordering::Acquire)
        }
    }
    impl Drop for Watch {
        fn drop(&mut self) {
            // Remove the exact generation before releasing Child ownership. Late
            // queued kernel events cannot identify a replacement plugin run.
            if let Some(entry) = self.registry.entries.lock().unwrap().1.remove(&self.id) {
                let _ = self.registry.change(event(
                    entry.pid as usize,
                    libc::EVFILT_PROC,
                    libc::EV_DELETE,
                    0,
                    self.id,
                ));
            }
        }
    }
}
#[cfg(target_os = "macos")]
pub(crate) use native::{Registry, Watch};
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod fallback {
    use std::{sync::Arc, thread::JoinHandle};
    pub(crate) struct Registry;
    pub(crate) struct Watch;
    impl Registry {
        pub fn new(_: Arc<dyn Fn() + Send + Sync>) -> Option<(Arc<Self>, JoinHandle<()>)> {
            None
        }
        pub fn register(self: &Arc<Self>, _: &std::process::Child) -> Option<Watch> {
            None
        }
        pub fn shutdown(&self) {}
    }
    impl Watch {
        pub fn healthy(&self) -> bool {
            false
        }
        pub fn ready(&self) -> bool {
            false
        }
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) use fallback::{Registry, Watch};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::{
        process::Command,
        sync::{mpsc, Arc},
        time::Duration,
    };
    #[test]
    fn owned_exit_is_not_reaped_and_observer_shutdown_is_signaled() {
        let (send, receive) = mpsc::channel();
        let (registry, thread) = Registry::new(Arc::new(move || {
            let _ = send.send(());
        }))
        .unwrap();
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let watch = registry.register(&child).unwrap();
        assert!(watch.healthy());
        assert!(!watch.ready());
        assert!(receive.recv_timeout(Duration::from_millis(120)).is_err());
        child.kill().unwrap();
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(watch.ready());
        // The observer must not steal status or reap the child's reserved PID.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            assert_eq!(
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT
                ),
                0
            );
            assert_eq!(info.si_pid() as u32, child.id());
        }
        drop(watch);
        assert!(!child.wait().unwrap().success());
        registry.shutdown();
        thread.join().unwrap();
    }
    #[test]
    fn cancellation_does_not_transfer_a_stale_exit_to_a_new_watch() {
        let (send, receive) = mpsc::channel();
        let (registry, thread) = Registry::new(Arc::new(move || {
            let _ = send.send(());
        }))
        .unwrap();
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let old = registry.register(&child).unwrap();
        drop(old);
        let current = registry.register(&child).unwrap();
        assert!(!current.ready());
        child.kill().unwrap();
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(current.ready());
        drop(current);
        child.wait().unwrap();
        // Shutdown is independent of any further process events.
        registry.shutdown();
        thread.join().unwrap();
    }
    #[test]
    fn observer_shutdown_does_not_wait_for_a_live_owned_child() {
        let (registry, thread) = Registry::new(Arc::new(|| {})).unwrap();
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let watch = registry.register(&child).unwrap();
        let started = std::time::Instant::now();
        registry.shutdown();
        thread.join().unwrap();
        let elapsed = started.elapsed();
        let still_alive = child.try_wait().unwrap().is_none();
        drop(watch);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            still_alive,
            "observer must never own termination or reaping"
        );
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn short_lived_children_have_readiness_or_keep_polling_fallback() {
        let (registry, thread) = Registry::new(Arc::new(|| {})).unwrap();
        for _ in 0..12 {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap();
            let watch = registry.register(&child);
            if let Some(watch) = &watch {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !watch.ready() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(watch.ready());
            }
            drop(watch);
            assert_eq!(child.wait().unwrap().code(), Some(7));
        }
        registry.shutdown();
        thread.join().unwrap();
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[path = "process_exit/portable.rs"]
mod portable;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use portable::{Registry, Watch};
