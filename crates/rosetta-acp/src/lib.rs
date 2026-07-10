pub mod client;
pub mod transport;

pub use client::{AcpClient, AcpStreamItem};
pub use transport::AcpTransport;

use thiserror::Error;

/// Errors that can occur when interacting with the ACP transport or client.
#[derive(Error, Debug)]
pub enum AcpError {
    /// An underlying I/O error (e.g., broken pipe, process failure).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A protocol-level error (unexpected message, JSON-RPC error response, etc.).
    #[error("Protocol error: {message}")]
    Protocol { message: String },

    /// The transport stream has closed (process disconnected or EOF).
    #[error("Disconnected")]
    Disconnected,
}
