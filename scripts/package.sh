#!/usr/bin/env bash
# Assemble a distributable bundle in dist/.
#
# Layout mirrors what the app expects at runtime: the TUI binary sits next to a
# runtime/ directory holding the already-built official DSH Agent. Nothing in the
# bundle downloads, installs or upgrades anything at startup.
#
#   dist/lingxi-dsh-tui-$VERSION-$PLATFORM/
#     bin/dsh-tui
#     runtime/            (the official agent build, pinned by the lock file)
#     runtime.lock.toml   (which agent version this bundle carries)
#     VERSION             (product version, agent version, agent commit)
set -euo pipefail

VERSION=${1:-}
[[ -n "$VERSION" ]] || { printf 'usage: package.sh <version>\n' >&2; exit 2; }

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
LOCK="$ROOT/runtime/upstream.lock.toml"
SUBMODULE="$ROOT/runtime/upstream"
BINARY="$ROOT/target/release/dsh-tui"

fail() { printf 'package: %s\n' "$1" >&2; exit 1; }

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

[[ -x "$BINARY" ]] || fail "missing $BINARY; run cargo build --release -p dsh-tui first"
[[ -f "$SUBMODULE/$(read_lock packaging runtime_entry)" ]] || fail "runtime is not built; run scripts/build-runtime.sh first"
[[ "$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)" = "$VERSION" ]] \
    || fail "workspace version and requested $VERSION disagree"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) PLATFORM=darwin-arm64 ;;
    Darwin-x86_64) PLATFORM=darwin-x64 ;;
    Linux-x86_64) PLATFORM=linux-x64 ;;
    Linux-aarch64) PLATFORM=linux-arm64 ;;
    *) fail "unsupported platform $(uname -s)-$(uname -m)" ;;
esac

BUNDLE="$ROOT/dist/lingxi-dsh-tui-$VERSION-$PLATFORM"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/bin"

cp "$BINARY" "$BUNDLE/bin/dsh-tui"
cp "$LOCK" "$BUNDLE/runtime.lock.toml"

# The upstream build output is what makes the bundle self-contained. Node is the
# one host dependency and is documented in the README; nothing is fetched here.

for path in apps packages node_modules package.json pnpm-lock.yaml; do
    [[ -e "$SUBMODULE/$path" ]] || continue
    mkdir -p "$BUNDLE/runtime"
    cp -R "$SUBMODULE/$path" "$BUNDLE/runtime/"
done

{
    echo "product_version=$VERSION"
    echo "agent_version=$(read_lock runtime version)"
    echo "agent_tag=$(read_lock runtime tag)"
    echo "agent_commit=$(read_lock runtime commit)"
} > "$BUNDLE/VERSION"

printf 'package: ok %s\n' "${BUNDLE#"$ROOT"/}"