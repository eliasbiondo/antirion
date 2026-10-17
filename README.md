# Antirion

An open-source AI gateway: one binary in front of model providers, with virtual keys, routing,
limits, budgets, caching, guardrails and observability.

**To try it, start with [docs/getting-started.md](docs/getting-started.md)**: `docker compose up`,
the console at <http://localhost:8081/ui/> and a first request through a mock upstream, then
Kubernetes with Helm.

## Build

```bash
cargo build -p antirion                # target/debug/antirion
cargo build --release -p antirion      # target/release/antirion
```

The toolchain is pinned in `rust-toolchain.toml`. The binary embeds the config schema
(`schema/antirion-config.schema.json`) and the Admin API contract (`openapi/admin-api.yaml`).

## Run

```bash
antirion serve --config <file> [--test-mode] [--log-format text|json]
antirion validate -f <file>     # offline validation; prints every error with its path
antirion schema                 # the published config schema
antirion version [--json]
antirion healthcheck [--url http://127.0.0.1:8080]
```

`--config` takes a YAML or JSON document. The static keys (`server`, `admin`, `store`, `cluster`)
are read once at startup; the dynamic keys are applied as the source `admin.config.source`
(default `file`). Everything else is configured at run time through the Admin API on the admin
listener.

### A development node in test mode

`ci/standalone.yaml` starts a standalone node with the embedded store, the data port on
`127.0.0.1:18080` and the admin port on `127.0.0.1:18081`. It reads the admin bootstrap token from
an environment variable:

```bash
export ANTIRION_BOOTSTRAP_TOKEN=dev-bootstrap-token-0123456789abcdef0123
./target/debug/antirion serve --test-mode --config ci/standalone.yaml
```

Test mode adds the test clock, seed and reset endpoints under `/admin/v1/test/` and honours the
test-only request headers; it is reported on every data-plane response and by the probes. Never
run it in production. Without `auth.admin.bootstrap_token`, a standalone node writes a generated
token to `<store.embedded.path>/bootstrap-token`.

```bash
curl -s localhost:18080/-/readyz
curl -s -H "authorization: Bearer $ANTIRION_BOOTSTRAP_TOKEN" localhost:18081/admin/v1/status
```

## Checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask policy    # no open-work markers in the repository's text
cargo xtask layers    # crate dependencies only point to lower layers
cargo xtask lints     # crates inherit the workspace lints
```

`docs/development.md` describes the crates and the conventions of the code.

The console has its own checks (`pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm e2e`; see
`console/README.md`). `ci/advisory-ignores.sh` checks that every advisory ignore has an unexpired
date. The workflows in `.github/workflows/` call these scripts and the checks above, `cargo deny` and
`cargo audit`, and build the release binary with `cargo auditable`, its CycloneDX SBOM, signatures
and provenance. Release binaries are built on Debian 12 and need glibc 2.36 or later
(`ci/glibc-floor.sh` checks a binary against that floor).

## Container image

`docker/Dockerfile` builds the console and the binary into a distroless image that runs as UID
65532 with a read-only root filesystem; `/var/lib/antirion` is the only writable path, and the
health check is `antirion healthcheck`. `docker/test/smoke.sh <image>` checks those properties.
`compose.yaml` and `compose.cluster.yaml` run it locally, and `deploy/helm/antirion` is the Helm
chart (see `docs/getting-started.md`).

## Security

Report vulnerabilities privately as described in `SECURITY.md`.

## License

Apache-2.0. See `LICENSE`.
