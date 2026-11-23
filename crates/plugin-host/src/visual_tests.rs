use super::*;

#[test]
// Verify palette change repaints cached layout without replacing live feedback
fn palette_change_repaints_cached_layout_without_replacing_live_feedback() {
    let root = tempfile::tempdir().unwrap();
    let layout = |color: &str| {
        json!({"id":"test","background":color,"items":[
            {"key":"label","type":"text","rect":[0,0,200,100]}
        ]})
        .to_string()
    };
    std::fs::write(root.path().join("tile.json"), layout("#000000")).unwrap();
    for (palette, color) in [("default", "#202020"), ("midnight", "#151d31")] {
        let folder = root.path().join(format!("palettes/dark/{palette}"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("tile.json"), layout(color)).unwrap();
    }
    let mut hub = Hub {
        ui_appearance: "dark".into(),
        ..Default::default()
    };
    hub.roots.insert("plugin".into(), root.path().into());
    hub.context_state
        .contexts
        .insert("key".into(), "plugin".into());
    hub.capabilities.insert("plugin".into(), Default::default());
    for mut message in [
        json!({"event":"setFeedbackLayout","context":"key","payload":{"layout":"tile.json"}}),
        json!({"event":"setFeedback","context":"key","payload":{"label":"42%"}}),
    ] {
        prepare(&mut hub, "plugin", &mut message).unwrap();
    }
    assert_eq!(
        hub.visual_state.visuals["key"]["layout"]["background"],
        "#202020"
    );
    hub.ui_theme_style.dark_palette = "midnight".into();
    refresh_themes(&mut hub);
    assert_eq!(
        hub.visual_state.visuals["key"]["layout"]["background"],
        "#151d31"
    );
    assert_eq!(hub.visual_state.visuals["key"]["feedback"]["label"], "42%");
    hub.appearance.insert("plugin".into(), "light".into());
    refresh_themes(&mut hub);
    assert_eq!(
        hub.visual_state.visuals["key"]["layout"]["background"],
        "#000000"
    );
    assert_eq!(hub.visual_state.visuals["key"]["paletteTile"], false);

    hub.appearance.remove("plugin");
    hub.system_dark = true;
    hub.context_state
        .targets
        .insert("key".into(), ("dock-a".into(), "key-1".into()));
    hub.context_state
        .targets
        .insert("other".into(), ("dock-b".into(), "key-1".into()));
    hub.context_state
        .contexts
        .insert("other".into(), "plugin".into());
    hub.profile_palettes.insert(
        "dock-a".into(),
        crate::ProfilePalette {
            target: crate::ProfilePaletteTarget::Device,
            ..Default::default()
        },
    );
    let mut other =
        json!({"event":"setFeedbackLayout","context":"other","payload":{"layout":"tile.json"}});
    prepare(&mut hub, "plugin", &mut other).unwrap();
    refresh_themes(&mut hub);
    assert_eq!(
        hub.visual_state.visuals["key"]["layout"]["background"],
        "#202020"
    );
    assert_eq!(
        hub.visual_state.visuals["other"]["layout"]["background"],
        "#151d31"
    );
    assert_eq!(hub.visual_state.visuals["key"]["feedback"]["label"], "42%");
    hub.system_dark = false;
    refresh_themes(&mut hub);
    assert_eq!(
        hub.visual_state.visuals["key"]["layout"]["background"],
        "#000000"
    );
    assert_eq!(
        hub.visual_state.visuals["other"]["layout"]["background"],
        "#151d31"
    );
}

#[test]
// Verify text feedback preserves cached icons and theme references
fn text_feedback_preserves_cached_icons_and_theme_references() {
    let root = tempfile::tempdir().unwrap();
    for (name, color) in [("light.svg", "black"), ("dark.svg", "white")] {
        std::fs::write(
            root.path().join(name),
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="8" fill="{color}"/></svg>"#
            ),
        )
        .unwrap();
    }
    for nested in [false, true] {
        let mut hub = Hub::default();
        hub.roots.insert("plugin".into(), root.path().into());
        hub.context_state
            .contexts
            .insert("key".into(), "plugin".into());
        hub.capabilities.insert("plugin".into(), Default::default());
        hub.appearance.insert("plugin".into(), "dark".into());
        hub.visual_state.visuals.insert("key".into(), Arc::new(json!({"feedback":{},"layout":{"items":[{"key":"icon","type":"pixmap"},{"key":"label","type":"text"}]}})));
        let reference = json!({"light":"light.svg","dark":"dark.svg"});
        let icon = if nested {
            json!({"value":reference,"rect":[0,0,100,100]})
        } else {
            reference.clone()
        };
        let mut initial =
            json!({"event":"setFeedback","context":"key","payload":{"icon":icon,"label":"Ready"}});
        prepare(&mut hub, "plugin", &mut initial).unwrap();
        let cached_icon = hub.visual_state.visuals["key"]["feedback"]["icon"].clone();
        let cached_reference =
            hub.visual_state.visuals["key"]["feedbackReferences"]["icon"].clone();

        let mut text = json!({"event":"setFeedback","context":"key","payload":{"label":"Updated"}});
        prepare(&mut hub, "plugin", &mut text).unwrap();
        assert_eq!(
            hub.visual_state.visuals["key"]["feedback"]["icon"],
            cached_icon
        );
        assert_eq!(
            hub.visual_state.visuals["key"]["feedbackReferences"]["icon"],
            cached_reference
        );
        assert_eq!(
            hub.visual_state.visuals["key"]["feedback"]["label"],
            "Updated"
        );

        hub.appearance.insert("plugin".into(), "light".into());
        refresh_themes(&mut hub);
        let themed_icon = &hub.visual_state.visuals["key"]["feedback"]["icon"];
        let expected = image_with_theme(root.path(), "light.svg", &Value::Null).unwrap();
        assert_eq!(
            if nested {
                &themed_icon["value"]
            } else {
                themed_icon
            },
            &expected
        );
        assert_ne!(*themed_icon, cached_icon);
    }
}

#[test]
// Verify legacy backplates are transparent in metadata states and feedback icons
fn legacy_backplates_are_transparent_in_metadata_states_and_feedback_icons() {
    let root = tempfile::tempdir().unwrap();
    for fill in ["#121212", "#ffffff"] {
        let source = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" fill="{fill}"/><circle cx="16" cy="16" r="8" fill="red"/></svg>"#
        );
        std::fs::write(root.path().join("icon.svg"), &source).unwrap();
        let asset = ThemedAsset {
            light: "icon.svg".into(),
            dark: "icon.svg".into(),
        };
        let mut images = vec![
            theme_icon(root.path(), &asset, "light").unwrap(),
            theme_icon(root.path(), &asset, "dark").unwrap(),
        ];
        for reference in [
            "icon.svg".into(),
            format!(
                "data:image/svg+xml;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&source)
            ),
            format!(
                "data:image/svg+xml,{}",
                percent_encoding::utf8_percent_encode(&source, percent_encoding::NON_ALPHANUMERIC)
            ),
        ] {
            images.push(
                image_with_theme(root.path(), &reference, &Value::Null)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        for uri in images {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(uri.split_once(',').unwrap().1)
                .unwrap();
            let png = hardware_core::render::render(&bytes, 64, 64, 0, "png").unwrap();
            let icon = image::load_from_memory(&png).unwrap().to_rgba8();
            assert_eq!(icon.get_pixel(0, 0)[3], 0);
            assert_eq!(icon.get_pixel(32, 32).0, [255, 0, 0, 255]);
            let layout = json!({"background":"#2468ac","items":[{"key":"icon","type":"pixmap","rect":[50,0,100,50]}]});
            let composed =
                hardware_core::feedback::draw(&layout, &json!({"icon":uri}), 64, 64, 0).unwrap();
            let composed = image::load_from_memory(&composed).unwrap().to_rgba8();
            assert_eq!(
                composed.get_pixel(16, 0).0,
                [36, 104, 172, 255],
                "icon corner must show the surrounding background"
            );
            assert_eq!(composed.get_pixel(32, 16).0, [255, 0, 0, 255]);
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join("icon.svg")).unwrap(),
            source
        );
    }
}

#[test]
// Verify repeated references reuse validated visuals and layout changes release sources
fn repeated_references_reuse_validated_visuals_and_layout_changes_release_sources() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("icon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="8"/></svg>"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("layout.json"),
        r#"{"items":[{"key":"icon","type":"pixmap"}]}"#,
    )
    .unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.context_state
        .contexts
        .insert("key".into(), "plugin".into());
    let mut layout =
        json!({"event":"setFeedbackLayout","context":"key","payload":{"layout":"layout.json"}});
    prepare(&mut hub, "plugin", &mut layout).unwrap();
    let mut image = json!({"event":"setImage","context":"key","payload":{"image":"icon.svg"}});
    prepare(&mut hub, "plugin", &mut image).unwrap();
    let mut feedback = json!({"event":"setFeedback","context":"key","payload":{"icon":{"value":"icon.svg","rect":[0,0,100,100]}}});
    prepare(&mut hub, "plugin", &mut feedback).unwrap();
    let cached = hub.visual_state.visuals["key"]["feedback"]["icon"]["value"].clone();
    std::fs::remove_file(root.path().join("icon.svg")).unwrap();
    std::fs::remove_file(root.path().join("layout.json")).unwrap();
    prepare(&mut hub, "plugin", &mut image).unwrap();
    prepare(&mut hub, "plugin", &mut layout).unwrap();
    feedback["payload"]["icon"]["rect"] = json!([10, 10, 80, 80]);
    prepare(&mut hub, "plugin", &mut feedback).unwrap();
    assert_eq!(
        hub.visual_state.visuals["key"]["feedback"]["icon"]["value"],
        cached
    );
    assert_eq!(
        hub.visual_state.visuals["key"]["feedback"]["icon"]["rect"],
        json!([10, 10, 80, 80])
    );
    // A new package/theme generation must validate the source again
    hub.visual_state.visual_sources.clear();
    assert!(prepare(&mut hub, "plugin", &mut image).is_err());
}

#[test]
// Verify vendor trigger descriptions are encoder scoped bounded and replace previous values
fn vendor_trigger_descriptions_are_encoder_scoped_bounded_and_replace_previous_values() {
    let root = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.context_state.visible.insert(
        "dial".into(),
        json!({"action":"plugin.action", "payload":{"controller":"Encoder"}}),
    );
    let mut message = json!({"event":"setTriggerDescription","context":"dial","payload":{"rotate":"Adjust volume","push":"Mute"}});
    prepare(&mut hub, "plugin", &mut message).unwrap();
    assert_eq!(
        hub.visual_state.visuals["dial"]["triggerDescriptions"]["push"],
        "Mute"
    );
    message["payload"] = json!({"touch":"Select"});
    prepare(&mut hub, "plugin", &mut message).unwrap();
    assert!(hub.visual_state.visuals["dial"]["triggerDescriptions"]["push"].is_null());
    let previous = hub.visual_state.visuals["dial"].clone();
    for payload in [
        json!({"rotate":42}),
        json!({"other":"unknown"}),
        json!({"push":"x".repeat(2049)}),
    ] {
        message["payload"] = payload;
        assert!(prepare(&mut hub, "plugin", &mut message).is_err());
        assert_eq!(hub.visual_state.visuals["dial"], previous);
    }
    message["payload"] = json!({});
    message["context"] = json!("key");
    assert!(prepare(&mut hub, "plugin", &mut message).is_err());
}

#[test]
// Verify vendor targets keep hardware and editor titles independent
fn vendor_targets_keep_hardware_and_editor_titles_independent() {
    let root = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    for (target, title) in [(0, "both"), (1, "hardware"), (2, "editor")] {
        prepare(&mut hub,"plugin",&mut json!({"event":"setTitle","context":"ctx","payload":{"target":target,"title":title}})).unwrap();
    }
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "hardware");
    assert_eq!(hub.visual_state.software_visuals["ctx"]["title"], "editor");
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"target":0,"title":"together"}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "together");
    assert_eq!(
        hub.visual_state.software_visuals["ctx"]["title"],
        "together"
    );
    assert!(prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"target":3,"title":"invalid"}})
    )
    .is_err());
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "together");
    hub.visual_state.remove("ctx");
    assert!(hub.visual_state.software_visuals.is_empty());
}

#[test]
// Verify vendor state specific titles survive switches and reset to declared values
fn vendor_state_specific_titles_survive_switches_and_reset_to_declared_values() {
    let root = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.metadata.insert(
        "plugin".into(),
        Arc::new(HashMap::from([(
            "plugin.action".into(),
            json!({"States":[{"Title":"zero"},{"Title":"one"}]}),
        )])),
    );
    hub.context_state
        .context_actions
        .insert("ctx".into(), "plugin.action".into());
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"ctx","payload":{"state":0}}),
    )
    .unwrap();
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"state":1,"title":"custom one"}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "zero");
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"ctx","payload":{"state":1}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "custom one");
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"state":1}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "one");
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"title":"all"}}),
    )
    .unwrap();
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"ctx","payload":{"state":0}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "all");
    assert!(prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"state":2,"title":"invalid"}})
    )
    .is_err());
}

#[test]
// Verify first targeted update does not leak into the other target
fn first_targeted_update_does_not_leak_into_the_other_target() {
    for target in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let mut hub = Hub::default();
        hub.roots.insert("plugin".into(), root.path().into());
        prepare(&mut hub,"plugin",&mut json!({"event":"setTitle","context":"ctx","payload":{"target":target,"title":"specific"}})).unwrap();
        assert_eq!(
            hub.visual_state.visuals["ctx"]["title"],
            if target == 1 { "specific" } else { "" }
        );
        assert_eq!(
            hub.visual_state.software_visuals["ctx"]["title"],
            if target == 2 { "specific" } else { "" }
        );
    }
}

#[test]
// Verify vendor state title defaults follow selected state and keep plugin title overrides
fn vendor_state_title_defaults_follow_selected_state_and_keep_plugin_title_overrides() {
    let root = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.metadata.insert("plugin".into(), Arc::new(HashMap::from([("plugin.action".into(), json!({"States":[
        {"Title":"zero","FontFamily":"Inter","FontSize":"10","FontStyle":"Bold Italic","FontUnderline":true,"ShowTitle":false,"TitleAlignment":"top","TitleColor":"#ff0000"},
        {"Title":"one","FontSize":18,"ShowTitle":true,"TitleAlignment":"bottom","TitleColor":"#00ff00"}
    ]}))])));
    hub.context_state
        .context_actions
        .insert("ctx".into(), "plugin.action".into());
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"ctx","payload":{"state":0}}),
    )
    .unwrap();
    let parameters = hub.visual_state.visuals["ctx"]["titleParameters"].clone();
    assert_eq!(
        parameters,
        json!({"fontFamily":"Inter","fontSize":10.0,"fontStyle":"Bold Italic","fontUnderline":true,"showTitle":false,"titleAlignment":"top","titleColor":"#ff0000"})
    );
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setTitle","context":"ctx","payload":{"title":"live"}}),
    )
    .unwrap();
    assert_eq!(
        hub.visual_state.visuals["ctx"]["titleParameters"],
        parameters
    );
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"ctx","payload":{"state":1}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["ctx"]["title"], "live");
    assert_eq!(
        hub.visual_state.visuals["ctx"]["titleParameters"],
        json!({"fontSize":18.0,"showTitle":true,"titleAlignment":"bottom","titleColor":"#00ff00"})
    );
    assert_eq!(
        hub.visual_state.software_visuals["ctx"]["titleParameters"],
        hub.visual_state.visuals["ctx"]["titleParameters"]
    );
}

#[test]
// Verify extensionless vendor GIF and SVG keep real mime and complete source bytes
fn extensionless_vendor_gif_and_svg_keep_real_mime_and_complete_source_bytes() {
    let root = tempfile::tempdir().unwrap();
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(2, 2, image::Rgba(color)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    std::fs::write(root.path().join("state@2x.gif"), &gif).unwrap();
    let uri = image_with_theme(root.path(), "state", &Value::Null).unwrap();
    let encoded = uri
        .as_str()
        .unwrap()
        .strip_prefix("data:image/gif;base64,")
        .unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(decoded, gif);
    use image::AnimationDecoder;
    let frames = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(decoded))
        .unwrap()
        .into_frames()
        .collect_frames()
        .unwrap();
    assert_eq!(frames.len(), 2);
    // Native render currently decodes a still frame. This test does not claim timed playback
    assert!(hardware_core::render::visual(&json!({"image":uri}), 32, 32, 0).is_ok());
    std::fs::write(root.path().join("vector.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"2\"><circle cx=\"1\" cy=\"1\" r=\"1\"/></svg>").unwrap();
    assert!(image_with_theme(root.path(), "vector", &Value::Null)
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("data:image/svg+xml;base64,"));
    assert!(image_with_theme(root.path(), "../state", &Value::Null).is_err());
}

#[test]
// Verify set image rejects GIF without mutating targets but manifest state keeps animation
fn set_image_rejects_gif_without_mutating_targets_but_manifest_state_keeps_animation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("animated.gif"), b"GIF89a").unwrap();
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.context_state
        .contexts
        .insert("context".into(), "plugin".into());
    assert!(prepare(&mut hub,"plugin",&mut json!({"event":"setImage","context":"context","payload":{"image":"animated.gif","target":1}})).unwrap_err().contains("manifest States"));
    assert!(hub.visual_state.visuals.is_empty());
    assert!(hub.visual_state.software_visuals.is_empty());
    assert!(image_with_theme(root.path(), "animated.gif", &Value::Null)
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("data:image/gif;base64,"));
}

#[test]
// Verify uncached action revalidates image state and feedback sources
fn uncached_action_revalidates_image_state_and_feedback_sources() {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("icon.svg");
    let write = |color: &str| {
        std::fs::write(&image, format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="8" height="8" fill="{color}"/></svg>"#)).unwrap()
    };
    write("red");
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.context_state
        .context_actions
        .insert("key".into(), "plugin.action".into());
    hub.metadata.insert(
        "plugin".into(),
        Arc::new(HashMap::from([(
            "plugin.action".into(),
            json!({"DisableCaching":true,"States":[{"Image":"icon"}]}),
        )])),
    );
    let mut command = json!({"event":"setImage","context":"key","payload":{"image":"icon.svg"}});
    prepare(&mut hub, "plugin", &mut command).unwrap();
    let first = hub.visual_state.visuals["key"]["image"].clone();
    write("blue");
    prepare(&mut hub, "plugin", &mut command).unwrap();
    assert_ne!(hub.visual_state.visuals["key"]["image"], first);
    let second = hub.visual_state.visuals["key"]["image"].clone();
    write("green");
    let mut state = json!({"event":"setState","context":"key","payload":{"state":0}});
    prepare(&mut hub, "plugin", &mut state).unwrap();
    assert_ne!(hub.visual_state.visuals["key"]["image"], second);
    let mut layout =
        json!({"event":"setFeedbackLayout","context":"key","payload":{"layout":"layout.json"}});
    std::fs::write(
        root.path().join("layout.json"),
        r#"{"items":[{"key":"icon","type":"pixmap"}]}"#,
    )
    .unwrap();
    prepare(&mut hub, "plugin", &mut layout).unwrap();
    let mut feedback = json!({"event":"setFeedback","context":"key","payload":{"icon":"icon.svg"}});
    prepare(&mut hub, "plugin", &mut feedback).unwrap();
    let first = hub.visual_state.visuals["key"]["feedback"]["icon"].clone();
    write("yellow");
    prepare(&mut hub, "plugin", &mut feedback).unwrap();
    assert_ne!(hub.visual_state.visuals["key"]["feedback"]["icon"], first);
    command["payload"]["image"] = json!("../outside.svg");
    assert!(prepare(&mut hub, "plugin", &mut command).is_err());
    command["payload"]["image"] = json!("icon.svg");
    // Same-path changes must revalidate actual bytes, not retain a prior non-GIF proof
    std::fs::write(&image, b"GIF89a").unwrap();
    assert!(prepare(&mut hub, "plugin", &mut command)
        .unwrap_err()
        .contains("GIF"));
    // Manifest refresh preserves GIF MIME/source for the bounded runtime decoder
    prepare(&mut hub, "plugin", &mut state).unwrap();
    assert!(hub.visual_state.visuals["key"]["image"]
        .as_str()
        .unwrap()
        .starts_with("data:image/gif;base64,"));
    std::fs::remove_file(&image).unwrap();
    assert!(prepare(&mut hub, "plugin", &mut state).is_err());
}

#[test]
// Verify elgato multi action image is software only and preserves target overrides
fn elgato_multi_action_image_is_software_only_and_preserves_target_overrides() {
    let root = tempfile::tempdir().unwrap();
    for (name, color) in [
        ("key", "#ff0000"),
        ("multi", "#0000ff"),
        ("override", "#00ff00"),
    ] {
        std::fs::write(root.path().join(format!("{name}.svg")),format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="{color}"/></svg>"#)).unwrap();
    }
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.path().into());
    hub.manifest_dialects
        .insert("plugin".into(), "streamdeck".into());
    hub.metadata.insert(
        "plugin".into(),
        Arc::new(HashMap::from([(
            "action".into(),
            json!({"States":[{"Image":"key","MultiActionImage":"multi"}]}),
        )])),
    );
    hub.context_state
        .context_actions
        .insert("context".into(), "action".into());
    hub.context_state
        .scopes
        .insert("context".into(), "profile:page@0".into());
    let image = |name| image_with_theme(root.path(), name, &Value::Null).unwrap();
    hub.context_state
        .context_actions
        .insert("initial".into(), "action".into());
    hub.context_state
        .scopes
        .insert("initial".into(), "profile:page@1".into());
    prepare(&mut hub,"plugin",&mut json!({"event":"setTitle","context":"initial","payload":{"title":"Only hardware","target":1}})).unwrap();
    assert_eq!(hub.visual_state.visuals["initial"]["image"], image("key"));
    assert_eq!(
        hub.visual_state.software_visuals["initial"]["image"],
        image("multi")
    );

    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"context","payload":{"state":0}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals["context"]["image"], image("key"));
    assert_eq!(
        hub.visual_state.software_visuals["context"]["image"],
        image("multi")
    );
    prepare(&mut hub,"plugin",&mut json!({"event":"setImage","context":"context","payload":{"image":"override","target":2}})).unwrap();
    prepare(&mut hub,"plugin",&mut json!({"event":"setTitle","context":"context","payload":{"title":"Hardware","target":1}})).unwrap();
    assert_eq!(hub.visual_state.visuals["context"]["image"], image("key"));
    assert_eq!(
        hub.visual_state.software_visuals["context"]["image"],
        image("override")
    );
    assert_eq!(hub.visual_state.visuals["context"]["title"], "Hardware");
    assert_ne!(
        hub.visual_state.software_visuals["context"]["title"],
        "Hardware"
    );
    // Omitting image resets only that target to its distinct manifest default
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setImage","context":"context","payload":{"target":2}}),
    )
    .unwrap();
    assert_eq!(
        hub.visual_state.software_visuals["context"]["image"],
        image("multi")
    );
    hub.manifest_dialects.insert("plugin".into(), "vsd".into());
    prepare(
        &mut hub,
        "plugin",
        &mut json!({"event":"setState","context":"context","payload":{"state":0}}),
    )
    .unwrap();
    assert_eq!(
        hub.visual_state.software_visuals["context"]["image"],
        image("key")
    );
}

// Construct the default layout hub test fixture
fn default_layout_hub(root: &Path, layout: &str, disable_cache: bool) -> Hub {
    let mut hub = Hub::default();
    hub.roots.insert("plugin".into(), root.into());
    hub.metadata.insert(
        "plugin".into(),
        Arc::new(HashMap::from([(
            "action".into(),
            json!({"Encoder":{"layout":layout},"DisableCaching":disable_cache}),
        )])),
    );
    hub.context_state.visible.insert(
        "dial".into(),
        json!({"action":"action","payload":{"controller":"Encoder"}}),
    );
    let defaults = encoder_defaults(
        &mut hub,
        "plugin",
        "dial",
        &json!({"Encoder":{"layout":layout}}),
    )
    .unwrap();
    for mut message in defaults {
        prepare(&mut hub, "plugin", &mut message).unwrap();
    }
    hub
}
// Construct the default layout pixels test fixture
fn default_layout_pixels(hub: &Hub) -> image::RgbaImage {
    image::load_from_memory(
        &hardware_core::render::visual(&hub.visual_state.visuals["dial"], 200, 100, 0).unwrap(),
    )
    .unwrap()
    .into_rgba8()
}

#[test]
// Verify declared builtin layouts render documented image regions with live feedback
fn declared_builtin_layouts_render_documented_image_regions_with_live_feedback() {
    let root = tempfile::tempdir().unwrap();
    for (name, color) in [("red", [255, 0, 0, 255]), ("green", [0, 255, 0, 255])] {
        std::fs::write(
            root.path().join(format!("{name}.png")),
            hardware_core::render::solid_rgba(20, 10, color).unwrap(),
        )
        .unwrap();
    }
    for layout in ["$X1", "$A0", "$A1"] {
        let mut hub = default_layout_hub(root.path(), layout, false);
        let payload = if layout == "$A0" {
            json!({"full-canvas":"red.png","canvas":"green.png","title":""})
        } else {
            json!({"icon":"green.png","title":"","value":"8"})
        };
        prepare(
            &mut hub,
            "plugin",
            &mut json!({"event":"setFeedback","context":"dial","payload":payload}),
        )
        .unwrap();
        let pixels = default_layout_pixels(&hub);
        match layout {
            "$X1" => {
                assert_eq!(pixels.get_pixel(100, 64).0, [0, 255, 0, 255]);
                assert_eq!(pixels.get_pixel(20, 64).0, [15, 20, 29, 255]);
            }
            "$A0" => {
                assert_eq!(pixels.get_pixel(100, 64).0, [0, 255, 0, 255]);
                assert_eq!(pixels.get_pixel(180, 64).0, [255, 0, 0, 255]);
                assert_eq!(pixels.get_pixel(2, 2).0, [255, 0, 0, 255]);
            }
            "$A1" => {
                assert_eq!(pixels.get_pixel(40, 64).0, [0, 255, 0, 255]);
                assert_eq!(
                    pixels.get_pixel(110, 80).0,
                    [15, 20, 29, 255],
                    "A1 has no indicator"
                );
                assert!((76..184).any(|x| (40..72).any(|y| pixels.get_pixel(x,y).0 == [237,242,252,255])),"right-side text value rendered");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
// Verify declared custom pixmaps render without feedback and honor cache policy
fn declared_custom_pixmaps_render_without_feedback_and_honor_cache_policy() {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("default.png");
    let layout = root.path().join("custom.json");
    std::fs::write(&layout,json!({"id":"custom","items":[{"key":"icon","type":"pixmap","rect":[0,0,200,100],"value":"default.png"}]}).to_string()).unwrap();
    for disable in [false, true] {
        std::fs::write(
            &image,
            hardware_core::render::solid_rgba(20, 10, [255, 0, 0, 255]).unwrap(),
        )
        .unwrap();
        let mut hub = default_layout_hub(root.path(), "custom.json", disable);
        assert_eq!(
            default_layout_pixels(&hub).get_pixel(100, 50).0,
            [255, 0, 0, 255]
        );
        assert!(
            hub.visual_state.visuals["dial"]["feedback"]["icon"].is_null(),
            "layout default needs no setFeedback"
        );
        std::fs::write(
            &image,
            hardware_core::render::solid_rgba(20, 10, [0, 0, 255, 255]).unwrap(),
        )
        .unwrap();
        prepare(&mut hub,"plugin",&mut json!({"event":"setFeedbackLayout","context":"dial","payload":{"layout":"custom.json"}})).unwrap();
        assert_eq!(
            default_layout_pixels(&hub).get_pixel(100, 50).0,
            if disable {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            }
        );
        std::fs::write(
            &image,
            hardware_core::render::solid_rgba(20, 10, [0, 255, 0, 255]).unwrap(),
        )
        .unwrap();
        prepare(&mut hub,"plugin",&mut json!({"event":"setFeedback","context":"dial","payload":{"title":"no icon override"}})).unwrap();
        assert_eq!(
            default_layout_pixels(&hub).get_pixel(100, 50).0,
            if disable {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            }
        );
    }
}

#[test]
// Verify declared pixmap paths and layout budgets are bounded and rejections keep previous pixels
fn declared_pixmap_paths_and_layout_budgets_are_bounded_and_rejections_keep_previous_pixels() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let solid = hardware_core::render::solid_rgba(20, 10, [255, 0, 0, 255]).unwrap();
    std::fs::write(root.path().join("safe.png"), &solid).unwrap();
    std::fs::write(outside.path().join("private.png"), &solid).unwrap();
    let layout = |sources: Vec<String>| json!({"id":"custom","items":sources.into_iter().enumerate().map(|(index,source)| json!({"key":format!("image-{index}"),"type":"pixmap","rect":[index*3,0,3,100],"value":source})).collect::<Vec<_>>()});
    std::fs::write(
        root.path().join("safe.json"),
        layout(vec!["safe.png".into()]).to_string(),
    )
    .unwrap();
    let mut hub = default_layout_hub(root.path(), "safe.json", false);
    let before =
        hardware_core::render::visual(&hub.visual_state.visuals["dial"], 200, 100, 0).unwrap();
    let assert_rejected = |hub: &mut Hub, value: Value| {
        std::fs::write(root.path().join("bad.json"), value.to_string()).unwrap();
        assert!(prepare(hub,"plugin",&mut json!({"event":"setFeedbackLayout","context":"dial","payload":{"layout":"bad.json"}})).is_err());
        assert_eq!(
            hardware_core::render::visual(&hub.visual_state.visuals["dial"], 200, 100, 0).unwrap(),
            before
        );
    };
    assert_rejected(&mut hub, layout(vec!["../private.png".into()]));
    assert_rejected(
        &mut hub,
        layout(vec![outside
            .path()
            .join("private.png")
            .to_string_lossy()
            .into_owned()]),
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("private.png"),
            root.path().join("link.png"),
        )
        .unwrap();
        assert_rejected(&mut hub, layout(vec!["link.png".into()]));
    }
    std::fs::write(root.path().join("large.png"), vec![0; 2 * 1024 * 1024 + 1]).unwrap();
    assert_rejected(&mut hub, layout(vec!["large.png".into()]));
    let svg = |padding: usize| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\"><!--{}--><rect width=\"20\" height=\"10\" fill=\"red\"/></svg>",
            "x".repeat(padding)
        )
    };
    for i in 0..3 {
        std::fs::write(
            root.path().join(format!("source-{i}.svg")),
            svg(1400 * 1024),
        )
        .unwrap();
    }
    assert_rejected(
        &mut hub,
        layout((0..3).map(|i| format!("source-{i}.svg")).collect()),
    );
    std::fs::write(root.path().join("repeated.svg"), svg(128 * 1024)).unwrap();
    assert_rejected(&mut hub, layout(vec!["repeated.svg".into(); 32]));
    for i in 0..33 {
        std::fs::write(root.path().join(format!("small-{i}.png")), &solid).unwrap();
    }
    assert_rejected(
        &mut hub,
        layout((0..33).map(|i| format!("small-{i}.png")).collect()),
    );
}

#[test]
fn large_artwork_is_shared_by_states_feedback_and_both_targets_until_the_last_render() {
    use image::ImageEncoder;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let mut hub = Hub::default();
    let cache = Arc::new(ArtworkCache::new(directory.join("cached-media")));
    hub.visual_state.artwork_cache = Some(cache.clone());
    hub.roots.insert("plugin".into(), directory.clone());
    hub.metadata.insert(
        "plugin".into(),
        Arc::new(HashMap::from([("art".into(), json!({"States":[{},{}]}))])),
    );
    hub.context_state
        .context_actions
        .insert("key".into(), "art".into());
    std::fs::write(
        directory.join("tile.json"),
        r#"{"items":[{"type":"pixmap","key":"art","rect":[0,0,200,100]}]}"#,
    )
    .unwrap();
    let mut png = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut png);
    encoder.set_exif_metadata(vec![3; 96 * 1024]).unwrap();
    encoder
        .write_image(&[10, 22, 31], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    let uri = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    for mut event in [
        json!({"event":"setImage","context":"key","payload":{"image":uri}}),
        json!({"event":"setFeedbackLayout","context":"key","payload":{"layout":"tile.json"}}),
        json!({"event":"setFeedback","context":"key","payload":{"art":{"value":uri}}}),
        json!({"event":"setState","context":"key","payload":{"state":1}}),
        json!({"event":"setTitle","context":"key","payload":{"title":"new title","target":2}}),
    ] {
        prepare(&mut hub, "plugin", &mut event).unwrap();
    }
    refresh_themes(&mut hub);
    let snapshot = hub.visual_state.visuals["key"].clone();
    let files = hub.visual_state.artwork("key", false).unwrap().clone();
    assert!(snapshot.to_string().len() < 2000);
    assert!(hub.visual_state.software_visuals["key"].to_string().len() < 2000);
    assert_eq!(cache.maintain(None).unwrap().entries, 1);
    hub.visual_state.remove("key");
    assert_eq!(cache.maintain(Some(true)).unwrap().removed_entries, 0);
    let mut expanded = snapshot.as_ref().clone();
    files.expand(&mut expanded).unwrap();
    assert_eq!(expanded["image"], uri);
    let resolve = |token: &str| {
        files
            .image_bytes(token)
            .map_err(hardware_core::Error::Protocol)
    };
    for visual in [
        snapshot.as_ref().clone(),
        json!({"image":snapshot["image"],"title":"a"}),
    ] {
        let resolved =
            hardware_core::render::visual_resolved(&visual, 112, 112, 90, &resolve).unwrap();
        let mut inline = visual;
        files.expand(&mut inline).unwrap();
        assert_eq!(
            resolved,
            hardware_core::render::visual(&inline, 112, 112, 90).unwrap()
        );
    }
    assert_eq!(expanded["feedback"]["art"]["value"], uri);
    assert_eq!(expanded["stateOverrides"]["1"]["image"], uri);
    for (w, h) in [(112, 112), (176, 112)] {
        assert_eq!(
            hardware_core::render::visual(&expanded, w, h, 0).unwrap(),
            hardware_core::render::visual(
                &json!({"layout":expanded["layout"],"feedback":{"art":{"value":uri}}}),
                w,
                h,
                0
            )
            .unwrap()
        );
    }
    drop(files);
    assert_eq!(cache.maintain(Some(true)).unwrap().removed_entries, 1);
}
