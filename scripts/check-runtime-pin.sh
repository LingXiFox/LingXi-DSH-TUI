#!/usr/bin/env bash
# Verify the runtime pin from a clean shell, without needing cargo.
#
# Used by ci.yml, runtime-check.yml and the sync workflow before they trust the
# lock file. Fails loudly instead of guessing: every mismatch prints the exact
# command that fixes it.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
LOCK="$ROOT/runtime/upstream.lock.toml"
SUBMODULE="$ROOT/runtime/upstream"

fail() { printf 'check-runtime-pin: %s\n' "$1" >&2; exit 1; }

[[ -f "$LOCK" ]] || fail "missing $LOCK"

read_lock() {
    # Reads a single key from a single table of the lock file. The file is small
    # and written by our own tooling, so a targeted awk pass is enough and avoids
    # requiring a TOML parser in pre-install CI.
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

COMMIT=$(read_lock runtime commit)
TAG=$(read_lock runtime tag)
VERSION=$(read_lock runtime version)

[[ -n "$COMMIT" ]] || fail "runtime.commit missing from $LOCK"
[[ -n "$TAG" ]] || fail "runtime.tag missing from $LOCK"
[[ "$TAG" == "dsh-v$VERSION" ]] || fail "tag $TAG contradicts version $VERSION"

if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
    fail "runtime.commit $COMMIT is not a full lowercase 40-character SHA"
fi

# Workspace version must mirror the bundled runtime exactly.
workspace_version=$(sed -n '/^\[workspace.package\]/,/^\[/p' "$ROOT/Cargo.toml" \
    | sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' | head -n 1)
[[ -n "$workspace_version" ]] || fail "cannot read [workspace.package] version from Cargo.toml"
[[ "$workspace_version" == "$VERSION" ]] \
    || fail "workspace version $workspace_version does not mirror bundled runtime $VERSION (tag $TAG)"

if [[ -e "$SUBMODULE/.git" ]]; then
    head=$(git -C "$SUBMODULE" rev-parse HEAD 2>/dev/null \
        || fail "cannot read the submodule HEAD; re-run git submodule update --init")
    [[ "$head" == "$COMMIT" ]] \
        || fail "submodule HEAD $head does not match lock commit $COMMIT (pin and checkout drifted)"
else
    printf 'check-runtime-pin: %s\n' \
        "submodule not checked out; skipping commit comparison (run git submodule update --init --depth 1)" >&2
fi

printf 'check-runtime-pin: ok %s at %s (workspace version %s)\n' "$TAG" "$COMMIT" "$VERSION"
