use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ToolArtifactError {
    #[error(
        "tool artifact '{name}' targets platform '{platform}' which is not supported on this host '{host}'"
    )]
    UnsupportedArtifactPlatform {
        name: String,
        platform: String,
        host: String,
    },

    #[error("tool artifact '{name}' download from {url} failed: {source}")]
    ArtifactDownloadFailed {
        name: String,
        url: String,
        #[source]
        source: anyhow::Error,
    },

    #[error(
        "tool artifact '{name}' sha256 mismatch (downloaded from {url}): expected {expected}, got {got}"
    )]
    ArtifactChecksumMismatch {
        name: String,
        url: String,
        expected: String,
        got: String,
    },

    #[error("tool artifact '{name}' could not be unpacked: {reason}")]
    ArtifactUnpackFailed { name: String, reason: String },

    #[error(
        "tool artifact '{name}' is missing required command '{command}' under {}",
        bin_dir.display()
    )]
    ArtifactMissingProvidedCommand {
        name: String,
        command: String,
        bin_dir: PathBuf,
    },

    #[error("tool artifact '{name}' manifest is invalid: {reason}")]
    InvalidArtifactManifest { name: String, reason: String },

    #[error("tool artifact '{name}' store error: {reason}")]
    StoreError { name: String, reason: String },
}
