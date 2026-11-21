# AGENTS.md

Engineering contract for this repository. Read it before changing anything; a
change that breaks a rule here is a bug even if it compiles.

## What this project is

`LingXi-DSH-TUI` is a native terminal client for the official DeepSeek Harness
(DSH) Agent. Rust + Ratatui draw the UI. The Agent itself is the official one,
built from the pinned submodule and shipped inside the bundle.

**We rewrite the UI. We do not rewrite the Agent.**

Everything that makes the Agent an Agent stays upstream:

- the Agent Loop, the Provider adapters, tool calling, MCP, SubAgent fan-out,
- context management and compaction,
- session persistence.

If a feature needs Agent behaviour that does not exist upstream, the answer is a
question for upstream, not a local reimplementation. A Rust copy of the Agent loop
is out of scope by principle, not by schedule.

## Architecture rules

### Process boundary

The TUI and the Agent are separate processes. The TUI spawns the bundled runtime
and talks to it over the official SDK transport: newline-delimited JSON-RPC 2.0 on
stdio, exactly as `packages/sdk/protocol` defines it in the submodule.

- Never link the Agent into the Rust process.
- Never reimplement a frame format, an id scheme or a method name from memory.
  Read `runtime/upstream/packages/sdk/protocol/src/` and mirror it in
  `crates/dsh-protocol/src/protocol.rs`.
- stdout of the child process carries frames only. Anything we print for humans
  goes to our own stderr or to the log file.

### Crate boundaries

| crate | owns | must not |
| --- | --- | --- |
| `dsh-protocol` | wire types, the runtime lock file, version comparison, release selection, the `dsh-runtime-lock` tool | draw anything, hold UI state |
| `dsh-state` | UI state, event projection, tool-call aggregation, message folding, session view model | spawn processes, touch the terminal |
| `dsh-tui` | Ratatui layout, rendering, key handling, terminal lifecycle | parse JSON-RPC by hand, own business rules |

Data flows one way: transport frames become `dsh-protocol` types, `dsh-state`
projects them into view state, `dsh-tui` renders that state. A renderer that
reaches back into protocol types for anything other than display is a design smell.

### No network at startup

The client never downloads, checks for, or installs a DSH runtime at startup. The
runtime ships with the bundle. `scripts/build-runtime.sh` runs during packaging and
CI only.

## Versioning

`runtime/upstream.lock.toml` is the single source of truth. It records the upstream
repository, release tag, product version, full commit, submodule path and build
recipe. `[workspace.package].version` in `Cargo.toml` mirrors it.

- The product version equals the bundled DSH version, always. `0.2.2-rc.5` here
  means the bundle carries DSH `0.2.2-rc.5`.
- A UI-only fix does not bump the version. It bumps the build id
  (`DSH_TUI_BUILD_ID`, normally the git commit) or the git commit itself.
- A version bump always moves the runtime commit with it. `dsh-runtime-lock set`
  does both in one step; never edit the version by hand.
- Published tags are immutable. An existing tag is never moved, deleted or
  force-pushed.
- End users are never upgraded because upstream shipped something. They upgrade
  when this repository publishes a release.

Guards that enforce the above and must stay green:

```sh
./scripts/check-runtime-pin.sh
cargo run --locked -p dsh-protocol --bin dsh-runtime-lock -- check
./scripts/test-pin-guards.sh
```

## Upstream tracking

`.github/workflows/upstream-sync.yml` runs every six hours and on demand. It reads
published releases only, never default-branch commits, and opens a pull request
when a newer version appears.

Auto-merge is off unless the repository variable `AUTO_MERGE_UPSTREAM` is `true`,
and even then it only merges after every gate passes. Auto-merging source is never
auto-releasing: `release.yml` fires on manual dispatch only.

Nothing in `runtime/upstream/` is edited by hand. If a change is truly unavoidable
it goes in `patches/` as a patch file applied by the packaging scripts, so the
upgrade diff stays visible and the patch check in the sync workflow can prove it
still applies.

## Required checks before a commit

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
./scripts/check-runtime-pin.sh
```

CI runs the same commands plus `shellcheck scripts/*.sh`, the pin guard tests, the
wire contract check and a bundled-runtime build. A green local `cargo test` is not
enough on its own because the contract and pin checks do not live in `cargo test`.

## Tests

Every test asserts behaviour that could actually break. A test that cannot fail is
worse than no test: it hides the gap.

- Do not add placeholder tests that always pass.
- If something cannot be tested yet, mark it explicitly (a `// UNCOVERED:` comment
  naming what is missing and why) instead of faking coverage.
- Round-one tests cover the lock parser, version ordering, release selection, pin
  rewriting, terminal lifecycle, key mapping, layout degradation and folding.
  Nothing in round one drives a real Agent process; that is round two.

## Style

- Edition 2024, stable Rust, `rustfmt` defaults.
- Comments and commit messages in English. User-facing doc prose may be Chinese.
- `clippy` warnings are errors. Do not silence a lint without saying why on the
  line above it.
- No `unwrap()`/`expect()` outside tests and startup paths that legitimately cannot
  continue.

## Repository hygiene

Never commit build output, `target/`, `node_modules/`, `.dsh-tui/`, credentials or
API keys. `.gitignore` covers the usual suspects; do not weaken it.

## Licensing

This project is MIT. The submodule keeps upstream's own license and copyright
notices untouched; never strip or rewrite a copyright header inside
`runtime/upstream/`.