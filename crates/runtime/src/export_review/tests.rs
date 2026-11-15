use super::*;
use crate::{
    credentials::{CredentialProvider, CredentialState, Credentials},
    workspace::Edit,
};
use std::{
    io::Read,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
struct Vault(Arc<AtomicUsize>);
impl CredentialProvider for Vault {
    // Read the requested tests data
    fn read(&self, _: &str) -> Result<String, CredentialState> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("os-only-secret".into())
    }
    // Write the supplied tests data
    fn write(&self, _: &str, _: &str) -> Result<(), CredentialState> {
        Ok(())
    }
    // Remove the selected tests entry
    fn delete(&self, _: &str) -> Result<(), CredentialState> {
        Ok(())
    }
}
// Submit the tests request
fn request(kind: &str, ids: Vec<String>, include: bool) -> ExportReviewRequest {
    ExportReviewRequest {
        kind: kind.into(),
        ids,
        include_plugin_settings: include,
    }
}
// Build a named saved-macro fixture without input parameters
fn saved(id: &str, action: Action) -> crate::builtins::SavedMacro {
    crate::builtins::SavedMacro {
        id: id.into(),
        name: id.into(),
        inputs: vec![],
        action,
    }
}
// Construct a saved-macro call fixture with no arguments
fn call(id: &str) -> Action {
    Action::Builtin {
        action: Box::new(BuiltinAction::SavedMacro {
            macro_id: id.into(),
            arguments: Default::default(),
        }),
    }
}
// Apply the requested export review test change
fn change(
    report: &ExportReview,
    value: &Value,
    operation: &str,
    name: &str,
    replacement: Value,
) -> ExportFieldEdit {
    ExportFieldEdit {
        key: report
            .fields
            .iter()
            .find(|f| f.value == *value)
            .unwrap()
            .key
            .clone(),
        operation: operation.into(),
        name: name.into(),
        replacement,
    }
}
#[test]
#[ignore = "Requires a disposable loopback host; reviewed exports never execute actions or read credentials"]
// Verify that frozen candidate uses reviewed document and asset bytes after workspace changes
fn frozen_candidate_uses_reviewed_document_and_asset_bytes_after_workspace_changes() {
    let root = tempfile::tempdir().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::build_credentials(
        root.path().into(),
        false,
        Credentials::with_provider(Arc::new(Vault(reads.clone()))),
    )
    .unwrap();
    let initial = hardware_core::render::tile(8, 8, [10, 20, 30], "", 0, "png").unwrap();
    let source = root.path().join("source.png");
    std::fs::write(&source, &initial).unwrap();
    let asset = runtime.profiles.import_asset(&source).unwrap();
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.parameters.push(ProfileParameter {
        name: "credential".into(),
        label: "Credential".into(),
        r#type: "string".into(),
        required: false,
        default: None,
        sensitive: true,
    });
    profile.parameters.push(ProfileParameter {
        name: "ordinary".into(),
        label: "Ordinary default".into(),
        r#type: "string".into(),
        required: false,
        default: Some(json!("private-default")),
        sensitive: false,
    });
    let local_id = format!("parameter-{}", uuid::Uuid::new_v4());
    let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
    binding.asset = Some(asset.clone());
    binding.action = Action::Macro {
        actions: vec![
            Action::Command {
                program: "private-program".into(),
                args: vec!["private-argument".into()],
            },
            Action::OpenUrl {
                url: "https://private.example/token".into(),
            },
            Action::OpenPath {
                path: "/private/path".into(),
            },
            Action::Builtin {
                action: Box::new(BuiltinAction::TypeText {
                    text: "private-text".into(),
                    template: false,
                }),
            },
            Action::Plugin {
                user_desired_state: None,
                plugin_id: "private.plugin".into(),
                action_id: "private.action".into(),
                settings: json!({"known":"literal-plugin-secret","unknown":{"keep":false},"credential":{"$credential":local_id},"nested":{"$credential":local_id,"preserved":true},"portable":"${parameter.credential}"}),
                resources: Default::default(),
            },
        ],
    };
    runtime
        .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
        .unwrap();
    runtime
        .write_credential(crate::parameter_credentials::CredentialWrite {
            target: crate::parameter_credentials::CredentialTarget {
                profile_id: profile.id.clone(),
                name: "credential".into(),
                device_id: None,
            },
            expected_revision: runtime.snapshot().configuration_revision,
            value: "os-only-secret".into(),
        })
        .unwrap();
    let mut report = runtime
        .prepare_export_review(request("profiles", vec![profile.id.clone()], true))
        .unwrap();
    for literal in [
        "private-program",
        "private-argument",
        "https://private.example/token",
        "/private/path",
        "private-text",
        "private-default",
        "literal-plugin-secret",
    ] {
        assert!(
            report.fields.iter().any(|f| f.value == json!(literal)),
            "missing {literal}"
        );
    }
    assert!(!report.candidate_json.contains(&local_id));
    assert!(report.candidate_json.contains("\"preserved\": true"));
    assert!(!report.candidate_json.contains("os-only-secret"));
    assert!(report.candidate_json.contains("${parameter.credential}"));
    assert!(report.candidate_json.contains("\"keep\": false"));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let pins = runtime.profiles.asset_leases.lock();
    assert!(pins
        .live
        .values()
        .any(|(why, ids)| why == "Reviewed export candidate" && ids.contains(&asset)));
    drop(pins);
    let edit = change(
        &report,
        &json!("private-default"),
        "required",
        "",
        Value::Null,
    );
    report = runtime
        .update_export_review(&report.id, report.revision, edit)
        .unwrap();
    let doc: ProfileData = serde_json::from_str(&report.candidate_json).unwrap();
    assert!(
        doc.profiles[0]
            .parameters
            .iter()
            .find(|p| p.name == "ordinary")
            .unwrap()
            .sensitive
    );
    profile.name = "Changed after review".into();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::None;
    runtime
        .save_profile(profile, runtime.snapshot().configuration_revision)
        .unwrap();
    std::fs::write(
        runtime
            .profiles
            .root
            .join("assets")
            .join(format!("{asset}.png")),
        b"changed after review",
    )
    .unwrap();
    let output = root.path().join("reviewed.zip");
    std::fs::write(&output, b"existing output").unwrap();
    assert!(runtime
        .write_export_review(
            &report.id,
            report.revision - 1,
            &report.archive_sha256,
            &output
        )
        .is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
    runtime
        .write_export_review(&report.id, report.revision, &report.archive_sha256, &output)
        .unwrap();
    assert_eq!(archive_digest(&output).unwrap(), report.archive_sha256);
    let (mut zip, document) = profiles::open_archive(&output, "profiles.json").unwrap();
    assert_eq!(document, report.candidate_json);
    let mut bytes = vec![];
    zip.by_name(&format!("assets/{asset}.png"))
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, initial);
    assert!(runtime.exports.lock().unwrap().is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    runtime.shutdown();
}
#[test]
#[ignore = "Requires disposable loopback host; conversions only change the portable candidate"]
// Verify that required macro inputs forward through a diamond without changing live macros
fn required_macro_inputs_forward_through_a_diamond_without_changing_live_macros() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let leaf = saved(
        "leaf",
        Action::Command {
            program: "program".into(),
            args: vec!["private-value".into()],
        },
    );
    let b = saved("b", call("leaf"));
    let c = saved("c", call("leaf"));
    let top = saved(
        "top",
        Action::Macro {
            actions: vec![call("b"), call("c")],
        },
    );
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = call("top");
    runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Fixture",
            vec![
                Edit::SaveMacro { value: leaf },
                Edit::SaveMacro { value: b },
                Edit::SaveMacro { value: c },
                Edit::SaveMacro { value: top },
                Edit::SaveProfile {
                    profile: profile.clone(),
                },
            ],
        )
        .unwrap();
    let before = runtime.snapshot();
    let report = runtime
        .prepare_export_review(request("profiles", vec![profile.id.clone()], false))
        .unwrap();
    let edit = change(
        &report,
        &json!("private-value"),
        "input",
        "api_token",
        Value::Null,
    );
    let report = runtime
        .update_export_review(&report.id, report.revision, edit)
        .unwrap();
    assert!(!report.candidate_json.contains("private-value"));
    let data: ProfileData = serde_json::from_str(&report.candidate_json).unwrap();
    profiles::validate_data(&data).unwrap();
    for m in &data.saved_macros {
        assert_eq!(m.inputs.len(), 1, "{}", m.id);
        assert_eq!(m.inputs[0].name, "api_token");
        assert!(m.inputs[0].required);
        assert!(m.inputs[0].default.is_none());
    }
    let p = data.profiles[0]
        .parameters
        .iter()
        .find(|p| p.name == "api_token")
        .unwrap();
    assert!(p.sensitive && p.required && p.default.is_none());
    assert_eq!(
        runtime.snapshot().configuration_revision,
        before.configuration_revision
    );
    assert!(runtime
        .snapshot()
        .saved_macros
        .iter()
        .all(|m| m.inputs.is_empty()));
    let standalone = runtime
        .prepare_export_review(request("macros", vec!["top".into()], false))
        .unwrap();
    let edit = change(
        &standalone,
        &json!("private-value"),
        "input",
        "api_token",
        Value::Null,
    );
    let standalone = runtime
        .update_export_review(&standalone.id, standalone.revision, edit)
        .unwrap();
    let data: ProfileData = serde_json::from_str(&standalone.candidate_json).unwrap();
    assert!(data.profiles.is_empty());
    assert!(data.saved_macros.iter().all(|m| m.inputs.len() == 1));
    runtime.discard_export_review(&report.id);
    runtime.discard_export_review(&standalone.id);
    runtime.shutdown();
}
#[test]
#[ignore = "Requires disposable loopback host; invalid edits and altered candidates preserve existing output"]
// Verify that invalid edits stale versions credential markers and tampering are rejected
fn invalid_edits_stale_versions_credential_markers_and_tampering_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Macro {
        actions: vec![
            Action::OpenUrl {
                url: "https://private.example/".into(),
            },
            Action::Plugin {
                user_desired_state: None,
                plugin_id: "unknown.plugin".into(),
                action_id: "unknown.action".into(),
                settings: json!({"unknown":"keep me"}),
                resources: Default::default(),
            },
        ],
    };
    runtime
        .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
        .unwrap();
    let report = runtime
        .prepare_export_review(request("profiles", vec![profile.id.clone()], true))
        .unwrap();
    let invalid = change(
        &report,
        &json!("https://private.example/"),
        "replace",
        "",
        json!("file:///private"),
    );
    assert!(runtime
        .update_export_review(&report.id, report.revision, invalid)
        .is_err());
    let unknown = change(
        &report,
        &json!("keep me"),
        "parameter",
        "token",
        Value::Null,
    );
    assert!(runtime
        .update_export_review(&report.id, report.revision, unknown)
        .is_err());
    let local = change(
        &report,
        &json!("keep me"),
        "replace",
        "",
        json!({"$credential":"parameter-local"}),
    );
    assert!(runtime
        .update_export_review(&report.id, report.revision, local)
        .is_err());
    assert_eq!(
        runtime.exports.lock().unwrap()[&report.id]
            .report
            .archive_sha256,
        report.archive_sha256
    );
    let valid = change(
        &report,
        &json!("https://private.example/"),
        "parameter",
        "private_url",
        Value::Null,
    );
    let changed = runtime
        .update_export_review(&report.id, report.revision, valid.clone())
        .unwrap();
    assert!(!changed.candidate_json.contains("https://private.example/"));
    assert!(changed.candidate_json.contains("${parameter.private_url}"));
    assert!(runtime
        .update_export_review(&report.id, report.revision, valid)
        .is_err());
    let output = root.path().join("existing.zip");
    std::fs::write(&output, b"preserved").unwrap();
    let stages = runtime.exports.lock().unwrap();
    std::fs::write(
        stages[&report.id].directory.path().join("candidate.zip"),
        b"altered frozen bytes",
    )
    .unwrap();
    drop(stages);
    assert!(runtime
        .write_export_review(
            &report.id,
            changed.revision,
            &changed.archive_sha256,
            &output
        )
        .is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"preserved");
    runtime.discard_export_review(&report.id);
    assert!(runtime.exports.lock().unwrap().is_empty());
    runtime.shutdown();
}
#[test]
// Verify that default and literal argument conversion keep zero and false types
fn default_and_literal_argument_conversion_keep_zero_and_false_types() {
    let mut data = ProfileData::default();
    let mut child = saved("child", Action::None);
    child.inputs = vec![MacroInput {
        name: "flag".into(),
        label: "Flag".into(),
        r#type: "boolean".into(),
        required: false,
        default: Some(json!(false)),
    }];
    let mut parent = saved("parent", call("child"));
    if let Action::Builtin { action } = &mut parent.action {
        if let BuiltinAction::SavedMacro { arguments, .. } = &mut **action {
            arguments.insert(
                "flag".into(),
                MacroArgument::Literal {
                    value: json!(false),
                },
            );
        }
    }
    data.saved_macros = vec![child, parent];
    let contracts = Contracts::new();
    let report = fields(&mut data, &contracts).unwrap();
    let argument = report
        .iter()
        .find(|f| f.label.contains("argument"))
        .unwrap();
    assert!(argument.can_input);
    let key = argument.key.clone();
    edit(
        &mut data,
        &contracts,
        &ExportFieldEdit {
            key,
            operation: "input".into(),
            replacement: Value::Null,
            name: "flag_value".into(),
        },
    )
    .unwrap();
    assert_eq!(data.saved_macros[1].inputs[0].r#type, "boolean");
    if let Action::Builtin { action } = &data.saved_macros[1].action {
        assert!(
            matches!(&**action,BuiltinAction::SavedMacro{arguments,..} if matches!(&arguments["flag"],MacroArgument::Input{name} if name=="flag_value"))
        );
    }
    let defaults = fields(&mut data, &contracts).unwrap();
    let default = defaults.iter().find(|f| f.can_require).unwrap();
    assert_eq!(default.value, json!(false));
    let key = default.key.clone();
    edit(
        &mut data,
        &contracts,
        &ExportFieldEdit {
            key,
            operation: "required".into(),
            replacement: Value::Null,
            name: String::new(),
        },
    )
    .unwrap();
    assert!(data.saved_macros[0].inputs[0].default.is_none());
    assert!(data.saved_macros[0].inputs[0].required);
}

#[test]
// Verify that declared plugin fields preserve typed values and large reviews fail closed
fn declared_plugin_fields_preserve_typed_values_and_large_reviews_fail_closed() {
    let mut data = ProfileData::default();
    data.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = Action::Plugin {
        user_desired_state: None,
        plugin_id: "plugin".into(),
        action_id: "action".into(),
        settings: json!({"flag":false,"count":0,"opaque":{"nested":"private"}}),
        resources: Default::default(),
    };
    let contracts = BTreeMap::from([(
        ("plugin".into(), "action".into()),
        BTreeSet::from(["flag".into(), "count".into()]),
    )]);
    let review = fields(&mut data, &contracts).unwrap();
    let field = review.iter().find(|f| f.value == json!(false)).unwrap();
    assert!(field.can_parameter);
    let key = field.key.clone();
    edit(
        &mut data,
        &contracts,
        &ExportFieldEdit {
            key,
            operation: "parameter".into(),
            replacement: Value::Null,
            name: "flag_value".into(),
        },
    )
    .unwrap();
    let p = data.profiles[0]
        .parameters
        .iter()
        .find(|p| p.name == "flag_value")
        .unwrap();
    assert_eq!(p.r#type, "boolean");
    assert!(!p.sensitive);
    assert!(p.required && p.default.is_none());
    if let Action::Plugin { settings, .. } = &data.profiles[0].pages[0].bindings["key-1"].action {
        assert_eq!(settings["flag"], json!({"$parameter":"flag_value"}));
        assert_eq!(settings["count"], json!(0));
        assert_eq!(settings["opaque"]["nested"], "private");
    }
    data.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = Action::Macro {
        actions: (0..20)
            .map(|_| Action::Command {
                program: "program".into(),
                args: vec!["argument".into(); 128],
            })
            .collect(),
    };
    assert!(fields(&mut data, &contracts).unwrap_err().contains("2,048"));
}
#[test]
#[ignore = "Requires a disposable loopback host; review lifetime and ownership only"]
// Verify that bounded sessions expire and cancel without writing or leaking ownership
fn bounded_sessions_expire_and_cancel_without_writing_or_leaking_ownership() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let id = runtime.snapshot().profiles[0].id.clone();
    let mut reports = vec![];
    for _ in 0..8 {
        reports.push(
            runtime
                .prepare_export_review(request("profiles", vec![id.clone()], false))
                .unwrap(),
        );
    }
    assert!(runtime
        .prepare_export_review(request("profiles", vec![id.clone()], false))
        .is_err());
    runtime
        .exports
        .lock()
        .unwrap()
        .get_mut(&reports[0].id)
        .unwrap()
        .created = Instant::now() - LIFETIME;
    let output = root.path().join("existing.zip");
    std::fs::write(&output, b"preserved").unwrap();
    assert!(runtime
        .write_export_review(&reports[0].id, 1, &reports[0].archive_sha256, &output)
        .is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"preserved");
    let fresh = runtime
        .prepare_export_review(request("profiles", vec![id], false))
        .unwrap();
    assert!(!runtime.exports.lock().unwrap().contains_key(&reports[0].id));
    for report in reports {
        runtime.discard_export_review(&report.id);
    }
    runtime.discard_export_review(&fresh.id);
    assert!(runtime.exports.lock().unwrap().is_empty());
    runtime.shutdown();
}
