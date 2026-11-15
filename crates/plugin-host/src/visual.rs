use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};
#[cfg(test)]
#[path = "visual_tests.rs"]
mod tests;
#[derive(Default, Clone)]
pub(super) struct Sources {
    image: Option<[u8; 32]>,
    layout: Option<String>,
    feedback: HashMap<String, ([u8; 32], bool)>,
}
// Read the byte charge of retained data
fn bytes(root: &Path, source: &str) -> Result<Vec<u8>, String> {
    bytes_limited(root, source, 16 * 1024 * 1024)
}
// Read visual bytes within the configured size limit
fn bytes_limited(root: &Path, source: &str, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(data) = source.strip_prefix("data:") {
        let (metadata, data) = data.split_once(',').ok_or("invalid image URI")?;
        let bytes = if metadata.ends_with(";base64") {
            if data.len() > limit.saturating_add(2) / 3 * 4 {
                return Err("image exceeds limits".into());
            }
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| e.to_string())?
        } else {
            percent_encoding::percent_decode_str(data)
                .take(limit + 1)
                .collect()
        };
        if bytes.len() > limit {
            return Err("image exceeds limits".into());
        }
        return Ok(bytes);
    }
    let mut candidates = vec![source.to_string()];
    if Path::new(source).extension().is_none() {
        candidates.extend([
            format!("{source}@2x.png"),
            format!("{source}.png"),
            format!("{source}.jpg"),
            format!("{source}@2x.svg"),
            format!("{source}.svg"),
            format!("{source}@2x.gif"),
            format!("{source}.gif"),
        ]);
    }
    for candidate in candidates {
        if let Ok(path) = safe_file(root, &candidate) {
            let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if metadata.len() > limit as u64 || !metadata.is_file() {
                return Err("image exceeds limits".into());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > limit {
                return Err("image exceeds limits".into());
            }
            return Ok(bytes);
        }
    }
    Err("plugin image is absent or outside the package".into())
}
// Resolve the image using the selected theme
pub(super) fn image_with_theme(root: &Path, source: &str, colors: &Value) -> Result<Value, String> {
    if source.is_empty() {
        return Ok(Value::Null);
    }
    Ok(image_value(bytes(root, source)?, source, colors))
}
// Decode the image value from the plugin payload
fn image_value(mut bytes: Vec<u8>, source: &str, colors: &Value) -> Value {
    let svg = hardware_core::render::is_svg_media(&bytes);
    if svg && !source.starts_with("data:") && !colors.is_null() {
        if let Some(themed) = resolve_svg_roles(&bytes, colors) {
            bytes = themed.into_bytes();
        }
    }
    // Extensionless manifest references and data URIs must retain their real format
    // In particular, keep all GIF frames intact rather than relabeling them as PNG
    let mime = if svg {
        "image/svg+xml"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else {
        "image/png"
    };
    json!(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD
            .encode(hardware_core::icons::transparent_svg(&bytes))
    ))
}
// Resolve SVG roles
pub(super) fn resolve_svg_roles(bytes: &[u8], colors: &Value) -> Option<String> {
    let svg = std::str::from_utf8(bytes).ok()?;
    if !svg.contains("data-sdm-") || bytes.len() > 256 * 1024 {
        return None;
    }
    let doc = roxmltree::Document::parse(svg).ok()?;
    let mut replacements = Vec::new();
    for node in doc.descendants().filter(roxmltree::Node::is_element) {
        for (attribute, role) in [("fill", "data-sdm-fill"), ("stroke", "data-sdm-stroke")] {
            if let Some(role) = node.attribute(role).filter(|role| {
                ["accent", "surface", "ink", "muted", "border", "meter"].contains(role)
            }) {
                if let (Some(color), Some(value)) =
                    (colors[role].as_str(), node.attribute_node(attribute))
                {
                    replacements.push((value.range_value(), color));
                }
            }
        }
    }
    if replacements.is_empty() {
        return None;
    }
    replacements.sort_unstable_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut resolved = svg.to_owned();
    for (range, color) in replacements {
        resolved.replace_range(range, color);
    }
    Some(resolved)
}

// Resolve layout roles
fn resolve_layout_roles(mut layout: Value, colors: &Value) -> (Value, bool) {
    if layout["streamDockManager"]["displayPalette"] != true {
        return (layout, false);
    }
    layout["background"] = colors["surface"].clone();
    if let Some(items) = layout["items"].as_array_mut() {
        for item in items {
            match item["type"].as_str() {
                Some("text") => {
                    let role = match item["key"].as_str() {
                        Some("line1" | "line2" | "label" | "artist" | "detail" | "name") => "muted",
                        _ => "ink",
                    };
                    item["color"] = colors[role].clone();
                }
                Some("bar") => {
                    item["bar_fill_c"] = colors["meter"].clone();
                    item["bar_bg_c"] = colors["border"].clone();
                }
                _ => {}
            }
        }
    }
    (layout, true)
}

// Resolve the display layout declared by the plugin
pub(super) fn declared_layout(root: &Path, name: &str) -> Result<Value, String> {
    if name.starts_with('$') {
        return hardware_core::feedback::builtin(name).map_err(|e| e.to_string());
    }
    let path = safe_file(root, name)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("layout exceeds 1 MiB".into());
    }
    let layout: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if layout["items"]
        .as_array()
        .is_none_or(|items| items.len() > 64)
    {
        return Err("layout must contain at most 64 items".into());
    }
    Ok(layout)
}

// Select the display layout for the current theme
pub(super) fn themed_layout(
    root: &Path,
    name: &str,
    mode: &str,
    palette: &str,
    colors: &Value,
) -> Result<(Value, bool), String> {
    let (mut layout, themed) = if let Some(layout) =
        theme::variant(root, name, mode, palette).and_then(|path| declared_layout(root, &path).ok())
    {
        (layout, true)
    } else {
        resolve_layout_roles(declared_layout(root, name)?, colors)
    };
    resolve_layout_pixmaps(root, &mut layout, mode, palette, colors)?;
    Ok((layout, themed))
}

/// Default pixmaps use the same package-safe loader as runtime feedback, with
/// a bounded aggregate budget before this resolved layout can become visible.
fn resolve_layout_pixmaps(
    root: &Path,
    layout: &mut Value,
    mode: &str,
    palette: &str,
    colors: &Value,
) -> Result<(), String> {
    let mut cache = HashMap::<String, Value>::new();
    let mut input_bytes = 0usize;
    let mut output_bytes = 0usize;
    for item in layout["items"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "pixmap")
    {
        let Some(reference) = item.get("value") else {
            continue;
        };
        if reference.is_null() || reference.as_str() == Some("") {
            continue;
        }
        let source = reference
            .as_str()
            .or_else(|| reference[mode].as_str())
            .ok_or("Invalid layout pixmap default")?;
        let source = themed_source(root, source, mode, palette).into_owned();
        let value = if let Some(value) = cache.get(&source) {
            value.clone()
        } else {
            if cache.len() >= 32 {
                return Err("Layout default pixmaps exceed 32 distinct sources".into());
            }
            let bytes = bytes_limited(root, &source, 2 * 1024 * 1024)?;
            input_bytes += bytes.len();
            if input_bytes > 4 * 1024 * 1024 {
                return Err("Layout default pixmaps exceed 4 MiB source budget".into());
            }
            let value = image_value(bytes, &source, colors);
            cache.insert(source, value.clone());
            value
        };
        output_bytes += value.as_str().map_or(0, str::len);
        if output_bytes > 4 * 1024 * 1024 {
            return Err("Layout default pixmaps exceed 4 MiB output budget".into());
        }
        item["value"] = value;
    }
    Ok(())
}

// Select the media source for the current theme
fn themed_source<'a>(
    root: &Path,
    source: &'a str,
    mode: &str,
    palette: &str,
) -> std::borrow::Cow<'a, str> {
    theme::variant(root, source, mode, palette)
        .or_else(|| {
            let role_asset = format!("semantic/{source}");
            safe_file(root, &role_asset).ok()?;
            Some(role_asset)
        })
        .map(std::borrow::Cow::Owned)
        .unwrap_or_else(|| std::borrow::Cow::Borrowed(source))
}
// Resolve the image declared by the OpenAction state
fn openaction_state_image(action: &Value, state: &Value) -> Value {
    match state["Image"].as_str() {
        None | Some("actionDefaultImage") => action["Icon"].clone(),
        Some(_) => state["Image"].clone(),
    }
}
// Resolve an OpenAction image payload
fn openaction_image_source<'a>(
    root: &Path,
    source: &'a str,
    openaction: bool,
) -> std::borrow::Cow<'a, str> {
    if openaction
        && !source.is_empty()
        && !source.contains(':')
        && Path::new(source).extension().is_none()
    {
        for suffix in [".svg", "@2x.png", ".png"] {
            let candidate = format!("{source}{suffix}");
            if safe_file(root, &candidate).is_ok() {
                return std::borrow::Cow::Owned(candidate);
            }
        }
    }
    std::borrow::Cow::Borrowed(source)
}
// Elgato manifest State fields (pinned manifest-schema/latest.ts); VSD's pinned
// JavaScript sample also serializes FontSize as a decimal string
pub(super) fn manifest_title_parameters(state: &Value) -> Value {
    let mut parameters = json!({});
    for (source, target) in [
        ("FontFamily", "fontFamily"),
        ("FontStyle", "fontStyle"),
        ("FontUnderline", "fontUnderline"),
        ("ShowTitle", "showTitle"),
        ("TitleAlignment", "titleAlignment"),
        ("TitleColor", "titleColor"),
    ] {
        if let Some(value) = state.get(source) {
            parameters[target] = value.clone();
        }
    }
    if let Some(size) = state["FontSize"]
        .as_f64()
        .or_else(|| {
            state["FontSize"]
                .as_str()
                .and_then(|size| size.parse::<f64>().ok())
        })
        .filter(|size| size.is_finite() && *size > 0.0 && *size <= 1024.0)
    {
        parameters["fontSize"] = json!(size);
    }
    parameters
}

// Check whether image caching was explicitly disabled
fn image_cache_disabled(hub: &Hub, owner: &str, context: &str) -> bool {
    let action = hub
        .context_state
        .visible
        .get(context)
        .and_then(|event| event["action"].as_str())
        .or_else(|| {
            hub.context_state
                .context_actions
                .get(context)
                .map(String::as_str)
        });
    action
        .and_then(|action| hub.metadata.get(owner)?.get(action))
        .is_some_and(|action| action["DisableCaching"] == true)
}

// Prepare for the current operation
pub(super) fn prepare(hub: &mut Hub, owner: &str, message: &mut Value) -> Result<(), String> {
    // OpenAction Rust serializes Option fields as explicit null, unlike Elgato's
    // omitted reset fields. Normalize only this dialect before common validation.
    let openaction = hub
        .manifest_dialects
        .get(owner)
        .is_some_and(|d| d == "openaction");
    if openaction && matches!(message["event"].as_str(), Some("setTitle" | "setImage")) {
        if let Some(payload) = message["payload"].as_object_mut() {
            for key in ["state", "title", "image"] {
                if payload.get(key).is_some_and(Value::is_null) {
                    payload.remove(key);
                }
            }
        }
    }
    let event = message["event"].as_str().unwrap_or("");
    // Standard setImage does not support GIF. An omitted image resets to the
    // manifest state image, where animation is supported.
    if event == "setImage" && !openaction {
        if let Some(reference) = message["payload"].get("image") {
            let root = hub.roots.get(owner).ok_or("package root missing")?;
            let device = message["context"]
                .as_str()
                .and_then(|context| hub.context_state.targets.get(context))
                .map(|(device, _)| device.as_str())
                .unwrap_or("");
            let theme = hub.resolved_device_theme_for(owner, device);
            if let Some(source) = reference
                .as_str()
                .or_else(|| reference[theme].as_str())
                .filter(|source| !source.is_empty())
            {
                let source = themed_source(
                    root,
                    source,
                    theme,
                    hub.device_palette_id_for(theme, device),
                );
                let fingerprint: [u8; 32] = Sha256::digest(source.as_bytes()).into();
                let context = message["context"].as_str().unwrap_or("");
                let (sources, visuals) = if message["payload"]["target"] == 2 {
                    (
                        &hub.visual_state.software_sources,
                        &hub.visual_state.software_visuals,
                    )
                } else {
                    (&hub.visual_state.visual_sources, &hub.visual_state.visuals)
                };
                // Reuse only the current target's already validated source. Package/theme
                // invalidation clears this proof, requiring safe_file validation again.
                let cached = sources
                    .get(context)
                    .filter(|entry| {
                        !image_cache_disabled(hub, owner, context)
                            && entry.image == Some(fingerprint)
                    })
                    .and_then(|_| visuals.get(context))
                    .and_then(|visual| visual["image"].as_str());
                let is_gif = if let Some(image) = cached {
                    image.starts_with("data:image/gif;base64,")
                } else {
                    let data = bytes(root, &source)?;
                    data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")
                };
                if is_gif {
                    return Err(
                        "setImage does not support GIF; declare animated images in manifest States"
                            .into(),
                    );
                }
            }
        }
    }
    let target = if matches!(event, "setImage" | "setTitle") {
        match message["payload"].get("target") {
            None => 0,
            Some(value) => value
                .as_u64()
                .filter(|v| *v <= 2)
                .ok_or("Invalid visual target")?,
        }
    } else {
        0
    };
    let context = message["context"].as_str().unwrap_or("").to_owned();
    if target != 0 && !hub.visual_state.visuals.contains_key(&context) {
        let action = hub.context_state.context_actions.get(&context).or_else(|| {
            hub.context_state
                .visible
                .get(&context)
                .and_then(|event| event["action"].as_str())
                .and_then(|id| hub.metadata.get(owner)?.get_key_value(id).map(|(id, _)| id))
        });
        let declared = action
            .and_then(|id| hub.metadata.get(owner)?.get(id))
            .and_then(|action| action["States"].as_array())
            .is_some_and(|states| !states.is_empty());
        if declared {
            prepare_target(
                hub,
                owner,
                &mut json!({"event":"setState","context":context,"payload":{"state":0}}),
                false,
            )?;
            if elgato_multi_context(hub, owner, &context) {
                swap_target(hub);
                let result = prepare_target(
                    hub,
                    owner,
                    &mut json!({"event":"setState","context":context,"payload":{"state":0}}),
                    true,
                );
                swap_target(hub);
                result?;
            }
        } else {
            hub.visual_state.visuals.insert(
                context.clone(),
                Arc::new(json!({"title":"","image":null,"layout":null,"feedback":{}})),
            );
        }
    }
    let shared = match (
        hub.visual_state.visuals.get(&context),
        hub.visual_state.software_visuals.get(&context),
    ) {
        (Some(hardware), Some(software)) => Arc::ptr_eq(hardware, software),
        (_, None) => true,
        _ => false,
    };
    if target == 0 && shared && !elgato_multi_context(hub, owner, &context) {
        prepare_target(hub, owner, message, false)?;
        if let Some(value) = hub.visual_state.visuals.get(&context).cloned() {
            hub.visual_state
                .software_visuals
                .insert(context.clone(), value);
        }
        if let Some(value) = hub.visual_state.visual_sources.get(&context).cloned() {
            hub.visual_state
                .software_sources
                .insert(context.clone(), value);
        }
        return Ok(());
    }
    if !hub.visual_state.software_visuals.contains_key(&context) {
        let order = hub
            .visual_state
            .visual_order
            .get(&context)
            .copied()
            .unwrap_or(0);
        hub.visual_state
            .software_order
            .entry(context.clone())
            .or_insert(order);
        if let Some(value) = hub.visual_state.visuals.get(&context).cloned() {
            hub.visual_state
                .software_visuals
                .insert(context.clone(), value);
        }
        if let Some(value) = hub.visual_state.visual_sources.get(&context).cloned() {
            hub.visual_state
                .software_sources
                .insert(context.clone(), value);
        }
    }
    if target != 1 {
        swap_target(hub);
        let result = prepare_target(hub, owner, &mut message.clone(), true);
        swap_target(hub);
        result?;
    }
    if target != 2 {
        prepare_target(hub, owner, message, false)?;
    }
    Ok(())
}
// Choose the target of the visual state update
fn swap_target(hub: &mut Hub) {
    let state = &mut hub.visual_state;
    std::mem::swap(&mut state.visuals, &mut state.software_visuals);
    std::mem::swap(&mut state.visual_sources, &mut state.software_sources);
}
// Prepare target
fn prepare_target(
    hub: &mut Hub,
    owner: &str,
    message: &mut Value,
    software: bool,
) -> Result<(), String> {
    let event = message["event"].as_str().unwrap_or("").to_string();
    if ![
        "setTitle",
        "setImage",
        "setState",
        "setFeedback",
        "setFeedbackLayout",
        "setTriggerDescription",
        "showAlert",
        "showOk",
    ]
    .contains(&event.as_str())
    {
        return Ok(());
    }
    let context = message["context"]
        .as_str()
        .ok_or("visual context missing")?
        .to_string();
    if event == "setTriggerDescription" {
        let controller = &hub
            .context_state
            .visible
            .get(&context)
            .map(|value| &value["payload"]["controller"]);
        if !controller.is_some_and(|value| value == "Encoder") {
            return Err("Trigger descriptions require an encoder context".into());
        }
        let payload = message["payload"]
            .as_object()
            .ok_or("Trigger descriptions must be an object")?;
        if payload.iter().any(|(key, value)| {
            !["longTouch", "push", "rotate", "touch"].contains(&key.as_str())
                || value.as_str().is_none_or(|text| text.len() > 2048)
        }) {
            return Err("Invalid trigger description".into());
        }
    }
    let openaction = hub
        .manifest_dialects
        .get(owner)
        .is_some_and(|d| d == "openaction");
    let disable_image_cache = image_cache_disabled(hub, owner, &context);
    let root = hub.roots.get(owner).ok_or("package root missing")?.clone();
    let device = hub
        .context_state
        .targets
        .get(&context)
        .map(|(device, _)| device.as_str())
        .unwrap_or("");
    let theme = hub.resolved_device_theme_for(owner, device);
    let palette = hub.device_palette_id_for(theme, device).to_owned();
    let colors = hub.device_colors_for(theme, device);
    let multi_action_image = software && elgato_multi_context(hub, owner, &context);
    let sources = hub
        .visual_state
        .visual_sources
        .entry(context.clone())
        .or_default();
    let metadata = hub.metadata.get(owner);
    let action_id = hub
        .context_state
        .visible
        .get(&context)
        .and_then(|event| event["action"].as_str())
        .or_else(|| {
            hub.context_state
                .context_actions
                .get(&context)
                .map(String::as_str)
        });
    if hub
        .visual_state
        .visuals
        .get(&context)
        .is_some_and(|value| Arc::strong_count(value) > 1)
    {
        hub.visual_state.visual_cow_copies = hub.visual_state.visual_cow_copies.saturating_add(1);
    }
    let visual = hub
        .visual_state
        .visuals
        .entry(context.clone())
        .or_insert_with(|| Arc::new(json!({"title":"","image":null,"layout":null,"feedback":{}})));
    let visual = Arc::make_mut(visual);
    visual["plugin_id"] = json!(owner);
    visual["action_id"] = json!(action_id);
    if visual["states"].is_null() {
        let action = visual["action_id"].as_str().unwrap_or("");
        let states: Vec<_> = metadata.and_then(|m| m.get(action)).and_then(|a| a["States"].as_array()).into_iter().flatten().take(64).enumerate().map(|(index, state)| json!({"index":index,"label":state["Title"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(||format!("State {}",index+1))})).collect();
        visual["states"] = json!(states);
    }
    if disable_image_cache && matches!(event.as_str(), "setFeedback" | "setState") {
        if let Some(name) = visual["layoutReference"].as_str() {
            let (layout, palette_tile) = themed_layout(&root, name, theme, &palette, &colors)?;
            visual["layout"] = layout;
            visual["paletteTile"] = json!(palette_tile);
        }
    }
    if event == "setFeedback" {
        visual["host_feedback_at"] = json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64);
    }
    if matches!(event.as_str(), "setImage" | "setTitle") {
        let action = action_id.and_then(|id| metadata.and_then(|items| items.get(id)));
        let count = action
            .and_then(|action| action["States"].as_array())
            .map_or(1, Vec::len)
            .max(1);
        let selected = match message["payload"].get("state") {
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|state| (*state as usize) < count)
                    .ok_or("Visual state is not defined by the action")? as usize,
            ),
            None => None,
        };
        let key = if event == "setImage" {
            "image"
        } else {
            "title"
        };
        if let Some(value) = message["payload"].get(key) {
            if !(value.is_string()
                || (key == "image" && value.is_object() && hub.capabilities.contains_key(owner)))
            {
                return Err("Invalid visual value".into());
            }
        }
        if !visual["stateOverrides"].is_object() {
            visual["stateOverrides"] = json!({});
        }
        for state in 0..count {
            if selected.is_some_and(|selected| selected != state) {
                continue;
            }
            let index = state.to_string();
            if !visual["stateOverrides"][&index].is_object() {
                visual["stateOverrides"][&index] = json!({});
            }
            if let Some(value) = message["payload"].get(key) {
                visual["stateOverrides"][&index][key] = value.clone();
            } else {
                visual["stateOverrides"][&index]
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
            }
        }
        let active = visual["state"].as_u64().unwrap_or(0) as usize;
        if selected.is_some_and(|selected| selected != active) {
            return Ok(());
        }
        if message["payload"].get(key).is_none() {
            let manifest_key = if key == "image" { "Image" } else { "Title" };
            message["payload"][key] = action
                .map(|action| {
                    let state = &action["States"][active];
                    if key == "image" && multi_action_image {
                        state
                            .get("MultiActionImage")
                            .unwrap_or(&state[manifest_key])
                            .clone()
                    } else if openaction && key == "image" {
                        openaction_state_image(action, state)
                    } else {
                        state[manifest_key].clone()
                    }
                })
                .filter(|value| value.is_string())
                .unwrap_or(json!(""));
        }
    }
    match event.as_str() {
        "setTriggerDescription" => visual["triggerDescriptions"] = message["payload"].clone(),
        "setTitle" => visual["title"] = message["payload"]["title"].clone(),
        "setImage" => {
            let reference = &message["payload"]["image"];
            visual["imageReference"] = reference.clone();
            let source = reference
                .as_str()
                .or_else(|| reference[theme].as_str())
                .unwrap_or("");
            let source = openaction_image_source(&root, source, openaction);
            let source = themed_source(&root, &source, theme, &palette);
            let fingerprint: [u8; 32] = Sha256::digest(source.as_bytes()).into();
            if disable_image_cache || sources.image != Some(fingerprint) {
                visual["image"] = image_with_theme(&root, &source, &colors)?;
                sources.image = Some(fingerprint);
            }
        }
        "setState" => {
            let state = message["payload"]["state"]
                .as_u64()
                .ok_or("invalid state")? as usize;
            let action_id = action_id.ok_or("Plugin action context is no longer available")?;
            let action = metadata
                .and_then(|items| items.get(action_id))
                .ok_or("action metadata missing")?;
            let state_index = state;
            let state = action["States"]
                .get(state)
                .ok_or("state is not defined by the action")?;
            visual["state"] = json!(state_index);
            // Keep the upstream state defaults separate from authored binding overrides
            // The native renderer consumes these only for vendor packages
            if !hub.capabilities.contains_key(owner) {
                visual["titleParameters"] = hub
                    .context_state
                    .vendor_profile_appearances
                    .get(&context)
                    .and_then(|p| p.parameters(state_index as u32))
                    .unwrap_or_else(|| {
                        if openaction {
                            let mut defaults = json!({"ShowTitle":true,"TitleColor":"#FFFFFF","TitleAlignment":"middle","FontStyle":"Regular","FontSize":"16","FontUnderline":false});
                            defaults.as_object_mut().unwrap().extend(state.as_object().into_iter().flatten().map(|(k,v)|(k.clone(),v.clone())));
                            manifest_title_parameters(&defaults)
                        } else { manifest_title_parameters(state) }
                    });
            }
            let overrides = visual["stateOverrides"][state_index.to_string()].clone();
            visual["title"] = overrides.get("title").unwrap_or(&state["Title"]).clone();
            if openaction && visual["title"].is_null() {
                visual["title"] = json!("");
            }
            if let Some(title) = visual["title"].as_str().map(str::to_owned) {
                visual["feedback"]["title"] = json!(title);
            }
            let reference = hub
                .capabilities
                .get(owner)
                .and_then(|cap| cap.actions.get(action_id))
                .and_then(|cap| cap.states.get(state_index))
                .and_then(|asset| serde_json::to_value(asset).ok())
                .unwrap_or_else(|| {
                    if multi_action_image {
                        state
                            .get("MultiActionImage")
                            .unwrap_or(&state["Image"])
                            .clone()
                    } else if openaction {
                        openaction_state_image(action, state)
                    } else {
                        state["Image"].clone()
                    }
                });
            let reference = overrides.get("image").cloned().unwrap_or(reference);
            visual["imageReference"] = reference.clone();
            if let Some(source) = reference.as_str().or_else(|| reference[theme].as_str()) {
                let source = openaction_image_source(&root, source, openaction);
                let source = themed_source(&root, &source, theme, &palette);
                let fingerprint: [u8; 32] = Sha256::digest(source.as_bytes()).into();
                if disable_image_cache || sources.image != Some(fingerprint) {
                    visual["image"] = image_with_theme(&root, &source, &colors)?;
                    sources.image = Some(fingerprint);
                }
            } else {
                visual["image"] = Value::Null;
                sources.image = None;
            }
        }
        "setFeedbackLayout" => {
            let name = message["payload"]["layout"]
                .as_str()
                .ok_or("layout missing")?;
            if disable_image_cache || sources.layout.as_deref() != Some(name) {
                let (layout, palette_tile) = themed_layout(&root, name, theme, &palette, &colors)?;
                visual["layout"] = layout;
                visual["paletteTile"] = json!(palette_tile);
                visual["layoutReference"] = json!(name);
                sources.layout = Some(name.to_owned());
                sources.feedback.clear();
            }
        }

        "setFeedback" => {
            let feedback = message["payload"]
                .as_object()
                .ok_or("feedback must be an object")?;
            if let Some(items) = visual["layout"]["items"].as_array() {
                let keys: Vec<String> = items
                    .iter()
                    .filter(|i| i["type"] == "pixmap")
                    .filter_map(|i| i["key"].as_str().map(String::from))
                    .collect();
                for key in keys {
                    let Some(value) = feedback.get(&key) else {
                        continue;
                    };
                    let nested = value.get("value").is_some();
                    let reference = if nested {
                        value["value"].clone()
                    } else {
                        value.clone()
                    };
                    if (reference.is_object() && reference[theme].is_string())
                        || reference
                            .as_str()
                            .is_some_and(|path| !path.contains(':') && path.ends_with(".svg"))
                    {
                        visual["feedbackReferences"][&key] =
                            json!({"reference":reference,"nested":nested});
                    } else {
                        visual["feedbackReferences"][&key] = Value::Null;
                    }
                    if let Some(source) = reference.as_str().or_else(|| reference[theme].as_str()) {
                        let source = openaction_image_source(&root, source, openaction);
                        let source = themed_source(&root, &source, theme, &palette);
                        let fingerprint: [u8; 32] = Sha256::digest(source.as_bytes()).into();
                        let resolved = if let Some((_, old_nested)) = sources
                            .feedback
                            .get(&key)
                            .filter(|(old, _)| !disable_image_cache && *old == fingerprint)
                        {
                            if *old_nested {
                                visual["feedback"][&key]["value"].take()
                            } else {
                                visual["feedback"][&key].take()
                            }
                        } else {
                            image_with_theme(&root, &source, &colors)?
                        };
                        sources.feedback.insert(key.clone(), (fingerprint, nested));
                        visual["feedback"][&key] = value.clone();
                        if nested {
                            visual["feedback"][&key]["value"] = resolved;
                        } else {
                            visual["feedback"][&key] = resolved;
                        }
                    } else {
                        visual["feedback"][&key] = value.clone();
                        sources.feedback.remove(&key);
                    }
                }
            }
            for (key, value) in feedback {
                if !sources.feedback.contains_key(key) {
                    visual["feedback"][key] = value.clone();
                }
            }
        }
        "showAlert" | "showOk" => {
            visual["badge"] = json!(if event == "showAlert" { "!" } else { "OK" });
            hub.visual_state.badges.insert(
                message["context"].as_str().unwrap().into(),
                std::time::Instant::now() + Duration::from_millis(1500),
            );
        }
        _ => {}
    }
    if event == "setState"
        && !software
        && hub
            .context_state
            .vendor_profile_appearances
            .contains_key(&context)
    {
        vendor_profile_appearance::publish(hub, owner, &context);
    }
    Ok(())
}

// Recognize a Stream Deck multi-action context without Manager capabilities
fn elgato_multi_context(hub: &Hub, owner: &str, context: &str) -> bool {
    !hub.capabilities.contains_key(owner)
        && hub
            .manifest_dialects
            .get(owner)
            .is_some_and(|dialect| dialect == "streamdeck")
        && (hub
            .context_state
            .scopes
            .get(context)
            .is_some_and(|scope| scope.contains('@'))
            || hub
                .context_state
                .visible
                .get(context)
                .is_some_and(|event| event["payload"]["isInMultiAction"] == true))
}

/// Apply only source-defined encoder defaults. Runtime setters subsequently override
/// layout/trigger text; background remains behind the touchscreen layout.
pub(super) fn encoder_defaults(
    hub: &mut Hub,
    owner: &str,
    context: &str,
    action: &Value,
) -> Result<Vec<Value>, String> {
    let encoder = &action["Encoder"];
    let mut messages = Vec::new();
    if let Some(source) = encoder["background"].as_str() {
        let root = hub.roots.get(owner).ok_or("package root missing")?;
        let source = bytes(root, source)?;
        if source.len() > 2 * 1024 * 1024
            || !(hardware_core::render::is_svg_media(&source)
                || source.starts_with(b"\x89PNG\r\n\x1a\n"))
        {
            return Err("Encoder background must be PNG or SVG within 2 MiB".into());
        }
        let pixels = hardware_core::render::render(&source, 400, 200, 0, "png")
            .map_err(|error| error.to_string())?;
        if pixels.len() > 512 * 1024 {
            return Err("Encoder background output exceeds 512 KiB".into());
        }
        let background = json!(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(pixels)
        ));
        for values in [
            &mut hub.visual_state.visuals,
            &mut hub.visual_state.software_visuals,
        ] {
            let visual = values.entry(context.into()).or_insert_with(|| {
                Arc::new(json!({"title":"","image":null,"layout":null,"feedback":{}}))
            });
            Arc::make_mut(visual)["encoderBackground"] = background.clone();
        }
    }
    if let Some(layout) = encoder["layout"].as_str() {
        messages.push(
            json!({"event":"setFeedbackLayout","context":context,"payload":{"layout":layout}}),
        );
    }
    if let Some(descriptions) = encoder["TriggerDescription"].as_object() {
        let mut payload = json!({});
        for (field, key) in [
            ("Push", "push"),
            ("Rotate", "rotate"),
            ("Touch", "touch"),
            ("LongTouch", "longTouch"),
        ] {
            if let Some(value) = descriptions.get(field) {
                payload[key] = value.clone();
            }
        }
        messages.push(json!({"event":"setTriggerDescription","context":context,"payload":payload}));
    }
    Ok(messages)
}

// Refresh themes
pub(super) fn refresh_themes(hub: &mut Hub) {
    refresh_target_themes(hub);
    swap_target(hub);
    refresh_target_themes(hub);
    swap_target(hub);
}
// Refresh target themes
fn refresh_target_themes(hub: &mut Hub) {
    hub.visual_state.visual_generation = hub.visual_state.visual_generation.wrapping_add(1);
    hub.visual_state.visual_sources.clear();
    let contexts: Vec<_> = hub.visual_state.visuals.keys().cloned().collect();
    for context in contexts {
        let Some(owner) = hub.context_state.contexts.get(&context).cloned() else {
            continue;
        };
        if !hub.capabilities.contains_key(&owner) {
            continue;
        }
        let device = hub
            .context_state
            .targets
            .get(&context)
            .map(|(device, _)| device.as_str())
            .unwrap_or("");
        let theme = hub.resolved_device_theme_for(&owner, device);
        let palette = hub.device_palette_id_for(theme, device);
        let colors = hub.device_colors_for(theme, device);
        let Some(root) = hub.roots.get(&owner) else {
            continue;
        };
        let value = &hub.visual_state.visuals[&context];
        let mut updates = vec![];
        let resolve = |reference: &Value| {
            let source = reference[theme].as_str().or_else(|| reference.as_str())?;
            if reference.is_string() && (source.contains(':') || !source.ends_with(".svg")) {
                return None;
            }
            image_with_theme(root, &themed_source(root, source, theme, palette), &colors).ok()
        };
        let layout = value["layoutReference"]
            .as_str()
            .and_then(|name| themed_layout(root, name, theme, palette, &colors).ok())
            .filter(|(layout, managed)| {
                json!(*managed) != value["paletteTile"] || *layout != value["layout"]
            });
        if let Some(refs) = value["feedbackReferences"].as_object() {
            for (key, entry) in refs {
                let nested = entry["nested"].as_bool().unwrap_or(false);
                let current = if nested {
                    &value["feedback"][key]["value"]
                } else {
                    &value["feedback"][key]
                };
                if let Some(image) = resolve(&entry["reference"]).filter(|image| image != current) {
                    updates.push((key.clone(), image, nested));
                }
            }
        }
        let image = resolve(&value["imageReference"]).filter(|image| *image != value["image"]);
        let changed = layout.is_some() || image.is_some() || !updates.is_empty();
        if !changed {
            continue;
        }
        if hub
            .visual_state
            .visuals
            .get(&context)
            .is_some_and(|value| Arc::strong_count(value) > 1)
        {
            hub.visual_state.visual_cow_copies =
                hub.visual_state.visual_cow_copies.saturating_add(1);
        }
        let visual = Arc::make_mut(hub.visual_state.visuals.get_mut(&context).unwrap());
        if let Some((layout, palette_tile)) = layout {
            visual["layout"] = layout;
            visual["paletteTile"] = json!(palette_tile);
        }
        if let Some(image) = image {
            visual["image"] = image;
        }
        for (key, image, nested) in updates {
            if nested {
                visual["feedback"][key]["value"] = image;
            } else {
                visual["feedback"][key] = image;
            }
        }
        if changed && hub.context_state.targets.contains_key(&context) && hub.effects.len() < 512 {
            let effect = hub.feedback_effect(FeedbackKind::Image, &context, None);
            hub.effects.push_back(effect);
        }
    }
}
