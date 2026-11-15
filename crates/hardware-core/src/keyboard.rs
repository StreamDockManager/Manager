use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardOsMode {
    Auto,
    Windows,
    Macos,
}

impl KeyboardOsMode {
    /// Resolve the saved preference only when preparing an explicit keyboard write
    pub fn resolved(self) -> Self {
        self.resolve_for_os(std::env::consts::OS)
    }

    // Resolve for OS
    fn resolve_for_os(self, os: &str) -> Self {
        match self {
            Self::Auto if os == "macos" => Self::Macos,
            Self::Auto => Self::Windows,
            explicit => explicit,
        }
    }
}

/// K1's background command carries hue and saturation only (full value)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct KeyboardBackgroundColor {
    pub hue: u16,
    pub saturation: u8,
}

/// Only explicitly supplied properties are written. These are keyboard outputs,
/// separate from display brightness and the device's lighting channels.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Keyboard backlight percentage (0..100), independent of LCD brightness
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<[u8; 3]>,
    /// False selects the device's rainbow palette without forgetting custom RGB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_color: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_color: Option<KeyboardBackgroundColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effect: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub speed: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub os_mode: Option<KeyboardOsMode>,
    /// Device-managed idle timer; u32::MAX disables timed sleep. Apply-only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sleep_timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardCapabilities {
    #[serde(default)]
    pub factory_reset: bool,
    #[serde(default)]
    pub settings_query: bool,
    pub brightness_max: u8,
    pub global_rgb: bool,
    pub os_modes: Vec<KeyboardOsMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effect_max: Option<u8>,
    /// Ordered wire indexes, not the vendor UI's decorative IDs
    #[serde(default)]
    pub effect_names: Vec<String>,
    #[serde(default)]
    pub background_color_effects: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub speed_max: Option<u8>,
    /// Supported firmware idle timers in seconds, including u32::MAX for Never
    #[serde(default)]
    pub sleep_timeout_seconds_options: Vec<u32>,
    #[serde(default)]
    pub effects_unavailable_reason: String,
    #[serde(default)]
    pub speed_unavailable_reason: String,
}

impl KeyboardCapabilities {
    // Validate for the current operation
    pub fn validate(&self) -> Result<()> {
        if self.brightness_max > 100
            || self.os_modes.len() > 2
            || self.os_modes.contains(&KeyboardOsMode::Auto)
            || (self.os_modes.len() == 2 && self.os_modes[0] == self.os_modes[1])
            || self.effect_max.is_some_and(|value| value > 15)
            || (!self.effect_names.is_empty()
                && (self.effect_max.map(usize::from).map(|max| max + 1)
                    != Some(self.effect_names.len())
                    || self.effect_names.iter().any(|name| name.trim().is_empty())))
            || self
                .background_color_effects
                .iter()
                .any(|effect| self.effect_max.is_none_or(|max| *effect > max))
            || self
                .background_color_effects
                .iter()
                .enumerate()
                .any(|(index, effect)| self.background_color_effects[..index].contains(effect))
            || self.sleep_timeout_seconds_options.len() > 32
            || self
                .sleep_timeout_seconds_options
                .iter()
                .enumerate()
                .any(|(index, value)| {
                    *value == 0 || self.sleep_timeout_seconds_options[..index].contains(value)
                })
            || self.speed_max.is_some_and(|value| value > 7)
            || (self.effect_max.is_none() && self.effects_unavailable_reason.trim().is_empty())
            || (self.speed_max.is_none() && self.speed_unavailable_reason.trim().is_empty())
        {
            return Err(Error::Descriptor("invalid keyboard capabilities".into()));
        }
        Ok(())
    }
}

impl KeyboardSettings {
    // Check whether the value is empty
    pub fn is_empty(&self) -> bool {
        self.brightness.is_none()
            && self.color.is_none()
            && self.custom_color.is_none()
            && self.background_color.is_none()
            && self.effect.is_none()
            && self.speed.is_none()
            && self.os_mode.is_none()
            && self.sleep_timeout_seconds.is_none()
    }

    // Validate values
    pub fn validate_values(&self) -> Result<()> {
        if self.is_empty() {
            return Err(Error::Protocol(
                "keyboard settings must not be empty".into(),
            ));
        }
        if self.brightness.is_some_and(|value| value > 100) {
            return Err(Error::Protocol("keyboard brightness must be 0..100".into()));
        }
        if self.effect.is_some_and(|value| value > 15) {
            return Err(Error::Protocol("keyboard effect must be 0..15".into()));
        }
        if self
            .background_color
            .is_some_and(|color| color.hue > 359 || color.saturation > 100)
        {
            return Err(Error::Protocol(
                "keyboard background color requires hue 0..359 and saturation 0..100".into(),
            ));
        }
        if self.sleep_timeout_seconds == Some(0) {
            return Err(Error::Protocol(
                "keyboard sleep timer must be positive seconds or Never".into(),
            ));
        }
        if self.speed.is_some_and(|value| value > 7) {
            return Err(Error::Protocol("keyboard speed must be 0..7".into()));
        }
        Ok(())
    }

    // Validate for
    pub fn validate_for(&self, capabilities: &KeyboardCapabilities) -> Result<()> {
        self.validate_values()?;
        capabilities.validate()?;
        if self
            .brightness
            .is_some_and(|value| value > capabilities.brightness_max)
            || ((self.color.is_some() || self.custom_color.is_some()) && !capabilities.global_rgb)
            || (self.background_color.is_some() && capabilities.background_color_effects.is_empty())
            || self
                .effect
                .is_some_and(|value| capabilities.effect_max.is_none_or(|max| value > max))
            || self
                .speed
                .is_some_and(|value| capabilities.speed_max.is_none_or(|max| value > max))
            || self
                .sleep_timeout_seconds
                .is_some_and(|value| !capabilities.sleep_timeout_seconds_options.contains(&value))
            || (self.effect == Some(0) && capabilities.speed_max.is_none())
            || self
                .os_mode
                .is_some_and(|mode| !capabilities.os_modes.contains(&mode.resolved()))
        {
            return Err(Error::Unsupported(
                "keyboard settings exceed device capabilities".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify automatic mode resolves host without changing explicit preferences
    fn automatic_mode_resolves_host_without_changing_explicit_preferences() {
        for (os, expected) in [
            ("macos", KeyboardOsMode::Macos),
            ("linux", KeyboardOsMode::Windows),
            ("windows", KeyboardOsMode::Windows),
        ] {
            assert_eq!(KeyboardOsMode::Auto.resolve_for_os(os), expected);
            assert_eq!(
                KeyboardOsMode::Macos.resolve_for_os(os),
                KeyboardOsMode::Macos
            );
            assert_eq!(
                KeyboardOsMode::Windows.resolve_for_os(os),
                KeyboardOsMode::Windows
            );
        }
        let mut capabilities = KeyboardCapabilities {
            factory_reset: false,
            settings_query: false,
            brightness_max: 100,
            global_rgb: true,
            effect_max: Some(15),
            effect_names: vec![],
            background_color_effects: vec![14],
            speed_max: Some(7),
            sleep_timeout_seconds_options: vec![],
            os_modes: vec![KeyboardOsMode::Auto.resolved()],
            effects_unavailable_reason: "Unverified".into(),
            speed_unavailable_reason: "Unverified".into(),
        };
        let settings = KeyboardSettings {
            os_mode: Some(KeyboardOsMode::Auto),
            ..Default::default()
        };
        settings.validate_for(&capabilities).unwrap();
        capabilities.os_modes.clear();
        assert!(settings.validate_for(&capabilities).is_err());
        capabilities.os_modes.push(KeyboardOsMode::Auto);
        assert!(capabilities.validate().is_err());
    }

    #[test]
    // Verify keyboard brightness is a raw percentage without legacy rescaling
    fn keyboard_brightness_is_a_raw_percentage_without_legacy_rescaling() {
        for value in 0..=u8::MAX {
            let settings: KeyboardSettings =
                serde_json::from_value(serde_json::json!({"brightness": value})).unwrap();
            assert_eq!(settings.validate_values().is_ok(), value <= 100);
            assert_eq!(settings.brightness, Some(value));
            assert_eq!(serde_json::to_value(settings).unwrap()["brightness"], value);
        }
    }

    #[test]
    // Verify partial settings round trip without adding unselected fields
    fn partial_settings_round_trip_without_adding_unselected_fields() {
        for value in [
            serde_json::json!({"brightness": 6}),
            serde_json::json!({"color": [0, 127, 255]}),
            serde_json::json!({"effect": 15}),
            serde_json::json!({"background_color": {"hue": 359, "saturation": 100}}),
            serde_json::json!({"speed": 7}),
            serde_json::json!({"os_mode": "macos"}),
            serde_json::json!({"os_mode": "windows"}),
            serde_json::json!({"os_mode": "auto"}),
        ] {
            let settings: KeyboardSettings = serde_json::from_value(value.clone()).unwrap();
            settings.validate_values().unwrap();
            assert_eq!(serde_json::to_value(settings).unwrap(), value);
        }
        assert!(serde_json::from_str::<KeyboardSettings>("{}")
            .unwrap()
            .is_empty());
        assert!(KeyboardSettings::default().validate_values().is_err());
    }
    #[test]
    // Verify effects and speed validate ranges and connection capabilities
    fn effects_and_speed_validate_ranges_and_connection_capabilities() {
        let mut caps = KeyboardCapabilities {
            factory_reset: false,
            settings_query: false,
            brightness_max: 100,
            global_rgb: true,
            os_modes: vec![KeyboardOsMode::Windows, KeyboardOsMode::Macos],
            effect_max: Some(15),
            effect_names: vec![],
            background_color_effects: vec![14],
            speed_max: Some(7),
            sleep_timeout_seconds_options: vec![],
            effects_unavailable_reason: String::new(),
            speed_unavailable_reason: String::new(),
        };
        for effect in 0..=u8::MAX {
            let settings = KeyboardSettings {
                effect: Some(effect),
                ..Default::default()
            };
            assert_eq!(settings.validate_for(&caps).is_ok(), effect <= 15);
        }
        for speed in 0..=u8::MAX {
            let settings = KeyboardSettings {
                speed: Some(speed),
                ..Default::default()
            };
            assert_eq!(settings.validate_for(&caps).is_ok(), speed <= 7);
        }
        caps.background_color_effects.clear();
        caps.effect_max = Some(2);
        caps.speed_max = Some(3);
        assert!(KeyboardSettings {
            effect: Some(3),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_err());
        assert!(KeyboardSettings {
            speed: Some(4),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_err());
        caps.effect_max = None;
        caps.speed_max = None;
        caps.effects_unavailable_reason = "Not supported".into();
        caps.speed_unavailable_reason = "Not supported".into();
        caps.validate().unwrap();
        assert!(KeyboardSettings {
            effect: Some(0),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_err());
        assert!(KeyboardSettings {
            speed: Some(0),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_err());
        caps.effect_max = Some(9);
        assert!(KeyboardSettings {
            effect: Some(0),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_err());
        assert!(KeyboardSettings {
            effect: Some(1),
            ..Default::default()
        }
        .validate_for(&caps)
        .is_ok());
        let old: KeyboardCapabilities = serde_json::from_value(serde_json::json!({
            "brightness_max": 100, "global_rgb": true, "os_modes": ["windows"],
            "effects_unavailable_reason": "Unknown", "speed_unavailable_reason": "Unknown"
        }))
        .unwrap();
        old.validate().unwrap();
        assert!(old.effect_max.is_none());
        assert!(old.speed_max.is_none());
    }
    #[test]
    // Verify sleep timer is optional nonzero and capability scoped
    fn sleep_timer_is_optional_nonzero_and_capability_scoped() {
        let mut caps: KeyboardCapabilities = serde_json::from_value(serde_json::json!({
            "brightness_max":100,"global_rgb":true,"os_modes":["windows"],
            "effect_max":15,"speed_max":5,
            "sleep_timeout_seconds_options":[60,300,900,1800,3600,7200,4294967295u32]
        }))
        .unwrap();
        for seconds in caps.sleep_timeout_seconds_options.clone() {
            let json = serde_json::json!({"sleep_timeout_seconds":seconds});
            let settings: KeyboardSettings = serde_json::from_value(json.clone()).unwrap();
            assert!(!settings.is_empty());
            settings.validate_for(&caps).unwrap();
            assert_eq!(serde_json::to_value(settings).unwrap(), json);
        }
        let timer = KeyboardSettings {
            sleep_timeout_seconds: Some(60),
            ..Default::default()
        };
        caps.sleep_timeout_seconds_options.clear();
        assert!(timer.validate_for(&caps).is_err());
        assert!(KeyboardSettings {
            sleep_timeout_seconds: Some(0),
            ..Default::default()
        }
        .validate_values()
        .is_err());
        for values in [vec![0], vec![60, 60]] {
            caps.sleep_timeout_seconds_options = values;
            assert!(caps.validate().is_err());
        }
    }
}
