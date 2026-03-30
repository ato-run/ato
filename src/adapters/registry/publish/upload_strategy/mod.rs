use anyhow::Result;

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

pub(crate) fn select_upload_strategy(registry_url: &str) -> Box<dyn UploadStrategy> {
    match select_upload_strategy_kind(registry_url).kind {
        UploadStrategyKind::Direct => Box::new(direct::DirectUploadStrategy),
        UploadStrategyKind::Presigned => Box::new(presigned::PresignedUploadStrategy),
    }
}

pub(crate) fn select_upload_strategy_kind(registry_url: &str) -> UploadStrategySelection {
    if let Some(kind) = upload_strategy_env_override() {
        return UploadStrategySelection {
            kind,
            reason: UploadStrategySelectionReason::ExplicitEnvironmentOverride,
        };
    }

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

pub(crate) fn enforce_upload_preflight(request: &UploadPreflightRequest) -> Result<()> {
    let strategy = select_upload_strategy(&request.registry_url);
    strategy.validate_preflight(request)
}

fn upload_strategy_env_override() -> Option<UploadStrategyKind> {
    let raw = std::env::var(ENV_UPLOAD_STRATEGY).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "direct" => Some(UploadStrategyKind::Direct),
        "presigned" => Some(UploadStrategyKind::Presigned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn selector_honors_environment_override_for_presigned_strategy() {
        std::env::set_var(ENV_UPLOAD_STRATEGY, "presigned");

        let selection = select_upload_strategy_kind("https://api.ato.run");

        std::env::remove_var(ENV_UPLOAD_STRATEGY);
        assert_eq!(selection.kind, UploadStrategyKind::Presigned);
        assert_eq!(
            selection.reason,
            UploadStrategySelectionReason::ExplicitEnvironmentOverride
        );
    }
}
