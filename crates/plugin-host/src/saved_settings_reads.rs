//! Ordered reads fence only writes accepted before this session/context's getter
use super::*;

// A session admits eight writes. Allow four lookups per write (including both
// getter kinds); globally allow two per admitted write. Deferred responses stay
// below the transport's 256-message queue, with at most 128 KiB of request IDs.
const MAX_SESSION_READS: usize = 32;
const MAX_READS: usize = 128;

pub(super) struct SavedRead {
    owner: String,
    session: String,
    context: String,
    resources: bool,
    id: Option<Value>,
    writes: HashSet<String>,
}
impl Hub {
    // Read saved state
    pub(super) fn read_saved_state(
        &mut self,
        owner: &str,
        session: &str,
        context: &str,
        resources: bool,
        id: Option<&Value>,
    ) -> Result<(), String> {
        if self.sessions.tokens.get(session).map(String::as_str) != Some(owner)
            || self.context_state.contexts.get(context).map(String::as_str) != Some(owner)
            || self
                .sessions
                .files
                .get(session)
                .and_then(|grant| grant.context.as_deref())
                .is_some_and(|granted| granted != context)
        {
            return Err("Unowned saved-state read".into());
        }
        if id.is_some_and(|id| id.as_str().is_none_or(|id| id.len() > 1024)) {
            return Err("Invalid saved-state request identifier".into());
        }
        let writes = self
            .pending_settings
            .iter()
            .filter(|(_, (writer, target))| writer == session && target == context)
            .map(|(operation, _)| operation.clone())
            .collect::<HashSet<_>>();
        let read = SavedRead {
            owner: owner.into(),
            session: session.into(),
            context: context.into(),
            resources,
            id: id.cloned(),
            writes,
        };
        if read.writes.is_empty() {
            self.publish_saved_read(read);
        } else {
            if self.pending_saved_reads.len() >= MAX_READS
                || self
                    .pending_saved_reads
                    .iter()
                    .filter(|read| read.session == session)
                    .count()
                    >= MAX_SESSION_READS
            {
                return Err("Saved-state read queue is full".into());
            }
            self.pending_saved_reads.push_back(read);
        }
        Ok(())
    }
    // Publish saved read
    fn publish_saved_read(&mut self, read: SavedRead) {
        if self.sessions.tokens.get(&read.session) != Some(&read.owner)
            || self.context_state.contexts.get(&read.context) != Some(&read.owner)
        {
            return;
        }
        let inspector = self
            .sessions
            .files
            .get(&read.session)
            .and_then(|grant| grant.context.as_ref());
        if inspector.is_some_and(|context| context != &read.context) {
            return;
        }
        let mut event = if read.resources {
            self.resources_event(&read.context)
        } else {
            let settings = if inspector.is_some() {
                self.context_state
                    .settings
                    .get(&read.context)
                    .cloned()
                    .unwrap_or(json!({}))
            } else {
                self.execution_settings(&read.context, &json!({}))
            };
            self.settings_event(&read.context, &settings)
        };
        if let Some(id) = read.id {
            event["id"] = id;
        }
        let queue = if inspector.is_some() {
            format!("pi:{}", read.session)
        } else {
            read.owner
        };
        self.sessions
            .queues
            .entry(queue)
            .or_default()
            .push_back(event);
    }
    // Flush saved reads
    pub(super) fn flush_saved_reads(&mut self) {
        for mut read in std::mem::take(&mut self.pending_saved_reads) {
            read.writes
                .retain(|operation| self.pending_settings.contains_key(operation));
            if read.writes.is_empty() {
                self.publish_saved_read(read);
            } else {
                self.pending_saved_reads.push_back(read);
            }
        }
    }
    // Forget saved reads for session
    pub(super) fn forget_saved_reads_for_session(&mut self, session: &str) {
        self.pending_saved_reads
            .retain(|read| read.session != session);
    }
    // Forget saved reads for context
    pub(super) fn forget_saved_reads_for_context(&mut self, context: &str) {
        self.pending_saved_reads
            .retain(|read| read.context != context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct the isolated test fixture
    fn fixture(root: &Path) -> (PluginHost, std::sync::mpsc::Receiver<OwnedSettingsWrite>) {
        let mut host = lifecycle_tests::host(root);
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        host.set_settings_writer(sender);
        let mut hub = host.hub.lock().unwrap();
        for session in ["session", "other-session"] {
            hub.sessions.tokens.insert(session.into(), "vendor".into());
        }
        for context in ["action", "unrelated"] {
            hub.context_state
                .contexts
                .insert(context.into(), "vendor".into());
            hub.context_state
                .context_actions
                .insert(context.into(), "vendor.action".into());
            hub.context_state
                .addresses
                .insert(context.into(), json!({"control_id":context,"path":[]}));
            hub.context_state
                .settings
                .insert(context.into(), json!({"marker":"old"}));
        }
        drop(hub);
        (host, receiver)
    }
    // Queue for the current operation
    fn queue(host: &PluginHost, value: Value, resources: bool) {
        host.hub
            .lock()
            .unwrap()
            .queue_owned_write(
                "vendor",
                "session",
                "action",
                if resources {
                    Value::Null
                } else {
                    value.clone()
                },
                resources.then_some(value),
            )
            .unwrap();
    }
    // Read for the current operation
    fn read(host: &PluginHost, id: &str, context: &str, resources: bool) {
        host.hub
            .lock()
            .unwrap()
            .read_saved_state("vendor", "session", context, resources, Some(&json!(id)))
            .unwrap();
    }
    // Collect the events emitted by this operation
    fn events(host: &PluginHost) -> Vec<Value> {
        let mut hub = host.hub.lock().unwrap();
        let queue = hub.sessions.queues.entry("vendor".into()).or_default();
        let mut result = vec![];
        while let Some(event) = queue.pop_front() {
            result.push(event)
        }
        result
    }
    #[test]
    // Verify getters fence prior accepted writes not later writes or other contexts
    fn getters_fence_prior_accepted_writes_not_later_writes_or_other_contexts() {
        let root = tempfile::tempdir().unwrap();
        let (mut host, receiver) = fixture(root.path());
        queue(&host, json!({"marker":"first"}), false);
        let first = receiver.recv().unwrap();
        read(&host, "after-first", "action", false);
        queue(&host, json!({"marker":"second"}), false);
        let second = receiver.recv().unwrap();
        read(&host, "after-both", "action", false);
        read(&host, "independent", "unrelated", false);
        host.hub
            .lock()
            .unwrap()
            .read_saved_state(
                "vendor",
                "other-session",
                "action",
                false,
                Some(&json!("other-session")),
            )
            .unwrap();
        let ready = events(&host);
        assert_eq!(
            ready
                .iter()
                .map(|e| e["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["independent", "other-session"]
        );
        assert!(ready
            .iter()
            .all(|e| e["payload"]["settings"]["marker"] == "old"));
        host.commit_saved_settings(&first, 0, |_| Ok(())).unwrap();
        let ready = events(&host);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["id"], "after-first");
        assert_eq!(ready[0]["payload"]["settings"]["marker"], "first");
        host.reject_saved_settings(&second, "injected rejection");
        let ready = events(&host);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["id"], "after-both");
        assert_eq!(ready[0]["payload"]["settings"]["marker"], "first");
    }
    #[test]
    // Verify resource reads wait for commit and rejected transactions return committed state
    fn resource_reads_wait_for_commit_and_rejected_transactions_return_committed_state() {
        let root = tempfile::tempdir().unwrap();
        let (mut host, receiver) = fixture(root.path());
        queue(&host, json!({"file":"managed-file"}), true);
        let request = receiver.recv().unwrap();
        read(&host, "resources-1", "action", true);
        assert!(events(&host).is_empty());
        host.commit_saved_settings(&request, 0, |_| Ok(())).unwrap();
        let ready = events(&host);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["id"], "resources-1");
        assert_eq!(ready[0]["payload"]["resources"]["file"], "managed-file");
        queue(&host, json!({"file":"never-committed"}), true);
        let request = receiver.recv().unwrap();
        read(&host, "resources-failed", "action", true);
        let failed: Result<(), String> =
            host.commit_saved_settings(&request, 0, |_| Err("injected storage rejection".into()));
        assert!(failed.is_err());
        let ready = events(&host);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["id"], "resources-failed");
        assert_eq!(ready[0]["payload"]["resources"]["file"], "managed-file");
        assert!(host.hub.lock().unwrap().pending_saved_reads.is_empty());
    }
    #[test]
    // Verify read fences are bounded and revocation never publishes to reused owners
    fn read_fences_are_bounded_and_revocation_never_publishes_to_reused_owners() {
        let root = tempfile::tempdir().unwrap();
        let (mut host, receiver) = fixture(root.path());
        queue(&host, json!({"marker":"pending"}), false);
        let request = receiver.recv().unwrap();
        for index in 0..MAX_SESSION_READS {
            read(&host, &index.to_string(), "action", false);
        }
        assert!(host
            .hub
            .lock()
            .unwrap()
            .read_saved_state(
                "vendor",
                "session",
                "action",
                false,
                Some(&json!("overflow"))
            )
            .is_err());
        assert!(host
            .hub
            .lock()
            .unwrap()
            .read_saved_state(
                "vendor",
                "other-session",
                "action",
                false,
                Some(&json!("x".repeat(1025)))
            )
            .is_err());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.revoke_storage_session("session");
            hub.sessions.tokens.remove("session");
            assert!(hub.pending_saved_reads.is_empty());
        }
        host.reject_saved_settings(&request, "revoked");
        assert!(events(&host).is_empty());
        let mut hub = host.hub.lock().unwrap();
        assert!(hub
            .read_saved_state("vendor", "session", "action", false, None)
            .is_err());
        assert!(hub
            .read_saved_state("foreign", "other-session", "action", false, None)
            .is_err());
    }
    #[test]
    // Verify global read budget is shared and context retirement clears its fences
    fn global_read_budget_is_shared_and_context_retirement_clears_its_fences() {
        let mut hub = Hub::default();
        hub.context_state
            .contexts
            .insert("action".into(), "vendor".into());
        for session in 0..5 {
            let session = format!("session-{session}");
            hub.sessions.tokens.insert(session.clone(), "vendor".into());
            hub.pending_settings.insert(
                format!("write-{session}"),
                (session.clone(), "action".into()),
            );
            for read in 0..MAX_SESSION_READS {
                let result = hub.read_saved_state(
                    "vendor",
                    &session,
                    "action",
                    false,
                    Some(&json!(format!("{session}-{read}"))),
                );
                if session == "session-4" {
                    assert!(result.is_err());
                } else {
                    result.unwrap();
                }
            }
        }
        assert_eq!(hub.pending_saved_reads.len(), MAX_READS);
        hub.retire_context("action");
        assert!(hub.pending_saved_reads.is_empty());
        hub.flush_saved_reads();
        assert!(!hub.sessions.queues.contains_key("vendor"));
    }
    #[test]
    // Verify full device getters do not block unrelated contexts or starve ready reads
    fn full_device_getters_do_not_block_unrelated_contexts_or_starve_ready_reads() {
        let root = tempfile::tempdir().unwrap();
        let (mut host, receiver) = fixture(root.path());
        let mut held = vec![];
        for index in 0..24 {
            let context = format!("key-{index}");
            {
                let mut hub = host.hub.lock().unwrap();
                hub.context_state
                    .contexts
                    .insert(context.clone(), "vendor".into());
                hub.context_state
                    .context_actions
                    .insert(context.clone(), "vendor.action".into());
                hub.context_state
                    .addresses
                    .insert(context.clone(), json!({"control_id":context,"path":[]}));
                hub.context_state
                    .settings
                    .insert(context.clone(), json!({"marker":"old"}));
                if index < 8 {
                    hub.queue_saved_settings(
                        "vendor",
                        "session",
                        &context,
                        json!({"marker":index}),
                    )
                    .unwrap();
                }
            }
            if index < 8 {
                held.push(receiver.recv().unwrap());
            }
            read(&host, &format!("settings-{index}"), &context, false);
            read(&host, &format!("resources-{index}"), &context, true);
        }
        assert_eq!(host.hub.lock().unwrap().pending_saved_reads.len(), 16);
        let ready = events(&host);
        assert_eq!(ready.len(), 32);
        assert!(ready.iter().all(|e| e["context"].as_str().unwrap()[4..]
            .parse::<usize>()
            .unwrap()
            >= 8));
        // Complete the eighth independent context first: the first seven cannot
        // hold its getters behind a queue-wide barrier.
        host.commit_saved_settings(&held[7], 0, |_| Ok(())).unwrap();
        let ready = events(&host);
        assert_eq!(ready.len(), 2);
        assert!(ready.iter().all(|e| e["context"] == "key-7"));
        for request in &held[..7] {
            host.commit_saved_settings(request, 0, |_| Ok(())).unwrap();
        }
        assert_eq!(events(&host).len(), 14);
        assert!(host.hub.lock().unwrap().pending_saved_reads.is_empty());
    }
    #[test]
    // Verify baseline elgato initialization requires correlated global getter but other dialects keep push
    fn baseline_elgato_initialization_requires_correlated_global_getter_but_other_dialects_keep_push(
    ) {
        for (dialect, managed, expected) in [
            ("streamdeck", false, false),
            ("streamdeck", true, true),
            ("vsd", false, true),
            ("manager", true, true),
        ] {
            let mut hub = Hub::default();
            hub.sessions
                .tokens
                .insert("session".into(), "vendor".into());
            hub.manifest_dialects
                .insert("vendor".into(), dialect.into());
            let mut message = json!({"event":"registerPlugin","uuid":"session"});
            if managed {
                let capability = capabilities::Capabilities::default();
                message["payload"]["capabilities"] = serde_json::to_value(&capability).unwrap();
                hub.capabilities.insert("vendor".into(), capability);
            }
            let poll = mio::Poll::new().unwrap();
            let wake = Arc::new(mio::Waker::new(poll.registry(), mio::Token(7)).unwrap());
            assert!(transport::register(&mut hub, &message, wake).is_some());
            let has_push = hub.sessions.queues["vendor"]
                .iter()
                .any(|e| e["event"] == "didReceiveGlobalSettings" && e.get("id").is_none());
            assert_eq!(has_push, expected, "{dialect}/{managed}");
        }
    }
}
