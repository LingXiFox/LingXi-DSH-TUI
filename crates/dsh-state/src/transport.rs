//! Official SDK transport lifecycle state, independent of any terminal UI.

/// Process connection status published by the SDK transport worker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TransportState {
    /// The runtime has not been launched.
    #[default]
    Detached,
    /// A subprocess is launching or performing its handshake.
    Connecting,
    /// The SDK handshake completed.
    Ready {
        /// Official SDK server name.
        server_name: String,
        /// SDK version if reported by the server.
        server_version: Option<String>,
    },
    /// The runtime or connection failed.
    Failed(String),
}

impl TransportState {
    /// Short transport status string, usable by a new UI without inheriting old layouts.
    pub fn label(&self) -> &str {
        match self {
            Self::Detached => "detached",
            Self::Connecting => "connecting",
            Self::Ready { .. } => "ready",
            Self::Failed(_) => "failed",
        }
    }
}
