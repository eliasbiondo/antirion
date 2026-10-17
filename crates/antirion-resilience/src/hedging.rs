//! Hedged requests (`policies.resilience.hedging`): when the first attempt of a generation request
//! has neither committed nor completed `delay` after it started, up to `max_extra` more attempts
//! start on other deployments; the first success wins and the others are cancelled.

use std::time::Duration;

use antirion_config::scalar::parse_duration;
use serde_json::Value;

/// The hedging of a merged `policies.resilience` value; `None` when it is off.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Hedging {
    pub delay: Duration,
    pub max_extra: u32,
}

impl Hedging {
    #[must_use]
    pub fn from_value(resilience: &Value) -> Option<Hedging> {
        let h = resilience.get("hedging")?;
        if h.get("enabled").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let delay = h
            .get("delay")
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(2));
        let max_extra = h
            .get("max_extra")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1)
            .clamp(1, 2);
        Some(Hedging { delay, max_extra })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_and_off() {
        assert_eq!(Hedging::from_value(&json!({})), None);
        assert_eq!(Hedging::from_value(&json!({"hedging": {"enabled": false}})), None);
        assert_eq!(
            Hedging::from_value(&json!({"hedging": {"enabled": true}})),
            Some(Hedging {
                delay: Duration::from_secs(2),
                max_extra: 1
            })
        );
        assert_eq!(
            Hedging::from_value(&json!({"hedging": {"enabled": true, "delay": "200ms", "max_extra": 2}})),
            Some(Hedging {
                delay: Duration::from_millis(200),
                max_extra: 2
            })
        );
    }
}
