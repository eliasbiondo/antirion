# Comparison targets

Other gateways run the `compare` suite through `--target external`: the harness sends the same
corpus, at the same rates, through the same generator, to `--target-url host:port` (one per node),
with `--target-key` as the key. The key goes in `--target-auth-header` (`authorization`, with the
Bearer scheme, by default; any other header carries the key alone). `--target-model` renames corpus
models where the target names them differently: `TO` for the chat model, `FROM=TO`, or
`FROM@PROFILE=TO` for one latency profile (a target that reaches each mock latency profile through
its own upstream). Memory and CPU are sampled from `--target-pid`, or from the processes whose
command line matches `--target-process` (and their children); `--target-agent host:port` samples
them on the target's machine through its agent. `--target-name` labels the result directory and
`--target-version` the manifest.

## Interleaved runs

`antirion-bench cell --cell-file <cells> --interleave <targets.yaml>` runs every cell for every
target of the file, interleaved per cell and repetition (each target in turn, on a fresh process),
so drift of the machine reaches every target alike. Each target's results go to `<out>/<name>/`, and
`<out>/comparison.md|html|json` puts them side by side (`antirion-bench report <out>` renders it
again). A targets file lists, in round order:

```yaml
- {name: antirion, kind: antirion}          # the harness's own gateway, from the topology
- name: other
  nodes: ["10.0.0.11:18080"]
  key: sk-…
  auth_header: authorization
  models: {bench-chat: provider/upstream, bench-chat@LS: provider-ls/upstream}
  statuses: {S8: 400}                       # the status the target rejects a scenario with
  command: [/opt/bench/other/start.sh]      # started for every repetition, under the topology's
  env: {MOCK: "10.0.0.12:19401"}            #   gateway priority and CPU set
  ready: /health
  agent: "10.0.0.11:19500"                  # start and sample it on its machine
  version: "1.2.3"
```

## Machines other than the generator's

`antirion-bench agent --listen 0.0.0.0:19500` on a gateway or mock machine lets the harness start,
sample and stop processes there; a topology names it per gateway node (`agent:`) and for the mock
(`mock.agent`). Both sides read the shared token from `ANTIRION_BENCH_AGENT_TOKEN`. Paths in the
topology and the targets file are paths on the machine that runs the process.

## Drivers

A driver for a gateway is a directory holding:

- `VERSION`: the release under test, the latest at the time of the run;
- the gateway's configuration for each profile, rendered by hand from `profiles/` (the neutral form):
  OpenAI and Anthropic upstreams at the mock (`http://<mock>/<latency profile>/v1` and
  `http://<mock>/<latency profile>`), the models `bench-chat`, `bench-messages` and `bench-embed`
  mapped to upstream model `bench-upstream`, the keys and limits of the profile, request logs to
  `http://<mock>/__bench/null/logs`;
- start and stop scripts, and tuning notes with links to the project's own performance guidance.

A feature the gateway lacks makes the cell `n/a`; the profile is never weakened. Cells that need
MCP, per-key model lists or a zero rate limit are skipped for external targets.
