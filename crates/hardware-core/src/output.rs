use crate::{
    Error, FrameUpload, OutputMode, OutputOutcome, OutputPolicy, OutputPriority, OutputQueueStats,
    OutputStatus, Result, SurfaceSubmission,
};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct DeviceTransaction {
    pub reports: Vec<Vec<u8>>,
    pub coalesce_key: Option<String>,
}
impl DeviceTransaction {
    // Construct an output with a single target
    pub fn single(report: Vec<u8>) -> Self {
        Self {
            reports: vec![report],
            coalesce_key: None,
        }
    }
}
/// Backends must bound each write using their platform's transport facilities
/// Output budgets are admission estimates, not a way to interrupt a blocked OS call
pub trait Transport: Send + 'static {
    // Write for the current operation
    fn write(&mut self, report: &[u8]) -> Result<()>;
}
#[derive(Clone)]
struct SurfaceMeta {
    id: String,
    revision: u64,
    scene: u64,
    priority: OutputPriority,
    epoch: u64,
}
/// One receipt for a frozen scene, independent of the bounded diagnostic stream
struct SceneCompletion {
    remaining: usize,
    status: OutputStatus,
    sender: std::sync::mpsc::Sender<OutputStatus>,
}
impl SceneCompletion {
    // Complete
    fn complete(&mut self, status: OutputStatus) {
        if status != OutputStatus::Completed && self.status != OutputStatus::Failed {
            self.status = status;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            let _ = self.sender.send(self.status);
        }
    }
}
struct Pending {
    generation: u64,
    upload: FrameUpload,
    surface: Option<SurfaceMeta>,
    completion: Option<std::sync::mpsc::Sender<OutputStatus>>,
    scene_completion: Option<Arc<Mutex<SceneCompletion>>>,
    admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    per_report_guard: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    queued_at: Instant,
    // Preserve age on replacement: a continuously changing surface cannot starve
    first_queued_at: Instant,
    bytes: usize,
}
impl Pending {
    // Translate the output result into its reported outcome
    fn outcome(
        &self,
        status: OutputStatus,
        transfer_ms: f64,
        bytes: usize,
        commits: usize,
    ) -> Option<OutputOutcome> {
        if let Some(scene) = &self.scene_completion {
            scene.lock().unwrap().complete(status);
        }
        if let Some(completion) = &self.completion {
            let _ = completion.send(status);
        }
        self.surface.as_ref().map(|s| OutputOutcome {
            surface_id: s.id.clone(),
            content_revision: s.revision,
            scene_revision: s.scene,
            status,
            queue_ms: self.queued_at.elapsed().as_secs_f64() * 1000.0,
            transfer_ms,
            report_bytes: bytes,
            commit_count: commits,
        })
    }
}
struct Queue {
    pending: VecDeque<Pending>,
    closed: bool,
    draining: bool,
    failure: Option<String>,
    measurements: VecDeque<(f64, f64)>,
    outcomes: VecDeque<OutputOutcome>,
    policy: OutputPolicy,
    stats: OutputQueueStats,
}
impl Queue {
    // Record for the current operation
    fn record(&mut self, outcome: Option<OutputOutcome>) {
        if let Some(outcome) = outcome {
            match outcome.status {
                OutputStatus::Completed => {
                    self.stats.total_completed = self.stats.total_completed.saturating_add(1)
                }
                OutputStatus::Failed => {
                    self.stats.total_failed = self.stats.total_failed.saturating_add(1)
                }
                _ => self.stats.total_dropped = self.stats.total_dropped.saturating_add(1),
            }
            // Bounded diagnostic event stream. Consumers should drain every runtime tick
            if self.outcomes.len() == 4096 {
                self.outcomes.pop_front();
                self.stats.outcomes_lost = self.stats.outcomes_lost.saturating_add(1);
            }
            self.outcomes.push_back(outcome);
        }
    }
    // Remove for the current operation
    fn remove(&mut self, index: usize) -> Pending {
        let item = self.pending.remove(index).expect("pending index");
        self.stats.pending_bytes -= item.bytes;
        self.stats.pending_count = self.pending.len();
        item
    }
    // Push for the current operation
    fn push(&mut self, item: Pending) {
        self.stats.pending_bytes += item.bytes;
        self.pending.push_back(item);
        self.stats.pending_count = self.pending.len();
        self.stats.peak_pending_bytes = self.stats.peak_pending_bytes.max(self.stats.pending_bytes);
        self.stats.peak_pending_count = self.stats.peak_pending_count.max(self.pending.len());
    }
    // Clear for the current operation
    fn clear(&mut self, status: OutputStatus) {
        while !self.pending.is_empty() {
            let item = self.remove(0);
            self.record(item.outcome(status, 0.0, 0, 0));
        }
    }
    // Select the next queued output index
    fn next_index(&self, now: Instant) -> usize {
        // A frozen scene precedes subsequent ordinary updates, including updates
        // with interaction priority. The worker takes the entire scene at once.
        self.pending
            .iter()
            .position(|p| p.scene_completion.is_some())
            .unwrap_or_else(|| {
                if self.policy.mode == OutputMode::IndividualUpdates {
                    0
                } else {
                    self.select_index(now)
                }
            })
    }
    // Select index
    fn select_index(&self, now: Instant) -> usize {
        // Controls get interaction priority, but age promotion prevents a
        // sustained stream of ordinary controls from starving image work.
        // Urgent shutdown uses finish(), which invalidates queued frame work
        self.pending
            .iter()
            .enumerate()
            .max_by_key(|(index, item)| {
                let age = now
                    .saturating_duration_since(item.first_queued_at)
                    .as_millis();
                let priority = match item.surface.as_ref().map(|s| s.priority) {
                    Some(OutputPriority::Interaction) => 2,
                    Some(OutputPriority::Normal) => 1,
                    None => {
                        if item.upload.transaction.coalesce_key.is_none() {
                            2
                        } else {
                            1
                        }
                    }
                    Some(OutputPriority::Animation) => 0,
                };
                // At 250 ms age the oldest surface outranks any fresh interaction
                (
                    age / 250 * 3 + priority,
                    std::cmp::Reverse(item.first_queued_at),
                    std::cmp::Reverse(*index),
                )
            })
            .map_or(0, |(i, _)| i)
    }
    // Take batch
    fn take_batch(&mut self, now: Instant) -> Vec<Pending> {
        let index = self.next_index(now);
        let first = self.remove(index);
        if let Some(scene) = first.scene_completion.clone() {
            let mut batch = vec![first];
            while let Some(index) = self.pending.iter().position(|p| {
                p.scene_completion
                    .as_ref()
                    .is_some_and(|other| Arc::ptr_eq(&scene, other))
            }) {
                batch.push(self.remove(index));
            }
            return batch;
        }
        if self.policy.mode == OutputMode::IndividualUpdates || first.surface.is_none() {
            return vec![first];
        }
        let scene = first.surface.as_ref().map(|s| s.scene);
        let mut bytes = first.bytes;
        let mut batch = vec![first];
        let time_budget = self.stats.estimated_bytes_per_ms.max(0.000_001)
            * f64::from(self.policy.max_batch_estimated_ms);
        let budget = self
            .policy
            .max_batch_bytes
            .min(time_budget.max(1.0) as usize);
        // A valid oversized image gets one exclusive, still finite transfer
        if bytes > budget {
            return batch;
        }
        while batch.len() < self.policy.max_batch_surfaces && !self.pending.is_empty() {
            let index = self.select_index(now);
            let item = &self.pending[index];
            if item.scene_completion.is_some()
                || item.surface.as_ref().map(|s| s.scene) != scene
                || bytes.saturating_add(item.bytes) > budget
            {
                break;
            }
            bytes += item.bytes;
            batch.push(self.remove(index));
        }
        batch
    }
}
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    generation: AtomicU64,
    scene: AtomicU64,
    surface_epochs: Mutex<std::collections::HashMap<String, u64>>,
    failure_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
pub struct OutputWorker {
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}
impl OutputWorker {
    // Spawn with policy
    pub fn spawn_with_policy(
        mut transport: Box<dyn Transport>,
        fps: u8,
        policy: OutputPolicy,
    ) -> Result<Self> {
        policy.validate()?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                pending: VecDeque::new(),
                closed: false,
                draining: false,
                failure: None,
                measurements: VecDeque::new(),
                outcomes: VecDeque::new(),
                policy,
                // Admission seed only; replaced by first observed nonzero transfer sample
                stats: OutputQueueStats {
                    estimated_bytes_per_ms: 4096.0,
                    ..Default::default()
                },
            }),
            ready: Condvar::new(),
            generation: AtomicU64::new(1),
            scene: AtomicU64::new(1),
            surface_epochs: Mutex::new(Default::default()),
            failure_waker: Mutex::new(None),
        });
        let state = shared.clone();
        let worker = thread::spawn(move || {
            let mut last_surface = Instant::now() - Duration::from_secs(1);
            let mut measured = false;
            loop {
                let batch = {
                    let mut queue = state.queue.lock().unwrap();
                    loop {
                        while queue.pending.is_empty() && !queue.closed && !queue.draining {
                            queue = state.ready.wait(queue).unwrap();
                        }
                        if queue.closed || queue.pending.is_empty() {
                            return;
                        }
                        let index = queue.next_index(Instant::now());
                        let producer_paced = queue.pending[index]
                            .surface
                            .as_ref()
                            .is_some_and(|surface| surface.priority == OutputPriority::Animation);
                        let paced = queue.pending[index].scene_completion.is_none()
                            && !producer_paced
                            && (queue.pending[index]
                                .upload
                                .transaction
                                .coalesce_key
                                .is_some()
                                || queue.pending[index].surface.is_some());
                        let hz = if queue.policy.mode == OutputMode::IndividualUpdates {
                            fps.clamp(1, 20)
                        } else {
                            queue.policy.target_frame_hz
                        };
                        let wait = Duration::from_secs_f64(1.0 / f64::from(hz))
                            .saturating_sub(last_surface.elapsed());
                        if paced && !wait.is_zero() {
                            let (next, _) = state.ready.wait_timeout(queue, wait).unwrap();
                            queue = next;
                            continue;
                        }
                        let batch = queue.take_batch(Instant::now());
                        queue.stats.in_flight_count = batch.len();
                        queue.stats.in_flight_bytes = batch.iter().map(|p| p.bytes).sum();
                        if paced {
                            last_surface = Instant::now();
                        }
                        break batch;
                    }
                };
                let mut remaining = VecDeque::from(batch);
                while let Some(first) = remaining.pop_front() {
                    let commit = first.upload.commit.clone();
                    let mut group = vec![first];
                    if commit.is_some() {
                        while remaining.front().is_some_and(|p| {
                            p.upload.commit == commit
                                && p.surface.as_ref().map(|s| s.scene)
                                    == group[0].surface.as_ref().map(|s| s.scene)
                        }) {
                            group.push(remaining.pop_front().unwrap());
                        }
                    }
                    {
                        let epochs = state.surface_epochs.lock().unwrap();
                        let mut invalid = Vec::new();
                        group.retain(|item| {
                            let current = item
                                .surface
                                .as_ref()
                                .is_none_or(|s| epochs.get(&s.id).copied().unwrap_or(0) == s.epoch);
                            if !current {
                                invalid.push((
                                    item.bytes,
                                    item.outcome(OutputStatus::Invalidated, 0.0, 0, 0),
                                ));
                            }
                            current
                        });
                        drop(epochs);
                        if !invalid.is_empty() {
                            let mut queue = state.queue.lock().unwrap();
                            for (bytes, outcome) in invalid {
                                queue.stats.in_flight_count -= 1;
                                queue.stats.in_flight_bytes -= bytes;
                                queue.record(outcome);
                            }
                            state.ready.notify_all();
                        }
                        if group.is_empty() {
                            continue;
                        }
                    }
                    let invalid_connection =
                        group[0].generation != state.generation.load(Ordering::SeqCst);
                    let invalid_scene = group[0]
                        .surface
                        .as_ref()
                        .is_some_and(|s| s.scene != state.scene.load(Ordering::SeqCst));
                    // A shared guard represents one immutable frame contract
                    // Check it once per legal group, outside worker locks
                    let mut checked = Vec::new();
                    let invalid_admission = group
                        .iter()
                        .filter_map(|item| item.admission.as_ref())
                        .any(|check| {
                            if checked.iter().any(|previous| Arc::ptr_eq(previous, check)) {
                                return false;
                            }
                            checked.push(check.clone());
                            !check()
                        });
                    if invalid_connection || invalid_scene || invalid_admission {
                        let mut queue = state.queue.lock().unwrap();
                        for item in group {
                            queue.stats.in_flight_count -= 1;
                            queue.stats.in_flight_bytes -= item.bytes;
                            queue.record(item.outcome(OutputStatus::Invalidated, 0.0, 0, 0));
                        }
                        state.ready.notify_all();
                        continue;
                    }
                    // A scene switch never truncates this legal group; a disconnect may
                    let start = Instant::now();
                    let queue_ms = group[0].queued_at.elapsed().as_secs_f64() * 1000.0;
                    let mut bytes = 0usize;
                    let mut commits = 0usize;
                    let mut failure = None;
                    let mut disconnected = false;
                    for report in group
                        .iter()
                        .flat_map(|p| p.upload.transaction.reports.iter())
                        .chain(commit.iter().flat_map(|c| c.reports.iter()))
                    {
                        if group[0].generation != state.generation.load(Ordering::SeqCst) {
                            disconnected = true;
                            break;
                        }
                        if group
                            .iter()
                            .filter_map(|item| item.per_report_guard.as_ref())
                            .any(|guard| !guard())
                        {
                            disconnected = true;
                            break;
                        }
                        if let Err(error) = transport.write(report) {
                            failure = Some(error.to_string());
                            break;
                        }
                        bytes += report.len();
                    }
                    if failure.is_none() && !disconnected {
                        commits = commit.as_ref().map_or(0, |c| c.reports.len())
                            + group
                                .iter()
                                .map(|p| p.upload.embedded_commit_count)
                                .sum::<usize>();
                    }
                    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                    let mut queue = state.queue.lock().unwrap();
                    queue.stats.total_report_bytes =
                        queue.stats.total_report_bytes.saturating_add(bytes as u64);
                    queue.stats.total_commits =
                        queue.stats.total_commits.saturating_add(commits as u64);
                    if elapsed > 0.0 && bytes > 0 {
                        let rate = bytes as f64 / elapsed;
                        queue.stats.estimated_bytes_per_ms = if measured {
                            queue.stats.estimated_bytes_per_ms * 0.8 + rate * 0.2
                        } else {
                            rate
                        };
                        measured = true;
                    }
                    if queue.measurements.len() >= 512 {
                        queue.measurements.pop_front();
                    }
                    queue.measurements.push_back((queue_ms, elapsed));
                    let status = if failure.is_some() {
                        OutputStatus::Failed
                    } else if disconnected {
                        OutputStatus::Invalidated
                    } else {
                        OutputStatus::Completed
                    };
                    for (index, item) in group.iter().enumerate() {
                        queue.stats.in_flight_count -= 1;
                        queue.stats.in_flight_bytes -= item.bytes;
                        // Completed bytes are apportioned; the final member owns commit bytes
                        let own_bytes = if status == OutputStatus::Completed {
                            item.upload
                                .transaction
                                .reports
                                .iter()
                                .map(Vec::len)
                                .sum::<usize>()
                                + if index + 1 == group.len() {
                                    commit
                                        .as_ref()
                                        .map_or(0, |c| c.reports.iter().map(Vec::len).sum())
                                } else {
                                    0
                                }
                        } else {
                            0
                        };
                        let mut outcome = item.outcome(
                            status,
                            elapsed,
                            own_bytes,
                            if status == OutputStatus::Completed {
                                item.upload.embedded_commit_count
                                    + if index + 1 == group.len() {
                                        commit.as_ref().map_or(0, |c| c.reports.len())
                                    } else {
                                        0
                                    }
                            } else {
                                0
                            },
                        );
                        if let Some(outcome) = outcome.as_mut() {
                            outcome.queue_ms = start
                                .saturating_duration_since(item.queued_at)
                                .as_secs_f64()
                                * 1000.0;
                        }
                        queue.record(outcome);
                    }
                    state.ready.notify_all();
                    if let Some(error) = failure {
                        state.generation.fetch_add(1, Ordering::SeqCst);
                        for item in remaining.drain(..) {
                            queue.stats.in_flight_count -= 1;
                            queue.stats.in_flight_bytes -= item.bytes;
                            queue.record(item.outcome(OutputStatus::Failed, 0.0, 0, 0));
                        }
                        queue.clear(OutputStatus::Failed);
                        queue.failure = Some(error);
                        queue.closed = true;
                        drop(queue);
                        if let Some(wake) = state.failure_waker.lock().unwrap().as_ref() {
                            wake();
                        }
                        return;
                    }
                }
            }
        });
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    // Set failure waker
    pub fn set_failure_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.failure_waker.lock().unwrap() = Some(wake);
    }
    // Drain measurements
    pub fn drain_measurements(&self) -> Vec<(f64, f64)> {
        self.shared
            .queue
            .lock()
            .unwrap()
            .measurements
            .drain(..)
            .collect()
    }
    // Drain outcomes
    pub fn drain_outcomes(&self) -> Vec<OutputOutcome> {
        self.shared
            .queue
            .lock()
            .unwrap()
            .outcomes
            .drain(..)
            .collect()
    }
    // Queue depth
    pub fn queue_depth(&self) -> usize {
        self.shared.queue.lock().unwrap().pending.len()
    }
    // Queue stats
    pub fn queue_stats(&self) -> OutputQueueStats {
        let queue = self.shared.queue.lock().unwrap();
        let mut stats = queue.stats.clone();
        stats.oldest_pending_ms = queue
            .pending
            .iter()
            .map(|p| p.first_queued_at.elapsed().as_secs_f64() * 1000.0)
            .fold(0.0, f64::max);
        stats
    }
    /// Wait for both queued and already-selected legal groups to finish
    /// Does not interrupt transport writes; false means timeout or a failed session
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut queue = self.shared.queue.lock().unwrap();
        loop {
            if queue.failure.is_some() {
                return false;
            }
            if queue.pending.is_empty() && queue.stats.in_flight_count == 0 {
                return true;
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return false;
            }
            let (next, _) = self.shared.ready.wait_timeout(queue, wait).unwrap();
            queue = next;
        }
    }
    // Return the current generation identifier
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }
    // Read the revision of the current output scene
    pub fn scene_revision(&self) -> u64 {
        self.shared.scene.load(Ordering::SeqCst)
    }
    /// Invalidate only selected surfaces, preserving unrelated queued output
    /// An already-writing legal transport group finishes before its replacement
    pub fn invalidate_surfaces(&self, surfaces: &[String]) {
        let mut queue = self.shared.queue.lock().unwrap();
        let mut epochs = self.shared.surface_epochs.lock().unwrap();
        for surface in surfaces {
            // A connection's descriptor has a bounded number of surfaces
            if epochs.len() >= 128 && !epochs.contains_key(surface) {
                continue;
            }
            let epoch = epochs.entry(surface.clone()).or_default();
            *epoch = epoch.wrapping_add(1);
        }
        let mut index = 0;
        while index < queue.pending.len() {
            if queue.pending[index]
                .surface
                .as_ref()
                .is_some_and(|s| surfaces.contains(&s.id))
            {
                let item = queue.remove(index);
                queue.record(item.outcome(OutputStatus::Invalidated, 0.0, 0, 0));
            } else {
                index += 1;
            }
        }
        self.shared.ready.notify_all();
    }
    // Set scene revision
    pub fn set_scene_revision(&self, revision: u64) {
        let mut queue = self.shared.queue.lock().unwrap();
        self.shared.scene.store(revision, Ordering::SeqCst);
        let mut i = 0;
        while i < queue.pending.len() {
            if queue.pending[i]
                .surface
                .as_ref()
                .is_some_and(|s| s.scene != revision)
            {
                let item = queue.remove(i);
                queue.record(item.outcome(OutputStatus::Invalidated, 0.0, 0, 0));
            } else {
                i += 1;
            }
        }
        self.shared.ready.notify_all();
    }
    /// Discard obsolete image/control cache entries while preserving explicit,
    /// non-coalescing persistent transactions and their admission guards.
    pub fn invalidate_transient(&self) {
        let mut queue = self.shared.queue.lock().unwrap();
        let mut index = 0;
        while index < queue.pending.len() {
            if queue.pending[index].surface.is_some()
                || queue.pending[index]
                    .upload
                    .transaction
                    .coalesce_key
                    .is_some()
            {
                let item = queue.remove(index);
                queue.record(item.outcome(OutputStatus::Invalidated, 0.0, 0, 0));
            } else {
                index += 1;
            }
        }
        self.shared.ready.notify_all();
    }
    // Set policy
    pub fn set_policy(&self, policy: OutputPolicy) -> Result<()> {
        policy.validate()?;
        let mut queue = self.shared.queue.lock().unwrap();
        if queue.closed {
            return Err(Error::StaleConnection);
        }
        if queue.policy != policy {
            // Policy change is a scene boundary. In-flight legal groups complete
            self.shared.scene.fetch_add(1, Ordering::SeqCst);
            queue.clear(OutputStatus::Invalidated);
            queue.policy = policy;
            self.shared.ready.notify_all();
        }
        Ok(())
    }
    // Submit frame
    pub fn submit_frame(
        &self,
        generation: u64,
        scene_revision: u64,
        surfaces: Vec<SurfaceSubmission>,
    ) -> Result<()> {
        self.submit_surfaces(generation, scene_revision, surfaces, None, None)
    }
    // Submit frame guarded
    pub fn submit_frame_guarded(
        &self,
        generation: u64,
        scene_revision: u64,
        surfaces: Vec<SurfaceSubmission>,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<()> {
        self.submit_surfaces(generation, scene_revision, surfaces, None, admission)
    }
    /// Admit a complete immutable scene. Hard surface/queue limits still apply,
    /// but soft animation budgets cannot split its model-owned refresh groups.
    /// Completion means host transfer, not simultaneous firmware presentation
    pub fn submit_scene(
        &self,
        generation: u64,
        scene_revision: u64,
        surfaces: Vec<SurfaceSubmission>,
    ) -> Result<std::sync::mpsc::Receiver<OutputStatus>> {
        self.submit_scene_guarded(generation, scene_revision, surfaces, None)
    }
    // Submit scene guarded
    pub fn submit_scene_guarded(
        &self,
        generation: u64,
        scene_revision: u64,
        surfaces: Vec<SurfaceSubmission>,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<std::sync::mpsc::Receiver<OutputStatus>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        // Keep equal model commit groups adjacent, preserving the first group's
        // position and each independent wallpaper/touch/display boundary.
        let mut pending = VecDeque::from(surfaces);
        let mut grouped = Vec::new();
        while let Some(first) = pending.pop_front() {
            let commit = first.upload.commit.clone();
            grouped.push(first);
            if commit.is_some() {
                let mut index = 0;
                while index < pending.len() {
                    if pending[index].upload.commit == commit {
                        grouped.push(pending.remove(index).unwrap());
                    } else {
                        index += 1;
                    }
                }
            }
        }
        self.submit_surfaces(generation, scene_revision, grouped, Some(sender), admission)?;
        Ok(receiver)
    }
    // Submit surfaces
    fn submit_surfaces(
        &self,
        generation: u64,
        scene_revision: u64,
        surfaces: Vec<SurfaceSubmission>,
        completion: Option<std::sync::mpsc::Sender<OutputStatus>>,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<()> {
        let mut queue = self.shared.queue.lock().unwrap();
        if queue.closed
            || queue.draining
            || generation != self.generation()
            || scene_revision != self.scene_revision()
        {
            return Err(Error::StaleConnection);
        }
        let mut ids = HashSet::new();
        let mut added = 0usize;
        let mut removed = 0usize;
        let mut removed_count = 0usize;
        for surface in &surfaces {
            if surface.surface_id.is_empty()
                || surface.surface_id.len() > 256
                || !ids.insert(surface.surface_id.clone())
                || surface.upload.transaction.reports.is_empty()
            {
                return Err(Error::Protocol("invalid or duplicate frame surface".into()));
            }
            if queue.policy.mode == OutputMode::IndividualUpdates && surface.upload.commit.is_some()
            {
                return Err(Error::Unsupported(
                    "grouped commits require device-frame scheduling".into(),
                ));
            }
            if surface
                .upload
                .commit
                .as_ref()
                .is_some_and(|c| c.group.is_empty() || c.group.len() > 128 || c.reports.is_empty())
            {
                return Err(Error::Protocol("invalid frame commit".into()));
            }
            let bytes = surface.upload.report_bytes();
            if bytes == 0 || bytes > queue.policy.max_surface_bytes {
                return Err(Error::Transport("frame surface exceeds byte budget".into()));
            }
            added = added
                .checked_add(bytes)
                .ok_or_else(|| Error::Transport("frame byte overflow".into()))?;
            if let Some(item) = queue.pending.iter().find(|p| {
                p.scene_completion.is_none()
                    && p.surface
                        .as_ref()
                        .is_some_and(|s| s.id == surface.surface_id)
            }) {
                removed += item.bytes;
                removed_count += 1;
            }
        }
        if queue.pending.len() - removed_count + surfaces.len() > 128
            || queue.stats.pending_bytes - removed + added > queue.policy.max_pending_bytes
        {
            return Err(Error::Transport(
                "output queue byte/count budget exceeded".into(),
            ));
        }
        let now = Instant::now();
        let scene_completion = completion.map(|sender| {
            if surfaces.is_empty() {
                let _ = sender.send(OutputStatus::Completed);
            }
            Arc::new(Mutex::new(SceneCompletion {
                remaining: surfaces.len(),
                status: OutputStatus::Completed,
                sender,
            }))
        });
        for surface in surfaces {
            let old = queue.pending.iter().position(|p| {
                p.scene_completion.is_none()
                    && p.surface
                        .as_ref()
                        .is_some_and(|s| s.id == surface.surface_id)
            });
            let first_queued_at = if let Some(index) = old {
                let item = queue.remove(index);
                let age = item.first_queued_at;
                queue.record(item.outcome(OutputStatus::Replaced, 0.0, 0, 0));
                age
            } else {
                now
            };
            let bytes = surface.upload.report_bytes();
            queue.push(Pending {
                generation,
                upload: surface.upload,
                completion: None,
                scene_completion: scene_completion.clone(),
                admission: admission.clone(),
                per_report_guard: None,
                surface: Some(SurfaceMeta {
                    epoch: self
                        .shared
                        .surface_epochs
                        .lock()
                        .unwrap()
                        .get(&surface.surface_id)
                        .copied()
                        .unwrap_or(0),
                    id: surface.surface_id,
                    revision: surface.content_revision,
                    scene: scene_revision,
                    priority: surface.priority,
                }),
                queued_at: now,
                first_queued_at,
                bytes,
            });
        }
        self.shared.ready.notify_all();
        Ok(())
    }
    // Submit for the current operation
    pub fn submit(&self, generation: u64, transaction: DeviceTransaction) -> Result<()> {
        self.submit_inner(generation, transaction, None, None, None)
    }
    /// Completion means all host writes finished; it is not firmware readback
    pub fn submit_tracked(
        &self,
        generation: u64,
        transaction: DeviceTransaction,
    ) -> Result<std::sync::mpsc::Receiver<OutputStatus>> {
        let (send, receive) = std::sync::mpsc::channel();
        self.submit_inner(generation, transaction, Some(send), None, None)?;
        Ok(receive)
    }
    /// Revalidate immutable persistent work at the worker boundary, before any
    /// report is sent. Once admitted, the legal transfer is never auto-retried.
    pub fn submit_tracked_guarded(
        &self,
        generation: u64,
        transaction: DeviceTransaction,
        admission: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<std::sync::mpsc::Receiver<OutputStatus>> {
        let (send, receive) = std::sync::mpsc::channel();
        self.submit_inner(
            generation,
            transaction,
            Some(send),
            Some(Arc::from(admission)),
            None,
        )?;
        Ok(receive)
    }
    /// Interruptible settings batch: revalidate before every not-yet-started
    /// transport call. An already running OS/HID call cannot be cancelled.
    /// Do not use for image or startup-logo transfers with atomic legal groups
    pub fn submit_tracked_per_report_guarded(
        &self,
        generation: u64,
        transaction: DeviceTransaction,
        guard: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<std::sync::mpsc::Receiver<OutputStatus>> {
        let (send, receive) = std::sync::mpsc::channel();
        let guard = Arc::from(guard);
        self.submit_inner(
            generation,
            transaction,
            Some(send),
            Some(Arc::clone(&guard)),
            Some(guard),
        )?;
        Ok(receive)
    }
    // Submit inner
    fn submit_inner(
        &self,
        generation: u64,
        transaction: DeviceTransaction,
        completion: Option<std::sync::mpsc::Sender<OutputStatus>>,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
        per_report_guard: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<()> {
        let mut queue = self.shared.queue.lock().unwrap();
        if queue.closed || queue.draining || generation != self.generation() {
            return Err(Error::StaleConnection);
        }
        let bytes = transaction.reports.iter().map(Vec::len).sum::<usize>();
        let old = transaction.coalesce_key.as_ref().and_then(|key| {
            queue.pending.iter().position(|p| {
                p.surface.is_none() && p.upload.transaction.coalesce_key.as_ref() == Some(key)
            })
        });
        let removed = old.map_or(0, |i| queue.pending[i].bytes);
        let (surface_limit, pending_limit) = if queue.policy.mode == OutputMode::FrameScheduling {
            (
                queue.policy.max_surface_bytes,
                queue.policy.max_pending_bytes,
            )
        } else {
            (32 * 1024 * 1024, 64 * 1024 * 1024)
        };
        if bytes > surface_limit
            || queue.stats.pending_bytes - removed + bytes > pending_limit
            || old.is_none() && queue.pending.len() >= 128
        {
            return Err(Error::Transport("output queue full".into()));
        }
        let now = Instant::now();
        let item = Pending {
            generation,
            upload: transaction.into(),
            surface: None,
            completion,
            scene_completion: None,
            admission,
            per_report_guard,
            queued_at: now,
            first_queued_at: now,
            bytes,
        };
        if let Some(index) = old {
            let previous = queue.remove(index);
            queue.record(previous.outcome(OutputStatus::Replaced, 0.0, 0, 0));
            queue.stats.pending_bytes += bytes;
            queue.pending.insert(index, item);
            queue.stats.pending_count = queue.pending.len();
            queue.stats.peak_pending_bytes = queue
                .stats
                .peak_pending_bytes
                .max(queue.stats.pending_bytes);
        } else {
            queue.push(item);
        }
        self.shared.ready.notify_all();
        Ok(())
    }
    // Disconnect for the current operation
    pub fn disconnect(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.shared.queue.lock().unwrap();
        queue.clear(OutputStatus::Invalidated);
        queue.closed = true;
        self.shared.ready.notify_all();
    }
    // Record the output failure
    pub fn failure(&self) -> Option<String> {
        self.shared.queue.lock().unwrap().failure.clone()
    }
    /// Complete the active legal group, discard obsolete work, then send final command
    pub fn finish(&mut self, transaction: DeviceTransaction) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            if queue.closed {
                return;
            }
            self.shared.scene.fetch_add(1, Ordering::SeqCst);
            queue.clear(OutputStatus::Invalidated);
            let bytes = transaction.reports.iter().map(Vec::len).sum();
            let now = Instant::now();
            queue.push(Pending {
                generation: self.generation(),
                upload: transaction.into(),
                surface: None,
                completion: None,
                scene_completion: None,
                admission: None,
                per_report_guard: None,
                queued_at: now,
                first_queued_at: now,
                bytes,
            });
            queue.draining = true;
            self.shared.ready.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.disconnect();
    }
}
impl Drop for OutputWorker {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.disconnect();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Transport for Capture {
        // Write for the current operation
        fn write(&mut self, data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().extend(data);
            Ok(())
        }
    }
    #[test]
    // Verify keyboard per report guard stops late OS mode after blocked write
    fn keyboard_per_report_guard_stops_late_os_mode_after_blocked_write() {
        use std::sync::mpsc;
        struct BlockingTransport {
            reports: Arc<Mutex<Vec<Vec<u8>>>>,
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Transport for BlockingTransport {
            // Write for the current operation
            fn write(&mut self, report: &[u8]) -> Result<()> {
                self.reports.lock().unwrap().push(report.to_vec());
                if report == b"LLUM" {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(())
            }
        }
        let reports = Arc::new(Mutex::new(Vec::new()));
        let (entered, observed) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(BlockingTransport {
                reports: reports.clone(),
                entered,
                release: blocked,
            }),
            30,
            OutputPolicy::default(),
        )
        .unwrap();
        // Move a synthetic deadline while the first call is blocked: deterministic,
        // with no dependence on OS scheduling or real hardware.
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(60)));
        let guard_deadline = deadline.clone();
        let receipt = worker
            .submit_tracked_per_report_guarded(
                1,
                DeviceTransaction {
                    reports: vec![b"LLUM".to_vec(), b"COLOR".to_vec(), b"CPOS".to_vec()],
                    coalesce_key: None,
                },
                Box::new(move || Instant::now() < *guard_deadline.lock().unwrap()),
            )
            .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        *deadline.lock().unwrap() = Instant::now();
        assert!(
            receipt.try_recv().is_err(),
            "in-flight transport call is not cancelled"
        );
        release.send(()).unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Invalidated
        );
        assert_eq!(*reports.lock().unwrap(), vec![b"LLUM".to_vec()]);
        drop(worker);
    }
    #[test]
    // Verify persistent guard is checked after queueing and only once before transfer
    fn persistent_guard_is_checked_after_queueing_and_only_once_before_transfer() {
        use std::sync::{atomic::AtomicBool, mpsc};
        struct BarrierTransport {
            captured: Arc<Mutex<Vec<u8>>>,
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
            valid: Arc<AtomicBool>,
        }
        impl Transport for BarrierTransport {
            // Write for the current operation
            fn write(&mut self, data: &[u8]) -> Result<()> {
                self.captured.lock().unwrap().extend(data);
                if data == [1] {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                if data == [3] {
                    self.valid.store(false, Ordering::SeqCst);
                }
                Ok(())
            }
        }
        let captured = Arc::new(Mutex::new(Vec::new()));
        let valid = Arc::new(AtomicBool::new(true));
        let (entered, observed) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(BarrierTransport {
                captured: captured.clone(),
                entered,
                release: waiting,
                valid: valid.clone(),
            }),
            30,
            OutputPolicy::default(),
        )
        .unwrap();
        worker
            .submit(1, DeviceTransaction::single(vec![1]))
            .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let check = valid.clone();
        let receipt = worker
            .submit_tracked_guarded(
                1,
                DeviceTransaction::single(vec![2]),
                Box::new(move || check.load(Ordering::SeqCst)),
            )
            .unwrap();
        valid.store(false, Ordering::SeqCst);
        release.send(()).unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Invalidated
        );
        assert_eq!(*captured.lock().unwrap(), vec![1]);
        // Admission is the linearization point. Later changes do not truncate
        // a legal persistent group or cause it to be retried.
        valid.store(true, Ordering::SeqCst);
        let check = valid.clone();
        let receipt = worker
            .submit_tracked_guarded(
                1,
                DeviceTransaction {
                    reports: vec![vec![3], vec![4], vec![5]],
                    coalesce_key: None,
                },
                Box::new(move || check.load(Ordering::SeqCst)),
            )
            .unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Completed
        );
        assert_eq!(*captured.lock().unwrap(), vec![1, 3, 4, 5]);
    }
    #[test]
    // Verify transient invalidation keeps explicit persistent work
    fn transient_invalidation_keeps_explicit_persistent_work() {
        use std::sync::mpsc;
        struct Barrier {
            captured: Arc<Mutex<Vec<u8>>>,
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Transport for Barrier {
            // Write for the current operation
            fn write(&mut self, data: &[u8]) -> Result<()> {
                self.captured.lock().unwrap().extend(data);
                if data == [1] {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(())
            }
        }
        let captured = Arc::new(Mutex::new(vec![]));
        let (entered, observed) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Barrier {
                captured: captured.clone(),
                entered,
                release: waiting,
            }),
            30,
            OutputPolicy::default(),
        )
        .unwrap();
        worker
            .submit(1, DeviceTransaction::single(vec![1]))
            .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let image = worker
            .submit_tracked(
                1,
                DeviceTransaction {
                    reports: vec![vec![2]],
                    coalesce_key: Some("key-1".into()),
                },
            )
            .unwrap();
        let boot = worker
            .submit_tracked_guarded(1, DeviceTransaction::single(vec![3]), Box::new(|| true))
            .unwrap();
        worker.invalidate_transient();
        release.send(()).unwrap();
        assert_eq!(
            image.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Invalidated
        );
        assert_eq!(
            boot.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Completed
        );
        assert_eq!(*captured.lock().unwrap(), vec![1, 3]);
    }
    #[test]
    // Verify transaction packets are not interleaved and stale generation is rejected
    fn transaction_packets_are_not_interleaved_and_stale_generation_is_rejected() {
        let captured = Arc::new(Mutex::new(vec![]));
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Capture(captured.clone())),
            20,
            OutputPolicy::default(),
        )
        .unwrap();
        worker
            .submit(
                1,
                DeviceTransaction {
                    reports: vec![vec![1], vec![2], vec![3]],
                    coalesce_key: None,
                },
            )
            .unwrap();
        worker
            .submit(1, DeviceTransaction::single(vec![4]))
            .unwrap();
        for _ in 0..100 {
            if captured.lock().unwrap().len() == 4 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(*captured.lock().unwrap(), vec![1, 2, 3, 4]);
        worker.disconnect();
        assert!(matches!(
            worker.submit(1, DeviceTransaction::single(vec![5])),
            Err(Error::StaleConnection)
        ));
    }
    struct Failed;

    #[test]
    // Verify tracked upload reports completion or transport failure
    fn tracked_upload_reports_completion_or_transport_failure() {
        let captured = Arc::new(Mutex::new(vec![]));
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Capture(captured.clone())),
            20,
            OutputPolicy::default(),
        )
        .unwrap();
        let receipt = worker
            .submit_tracked(
                1,
                DeviceTransaction {
                    reports: vec![vec![1], vec![2]],
                    coalesce_key: None,
                },
            )
            .unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Completed
        );
        assert_eq!(*captured.lock().unwrap(), vec![1, 2]);
        let failed =
            OutputWorker::spawn_with_policy(Box::new(Failed), 20, OutputPolicy::default()).unwrap();
        let receipt = failed
            .submit_tracked(1, DeviceTransaction::single(vec![3]))
            .unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Failed
        );
        assert!(failed.failure().is_some());
    }
    impl Transport for Failed {
        // Write for the current operation
        fn write(&mut self, _: &[u8]) -> Result<()> {
            Err(Error::Transport("unplugged".into()))
        }
    }
    #[test]
    // Verify failed write invalidates connection
    fn failed_write_invalidates_connection() {
        let worker =
            OutputWorker::spawn_with_policy(Box::new(Failed), 20, OutputPolicy::default()).unwrap();
        worker
            .submit(1, DeviceTransaction::single(vec![1]))
            .unwrap();
        for _ in 0..100 {
            if worker.failure().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(worker.generation(), 2);
        assert!(worker.failure().unwrap().contains("unplugged"));
    }
    #[test]
    // Verify obsolete surface updates coalesce behind an atomic transfer
    fn obsolete_surface_updates_coalesce_behind_an_atomic_transfer() {
        struct Gated {
            captured: Arc<Mutex<Vec<u8>>>,
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl Transport for Gated {
            // Write for the current operation
            fn write(&mut self, data: &[u8]) -> Result<()> {
                if data == [1] {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                self.captured.lock().unwrap().extend(data);
                Ok(())
            }
        }
        let captured = Arc::new(Mutex::new(vec![]));
        let (entered, wait) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Gated {
                captured: captured.clone(),
                entered,
                release: gate,
            }),
            20,
            OutputPolicy::default(),
        )
        .unwrap();
        worker
            .set_policy(OutputPolicy {
                mode: OutputMode::IndividualUpdates,
                ..Default::default()
            })
            .unwrap();
        worker
            .submit(
                1,
                DeviceTransaction {
                    reports: vec![vec![1], vec![2]],
                    coalesce_key: None,
                },
            )
            .unwrap();
        wait.recv_timeout(Duration::from_secs(1)).unwrap();
        let replaced = worker
            .submit_tracked(
                1,
                DeviceTransaction {
                    reports: vec![vec![4]],
                    coalesce_key: Some("key-1".into()),
                },
            )
            .unwrap();
        worker
            .submit(
                1,
                DeviceTransaction {
                    reports: vec![vec![5]],
                    coalesce_key: Some("key-2".into()),
                },
            )
            .unwrap();
        worker
            .submit(
                1,
                DeviceTransaction {
                    reports: vec![vec![6]],
                    coalesce_key: Some("key-3".into()),
                },
            )
            .unwrap();
        for value in 10..100 {
            worker
                .submit(
                    1,
                    DeviceTransaction {
                        reports: vec![vec![value]],
                        coalesce_key: Some("key-1".into()),
                    },
                )
                .unwrap();
        }
        assert_eq!(
            replaced.recv_timeout(Duration::from_secs(1)).unwrap(),
            OutputStatus::Replaced
        );
        release.send(()).unwrap();
        for _ in 0..200 {
            if captured.lock().unwrap().len() == 5 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(*captured.lock().unwrap(), vec![1, 2, 99, 5, 6]);
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use crate::FrameCommit;
    use std::sync::mpsc;
    // Construct the policy used by this operation
    fn policy() -> OutputPolicy {
        OutputPolicy {
            mode: OutputMode::FrameScheduling,
            ..Default::default()
        }
    }
    // Resolve the surface value for this operation
    fn surface(id: &str, revision: u64, value: u8) -> SurfaceSubmission {
        SurfaceSubmission {
            surface_id: id.into(),
            content_revision: revision,
            priority: OutputPriority::Animation,
            upload: DeviceTransaction {
                reports: vec![vec![value]],
                coalesce_key: Some(id.into()),
            }
            .into(),
        }
    }
    struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);
    impl Transport for Capture {
        // Write for the current operation
        fn write(&mut self, report: &[u8]) -> Result<()> {
            self.0.lock().unwrap().push(report.to_vec());
            Ok(())
        }
    }
    struct Gated {
        captured: Arc<Mutex<Vec<Vec<u8>>>>,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        gate_byte: u8,
    }
    impl Transport for Gated {
        // Write for the current operation
        fn write(&mut self, report: &[u8]) -> Result<()> {
            if report.first() == Some(&self.gate_byte) {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.captured.lock().unwrap().push(report.to_vec());
            Ok(())
        }
    }
    type GatedWorker = (
        OutputWorker,
        Arc<Mutex<Vec<Vec<u8>>>>,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
    );
    // Construct the gate used by the output test
    fn gated(policy: OutputPolicy) -> GatedWorker {
        let captured = Arc::new(Mutex::new(vec![]));
        let (entered, wait) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Gated {
                captured: captured.clone(),
                entered,
                release: gate,
                gate_byte: 250,
            }),
            20,
            policy,
        )
        .unwrap();
        (worker, captured, wait, release)
    }
    #[test]
    // Verify source paced animation bypasses both worker intervals
    fn source_paced_animation_bypasses_both_worker_intervals() {
        for mode in [OutputMode::IndividualUpdates, OutputMode::FrameScheduling] {
            let captured = Arc::new(Mutex::new(vec![]));
            let worker = OutputWorker::spawn_with_policy(
                Box::new(Capture(captured.clone())),
                20,
                OutputPolicy { mode, ..policy() },
            )
            .unwrap();
            let start = Instant::now();
            for index in 0..12 {
                worker
                    .submit_frame(
                        worker.generation(),
                        worker.scene_revision(),
                        vec![surface("background", index, index as u8)],
                    )
                    .unwrap();
                assert!(worker.wait_idle(Duration::from_millis(100)));
            }
            assert_eq!(captured.lock().unwrap().len(), 12);
            // 20/24 Hz pacing would require at least 458 ms for these 12 writes.
            assert!(start.elapsed() < Duration::from_millis(350));
        }
    }
    #[test]
    // Verify policy validation and default are conservative
    fn policy_validation_and_default_are_conservative() {
        assert_eq!(OutputPolicy::default().mode, OutputMode::FrameScheduling);
        for invalid in [
            OutputPolicy {
                target_frame_hz: 0,
                ..policy()
            },
            OutputPolicy {
                target_frame_hz: 25,
                ..policy()
            },
            OutputPolicy {
                max_batch_surfaces: 129,
                ..policy()
            },
            OutputPolicy {
                max_pending_bytes: 1,
                ..policy()
            },
            OutputPolicy {
                max_batch_estimated_ms: 101,
                ..policy()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
    #[test]
    // Verify frame submissions are atomic coalesced by surface and byte bounded
    fn frame_submissions_are_atomic_coalesced_by_surface_and_byte_bounded() {
        let (worker, captured, entered, release) = gated(OutputPolicy {
            max_batch_bytes: 4,
            max_surface_bytes: 16,
            max_pending_bytes: 32,
            ..policy()
        });
        worker
            .submit(1, DeviceTransaction::single(vec![250]))
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        worker
            .submit_frame(1, 1, vec![surface("a", 1, 1), surface("b", 1, 2)])
            .unwrap();
        for revision in 2..100 {
            worker
                .submit_frame(1, 1, vec![surface("a", revision, revision as u8)])
                .unwrap();
            assert_eq!(worker.queue_stats().pending_bytes, 2);
        }
        let mut oversized = surface("c", 1, 3);
        oversized.upload.transaction.reports = vec![vec![3; 17]];
        assert!(worker
            .submit_frame(1, 1, vec![surface("b", 2, 4), oversized])
            .is_err());
        assert_eq!(worker.queue_stats().pending_bytes, 2);
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        let data = captured.lock().unwrap();
        assert_eq!(data.len(), 3);
        assert!(data.contains(&vec![2]));
        assert!(data.contains(&vec![99]));
        assert!(!data.contains(&vec![4]));
        let stats = worker.queue_stats();
        assert_eq!(stats.pending_bytes, 0);
        assert_eq!(stats.in_flight_bytes, 0);
        assert_eq!(stats.total_completed, 2);
        assert_eq!(stats.total_dropped, 98);
        assert_eq!(
            worker
                .drain_outcomes()
                .iter()
                .filter(|o| o.status == OutputStatus::Completed)
                .count(),
            2
        );
    }
    #[test]
    // Verify explicit group commit is written once without packet interleave
    fn explicit_group_commit_is_written_once_without_packet_interleave() {
        let (worker, captured, entered, release) = gated(policy());
        // The synchronization gate is deliberately not a one-byte throughput
        // sample: scheduler load can otherwise teach an arbitrarily low rate
        // and split this protocol-group test into several legitimate batches.
        // Even the gate's five-second timeout leaves this seed sufficient for
        // the 30-byte admission estimate. Packet grouping assertions stay exact.
        let mut seed = vec![0; 4096];
        seed[0] = 250;
        worker.submit(1, DeviceTransaction::single(seed)).unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let commit = FrameCommit {
            group: "test-keys".into(),
            reports: vec![vec![99]],
        };
        let surfaces = (1..=10)
            .map(|id| {
                let mut s = surface(&format!("key-{id}"), 1, id);
                s.upload.transaction.reports.push(vec![id + 20]);
                s.upload.commit = Some(commit.clone());
                s
            })
            .collect();
        worker.submit_frame(1, 1, surfaces).unwrap();
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        let data = captured.lock().unwrap();
        assert_eq!(data.len(), 22);
        assert_eq!(data.last(), Some(&vec![99]));
        assert_eq!(data.iter().filter(|r| **r == vec![99]).count(), 1);
        let stats = worker.queue_stats();
        assert_eq!(stats.total_commits, 1);
        assert_eq!(stats.total_completed, 10);
        assert_eq!(stats.total_report_bytes, 4096 + 21);
        assert_eq!(
            worker
                .drain_outcomes()
                .iter()
                .map(|o| o.report_bytes)
                .sum::<usize>(),
            21
        );
    }
    #[test]
    // Verify scene change finishes current group but skips selected unsent groups
    fn scene_change_finishes_current_group_but_skips_selected_unsent_groups() {
        let (worker, captured, entered, release) = gated(policy());
        let mut a = surface("a", 1, 250);
        a.upload.transaction.reports.push(vec![251]);
        a.upload.commit = Some(FrameCommit {
            group: "keys".into(),
            reports: vec![vec![99]],
        });
        let mut b = surface("b", 1, 2);
        b.upload.commit = Some(FrameCommit {
            group: "keys".into(),
            reports: vec![vec![99]],
        });
        worker
            .submit_frame(1, 1, vec![a, b, surface("background", 1, 3)])
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.set_scene_revision(2);
        worker
            .submit_frame(1, 2, vec![surface("new", 1, 4)])
            .unwrap();
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(
            *captured.lock().unwrap(),
            vec![vec![250], vec![251], vec![2], vec![99], vec![4]]
        );
        let outcomes = worker.drain_outcomes();
        assert!(outcomes
            .iter()
            .any(|o| o.surface_id == "background" && o.status == OutputStatus::Invalidated));
        assert!(outcomes
            .iter()
            .any(|o| o.surface_id == "a" && o.status == OutputStatus::Completed));
    }
    #[test]
    // Verify preview surface invalidation preserves unrelated and already writing groups
    fn preview_surface_invalidation_preserves_unrelated_and_already_writing_groups() {
        for mode in [OutputMode::IndividualUpdates, OutputMode::FrameScheduling] {
            let (worker, captured, entered, release) = gated(OutputPolicy { mode, ..policy() });
            let mut writing = surface("writing", 1, 250);
            writing.upload.transaction.reports.push(vec![251]);
            worker
                .submit_frame(
                    1,
                    1,
                    vec![writing, surface("preview", 1, 2), surface("live", 1, 3)],
                )
                .unwrap();
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.invalidate_surfaces(&["preview".into()]);
            worker
                .submit_frame(1, 1, vec![surface("preview", 2, 4)])
                .unwrap();
            release.send(()).unwrap();
            assert!(worker.wait_idle(Duration::from_secs(2)));
            let data = captured.lock().unwrap();
            assert!(data.contains(&vec![250]) && data.contains(&vec![251]));
            assert!(!data.contains(&vec![2]));
            assert!(data.contains(&vec![3]) && data.contains(&vec![4]));
            assert!(worker
                .drain_outcomes()
                .iter()
                .any(|o| o.surface_id == "preview"
                    && o.content_revision == 1
                    && o.status == OutputStatus::Invalidated));
        }
    }
    #[test]
    // Verify physical disconnect can stop payload and never reports completion
    fn physical_disconnect_can_stop_payload_and_never_reports_completion() {
        let (worker, captured, entered, release) = gated(policy());
        let mut s = surface("a", 1, 250);
        s.upload.transaction.reports.push(vec![2]);
        worker.submit_frame(1, 1, vec![s]).unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.disconnect();
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(*captured.lock().unwrap(), vec![vec![250]]);
        assert_eq!(worker.queue_stats().total_completed, 0);
        assert!(worker
            .drain_outcomes()
            .iter()
            .all(|o| o.status == OutputStatus::Invalidated));
    }
    #[test]
    // Verify failure at every packet or commit clears queue and cannot complete surface
    fn failure_at_every_packet_or_commit_clears_queue_and_cannot_complete_surface() {
        struct FailAt {
            at: usize,
            writes: usize,
        }
        impl Transport for FailAt {
            // Write for the current operation
            fn write(&mut self, _: &[u8]) -> Result<()> {
                self.writes += 1;
                if self.writes == self.at {
                    Err(Error::Transport("injected".into()))
                } else {
                    Ok(())
                }
            }
        }
        for at in 1..=3 {
            let worker =
                OutputWorker::spawn_with_policy(Box::new(FailAt { at, writes: 0 }), 20, policy())
                    .unwrap();
            let mut s = surface("a", 1, 1);
            s.upload.transaction.reports.push(vec![2]);
            s.upload.commit = Some(FrameCommit {
                group: "keys".into(),
                reports: vec![vec![99]],
            });
            worker
                .submit_frame(1, 1, vec![s, surface("next", 1, 3)])
                .unwrap();
            assert!(!worker.wait_idle(Duration::from_secs(2)));
            let stats = worker.queue_stats();
            assert_eq!(stats.total_completed, 0);
            assert_eq!(stats.pending_bytes, 0);
            assert_eq!(stats.in_flight_bytes, 0);
            assert_eq!(stats.total_failed, 2);
            assert!(worker
                .submit_frame(1, 1, vec![surface("retry", 1, 4)])
                .is_err());
        }
    }
    #[test]
    // Verify policy change rejects invalid and keeps current safe group
    fn policy_change_rejects_invalid_and_keeps_current_safe_group() {
        let (worker, captured, entered, release) = gated(policy());
        let mut s = surface("a", 1, 250);
        s.upload.transaction.reports.push(vec![2]);
        worker
            .submit_frame(1, 1, vec![s, surface("old", 1, 3)])
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(worker
            .set_policy(OutputPolicy {
                target_frame_hz: 0,
                ..policy()
            })
            .is_err());
        assert_eq!(worker.scene_revision(), 1);
        worker
            .set_policy(OutputPolicy {
                mode: OutputMode::IndividualUpdates,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(worker.scene_revision(), 2);
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(*captured.lock().unwrap(), vec![vec![250], vec![2]]);
    }
    #[test]
    // Verify selection budget fairness and exclusive admission use injected time
    fn selection_budget_fairness_and_exclusive_admission_use_injected_time() {
        let now = Instant::now();
        let mut queue = Queue {
            pending: VecDeque::new(),
            closed: false,
            draining: false,
            failure: None,
            measurements: VecDeque::new(),
            outcomes: VecDeque::new(),
            policy: OutputPolicy {
                max_batch_bytes: 4,
                max_surface_bytes: 16,
                max_pending_bytes: 32,
                ..policy()
            },
            stats: OutputQueueStats {
                estimated_bytes_per_ms: 100.0,
                ..Default::default()
            },
        };
        let make = |id: &str, priority, age: Duration, bytes| Pending {
            generation: 1,
            upload: DeviceTransaction {
                reports: vec![vec![1; bytes]],
                coalesce_key: Some(id.into()),
            }
            .into(),
            completion: None,
            scene_completion: None,
            admission: None,
            per_report_guard: None,
            surface: Some(SurfaceMeta {
                epoch: 0,
                id: id.into(),
                revision: 1,
                scene: 1,
                priority,
            }),
            queued_at: now - age,
            first_queued_at: now - age,
            bytes,
        };
        queue.push(make(
            "background",
            OutputPriority::Animation,
            Duration::from_millis(250),
            10,
        ));
        queue.push(make(
            "feedback",
            OutputPriority::Interaction,
            Duration::ZERO,
            1,
        ));
        let batch = queue.take_batch(now);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].surface.as_ref().unwrap().id, "background");
        assert_eq!(queue.pending.len(), 1);
        let batch = queue.take_batch(now);
        assert_eq!(batch[0].surface.as_ref().unwrap().id, "feedback");
        assert_eq!(queue.stats.pending_bytes, 0);
        // The same monotonic selection math is stable under 2x/5x/10x transfer cost
        for slow in [2.0, 5.0, 10.0] {
            queue.stats.estimated_bytes_per_ms = 0.1 / slow;
            queue.push(make("a", OutputPriority::Animation, Duration::ZERO, 1));
            queue.push(make("b", OutputPriority::Animation, Duration::ZERO, 1));
            let batch = queue.take_batch(now);
            let admitted = if slow == 2.0 { 2 } else { 1 };
            assert_eq!(batch.len(), admitted);
            assert_eq!(queue.stats.pending_bytes, 2 - admitted);
            queue.clear(OutputStatus::Invalidated);
        }
    }
    #[test]
    // Verify embedded commit counts and outcome overflow are explicit
    fn embedded_commit_counts_and_outcome_overflow_are_explicit() {
        let captured = Arc::new(Mutex::new(vec![]));
        let worker =
            OutputWorker::spawn_with_policy(Box::new(Capture(captured)), 20, policy()).unwrap();
        let mut s = surface("legacy-key", 1, 1);
        s.upload.transaction.reports.push(vec![99]);
        s.upload.embedded_commit_count = 1;
        worker.submit_frame(1, 1, vec![s]).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(worker.queue_stats().total_commits, 1);
        assert_eq!(worker.drain_outcomes()[0].commit_count, 1);
        let mut queue = worker.shared.queue.lock().unwrap();
        for revision in 0..4100 {
            queue.record(Some(OutputOutcome {
                surface_id: "key".into(),
                content_revision: revision,
                scene_revision: 1,
                status: OutputStatus::Replaced,
                queue_ms: 0.0,
                transfer_ms: 0.0,
                report_bytes: 0,
                commit_count: 0,
            }));
        }
        assert_eq!(queue.outcomes.len(), 4096);
        assert_eq!(queue.stats.outcomes_lost, 4);
    }
    #[test]
    // Verify idle device has no output and devices have independent budgets
    fn idle_device_has_no_output_and_devices_have_independent_budgets() {
        let first = Arc::new(Mutex::new(vec![]));
        let second = Arc::new(Mutex::new(vec![]));
        let a = OutputWorker::spawn_with_policy(Box::new(Capture(first.clone())), 20, policy())
            .unwrap();
        let b = OutputWorker::spawn_with_policy(Box::new(Capture(second.clone())), 20, policy())
            .unwrap();
        assert!(a.wait_idle(Duration::from_millis(1)));
        assert!(first.lock().unwrap().is_empty());
        b.submit_frame(1, 1, vec![surface("b", 1, 2)]).unwrap();
        assert!(b.wait_idle(Duration::from_secs(1)));
        assert_eq!(b.queue_stats().total_completed, 1);
        assert_eq!(a.queue_stats().total_completed, 0);
    }
    #[test]
    // Verify frozen scene ignores soft budgets and cannot be replaced or interleaved
    fn frozen_scene_ignores_soft_budgets_and_cannot_be_replaced_or_interleaved() {
        for mode in [OutputMode::IndividualUpdates, OutputMode::FrameScheduling] {
            let (worker, captured, entered, release) = gated(OutputPolicy {
                mode,
                max_batch_bytes: 1,
                max_batch_surfaces: 1,
                max_batch_estimated_ms: 1,
                ..policy()
            });
            worker
                .submit(1, DeviceTransaction::single(vec![250]))
                .unwrap();
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut scene = vec![surface("wallpaper", 1, 10)];
            for id in 1..=10 {
                let mut item = surface(&format!("key-{id}"), 1, id);
                if mode == OutputMode::FrameScheduling {
                    item.upload.commit = Some(FrameCommit {
                        group: "keys".into(),
                        reports: vec![vec![99]],
                    });
                } else {
                    item.upload.transaction.reports.push(vec![99]);
                    item.upload.embedded_commit_count = 1;
                }
                scene.push(item);
            }
            scene.push(surface("touch", 1, 11));
            let receipt = worker.submit_scene(1, 1, scene).unwrap();
            worker
                .submit_frame(1, 1, vec![surface("key-1", 2, 42)])
                .unwrap();
            worker
                .submit_frame(1, 1, vec![surface("key-1", 3, 43)])
                .unwrap();
            assert!(matches!(receipt.try_recv(), Err(mpsc::TryRecvError::Empty)));
            release.send(()).unwrap();
            assert_eq!(
                receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
                OutputStatus::Completed
            );
            assert!(worker.wait_idle(Duration::from_secs(2)));
            let data = captured.lock().unwrap();
            assert_eq!(data[1], vec![10]);
            assert_eq!(data[data.len() - 2], vec![11]);
            assert_eq!(data.last(), Some(&vec![43]));
            assert!(!data.contains(&vec![42]));
            assert_eq!(
                data.iter().filter(|r| **r == vec![99]).count(),
                if mode == OutputMode::FrameScheduling {
                    1
                } else {
                    10
                }
            );
            assert_eq!(worker.queue_stats().total_completed, 13);
        }
    }
    #[test]
    // Verify superseded scene completes started commit only and invalidates receipt
    fn superseded_scene_completes_started_commit_only_and_invalidates_receipt() {
        let (worker, captured, entered, release) = gated(OutputPolicy {
            max_batch_bytes: 1,
            max_batch_surfaces: 1,
            ..policy()
        });
        let mut first = surface("key-1", 1, 250);
        let mut second = surface("key-2", 1, 2);
        let commit = Some(FrameCommit {
            group: "keys".into(),
            reports: vec![vec![99]],
        });
        first.upload.commit = commit.clone();
        second.upload.commit = commit;
        let receipt = worker
            .submit_scene(1, 1, vec![first, surface("touch", 1, 3), second])
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.set_scene_revision(2);
        let next = worker
            .submit_scene(1, 2, vec![surface("next", 1, 4)])
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Invalidated
        );
        assert_eq!(
            next.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Completed
        );
        assert_eq!(
            *captured.lock().unwrap(),
            vec![vec![250], vec![2], vec![99], vec![4]]
        );
    }
    #[test]
    // Verify grouped scene checks shared contract once before any report
    fn grouped_scene_checks_shared_contract_once_before_any_report() {
        let (worker, captured, _, _) = gated(policy());
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = checks.clone();
        let surfaces = (1..=32)
            .map(|index| {
                let mut item = surface(&format!("key-{index}"), 1, index);
                item.upload.commit = Some(FrameCommit {
                    group: "keys".into(),
                    reports: vec![vec![99]],
                });
                item
            })
            .collect();
        let receipt = worker
            .submit_scene_guarded(
                1,
                1,
                surfaces,
                Some(Arc::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    true
                })),
            )
            .unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Completed
        );
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        assert_eq!(captured.lock().unwrap().len(), 33);
    }
    #[test]
    // Verify scene admission is atomic and receipt reports every failure boundary
    fn scene_admission_is_atomic_and_receipt_reports_every_failure_boundary() {
        struct FailAt(usize);
        impl Transport for FailAt {
            // Write for the current operation
            fn write(&mut self, _: &[u8]) -> Result<()> {
                self.0 -= 1;
                if self.0 == 0 {
                    Err(Error::Transport("injected".into()))
                } else {
                    Ok(())
                }
            }
        }
        for boundary in 1..=4 {
            let worker =
                OutputWorker::spawn_with_policy(Box::new(FailAt(boundary)), 20, policy()).unwrap();
            let mut key = surface("key", 1, 2);
            key.upload.transaction.reports.push(vec![3]);
            key.upload.commit = Some(FrameCommit {
                group: "keys".into(),
                reports: vec![vec![99]],
            });
            let receipt = worker
                .submit_scene(1, 1, vec![key, surface("touch", 1, 4)])
                .unwrap();
            assert_eq!(
                receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
                OutputStatus::Failed
            );
        }
        let (worker, captured, entered, release) = gated(OutputPolicy {
            max_batch_bytes: 1,
            max_surface_bytes: 4,
            max_pending_bytes: 4,
            ..policy()
        });
        worker
            .submit(1, DeviceTransaction::single(vec![250]))
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(worker
            .submit_scene(
                1,
                1,
                (0..5).map(|id| surface(&id.to_string(), 1, id)).collect()
            )
            .is_err());
        assert_eq!(worker.queue_depth(), 0);
        let receipt = worker
            .submit_scene(1, 1, vec![surface("pending", 1, 1)])
            .unwrap();
        worker.set_scene_revision(2);
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(2)).unwrap(),
            OutputStatus::Invalidated
        );
        release.send(()).unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(*captured.lock().unwrap(), vec![vec![250]]);
    }
}
