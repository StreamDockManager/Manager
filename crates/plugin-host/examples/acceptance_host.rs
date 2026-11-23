//! JSON-line harness for pinned external-package tests; never opens physical devices
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::Path,
    sync::mpsc,
    time::Duration,
};
use stream_dock_manager_plugin_host::{ActiveAction, HostEffect, PluginHost};
mod support;
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("package path required")?;
    let root = tempfile::tempdir()?;
    let path = Path::new(&path);
    let mut host = PluginHost::new(root.path().join("plugins"))?;
    support::set_test_display(&host)?;
    let prepared = PluginHost::prepare_install(&host.installation_root(), path)?;
    let source = PluginHost::prepared_source(&prepared);
    let manifest = PluginHost::prepared_manifest(&prepared);
    let info = host.replace_prepared(prepared)?;
    support::runtime(
        &host,
        &info.id,
        std::env::args().any(|a| a == "--allow-runtime-override"),
    )?;
    host.enable(&info.id, true)?;
    support::demand(&mut host, &info.id, "test-device");
    let view = host.browser_view(&info.id)?;
    println!(
        "{}",
        json!({"ready":info,"url":view.map(|v|v.url),"source":source,"manifest":manifest,"compatibility":host.preflight(&info.id)?})
    );
    io::stdout().flush()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    loop {
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(line) => {
                let command: Value = serde_json::from_str(&line)?;
                let action = command["action"].as_str().unwrap_or(&info.actions[0].id);
                let control = command["control"].as_str().unwrap_or("key-1");
                let settings = command.get("settings").cloned().unwrap_or(json!({}));
                match command["op"].as_str().unwrap_or("") {
                    "connect" => host.synchronize(vec![],vec![json!({"id":"test-device","name":"Descriptor simulator","type":7,"size":{"columns":5,"rows":2}})]),
                    "disconnect" => { host.reconcile_demand(Default::default()); host.synchronize(vec![],vec![]); },
                    "status" => println!("{}",json!({"status":host.list()})),
                    "appear" => host.synchronize(vec![ActiveAction{plugin_id:info.id.clone(),action_id:action.into(),device_id:"test-device".into(),control_id:control.into(),scope:"test:main".into(),settings,surface:None,column:0,row:0,encoder:control.starts_with("dial")}],vec![json!({"id":"test-device","name":"Descriptor simulator","type":7,"size":{"columns":5,"rows":2}})]),
                    "input" => { let input: hardware_core::Input = serde_json::from_value(command.get("input").cloned().unwrap_or(json!({"kind":"button","pressed":true})))?; host.dispatch(&info.id,action,"test-device",control,"test:main",&input,&settings)?; },
                    "inspector" => { let view=host.settings_panel(&info.id, Some(action), &settings)?; println!("{}",json!({"inspector":view.url})); },
                    "disable" => host.enable(&info.id,false)?,
                    "quit" => break,
                    _=>return Err("unknown harness command".into()),
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        host.poll();
        for effect in host.drain_effects() {
            // The harness retains its JSON-line reporting contract. Host-only
            // metadata is projected here, never injected into plugin messages.
            let (effect, rendered) = match effect {
                HostEffect::Feedback(feedback) => {
                    let rendered = feedback
                        .visual
                        .as_deref()
                        .map(|visual| {
                            hardware_core::render::visual(visual, 176, 112, 0)
                                .map(|bytes| bytes.len())
                                .map_err(|error| error.to_string())
                        })
                        .transpose()?;
                    let mut message = feedback.message;
                    let fields = message.as_object_mut().expect("validated feedback object");
                    for key in [
                        "hostPlugin",
                        "hostSession",
                        "hostDevice",
                        "hostControl",
                        "hostVisual",
                        "hostRenderGeneration",
                    ] {
                        fields.remove(key);
                    }
                    if let Some(target) = feedback.target {
                        fields.insert("hostDevice".into(), json!(target.device));
                        fields.insert("hostControl".into(), json!(target.control));
                    }
                    if let Some(visual) = feedback.visual {
                        fields.insert("hostVisual".into(), visual.as_ref().clone());
                    }
                    if let Some(generation) = feedback.generation {
                        fields.insert("hostRenderGeneration".into(), json!(generation));
                    }
                    (message, rendered)
                }
                HostEffect::SwitchProfile(request) => (
                    json!({"event":"switchToProfile","hostPlugin":request.plugin,"device":request.device,"payload":{"profile":request.profile,"page":request.page},"status":"requires-runtime-profile-review"}),
                    None,
                ),
                HostEffect::DesktopInput(input) => (
                    json!({"event":"desktopInput","context":input.context,"hostPlugin":input.plugin,"hostSession":input.session,"payload":input.payload}),
                    None,
                ),
                HostEffect::OpenUrl { context, url } => (
                    json!({"event":"openUrl","context":context,"payload":{"url":url}}),
                    None,
                ),
            };
            println!("{}", json!({"effect":effect,"rendered_bytes":rendered}));
        }
        io::stdout().flush()?;
    }
    host.shutdown();
    Ok(())
}
