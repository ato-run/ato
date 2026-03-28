use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootstrapSubjectKind {
    Runtime,
    Tool,
    Engine,
    FinalizeHelper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootstrapAuthorityKind {
    HostCapability,
    LockedArtifact,
    NetworkBootstrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootstrapVerificationKind {
    HostTrust,
    ChecksumRequired,
    ChecksumUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootstrapClosureRole {
    HostCapability,
    LockedClosureInput,
    BuildEnvironmentClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootstrapCacheScope {
    None,
    RuntimeCache,
    ToolchainCache,
    EngineCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkBootstrapPolicy {
    pub network_allowed: bool,
    pub disabled_reason: Option<String>,
}

impl NetworkBootstrapPolicy {
    pub fn allowed() -> Self {
        Self {
            network_allowed: true,
            disabled_reason: None,
        }
    }

    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            network_allowed: false,
            disabled_reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapBoundary {
    pub subject_kind: BootstrapSubjectKind,
    pub subject_name: String,
    pub authority_kind: BootstrapAuthorityKind,
    pub verification_kind: BootstrapVerificationKind,
    pub closure_role: BootstrapClosureRole,
    pub cache_scope: BootstrapCacheScope,
    pub network_policy: NetworkBootstrapPolicy,
}

impl BootstrapBoundary {
    pub fn locked_runtime(name: &str) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::Runtime,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::LockedArtifact,
            verification_kind: BootstrapVerificationKind::ChecksumRequired,
            closure_role: BootstrapClosureRole::LockedClosureInput,
            cache_scope: BootstrapCacheScope::RuntimeCache,
            network_policy: NetworkBootstrapPolicy::allowed(),
        }
    }

    pub fn host_runtime(name: &str) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::Runtime,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::HostCapability,
            verification_kind: BootstrapVerificationKind::HostTrust,
            closure_role: BootstrapClosureRole::HostCapability,
            cache_scope: BootstrapCacheScope::None,
            network_policy: NetworkBootstrapPolicy::disabled(
                "authoritative lock execution requires a host runtime",
            ),
        }
    }

    pub fn host_tool(name: &str) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::Tool,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::HostCapability,
            verification_kind: BootstrapVerificationKind::HostTrust,
            closure_role: BootstrapClosureRole::HostCapability,
            cache_scope: BootstrapCacheScope::None,
            network_policy: NetworkBootstrapPolicy::disabled(
                "authoritative lock execution requires a host tool",
            ),
        }
    }

    pub fn network_tool(name: &str, verification_kind: BootstrapVerificationKind) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::Tool,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::NetworkBootstrap,
            verification_kind,
            closure_role: BootstrapClosureRole::HostCapability,
            cache_scope: BootstrapCacheScope::ToolchainCache,
            network_policy: NetworkBootstrapPolicy::allowed(),
        }
    }

    pub fn engine(name: &str, network_policy: NetworkBootstrapPolicy) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::Engine,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::NetworkBootstrap,
            verification_kind: BootstrapVerificationKind::ChecksumRequired,
            closure_role: BootstrapClosureRole::HostCapability,
            cache_scope: BootstrapCacheScope::EngineCache,
            network_policy,
        }
    }

    pub fn finalize_helper(name: &str) -> Self {
        Self {
            subject_kind: BootstrapSubjectKind::FinalizeHelper,
            subject_name: name.to_string(),
            authority_kind: BootstrapAuthorityKind::HostCapability,
            verification_kind: BootstrapVerificationKind::HostTrust,
            closure_role: BootstrapClosureRole::BuildEnvironmentClaim,
            cache_scope: BootstrapCacheScope::None,
            network_policy: NetworkBootstrapPolicy::disabled(
                "finalize helpers are host-local capabilities",
            ),
        }
    }

    pub fn missing_on_path_message(&self) -> String {
        let subject = match self.subject_kind {
            BootstrapSubjectKind::Runtime => "runtime",
            BootstrapSubjectKind::Tool => "tool",
            BootstrapSubjectKind::Engine => "engine",
            BootstrapSubjectKind::FinalizeHelper => "finalize helper",
        };
        format!(
            "lock-derived source execution requires a host-local '{}' {} on PATH",
            self.subject_name, subject
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_helper_is_treated_as_build_environment_claim() {
        let boundary = BootstrapBoundary::finalize_helper("codesign");
        assert_eq!(boundary.subject_kind, BootstrapSubjectKind::FinalizeHelper);
        assert_eq!(
            boundary.authority_kind,
            BootstrapAuthorityKind::HostCapability
        );
        assert_eq!(
            boundary.closure_role,
            BootstrapClosureRole::BuildEnvironmentClaim
        );
        assert!(!boundary.network_policy.network_allowed);
    }

    #[test]
    fn locked_runtime_requires_checksum_and_runtime_cache() {
        let boundary = BootstrapBoundary::locked_runtime("node");
        assert_eq!(
            boundary.authority_kind,
            BootstrapAuthorityKind::LockedArtifact
        );
        assert_eq!(
            boundary.verification_kind,
            BootstrapVerificationKind::ChecksumRequired
        );
        assert_eq!(boundary.cache_scope, BootstrapCacheScope::RuntimeCache);
        assert!(boundary.network_policy.network_allowed);
    }
}
