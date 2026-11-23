//! Bounded host rendering contracts. These limits do not qualify physical throughput
//! These are host resource limits, not declarations of firmware throughput
use crate::{DeviceTransaction, Error, Result};
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    IndividualUpdates,
    #[default]
    FrameScheduling,
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FrameEncoding {
    #[default]
    IndividualUpdates,
    GroupedKeyUpdates,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct OutputPolicy {
    pub mode: OutputMode,
    pub target_frame_hz: u8,
    pub max_pending_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_batch_estimated_ms: u32,
    pub max_batch_surfaces: usize,
    pub max_surface_bytes: usize,
}
impl Default for OutputPolicy {
    // Construct the default value
    fn default() -> Self {
        Self {
            mode: OutputMode::FrameScheduling,
            target_frame_hz: 24,
            max_pending_bytes: 16 * 1024 * 1024,
            max_batch_bytes: 256 * 1024,
            max_batch_estimated_ms: 40,
            max_batch_surfaces: 64,
            max_surface_bytes: 8 * 1024 * 1024,
        }
    }
}
impl OutputPolicy {
    // Validate for the current operation
    pub fn validate(&self) -> Result<()> {
        if !(1..=24).contains(&self.target_frame_hz)
            || !(1..=128).contains(&self.max_batch_surfaces)
            || !(1..=100).contains(&self.max_batch_estimated_ms)
            || self.max_batch_bytes == 0
            || self.max_batch_bytes > 4 * 1024 * 1024
            || self.max_surface_bytes < self.max_batch_bytes
            || self.max_surface_bytes > 32 * 1024 * 1024
            || self.max_pending_bytes < self.max_surface_bytes
            || self.max_pending_bytes > 64 * 1024 * 1024
        {
            return Err(Error::Descriptor(
                "invalid device-frame output budgets".into(),
            ));
        }
        Ok(())
    }
}
/// A model-owned protocol commit, emitted after a contiguous legal group
/// Equal group IDs AND equal report bytes permit grouping. No generic code strips
/// or invents packets from a pre-encoded legacy transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCommit {
    pub group: String,
    pub reports: Vec<Vec<u8>>,
}
#[derive(Debug, Clone)]
pub struct FrameUpload {
    pub transaction: DeviceTransaction,
    pub commit: Option<FrameCommit>,
    /// Refresh/commit reports already embedded by the model in this upload
    /// Generic/unknown transactions report zero rather than inspecting wire bytes
    pub embedded_commit_count: usize,
}
impl From<DeviceTransaction> for FrameUpload {
    // Convert the input to this type
    fn from(transaction: DeviceTransaction) -> Self {
        Self {
            transaction,
            commit: None,
            embedded_commit_count: 0,
        }
    }
}
impl FrameUpload {
    // Report bytes
    pub fn report_bytes(&self) -> usize {
        self.transaction.reports.iter().map(Vec::len).sum::<usize>()
            + self
                .commit
                .as_ref()
                .map_or(0, |c| c.reports.iter().map(Vec::len).sum())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPriority {
    Interaction,
    Normal,
    Animation,
}
#[derive(Debug, Clone)]
pub struct SurfaceSubmission {
    pub upload: FrameUpload,
    pub surface_id: String,
    pub content_revision: u64,
    pub priority: OutputPriority,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OutputStatus {
    Completed,
    Replaced,
    Invalidated,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct OutputOutcome {
    /// Process-local interval of the complete legal write group, including its commit.
    #[serde(skip)]
    #[ts(skip)]
    pub write_interval: Option<(std::time::Instant, std::time::Instant)>,
    pub surface_id: String,
    #[ts(type = "number")]
    pub content_revision: u64,
    #[ts(type = "number")]
    pub scene_revision: u64,
    pub status: OutputStatus,
    pub queue_ms: f64,
    pub transfer_ms: f64,
    pub report_bytes: usize,
    pub commit_count: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct OutputQueueStats {
    pub pending_bytes: usize,
    pub pending_count: usize,
    pub in_flight_count: usize,
    pub in_flight_bytes: usize,
    pub oldest_pending_ms: f64,
    #[ts(type = "number")]
    pub total_completed: u64,
    #[ts(type = "number")]
    pub total_dropped: u64,
    #[ts(type = "number")]
    pub total_report_bytes: u64,
    #[ts(type = "number")]
    pub total_commits: u64,
    pub peak_pending_bytes: usize,
    pub peak_pending_count: usize,
    #[ts(type = "number")]
    pub total_failed: u64,
    #[ts(type = "number")]
    pub outcomes_lost: u64,
    pub estimated_bytes_per_ms: f64,
}

/// Thread-local adapter counters. Reading a delta around a synchronous session call
/// excludes other connections and needs no global lock or payload retention.
#[derive(Clone, Copy, Default)]
pub struct AdapterWork {
    pub framing: std::time::Duration,
    pub payload_bytes: u64,
    pub jpeg_attempts: u64,
}
thread_local! {
    static ADAPTER_WORK: std::cell::Cell<AdapterWork> = Default::default();
}
impl AdapterWork {
    pub fn current() -> Self {
        ADAPTER_WORK.get()
    }
    pub fn framed(started: std::time::Instant, payload: usize) {
        let mut work = Self::current();
        work.framing += started.elapsed();
        work.payload_bytes = work.payload_bytes.wrapping_add(payload as u64);
        ADAPTER_WORK.set(work);
    }
    pub fn jpeg_attempt() {
        let mut work = Self::current();
        work.jpeg_attempts = work.jpeg_attempts.wrapping_add(1);
        ADAPTER_WORK.set(work);
    }
}
