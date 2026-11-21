#!/usr/bin/env bash
# Exercise the negative paths of scripts/check-runtime-pin.sh.
#
# A guard that never fails is indistinguishable from no guard, so CI proves the
# two realistic breakages (version drift, bad commit) really are rejected. Every
# mutation happens in a throwaway copy under $TMPDIR.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pin-guards.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fail() { printf 'test-pin-guards: %s\n' "$1" >&2; exit 1; }

seed() {
    local dir="$1"
    mkdir -p "$dir/runtime"
    cp "$ROOT/runtime/upstream.lock.toml" "$dir/runtime/"
    mkdir -p "$dir/scripts"
    cp "$ROOT"/scripts/*.sh "$dir/scripts/"
    cp "$ROOT/Cargo.toml" "$dir/"
}

expect_failure() {
    local dir="$1"
    if "$dir/scripts/check-runtime-pin.sh" >"$WORK/out" 2>"$WORK/err"; then
        fail "guard accepted a broken tree: $(cat "$WORK/err")"
    fi
    cat "$WORK/err"
}

# Baseline: the real tree must pass, otherwise the tests below prove nothing.
"$ROOT/scripts/check-runtime-pin.sh" >/dev/null || fail "baseline pin check failed"

# 1. Workspace version drifts away from the bundled runtime.
seed "$WORK/drift"
sed -i.bak 's/^version = "0\.2\.2-rc\.5"$/version = "0.2.2-rc.4"/' "$WORK/drift/Cargo.toml"
expect_failure "$WORK/drift" | grep -q 'does not mirror' \
    || fail "version drift was rejected for the wrong reason"

# 2. A truncated or uppercase SHA is not a commit.
seed "$WORK/sha"
sed -i.bak 's/^commit = .*/commit = "9C2670D"/' "$WORK/sha/runtime/upstream.lock.toml"
expect_failure "$WORK/sha" | grep -q 'not a full lowercase 40-character SHA' \
    || fail "bad commit was rejected for the wrong reason"

# 3. A tag that contradicts the version is refused before any comparison.
seed "$WORK/tag"
sed -i.bak 's/^tag = .*/tag = "dsh-v0.2.1"/' "$WORK/tag/runtime/upstream.lock.toml"
expect_failure "$WORK/tag" | grep -q 'contradicts version' \
    || fail "tag/version drift was rejected for the wrong reason"

printf 'test-pin-guards: ok 3 guards rejected the mutations they exist for\n'
