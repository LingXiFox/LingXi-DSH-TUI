//! Wire contract between the LingXi TUI and the bundled DSH runtime.
//!
//! The layout mirrors the four things this crate may ever own:
//!
//! * [`lock`] — the upstream pin (`runtime/upstream.lock.toml`) and its parse/verify rules.
//! * [`version`] — released-tag ordering and compatibility checks.
//! * [`releases`] — picking which upstream release to pin next.
//! * [`protocol`] — request/response/notification shapes for later Host RPC work.
//! * [`sync`] — pure pin-move and contract-check helpers used by the sync workflow.
//!
//! Nothing here talks to the network or starts a process: round one only ships
//! the version contract that every other component validates against.

#![deny(missing_docs)]

pub mod lock;
pub mod protocol;
pub mod releases;
pub mod stdio;
pub mod sync;
pub mod version;

pub use lock::{LockError, RuntimeLock};
pub use releases::{ReleaseCandidate, Selection, select_newer};
pub use sync::{
    PinMove, SyncError, apply_pin, missing_wire_names, rewrite_workspace_version, workspace_version,
};
pub use version::{Compatibility, ReleaseVersion, VersionError};
