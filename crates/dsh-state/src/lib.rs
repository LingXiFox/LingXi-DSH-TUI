//! SDK session projection, disk journal and Unicode draft editing.
//!
//! No legacy P1 demo UI model is exported after the clean-slate reset.

#![deny(missing_docs)]

mod archive;
pub mod conversation;
pub mod draft;
mod transport;

pub use transport::TransportState;
