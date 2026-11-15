//! One bounded preparation/transfer at a time per physical connection
use super::*;

const READINESS_WAIT: Duration = Duration::from_millis(150);
const RENDER_SLICE: Duration = Duration::from_millis(4);

pub(super) struct Transition {
    pub(super) generation: u64,
    pub(super) started: Instant,
    scope: String,
    sources: Vec<(String, String)>,
    profile: Option<crate::Profile>,
    assets: Option<crate::asset_leases::AssetLease>,
    context: Option<crate::render_scene::RenderSceneContext>,
    cursor: usize,
    commands: Vec<DeviceCommand>,
    source_bytes: usize,
    receipt: Option<mpsc::Receiver<OutputStatus>>,
    submitted: Option<Instant>,
    pub failed: bool,
}
impl Transition {
    // Initialize device transitions state from the supplied inputs
    pub(super) fn new(
        generation: u64,
        profile: Option<crate::Profile>,
        assets: crate::asset_leases::AssetLease,
        session: &crate::workspace::DeviceSession,
        overrides: &BTreeMap<String, crate::builtins::EffectiveBinding>,
        manifest: &ModelManifest,
    ) -> Self {
        let mut sources = Vec::new();
        let scope = format!("{}:{}", session.profile_id, session.page_id);
        if let Some(page) = profile
            .as_ref()
            .and_then(|p| p.pages.iter().find(|p| p.id == session.page_id))
        {
            for control in manifest.controls.iter().filter(|c| c.image_slot.is_some()) {
                let source = page.surface_source(manifest, &control.id);
                let scope = overrides
                    .get(source)
                    .map(|v| v.scope.clone())
                    .unwrap_or_else(|| scope.clone());
                if !sources.contains(&(source.to_owned(), scope.clone())) {
                    sources.push((source.into(), scope));
                }
            }
        }
        Self {
            generation,
            started: Instant::now(),
            scope,
            sources,
            profile,
            assets: Some(assets),
            context: None,
            cursor: 0,
            commands: Vec::new(),
            source_bytes: 0,
            receipt: None,
            submitted: None,
            failed: false,
        }
    }
    // Mark the device transitions operation as failed
    pub(super) fn fail(&mut self, runtime: &Runtime, device: &str, error: String) {
        self.failed = true;
        self.commands.clear();
        runtime.metrics.record("transition_failures", 1.0);
        runtime.emit(
            "device-error",
            Some(device.into()),
            None,
            Some(format!("Page transfer did not complete: {error}")),
        );
    }
    // Retain the selected device transitions entries
    fn retain(
        &mut self,
        c: &Connection,
        command: DeviceCommand,
    ) -> std::result::Result<(), String> {
        let bytes = match &command {
            DeviceCommand::Image { data, .. } | DeviceCommand::Background { data, .. } => {
                data.len()
            }
            _ => 0,
        };
        self.source_bytes = self.source_bytes.saturating_add(bytes);
        if bytes > c.policy.max_surface_bytes
            || self.source_bytes > c.policy.max_pending_bytes
            || self.commands.len() >= 128
        {
            return Err("Prepared scene exceeds the bounded output budget".into());
        }
        self.commands.push(command);
        Ok(())
    }
}

// Reject the invalid device transitions request
pub(super) fn reject(runtime: &Runtime, c: &mut Connection, generation: u64, error: String) {
    let session = runtime.snapshot().session(&c.device_id);
    let mut transition = Transition {
        generation,
        started: Instant::now(),
        scope: format!("{}:{}", session.profile_id, session.page_id),
        sources: Vec::new(),
        profile: None,
        assets: None,
        context: None,
        cursor: 0,
        commands: Vec::new(),
        source_bytes: 0,
        receipt: None,
        submitted: None,
        failed: false,
    };
    transition.fail(runtime, &c.device_id, error);
    c.transition = Some(transition);
}

// Advance the device transitions state machine
pub(super) fn advance(runtime: &Runtime, c: &mut Connection) {
    let Some(mut transition) = c.transition.take() else {
        return;
    };
    if transition.failed {
        c.transition = Some(transition);
        return;
    }
    let current = runtime.transition_inputs.lock().unwrap().current(
        &c.device_id,
        c.epoch,
        transition.generation,
    );
    let session = runtime.snapshot().session(&c.device_id);
    if !current || format!("{}:{}", session.profile_id, session.page_id) != transition.scope {
        c.transition = Some(transition);
        restore(runtime, c);
        return;
    }
    if transition.receipt.is_none()
        && transition
            .context
            .as_ref()
            .is_some_and(|context| !context.current(runtime))
    {
        c.transition = Some(transition);
        restore(runtime, c);
        return;
    }
    if let Some(receipt) = &transition.receipt {
        match receipt.try_recv() {
            Ok(OutputStatus::Completed) => {
                runtime.metrics.record(
                    "transition_transfer_ms",
                    transition.submitted.unwrap().elapsed().as_secs_f64() * 1000.0,
                );
                runtime.metrics.record("transition_completions", 1.0);
                runtime.transition_inputs.lock().unwrap().complete(
                    &c.device_id,
                    c.epoch,
                    transition.generation,
                );
                c.next_frame = Instant::now();
                // Soft changes accumulated in PluginHost during the frozen transfer
                // Their dirty entries remain coalesced until the next actor cycle
                refresh_live(runtime, c);
                return;
            }
            Ok(status) => transition.fail(runtime, &c.device_id, format!("{status:?}")),
            Err(mpsc::TryRecvError::Disconnected) => {
                transition.fail(runtime, &c.device_id, "Output receipt disconnected".into())
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        c.transition = Some(transition);
        return;
    }
    if transition.context.is_none() {
        let ready = runtime.plugins.lock().unwrap().transition_readiness(
            &c.device_id,
            &transition.sources,
            Instant::now(),
        );
        if !ready && transition.started.elapsed() < READINESS_WAIT {
            c.transition = Some(transition);
            return;
        }
        if !ready {
            runtime.metrics.record("transition_readiness_timeouts", 1.0);
        }
        runtime.metrics.record(
            "transition_plugin_wait_ms",
            transition.started.elapsed().as_secs_f64() * 1000.0,
        );
        match runtime.render_scene(&c.device_id) {
            Ok(context) => {
                let snapshot = runtime.snapshot();
                if !context.matches_snapshot(&snapshot) {
                    c.transition = Some(transition);
                    return;
                }
                transition.profile = snapshot
                    .profiles
                    .iter()
                    .find(|p| p.id == session.profile_id)
                    .cloned();
                let ids = context
                    .page
                    .iter()
                    .flat_map(|p| p.bindings.values())
                    .chain(context.overrides.values().map(|v| &v.binding))
                    .filter_map(|b| b.asset.clone())
                    .chain(
                        context
                            .background_media(transition.profile.as_ref())
                            .map(|(asset, _)| asset.to_owned()),
                    );
                match runtime
                    .profiles
                    .asset_leases
                    .pin(ids, "Prepared device scene")
                {
                    Ok(assets) => transition.assets = Some(assets),
                    Err(error) => {
                        transition.fail(runtime, &c.device_id, error);
                        c.transition = Some(transition);
                        return;
                    }
                }
                transition.context = Some(context);
            }
            Err(_) => {
                c.transition = Some(transition);
                return;
            }
        }
    }
    if !transition.context.as_ref().unwrap().current(runtime) {
        c.transition = Some(transition);
        restore(runtime, c);
        return;
    }
    let deadline = Instant::now() + RENDER_SLICE;
    let surfaces: Vec<_> = c
        .manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some())
        .cloned()
        .collect();
    // Index zero is the independently committed wallpaper. At most two surfaces
    // per turn (and a time slice between surfaces) leave polling/devices runnable
    for _ in 0..2 {
        let context = transition.context.as_ref().unwrap();
        let command = if transition.cursor == 0 {
            c.manifest
                .capabilities
                .background_size
                .map(|[width, height]| {
                    runtime
                        .render_scene_wallpaper(
                            context,
                            transition.profile.as_ref(),
                            [width, height],
                            c.manifest.layout.rotation,
                            &c.manifest.capabilities.image_formats,
                        )
                        .map(|data| DeviceCommand::Background {
                            data: runtime.owned_render_bytes(data),
                            x: 0,
                            y: 0,
                            width,
                            height,
                        })
                })
                .transpose()
        } else if let Some(control) = surfaces.get(transition.cursor - 1) {
            let source = context
                .page
                .as_ref()
                .map(|p| p.surface_source(&c.manifest, &control.id))
                .unwrap_or(&control.id);
            let binding = context.page.as_ref().and_then(|p| p.bindings.get(source));
            runtime
                .render_bound_surface_in_scene(
                    control,
                    binding,
                    c.manifest.layout.rotation,
                    &context.scope,
                    source,
                    context,
                )
                .or_else(|error| {
                    runtime.metrics.record("transition_render_fallbacks", 1.0);
                    runtime.emit("media-error", Some(c.device_id.clone()), None, Some(error));
                    hardware_core::render::visual(
                        &serde_json::json!({"background": context.device_surface}),
                        control.image_width.unwrap_or(1),
                        control.image_height.unwrap_or(1),
                        c.manifest.layout.rotation,
                    )
                    .map(crate::media::shared_bytes)
                    .map_err(|e| e.to_string())
                })
                .map(|data| {
                    Some(DeviceCommand::Image {
                        control_id: control.id.clone(),
                        data: runtime.owned_render_bytes(data),
                    })
                })
        } else {
            break;
        };
        transition.cursor += 1;
        if let Err(error) = command
            .and_then(|command| command.map_or(Ok(()), |command| transition.retain(c, command)))
        {
            transition.fail(runtime, &c.device_id, error);
            c.transition = Some(transition);
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    if transition.cursor > surfaces.len() {
        // Recheck navigation while holding its admission lock. Mapping changes
        // close this gate before publishing, so a stale scene cannot be admitted
        if !transition.context.as_ref().unwrap().current(runtime) {
            c.transition = Some(transition);
            restore(runtime, c);
            return;
        }
        let input_gate = runtime.transition_inputs.lock().unwrap();
        if !input_gate.current(&c.device_id, c.epoch, transition.generation) {
            drop(input_gate);
            c.transition = Some(transition);
            restore(runtime, c);
            return;
        }
        match submit(c, std::mem::take(&mut transition.commands)) {
            Ok(receipt) => {
                transition.receipt = Some(receipt);
                transition.submitted = Some(Instant::now());
                runtime.metrics.record(
                    "transition_preparation_ms",
                    transition.started.elapsed().as_secs_f64() * 1000.0,
                );
                drop(input_gate);
                install_playbacks(runtime, c, &transition);
            }
            Err(error) => {
                drop(input_gate);
                transition.fail(runtime, &c.device_id, error);
            }
        }
    }
    c.transition = Some(transition);
}

// Submit the device transitions work item
pub(super) fn submit(
    c: &mut Connection,
    commands: Vec<DeviceCommand>,
) -> std::result::Result<mpsc::Receiver<OutputStatus>, String> {
    let encoding = if !c.force_individual_updates
        && c.policy.mode == OutputMode::FrameScheduling
        && c.manifest
            .capabilities
            .frame_output
            .as_ref()
            .is_some_and(|f| f.grouped_key_updates)
        && c.session
            .lock()
            .unwrap()
            .supports_frame_encoding(FrameEncoding::GroupedKeyUpdates)
    {
        FrameEncoding::GroupedKeyUpdates
    } else {
        FrameEncoding::IndividualUpdates
    };
    let commands = commands
        .into_iter()
        .map(|command| prepare_command(c, command))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let metadata: Vec<_> = commands
        .iter()
        .map(|command| (command_surface(command).unwrap(), command_digest(command)))
        .collect();
    let uploads = c
        .session
        .lock()
        .unwrap()
        .encode_frame(commands, encoding)
        .map_err(|e| e.to_string())?;
    if uploads.len() != metadata.len() {
        return Err("Encoder did not preserve scene surfaces".into());
    }
    let mut deliveries = Vec::new();
    let submissions = uploads
        .into_iter()
        .zip(metadata)
        .map(|(upload, (surface_id, digest))| {
            c.content_revision = c.content_revision.wrapping_add(1).max(1);
            deliveries.push((surface_id.clone(), digest, c.content_revision));
            SurfaceSubmission {
                upload,
                surface_id,
                content_revision: c.content_revision,
                priority: OutputPriority::Interaction,
            }
        })
        .collect();
    let receipt = c
        .worker
        .submit_scene_guarded(
            c.worker.generation(),
            c.scene_revision,
            submissions,
            c.image_admission(),
        )
        .map_err(|e| e.to_string())?;
    for (surface, digest, revision) in deliveries {
        c.delivery.insert(
            surface,
            Delivery {
                digest,
                revision,
                pending: true,
                completed: false,
            },
        );
    }
    Ok(receipt)
}

// Install playbacks
fn install_playbacks(runtime: &Runtime, c: &mut Connection, transition: &Transition) {
    let context = transition.context.as_ref().unwrap();
    let mut media = Vec::new();
    if let Some((asset, options)) = context.background_media(transition.profile.as_ref()) {
        media.push((
            "@background".to_string(),
            None,
            asset.to_owned(),
            options.clone(),
        ));
    }
    for control in c
        .manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some())
    {
        let Some(page) = &context.page else {
            continue;
        };
        let source = page.surface_source(&c.manifest, &control.id);
        let binding = context
            .overrides
            .get(source)
            .map(|v| &v.binding)
            .or_else(|| page.bindings.get(source));
        if let Some(binding) = binding {
            if let Some(asset) = &binding.asset {
                media.push((
                    control.id.clone(),
                    Some(control.id.clone()),
                    asset.clone(),
                    binding.media.clone(),
                ));
            }
        }
    }
    for (key, control, asset, media) in media {
        if let Some(clip) = runtime
            .profiles
            .clip(&asset)
            .ok()
            .flatten()
            .filter(|c| c.frames.len() > 1)
            .and_then(|c| runtime.profiles.media_budget.protect(c).ok())
        {
            let assets = runtime
                .profiles
                .asset_leases
                .pin([asset.clone()], "Transition playback")
                .unwrap_or_else(|_| {
                    transition
                        .assets
                        .as_ref()
                        .expect("prepared scene assets")
                        .clone()
                });
            c.start_playback(
                key,
                Playback {
                    vendor_started: None,
                    asset,
                    clip: Some(clip),
                    _assets: assets,
                    control,
                    format: "png".into(),
                    last_frame: None,
                    next_sample: Duration::ZERO,
                    pending_frame: None,
                    media,
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "device_transition_tests.rs"]
mod tests;
