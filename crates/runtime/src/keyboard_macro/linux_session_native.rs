//! Production bridge for supported KDE/GNOME X11 session protocols. Only read-only
//! D-Bus subscriptions persist across transactions, to establish initial state
//! Each input transaction pins an observer instance and generation; reconnects
//! never revive an old lease. No keyboard events are observed by this module
use super::{kde, Desktop, KdeStatus};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const BARRIER_TIMEOUT: Duration = Duration::from_millis(220);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const BOOTSTRAP: &str = "KDE session state is not established. Lock and unlock once after starting Manager, then retry. Manager never locks the desktop for you.";

pub(super) struct Refresh {
    pub requested: AtomicU64,
    pub completed: AtomicU64,
    pub notify: tokio::sync::Notify,
    changed: Condvar,
}
impl Default for Refresh {
    // Construct the default Linux session native value
    fn default() -> Self {
        Self {
            requested: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            notify: tokio::sync::Notify::new(),
            changed: Condvar::new(),
        }
    }
}
impl Refresh {
    // Complete the pending Linux session native operation
    pub(super) fn complete(&self, request: u64) {
        self.completed.store(request, Ordering::Release);
        self.changed.notify_all();
    }
}
struct Service {
    desktop: Desktop,
    status: Arc<Mutex<kde::Status>>,
    refresh: Arc<Refresh>,
    failed: AtomicBool,
    error: Mutex<Option<String>>,
    started: Instant,
}
impl Service {
    // Construct the Linux session native error result
    fn error(&self) -> String {
        self.error
            .try_lock()
            .ok()
            .and_then(|error| error.clone())
            .unwrap_or_else(|| "Desktop session observer is unavailable".into())
    }
    // Reject protected or unobserved session states with the admission reason
    fn state_reason(&self, state: KdeStatus) -> Result<(), String> {
        if self.failed.load(Ordering::Acquire) {
            return Err(self.error());
        }
        match state {
            KdeStatus::UnlockedAfterCycle => Ok(()),
            KdeStatus::AwaitingUnlockCycle => Err(if self.desktop == Desktop::GnomeGuard {
                "Waiting for the GNOME Session Guard extension. Install and enable the optional companion explicitly if it is unavailable.".into()
            } else {
                BOOTSTRAP.into()
            }),
            KdeStatus::Protected => Err(
                "Desktop session is locked, inactive or preparing for sleep. Unlock and retry."
                    .into(),
            ),
            KdeStatus::Unavailable => Err(self.error()),
        }
    }
    // Request a fresh session snapshot and await a bounded candidate lease
    fn fresh(&self) -> Result<kde::CandidateLease, String> {
        let request = self
            .refresh
            .requested
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "Session request counter exhausted")?
            + 1;
        self.refresh.notify.notify_one();
        let deadline = Instant::now() + BARRIER_TIMEOUT;
        let mut status = self
            .status
            .lock()
            .map_err(|_| "Session status unavailable")?;
        loop {
            self.state_reason(status.state)?;
            if self.refresh.completed.load(Ordering::Acquire) >= request {
                return status.lease(Instant::now()).map_err(str::to_owned);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Desktop session verification timed out".into());
            }
            // Short bounded waits also handle a notification between the atomic
            // check and wait. No async runtime is blocked by the observer itself
            status = self
                .refresh
                .changed
                .wait_timeout(status, remaining.min(Duration::from_millis(10)))
                .map_err(|_| "Session status unavailable")?
                .0;
        }
    }
}

// Select a supported X11 desktop and reject unsupported session environments
fn environment() -> Result<Desktop, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind != "x11")
    {
        return Err("Native macro session admission currently requires a supported X11 desktop; the companion does not enable Wayland keyboard macros.".into());
    }
    let names = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktop = if names
        .split(':')
        .any(|part| part.eq_ignore_ascii_case("KDE"))
    {
        Desktop::KdeScreenSaver
    } else if names
        .split(':')
        .any(|part| part.eq_ignore_ascii_case("GNOME"))
    {
        Desktop::GnomeGuard
    } else {
        return Err("Native keyboard macros support KDE X11 or GNOME X11 with the optional Session Guard companion; this desktop has no supported session provider.".into());
    };
    if std::env::var("DISPLAY")
        .ok()
        .and_then(|value| local_display(&value))
        .is_none()
    {
        return Err(
            "Keyboard macros need a local X11 display belonging to this login session.".into(),
        );
    }
    Ok(desktop)
}
/// Compare server identity, not screen number. Remote/forwarded displays are not
/// covered by the local logind session even when Xauthority permits connection
pub(super) fn local_display(value: &str) -> Option<u32> {
    let value = value
        .strip_prefix("unix:")
        .or_else(|| value.strip_prefix(':'))?;
    let (server, screen) = value
        .split_once('.')
        .map_or((value, None), |(server, screen)| (server, Some(screen)));
    if server.is_empty()
        || !server.bytes().all(|b| b.is_ascii_digit())
        || screen.is_some_and(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    server.parse().ok()
}
// Reuse or restart the desktop session observer with its retry delay
fn service() -> Result<Arc<Service>, String> {
    let desktop = environment()?;
    static SERVICE: OnceLock<Mutex<Option<Arc<Service>>>> = OnceLock::new();
    let mut slot = SERVICE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Session service unavailable")?;
    if let Some(service) = slot.as_ref() {
        if service.desktop != desktop {
            service.failed.store(true, Ordering::Release);
        }
        if service.desktop == desktop
            && (!service.failed.load(Ordering::Acquire) || service.started.elapsed() < RETRY_DELAY)
        {
            return Ok(service.clone());
        }
    }
    let service = Arc::new(Service {
        desktop,
        status: Arc::new(Mutex::new(kde::Status {
            state: KdeStatus::AwaitingUnlockCycle,
            generation: 0,
            snapshot_at: None,
        })),
        refresh: Arc::new(Refresh::default()),
        failed: AtomicBool::new(false),
        error: Mutex::new(None),
        started: Instant::now(),
    });
    let worker = service.clone();
    std::thread::Builder::new()
        .name("keyboard-session".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Cannot start desktop session observer".to_string())
                .and_then(|runtime| {
                    runtime.block_on(kde::observe_native(
                        Arc::new(AtomicBool::new(false)),
                        worker.status.clone(),
                        worker.refresh.clone(),
                        desktop,
                    ))
                });
            if let Ok(mut error) = worker.error.lock() {
                *error = Some(
                    result
                        .err()
                        .unwrap_or_else(|| "Desktop session observer stopped".into()),
                );
            }
            worker.failed.store(true, Ordering::Release);
            worker.refresh.changed.notify_all();
        })
        .map_err(|_| "Cannot start desktop session observer")?;
    *slot = Some(service.clone());
    Ok(service)
}

/// Read-only capability check. Starts no keyboard hooks or native output
pub(crate) fn readiness() -> Result<(), String> {
    let service = service()?;
    let status = service
        .status
        .lock()
        .map_err(|_| "Session status unavailable")?;
    service.state_reason(status.state)?;
    status
        .lease(Instant::now())
        .map(|_| ())
        .map_err(str::to_owned)
}

pub(crate) struct Lease {
    service: Arc<Service>,
    epoch: kde::CandidateLease,
    revoked: AtomicBool,
}
// Acquire the owned Linux session native resource
pub(crate) fn acquire() -> Result<Lease, String> {
    let service = service()?;
    let epoch = service.fresh()?;
    Ok(Lease {
        service,
        epoch,
        revoked: AtomicBool::new(false),
    })
}
impl Lease {
    /// Bounded roundtrip barrier. Call outside the global native-input mutex
    pub(crate) fn check(&self) -> Result<(), String> {
        let result = (|| {
            if self.revoked.load(Ordering::Acquire) {
                return Err("Keyboard session lease was revoked".into());
            }
            self.service.fresh()?;
            self.check_current()
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        result
    }
    /// Final nonblocking freshness/generation veto, safe under the input mutex
    pub(crate) fn check_current(&self) -> Result<(), String> {
        let result = (|| {
            if self.revoked.load(Ordering::Acquire) {
                return Err("Keyboard session lease was revoked".into());
            }
            let status = self
                .service
                .status
                .try_lock()
                .map_err(|_| "Session status is being updated")?;
            self.service.state_reason(status.state)?;
            status
                .check_lease(self.epoch, Instant::now())
                .map_err(str::to_owned)
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct a fresh unlocked session service for the test
    fn observed_service() -> Arc<Service> {
        Arc::new(Service {
            desktop: Desktop::KdeScreenSaver,
            status: Arc::new(Mutex::new(kde::Status {
                state: KdeStatus::UnlockedAfterCycle,
                generation: 7,
                snapshot_at: Some(Instant::now()),
            })),
            refresh: Arc::new(Refresh::default()),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
            started: Instant::now(),
        })
    }
    #[test]
    // Verify that a revoked generation never rearms even after unlock
    fn a_revoked_generation_never_rearms_even_after_unlock() {
        let service = observed_service();
        let epoch = service
            .status
            .lock()
            .unwrap()
            .lease(Instant::now())
            .unwrap();
        let lease = Lease {
            service: service.clone(),
            epoch,
            revoked: AtomicBool::new(false),
        };
        assert!(lease.check_current().is_ok());
        service.status.lock().unwrap().generation += 1;
        assert!(lease.check_current().is_err());
        // Even malformed state rollback cannot revive a lease already rejected
        service.status.lock().unwrap().generation -= 1;
        assert!(lease.check_current().is_err());
    }
    #[test]
    // Verify that no native input is needed to fail closed on contention or provider loss
    fn no_native_input_is_needed_to_fail_closed_on_contention_or_provider_loss() {
        let service = observed_service();
        let epoch = service
            .status
            .lock()
            .unwrap()
            .lease(Instant::now())
            .unwrap();
        let lease = Lease {
            service: service.clone(),
            epoch,
            revoked: AtomicBool::new(false),
        };
        let held = service.status.lock().unwrap();
        assert!(lease.check_current().is_err());
        drop(held);
        assert!(lease.check_current().is_err());
        service.failed.store(true, Ordering::Release);
        assert!(service.state_reason(KdeStatus::UnlockedAfterCycle).is_err());
    }
    #[test]
    // Verify that only local display servers match session identity
    fn only_local_display_servers_match_session_identity() {
        assert_eq!(local_display(":0"), Some(0));
        assert_eq!(local_display(":0.1"), Some(0));
        assert_eq!(local_display("unix:12.0"), Some(12));
        for invalid in ["localhost:10.0", "remote:0", ":", ":0.", ":0.-1", ":0.1.2"] {
            assert_eq!(local_display(invalid), None);
        }
    }
}
