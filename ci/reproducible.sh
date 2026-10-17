#!/usr/bin/env bash
# Builds one package twice, in two fresh copies of the committed source at different paths, with the
# release settings that make the output independent of where and when it is built, and fails unless
# the two outputs are identical byte for byte.
#
#   ci/reproducible.sh                                  # the gateway (antirion), release profile
#   ci/reproducible.sh --package xtask --bin xtask      # a small binary, for a quick local check
#   ci/reproducible.sh --package antirion-json --lib    # a library: its .rlib is compared
#
# Options: --package <name>, --bin <name> | --lib, --profile <name> (default release), --rev <git rev>
# (default HEAD), --features <list>, --auditable (build with `cargo auditable`, as the release does),
# --keep (leave the two build directories for inspection).
#
# Settings: the toolchain pinned by rust-toolchain.toml, `--locked`, SOURCE_DATE_EPOCH (the commit
# time), ANTIRION_BUILD_COMMIT (the commit), CARGO_INCREMENTAL=0, the source, Cargo home and rustup
# paths remapped out of the output (--remap-path-prefix, and -ffile-prefix-map for C code). A console
# export must already be in console/out (built with ANTIRION_BUILD_COMMIT set, so its build id is
# the commit) or the placeholder page is embedded.
set -euo pipefail

package=antirion
target_kind=bin
target_name=antirion
profile=release
rev=HEAD
features=""
keep=0
build_cmd=(build)
while [[ $# -gt 0 ]]; do
  case "$1" in
    --package) package="$2"; shift 2 ;;
    --bin) target_kind=bin; target_name="$2"; shift 2 ;;
    --lib) target_kind=lib; target_name=""; shift ;;
    --profile) profile="$2"; shift 2 ;;
    --rev) rev="$2"; shift 2 ;;
    --features) features="$2"; shift 2 ;;
    --keep) keep=1; shift ;;
    --auditable) build_cmd=(auditable build); shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

repo="$(git rev-parse --show-toplevel)"
commit="$(git -C "$repo" rev-parse "$rev")"
epoch="$(git -C "$repo" log -1 --format=%ct "$commit")"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
work="$(mktemp -d)"
cleanup() { if [[ "$keep" == 0 ]]; then rm -rf "$work"; else echo "kept $work"; fi; }
trap cleanup EXIT

# Two copies at paths of different lengths, so a leaked absolute path cannot cancel out.
copies=("$work/a" "$work/second-copy")
for dir in "${copies[@]}"; do
  mkdir -p "$dir"
  git -C "$repo" archive --format=tar "$commit" | tar -x -C "$dir"
  if [[ -f "$repo/console/out/index.html" ]]; then
    mkdir -p "$dir/console/out"
    cp -R "$repo/console/out/." "$dir/console/out/"
  fi
done

build() {
  local dir="$1"
  local rustflags="--remap-path-prefix=$dir=/antirion --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$rustup_home=/rustup"
  # Apple's linker records the absolute paths of the object files that hold the debug information.
  if [[ "$(uname -s)" == Darwin ]]; then rustflags+=" -C link-arg=-Wl,-oso_prefix,$dir/"; fi
  local args=("${build_cmd[@]}" --locked --profile "$profile" --package "$package")
  if [[ "$target_kind" == bin ]]; then args+=(--bin "$target_name"); else args+=(--lib); fi
  if [[ -n "$features" ]]; then args+=(--features "$features"); fi
  (
    cd "$dir"
    env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
      SOURCE_DATE_EPOCH="$epoch" \
      ANTIRION_BUILD_COMMIT="$commit" \
      CARGO_INCREMENTAL=0 \
      CARGO_TARGET_DIR="$dir/target" \
      CARGO_BUILD_RUSTFLAGS="$rustflags" \
      CFLAGS="-ffile-prefix-map=$dir=/antirion -ffile-prefix-map=$cargo_home=/cargo" \
      CXXFLAGS="-ffile-prefix-map=$dir=/antirion -ffile-prefix-map=$cargo_home=/cargo" \
      cargo "${args[@]}"
  )
}

out_dir() {
  local dir="$1"
  if [[ "$profile" == dev ]]; then echo "$dir/target/debug"; else echo "$dir/target/$profile"; fi
}

artifact() {
  local dir; dir="$(out_dir "$1")"
  if [[ "$target_kind" == bin ]]; then
    echo "$dir/$target_name"
  else
    ls "$dir"/lib"${package//-/_}".rlib
  fi
}

for dir in "${copies[@]}"; do
  echo "building $package ($profile) in $dir" >&2
  build "$dir"
done

first="$(artifact "${copies[0]}")"
second="$(artifact "${copies[1]}")"
sum() { shasum -a 256 "$1" | awk '{print $1}'; }
a="$(sum "$first")"
b="$(sum "$second")"
echo "$a  ${first#"$work"/}"
echo "$b  ${second#"$work"/}"
if [[ "$a" != "$b" ]]; then
  echo "the two builds differ" >&2
  exit 1
fi
echo "reproducible: $package at $commit"
