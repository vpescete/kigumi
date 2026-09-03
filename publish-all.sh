#!/bin/bash
# Publishes every Kigumi crate to crates.io in dependency order. Resumable AND bump-aware: a crate is
# skipped only when the version in its own manifest is already on crates.io, so re-running after a
# version bump publishes exactly the crates that moved. Rate limits (HTTP 429 on new-crate bursts) are
# handled by sleeping and retrying the same crate.
set -u
cd "$(dirname "$0")"

UA='kigumi-publish (teamdev1@meshble.com)'

ORDER=(
  kigumi-core kigumi-macros kigumi-config kigumi-storage kigumi-schema kigumi-auth
  kigumi-db kigumi-test kigumi kigumi-server
  kigumi-mod-base kigumi-mod-mail kigumi-mod-account kigumi-mod-sales kigumi-mod-stock
  kigumi-runtime kigumi-mcp kigumi-cli
)

# Core crates inherit `version.workspace = true`; modules and the CLI carry their own version.
ws_version=$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version[ ]*=[ ]*"/{print;exit}' Cargo.toml | cut -d'"' -f2)

# First `name = "…"` in a manifest is the [package] name — [[bin]] names come later, so `kigumi`
# resolves to its own crate and not to the CLI's binary of the same name.
manifest_of() {
  local m
  for m in crates/*/Cargo.toml modules/*/Cargo.toml apps/*/Cargo.toml; do
    [ "$(grep -m1 '^name[ ]*=[ ]*"' "$m" | cut -d'"' -f2)" = "$1" ] && { echo "$m"; return; }
  done
}

version_of() {
  local v
  v=$(awk '/^\[package\]/{f=1;next} /^\[/{f=0} f && /^version[ ]*=[ ]*"/{print;exit}' "$1" | cut -d'"' -f2)
  echo "${v:-$ws_version}"
}

PUBLISHED=()
for crate in "${ORDER[@]}"; do
  manifest=$(manifest_of "$crate")
  if [ -z "$manifest" ]; then
    echo "== FATAL: no manifest found for $crate"
    exit 1
  fi
  version=$(version_of "$manifest")
  # crates.io throttles this endpoint mid-run (a dozen publishes in a row will do it) and an
  # unbounded curl then HANGS rather than erroring, so: bounded, and retried before giving up.
  check=0
  while true; do
    check=$((check + 1))
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 -A "$UA" "https://crates.io/api/v1/crates/$crate/$version")
    [ "$code" = "200" ] || [ "$code" = "404" ] && break
    if [ $check -ge 5 ]; then
      # Anything but a clean 200/404 is not evidence either way: publishing on a guess would abort
      # the run on the "already uploaded" error a few seconds later.
      echo "== FATAL: crates.io answered $code for $crate/$version after $check tries — not publishing on a guess"
      exit 1
    fi
    echo "== crates.io answered $code for $crate/$version; retrying the check in 20s ($check/5)"
    sleep 20
  done
  if [ "$code" = "200" ]; then
    echo "== $crate $version: already on crates.io, skipping"
    continue
  fi
  attempt=0
  while true; do
    attempt=$((attempt + 1))
    echo "== publishing $crate $version (attempt $attempt)"
    out=$(cargo publish -p "$crate" 2>&1)
    status=$?
    echo "$out" | tail -4
    if [ $status -eq 0 ]; then
      PUBLISHED+=("$crate@$version")
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

if [ ${#PUBLISHED[@]} -eq 0 ]; then
  echo "== nothing to publish: every manifest version is already on crates.io"
else
  echo "== PUBLISHED: ${PUBLISHED[*]}"
  echo "== now tag it: git tag -a v$ws_version -m 'framework $ws_version' && git push --tags"
fi
