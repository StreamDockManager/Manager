//! Read-only logind transport for KDE early-lock and GNOME companion protocols on X11.
//! A fresh observer waits for positive active state followed by an observed
//! ActiveChanged(false); it never locks/unlocks/inhibits the desktop
//! Provider owners, session/display identity and the early-lock interface are
//! pinned before production admission. Native/device qualification is separate

use super::{Binding, Desktop, GuardState, KdeCycle, KdeStatus, Signal, Snapshot};
use futures_util::{FutureExt, Stream, StreamExt};
use std::{
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zbus::{Connection, MatchRule, Message, MessageStream, Proxy};

const LOGIND: &str = "org.freedesktop.login1";
const DESKTOP: &str = "org.freedesktop.ScreenSaver";
const SESSION: &str = "org.freedesktop.login1.Session";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const TIMEOUT: Duration = Duration::from_millis(500);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(180);

/// Session observations only. No keys or window titles
#[derive(Clone, Copy, Debug)]
pub(crate) struct Status {
    pub state: KdeStatus,
    pub generation: u64,
    pub snapshot_at: Option<Instant>,
}
const MAX_SNAPSHOT_AGE: Duration = Duration::from_millis(250);
/// Snapshot generation pinned by a transaction; never survives a session change
#[derive(Clone, Copy)]
pub(super) struct CandidateLease {
    generation: u64,
}
impl Status {
    // Issue a candidate lease only after a fresh observed unlock cycle
    pub(super) fn lease(&self, now: Instant) -> Result<CandidateLease, &'static str> {
        if self.state != KdeStatus::UnlockedAfterCycle {
            return Err("No observed unlock cycle");
        }
        let age = self
            .snapshot_at
            .and_then(|at| now.checked_duration_since(at))
            .ok_or("No fresh session snapshot")?;
        if age > MAX_SNAPSHOT_AGE {
            return Err("Session snapshot expired");
        }
        Ok(CandidateLease {
            generation: self.generation,
        })
    }
    // Check lease
    pub(super) fn check_lease(
        &self,
        lease: CandidateLease,
        now: Instant,
    ) -> Result<(), &'static str> {
        let current = self.lease(now)?;
        if current.generation != lease.generation {
            return Err("Session epoch changed");
        }
        Ok(())
    }
}
// Publish the current Linux session kde result
fn publish(state: &Arc<Mutex<Status>>, cycle: &KdeCycle, fresh_snapshot: bool) {
    if let Ok(mut status) = state.lock() {
        let snapshot_at = if fresh_snapshot {
            Some(Instant::now())
        } else if status.generation == cycle.generation {
            status.snapshot_at
        } else {
            None
        };
        *status = Status {
            state: cycle.status,
            generation: cycle.generation,
            snapshot_at,
        };
    }
}
struct RetireStatus(Arc<Mutex<Status>>);
impl Drop for RetireStatus {
    // Release this Linux session kde owner when it leaves scope
    fn drop(&mut self) {
        if let Ok(mut status) = self.0.lock() {
            status.state = KdeStatus::Unavailable;
            status.generation = status.generation.saturating_add(1);
        }
    }
}
// Resolve the unique D-Bus owner of the required service
async fn owner(connection: &Connection, name: &str) -> Result<String, String> {
    let proxy = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|_| "D-Bus unavailable")?;
    let name = zbus::names::BusName::try_from(name).map_err(|_| "Invalid service")?;
    proxy
        .get_name_owner(name)
        .await
        .map(|value| value.to_string())
        .map_err(|_| "Required service has no owner".into())
}
// Subscribe to a bounded signal stream from the pinned service owner
async fn stream(connection: &Connection, sender: &str) -> Result<MessageStream, String> {
    // One provider stream preserves ordering across its desktop interfaces
    // Only the pinned provider is observed; no unrelated bus signals or keys
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(sender)
        .build()
        .map_err(|_| "Invalid match")?;
    MessageStream::for_match_rule(rule, connection, NonZeroUsize::new(64))
        .await
        .map_err(|_| "Cannot subscribe to session status".into())
}
// Subscribe to ownership changes for the required D-Bus services
async fn owner_stream(connection: &Connection, service: &str) -> Result<MessageStream, String> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .interface("org.freedesktop.DBus")
        .member("NameOwnerChanged")
        .add_arg(service)
        .build()
        .map_err(|_| "Invalid owner match")?;
    MessageStream::for_match_rule(rule, connection, NonZeroUsize::new(16))
        .await
        .map_err(|_| "Cannot monitor provider lifetime".into())
}
// Return a snapshot of Linux session kde state
async fn snapshot(
    system: &Connection,
    session: &Connection,
    b: &Binding,
) -> Result<Snapshot, String> {
    if owner(system, LOGIND).await? != b.logind_owner
        || owner(session, b.desktop.service()).await? != b.desktop_owner
    {
        return Err("Session service owner changed".into());
    }
    if b.desktop == Desktop::GnomeGuard
        && owner(session, "org.gnome.Shell").await? != b.desktop_owner
    {
        return Err("GNOME Shell owner changed".into());
    }
    let m = Proxy::new(
        system,
        b.logind_owner.as_str(),
        "/org/freedesktop/login1",
        MANAGER,
    )
    .await
    .map_err(|_| "Session manager unavailable")?;
    let d = Proxy::new(
        session,
        b.desktop_owner.as_str(),
        b.desktop.path(),
        b.desktop.service(),
    )
    .await
    .map_err(|_| "ScreenSaver unavailable")?;
    // Property caching is intentionally not used as an unlocked-state authority
    let properties =
        zbus::fdo::PropertiesProxy::new(system, b.logind_owner.as_str(), b.session_path.as_str())
            .await
            .map_err(|_| "Properties unavailable")?;
    let values = properties
        .get_all(SESSION.try_into().map_err(|_| "Invalid interface")?)
        .await
        .map_err(|_| "Session snapshot unavailable")?;
    let boolean = |key: &str| -> Result<bool, String> {
        values
            .get(key)
            .ok_or_else(|| "Session property missing".to_string())
            .and_then(|v| bool::try_from(v).map_err(|_| "Session property malformed".into()))
    };
    let string = |key: &str| -> Result<String, String> {
        values
            .get(key)
            .ok_or_else(|| "Session property missing".to_string())
            .and_then(|v| {
                <&str>::try_from(v)
                    .map(str::to_owned)
                    .map_err(|_| "Session property malformed".into())
            })
    };
    let configured_display =
        std::env::var("DISPLAY").map_err(|_| "Local X11 display unavailable")?;
    let session_display = string("Display")?;
    if super::native::local_display(&configured_display).is_none()
        || super::native::local_display(&configured_display)
            != super::native::local_display(&session_display)
    {
        return Err("X11 display does not belong to the concrete login session".into());
    }
    let guard_state = if b.desktop == Desktop::GnomeGuard {
        let (version, instance, generation, protected): (u32, String, u64, bool) = d
            .call("GetState", &())
            .await
            .map_err(|_| "GNOME session companion is unavailable")?;
        if version != 1 {
            return Err("Unsupported GNOME session companion protocol".into());
        }
        Some(GuardState::parse(&instance, generation, protected)?)
    } else {
        None
    };
    let result = Snapshot {
        active: boolean("Active")?,
        user_class: string("Class")? == "user",
        x11: string("Type")? == "x11",
        remote: boolean("Remote")?,
        locked_hint: boolean("LockedHint")?,
        preparing_for_sleep: m
            .get_property("PreparingForSleep")
            .await
            .map_err(|_| "Sleep status unavailable")?,
        screensaver_active: if let Some(state) = guard_state {
            state.protected
        } else {
            d.call("GetActive", &())
                .await
                .map_err(|_| "ScreenSaver status unavailable")?
        },
        guard_state,
    };
    // Service ownership must remain pinned on both sides of the awaited reads
    if owner(system, LOGIND).await? != b.logind_owner
        || owner(session, b.desktop.service()).await? != b.desktop_owner
    {
        return Err("Session service owner changed during snapshot".into());
    }
    if b.desktop == Desktop::GnomeGuard
        && owner(session, "org.gnome.Shell").await? != b.desktop_owner
    {
        return Err("GNOME Shell owner changed during snapshot".into());
    }
    Ok(result)
}
// Signal a change to the Linux session kde worker
fn signal(message: &Message, desktop_bus: bool, b: &Binding) -> Result<Option<Signal>, String> {
    let h = message.header();
    let sender = h.sender().map(|s| s.as_str()).unwrap_or("");
    let interface = h.interface().map(|s| s.as_str()).unwrap_or("");
    let member = h.member().map(|s| s.as_str()).unwrap_or("");
    let path = h.path().map(|s| s.as_str()).unwrap_or("");
    if sender == "org.freedesktop.DBus"
        && interface == "org.freedesktop.DBus"
        && member == "NameOwnerChanged"
    {
        let (name, old, new): (String, String, String) = message
            .body()
            .deserialize()
            .map_err(|_| "Malformed owner change")?;
        let service = if desktop_bus {
            b.desktop.service()
        } else {
            LOGIND
        };
        if (name == service
            || (desktop_bus && b.desktop == Desktop::GnomeGuard && name == "org.gnome.Shell"))
            && old != new
        {
            return Ok(Some(Signal::ProviderLost));
        }
        return Ok(None);
    }
    let expected = if desktop_bus {
        &b.desktop_owner
    } else {
        &b.logind_owner
    };
    if sender != expected {
        return Ok(None);
    }
    let boolean = || {
        message
            .body()
            .deserialize::<(bool,)>()
            .map(|v| v.0)
            .map_err(|_| "Malformed session signal".into())
    };
    if desktop_bus
        && b.desktop == Desktop::GnomeGuard
        && path == b.desktop.path()
        && interface == b.desktop.service()
        && member == "StateChanged"
    {
        let (instance, generation, protected): (String, u64, bool) =
            message
                .body()
                .deserialize()
                .map_err(|_| "Malformed GNOME session state")?;
        return Ok(Some(Signal::GuardState(GuardState::parse(
            &instance, generation, protected,
        )?)));
    }
    if desktop_bus && b.desktop == Desktop::KdeScreenSaver && path == "/ScreenSaver" {
        return match (interface, member) {
            (DESKTOP, "ActiveChanged") => boolean().map(|v| Some(Signal::ScreenSaverActive(v))),
            ("org.kde.screensaver", "AboutToLock") => {
                message
                    .body()
                    .deserialize::<()>()
                    .map_err(|_| "Malformed lock precursor")?;
                Ok(Some(Signal::AboutToLock))
            }
            _ => Ok(None),
        };
    }
    if !desktop_bus {
        if path == "/org/freedesktop/login1" && interface == MANAGER && member == "SessionRemoved" {
            let (_, removed): (String, zbus::OwnedObjectPath) = message
                .body()
                .deserialize()
                .map_err(|_| "Malformed session removal")?;
            if removed.as_str() == b.session_path {
                return Ok(Some(Signal::ProviderLost));
            }
        }
        if path == b.session_path && interface == SESSION && member == "Lock" {
            return Ok(Some(Signal::LockRequested));
        }
        if path == "/org/freedesktop/login1" && interface == MANAGER && member == "PrepareForSleep"
        {
            return boolean().map(|v| Some(Signal::PrepareForSleep(v)));
        }
        if path == b.session_path
            && interface == "org.freedesktop.DBus.Properties"
            && member == "PropertiesChanged"
        {
            let (changed_interface, changed, invalidated): (
                String,
                std::collections::HashMap<String, zbus::OwnedVariant>,
                Vec<String>,
            ) = message
                .body()
                .deserialize()
                .map_err(|_| "Malformed session properties")?;
            if changed_interface != SESSION {
                return Ok(None);
            }
            const WATCHED: &[&str] =
                &["Active", "Class", "Type", "Remote", "LockedHint", "Display"];
            if invalidated
                .iter()
                .any(|key| WATCHED.contains(&key.as_str()))
            {
                return Ok(Some(Signal::ProviderLost));
            }
            for (key, value) in changed {
                let unsafe_value = match key.as_str() {
                    "Active" => !bool::try_from(&value).map_err(|_| "Malformed Active")?,
                    "LockedHint" | "Remote" => {
                        bool::try_from(&value).map_err(|_| "Malformed session flag")?
                    }
                    "Class" => <&str>::try_from(&value).map_err(|_| "Malformed Class")? != "user",
                    "Type" => <&str>::try_from(&value).map_err(|_| "Malformed Type")? != "x11",
                    // This identity was pinned at admission; even an apparently
                    // equivalent value must not silently rebind an active lease
                    "Display" => true,
                    _ => false,
                };
                if unsafe_value {
                    return Ok(Some(match key.as_str() {
                        "LockedHint" => Signal::LockedHint(true),
                        "Active" => Signal::SessionActive(false),
                        _ => Signal::ProviderLost,
                    }));
                }
            }
        }
    }
    Ok(None)
}

/// Consume already-delivered signals before publishing snapshot freshness. A
/// bounded drain is not an atomic cross-bus/X-server lock oracle: future signals
/// can still arrive. The caller also checks the pinned target and physical state
fn drain_pending<S>(
    stream: &mut S,
    desktop_bus: bool,
    binding: &Binding,
    cycle: &mut KdeCycle,
    budget: &mut usize,
) -> Result<(), String>
where
    S: Stream<Item = zbus::Result<Message>> + Unpin,
{
    while let Some(value) = stream.next().now_or_never() {
        *budget = budget
            .checked_sub(1)
            .ok_or("Session observer signal budget exceeded")?;
        let message = value
            .ok_or("Session signal stream ended")?
            .map_err(|_| "Session signal stream failed")?;
        if let Some(signal) = signal(&message, desktop_bus, binding)? {
            cycle.signal(signal);
        }
    }
    Ok(())
}
// Finalize snapshot
fn finish_snapshot<S, D>(
    system_stream: &mut S,
    session_stream: &mut D,
    binding: &Binding,
    cycle: &mut KdeCycle,
    snapshot: Snapshot,
    epoch_before_read: u64,
) -> Result<bool, String>
where
    S: Stream<Item = zbus::Result<Message>> + Unpin,
    D: Stream<Item = zbus::Result<Message>> + Unpin,
{
    let mut budget = 256;
    drain_pending(system_stream, false, binding, cycle, &mut budget)?;
    drain_pending(session_stream, true, binding, cycle, &mut budget)?;
    // A reply from an older epoch must not even seed the locked phase. A
    // newer AboutToLock/inactive/sleep signal may have deliberately cleared it;
    // replaying stale true here would let a later false fabricate an unlock cycle
    if cycle.generation != epoch_before_read {
        return Ok(false);
    }
    cycle.snapshot(snapshot);
    // A positive initial locked snapshot changes generation too, so another
    // snapshot is needed before advertising freshness for that new epoch
    Ok(cycle.generation == epoch_before_read)
}

/// Read-only diagnostic wrapper for the explicit KDE profile probe. Production
/// uses `observe_native` below with fresh-snapshot request barriers; this wrapper
/// never grants input readiness. Both paths verify the pinned KDE protocol
/// Dropping/cancelling closes owned buses and subscriptions. No inhibitors,
/// desktop transitions, capture hooks or synthesized input are used
#[cfg(feature = "native-session-probe")]
// Observe kde 65
pub(crate) async fn observe_kde_65(
    cancelled: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
) -> Result<(), String> {
    let _retire = RetireStatus(status.clone());
    let mut cycle = KdeCycle::default();
    publish(&status, &cycle, false);
    let result = run(
        &cancelled,
        &status,
        &mut cycle,
        None,
        Desktop::KdeScreenSaver,
    )
    .await;
    cycle.signal(Signal::ProviderLost);
    publish(&status, &cycle, false);
    result
}
/// Production entry point: read-only watcher plus explicit refresh barriers
pub(super) async fn observe_native(
    cancelled: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    refresh: Arc<super::native::Refresh>,
    desktop: Desktop,
) -> Result<(), String> {
    let _retire = RetireStatus(status.clone());
    let mut cycle = KdeCycle::default();
    publish(&status, &cycle, false);
    run(&cancelled, &status, &mut cycle, Some(&refresh), desktop).await
}
// Drive the Linux session kde operation
async fn run(
    cancelled: &AtomicBool,
    status: &Arc<Mutex<Status>>,
    cycle: &mut KdeCycle,
    refresh: Option<&super::native::Refresh>,
    desktop: Desktop,
) -> Result<(), String> {
    let setup = async {
        let system = Connection::system()
            .await
            .map_err(|_| "System bus unavailable")?;
        let session = Connection::session()
            .await
            .map_err(|_| "Session bus unavailable")?;
        let logind_owner = owner(&system, LOGIND).await?;
        let desktop_owner = owner(&session, desktop.service()).await.map_err(|_| {
            if desktop == Desktop::GnomeGuard {
                "GNOME session protection requires the optional Stream Dock Manager Session Guard extension. Install and enable it explicitly, then retry.".to_string()
            } else { "Required desktop session service has no owner".to_string() }
        })?;
        if desktop == Desktop::GnomeGuard
            && owner(&session, "org.gnome.Shell").await? != desktop_owner
        {
            return Err("GNOME session companion is not owned by the running Shell".into());
        }
        let manager = Proxy::new(
            &system,
            logind_owner.as_str(),
            "/org/freedesktop/login1",
            MANAGER,
        )
        .await
        .map_err(|_| "Session manager unavailable")?;
        let path: zbus::OwnedObjectPath = manager
            .call("GetSessionByPID", &(std::process::id(),))
            .await
            .map_err(|_| "No concrete login session")?;
        drop(manager);
        let b = Binding {
            session_path: path.to_string(),
            logind_owner,
            desktop_owner,
            desktop,
        };
        if !b.valid() {
            return Err("Invalid session binding".into());
        }
        if desktop == Desktop::KdeScreenSaver {
            // Require KDE's early-lock protocol from the same pinned owner, not a
            // generic ScreenSaver service or an environment string alone
            let introspection = Proxy::new(
                &session,
                b.desktop_owner.as_str(),
                "/ScreenSaver",
                "org.freedesktop.DBus.Introspectable",
            )
            .await
            .map_err(|_| "KDE ScreenSaver protocol unavailable")?;
            let xml: String = introspection
                .call("Introspect", &())
                .await
                .map_err(|_| "KDE ScreenSaver protocol unavailable")?;
            let interface = xml
                .split("<interface name=\"org.kde.screensaver\">")
                .nth(1)
                .and_then(|value| value.split("</interface>").next());
            if !interface.is_some_and(|value| value.contains("<signal name=\"AboutToLock\"")) {
                return Err(
                    "The desktop does not provide the supported KDE early-lock protocol".into(),
                );
            }
            drop(introspection);
        }
        // Subscriptions precede snapshot; only KDE false needs an observed unlock cycle
        let mut system_stream = futures_util::stream::select(
            stream(&system, &b.logind_owner).await?,
            owner_stream(&system, LOGIND).await?,
        );
        let shell_owner = if desktop == Desktop::GnomeGuard {
            Some(owner_stream(&session, "org.gnome.Shell").await?)
        } else {
            None
        };
        let extra = futures_util::stream::iter(shell_owner).flatten();
        let mut session_stream = futures_util::stream::select(
            stream(&session, &b.desktop_owner).await?,
            futures_util::stream::select(owner_stream(&session, b.desktop.service()).await?, extra),
        );
        let epoch_before_read = cycle.generation;
        let initial = snapshot(&system, &session, &b).await?;
        if !initial.user_class || !initial.x11 || initial.remote {
            return Err("Only local user X11 sessions are supported".into());
        }
        finish_snapshot(
            &mut system_stream,
            &mut session_stream,
            &b,
            cycle,
            initial,
            epoch_before_read,
        )?;
        publish(status, cycle, false);
        Ok::<_, String>((system, session, system_stream, session_stream, b))
    };
    let (system, session, mut system_stream, mut session_stream, binding) =
        tokio::time::timeout(TIMEOUT, setup)
            .await
            .map_err(|_| "Session setup timed out")??;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut messages_since_snapshot = 0usize;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let incoming = tokio::select! {
            value = system_stream.next() => Some((false, value)),
            value = session_stream.next() => Some((true, value)),
            _ = tick.tick() => None,
            _ = async { match refresh { Some(control) => control.notify.notified().await, None => std::future::pending::<()>().await } } => None,
        };
        let mut fresh_snapshot = false;
        if let Some((desktop_bus, value)) = incoming {
            messages_since_snapshot += 1;
            if messages_since_snapshot > 256 {
                return Err("Session observer signal budget exceeded".into());
            }
            let message = value
                .ok_or("Session signal stream ended")?
                .map_err(|_| "Session signal stream failed")?;
            if let Some(signal) = signal(&message, desktop_bus, &binding)? {
                cycle.signal(signal);
            }
        } else {
            let epoch_before_read = cycle.generation;
            let request = refresh
                .map(|control| control.requested.load(Ordering::Acquire))
                .unwrap_or(0);
            let read =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, snapshot(&system, &session, &binding));
            tokio::pin!(read);
            let s = loop {
                // Do not defer lock/owner revocations behind a slow D-Bus read
                let incoming = tokio::select! {
                    result = &mut read => break result.map_err(|_| "Session status timed out")??,
                    value = system_stream.next() => (false, value),
                    value = session_stream.next() => (true, value),
                };
                messages_since_snapshot += 1;
                if messages_since_snapshot > 256 {
                    return Err("Session observer signal budget exceeded".into());
                }
                let message = incoming
                    .1
                    .ok_or("Session signal stream ended")?
                    .map_err(|_| "Session signal stream failed")?;
                if let Some(signal) = signal(&message, incoming.0, &binding)? {
                    cycle.signal(signal);
                }
                publish(status, cycle, false);
                if cycle.status == KdeStatus::Unavailable {
                    return Err("Session provider changed or disappeared".into());
                }
            };
            fresh_snapshot = finish_snapshot(
                &mut system_stream,
                &mut session_stream,
                &binding,
                cycle,
                s,
                epoch_before_read,
            )?;
            publish(status, cycle, fresh_snapshot);
            if fresh_snapshot {
                if let Some(control) = refresh {
                    control.complete(request);
                }
            }
            messages_since_snapshot = 0;
        }
        publish(status, cycle, fresh_snapshot);
        if cycle.status == KdeStatus::Unavailable {
            return Err("Session provider changed or disappeared".into());
        }
    }
}

/// Explicit read-only qualification helper. The chosen profile is a declaration
/// to be independently checked on the test machine, not a runtime override
#[cfg(feature = "native-session-probe")]
// Run probe
pub(crate) fn run_probe(profile: &str, duration: Duration) -> Result<(), String> {
    if profile != "kde-plasma-6.5-x11" || !(1..=600).contains(&duration.as_secs()) {
        return Err("Use --profile kde-plasma-6.5-x11 --seconds 1..600 on that exact desktop; this probe never enables native input.".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start session diagnostic runtime")?;
    runtime.block_on(async {
        let cancelled = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status { state: KdeStatus::AwaitingUnlockCycle,
            generation: 0, snapshot_at: None }));
        let mut worker = tokio::spawn(observe_kde_65(cancelled.clone(), status.clone()));
        let start = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let deadline = tokio::time::sleep(duration);
        tokio::pin!(deadline);
        let mut lease = None;
        let mut worker_completed = false;
        let result = loop {
            tokio::select! {
                result = &mut worker => {
                    worker_completed = true;
                    break result.map_err(|_| "Session observer task failed".to_string())?;
                },
                _ = &mut deadline => break Ok(()),
                _ = tick.tick() => {
                    let state = *status.lock().map_err(|_| "Session status unavailable")?;
                    let now = Instant::now();
                    let previous_lease_valid = lease.is_some_and(|value| state.check_lease(value, now).is_ok());
                    lease = state.lease(now).ok();
                    println!("{}", serde_json::json!({
                        "profile": profile, "elapsed_ms": start.elapsed().as_millis(),
                        "state": format!("{:?}", state.state), "generation": state.generation,
                        "candidate_fresh": lease.is_some(), "previous_epoch_lease_valid": previous_lease_valid,
                        "native_ready": false,
                    }));
                }
            }
        };
        cancelled.store(true, Ordering::Release);
        if !worker_completed {
            match tokio::time::timeout(Duration::from_secs(2), &mut worker).await {
                Ok(joined) => joined.map_err(|_| "Session observer cleanup failed")??,
                Err(_) => {
                    worker.abort();
                    let _ = worker.await;
                    return Err("Session observer cleanup timed out".into());
                }
            }
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Resolve and pin the login and desktop service identities
    fn binding() -> Binding {
        Binding {
            session_path: "/org/freedesktop/login1/session/_32".into(),
            logind_owner: ":1.2".into(),
            desktop_owner: ":1.9".into(),
            desktop: Desktop::KdeScreenSaver,
        }
    }
    #[test]
    // Verify that gnome companion signals pin instance path and shell owner
    fn gnome_companion_signals_pin_instance_path_and_shell_owner() {
        let mut b = binding();
        b.desktop = Desktop::GnomeGuard;
        let message = Message::signal(b.desktop.path(), b.desktop.service(), "StateChanged")
            .sender(b.desktop_owner.as_str())
            .build(&("12345678-1234-1234-1234-123456789abc", 2u64, true))
            .unwrap();
        assert!(
            matches!(signal(&message, true, &b).unwrap(), Some(Signal::GuardState(state)) if state.protected)
        );
        let replacement = Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
        )
        .sender("org.freedesktop.DBus")
        .build(&("org.gnome.Shell", ":1.9", ":1.10"))
        .unwrap();
        assert!(matches!(
            signal(&replacement, true, &b).unwrap(),
            Some(Signal::ProviderLost)
        ));
    }
    #[test]
    // Verify that queued revocation during snapshot is drained before freshness
    fn queued_revocation_during_snapshot_is_drained_before_freshness() {
        for owner_changed in [false, true] {
            let mut cycle = KdeCycle::default();
            cycle.signal(Signal::ScreenSaverActive(true));
            cycle.signal(Signal::ScreenSaverActive(false));
            let epoch = cycle.generation;
            let queued = if owner_changed {
                Message::signal(
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "NameOwnerChanged",
                )
                .sender("org.freedesktop.DBus")
                .build(&(DESKTOP, ":1.9", ":1.10"))
                .unwrap()
            } else {
                Message::signal("/ScreenSaver", "org.kde.screensaver", "AboutToLock")
                    .sender(":1.9")
                    .build(&())
                    .unwrap()
            };
            let mut system = futures_util::stream::pending();
            let mut desktop =
                futures_util::stream::iter([Ok(queued)]).chain(futures_util::stream::pending());
            let snapshot = Snapshot {
                active: true,
                user_class: true,
                x11: true,
                remote: false,
                locked_hint: false,
                preparing_for_sleep: false,
                screensaver_active: false,
                guard_state: None,
            };
            let fresh = finish_snapshot(
                &mut system,
                &mut desktop,
                &binding(),
                &mut cycle,
                snapshot,
                epoch,
            )
            .unwrap();
            assert!(!fresh);
            let status = Arc::new(Mutex::new(Status {
                state: KdeStatus::UnlockedAfterCycle,
                generation: epoch,
                snapshot_at: None,
            }));
            publish(&status, &cycle, fresh);
            assert!(status.lock().unwrap().lease(Instant::now()).is_err());
            assert_eq!(
                cycle.status,
                if owner_changed {
                    KdeStatus::Unavailable
                } else {
                    KdeStatus::Protected
                }
            );
        }
    }
    #[test]
    // Verify that stale locked snapshot never reseeds phase cleared by newer revocation
    fn stale_locked_snapshot_never_reseeds_phase_cleared_by_newer_revocation() {
        for signal in [
            Signal::AboutToLock,
            Signal::SessionActive(false),
            Signal::PrepareForSleep(true),
        ] {
            let mut cycle = KdeCycle::default();
            cycle.signal(Signal::ScreenSaverActive(true));
            let epoch_before_read = cycle.generation;
            // The async reader applies signals immediately while awaiting the
            // reply. A completed old unlock followed by a new lock attempt must
            // not be turned back into an observed locked phase by the old reply
            cycle.signal(Signal::ScreenSaverActive(false));
            cycle.signal(signal);
            let mut system = futures_util::stream::pending();
            let mut desktop = futures_util::stream::pending();
            let stale = Snapshot {
                active: true,
                user_class: true,
                x11: true,
                remote: false,
                locked_hint: false,
                preparing_for_sleep: false,
                screensaver_active: true,
                guard_state: None,
            };
            assert!(!finish_snapshot(
                &mut system,
                &mut desktop,
                &binding(),
                &mut cycle,
                stale,
                epoch_before_read
            )
            .unwrap());
            cycle.signal(Signal::ScreenSaverActive(false));
            assert_eq!(cycle.status, KdeStatus::AwaitingUnlockCycle);
        }
    }
    #[test]
    // Verify that initial snapshot drain cannot seed phase after queued early lock
    fn initial_snapshot_drain_cannot_seed_phase_after_queued_early_lock() {
        let mut cycle = KdeCycle::default();
        let epoch_before_read = cycle.generation;
        let queued = Message::signal("/ScreenSaver", "org.kde.screensaver", "AboutToLock")
            .sender(":1.9")
            .build(&())
            .unwrap();
        let mut system = futures_util::stream::pending();
        let mut desktop =
            futures_util::stream::iter([Ok(queued)]).chain(futures_util::stream::pending());
        let stale = Snapshot {
            active: true,
            user_class: true,
            x11: true,
            remote: false,
            locked_hint: false,
            preparing_for_sleep: false,
            screensaver_active: true,
            guard_state: None,
        };
        assert!(!finish_snapshot(
            &mut system,
            &mut desktop,
            &binding(),
            &mut cycle,
            stale,
            epoch_before_read
        )
        .unwrap());
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::AwaitingUnlockCycle);
    }
    #[test]
    // Verify that diagnostic leases reject stale snapshots epoch changes and clock regression
    fn diagnostic_leases_reject_stale_snapshots_epoch_changes_and_clock_regression() {
        let now = Instant::now();
        let mut status = Status {
            state: KdeStatus::UnlockedAfterCycle,
            generation: 3,
            snapshot_at: Some(now),
        };
        let lease = status.lease(now).unwrap();
        assert!(status
            .check_lease(lease, now + Duration::from_millis(250))
            .is_ok());
        assert!(status
            .check_lease(lease, now + Duration::from_millis(251))
            .is_err());
        assert!(status.lease(now - Duration::from_millis(1)).is_err());
        status.generation += 1;
        assert!(status.check_lease(lease, now).is_err());
        status.snapshot_at = None;
        assert!(status.lease(now).is_err());
        status.snapshot_at = Some(now);
        status.state = KdeStatus::Protected;
        assert!(status.lease(now).is_err());
    }
    #[test]
    // Verify that routes only pinned sender path and interface
    fn routes_only_pinned_sender_path_and_interface() {
        for (sender, path, accepted) in [
            (":1.9", "/ScreenSaver", true),
            (":1.8", "/ScreenSaver", false),
            (":1.9", "/Other", false),
        ] {
            let message = Message::signal(path, DESKTOP, "ActiveChanged")
                .sender(sender)
                .build(&(true,))
                .unwrap();
            assert_eq!(
                signal(&message, true, &binding()).unwrap().is_some(),
                accepted
            );
        }
        let message = Message::signal("/ScreenSaver", "org.kde.screensaver", "AboutToLock")
            .sender(":1.9")
            .build(&())
            .unwrap();
        assert!(matches!(
            signal(&message, true, &binding()).unwrap(),
            Some(Signal::AboutToLock)
        ));
        let malformed = Message::signal("/ScreenSaver", DESKTOP, "ActiveChanged")
            .sender(":1.9")
            .build(&("false",))
            .unwrap();
        assert!(signal(&malformed, true, &binding()).is_err());
    }
    #[test]
    // Verify that session properties cannot restore candidate and invalidations revoke
    fn session_properties_cannot_restore_candidate_and_invalidations_revoke() {
        let property = |value: bool, invalidated: Vec<String>| {
            let mut changed = std::collections::HashMap::<String, zbus::OwnedVariant>::new();
            changed.insert("LockedHint".into(), value.into());
            Message::signal(
                binding().session_path,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
            )
            .sender(":1.2")
            .build(&(SESSION, changed, invalidated))
            .unwrap()
        };
        assert!(signal(&property(false, vec![]), false, &binding())
            .unwrap()
            .is_none());
        assert!(matches!(
            signal(&property(true, vec![]), false, &binding()).unwrap(),
            Some(Signal::LockedHint(true))
        ));
        assert!(matches!(
            signal(&property(false, vec!["Active".into()]), false, &binding()).unwrap(),
            Some(Signal::ProviderLost)
        ));
    }
    #[test]
    // Verify that display changes and invalidations revoke pinned session identity
    fn display_changes_and_invalidations_revoke_pinned_session_identity() {
        for invalidated in [false, true] {
            let mut changed = std::collections::HashMap::<String, zbus::OwnedVariant>::new();
            let mut invalidations = Vec::<String>::new();
            if invalidated {
                invalidations.push("Display".into());
            } else {
                changed.insert("Display".into(), zbus::Str::from(":0").into());
            }
            let message = Message::signal(
                binding().session_path,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
            )
            .sender(":1.2")
            .build(&(SESSION, changed, invalidations))
            .unwrap();
            assert!(matches!(
                signal(&message, false, &binding()).unwrap(),
                Some(Signal::ProviderLost)
            ));
        }
    }
    #[test]
    // Verify that drop retires published candidate without native calls
    fn drop_retires_published_candidate_without_native_calls() {
        let status = Arc::new(Mutex::new(Status {
            state: KdeStatus::UnlockedAfterCycle,
            generation: 3,
            snapshot_at: None,
        }));
        drop(RetireStatus(status.clone()));
        let state = status.lock().unwrap();
        assert_eq!(state.state, KdeStatus::Unavailable);
        assert_eq!(state.generation, 4);
    }
}
