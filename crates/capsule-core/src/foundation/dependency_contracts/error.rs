//! Errors raised by the lock-time verifier defined in
//! `CAPSULE_DEPENDENCY_CONTRACTS.md` §9.1. Each variant maps to a numbered
//! verification rule so that `Display` output references the RFC directly.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LockError {
    #[error("[v9.1.3] dependency '{dep}' requested contract '{contract}' but provider does not declare it")]
    ContractNotFound { dep: String, contract: String },

    #[error(
        "[v9.1.4] dependency '{dep}' contract '{contract}' binds target '{target}' that does not exist in provider"
    )]
    TargetNotFound {
        dep: String,
        contract: String,
        target: String,
    },

    #[error(
        "[v9.1.5] dependency '{dep}' parameter '{key}' has wrong type: expected {expected:?}, got {actual:?}"
    )]
    ParameterTypeMismatch {
        dep: String,
        key: String,
        expected: String,
        actual: String,
    },

    #[error("[v9.1.5] dependency '{dep}' is missing required parameter '{key}'")]
    ParameterRequired { dep: String, key: String },

    #[error(
        "[v9.1.5] dependency '{dep}' declares unknown parameter '{key}' (not in provider contract)"
    )]
    ParameterUnknown { dep: String, key: String },

    #[error("[v9.1.6] dependency '{dep}' is missing required credential '{key}'")]
    CredentialRequired { dep: String, key: String },

    #[error("[v9.1.6] dependency '{dep}' declares unknown credential '{key}' (not in provider contract)")]
    CredentialUnknown { dep: String, key: String },

    #[error(
        "[v9.1.6/inv5] dependency '{dep}' credential '{key}' must be a `{{{{env.X}}}}` template; literal values are forbidden"
    )]
    CredentialLiteralForbidden { dep: String, key: String },

    #[error(
        "[v9.1.6/inv9] {scope}.credentials.{key} must not declare a default value (Safe by default)"
    )]
    CredentialDefaultForbidden { scope: String, key: String },

    #[error(
        "[v9.1.6/inv6] dependency '{dep}' credential '{key}' references {{{{env.{env_key}}}}} but '{env_key}' is not in manifest top-level required_env"
    )]
    CredentialEnvKeyOutOfScope {
        dep: String,
        key: String,
        env_key: String,
    },

    #[error(
        "[v9.1.6/inv6] dependency '{dep}' parameter '{key}' references {{{{env.{env_key}}}}} but '{env_key}' is not in manifest top-level required_env"
    )]
    ParameterEnvKeyOutOfScope {
        dep: String,
        key: String,
        env_key: String,
    },

    #[error(
        "[v9.1.7/inv4] contract '{contract}' identity_exports.{key} contains {{{{credentials.X}}}}; identity must not depend on credentials"
    )]
    IdentityExportContainsCredential { contract: String, key: String },

    #[error("[v9.1.8] dependency '{dep}' provider requires state but consumer did not specify [dependencies.{dep}.state] name")]
    StateRequiredButMissing { dep: String },

    #[error("[v9.1.8] dependency '{dep}' state.ownership = \"shared\" is not implemented in v1; only \"parent\" is allowed")]
    StateOwnershipShared { dep: String },

    #[error("[v9.1.8] contract '{contract}' has state.required = true but does not declare state.version")]
    StateVersionMissing { contract: String },

    #[error(
        "[v9.1.9] target '{target}' lists need '{name}' that is not declared in [dependencies.*]"
    )]
    NeedsNotInDependencies { target: String, name: String },

    #[error("[v9.1.10] dependency graph cycle detected: {path}")]
    CycleDetected { path: String },

    #[error(
        "[v9.1.11] capsule '{capsule_source}' appears with multiple major versions in the same graph: {majors:?}"
    )]
    MajorVersionConflict {
        capsule_source: String,
        majors: Vec<String>,
    },

    #[error(
        "[v9.1.12] dependencies '{a}' and '{b}' resolve to the same instance hash (resolved={resolved}, contract={contract}); v1 forbids two aliases pointing at the same instance"
    )]
    InstanceUniquenessViolation {
        a: String,
        b: String,
        resolved: String,
        contract: String,
    },

    #[error("[v9.1.13] dependency '{dep}' provider target uses unix_socket = \"auto\" which is reserved-only in v1 (lock fail-closed)")]
    ReservedVariantUnixSocketEndpoint { dep: String },

    #[error(
        "[v9.1.13] contract '{contract}' uses ready.type = \"{variant}\" which is reserved-only in v1 (lock fail-closed)"
    )]
    ReservedVariantReadyProbe { contract: String, variant: String },

    #[error("dependency '{dep}' references unknown provider entry: {detail}")]
    ProviderMissing { dep: String, detail: String },

    #[error("internal: failed to canonicalize instance hash input for '{dep}': {detail}")]
    InternalHashFailure { dep: String, detail: String },
}
