use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishableArtifact {
    pub bytes: Vec<u8>,
    pub scoped_id: String,
    pub version: String,
    pub normalized_file_name: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationSpec {
    LocalCas {
        output_dir: Option<PathBuf>,
        scoped_id: String,
        version: String,
        normalized_file_name: String,
    },
    RemoteRegistry {
        registry_url: String,
        scoped_id: String,
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedLocation {
    pub destination: DestinationSpec,
    pub receipt: String,
    pub locator: String,
}

#[async_trait]
pub trait DestinationPort: Send + Sync {
    async fn publish(
        &self,
        artifact: &PublishableArtifact,
        destination: &DestinationSpec,
    ) -> Result<PublishedLocation>;
}

pub type SharedDestinationPort = Arc<dyn DestinationPort>;
