pub mod builder;
pub mod config;
pub mod daemon;
pub mod detector;
pub mod notifier;
pub mod output;
pub mod queue;
pub mod state;
pub mod watcher;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuildWatchError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

/// Project hash used for state directory naming.
/// Deterministic SHA-256 (first 8 bytes) of the canonical project root path.
pub fn project_hash(root: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
    hash[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// Returns the platform-specific state directory for BuildWatch.
pub fn state_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir().join("buildwatch")
    } else {
        std::path::PathBuf::from("/tmp/buildwatch")
    }
}
