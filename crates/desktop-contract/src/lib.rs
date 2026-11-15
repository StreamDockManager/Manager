//! Desktop IPC contracts come from the actual Rust handlers, not a second command list
use std::{collections::BTreeSet, path::Path};
use syn::{FnArg, GenericArgument, Item, PathArguments, ReturnType, Type};

pub struct Command {
    pub name: String,
    pub path: String,
    arguments: Vec<(String, Type)>,
    result: Type,
    raw_body: bool,
}

// Recognize a Tauri command annotation
fn command_attribute(attribute: &syn::Attribute) -> bool {
    let segments = &attribute.path().segments;
    segments.len() == 2 && segments[0].ident == "tauri" && segments[1].ident == "command"
}

// Convert a Rust identifier to its JavaScript command spelling
fn camel_case(name: &str) -> String {
    let mut parts = name.split('_');
    let mut out = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars);
        }
    }
    out
}

// Extract the final identifier from a Rust type path
fn last_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(reference) => last_name(&reference.elem),
        _ => None,
    }
}

// Parse for the current operation
fn parse(source: &str, module: &str) -> Result<Vec<Command>, String> {
    let file = syn::parse_file(source).map_err(|e| format!("{module}: {e}"))?;
    let mut commands = vec![];
    for item in file.items {
        let Item::Fn(function) = item else { continue };
        let Some(attribute) = function.attrs.iter().find(|a| command_attribute(a)) else {
            continue;
        };
        if !matches!(attribute.meta, syn::Meta::Path(_)) {
            return Err("Command attribute options require explicit contract support".into());
        }
        let name = function.sig.ident.to_string();
        let mut arguments = vec![];
        let mut raw_body = false;
        for argument in &function.sig.inputs {
            let FnArg::Typed(argument) = argument else {
                return Err(format!("{name}: command methods are unsupported"));
            };
            if matches!(
                last_name(&argument.ty).as_deref(),
                Some("State" | "AppHandle" | "WebviewWindow" | "EditorWork" | "EditorRuntime")
            ) {
                continue;
            }
            if last_name(&argument.ty).as_deref() == Some("Request") {
                raw_body = true;
                continue;
            }
            let syn::Pat::Ident(pattern) = argument.pat.as_ref() else {
                return Err(format!("{name}: command parameters must be named"));
            };
            arguments.push((
                camel_case(&pattern.ident.to_string()),
                (*argument.ty).clone(),
            ));
        }
        let result = match function.sig.output {
            ReturnType::Default => syn::parse_quote!(()),
            ReturnType::Type(_, ty) => *ty,
        };
        commands.push(Command {
            path: if module.is_empty() {
                name.clone()
            } else {
                format!("{module}::{name}")
            },
            name,
            arguments,
            result,
            raw_body,
        });
    }
    Ok(commands)
}

// Discover for the current operation
pub fn discover(root: &Path) -> Result<Vec<Command>, String> {
    let mut commands = vec![];
    // Visit for the current operation
    fn visit(directory: &Path, module: &str, commands: &mut Vec<Command>) -> Result<(), String> {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let name = path
                .file_stem()
                .and_then(|v| v.to_str())
                .unwrap_or_default();
            let qualified = if module.is_empty() {
                name.to_string()
            } else {
                format!("{module}::{name}")
            };
            if path.is_dir() {
                visit(&path, &qualified, commands)?;
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let module = if name == "main" || name == "mod" {
                    module
                } else {
                    &qualified
                };
                commands.extend(parse(
                    &std::fs::read_to_string(&path).map_err(|e| e.to_string())?,
                    module,
                )?);
            }
        }
        Ok(())
    }
    visit(&root.join("src-tauri/src"), "", &mut commands)?;
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in commands.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!("Duplicate desktop command {}", pair[0].name));
        }
    }
    if commands.is_empty() {
        return Err("No native desktop commands found".into());
    }
    Ok(commands)
}

/// Capabilities stay an explicit security policy: a new handler cannot grant itself access
pub fn validate_capabilities(root: &Path, commands: &[Command]) -> Result<(), String> {
    let names: BTreeSet<_> = commands.iter().map(|c| c.name.as_str()).collect();
    let mut covered = BTreeSet::new();
    for entry in
        std::fs::read_dir(root.join("src-tauri/capabilities")).map_err(|e| e.to_string())?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let capability: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let permissions = capability["permissions"]
            .as_array()
            .ok_or("Missing capability permissions")?;
        for permission in permissions {
            let Some(permission) = permission.as_str() else {
                continue;
            };
            let Some(command) = permission
                .strip_prefix("allow-")
                .or_else(|| permission.strip_prefix("deny-"))
            else {
                continue;
            };
            let command = command.replace('-', "_");
            if !names.contains(command.as_str()) {
                return Err(format!(
                    "{} refers to unknown desktop command {command}",
                    path.display()
                ));
            }
            if permission.starts_with("allow-") {
                let popup = matches!(command.as_str(), "action_ui_request" | "action_ui_reply");
                let log_reader = matches!(
                    command.as_str(),
                    "client_preferences"
                        | "plugin_log_plugins"
                        | "plugin_log_preferences"
                        | "save_plugin_log_preferences"
                        | "plugin_log_captures"
                        | "start_plugin_log_capture"
                        | "stop_plugin_log_capture"
                        | "resume_plugin_log_capture"
                        | "clear_plugin_log_capture"
                        | "subscribe_plugin_logs"
                        | "unsubscribe_plugin_logs"
                        | "read_plugin_logs"
                        | "plugin_log_network_detail"
                        | "export_plugin_log_capture"
                );
                let scopes = if popup {
                    vec![vec!["action-popup-*"]]
                } else if matches!(
                    command.as_str(),
                    "appearance_preferences" | "report_frontend_error"
                ) {
                    vec![vec!["main"], vec!["log-reader-*"], vec!["action-popup-*"]]
                } else if log_reader {
                    vec![vec!["main"], vec!["log-reader-*"]]
                } else {
                    vec![vec!["main"]]
                };
                if !scopes
                    .iter()
                    .any(|windows| capability["windows"] == serde_json::json!(windows))
                    || capability.get("remote").is_some()
                    || capability.get("webviews").is_some()
                {
                    return Err(format!(
                        "{command}: desktop permissions must stay scoped to {}",
                        scopes
                            .iter()
                            .map(|scope| scope.join(", "))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    ));
                }
            }
            covered.insert(command);
        }
    }
    let missing: Vec<_> = names
        .into_iter()
        .filter(|name| !covered.contains(*name))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Desktop commands require an explicit capability decision: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

// Generate the Tauri command handler expression
pub fn handler(commands: &[Command]) -> String {
    format!(
        "tauri::generate_handler![{}]",
        commands
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>()
            .join(",")
    )
}

// Translate a Rust command type into its TypeScript representation
fn types(ty: &Type, exports: &BTreeSet<&str>) -> Result<String, String> {
    match ty {
        Type::Reference(reference) => types(&reference.elem, exports),
        Type::Tuple(tuple) if tuple.elems.is_empty() => Ok("void".into()),
        Type::Tuple(tuple) => Ok(format!(
            "[{}]",
            tuple
                .elems
                .iter()
                .map(|ty| types(ty, exports))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        )),
        Type::Path(path) => {
            let segment = path.path.segments.last().ok_or("Empty Rust type")?;
            let name = segment.ident.to_string();
            let arguments: Vec<_> = match &segment.arguments {
                PathArguments::AngleBracketed(args) => args
                    .args
                    .iter()
                    .filter_map(|arg| {
                        if let GenericArgument::Type(ty) = arg {
                            Some(ty)
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => vec![],
            };
            let argument = |index: usize| -> Result<String, String> {
                types(
                    arguments
                        .get(index)
                        .ok_or_else(|| format!("Missing type parameter in {name}"))?,
                    exports,
                )
            };
            match name.as_str() {
                "String" | "str" => Ok("string".into()),
                "bool" => Ok("boolean".into()),
                "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize"
                | "f32" | "f64" => Ok("number".into()),
                "Result" => argument(0),
                "Option" => Ok(format!("{} | null", argument(0)?)),
                "Vec" => Ok(format!("Array<{}>", argument(0)?)),
                "BTreeMap" | "HashMap" => Ok(format!(
                    "Partial<Record<{}, {}>>",
                    argument(0)?,
                    argument(1)?
                )),
                "Channel" => Ok(format!("Channel<{}>", argument(0)?)),
                "Response" => Ok("ArrayBuffer".into()),
                "Value" => Ok("runtime.JsonValue".into()),
                _ if exports.contains(name.as_str()) => Ok(format!("runtime.{name}")),
                _ => Err(format!("Export the command DTO {name} through xtask types")),
            }
        }
        Type::Array(array) => {
            let syn::Expr::Lit(length) = &array.len else {
                return Err("Unsupported array length".into());
            };
            let syn::Lit::Int(length) = &length.lit else {
                return Err("Unsupported array length".into());
            };
            let element = types(&array.elem, exports)?;
            Ok(format!(
                "[{}]",
                vec![element; length.base10_parse::<usize>().map_err(|e| e.to_string())?]
                    .join(", ")
            ))
        }
        _ => Err("Unsupported desktop IPC type".into()),
    }
}

// Generate the typed frontend command interface
pub fn typescript(commands: &[Command], runtime: &str) -> Result<String, String> {
    let exports = runtime
        .lines()
        .filter_map(|line| {
            line.strip_prefix("export type ")?
                .split_once(' ')
                .map(|(name, _)| name)
        })
        .collect();
    let mut errors = BTreeSet::new();
    for command in commands {
        for ty in command
            .arguments
            .iter()
            .map(|(_, ty)| ty)
            .chain(std::iter::once(&command.result))
        {
            if let Err(error) = types(ty, &exports) {
                errors.insert(format!("{}: {error}", command.name));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors.into_iter().collect::<Vec<_>>().join("\n"));
    }
    let mut out = "// Generated by cargo run -p xtask -- types. Do not edit.\nimport type * as runtime from './runtime';\nimport type { Channel } from '@tauri-apps/api/core';\n\nexport interface CommandContracts {\n".to_string();
    for command in commands {
        out.push_str(&format!("  {}: {{ args: ", command.name));
        if command.raw_body {
            if !command.arguments.is_empty() {
                return Err(format!(
                    "{} mixes raw IPC bodies and JSON arguments",
                    command.name
                ));
            }
            out.push_str("ArrayBuffer | Uint8Array | number[]");
        } else {
            out.push('{');
            for (name, ty) in &command.arguments {
                let optional = if last_name(ty).as_deref() == Some("Option") {
                    "?"
                } else {
                    ""
                };
                out.push_str(&format!(
                    " {name}{optional}: {};",
                    types(ty, &exports).map_err(|e| format!("{}: {e}", command.name))?
                ));
            }
            out.push_str(" }");
        }
        out.push_str(&format!(
            "; result: {} }};\n",
            types(&command.result, &exports).map_err(|e| format!("{}: {e}", command.name))?
        ));
    }
    out.push_str("}\nexport type CommandName = keyof CommandContracts;\nexport type CommandParameters<C extends CommandName> = {} extends CommandContracts[C]['args'] ? [args?: CommandContracts[C]['args']] : [args: CommandContracts[C]['args']];\nexport type CommandResult<C extends CommandName> = CommandContracts[C]['result'];\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify desktop benchmarks require revision and only expose progress contracts
    fn desktop_benchmarks_require_revision_and_only_expose_progress_contracts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let commands = discover(&root).unwrap();
        for retired in [
            "benchmark_start",
            "benchmark_status",
            "benchmark_cancel",
            "migrate_credential",
        ] {
            assert!(!commands.iter().any(|command| command.name == retired));
        }
        let export = commands
            .iter()
            .find(|command| command.name == "benchmark_export")
            .unwrap();
        for field in ["runId", "reportRevision"] {
            let (_, ty) = export
                .arguments
                .iter()
                .find(|(name, _)| name == field)
                .unwrap();
            assert_ne!(last_name(ty).as_deref(), Some("Option"));
        }
        validate_capabilities(&root, &commands).unwrap();
    }

    #[test]
    // Verify contracts follow real signatures and Tauri injection rules
    fn contracts_follow_real_signatures_and_tauri_injection_rules() {
        let commands = parse("#[tauri::command] async fn review(_editor_work: crate::editor_lifecycle::EditorWork, runtime: tauri::State<'_, Arc<Runtime>>, window: tauri::WebviewWindow, expected_revision: u64, page_id: Option<String>) -> Result<Vec<Preview>, String> { todo!() }", "commands").unwrap();
        let generated = typescript(&commands, "export type Preview = {};").unwrap();
        assert!(generated.contains("expectedRevision: number; pageId?: string | null;"));
        assert!(generated.contains("result: Array<runtime.Preview>"));
        assert!(!generated.contains("runtime:"));
        assert!(!generated.contains("editorWork"));
        assert_eq!(
            handler(&commands),
            "tauri::generate_handler![commands::review]"
        );
    }
    #[test]
    // Verify grouped editor runtime preserves the command contract
    fn grouped_editor_runtime_preserves_the_command_contract() {
        let separate = "#[tauri::command] async fn video_recording_frame(_editor_work: crate::editor_lifecycle::EditorWork, runtime: tauri::State<'_, Arc<Runtime>>, recording: String, position_us: u64, edge: Option<u16>, device_id: Option<String>, control_id: Option<String>, options: Option<MediaOptions>) -> Result<tauri::ipc::Response, String> { todo!() }";
        let grouped = separate.replace(
            "_editor_work: crate::editor_lifecycle::EditorWork, runtime: tauri::State<'_, Arc<Runtime>>",
            "editor: crate::editor_lifecycle::EditorRuntime",
        );
        let separate = parse(separate, "commands").unwrap();
        let grouped = parse(&grouped, "commands").unwrap();
        let exports = "export type MediaOptions = {};";
        assert_eq!(
            typescript(&grouped, exports).unwrap(),
            typescript(&separate, exports).unwrap()
        );
        assert_eq!(handler(&grouped), handler(&separate));
    }
    #[test]
    // Verify unsupported contract changes fail instead of silently widening types
    fn unsupported_contract_changes_fail_instead_of_silently_widening_types() {
        let commands = parse(
            "#[tauri::command] fn missing() -> NewResponse {todo!()}",
            "",
        )
        .unwrap();
        assert!(typescript(&commands, "")
            .unwrap_err()
            .contains("NewResponse"));
        assert!(parse(
            "#[tauri::command(rename_all = \"snake_case\")] fn changed() {}",
            ""
        )
        .is_err());
    }
    #[test]
    // Verify missing and misspelled permissions fail before desktop startup
    fn missing_and_misspelled_permissions_fail_before_desktop_startup() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("src-tauri/capabilities");
        std::fs::create_dir_all(&directory).unwrap();
        let commands = parse("#[tauri::command] fn pending_plugin_profile_setup() {}", "").unwrap();
        std::fs::write(directory.join("main.json"), r#"{"permissions":[]}"#).unwrap();
        assert!(validate_capabilities(root.path(), &commands)
            .unwrap_err()
            .contains("pending_plugin_profile_setup"));
        std::fs::write(
            directory.join("main.json"),
            r#"{"permissions":["allow-pending-plugin-profile-setpu"]}"#,
        )
        .unwrap();
        assert!(validate_capabilities(root.path(), &commands)
            .unwrap_err()
            .contains("unknown desktop command"));
        std::fs::write(
            directory.join("main.json"),
            r#"{"windows":["main"],"permissions":["allow-pending-plugin-profile-setup"]}"#,
        )
        .unwrap();
        validate_capabilities(root.path(), &commands).unwrap();
        for windows in [
            serde_json::json!(["*"]),
            serde_json::json!(["pi-*"]),
            serde_json::json!(["action-popup-*"]),
        ] {
            std::fs::write(directory.join("main.json"), serde_json::json!({"windows": windows, "permissions": ["allow-pending-plugin-profile-setup"]}).to_string()).unwrap();
            assert!(validate_capabilities(root.path(), &commands)
                .unwrap_err()
                .contains("must stay scoped"));
        }
    }
    #[test]
    // Verify log windows cannot gain workspace execution or plugin view permissions
    fn log_windows_cannot_gain_workspace_execution_or_plugin_view_permissions() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("src-tauri/capabilities");
        std::fs::create_dir_all(&directory).unwrap();
        let commands = parse(
            "#[tauri::command] fn read_plugin_logs() {} #[tauri::command] fn trigger_control() {}",
            "",
        )
        .unwrap();
        std::fs::write(
            directory.join("main.json"),
            r#"{"windows":["main"],"permissions":["allow-trigger-control"]}"#,
        )
        .unwrap();
        let write = |value: serde_json::Value| {
            std::fs::write(directory.join("logs.json"), value.to_string()).unwrap()
        };
        write(
            serde_json::json!({"windows":["log-reader-*"],"permissions":["allow-read-plugin-logs"]}),
        );
        validate_capabilities(root.path(), &commands).unwrap();
        for windows in [
            serde_json::json!(["pi-*"]),
            serde_json::json!(["*"]),
            serde_json::json!(["action-popup-*"]),
        ] {
            write(serde_json::json!({"windows":windows,"permissions":["allow-read-plugin-logs"]}));
            assert!(validate_capabilities(root.path(), &commands).is_err());
        }
        write(
            serde_json::json!({"windows":["log-reader-*"],"permissions":["allow-read-plugin-logs","allow-trigger-control"]}),
        );
        assert!(validate_capabilities(root.path(), &commands)
            .unwrap_err()
            .contains("trigger_control"));
    }
}
