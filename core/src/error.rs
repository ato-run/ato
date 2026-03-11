use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Manifest error in {0}: {1}")]
    Manifest(PathBuf, String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Process execution error: {0}")]
    Execution(String),

    #[error("Hash mismatch: expected {0}, got {1}")]
    HashMismatch(String, String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error("Sidecar IPC error: {0}")]
    SidecarIpc(String),

    #[error("Sidecar request failed ({0}): {1}")]
    SidecarRequest(String, String),

    #[error("Sidecar response error: {0}")]
    SidecarResponse(String),

    #[error("Container engine error: {0}")]
    ContainerEngine(String),

    #[error("Process spawn error: {0}")]
    ProcessStart(String),

    #[error("Execution timed out")]
    Timeout,

    #[error("Cryptographic error: {0}")]
    Crypto(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Authentication required: {0}")]
    AuthRequired(String),

    #[error("Build/Pack error: {0}")]
    Pack(String),

    #[error("Strict manifest fallback is not allowed: {0}")]
    StrictManifestFallbackNotAllowed(String),

    #[error("Unknown error: {0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, CapsuleError>;
