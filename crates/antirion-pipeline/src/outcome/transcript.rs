//! The output stage of a transcription answer, read whole: the `text` of a JSON answer, or the
//! whole body of a text-format answer (`text`, `srt`, `vtt`) as one content segment.

use antirion_guardrails::segments::Dialect;
use bytes::Bytes;
use serde_json::{Value, json};

use super::Attempt;
use antirion_resilience::Failure;

/// Whether the answer has output guardrails to run.
pub(super) fn applies(a: &Attempt<'_>) -> bool {
    a.plan
        .guard
        .as_deref()
        .is_some_and(|g| g.dialect == Some(Dialect::Transcription) && !g.attached.output.is_empty())
}

/// The answer after its output stage.
pub(super) async fn checked(a: &Attempt<'_>, status: u16, raw: Bytes, json: bool) -> Result<Bytes, Failure> {
    let Some(g) = a.plan.guard.as_deref().filter(|_| applies(a)) else {
        return Ok(raw);
    };
    let mut body = if json {
        match serde_json::from_slice::<Value>(&raw) {
            Ok(v) => v,
            Err(_) => return Ok(raw),
        }
    } else {
        json!({"text": String::from_utf8_lossy(&raw)})
    };
    let changed = Box::pin(crate::guard::output(a.cx, g, &mut body, a.attempts))
        .await
        .map_err(|e| crate::streamed::guarded(status, e))?;
    if !changed {
        return Ok(raw);
    }
    Ok(if json {
        Bytes::from(serde_json::to_vec(&body).unwrap_or_default())
    } else {
        Bytes::from(
            body.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    })
}
