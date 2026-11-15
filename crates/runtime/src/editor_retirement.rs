//! Conservative snapshots after the native editor admission fence has drained
//! These checks do not replace that fence: native commands must own their full
//! operation, including blocking workers and callbacks, until they settle
use crate::Runtime;
use std::sync::Mutex;

/// Contention or poisoning retains the window. Never wait while committing a
/// window transition, and release each service snapshot before inspecting another
pub(crate) fn unavailable_or<T>(lock: &Mutex<T>, blocked: impl FnOnce(&T) -> bool) -> bool {
    match lock.try_lock() {
        Ok(value) => blocked(&value),
        Err(_) => true,
    }
}

impl Runtime {
    /// Passive main-editor readers can outlive a reloaded page: its asynchronous
    /// unsubscribe calls are not guaranteed to reach Rust. Cancel from native state
    /// at document bootstrap and after fencing a confirmed close. Keep worker entries
    /// until actual exit; their EditorWork permits must still drain before destruction
    pub fn retire_editor_readers(&self) {
        crate::keyboard_macro::recorder::cancel_owner("main");
        crate::keyboard_macro::test_session::cancel_owner("main");
        for cancel in self.workspace_cancellations.lock().unwrap().values() {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.preview_subscriptions.cancel_all();
        self.plugin_logs.unsubscribe_owner("main");
    }

    /// Frontend-owned native state which must survive until its explicit release
    /// Persistent profiles/history, reusable media caches, hardware playback and
    /// independently hosted browser/inspector/log windows are not editor owners
    pub fn editor_retirement_blocked(&self) -> bool {
        !self.editor_retirement_blockers().is_empty()
    }

    /// Static categories only: safe for close errors and diagnostics, without paths,
    /// settings, plugin output, or user-selected names. Busy locks remain blockers
    pub fn editor_retirement_blockers(&self) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        macro_rules! occupied {
            ($field:ident, $name:literal) => {
                if unavailable_or(&self.$field, |entries| !entries.is_empty()) {
                    blockers.push($name);
                }
            };
        }
        occupied!(media_selections, "media selections");
        if self.preview_subscriptions.occupied() {
            blockers.push("device preview readers");
        }
        occupied!(workspace_cancellations, "workspace readers");
        occupied!(imports, "profile import reviews");
        occupied!(exports, "export reviews");
        occupied!(plugin_profile_imports, "plugin profile import reviews");
        occupied!(macro_imports, "macro import reviews");
        occupied!(conversions, "profile conversion reviews");
        occupied!(plugin_packages, "plugin package reviews");
        occupied!(plugin_rollbacks, "plugin rollback reviews");
        occupied!(media_cleanups, "media cleanup reviews");
        occupied!(media_deletions, "media deletion reviews");
        for (blocked, name) in [
            (
                crate::keyboard_macro::recorder::editor_retirement_blocked(),
                "keyboard macro recording cleanup",
            ),
            (
                crate::keyboard_macro::test_session::editor_retirement_blocked(),
                "keyboard macro test cleanup",
            ),
            (
                self.preparations.editor_retirement_blocked(),
                "media preparation",
            ),
            (
                self.video.editor_retirement_blocked(),
                "video editing or recording",
            ),
            (
                self.boot_logos.editor_retirement_blocked(),
                "startup logo preparation",
            ),
            (
                self.benchmark.editor_retirement_blocked(),
                "benchmark cleanup",
            ),
            (
                self.setup_capabilities.editor_retirement_blocked(),
                "setup checks",
            ),
            (
                self.plugin_logs.has_readers_for_owner("main"),
                "plugin log readers",
            ),
            (
                unavailable_or(&self.plugins, |plugins| plugins.has_settings_drafts()),
                "plugin settings drafts",
            ),
        ] {
            if blocked {
                blockers.push(name);
            }
        }
        let debug_blocked = match self.builtin_state.debug_sessions.try_lock() {
            Ok(sessions) => {
                let values = sessions.values().cloned().collect::<Vec<_>>();
                drop(sessions);
                values
                    .iter()
                    .any(|session| session.editor_retirement_blocked())
            }
            Err(_) => true,
        };
        if debug_blocked {
            blockers.push("macro debug sessions");
        }
        blockers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Wait for editor owners to retire and fail the test if they exceed two seconds
    fn unblocked(runtime: &Runtime) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime.editor_retirement_blocked() {
            assert!(
                Instant::now() < deadline,
                "Native editor owner did not retire"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    // Verify that frontend media and subscription owners block but playback and persisted drafts do not
    fn frontend_media_and_subscription_owners_block_but_playback_and_persisted_drafts_do_not() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        unblocked(&runtime);
        let path = root.path().join("frames.gif");
        {
            let mut encoder =
                image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
            for color in [[20, 30, 40, 255], [40, 30, 20, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(4, 4, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let asset = runtime.profiles.import_asset(&path).unwrap();
        let clip = runtime.profiles.clip(&asset).unwrap().unwrap();
        let playback = runtime.profiles.media_budget.protect(clip).unwrap();
        let pin = runtime
            .profiles
            .asset_leases
            .pin([asset.clone()], "independent playback")
            .unwrap();
        runtime.drafts.lock().unwrap().insert(
            "saved draft".into(),
            crate::workspace::Draft {
                key: "saved draft".into(),
                base: None,
                value: serde_json::json!({"title":"recoverable"}),
            },
        );
        unblocked(&runtime);
        let selection = runtime.retain_media(asset).unwrap();
        assert!(runtime.editor_retirement_blocked());
        runtime.release_media(&selection);
        unblocked(&runtime);
        let preview = runtime.preview_subscriptions.register("main".into());
        assert!(runtime.editor_retirement_blocked());
        drop(preview);
        runtime
            .workspace_cancellations
            .lock()
            .unwrap()
            .insert("main".into(), Default::default());
        assert!(runtime.editor_retirement_blocked());
        runtime.workspace_cancellations.lock().unwrap().clear();
        unblocked(&runtime);
        drop((playback, pin));
        runtime.shutdown();
    }

    #[test]
    // Verify that native reader retirement preserves workers and unrelated editor resources
    fn native_reader_retirement_preserves_workers_and_unrelated_editor_resources() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let workspace = Arc::new(AtomicBool::new(false));

        runtime
            .workspace_cancellations
            .lock()
            .unwrap()
            .insert("lost-workspace".into(), workspace.clone());
        let preview = runtime
            .preview_subscriptions
            .register("lost-preview".into());
        let path = root.path().join("image.png");
        image::RgbaImage::new(4, 4).save(&path).unwrap();
        let selection = runtime.import_asset(&path).unwrap();

        runtime.retire_editor_readers();
        assert!(workspace.load(Ordering::SeqCst));
        assert!(preview.canceled());
        let blockers = runtime.editor_retirement_blockers();
        for name in [
            "workspace readers",
            "device preview readers",
            "media selections",
        ] {
            assert!(blockers.contains(&name), "Missing blocker: {name}");
        }
        // Only the workers acknowledge their exit by removing their entries
        runtime.workspace_cancellations.lock().unwrap().clear();
        drop(preview);
        assert!(runtime
            .editor_retirement_blockers()
            .contains(&"media selections"));
        runtime.release_media(&selection);
        unblocked(&runtime);
        runtime.shutdown();
    }

    #[test]
    // Verify that background log capture and auxiliary readers survive main retirement
    fn background_log_capture_and_auxiliary_readers_survive_main_retirement() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let capture = runtime
            .plugin_logs
            .start("fixture.background", "Background plugin")
            .unwrap();
        let independent = runtime
            .plugin_logs
            .subscribe_owned(&capture.id, "log-reader-independent")
            .unwrap();
        assert!(runtime
            .plugin_logs
            .list()
            .iter()
            .any(|capture| capture.capturing));
        unblocked(&runtime);
        let main = runtime
            .plugin_logs
            .subscribe_owned(&capture.id, "main")
            .unwrap();
        assert!(runtime.editor_retirement_blocked());
        runtime.retire_editor_readers();
        assert!(!runtime.plugin_logs.has_readers_for_owner("main"));
        runtime.plugin_logs.unsubscribe(&main.id);
        unblocked(&runtime);
        assert!(runtime
            .plugin_logs
            .list()
            .iter()
            .any(|capture| capture.capturing));
        assert!(runtime
            .plugin_logs
            .has_readers_for_owner("log-reader-independent"));
        runtime.plugin_logs.unsubscribe(&independent.id);
        runtime.shutdown();
    }

    #[test]
    // Verify that busy snapshots retain without waiting and debug cancellation waits for completion
    fn busy_snapshots_retain_without_waiting_and_debug_cancellation_waits_for_completion() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        {
            let _owned = runtime.imports.lock().unwrap();
            assert!(runtime.editor_retirement_blocked());
            assert!(runtime
                .editor_retirement_blockers()
                .contains(&"profile import reviews"));
        }
        let session = crate::macro_debug::Session::new(
            crate::macro_debug::DebugMode::Simulation,
            vec![],
            false,
        );
        runtime
            .builtin_state
            .debug_sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session.clone());
        assert!(runtime.editor_retirement_blocked());
        session
            .cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(runtime.editor_retirement_blocked());
        session.complete(crate::actions::RunOutcome::Cancelled);
        unblocked(&runtime);
        runtime.shutdown();
    }
}
