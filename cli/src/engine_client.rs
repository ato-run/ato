//! Engine gRPC Client
//!
//! Provides a client for communicating with the nacelle daemon
//! via the nacelle.engine.v1 gRPC service.

use anyhow::{Context, Result};
use nacelle::capsule_types::capsule_v1::CapsuleManifestV1;
use nacelle::proto::nacelle::engine::v1::{
    deploy_request::Manifest as DeployManifest, engine_client::EngineClient, DeployRequest,
    DeployResponse, EngineLogEntry, GetSystemStatusRequest, LogRequest, StopRequest, StopResponse,
    SystemStatus,
};
use std::path::PathBuf;
use std::time::Duration;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Streaming;

/// Default engine endpoint
pub const DEFAULT_ENGINE_URL: &str = "http://127.0.0.1:50051";

/// Engine client wrapper with high-level operations
#[allow(dead_code)]
pub struct CapsuleEngineClient {
    client: EngineClient<Channel>,
    endpoint: String,
    auth_token: Option<String>,
}

/// Get the auth token file path for the current platform
fn get_auth_token_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| h.join("Library/Application Support/dev.gumball.app/auth_token"))
    }
    #[cfg(target_os = "linux")]
    {
        dirs::home_dir().map(|h| h.join(".local/share/dev.gumball.app/auth_token"))
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|d| d.join("dev.gumball.app/auth_token"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Load the auth token from the file system
fn load_auth_token() -> Option<String> {
    get_auth_token_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
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
        let auth_token = load_auth_token();

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            auth_token,
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
        let auth_token = load_auth_token();

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            auth_token,
        })
    }

    /// Get the endpoint this client is connected to
    #[allow(dead_code)]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Deploy a capsule to the engine
    ///
    /// Sends the manifest as TOML (capsule.toml) for Engine parsing. TOML
    /// preserves the on-disk representation and is preferred for imports.
    pub async fn deploy_capsule(
        &mut self,
        capsule_id: &str,
        manifest: &CapsuleManifestV1,
        signature: Option<&[u8]>,
    ) -> Result<DeployResponse> {
        // Serialize manifest to TOML for Engine processing
        let toml_str = manifest
            .to_toml()
            .context("Failed to serialize manifest to TOML")?;

        let request = DeployRequest {
            capsule_id: capsule_id.to_string(),
            manifest: Some(DeployManifest::TomlContent(toml_str)),
            oci_image: String::new(),
            digest: String::new(),
            manifest_signature: signature.map(|s| s.to_vec()).unwrap_or_default(),
        };

        // Build request with auth token if available
        let mut grpc_request = tonic::Request::new(request);
        if let Some(token) = &self.auth_token {
            if let Ok(value) = MetadataValue::try_from(format!("Bearer {}", token)) {
                grpc_request.metadata_mut().insert("authorization", value);
            }
        }

        let response = self
            .client
            .deploy_capsule(grpc_request)
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

        // Build request with auth token if available
        let mut grpc_request = tonic::Request::new(request);
        if let Some(token) = &self.auth_token {
            if let Ok(value) = MetadataValue::try_from(format!("Bearer {}", token)) {
                grpc_request.metadata_mut().insert("authorization", value);
            }
        }

        let response = self
            .client
            .stop_capsule(grpc_request)
            .await
            .context("Stop capsule RPC failed")?;

        Ok(response.into_inner())
    }

    /// Get system status including running capsules
    pub async fn get_system_status(&mut self) -> Result<SystemStatus> {
        let request = GetSystemStatusRequest {};

        // Build request with auth token if available
        let mut grpc_request = tonic::Request::new(request);
        if let Some(token) = &self.auth_token {
            if let Ok(value) = MetadataValue::try_from(format!("Bearer {}", token)) {
                grpc_request.metadata_mut().insert("authorization", value);
            }
        }

        let response = self
            .client
            .get_system_status(grpc_request)
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

        // Build request with auth token if available
        let mut grpc_request = tonic::Request::new(request);
        if let Some(token) = &self.auth_token {
            if let Ok(value) = MetadataValue::try_from(format!("Bearer {}", token)) {
                grpc_request.metadata_mut().insert("authorization", value);
            }
        }

        let response = self
            .client
            .stream_logs(grpc_request)
            .await
            .context("Stream logs RPC failed")?;

        Ok(response.into_inner())
    }

    /// Check if the engine is reachable
    #[allow(dead_code)]
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
