//! Pure UI state for the LingXi DSH TUI.
//!
//! This crate owns *what the screen shows* and nothing about how it is drawn:
//! rendering lives in `dsh-tui`, runtime I/O lives in `dsh-protocol`. Every
//! transition is a pure `Action -> State` reduction, so the whole UI model is
//! testable without a terminal, a timer, or a subprocess.

#![deny(missing_docs)]

pub mod activity;
mod archive;
pub mod conversation;
pub mod draft;
pub mod session;

pub use activity::{ActivityEntry, ActivityKind, ActivityLog};
pub use session::{Action, AppState, FoldPolicy, RunStatus, TranscriptEntry, TransportState};
