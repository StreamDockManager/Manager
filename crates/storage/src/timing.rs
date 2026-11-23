//! Optional process-local storage measurements for disposable performance runs.
//! Set STREAM_DOCK_MANAGER_STORAGE_TIMING=1 before launch to collect them.
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Phase {
    QueueWait,
    TransactionWork,
    TransactionCommit,
    WorkspaceValidation,
    WorkspaceCommitCheck,
    BackupCopy,
    BackupValidation,
    BackupCapture,
    BackupRetention,
    SnapshotInspection,
}

#[derive(Clone, Copy, Default)]
pub struct Measurement {
    pub calls: u64,
    pub elapsed: Duration,
}

type Totals = BTreeMap<Phase, Measurement>;
fn recorder() -> Option<&'static Mutex<Totals>> {
    static TOTALS: OnceLock<Option<Mutex<Totals>>> = OnceLock::new();
    TOTALS
        .get_or_init(|| {
            (std::env::var_os("STREAM_DOCK_MANAGER_STORAGE_TIMING").as_deref()
                == Some(std::ffi::OsStr::new("1")))
            .then(Mutex::default)
        })
        .as_ref()
}

pub fn snapshot() -> BTreeMap<Phase, Measurement> {
    recorder()
        .map(|r| r.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_default()
}

pub(crate) fn timestamp() -> Option<Instant> {
    recorder().map(|_| Instant::now())
}

pub(crate) fn record_since(phase: Phase, started: Option<Instant>) {
    if let Some(start) = started {
        let elapsed = start.elapsed();
        let mut totals = recorder()
            .unwrap()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = totals.entry(phase).or_default();
        entry.calls = entry.calls.saturating_add(1);
        entry.elapsed = entry.elapsed.saturating_add(elapsed);
    }
}

#[must_use]
pub struct Timer(Phase, Option<Instant>);
pub fn start(phase: Phase) -> Timer {
    Timer(phase, timestamp())
}
impl Drop for Timer {
    fn drop(&mut self) {
        record_since(self.0, self.1);
    }
}
