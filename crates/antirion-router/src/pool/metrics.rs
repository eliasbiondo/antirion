//! The model-server metrics a pool reads: the names to look for, and a minimal reader of the
//! Prometheus text format that sums the samples of the wanted metrics and reads the labels of the
//! LoRA adapter state.

use serde_json::Value;

/// The metric names of a pool: for each figure, the names tried in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricNames {
    pub waiting: Vec<String>,
    pub running: Vec<String>,
    pub kv: Vec<String>,
    /// The LoRA adapter state: labels `running_lora_adapters`, `waiting_lora_adapters` and
    /// `max_lora` of the newest sample.
    pub lora: Vec<String>,
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

impl MetricNames {
    /// vLLM's names; older releases report the KV cache as `gpu_cache_usage_perc`.
    #[must_use]
    pub fn vllm() -> MetricNames {
        MetricNames {
            waiting: names(&["vllm:num_requests_waiting"]),
            running: names(&["vllm:num_requests_running"]),
            kv: names(&["vllm:kv_cache_usage_perc", "vllm:gpu_cache_usage_perc"]),
            lora: names(&["vllm:lora_requests_info"]),
        }
    }

    #[must_use]
    pub fn sglang() -> MetricNames {
        MetricNames {
            waiting: names(&["sglang:num_queue_reqs"]),
            running: names(&["sglang:num_running_reqs"]),
            kv: names(&["sglang:token_usage"]),
            lora: Vec::new(),
        }
    }

    /// Reads `metrics.names`: a preset name, or explicit names over the vLLM preset.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> MetricNames {
        match v {
            Some(Value::String(s)) if s == "sglang" => MetricNames::sglang(),
            Some(Value::Object(o)) => {
                let mut out = MetricNames::vllm();
                let one = |k: &str| o.get(k).and_then(Value::as_str).map(|s| vec![s.to_owned()]);
                if let Some(n) = one("waiting") {
                    out.waiting = n;
                }
                if let Some(n) = one("running") {
                    out.running = n;
                }
                if let Some(n) = one("kv") {
                    out.kv = n;
                }
                if let Some(n) = one("lora") {
                    out.lora = n;
                }
                out
            }
            _ => MetricNames::vllm(),
        }
    }
}

/// What one scrape read.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub waiting: f64,
    pub running: f64,
    /// KV-cache utilisation in [0, 1], when the server reports it.
    pub kv: Option<f64>,
    /// The server's base model (the `model_name` label of its queue metric).
    pub base_model: Option<String>,
    /// The LoRA adapter state, when the server reports it.
    pub lora: Option<Lora>,
    /// The model the pool's leaf sends to this endpoint: an adapter when it is not the base model.
    pub served: Option<String>,
}

/// A server's LoRA adapters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lora {
    pub running: Vec<String>,
    pub waiting: Vec<String>,
    pub max: Option<usize>,
}

/// How an endpoint suits a request for a LoRA adapter (`scheduler.lora`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LoraFit {
    /// The request is not for an adapter of this server, or its state is unknown.
    Unknown,
    /// The adapter is loaded.
    Running,
    /// The server has room for another adapter.
    Free,
    /// Every adapter slot is taken.
    Full,
}

impl Reading {
    /// How this endpoint suits a request for the model it is sent.
    #[must_use]
    pub fn lora_fit(&self) -> LoraFit {
        let (Some(lora), Some(adapter)) = (&self.lora, &self.served) else {
            return LoraFit::Unknown;
        };
        if self.base_model.as_ref() == Some(adapter) {
            return LoraFit::Unknown;
        }
        if lora.running.contains(adapter) {
            LoraFit::Running
        } else if lora.max.is_none_or(|m| lora.running.len() < m) {
            LoraFit::Free
        } else {
            LoraFit::Full
        }
    }
}

/// The labels of a sample's `{…}` part: `name="value"` pairs, escapes kept as written.
fn labels(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(eq) = rest.find('=') {
        let name = rest
            .get(..eq)
            .unwrap_or_default()
            .trim()
            .trim_start_matches(',')
            .trim();
        let after = rest.get(eq + 1..).unwrap_or_default();
        let Some(body) = after.strip_prefix('"') else {
            break;
        };
        let mut end = None;
        let mut escaped = false;
        for (i, ch) in body.char_indices() {
            match ch {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    end = Some(i);
                    break;
                }
                _ => escaped = false,
            }
        }
        let Some(end) = end else { break };
        out.push((name, body.get(..end).unwrap_or_default()));
        rest = body.get(end + 1..).unwrap_or_default();
    }
    out
}

/// The labels and value of each sample of metric `name`.
fn samples<'t>(text: &'t str, name: &str) -> Vec<(Vec<(&'t str, &'t str)>, f64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix(name) else {
            continue;
        };
        let (labels_text, after) = match rest.strip_prefix('{') {
            Some(l) => match l.rfind('}') {
                Some(end) => (
                    l.get(..end).unwrap_or_default(),
                    l.get(end + 1..).unwrap_or_default(),
                ),
                None => continue,
            },
            None if rest.starts_with([' ', '\t']) => ("", rest),
            None => continue,
        };
        if let Some(v) = after
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
        {
            out.push((labels(labels_text), v));
        }
    }
    out
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The LoRA state of the newest sample of the first of `names` that has one.
fn lora(text: &str, names: &[String]) -> Option<Lora> {
    names.iter().find_map(|n| {
        let newest = samples(text, n).into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
        let get = |k: &str| newest.0.iter().find(|(name, _)| *name == k).map(|(_, v)| *v);
        Some(Lora {
            running: get("running_lora_adapters").map(list).unwrap_or_default(),
            waiting: get("waiting_lora_adapters").map(list).unwrap_or_default(),
            max: get("max_lora").and_then(|m| m.trim().parse().ok()),
        })
    })
}

/// The `model_name` label of the first sample of the first of `names` that has one.
fn base_model(text: &str, names: &[String]) -> Option<String> {
    names.iter().find_map(|n| {
        samples(text, n).into_iter().find_map(|(l, _)| {
            l.iter()
                .find(|(k, _)| *k == "model_name")
                .map(|(_, v)| (*v).to_owned())
        })
    })
}

/// The sum of the samples of metric `name` in a Prometheus text exposition.
fn sum(text: &str, name: &str) -> Option<f64> {
    let mut total: Option<f64> = None;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(name) else {
            continue;
        };
        let after = if let Some(labels) = rest.strip_prefix('{') {
            match labels.find('}') {
                Some(end) => labels.get(end + 1..).unwrap_or_default(),
                None => continue,
            }
        } else if rest.starts_with([' ', '\t']) {
            rest
        } else {
            continue;
        };
        let Some(v) = after
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
        else {
            continue;
        };
        if v.is_finite() {
            total = Some(total.unwrap_or(0.0) + v);
        }
    }
    total
}

fn first(text: &str, names: &[String]) -> Option<f64> {
    names.iter().find_map(|n| sum(text, n))
}

/// Reads a scrape: `None` when the queue length is missing.
#[must_use]
pub fn read(text: &str, names: &MetricNames) -> Option<Reading> {
    Some(Reading {
        waiting: first(text, &names.waiting)?.max(0.0),
        running: first(text, &names.running).unwrap_or(0.0).max(0.0),
        kv: first(text, &names.kv).map(|k| k.clamp(0.0, 1.0)),
        base_model: base_model(text, &names.waiting),
        lora: lora(text, &names.lora),
        served: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_summed_and_names_matched_exactly() {
        let text = "# HELP vllm:num_requests_waiting x\n# TYPE vllm:num_requests_waiting gauge\n\
vllm:num_requests_waiting{model_name=\"a\"} 3.0\nvllm:num_requests_waiting{model_name=\"b\"} 2\n\
vllm:num_requests_waiting_total 99\nvllm:gpu_cache_usage_perc 0.95 1700000000\n";
        let r = read(text, &MetricNames::vllm()).unwrap();
        assert_eq!(r.waiting, 5.0);
        assert_eq!(r.running, 0.0);
        assert_eq!(r.kv, Some(0.95));
        assert!(read("vllm:num_requests_running 1\n", &MetricNames::vllm()).is_none());
    }

    #[test]
    fn lora_state_is_read_from_the_newest_sample() {
        let text = "vllm:num_requests_waiting{model_name=\"base\"} 0\n\
vllm:lora_requests_info{running_lora_adapters=\"old\",waiting_lora_adapters=\"\",max_lora=\"2\"} 1.0e9\n\
vllm:lora_requests_info{max_lora=\"2\",running_lora_adapters=\"a, b\",waiting_lora_adapters=\"c\"} 1.7e9\n";
        let mut r = read(text, &MetricNames::vllm()).unwrap();
        assert_eq!(r.base_model.as_deref(), Some("base"));
        assert_eq!(
            r.lora,
            Some(Lora {
                running: vec!["a".into(), "b".into()],
                waiting: vec!["c".into()],
                max: Some(2)
            })
        );
        assert_eq!(r.lora_fit(), LoraFit::Unknown, "no served model yet");
        r.served = Some("a".into());
        assert_eq!(r.lora_fit(), LoraFit::Running);
        r.served = Some("c".into());
        assert_eq!(r.lora_fit(), LoraFit::Full);
        r.lora.as_mut().unwrap().max = Some(3);
        assert_eq!(r.lora_fit(), LoraFit::Free);
        r.served = Some("base".into());
        assert_eq!(r.lora_fit(), LoraFit::Unknown, "the base model is no adapter");
    }
}
