#!/usr/bin/env bash
# Exercise the production merge gate with local gh responses and no network.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/sync-merge.XXXXXX")
trap 'find "$WORK" -depth -delete' EXIT
mkdir "$WORK/bin"
cat > "$WORK/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
    'pr view') cat "$MERGE_TEST_PR" ;;
    'pr checks') cat "$MERGE_TEST_CHECKS" ;;
    *) echo "unexpected gh operation" >&2; exit 99 ;;
esac
MOCK
chmod +x "$WORK/bin/gh"
export PATH="$WORK/bin:$PATH"
export GITHUB_REPOSITORY=example/test
export MERGE_TEST_PR="$WORK/pr.json"
export MERGE_TEST_CHECKS="$WORK/checks.json"
cat > "$WORK/base-pr.json" <<'JSON'
{"state":"OPEN","isDraft":false,"baseRefName":"main","headRefOid":"verified-head","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","labels":[],"reviewDecision":"APPROVED"}
JSON
jq -n '[
    "cargo fmt", "clippy (ubuntu-latest)", "clippy (macos-latest)",
    "clippy (windows-latest)", "test (ubuntu-latest)", "test (macos-latest)",
    "test (windows-latest)", "release build", "runtime pin", "scripts"
] | map({name: ., workflow: "CI", bucket: "pass"})
  + [{name: "build bundled runtime", workflow: "Runtime check", bucket: "pass"}]' > "$WORK/base-checks.json"

seed() {
    cp "$WORK/base-pr.json" "$MERGE_TEST_PR"
    cp "$WORK/base-checks.json" "$MERGE_TEST_CHECKS"
}

reject() {
    if "$ROOT/scripts/check-sync-merge.sh" 1 verified-head > "$WORK/output" 2>&1; then
        echo "test-sync-merge: accepted $1" >&2
        exit 1
    fi
}

seed
"$ROOT/scripts/check-sync-merge.sh" 1 verified-head
for change in '.headRefOid = "new-head"' '.mergeable = "CONFLICTING"' \
    '.mergeStateStatus = "BEHIND"' '.isDraft = true' '.state = "CLOSED"' \
    '.baseRefName = "other"' '.reviewDecision = "CHANGES_REQUESTED"'; do
    seed
    jq "$change" "$WORK/base-pr.json" > "$MERGE_TEST_PR"
    reject "$change"
done
for label in runtime-sync/blocked runtime-sync/failed no-auto-merge do-not-merge breaking-change major-risk hold blocked; do
    seed
    jq --arg label "$label" '.labels = [{name: $label}]' "$WORK/base-pr.json" > "$MERGE_TEST_PR"
    reject "$label"
done
for bucket in pending fail skipping cancel; do
    seed
    jq --arg bucket "$bucket" '.[0].bucket = $bucket' "$WORK/base-checks.json" > "$MERGE_TEST_CHECKS"
    reject "$bucket check"
done
for change in '[]' '.[1:]' 'map(select(.workflow != "Runtime check"))' \
    'map(.workflow = "Other")' '. + [{name: "external", workflow: "Other", bucket: "fail"}]'; do
    seed
    jq "$change" "$WORK/base-checks.json" > "$MERGE_TEST_CHECKS"
    reject "$change"
done
echo "test-sync-merge: ok baseline accepted and 24 unsafe states rejected"
