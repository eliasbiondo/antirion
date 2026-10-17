//! The probes on the admin listener, and its answers while the node waits for its store.

use antirion_core::RequestId;
use antirion_runtime::Runtime;
use antirion_runtime::probes::Waiting;
use axum::body::Body;
use axum::response::Response;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method, StatusCode};
use serde_json::Value;

use crate::error::AdminError;

pub(crate) fn probe(rt: &Runtime, method: &Method, live: bool) -> Result<Response, AdminError> {
    if method != Method::GET && method != Method::HEAD {
        return Err(AdminError::method_not_allowed("GET, HEAD"));
    }
    let answer = if live {
        antirion_runtime::probes::livez(rt)
    } else {
        antirion_runtime::probes::readyz(rt)
    };
    Ok(render(method, answer))
}

fn render(method: &Method, (status, body): (StatusCode, Value)) -> Response {
    let bytes = if method == Method::HEAD {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap_or_default()
    };
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The admin listener's answer while the node waits for its store at startup: the probes, and
/// 503 `store_unavailable` for every other call.
#[must_use]
pub fn waiting(w: &Waiting, method: &Method, path: &str) -> Response {
    let live = match path {
        "/-/livez" => true,
        "/-/readyz" => false,
        _ => return AdminError::store_unavailable().into_response(&RequestId::generate()),
    };
    if method != Method::GET && method != Method::HEAD {
        return AdminError::method_not_allowed("GET, HEAD").into_response(&RequestId::generate());
    }
    render(method, w.probe(live))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting_node() -> Waiting {
        Waiting {
            node_id: "n1".into(),
            version: "1.0.0".into(),
        }
    }

    #[test]
    fn a_waiting_node_answers_probes_and_refuses_the_api() {
        let w = waiting_node();
        assert_eq!(waiting(&w, &Method::GET, "/-/livez").status(), StatusCode::OK);
        assert_eq!(
            waiting(&w, &Method::HEAD, "/-/readyz").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            waiting(&w, &Method::POST, "/-/livez").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        let api = waiting(&w, &Method::GET, "/admin/v1/keys");
        assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.headers().get(http::header::RETRY_AFTER).unwrap(), "1");
    }
}
