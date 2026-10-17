//! The normalised usage record, its disjoint pricing classes, and normalisation from each
//! provider's report.

use std::collections::BTreeMap;

use serde_json::Value;

/// The usage of one upstream call (or the sum over a request's calls). Cache reads and writes are
/// subsets of `input`, reasoning a subset of `output`; subsets are never added to their parent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Usage {
    pub input: u64,
    pub input_cache_read: u64,
    pub input_cache_write: u64,
    pub input_cache_write_1h: u64,
    pub input_audio: u64,
    pub input_image: u64,
    pub input_video: u64,
    pub output: u64,
    pub output_reasoning: u64,
    pub output_audio: u64,
    pub output_image: u64,
}

impl Usage {
    /// The record's fields under their names.
    #[must_use]
    pub fn fields(&self) -> [(&'static str, u64); 11] {
        [
            ("input", self.input),
            ("input_cache_read", self.input_cache_read),
            ("input_cache_write", self.input_cache_write),
            ("input_cache_write_1h", self.input_cache_write_1h),
            ("input_audio", self.input_audio),
            ("input_image", self.input_image),
            ("input_video", self.input_video),
            ("output", self.output),
            ("output_reasoning", self.output_reasoning),
            ("output_audio", self.output_audio),
            ("output_image", self.output_image),
        ]
    }

    /// The record read back from its fields; absent fields are 0.
    #[must_use]
    pub fn from_fields(v: &serde_json::Value) -> Usage {
        let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
        Usage {
            input: n("input"),
            input_cache_read: n("input_cache_read"),
            input_cache_write: n("input_cache_write"),
            input_cache_write_1h: n("input_cache_write_1h"),
            input_audio: n("input_audio"),
            input_image: n("input_image"),
            input_video: n("input_video"),
            output: n("output"),
            output_reasoning: n("output_reasoning"),
            output_audio: n("output_audio"),
            output_image: n("output_image"),
        }
    }

    /// Field-wise sum, saturating.
    #[must_use]
    pub fn plus(&self, o: &Usage) -> Usage {
        Usage {
            input: self.input.saturating_add(o.input),
            input_cache_read: self.input_cache_read.saturating_add(o.input_cache_read),
            input_cache_write: self.input_cache_write.saturating_add(o.input_cache_write),
            input_cache_write_1h: self.input_cache_write_1h.saturating_add(o.input_cache_write_1h),
            input_audio: self.input_audio.saturating_add(o.input_audio),
            input_image: self.input_image.saturating_add(o.input_image),
            input_video: self.input_video.saturating_add(o.input_video),
            output: self.output.saturating_add(o.output),
            output_reasoning: self.output_reasoning.saturating_add(o.output_reasoning),
            output_audio: self.output_audio.saturating_add(o.output_audio),
            output_image: self.output_image.saturating_add(o.output_image),
        }
    }

    /// The record for a call whose upstream reported no usage: `input` from the admission estimate,
    /// `output` from the gateway's own count, every subset 0.
    #[must_use]
    pub fn estimated(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            ..Usage::default()
        }
    }

    /// The record when only a total was reported: `input` from the estimate, the rest as output.
    #[must_use]
    pub fn from_total(total: u64, estimated_input: u64) -> Usage {
        Usage::estimated(estimated_input, total.saturating_sub(estimated_input))
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Usage::default()
    }

    /// The context size that selects a pricing tier: the whole prompt.
    #[must_use]
    pub const fn context(&self) -> u64 {
        self.input
    }

    /// The disjoint pricing classes of this record, clamped at zero.
    #[must_use]
    pub fn classes(&self) -> Classes {
        let uncached = self
            .input
            .saturating_sub(self.input_cache_read)
            .saturating_sub(self.input_cache_write);
        let audio = self.input_audio.min(uncached);
        let image = self.input_image.min(uncached - audio);
        let video = self.input_video.min(uncached - audio - image);
        let mut c = Classes::default();
        c.set(Class::InputAudioUncached, audio);
        c.set(Class::InputImageUncached, image);
        c.set(Class::InputVideoUncached, video);
        c.set(Class::InputText, uncached - audio - image - video);
        c.set(Class::InputCacheRead, self.input_cache_read);
        c.set(
            Class::InputCacheWrite5m,
            self.input_cache_write.saturating_sub(self.input_cache_write_1h),
        );
        c.set(Class::InputCacheWrite1h, self.input_cache_write_1h);
        c.set(
            Class::OutputText,
            self.output
                .saturating_sub(self.output_reasoning)
                .saturating_sub(self.output_audio)
                .saturating_sub(self.output_image),
        );
        c.set(Class::OutputReasoning, self.output_reasoning);
        c.set(Class::OutputAudio, self.output_audio);
        c.set(Class::OutputImage, self.output_image);
        c
    }
}

/// Non-token units of one call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Units {
    /// 1 per upstream call that produced a billable response, 0 for a failed call.
    pub requests: u64,
    /// Images by `"<size>/<quality>"`.
    pub images: BTreeMap<String, u64>,
    /// Input audio duration in milliseconds (durations are fractional seconds).
    pub audio_ms_in: u64,
    pub characters_in: u64,
    pub pages: u64,
    pub web_searches: u64,
}

impl Units {
    #[must_use]
    pub fn plus(&self, o: &Units) -> Units {
        let mut images = self.images.clone();
        for (k, v) in &o.images {
            let e = images.entry(k.clone()).or_insert(0);
            *e = e.saturating_add(*v);
        }
        Units {
            requests: self.requests.saturating_add(o.requests),
            images,
            audio_ms_in: self.audio_ms_in.saturating_add(o.audio_ms_in),
            characters_in: self.characters_in.saturating_add(o.characters_in),
            pages: self.pages.saturating_add(o.pages),
            web_searches: self.web_searches.saturating_add(o.web_searches),
        }
    }
}

/// A disjoint pricing class. Every token of a record is in exactly one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    InputText,
    InputAudioUncached,
    InputImageUncached,
    InputVideoUncached,
    InputCacheRead,
    InputCacheWrite5m,
    InputCacheWrite1h,
    OutputText,
    OutputReasoning,
    OutputAudio,
    OutputImage,
}

impl Class {
    pub const ALL: [Class; 11] = [
        Class::InputText,
        Class::InputAudioUncached,
        Class::InputImageUncached,
        Class::InputVideoUncached,
        Class::InputCacheRead,
        Class::InputCacheWrite5m,
        Class::InputCacheWrite1h,
        Class::OutputText,
        Class::OutputReasoning,
        Class::OutputAudio,
        Class::OutputImage,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::InputText => "input_text",
            Class::InputAudioUncached => "input_audio_uncached",
            Class::InputImageUncached => "input_image_uncached",
            Class::InputVideoUncached => "input_video_uncached",
            Class::InputCacheRead => "input_cache_read",
            Class::InputCacheWrite5m => "input_cache_write_5m",
            Class::InputCacheWrite1h => "input_cache_write_1h",
            Class::OutputText => "output_text",
            Class::OutputReasoning => "output_reasoning",
            Class::OutputAudio => "output_audio",
            Class::OutputImage => "output_image",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.as_str() == s)
    }

    #[must_use]
    pub const fn is_input(self) -> bool {
        (self as usize) < (Class::OutputText as usize)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// The amount of every pricing class of one record.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Classes([u64; 11]);

impl Classes {
    #[must_use]
    pub fn get(&self, c: Class) -> u64 {
        self.0.get(c.index()).copied().unwrap_or(0)
    }

    fn set(&mut self, c: Class, v: u64) {
        if let Some(slot) = self.0.get_mut(c.index()) {
            *slot = v;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Class, u64)> + '_ {
        Class::ALL.into_iter().map(|c| (c, self.get(c)))
    }
}

/// Which provider convention a usage report follows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Convention {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Gemini,
    BedrockConverse,
    Embeddings,
}

/// A usage report that cannot be read (a count that is not a non-negative integer).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("usage field {field} is not a non-negative integer")]
pub struct UsageError {
    pub field: String,
}

fn count(v: &Value, path: &[&str]) -> Result<u64, UsageError> {
    let mut cur = v;
    for p in path {
        match cur.get(p) {
            Some(next) => cur = next,
            None => return Ok(0),
        }
    }
    match cur {
        Value::Null => Ok(0),
        Value::Number(n) => n.as_u64().ok_or_else(|| UsageError {
            field: path.join("."),
        }),
        _ => Err(UsageError {
            field: path.join("."),
        }),
    }
}

fn gemini_modalities(v: &Value, field: &str) -> Result<BTreeMap<String, u64>, UsageError> {
    let mut out = BTreeMap::new();
    for entry in v.get(field).and_then(Value::as_array).into_iter().flatten() {
        let modality = entry
            .get("modality")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_uppercase();
        let n = count(entry, &["tokenCount"])?;
        let e = out.entry(modality).or_insert(0u64);
        *e = e.saturating_add(n);
    }
    Ok(out)
}

/// Normalises a provider's usage object into the record, and the web searches it reports.
pub fn normalize(conv: Convention, v: &Value) -> Result<(Usage, Units), UsageError> {
    let mut units = Units::default();
    let u = match conv {
        Convention::OpenAiChat => Usage {
            input: count(v, &["prompt_tokens"])?,
            input_cache_read: count(v, &["prompt_tokens_details", "cached_tokens"])?,
            input_cache_write: count(v, &["prompt_tokens_details", "cache_write_tokens"])?,
            input_audio: count(v, &["prompt_tokens_details", "audio_tokens"])?,
            input_image: count(v, &["prompt_tokens_details", "image_tokens"])?,
            output: count(v, &["completion_tokens"])?,
            output_reasoning: count(v, &["completion_tokens_details", "reasoning_tokens"])?,
            output_audio: count(v, &["completion_tokens_details", "audio_tokens"])?,
            ..Usage::default()
        },
        Convention::OpenAiResponses => Usage {
            input: count(v, &["input_tokens"])?,
            input_cache_read: count(v, &["input_tokens_details", "cached_tokens"])?,
            input_cache_write: count(v, &["input_tokens_details", "cache_write_tokens"])?,
            input_audio: count(v, &["input_tokens_details", "audio_tokens"])?,
            input_image: count(v, &["input_tokens_details", "image_tokens"])?,
            output: count(v, &["output_tokens"])?,
            output_reasoning: count(v, &["output_tokens_details", "reasoning_tokens"])?,
            ..Usage::default()
        },
        Convention::Anthropic => {
            let read = count(v, &["cache_read_input_tokens"])?;
            let breakdown_5m = count(v, &["cache_creation", "ephemeral_5m_input_tokens"])?;
            let breakdown_1h = count(v, &["cache_creation", "ephemeral_1h_input_tokens"])?;
            let write = match v.get("cache_creation_input_tokens") {
                Some(_) => count(v, &["cache_creation_input_tokens"])?,
                None => breakdown_5m.saturating_add(breakdown_1h),
            };
            units.web_searches = count(v, &["server_tool_use", "web_search_requests"])?;
            Usage {
                input: count(v, &["input_tokens"])?
                    .saturating_add(read)
                    .saturating_add(write),
                input_cache_read: read,
                input_cache_write: write,
                input_cache_write_1h: breakdown_1h,
                output: count(v, &["output_tokens"])?,
                ..Usage::default()
            }
        }
        Convention::Gemini => {
            let thoughts = count(v, &["thoughtsTokenCount"])?;
            let prompt = gemini_modalities(v, "promptTokensDetails")?;
            let candidates = gemini_modalities(v, "candidatesTokensDetails")?;
            let of = |m: &BTreeMap<String, u64>, k: &str| m.get(k).copied().unwrap_or(0);
            Usage {
                input: count(v, &["promptTokenCount"])?
                    .saturating_add(count(v, &["toolUsePromptTokenCount"])?),
                input_cache_read: count(v, &["cachedContentTokenCount"])?,
                input_audio: of(&prompt, "AUDIO"),
                input_image: of(&prompt, "IMAGE"),
                input_video: of(&prompt, "VIDEO"),
                output: count(v, &["candidatesTokenCount"])?.saturating_add(thoughts),
                output_reasoning: thoughts,
                output_audio: of(&candidates, "AUDIO"),
                output_image: of(&candidates, "IMAGE"),
                ..Usage::default()
            }
        }
        Convention::BedrockConverse => {
            let read = count(v, &["cacheReadInputTokens"])?;
            let write = count(v, &["cacheWriteInputTokens"])?;
            let mut write_1h = 0u64;
            for d in v
                .get("cacheDetails")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if d.get("ttl").and_then(Value::as_str) == Some("1h") {
                    write_1h = write_1h.saturating_add(count(d, &["inputTokens"])?);
                }
            }
            Usage {
                input: count(v, &["inputTokens"])?
                    .saturating_add(read)
                    .saturating_add(write),
                input_cache_read: read,
                input_cache_write: write,
                input_cache_write_1h: write_1h,
                output: count(v, &["outputTokens"])?,
                ..Usage::default()
            }
        }
        Convention::Embeddings => Usage {
            input: match v.get("prompt_tokens") {
                Some(_) => count(v, &["prompt_tokens"])?,
                None => count(v, &["input_tokens"])?,
            },
            ..Usage::default()
        },
    };
    Ok((u, units))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    #[test]
    fn openai_cached_and_reasoning_are_subsets() {
        let (u, _) = normalize(
            Convention::OpenAiChat,
            &json!({"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 300},
                    "completion_tokens": 500, "completion_tokens_details": {"reasoning_tokens": 200}}),
        )
        .unwrap();
        assert_eq!(
            (u.input, u.input_cache_read, u.output, u.output_reasoning),
            (1000, 300, 500, 200)
        );
        let c = u.classes();
        assert_eq!(c.get(Class::InputText), 700);
        assert_eq!(c.get(Class::InputCacheRead), 300);
        assert_eq!(c.get(Class::OutputText), 300);
        assert_eq!(c.get(Class::OutputReasoning), 200);
    }

    #[test]
    fn anthropic_disjoint_counts_are_summed() {
        let (u, _) = normalize(
            Convention::Anthropic,
            &json!({"input_tokens": 700, "cache_read_input_tokens": 300,
                    "cache_creation_input_tokens": 100, "output_tokens": 500}),
        )
        .unwrap();
        assert_eq!(
            (u.input, u.input_cache_read, u.input_cache_write, u.output),
            (1100, 300, 100, 500)
        );
        let c = u.classes();
        assert_eq!(c.get(Class::InputText), 700);
        assert_eq!(c.get(Class::InputCacheWrite5m), 100);
        assert_eq!(c.get(Class::OutputText), 500);
    }

    #[test]
    fn anthropic_breakdown_and_web_search() {
        let (u, units) = normalize(
            Convention::Anthropic,
            &json!({"input_tokens": 10, "cache_creation": {"ephemeral_5m_input_tokens": 4, "ephemeral_1h_input_tokens": 6},
                    "output_tokens": 1, "server_tool_use": {"web_search_requests": 2}}),
        )
        .unwrap();
        assert_eq!(
            (u.input, u.input_cache_write, u.input_cache_write_1h),
            (20, 10, 6)
        );
        assert_eq!(u.classes().get(Class::InputCacheWrite5m), 4);
        assert_eq!(units.web_searches, 2);
    }

    #[test]
    fn cached_larger_than_prompt_clamps() {
        let (u, _) = normalize(
            Convention::OpenAiChat,
            &json!({"prompt_tokens": 500, "prompt_tokens_details": {"cached_tokens": 1000}}),
        )
        .unwrap();
        assert_eq!(u.classes().get(Class::InputText), 0);
    }

    #[test]
    fn gemini_thoughts_and_modalities() {
        let (u, _) = normalize(
            Convention::Gemini,
            &json!({"promptTokenCount": 100, "toolUsePromptTokenCount": 5, "cachedContentTokenCount": 20,
                    "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 60}, {"modality": "IMAGE", "tokenCount": 40}],
                    "candidatesTokenCount": 30, "thoughtsTokenCount": 12}),
        )
        .unwrap();
        assert_eq!(
            (u.input, u.output, u.output_reasoning, u.input_image),
            (105, 42, 12, 40)
        );
        let c = u.classes();
        assert_eq!(c.get(Class::InputImageUncached), 40);
        assert_eq!(c.get(Class::InputText), 45);
        assert_eq!(c.get(Class::OutputText), 30);
    }

    #[test]
    fn bedrock_and_embeddings() {
        let (u, _) = normalize(
            Convention::BedrockConverse,
            &json!({"inputTokens": 10, "cacheReadInputTokens": 5, "cacheWriteInputTokens": 3,
                    "cacheDetails": [{"ttl": "1h", "inputTokens": 2}], "outputTokens": 7}),
        )
        .unwrap();
        assert_eq!((u.input, u.input_cache_write_1h, u.output), (18, 2, 7));
        let (e, _) = normalize(Convention::Embeddings, &json!({"input_tokens": 9})).unwrap();
        assert_eq!((e.input, e.output), (9, 0));
    }

    #[test]
    fn malformed_counts_are_errors() {
        let e = normalize(Convention::OpenAiChat, &json!({"prompt_tokens": "abc"})).unwrap_err();
        assert_eq!(e.field, "prompt_tokens");
        assert!(normalize(Convention::OpenAiChat, &json!({"prompt_tokens": -1})).is_err());
        assert_eq!(Usage::from_total(50, 20), Usage::estimated(20, 30));
    }

    proptest! {
        #[test]
        fn classes_partition_the_record_without_clamps(
            input in 0u64..1_000_000, read in 0u64..1_000_000, write in 0u64..1_000_000, w1h in 0u64..1_000_000,
            audio in 0u64..1_000_000, image in 0u64..1_000_000, video in 0u64..1_000_000,
            output in 0u64..1_000_000, reasoning in 0u64..1_000_000, oa in 0u64..1_000_000, oi in 0u64..1_000_000,
        ) {
            let u = Usage {
                input, input_cache_read: read, input_cache_write: write, input_cache_write_1h: w1h,
                input_audio: audio, input_image: image, input_video: video,
                output, output_reasoning: reasoning, output_audio: oa, output_image: oi,
            };
            let c = u.classes();
            let ins: u64 = c.iter().filter(|(k, _)| k.is_input()).map(|(_, v)| v).sum();
            let outs: u64 = c.iter().filter(|(k, _)| !k.is_input()).map(|(_, v)| v).sum();
            if read + write <= input && w1h <= write {
                prop_assert_eq!(ins, input);
            }
            if reasoning + oa + oi <= output {
                prop_assert_eq!(outs, output);
            }
            prop_assert!(c.get(Class::InputText) <= input);
        }
    }
}
