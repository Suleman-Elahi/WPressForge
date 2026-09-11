//! Types shared between the control plane (`wp-panel`) and the node agent (`wp-agent`).
//!
//! Nothing in here may depend on axum, sqlx or any runtime detail: it is the
//! contract between the two binaries and is versioned with [`PROTOCOL_VERSION`].

pub mod error;
pub mod fmt;
pub mod models;
pub mod protocol;

/// Bumped whenever the wire format changes in a backwards-incompatible way.
/// The agent refuses requests carrying a different major version.
pub const PROTOCOL_VERSION: u32 = 1;

pub use error::{Error, Result};
