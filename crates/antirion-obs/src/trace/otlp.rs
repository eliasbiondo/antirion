//! OTLP `ExportTraceServiceRequest` encodings: the protobuf binary form (OTLP/HTTP protobuf and the
//! gRPC message) and the JSON form of OTLP/HTTP JSON.

use serde_json::{Value as Json, json};

use super::{SpanData, SpanKind, Status, Value};

/// The instrumentation scope every span is exported under.
const SCOPE: &str = "antirion";

fn kind_code(k: SpanKind) -> u64 {
    match k {
        SpanKind::Internal => 1,
        SpanKind::Server => 2,
        SpanKind::Client => 3,
    }
}

fn json_value(v: &Value) -> Json {
    match v {
        Value::Str(s) => json!({"stringValue": s}),
        Value::Int(n) => json!({"intValue": n.to_string()}),
        Value::F64(f) if f.is_finite() => json!({"doubleValue": f}),
        Value::F64(_) => json!({"stringValue": "NaN"}),
        Value::Bool(b) => json!({"boolValue": b}),
        Value::StrArray(items) => {
            json!({"arrayValue": {"values": items.iter().map(|s| json!({"stringValue": s})).collect::<Vec<_>>()}})
        }
    }
}

fn json_attrs(attrs: &[(String, Value)]) -> Json {
    Json::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({"key": k, "value": json_value(v)}))
            .collect(),
    )
}

/// The OTLP/HTTP JSON body for `spans` under one resource.
#[must_use]
pub fn encode_json(resource: &[(String, Value)], version: &str, spans: &[SpanData]) -> Vec<u8> {
    let spans: Vec<Json> = spans
        .iter()
        .map(|s| {
            let mut o = json!({
                "traceId": s.trace_id.hex(),
                "spanId": s.span_id.hex(),
                "name": s.name,
                "kind": kind_code(s.kind),
                "startTimeUnixNano": s.start_unix_nanos.to_string(),
                "endTimeUnixNano": s.end_unix_nanos.to_string(),
                "attributes": json_attrs(&s.attributes),
                "status": match &s.status {
                    Status::Unset => json!({"code": 0}),
                    Status::Error(m) => json!({"code": 2, "message": m}),
                },
            });
            if let (Some(p), Some(obj)) = (s.parent, o.as_object_mut()) {
                obj.insert("parentSpanId".into(), Json::String(p.hex()));
            }
            o
        })
        .collect();
    let body = json!({"resourceSpans": [{
        "resource": {"attributes": json_attrs(resource)},
        "scopeSpans": [{"scope": {"name": SCOPE, "version": version}, "spans": spans}],
    }]});
    serde_json::to_vec(&body).unwrap_or_default()
}

/// A minimal protobuf writer: length-delimited fields are written into a scratch buffer, then
/// prefixed with their length.
#[derive(Default)]
pub(crate) struct Pb(pub(crate) Vec<u8>);

impl Pb {
    pub(crate) fn varint(&mut self, mut n: u64) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    pub(crate) fn tag(&mut self, field: u32, wire: u8) {
        self.varint((u64::from(field) << 3) | u64::from(wire));
    }

    pub(crate) fn bytes(&mut self, field: u32, b: &[u8]) {
        self.tag(field, 2);
        self.varint(b.len() as u64);
        self.0.extend_from_slice(b);
    }

    pub(crate) fn string(&mut self, field: u32, s: &str) {
        if !s.is_empty() {
            self.bytes(field, s.as_bytes());
        }
    }

    pub(crate) fn uint(&mut self, field: u32, n: u64) {
        if n != 0 {
            self.tag(field, 0);
            self.varint(n);
        }
    }

    pub(crate) fn fixed64(&mut self, field: u32, n: u64) {
        self.tag(field, 1);
        self.0.extend_from_slice(&n.to_le_bytes());
    }

    pub(crate) fn message(&mut self, field: u32, f: impl FnOnce(&mut Pb)) {
        let mut inner = Pb::default();
        f(&mut inner);
        self.bytes(field, &inner.0);
    }
}

fn pb_any(p: &mut Pb, v: &Value) {
    match v {
        Value::Str(s) => p.bytes(1, s.as_bytes()),
        Value::Bool(b) => {
            p.tag(2, 0);
            p.varint(u64::from(*b));
        }
        Value::Int(n) => {
            p.tag(3, 0);
            p.varint(*n as u64);
        }
        Value::F64(f) => {
            p.tag(4, 1);
            p.0.extend_from_slice(&f.to_le_bytes());
        }
        Value::StrArray(items) => p.message(5, |a| {
            for s in items {
                a.message(1, |any| any.bytes(1, s.as_bytes()));
            }
        }),
    }
}

fn pb_attrs(p: &mut Pb, field: u32, attrs: &[(String, Value)]) {
    for (k, v) in attrs {
        p.message(field, |kv| {
            kv.string(1, k);
            kv.message(2, |any| pb_any(any, v));
        });
    }
}

/// The protobuf `ExportTraceServiceRequest` for `spans` under one resource.
#[must_use]
pub fn encode_proto(resource: &[(String, Value)], version: &str, spans: &[SpanData]) -> Vec<u8> {
    let mut req = Pb::default();
    req.message(1, |rs| {
        rs.message(1, |r| pb_attrs(r, 1, resource));
        rs.message(2, |ss| {
            ss.message(1, |scope| {
                scope.string(1, SCOPE);
                scope.string(2, version);
            });
            for s in spans {
                ss.message(2, |sp| {
                    sp.bytes(1, &s.trace_id.0);
                    sp.bytes(2, &s.span_id.0);
                    if let Some(parent) = s.parent {
                        sp.bytes(4, &parent.0);
                    }
                    sp.string(5, &s.name);
                    sp.uint(6, kind_code(s.kind));
                    sp.fixed64(7, s.start_unix_nanos);
                    sp.fixed64(8, s.end_unix_nanos);
                    pb_attrs(sp, 9, &s.attributes);
                    sp.message(15, |st| {
                        if let Status::Error(m) = &s.status {
                            st.string(2, m);
                            st.uint(3, 2);
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
    use super::super::{SpanId, TraceId};
    use super::*;

    fn span() -> SpanData {
        let mut s = SpanData::new(
            TraceId([1; 16]),
            Some(SpanId([2; 8])),
            "chat m".into(),
            SpanKind::Client,
        );
        s.span_id = SpanId([3; 8]);
        s.start_unix_nanos = 10;
        s.end_unix_nanos = 20;
        s.set("gen_ai.usage.input_tokens", 3i64);
        s.set(
            "gen_ai.response.finish_reasons",
            Value::StrArray(vec!["stop".into()]),
        );
        s.status = Status::Error("boom".into());
        s
    }

    #[test]
    fn json_has_the_otlp_shape() {
        let body: Json = serde_json::from_slice(&encode_json(
            &[("service.name".into(), "antirion".into())],
            "0.1.0",
            &[span()],
        ))
        .unwrap();
        let s = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["traceId"], "01010101010101010101010101010101");
        assert_eq!(s["parentSpanId"], "0202020202020202");
        assert_eq!(s["kind"], 3);
        assert_eq!(s["startTimeUnixNano"], "10");
        assert_eq!(s["attributes"][0]["value"]["intValue"], "3");
        assert_eq!(
            s["attributes"][1]["value"]["arrayValue"]["values"][0]["stringValue"],
            "stop"
        );
        assert_eq!(s["status"], json!({"code": 2, "message": "boom"}));
        assert_eq!(
            body["resourceSpans"][0]["resource"]["attributes"][0]["key"],
            "service.name"
        );
    }

    #[test]
    fn protobuf_varints_and_layout() {
        let mut p = Pb::default();
        p.varint(300);
        assert_eq!(p.0, [0xac, 0x02]);
        let bytes = encode_proto(&[], "v", &[span()]);
        assert_eq!(bytes.first(), Some(&0x0a));
        let trace = [0x0a, 0x10];
        assert!(bytes.windows(2).any(|w| w == trace));
        assert!(bytes.windows(4).any(|w| w == b"boom"));
    }
}
