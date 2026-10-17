//! Gateway profiles rendered into the gateway's configuration: the profile's document with the mock
//! and collector addresses filled in, plus the generated keys, models, deployments and MCP servers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use antirion_bench_corpus::keys;
use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use crate::config::{NodeAddr, Paths, StoreCfg};

/// What a profile file says.
#[derive(Debug, Clone)]
struct ProfileFile {
    name: String,
    raw: Value,
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

fn load(paths: &Paths, name: &str, subst: &[(&str, &str)], depth: usize) -> anyhow::Result<ProfileFile> {
    if depth > 8 {
        bail!("profile {name} extends too deeply");
    }
    let path = paths.profile(name);
    let mut text =
        std::fs::read_to_string(&path).with_context(|| format!("no profile {name} ({})", path.display()))?;
    for (from, to) in subst {
        text = text.replace(from, to);
    }
    let mut raw: Value =
        serde_saphyr::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
    let parent = raw.get("extends").and_then(Value::as_str).map(str::to_owned);
    if let Some(parent) = parent {
        let mut base = load(paths, &parent, subst, depth + 1)?.raw;
        if let Some(o) = raw.as_object_mut() {
            o.remove("extends");
        }
        merge(&mut base, raw);
        raw = base;
    }
    Ok(ProfileFile {
        name: name.to_owned(),
        raw,
    })
}

/// A rendered profile.
#[derive(Debug, Clone)]
pub(crate) struct Rendered {
    pub(crate) name: String,
    /// The configuration document without the node-specific static blocks.
    pub(crate) doc: Value,
    /// The environment that carries every key secret (`AK_<name>`).
    pub(crate) env: Vec<(String, String)>,
    /// The secrets requests are spread over.
    pub(crate) used_keys: Vec<String>,
    /// Secrets of the named keys.
    pub(crate) named: BTreeMap<String, String>,
    /// Generated model names, in order.
    pub(crate) models: Vec<String>,
    pub(crate) cluster: bool,
}

/// Where the rendered gateway finds the mock.
#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    /// `http://host:port/<latency profile>`.
    pub(crate) mock: String,
    /// The null collector, or a listener that never answers.
    pub(crate) sink: String,
}

fn count(v: &Value, path: &str) -> usize {
    v.pointer(path)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0)
}

fn map_at<'a>(doc: &'a mut Value, key: &str) -> anyhow::Result<&'a mut Map<String, Value>> {
    let root = doc
        .as_object_mut()
        .context("the profile document is not a mapping")?;
    root.entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("{key} is not a mapping"))
}

/// Renders profile `name` against `endpoints`, with every key derived from `seed`.
pub(crate) fn render(
    paths: &Paths,
    name: &str,
    endpoints: &Endpoints,
    seed: u64,
) -> anyhow::Result<Rendered> {
    let subst = [
        ("{{mock}}", endpoints.mock.as_str()),
        ("{{sink}}", endpoints.sink.as_str()),
    ];
    let file = load(paths, name, &subst, 0)?;
    let raw = &file.raw;
    let mut doc = raw.get("document").cloned().unwrap_or_else(|| json!({}));
    if let Some(o) = doc.as_object_mut() {
        o.insert("version".to_owned(), json!(1));
    }
    let mut env = Vec::new();
    let mut key_doc = |name: &str, extra: Option<&Value>| {
        let mut k = json!({"project": "bench", "secret": format!("${{env:{}}}", keys::env_var(name))});
        if let Some(e) = extra {
            merge(&mut k, e.clone());
        }
        env.push((keys::env_var(name), keys::secret(seed, name)));
        k
    };
    let pool = count(raw, "/keys/pool").max(1);
    let used = count(raw, "/keys/used").clamp(1, pool);
    let pool_policies = raw.pointer("/keys/policies").map(|p| json!({"policies": p}));
    let mut key_map = Map::new();
    for i in 0..pool {
        let n = keys::pool_name(i);
        key_map.insert(n.clone(), key_doc(&n, pool_policies.as_ref()));
    }
    let mut named = BTreeMap::new();
    if let Some(Value::Object(m)) = raw.pointer("/keys/named") {
        for (n, spec) in m {
            key_map.insert(n.clone(), key_doc(n, Some(spec)));
            named.insert(n.clone(), keys::secret(seed, n));
        }
    }
    map_at(&mut doc, "keys")?.extend(key_map);
    let models = generate(&mut doc, raw)?;
    Ok(Rendered {
        name: file.name,
        doc,
        env,
        used_keys: (0..used)
            .map(|i| keys::secret(seed, &keys::pool_name(i)))
            .collect(),
        named,
        models,
        cluster: raw.get("cluster").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn generate(doc: &mut Value, raw: &Value) -> anyhow::Result<Vec<String>> {
    let models = count(raw, "/generate/models");
    let per = count(raw, "/generate/deployments_per_model").max(1);
    let extra = count(raw, "/generate/extra_deployments");
    let servers = count(raw, "/generate/mcp_servers");
    let price = json!({"input": "1.00", "output": "2.00"});
    let deployment =
        || json!({"provider": "bench-openai", "model": "bench-upstream", "price": price.clone()});
    let mut deps = Map::new();
    let mut model_map = Map::new();
    let mut names = Vec::with_capacity(models);
    for i in 0..models {
        let model = format!("bench-m{i:05}");
        let children: Vec<Value> = (0..per)
            .map(|j| {
                let d = format!("bench-d{i:05}-{j}");
                deps.insert(d.clone(), deployment());
                json!({"deployment": d})
            })
            .collect();
        let target = if per == 1 {
            children.into_iter().next().unwrap_or(Value::Null)
        } else {
            json!({"type": "weighted", "children": children})
        };
        model_map.insert(model.clone(), json!({"target": target}));
        names.push(model);
    }
    for i in 0..extra {
        deps.insert(format!("bench-x{i:05}"), deployment());
    }
    map_at(doc, "deployments")?.extend(deps);
    map_at(doc, "models")?.extend(model_map);
    if servers > 0 {
        let url = doc
            .pointer("/mcp/servers/a/url")
            .cloned()
            .context("generated MCP servers copy server `a`, which the profile lacks")?;
        let mcp = map_at(doc, "mcp")?;
        let list = mcp
            .entry("servers".to_owned())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("mcp.servers is not a mapping")?;
        for i in 0..servers {
            list.insert(
                format!("s{i:04}"),
                json!({"transport": "streamable_http", "url": url}),
            );
        }
    }
    Ok(names)
}

/// The node-specific blocks of a locally started node.
#[derive(Debug, Clone)]
pub(crate) struct NodeStatics<'a> {
    pub(crate) node: NodeAddr,
    pub(crate) index: usize,
    pub(crate) data_dir: PathBuf,
    pub(crate) admin_token: &'a str,
    pub(crate) store: Option<&'a StoreCfg>,
}

/// A 32-byte key in base64 shared by every node of a cluster profile (benchmark-only material).
const CLUSTER_KEY: &str = "YmVuY2htYXJrLWNsdXN0ZXIta2V5LTAxMjM0NTY3ODk=";

/// The startup file of one node: the profile's document plus listeners, store and cluster blocks.
pub(crate) fn startup_doc(r: &Rendered, s: &NodeStatics<'_>) -> anyhow::Result<Value> {
    let mut doc = r.doc.clone();
    let o = doc.as_object_mut().context("the document is not a mapping")?;
    o.insert("server".to_owned(), json!({"listen": s.node.data.to_string()}));
    o.insert("admin".to_owned(), json!({"listen": s.node.admin.to_string()}));
    merge(
        &mut doc,
        json!({"auth": {"admin": {"bootstrap_token": s.admin_token}}}),
    );
    let o = doc.as_object_mut().context("the document is not a mapping")?;
    if r.cluster {
        let store = s.store.context(
            "this profile runs a cluster: the topology must give a PostgreSQL and a Valkey (`store`)",
        )?;
        o.insert(
            "store".to_owned(),
            json!({"backend": "postgres", "postgres": {"url": store.postgres_url},
                   "embedded": {"path": s.data_dir.display().to_string()}}),
        );
        o.insert(
            "cluster".to_owned(),
            json!({"mode": "cluster", "node_id": format!("bench-{}", s.index + 1),
                   "counters": {"backend": "redis", "redis": {"url": store.valkey_url}}}),
        );
        merge(
            &mut doc,
            json!({"security": {"secrets": {"encryption_keys": [CLUSTER_KEY]}}}),
        );
    } else {
        o.insert(
            "store".to_owned(),
            json!({"backend": "embedded", "embedded": {"path": s.data_dir.display().to_string()}}),
        );
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints() -> Endpoints {
        Endpoints {
            mock: "http://127.0.0.1:1/L0".into(),
            sink: "http://127.0.0.1:1/__bench/null".into(),
        }
    }

    #[test]
    fn profiles_render() {
        let p = Paths::locate(None).unwrap();
        let min = render(&p, "P-min", &endpoints(), 1).unwrap();
        assert_eq!(min.used_keys.len(), 1);
        assert_eq!(
            min.doc["providers"]["bench-openai"]["base_url"],
            "http://127.0.0.1:1/L0/v1"
        );
        assert_eq!(min.doc["observability"]["request_logs"]["enabled"], false);

        let std = render(&p, "P-std", &endpoints(), 1).unwrap();
        assert_eq!(std.doc["keys"].as_object().unwrap().len(), 10_002);
        assert_eq!(std.used_keys.len(), 1000);
        assert_eq!(std.env.len(), 10_002);
        assert!(std.named.contains_key("restricted"));
        assert_eq!(std.doc["observability"]["request_logs"]["enabled"], true);
        assert_eq!(
            std.doc["observability"]["request_logs"]["sinks"]["http"]["collector"]["url"],
            "http://127.0.0.1:1/__bench/null/logs"
        );
        assert!(std.doc["keys"]["k00001"]["policies"]["limits"]["rpm"].is_object());
        assert_eq!(std.doc["keys"]["k00001"]["secret"], "${env:AK_k00001}");

        let hundred = render(&p, "P-std-100", &endpoints(), 1).unwrap();
        assert_eq!(hundred.doc["models"].as_object().unwrap().len(), 100);

        let cluster = render(&p, "P-cluster", &endpoints(), 1).unwrap();
        assert!(cluster.cluster);
        let node = NodeStatics {
            node: NodeAddr {
                data: "127.0.0.1:1".parse().unwrap(),
                admin: "127.0.0.1:2".parse().unwrap(),
                agent: None,
            },
            index: 0,
            data_dir: PathBuf::from("/tmp/x"),
            admin_token: "t",
            store: None,
        };
        assert!(startup_doc(&cluster, &node).is_err());
        let doc = startup_doc(&min, &node).unwrap();
        assert_eq!(doc["server"]["listen"], "127.0.0.1:1");
        assert_eq!(doc["store"]["backend"], "embedded");
    }

    #[test]
    fn scale_profile_counts() {
        let p = Paths::locate(None).unwrap();
        let scale = render(&p, "P-scale", &endpoints(), 1).unwrap();
        assert_eq!(scale.doc["models"].as_object().unwrap().len(), 10_000);
        assert_eq!(scale.doc["deployments"].as_object().unwrap().len(), 20_000);
        assert_eq!(scale.doc["mcp"]["servers"].as_object().unwrap().len(), 1_000);
        assert_eq!(scale.env.len(), 100_002);
        assert_eq!(scale.models.len(), 9_996);
    }
}
