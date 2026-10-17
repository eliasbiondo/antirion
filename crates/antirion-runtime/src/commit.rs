//! Committing a configuration change: validate the resulting document as a whole, resolve its
//! secrets and build its snapshot, persist the generation with its audit entry, then swap it in.
//! A failure at any step changes nothing.

use std::collections::HashSet;
use std::sync::Arc;

use antirion_config::model::{self, STATIC_KEYS};
use antirion_config::secrets::SecretSource;
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::ResourceUid;
use antirion_core::audit::{Actor, AuditEntry, Outcome};
use antirion_store::{GenerationRecord, StoreError, StoredConfig};
use serde_json::Value;

use crate::Runtime;
use crate::changes::{self, Change, Op, Target};
use crate::snapshot::{self, Snapshot};
use crate::state::{ConfigState, KeyCredential, ResourceMeta, etag_of};

/// Who caused a commit, for its audit entry.
#[derive(Clone, Debug)]
pub struct AuditContext {
    pub actor: Actor,
    pub action: String,
    pub resource: String,
    pub request_id: String,
    pub source_ip: Option<String>,
    pub status_code: u16,
    /// The operator's stated reason for the change.
    pub reason: Option<String>,
}

/// A proposed change: the whole resulting document and how to record it.
#[derive(Debug)]
pub struct Mutation {
    /// The resulting document; it may still carry `version` and static keys for validation.
    pub candidate: Value,
    /// `api` or `apply:<source>`: the `managed_by` of what this mutation wrote and changed.
    pub writer: String,
    pub written: Vec<Target>,
    /// Credentials of keys this mutation creates.
    pub credentials: Vec<(String, KeyCredential)>,
    /// Metadata changes recorded with the document change; they commit a generation on their own.
    pub meta_edits: Vec<MetaEdit>,
    pub dry_run: bool,
    pub source: String,
    pub audit: Option<AuditContext>,
}

/// A change of a resource's metadata that a mutation records.
#[derive(Clone, Debug)]
pub enum MetaEdit {
    /// A key's credential is replaced (a rotation).
    Credential { key: String, credential: KeyCredential },
    /// A key was disabled by the gateway itself, for `reason`.
    DisabledReason { key: String, reason: String },
}

/// The result of a mutation.
#[derive(Debug)]
pub struct Committed {
    /// The serving generation afterwards (unchanged for a dry run or a no-op).
    pub generation: u64,
    pub changes: Vec<Change>,
    pub snapshot: Arc<Snapshot>,
}

/// Why a mutation did not commit.
#[derive(Debug, thiserror::Error)]
pub enum CommitError<E> {
    #[error("rejected before validation")]
    Rejected(E),
    #[error("the resulting configuration is invalid")]
    Invalid(Vec<ConfigError>),
    #[error("the store cannot be written: {0}")]
    Store(#[from] StoreError),
    #[error("the snapshot builder stopped")]
    Builder,
}

/// Validation of a candidate document: schema, semantic rules and secret resolution.
#[derive(Debug)]
pub struct Checked {
    pub doc: Value,
    pub secret_paths: Vec<DocPath>,
}

impl Runtime {
    /// Validates a whole candidate document without committing anything.
    pub fn check(&self, candidate: Value, previous: &Value) -> Result<Checked, Vec<ConfigError>> {
        self.check_with(candidate, previous, self.secrets.as_ref())
    }

    /// [`check`](Self::check) without contacting a secret store: values not read yet are taken as
    /// read. For checks anyone who reads the configuration may run (a validation, a diff, a change
    /// request's preview) and dry runs.
    pub fn check_unread(&self, candidate: Value, previous: &Value) -> Result<Checked, Vec<ConfigError>> {
        self.check_with(
            candidate,
            previous,
            &crate::secretstores::Unread(Arc::clone(&self.secrets)),
        )
    }

    fn check_with(
        &self,
        mut candidate: Value,
        previous: &Value,
        secrets: &dyn SecretSource,
    ) -> Result<Checked, Vec<ConfigError>> {
        let validation = antirion_config::validate(&mut candidate, previous);
        if !validation.is_valid() {
            return Err(validation.errors);
        }
        if let Some(root) = candidate.as_object_mut() {
            let admission = root.get("server").and_then(|s| s.get("admission")).cloned();
            for key in STATIC_KEYS {
                root.remove(*key);
            }
            if let Some(admission) = admission {
                root.insert("server".into(), serde_json::json!({ "admission": admission }));
            }
            root.insert(
                "version".into(),
                Value::from(antirion_config::schema::CURRENT_VERSION),
            );
        }
        model::normalise(&mut candidate);
        let mut egress = antirion_egress::config::check_document(&candidate);
        egress.extend(antirion_guardrails::check_document(&candidate));
        egress.extend(antirion_mcp::check_document(&candidate));
        egress.extend(antirion_ext::check_document(&candidate));
        egress.extend(crate::admission::memory::check_document(&candidate));
        egress.extend(crate::security::check_listeners(&candidate));
        egress.extend(antirion_identity::roles::check_document(&candidate));
        egress.extend(antirion_identity::saml::check_document(&candidate));
        egress.extend(antirion_identity::saml::check_transport(
            &candidate,
            self.test.is_some(),
        ));
        egress.extend(crate::jwt::check_transport(&candidate, self.test.is_some()));
        egress.extend(crate::budgets::config::check_document(&candidate));
        egress.extend(crate::alerting::config::check_document(&candidate));
        if !egress.is_empty() {
            return Err(egress);
        }
        let limits = antirion_limits::check::check_document(&candidate);
        if !limits.is_empty() {
            return Err(limits);
        }
        antirion_config::secrets::expand_document(&candidate, secrets)?;
        let secret_paths = validation
            .secret_paths
            .into_iter()
            .filter(|p| !matches!(p.segments().first(), Some(antirion_config::Seg::Key(k)) if STATIC_KEYS.contains(&k.as_str())))
            .collect();
        Ok(Checked {
            doc: candidate,
            secret_paths,
        })
    }

    /// What `config:validate` reports: the checks of a write through the Admin API, then the
    /// compilation a commit does (routes and their model files included). It contacts no secret
    /// store: references to values not read yet are checked by their syntax.
    pub async fn validate_candidate(
        &self,
        candidate: Value,
        previous: &ConfigState,
    ) -> Result<Checked, Vec<ConfigError>> {
        let checked = self.check_unread(candidate, &previous.doc)?;
        let refused = crate::secretstores::check_api_write(&previous.doc, &checked.doc);
        if !refused.is_empty() {
            return Err(refused);
        }
        let state = ConfigState {
            generation: previous.generation,
            doc: checked.doc.clone(),
            meta: previous.meta.clone(),
        };
        let unread: Arc<dyn SecretSource> = Arc::new(crate::secretstores::Unread(Arc::clone(&self.secrets)));
        let (secrets, pepper, paths) = (unread, self.pepper.clone(), checked.secret_paths.clone());
        tokio::task::spawn_blocking(move || {
            snapshot::build(state, paths, secrets.as_ref(), &pepper).map(drop)
        })
        .await
        .map_err(|_| {
            vec![ConfigError::new(
                &DocPath::root(),
                DetailCode::InvalidFormat,
                "the configuration could not be compiled",
            )]
        })??;
        Ok(checked)
    }

    /// Applies `build` to the current configuration under the write lock and commits the result.
    pub async fn mutate<E>(
        &self,
        build: impl FnOnce(&ConfigState) -> Result<Mutation, E>,
    ) -> Result<Committed, CommitError<E>> {
        let _guard = self.write_lock.lock().await;
        if self.cluster.is_some() {
            self.catch_up_locked().await.map_err(|e| match e {
                crate::cluster::SyncError::Store(s) => CommitError::Store(s),
                other => {
                    tracing::error!(error = %other, "a change cannot be built on the stored generation");
                    CommitError::Builder
                }
            })?;
        }
        let current = self.gens.load();
        let previous = Arc::clone(&current.config);
        let Mutation {
            candidate,
            writer,
            written,
            credentials,
            meta_edits,
            dry_run,
            source,
            audit,
        } = build(&previous).map_err(CommitError::Rejected)?;
        let actor = audit.as_ref().and_then(|a| a.actor.id.clone());
        let rejected = |errors: &Vec<ConfigError>| {
            if !dry_run {
                self.config_applied(&source, false);
                self.config_event(&current, &source, actor.as_deref(), Err(errors.len()));
            }
        };
        // Nothing is fetched for a document before it is known to be valid and allowed: the build
        // checked the writer's permission, the check below the document without the stores.
        let shaped = self
            .check_unread(candidate.clone(), &previous.doc)
            .inspect_err(rejected)
            .map_err(CommitError::Invalid)?;
        let from_file = audit
            .as_ref()
            .is_some_and(|a| a.actor.kind == antirion_core::audit::ActorKind::System);
        if !from_file {
            let refused = crate::secretstores::check_api_write(&previous.doc, &shaped.doc);
            if !refused.is_empty() {
                rejected(&refused);
                return Err(CommitError::Invalid(refused));
            }
        }
        let (checked, secrets): (Checked, Arc<dyn SecretSource>) = if dry_run {
            (
                shaped,
                Arc::new(crate::secretstores::Unread(Arc::clone(&self.secrets))),
            )
        } else {
            Box::pin(self.prefetch_secrets(&candidate)).await;
            let checked = self
                .check(candidate, &previous.doc)
                .inspect_err(rejected)
                .map_err(CommitError::Invalid)?;
            (checked, Arc::clone(&self.secrets))
        };
        let file_only = if from_file {
            Vec::new()
        } else {
            crate::fileonly::check(&previous.doc, &checked.doc)
        };
        if !file_only.is_empty() {
            rejected(&file_only);
            return Err(CommitError::Invalid(file_only));
        }
        let unfetched = if dry_run {
            Vec::new()
        } else {
            self.fetch_wasm_modules(&checked.doc).await
        };
        if !unfetched.is_empty() {
            rejected(&unfetched);
            return Err(CommitError::Invalid(unfetched));
        }
        let raw = changes::compute(&previous.doc, &checked.doc);
        let change_list =
            self.render_changes(&raw, &current.secret_paths, &checked.doc, &checked.secret_paths);
        if raw.is_empty() && meta_edits.is_empty() {
            return Ok(Committed {
                generation: previous.generation,
                changes: Vec::new(),
                snapshot: current,
            });
        }
        let now = self.clock.now();
        let next_generation = previous.generation + 1;
        let recorded = Recorded {
            writer: &writer,
            written: &written,
            credentials: &credentials,
            meta_edits: &meta_edits,
            actor: audit.as_ref().and_then(|a| a.actor.id.as_deref()),
        };
        let next = next_state(&previous, checked.doc, &raw, &recorded, next_generation, now);
        let state_for_store = if dry_run {
            None
        } else {
            let sealed = self
                .sealer
                .seal_document(&next.doc, &checked.secret_paths)
                .map_err(|_| CommitError::Builder)?;
            Some((serde_json::to_string(&sealed), serde_json::to_string(&next.meta)))
        };
        let pepper = self.pepper.clone();
        let secret_paths = checked.secret_paths;
        let built = tokio::task::spawn_blocking(move || {
            snapshot::build(next, secret_paths, secrets.as_ref(), &pepper)
        })
        .await
        .map_err(|_| CommitError::Builder)?
        .inspect_err(rejected)
        .map_err(CommitError::Invalid)?;
        if dry_run {
            return Ok(Committed {
                generation: previous.generation,
                changes: change_list,
                snapshot: current,
            });
        }
        let audit = audit.map(|a| AuditEntry {
            id: uuid_like(),
            time: now.to_rfc3339(),
            actor: a.actor,
            action: a.action,
            resource: a.resource,
            outcome: Outcome::Success,
            status_code: a.status_code,
            generation_before: Some(previous.generation),
            generation_after: Some(next_generation),
            request_id: a.request_id,
            source_ip: a.source_ip,
            kind: antirion_core::audit::AuditKind::Write,
            reason: a.reason,
            changes: if self.statics.include_diffs {
                serde_json::to_value(&change_list).unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        });
        if let Some((Ok(document), Ok(metadata))) = state_for_store {
            let record = GenerationRecord {
                generation: next_generation,
                created_at: now.to_rfc3339(),
                actor: audit
                    .as_ref()
                    .and_then(|a| a.actor.id.clone())
                    .unwrap_or_else(|| "system".to_owned()),
                source: source.clone(),
                summary: match change_list.len() {
                    1 => "1 change".to_owned(),
                    n => format!("{n} changes"),
                },
            };
            let kept = (
                document.clone(),
                audit
                    .as_ref()
                    .map_or(antirion_core::audit::ActorKind::System, |a| a.actor.kind),
            );
            let stored = StoredConfig {
                generation: next_generation,
                document,
                metadata,
            };
            self.store.commit_config(stored, record, audit).await?;
            self.keep_generation(next_generation, kept.0, kept.1).await;
        }
        let built = Arc::new(built);
        let previous = self.gens.load();
        self.gens.swap(Arc::clone(&built));
        self.health_follows(&previous, &built);
        self.note_budget_changes(&previous, &built, actor.as_deref());
        self.note_cache_changes(&previous, &built).await;
        if let Some(c) = &self.cluster {
            c.confirm(now);
            self.persist_local(&built, now).await;
        }
        self.config_applied(&source, true);
        self.config_event(&built, &source, actor.as_deref(), Ok(next_generation));
        self.observe_generation(&built);
        let live: HashSet<ResourceUid> = built.config.live_uids().collect();
        self.state.key_usage.retain(&live);
        self.state
            .verified
            .retain(|kid| built.identity.keys_by_kid.contains_key(kid));
        Ok(Committed {
            generation: next_generation,
            changes: change_list,
            snapshot: built,
        })
    }

    fn render_changes(
        &self,
        raw: &[(Target, Op, Option<Value>, Option<Value>)],
        old_secrets: &[DocPath],
        new_doc: &Value,
        new_secrets: &[DocPath],
    ) -> Vec<Change> {
        let current = self.gens.load();
        let old_shown = snapshot::redacted(&current.config.doc, old_secrets);
        let new_shown = snapshot::redacted(new_doc, new_secrets);
        raw.iter()
            .map(|(target, op, _, _)| {
                let pick = |doc: &Value| match target {
                    Target::Resource(c, name) => model::collection(c)
                        .and_then(|c| model::collection_map(doc, c))
                        .and_then(|m| m.get(name))
                        .cloned(),
                    Target::Setting("server.admission") => doc.pointer("/server/admission").cloned(),
                    Target::Setting(s) => model::singleton(s).and_then(|s| model::singleton_value(doc, s)),
                };
                let before = if *op == Op::Create { None } else { pick(&old_shown) };
                let after = if *op == Op::Delete { None } else { pick(&new_shown) };
                Change {
                    op: *op,
                    path: target.path(),
                    before,
                    after,
                }
            })
            .collect()
    }
}

/// How a mutation's changes are recorded in metadata.
struct Recorded<'a> {
    writer: &'a str,
    written: &'a [Target],
    credentials: &'a [(String, KeyCredential)],
    meta_edits: &'a [MetaEdit],
    actor: Option<&'a str>,
}

fn next_state(
    previous: &ConfigState,
    doc: Value,
    raw: &[(Target, Op, Option<Value>, Option<Value>)],
    mutation: &Recorded<'_>,
    generation: u64,
    now: antirion_core::UtcMicros,
) -> ConfigState {
    let mut meta = previous.meta.clone();
    let stamp = now.to_rfc3339_seconds();
    for (target, op, before, after) in raw {
        let written = mutation.written.contains(target);
        let slot = match target {
            Target::Resource(c, name) => meta
                .resources
                .entry((*c).to_owned())
                .or_default()
                .remove(name)
                .map(|m| (m, Some((*c, name)))),
            Target::Setting(s) => meta.settings.remove(*s).map(|m| (m, None)),
        };
        if *op == Op::Delete {
            continue;
        }
        let spec = after.clone().unwrap_or(Value::Null);
        let existing = slot.map(|(m, _)| m);
        let mut entry = match (existing, op) {
            (Some(m), Op::Update) => m,
            _ => ResourceMeta {
                etag: String::new(),
                generation,
                created_at: stamp.clone(),
                updated_at: stamp.clone(),
                managed_by: mutation.writer.to_owned(),
                uid: ResourceUid::random(),
                credential: None,
                created_by: mutation.actor.map(str::to_owned),
                disabled_reason: None,
                lifecycle_at: None,
                credentials_rotated_at: std::collections::BTreeMap::new(),
                rotated_at: None,
            },
        };
        if spec.get("enabled") != Some(&Value::Bool(false)) {
            entry.disabled_reason = None;
        }
        entry.etag = etag_of(&spec);
        entry.generation = generation;
        entry.updated_at = stamp.clone();
        if written || *op == Op::Create {
            entry.managed_by = mutation.writer.to_owned();
        }
        match target {
            Target::Resource("models", _)
                if lifecycle_state(before.as_ref()) != lifecycle_state(Some(&spec)) || *op == Op::Create =>
            {
                entry.lifecycle_at = Some(stamp.clone());
            }
            Target::Resource("providers", _) => {
                for (credential, _) in credentials(&spec)
                    .into_iter()
                    .filter(|c| !credentials(before.as_ref().unwrap_or(&Value::Null)).contains(c))
                {
                    entry.credentials_rotated_at.insert(credential, stamp.clone());
                }
                let now: Vec<String> = credentials(&spec).into_iter().map(|(n, _)| n).collect();
                entry.credentials_rotated_at.retain(|n, _| now.contains(n));
            }
            _ => {}
        }
        match target {
            Target::Resource(c, name) => {
                if let Some((_, cred)) = mutation
                    .credentials
                    .iter()
                    .find(|(n, _)| n == name)
                    .filter(|_| *c == "keys")
                {
                    entry.credential = Some(cred.clone());
                }
                meta.resources
                    .entry((*c).to_owned())
                    .or_default()
                    .insert(name.clone(), entry);
            }
            Target::Setting(s) => {
                meta.settings.insert((*s).to_owned(), entry);
            }
        }
    }
    for edit in mutation.meta_edits {
        let (key, apply): (&str, &dyn Fn(&mut ResourceMeta)) = match edit {
            MetaEdit::Credential { key, credential } => (key, &|m| {
                m.credential = Some(credential.clone());
                m.rotated_at = Some(stamp.clone());
            }),
            MetaEdit::DisabledReason { key, reason } => (key, &|m| m.disabled_reason = Some(reason.clone())),
        };
        if let Some(m) = meta.resources.get_mut("keys").and_then(|k| k.get_mut(key)) {
            apply(m);
            m.generation = generation;
            m.updated_at = stamp.clone();
        }
    }
    meta.resources.retain(|_, m| !m.is_empty());
    ConfigState {
        generation,
        doc,
        meta,
    }
}

/// A Model's lifecycle state as written (`ga` when absent).
fn lifecycle_state(spec: Option<&Value>) -> &str {
    spec.and_then(|s| s.pointer("/lifecycle/state"))
        .and_then(Value::as_str)
        .unwrap_or("ga")
}

/// A provider's credentials, each with its value: `default` for `auth.api_key` (the name its key
/// has), each `auth.api_keys` entry by name, and the other secret members of `auth` by member.
fn credentials(spec: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if let Some(k) = spec.pointer("/auth/api_key") {
        out.push((antirion_providers::SINGLE_KEY.to_owned(), k.clone()));
    }
    for member in ["service_account_json", "client_secret", "secret_access_key"] {
        if let Some(k) = spec.pointer(&format!("/auth/{member}")) {
            out.push((member.to_owned(), k.clone()));
        }
    }
    for k in spec
        .pointer("/auth/api_keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = k.get("name").and_then(Value::as_str) {
            out.push((name.to_owned(), k.get("key").cloned().unwrap_or(Value::Null)));
        }
    }
    out
}

fn uuid_like() -> String {
    antirion_core::RequestId::generate().as_str().to_owned()
}

/// A validation error at a path, for rejections that happen before the whole-document check.
#[must_use]
pub fn reject(path: &DocPath, code: DetailCode, message: &str) -> Vec<ConfigError> {
    vec![ConfigError::new(path, code, message)]
}
