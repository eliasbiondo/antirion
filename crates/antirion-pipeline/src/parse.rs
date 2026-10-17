//! Stage `parse`: the content type, the content encoding, the body read under the size limit, and
//! the endpoint's schema checks. Nothing here depends on the model.

use std::borrow::Cow;
use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::{
    BodyFacts, Endpoint, ModelField, ValidateOpts, Violation, ViolationKind, body_facts, validate_json,
};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_json::{JsonErrorKind, JsonLimits, Skim, skim};
use antirion_wire::decode::{Coding, DecodeError, decode_all, parse_coding};
use antirion_wire::multipart::{self, Form};
use bytes::Bytes;
use http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use http::request::Parts;
use hyper::body::Incoming;
use serde_json::Value;

use crate::body::{self, BodyError};
use crate::ctx::Ctx;
use crate::reasons;

/// A parsed request body.
pub(crate) enum Parsed {
    Json {
        raw: Bytes,
        skim: Skim,
        facts: BodyFacts,
        /// The body as a value, parsed once for every stage that reads it; `None` when the value
        /// parser refuses what the skim accepted (nesting past its own limit).
        doc: Option<Arc<Value>>,
        /// The provider the body's gateway file ids belong to: only its deployments may serve it.
        pinned: Option<String>,
    },
    Multipart {
        /// The body, or its head when the rest streams.
        raw: Bytes,
        /// The parts read whole.
        form: Form,
        model: ModelField,
        stream: bool,
        /// The rest of a body larger than the read-ahead.
        rest: Option<crate::upload::Upload>,
    },
}

impl Parsed {
    /// A JSON body, its skim and its value.
    pub(crate) fn json(raw: Bytes, skim: Skim, doc: Option<Value>) -> Parsed {
        let facts = body_facts(&raw, &skim);
        Parsed::Json {
            raw,
            skim,
            facts,
            doc: doc.map(Arc::new),
            pinned: None,
        }
    }

    /// Confines the request to the deployments of `provider`.
    pub(crate) fn pin(&mut self, provider: Option<String>) {
        if let Parsed::Json { pinned, .. } = self {
            *pinned = provider;
        }
    }

    /// The provider the request is confined to.
    pub(crate) fn pinned_provider(&self) -> Option<&str> {
        match self {
            Parsed::Json { pinned, .. } => pinned.as_deref(),
            Parsed::Multipart { .. } => None,
        }
    }

    /// The JSON body as a value.
    pub(crate) fn doc(&self) -> Option<&Value> {
        match self {
            Parsed::Json { doc, .. } => doc.as_deref(),
            Parsed::Multipart { .. } => None,
        }
    }

    /// The JSON body as a shared value, kept as it was when the body is rewritten later.
    pub(crate) fn shared_doc(&self) -> Option<Arc<Value>> {
        match self {
            Parsed::Json { doc, .. } => doc.clone(),
            Parsed::Multipart { .. } => None,
        }
    }

    /// A top-level member of a JSON body.
    pub(crate) fn member(&self, name: &str) -> Option<Cow<'_, Value>> {
        let Parsed::Json { raw, skim, doc, .. } = self else {
            return None;
        };
        member(raw, skim, doc.as_deref(), name)
    }

    pub(crate) fn model(&self) -> &ModelField {
        match self {
            Parsed::Json { facts, .. } => &facts.model,
            Parsed::Multipart { model, .. } => model,
        }
    }

    pub(crate) fn stream(&self) -> bool {
        match self {
            Parsed::Json { facts, .. } => facts.stream,
            Parsed::Multipart { stream, .. } => *stream,
        }
    }
}

/// Endpoints whose body is `multipart/form-data`.
pub(crate) fn is_multipart(endpoint: &Endpoint) -> bool {
    matches!(
        endpoint.upstream_path,
        Some("/audio/transcriptions" | "/audio/translations" | "/images/edits" | "/images/variations")
    )
}

/// The file parts of a multipart endpoint (any one of them), and its other required parts.
fn required_parts(endpoint: &Endpoint) -> (&'static [&'static str], &'static [&'static str]) {
    match endpoint.upstream_path {
        Some("/images/edits") => (&["image", "image[]"], &["prompt"]),
        Some("/images/variations") => (&["image"], &[]),
        _ => (&["file"], &[]),
    }
}

fn unsupported_type(expected: &str) -> GatewayError {
    GatewayError::with_reason(
        reasons::UNSUPPORTED_CONTENT_TYPE,
        Stage::Parse,
        Message::built(format!("The request Content-Type must be {expected}.")),
    )
}

/// The lowercase `charset` parameter of a content type.
fn charset(ct: &str) -> Option<String> {
    ct.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k.trim().eq_ignore_ascii_case("charset")).then(|| v.trim().trim_matches('"').to_ascii_lowercase())
    })
}

fn is_json_type(ct: &str) -> bool {
    let essence = ct
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "application/json" || (essence.starts_with("application/") && essence.ends_with("+json"))
}

pub(crate) async fn run(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
) -> Result<Parsed, GatewayError> {
    let content_type = parts.headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());
    let multipart_body = is_multipart(endpoint);
    let boundary = if multipart_body {
        match content_type.and_then(multipart::form_data_boundary) {
            Some(b) => Some(b),
            None => return Err(unsupported_type("multipart/form-data with a boundary")),
        }
    } else {
        if content_type.is_some_and(|ct| !is_json_type(ct)) {
            return Err(unsupported_type("application/json"));
        }
        if content_type.is_none() && cx.origin.is_some() {
            return Err(unsupported_type("application/json"));
        }
        if content_type
            .and_then(charset)
            .is_some_and(|c| c != "utf-8" && c != "utf8")
        {
            return Err(GatewayError::with_reason(
                reasons::UNSUPPORTED_CHARSET,
                Stage::Parse,
                Message::Fixed("JSON request bodies must be UTF-8."),
            ));
        }
        None
    };
    if let Some(boundary) = boundary {
        let (files, others) = required_parts(endpoint);
        let head =
            crate::upload::read(cx, parts, incoming, &boundary, &crate::upload::Wanted { files }).await?;
        return parse_multipart(head, files, others);
    }
    let limit = cx.snap.security.max_body;
    let raw = read(cx, parts, incoming, limit).await?;
    let skimmed = skim(
        &raw,
        &JsonLimits {
            max_depth: cx.snap.security.max_json_depth,
        },
    )
    .map_err(json_error)?;
    if !skimmed.top_is_object {
        return Err(GatewayError::with_reason(
            hygiene::INVALID_JSON,
            Stage::Parse,
            Message::Fixed("The request body must be a JSON object."),
        ));
    }
    let doc: Option<Value> = serde_json::from_slice(&raw).ok();
    let opts = ValidateOpts {
        images_max_n: images_max_n(cx),
        templated: true,
    };
    match validate_json(endpoint, &raw, &skimmed, doc.as_ref(), &opts) {
        Ok(()) => {}
        Err(v) if v.path.ends_with(".image_url.url") && v.kind == ViolationKind::Value => {
            return Err(empty_image(v));
        }
        Err(v) => return Err(violation(v)),
    }
    crate::account::check_caps(cx, endpoint, doc.as_ref())?;
    check_message_count(&raw, &skimmed, doc.as_ref(), cx.snap.security.max_messages)?;
    Ok(Parsed::json(raw, skimmed, doc))
}

/// Validates a templated body again once the input stage ran, without the allowance a `template`
/// object gave it: a body no template rendered still needs its messages.
pub(crate) fn revalidate(cx: &Ctx, endpoint: &Endpoint, body: &Value) -> Result<(), GatewayError> {
    let Ok(raw) = serde_json::to_vec(body) else {
        return Ok(());
    };
    let Ok(skimmed) = skim(&raw, &JsonLimits::default()) else {
        return Ok(());
    };
    let opts = ValidateOpts {
        images_max_n: images_max_n(cx),
        templated: false,
    };
    validate_json(endpoint, &raw, &skimmed, Some(body), &opts).map_err(violation)
}

/// A top-level member: from the parsed value, or parsed from its span when there is none.
fn member<'a>(raw: &[u8], skim: &Skim, doc: Option<&'a Value>, name: &str) -> Option<Cow<'a, Value>> {
    if let Some(d) = doc {
        return d.get(name).map(Cow::Borrowed);
    }
    let m = skim.member(name)?;
    serde_json::from_slice(raw.get(m.value.clone())?)
        .ok()
        .map(Cow::Owned)
}

/// The 400 for a Chat image part without a usable URL.
fn empty_image(v: Violation) -> GatewayError {
    crate::xlate::invalid(crate::xlate::Invalid {
        message: format!("{} must be a non-empty URL.", v.path),
        param: v.path,
    })
}

/// Refuses a message (or input item) array longer than `security.limits.max_messages`.
fn check_message_count(raw: &[u8], skim: &Skim, doc: Option<&Value>, max: usize) -> Result<(), GatewayError> {
    for field in ["messages", "input", "contents"] {
        let value = member(raw, skim, doc, field);
        let Some(Value::Array(items)) = value.as_deref() else {
            continue;
        };
        if items.len() > max {
            return Err(GatewayError::with_reason(
                reasons::TOO_MANY_MESSAGES,
                Stage::Parse,
                Message::built(format!("{field} has more than {max} items.")),
            )
            .param(field));
        }
    }
    Ok(())
}

fn images_max_n(cx: &Ctx) -> u64 {
    cx.snap
        .config
        .doc
        .get("api")
        .and_then(|a| a.get("images"))
        .and_then(|i| i.get("max_n"))
        .and_then(Value::as_u64)
        .unwrap_or(10)
}

/// The 400 for a schema violation, with the reason and OpenAI code of its kind.
fn violation(v: Violation) -> GatewayError {
    let (reason, code) = match v.kind {
        ViolationKind::Missing => (reasons::MISSING_REQUIRED_PARAMETER, "missing_required_parameter"),
        ViolationKind::Type => (reasons::INVALID_TYPE, "invalid_type"),
        ViolationKind::Value => (reasons::INVALID_VALUE, "invalid_value"),
    };
    GatewayError::with_reason(reason, Stage::Parse, Message::built(v.message))
        .param(v.path)
        .openai_code(code)
}

/// Reads the body, decoded from its `Content-Encoding`, refusing it once its decoded size exceeds
/// `limit`.
pub(crate) async fn read(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    limit: u64,
) -> Result<Bytes, GatewayError> {
    let fields = parts.headers.get_all(CONTENT_ENCODING);
    let Ok(coding) = parse_coding(fields.iter().map(|v| v.as_bytes())) else {
        return Err(GatewayError::with_reason(
            hygiene::UNSUPPORTED_CONTENT_ENCODING,
            Stage::Parse,
            Message::Fixed("The Content-Encoding of the request is not supported."),
        ));
    };
    let gap = cx.rt.statics.http.body_read;
    let result = if coding == Coding::Identity {
        let declared = parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        body::read(incoming, limit, gap, declared).await
    } else {
        match decode_all(coding, body::stream(incoming, gap), limit).await {
            Ok(b) => Ok(b),
            Err(DecodeError::TooLarge) => Err(BodyError::TooLarge),
            Err(DecodeError::Source(e)) => Err(e
                .downcast_ref::<BodyError>()
                .copied()
                .unwrap_or(BodyError::Framing)),
            Err(DecodeError::Corrupt) => {
                return Err(GatewayError::with_reason(
                    crate::reasons::INVALID_COMPRESSED_BODY,
                    Stage::Parse,
                    Message::Fixed("The request body could not be decoded with its Content-Encoding."),
                ));
            }
        }
    };
    let raw = match result {
        Ok(b) => b,
        Err(BodyError::TooLarge) => return Err(crate::accept::too_large(Stage::Parse)),
        Err(BodyError::Framing) => {
            return Err(GatewayError::with_reason(
                hygiene::FRAMING_ERROR,
                Stage::Parse,
                Message::Fixed("The request body is not framed correctly."),
            )
            .closing());
        }
        Err(BodyError::Timeout) => {
            return Err(GatewayError::with_reason(
                reasons::BODY_READ_TIMEOUT,
                Stage::Parse,
                Message::Fixed("The request body did not arrive in time."),
            )
            .closing());
        }
    };
    cx.body_read = true;
    Ok(raw)
}

fn parse_multipart(
    head: crate::upload::Head,
    files: &[&str],
    others: &[&str],
) -> Result<Parsed, GatewayError> {
    let crate::upload::Head { raw, scanner, rest } = head;
    if rest.is_none() && !scanner.closed() {
        return Err(crate::upload::malformed());
    }
    let form = scanner.form();
    let present = |name: &str| form.parts().iter().any(|p| p.name.as_deref() == Some(name));
    let open = scanner.open_part().and_then(|p| p.name);
    let file_seen = files.iter().any(|f| present(f) || open.as_deref() == Some(*f));
    let missing = if file_seen {
        others.iter().find(|n| !present(n)).copied()
    } else {
        files.first().copied()
    };
    if let Some(name) = missing {
        return Err(violation(Violation {
            path: name.to_owned(),
            kind: ViolationKind::Missing,
            message: format!("{name}: a {name} part is required."),
        }));
    }
    let text = |name: &str| {
        form.field(&raw, name)
            .map(|v| String::from_utf8_lossy(v).into_owned())
    };
    let model = match text("model") {
        None => ModelField::Missing,
        Some(m) if m.is_empty() => ModelField::Missing,
        Some(m) => ModelField::Name(m),
    };
    let stream = text("stream").is_some_and(|s| s.trim().eq_ignore_ascii_case("true"));
    Ok(Parsed::Multipart {
        raw,
        form,
        model,
        stream,
        rest,
    })
}

/// A JSON path in the style of request field names: `messages[0].role`.
fn field_path(path: &[antirion_json::PathSeg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            antirion_json::PathSeg::Key(k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            antirion_json::PathSeg::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

pub(crate) fn json_error(e: antirion_json::JsonError) -> GatewayError {
    let at = field_path(&e.path);
    let (reason, message) = match e.kind {
        JsonErrorKind::Empty => (hygiene::EMPTY_BODY, String::from("The request body is empty.")),
        JsonErrorKind::TooDeep => (
            hygiene::JSON_TOO_DEEP,
            format!("The request body is nested too deeply ({e})."),
        ),
        JsonErrorKind::DuplicateKey => (
            hygiene::DUPLICATE_KEY,
            format!("The request body repeats a member name ({e})."),
        ),
        _ => (
            hygiene::INVALID_JSON,
            format!("The request body is not valid JSON: {e}."),
        ),
    };
    let err = GatewayError::with_reason(reason, Stage::Parse, Message::built(message));
    if at.is_empty() || reason != hygiene::DUPLICATE_KEY {
        err
    } else {
        err.param(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_types() {
        assert!(is_json_type("application/json"));
        assert!(is_json_type("Application/JSON; charset=utf-8"));
        assert!(is_json_type("application/vnd.acme+json"));
        assert!(!is_json_type("text/plain"));
        assert!(!is_json_type("application/xml"));
        assert!(!is_json_type("multipart/form-data; boundary=x"));
    }
}
