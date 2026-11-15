//! In-memory effects for the production Rust action traversal. No native fallback
use super::*;
use crate::macro_debug::SimulationConfig;
#[derive(Clone)]
pub(super) struct Simulation {
    pub config: SimulationConfig,
    pub elapsed_ms: u64,
    indices: HashMap<String, usize>,
    slots: BTreeMap<String, Value>,
    responses: HashMap<String, usize>,
    pub profile: String,
    pub page: String,
    history: Vec<(String, String)>,
}
impl Simulation {
    // Initialize simulation state from the supplied inputs
    pub fn new(config: SimulationConfig, profile: String, page: String) -> Self {
        Self {
            config,
            elapsed_ms: 0,
            indices: HashMap::new(),
            slots: BTreeMap::new(),
            responses: HashMap::new(),
            profile,
            page,
            history: vec![],
        }
    }
    // Consume the next simulated response, repeating the final configured array value
    fn response(&mut self, key: &str) -> Option<Value> {
        let value = self.config.responses.get(key)?;
        if let Some(values) = value.as_array() {
            let index = self.responses.entry(key.into()).or_default();
            let value = values.get(*index).or_else(|| values.last()).cloned();
            *index += 1;
            value
        } else {
            Some(value.clone())
        }
    }
}
// Apply the action to simulated state instead of performing its external side effect
pub(super) fn effect(
    run: &mut Run,
    action: &Action,
    depth: usize,
    path: Vec<usize>,
) -> Option<Result<(), String>> {
    let shared = run.simulation.clone()?;
    let kind = match action {
        Action::Builtin { action } => serde_json::to_value(action).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string(),
        _ => serde_json::to_value(action).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string(),
    };
    let response = shared.lock().unwrap().response(&kind);
    if let Some(error) = response
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
    {
        return Some(Err(error.into()));
    }
    if matches!(
        action,
        Action::Macro { .. } | Action::Dial { .. } | Action::Delay { .. }
    ) {
        return None;
    }
    if let Action::Builtin { action } = action {
        if matches!(
            **action,
            BuiltinAction::RepeatActions { .. }
                | BuiltinAction::Conditional { .. }
                | BuiltinAction::WaitCondition { .. }
                | BuiltinAction::Parallel { .. }
                | BuiltinAction::SavedMacro { .. }
                | BuiltinAction::FailurePolicy { .. }
        ) {
            return None;
        }
    }
    let result = (|| {
        match action {
            Action::Toggle { on, off } => {
                let key = format!("{path:?}");
                let index = {
                    let mut s = shared.lock().unwrap();
                    let n = s.indices.entry(key).or_default();
                    let index = *n % 2;
                    *n += 1;
                    index
                };
                let mut p = path.clone();
                p.push(index);
                execute(run, if index == 0 { on } else { off }, depth + 1, p)?;
            }
            Action::Plugin {
                plugin_id,
                action_id,
                ..
            } => {
                let response = shared
                    .lock()
                    .unwrap()
                    .response(&format!("{plugin_id}/{action_id}"));
                if let Some(error) = response
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(Value::as_str)
                {
                    return Err(error.into());
                }
                run.debug.as_ref().unwrap().record(
                    path.clone(),
                    "plugin_result",
                    run.runtime
                        .credentials
                        .redact(response.unwrap_or(Value::Null).to_string()),
                );
            }
            Action::Profile { profile_id } => {
                let mut state = shared.lock().unwrap();
                if state.profile.is_empty() {
                    let profile = run
                        .parameters
                        .as_ref()
                        .unwrap()
                        .profiles
                        .iter()
                        .find(|profile| profile.id == *profile_id)
                        .ok_or("Profile missing")?;
                    state.page.clone_from(&profile.active_page);
                }
                state.profile.clone_from(profile_id);
            }
            Action::Page { page_id } => {
                let mut state = shared.lock().unwrap();
                if state.profile.is_empty() {
                    return Err("Select a test profile before switching pages".into());
                }
                state.page.clone_from(page_id);
            }
            Action::Navigate { target, direction } => {
                let mut state = shared.lock().unwrap();
                let (profile, page) = crate::actions::navigation_destination(
                    &run.parameters.as_ref().unwrap().profiles,
                    &state.profile,
                    &state.page,
                    *target,
                    *direction,
                )?;
                state.profile = profile;
                state.page = page;
            }
            Action::SwipeNavigation { .. } if shared.lock().unwrap().profile.is_empty() => {
                return Err("Select a test profile before testing navigation".into());
            }
            Action::Builtin { action } => {
                use BuiltinAction::*;
                match &**action {
                    KeyboardMacro {
                        recording,
                        playback,
                    } => {
                        let summary =
                            crate::keyboard_macro::validation::validate(recording, playback)?;
                        run.sleep(summary.playback_duration_ms as i32)?;
                    }
                    TypeText { text, template } | PasteText { text, template, .. } => {
                        let text = run.text(text, *template)?;
                        run.debug.as_ref().unwrap().record(
                            path.clone(),
                            "input",
                            run.runtime.credentials.redact(text),
                        );
                    }
                    SetClipboard { text, template } => {
                        shared.lock().unwrap().config.clipboard = run.text(text, *template)?
                    }
                    ClearClipboard => shared.lock().unwrap().config.clipboard.clear(),
                    TransformText { operation } => {
                        let mut s = shared.lock().unwrap();
                        s.config.selection = transform(&s.config.selection, operation);
                    }
                    ClipboardSlot {
                        name, operation, ..
                    } => {
                        let mut s = shared.lock().unwrap();
                        match operation.as_str() {
                            "save" => {
                                let v = json!(s.config.clipboard);
                                s.slots.insert(name.clone(), v);
                            }
                            "restore" => {
                                s.config.clipboard = s
                                    .slots
                                    .get(name)
                                    .map(display)
                                    .ok_or("Clipboard slot is empty")?
                            }
                            _ => {
                                s.slots.remove(name);
                            }
                        }
                    }
                    DateTime { format, .. } => {
                        let s = shared.lock().unwrap();
                        let time = chrono::DateTime::parse_from_rfc3339(
                            s.config.clock.as_deref().unwrap_or("2020-01-01T00:00:00Z"),
                        )
                        .map_err(|_| "Simulation clock must be RFC3339")?
                            + chrono::Duration::milliseconds(s.elapsed_ms as i64);
                        run.debug.as_ref().unwrap().record(
                            path.clone(),
                            "input",
                            time.format(format).to_string(),
                        );
                    }
                    InputPrompt { output, .. } | ChoosePath { output, .. } => {
                        run.locals
                            .insert(output.clone(), response.clone().unwrap_or(json!("")));
                    }
                    CommandResult { output, .. } => {
                        let result = response
                            .clone()
                            .unwrap_or(json!({"stdout":"","stderr":"","exit_status":0}));
                        run.locals.insert(output.clone(), result.clone());
                        for key in ["stdout", "stderr", "exit_status"] {
                            run.locals
                                .insert(format!("{output}.{key}"), result[key].clone());
                        }
                        if result["exit_status"] != 0 {
                            return Err("Command exited unsuccessfully; result retained".into());
                        }
                    }
                    WriteFile {
                        path,
                        text,
                        operation,
                        replace,
                        template,
                    } => {
                        let path = run.text(path, *template)?;
                        let text = run.text(text, *template)?;
                        let mut s = shared.lock().unwrap();
                        if operation == "append" {
                            s.config.files.entry(path).or_default().push_str(&text);
                        } else if (operation == "create" || !replace)
                            && s.config.files.contains_key(&path)
                        {
                            return Err("Destination already exists".into());
                        } else {
                            s.config.files.insert(path, text);
                        }
                        if s.config.files.values().map(String::len).sum::<usize>() > 1024 * 1024 {
                            return Err("Simulated files exceed 1 MiB".into());
                        }
                    }
                    FileOperation {
                        operation,
                        source,
                        destination,
                        replace,
                        template,
                    } => {
                        let source = run.text(source, *template)?;
                        let destination = run.text(destination, *template)?;
                        let mut s = shared.lock().unwrap();
                        let text = s
                            .config
                            .files
                            .get(&source)
                            .cloned()
                            .ok_or("Source file is missing")?;
                        if operation == "trash" {
                            s.config.files.remove(&source);
                        } else {
                            if !replace && s.config.files.contains_key(&destination) {
                                return Err("Destination already exists".into());
                            }
                            s.config.files.insert(destination, text);
                            if operation == "move" || operation == "rename" {
                                s.config.files.remove(&source);
                            }
                        }
                    }
                    CycleActions { children } => {
                        let index = {
                            let mut s = shared.lock().unwrap();
                            let n = s.indices.entry(format!("{path:?}")).or_default();
                            let index = *n % children.len().max(1);
                            *n += 1;
                            index
                        };
                        branch(run, children, index, depth, &path)?;
                    }
                    RandomAction { children }
                    | ActionMenu { children, .. }
                    | Gesture { children, .. } => {
                        let choice = shared.lock().unwrap().config.choice;
                        branch(run, children, choice, depth, &path)?;
                    }
                    RepeatHeld { children, .. } => sequence(run, children, depth, &path)?,
                    PressRelease { children } => sequence(run, children, depth, &path)?,
                    Cooldown {
                        children,
                        milliseconds,
                    } => {
                        let ready = {
                            let mut s = shared.lock().unwrap();
                            let now = s.elapsed_ms as usize;
                            let last = s.indices.get(&format!("cooldown:{path:?}")).copied();
                            if last.is_none_or(|last| {
                                now.saturating_sub(last) >= *milliseconds as usize
                            }) {
                                s.indices.insert(format!("cooldown:{path:?}"), now);
                                true
                            } else {
                                false
                            }
                        };
                        if ready {
                            branch(run, children, 0, depth, &path)?;
                        }
                    }
                    ResetState { .. } => {
                        let mut s = shared.lock().unwrap();
                        s.indices.clear();
                        s.slots.clear();
                    }
                    PageBack => {
                        let mut s = shared.lock().unwrap();
                        if s.profile.is_empty() {
                            return Err("Select a test profile before switching pages".into());
                        }
                        if let Some((profile, page)) = s.history.pop() {
                            s.profile = profile;
                            s.page = page;
                        }
                    }
                    MomentaryPage { page_id } => {
                        let mut s = shared.lock().unwrap();
                        if !run
                            .parameters
                            .as_ref()
                            .unwrap()
                            .profiles
                            .iter()
                            .find(|p| p.id == s.profile)
                            .is_some_and(|p| p.pages.iter().any(|p| p.id == *page_id))
                        {
                            return Err("Page missing".into());
                        }
                        let old = std::mem::replace(&mut s.page, page_id.clone());
                        s.page = old;
                    }
                    PageStep { direction, wrap } => {
                        let mut s = shared.lock().unwrap();
                        let profile = run
                            .parameters
                            .as_ref()
                            .unwrap()
                            .profiles
                            .iter()
                            .find(|p| p.id == s.profile)
                            .ok_or("Profile missing")?;
                        let index = profile
                            .pages
                            .iter()
                            .position(|p| p.id == s.page)
                            .ok_or("Page missing")?;
                        let mut next = index as i32 + if direction == "previous" { -1 } else { 1 };
                        if *wrap {
                            next = next.rem_euclid(profile.pages.len() as i32);
                        }
                        if let Some(page) = usize::try_from(next)
                            .ok()
                            .and_then(|i| profile.pages.get(i))
                        {
                            let old = (s.profile.clone(), s.page.clone());
                            s.history.push(old);
                            s.page = page.id.clone();
                        }
                    }
                    BindingLayer { .. } | EditBinding { .. }
                        if shared.lock().unwrap().profile.is_empty() =>
                    {
                        return Err("Select a test profile for this action".into());
                    }
                    TriggerBinding {
                        profile_id,
                        page_id,
                        control_id,
                        ..
                    } => {
                        let s = shared.lock().unwrap();
                        let profile = if profile_id.is_empty() {
                            &s.profile
                        } else {
                            profile_id
                        };
                        let page = if page_id.is_empty() { &s.page } else { page_id };
                        let call = format!("binding:{profile}:{page}:{control_id}");
                        if run.calls.contains(&call) {
                            return Err("Binding reference cycle".into());
                        }
                        let action = run
                            .parameters
                            .as_ref()
                            .unwrap()
                            .profiles
                            .iter()
                            .find(|p| p.id == *profile)
                            .and_then(|p| p.pages.iter().find(|p| p.id == *page))
                            .and_then(|p| p.bindings.get(control_id))
                            .filter(|b| !b.disabled)
                            .ok_or("Target binding missing or disabled")?
                            .action
                            .clone();
                        let scope = format!("{profile}:{page}");
                        drop(s);
                        let mut child = run.clone();
                        child.calls.push(call);
                        Arc::make_mut(&mut child.request).binding_scope = scope;
                        let mut p = path.clone();
                        p.push(0);
                        execute(&mut child, &action, depth + 1, p)?;
                    }
                    // Every remaining variant is an isolated recorded effect. Adding a
                    // native handler cannot bypass this exhaustive simulation boundary
                    _ => {}
                }
            }
            _ => {}
        }
        let detail = {
            let simulation = shared.lock().unwrap();
            let mut action = action.clone();
            action.try_visit_fields_mut(&mut |_, _| Ok(()), &mut |_, _, settings| {
                *settings = Value::Null;
                Ok::<_, String>(())
            })?;
            let mut action =
                serde_json::to_value(action).map_err(|_| "Cannot summarize simulation")?;
            crate::keyboard_macro::redact_keyboard_recordings(&mut action);
            json!({"action":action,"clipboard":simulation.config.clipboard,"page":simulation.page})
                .to_string()
        };
        run.debug.as_ref().unwrap().record(
            path,
            "simulated",
            run.runtime.credentials.redact(detail),
        );
        Ok(())
    })();
    Some(result)
}

#[cfg(test)]
mod keyboard_macro_tests {
    use super::*;
    #[test]
    // Verify that diagnostic redaction bounds arbitrary JSON timing without panicking
    fn diagnostic_redaction_bounds_arbitrary_json_timing_without_panicking() {
        for speed in [json!(0), json!(-1), json!("invalid"), Value::Null] {
            let mut value = json!({"opaque":{"kind":"keyboard_macro","recording":{"events":[
                {"delay_ms":u64::MAX,"key":"private"},{"delay_ms":1,"key":"private"}
            ]},"playback":{"speed":speed}}});
            crate::keyboard_macro::redact_keyboard_recordings(&mut value);
            assert_eq!(value["opaque"]["duration_ms"], u64::MAX);
            assert_eq!(value["opaque"]["playback_duration_ms"], u64::MAX);
            assert!(!value.to_string().contains("private"));
        }
    }
    #[test]
    // Verify that diagnostic redaction removes sequences from nested and direct effects
    fn diagnostic_redaction_removes_sequences_from_nested_and_direct_effects() {
        let mut value = json!({"kind":"builtin","action":{"kind":"cycle_actions","children":[
            {"kind":"builtin","action":{"kind":"keyboard_macro","recording":{"events":[
                {"key":"KeyS","kind":"key_down","delay_ms":0},
                {"key":"KeyS","kind":"key_up","delay_ms":100}
            ],"layout":"private-layout"},"playback":{"speed":2.0}}}
        ]}});
        crate::keyboard_macro::redact_keyboard_recordings(&mut value);
        let encoded = value.to_string();
        for private in ["KeyS", "private-layout", "key_down", "events"] {
            assert!(!encoded.contains(private));
        }
        let summary = &value["action"]["children"][0]["action"];
        assert_eq!(summary["event_count"], 2);
        assert_eq!(summary["duration_ms"], 100);
        assert_eq!(summary["playback_duration_ms"], 50);
    }
}
