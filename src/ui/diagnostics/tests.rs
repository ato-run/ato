use std::path::PathBuf;

use anyhow::anyhow;
use capsule_core::execution_plan::error::AtoExecutionError;

use super::{from_anyhow, CliDiagnosticCode, CommandContext, JsonErrorEnvelopeV1};

#[test]
fn maps_manifest_parse_to_e001() {
    let err = anyhow!(capsule_core::CapsuleError::Manifest(
        PathBuf::from("capsule.toml"),
        "Failed to parse manifest TOML: expected value".to_string()
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Build);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E001);
    assert_eq!(diagnostic.path.as_deref(), Some("capsule.toml"));
}

#[test]
fn maps_required_default_target_to_e003() {
    let err = anyhow!(capsule_core::CapsuleError::Manifest(
        PathBuf::from("capsule.toml"),
        "Manifest validation failed: default_target is required".to_string()
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Build);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E003);
    assert_eq!(diagnostic.field.as_deref(), Some("default_target"));
}

#[test]
fn maps_entrypoint_failure_to_e101() {
    let err = anyhow!(capsule_core::CapsuleError::Pack(
        "Entrypoint not found".to_string()
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Build);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E101);
}

#[test]
fn maps_strict_manifest_error_to_e106() {
    let err = anyhow!(
        capsule_core::CapsuleError::StrictManifestFallbackNotAllowed(
            "fallback blocked".to_string()
        )
    );
    let diagnostic = from_anyhow(&err, CommandContext::Build);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E106);
    assert_eq!(diagnostic.field.as_deref(), Some("strict-v3"));
}

#[test]
fn maps_publish_version_exists_from_error_type_to_e202() {
    let err = anyhow!(
        crate::publish_artifact::PublishArtifactError::VersionExists {
            message: "same version is already published".to_string(),
        }
    );
    let diagnostic = from_anyhow(&err, CommandContext::Publish);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E202);
}

#[test]
fn maps_execution_contract_error_to_e302() {
    let err = anyhow!(AtoExecutionError::execution_contract_invalid(
        "IPC validation failed",
        Some("services.api.readiness_probe"),
        Some("api"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E302);
    assert_eq!(
        diagnostic.field.as_deref(),
        Some("services.api.readiness_probe")
    );
    assert!(diagnostic.details.is_some());
}

#[test]
fn maps_security_policy_error_to_e301() {
    let err = anyhow!(AtoExecutionError::security_policy_violation(
        "network policy violation: blocked egress to example.com",
        Some("network"),
        Some("example.com"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E301);
    assert_eq!(diagnostic.phase, "execution");
}

#[test]
fn maps_manual_intervention_execution_error_to_e102() {
    let err = anyhow!(AtoExecutionError::manual_intervention_required(
        "manual intervention required: DATABASE_URL is required\nGenerated capsule.toml: /repo/.tmp/capsule.toml",
        Some("/repo/.tmp/capsule.toml"),
        vec![
            "Set DATABASE_URL before rerunning.".to_string(),
            "Review the generated capsule.toml.".to_string(),
        ],
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E102);
    assert!(diagnostic.interactive_resolution);
}

#[test]
fn maps_missing_required_env_error_to_e103() {
    let err = anyhow!(AtoExecutionError::missing_required_env(
        "missing required environment variables for target 'default': DATABASE_URL",
        vec!["DATABASE_URL".to_string()],
        Some("default"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E103);
    assert!(diagnostic.interactive_resolution);
}

#[test]
fn maps_ambiguous_entrypoint_error_to_e105() {
    let err = anyhow!(AtoExecutionError::ambiguous_entrypoint(
        "ambiguous entrypoint detected",
        vec!["main.py".to_string(), "src/main.py".to_string()],
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Build);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E105);
    assert!(diagnostic.interactive_resolution);
}

#[test]
fn json_envelope_has_status_error() {
    let err = anyhow!(AtoExecutionError::runtime_not_resolved(
        "deno runtime is not resolved",
        Some("deno"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    let envelope: JsonErrorEnvelopeV1 = diagnostic.to_json_envelope();
    assert_eq!(envelope.schema_version, "1");
    assert_eq!(envelope.status, "error");
    assert_eq!(envelope.error.code, CliDiagnosticCode::E303);
    assert_eq!(envelope.error.name, "runtime_not_resolved");
}

#[test]
fn maps_ingress_tls_bootstrap_required_to_e209() {
    let err = anyhow!(
        "ingress TLS bootstrap required for binding 'binding-demo'. Run `ato binding bootstrap-tls --binding binding-demo` first."
    );
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E209);
    assert!(diagnostic
        .hint
        .as_deref()
        .unwrap_or_default()
        .contains("bootstrap-tls"));
}
