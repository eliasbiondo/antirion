# Antirion console

The admin console: a static export (Next.js App Router with `output: 'export'`, React, TypeScript strict,
Tailwind, Zustand) that the gateway embeds and serves on its admin listener under `admin.ui.path`
(default `/ui`). Every byte of data comes from the Admin API (`/admin/v1`) in the browser; there are no
server components that fetch, no route handlers and no middleware.

## Commands

| Command                          | What it does                                                                                                                                                         |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pnpm install --frozen-lockfile` | Installs dependencies (Node 22 or newer, pnpm from `packageManager`).                                                                                                |
| `pnpm dev`                       | Dev server on `:3000`, proxying `/admin/v1` to `ANTIRION_DEV_ADMIN_URL` (default `http://127.0.0.1:20481`). Open `http://localhost:3000/ui/`.                        |
| `pnpm dev:mock`                  | The same, with the contract mocks in the browser for the endpoints a gateway does not serve yet (see below) plus demo data.                                          |
| `pnpm build`                     | Static export to `out/`, then `scripts/externalize-inline.mjs` moves Next's inline scripts to files and fails on any inline script or style left.                    |
| `pnpm serve`                     | Serves `out/` under `/ui` with the gateway's security headers and proxies `/admin/v1` (for checking a build without rebuilding the gateway).                         |
| `pnpm lint`                      | ESLint (Next and typescript-eslint strict) with no warnings allowed, then the repository's open-work marker check over the console.                                  |
| `pnpm typecheck`                 | `tsc --noEmit`.                                                                                                                                                      |
| `pnpm test`                      | Unit and screen tests (Vitest, Testing Library, MSW).                                                                                                                |
| `pnpm e2e`                       | Playwright against a real test-mode gateway (see below).                                                                                                             |
| `pnpm format`                    | Prettier.                                                                                                                                                            |
| `pnpm api:generate`              | Regenerates `src/lib/api/schema.d.ts` and `src/lib/api/operations.ts` from `../openapi/admin-api.yaml`. Run it after the contract changes; both files are committed. |

## Embedding in the gateway

- **Output**: `console/out`. `crates/antirion-console` embeds `ANTIRION_CONSOLE_DIR` when set, else
  `console/out` when it holds an `index.html`. After a new export, rebuild with
  `ANTIRION_CONSOLE_DIR=$PWD/console/out cargo build -p antirion`.
- **Base path**: `basePath` and `assetPrefix` both equal `admin.ui.path`. The default is `/ui`; build for
  another path with `ANTIRION_UI_PATH=/console pnpm build`.
- **Layout**: `trailingSlash: true`, so each route is `<route>/index.html`; RSC payloads (`*.txt`) sit next
  to them and must be served too; `404.html` is the not-found page.
- **Headers**: the export runs under `Content-Security-Policy` with `default-src 'self'` and
  `script-src 'self'` / `style-src 'self'` (no `unsafe-inline`), and `X-Frame-Options: DENY`. The build
  contains no inline `<script>`, `<style>` or `style=` attribute; dynamic colours are set through the CSSOM.
- **Sign-in**: the admin token (or password and second factor) goes once to the Admin API, which sets the
  HttpOnly `antirion_session` cookie. Nothing secret is kept in the browser: only display preferences and favourites
  are persisted (`localStorage` key `antirion.console.preferences`, mirrored to `/auth/me/preferences` for
  user accounts), with the names of recently opened items and recent searches (`antirion.console.recent`,
  this browser only). Writes send the console's own `Origin`, as the gateway requires for cookie sessions.
- The gateway's mailed links land on `/ui/accept-invitation?token=…`, `/ui/reset-password?token=…` and
  `/ui/confirm-email?token=…` (a new email address).

## Layout

```
src/app/                 routes only: (console)/<screen>/page.tsx, login, accept-invitation, reset-password, confirm-email, device, not-found
src/features/<domain>/   components/, store.ts, api.ts, hooks.ts, types.ts, __tests__/
src/components/ui/       design system: buttons, forms, tables, dialogs, drawers, charts, states, reveal-once
src/components/layout/   shell: sidebar, top bar, time range, scope, account menu, command palette
src/lib/api/             generated types and operation table, the fetch client (errors, ETags, 401)
src/lib/                 formatting, time model, validation (zod), CSV, collection store, hooks
src/stores/              cross-cutting stores (below)
src/mocks/               MSW handlers typed from the contract; dev:mock and tests only, aliased out of builds
src/styles/              tokens (light and dark), Tailwind theme, bundled fonts
tests/e2e/               Playwright suite and the test-mode gateway it starts
```

Stores: `session` (principal, permissions, expiry; never the token), `preferences` (theme, density, time
zone, refresh, price unit, favourites, default organization and range; persisted), `time-range` (preset
or custom window, bucket, comparison window, in the URL), `scope` (gateway or organization,
environment, in the URL), `shell` (version, counts, nodes, ingress, pending change requests), `ui`
(toasts, palette, shortcuts, a change request to open), `recents` (items opened and searches run,
this browser only), `provider-types`, and one store per feature for its data and selection.

## Live screens

Every operation the console calls is served by the gateway; no screen depends on the contract mocks
any more. A live pass against a test-mode gateway with traffic opened every screen, each of its tabs,
every Settings section and the key and Model details, and every Admin API read they made answered 2xx:

| Screen          | Reads checked against the gateway                                                                                                                                                                                                               |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Overview        | usage analytics (current and comparison windows), availability, newest requests                                                                                                                                                                 |
| Request log     | records, saved views, facets and aggregates                                                                                                                                                                                                     |
| Analytics       | usage by Model, provider, team, deployment and guardrail category, month to date, provider pressure, budget counters, configuration generations, alert signals                                                                                  |
| Playground      | projects, Models, playground sends (streamed and whole), the sent request's record                                                                                                                                                              |
| Alerts          | inbox, 60 days of instances, similar fires, signals, rules, channels and their delivery status, event webhooks and their status, SLOs and daily availability, usage for rule previews                                                           |
| Budgets         | ledger lines (Ledger tab), counters, budget events, overage requests, organizations, teams, projects, keys, users, Models, usage by day, team, Model and person, refusals, price settings and deployment prices                                 |
| Safety          | guardrails, safety events, guardrail runtime, the gateway's policies, safety counts by category and entity, the last hour of usage by provider, the configuration and its generations (posture), users, keys, teams, Models                     |
| API keys        | keys, anomalies, usage per key and hour, a key's limit counters and source addresses, auth settings, organizations, deployments' regions, the keys audit feed                                                                                   |
| Teams & users   | users, teams, projects, organizations, roles, role bindings, invitations, a user's sessions, flags and usage, team spend, sign-in settings, SCIM status, the identity audit feed                                                                |
| Models          | Models, deployments, providers, routes, tiers, effective prices, deployment health                                                                                                                                                              |
| Providers       | providers and their runtime readings, deployments and their health (the state of each key)                                                                                                                                                      |
| Cache           | settings, status, entries and their responses, rules and their firings, usage by hour, Model, team and prompt prefix, prices, routes, overrides, audit, dual control, change requests                                                           |
| Routing         | routes, tiers, deployment health, route state, usage by tree position and guardrail category, recent records and the live tail, audit, alerts, defaults (policies, resilience, server), change requests                                         |
| Change requests | change requests in every state                                                                                                                                                                                                                  |
| MCP             | MCP servers and each one's status (tools seen, their approval, the last contact), gateways, session counters, the `mcp` settings                                                                                                                |
| Agents          | A2A agents                                                                                                                                                                                                                                      |
| Plugins         | plugins, their runtime counters, the gateway level's policies (attachments)                                                                                                                                                                     |
| Settings        | configuration and its versions, audit log, health, nodes, status, auth, security, encryption, limits, observability, policies, governance, routing, guardrails, webhooks, audit sinks, SCIM status, held actions, organizations and export jobs |

`pnpm dev:mock` still serves the contract mocks with demo data, for working on a screen without a
gateway; the unit tests use the same handlers. A gateway older than the console answers 404 for what
it lacks, and the console shows "isn't available on this gateway yet" in that place.

## Change control

A write the gateway's change control refuses (409 `change_request_required`) is not an error: the
console keeps the call and offers to open a change request with it. A write the two-person rule holds
(202 with a change request) is not a success: the console says approval was requested and links to
the request. Both come from the API client (`setChangeControlHandler`), so every screen gets them;
`ConfirmDialog` closes and `InlineError` shows a note instead of an error. The Change requests screen
reviews, applies and cancels them.

## Shared helpers

- `useUrlFilters(defaults)` (`src/lib/url-filters.ts`): a list's filters in the URL, so a copied link
  reopens the same view; `useStarredFilter(kind)` builds the Starred filter on it. Every list uses it
  (keys, users, safety events, the alert inbox, budgets, routing rules, Models, cached responses);
  `setWith(filters, params)` changes filters and a tab or selection in one navigation, since two in a
  row race. Unit tests' router re-renders on `replace`, and each test starts without a query.
- `<CopyLinkButton>` copies the current URL: an entity opened from its URL has one (`useQueryParam`
  for the selection; budgets, routing rules, cached responses, teams, organizations and projects too).
- `exportCsv` and `<ExportCsvButton>`: CSV of the rows a table shows, with typed columns.
- `<EmptyState>` (`src/components/ui/states.tsx`) is the one empty state: a small bordered pill with
  muted text, at most one muted line and quiet `<EmptyAction>`s; `size="sm"` in charts, cards and
  drawers, `md` in place of a page's table. `<EmptyPill>` is the bare pill for charts and popovers;
  `<NoMatch>` adds "Clear filters". Errors stay `<ErrorState>`.
- `<Section>` is a flat, full-bleed page section with a hairline under it (title, muted subtitle,
  actions right; `flush` for a full-width table); `<Panel>` is the same inside a `<TileGrid>`.
  `<TileGrid cols={{ base: 1, lg: 2 }}>` fills a short last row with blank tiles, so every tile keeps
  its hairlines. No boxed cards inside pages.
- `<KeyboardHints>` under list screens, `<FavouriteButton>` on details.
- Marks (`src/components/ui/marks.tsx`, resolver in `src/lib/marks.ts`): `<ProviderMark>`
  and `<ProviderName>` wherever a provider appears (its type's brand; generic types a monogram);
  `<ModelMark>`, `<ModelName>` and `modelOption()` wherever a Model appears, in rows, chips, menus and
  every select that lists Models (its maker's brand from `owned_by`, `catalog.family`, its upstream
  ids or its name, never the provider that serves it; unknown makers a monogram). Sizes `xs` 12 px
  (chips, badges), `sm` 14 px (rows), `md` 16 px (menus, options, titles), `lg` 18 px in a
  `<MarkTile>` (the Models list). Marks are monochrome in the text colour; never draw a brand logo by
  hand or pick a mark from a provider's name. Path data from `THIRD_PARTY_NOTICES`.
- `useTokenPrice()`: per-token prices in the user's unit (per 1M or per 1K tokens).
- `<PlaygroundLink>` (`src/features/playground`): "Try in playground" for principals with
  `playground:send`, with an optional Model, endpoint, project and body (a body goes through memory,
  never the URL). Models uses it in its header and on a Model; the request drawer's replay dialog
  opens the stored or edited body with it.
- `<TimeChart annotations>`: events on a time axis (configuration changes, alerts), listed in the
  hover card and the hidden table.
- `<SavedViews>` takes an optional star toggle, a menu per view, a note line and a header action; the
  request log uses all four for its saved views.

## Request log

The log's filters and sort are in its URL (`status`, `provider`, `model`, `team`, `key`, `environment`,
`tag`, `cache`, `stream`, `flagged`, `latency_min`, `latency_max`, `cost_min`, `cost_max`, `q`, `sort`
as `key:order`), saved views store them in the list's query-parameter names, and the column choice is
the `log_columns` preference. Live mode reads the gateway's live tail (server-sent events) and checks
the filters the tail does not take on each record; it falls back to polling for newer records. The
development server does not compress the proxied Admin API, which would buffer the tail.

## Playground

`/playground/` sends through `POST /playground:send` with a key the gateway mints and keeps. A
streamed send asks for `text/event-stream`: the gateway answers with a `playground` event (status,
decision headers, log id, key) and then relays the data plane's events as they arrive, so the answer
shows word by word; a gateway that answers JSON is read whole. The usage, cost and timing come from
the request's record once the gateway writes it. `model`, `endpoint` and `project` are in the URL,
and `request=<log id>` reopens a logged request's stored body. A development server on another origin
than the gateway's needs `auth.admin.public_origin` set to it for cookie-session writes (sends
included).

## Analytics

Besides the usage tables, Analytics reads the scope's and the teams' monthly budget counters
(`policies:read`), provider pressure (`/reports/provider_pressure`), configuration generations and the
latency objective (`config:read`), the open alert instances with their signal series, and the attempt
trails of recent fallbacks. Usage reads page through the analytics API (a week of hourly buckets is
more than one page). One "Export CSV" menu exports each table.

## Models

The list's filters (`q`, `provider`, `status`, `tier`, `modality`, `label`, `sort`), the selected
Model (`model`) and the tab (`tab`) are in the URL. A row's modality comes from the operations a
deployment, its provider or the catalog states (a provider type's defaults list every operation, so
they are ignored), else from the upstream model id; its prices are the lowest of its leaves'
effective prices (`GET /pricing/deployments`, one read), in the reader's unit. The hero compares the
last 30 days with the 30 before, names the Model with the largest spend and the one with the worst
error rate (20 requests or more) against the gateway-wide `error_rate` alert rule, and its provider
bars filter the list. A Model's page adds its usage by team, key and requested name, its latency
histogram (merged to 12 buckets) and the alerts scoped to it; the p95 sort is the request-weighted
mean of the daily p95s. Writes: the fallback chain and aliases replace the Model's spec (`PUT`, with
its ETag), access lists and promotion merge (`PATCH`), access requests are approved or denied with
their own operations, default Models and custom tiers merge into `routing`, retries and prompt
caching into `policies`. The price grid shows a chat Model's service-tier and batch multipliers and
edits them on the list price `pricing.models["<provider_type>/<upstream_model>"]` of a leaf
(`tiers.ts`), a merge patch that removes dropped tiers with `null`. The Routing tab maps each Model's 24-hour `route_node` paths to its rules
(`<route>/r<i>/…`, `<route>/default/…`).

## Routing

The rule list is the conditional root of a shared route (`route` in the URL, the first one with rules
otherwise); the tab (`tab`: rules, health, live, changes) and the strategy filter (`strategy`) are in
the URL too. The wizard writes each rule's target as a route-tree node: weighted, fallback, least
busy and least latency over deployments whose role labels are the children's ids; a canary as a
weighted node with `canary` (ramp steps in percent, the error budget); a shadow as a deployment with
`mirror`; a tier node; sticky splits as `sticky_by`; per-rule timeouts, attempts and the error classes
that move on to another deployment under the node's `policies.resilience`. Conditions write
`identity.*`, `request.*`, `metadata.<key>`, `guard.<name>.<passed|verdict|category>` and `sample`.
An adaptive split is a weighted node with `algorithm: thompson`; a self-hosted pool is a `pool` node
with `picker`, `picker_service` and `picker_on_error` for an external endpoint picker and
`scheduler.lora` for adapter affinity; "Strong or weak model" is a `cost_quality` node with the
matrix-factorisation scorer (another scorer is kept); for strategies that move on, hedging and the
answers that count as failures go to the target's `policies.resilience.hedging` and
`retry.retry_on_response` (`resilience.ts`). Settings the wizard doesn't build (nested nodes, other node
types) are kept as written and shown read-only. Duplicating a rule inserts a copy under it, turned off. Each rule's
24-hour share comes from usage by `route_node`, its last editor from the route's audit entries (the
newest write that changed that rule), its canary and shadow results and a pool's endpoint picker (address, state, last pick and the
deployment that served) from the route's state, and its
alerting status from firing alerts scoped to its route or deployments. The hero adds the request rate,
the decision time (the `resolve` and `route` stages of the last hour's records), the fallback share
and blocks by guardrail category. Health and defaults reads deployment breakers and `/health`'s
admission, and edits retries, timeouts, hedging and failed answers (`policies`), the breaker
(`resilience`) and admission (`server`) as merge patches; "Reset sessions" ends a route's affinity bindings. Live decisions read
the live tail (polling when it is refused) and keep the route's newest 50. Changes lists the open
change requests that touch routes or these settings, and a feed of route and default edits, routing
alerts and stopped canaries. "Export YAML" writes the `routes` and `models` of the configuration.

## Cache

The hero compares the last 24 hours with the 24 before (one read of 48 hourly buckets): saved, the
hit rate and its change in points, what was paid to providers and what it would have cost without
the cache; a storage that can't count its entries says "Not reported". The chart draws the dollars
saved as a line on its own scale (`<TimeChart overlay>`). The tab (`tab`: models, entries, rules,
stats, activity) is in the URL. Models: response caching, similar-prompt matching and provider
prompt caching are separate switches (`enabled`, `semantic.enabled`, `prompt_caching.auto_inject`);
a Model's details show its key strategy (presets over `key.ignore_fields`; a field that changes the
answer can't be ignored), sizes, entry cap, read-only and fail mode, per-item embedding caching,
the cached-read discount of its deployments (`1 - cache_read / input` of the effective prices), its
`note` annotation and its 30 days; the settings drawer writes only the members that changed, as a
merge patch of the Model's `policies.cache`. Cached responses keep their filters in the URL (`key`,
a hexadecimal prefix; `match`; `model`; `sort` as `key:order`), page with "Show more", total the
filtered set, open a drawer whose stored response shows only with `logs:read_content`, and remove a
selection one call per entry. Clearing rules show `/runtime/cache/invalidation_rules` firings and
are created, replaced (with the ETag) and deleted in an editor with two presets. Statistics read
usage by team and by `cache_prefix` and `deployment` (savings are cached reads at the input price
less the cached price), the status's semantic groups, and the `policies.cache` of organizations,
teams, projects, keys and routes. Activity is the audit log's `cache.*` entries plus writes to the
cache settings, the gateway policies and Models that changed a cache policy. Clearing the whole
cache says when `governance.dual_control` covers it and shows the open change request.

## Budgets

The tree nests organization, team, project and key budgets by the tenancy, shows a partitioned
budget as one row with its partitions inside, and puts an organization's per-user seats under the
person's team budget or "Unassigned". State and projection are the gateway's (`state`, `forecast`)
when it reports them. Row facts (Models covered and used, projects, people, a project's share of
the team's spend) and the People tab read this month's usage by `<level>,model` and by `user`; the
top issue's refusals in the last hour are usage with `code=budget_exceeded`. The drawer writes the
budget as a merge patch on its owner, with `null` for what the form turned off; a partition row
writes its value in `overrides`. Approve overage calls `:grant_overage` (with the partition), and
approved overages are the counters' `grants`, revoked one by one. "Cost tools" re-costs whole days
(`POST /reports/spend:recost`) and edits per-Model service-tier and batch multipliers in
`/settings/pricing`. The tab (`view`: people, activity) is in the URL.

## API keys

A key's type is the gateway's `metadata.type`. The list filters by type, status, team, owner and
text, and each row carries its tightest budget. The summary's sparklines come from one read of 48
hourly buckets; whether anomaly detection is on from `/settings/auth`. The tab (`view`: teams,
rotation, activity) and the key (`key`) are in the URL. A key's page reads its hours, its limit
counters (`/runtime/limits?level=key`) and its source addresses (usage by `client_ip` and
`client_country`). The rotation tab edits an organization's `key_policy` (reading its ETag just
before) and lists keys due within 14 days; the activity tab is `/audit?resource=keys/*`. Hiding
the unusual-activity notice is the `key_anomaly_notice` preference.

## Teams & users

A user is flagged while a key they own has an open flag, and inactive after 60 days without
activity (counted from creation when never active); the status filter has both and "need review".
The summary's seats come from `/status`; at the cap, invite, add and enable of a person are turned
off. A user's page reads their usage (`user=` per day over 30 days and the last 24 hours, against
the primary team's 30 days), their open flags (`/anomalies?user=`, dismissed in place), their
sessions and, from `/roles`, what their roles let them do per scope. "Downgrade to viewer" replaces
each binding by the viewer role at its scope, one binding at a time. Custom roles are written as a
merge patch of `admin.roles` in `/settings/auth`. Organization and project details open from their
table (`detail` in the URL, since `org` is the scope); a delete with anything below sends
`cascade=true`. The Access tab reads `/settings/auth` and `/auth/scim/status`; the Activity tab
merges the audit feed of users, invitations, role bindings, key rotations and flags. The tab is
`tab` in the URL.

## Providers

A row opens the provider's keys: the names of `auth.api_keys` (or `default` for `auth.api_key`),
each with its age against `credential_rotation_max_age`, its state over the provider's deployments
(from `/runtime/deployments`: set aside on any deployment means not working) with "Use it again
now" (the key's runtime reset), and the latest upstream rate-limit reading the gateway kept for it.
The list's "Key age" is the oldest key's.

## MCP, agents and plugins

The **Extend** group of the sidebar holds them. MCP's tab (`tab`: gateways, sessions), the open server
(`server`) or gateway (`gateway`) and the servers' filters (`q`, `transport`, `state`, `starred`) are in the
URL. A server's status is read once per server with the list and on every refresh: its health is the last
contact (`last_contact_at`, `last_contact_error`: the node's last attempt to initialize it or read its
tools), not `last_error`, which is the modern era's discovery probe. Its tools come with their fingerprint
and whether they match the approved one; "Approve current tools" calls `:approve_tools`. Saving a server,
gateway, agent or plugin replaces it (`PUT` with the read's ETag), keeping the members the form doesn't
show; a stored secret (a header value, a client or signing secret) reads back as `[REDACTED]` and is
written back as such, which keeps it. A `stdio` server is file-only and shown read-only. Sessions are
counts of the answering node. Agents and plugins keep `agent` and `plugin` in the URL with their filters;
a plugin's "Every request" switch adds or removes it from `plugins.attach` of `/settings/policies`
(read just before the merge patch, with its ETag). Servers, agents and plugins are favourites
(`mcp_server`, `agent`, `plugin`), recent items and ⌘K results.

## Ledger, erasure and test mode

The request drawer lists the request's cost ledger lines (`/reports/spend/requests/{id}` with the
record's `id`, the log id that ledger lines carry as `request_id`) for `analytics:read`. Budgets has a
Ledger tab (`view=ledger`) over `/reports/spend/ledger` for the page's time range, with `team`, `project`,
`key`, `user`, `end_user`, `model`, `deployment`, `tag` and `status` in the URL, pages of 100 with "Show
more", and CSV of the loaded lines. With `logs:delete`, the request log's "More" menu and the drawer's
end user erase an end user's records (`DELETE /logs?end_user=`). While `/status` says `test_mode`, the
Status settings section shows the test clock, a reset by scope and the seed to gateway admins
(`features/test-mode`, mounted by one line in `health-section.tsx`).

## Settings: data, configuration and danger zone

Data & privacy edits `policies.observability` of the scope's level (`/settings/policies`, or the
organization's `spec.policies`); retention stays in `/settings/observability` for the whole gateway,
and a retention that `governance.change_control.protected_paths` guards says before saving that it
opens a change request. Custom entities are `regex` guardrails with a label, added to the level's
`policies.guardrails.input`. Configuration versions compare the running document with a stored one
in the browser (`doc-diff.ts`): `config:diff` merges and would not show what a rollback removes.
The audit log asks for `kind=write` unless reads are shown. The danger zone reads `/status`
(dry run, pauses, the next maintenance window), `/settings/governance` (the two-person rule and the
windows, written as merge patches) and the open change requests of kind `action`; an organization's
pending deletion is `metadata.deletion` on its read, and an export job is followed every 2 s while
it runs. While the dry run, a gateway-wide pause or a maintenance window holds, every page shows a
notice (`gateway-notices.tsx`, mounted by the shell).

## Safety

The rule drawer writes guardrails with `POST /guardrails` or `PUT /guardrails/{name}` and the read's
ETag (settings the form doesn't show are kept), tests the saved rule with `:test`, and places it on
the gateway's input and output lists with a merge patch of `/settings/policies`, keeping each rule's
place (the order is the order of execution) and the stages a method can use (`rules.ts`
`stagesFor`). "Allow similar" and "Block similar" open it with the change applied
(`prepareSimilar`). The posture panel reads `GET /config` and, for its 30 days, the generation in
force at the end of each day (`posture.ts`); people and keys are read as they are now. The data
flow is the last hour of usage by provider; "Events over time" is the safety summary by category at
the range's interval. The personal data cards filter the feed with `entity` in the URL; the tab is
`view`.

## Sign-in and account

The invitation and reset pages read their link with `:check` before showing a password field. "Trust
this browser for N days" appears only while `login_options.trusted_device.enabled`, under the single
sign-on buttons (OpenID providers only, sent as `trust_device=true`) and on the second-factor step.
A gateway with `setup_required` opens on the bootstrap token. The photo is a small `data:` image in
`ui.photo` of the preferences (`features/account/photo.ts`); the language is the `locale`
preference over the catalog in `features/account/i18n` (English is the only translation so far).
Trusted browsers are the sessions with `trusted: true`; forgetting one ends it. Notifications store each
channel's choice as the gateway reads it (`notifications.email` and `.push`: `{severities, categories}`, no
categories meaning all). Browser push (`features/account/push.ts`) registers `public/push-worker.js`, a
same-origin static service worker under the console's path, subscribes with the key of `GET /auth/me/push`
and posts the subscription; the browser keeps only its own subscription id, to name its row.

## Settings: identity, access and integrations

Sign-in & SSO writes `/settings/auth` as merge patches with its ETag: the sign-in policy (password sign-in,
second steps, password rules, domains that must use single sign-on, sessions, trusted browsers) and the
organization's `local_login` in one form, and each OIDC, SAML or GitHub provider under
`admin.sso.<protocol>.<name>` with only the members that changed (a cleared one as `null`, a secret left
blank kept). People per provider come from users' `last_login_provider` over 30 days; the test sign-in
opens `…/login?test=true` in a new tab (OIDC and SAML). A SAML provider's editor reads
`/auth/saml/{provider}` and fetches its metadata with `:resync`. API access edits the scope's organization
(the default one for all organizations): `allowed_ips` with `set:` names from `auth.ip_sets`, its
`policies.limits` entries (with `priority_shares` and `priority_mode`, `priority.ts`) and its key
lifetimes, and checks a request with `/runtime/limits:simulate`.
Notifications' connections open the Alerts channel editor over Settings (`alert-editors.tsx` loads the
alert configuration without the alert history), and write the OTLP collector (`metrics.otlp`, `traces`
and one OTLP log sink), warehouse and trace-ingestion request-log sinks and `security.secrets.stores`;
"Changed by" is the newest of the last 200 audit writes that touched the connection's path
(`last-change.ts`). The notification matrix (`matrix.ts`) adds and removes channels on alert rules (a
severity's metric rules, or the category's `notify-<row>` event rule) and events on webhook
subscriptions. Webhooks open the Alerts webhook editor; their delivery log and each alert channel's read
`/alerts/deliveries?webhook=`. Audit log destinations are `governance.audit_sinks`; the email server is
`/settings/smtp` with a test message.

## Alerts

The summary's 30-day figures (time to resolve against the 30 days before, noise, notifications by
channel, fires over 24 hours or 30 days) come from every instance fired in the last 60 days, read
again at most every 5 minutes; the error budget's burn line from the gateway's daily availability.
Rules are created from eleven built-in templates (they live in `features/alerts/templates.ts`; the
gateway has no template operation), a blank metric rule or an event subscription, and edited in a
drawer that says the condition in words and replays the last 7 days of usage to say how often the
rule would have fired (`preview.ts`; usage is read in parts of at most 1,440 buckets). Channels
(Slack, PagerDuty, email, event webhook) and event webhooks (`observability.alerts.webhooks`,
written as a merge patch of the observability settings) have their own editors, test sends and
last delivery; a secret left empty is sent as `[REDACTED]`, which keeps the stored one. The event
sources the gateway emits are listed from `events.ts`, with who subscribes to each. Editors open
over each other (a channel from a rule) through `editors.ts`.

## Tests

- `pnpm test`: logic (formatting, time model, CSV, validation, client headers and errors, stores) and
  screens (sign-in steps, API keys, Alerts) with MSW handlers typed from the contract.
- `pnpm e2e`: builds nothing itself. Build the export, then the gateway with it embedded, then run:

  ```
  pnpm build
  (cd .. && ANTIRION_CONSOLE_DIR=$PWD/console/out cargo build -p antirion)
  pnpm exec playwright install chromium
  pnpm e2e
  ```

  `tests/e2e/start-gateway.mjs` starts `../target/debug/antirion serve --test-mode` (override with
  `ANTIRION_BIN`) on 20480/20481 with a fresh embedded store under `~/.cache/antirion-console/e2e-gateway`,
  the bootstrap token `dev-bootstrap-token-0123456789abcdef0123` and local accounts on. A gateway already
  on those ports is reused. Set `ANTIRION_E2E_CONSOLE_URL` to test `out/` through `pnpm serve` against a
  gateway built without the console. The suite covers sign-in and sign-out, an invitation through TOTP
  enrolment to personal tokens and a second sign-in, security headers and CSP on every screen, create,
  edit and delete for Models, API keys and teams, and the request log's live tail, drawer, replay,
  saved views, URL filters and retention against a mock upstream it starts itself, the playground's
  streamed answer, its record, and a logged request reopened in the playground, and an event webhook
  tested against a receiver, a webhook channel and a rule from a template edited, turned off and deleted,
  and a Model's fallback chain, alias and shared route edited, an access request approved, a preview
  Model promoted and a custom tier added, and a routing rule's share, shadow results and last editor,
  a duplicated rule, a canary built in the wizard with a sample and per-rule limits, the defaults
  edited, a route's sessions reset, live decisions, the YAML export and a pending change, and a Model's
  cache set to fail closed with per-item embeddings, prompt caching and a key preset, a cached
  response opened and removed, a similar match reported, and a clearing rule created from a preset,
  run, seen in the activity feed and deleted, a blocked budget's overage approved from its row, listed and
  revoked, a rolling budget that lets cache hits through, the People tab and a re-cost, an ownerless key typed and
  filtered as a service key, its priority class, the organization's key policy, keys by team and the key activity feed,
  a custom role created, edited and deleted, a user's sessions, permissions and activity, the seat cap, an
  organization's details and its deletion with everything in it, and the terminal sign-in hint with a command-line
  sign-in approved on the device page, an adaptive split built in the routing wizard, a self-hosted pool with
  an external picker, adapter affinity, hedging and a failed-answer check built in the wizard and its picker shown,
  a strong-or-weak rule refused for a missing model file, hedging and a finish-reason check in the defaults, a
  default limit with priority shares, tier prices edited in the Models price grid, the catalog prices the Models
  screen names, and a key bound to a client certificate, a safety rule created, tested and deleted, an event's
  "allow similar" saved and a strictness preset applied, and an account's photo, email change, language and the email
  landing page, and an uploaded organization logo, OIDC, GitHub and SAML providers added, edited, tested and removed
  with a SAML provider's metadata fetched from a test identity provider, the sign-in policy, the IP allow-list and an
  IP set, a default limit, key lifetimes and a limit check, the OpenTelemetry collector, warehouse and trace-ingestion
  copies and Vault connected, the notification matrix, the email server and its test, a webhook paused, resumed,
  tested and its delivery log, and an audit log destination added, tested and removed, and an MCP server added with
  pinning behind a new gateway whose changed tool is approved, an agent and a plugin added, switched and deleted, a
  request's ledger lines in its drawer and the Budgets ledger, its end user's records erased, and the test-mode clock
  and reset.
