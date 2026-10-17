//! Non-token units of a call: images generated, seconds of audio transcribed and characters
//! synthesised. What the request already says is reserved at admission; what the response says is
//! charged at settlement, and only for a call that succeeded.

use antirion_dialects::{Dialect, Endpoint};
use antirion_limits::cost::FixedUnits;
use serde_json::Value;

use crate::parse::Parsed;

/// The `size` and `quality` an image generation without them gets.
const DEFAULT_SIZE: &str = "1024x1024";
const DEFAULT_QUALITY: &str = "standard";

/// What the request says about its units.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RequestUnits {
    /// Unit charges known before the call, reserved in `ρ_usd`.
    pub(crate) fixed: FixedUnits,
    /// An image generation's `"<size>/<quality>"` and `n`.
    pub(crate) images: Option<(String, u64)>,
    /// A transcription's duration read from its WAV input, in milliseconds.
    pub(crate) audio_ms: Option<u64>,
}

fn operation(endpoint: &Endpoint) -> Option<&'static str> {
    match endpoint.dialect {
        Dialect::OpenAiImages => Some("images"),
        Dialect::OpenAiAudio => endpoint.upstream_path,
        _ => None,
    }
}

/// The price key of an image generation: `"<size>/<quality>"`.
fn image_key(body: &Value) -> String {
    let field = |k: &str, d: &str| body.get(k).and_then(Value::as_str).unwrap_or(d).to_owned();
    format!(
        "{}/{}",
        field("size", DEFAULT_SIZE),
        field("quality", DEFAULT_QUALITY)
    )
}

/// The units a request asks for.
pub(crate) fn of_request(endpoint: &Endpoint, parsed: &Parsed) -> RequestUnits {
    let mut out = RequestUnits::default();
    match (operation(endpoint), parsed) {
        (Some("images"), Parsed::Json { .. }) => {
            let body = parsed.doc().unwrap_or(&Value::Null);
            let n = body.get("n").and_then(Value::as_u64).unwrap_or(1).max(1);
            let key = image_key(body);
            out.fixed.images = Some((n, key.clone()));
            out.images = Some((key, n));
        }
        (Some("images"), Parsed::Multipart { raw, form, .. }) => {
            let text = |k: &str| {
                form.field(raw, k)
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .map(|v| v.trim().to_owned())
            };
            let n = text("n").and_then(|v| v.parse::<u64>().ok()).unwrap_or(1).max(1);
            let key = format!(
                "{}/{}",
                text("size").unwrap_or_else(|| DEFAULT_SIZE.to_owned()),
                text("quality").unwrap_or_else(|| DEFAULT_QUALITY.to_owned())
            );
            out.fixed.images = Some((n, key.clone()));
            out.images = Some((key, n));
        }
        (Some("/audio/speech"), Parsed::Json { .. }) => {
            let chars = parsed
                .doc()
                .and_then(|b| b.get("input"))
                .and_then(Value::as_str)
                .map_or(0, |s| s.chars().count() as u64);
            out.fixed.characters_in = chars;
        }
        (Some("/audio/transcriptions" | "/audio/translations"), Parsed::Multipart { raw, form, .. }) => {
            out.audio_ms = form.field(raw, "file").and_then(wav_duration_ms);
        }
        _ => {}
    }
    out
}

/// The images a generation response carries: entries with a payload, an image split across a
/// `url` entry and a `b64_json` entry counted once, never more than were asked for.
pub(crate) fn images_in(v: &Value, asked: u64) -> u64 {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let with = |k: &str| {
        data.iter()
            .filter(|e| e.get(k).is_some_and(|x| !x.is_null()))
            .count() as u64
    };
    with("url").max(with("b64_json")).min(asked)
}

/// The audio duration a transcription response reports, in milliseconds.
pub(crate) fn duration_ms(v: &Value) -> Option<u64> {
    let secs = v.get("duration")?.as_f64()?;
    (secs.is_finite() && secs >= 0.0).then(|| (secs * 1000.0).round() as u64)
}

/// The duration of a RIFF/WAVE file from its `fmt ` byte rate and its `data` chunk size.
fn wav_duration_ms(b: &[u8]) -> Option<u64> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    let mut at = 12;
    let mut byte_rate = None;
    while let (Some(id), Some(size)) = (b.get(at..at + 4), u32_at(at + 4)) {
        let body = at + 8;
        match id {
            b"fmt " => byte_rate = u32_at(body + 8).filter(|r| *r > 0),
            b"data" => {
                let rate = u64::from(byte_rate?);
                return Some(u64::from(size).saturating_mul(1000) / rate);
            }
            _ => {}
        }
        at = body
            .checked_add(usize::try_from(size).ok()?)?
            .checked_add(usize::from(size % 2 == 1))?;
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn wav(seconds: u32) -> Vec<u8> {
        let rate = 8000u32;
        let data = rate * 2 * seconds;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        b.resize(b.len() + data as usize, 0);
        b
    }

    #[test]
    fn images_count_payloads_once() {
        let v = json!({"data": [{"url": "u"}, {"b64_json": "x"}, {"revised_prompt": "p"}]});
        assert_eq!(images_in(&v, 2), 1);
        let two = json!({"data": [{"url": "u"}, {"url": "v"}]});
        assert_eq!(images_in(&two, 2), 2);
        assert_eq!(images_in(&two, 1), 1);
        assert_eq!(images_in(&json!({}), 3), 0);
        assert_eq!(
            image_key(&json!({"size": "1024x1024", "quality": "hd"})),
            "1024x1024/hd"
        );
        assert_eq!(image_key(&json!({})), "1024x1024/standard");
    }

    #[test]
    fn durations() {
        assert_eq!(duration_ms(&json!({"duration": 12.5})), Some(12_500));
        assert_eq!(duration_ms(&json!({"text": "x"})), None);
        assert_eq!(wav_duration_ms(&wav(3)), Some(3000));
        assert_eq!(wav_duration_ms(b"RIFF"), None);
        assert_eq!(wav_duration_ms(&wav(1)[..30]), None);
    }
}
