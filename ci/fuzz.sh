#!/usr/bin/env bash
# Runs every fuzz target of fuzz/ for the given number of seconds (default 60), from its seeds and the
# corpus kept between runs; a crash fails the script and leaves its input in fuzz/artifacts/.
# Needs a nightly toolchain and cargo-fuzz.
set -euo pipefail
seconds="${1:-60}"
cd "$(dirname "$0")/../fuzz"
for target in $(cargo +nightly fuzz list); do
  mkdir -p "corpus/$target"
  seeds=()
  if [ -d "seeds/$target" ]; then seeds=("seeds/$target"); fi
  echo "fuzzing $target for ${seconds}s"
  cargo +nightly fuzz run "$target" "corpus/$target" "${seeds[@]}" -- \
    -max_total_time="$seconds" -rss_limit_mb=2048 -timeout=10
done
