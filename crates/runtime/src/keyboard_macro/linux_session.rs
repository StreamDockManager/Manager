//! Scoped Linux desktop-session state and generation leases
//!
//! KDE's GetActive excludes AcquiringLock, so an initial false result is unknown
//! The read-only native watcher subscribes before snapshotting and learns a full
//! active-to-inactive cycle before admission. AboutToLock, logind, provider loss,
//! stale snapshots and transport failures revoke existing transaction leases
//! GNOME uses an optional versioned Shell companion with main-loop snapshots,
//! per-enable instance identity and monotonic state generations instead of the
//! delayed public ScreenSaver animation signal. No input hook, inhibitor or
//! desktop transition is created by readiness
//!
//! Sources: freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html;
//! github.com/KDE/kscreenlocker/blob/Plasma/6.5/ksldapp.cpp;
//! github.com/KDE/kscreenlocker/blob/Plasma/6.5/dbus/org.kde.screensaver.xml

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desktop {
    #[cfg(test)]
    GnomeScreenShield,
    KdeScreenSaver,
    GnomeGuard,
}
impl Desktop {
    // Return the D-Bus service name for the selected desktop observer
    pub(crate) fn service(self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::GnomeScreenShield => "org.gnome.Shell.ScreenShield",
            Self::KdeScreenSaver => "org.freedesktop.ScreenSaver",
            Self::GnomeGuard => "org.streamdockmanager.SessionGuard1",
        }
    }
    // Return the D-Bus object path for the selected desktop observer
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::GnomeGuard => "/org/streamdockmanager/SessionGuard1",
            _ => "/ScreenSaver",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub session_path: String,
    pub logind_owner: String,
    pub desktop_owner: String,
    pub desktop: Desktop,
}
impl Binding {
    // Report the valid condition for Linux session
    fn valid(&self) -> bool {
        let session = self
            .session_path
            .strip_prefix("/org/freedesktop/login1/session/");
        let concrete = session.is_some_and(|id| {
            !id.is_empty()
                && !matches!(id, "self" | "auto")
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        // Report the unique owner condition for Linux session
        fn unique_owner(value: &str) -> bool {
            let Some(value) = value.strip_prefix(':') else {
                return false;
            };
            value.split_once('.').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
            })
        }
        concrete && unique_owner(&self.logind_owner) && unique_owner(&self.desktop_owner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GuardState {
    instance: [u64; 2],
    generation: u64,
    pub protected: bool,
}
impl GuardState {
    // Parse the supplied Linux session representation
    pub(crate) fn parse(instance: &str, generation: u64, protected: bool) -> Result<Self, String> {
        if instance.len() != 36
            || !instance.bytes().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
        {
            return Err("Malformed GNOME session instance".into());
        }
        let digits = instance.replace('-', "");
        let half = |s| {
            u64::from_str_radix(s, 16).map_err(|_| "Malformed GNOME session instance".to_string())
        };
        let instance = [half(&digits[..16])?, half(&digits[16..])?];
        if instance == [0, 0] || generation == 0 {
            return Err("Invalid GNOME session epoch".into());
        }
        Ok(Self {
            instance,
            generation,
            protected,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Snapshot {
    pub active: bool,
    pub user_class: bool,
    pub x11: bool,
    pub remote: bool,
    pub locked_hint: bool,
    pub preparing_for_sleep: bool,
    pub screensaver_active: bool,
    pub guard_state: Option<GuardState>,
}
impl Snapshot {
    // Report the unsafe session condition for Linux session
    fn unsafe_session(self) -> bool {
        !self.active
            || !self.user_class
            || !self.x11
            || self.remote
            || self.locked_hint
            || self.preparing_for_sleep
            || self.screensaver_active
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    InvalidBinding,
    ProtectedSession,
    ProviderLost,
    Stale,
    UnknownBootstrap,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Signal {
    /// logind Lock is a request, sufficient to stop, never evidence of unlocking
    LockRequested,
    PrepareForSleep(bool),
    SessionActive(bool),
    LockedHint(bool),
    /// GNOME ScreenShield / KDE ScreenSaver ActiveChanged
    ScreenSaverActive(bool),
    /// KDE publishes this before attempting its input grab
    AboutToLock,
    /// Versioned companion state, sampled on the compositor main loop
    GuardState(GuardState),
    /// Also used for invalidated properties, decode failures and queue overflow
    ProviderLost,
}

#[cfg(test)]
pub(crate) struct Observer {
    binding: Binding,
    failed: Option<Failure>,
    last_snapshot_ms: u64,
}
#[cfg(test)]
impl Observer {
    /// Called only after the adapter acknowledges all subscriptions and checks
    /// unchanged unique owners around its initial snapshot. This pure candidate
    /// cannot verify those transport obligations or bootstrap safety itself
    pub(crate) fn new(binding: Binding, snapshot: Snapshot, now_ms: u64) -> Result<Self, Failure> {
        if !binding.valid() {
            return Err(Failure::InvalidBinding);
        }
        if snapshot.unsafe_session() {
            return Err(Failure::ProtectedSession);
        }
        Ok(Self {
            binding,
            failed: None,
            last_snapshot_ms: now_ms,
        })
    }
    // Mark the Linux session operation as failed
    fn fail(&mut self, reason: Failure) {
        // Preserve the first cause; a later false/unlock snapshot cannot rearm
        self.failed.get_or_insert(reason);
    }
    // Refresh the Linux session observation
    pub(crate) fn refresh(&mut self, binding: &Binding, snapshot: Snapshot, now_ms: u64) {
        if binding != &self.binding {
            self.fail(Failure::ProviderLost);
        }
        if now_ms < self.last_snapshot_ms {
            self.fail(Failure::Stale);
        }
        if snapshot.unsafe_session() {
            self.fail(Failure::ProtectedSession);
        }
        self.last_snapshot_ms = now_ms;
    }
    // Signal a change to the Linux session worker
    pub(crate) fn signal(&mut self, sender: &str, signal: Signal) {
        let desktop_signal = matches!(
            signal,
            Signal::ScreenSaverActive(_) | Signal::AboutToLock | Signal::GuardState(_)
        );
        let owner = if desktop_signal {
            &self.binding.desktop_owner
        } else {
            &self.binding.logind_owner
        };
        if sender != owner {
            self.fail(Failure::ProviderLost);
            return;
        }
        match signal {
            Signal::LockRequested
            | Signal::PrepareForSleep(true)
            | Signal::SessionActive(false)
            | Signal::LockedHint(true)
            | Signal::ScreenSaverActive(true) => self.fail(Failure::ProtectedSession),
            Signal::AboutToLock if self.binding.desktop == Desktop::KdeScreenSaver => {
                self.fail(Failure::ProtectedSession)
            }
            Signal::AboutToLock | Signal::ProviderLost | Signal::GuardState(_) => {
                self.fail(Failure::ProviderLost)
            }
            // Positive observations cannot undo revocation or establish bootstrap
            _ => (),
        }
    }
    // Check the current Linux session conditions
    pub(crate) fn check(&mut self, now_ms: u64, max_age_ms: u64) -> Result<(), Failure> {
        if now_ms < self.last_snapshot_ms || now_ms - self.last_snapshot_ms > max_age_ms {
            self.fail(Failure::Stale);
        }
        // Deliberately no success path until a supported native bootstrap and
        // transition contract is implemented and qualified for a scoped profile
        Err(self.failed.unwrap_or(Failure::UnknownBootstrap))
    }
}

/// Persistent KDE status learns an unlock cycle; transaction leases still revoke
/// on any intervening lock/session/sleep event
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum KdeStatus {
    #[default]
    AwaitingUnlockCycle,
    Protected,
    UnlockedAfterCycle,
    Unavailable,
}
#[derive(Default)]
pub(crate) struct KdeCycle {
    pub status: KdeStatus,
    observed_active: bool,
    pub generation: u64,
    guard: Option<GuardState>,
    guard_lock_pending: bool,
    guard_saw_protected: bool,
    guard_sleep_pending: bool,
}
impl KdeCycle {
    // Track a complete protected-to-unlocked guard cycle before allowing admission
    fn guard_status(&mut self, state: GuardState) -> KdeStatus {
        if state.protected && self.guard_lock_pending {
            self.guard_saw_protected = true;
        } else if !state.protected && self.guard_lock_pending && self.guard_saw_protected {
            self.guard_lock_pending = false;
            self.guard_saw_protected = false;
        }
        if state.protected || self.guard_lock_pending || self.guard_sleep_pending {
            KdeStatus::Protected
        } else {
            KdeStatus::UnlockedAfterCycle
        }
    }
    // Signal a change to the Linux session worker
    pub(crate) fn signal(&mut self, signal: Signal) {
        if self.status == KdeStatus::Unavailable {
            return;
        }
        match signal {
            Signal::GuardState(state) => {
                if self.guard.is_some_and(|previous| {
                    state.instance != previous.instance
                        || state.generation < previous.generation
                        || (state.generation == previous.generation
                            && state.protected != previous.protected)
                }) {
                    self.status = KdeStatus::Unavailable;
                } else if self.guard == Some(state) {
                    return;
                } else {
                    self.guard = Some(state);
                    self.status = self.guard_status(state);
                    self.observed_active = false;
                }
            }
            Signal::ProviderLost => self.status = KdeStatus::Unavailable,
            Signal::ScreenSaverActive(true) => {
                self.observed_active = true;
                self.status = KdeStatus::Protected;
            }
            Signal::ScreenSaverActive(false) => {
                self.status = if self.observed_active {
                    KdeStatus::UnlockedAfterCycle
                } else {
                    KdeStatus::AwaitingUnlockCycle
                };
                self.observed_active = false;
            }
            Signal::LockedHint(true) => {
                // logind may echo the locked signal after KDE emitted it. This
                // vetoes safety without discarding that observed locked state
                self.status = KdeStatus::Protected;
            }
            Signal::LockRequested | Signal::AboutToLock => {
                self.guard_lock_pending = true;
                self.guard_saw_protected = false;
                self.observed_active = false;
                self.status = KdeStatus::Protected;
            }
            Signal::PrepareForSleep(true) => {
                self.guard_sleep_pending = true;
                self.observed_active = false;
                self.status = KdeStatus::Protected;
            }
            Signal::PrepareForSleep(false) if self.guard_sleep_pending => {
                // Explicit resume/cancellation is distinct from a stale false
                // property sampled before the sleep request was processed
                self.guard_sleep_pending = false;
            }
            Signal::SessionActive(false) => {
                self.observed_active = false;
                self.status = KdeStatus::Protected;
            }
            _ => return,
        }
        if let Some(next) = self.generation.checked_add(1) {
            self.generation = next;
        } else {
            self.status = KdeStatus::Unavailable;
        }
    }
    /// KDE snapshots never create an observed unlock cycle. Versioned GNOME
    /// companion snapshots may establish state, but never clear pending requests
    pub(crate) fn snapshot(&mut self, snapshot: Snapshot) {
        if let Some(state) = snapshot.guard_state {
            self.signal(Signal::GuardState(state));
            if self.status == KdeStatus::Unavailable {
                return;
            }
            let guarded = self.guard_status(state);
            let next = if snapshot.unsafe_session() {
                KdeStatus::Protected
            } else {
                guarded
            };
            if next != self.status {
                self.status = next;
                if let Some(next) = self.generation.checked_add(1) {
                    self.generation = next;
                } else {
                    self.status = KdeStatus::Unavailable;
                }
            }
            return;
        }
        if snapshot.screensaver_active && self.status != KdeStatus::Unavailable {
            // Unlike initial false, true is positive evidence of the locked phase
            // Starting Manager while locked can therefore observe the later unlock
            if !self.observed_active || self.status != KdeStatus::Protected {
                self.signal(Signal::ScreenSaverActive(true));
            }
        } else if snapshot.unsafe_session() && self.status == KdeStatus::UnlockedAfterCycle {
            self.signal(Signal::LockRequested);
        }
    }
}

#[cfg(target_os = "linux")]
#[path = "linux_session_kde.rs"]
pub(crate) mod kde;

#[cfg(target_os = "linux")]
#[path = "linux_session_native.rs"]
pub(crate) mod native;

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the pinned KDE service identities used by the test
    fn binding() -> Binding {
        Binding {
            session_path: "/org/freedesktop/login1/session/_32".into(),
            logind_owner: ":1.2".into(),
            desktop_owner: ":1.9".into(),
            desktop: Desktop::KdeScreenSaver,
        }
    }
    // Return a snapshot of Linux session state
    fn snapshot() -> Snapshot {
        Snapshot {
            active: true,
            user_class: true,
            x11: true,
            remote: false,
            locked_hint: false,
            preparing_for_sleep: false,
            screensaver_active: false,
            guard_state: None,
        }
    }
    // Construct a session observer from the test binding and snapshot
    fn observer() -> Observer {
        Observer::new(binding(), snapshot(), 10).unwrap()
    }
    #[test]
    // Verify that pending lock request cannot be cleared by unchanged guard snapshot
    fn pending_lock_request_cannot_be_cleared_by_unchanged_guard_snapshot() {
        let state = GuardState::parse("12345678-1234-1234-1234-123456789abc", 1, false).unwrap();
        let mut observed = snapshot();
        observed.guard_state = Some(state);
        // A request received before the first companion snapshot is also sticky
        let mut initial = KdeCycle::default();
        initial.signal(Signal::LockRequested);
        initial.snapshot(observed);
        assert_eq!(initial.status, KdeStatus::Protected);
        let mut cycle = KdeCycle::default();
        cycle.snapshot(observed);
        cycle.signal(Signal::LockRequested);
        let epoch = cycle.generation;
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::Protected);
        assert_eq!(cycle.generation, epoch);
        cycle.signal(Signal::GuardState(GuardState {
            generation: 2,
            protected: true,
            ..state
        }));
        cycle.signal(Signal::GuardState(GuardState {
            generation: 3,
            protected: false,
            ..state
        }));
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
    }
    #[test]
    // Verify that pending sleep requires explicit resume not an unchanged false property
    fn pending_sleep_requires_explicit_resume_not_an_unchanged_false_property() {
        let state = GuardState::parse("12345678-1234-1234-1234-123456789abc", 1, false).unwrap();
        let mut observed = snapshot();
        observed.guard_state = Some(state);
        let mut cycle = KdeCycle::default();
        cycle.snapshot(observed);
        cycle.signal(Signal::PrepareForSleep(true));
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::Protected);
        cycle.signal(Signal::PrepareForSleep(false));
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
        cycle.signal(Signal::LockRequested);
        cycle.signal(Signal::PrepareForSleep(true));
        cycle.signal(Signal::PrepareForSleep(false));
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::Protected);
    }
    #[test]
    // Verify that gnome companion has a distinct versioned service and path
    fn gnome_companion_has_a_distinct_versioned_service_and_path() {
        assert_eq!(
            Desktop::GnomeGuard.service(),
            "org.streamdockmanager.SessionGuard1"
        );
        assert_eq!(
            Desktop::GnomeGuard.path(),
            "/org/streamdockmanager/SessionGuard1"
        );
        assert_eq!(Desktop::KdeScreenSaver.path(), "/ScreenSaver");
    }
    #[test]
    // Verify that gnome guard has authoritative bootstrap and sticky instance identity
    fn gnome_guard_has_authoritative_bootstrap_and_sticky_instance_identity() {
        let state = GuardState::parse("12345678-1234-1234-1234-123456789abc", 1, false).unwrap();
        let mut cycle = KdeCycle::default();
        let mut observed = snapshot();
        observed.guard_state = Some(state);
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
        let epoch = cycle.generation;
        let locked = GuardState {
            generation: 2,
            protected: true,
            ..state
        };
        cycle.signal(Signal::GuardState(locked));
        assert_eq!(cycle.status, KdeStatus::Protected);
        assert!(cycle.generation > epoch);
        let unlocked = GuardState {
            generation: 3,
            protected: false,
            ..state
        };
        cycle.signal(Signal::GuardState(unlocked));
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
        cycle.signal(Signal::GuardState(GuardState {
            instance: [5, 6],
            ..unlocked
        }));
        assert_eq!(cycle.status, KdeStatus::Unavailable);
        cycle.signal(Signal::GuardState(unlocked));
        assert_eq!(cycle.status, KdeStatus::Unavailable);
    }
    #[test]
    // Verify that gnome guard cannot override logind or accept epoch rollback
    fn gnome_guard_cannot_override_logind_or_accept_epoch_rollback() {
        let state = GuardState::parse("12345678-1234-1234-1234-123456789abc", 3, false).unwrap();
        let mut cycle = KdeCycle::default();
        let mut observed = snapshot();
        observed.guard_state = Some(state);
        observed.active = false;
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::Protected);
        observed.active = true;
        cycle.snapshot(observed);
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
        cycle.signal(Signal::GuardState(GuardState {
            generation: 2,
            ..state
        }));
        assert_eq!(cycle.status, KdeStatus::Unavailable);
        assert!(GuardState::parse("00000000-0000-0000-0000-000000000000", 1, false).is_err());
        assert!(GuardState::parse("12345678-1234-1234-1234-123456789abc", 0, false).is_err());
        assert!(GuardState::parse("not-an-instance", 1, false).is_err());
    }
    #[test]
    // Verify that kde bootstrap requires observed complete cycle not false snapshot
    fn kde_bootstrap_requires_observed_complete_cycle_not_false_snapshot() {
        let mut cycle = KdeCycle::default();
        cycle.snapshot(snapshot());
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::AwaitingUnlockCycle);
        cycle.signal(Signal::ScreenSaverActive(true));
        cycle.signal(Signal::LockedHint(true));
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
        let generation = cycle.generation;
        cycle.signal(Signal::AboutToLock);
        assert_eq!(cycle.status, KdeStatus::Protected);
        assert!(cycle.generation > generation);
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::AwaitingUnlockCycle);
        cycle.signal(Signal::ScreenSaverActive(true));
        cycle.signal(Signal::ScreenSaverActive(false));
        cycle.signal(Signal::ProviderLost);
        cycle.signal(Signal::ScreenSaverActive(true));
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::Unavailable);
    }
    #[test]
    // Verify that launch locked snapshot can bootstrap only after unlock signal
    fn launch_locked_snapshot_can_bootstrap_only_after_unlock_signal() {
        let mut cycle = KdeCycle::default();
        let mut locked = snapshot();
        locked.screensaver_active = true;
        cycle.snapshot(locked);
        assert_eq!(cycle.status, KdeStatus::Protected);
        cycle.snapshot(snapshot());
        assert_eq!(cycle.status, KdeStatus::Protected);
        cycle.signal(Signal::ScreenSaverActive(false));
        assert_eq!(cycle.status, KdeStatus::UnlockedAfterCycle);
    }
    #[test]
    // Verify that logind veto and generation overflow invalidate kde candidate
    fn logind_veto_and_generation_overflow_invalidate_kde_candidate() {
        let mut cycle = KdeCycle::default();
        cycle.signal(Signal::ScreenSaverActive(true));
        cycle.signal(Signal::ScreenSaverActive(false));
        let mut s = snapshot();
        s.locked_hint = true;
        cycle.snapshot(s);
        assert_eq!(cycle.status, KdeStatus::Protected);
        cycle.generation = u64::MAX;
        cycle.signal(Signal::ScreenSaverActive(true));
        assert_eq!(cycle.status, KdeStatus::Unavailable);
    }
    #[test]
    // Verify that inactive screensaver and lock hint never prove initial unlock
    fn inactive_screensaver_and_lock_hint_never_prove_initial_unlock() {
        for desktop in [Desktop::GnomeScreenShield, Desktop::KdeScreenSaver] {
            let mut b = binding();
            b.desktop = desktop;
            assert!(desktop.service().starts_with("org."));
            let mut state = Observer::new(b.clone(), snapshot(), 10).unwrap();
            state.signal(&b.desktop_owner, Signal::ScreenSaverActive(false));
            state.signal(&b.logind_owner, Signal::LockedHint(false));
            state.refresh(&b, snapshot(), 20);
            assert_eq!(state.check(20, 50), Err(Failure::UnknownBootstrap));
        }
    }
    #[test]
    // Verify that every protected snapshot fails closed
    fn every_protected_snapshot_fails_closed() {
        for index in 0..7 {
            let mut s = snapshot();
            match index {
                0 => s.active = false,
                1 => s.user_class = false,
                2 => s.x11 = false,
                3 => s.remote = true,
                4 => s.locked_hint = true,
                5 => s.preparing_for_sleep = true,
                _ => s.screensaver_active = true,
            }
            assert!(matches!(
                Observer::new(binding(), s, 10),
                Err(Failure::ProtectedSession)
            ));
            let mut state = observer();
            state.refresh(&binding(), s, 20);
            state.refresh(&binding(), snapshot(), 30);
            assert_eq!(state.check(30, 50), Err(Failure::ProtectedSession));
        }
    }
    #[test]
    // Verify that lock sleep session and grab precursors revoke stickily
    fn lock_sleep_session_and_grab_precursors_revoke_stickily() {
        for signal in [
            Signal::LockRequested,
            Signal::PrepareForSleep(true),
            Signal::SessionActive(false),
            Signal::LockedHint(true),
            Signal::ScreenSaverActive(true),
            Signal::AboutToLock,
        ] {
            let mut state = observer();
            let sender = if matches!(
                signal,
                Signal::ScreenSaverActive(_) | Signal::AboutToLock | Signal::GuardState(_)
            ) {
                ":1.9"
            } else {
                ":1.2"
            };
            state.signal(sender, signal);
            state.signal(":1.2", Signal::PrepareForSleep(false));
            state.signal(":1.2", Signal::SessionActive(true));
            state.refresh(&binding(), snapshot(), 20);
            assert_eq!(state.check(20, 50), Err(Failure::ProtectedSession));
        }
    }
    #[test]
    // Verify that owner session and transport loss cannot be recovered in place
    fn owner_session_and_transport_loss_cannot_be_recovered_in_place() {
        for index in 0..5 {
            let mut state = observer();
            let mut b = binding();
            match index {
                0 => b.session_path.push('3'),
                1 => b.logind_owner = ":1.3".into(),
                2 => b.desktop_owner = ":1.10".into(),
                3 => state.signal(":1.77", Signal::ScreenSaverActive(false)),
                _ => state.signal(":1.2", Signal::ProviderLost),
            }
            state.refresh(&b, snapshot(), 20);
            state.refresh(&binding(), snapshot(), 30);
            assert_eq!(state.check(30, 50), Err(Failure::ProviderLost));
        }
    }
    #[test]
    // Verify that aliases malformed owners and stale observations are rejected
    fn aliases_malformed_owners_and_stale_observations_are_rejected() {
        for id in ["self", "auto", "", "a/b"] {
            let mut b = binding();
            b.session_path = format!("/org/freedesktop/login1/session/{id}");
            assert!(matches!(
                Observer::new(b, snapshot(), 10),
                Err(Failure::InvalidBinding)
            ));
        }
        let mut b = binding();
        b.desktop_owner = "org.freedesktop.ScreenSaver".into();
        assert!(matches!(
            Observer::new(b, snapshot(), 10),
            Err(Failure::InvalidBinding)
        ));
        assert_eq!(observer().check(61, 50), Err(Failure::Stale));
        assert_eq!(observer().check(9, 50), Err(Failure::Stale));
    }
}
