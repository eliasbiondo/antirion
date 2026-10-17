#!/usr/bin/env bash
# Every advisory ignore in deny.toml and .cargo/audit.toml must carry a justification and an expiry
# date, written in its reason as "expires YYYY-MM-DD"; this fails once a date has passed, so an
# ignore is reviewed again instead of living forever.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
today=$(date -u +%Y-%m-%d)
status=0

check() {
  local file=$1
  [ -f "$file" ] || return 0
  while IFS= read -r line; do
    id=$(printf '%s' "$line" | grep -oE 'RUSTSEC-[0-9]{4}-[0-9]{4}' | head -1 || true)
    [ -n "$id" ] || continue
    expiry=$(printf '%s' "$line" | grep -oE 'expires [0-9]{4}-[0-9]{2}-[0-9]{2}' | awk '{print $2}' | head -1 || true)
    if [ -z "$expiry" ]; then
      echo "$file: $id is ignored without an expiry date (\"expires YYYY-MM-DD\" in its reason)" >&2
      status=1
    elif [[ "$expiry" < "$today" ]]; then
      echo "$file: the ignore of $id expired on $expiry" >&2
      status=1
    fi
  done < "$file"
}

check "$root/deny.toml"
check "$root/.cargo/audit.toml"
exit $status
