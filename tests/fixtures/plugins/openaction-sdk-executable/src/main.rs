// Documented event contract pinned at OpenActionAPI/docs
// ffe2564bd42f54b19443cf1dac821adf6a85a848/src/4_clientbound/.
// This probe uses the unmodified upstream SDK pinned in Cargo.toml
use openaction::{global_events::*, *};
use serde_json::{json, Value};
use std::io::Write;
// Record for the current operation
fn record(event: &str, instance: Option<&Instance>, payload: Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("probe.jsonl")
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({"event":event,"context":instance.map(|i| &i.instance_id),"payload":payload})
    )
    .unwrap();
}
struct Probe;
#[async_trait]
impl Action for Probe {
    const UUID: &'static str = "com.manager.fixture.openaction.probe";
    type Settings = Value;
    // Record the will appear fixture callback
    async fn will_appear(&self, i: &Instance, s: &Value) -> OpenActionResult<()> {
        record("willAppear", Some(i), s.clone());
        i.set_title(Some("SDK ready"), None).await?;
        i.set_image(Some("icon"), None).await?;
        i.get_settings().await
    }
    // Record the will disappear fixture callback
    async fn will_disappear(&self, i: &Instance, s: &Value) -> OpenActionResult<()> {
        record("willDisappear", Some(i), s.clone());
        Ok(())
    }
    // Record the key down fixture callback
    async fn key_down(&self, i: &Instance, _: &Value) -> OpenActionResult<()> {
        record(
            "keyDown",
            Some(i),
            json!({"state":i.current_state_index.load(std::sync::atomic::Ordering::Relaxed)}),
        );
        i.set_title(None::<String>, None).await?;
        i.set_image(None::<String>, None).await
    }
    // Record the key up fixture callback
    async fn key_up(&self, i: &Instance, _: &Value) -> OpenActionResult<()> {
        record(
            "keyUp",
            Some(i),
            json!({"state":i.current_state_index.load(std::sync::atomic::Ordering::Relaxed)}),
        );
        Ok(())
    }
    // Record the dial rotate fixture callback
    async fn dial_rotate(
        &self,
        i: &Instance,
        _: &Value,
        ticks: i16,
        pressed: bool,
    ) -> OpenActionResult<()> {
        record(
            "dialRotate",
            Some(i),
            json!({"ticks":ticks,"pressed":pressed}),
        );
        Ok(())
    }
    // Record the dial down fixture callback
    async fn dial_down(&self, i: &Instance, _: &Value) -> OpenActionResult<()> {
        record("dialDown", Some(i), Value::Null);
        Ok(())
    }
    // Record the dial up fixture callback
    async fn dial_up(&self, i: &Instance, _: &Value) -> OpenActionResult<()> {
        record("dialUp", Some(i), Value::Null);
        Ok(())
    }
    // Touch tap
    async fn touch_tap(
        &self,
        i: &Instance,
        _: &Value,
        position: (u16, u16),
        hold: bool,
    ) -> OpenActionResult<()> {
        record(
            "touchTap",
            Some(i),
            json!({"position":position,"hold":hold}),
        );
        Ok(())
    }
    // Record the fixture notification that title parameters changed
    async fn title_parameters_did_change(
        &self,
        i: &Instance,
        _: &Value,
        event: &TitleParametersDidChangePayload,
    ) -> OpenActionResult<()> {
        record(
            "titleParametersDidChange",
            Some(i),
            json!({"title":event.title,"state":event.state,"fontSize":event.titleParameters.fontSize,"fontFamily":event.titleParameters.fontFamily,"showTitle":event.titleParameters.showTitle}),
        );
        Ok(())
    }
    // Record the property inspector did appear fixture callback
    async fn property_inspector_did_appear(&self, i: &Instance, _: &Value) -> OpenActionResult<()> {
        record("propertyInspectorDidAppear", Some(i), Value::Null);
        Ok(())
    }
    // Record the property inspector did disappear fixture callback
    async fn property_inspector_did_disappear(
        &self,
        i: &Instance,
        _: &Value,
    ) -> OpenActionResult<()> {
        record("propertyInspectorDidDisappear", Some(i), Value::Null);
        Ok(())
    }
    // Record the did receive settings fixture callback
    async fn did_receive_settings(&self, i: &Instance, s: &Value) -> OpenActionResult<()> {
        record("didReceiveSettings", Some(i), s.clone());
        Ok(())
    }
    // Send to plugin
    async fn send_to_plugin(
        &self,
        i: &Instance,
        _: &Value,
        payload: &Value,
    ) -> OpenActionResult<()> {
        record("sendToPlugin", Some(i), payload.clone());
        i.send_to_property_inspector(json!({"echo":payload})).await
    }
}
#[async_trait]
impl GlobalEventHandler for Probe {
    // Record the device did connect fixture callback
    async fn device_did_connect(&self, event: DeviceDidConnectEvent) -> OpenActionResult<()> {
        record(
            "deviceDidConnect",
            None,
            json!({"device":event.device,"name":event.deviceInfo.name,"rows":event.deviceInfo.size.rows,"columns":event.deviceInfo.size.columns}),
        );
        Ok(())
    }
    // Record the device did disconnect fixture callback
    async fn device_did_disconnect(&self, event: DeviceDidDisconnectEvent) -> OpenActionResult<()> {
        record("deviceDidDisconnect", None, json!({"device":event.device}));
        Ok(())
    }
    // Record the system did wake up fixture callback
    async fn system_did_wake_up(&self, _: SystemDidWakeUpEvent) -> OpenActionResult<()> {
        record("systemDidWakeUp", None, Value::Null);
        Ok(())
    }
    // Upstream does not publicly reexport ApplicationEvent or
    // DidReceiveDeepLinkEvent, so external crates cannot override those callbacks
    async fn plugin_ready(&self) -> OpenActionResult<()> {
        record("pluginReady", None, Value::Null);
        set_global_settings(json!(["opaque", 7])).await?;
        get_global_settings().await
    }
    // Record the did receive global settings fixture callback
    async fn did_receive_global_settings(
        &self,
        event: DidReceiveGlobalSettingsEvent,
    ) -> OpenActionResult<()> {
        record("didReceiveGlobalSettings", None, event.payload.settings);
        Ok(())
    }
    // Show settings interface
    async fn show_settings_interface(&self, _: ShowSettingsInterfaceEvent) -> OpenActionResult<()> {
        record("showSettingsInterface", None, Value::Null);
        Ok(())
    }
}
#[tokio::main]
// Run this executable entry point
async fn main() -> OpenActionResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let info = args.iter().position(|s| s == "-info").unwrap();
    record(
        "registrationInfo",
        None,
        serde_json::from_str(&args[info + 1]).unwrap(),
    );
    register_action(Probe).await;
    set_global_event_handler(&Probe);
    run(args).await
}
