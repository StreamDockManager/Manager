//! Project-owned native protocol acceptance package; compile on every target OS
use serde_json::{json, Value};
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--workload-child") {
        let mut allocation = vec![1u8; 24 * 1024 * 1024];
        loop {
            for byte in &mut allocation {
                *byte = byte.wrapping_add(1);
            }
            std::hint::black_box(&allocation);
        }
    }
    let _workload = if std::env::var_os("STREAM_DOCK_MANAGER_METRICS_WORKLOAD").is_some() {
        Some(
            std::process::Command::new(std::env::current_exe()?)
                .arg("--workload-child")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        )
    } else {
        None
    };
    let value = |flag: &str| {
        args.windows(2)
            .find(|p| p[0] == flag)
            .map(|p| p[1].clone())
            .ok_or("missing launch argument")
    };
    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", value("-port")?))?;
    ws.send(tungstenite::Message::text(
        json!({"event":value("-registerEvent")?,"uuid":value("-pluginUUID")?}).to_string(),
    ))?;
    loop {
        let message = ws.read()?;
        if let tungstenite::Message::Text(text) = message {
            let event: Value = serde_json::from_str(&text)?;
            if event["event"] == "shutdown" {
                return Ok(());
            }
            if event["event"] == "keyDown" {
                ws.send(tungstenite::Message::text(json!({"event":"setTitle","context":event["context"],"payload":{"title":"Native OK"}}).to_string()))?;
            }
        }
    }
}
