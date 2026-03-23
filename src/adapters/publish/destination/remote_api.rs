use anyhow::Result;
use async_trait::async_trait;

use crate::application::ports::publish::{
    DestinationPort, DestinationSpec, PublishableArtifact, PublishedLocation,
};

#[derive(Debug, Default)]
pub(crate) struct RemoteRegistryDestination;

#[async_trait]
impl DestinationPort for RemoteRegistryDestination {
    async fn publish(
        &self,
        _artifact: &PublishableArtifact,
        destination: &DestinationSpec,
    ) -> Result<PublishedLocation> {
        let DestinationSpec::RemoteRegistry {
            registry_url,
            scoped_id,
            version,
        } = destination
        else {
            anyhow::bail!("remote registry destination requires DestinationSpec::RemoteRegistry")
        };

        anyhow::bail!(
            "remote publish destination is not wired yet for {} {}@{}",
            registry_url,
            scoped_id,
            version
        )
    }
}
