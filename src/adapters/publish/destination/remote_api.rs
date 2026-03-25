use anyhow::Result;
use async_trait::async_trait;

use crate::application::ports::publish::{
    DestinationPort, DestinationSpec, PublishReceiptMetadata, PublishableArtifact,
    PublishedLocation,
};

#[derive(Debug, Default)]
pub(crate) struct RemoteRegistryDestination;

#[async_trait]
impl DestinationPort for RemoteRegistryDestination {
    async fn publish(
        &self,
        artifact: &PublishableArtifact,
        destination: &DestinationSpec,
    ) -> Result<PublishedLocation> {
        let DestinationSpec::RemoteRegistry {
            registry_url,
            scoped_id,
            version: _,
            allow_existing,
            force_large_payload,
        } = destination
        else {
            anyhow::bail!("remote registry destination requires DestinationSpec::RemoteRegistry")
        };

        let published = tokio::task::spawn_blocking({
            let args = crate::publish_artifact::PublishArtifactBytesArgs {
                artifact_bytes: artifact.bytes.clone(),
                scoped_id: scoped_id.clone(),
                registry_url: registry_url.clone(),
                force_large_payload: *force_large_payload,
                allow_existing: *allow_existing,
                lock_id: artifact.lock_id.clone(),
                closure_digest: artifact.closure_digest.clone(),
            };
            move || crate::publish_artifact::publish_artifact_bytes(args)
        })
        .await
        .map_err(anyhow::Error::from)??;

        Ok(PublishedLocation {
            destination: destination.clone(),
            receipt: format!("uploaded {}", published.file_name),
            locator: published.artifact_url,
            metadata: Some(PublishReceiptMetadata {
                file_name: published.file_name,
                sha256: published.sha256,
                blake3: published.blake3,
                size_bytes: published.size_bytes,
                already_existed: published.already_existed,
            }),
        })
    }
}
