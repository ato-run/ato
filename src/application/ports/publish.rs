use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishArtifactIdentityClass {
    SourceDerivedUnsignedBundle,
    LocallyFinalizedSignedBundle,
    ImportedThirdPartyArtifact,
}

impl PublishArtifactIdentityClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SourceDerivedUnsignedBundle => "source_derived_unsigned_bundle",
            Self::LocallyFinalizedSignedBundle => "locally_finalized_signed_bundle",
            Self::ImportedThirdPartyArtifact => "imported_third_party_artifact",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "source_derived_unsigned_bundle" => Some(Self::SourceDerivedUnsignedBundle),
            "locally_finalized_signed_bundle" => Some(Self::LocallyFinalizedSignedBundle),
            "imported_third_party_artifact" => Some(Self::ImportedThirdPartyArtifact),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishArtifactMetadata {
    pub identity_class: PublishArtifactIdentityClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<String>,
    #[serde(default)]
    pub provenance_limited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishableArtifact {
    pub bytes: Vec<u8>,
    pub scoped_id: String,
    pub version: String,
    pub normalized_file_name: String,
    pub content_hash: String,
    pub lock_id: Option<String>,
    pub closure_digest: Option<String>,
    pub publish_metadata: Option<PublishArtifactMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReceiptMetadata {
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    pub already_existed: bool,
    pub publish_metadata: Option<PublishArtifactMetadata>,
}

#[allow(dead_code)]
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
        allow_existing: bool,
        force_large_payload: bool,
        paid_large_payload: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedLocation {
    pub destination: DestinationSpec,
    pub receipt: String,
    pub locator: String,
    pub metadata: Option<PublishReceiptMetadata>,
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
