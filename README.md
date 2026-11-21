# LingXi DSH-TUI

Rust terminal client for the **official DeepSeek Harness Agent**. Product version follows the bundled official runtime version.

## UI clean-slate branch

**This branch intentionally removes the previous P1–P6.1 terminal interface.**
The normal `dsh-tui` command currently opens a minimal placeholder screen and exits with `q`, `Esc`, or `Ctrl+C`. Chatting, session pickers, approvals and tool details are **temporarily unavailable in the UI**, awaiting a fresh design. Do not present this branch as a functioning chat client.

The reset protects the existing SDK/backend work, including:

- `crates/dsh-protocol/`: official JSON-RPC contract, version lock and upstream sync checks
- `crates/dsh-state/src/conversation.rs`: SDK event projection, session journal and replay
- `crates/dsh-state/src/archive.rs`: bounded storage and journal content loading
- `crates/dsh-state/src/draft.rs`: Unicode-aware editable draft state
- `crates/dsh-tui/src/runtime.rs`, `runtime/host.mjs`: bundled official SDK process lifecycle and optional ecosystem bridge
- `crates/dsh-tui/src/terminal.rs`: terminal setup and cleanup
- `runtime/upstream/` and `runtime/upstream.lock.toml`: **unchanged official runtime pin**

No Agent Loop, Provider, MCP, Skills or official persisted session implementation is rewritten. The bundled Agent remains pinned to `dsh-v0.2.2-rc.5` (`9c2670d3bce5275e0d1610272958121af87787bb`).

## Build and checks

```sh
git submodule update --init runtime/upstream
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p dsh-tui -- --version

# Requires a built pinned SDK runtime:
./scripts/build-runtime.sh
cargo run -p dsh-tui -- --check-runtime

# Empty UI scaffold:
cargo run -p dsh-tui
```

This is a temporary development branch; the historical P2–P5 research documents remain for backend integration and performance context. **Do not reuse the deleted UI renderer or import old screen layout patterns.** Start a new view architecture after design review.

The upstream sync workflow and manually triggered release pipeline are retained. **Do not release the placeholder as a user build.** See `AGENTS.md` and `docs/UI-RESET.md`.
