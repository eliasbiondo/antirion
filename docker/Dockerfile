# syntax=docker/dockerfile:1
#
# The official image: the antirion binary on a distroless base, running as UID/GID 65532 with no
# shell and no package manager. It works with a read-only root filesystem: the only writable path
# is the data directory, /var/lib/antirion (the embedded store, its spool and generated keys).
#
#   docker build -f docker/Dockerfile .
#       builds the console's static export (console/, pnpm with the lockfile) and compiles the
#       binary with it embedded (cargo auditable, --locked)
#   docker build -f docker/Dockerfile --build-arg BINARY=prebuilt .
#       packages dist/<arch>/antirion, the binary CI built and tested, for each target platform;
#       it must need no newer glibc than the base's (Debian 12, 2.36), so CI builds it in
#       rust:<version>-bookworm and checks it with ci/glibc-floor.sh
#
# Build arguments for builders with little memory:
#   CARGO_BUILD_JOBS (default 4) caps the compiler's parallel jobs; each job needs memory.
#   CARGO_PROFILE_RELEASE_LTO (default fat) and CARGO_PROFILE_RELEASE_CODEGEN_UNITS (default 1):
#       the release profile's fat LTO needs several GiB for the final link. thin and 16 build with
#       far less, for a somewhat slower binary; compose.yaml builds that way.
#
# Smoke test: docker/test/smoke.sh <image>.

ARG RUST_VERSION=1.98.1
ARG NODE_VERSION=24.15.0
ARG BINARY=build

FROM node:${NODE_VERSION}-bookworm-slim AS console
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0 NEXT_TELEMETRY_DISABLED=1 PNPM_HOME=/pnpm
WORKDIR /src/console
COPY console/package.json console/pnpm-lock.yaml console/pnpm-workspace.yaml ./
RUN corepack enable
RUN --mount=type=cache,target=/pnpm/store \
    pnpm config set store-dir /pnpm/store && pnpm install --frozen-lockfile
COPY console/ ./
ARG ANTIRION_BUILD_COMMIT=""
# An empty commit would name an empty build id: the export then picks a fresh one.
RUN if [ -z "$ANTIRION_BUILD_COMMIT" ]; then unset ANTIRION_BUILD_COMMIT; fi && pnpm build

FROM rust:${RUST_VERSION}-slim-bookworm AS build
ARG CARGO_BUILD_JOBS=4
ARG CARGO_PROFILE_RELEASE_LTO=fat
ARG CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO} \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=${CARGO_PROFILE_RELEASE_CODEGEN_UNITS}
RUN apt-get update && apt-get install -y --no-install-recommends make && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-auditable --locked
WORKDIR /src
COPY . .
COPY --from=console /src/console/out /console
ARG ANTIRION_BUILD_COMMIT=""
ENV ANTIRION_BUILD_COMMIT=${ANTIRION_BUILD_COMMIT} ANTIRION_CONSOLE_DIR=/console
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target-cache \
    CARGO_TARGET_DIR=/src/target-cache cargo auditable build --release --locked -p antirion \
    && cp /src/target-cache/release/antirion /antirion

FROM scratch AS binary-build
COPY --from=build /antirion /antirion

FROM scratch AS binary-prebuilt
ARG TARGETARCH
COPY dist/${TARGETARCH}/antirion /antirion

FROM binary-${BINARY} AS binary

# COPY --chmod would also apply the file's mode to the directories it creates, leaving
# /etc/antirion untraversable, so the config's modes are set here.
FROM debian:bookworm-slim AS layout
COPY docker/antirion.yaml /out/etc/antirion/antirion.yaml
RUN mkdir -p /out/var/lib/antirion && chown -R 65532:65532 /out/var/lib/antirion \
    && chmod 0755 /out/etc/antirion && chmod 0444 /out/etc/antirion/antirion.yaml

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=binary --chmod=0555 /antirion /antirion
COPY --from=layout --chown=65532:65532 /out/var/lib/antirion /var/lib/antirion
COPY --from=layout /out/etc/antirion /etc/antirion
USER 65532:65532
WORKDIR /var/lib/antirion
VOLUME ["/var/lib/antirion"]
EXPOSE 8080 8081
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["/antirion", "healthcheck"]
ENTRYPOINT ["/antirion"]
CMD ["serve", "--config", "/etc/antirion/antirion.yaml"]
