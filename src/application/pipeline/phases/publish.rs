use anyhow::Result;

use crate::application::ports::publish::{
    DestinationSpec, PublishableArtifact, PublishedLocation, SharedDestinationPort,
};

#[derive(Debug, Clone)]
pub struct PublishPhaseRequest {
    pub artifact: PublishableArtifact,
    pub destination: DestinationSpec,
}

pub struct PublishPhase {
    destination: SharedDestinationPort,
}

impl PublishPhase {
    pub fn new(destination: SharedDestinationPort) -> Self {
        Self { destination }
    }

    pub async fn execute(&self, request: &PublishPhaseRequest) -> Result<PublishedLocation> {
        self.destination
            .publish(&request.artifact, &request.destination)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{PublishPhase, PublishPhaseRequest};
    use crate::application::ports::publish::{
        DestinationPort, DestinationSpec, PublishableArtifact, PublishedLocation,
    };

    #[derive(Debug)]
    struct StubDestination;

    #[async_trait]
    impl DestinationPort for StubDestination {
        async fn publish(
            &self,
            artifact: &PublishableArtifact,
            destination: &DestinationSpec,
        ) -> anyhow::Result<PublishedLocation> {
            Ok(PublishedLocation {
                destination: destination.clone(),
                receipt: format!("published {}", artifact.normalized_file_name),
                locator: "memory://published".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn publish_phase_routes_artifact_to_destination_port() {
        let phase = PublishPhase::new(Arc::new(StubDestination));
        let request = PublishPhaseRequest {
            artifact: PublishableArtifact {
                bytes: b"capsule".to_vec(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
                normalized_file_name: "demo-0.1.0.capsule".to_string(),
                content_hash: "blake3:demo".to_string(),
            },
            destination: DestinationSpec::RemoteRegistry {
                registry_url: "https://example.invalid".to_string(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
            },
        };

        let published = phase.execute(&request).await.expect("publish");

        assert_eq!(published.receipt, "published demo-0.1.0.capsule");
        assert_eq!(published.locator, "memory://published");
    }
}
