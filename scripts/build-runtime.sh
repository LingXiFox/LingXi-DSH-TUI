#!/usr/bin/env bash
# Build the bundled DSH runtime from the pinned submodule.
#
# The packaging flow is intentionally the upstream one: install with the exact
# package manager version from the lock file, then run upstream's own build.
# Nothing here patches or rewrites upstream sources; if a patch were ever needed
# it lives in patches/ and would be applied explicitly before this script.
#
# The client never calls this at startup. It runs during CI and packaging only,
# so a user can never be upgraded to a runtime they did not install.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SUBMODULE="$ROOT/runtime/upstream"
LOCK="$ROOT/runtime/upstream.lock.toml"

fail() { printf 'build-runtime: %s\n' "$1" >&2; exit 1; }

[[ -d "$SUBMODULE" ]] || fail "submodule missing; run git submodule update --init --depth 1"

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

PACKAGE_MANAGER=$(read_lock "runtime.build" package_manager)
ENTRY=$(read_lock packaging runtime_entry)
COMMIT=$(read_lock runtime commit)
PACKAGE_MANAGER=${PACKAGE_MANAGER:-pnpm@11.28.5}
ENTRY=${ENTRY:-apps/cli/lib/bin.js}

# Verify the checkout before spending minutes on a build.
"$ROOT/scripts/check-runtime-pin.sh"

# Resolve the declared package manager without guessing. corepack honours the
# packageManager field exactly; an already-installed pnpm self-manages its version
# from that same field, so a newer launcher still runs the locked release.
PM_BIN=""
if command -v corepack >/dev/null 2>&1; then
    corepack enable >/dev/null 2>&1 || true
    PM_BIN=$PACKAGE_MANAGER
else
    name=${PACKAGE_MANAGER%@*}
    command -v "$name" >/dev/null 2>&1 \
        || fail "needs corepack or $PACKAGE_MANAGER on PATH; install Node.js >= 24.9.0"
    PM_BIN=$name
fi

cd "$SUBMODULE"

# Upstream embeds the source commit in build metadata. Deriving it from the lock
# keeps the build reproducible even when the checkout carries no git metadata.
export DSH_CLIENT_COMMIT_HASH="$COMMIT"

# CI marks the build so upstream's git-hook installer stands down. Inside a
# submodule that installer refuses to run (the submodule config carries
# core.worktree), and this build has no use for commit hooks anyway.
export CI=true

printf 'build-runtime: installing with %s\n' "$PACKAGE_MANAGER"
"$PM_BIN" install --frozen-lockfile

printf 'build-runtime: building upstream runtime\n'
"$PM_BIN" run build

[[ -f "$SUBMODULE/$ENTRY" ]] \
    || fail "expected runtime entry $ENTRY after build; update [packaging] runtime_entry in the lock file"

printf 'build-runtime: ok %s\n' "$ENTRY"