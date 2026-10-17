//! The messages of the endpoint-picker protocol, the external processing API (v3) over gRPC: the
//! `ProcessingRequest`s the gateway sends (request headers with the subset hint in the
//! [`SUBSET_NAMESPACE`] filter metadata, the request body, the response headers that name the
//! served endpoint) and what it reads of each `ProcessingResponse`: the destination, from a header
//! mutation or the [`LB_NAMESPACE`] dynamic metadata, and an immediate response. Only the fields the
//! protocol uses are written and read. The namespaces and the service name are the protocol's own
//! wire identifiers.

use bytes::{Buf, Bytes, BytesMut};

/// The header that carries the picker's destination, and its served counterpart.
pub(crate) const DESTINATION: &str = "x-gateway-destination-endpoint";
pub(crate) const SERVED: &str = "x-gateway-destination-endpoint-served";
pub(crate) const SUBSET: &str = "x-gateway-destination-endpoint-subset";
const SUBSET_NAMESPACE: &str = "envoy.lb.subset_hint";
const LB_NAMESPACE: &str = "envoy.lb";

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn len_field(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    varint(out, u64::from(field) << 3 | 2);
    varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn bool_field(out: &mut Vec<u8>, field: u32, v: bool) {
    varint(out, u64::from(field) << 3);
    varint(out, u64::from(v));
}

/// `HttpHeaders { headers: HeaderMap { headers: [HeaderValue { key, raw_value }] }, end_of_stream }`.
fn http_headers(headers: &[(String, Vec<u8>)], end_of_stream: bool) -> Vec<u8> {
    let mut map = Vec::new();
    for (k, v) in headers {
        let mut hv = Vec::new();
        len_field(&mut hv, 1, k.as_bytes());
        len_field(&mut hv, 3, v);
        len_field(&mut map, 1, &hv);
    }
    let mut out = Vec::new();
    len_field(&mut out, 1, &map);
    bool_field(&mut out, 3, end_of_stream);
    out
}

/// A `google.protobuf.Value` holding a string.
fn string_value(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    len_field(&mut v, 3, s.as_bytes());
    v
}

/// A `Struct` with one field, `key`, holding the encoded `Value` `value`.
fn single_struct(key: &str, value: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    len_field(&mut entry, 1, key.as_bytes());
    len_field(&mut entry, 2, value);
    let mut s = Vec::new();
    len_field(&mut s, 1, &entry);
    s
}

/// `ProcessingRequest { request_headers, metadata_context: { filter_metadata: { <SUBSET_NAMESPACE>:
/// { x-gateway-destination-endpoint-subset: [endpoints] } } } }`.
pub(crate) fn request_headers(headers: &[(String, Vec<u8>)], subset: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    len_field(&mut out, 2, &http_headers(headers, false));
    let mut list = Vec::new();
    for e in subset {
        len_field(&mut list, 1, &string_value(e));
    }
    let mut list_value = Vec::new();
    len_field(&mut list_value, 6, &list);
    let hint = single_struct(SUBSET, &list_value);
    let mut entry = Vec::new();
    len_field(&mut entry, 1, SUBSET_NAMESPACE.as_bytes());
    len_field(&mut entry, 2, &hint);
    let mut metadata = Vec::new();
    len_field(&mut metadata, 1, &entry);
    len_field(&mut out, 8, &metadata);
    out
}

/// `ProcessingRequest { request_body: HttpBody { body, end_of_stream: true } }`.
pub(crate) fn request_body(body: &[u8]) -> Vec<u8> {
    let mut http_body = Vec::new();
    len_field(&mut http_body, 1, body);
    bool_field(&mut http_body, 2, true);
    let mut out = Vec::new();
    len_field(&mut out, 4, &http_body);
    out
}

/// `ProcessingRequest { response_headers }`.
pub(crate) fn response_headers(headers: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    len_field(&mut out, 3, &http_headers(headers, false));
    out
}

/// A gRPC message frame: no compression, the length, the message.
pub(crate) fn frame(msg: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(msg.len() + 5);
    out.push(0);
    out.extend_from_slice(&u32::try_from(msg.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(msg);
    Bytes::from(out)
}

/// The gRPC frames of a response body, as they arrive.
#[derive(Default)]
pub(crate) struct Frames {
    buf: BytesMut,
}

impl Frames {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// The next whole message; `Err` for a compressed message, which the picker must not send.
    pub(crate) fn next(&mut self) -> Option<Result<Bytes, ()>> {
        let header = self.buf.get(..5)?;
        let compressed = header.first().copied() != Some(0);
        let len = header
            .get(1..5)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_be_bytes)
            .and_then(|l| usize::try_from(l).ok())?;
        if self.buf.len() < 5 + len {
            return None;
        }
        self.buf.advance(5);
        let msg = self.buf.split_to(len).freeze();
        Some(if compressed { Err(()) } else { Ok(msg) })
    }
}

/// One field of a message: a length-delimited value, or a scalar (not read).
enum Field<'a> {
    Bytes(&'a [u8]),
    Scalar,
}

fn read_varint(buf: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = buf.split_first()?;
        *buf = rest;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The fields of a message, in order; `None` when it is not well formed.
fn fields(mut buf: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let key = read_varint(&mut buf)?;
        let field = match key & 7 {
            0 => {
                read_varint(&mut buf)?;
                Field::Scalar
            }
            2 => {
                let len = usize::try_from(read_varint(&mut buf)?).ok()?;
                let (data, rest) = (buf.get(..len)?, buf.get(len..)?);
                buf = rest;
                Field::Bytes(data)
            }
            1 => {
                buf = buf.get(8..)?;
                Field::Scalar
            }
            5 => {
                buf = buf.get(4..)?;
                Field::Scalar
            }
            _ => return None,
        };
        out.push((key >> 3, field));
    }
    Some(out)
}

fn sub(buf: &[u8], field: u64) -> Option<Vec<&[u8]>> {
    Some(
        fields(buf)?
            .into_iter()
            .filter_map(|(n, f)| match f {
                Field::Bytes(b) if n == field => Some(b),
                _ => None,
            })
            .collect(),
    )
}

fn first(buf: &[u8], field: u64) -> Option<&[u8]> {
    sub(buf, field)?.into_iter().next()
}

/// The destination a `CommonResponse` sets in its header mutation.
fn mutation_destination(common: &[u8]) -> Option<String> {
    let mutation = first(common, 2)?;
    for option in sub(mutation, 1)? {
        let header = first(option, 1)?;
        let key = first(header, 1).map(String::from_utf8_lossy);
        if key.as_deref().map(str::to_ascii_lowercase).as_deref() != Some(DESTINATION) {
            continue;
        }
        let value = first(header, 3).or_else(|| first(header, 2)).unwrap_or_default();
        return Some(String::from_utf8_lossy(value).into_owned());
    }
    None
}

/// The value of `key` in a `Struct`.
fn struct_field<'a>(s: &'a [u8], key: &str) -> Option<&'a [u8]> {
    sub(s, 1)?
        .into_iter()
        .find(|entry| first(entry, 1) == Some(key.as_bytes()))
        .and_then(|entry| first(entry, 2))
}

/// The destination in the [`LB_NAMESPACE`] dynamic metadata.
fn metadata_destination(dynamic: &[u8]) -> Option<String> {
    let lb = struct_field(dynamic, LB_NAMESPACE)?;
    let inner = first(lb, 5)?;
    let value = struct_field(inner, DESTINATION)?;
    first(value, 3).map(|s| String::from_utf8_lossy(s).into_owned())
}

/// What a `ProcessingResponse` answered.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Answer {
    /// It answers the request headers.
    pub(crate) headers: bool,
    /// It answers the request body.
    pub(crate) body: bool,
    /// An immediate response: the picker ends the request itself (no endpoint).
    pub(crate) immediate: bool,
    pub(crate) destination: Option<String>,
}

/// Reads a `ProcessingResponse`; `None` when it is not well formed.
pub(crate) fn answer(msg: &[u8]) -> Option<Answer> {
    let mut a = Answer::default();
    for (n, f) in fields(msg)? {
        let Field::Bytes(b) = f else { continue };
        match n {
            1 | 3 => {
                if n == 1 {
                    a.headers = true;
                } else {
                    a.body = true;
                }
                if let Some(d) = first(b, 1).and_then(mutation_destination) {
                    a.destination = Some(d);
                }
            }
            7 => a.immediate = true,
            8 if a.destination.is_none() => a.destination = metadata_destination(b),
            _ => {}
        }
    }
    Some(a)
}

/// The entries of a destination or subset value: a comma list or a JSON array of strings, each
/// trimmed, empty ones left out.
pub(crate) fn entries(value: &str) -> Vec<String> {
    let v = value.trim();
    let parsed: Option<Vec<String>> = v
        .starts_with('[')
        .then(|| serde_json::from_str::<Vec<String>>(v).ok())
        .flatten();
    parsed
        .unwrap_or_else(|| v.split(',').map(str::to_owned).collect())
        .into_iter()
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn common_with(dest: &str) -> Vec<u8> {
        let mut hv = Vec::new();
        len_field(&mut hv, 1, DESTINATION.as_bytes());
        len_field(&mut hv, 3, dest.as_bytes());
        let mut opt = Vec::new();
        len_field(&mut opt, 1, &hv);
        let mut mutation = Vec::new();
        len_field(&mut mutation, 1, &opt);
        let mut common = Vec::new();
        len_field(&mut common, 2, &mutation);
        common
    }

    #[test]
    fn a_body_response_with_a_header_mutation() {
        let mut body_resp = Vec::new();
        len_field(&mut body_resp, 1, &common_with("10.0.0.2:8000"));
        let mut msg = Vec::new();
        len_field(&mut msg, 3, &body_resp);
        let a = answer(&msg).unwrap();
        assert!(a.body && !a.headers && !a.immediate);
        assert_eq!(a.destination.as_deref(), Some("10.0.0.2:8000"));
    }

    #[test]
    fn dynamic_metadata_and_immediate_responses() {
        let mut lb_value = Vec::new();
        len_field(
            &mut lb_value,
            5,
            &single_struct(DESTINATION, &string_value("10.0.0.3:9000")),
        );
        let dynamic = single_struct(LB_NAMESPACE, &lb_value);
        let mut msg = Vec::new();
        len_field(&mut msg, 3, &[]);
        len_field(&mut msg, 8, &dynamic);
        assert_eq!(
            answer(&msg).unwrap().destination.as_deref(),
            Some("10.0.0.3:9000")
        );
        let mut imm = Vec::new();
        len_field(&mut imm, 7, &[8, 0xf7, 0x03]);
        assert!(answer(&imm).unwrap().immediate);
        assert!(answer(&[0x1a, 0x05, 0x00]).is_none(), "a field cut short");
    }

    #[test]
    fn requests_round_trip_through_the_reader() {
        let msg = request_headers(
            &[(":path".into(), b"/v1/chat/completions".to_vec())],
            &["a:1".into(), "b:2".into()],
        );
        let f = fields(&msg).unwrap();
        assert_eq!(f.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![2, 8]);
        let metadata = first(&msg, 8).unwrap();
        let entry = first(metadata, 1).unwrap();
        assert_eq!(first(entry, 1), Some(SUBSET_NAMESPACE.as_bytes()));
        let subset = struct_field(first(entry, 2).unwrap(), SUBSET).unwrap();
        let list = first(subset, 6).unwrap();
        assert_eq!(sub(list, 1).unwrap().len(), 2);
        let body = request_body(b"{}");
        assert_eq!(fields(&body).unwrap().first().map(|(n, _)| *n), Some(4));
    }

    #[test]
    fn frames_split_and_join() {
        let mut f = Frames::default();
        let a = frame(b"abc");
        let b = frame(b"de");
        let mut both = a.to_vec();
        both.extend_from_slice(&b);
        f.push(both.get(..4).unwrap());
        assert!(f.next().is_none());
        f.push(both.get(4..).unwrap());
        assert_eq!(f.next(), Some(Ok(Bytes::from_static(b"abc"))));
        assert_eq!(f.next(), Some(Ok(Bytes::from_static(b"de"))));
        assert!(f.next().is_none());
    }

    #[test]
    fn destination_lists() {
        assert_eq!(
            entries(" 10.0.0.1:80 , ,10.0.0.2:80"),
            vec!["10.0.0.1:80", "10.0.0.2:80"]
        );
        assert_eq!(
            entries("[\"10.0.0.1:80\", \" \", \"10.0.0.2\"]"),
            vec!["10.0.0.1:80", "10.0.0.2"]
        );
        assert!(entries("").is_empty());
    }
}
