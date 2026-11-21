# AGENTS.md — Clean-slate UI development contract

## Scope

LingXi DSH-TUI is a Rust + Ratatui frontend for the **official** bundled DSH Agent. The product version tracks the official Agent version. Do not rewrite the Agent Loop, Provider adapters, MCP, Skills, tools, session persistence or JSON-RPC wire contract.

The old UI was deliberately deleted on `refactor/tui-clean-slate` after repeated visual failures. **Do not recover or copy the old `render/`, `app.rs`, `app/interaction.rs`, old P1 demo state, golden snapshots or PTY UI tests as starting points.** Previous commits are historical evidence only. New UI architecture and component specs must be written from scratch.

## Preserve

- `crates/dsh-protocol/` and `runtime/upstream.lock.toml` with the locked upstream submodule
- `crates/dsh-state/src/conversation.rs`, `archive.rs`, `draft.rs`
- `crates/dsh-tui/src/runtime.rs`, `runtime/tests.rs`, `terminal.rs`, `build_info.rs`
- `runtime/host.mjs`, scripts and upstream-sync workflows

The SDK/runtime and terminal cleanup are independent of presentation. Projection data can feed a new semantic ViewModel, but UI components must not be copied from old files.

## Next UI architecture

Draft and approve a concrete layout specification before writing the renderer. Prefer typed components for user/assistant messages, activity groups, tools, reasoning, editor, modal and optional sidebar; implement a real Markdown parser and strict height budgets for tool previews. Full tool logs belong in an independently scrollable detail surface. Preserve raw events in storage and make debug output opt-in.

Use independent branch visual reviews with screenshots of actual terminal states. Do not merge or release a redesigned UI without explicit user acceptance.

## Engineering rules

- Rust Edition 2024; `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`
- `./scripts/check-runtime-pin.sh` and `cargo run --locked -p dsh-protocol --bin dsh-runtime-lock -- check`
- Never alter the pinned `runtime/upstream/` source or update it at application startup.
- Keep SDK approval fail-closed, preserve event ordering, maintain bounded queues and process cleanup.
- Never commit `target/`, `.dsh-tui/`, credentials, build output or session records.
- Clearly disclose that current branch has a nonfunctional UI placeholder until rebuilt.
