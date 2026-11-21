# UI reset manifest

Baseline: `feat/p6-1-ui` at `0ac12c35959971bc5edbcc016b7f1a4150af1b47`.

Clean-slate branch: `refactor/tui-clean-slate`.

The legacy `crates/dsh-tui/src/render/` tree, mixed UI/controller `app.rs` and `app/interaction.rs`, old golden screenshots, and UI-dependent PTY chat script are removed. Old P1 demo activity/session state is also removed and replaced by a tiny SDK `TransportState` module.

**Preserved:** official SDK process wrapper, configuration sharing, SDK types, source pin, event archive/projection, Unicode draft handling, terminal lifecycle, CI pin verification and SDK startup smoke test. The minimal terminal placeholder deliberately does not run a conversation.

**CI limitation during reset:** the previous PTY chat regression is no longer an active check because it tested a deleted interface. The SDK handshake/smoke check remains. New end-to-end chat/approval/TUI tests are required before the frontend can be considered usable or released.

New design must start from a written component/layout contract, not by recovering deleted files. Avoid using any old screenshot as the intended appearance; historical UI notes under docs are for performance/backend analysis only.
