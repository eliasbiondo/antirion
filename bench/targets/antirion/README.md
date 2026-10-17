# Antirion

The harness starts and stops Antirion itself. On a `launch: local` topology it renders each cell's
profile into a startup file per node (kept under `configs/` of the result directory), passes the
keys through the environment, starts `<repository>/<gateway.bin>` (`target/release/antirion` by
default, or `--gateway-bin`) under the topology's priority and CPU set, and waits for `/-/readyz`.
On a `launch: external` topology the nodes run already (the container image built from `docker/`,
with host networking); the harness applies each profile through `POST /admin/v1/config:apply`
with the token in `ANTIRION_BENCH_ADMIN_TOKEN`, so the nodes must have been started with the
profile's key environment. P-scale's keys include every other profile's, so one environment serves
the whole suite: `antirion-bench env --profile P-scale --mock <mock host:port> --out keys.env`.

The version under test is the binary's own (`antirion --version`) and the repository's commit, both
recorded in `run.json`. Build it with `cargo build --release --locked -p antirion`; no tuning beyond
the profile is applied.
