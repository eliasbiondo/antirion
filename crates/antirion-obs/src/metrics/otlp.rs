//! OTLP `ExportMetricsServiceRequest` encodings (JSON and protobuf) of the registry's cumulative
//! state, with the OpenTelemetry GenAI names for the families that have one: the request, token,
//! time-to-first-token, time-per-output-token and upstream-call families, with GenAI attribute
//! names for provider, model, operation and token type. Every other family keeps its Prometheus name
//! with `_` as `.` and without `_total`.

use serde_json::{Value as Json, json};

use super::catalog::Kind;
use crate::trace::otlp::Pb;

/// The cumulative state of one series.
#[derive(Clone, Debug, PartialEq)]
pub enum Point {
    Value(f64),
    Histogram { counts: Vec<u64>, sum: f64, count: u64 },
}

/// One family's series, as exported.
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyData {
    pub name: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub labels: Vec<String>,
    pub buckets: Vec<f64>,
    pub series: Vec<(Vec<String>, Point)>,
}

/// What the encodings need besides the families.
pub struct Export<'a> {
    /// Resource attributes (`service.name`, …).
    pub resource: &'a [(String, String)],
    pub version: &'a str,
    pub start_unix_nano: u64,
    pub now_unix_nano: u64,
    /// The provider type of a provider name, for `gen_ai.provider.name`.
    pub provider_type: &'a (dyn Fn(&str) -> Option<String> + Sync),
}

impl std::fmt::Debug for Export<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// The GenAI instrument name of a family, when it has one.
#[must_use]
pub fn genai_name(family: &str) -> Option<&'static str> {
    Some(match family {
        "antirion_token_usage" => "gen_ai.client.token.usage",
        "antirion_request_duration_seconds" => "gen_ai.server.request.duration",
        "antirion_time_to_first_token_seconds" => "gen_ai.server.time_to_first_token",
        "antirion_time_per_output_token_seconds" => "gen_ai.server.time_per_output_token",
        "antirion_upstream_attempt_duration_seconds" => "gen_ai.client.operation.duration",
        _ => return None,
    })
}

/// The exported name of a family.
#[must_use]
pub fn export_name(family: &str) -> String {
    genai_name(family).map_or_else(
        || family.strip_suffix("_total").unwrap_or(family).replace('_', "."),
        str::to_owned,
    )
}

/// `gen_ai.provider.name` of a provider type.
#[must_use]
pub fn genai_provider(provider_type: &str) -> String {
    match provider_type {
        "azure_openai" => "azure.ai.openai",
        "bedrock" => "aws.bedrock",
        "vertex" => "gcp.vertex_ai",
        "gemini" => "gcp.gemini",
        "mistral" => "mistral_ai",
        "xai" => "x_ai",
        "ibm_watsonx" => "ibm.watsonx.ai",
        other => other,
    }
    .to_owned()
}

/// `gen_ai.operation.name` of an operation id.
#[must_use]
pub fn genai_operation(operation: &str) -> String {
    match operation {
        "chat_completions" | "messages" | "responses" | "converse" | "converse_stream" => "chat",
        "generate_content" | "stream_generate_content" => "generate_content",
        "completions" => "text_completion",
        "embeddings" => "embeddings",
        other => other,
    }
    .to_owned()
}

/// The attributes of one series: GenAI names on GenAI families, the label names elsewhere.
fn attributes(f: &FamilyData, values: &[String], x: &Export<'_>) -> Vec<(String, String)> {
    let genai = genai_name(f.name).is_some();
    let mut out = Vec::new();
    for (label, value) in f.labels.iter().zip(values) {
        if !genai {
            out.push((label.clone(), value.clone()));
            continue;
        }
        match label.as_str() {
            "provider" => out.push((
                "gen_ai.provider.name".to_owned(),
                genai_provider(&(x.provider_type)(value).unwrap_or_else(|| value.clone())),
            )),
            "upstream_model" => out.push(("gen_ai.request.model".to_owned(), value.clone())),
            "operation" => out.push(("gen_ai.operation.name".to_owned(), genai_operation(value))),
            "token_type" => out.push(("gen_ai.token.type".to_owned(), value.clone())),
            "outcome" if value != "success" => out.push(("error.type".to_owned(), value.clone())),
            "outcome" => {}
            other => out.push((other.to_owned(), value.clone())),
        }
    }
    out
}

fn unit(f: &FamilyData) -> &'static str {
    if f.name.ends_with("_seconds") {
        "s"
    } else if f.name.contains("token") && f.kind == Kind::Histogram {
        "{token}"
    } else {
        ""
    }
}

fn json_attrs(attrs: &[(String, String)]) -> Json {
    Json::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({"key": k, "value": {"stringValue": v}}))
            .collect(),
    )
}

/// The OTLP/HTTP JSON body.
#[must_use]
pub fn encode_json(families: &[FamilyData], x: &Export<'_>) -> Vec<u8> {
    let start = x.start_unix_nano.to_string();
    let now = x.now_unix_nano.to_string();
    let metrics: Vec<Json> = families
        .iter()
        .filter(|f| !f.series.is_empty())
        .map(|f| {
            let points: Vec<Json> = f
                .series
                .iter()
                .map(|(values, p)| {
                    let attrs = json_attrs(&attributes(f, values, x));
                    match p {
                        Point::Value(v) => json!({"attributes": attrs, "startTimeUnixNano": start,
                                                  "timeUnixNano": now, "asDouble": v}),
                        Point::Histogram { counts, sum, count } => json!({
                            "attributes": attrs, "startTimeUnixNano": start, "timeUnixNano": now,
                            "count": count.to_string(), "sum": sum,
                            "bucketCounts": counts.iter().map(u64::to_string).collect::<Vec<_>>(),
                            "explicitBounds": f.buckets,
                        }),
                    }
                })
                .collect();
            let mut m = json!({"name": export_name(f.name), "description": f.help, "unit": unit(f)});
            let data = match f.kind {
                Kind::Counter => (
                    "sum",
                    json!({"dataPoints": points, "aggregationTemporality": 2, "isMonotonic": true}),
                ),
                Kind::Gauge => ("gauge", json!({"dataPoints": points})),
                Kind::Histogram => (
                    "histogram",
                    json!({"dataPoints": points, "aggregationTemporality": 2}),
                ),
            };
            if let Some(obj) = m.as_object_mut() {
                obj.insert(data.0.to_owned(), data.1);
            }
            m
        })
        .collect();
    let body = json!({"resourceMetrics": [{
        "resource": {"attributes": json_attrs(x.resource)},
        "scopeMetrics": [{"scope": {"name": "antirion", "version": x.version}, "metrics": metrics}],
    }]});
    serde_json::to_vec(&body).unwrap_or_default()
}

fn pb_attrs(p: &mut Pb, field: u32, attrs: &[(String, String)]) {
    for (k, v) in attrs {
        p.message(field, |kv| {
            kv.string(1, k);
            kv.message(2, |any| any.bytes(1, v.as_bytes()));
        });
    }
}

fn pb_double(p: &mut Pb, field: u32, v: f64) {
    p.fixed64(field, v.to_bits());
}

/// The protobuf body (OTLP/HTTP protobuf and the gRPC message).
#[must_use]
pub fn encode_proto(families: &[FamilyData], x: &Export<'_>) -> Vec<u8> {
    let mut req = Pb::default();
    req.message(1, |rm| {
        rm.message(1, |res| pb_attrs(res, 1, x.resource));
        rm.message(2, |sm| {
            sm.message(1, |scope| {
                scope.string(1, "antirion");
                scope.string(2, x.version);
            });
            for f in families.iter().filter(|f| !f.series.is_empty()) {
                sm.message(2, |m| {
                    m.string(1, &export_name(f.name));
                    m.string(2, f.help);
                    m.string(3, unit(f));
                    let field = match f.kind {
                        Kind::Gauge => 5,
                        Kind::Counter => 7,
                        Kind::Histogram => 9,
                    };
                    m.message(field, |data| {
                        for (values, p) in &f.series {
                            let attrs = attributes(f, values, x);
                            data.message(1, |dp| match p {
                                Point::Value(v) => {
                                    dp.fixed64(2, x.start_unix_nano);
                                    dp.fixed64(3, x.now_unix_nano);
                                    pb_double(dp, 4, *v);
                                    pb_attrs(dp, 7, &attrs);
                                }
                                Point::Histogram { counts, sum, count } => {
                                    dp.fixed64(2, x.start_unix_nano);
                                    dp.fixed64(3, x.now_unix_nano);
                                    dp.fixed64(4, *count);
                                    pb_double(dp, 5, *sum);
                                    let mut packed = Pb::default();
                                    for c in counts {
                                        packed.0.extend_from_slice(&c.to_le_bytes());
                                    }
                                    dp.bytes(6, &packed.0);
                                    let mut bounds = Pb::default();
                                    for b in &f.buckets {
                                        bounds.0.extend_from_slice(&b.to_le_bytes());
                                    }
                                    dp.bytes(7, &bounds.0);
                                    pb_attrs(dp, 9, &attrs);
                                }
                            });
                        }
                        if f.kind != Kind::Gauge {
                            data.uint(2, 2);
                        }
                        if f.kind == Kind::Counter {
                            data.uint(3, 1);
                        }
                    });
                });
            }
        });
    });
    req.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn families() -> Vec<FamilyData> {
        vec![
            FamilyData {
                name: "antirion_token_usage",
                help: "Tokens per request",
                kind: Kind::Histogram,
                labels: vec!["model".into(), "provider".into(), "token_type".into()],
                buckets: vec![1.0, 4.0],
                series: vec![(
                    vec!["m".into(), "openai_main".into(), "input".into()],
                    Point::Histogram {
                        counts: vec![0, 1, 0],
                        sum: 3.0,
                        count: 1,
                    },
                )],
            },
            FamilyData {
                name: "antirion_cache_lookups_total",
                help: "Response cache lookups",
                kind: Kind::Counter,
                labels: vec!["layer".into(), "result".into()],
                buckets: Vec::new(),
                series: vec![(vec!["exact".into(), "hit".into()], Point::Value(2.0))],
            },
        ]
    }

    fn export<'a>(
        resource: &'a [(String, String)],
        pt: &'a (dyn Fn(&str) -> Option<String> + Sync),
    ) -> Export<'a> {
        Export {
            resource,
            version: "1",
            start_unix_nano: 1,
            now_unix_nano: 2,
            provider_type: pt,
        }
    }

    #[test]
    fn genai_names_and_attributes() {
        let pt = |p: &str| (p == "openai_main").then(|| "azure_openai".to_owned());
        let resource = vec![("service.name".to_owned(), "antirion".to_owned())];
        let body: Json = serde_json::from_slice(&encode_json(&families(), &export(&resource, &pt))).unwrap();
        let metrics = &body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"];
        assert_eq!(metrics[0]["name"], "gen_ai.client.token.usage");
        assert_eq!(metrics[0]["unit"], "{token}");
        let attrs = metrics[0]["histogram"]["dataPoints"][0]["attributes"].to_string();
        assert!(
            attrs.contains("gen_ai.token.type") && attrs.contains("azure.ai.openai"),
            "{attrs}"
        );
        assert_eq!(metrics[1]["name"], "antirion.cache.lookups");
        assert_eq!(metrics[1]["sum"]["isMonotonic"], true);
        assert!(!encode_proto(&families(), &export(&resource, &pt)).is_empty());
    }

    #[test]
    fn operations_and_providers_follow_the_mapping() {
        assert_eq!(genai_operation("chat_completions"), "chat");
        assert_eq!(genai_operation("completions"), "text_completion");
        assert_eq!(genai_operation("images"), "images");
        assert_eq!(genai_provider("vertex"), "gcp.vertex_ai");
        assert_eq!(genai_provider("groq"), "groq");
        assert_eq!(
            export_name("antirion_requests_in_flight"),
            "antirion.requests.in.flight"
        );
    }
}
