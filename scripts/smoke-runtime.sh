#!/usr/bin/env bash
# Smoke-test the bundled runtime entry point.
#
# This is the only place CI executes the agent: does the entry start at all, and
# does it report the version the lock pins? Anything needing a model provider is
# out of scope by design.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SUBMODULE="$ROOT/runtime/upstream"
LOCK="$ROOT/runtime/upstream.lock.toml"

fail() { printf 'smoke-runtime: %s\n' "$1" >&2; exit 1; }

read_lock() {
    awk -v table="$1" -v key="$2" '
        /^[[:space:]]*\[/ { current = $0; next }
        {
            if (current != "[" table "]") next
            line = $0
            sub(/[[:space:]]*#.*$/, "", line)
            split(line, parts, "=")
            if (parts[1] ~ "^[[:space:]]*" key "[[:space:]]*$") {
                value = substr(line, index(line, "=") + 1)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
                gsub(/^"|"$/, "", value)
                print value
                exit
            }
        }
    ' "$LOCK"
}

ENTRY=$(read_lock packaging runtime_entry)
ENTRY=${ENTRY:-apps/cli/lib/bin.js}
VERSION=$(read_lock runtime version)

[[ -f "$SUBMODULE/$ENTRY" ]] \
    || fail "missing $ENTRY; run scripts/build-runtime.sh first"

reported=$(node "$SUBMODULE/$ENTRY" --version 2>&1 | tail -1)
[[ -n "$reported" ]] || fail "$ENTRY --version produced no output"

case "$reported" in
    *"$VERSION"*) ;;
    *) fail "runtime reports '$reported' but the lock pins $VERSION" ;;
esac

printf 'smoke-runtime: ok %s reports %s\n' "$ENTRY" "$VERSION"