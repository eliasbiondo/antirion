#!/usr/bin/env bash
# Fails when a Linux binary needs a newer glibc than the runtime image's base provides, the floor
# that release binaries and archives promise:
#
#   ci/glibc-floor.sh <binary> [floor]        e.g. ci/glibc-floor.sh target/release/antirion 2.36
#
# The floor defaults to the glibc of Debian 12, the base of docker/Dockerfile's runtime and build
# stages. Release binaries are built in a container of that release so they always meet it.
set -euo pipefail

bin=${1:?usage: ci/glibc-floor.sh <binary> [floor]}
floor=${2:-2.36}

needed=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV | tail -n 1)
if [ -z "$needed" ]; then
  echo "$bin needs no versioned glibc symbol"
  exit 0
fi
highest=$(printf '%s\n%s\n' "$needed" "$floor" | sort -V | tail -n 1)
if [ "$highest" != "$floor" ]; then
  echo "$bin needs glibc $needed, newer than the floor $floor" >&2
  exit 1
fi
echo "$bin needs glibc $needed (floor $floor)"
