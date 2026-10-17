//! Scenarios turned into request sources for the load generator: the gateway side (keys spread over
//! the profile's pool, round robin over the nodes, mixes drawn per request) and the direct side.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use antirion_bench_corpus::prng::SplitMix64;
use antirion_bench_corpus::{SEED, entries, keys};
use antirion_bench_loadgen::{Prepared, Source};
use antirion_bench_mock::Profile;
use anyhow::{Context, bail};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::Full;

use crate::config::{Paths, ScenarioDef};

/// One request shape: path, fixed headers and body.
#[derive(Debug, Clone)]
struct Shape {
    path: String,
    headers: HeaderMap,
    body: Bytes,
    expect: u16,
    stream: bool,
}

fn headers(pairs: &[(String, String)]) -> anyhow::Result<HeaderMap> {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(HeaderName::from_bytes(k.as_bytes())?, HeaderValue::from_str(v)?);
    }
    Ok(h)
}

/// A scenario's request components, before keys and nodes are chosen.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    pub(crate) name: String,
    gateway: Vec<Shape>,
    direct: Vec<Shape>,
    /// Cumulative weights over the components (one component: no draw).
    weights: Vec<u32>,
    pub(crate) key: KeyChoice,
    pub(crate) has_direct: bool,
    pub(crate) stream: bool,
    pub(crate) session: Option<String>,
    /// The largest request body.
    pub(crate) body_bytes: usize,
    /// Cycle requests over the profile's generated models.
    pub(crate) cycle_models: bool,
    model_placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyChoice {
    Pool,
    Unknown,
    Named(String),
}

fn shape(t: &entries::Template, expect: u16, stream: bool) -> anyhow::Result<Shape> {
    Ok(Shape {
        path: t.path.clone(),
        headers: headers(&t.headers)?,
        body: Bytes::from(t.body.clone()),
        expect,
        stream,
    })
}

/// Loads scenario `name` (a mix loads its components).
pub(crate) fn load(paths: &Paths, all: &BTreeMap<String, ScenarioDef>, name: &str) -> anyhow::Result<Loaded> {
    let def = all.get(name).with_context(|| format!("no scenario {name}"))?;
    if let Some(mix) = &def.mix {
        let mut out: Option<Loaded> = None;
        let mut weights = Vec::new();
        let mut total = 0;
        for (part, w) in mix {
            let l = load(paths, all, part)?;
            total += w;
            weights.push(total);
            out = Some(match out {
                None => Loaded {
                    name: name.to_owned(),
                    weights: Vec::new(),
                    ..l
                },
                Some(mut acc) => {
                    acc.gateway.extend(l.gateway);
                    acc.direct.extend(l.direct);
                    acc.stream |= l.stream;
                    acc.body_bytes = acc.body_bytes.max(l.body_bytes);
                    acc
                }
            });
        }
        let mut out = out.context("an empty mix")?;
        out.weights = weights;
        return Ok(out);
    }
    let entry_name = def
        .entry
        .as_deref()
        .with_context(|| format!("scenario {name} has no entry"))?;
    let entry = antirion_bench_corpus::load(&paths.corpus(), entry_name)?;
    let expect = def.expect.unwrap_or(200);
    let mut gw = shape(&entry.gateway, expect, def.stream)?;
    if let Some(model) = &def.model {
        let body = String::from_utf8_lossy(&gw.body).replacen(
            &format!("\"model\":\"{}\"", entries::CHAT_MODEL),
            &format!("\"model\":\"{model}\""),
            1,
        );
        gw.body = Bytes::from(body);
    }
    let key = match def.key.as_deref() {
        None | Some("pool") => KeyChoice::Pool,
        Some("unknown") => KeyChoice::Unknown,
        Some(other) => KeyChoice::Named(other.to_owned()),
    };
    Ok(Loaded {
        name: name.to_owned(),
        body_bytes: gw.body.len(),
        gateway: vec![gw],
        direct: vec![shape(&entry.direct, 200, def.stream)?],
        weights: Vec::new(),
        key,
        has_direct: def.direct,
        stream: def.stream,
        session: def.session.clone(),
        cycle_models: def.models.as_deref() == Some("cycle"),
        model_placeholder: Some(format!("\"model\":\"{}\"", entries::CHAT_MODEL)),
    })
}

impl Loaded {
    /// Counts `status` as the gateway's expected answer to every request.
    pub(crate) fn expect_status(&mut self, status: u16) {
        for s in &mut self.gateway {
            s.expect = status;
        }
    }

    /// Renames the model of each gateway request where `rename` gives another name for it.
    pub(crate) fn rename_models(&mut self, rename: impl Fn(&str) -> Option<String>) {
        const KEY: &str = "\"model\":\"";
        for s in &mut self.gateway {
            let text = String::from_utf8_lossy(&s.body).into_owned();
            let Some(start) = text.find(KEY).map(|i| i + KEY.len()) else {
                continue;
            };
            let Some(len) = text.get(start..).and_then(|t| t.find('"')) else {
                continue;
            };
            let Some(to) = text.get(start..start + len).and_then(&rename) else {
                continue;
            };
            let mut body = String::with_capacity(text.len() + to.len());
            body.push_str(text.get(..start).unwrap_or_default());
            body.push_str(&to);
            body.push_str(text.get(start + len..).unwrap_or_default());
            s.body = Bytes::from(body);
        }
        self.body_bytes = self.gateway.iter().map(|s| s.body.len()).max().unwrap_or(0);
    }

    /// The longest a direct answer takes under `profile`.
    pub(crate) fn scripted(&self, profile: Profile) -> Duration {
        profile.scripted(self.stream)
    }
}

/// Per-request randomness: a function of the seed and the request's index only.
fn unit(seed: u64, index: u64, salt: u64) -> f64 {
    SplitMix64::new(seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt).unit()
}

/// A source over a loaded scenario.
#[derive(Debug)]
pub(crate) struct ScenarioSource {
    shapes: Vec<Shape>,
    weights: Vec<u32>,
    /// Values of `auth_header`, one per key the requests are spread over (none for direct).
    auth: Vec<HeaderValue>,
    auth_header: HeaderName,
    /// MCP session ids, parallel to `auth`.
    sessions: Vec<HeaderValue>,
    nodes: Vec<SocketAddr>,
    absolute: bool,
    prefix: String,
    disconnect_fraction: f64,
    /// Bodies with the model replaced, one per generated model.
    cycled: Vec<Bytes>,
}

/// How a source sends.
#[derive(Debug, Clone)]
pub(crate) struct SourceCfg {
    pub(crate) nodes: Vec<SocketAddr>,
    /// h2 needs absolute URIs; HTTP/1.1 gets origin-form with a Host header.
    pub(crate) absolute: bool,
    pub(crate) disconnect_fraction: f64,
    /// The header carrying the key; `authorization` gets the `Bearer` scheme.
    pub(crate) auth_header: HeaderName,
}

impl SourceCfg {
    pub(crate) fn new(nodes: Vec<SocketAddr>, absolute: bool, disconnect_fraction: f64) -> Self {
        Self {
            nodes,
            absolute,
            disconnect_fraction,
            auth_header: http::header::AUTHORIZATION,
        }
    }
}

impl ScenarioSource {
    /// The gateway side, with `keys` (secrets) and optional MCP `sessions` (one per key).
    pub(crate) fn gateway(
        l: &Loaded,
        cfg: &SourceCfg,
        keys: &[String],
        sessions: &[String],
        models: &[String],
    ) -> anyhow::Result<Self> {
        if keys.is_empty() {
            bail!("no key to send with");
        }
        let bearer = cfg.auth_header == http::header::AUTHORIZATION;
        let auth = keys
            .iter()
            .map(|k| {
                if bearer {
                    HeaderValue::from_str(&format!("Bearer {k}"))
                } else {
                    HeaderValue::from_str(k)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sessions = sessions
            .iter()
            .map(|s| HeaderValue::from_str(s))
            .collect::<Result<Vec<_>, _>>()?;
        let cycled = if l.cycle_models {
            let placeholder = l.model_placeholder.clone().unwrap_or_default();
            let base = l.gateway.first().context("no request")?;
            let text = String::from_utf8_lossy(&base.body).into_owned();
            models
                .iter()
                .map(|m| Bytes::from(text.replacen(&placeholder, &format!("\"model\":\"{m}\""), 1)))
                .collect()
        } else {
            Vec::new()
        };
        if l.cycle_models && cycled.is_empty() {
            bail!(
                "scenario {} cycles over generated models; the profile generates none",
                l.name
            );
        }
        Ok(Self {
            shapes: l.gateway.clone(),
            weights: l.weights.clone(),
            auth,
            auth_header: cfg.auth_header.clone(),
            sessions,
            nodes: cfg.nodes.clone(),
            absolute: cfg.absolute,
            prefix: String::new(),
            disconnect_fraction: cfg.disconnect_fraction,
            cycled,
        })
    }

    /// The direct side: the same requests to the mock under `profile`.
    pub(crate) fn direct(
        l: &Loaded,
        cfg: &SourceCfg,
        profile: Profile,
        session: Option<&str>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            shapes: l.direct.clone(),
            weights: l.weights.clone(),
            auth: Vec::new(),
            auth_header: cfg.auth_header.clone(),
            sessions: session
                .map(HeaderValue::from_str)
                .transpose()?
                .into_iter()
                .collect(),
            nodes: cfg.nodes.clone(),
            absolute: cfg.absolute,
            prefix: format!("/{}", profile.name()),
            disconnect_fraction: cfg.disconnect_fraction,
            cycled: Vec::new(),
        })
    }

    fn pick(&self, index: u64) -> usize {
        let Some(&total) = self.weights.last() else {
            return 0;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let draw = (unit(SEED, index, 0x006d_6978) * f64::from(total)) as u32;
        self.weights.iter().position(|&w| draw < w).unwrap_or(0)
    }

    fn build(&self, index: u64) -> anyhow::Result<Prepared> {
        let n = usize::try_from(index).unwrap_or(0);
        let shape = self.shapes.get(self.pick(index)).context("no request shape")?;
        let node = n % self.nodes.len().max(1);
        let addr = self.nodes.get(node).context("no node")?;
        let path = format!("{}{}", self.prefix, shape.path);
        let uri: Uri = if self.absolute {
            format!("http://{addr}{path}").parse()?
        } else {
            path.parse()?
        };
        let body = if self.cycled.is_empty() {
            shape.body.clone()
        } else {
            self.cycled
                .get(n % self.cycled.len())
                .cloned()
                .unwrap_or_default()
        };
        let mut req = Request::builder().method(Method::POST).uri(uri);
        if !self.absolute {
            req = req.header(http::header::HOST, addr.to_string());
        }
        let mut request = req.body(Full::new(body))?;
        request.headers_mut().extend(shape.headers.clone());
        if !self.auth.is_empty() {
            let k = n % self.auth.len();
            if let Some(a) = self.auth.get(k) {
                request.headers_mut().insert(self.auth_header.clone(), a.clone());
            }
            if let Some(s) = self.sessions.get(k) {
                request.headers_mut().insert("mcp-session-id", s.clone());
            }
        } else if let Some(s) = self.sessions.first() {
            request.headers_mut().insert("mcp-session-id", s.clone());
        }
        let disconnect = shape.stream && unit(SEED, index, 0x6463) < self.disconnect_fraction;
        Ok(Prepared {
            node,
            request,
            expect: shape.expect,
            disconnect,
        })
    }
}

impl Source for ScenarioSource {
    fn prepare(&self, index: u64) -> Prepared {
        self.build(index).unwrap_or_else(|_| Prepared {
            node: 0,
            request: Request::new(Full::new(Bytes::new())),
            expect: 0,
            disconnect: false,
        })
    }
}

/// The keys a scenario sends with under a rendered profile.
pub(crate) fn keys_for(
    l: &Loaded,
    used: &[String],
    named: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<String>> {
    Ok(match &l.key {
        KeyChoice::Pool => used.to_vec(),
        KeyChoice::Unknown => vec![keys::unknown(SEED)],
        KeyChoice::Named(n) => vec![
            named
                .get(n)
                .cloned()
                .with_context(|| format!("the profile declares no key {n}"))?,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::scenarios;

    fn cfg() -> SourceCfg {
        SourceCfg::new(
            vec!["127.0.0.1:1".parse().unwrap(), "127.0.0.1:2".parse().unwrap()],
            false,
            0.0,
        )
    }

    #[test]
    fn another_header_and_model() {
        let p = Paths::locate(None).unwrap();
        let all = scenarios(&p).unwrap();
        let mut s1 = load(&p, &all, "S1").unwrap();
        s1.rename_models(|m| (m == "bench-chat").then(|| "vendor/upstream-model".to_owned()));
        let mut m1 = load(&p, &all, "M1").unwrap();
        m1.rename_models(|m| Some(format!("x/{m}")));
        let models: Vec<String> = m1
            .gateway
            .iter()
            .map(|s| {
                String::from_utf8_lossy(&s.body)
                    .split('"')
                    .nth(3)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert!(models.contains(&"x/bench-messages".to_owned()), "{models:?}");
        assert!(models.contains(&"x/bench-embed".to_owned()), "{models:?}");
        let cfg = SourceCfg {
            auth_header: HeaderName::from_static("x-key"),
            ..cfg()
        };
        let src = ScenarioSource::gateway(&s1, &cfg, &["secret".into()], &[], &[]).unwrap();
        let r = src.build(0).unwrap();
        assert_eq!(r.request.headers()["x-key"], "secret");
        assert!(r.request.headers().get("authorization").is_none());
        let direct = ScenarioSource::direct(&s1, &cfg, Profile::L0, None).unwrap();
        let body = direct.build(0).unwrap();
        assert!(body.request.headers().get("x-key").is_none());
        let text = String::from_utf8_lossy(&src.shapes[0].body).into_owned();
        assert!(text.contains("\"model\":\"vendor/upstream-model\""));
        assert_eq!(s1.body_bytes, src.shapes[0].body.len());
    }

    #[test]
    fn mix_follows_its_weights() {
        let p = Paths::locate(None).unwrap();
        let all = scenarios(&p).unwrap();
        let m1 = load(&p, &all, "M1").unwrap();
        let src = ScenarioSource::gateway(&m1, &cfg(), &["k".into()], &[], &[]).unwrap();
        let mut counts = BTreeMap::new();
        for i in 0..20_000 {
            *counts.entry(src.pick(i)).or_insert(0u32) += 1;
        }
        let s1 = counts.values().copied().max().unwrap();
        assert!((11_400..12_600).contains(&s1), "{counts:?}");
        assert_eq!(src.pick(17), src.pick(17));
    }

    #[test]
    fn requests_carry_key_node_and_model() {
        let p = Paths::locate(None).unwrap();
        let all = scenarios(&p).unwrap();
        let s = load(&p, &all, "S8-403").unwrap();
        assert_eq!(s.key, KeyChoice::Named("restricted".into()));
        assert!(!s.has_direct);
        let src = ScenarioSource::gateway(&s, &cfg(), &["a".into(), "b".into()], &[], &[]).unwrap();
        let p1 = src.build(1).unwrap();
        assert_eq!(p1.node, 1);
        assert_eq!(p1.expect, 403);
        assert_eq!(p1.request.headers()["authorization"], "Bearer b");
        assert_eq!(p1.request.headers()["host"], "127.0.0.1:2");
        let s1 = load(&p, &all, "S1").unwrap();
        let d = ScenarioSource::direct(&s1, &cfg(), Profile::L60, None).unwrap();
        assert_eq!(
            d.build(0).unwrap().request.uri().path(),
            "/L60/v1/chat/completions"
        );
        let every = load(&p, &all, "S1-every-model").unwrap();
        let src =
            ScenarioSource::gateway(&every, &cfg(), &["k".into()], &[], &["m0".into(), "m1".into()]).unwrap();
        let body = src.build(1).unwrap();
        let _ = body;
        assert_eq!(
            src.cycled[1].len(),
            every.body_bytes - entries::CHAT_MODEL.len() + 2
        );
    }
}
