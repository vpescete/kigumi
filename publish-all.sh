#!/bin/bash
# Publishes every Kigumi crate to crates.io in dependency order. Resumable: crates already
# present (any version) are skipped. Rate limits (HTTP 429 on new-crate bursts) are handled by
# sleeping and retrying the same crate. Not committed — a one-shot operational script.
set -u
cd "$(dirname "$0")"

ORDER=(
  kigumi-core kigumi-macros kigumi-config kigumi-storage kigumi-schema kigumi-auth
  kigumi-db kigumi-test kigumi kigumi-server
  kigumi-mod-base kigumi-mod-mail kigumi-mod-account kigumi-mod-sales kigumi-mod-stock
  kigumi-runtime kigumi-mcp kigumi-cli
)

for crate in "${ORDER[@]}"; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -A 'kigumi-publish (teamdev1@meshble.com)' "https://crates.io/api/v1/crates/$crate")
  if [ "$code" = "200" ]; then
    echo "== $crate: already on crates.io, skipping"
    continue
  fi
  attempt=0
  while true; do
    attempt=$((attempt + 1))
    echo "== publishing $crate (attempt $attempt)"
    out=$(cargo publish -p "$crate" 2>&1)
    status=$?
    echo "$out" | tail -4
    if [ $status -eq 0 ]; then
      break
    fi
    if echo "$out" | grep -qi 'too many\|429\|rate limit'; then
      echo "== rate limited; sleeping 620s before retrying $crate"
      sleep 620
      continue
    fi
    echo "== FATAL: $crate failed for a non-rate-limit reason"
    exit 1
  done
  sleep 5
done
echo "== ALL PUBLISHED"
