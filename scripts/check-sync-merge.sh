#!/usr/bin/env bash
# Read-only final gate for the exact upstream-sync commit.
set -euo pipefail

PR=${1:?usage: check-sync-merge.sh <pr> <expected-head>}
EXPECTED_HEAD=${2:?usage: check-sync-merge.sh <pr> <expected-head>}
REPO=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}

pr=$(gh pr view "$PR" --repo "$REPO" \
    --json state,isDraft,baseRefName,headRefOid,mergeable,mergeStateStatus,labels,reviewDecision)
if ! jq -e --arg head "$EXPECTED_HEAD" '
    .state == "OPEN" and .isDraft == false and .baseRefName == "main"
    and .headRefOid == $head and .mergeable == "MERGEABLE"
    and .mergeStateStatus == "CLEAN" and .reviewDecision != "CHANGES_REQUESTED"
    and all(.labels[]; .name | test("^(runtime-sync/(blocked|failed)|no-auto-merge|do-not-merge|breaking-change|major-risk|hold|blocked)$"; "i") | not)
' <<< "$pr" >/dev/null; then
    echo "check-sync-merge: PR head, merge state, reviews or blocking labels require human review" >&2
    exit 1
fi

checks=$(gh pr checks "$PR" --repo "$REPO" --json name,workflow,bucket)
if ! jq -e '
    length > 0 and all(.[]; .bucket == "pass")
    and ([
        "cargo fmt", "clippy (ubuntu-latest)", "clippy (macos-latest)",
        "clippy (windows-latest)", "test (ubuntu-latest)", "test (macos-latest)",
        "test (windows-latest)", "release build", "runtime pin", "scripts"
    ] - [.[] | select(.workflow == "CI") | .name] | length == 0)
    and any(.[]; .workflow == "Runtime check" and .name == "build bundled runtime")
' <<< "$checks" >/dev/null; then
    echo "check-sync-merge: all CI, runtime and additional checks must be present and passing" >&2
    exit 1
fi

echo "check-sync-merge: verified $EXPECTED_HEAD is ready"
