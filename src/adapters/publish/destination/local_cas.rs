use anyhow::Result;
use async_trait::async_trait;

use crate::application::ports::publish::{
    DestinationPort, DestinationSpec, PublishableArtifact, PublishedLocation,
};

#[derive(Debug, Default)]
pub(crate) struct LocalCasDestination;

#[async_trait]
impl DestinationPort for LocalCasDestination {
    async fn publish(
        &self,
        artifact: &PublishableArtifact,
        destination: &DestinationSpec,
    ) -> Result<PublishedLocation> {
        let DestinationSpec::LocalCas {
            output_dir,
            scoped_id,
            version,
            normalized_file_name,
        } = destination
        else {
            anyhow::bail!("local CAS destination requires DestinationSpec::LocalCas")
        };

        let path = crate::install::register_verified_artifact_for_publish(
            output_dir.clone(),
            scoped_id,
            version,
            normalized_file_name,
            &artifact.bytes,
            &artifact.content_hash,
        )?;

        Ok(PublishedLocation {
            destination: destination.clone(),
            receipt: format!("registered {}", path.display()),
            locator: path.display().to_string(),
            metadata: None,
        })
    }
}
