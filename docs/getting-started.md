# Getting started

Run Antirion on your machine, open the console, and send a first request through it. Docker
Compose is the quickest way; Kubernetes with Helm and running from source follow.

## Docker Compose

You need Docker with Compose v2.24 or later and a clone of the repository. From the repository
root:

```bash
cp .env.example .env
# put a bootstrap token and an encryption key in .env:
sed -i.bak "s|^ANTIRION_BOOTSTRAP_TOKEN=.*|ANTIRION_BOOTSTRAP_TOKEN=$(openssl rand -hex 24)|; s|^ANTIRION_ENCRYPTION_KEY=.*|ANTIRION_ENCRYPTION_KEY=$(openssl rand -base64 32)|" .env && rm .env.bak
docker compose --profile mock up -d    # the first run builds the images: 10 to 20 minutes
```

This starts one gateway (`compose.yaml`) with its data in the volume `antirion_data`, and the mock
upstream (the `mock` profile; leave `--profile mock` out if you only use real providers). Both
ports are published on 127.0.0.1 only:

| Port | What                                                     |
| ---- | -------------------------------------------------------- |
| 8080 | the data plane: the `/v1` API your apps call             |
| 8081 | the admin plane: the Admin API and the console at `/ui/` |

Open <http://localhost:8081/ui/> and paste `ANTIRION_BOOTSTRAP_TOKEN` from `.env` into **Admin
token**. A new gateway asks for it straight away ("Set up your gateway"); later, the sign-in page
has it under **Use an admin token**.

Ports already taken on your machine? Set `ANTIRION_DATA_PORT` and `ANTIRION_ADMIN_PORT` in `.env`
(see [Ports, host names and the public origin](#ports-host-names-and-the-public-origin)).

Both keys are optional. Without a bootstrap token the gateway generates one into its volume:

```bash
docker compose cp gateway:/var/lib/antirion/data/bootstrap-token - | tar -xO
```

Without an encryption key it generates that too. Keep whichever key seals your stored secrets:
without it, provider keys saved in the gateway cannot be read.

`docker compose ps` shows the gateway as `healthy` once it is ready, and `docker compose logs -f
gateway` follows its log.

## Your first request

The mock upstream answers like OpenAI and Anthropic, with fixed text, token counts and a little
latency, and costs nothing. Use a real provider instead by choosing it and pasting its API key.

1. **Add a provider.** Providers → **Add provider**. Provider: **OpenAI-compatible**; Name:
   `mock`; Sign in with: **No authentication**; Base URL: `http://mock-upstream:19401/v1` (the
   gateway reaches the mock by its Compose service name). **Add provider**.
2. **Add a Model.** Models → **Add model**. Provider: `mock`; Provider’s model ID: `mock-chat`;
   Name: `chat` (apps send this name); prices, say `0.15` and `0.60`, so the mock's requests
   cost something (they are fake: leave them blank for a real provider's list prices). **Add
   model**.
3. **Create an API key.** API keys → **Create key**. Name: `my-app`; the project `default` is
   already chosen. **Create key**, then **Copy**: the key is shown once.
4. **Send a request** with the key:

   ```bash
   export ANTIRION_API_KEY=sk-antirion-…   # the key you copied
   curl -s http://localhost:8080/v1/chat/completions \
     -H "Authorization: Bearer $ANTIRION_API_KEY" -H 'Content-Type: application/json' \
     -d '{"model": "chat", "messages": [{"role": "user", "content": "Hello"}]}'
   ```

   The mock answers after about 200 ms with filler text and 256 tokens each way. Add
   `"stream": true` for a one-second streamed answer.
5. **See it in the Request log.** Request log lists the request with its status, Model, key,
   tokens, latency and cost; select it for the details, from the route taken to the cost ledger.

The Playground sends requests from the console. The mock also speaks Anthropic Messages
(`/v1/messages`, also streamed) and embeddings: add an **Anthropic-compatible** provider with the
base URL `http://mock-upstream:19401` and any API key; any Model on `mock` answers `/v1/embeddings`.
A path prefix picks another latency profile, as in `http://mock-upstream:19401/L1500/v1` (1.5 s
before every answer).

## Stop, reset and upgrade

```bash
docker compose --profile mock stop      # stop; docker compose --profile mock start resumes
docker compose --profile mock down      # remove the containers, keep the data
docker compose --profile mock down -v   # remove the containers and the data volume: a fresh gateway
git pull && docker compose --profile mock build && docker compose --profile mock up -d   # upgrade, keeping the data
```

Leave out `--profile mock` if you never started the mock upstream.

## Ports, host names and the public origin

The console signs you in with a cookie, and the gateway only accepts changes made with that cookie
from its **public origin** (`auth.admin.public_origin`): the scheme, host and port your browser
shows. Compose sets it to `http://localhost:<ANTIRION_ADMIN_PORT>`, so open the console at
`localhost`, not `127.0.0.1`.

- **Other ports**: set `ANTIRION_DATA_PORT` and `ANTIRION_ADMIN_PORT` in `.env`; the public
  origin follows the admin port.
- **Another host name** (`127.0.0.1`, a LAN name): set `ANTIRION_PUBLIC_ORIGIN` to exactly what
  the browser shows, such as `http://127.0.0.1:8081`.
- **Another machine**: the session cookie is `Secure`, which browsers accept over plain HTTP only
  from the machine itself (`localhost`, `127.0.0.1`). Put a TLS proxy in front, set
  `ANTIRION_PUBLIC_ORIGIN=https://<its name>`, and publish the ports beyond 127.0.0.1 by editing
  `ports:` in `compose.yaml`.

Then `docker compose up -d` again to apply. After editing the startup file in `compose.yaml`
itself (its `configs:` section), add `--force-recreate`: Compose does not notice that change.

## Test mode

Test mode adds the test clock, seed and reset endpoints under `/admin/v1/test/` and honours the
test-only request headers. It is for trying things out only. Turn it on with
`ANTIRION_SERVE_FLAGS=--test-mode` in `.env` and `docker compose up -d`; remove the line to turn it
off.

## A cluster with Compose

`compose.cluster.yaml` runs two gateways on PostgreSQL and Valkey behind a proxy on the same
ports. It needs both keys in `.env`, a bootstrap token of at least 32 characters:

```bash
docker compose --profile mock down                        # the standalone gateway uses the same ports
docker compose -f compose.cluster.yaml --profile mock up -d
docker compose -f compose.cluster.yaml down -v            # remove it with its data
```

## Kubernetes with Helm

The chart is `deploy/helm/antirion`. Build the image (`docker compose build gateway` makes
`antirion:local`) and make it available to the cluster: push it to a registry, or on a local
[kind](https://kind.sigs.k8s.io) cluster:

```bash
kind create cluster --name antirion
kind load docker-image antirion:local --name antirion
helm install antirion deploy/helm/antirion \
  --set image.repository=antirion --set image.tag=local \
  --set console.publicOrigin=http://localhost:8081
kubectl get secret antirion-keys -o jsonpath='{.data.bootstrap-token}' | base64 -d; echo   # the token
kubectl port-forward svc/antirion 8080 8081
```

Then open <http://localhost:8081/ui/>. The chart generates the bootstrap token and the encryption
key into the Secret `<release>-keys` once and keeps them across upgrades and uninstalls; name your
own Secret with `secrets.existingSecret`.

**Standalone** (the default) runs one replica with the embedded store on a persistent volume:

```yaml
mode: standalone
persistence:
  size: 8Gi
console:
  publicOrigin: https://antirion.example.com   # what browsers open the console at
  dataPlaneUrl: https://api.antirion.example.com
```

**Cluster** runs several replicas on PostgreSQL and Valkey. `--set mode=cluster` alone bundles
both, for trying it out; for real use, bring your own:

```yaml
mode: cluster
replicaCount: 3
postgresql:
  bundled:
    enabled: false
  external:
    existingSecret: antirion-postgres   # key "url": postgres://user:password@host:5432/antirion
valkey:
  bundled:
    enabled: false
  external:
    existingSecret: antirion-valkey     # key "url": redis://host:6379/0
console:
  publicOrigin: https://antirion.example.com
```

`config:` takes any other configuration, merged over what the chart renders, and `extraArgs:
[--test-mode]` turns test mode on. `values.yaml` documents every value. `kind delete cluster
--name antirion` removes the local cluster.

## From source

For working on Antirion itself: the toolchain is pinned in `rust-toolchain.toml`, the console needs
Node 22 or later and pnpm.

```bash
(cd console && pnpm install --frozen-lockfile && pnpm build)   # optional: without it the console is a placeholder page
cargo build -p antirion
export ANTIRION_BOOTSTRAP_TOKEN=dev-bootstrap-token-0123456789abcdef0123
./target/debug/antirion serve --config ci/standalone.yaml     # data on :18080, admin on :18081
```

The console is then at <http://localhost:18081/ui/>. For console work, `pnpm dev` in `console/`
serves it on <http://localhost:3000/ui/> with live reload against a gateway's Admin API (see
`console/README.md`). The mock upstream runs from source with
`cargo run --manifest-path bench/Cargo.toml -p antirion-bench-mock -- --profile LS`; a provider
then reaches it at `http://127.0.0.1:19401/v1`. Delete `dev-data` for a fresh node.
`docs/development.md` covers the code and the checks.
