//! Engine gRPC Client
//!
//! Provides a client for communicating with the capsuled daemon
//! via the onescluster.engine.v1 gRPC service.

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::CapsuleManifestV1;
use capsuled::proto::onescluster::engine::v1::{
    engine_client::EngineClient, deploy_request::Manifest as DeployManifest,
    DeployRequest, DeployResponse, EngineLogEntry, GetSystemStatusRequest,
    LogRequest, StopRequest, StopResponse, SystemStatus,
};
use std::time::Duration;
use tonic::transport::Channel;
use tonic::Streaming;

/// Default engine endpoint
pub const DEFAULT_ENGINE_URL: &str = "http://127.0.0.1:50051";

/// Engine client wrapper with high-level operations
pub struct CapsuleEngineClient {
    client: EngineClient<Channel>,
    endpoint: String,
}

impl CapsuleEngineClient {
    /// Create a new engine client and connect to the endpoint
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())
            .context("Invalid endpoint URL")?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .connect()
            .await
            .with_context(|| format!("Failed to connect to engine at {}", endpoint))?;

        let client = EngineClient::new(channel);

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
        })
    }

    /// Try to connect with a quick timeout (for health check)
    pub async fn try_connect(endpoint: &str) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())
            .context("Invalid endpoint URL")?
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .connect()
            .await
            .with_context(|| format!("Engine not reachable at {}", endpoint))?;

        let client = EngineClient::new(channel);

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
        })
    }

    /// Get the endpoint this client is connected to
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Deploy a capsule to the engine
    ///
    /// Sends the manifest as TOML content for Engine parsing
    pub async fn deploy_capsule(
        &mut self,
        capsule_id: &str,
        manifest: &CapsuleManifestV1,
        signature: Option<&[u8]>,
    ) -> Result<DeployResponse> {
        // Serialize manifest to TOML for Engine processing
        let toml_content = toml::to_string_pretty(manifest)
            .context("Failed to serialize manifest to TOML")?;

        let request = DeployRequest {
            capsule_id: capsule_id.to_string(),
            manifest: Some(DeployManifest::TomlContent(toml_content)),
            oci_image: String::new(),
            digest: String::new(),
            manifest_signature: signature.map(|s| s.to_vec()).unwrap_or_default(),
        };

        let response = self
            .client
            .deploy_capsule(request)
            .await
            .context("Deploy capsule RPC failed")?;

        Ok(response.into_inner())
    }

    /// Deploy a capsule with source working directory
    ///
    /// For local development, this specifies where the source files are located
    /// so the engine can access them directly.
    pub async fn deploy_capsule_with_source(
        &mut self,
        capsule_id: &str,
        manifest: &CapsuleManifestV1,
        signature: Option<&[u8]>,
        _source_working_dir: &str, // Reserved for future use in DeployRequest extension
    ) -> Result<DeployResponse> {
        // For now, use the standard deploy flow
        // In future, we may extend DeployRequest to include source_working_dir
        self.deploy_capsule(capsule_id, manifest, signature).await
    }

    /// Stop a running capsule
    pub async fn stop_capsule(&mut self, capsule_id: &str) -> Result<StopResponse> {
        let request = StopRequest {
            capsule_id: capsule_id.to_string(),
        };

        let response = self
            .client
            .stop_capsule(request)
            .await
            .context("Stop capsule RPC failed")?;

        Ok(response.into_inner())
    }

    /// Get system status including running capsules
    pub async fn get_system_status(&mut self) -> Result<SystemStatus> {
        let request = GetSystemStatusRequest {};

        let response = self
            .client
            .get_system_status(request)
            .await
            .context("Get system status RPC failed")?;

        Ok(response.into_inner())
    }

    /// Stream logs from a capsule
    pub async fn stream_logs(
        &mut self,
        capsule_id: &str,
        follow: bool,
        tail_lines: u64,
    ) -> Result<Streaming<EngineLogEntry>> {
        let request = LogRequest {
            capsule_id: capsule_id.to_string(),
            follow,
            tail_lines,
        };

        let response = self
            .client
            .stream_logs(request)
            .await
            .context("Stream logs RPC failed")?;

        Ok(response.into_inner())
    }

    /// Check if the engine is reachable
    pub async fn health_check(&mut self) -> Result<bool> {
        match self.get_system_status().await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

/// Resolve the engine URL from environment or default
pub fn resolve_engine_url(cli_override: Option<&str>) -> String {
    if let Some(url) = cli_override {
        return url.to_string();
    }

    std::env::var("CAPSULE_ENGINE_URL").unwrap_or_else(|_| DEFAULT_ENGINE_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_engine_url_default() {
        // Clear env var for test
        std::env::remove_var("CAPSULE_ENGINE_URL");
        assert_eq!(resolve_engine_url(None), DEFAULT_ENGINE_URL);
    }

    #[test]
    fn test_resolve_engine_url_cli_override() {
        let url = "http://custom:9999";
        assert_eq!(resolve_engine_url(Some(url)), url);
    }
}
