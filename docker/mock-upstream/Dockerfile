# syntax=docker/dockerfile:1
#
# The mock upstream for trying the gateway without provider keys: the benchmark mock (bench/mock)
# on a distroless base. It answers OpenAI Chat Completions (also streamed) and embeddings and
# Anthropic Messages (also streamed) with fixed text and usage, after the delays of a latency
# profile: LS by default (200 ms to the first byte, then a 1 s stream). A path may name another
# profile first, as in /L1500/v1/chat/completions.
#
#   docker build -f docker/mock-upstream/Dockerfile -t antirion-mock-upstream .

ARG RUST_VERSION=1.98.1

FROM rust:${RUST_VERSION}-slim-bookworm AS build
ARG CARGO_BUILD_JOBS=4
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
WORKDIR /src
COPY bench/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=antirion-mock-upstream-target,target=/src/target-cache \
    CARGO_TARGET_DIR=/src/target-cache cargo build --release --locked -p antirion-bench-mock \
    && cp /src/target-cache/release/antirion-bench-mock /mock-upstream

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build --chmod=0555 /mock-upstream /mock-upstream
USER 65532:65532
EXPOSE 19401
ENTRYPOINT ["/mock-upstream"]
CMD ["--listen", "0.0.0.0:19401", "--profile", "LS"]
