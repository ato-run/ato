use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::application::ports::publish::PublishArtifactMetadata;

use super::artifact::{PublishArtifactResult, V3SyncPayload};

pub(crate) mod direct;
pub(crate) mod presigned;

pub(crate) const ENV_UPLOAD_STRATEGY: &str = "ATO_PUBLISH_UPLOAD_STRATEGY";

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadStrategyKind {
    Direct,
    Presigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadStrategySelectionReason {
    ExplicitEnvironmentOverride,
    RegistryCapabilityDiscovery,
    ManagedStoreHostDefaultDirect,
    CustomRegistryDefaultDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UploadStrategySelection {
    pub(crate) kind: UploadStrategyKind,
    pub(crate) reason: UploadStrategySelectionReason,
}

#[derive(Debug, Clone)]
pub(crate) struct UploadArtifactDescriptor {
    pub(crate) publisher: String,
    pub(crate) slug: String,
    pub(crate) version: String,
    pub(crate) file_name: String,
    pub(crate) sha256: String,
    pub(crate) blake3: String,
    pub(crate) size_bytes: u64,
    pub(crate) allow_existing: bool,
    pub(crate) lock_id: Option<String>,
    pub(crate) closure_digest: Option<String>,
    pub(crate) publish_metadata: Option<PublishArtifactMetadata>,
}

#[derive(Debug, Clone)]
pub(crate) struct UploadPreflightRequest {
    pub(crate) registry_url: String,
    pub(crate) artifact_size_bytes: u64,
    pub(crate) force_large_payload: bool,
    pub(crate) paid_large_payload: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StartUploadRequest {
    pub(crate) registry_url: String,
    pub(crate) artifact: UploadArtifactDescriptor,
    pub(crate) force_large_payload: bool,
    pub(crate) paid_large_payload: bool,
}

pub(crate) struct TransferArtifactRequest {
    pub(crate) registry_url: String,
    pub(crate) session: UploadSession,
    pub(crate) artifact_bytes: Vec<u8>,
}

pub(crate) struct FinalizeUploadRequest {
    pub(crate) registry_url: String,
    #[allow(dead_code)]
    pub(crate) artifact: UploadArtifactDescriptor,
    pub(crate) transfer: TransferArtifactResponse,
    pub(crate) v3_sync_payload: Option<V3SyncPayload>,
}

#[allow(dead_code)]
pub(crate) enum UploadSession {
    Direct(direct::DirectUploadSession),
    Presigned(presigned::PresignedUploadSession),
}

#[allow(dead_code)]
pub(crate) enum TransferArtifactResponse {
    Direct(direct::DirectTransferArtifactResponse),
    Presigned(presigned::PresignedTransferArtifactResponse),
}

pub(crate) trait UploadStrategy {
    fn validate_preflight(&self, request: &UploadPreflightRequest) -> Result<()>;

    fn start_upload(&self, request: &StartUploadRequest) -> Result<UploadSession>;

    fn transfer(&self, request: TransferArtifactRequest) -> Result<TransferArtifactResponse>;

    fn finalize_upload(&self, request: FinalizeUploadRequest) -> Result<PublishArtifactResult>;
}

pub(crate) fn resolve_upload_strategy(registry_url: &str) -> Box<dyn UploadStrategy> {
    match resolve_upload_strategy_kind(registry_url).kind {
        UploadStrategyKind::Direct => Box::new(direct::DirectUploadStrategy),
        UploadStrategyKind::Presigned => Box::new(presigned::PresignedUploadStrategy),
    }
}

pub(crate) fn select_upload_strategy_kind(registry_url: &str) -> UploadStrategySelection {
    if super::artifact::is_managed_store_direct_registry(registry_url) {
        return UploadStrategySelection {
            kind: UploadStrategyKind::Direct,
            reason: UploadStrategySelectionReason::ManagedStoreHostDefaultDirect,
        };
    }

    UploadStrategySelection {
        kind: UploadStrategyKind::Direct,
        reason: UploadStrategySelectionReason::CustomRegistryDefaultDirect,
    }
}

pub(crate) fn resolve_upload_strategy_kind(registry_url: &str) -> UploadStrategySelection {
    if let Some(kind) = upload_strategy_env_override() {
        return UploadStrategySelection {
            kind,
            reason: UploadStrategySelectionReason::ExplicitEnvironmentOverride,
        };
    }

    if let Some(discovered) = discover_upload_strategy_kind(registry_url) {
        return discovered;
    }

    select_upload_strategy_kind(registry_url)
}

pub(crate) fn enforce_upload_preflight(request: &UploadPreflightRequest) -> Result<()> {
    let strategy = resolve_upload_strategy(&request.registry_url);
    strategy.validate_preflight(request)
}

fn upload_strategy_env_override() -> Option<UploadStrategyKind> {
    let raw = std::env::var(ENV_UPLOAD_STRATEGY).ok()?;
    parse_upload_strategy_kind(raw.trim())
}

fn parse_upload_strategy_kind(raw: &str) -> Option<UploadStrategyKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "direct" => Some(UploadStrategyKind::Direct),
        "presigned" => Some(UploadStrategyKind::Presigned),
        _ => None,
    }
}

fn discover_upload_strategy_kind(registry_url: &str) -> Option<UploadStrategySelection> {
    let client = crate::registry::http::blocking_client_builder(registry_url)
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}/v1/publish/capabilities", registry_url))
        .header("accept", "application/json")
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }

    let payload = response.json::<PublishCapabilitiesResponse>().ok()?;
    let default_kind = payload
        .default_upload_strategy
        .as_deref()
        .and_then(parse_upload_strategy_kind)?;
    if !payload.supported_upload_strategies.is_empty()
        && !payload
            .supported_upload_strategies
            .iter()
            .filter_map(|item| parse_upload_strategy_kind(item))
            .any(|item| item == default_kind)
    {
        return None;
    }

    Some(UploadStrategySelection {
        kind: default_kind,
        reason: UploadStrategySelectionReason::RegistryCapabilityDiscovery,
    })
}

#[derive(Debug, Deserialize)]
struct PublishCapabilitiesResponse {
    #[serde(default)]
    default_upload_strategy: Option<String>,
    #[serde(default)]
    supported_upload_strategies: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use serial_test::serial;

    #[test]
    fn selector_uses_direct_strategy_for_managed_store_hosts_today() {
        let selection = select_upload_strategy_kind("https://api.ato.run");

        assert_eq!(selection.kind, UploadStrategyKind::Direct);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::ManagedStoreHostDefaultDirect
        );
    }

    #[test]
    fn selector_uses_direct_strategy_for_custom_registry_hosts_today() {
        let selection = select_upload_strategy_kind("http://127.0.0.1:8787");

        assert_eq!(selection.kind, UploadStrategyKind::Direct);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::CustomRegistryDefaultDirect
        );
    }

    #[test]
    #[serial]
    fn resolved_selector_honors_environment_override_for_presigned_strategy() {
        std::env::set_var(ENV_UPLOAD_STRATEGY, "presigned");

        let selection = resolve_upload_strategy_kind("https://api.ato.run");

        std::env::remove_var(ENV_UPLOAD_STRATEGY);
        assert_eq!(selection.kind, UploadStrategyKind::Presigned);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::ExplicitEnvironmentOverride
        );
    }

    #[test]
    #[serial]
    fn resolved_selector_honors_environment_override_for_direct_strategy() {
        std::env::set_var(ENV_UPLOAD_STRATEGY, "direct");

        let selection = resolve_upload_strategy_kind("https://api.ato.run");

        std::env::remove_var(ENV_UPLOAD_STRATEGY);
        assert_eq!(selection.kind, UploadStrategyKind::Direct);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::ExplicitEnvironmentOverride
        );
    }

    #[test]
    fn resolved_selector_uses_registry_capability_discovery_default() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (base_url, handle) = runtime.block_on(async {
            let app = Router::new().route(
                "/v1/publish/capabilities",
                get(|| async {
                    Json(serde_json::json!({
                        "default_upload_strategy": "presigned",
                        "supported_upload_strategies": ["direct", "presigned"],
                    }))
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let handle = tokio::spawn(async move {
                axum::serve(listener, app).await.expect("serve");
            });
            (format!("http://{}", addr), handle)
        });

        let selection = resolve_upload_strategy_kind(&base_url);

        handle.abort();
        let _ = runtime.block_on(handle);
        assert_eq!(selection.kind, UploadStrategyKind::Presigned);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::RegistryCapabilityDiscovery
        );
    }

    #[test]
    fn resolved_selector_falls_back_when_capabilities_payload_is_inconsistent() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (base_url, handle) = runtime.block_on(async {
            let app = Router::new().route(
                "/v1/publish/capabilities",
                get(|| async {
                    Json(serde_json::json!({
                        "default_upload_strategy": "presigned",
                        "supported_upload_strategies": ["direct"],
                    }))
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let handle = tokio::spawn(async move {
                axum::serve(listener, app).await.expect("serve");
            });
            (format!("http://{}", addr), handle)
        });

        let selection = resolve_upload_strategy_kind(&base_url);

        handle.abort();
        let _ = runtime.block_on(handle);
        assert_eq!(selection.kind, UploadStrategyKind::Direct);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::CustomRegistryDefaultDirect
        );
    }
}
