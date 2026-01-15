//! Docker Registry API v2 Client
//!
//! Implements the Docker Registry HTTP API V2 specification for:
//! - Image manifest retrieval
//! - Layer blob download
//! - Authentication (Bearer token)

use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, info};

/// Registry client errors
#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Manifest not found: {0}")]
    ManifestNotFound(String),

    #[error("Blob not found: {0}")]
    BlobNotFound(String),

    #[error("Digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),
}

pub type RegistryResult<T> = Result<T, RegistryError>;

/// Image reference (e.g., "docker.io/library/nginx:latest")
#[derive(Debug, Clone)]
pub struct ImageRef {
    pub registry: String,
    pub repository: String,
    pub tag: String,
}

impl ImageRef {
    /// Parse an image reference string
    pub fn parse(image: &str) -> RegistryResult<Self> {
        let parts: Vec<&str> = image.splitn(2, '/').collect();

        let (registry, repo_tag) = if parts.len() == 1 {
            // No registry specified, assume Docker Hub
            (
                "registry-1.docker.io".to_string(),
                format!("library/{}", parts[0]),
            )
        } else if parts[0].contains('.') || parts[0].contains(':') {
            // Custom registry
            (parts[0].to_string(), parts[1].to_string())
        } else {
            // Docker Hub with namespace
            ("registry-1.docker.io".to_string(), image.to_string())
        };

        // Split repository and tag
        let repo_tag_parts: Vec<&str> = repo_tag.splitn(2, ':').collect();
        let repository = repo_tag_parts[0].to_string();
        let tag = repo_tag_parts.get(1).unwrap_or(&"latest").to_string();

        Ok(Self {
            registry,
            repository,
            tag,
        })
    }

    /// Get the manifest URL
    pub fn manifest_url(&self) -> String {
        format!(
            "https://{}/v2/{}/manifests/{}",
            self.registry, self.repository, self.tag
        )
    }

    /// Get the blob URL for a given digest
    pub fn blob_url(&self, digest: &str) -> String {
        format!(
            "https://{}/v2/{}/blobs/{}",
            self.registry, self.repository, digest
        )
    }
}

/// OCI Image Manifest (simplified)
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    pub schema_version: i32,
    #[serde(default)]
    pub media_type: String,
    pub config: ManifestConfig,
    pub layers: Vec<ManifestLayer>,
}

/// Manifest List for multi-arch images
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestList {
    pub schema_version: i32,
    #[serde(default)]
    pub media_type: String,
    pub manifests: Vec<ManifestPlatform>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPlatform {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
    pub platform: Platform,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestConfig {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ManifestLayer {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
}

/// Docker Registry API v2 Client
pub struct RegistryClient {
    client: Client,
    auth_token: Option<String>,
}

impl RegistryClient {
    /// Create a new registry client
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("nacelle/1.0.0")
                .build()
                .expect("Failed to create HTTP client"),
            auth_token: None,
        }
    }

    /// Authenticate with the registry (Docker Hub Bearer token)
    pub async fn authenticate(&mut self, image_ref: &ImageRef) -> RegistryResult<()> {
        // Try to get manifest without auth first
        let response = self
            .client
            .get(image_ref.manifest_url())
            .header("Accept", "application/vnd.oci.image.manifest.v1+json")
            .header(
                "Accept",
                "application/vnd.docker.distribution.manifest.v2+json",
            )
            .send()
            .await?;

        if response.status().is_success() {
            // No auth needed
            return Ok(());
        }

        // Check for WWW-Authenticate header
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(auth_header) = response.headers().get("www-authenticate") {
                let auth_str = auth_header.to_str().unwrap_or("");
                return self.handle_bearer_auth(auth_str, image_ref).await;
            }
        }

        Err(RegistryError::AuthFailed(
            "Authentication required but no WWW-Authenticate header".to_string(),
        ))
    }

    /// Handle Bearer token authentication
    async fn handle_bearer_auth(
        &mut self,
        auth_header: &str,
        image_ref: &ImageRef,
    ) -> RegistryResult<()> {
        // Parse Bearer realm="...",service="...",scope="..."
        let realm = Self::extract_param(auth_header, "realm")
            .ok_or_else(|| RegistryError::AuthFailed("Missing realm in auth header".to_string()))?;
        let service = Self::extract_param(auth_header, "service").unwrap_or_default();
        let scope = format!("repository:{}:pull", image_ref.repository);

        // Request token
        let token_url = format!("{}?service={}&scope={}", realm, service, scope);
        debug!("Requesting auth token from: {}", token_url);

        let token_response: serde_json::Value =
            self.client.get(&token_url).send().await?.json().await?;

        if let Some(token) = token_response.get("token").and_then(|t| t.as_str()) {
            self.auth_token = Some(token.to_string());
            info!("Successfully authenticated with registry");
            Ok(())
        } else {
            Err(RegistryError::AuthFailed(
                "No token in response".to_string(),
            ))
        }
    }

    fn extract_param(header: &str, param: &str) -> Option<String> {
        let search = format!("{}=\"", param);
        if let Some(start) = header.find(&search) {
            let value_start = start + search.len();
            if let Some(end) = header[value_start..].find('"') {
                return Some(header[value_start..value_start + end].to_string());
            }
        }
        None
    }

    /// Get image manifest, handling manifest lists for multi-arch images
    pub async fn get_manifest(&self, image_ref: &ImageRef) -> RegistryResult<ImageManifest> {
        // First, try to get the manifest (might be a list or direct manifest)
        let body = self.fetch_manifest_raw(image_ref, &image_ref.tag).await?;

        // Try to parse as manifest list first
        if let Ok(list) = serde_json::from_str::<ManifestList>(&body) {
            // Check if it has manifests field (indicates manifest list)
            if !list.manifests.is_empty() {
                info!(
                    "Detected manifest list with {} platforms",
                    list.manifests.len()
                );

                // Prefer linux/arm64 platform for ARM-first edge targets
                let target_arch = "arm64";
                let target_os = "linux";

                let platform_manifest = list
                    .manifests
                    .iter()
                    .find(|m| m.platform.architecture == target_arch && m.platform.os == target_os)
                    .or_else(|| list.manifests.first()) // Fallback to first
                    .ok_or_else(|| {
                        RegistryError::Parse("No suitable platform found".to_string())
                    })?;

                info!(
                    "Selected platform: {}/{}",
                    platform_manifest.platform.os, platform_manifest.platform.architecture
                );

                // Fetch the actual manifest by digest
                let manifest_body = self
                    .fetch_manifest_raw(image_ref, &platform_manifest.digest)
                    .await?;
                let manifest: ImageManifest =
                    serde_json::from_str(&manifest_body).map_err(|e| {
                        RegistryError::Parse(format!("Failed to parse platform manifest: {}", e))
                    })?;

                info!("Retrieved manifest with {} layers", manifest.layers.len());
                return Ok(manifest);
            }
        }

        // Parse as direct image manifest
        let manifest: ImageManifest =
            serde_json::from_str(&body).map_err(|e| RegistryError::Parse(e.to_string()))?;

        info!("Retrieved manifest with {} layers", manifest.layers.len());
        Ok(manifest)
    }

    /// Fetch raw manifest body by tag or digest
    async fn fetch_manifest_raw(
        &self,
        image_ref: &ImageRef,
        reference: &str,
    ) -> RegistryResult<String> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            image_ref.registry, image_ref.repository, reference
        );

        let mut request = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.oci.image.manifest.v1+json")
            .header(
                "Accept",
                "application/vnd.docker.distribution.manifest.v2+json",
            )
            .header("Accept", "application/vnd.oci.image.index.v1+json")
            .header(
                "Accept",
                "application/vnd.docker.distribution.manifest.list.v2+json",
            );

        if let Some(token) = &self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::ManifestNotFound(format!(
                "{}:{}",
                image_ref.repository, reference
            )));
        }

        if !response.status().is_success() {
            return Err(RegistryError::AuthFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| RegistryError::Parse(e.to_string()))?;

        Ok(body)
    }

    /// Download a blob (layer) to a file
    pub async fn download_blob(
        &self,
        image_ref: &ImageRef,
        digest: &str,
        output_path: &PathBuf,
    ) -> RegistryResult<()> {
        use tokio::io::AsyncWriteExt;

        let mut request = self.client.get(image_ref.blob_url(digest));

        if let Some(token) = &self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::BlobNotFound(digest.to_string()));
        }

        if !response.status().is_success() {
            return Err(RegistryError::AuthFailed(format!(
                "HTTP {} for blob {}",
                response.status(),
                digest
            )));
        }

        // Stream the response to file
        let bytes = response.bytes().await?;

        // Verify digest
        let actual_digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual_digest != digest {
            return Err(RegistryError::DigestMismatch {
                expected: digest.to_string(),
                actual: actual_digest,
            });
        }

        // Write to file
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut file = tokio::fs::File::create(output_path).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;

        debug!("Downloaded blob {} ({} bytes)", digest, bytes.len());
        Ok(())
    }
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ref_parse_simple() {
        let img = ImageRef::parse("nginx").unwrap();
        assert_eq!(img.registry, "registry-1.docker.io");
        assert_eq!(img.repository, "library/nginx");
        assert_eq!(img.tag, "latest");
    }

    #[test]
    fn test_image_ref_parse_with_tag() {
        let img = ImageRef::parse("nginx:1.25").unwrap();
        assert_eq!(img.registry, "registry-1.docker.io");
        assert_eq!(img.repository, "library/nginx");
        assert_eq!(img.tag, "1.25");
    }

    #[test]
    fn test_image_ref_parse_with_namespace() {
        let img = ImageRef::parse("myuser/myimage:v1.0").unwrap();
        assert_eq!(img.registry, "registry-1.docker.io");
        assert_eq!(img.repository, "myuser/myimage");
        assert_eq!(img.tag, "v1.0");
    }

    #[test]
    fn test_image_ref_parse_custom_registry() {
        let img = ImageRef::parse("ghcr.io/owner/repo:sha-abc123").unwrap();
        assert_eq!(img.registry, "ghcr.io");
        assert_eq!(img.repository, "owner/repo");
        assert_eq!(img.tag, "sha-abc123");
    }

    #[test]
    fn test_image_ref_parse_localhost() {
        let img = ImageRef::parse("localhost:5000/myimage:dev").unwrap();
        assert_eq!(img.registry, "localhost:5000");
        assert_eq!(img.repository, "myimage");
        assert_eq!(img.tag, "dev");
    }
}
