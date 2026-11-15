//! Official web app DEVC reply to INFO. No fallback values or inferred defaults
use hardware_core::{Error, KeyboardBackgroundColor, KeyboardOsMode, KeyboardSettings, Result};
use serde_json::Value;

// Construct the invalid keyboard report error
fn invalid() -> Error {
    Error::Protocol("Invalid K1 keyboard settings report".into())
}
// Read a numeric keyboard setting
fn number(value: Option<&Value>, max: u64) -> Result<Option<u64>> {
    value
        .filter(|v| !v.is_null())
        .map(|v| v.as_u64().filter(|n| *n <= max).ok_or_else(invalid))
        .transpose()
}
// Decode the keyboard operating-system mode
fn os(value: &str) -> Result<KeyboardOsMode> {
    match value.chars().next() {
        Some('W') => Ok(KeyboardOsMode::Windows),
        Some('M') => Ok(KeyboardOsMode::Macos),
        _ => Err(invalid()),
    }
}
// Convert a keyboard HSV color to RGB
fn hsv_rgb(h: u16, s: u8, v: u8) -> [u8; 3] {
    let h = f64::from(h) / 60.0;
    let saturation = f64::from(s) / 255.0;
    let value = f64::from(v);
    let chroma = value * saturation;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let channels = match h as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    channels.map(|c| (c + value - chroma).round() as u8)
}
// Decode for the current operation
pub(super) fn decode(report: &[u8]) -> Result<Option<KeyboardSettings>> {
    let bytes = if report.first() == Some(&4) {
        &report[1..]
    } else {
        report
    };
    if !bytes.starts_with(b"DEVC") {
        return Ok(None);
    }
    if bytes.len() < 11 || bytes.len() > 4096 {
        return Err(invalid());
    }
    let mut settings = KeyboardSettings::default();
    if bytes[9] != 0 {
        settings.effect = Some(bytes[7]);
        settings.speed = Some(bytes[8]);
        settings.os_mode = Some(os(
            std::str::from_utf8(&bytes[9..10]).map_err(|_| invalid())?
        )?);
        settings.brightness = Some(bytes[10]);
    } else {
        let data = &bytes[11..];
        let end = data.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let value: Value = serde_json::from_slice(&data[..end]).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        if let Some(led) = value.get("led_info") {
            if !led.is_object() {
                return Err(invalid());
            }
            settings.effect = number(led.get("mode"), 15)?.map(|v| v as u8);
            settings.speed = number(led.get("speed"), 5)?.map(|v| v as u8);
            settings.brightness = number(led.get("brightness"), 100)?.map(|v| v as u8);
            settings.custom_color = number(led.get("flag"), 1)?.map(|v| v == 1);
            if let Some(hsv) = led.get("hsv").filter(|v| !v.is_null()) {
                let hsv = hsv
                    .as_array()
                    .filter(|a| a.len() == 3)
                    .ok_or_else(invalid)?;
                let h = number(hsv.first(), 359)?.ok_or_else(invalid)? as u16;
                let s = number(hsv.get(1), 255)?.ok_or_else(invalid)? as u8;
                let v = number(hsv.get(2), 255)?.ok_or_else(invalid)? as u8;
                settings.color = Some(hsv_rgb(h, s, v));
            }
            if let Some(hs) = led.get("base_hs").filter(|v| !v.is_null()) {
                let hs = hs.as_array().filter(|a| a.len() == 2).ok_or_else(invalid)?;
                let hue = number(hs.first(), 359)?.ok_or_else(invalid)? as u16;
                let saturation = number(hs.get(1), 255)?.ok_or_else(invalid)?;
                settings.background_color = Some(KeyboardBackgroundColor {
                    hue,
                    saturation: ((saturation * 100).div_ceil(255)) as u8,
                });
            }
        }
        if let Some(mode) = value.get("os").filter(|v| !v.is_null()) {
            let text = mode
                .as_str()
                .or_else(|| mode.as_array()?.first()?.as_str())
                .ok_or_else(invalid)?;
            settings.os_mode = Some(os(text)?);
        }
        // The vendor UI falls back for zero; no evidence establishes its meaning
        settings.sleep_timeout_seconds = if value.get("slpt").and_then(Value::as_i64) == Some(-1) {
            // Official UI accepts both signed and unsigned Never sentinels
            Some(u32::MAX)
        } else {
            number(value.get("slpt"), u64::from(u32::MAX))?
                .filter(|v| *v != 0)
                .map(|v| v as u32)
        };
    }
    settings.validate_values()?;
    if settings.speed.is_some_and(|v| v > 5) {
        return Err(invalid());
    }
    Ok(Some(settings))
}
