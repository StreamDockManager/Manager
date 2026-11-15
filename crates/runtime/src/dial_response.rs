//! Host-side dial response. Raw reports and plugin protocol remain unchanged
use hardware_core::Input;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DialCurve {
    Linear,
    Gentle,
    Fast,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct DialResponse {
    pub curve: DialCurve,
    pub max_steps: u8,
}
impl DialResponse {
    // Validate the supplied dial response value
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=100).contains(&self.max_steps) {
            return Err("Dial step cap must be between 1 and 100".into());
        }
        Ok(())
    }
}
struct Sample {
    at: Instant,
    direction: i32,
    identity: String,
    epoch: u64,
}
#[derive(Default)]
pub(crate) struct Responses {
    samples: HashMap<(String, String), Sample>,
}
impl Responses {
    #[allow(clippy::too_many_arguments)] // Keep device identity, binding state, and event timing explicit
                                         // Transform the dial response value
    pub(crate) fn transform(
        &mut self,
        device: &str,
        control: &str,
        epoch: u64,
        identity: String,
        response: Option<DialResponse>,
        input: &Input,
        now: Instant,
    ) -> Input {
        let key = (device.to_owned(), control.to_owned());
        let (Some(response), Input::Rotate { delta }) = (response, input) else {
            self.samples.remove(&key);
            return input.clone();
        };
        if *delta == 0 {
            return input.clone();
        }
        let direction = delta.signum();
        let mut multiplier = 1;
        if let Some(last) = self.samples.get(&key).filter(|last| {
            last.epoch == epoch && last.identity == identity && last.direction == direction
        }) {
            let elapsed = now.saturating_duration_since(last.at);
            if elapsed < Duration::from_millis(250) {
                let rate = f64::from(delta.unsigned_abs()) / elapsed.as_secs_f64().max(0.000001);
                let tier = if rate >= 16.0 {
                    2
                } else if rate >= 8.0 {
                    1
                } else {
                    0
                };
                multiplier = match response.curve {
                    DialCurve::Linear => 1,
                    DialCurve::Gentle => [1, 2, 3][tier],
                    DialCurve::Fast => [1, 3, 5][tier],
                };
            }
        }
        // Bound stale device/control entries even when a device disappears without a report
        self.samples
            .retain(|_, sample| now.saturating_duration_since(sample.at) < Duration::from_secs(1));
        self.samples.insert(
            key,
            Sample {
                at: now,
                direction,
                identity,
                epoch,
            },
        );
        let steps = (i64::from(*delta).abs() * multiplier).min(i64::from(response.max_steps));
        Input::Rotate {
            delta: steps as i32 * direction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that thresholds caps direction and resets
    fn thresholds_caps_direction_and_resets() {
        let mut state = Responses::default();
        let start = Instant::now();
        let mut apply = |ms, delta, curve, epoch, identity: &str, cap| {
            state.transform(
                "dock",
                "dial",
                epoch,
                identity.into(),
                Some(DialResponse {
                    curve,
                    max_steps: cap,
                }),
                &Input::Rotate { delta },
                start + Duration::from_millis(ms),
            )
        };
        assert_eq!(
            apply(0, 1, DialCurve::Gentle, 1, "a", 100),
            Input::Rotate { delta: 1 }
        );
        assert_eq!(
            apply(125, 1, DialCurve::Gentle, 1, "a", 100),
            Input::Rotate { delta: 2 }
        );
        assert_eq!(
            apply(187, 1, DialCurve::Gentle, 1, "a", 100),
            Input::Rotate { delta: 3 }
        );
        assert_eq!(
            apply(249, -1, DialCurve::Fast, 1, "a", 100),
            Input::Rotate { delta: -1 }
        );
        assert_eq!(
            apply(311, -2, DialCurve::Fast, 1, "a", 6),
            Input::Rotate { delta: -6 }
        );
        assert_eq!(
            apply(561, -1, DialCurve::Fast, 1, "a", 100),
            Input::Rotate { delta: -1 }
        );
        assert_eq!(
            apply(570, -1, DialCurve::Fast, 2, "a", 100),
            Input::Rotate { delta: -1 }
        );
        assert_eq!(
            apply(580, -1, DialCurve::Fast, 2, "b", 100),
            Input::Rotate { delta: -1 }
        );
        assert_eq!(
            apply(590, i32::MIN, DialCurve::Fast, 2, "b", 100),
            Input::Rotate { delta: -100 }
        );
    }
    #[test]
    // Verify that legacy delta and press release are preserved
    fn legacy_delta_and_press_release_are_preserved() {
        let mut state = Responses::default();
        let now = Instant::now();
        let response = Some(DialResponse {
            curve: DialCurve::Fast,
            max_steps: 3,
        });
        for input in [
            Input::Button { pressed: true },
            Input::Button { pressed: false },
            Input::Pulse,
        ] {
            state.transform(
                "d",
                "c",
                1,
                "b".into(),
                response,
                &Input::Rotate { delta: 1 },
                now,
            );
            assert_eq!(
                state.transform("d", "c", 1, "b".into(), response, &input, now),
                input
            );
            assert_eq!(
                state.transform(
                    "d",
                    "c",
                    1,
                    "b".into(),
                    response,
                    &Input::Rotate { delta: 1 },
                    now
                ),
                Input::Rotate { delta: 1 }
            );
        }
        assert_eq!(
            state.transform(
                "d",
                "c",
                1,
                "b".into(),
                None,
                &Input::Rotate { delta: 500 },
                now
            ),
            Input::Rotate { delta: 500 }
        );
        for cap in [0, 101, 255] {
            assert!(DialResponse {
                curve: DialCurve::Linear,
                max_steps: cap
            }
            .validate()
            .is_err());
        }
    }
}
