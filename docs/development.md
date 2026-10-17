# Development guide

How the workspace is organised, where each concern lives, and the conventions the code follows.
Read it before adding a feature; keep it current when you change a convention.

## Running a development node

```bash
source ~/.cargo/env
cargo build -p antirion
export ANTIRION_BOOTSTRAP_TOKEN=dev-bootstrap-token-0123456789abcdef0123
rm -rf dev-data && ./target/debug/antirion serve --test-mode --config ci/standalone.yaml
```

`ci/standalone.yaml` listens on `127.0.0.1:18080` (data) and `127.0.0.1:18081` (admin) and keeps
its embedded store in `./dev-data`. Delete that directory for a clean start: configuration and keys
persist across restarts. The bootstrap token is the admin credential for the Admin API
(`Authorization: Bearer $ANTIRION_BOOTSTRAP_TOKEN`) and for the console's first sign-in.

The `test-hooks` feature (`cargo build -p antirion --features test-hooks`) compiles the panic
trigger `x-antirion-test-panic: <stage>`, honoured only in test mode. Release builds never enable it.

The binary's allocator is jemalloc on Linux with glibc (its build needs `make`) and mimalloc
everywhere else; the `mimalloc` feature selects mimalloc on glibc too.

## The console

The admin console is a static export of the frontend in `console/` (`next build` with `output:
"export"`), embedded into the binary by `antirion-console` and served on the admin listener under
`admin.ui.path` (default `/ui`) without credentials, since the files hold no data.

- **Contract of `console/out`**: `index.html` at its root (its presence is what marks an export);
  each route as `route/index.html` (`trailingSlash: true`) or `route.html`, both resolved, with and
  without a trailing slash; content-hashed assets under `_next/static/` (cached for a year,
  `immutable`); an optional `404.html`, served with status 404 for unknown paths. HTML is sent with
  `Cache-Control: no-store`, every other file with `no-cache`. The export must be built with
  `basePath` equal to the mount (`/ui` by default), because the asset URLs in its HTML are absolute.
  It may not use inline scripts or styles: every console response carries `Content-Security-Policy:
  default-src 'self'; …` with `script-src 'self'` and `style-src 'self'`.
- **Building**: `crates/antirion-console/build.rs` embeds `ANTIRION_CONSOLE_DIR` when set, else
  `console/out` when it holds an `index.html`, else `crates/antirion-console/placeholder/`, so a
  Rust-only build never needs Node. Cargo watches the export directory only once it exists: after the
  first export of a fresh checkout, build with `ANTIRION_CONSOLE_DIR=console/out` once (or `cargo
  clean -p antirion-console`) so the placeholder is replaced.
- `admin.ui.enabled: false` makes every path under the mount 404; the data listener never serves it.
  `GET /status` reports `console` and `data_plane_url`.
- **Frontend work**: `console/README.md` has the commands (`pnpm dev` proxies the Admin API of a local
  gateway, `pnpm dev:mock` adds the contract mocks and demo data), the layout, what each screen reads
  and writes, and how to run the end-to-end suite against an embedded build.
  For a mount other than `/ui`, build the export with `ANTIRION_UI_PATH` set to it.

## Operator identity (console sign-in)

- **State**: per-user identity state is not configuration. It lives in the store's
  `identity_records` table (`antirion-store/src/identity.rs`): one JSON body per `(kind, id)`, owned
  by a user, with an optional expiry the caller checks against its clock. Kinds: `password` (argon2id
  PHC hash and history), `factor` (TOTP secret sealed with the at-rest key, last accepted step),
  `recovery` (peppered verifiers), `challenge` (sign-in challenges, by digest), `lockout`, `reset`,
  `email`, `invitation` and `invite_token`, `preferences`, `webauthn_handle`. Single-use records are taken with
  `DELETE … RETURNING`, and TOTP steps and recovery codes are consumed with a compare-and-set
  (`swap_record`), so a code works once across nodes. Deleting a user deletes its records.
- **Sessions** carry `user_name` and `facts` (method, MFA, trust, organization, client); lists show a
  public id derived from the stored digest, with the client address's GeoIP location when a database
  is configured. A trusted session uses
  `auth.admin.session.trusted_device` for its idle and absolute limits; a local session is ended at its
  next use once its user may no longer sign in with a password (organization `local_login`,
  `sso_required_domains`, local accounts off).
- **Flows** (`antirion-runtime/src/operators/`): `signin.rs` (password step with the dummy
  verification for unknown emails, per-account lockout in the store, per-IP throttle in node memory
  under `auth.admin.login_throttle`, challenges valid five minutes and five wrong factors),
  `factors.rs`, `password.rs` (policy, history, change, reset), `records.rs` (invitations,
  preferences, email changes), `mail.rs` (the bounded queue and its tracked worker, paced by
  `max_messages_per_minute`, retried up to an hour). Hashing runs on `spawn_blocking`.
- **WebAuthn** (`operators/webauthn.rs`, through `webauthn_rp`): the relying party id is
  `local.mfa.webauthn.rp_id` or the host of the console's origin (`auth.admin.public_origin`, else the
  request's own), which must be a domain; user verification is required. The password step starts an
  assertion against the user's credentials, keeps its state with the challenge and returns
  `webauthn_options` beside `next` and `challenge`; an assertion is accepted only with the challenge it
  was started for. Ceremony state and credentials are stored in the library's binary form.
- **Admin API** (`antirion-admin`): `local.rs` holds the public endpoints, matched on exact paths
  before authentication (login options, local login and MFA, password reset, invitation acceptance,
  and the enrolment calls when they carry a sign-in challenge instead of a credential); `me.rs`,
  `mfa.rs`, `sessions.rs`, `invitations.rs`; `users.rs` has `:reset_mfa`. Personal tokens are admin
  tokens with `access` (`read_only` keeps only the `read` permissions of the user's bindings,
  `AdminPrincipal::read_only`) and a `last_used_us` recorded at most once a minute.
- `POST /test/reset` scope `counters` clears the sign-in throttles and the login-page ticker.
- **Throttles**: password sign-in and the public identity calls count failures per client address
  under `ip:<address>`; admin bearer tokens and token sign-ins under `bearer:<address>`, checked
  before any bearer credential is verified (`auth.admin.login_throttle`).
- **Link checks** (`token_checks.rs`, `operators/checks.rs`): `invitations:check` and
  `password_reset:check` read a token's state without using it. Using a reset or invitation token
  leaves a `used_token` record under `<kind>:<digest>`, so a used link reads `used`; an answer
  other than `valid` counts against the per-IP throttle. `signin_facts.rs` adds the trusted-device,
  password and first-run facts to the login options.
- **Principals without a user** (service accounts, the bootstrap credential) read `GET /auth/me`
  (`user: null`) and their own sessions, and get 403 from every other self-service call
  (`AdminError::needs_human`).
- **Single sign-on extras** (`oidc.rs`, `oauth.rs`): `default_role_bindings` for a provisioned user no
  mapping matched; `?test=true` on the OIDC login (authenticated, `security:write`) marks the login
  state, and its callback answers the claims and the bindings without a session or provisioning.
  `?trust_device=true` is kept in the login state; the callback starts a trusted session only while
  trusted devices are on and the ID token's `amr` names a second factor (`SignedIn::session_trusted`).
  GitHub providers (`auth.admin.sso.oauth`) sign in with the authorization code and PKCE against
  `<web>/login/oauth/…`, where the web base is `https://github.com` for the public API and the API
  base without `/api/v3` otherwise, then read `/user/emails`, `/user/orgs` and `/user/teams`. Both
  flows, and SAML's (see "SAML single sign-on"), end in `oidc::SignedIn::session` (invitation, cap,
  provisioning, session).
- **SCIM** (`scim/`): `/scim/v2/Users` and `/Groups` on the admin listener, matched before
  `/admin/v1`. Users are `human` users of the configuration named by their lowercased `userName`;
  their SCIM ids are `scim_user` identity records (deleted with the user), groups `scim_group`
  records; a group change rewrites its members' `role_bindings` from `group_mappings`. The generated
  token's verifier (and the previous one with its overlap) is the node value `scim_token`; a token
  configured by reference wins. `scim_status` holds the last request and error.
- **Active-user cap** (`users_cap.rs`): enabled humans plus disabled humans with a pending invitation;
  checked on user writes that activate a human, invitations, SCIM and SSO provisioning.
- **Provider hints** (`hints.rs`): `metadata.secret_hints` of provider reads, from the serving
  document's literal secrets of 16 characters or more.

## Alerting, notifications and audit delivery

`antirion-runtime/src/alerting/` holds the whole of it; the Admin API only calls it
(`antirion-admin/src/alerts.rs`, `delivery.rs`).

- **Nothing on the request path**: an event (`Runtime::send_alert`, `emit`) only queues work on the
  node's bounded queue (`Hub::submit`, 4096 jobs; a full queue drops and counts). One worker turns
  jobs into tracked delivery tasks; each holds a place in its target's queue (`queue_capacity` for
  webhooks, 1000 per channel) and retries with the sinks' backoff until `max_retry_duration` (an hour
  for channels). A paused webhook (`enabled: false`) keeps its deliveries waiting until it is resumed
  or their retry time runs out. `POST /test/reset` ends the deliveries in progress (`Hub::epoch`).
- **Rules** (`config.rs`, `evaluate.rs`, `metrics.rs`): metric rules are evaluated when the gateway's
  clock has moved `evaluation_interval` past the last evaluation (the loop wakes every 250 ms, so the
  test clock drives it), from the stored request-log records; in a cluster only the holder of
  the `alert_evaluator` lease evaluates. A condition must hold `for` before it fires; an open instance
  refires (a timeline entry, notifications unless snoozed) once `cooldown` has passed, and resolves
  after `resolve_after` clear. Event rules fire from the queued event, on the node that raised it.
- **Instances** (`instance.rs`, `engine.rs`): one JSON body per instance in `alert_instances`, with
  its evaluation state; every change is a compare-and-set on `version`, and the store keeps at most
  one open instance per fingerprint (a partial unique index), so concurrent raises end in one.
- **Channels** (`payloads.rs`, `deliver.rs`): Slack incoming webhooks, PagerDuty Events API v2
  (`dedup_key` = fingerprint), email through `smtp` (sent directly, so each attempt has an outcome)
  and webhook channels, which send the `alert.*` event to their webhook. Every attempt is a row of the
  delivery log (`deliveries`) and a timeline entry. Events are kept seven days for redelivery.
- **Signatures**: during a rotation's overlap the signature carries a `v1=` for each secret. The
  generated secret is written into the document (sealed at rest, redacted on read); the previous one
  is a sealed node value that every node reads back within two seconds (`status::report`).
- **Audit sinks** (`sinks.rs`): the audit log is the spool. Each sink has a position in it
  (`audit_sink_cursors`), starting at the governance change that configured it, moved only after a
  batch was accepted; entries older than `max_retry_duration` are skipped and logged as dropped. One
  node delivers (the `audit_sinks` lease).
- **Audit**: a write's `x-antirion-change-reason` is decoded in the router (`call::change_reason`) and
  recorded with every audit entry of the call; sensitive reads are `Call::audit_read` (`kind: read`).
- **Objectives and availability** (`slo.rs`): a 5xx is a failure (upstream when its code is
  `upstream_*` or `no_healthy_deployment`, or its stage `attempt`); everything else counts as a
  request only. Digests are sent by the evaluator when a user's `notifications.digest` period has
  passed since the last one (identity record `notification_digest`).

- **Browser push** (`antirion-runtime/src/push/`): `ece.rs` encrypts one `aes128gcm` record (RFC 8291,
  tested against the RFC's worked example), `vapid.rs` holds the cluster's P-256 key (a sealed node value
  written with `get_or_init`) and signs the ES256 token, `deliver.rs` is the queue: `push::run` takes its
  receiver at startup and sends each message in a tracked task, as client-influenced egress, retrying
  connection failures, 429 and 5xx, removing a subscription on 404 or 410. Subscriptions are identity
  records (`push_subscription`) with the endpoint and keys sealed; `alerting::engine::to_subscribers`
  pushes to users whose preferences ask for it.

## Known gaps in alerting

- Human text (summaries, mails, digests) is worded by `antirion-runtime/src/wording.rs` and
  `alerting/summaries.rs`. An instance does not carry its metric, so the channel payloads
  (`payloads::Wording`) read it from the rule in the serving configuration for the unit of their
  `Value` line; a built-in rule or a rule removed since shows the number with two decimals.
- Metric rules, objectives, availability and the signal report read request-log records from the
  store (`slo::records_between`, at most 200 000 per read), not the rollups, so very long windows
  and objectives are bounded by that. `budget_projection_ratio` reads unpartitioned USD budgets;
  utilization uses declared `requests` and `tokens` limits per minute.
- Change-request events (`change_request.*`) are defined but nothing raises them yet; the built-in
  rule templates are the console's (the contract has no operation for them).
  `budget_projection_ratio` still uses the rate over the whole elapsed period, not the budgets' own
  forecast.
- `secret_hints` are not reported for generated webhook secrets.

## Safety events and guardrail statistics

- **Facts** (`antirion-guardrails`): an `Evaluation` carries, besides its verdict and spans, the
  score and confidence of a finding, a digest of the normalised matched text (letters and digits,
  lowercased, sorted, deduplicated; never the text) and the gateway virtual keys found among secret
  matches (as `Secret`s). The pipeline's `guard::record` writes score and confidence into the
  record's entries and keeps a `Mark` per entry (digest, key ids whose secret verifies against the
  serving configuration) on the `Draft`, not in the record.
- **Recording** (`antirion-runtime/src/safety/`): settlement calls `Runtime::keep_safety` with the
  record and its marks (also when the store sink is off), which queues one `SafetyEventRow` per
  non-pass entry and adds every evaluation to per-hour, per-guardrail counts with a latency sketch;
  an asynchronous result that lands after settlement adds its own event (`only`). The fingerprint
  is the pepper's HMAC over guardrail, category and digest. The request-log writer
  (`write_logs`) then calls `flush_safety` (events in batches of 1 000, hours merged per node, read,
  merge, write) and `announce_safety` (`guardrail.event`, the `safety_event` built-in alert for
  critical events, `exposed` flags through `raise_flag`), so announcements happen once, on the
  recording node. Reads call `read_safety`, which writes the request log and the safety queue first
  and applies retention when due; `expire_logs`, `clear_logs` and `erase_logs` take events along.
- **Store** (`antirion-store/src/safety.rs`, `pg/safety.rs`): `safety_events` and `guardrail_hours`
  in `logs.db` or PostgreSQL (migration 55); lists are keyset on `(time_us, id)` descending.
- **Admin API** (`safety_routes.rs`): `safety.rs` (list, detail, `:review`), `safety_summary.rs`
  (grouping in memory over at most 200 000 events), `guardrail_stats.rs` (`/runtime/guardrails`
  over every definition of the document, disabled ones included).
- **Service checks**: `llm_judge`, `classifier` and the partner types are `Check::Judge`,
  `Check::Classifier` and `Check::Partner` (`services.rs`, `partners.rs`: requests and answer
  mapping); the pipeline's `GuardNet` sends them through `guardsvc.rs` (judges through the Model's
  first OpenAI Chat or Anthropic Messages deployment, services through the egress connector with a
  provider's credentials from `ProviderRt::authorize_service`).

## Benchmarks

`bench/` is its own workspace (its own `Cargo.lock`, excluded from the root one), so load-generation
dependencies never reach the gateway's lock file. Build it from inside `bench/`, which puts its
output under `target/bench`: `cd bench && cargo build --release --locked`. It holds:

- `mock/`: the benchmark mock (`antirion-bench-mock`): OpenAI Chat and embeddings, Anthropic
  Messages and MCP streamable HTTP from pre-rendered bodies, HTTP/1.1 and h2c, latency profiles
  selected by a path prefix (`/L60/v1/chat/completions`), a precise timer thread for the scripted
  delays, `/__bench/null/*` (a collector that discards) and `/__bench/hang/*` (one that never
  answers);
- `loadgen/`: the open-loop generator (a pacer thread spawns each request at its intended time,
  latency from the intended time into HdrHistograms), the closed loop, HTTP/1.1 and h2c pools, SSE
  timing (TTFT and gaps);
- `corpus/`: request bodies and keys from a fixed seed; `corpus/data/` is committed and checked
  against the generator (`antirion-bench corpus`);
- `report/`: summaries, pass conditions per cell (`gates.json`), the regression gate against a
  baseline run, and `report.md`/`report.html`;
- `cli/`: `antirion-bench` (`selftest`, `cell`, `run --suite`, `report`, `list`, `env`);
- `matrix/` (scenarios and cells as data), `profiles/` (gateway profiles in the configuration's
  own form, with generated keys, models and MCP servers), `topologies/`, `targets/`.

Keys of a profile are `${env:AK_<name>}` references whose secrets `antirion-bench` derives from
the corpus seed and passes in the gateway's environment. The criterion micro-benchmarks are
`crates/antirion-mcp/benches/session_token.rs` and `crates/antirion-router/benches/route_selection.rs`;
`antirion-bench` runs them with `cargo bench` and reads criterion's raw samples.

## Crate map

Layers go from 0 (no workspace dependencies) to 7 (the binary). A crate depends only on lower
layers; `cargo xtask layers` enforces it. The one same-layer edge is `antirion-error` →
`antirion-core`. `antirion-json` has no workspace dependencies and sits at layer 0, so the config
crate can use its strict parser.

| Layer | Crate | What it holds today |
|---|---|---|
| 0 | `antirion-core` | `UtcMicros`, the `Clock` trait and `SystemClock`; `RequestId`, `ResourceUid`; `Stage`; `Envelope`; header names; `SplitMix64`; the audit record; `Peer` (the connection facts a listener attaches) |
| 0 | `antirion-error` | `Code` (the closed table with every dialect's names), `Reason` and the `reasons!` macro, `GatewayError`, rendering in every envelope |
| 0 | `antirion-json` | `skim`: strict validation with depth, duplicate members, line and column, and top-level member spans; `Splice` |
| 0 | `antirion-console` | the console's static export embedded at build time (`console/out`, or a placeholder page when it is absent), path resolution for both export layouts, content types, cache policy and the console's security headers |
| 1 | `antirion-mail` | outgoing mail: an SMTP client for one message (STARTTLS, implicit TLS, `AUTH PLAIN`/`LOGIN`, dot-stuffing) and RFC 5322 messages with a base64 UTF-8 body |
| 1 | `antirion-config` | document parsing (strict JSON, YAML 1.2 core with duplicate and tag checks), the embedded schema and its evaluator, semantic rules, secret references and redaction, scalar formats, typed views, YAML output, the document model (collections, singletons, static keys) |
| 1 | `antirion-wire` | the SSE parser (original event bytes kept for passthrough, size limit, clean-end completion), NDJSON and streamed JSON-array parsers (`jsonstream`), multipart form parsing, field replacement and the incremental `Scanner`, content codings: parsing every `Content-Encoding` field, bounded and incremental decoding (gzip, deflate, br, zstd) |
| 2 | `antirion-catalog` | the model catalog: the bundled capability values and prices, the document's `catalog` block that extends or replaces them, and the lookup by provider type and base model (exact, then the longest `*` prefix) |
| 2 | `antirion-dialects` | the endpoint catalog, path normalisation, envelope selection, body facts (`model`, `stream`), request validation tables (`validate/`), embeddings and rerank in other providers' shapes (`embed`, `rerank`), cross-dialect translation through an internal representation (`xlate/`: client and upstream codecs for requests, responses and streams, warning entries, refusals, the signature envelope, citations, hosted tool equivalents, what the reasoning replay store keeps) |
| 2 | `antirion-egress` | `EgressClient` (pools per trust class, TLS profile and egress policy; HTTP/1.1 and h2 via ALPN, or the version a destination's profile requires (`HttpVersion`: `http1`, `h2` that fails without ALPN `h2`, `h2c`); h2 with prior knowledge fanned out over several connections per origin (`h2pool`); one resend when a pooled connection closed before answering; response bodies decoded with `antirion-wire`; a body's streamed `Tail`, sent once; WebSocket upgrades on a connection of their own, `upgrade.rs`), the SSRF catalogue (URL rules on the raw string, address rules on every resolved address, connect to the validated address), TLS profiles, header hygiene (always-deny list, forward/deny/set/response_forward), config checks of `security.egress` and `policies.headers`, typed `TransportError` and `BodyError`, the egress reasons |
| 2 | `antirion-identity` | key and admin-token format, peppered verifiers, data-plane credential selection, allow-list patterns, the key permission catalogue, end-user identity, admin roles, scopes and the admin permission catalogue, the auth reasons |
| 2 | `antirion-redis` | connections to a Redis-protocol server or a Redis Cluster (`RedisConn`: lazy, `ConnectionLike`, redirections retried while a slot moves, scans over every primary), the `topology` setting and the one-slot key prefix |
| 2 | `antirion-store` | `Store` (closed enum: SQLite for standalone, PostgreSQL for cluster mode): config generations, audit log, admin tokens, admin sessions, token role bindings, node values, spend ledger lines and their budget charges, alert instances and kept events (`alerting.rs`), the delivery log, queue reports and audit-sink positions (`deliveries.rs`), use times (`marks.rs`), key anomaly flags and key history (`anomalies.rs`), safety events and guardrail hours (`safety.rs`); `pg/`: the pool, migrations under an advisory lock, the configuration notification listener, the store's own health, and what only a cluster shares (node rows and drain commands, job leases, request-log records, Responses state, logins, cache epochs), provider-scoped objects (`objects.rs`: gateway file and batch ids) |
| 2 | `antirion-obs` | the metrics registry (every family of the catalogue, label truncation, the per-family series cap with `_overflow`, configurable buckets, Prometheus 0.0.4 and OpenMetrics expositions, the process collector), the request-log record's JSON Schema and bounded fields (bodies, metadata, header values), the overhead formula (`timing`), W3C trace context, samplers, spans and their OTLP JSON and protobuf encodings, the batch retry schedule, the `observability` settings |
| 3 | `antirion-providers` | compiled providers and deployments, resolved capability flags, the provider-type catalog (the supported types, hosted presets); building an upstream request (credential, TLS profile, egress policy, operation path overrides, base-URL queries) and a passthrough mount's request (`passthrough`); the token cache over GCP and Azure AD sources, AWS credentials and STS `AssumeRole` (`creds/`); the BYOK credential mode |
| 3 | `antirion-router` | Models, aliases and wildcards (`resolve`); route trees compiled into an arena with paths, params, policies and config-time validation (`tree`); the lazy attempt plan with transition labels and exclusion reasons (`plan`), each node's children ordered lazily by its algorithm, one pass over them for the first position and no name formatting (`select`); predicates (`predicate`); keyword and regex classifiers and the routing text (`classifier`); what call-out classifiers and cost-quality scorers ask and how their answers are read, the calibration window, continuation turns and the embedding cache (`content`); pool scheduling: metric names and reading, assumed load, prefix chains and indexes, the filters and the score (`pool`, `plan/pool.rs`); tag filtering (`tags`); `upstream_body` edits (`body_edit`); the prompt-prefix key (`prefix_key`); SWRR, PeakEWMA, success and in-flight state, mirror slots, score windows and replayed classes (`state`); session bindings (`affinity`) |
| 3 | `antirion-resilience` | failure classes and the decision table (`class`), classification of transport failures and upstream answers (`classify`), upstream hints (`hint`), the retry and timeout policy with the backoff (`policy`), health settings and the health registry: breakers, cooldowns, keys, manual disables, records shared with a cluster (`settings`, `health`), the cross-request retry budget (`budget`), the attempt loop generic over `Planner` and `Sender` (`engine`) |
| 3 | `antirion-limits` | limit and budget declarations of every level, their config checks (`check`), the applicable set, the token estimator, exact counter math (GCRA, sliding window, concurrency, budgets), the reservation engine over `MemoryCounters` or `ValkeyCounters`, degraded mode, pricing, cost, ledger lines, the bounded ledger window (`book`), a node's accounts (`spend`), rate-limit and cost headers (see Limits) |
| 3 | `antirion-mcp` | the MCP gateway (both eras, multiplexing, namespacing, grants, encrypted session tokens, upstream Streamable HTTP and HTTP+SSE, list caches, discovery, tool fingerprints, OAuth metadata) and the A2A proxy; their egress and request-log facts; hooks for limits and guardrails |
| 3 | `antirion-guardrails` | guardrail definitions and their configuration rules (`compile`, `check_document`), attachment across policy levels (`attach`), segments per dialect with their write-back (`segments`; embeddings, images, rerank and transcriptions in `endpoints`), the regex, keyword, PII and secret checks, `json_schema` (`structured`), `tool_permission` (`tools`), `length`, moderation windows and answers, the webhook contract (segment and body modes), the bound on in-flight external checks (`limiter`), transforms, redaction with format-preserving masks (`preserve`), the stage runner over a `Net` it is generic over (`runner`: the stage, and `conclude`, what each check finds), window mode for streams (`window`), the judge and classifier checks (`services`) and the partner services (`partners`) |
| 3 | `antirion-ext` | extensions (`plugins`): compilation and configuration rules, attachment (`resolve`), the hook payload, signature and answer contract (`hook`), JSON Patch (`patch`), the stable in-process API (`api`: `Extension`, `Registry`, `install`) and the process registry, the built-ins (`static_response`; `test_script`, registered in test mode), Wasm components (`wasm/`: loading by digest, the component cache, epoch-bounded calls, `oci://` blobs) |
| 3 | `antirion-cache` | the response cache: the effective `policies.cache` (`policy`), the `x-antirion-cache-control` grammar and its checks (`directive`), the canonical key (`jcs`, RFC 8785; `key`: the key document, the semantic partition key and body normalisation), entries and what they record (`entry`, `meta`), the memory backend and the Valkey backend with their entry indexes, listings (`listing`), cutoffs (`invalidate`), invalidation rules (`rules`), the node's `Cache` (settings, timeouts, epochs and cutoffs, hit and miss counts, console reads), semantic eligibility and polarity tokens (`semantic`), the node's vector store (`vectors`), leaders of identical concurrent misses (`flights`), prompt-cache breakpoints (`prompt`), the cache reasons |
| 4 | `antirion-runtime` | `ConfigState` (document plus metadata), `Snapshot` and its builder, `Generations`, runtime state, the node's accounts and spend ledger (`Runtime::accounts`, `ledger`: `record_spend`, the store-backed budget rebuilds and spend reports), alert webhooks, the commit pipeline (`Runtime::mutate`), apply merging and change sets, test mode, probes and the liveness watchdog, admission control (`admission`), the drain state (`drain`), encryption of stored secrets (`atrest`), `SecurityRt` (CORS, limits, egress policy, header rules per level, client IP, listener TLS, HSTS), admin authentication and sessions, key usage, the request log store, the response cache (`RuntimeState::cache`, its epochs persisted in the store), the secret-file watcher, the ledger writer and its spool (`spool`), single sign-on logins (`logins`), cluster mode (`cluster/`), alerting, audit sinks, objectives and notifications (`alerting/`), OTLP metric export (`otlp_metrics`) |
| 5 | `antirion-pipeline` | the data plane: `accept`, `authenticate`, `parse`, `resolve`, `admit` and settlement (`account/`), extensions and input guardrails, the exact cache (`cache/`), `route`, the attempt loop, output guardrails, `respond`, the stream relay, the request-log record; uploads (`upload.rs`), embeddings and rerank cells (`embed.rs`), Files, Batches and Realtime (`files.rs`, `batches.rs`, `realtime.rs`) |
| 5 | `antirion-admin` | the Admin API: resources, settings, config document operations, audit, status, health, metrics, test endpoints, admin tokens, key rotation, role bindings and roles, sessions, request-log reads and lists, provider types and capabilities, provider and deployment runtime state, budget counters and adjustments (`budgets.rs`), spend reports and effective prices (`spend.rs`), cache entries, epochs and status (`cache.rs`), alert instances, their actions and reports (`alerts.rs`), delivery logs, tests, secret rotation, delivery and audit-sink status, objectives, availability and notifications (`delivery.rs`) |
| 6 | `antirion-server` | listeners (each attaches `antirion_core::Peer`: address and TLS), TLS on either listener with certificate reload, the first-head framing and header-size checks, HTTP/1.1, h2 and h2c, connection draining, the data-plane request count the drain waits for, response compression (`compress`) |
| 7 | `antirion` | the CLI, startup, the stop sequence (`lifecycle`), the watched config file |
| dev | `antirion-testkit` | `FakeClock`, `ScratchDir` |

## Where things live

- **Threads**: the admin plane and the background tasks run on the multi-threaded runtime; data
  connections are served on the data threads (`antirion-server/src/threads.rs`, one per available
  CPU, each a single-threaded runtime). On Linux each data thread accepts on its own listener of a
  `SO_REUSEPORT` group (`listen.rs`); elsewhere one accept loop on the multi-threaded runtime hands
  each connection to the data thread serving the fewest. A data thread serves a connection whole: the connection, the request tasks it spawns and the HTTP/1.1 upstream
  connections they open (`antirion-egress` keys those pools by the single-threaded runtime's
  thread), so a request wakes no other thread. The requests of an HTTP/2 connection are spread over
  the data threads in turn (`DataThreads::spread`, `DataPlane::serve_on`), so one connection's
  streams are not bound to one CPU.
- **A request's path through the data plane**: `antirion-pipeline/src/lib.rs` (`DataPlane::serve`
  spawns each request on the runtime's `TaskTracker`, so a disconnect never cancels it and a panic
  becomes a 500 with reason `panic`), then `accept.rs`, `auth.rs`, `infer/` (parse through
  respond), `local.rs` (the Models list, stored responses) and `mcp.rs` (MCP and A2A endpoints:
  the head checks before authentication, `auth.rs`, the body through `parse::read`, then the
  hand-off to `antirion-mcp` and the request-log record).
- **Inference, stage by stage**: `parse.rs` (content type, content encoding, body, schema checks; a JSON body is parsed into a value once, `Parsed::doc`, which every later stage reads),
  `infer/` (model, access through `authorize.rs`, capabilities and translation feasibility through
  `xlate.rs`, admission, guardrails, cache),
  `routing.rs` (pins, affinity, the attempt plan driven by `antirion_resilience::run`, the binding
  written on success), `route.rs` (request facts for conditions, the eligibility filters, the
  planner that reads the plan as tiers, the no-eligible error), `attempt.rs` (the `Sender`: one
  attempt on one position with one key: route params applied to the client body, the upstream body
  from `upbody.rs` (identity, or the request encoded again for this deployment's dialect), the
  credential, its timers and statistics), `failure.rs` (a class becomes the
  client's error), `outcome/` (what an upstream response becomes: the error classification, the
  checks that make a 2xx unusable, the non-streaming body), `streamed.rs` (a streaming response),
  `relay/` (streams: hold until the commit point, then a tracked task relays through a byte-bounded
  queue; `ChannelBody` releases buffer permits as the server takes frames, and a closed queue is
  the client disconnect), `cell.rs` and `apis.rs` (identity or translation per deployment, and the
  API of each response), `policy.rs` (policy members merged across levels).
- **Every attempt goes through the plan**: whatever the client's dialect and the deployment's
  provider type, `infer/` hands the request to `routing::run`; a directly addressed deployment
  (`provider/model`) is a one-leaf route, and a pin (`x-antirion-pin-deployment`, the test header,
  or the deployment that created a `previous_response_id`) is a pinned plan. The `Sender` builds the
  cell's upstream request (native, translated or `InvokeAsMessages`), signs or authorizes it
  (SigV4, GCP tokens, static keys chosen by the loop's key rotation) and classifies the answer.
- **Egress**: every upstream request is built by `antirion-providers` (credential, TLS profile, egress
  policy, operation path), completed by `antirion-pipeline/src/egress.rs` (forwarded client headers,
  `set`, `user-agent`, `accept-encoding`) on every attempt and sent through `EgressClient::send`,
  which validates and connects (`antirion-egress/src/connect.rs`) and decodes the response body. The
  client sees the response headers `response_forward` allows (`egress::inbound`).
  An outbound body is a `Payload`: slices of the client's bytes around the spliced members, sent as one frame each, so an identity body is never copied (`BodyEdit` joins it only when a provider edits members). `OutboundRequest::resend_when_closed` allows the one resend on a pooled connection that closed
  before answering; providers set it, but the attempt loop clears it (a closed connection is an
  `upstream_reset` the loop decides on) and MCP and A2A calls never set it. `connect_timeout`
  overrides the client's connect bound per request, and `fresh_connection` keeps the request off
  pooled connections (the retry after a refused stream). A reader that stops before the end of a
  body (media past `max_bytes`) drops the body and awaits `InboundResponse::abort`: the HTTP/1.1
  connection's I/O fails, so it is closed unread and never pooled again (`antirion-egress/src/abort.rs`).
- **Client IP**: `antirion-pipeline/src/client_ip.rs` is the only place the data plane derives it
  (the listener's `Peer` plus `security.client_ip.trusted_proxies`); everything else reads
  `Ctx::client_ip`.
- **Guardrails and extensions of a request**: `infer/` builds the `RequestGuard` (`guard.rs`: the
  effective guardrails, stream settings, webhook facts) and the `Hooks` (`extend.rs`) after `admit`,
  refuses a stream in `reject` mode, runs the `request` hooks, then the input stage (an input block or
  an extension's own answer settles with no upstream call, so the reservation is refunded); a changed body is
  serialised again and translation feasibility decided again. Complete responses get the output stage
  and the `response` hooks in `outcome/` (`guard::complete_response`), inside the attempt: a block or
  a failure is a `GuardrailBlocked` failure, final for the attempt loop. `x-antirion-guardrail-select`
  adds guardrails to the `RequestGuard` (`select.rs`); `parallel_with_call` checks come back from the
  input stage and run alongside the routing call (`alongside.rs`), which holds the answer until they
  pass. Transcription answers get the output stage in `outcome/transcript.rs`. The test endpoint
  reaches the evaluation through `RuntimeState::guard_tests`, which the server installs with the
  pipeline's `guardtest::Tester`. Buffer mode with `buffer_heartbeat` commits at the first beat
  (`relay::hold`), and the output verdicts of a stream follow as a trailer. Streams get an `OutGuard`
  (`relay/guard.rs`) between the shaper and the writer: window and buffer modes hold the commit point
  in `relay::hold` until the first release. Webhook and moderation calls are `external.rs`
  (`GuardNet` implements the guardrail crate's `Net`). Verdicts reach the headers through
  `Ctx::guard` (`guard::finish`), the log record (`guardrails`, `extensions`, internal spans) and the
  metrics through `guard::record`.
- **An MCP request**: `antirion-mcp/src/http.rs` (`serve`: body, JSON-RPC parsing, era detection),
  then `legacy.rs` (initialize, sessions, GET stream, DELETE, stateless mode) or `modern.rs`
  (validation, `server/discover`); list methods in `fanout.rs` and `lists.rs`, item requests in
  `route.rs` and `relay.rs`; one upstream server per request is a `Link` (`link.rs`).
- **Error rendering**: `antirion-error/src/render.rs`. Every data-plane error goes through
  `Ctx::error` (`antirion-pipeline/src/ctx.rs`), which adds the headers every response carries.
- **Config validation**: `antirion_config::validate` runs the schema
  (`antirion-config/src/schema/`), restores `[REDACTED]` secrets from the previous document, then
  the semantic rules (`semantic.rs`). Secret resolution is `secrets::expand_document`.
- **Authentication and authorization on the data plane**: `auth.rs` (checks 1–9: credential,
  expiry, enabled states, client IP, endpoint scopes, then `controls.rs` for the permission-gated
  header controls and the end-user header) and `authorize.rs` (checks 10–13: model lists before
  existence, direct provider addressing, provider and deployment lists). The identity part of the
  snapshot is `antirion-runtime/src/identity.rs`: keys by kid (a rotated-out secret keeps its own
  entry until its grace ends), the tenancy chain with each level's access block, users and the
  `auth` settings.
- **The request log, metrics and traces**: each request that passed `authenticate` gathers facts in
  `Ctx::log` (`antirion-pipeline/src/reqlog/`), a `Draft` shared with the relay task of a committed
  stream. Stages write their own facts (`observe.rs`: content policy and zero data retention, the
  record's session value (`meta.rs`; the session header and `x-antirion-metadata` are parsed once,
  by `controls.rs`, for routing and the record), request parameters, the cache lookup and routing
  decisions (counted per plan position in `route.rs`); `outcome/`: one
  `AttemptFact` per attempt with its span id, key, wait, transition label and timer, recorded by
  the attempt loop's sender; `relay/`: stream instants, relay time and the
  assembled body). `reqlog::finish` adds the headers that depend on the record (log id, overhead,
  `observability` in `x-antirion-degraded`) and the request is settled (`settle.rs`) once the head
  is built and any committed stream has ended, whichever is last: timing, cost, the record
  (`json.rs`), metrics and spans (`emit.rs`), then the store and the sinks. Requests without a record
  (refused before or at `authenticate`) still count in the request metrics. The trace context of
  every request is `Ctx::trace` (`trace.rs`); each attempt sends it upstream with its own span as
  parent. See "The request log and analytics" for where records go.
- **Sinks and exporters**: `antirion-runtime/src/telemetry/`: one bounded queue and one tracked
  worker per HTTP sink and for the span exporter, reconciled with each generation
  (`Runtime::observe_generation`, called after every swap); a full queue drops and counts, a failing
  sink sets `observability` in `x-antirion-degraded`. Metrics are served on the admin listener at
  `observability.metrics.path` (`antirion-admin/src/ops.rs`).
- **Admin authentication**: `antirion-runtime/src/admin_auth.rs` (bearer tokens with their user's
  and their own bindings, sessions re-checked on every use); the router adds the `Origin` check for
  cookie-authenticated writes.
- **Committing a change**: `Runtime::mutate` (`antirion-runtime/src/commit.rs`). Metadata-only
  changes (a key rotation, a key disabled because its owner was deleted) are `MetaEdit`s and commit a
  generation on their own.
- **Admin routing**: `antirion-admin/src/lib.rs` (`route` authenticates, `handle` dispatches on
  path segments).
- **Startup**: `antirion/src/startup.rs`; the stop sequence is `antirion/src/lifecycle.rs`.

## The request log and analytics

- **Records** (`antirion-runtime/src/reqlog/`): settlement hands each record to `Runtime::keep_log`
  with the facts the record does not carry (`Extras`: the key's owner, the served leaf). `facts.rs`
  reads what analytics count of it and builds the store row: indexed columns (tenancy, key, owner,
  Models, provider, status, cache, environment, latency, cost, tokens, tags and guardrail facts as
  `logs::set` strings), the JSON without `content`, and the content apart (so `content_retention`
  can drop it). Rows queue in `RequestLogs` and one task (`Runtime::write_logs`) writes them every
  quarter second; every read writes what is queued first, so a node reads its own records at once.
  Every node reads the store directly (the shared PostgreSQL in cluster mode); nothing is copied
  between nodes. Retention (`sinks.store.retention`, `content_retention`) runs at most once a minute
  of gateway time, from the writer and from reads, and from the cluster's `retention` job.
- **Store** (`antirion-store/src/logs/`): `request_logs` and `log_rollups`, in `logs.db` beside
  `state.db` in standalone mode (normal synchronisation, its own thread). Queries are written once
  for both backends (`sql.rs`): repeated values are `IN`, set columns `LIKE '%␟v␟%'`, sorting is
  keyset pagination on `(sort column, started_us, id)` with sentinels `-1` for absent TTFT and cost.
- **Rollups**: per minute, hour and UTC day, per node and per combination of the rollup dimensions
  (`ROLLUP_DIMS`), the measures and three DDSketch-style sketches (`antirion-obs/src/sketch.rs`:
  latency, TTFT, output tokens per second; 1 % relative error, exact merges). Each node adds to its
  own rows (read, merge, write in one transaction). `Runtime::aggregate` cuts a query's range into
  pieces aligned to the coarsest grain the bucket boundaries allow (whole days, hours, minutes) and
  reads the partial minutes at the edges, and any query whose filters or dimensions the rollups lack
  (`q`, latency or cost ranges, tags, guardrail facts, `metadata.*`, end user, client IP), from the
  records of the range.
- **Admin API**: `logquery.rs` parses the selection every log and analytics endpoint shares (and its
  rollup form), `logs.rs` (list, totals, sort cursors bound to their sort), `analytics.rs` (usage
  buckets, time zones through `PeriodSpec`, the latency histogram), `export.rs` and `tail.rs`
  (streamed bodies), `views.rs` (saved views as `log_view` identity records).
- **Record additions** (`antirion-pipeline/src/reqlog/`): `tool_call_count` (complete responses in
  `apis::tool_calls`, streams counted by the classifiers), `query` without credential parameters,
  `replay_of` (honoured only on keys the playground minted), `client_geo`, guardrail entries with
  `category`, `severity` (configured, else by action), `score`, `confidence` and match `spans` in
  characters (`runner::Located`), and `timing.stage_ms` from the stage marks (`Draft::stage`) minus
  upstream time and waits, the remainder of the overhead (a stream's relay) in `respond`.
- **Stored content** may be scrubbed before it is written (`ContentFact::scrub`,
  `antirion_guardrails::scrub`: every PII entity and secret detector) under
  `policies.observability.content.redact_pii`.
- **GeoIP** (`antirion-runtime/src/geoip.rs`): the database is opened while the snapshot is built and
  reused while its file is unchanged.
- **Playground** (`antirion-admin/src/playground.rs`, `antirion-runtime/src/playground.rs`): the
  server installs a `LocalPlane` (`antirion-server/src/local.rs`): one HTTP/1.1 exchange over an
  in-memory pipe served by the same `DataPlane`, so a playground request takes every stage. Keys are
  minted as key resources with `created_by: playground` (the audit entry's actor), kept with their
  secret in node memory per caller and project, and deleted by `Runtime::sweep_playground_keys`
  once expired. Replays send the stored request body, patched, with `x-antirion-replay-of`.

## Operations

- **Admission** (`antirion-runtime/src/admission.rs`): inference requests take a slot in
  `infer/` just before limits and budgets; the `Permit` is kept in the request's `Draft` and
  released with its in-flight gauge, so a stream holds it until its last byte. Each waiter that is
  admitted hands the next free slot on when it resumes, so requests admitted together start in
  queue order. `server.admission` is the one dynamic part of `server`: the startup file's block is
  applied with the other dynamic keys, the stored document keeps it under `server.admission`
  (`Runtime::check` strips every other static key), `PATCH /settings/server` may change only it,
  and `Runtime::observe_generation` reconfigures the queue. The document's block wins over the
  startup file's.
- **Drain** (`antirion-runtime/src/drain.rs`, `antirion/src/lifecycle.rs`): `Drain::begun` fails
  readiness and asks the data connections open at that moment to close (HTTP/1.1 after the current
  response, HTTP/2 with `GOAWAY`), `closed` stops the listeners and asks every other connection to
  close, `cut` ends what still runs. HTTP/1.1 data connections close through their clock
  (`antirion-server/src/clock.rs`), never through the HTTP stack's own graceful close: one idle
  when the listeners close gets a FIN at once, one with a request head arriving or a request in
  progress closes after that response, even when the node stops, and every close keeps reading
  and discarding for a second so a request crossing the FIN meets an end of stream, not a reset.
  HTTP/1.1 cannot announce a close, so a request a client sends on an idle connection while that
  FIN is in flight gets no answer: the client sees a clean end of stream before any response byte
  and has to retry it on a new connection. Every request's disconnect token is a child of `cut`, so a cut
  request ends through the client-disconnect path; `infer::run` turns that into 503
  `drain_deadline`, and the stream relay into the in-stream error event. The server counts
  data-plane requests (probes excepted) until their response body ends: that count is the
  `in_flight` of readiness and of the drain action, and the drain waits for it. While draining,
  HTTP/1.1 responses carry `Connection: close`; once new requests are refused, `accept` answers 503
  `node_draining`.
- **Server timeouts** (`Statics::http`, `server.http`): the first head of a data connection is
  read within `header_read_timeout` (`head.rs`: nothing arrived → closed silently, part of a head →
  408); afterwards `clock.rs` closes a connection with no request in progress after `idle_timeout`,
  and an HTTP/1.1 connection whose next head started arriving but is not complete within
  `header_read_timeout` (without a response). `body_read_timeout` bounds the gap between body frames
  in `parse` (408). The admin listener keeps fixed bounds.
- **Stored secrets** (`antirion-runtime/src/atrest.rs`): `Runtime::mutate` stores the document with
  every literal secret value sealed (AES-256-GCM, a random nonce per value, the value's JSON pointer
  as associated data) as `enc:v1:<base64>`; startup opens them. The generated pepper and MCP session
  key are sealed node values. The keys are `security.secrets.encryption_keys` of the startup file,
  then `encryption.key` in the data directory, which is generated when none is configured.
- **Compression** (`antirion-server/src/compress.rs`): applied to complete data-plane responses
  after the pipeline; event streams and bodies of unknown size are never compressed.

## Cluster mode

`cluster.mode: cluster` (never with `--test-mode`) needs `store.backend: postgres`,
`cluster.counters.backend: redis` and `security.secrets.encryption_keys` (the same keys on every
node; the store never holds them). Startup (`antirion/src/cluster.rs`) opens the PostgreSQL store,
which migrates under an advisory lock (`store/src/pg/schema.rs`; a binary older than a migration's
`min_binary` exits with status 78), and builds the accounts over `ValkeyCounters`
(`antirion_limits::store::Counters`). The node id is `cluster.node_id` or a fresh UUIDv7.

- **TLS to PostgreSQL** (`store/src/pg/tls.rs`): the URL's `sslmode` and `sslrootcert` with libpq's
  meaning (`prefer` by default; `require` verifies only with a CA file; `verify-ca`, `verify-full`;
  the bundled web PKI roots plus `sslrootcert`, or `sslrootcert=system`). The two parameters are
  taken out of the URL before the driver parses it.
- **Local snapshot** (`runtime/src/cluster/local.rs`): `cluster-snapshot.json` under
  `store.embedded.path` holds the serving generation (secrets sealed as in the store), the sealed
  generated pepper (never the MCP session key) and the time the follower last confirmed that
  generation against the store. It is written before a commit returns (so a key just created
  survives an immediate restart), when the follower applies a generation, at startup once the
  configuration is read from the store, and renewed every 10 s; writes are serialized and never
  replace a newer generation with an older one. A node
  whose store is unreachable at startup serves it with the store down (`PgStore::offline`: the
  schema is checked before the probes bring the store back), counts its keys as verified at that
  time, reads the whole ledger once the store answers (`replicate.rs`), and applies the startup
  document then (`antirion/src/deferred.rs`), trying again while the store fails. The listeners are
  bound before the store is opened; without a snapshot the node retries 1 s doubling to 30 s while
  they answer from `server/src/waiting.rs`: `/-/livez` 200, `/-/readyz` 503 `not_ready` with
  `degraded: ["store"]`, every other request 503 `store_unavailable` (TLS from the startup
  document's `security`). A store that fails or stops answering at any step before the node is
  ready (opening it, the first reads in `cluster::open`, restoring state and applying the startup
  document in `deferred::first_contact`) sends the node back to that wait, never to an exit: only an
  invalid document ends startup with a config error (`ApplyError`). The MCP session key is retried
  every `probe_interval` until read.
- **Identity-store outage** (`pipeline/src/auth.rs`, `runtime/src/usage.rs`): while the store is
  down, a virtual key authenticates only if its kid passed verification less than
  `auth.store_outage_grace` ago (recorded per kid while the store is up; a disabled or expired key
  keeps its refusal); any other key gets 503 `identity_store_unavailable` with `Retry-After: 5`.
  JWTs are verified by signature and are not affected.
- **Metrics**: `antirion_dependency_up{dependency}`, `antirion_spool_bytes`,
  `antirion_ledger_rows_dropped_total` and `antirion_clock_skew_seconds` are refreshed at each
  scrape (`runtime/src/probes.rs`).

- **Configuration** (`runtime/src/cluster/sync.rs`): a commit is refused by the store unless the
  stored generation is the one before it, and `Runtime::mutate` first catches up with the store under
  the write lock, so every write builds on the newest generation. Commits `NOTIFY antirion_config`;
  every node listens and also polls every `config_sync.poll_interval`, and applies only newer
  generations.
- **Heartbeat** (`members.rs`): the node row in the store, membership keys in Valkey (the known node
  count for local shares; the store's count when Valkey is down), drain commands from other nodes,
  and the clock skew.
- **Probes** (`probe.rs`): a store that failed is probed every `probe_interval` and used after three
  good probes (`PgStore::probe`); then the spool is replayed and the node catches up. The counter
  store likewise; on recovery the engine replays queued settlements and the consumption its local
  shares admitted. `Runtime::degraded` names `store` and `counters` for readiness and every
  data-plane response (`antirion-pipeline/src/lib.rs`).
- **Jobs** (`jobs.rs`): `counter_reaper` and `retention`, each under a 15 s lease renewed every 5 s;
  `GET /cluster` shows the leases.
- **Shared reads** (`replicate.rs`): request-log records go to the store and other nodes' records
  come back into each node's in-memory log; every node reads every ledger line into its `Book`;
  cache epochs are raised in the store and read back. Reads overlap the last 5 s and dedupe by id.
- **Ledger** (`runtime/src/spool.rs`): settlement queues lines, flushed every
  `store.ledger.flush_interval`; while the store is down they go to `store.spool.path` (records with a
  length and CRC, synced per batch) up to `store.spool.max_size`. A full spool refuses budgeted
  requests with 503 `ledger_unavailable` (unless `on_full: drop`). Lines are idempotent by
  `(request_id, line)` everywhere, so replays and crashes never count twice.
- **MCP sessions** need nothing shared but the session key (`mcp_session_key`, first writer wins in
  the store) and the revocations, kept in the counter store: a deletion is written there before its
  204, every request with a session token asks it (`cluster/revocations.rs`), and when it cannot be
  asked the token is served with `session_store` in `x-antirion-degraded`.
- **Keys created elsewhere**: a key unknown to the serving generation makes the node read the
  store's generation once before it is checked (`auth::catch_up_for_unknown_key`); requests waiting
  together share one read.
- **Health** (`cluster/health.rs`): see Resilience.
- **Affinity** (`cluster/affinity.rs`): bindings are written to and read from the counter store
  (keys `<prefix>aff:<sha256 of the binding key>`); while it cannot be read, sessions are routed
  without affinity and responses list `affinity` in `x-antirion-degraded`.
- **Limits and budgets** evaluated without the counter store (`local_share`, `fail_open`, a queued
  settlement) list `limits` or `budgets` in `x-antirion-degraded` and the log record's `degraded`
  (`account::Slot`); a refused local share carries the header on its 429. `limits.lease_ttl` is the
  node lease, renewed every `probe_interval`; a concurrency refusal first expires the reservations
  of nodes whose lease ran out.
- **`?wait=`** on a write (`Call::mutated`) polls the live nodes until every ready one serves the
  new generation, and says whether it did in `x-antirion-config-propagated` (and `propagated` of
  `config:apply`).
- **Bootstrap token**: cluster mode refuses to start without `auth.admin.bootstrap_token` of at
  least 32 characters.
- **Responses state and logins** are written to the store before the client sees the response (or
  the redirect), and read from it on a local miss.

The PostgreSQL tests run when `ANTIRION_TEST_POSTGRES_URL` names a server the tests may create
databases on (`cargo test -p antirion-store`); the TLS test when `ANTIRION_TEST_POSTGRES_TLS_URL`
names one that accepts TLS, with its `sslmode` and `sslrootcert`.

## Conventions

### Errors

- The data plane has one error type, `GatewayError`. Build it from a registered reason when there is
  one: `GatewayError::with_reason(reasons::X, Stage::Y, Message::Fixed("…"))`. The code comes from the
  reason. Messages are fixed sentences, or `Message::built(…)` from safe parts (names the client
  sent, paths); never another error's `Display` text, never a secret.
- Reasons are declared with `antirion_error::reasons! { NAME = "name" => Code, … }` in the crate
  that owns the rule (`antirion_error::reason::hygiene`, `antirion_identity::reasons`,
  `antirion_pipeline::reasons`, `antirion_server::reasons`).
- The admin plane has its own `AdminError` (`antirion-admin/src/error.rs`) with the Admin API
  envelope; the two never mix.
- Library crates use `thiserror` enums with typed variants; `anyhow` only in the binary, `xtask`
  and tests.

### Configuration

- The stored document is a `serde_json::Value`; the schema, not Rust types, is the source of
  truth. Code that needs settings reads typed views (`antirion_config::views`) of a validated,
  secret-resolved document. Add a view field when you need one; views default every absent field.
- Document paths are `DocPath`; they render in the Admin API's style (`routes.r.tree.children[1].weight`).
- Collections, singletons and static keys are listed once in `antirion_config::model`.

### Snapshots and runtime state

- A change is `Runtime::mutate(|current| -> Mutation)`: the closure builds the whole resulting
  document from the current `ConfigState` under the write lock; `mutate` validates it, resolves its
  secrets, builds the new `Snapshot` on a blocking thread, persists the generation (with its audit
  entry) and swaps it in. A change that changes nothing creates no generation.
- A request loads the snapshot once (`rt.gens.load()`) and uses it to the end.
- State that must survive generations lives in `RuntimeState`, keyed by stable identities: limit
  counters by the declaring resource's `ResourceUid` (its incarnation: a delete and re-create starts
  fresh), weighted-node state by node key. Each engine compiles its own part from the resolved
  document (`antirion_providers::compile`, `antirion_router::compile`, `antirion_limits::compile`).
- Wall-clock time comes from `rt.now()` (the test clock in test mode); randomness from
  `rt.with_rng` (seeded in test mode after `PUT /test/seed`).

### Admin handlers

- A handler takes a `&Call` (principal, query, body, snapshot) and returns
  `Result<Response, AdminError>`. It checks its query parameters (`call.allow_params`), then the
  permission (`call.require(permission, &scopes)`); `perms.rs` maps collections and settings to
  permissions and computes scope chains. Tenancy resources outside the caller's scope are reported as
  `not_found`.
- Writes go through `commit::api_write`, which audits success with the change set and audits
  failures; denials (401, 403) are audited by the router.
- Lists use `pagination::page_params` and `pagination::encode`: cursors are bound to their
  collection and carry an integrity tag.
- Permission checks on keys pass the key's owner (`Call::require_owned`): a developer's key
  permissions hold only for keys it owns. Changing a key's `permissions` also needs
  `keys:grant_permissions`; granting a role needs `role_bindings:create` from a binding that ranks at
  least as high (`AdminPrincipal::may_grant`).
- The default organization, team and project exist even when the document does not list them
  (`tenancy::implicit_default`): reads show them, `DELETE` is 409, `PATCH` writes them.
- Operations are routed in `antirion-admin/src/routes.rs`: synchronous functions, one per group of
  operations, that build and box only the chosen operation's future (`Box::pin(handler(call))`, or
  `done(result)` for a synchronous handler). A family with its own table (`delivery`, `cache_routes`,
  …) exposes a synchronous `dispatch` returning `Handled` too. Never `.await` a handler inside a
  table: an unoptimised build gives every future a function builds a stack slot of its own, so an
  async table holds all of its branches' futures in the frame that stays on the stack while the
  handler runs. `route.rs` chooses the request's lane (probes, public sign-in steps, console files,
  authenticated calls) the same way.
- The gateway's runtime gives its workers `WORKER_STACK` (4 MiB). `crates/antirion/tests/admin_stack.rs`
  runs the deepest admin operations on workers with a quarter of that: a change that deepens the admin
  plane's frames fails there first. To measure, bisect the smallest stack that passes by running that
  test binary with smaller `thread_stack_size` values (in a debug build the deepest operations needed
  about 2 MiB before the tables were split, and under 600 KiB after).

### Tests

- Unit tests sit next to the code they test (`mod tests`). Integration tests of a crate's public API
  go in its `tests/` (for example `antirion-config/tests/admin_api_contract.rs`, which parses the
  Admin API contract in `openapi/admin-api.yaml` with the strict loader); tests that start the whole
  binary are in `crates/antirion/tests`. `cargo test --workspace` runs them all.
- The console has its own: `pnpm test` (Vitest) and `pnpm e2e` (Playwright against a test-mode
  gateway with the console embedded); see `console/README.md`. `docker/test/smoke.sh <image>`
  checks a built image.

### Lint and style

- Workspace lints apply to every crate (`[lints] workspace = true`); request-path crates also deny
  `unwrap`, `expect`, `panic`, indexing and `unreachable` in non-test code.
- No detached tasks: spawn on `rt.tasks` (a `TaskTracker`) or a `JoinSet` owned by the component;
  long-lived tasks watch `rt.shutdown`.
- Comments say what the code cannot: invariants, protocol quirks, the reason for a choice.

## MCP and A2A

- MCP state that must outlive a request but not a node lives in `antirion_mcp::McpState`
  (`RuntimeState::mcp`): list, discovery and card caches, the revocation set, held HTTP+SSE upstream
  streams, requests in flight (for `notifications/cancelled`), tool fingerprints and approvals.
  Nothing in it is needed to resume a session elsewhere: the session token carries every upstream
  session id.
- The session key is `mcp.session.keys` when configured, else the key the node reads or creates in
  the store at startup (`mcp_session_key`). Without one, MCP endpoints answer 503.
- Caches are keyed by the server's generation fingerprint (transport, URL, credentials), so a changed
  server never reuses an old list, discovery result or session.
- Limits and budgets attach through `antirion_mcp::Hooks::limits` (`pipeline/src/mcplimits.rs`,
  installed once the caller is authenticated); the `mcp_input` and `mcp_output` guardrail stages through
  `Hooks::guardrails` (`pipeline/src/mcpguard.rs`): argument and result strings become the segments of
  the guardrail crate's stage runner, so every check, redaction and fail mode applies. The hooks are
  asynchronous and may replace the arguments or the result.
- Server kinds besides the remote transports: `stdio` servers are pools of child processes per node
  (`stdio.rs`, pools kept in `McpState` by server generation, so an unrelated configuration change
  keeps them), and `openapi` servers are answered by the gateway itself from the parsed document
  (`openapi.rs`). `Link` dispatches to them before any HTTP. stdio servers and `mcp.stdio` are written
  by the configuration file only (`antirion-runtime/src/fileonly.rs`, checked in `Runtime::mutate`).
- Upstream OAuth (`credentials.rs`): the token cache belongs to the compiled `ServerRt`; the caller's
  JWT for token exchange travels in `Egress::subject`.
- Era bridging (`bridge.rs`): modern requests to legacy-only servers open a session per request
  (`Mode::link`); elicitation becomes an input-required round replayed on the retry. `protocol: modern`
  servers are reached from legacy sessions as modern links built from the token's `caps` and `ci`.
- The client-facing HTTP+SSE transport (`clientsse.rs`) keeps the open streams in `McpState`; each
  POST is served by `http::post` on a tracked task and its answer written to the stream.
- Legacy GET streams carry resumable event ids (`ag1:<server>:<base64url id>`, `legacy.rs`).
- Authorization-server helpers (`authserver.rs`) proxy the issuer's metadata and, when enabled, client
  registration.
- The server-side tool loop: `antirion_mcp::Toolbox` lists and calls the caller's granted tools of
  the configured servers; `pipeline/src/toolloop.rs` offers them and drives the loop from
  `infer::pipeline`, each model call a separate routing run.
- Telemetry: `CallLog` gathers the upstream calls; `pipeline/src/reqlog/mcptel.rs` turns them into
  the SERVER span's name and attributes, CLIENT spans and the MCP metric families.
- Calls to MCP servers and agents go through `antirion_mcp::Egress` (`Runtime::mcp_egress`): the
  shared `EgressClient` with operator trust, `security.egress` and its TLS profile, `x-request-id`
  set, never re-sent by the transport (the server's `retries` rule decides). Server and agent URLs
  are checked against the operator URL rules at config time (`antirion_egress::config`);
  push-notification URLs against the client-influenced rules (`Egress::refuse_client_url`).
- Authentication, IP allow-lists, endpoint scopes, request controls and the end user come from the
  data plane's `auth::run`; the public base trusts forwarding headers only from a peer in
  `security.client_ip.trusted_proxies` (`ClientIpRt::trusts_peer`) and is `https` on a TLS listener.
- Each authenticated MCP or A2A request writes one request-log record (`operation` and `dialect`
  `mcp` or `a2a`); the MCP crate gathers its `mcp` or `a2a` member and the error the client received
  in `Env::log` (`CallLog`).
- `POST /test/reset` scope `cache` drops the list, discovery and card caches; `sessions` closes held
  HTTP+SSE upstream streams and forgets revocations and session counters.

## Resilience

`antirion-resilience` holds the decision table, the attempt loop and the health registry; the data
plane drives them from `antirion-pipeline/src/routing.rs` (the planner is `route.rs`'s `Tiers`, the
sender `attempt.rs`'s `Upstream`).

- **The loop** (`engine.rs`, per-request state in `engine/state.rs`): RK, sibling ND, RS, next-tier ND,
  the attempt caps and the deadline. A context-length or content-policy verdict with a class
  fallback asks the planner to move to that Model (`Planner::class_fallback`), once; a rejected
  reasoning replay (an error code, Anthropic's signature message, or a Gemini candidate that
  finished with `MISSING_THOUGHT_SIGNATURE` and nothing else, `relay::native::missing_thought_signature`)
  is re-sent once, at once, with `Attempt::repair` set (the sender strips the replayed reasoning,
  `reasoning.rs`). The client's read timeout (`x-stainless-*`,
  `controls::read_timeout`) caps `ttft` and `stream_idle` of streams and the deadline of other
  requests; `AttemptTimers::shortened` keeps a shortened timer's expiry out of health.
- **Other Models in the plan** (`fallbacks.rs`): request-supplied fallbacks (`x-antirion-fallbacks`)
  and the class fallback Models of the request's policies are resolved when the plan is built;
  `Tiers` walks them as segments after (or instead of) the route, each with its own `AttemptPlan`.
  A 2xx refusal becomes the attempt's failure only while a class fallback is still to come.
- **Retry budget** (`budget.rs`, `HealthRegistry::budget`): per deployment and node, ten one-second
  buckets of first attempts and retries; a refused retry is a `retry_budget` skip.
- **Health registry** (`health/`): breakers, cooldowns, keys and pairs, manual disables (`mod.rs`);
  verdicts of attempts, of a stream failing after its commit and of active checks (`verdict.rs`):
  the success a stream's commit recorded is replaced by the failure (`Committed`, handed to the
  relay through `postcommit.rs`); following a config change (`reconcile.rs`, from
  `Runtime::health_follows` at every generation swap); and the records shared with a cluster
  (`shared.rs`).
- **Fail-open** (`routing.rs`, `all_unhealthy`): when every candidate of the route is ineligible
  only because of its breaker or a deployment cooldown and `on_all_unhealthy` (else the
  deployment's `all_unhealthy`) says `ignore_health`, those deployments are eligible for the
  request, which takes no probe slot and lists `routing` in `x-antirion-degraded`.
- **Active checks** (`antirion-runtime/src/checks.rs`): one task per node, every 100 ms it starts the
  checks that are due (each deployment's `interval`), at most one per deployment at a time.
- **Cluster** (`antirion-runtime/src/cluster/health.rs`): every 250 ms a node writes its pending
  records and, when the shared version moved, reads all of them and adopts the newer ones
  (`antirion-limits/src/store/valkey/health.rs`: one hash written by a compare-the-stamp script, and
  a version key). Requests never wait on the counter store.

## Known gaps in resilience

- Stored-response reads and deletes (`stored.rs`) honour the SDK read timeout but not the other
  request controls of the attempt loop.
- Hedging, response predicates and the cluster-wide probe lease: see Routing, resilience and
  cost.

## Routing

`antirion-router` holds route trees, plans and the selection state of a replica; the data plane
drives them from `antirion-pipeline/src/routing.rs` (the plan, pins and affinity), `route.rs` (the
eligibility filters and the planner the attempt loop reads) and `decide.rs`.

- **Decisions taken ahead** (`decide.rs`): when a route has a classifier that calls out or replays,
  a cost-quality node or a pool with an external picker (`RouteRt::decides_ahead`), every such node
  reachable through the decisions
  already taken is decided before the first attempt (`AttemptPlan::pending`, `set_decision`); the
  plan takes the other decisions itself (`plan/decide.rs`). Each call is bounded by its node's
  `timeout` with `tokio::time::timeout`; a failure takes `default` (or the scorer's `on_error`
  branch) and marks `routing` degraded with its cause, or fails with `router_unavailable`. Pinned
  plans and plans built later (class fallbacks, mirrors) decide without calls.
- **Calls to the gateway's own Models** (`internal.rs`): embeddings and short chat questions go to
  the Model's first usable deployment that speaks the API, then the next; each settles as a `router`
  ledger line tagged `antirion.routing`, after the fact. Webhooks go through `external::post_json`.
- **Embeddings** are cached per classifier node (`content/cache.rs`, in `RoutingState`): queries
  under the tenant scope, examples without one; one call carries every text not cached yet.
- **Cost-quality**: the scorer's win rate against a fixed threshold, or against the order statistic
  of the node's window of scores (`content/calibrate.rs`, `RoutingState::threshold`).
- **Continuation replay**: `content::is_continuation`; the class is kept per node, tenant and
  session value in `RoutingState` for the route's affinity TTL.
- **Pools**: `antirion-runtime/src/pools.rs` scrapes each endpoint of every `pool` node of the
  serving generation every `metrics.interval` (one scrape per endpoint at a time, on a `JoinSet`) and
  hands readings to `PoolState`. A pool's node plan (`plan/pool.rs`) ranks the eligible endpoints;
  the attempt counts its assumed load and teaches the endpoint the prompt's chunk hashes through
  `RoutingState::begin_in_pool` (the `Position`'s `PoolDispatch`). Stale metrics add the cause
  `pool_metrics`.
- **Leaves**: `upstream_body` edits are applied by the sender to the final upstream body, after
  translation and the prompt-cache markers (`attempt::edited_body`), and to a mirror's copy; leaf
  `tags` filter through `Filters::tagged` (`tags::allowed`, computed per route segment).
- **Affinity**: with `prompt_prefix`, a request without a session value is bound by
  `prefix_key::value` (system text, canonical tool definitions, first user message) in `prefer`
  mode; strict bindings make a one-position plan or fail with `affinity_unavailable`.
- **Experiments**: the arm of the experiment above the first position goes into the record
  (`experiment`) and, with `expose`, `x-antirion-experiment` (`routing::experiment`).
- **Shadow traffic** (`mirror.rs`): copies are counted in `antirion_shadow_requests_total` as
  `sent` or `dropped`.

## Known gaps in routing

- Classifier replays, score windows, embeddings, pool readings, assumed load and prefix indexes are
  per replica; only the attempts in flight of `least_busy` nodes with `scope: cluster` are shared
  (`antirion-runtime/src/cluster/inflight.rs`). Canary ramps, error budgets and shadow comparisons
  are per replica too, and a restart starts a ramp over.
- A classifier or cost-quality node reached only through a class fallback, `x-antirion-fallbacks`
  or a mirror decides without calling out (`default`, `strong`).
- LoRA adapter affinity, the external endpoint picker, the matrix-factorisation scorer and bandits:
  see Routing, resilience and cost.

## Routing, resilience and cost

- **Hedging** (`antirion-resilience/src/hedging.rs`, `engine/hedge.rs`): the first attempt of a
  generation request races hedges started every `delay` on the positions the plan would try next
  (`FuturesUnordered`); the first success wins and the others are dropped as `hedge_cancelled`.
  A raced attempt with a complete response runs on a detached `Exchange` (`pipeline/hedge_tail.rs`):
  a lost one is awaited for a second on `rt.tasks` so its usage is charged on its own ledger line
  (status `cancelled`); the record settles after those tails. Admission multiplies output
  reservations by `1 + max_extra` (`account::hedge_factor`).
- **Response predicates** (`antirion-resilience/src/predicate.rs`): checked on complete 2xx answers;
  a match fails the attempt as `upstream_bad_response` (reason `response_predicate_matched`) without
  counting against health, and its usage is still charged.
- **Probe lease** (`antirion-resilience/src/lease.rs`, `antirion-runtime/src/cluster/probe_lease.rs`,
  `antirion-limits/src/store/valkey/probe_lease.rs`): with `probe_scope: cluster`, a half-open probe
  also takes one of the deployment's leases in the counter store; a candidate whose leases are all
  held elsewhere is skipped as `ineligible`.
- **LoRA affinity** (`antirion-router/src/pool/metrics.rs`): readings carry the base model and the
  adapter state of `vllm:lora_requests_info`; `rank` filters by adapter before the scores.
- **External endpoint picker** (`antirion-pipeline/src/picker/`): one gRPC stream of the
  external-processing protocol per request, its messages encoded by hand (`proto.rs`); the request
  body's tail is a channel, so the stream stays open (HTTP/2 request bodies keep streaming after the
  response head, `antirion-egress/src/h2pool.rs`) and `Session::finish` reports the served endpoint
  from a task on `rt.tasks`. `AttemptPlan::set_picked` turns the pool's plan into the picked list.
- **Bandits** (`antirion-router/src/bandit.rs`, `antirion-runtime/src/bandits.rs`): feedback on an
  experiment's metric is a reward in the store (`antirion-store/src/bandits.rs`); every node reads
  the sums every second into `RoutingState::bandits`, where Thompson sampling draws, and records a
  stop decision once.
- **Matrix-factorisation scorer** (`antirion-router/src/content/mf.rs`): the model file is folded into
  one direction vector when the configuration is compiled; scoring embeds the routing text through
  the gateway's own embeddings Model and takes one dot product.
- **Pricing** (`antirion-limits/src/price.rs`, `Price::for_call`): service-tier and batch prices; the
  tier comes from the answer, else the request.
- **Re-costing** (`antirion-admin/src/recost.rs`, `antirion-limits/src/recost.rs`):
  `POST /reports/spend:recost` re-prices ledger lines of a window under the current prices and writes
  the differences as `adjustment` lines (originals are never rewritten), audited as `spend.recost`.
- **Rolling budgets** (`antirion-limits/src/math.rs`, algorithm `R` of the Valkey library): spend in
  sub-windows of the period, rebuilt from the ledger by sub-window (`ledger_charged_slots`).
- **Priority classes** (`antirion-limits/src/priority.rs`, hold mode `H`): a limit's share per class
  of the key; with `priority_mode: headroom` a class may use what the others leave.
- **Cache hits and budgets** (`antirion-limits/src/bypass.rs`): with `cache_hits_bypass_budgets`,
  an exhausted budget's refusal waits for the cache lookup; a hit is served and a miss refused.

## Known gaps in routing, resilience and cost

- A lost hedge that streams is cut at once; only complete responses are awaited for their usage.
- The probe lease does nothing on a standalone node (there is no shared counter store).
- A picker pool reached only as a mirror target is scheduled by the built-in scheduler.

## Safety, caching and telemetry

- **Templates** (`antirion-guardrails/src/template.rs`): templates are parsed into text and variable
  pieces when the configuration is compiled; rendering builds the messages as JSON values, so a value
  is data whatever it contains and nothing is ever parsed at request time. `run_stage` applies them
  first and reports a refusal in `StageOutcome::template`, which `guard::input` turns into the
  `*_template*` reasons. `parse` lets a `template` object stand in for absent or empty messages
  (`ValidateOpts::templated`); `parse::revalidate` checks a templated body no template rendered.
- **Reversible tokens** (`antirion-guardrails/src/tokens.rs`): the input stage's `Tokenizer` lives
  in the `RequestGuard`; its `Tokens` are put back by `guard::output` after the stage, by the stream
  `Window` (`Window::restoring`, which holds a text ending in the start of a token) and by buffer
  mode's final text. A tokenised request is `uncacheable` (`cache::Inputs::tokenised`).
- **Divert** (`Verdict::Divert`, `StageOutcome::divert`): `infer` resolves the divert Model and
  continues with its route, deployments and translation decision from the cache lookup on.
- **Re-ask** (`Trail::set_reask`, `take_reask`): a `json_schema` block with `reask` sets the flag
  (complete responses in `guard::output`, buffered streams in `OutGuard::finish`); `infer` runs the
  route again up to `reask` times, and the refused answer's usage becomes the attempt's (a refused
  complete stream hands its usage, completed by the estimate, back with `Held::Guarded`).
- **Rewrite and topic** (`rewrite.rs`, `topic.rs`): their Model calls go through `Net::chat` and
  `Net::embed`, which `GuardNet` serves with `internal::ask_for_guardrail` (first usable
  deployment, no ledger line). Topic descriptions are embedded once per generation, in the `Def`.
- **JSON repair** (`repair.rs`): an output transform, run only when the request asked for JSON
  (`RequestGuard::json_output`); such streams are buffered.
- **Fail-closed cache** (`cache::failed`): with `fail_mode: closed` any lookup failure is 503
  `cache_unavailable`.
- **Per-item embeddings** (`cache/items.rs`): `Lookup::Items` carries each element's key and hits;
  `infer` sends only the missing elements, and `items::finish` merges the answer and stores each
  computed element on the response cache's backend, unless a fallback computed them and the policy
  does not store answers after a fallback.
- **Stream summary trailer** (`relay/mod.rs`): `x-antirion-stream-end` and
  `x-antirion-overhead-ms` (`reqlog::stream_overhead`) beside the cost trailer.
- **Trace exporter** (`antirion-obs/src/ingest.rs`): a `sinks.langfuse` entry is an HTTP sink with
  `BatchFormat::TraceIngest`, basic authentication and the ingestion path; the batch is built from
  each record's JSON.

## Known gaps in safety, caching and telemetry

- Guardrail Model calls (rewrite, topic, judges) are not ledger lines of the request.

## Models, routing and change control (console backend)

- **Catalog** (`antirion-router/src/model.rs`, `antirion-admin/src/models.rs`): tiers, labels and
  lifecycle states are compiled into `ModelRt`; `authorize::lifecycle` refuses retired and gated
  Models and returns the deprecation notices `Ctx::extra_headers` adds to the response. When a
  Model's lifecycle state changed is kept in its resource metadata (`lifecycle_at`, set in
  `commit::next_state`), as is when each provider credential was last set
  (`credentials_rotated_at`). The Model list filters and counts in `models::ListFilter`.
- **Requests that wait for a decision** (`antirion-store/src/requests.rs`, PostgreSQL migration
  35): access requests (`access_requests.rs`), change requests (`change_requests.rs`), scheduled
  credential retirements and rotation-age notices (`antirion-runtime/src/rotation.rs`), one table
  with a compare-and-set on `version` per update. `POST /test/reset` scope `audit` clears it.
- **Change control** (`change_control.rs`): `commit::api_write` and an `api`-sourced
  `config:apply` refuse governed writes with 409; a change set is staged on a document by
  `change_control::stage` (resource and settings writes only) and applied in one `Runtime::mutate`
  that re-stages it on the current document and refuses when a touched resource moved. Actions under
  the two-person rule call `change_control::second_person` before acting; `:apply` runs the held
  call again with `Call::approved` set.
- **Tree additions** (`antirion-router/src/tree/`): `block` is a terminal node the plan yields only
  as its first position (`Step::block`, answered by `routing::route_block`); `tier` compiles to a
  composite over `Member` nodes (one per Model, inlined) ordered by the strategy's node kind;
  conditional rules are `Rule`s with names and switches; `Weighted` carries an optional `Canary`
  whose ramp and budget live in `RoutingState::canaries` (`canary.rs`), registered at each
  generation swap.
- **Decisions without a request** (`antirion-pipeline/src/decision.rs`): decide requests
  (`/antirion/decide/<path>`, recognised by rewriting the URI before `accept`) and the dry run
  build the plan with `SelectCx::dry` (SWRR peeked, no priming) and never bind affinity. Route
  simulation (`antirion-router/src/simulate.rs`, `antirion-admin/src/simulate.rs`) walks the
  plan over logged records on a `RoutingState` of its own.
- **Shadow comparison** (`antirion-pipeline/src/compare.rs`): each copy and its request share a
  `Pair`; whichever finishes last compares and records into `RoutingState::shadows`. Mirrored Chat
  Completions streams are assembled for it.
- **Providers**: rate-limit headers are read per credential in `attempt::one` into
  `RuntimeState::headroom` (`antirion-providers/src/headroom.rs`); each reading with a limit and a
  remaining count is also a minute of quota use (`pressure.rs`: the minute's peak per credential),
  written by the request-log writer to `provider_pressure` (state store, or PostgreSQL migration 85)
  and read by `GET /reports/provider_pressure` (`antirion-admin/src/pressure.rs`) over every node's
  rows; `:test` and
  `:rotate_credential` are `antirion-admin/src/provider_ops.rs`. A provider key of weight 0 is a
  standby the key selection uses only when no weighted key can serve.
- **Guardrail conditions** (`antirion-pipeline/src/guardsel.rs`): conditions reading
  `guard.<name>.<field>` are evaluated ahead of the plan, running each guardrail they reach once
  through the guardrail test path; the plan reads the kept results through `RequestFacts::guard`.

- **Small console reads and writes**: saved log views keep the creator's star in the view and any
  other reader's in a `log_view_star` record of its own (`views.rs`), so `starred` is always the
  caller's; `DELETE /routes/{name}/affinity` (`health.rs`) drops the node's bindings of the route and,
  in cluster mode, the shared ones (`Runtime::affinity_unbind_route`, a `SCAN` over the binding keys);
  `GET /status` reports `observability.traces.viewer_url` as `trace_viewer_url`.

## Known gaps in the console backend

- Change requests stage resource and settings writes only; keys and users cannot be staged.
- Decision latency per rule is read from the records' stage timing; the analytics rollups have no
  sketch of it.

## Operations and danger zone (console backend)

`antirion-runtime/src/operations/` holds what runs off the request path; the Admin API reaches it
through `danger_routes.rs` (`ingress.rs`, `jobs.rs`, `org_deletion.rs`, `rotate.rs`,
`encryption.rs`).

- **Shared state** is rows of `pending_requests` (no migration of its own): `ingress_pause` (id
  `gateway` or `organization/<name>`), `organization_deletion` (id the organization), `job` and
  `job_file` (the ZIP, base64, its `created_us` the expiry). One task per node
  (`operations::spawn`) reads the active pauses every 500 ms into `Operations::pauses` (an
  `ArcSwap`), deletes the organizations whose grace passed (claimed with a compare-and-set), learns
  other nodes' erasures and drops expired files. `POST /test/reset` scope `audit` clears them.
- **Ingress**: `accept` refuses while a gateway-wide pause holds (after the drain check), `auth::run`
  while the key's organization is paused (`accept::paused`, reason `ingress_paused`).
- **Deletion** (`operations/deletion.rs`): one generation without the subtree (`without`), then,
  after `ERASE_DELAY`, `Runtime::erase_tenant`: `Store::erase_tenant` (`antirion-store/src/
  erasure.rs`, `pg/erasure.rs`: every tenant-scoped table by explicit names, never a pattern), the
  request log through `erase_logs`, the organization's cache epoch. `Erased` hides the tenant's
  ledger lines admitted before the erasure from spend reads (`ledger.rs`), and a node that learns an
  erasure removes the lines it wrote afterwards (`Store::erase_ledger`).
- **Exports** (`jobs.rs`, `zipfile.rs`: stored entries, no compression): the job runs on a tracked
  task of the node that accepted it; its file is in the store, so any node serves the download.
- **Two-person rule**: `ingress_pause`, `organization_delete` and `secrets_rotate` call
  `change_control::second_person`; `run_approved` replays them on a task of their own, since a held
  action that commits a generation nests deeper than a worker's stack allows in debug builds.
- **Maintenance windows** (`operations/maintenance.rs`): `/status` and the alert engine's `raise`
  (`silence_during_maintenance`). **Encryption** (`encryption.rs`, `certinfo.rs`: the DER fields
  the report shows); key ids come from `Sealer::key_ids`, whose source `with_configured` records.

## Known gaps of the operations

- An export job runs on one node; if that node stops, the job stays `running`. The ZIP is built in
  memory and capped at 100 000 rows per member.
- Ledger lines spooled during a store outage longer than the erasure delay reach the store after the
  erasure; the node that wrote them removes them once it learns the erasure, others hide them.

## Keys, people and tenancy (console backend)

`antirion-runtime/src/people/` holds what runs off the request path; the Admin API reads it through
`antirion-admin/src/people.rs` (facts and list filters of keys and users), `keys.rs`, `offboard.rs`,
`anomalies.rs` and `limitsim.rs`, routed by `access_routes.rs`.

- **Use times** (`usage.rs`, `people/marks.rs`, store `marks.rs`, PostgreSQL migration 45): each key's
  last authentication is kept per node in a sharded map; a task writes the ones that moved a minute
  past what the store holds every five seconds, and once more at shutdown. The store keeps the later
  time (`use_marks`). Reads take the later of the store's and the node's. Each session start writes
  the user's last sign-in as the identity record `last_login`; user activity also reads sessions' and
  admin tokens' last use (`Store::user_activity`).
- **Key policy** (`keypolicy.rs`): `max_key_lifetime` (smallest of the levels) and `key_policy` (each
  field from the narrowest level). The admin checks lifetimes on create and update, fills the default
  expiry and the default grace of `:rotate`; a rotation records `rotated_at` in the key's metadata.
  Reminders (`people/reminders.rs`) are swept every second, claimed in `pending_requests` (kind
  `key_reminder`) so one node sends each, and written to the audit log.
- **Anomalies** (`people/anomalies.rs`, store `anomalies.rs`, PostgreSQL migration 46): `auth::run`
  adds each request to a sharded tally (hour, network, user agent family); a task drains it every
  second, compares each key with its history (`key_seen`, per-node hourly counts in `key_hours`),
  opens flags in `key_anomalies` (one open flag per key, kind and subject through a partial unique
  index) and announces new ones (`key.anomaly`, the built-in alert rule `key_anomaly`, an audit entry).
  A key's first hour is learnt.
- **Regions** (`DeploymentRt::effective_region`) are checked with the provider and deployment lists in
  `authorize::deployment_allowed`; `set:<name>` entries of `allowed_ips` are expanded when the identity
  snapshot is built. **Custom roles** are `Role::Custom` (`antirion-identity/src/roles.rs`), resolved
  through `IdentitySnapshot::roles`.
- **GCRA counters** keep their state across a changed `per` (`GcraState::per_us`, mirrored in the Lua
  library); sliding windows start over.
- `POST /test/reset` scope `counters` clears use times, the tally, flags and key history; `audit`
  clears sent reminders.

## Known gaps in keys and people

- Anomaly baselines are read from the store per key at most once a minute and other nodes' hourly
  counts at most every ten seconds, so a cluster sees a new network or a spike a little later than a
  single node. GeoIP countries are looked up once per network per evaluation.
- The `exposed` anomaly kind is defined but raised only by the safety work that detects exposed keys.

## Budgets and cost (console backend)

`antirion-runtime/src/budgets/` holds what budgets do off the request path; the Admin API reaches it
through `antirion-admin/src/budgets.rs` (counter items, `:adjust`, `:reset`, `:grant_overage`),
`overage.rs`, `budget_events.rs` and `generations.rs`, routed by `budget_routes.rs`.

- **Node memory for the request path** (`antirion-limits/src/overlay.rs`, `Accounts::overlay`):
  running grants, which `Accounts` adds to every budget hold's amount at admission, attempts, reads
  and adjustments (`HoldInfo::declared` keeps the declared amount, which alert ratios use); the last
  settled spend seen per budget identity (settlements and reads write it), which downgrades read;
  and the refused and downgraded requests counted since the last collection. A refusal is matched
  to its budget by the error's `limit` level and name.
- **Store** (`antirion-store/src/budgets.rs`, PostgreSQL migration 50): `budget_grants`,
  `overage_requests` (unique per budget identity and period start), `budget_events` (an optional
  `once_key`, unique, makes a period's `exhausted` and `forecast_exceeded` events once per cluster)
  and `budget_tallies` (per node, summed on read). Budgets are named by identity, never by partition
  values. `generations.rs` (migration 51) keeps the documents of the newest `history_limit`
  generations, sealed, written by `Runtime::mutate` after each commit.
- **The watch** (`budgets/watch.rs`): every 250 ms each node writes the events queued where the
  caller must not wait (threshold crossings from settlement, configuration changes from commits)
  and its tallies, handling each budget's first refusal of a period (`exhausted`, `budget.exhausted`,
  and for `approval` budgets the period's overage request and the owner's mail); every second it
  reloads the running grants and the spend of budgets that downgrade. One node (the `budget_watch`
  lease in a cluster) evaluates forecasts every second (`forecast_exceeded`, the `budget_forecast`
  alert instance, which the metric evaluator leaves open) and expires overage requests. Admin reads
  flush the node's queue first, so they see its own counts at once.
- **Forecasts** (`antirion-limits/src/forecast.rs`): the rate over the ledger charges of the trailing
  `min(elapsed, 7d)` (`Accounts::charged`), the projection, `runs_out_at` against the effective
  amount, and the pacing state. Computed by reads and by the watch, never per request.
- **Downgrades** (`antirion-pipeline/src/downgrade.rs`): after the Model is resolved, the applicable
  budgets with a `downgrade` (only when some budget declares one, `Policies::has_downgrades`)
  compare the overlay's spend with `at`; the target is resolved and authorized like a requested
  name and replaces the resolution.
- **Avoided cost**: a hit's record keeps the deployment that produced the entry
  (`CacheFact::origin`); settlement prices the entry's usage there (`LedgerLine::avoided`,
  `cost.saved`), summed by spend summaries and the analytics rollups.
- **Configuration** (`budgets/config.rs`): owners and downgrade targets must exist; over-allocation
  (`antirion-limits/src/allocation.rs`, parents per the nearest same-unit-and-period rule) is a
  warning of `config:validate`, `config:apply` and `config:rollback`.
- `POST /test/reset` scope `counters` or `budgets` clears the overlay, the queued events and the four
  budget tables.

## Known gaps in budgets and cost

- Forecasts and their events cover unpartitioned budgets and the partitions the evaluating node has
  seen; the partitions of a budget are listed per node.
- A downgrade or a grant made on another node takes effect here within a second.

## Limits

`antirion-limits` holds the whole limits, budgets and cost engine; the data plane reaches it through
`antirion-pipeline/src/account/` and `Runtime::accounts` (`antirion_limits::spend::Accounts`: the
reservation engine over the node's `MemoryCounters`, the ledger window and the budget counters rebuilt
from the ledger).

- **Snapshot build**: `Policies::compile(&doc, &uid_of)` compiles every limit and budget
  declaration (gateway, organization, team, project, key, route, model, route-tree nodes by route
  path, deployments) and the `limits`, `budgets` and `pricing` settings into `Snapshot::limits`.
  `check::check_document` is the config rule set the schema cannot express (declarations compile,
  prices parse with increasing tiers, a budget never exceeds a budget of the same unit and period at
  the level directly above it); `Runtime::check` runs it. Prices come from `Snapshot::pricing`
  (`deployments.<d>.price` over `pricing.models` over the bundled catalog), the one source for
  reservations and settlement.
- **Output caps** (`account::check_caps`, after the request schema): 400 `output_cap_out_of_range`.
- **`admit`** (`account::admit`): the body facts and caps of every dialect (`account/prompt.rs`), the
  estimate over the candidate deployments, `plan::plan` (400 `missing_partition_attribute`, 403
  `model_unpriced`) and `Accounts::admit` (402, 429 with `antirion.limit`, `Retry-After` and
  `RateLimit*` headers). The reservation is keyed by the request's log id and kept in the request's
  `account::Slot` with the admission headers (`RateLimit-Policy`/`RateLimit`, `x-ratelimit-*` on
  OpenAI routes, `x-antirion-reservation`, `x-antirion-budget-*`). A `Pending` dropped unsettled (a
  panic) refunds on a tracked task.
- **Each attempt**: the route's `gate` takes the holds of the position's tree nodes and its
  deployment (`account::admit_attempt`); a refusal skips the position (`limit`), and when every one
  is refused the request is 429 `deployment_limits`.
- **Settlement** (`account::settle`, exactly once per request): one ledger line per upstream call
  (or one `cache_hit` line), each priced with the deployment that served it; failed attempts are
  charged the usage their error body reported (`failed_with_usage`), and a response an output
  guardrail blocked is charged the upstream's usage (its stream's usage, or the counted output, when
  a stream is stopped); missing usage is completed with
  `E_in` and the counted output (`usage_source: estimated`). Non-streaming responses settle in
  `infer::run` before the body is returned; committed streams settle in the relay before the
  terminator is forwarded (`relay::settle_now`), or when they end otherwise. The settled cost is the
  record's cost, the metrics' cost and `x-antirion-cost` (a trailer, declared in `Trailer`, on
  streams whose client sent `TE: trailers`). Budget threshold alerts go to the alert webhooks
  (`Runtime::send_alert`, `budgets.alerts.sink`).
- **Ledger**: `Runtime::record_spend` (`antirion-runtime/src/ledger.rs`) appends lines to the
  node's `Book`, a bounded window of the most recent lines (`book::WINDOW`, idempotent by
  `(request_id, line)` within it, indexed by request and by budget charges), and queues them for the
  store (`ledger_lines`, with each line's charges in `ledger_charges`, indexed by counter and
  admission time). The window drops a line only once the writer reported it stored, so the window
  and the store together always hold every line; spend reports read both
  (`Runtime::spend_lines`, `spend_of_request`). Each line records the budgets it charged by budget
  identity (the counter id without its window), so a budget counter is seeded from the lines admitted
  within its period the first time that period is used (after a restart, after `POST /test/reset`, or
  when its `period` or `timezone` changed): from the window alone when it covers the period's start,
  otherwise from `ledger_charges` plus the window's unstored lines, read while flushes wait
  (`LedgerHistory`). Seeding is once per counter and period (`OnceCell` per counter in a sharded map);
  admissions of seeded counters take no shared lock.
- **Shadow traffic** (`antirion-pipeline/src/mirror.rs`): when the planner picks the first position,
  each node above it with a `mirror` (compiled as its own tree, `Node::mirror`) samples the request
  (a hash of the request id or session value against `fraction`), takes a `max_in_flight` slot and
  picks the first eligible position of its target; the first attempt starts the copies on tracked
  tasks. A copy is read whole and discarded, never retried, and settled by
  `account::settle_shadow`: a `shadow` ledger line (line numbers from 1000) whose cost is added to
  the gateway- and project-level budgets after the fact (`Accounts::charge_shadow`), never reserved.
- **Partitions**: end users are scoped to their project (`<project>/<end user>`); partition values
  stay in process memory and are hashed into counter ids.
- **Admin API**: `GET /runtime/budgets[/{level}/{owner}/{name}]`, `:adjust` and `:reset` (adjustment
  ledger lines), `GET /reports/spend/{requests/{id},ledger,summary}`, `GET /pricing/deployments/{name}`.
- **Test mode**: the test clock drives windows and periods; `POST /test/reset` scope `counters` or
  `budgets` clears counters, reservations and the ledger.

Counter stores implement `store::CounterStore`. `MemoryCounters` serves standalone mode and every
node's local share. `ValkeyCounters` runs `store/antirion.lua`, a function library whose amounts
are exact big integers (Lua numbers are doubles); `math` is the Rust model it mirrors. The Valkey
tests run when `ANTIRION_TEST_VALKEY_URL` is set:

```bash
docker run -d --name antirion-valkey -p 16379:6379 valkey/valkey:8
ANTIRION_TEST_VALKEY_URL=redis://127.0.0.1:16379 cargo test -p antirion-limits
```

They include a differential test of the library's arithmetic against `math` and of whole operation
sequences against `MemoryCounters`.

## Identity, limits and budgets

- **Access controls** (`antirion-runtime/src/access/`, `antirion-pipeline/src/access.rs`): the
  failed-authentication throttle counts every 401 at `authenticate` per client address (IPv6 by its
  /64) in a sliding window and answers 429 `auth_throttled` past `auth.failed_auth_throttle`
  (`auth.rs`, `throttled`); `auth.access_rules` and the external authorization hook run after the
  model is resolved, before `admit` (`access::check`); the hook's allow and deny answers are cached
  per hook URL, key, method, path and Model in `RuntimeState::access`. `end_user: required` refuses at
  `parse` (`access::require_end_user`). `POST /test/reset` scope `counters` clears both.
- **Imported keys**: `POST /keys` with `import` stores the legacy digest as the verifier
  `sha256:<hex>` (no kid); `IdentitySnapshot::imported` maps digests to keys, and credential
  selection hashes a presented value only while some key is imported (`select::ImportedKeys`).
  Rotation keeps the digest as the previous secret for the grace period.
- **Output caps**: with `output: clamp` the client's body is capped (or given a cap) after the
  request hooks and input guardrails (`account::clamp`, `prompt::clamp`), and a lowered value is
  reported in `x-antirion-max-output-clamped`.
- **Headers**: Anthropic routes get `anthropic-ratelimit-*` (`headers::anthropic`, token limits by
  the side their weights count); complete responses get `x-antirion-cost-details`, streams get it as
  a second trailer; with `pricing.report_in_body` the usage of complete answers gets `cost` and
  `cost_details` (`account/inbody.rs`), and in streams the events that carry usage are held until
  settlement (`relay/costed.rs`).
- **Units** (`account/units.rs`): image generations are reserved at `n × per_image` and charged the
  images their JSON answer carries (a streamed one: `n`); transcriptions by the answer's `duration`
  or a WAV input's header; speech by the characters of `input`; web searches from Anthropic usage.
  Image and transcription answers are read whole for this (`outcome::counted`).
- **Image tokens** (`antirion-limits/src/imagetokens.rs`): inline images' PNG, JPEG, GIF or WebP
  headers and the candidates' provider formulas replace `image_tokens_default`.
- **Deployment and node limits**: when admission reserved nothing, the first attempt's holds open the
  reservation in the store (`LimitsEngine::open_with_attempt`).
- **Self-service endpoint** (`selfservice.rs`): `GET /antirion/v1/limits` reads the counters of the
  key's own tenancy declarations without reserving (`applicable::standing`, `plan::reading`,
  `Accounts::read`).
- **MCP and A2A** (`mcplimits.rs`): `tools/call`, `prompts/get`, `resources/read` and A2A RPCs take a
  reservation with the server, tool and agent as attributes and a priced call's `per_call`; the
  reservation is settled when the server or agent answers and refunded when the call is dropped.
  A priced call writes one ledger line with `mcp_server`, `mcp_tool` or `a2a_agent`
  (`ledger::CallTarget`).

## Known gaps of the core gateway

- DNS goes through the system resolver, or through `server.upstream.dns.servers` when set
  (`antirion-egress/src/resolve.rs`); every answer is still validated and the socket connects to
  the validated address (or tunnels to it through the forward proxy, see Hardening).
- Header limits are fixed at 64 KiB and 200 fields. HTTP/2 header blocks are decoded up to
  2 MiB before the gateway's own 431, because the HTTP/2 stack derives its CONTINUATION-flood cut-off
  from that bound.
- Header names ending in `-api-key`, `-api-key-id`, `-virtual-key`, `-vk`, `-auth` or
  `-authorization` are treated as credentials and never forwarded (a superset of the always-deny
  list's named credential headers).
- Admission reads `H` from the effective `policies.resilience.hedging` (`account::hedge_factor`).
- Limits engine: budget periods are cut on the node's clock (rate counters use the store's clock);
  a full replay queue drops its oldest settlement; reaping reads each record before expiring it (two
  round trips); expired reservations write no ledger line.
- Transcription seconds come from the answer's `duration` or a WAV input's header; other audio
  containers without a reported duration have no per-second charge. OCR pages are never counted (no
  endpoint reports them).
- Identity: the identity state lives in each generation's snapshot, so
  a standalone node never authenticates against an unreachable store; the store-outage grace is
  parsed and key usage is tracked for it, but nothing triggers the 503 yet.
- Usage is upstream-reported or estimated at four characters per token.
- Routing and resilience: see Routing and Known gaps in routing. Sticky
  hashing uses `routing.hash_seed` or a fixed seed (none is generated and persisted).
  `least_cost` prices come from the deployment's own `price` only. Routing statistics are per node;
  health is shared in cluster mode (see Resilience). `x-antirion-route-debug`
  carries the visited plan and classifier choices. Inference requests are generation requests; only
  the native stored-response reads (resource-bound) and deletes (side-effecting) use the other
  `RequestKind`s (`stored.rs`).
- Error classification: a provider's catch-all server code (Anthropic `api_error`, Gemini
  `INTERNAL`/`UNKNOWN`, Bedrock `InternalServerException`) does not override an error status with a
  cause of its own (402, 401, 503, ...); every other structured code wins over the status.
- Passthrough releases each event once its blank line arrives; the counters are not reconciled
  before the terminator is released. Audio streams are relayed as bytes.
- Guardrails: an asynchronous guardrail's result completes its `async` entry of the record
  (`asyncguard.rs`): the record in progress, or the stored one after settlement; sinks receive the
  record as settled, so they may not see it. Gemini and ConverseStream streams are checked like the
  other dialects (`relay/guard/native.rs`; tool-use input deltas of ConverseStream are not segments);
  `on_block: finish` sends no content-filter ending for them. InvokeModel bodies are not
  inspectable. `max_concurrency` counts per configuration generation. Window mode scans the whole
  text of a segment stream on every delta.
- Secret files behind `${file:…}` are re-read every second; a change rebuilds the current
  generation's snapshot.
- Encryption keys come from the startup file only; a key list changed through the Admin API takes
  effect at the next start.
- Admission covers inference endpoints; MCP and A2A calls take limits and budgets but no admission
  slot.
- HTTP/2 fan-out applies to `server.upstream.http2: prior_knowledge`; an upstream that negotiates h2
  through ALPN still uses one connection per origin, and h2 prior-knowledge connections are not
  retired by age or idleness.
- A connection accepted after the drain began closes when the listeners close (HTTP/1.1 also after
  any response, through `Connection: close`), not at once.
- Configuration this build does not support is rejected with `not_supported_in_this_build` at its
  path (`antirion-config/src/unsupported.rs`, plus the provider and extension compilers); a block
  that only states its defaults is accepted.
- `admin.config.file` must be a file (not a directory).
- `GET /status`, `GET /runtime/nodes` and `GET /cluster` report each node's `in_flight` (the drain's
  count), the admission cap and queue, `started_at` and `ready_since` (`probes::load`, written with
  each heartbeat in cluster mode).

## Extensions, deployment and hardening

- **Hooks** (`antirion-pipeline/src/extend/`): `carrier.rs` calls one hook on its carrier (a webhook
  `POST`, an `Extension` from the registry, a Wasm component), each bounded by the extension's
  `timeout`; `attempt.rs` runs `attempt` hooks in `attempt::send` after the upstream body is built
  and before the request (retries and fallbacks included); `relay/chunks.rs` runs `stream_chunk`
  hooks on every client event after the output guard, in `relay::hold`, `guarded_end` and
  `forward::relay`. A request without the hook has no `ChunkHooks` and no attempt-hook pass.
- **The in-process API** (`antirion_ext::api`): a custom build calls `antirion::run_with(cli,
  Registry::builtin().with("name", Arc::new(MyExtension)))`; the registry is process-wide
  (`registry.rs`, set once). `Extension::call` is synchronous and runs on the request's task; a call
  that returns after its timeout counts as timed out and a panic is a hook failure
  (`api::call_guarded`). The module is the crate's stable surface: change it only compatibly
  within a major version, and add fields to its `#[non_exhaustive]` types rather than variants
  callers must match.
- **Wasm** (`antirion_ext::wasm`): modules are read and checked against `wasm.sha256` while a
  snapshot is built (`compile`, on the blocking pool), compiled once per digest, and linked ahead
  (`InstancePre` with an empty WASI context); each call instantiates in a store of its own with a
  memory cap and an epoch deadline. `antirion_runtime::wasm::clock` advances the epoch every
  millisecond while a Wasm extension is configured. `oci://` modules are fetched by
  `Runtime::fetch_wasm_modules` from `Runtime::mutate` before the snapshot is built. Test
  components are written in the component text format (`wasm/tests.rs`).
- **Configuration**: `Idempotency-Key` (`antirion-admin/src/idempotency.rs`, records `idempotency`
  in `identity_records`, claimed with `Store::claim_record`); the apply lock
  (`antirion-admin/src/applylock.rs`, checked in `commit::api_write`).
- **Admission** (`antirion-runtime/src/admission/`): `lanes.rs` (FIFO, or one lane per project
  served in turn; lanes keep their buffers), `memory.rs` (the resident-set sampler and its flag,
  read once per request in `infer::admission`).
- **Redis Cluster** (`antirion-redis`): the counter store's keys share the prefix as one hash tag;
  the response cache on the counter connection spreads over the slots. Run the store tests on a
  cluster with `ANTIRION_TEST_VALKEY_CLUSTER_URL` (for example three primaries in one container
  announcing 127.0.0.1).
- **ClickHouse request log** (`antirion-store/src/chlogs.rs`, `antirion/src/logstore.rs`): the
  runtime's `RequestLogs` holds the store when `store.logs.backend: clickhouse`; every record read
  and write in `reqlog/` goes through it, rollups and feedback stay in the primary store. Its tests
  run with `ANTIRION_TEST_CLICKHOUSE_URL` (`http://default@127.0.0.1:<port>` of a server started
  with `CLICKHOUSE_SKIP_USER_SETUP=1`).
- **TLS**: listener `client_auth` (`antirion-server/src/tls.rs`: the client verifier, the verified
  certificate's SHA-256 in `Peer::client_cert`); upstream client certificates (`TlsSettings::
  client_cert`, read in `antirion-providers` and re-read by `secretwatch.rs`); a TLS alert after the
  handshake is a TLS failure (`antirion-egress`, `tls_alert`).
- **Forward proxy** (`antirion-egress/src/proxy.rs`): part of `EgressPolicy`, so pools follow it;
  the connector resolves and validates, then tunnels to the validated address.
- **Secrets in memory**: `antirion_core::Secret` (own pages, locked where allowed, zeroed on drop,
  `[REDACTED]`); `antirion/src/hardening.rs` turns core dumps off at startup.
- **Helm chart**: `deploy/helm/antirion`, checked in CI with `helm lint` and `helm template`.
- **Fuzzing**: `fuzz/` is its own workspace (cargo-fuzz, nightly): `ci/fuzz.sh <seconds>` runs every
  target from its seeds. Parsers private to the pipeline are reached through
  `antirion_pipeline::fuzzing`, compiled only with `--cfg fuzzing`. Keep the target directory out of
  the repository: `cargo +nightly fuzz run -s none --target-dir <dir> <target>`.

## Known gaps of extensions, deployment and hardening

- A Rust extension's call is not interrupted at its timeout (it is measured after it returns); Wasm
  calls are.
- `stream_chunk` hooks do not run for Bedrock event-stream clients; a hook failing open after the
  commit is recorded in the log, not in `x-antirion-degraded` (the headers are sent).
- The counter store on a Redis Cluster uses one slot; `topology: sentinel` connects to `url` as a
  standalone server.
- Secret values inside the configuration document (the resolved JSON) and HTTP header values built
  from them are not zeroed; the secrets held apart from it are.
- With ClickHouse, a record's content retention is an asynchronous mutation, and lists read the newest
  version of each row (`FINAL`).

## Response cache

`antirion-cache` holds the cache's rules and backends; the data plane reaches it through
`antirion-pipeline/src/cache/` and `RuntimeState::cache`.

- **Lookup** (`cache::lookup`, after `admit` and the input guardrails): the directives are parsed
  first (400 `invalid_cache_directive` for any request), then the merged `policies.cache` decides
  `disabled`, the directive checks run (403 `override_not_allowed` and `permission_required`, 400
  `cache_ttl_above_max`), then `uncacheable` (endpoint, non-JSON body, stateful Responses fields),
  `bypass`, the key (`cache/keydoc.rs`: scope, epochs, namespace, operation, route, route and
  guardrail fingerprints, included headers, query, normalised body), `refresh`, and the backend
  lookup within `lookup_timeout`. A backend error is `error`, a miss that still writes, with `cache`
  in `x-antirion-degraded`. The status and key go in the record's `cache` member; `cache::decorate`
  adds `x-antirion-cache` and `x-antirion-cache-key` to every response of a request that got that
  far.
- **Hits** (`cache/replay.rs`): the stored body byte for byte, or a stream: a stream-written entry
  replays its events (Chat usage dropped or added as `stream_options.include_usage` asks), an entry
  written from JSON is synthesized with the relay's synthesizers. Hits settle as `End::Cached` (one
  `cache_hit` ledger line, cost 0, token limits at zero) and replay the recorded routing identity.
- **Writes**: non-streaming responses in `outcome/` after the client body is final
  (`store_response`, embeddings read whole when a slot exists); streams through `cache::Tee`, which
  the relay's `Writer` feeds with every byte it sends and which writes only when the upstream sent
  its terminal event. `store.rs` holds the never-store rules (final 200, no fallback unless allowed,
  upstream `no-store`, size, refusals, tool-call arguments, output-guardrail verdicts read from the
  record). The memory backend is written inline; a shared backend on a tracked task.
- **Backends**: `cache.backend` (`Settings::from_doc` on the resolved document, applied by
  `observe_generation`): `memory` (byte-bounded LRU) or `redis` (keys
  `<prefix><16 hex scope digest>:<key>`, `SET … PX` with the remaining TTL). Without `type` (or
  `type: redis` without a URL), a cluster node uses its counter store connection
  (`Cache::use_cluster_counters`). Epochs live in the node and the store
  (`cache_epochs`); `POST /test/reset` scope `cache` clears entries, epochs and counts.
- **Semantic layer** (`cache/semantic.rs`, after an exact `miss`): an eligible request
  (`antirion_cache::semantic::eligible`) is embedded through the embedding Model with
  `internal::ask` (an auxiliary `cache_embedding` ledger line, numbered from 401), within
  `semantic_store.lookup_timeout`; the vector is searched in the node's `VectorStore` under the
  partition key (the key document with the last user text emptied, version `s1`), with the polarity
  guard. A hit replays the candidate's exact entry and copies it under the request's key; a miss
  puts the vector in the slot, and `store::write` indexes it once the entry is written. Only the
  `memory` semantic store exists.
- **Coalescing** (`policies.cache.coalesce`): the first exact miss of a key with a slot leads
  (`Cache::flights`); its `Flight` lives in the slot (the plan, the tee, a shared backend's write
  task) and ends when the last copy drops. Followers wait for that, then look up again.
- **Prompt-cache breakpoints** (`cache/prompt.rs`): `x-antirion-prompt-cache` is checked with the
  lookup; each attempt to a deployment with `prompt_cache_markers` gets markers on its upstream
  Messages body unless the client placed any.
- **Admin API**: `DELETE /cache/entries/{key}`, `POST /cache:invalidate`, `GET /runtime/cache`
  (`cache:admin`).

The Valkey tests run when `ANTIRION_TEST_VALKEY_URL` is set (see Limits).

## Known gaps in cluster mode

- The PostgreSQL schema keeps the whole document per generation (as the embedded store does), not
  one row per resource version. TLS to PostgreSQL has no client certificate (`sslcert`) and no
  channel binding.
- Ledger lines are copied into every node's memory (the whole ledger); the spend reports read
  memory, not the store.
- Expired reservations of a stopped node release their holds but write no `expired` ledger line.
  Admin reads during a store outage are served from the snapshot, but only `GET /cluster` says
  `stale` (a document export has no place for it).

## Cache console

What the Cache screen reads and changes beyond the cache's own rules.

- **Entry facts** (`antirion-cache/src/meta.rs`, `antirion-pipeline/src/cache/facts.rs`): every
  entry carries an `EntryMeta` built at the lookup (Model, named route, tenancy, key, end user,
  tags, the request metadata of the keys invalidation rules name, tokens, the caps of every level
  that sets `max_entries`, the level whose semantic layer applied, and for the copy a semantic hit
  writes, the entry it was copied from). Its `dims` are the values listings filter by and
  invalidations select by (`model/m`, `tag/blue`, `metadata/k=v`…).
- **The index**: the memory backend (`memory/`) keeps its slots in a slab with intrusive recency
  lists (the global LRU and one per cap, so a cap evicts in constant time), and beside them ordered
  sets per sort order, the keys in order and a set per dimension value (`memory/index.rs`). The
  Valkey backend keeps the same index on the server (`valkey/index.rs`, scripts in
  `valkey/scripts.rs`) under `<prefix>{<prefix>}ix:`, one slot on a Redis Cluster; writes, hits
  (counted on a tracked task), removals and caps are one script each, and evicted entries are
  deleted by the caller from the `digest:key` pairs a script returns. Listings never scan the
  cache: an unfiltered page walks one ordered set from its keyset position, a filtered one sorts
  what its smallest dimension set selects. `Cache::list`, `read`, `count_dim` and `remove`
  (`service/console.rs`) are the API.
- **Finer invalidation** (`invalidate.rs`): `Target::Dim` and `Target::OlderThan` set a cutoff kept
  with the epochs (`service/epochs.rs`, names `cut:…`), so the store shares them like epochs and
  `Cache::raise_from_store` makes a memory node remove what a raised epoch or cutoff killed.
  Lookups and listings treat an entry written at or before an applying cutoff as gone; the admin
  call removes entries on a tracked task.
- **Invalidation rules** (`antirion-cache/src/rules.rs` compiles them into `Settings`; cron in
  `antirion-config/src/cron.rs`; checks in `antirion-config/src/invalidation_rules.rs`):
  `antirion-runtime/src/cache_console/rules.rs` fires them on commits (`note_cache_changes`), on
  their schedule (every node each minute, claimed by the firing's `once` key in the store), by hand
  (`:run`), and the pipeline fires `metadata_changed` ones before its lookup (`cache::Seen` keeps
  the last value per rule and scope). Firings are recorded (`antirion-store/src/cache_console.rs`,
  PostgreSQL migration 60) and audited as `cache.rule_fired`.
- **Semantic statistics** (`cache_console/stats.rs`): hits, summed similarity and false-positive
  reports tallied per group and UTC day in node memory, added to per-node store rows every quarter
  second; `GET /runtime/cache` reads 30 days of them.
- **Admin API**: `cache_routes.rs` routes the entry list and detail and the false-positive report
  (`cache_entries.rs`), `:run` and the rule firings; the rules themselves are the nested collection
  `cache/invalidation_rules`; `cache.rs` adds the finer levels to `POST /cache:invalidate`.
- `policies.cache.read_only` creates no slot (the record's `cache.write` says why);
  `prompt_caching.openai_cache_key` adds `prompt_cache_key` to attempts to `openai` deployments
  (`cache/prompt.rs`), which also records the request's prompt prefix for `group_by=cache_prefix`.
- `POST /test/reset` scope `cache` also clears the tallies, the firings and the metadata values seen.

## Known gaps in caching

- Not implemented: `hit_usage: zero` and the `redis` and `pgvector` semantic stores (rejected at
  validation). `fail_mode: closed` and per-item embeddings: see Safety, caching and telemetry.
  Semantic vectors and coalescing leaders are per node.
- Cluster nodes share epochs and cutoffs through the store, read every second: an invalidation
  reaches the other nodes within that delay.
- Gemini and Bedrock native endpoints are never cached.
- A delete on the Valkey backend scans for the key under every scope digest when the index does not
  know it (entries written before the index existed).
- On a memory backend in a cluster, listings, caps and a firing's `dropped` are the node's own; a
  `metadata_changed` rule compares with the last value the node saw. An entry a Valkey server's own
  eviction policy removed stays listed until its expiry.

## Native dialects, cloud providers and JWT

- Cells (`antirion-pipeline/src/cell.rs`, `apis.rs`): identity when the client's dialect is one the
  deployment speaks natively, otherwise a translated cell (see Translation). Upstream bodies are
  built in `upbody.rs`.
- Streams: `relay/pump.rs` reads SSE or the AWS event stream (`antirion-wire/src/eventstream.rs`),
  `relay/events.rs` classifies each dialect's events, `relay/shape.rs` forwards them or hands them to
  the translator, and `relay/synth.rs` turns a complete response into a stream when the upstream
  answered without one.
- Provider credentials: `antirion-providers/src/creds/` (one cache per credential with a single
  flight, background refresh before expiry, backoff, one refresh after a 401); AWS from the
  environment, profile files, web identity, the container endpoint and the instance metadata
  service; GCP service-account assertions, user refresh tokens, workload identity federation
  (`external_account` with a file or URL subject token) and the metadata server. SigV4 is `sigv4.rs`.
- Responses state for translated cells and native response ids lives in node memory
  (`antirion-runtime/src/responses.rs`), and in the store in cluster mode, as do OIDC logins in
  progress (`logins.rs`). `respstate::prepare` reads `previous_response_id`: a native id pins routing to the
  deployment that created it (503 `affinity_unavailable` when that deployment cannot serve), a
  gateway-stored one expands into the stored conversation. `POST /test/reset` scope `affinity`
  forgets the native links. JWKS are cached per issuer in `antirion-runtime/src/jwt.rs`.
- A Gemini upstream's error line sent as bare JSON inside a stream is an error event
  (`SseParser::with_bare_errors`). An InvokeModel stream served by an Anthropic Messages upstream is
  re-framed as AWS event-stream `chunk` messages (`relay/invoke.rs`).
- Not implemented: executable credential sources of GCP `external_account` documents. The
  Anthropic beta-header platform table is a small built-in list.

## Translation

`antirion-dialects/src/xlate/` holds the whole translation; the pipeline only calls it
(`antirion-pipeline/src/xlate.rs`).

- **IR** (`ir.rs`): a decoded request. Every element keeps its `Src`: its depth-first position in
  the client body, its path with indexes as `[]` (warning entries) and its concrete path (`param`,
  messages). Client codecs (`decode/`: Chat with its extensions, Responses, Anthropic, Gemini and
  Converse) decode once per request; they record unknown fields at every depth and the
  `media_invalid` 400s. Gemini and Converse bodies carry neither model nor stream flag: both come
  from the path (`xlate::decode_body`). Decoding also finds the first tool result that answers no
  earlier call (`Request::unpaired`); encoding refuses such a request with the `Unpaired` refusal,
  which `resolve` turns into the 400 `tool_result_unpaired`, so translated candidates are ineligible
  and identity ones still serve it.
- **Upstream codecs** (`encode/`): one per dialect over shared parts (`turns.rs` merges turns and
  turns late system messages into user text, `reason.rs` maps reasoning controls by
  `reasoning_control`, `sampling.rs` the generation controls and structured output, `media.rs` the
  format checks and which URLs need fetching). Encoding decides each feature's class for the target:
  warnings go to `Warnings`, class U to a `Refusal`, and under `on_lossy: reject` the first A/L/K
  entry becomes a refusal too. The same `encode` call with no fetched media is the dry run that
  decides feasibility at `resolve` (`xlate::resolve` in the pipeline: the 400 when no candidate is
  left) and in the routing filters (`route.rs`, the verdict cached per deployment), so a refused
  deployment is never attempted, a fallback included; each attempt encodes again from the IR, or
  decodes again first when the position's route params edited the client's body. Tool names and ids rewritten for the
  target go in `Restore`, which the response side uses to map them back.
- **Responses** (`resp/`): upstream bodies decode into `resp::Response` (stop reason and usage,
  reasoning with its family, and per candidate its citations and web searches, `resp/cite.rs`), then
  encode for the client (Chat, Responses, Anthropic, Gemini, Converse); elements the client cannot
  carry become `response:` entries. A citation is a span of the candidate's text in characters of its
  text parts joined. A malformed Gemini or Converse answer is `Bad::Malformed`, a retryable attempt
  failure.
- **Hosted tools** (`encode/hosted.rs`): web search, decoded from every client dialect into
  `WebSearch`, is sent as each target's own search tool (approximated) or dropped where there is
  none (Converse, Cohere). Anthropic's bash and text editor tools decode into function tools with
  Anthropic's published schemas (`defined.rs`). Responses `custom` tools become function tools with
  one string `input` (`Tool::custom`); `Restore::custom` names them so their calls go back as
  `custom_tool_call` items (held whole in streams).
- **Reasoning replay** (`replay.rs`, `antirion-runtime/src/replay.rs`): a translated response's
  signed or encrypted reasoning before each tool call (and a Gemini signature on the call) is kept
  per project and client call id: complete responses in `pipeline::xlate::response`, streams through
  the translator's `Recorder` when the relay ends. `resolve` puts it back into a request whose
  assistant turn lost it (`Reasoning::restored`, never reported); the repair re-send decodes the body
  again without it. `POST /test/reset` scope `affinity` clears the store.
- **Streams** (`stream/`): a `Translator` per stream, an upstream decoder emitting `Ev`s and a client
  encoder writing frames. The Anthropic and Responses encoders keep one block or item open and hold
  tool calls that cannot open yet; terminal events wait for the end of the upstream stream. Gemini
  clients get `Frame::Gemini` chunks (the relay's JSON-array writer re-frames them without `alt=sse`)
  and Converse clients `Frame::Event` AWS event-stream messages; both receive each tool call whole.
- **Envelope** (`envelope.rs`): `ant1.<family>.<provider>.<payload>`; Anthropic, Gemini and Converse
  identity cells unwrap their own family's payloads and drop other families' reasoning (`upbody.rs`).
- Remote media are fetched once per request (`media::fetch_needs`, kept in the request's `Xlate`)
  under `api.media_fetch` caps, and reused by later attempts.
- `serde_json` is built with `preserve_order`: tool arguments keep their member order. Code that
  needs a canonical order sorts explicitly (etags, metadata bounds, collection listings, the health
  list).

## Providers, streaming and the API edge

- **Provider types** (`antirion-providers/src/kind.rs`): `anthropic_compatible` (Messages at its base URL),
  `mistral` (Chat Completions), `cohere` (the `cohere_chat` upstream dialect: `xlate/encode/cohere.rs`,
  `resp/cohere.rs`, `stream/dec_cohere.rs`, reached only through translation) and the hosted presets
  (`Preset`: an OpenAI-compatible type with a default base URL and capability defaults). A type this
  build does not support is refused with `not_supported_in_this_build`. `bedrock.api: openai` sends
  Chat Completions to `/openai/v1/chat/completions` on the Bedrock base.
- **Credentials** (`creds/`): `TokenSource` (GCP or Azure AD, `creds/azure.rs`: client secret, workload
  identity from a token file, managed identity) shares the single-flight cache and the 401 refresh;
  `auth.assume_role` (`creds/assume.rs`) signs STS `AssumeRole` with the base credentials and caches the
  role credentials until `min(300 s, lifetime / 2)` before they expire.
- **BYOK** (`antirion-pipeline/src/byok.rs`): the client key (`x-antirion-provider-key`, or the provider's
  own key header beside `x-antirion-api-key`) for keys with `byok`; eligibility in the route filters
  (`byok: required` without a key is the `byok` capability exclusion, which becomes 401
  `provider_key_required`; a deployment with `byok: disabled` is excluded as forbidden); the key replaces
  the configured one in `build_request`, is scrubbed like an obtained credential, and an upstream 401/403
  on it is `caller_error` with reason `byok_credential_refused`.
- **Edge** (`accept.rs`): passthrough mounts are matched before the catalog (`passthrough.rs`, served with
  authentication, provider lists, request limits and a record); with `api.azure_paths`, the legacy Azure and
  engine paths map to their catalog endpoint with the model taken from the path (`legacy_path`).
- **Streams**: the pump reads SSE, AWS event-stream, NDJSON or a JSON array (`Framing`, chosen in
  `streamed.rs` from the content type, a JSON body starting with `[` being an array) under
  `max_response_bytes`; events of the JSON framings are written again in the client's framing
  (`Setup::reframe`). A Gemini client without `alt=sse` reads a JSON array (`relay/array.rs`). A Converse
  answer to a ConverseStream request is synthesized (`synth::converse`). `on_client_disconnect: complete`
  detaches the writer and keeps relaying into nothing (`Writer::detach`). `stream_upstream`
  (`aggregate.rs`) streams a non-streaming request and gathers the events with the stream decoders
  (`xlate::stream::Collector`) into the IR response, which the client codec encodes and
  `outcome::json::deliver` answers like any complete response.

## Embeddings, uploads, Files, Batches and Realtime

- **Embeddings and rerank on other providers** (`antirion-pipeline/src/embed.rs`, codecs in
  `antirion-dialects/src/embed.rs` and `rerank.rs`): `cell` gives an OpenAI embeddings request a
  `Cell::Embed` on Gemini, Vertex (Google), Bedrock Titan or Cohere models and Cohere, and rerank a
  `Cell::Rerank` (Cohere, Bedrock, OpenAI-compatible servers that declare the `rerank` operation, the
  `top_k` shape for `voyageai.com` hosts). `attempt::one` hands those cells to `embed::send`, which
  makes the attempt's calls (Titan: one per text), decodes the answers into the client's shape and
  delivers it like any complete response (`outcome::json::deliver`). Token inputs make the provider
  cells ineligible.
- **Uploads** (`upload.rs`): multipart bodies are limited by `max_upload_body`. A body that ends
  within 1 MiB is held whole; a larger one is read until its file part starts and the rest becomes
  the upstream body's streamed `Tail` (`antirion-egress/src/payload.rs`), counted and timed as it
  arrives. The tail can be sent once: `attempt::one` fails a later attempt with the error of the one
  that took it, and a body that broke while relayed answers the client's 413, 408 or 400.
- **Image edits and variations** are multipart `images` endpoints (required parts in `parse.rs`,
  units from the form in `account/units.rs`).
- **Files and Batches** (`files.rs`, `batches.rs`, `provider_call.rs`): gateway ids map to the
  project, the provider and the provider's id in the store's `upstream_objects` (SQLite, and the
  PostgreSQL migration 25). Calls on an object go through `provider_call::send`: the attempt loop on
  the first deployment of the provider the key may use that declares the operation, resource-bound or
  side-effecting. A batch's input is downloaded, rewritten line by line (`batches::Lines`) and
  uploaded again as the new batch's input, all streamed. Gateway file ids in inference bodies are
  replaced in `infer` (`files::resolve_in_body`), and `Parsed::pin` confines routing to their
  provider. Lists are answered from the store. Deferred usage (a completed batch, a Realtime
  response) is written by `account::settle_deferred` under the creating request's id.
- **Realtime** (`realtime.rs`): the data listener serves HTTP/1.1 connections with upgrades; the
  origin is checked at `accept`, the subprotocol credential in `auth.rs`. The upstream WebSocket is
  opened with `EgressClient::upgrade` and both sides are `tokio-tungstenite` streams relayed by one
  tracked task per session, which settles the session's reservation and its record when it ends.

## Known gaps of embeddings, Files, Batches and Realtime

- Gemini `embedContent` and `batchEmbedContents` clients still reach only Gemini deployments.
- Batch lists show each batch as last seen through the gateway; a batch's usage is settled the first
  time a client reads it in a final status.
- Realtime sessions are bridged over HTTP/1.1 only (no WebSocket over HTTP/2), and the record of a
  session shows its own zero-usage line; each `response.done` is a ledger line of its own.

## Known gaps of providers, streaming and the API edge

- A Bedrock Anthropic Messages endpoint other than `invoke` is not implemented. Managed identity is
  untested against a real instance metadata service.
- A disconnect before a stream commits cancels it even with `on_client_disconnect: complete`.
- A gathered response is encoded again from the IR, so members the IR does not hold (logprobs, unknown
  fields) are not in it; Gemini and Bedrock native clients are never gathered.
- Passthrough mounts take no token reservation (requests are counted, not priced) and their records carry
  no attempt.

- **Responses operations**: `POST /v1/responses/input_tokens` and `/compact` are the `responses`
  endpoint with an `action`; they never stream, cache or keep Responses state. Native deployments
  serve both; `input_tokens` elsewhere goes to the Gemini or Anthropic count API, else a local
  estimate, and an Anthropic `count_tokens` reaches a native Responses deployment's `input_tokens`.

## Known gaps in translation

- The reasoning replay store lives in node memory (at most 100 000 entries); nodes of a cluster do
  not share it.
- Streams carry no citations or web searches (complete responses do). Gemini clients have no form
  for Anthropic redacted thinking, and Gemini and Converse clients none for citations.
- Gemini `countTokens` and `embedContent` and Bedrock InvokeModel clients still reach only
  same-dialect deployments.
- `thought_signatures` without a configured value is assumed for model ids containing `gemini-3`;
  there is no catalog yet.
- Anthropic `thinking.display: summarized` is sent only when the client asked for summaries
  (Responses `reasoning.summary`, Chat `reasoning.exclude: false`).

## Known gaps in MCP and A2A

- JWT credentials are accepted through the data plane's `auth.rs`; on a gateway with `oauth`, a token
  must also name one of `oauth.audiences` (default: the gateway's resource URLs). That check runs
  after the body is read, so the 401 echoes the JSON-RPC id (`mcp.rs`).
- HTTP+SSE upstream sessions are held by the node that opened them; a request for such a session on
  another node gets `upstream_session_not_found`, and the client re-initializes.
- Not implemented: `subscriptions/listen`, the agent directory, the A2A REST binding.
- Tool-definition approvals do not survive a restart, and each node reports the changes it sees, so
  a cluster may send one alert event per node.
- The tool loop's model calls do not stream: a streaming client gets the final answer synthesized
  once the final call answered, without heartbeats in between. The request is admitted once.
- Bridged elicitation replays the call: the legacy server must ask the same questions in the same
  order on the retry.
- The client-facing HTTP+SSE stream and its handle live on one node; closing it does not end the
  upstream sessions the stream's `initialize` opened (they expire upstream).
- The request-log record holds what is known when the response head is ready: a JSON-RPC error
  inside a relayed SSE answer, the tool-level outcome and cost are not recorded.

## Known gaps in observability

- Hedged attempts overlap; the overhead formula takes the union of attempt intervals.
  `include_unauthenticated` writes a record with null identity fields.
- OTLP log sinks (`sinks.otlp`) are HTTP sinks whose batches are `ExportLogsServiceRequest`s
  (`antirion-obs/src/otlp_logs.rs`, encoded in the sink worker with the gateway clock as observed
  time); the `grpc` protocol has the trace exporter's TLS-only limit.
- Alert webhooks (`telemetry/alerts.rs` for their settings and signatures, `alerting/` for delivery,
  see Alerting): counted in `antirion_alert_deliveries_total`. Events: `budget.threshold_crossed`,
  `config.applied`, `config.rejected`, `deployment.state_changed` (the health registry's changes,
  published after each request's attempts and each manual action; a time-based change is published
  by the next request), `log_sink.dropping`, `alert.fired`, `alert.acknowledged`, `alert.resolved`,
  `audit.privileged`, `auth.login_failed` and `test.ping`.
- OTLP metrics (`otlp_metrics.rs`) push the registry's cumulative state every `interval`; the
  `grpc` protocol has the trace exporter's TLS-only limit.
- The `grpc` trace protocol reaches a collector only over TLS (HTTP/2 by ALPN); a plain `http://`
  gRPC endpoint fails and its spans are counted as dropped. `http/json` and `http/protobuf` work over
  both.
- Spans exist for the root, `authenticate`, `admit` (when a limit or budget applied), each guardrail
  that ran, each extension hook, the cache lookup (when the cache applied) and each attempt; external
  calls of guardrails and extensions get no CLIENT span of their own. `timing.external_ms` is a
  placeholder (always 0); `skipped_candidates` never names a credential. `wait_ms` counts
  the attempt loop's waits before each attempt.
- The cost of a record is the settled cost of its request (`account::settle`); requests that reach
  no upstream cost 0.
- Shutdown flushes sink and exporter queues for `shutdown_flush_timeout`, but within the node's
  ten-second task wait.

## API surface, identity and tooling

- **Responses over a WebSocket** (`antirion-pipeline/src/socket.rs`): `GET /v1/responses` with an upgrade
  is recognised before the endpoint catalog (`socket::is_upgrade`, endpoint `responses_websocket`,
  kind `ResponsesSocket`) and authenticated without a record. Each `response.create` becomes a
  `POST /v1/responses` stream sent through `LocalPlane::send_from` with the socket's own `Peer`, so the
  event takes every stage of a request and the socket counts nothing. One response at a time; frames over
  `security.limits.max_body` close with 1009 (tungstenite's message cap).
- **`n` by fan-out** (`fanout.rs`): `infer` decides `Plan::fan_out` (policy, non-streaming Chat, `n` within
  `max`, not under a strict translation policy), drops the `n_multiple`/`max_n` needs and decodes the
  translation IR without `n`. An attempt whose deployment cannot serve `n` sends copies of the built and
  authorized request at once, turns each answer into the client's Chat body and delivers the merged body
  as an identity Chat answer (`outcome::json::deliver`), with `emulated=n` among the warnings.
- **Completions as Chat** (`completions.rs`): after the Model and policies are known, a Completions request
  without a native deployment is rewritten into a Chat request and the endpoint swapped; `Ctx::
  completions_as_chat` makes `infer::run` turn the answer (JSON, or the SSE stream event by event)
  back into text completions. Completions are served natively only by deployments whose `dialects` list
  `openai_completions`.
- **Catalog** (`antirion-catalog`): the effective catalog is built with each generation and held by every
  `ProviderRt`; it is the third layer of `Capabilities` (source `catalog`), gives the `ttft` hint
  (`Timeouts::ttft_with_hint`, unless the policy sets `ttft`) and the catalog prices
  (`PriceCatalog::of`, below `pricing.models`). `catalog` is a settings singleton; `GET /catalog` is
  `antirion-admin/src/catalog_read.rs`. A deployment of a generic type (`openai_compatible`,
  `anthropic_compatible`, the presets) reads its capabilities with `Catalog::lookup_any_type`, which
  after its own type's and the bare patterns takes another type's entry for the same model id;
  prices keep the type's own lookup.
- **Certificate-bound keys** (`antirion-identity/src/certificate.rs`, `antirion-pipeline/src/certbind.rs`):
  the listener records the verified leaf's digest and subjectAltNames in `Peer::client_cert`; behind a
  trusted proxy with `auth.forwarded_client_cert`, RFC 9440's `Client-Cert` stands in. The binding is
  checked right after the credential, and a mismatch is the answer of a wrong key.
- **Device authorization** (`antirion-admin/src/device.rs`, `antirion-identity/src/device.rs`,
  `antirion/src/login.rs`): grants are identity records (`device_grant` by the device code's digest,
  `device_user_code` by the normalized code), decided with `swap_record` and redeemed with
  `take_record`, so any node serves any step. The token is a personal admin token. The CLI keeps tokens in
  `$XDG_CONFIG_HOME/antirion/credentials.json` (`ANTIRION_CREDENTIALS` overrides it).
- **MCP tool search** (`antirion-mcp/src/toolsearch.rs`): intercepted before list and item dispatch in both
  eras; the catalog is the merged, grant-filtered tool list; `call_tool` checks arguments with
  `UserSchema::first_violation` and forwards a `tools/call` of the target.
- **Admin MCP server** (`antirion-admin/src/admin_mcp.rs`): `/admin/mcp` behind `mcp.admin_server.enabled`,
  after the Admin API's authentication; each tool calls the API's own read with a copy of the `Call`.
- **Reproducible builds** (`ci/reproducible.sh`): builds a package twice in two fresh copies of a commit
  (`git archive`, with a prebuilt `console/out` copied in when there is one) with `--locked`,
  `SOURCE_DATE_EPOCH`, `ANTIRION_BUILD_COMMIT`, `CARGO_INCREMENTAL=0`, the source, Cargo home and rustup
  paths remapped, `-ffile-prefix-map` for C code and, on macOS, `-oso_prefix` for the object paths the
  linker records; it compares the outputs. Locally, check a small package (`--package xtask --bin xtask`
  or `--package antirion-json --lib`); the full gateway is checked by the release workflow, in its pinned
  container, before the release build (which uses the same settings). The console export takes
  `ANTIRION_BUILD_COMMIT` as its build id.

## Known gaps of the API surface, identity and tooling

- A fanned-out attempt never streams, and its record shows one attempt for the n calls.
- A Responses socket's events reach the data plane through an in-memory HTTP/1.1 exchange, so they are
  limited by the node's request limits like any request but are not counted as connections.
- `antirion auth login` speaks HTTP/1.1 only and trusts the public roots plus `--ca-file`.
- The upstream error tables and the platform beta tables are not part of the runtime catalog.

## External secret stores

- `${vault:…}`, `${aws-sm:…}`, `${gcp-sm:…}` and `${azure-kv:…}` resolve through
  `antirion-runtime/src/secretstores/`: `settings.rs` compiles `security.secrets.stores` (its own
  `${env:}`/`${file:}` resolved there), `vault.rs` and `cloud.rs` read one reference each, `fetch.rs`
  sends through the egress client as operator traffic. Resolution stays synchronous: the runtime's
  `SecretSource` is a `Resolver` that reads only the node's cache (`ExternalSecrets`). Only a write
  reads what it lacks (`Runtime::prefetch_secrets` in `mutate`, after the writer's permission and a
  check without the stores; cluster catch-up; `prefetch_at_startup` before the first snapshot, with a
  client of its own because the runtime does not exist yet). Checks a reader may run
  (`config:validate`, `config:diff`, change-request previews, route simulation) and dry runs use
  `Runtime::check_unread`, whose `Unread` source takes a value not read yet as read, so a submitted
  document never makes the node contact a store.
- Through the Admin API, store settings may not read the node's environment or files and a token
  file is one of `API_TOKEN_FILES` (`secretstores::check_api_write`, run by `mutate` for every write
  but the configuration file's, and by `validate_candidate`): store settings are sent to the store.
- `secretstores::run` reads values whose store `ttl` (or a shorter Vault lease) has passed, every
  second; a changed value rebuilds the serving snapshot (`secretwatch::rebuild`), a failure keeps the
  value and raises or re-raises the built-in alert `secret_store_unavailable` at most once a minute.
  `GET /runtime/encryption` reads the per-store status.
- `antirion validate` takes store references as they are (`startup::Offline`); a node checks them
  when it starts.

## SAML single sign-on

- `antirion-identity/src/saml/`: everything that reads or writes SAML. `xml.rs` parses with
  `roxmltree` (no DTD, bounded size, node count and depth) and reads qualified names from the source,
  which canonicalization needs; `c14n.rs` is exclusive canonicalization of one subtree minus the
  enveloped signature; `dsig.rs` verifies one enveloped signature against the configured
  certificates only (`aws-lc-rs`; the message's key material is ignored); `response.rs` checks a
  response and returns facts read from the verified assertion only; `metadata.rs` reads an identity
  provider's metadata and writes the service provider's; `request.rs` builds the HTTP-Redirect
  `AuthnRequest`; `config.rs` compiles `auth.admin.sso.saml` and its validation rules. The unit
  tests verify responses signed by another implementation (`testdata/`, made with libxml2's
  canonicalization) and the wrapping, comment and replay cases.
- `antirion-runtime/src/saml.rs`: metadata fetched from `metadata_url` is a node value
  (`saml_metadata/<provider>`) every node reads, parsed once per stored copy; accepted assertion ids
  are `saml_assertion` identity records written with `Store::put_record_once` (first writer wins until
  the record expires), so an assertion is used once across nodes.
- `antirion-admin/src/saml.rs`: login, ACS and service-provider metadata are public and matched before
  authentication (`lib.rs`); the pending request is an `OidcLogin` stored under `saml:<request id>`,
  so it reaches any node like an OIDC `state`; sign-ins end in `oidc::SignedIn::session`. Status and
  resync are ordinary authenticated routes.

