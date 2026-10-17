//! The admin console's static assets, embedded at build time, and the rules for serving them: which
//! file a path names in a static export, its content type, its cache policy and the security headers
//! every console response carries.
//!
//! The assets are the frontend's static export (`console/out`): one HTML file per route, either
//! `route/index.html` or `route.html`, hashed assets under `_next/static/`, and an optional `404.html`.
//! A build without the export embeds a placeholder page instead.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

use std::collections::HashMap;
use std::sync::OnceLock;

use http::{HeaderValue, Method, Response, StatusCode, header};
use include_dir::{Dir, DirEntry, include_dir};

static ASSETS: Dir<'static> = include_dir!("$ANTIRION_CONSOLE_ASSETS");

/// Whether the binary embeds the frontend's export rather than the placeholder page.
pub const BUILT: bool = matches!(env!("ANTIRION_CONSOLE_BUILT").as_bytes(), b"1");

/// The policy of every console response (the frontend needs no inline script or style; `img-src
/// https:` exists for organization logos).
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data: https:; style-src 'self'; \
     script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const HASHED_PREFIX: &str = "_next/static/";

fn index() -> &'static HashMap<&'static str, &'static [u8]> {
    static INDEX: OnceLock<HashMap<&'static str, &'static [u8]>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map = HashMap::new();
        let mut stack = vec![&ASSETS];
        while let Some(dir) = stack.pop() {
            for entry in dir.entries() {
                match entry {
                    DirEntry::Dir(d) => stack.push(d),
                    DirEntry::File(f) => {
                        if let Some(p) = f.path().to_str() {
                            map.insert(p, f.contents());
                        }
                    }
                }
            }
        }
        map
    })
}

/// One embedded file, ready to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset {
    /// Its path inside the export (`keys/index.html`).
    pub path: &'static str,
    pub body: &'static [u8],
    pub content_type: &'static str,
    pub cache_control: &'static str,
}

fn asset(path: &str) -> Option<Asset> {
    let (&path, &body) = index().get_key_value(path)?;
    Some(Asset {
        path,
        body,
        content_type: content_type(path),
        cache_control: cache_control(path),
    })
}

/// Hashed assets never change under their name; HTML is never stored, so a new build is seen at
/// once; anything else is revalidated.
fn cache_control(path: &str) -> &'static str {
    if path.starts_with(HASHED_PREFIX) {
        IMMUTABLE
    } else if content_type(path).starts_with("text/html") {
        "no-store"
    } else {
        "no-cache"
    }
}

/// The file a path under the console's mount names, `rel` being the part after the mount without
/// its leading slash (`""`, `keys/`, `keys`, `_next/static/chunks/a.js`). A directory path is served
/// by its `index.html` or its `.html` sibling, and an extensionless path by the `.html` file or the
/// `index.html` of the directory it names, which covers both layouts of a static export.
#[must_use]
pub fn resolve(rel: &str) -> Option<Asset> {
    pick(rel, |p| index().contains_key(p)).and_then(|p| asset(&p))
}

fn pick(rel: &str, exists: impl Fn(&str) -> bool) -> Option<String> {
    let candidates = if rel.is_empty() {
        vec!["index.html".to_owned()]
    } else if let Some(dir) = rel.strip_suffix('/') {
        vec![format!("{rel}index.html"), format!("{dir}.html")]
    } else {
        vec![rel.to_owned(), format!("{rel}.html"), format!("{rel}/index.html")]
    };
    candidates.into_iter().find(|c| exists(c))
}

/// The export's own not-found page, when it has one.
#[must_use]
pub fn not_found_page() -> Option<Asset> {
    asset("404.html")
}

/// The content type of a file in the export, by its extension.
#[must_use]
pub fn content_type(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// The headers every console response carries, whatever its status.
pub fn security_headers(headers: &mut http::HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}

/// The response to a request for `rel` under the console's mount: the file, the export's
/// `404.html` (or a short text) with status 404, or 405 for anything but a read.
#[must_use]
pub fn serve(method: &Method, rel: &str) -> Response<&'static [u8]> {
    if method != Method::GET && method != Method::HEAD {
        let mut resp = Response::new(&b""[..]);
        *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        security_headers(resp.headers_mut());
        return resp;
    }
    let (status, found) = match resolve(rel).map(|a| (StatusCode::OK, a)) {
        Some(found) => found,
        None => (
            StatusCode::NOT_FOUND,
            not_found_page().unwrap_or(Asset {
                path: "",
                body: b"Not found\n",
                content_type: "text/plain; charset=utf-8",
                cache_control: "no-store",
            }),
        ),
    };
    let mut resp = Response::new(found.body);
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(found.content_type));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if status == StatusCode::OK {
            found.cache_control
        } else {
            "no-store"
        }),
    );
    security_headers(h);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_an_html_page() {
        let a = resolve("").unwrap();
        assert_eq!(a.path, "index.html");
        assert_eq!(a.content_type, "text/html; charset=utf-8");
        assert_eq!(a.cache_control, "no-store");
        assert!(!a.body.is_empty());
    }

    #[test]
    fn unknown_paths_are_not_found() {
        assert!(resolve("nope").is_none());
        assert!(resolve("nope/").is_none());
        assert!(resolve("_next/static/none.js").is_none());
    }

    #[test]
    fn content_types_follow_extensions() {
        assert_eq!(content_type("a/b.JS"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("x.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("f.woff2"), "font/woff2");
        assert_eq!(content_type("i.svg"), "image/svg+xml");
        assert_eq!(content_type("keys.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type("README"), "application/octet-stream");
    }

    #[test]
    fn serving_sets_every_security_header() {
        let resp = serve(&Method::GET, "");
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY);
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert!(CONTENT_SECURITY_POLICY.starts_with("default-src 'self'"));
    }

    #[test]
    fn unknown_paths_answer_404_with_the_console_headers() {
        let resp = serve(&Method::GET, "no/such/page");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()[header::CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn only_reads_are_allowed() {
        let resp = serve(&Method::POST, "");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(resp.headers()[header::ALLOW], "GET, HEAD");
        assert_eq!(resp.headers()[header::X_FRAME_OPTIONS], "DENY");
    }

    #[test]
    fn a_build_without_the_export_embeds_only_the_placeholder() {
        assert!(index().contains_key("index.html"));
        if !BUILT {
            assert_eq!(index().len(), 1);
            assert!(not_found_page().is_none());
        }
    }

    #[test]
    fn both_export_layouts_resolve() {
        let files = [
            "index.html",
            "keys/index.html",
            "budgets.html",
            "_next/static/chunks/app.js",
            "logs.txt",
        ];
        let exists = |p: &str| files.contains(&p);
        let got = |rel: &str| pick(rel, exists);
        assert_eq!(got("").as_deref(), Some("index.html"));
        assert_eq!(got("keys").as_deref(), Some("keys/index.html"));
        assert_eq!(got("keys/").as_deref(), Some("keys/index.html"));
        assert_eq!(got("budgets").as_deref(), Some("budgets.html"));
        assert_eq!(got("budgets/").as_deref(), Some("budgets.html"));
        assert_eq!(got("budgets.html").as_deref(), Some("budgets.html"));
        assert_eq!(
            got("_next/static/chunks/app.js").as_deref(),
            Some("_next/static/chunks/app.js")
        );
        assert_eq!(got("logs.txt").as_deref(), Some("logs.txt"));
        assert_eq!(got("alerts"), None);
        assert_eq!(got("keys/x"), None);
    }

    #[test]
    fn hashed_assets_are_immutable_and_others_revalidate() {
        let policy = cache_control;
        assert_eq!(policy("_next/static/chunks/app-3f2a.js"), IMMUTABLE);
        assert_eq!(policy("keys/index.html"), "no-store");
        assert_eq!(policy("favicon.ico"), "no-cache");
    }
}
