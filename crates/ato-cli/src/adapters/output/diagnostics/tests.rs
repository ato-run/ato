use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use capsule_core::execution_plan::error::{
    AtoErrorClassification, AtoExecutionError, CleanupActionRecord, CleanupActionStatus,
    CleanupStatus, ManifestSuggestion,
};

use super::{from_anyhow, CliDiagnosticCode, CommandContext, JsonErrorEnvelopeV1};

fn assert_json_envelope_snapshot(name: &str, envelope: &JsonErrorEnvelopeV1) {
    let snapshot_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/adapters/output/diagnostics/snapshots")
        .join(format!("{name}.json"));
    let expected = fs::read_to_string(&snapshot_path).expect("snapshot fixture should be readable");
    let actual = serde_json::to_string_pretty(envelope).expect("json envelope should serialize")
        + "
";
    assert_eq!(
        actual,
        expected,
        "snapshot mismatch: {}",
        snapshot_path.display()
    );
}

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
fn maps_managed_store_large_payload_override_policy_to_e212() {
    let err = anyhow!(
        crate::publish_artifact::PublishArtifactError::ManagedStoreLargePayloadOverrideUnsupported {
            registry_url: "https://api.ato.run".to_string(),
            message: "--force-large-payload cannot be used with the managed Store direct upload path".to_string(),
        }
    );
    let diagnostic = from_anyhow(&err, CommandContext::Publish);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E212);
    assert!(diagnostic
        .hint
        .as_deref()
        .unwrap_or_default()
        .contains("large payload override"));
}

#[test]
fn maps_managed_store_conservative_preflight_limit_to_e212() {
    let err = anyhow!(
        crate::publish_artifact::PublishArtifactError::ManagedStoreDirectPayloadLimitExceeded {
            registry_url: "https://api.ato.run".to_string(),
            size_bytes: 187_371_520,
            limit_bytes: crate::publish_artifact::MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES,
        }
    );
    let diagnostic = from_anyhow(&err, CommandContext::Publish);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E212);
    assert!(diagnostic.message.contains("conservative preflight limit"));
    assert!(diagnostic.message.contains("187371520"));
}

#[test]
fn maps_publish_payload_too_large_to_e212() {
    let err = anyhow!(crate::publish_artifact::PublishArtifactError::PayloadTooLarge {
        status: 413,
        message: "managed Store direct upload rejected the request body as too large at the edge before the registry accepted it".to_string(),
    });
    let diagnostic = from_anyhow(&err, CommandContext::Publish);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E212);
    assert!(diagnostic.message.contains("too large"));
}

#[test]
fn maps_missing_distributable_artifact_to_e102_with_specific_message() {
    let err = anyhow!("Failed to build artifact for publish").context(
        "Native delivery build input is not a .app directory: dist/sample-project.app\nFound nearby .app bundle candidates: dist/mac-arm64/sample-project.app\nHint: update [artifact].input to the correct path.",
    );

    let diagnostic = from_anyhow(&err, CommandContext::Publish);

    assert_eq!(diagnostic.code, CliDiagnosticCode::E102);
    assert!(diagnostic
        .message
        .contains("Native delivery build input is not a .app directory"));
    assert!(diagnostic
        .message
        .contains("dist/mac-arm64/sample-project.app"));
    assert_eq!(
        diagnostic.field.as_deref(),
        Some("contract.delivery.artifact.path")
    );
    assert!(diagnostic
        .hint
        .as_deref()
        .unwrap_or_default()
        .contains("配布可能な成果物が見つかりません"));
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
        "manual intervention required: DATABASE_URL is required
Generated capsule.toml: /repo/.ato/capsule.toml",
        Some("/repo/.ato/capsule.toml"),
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
        Vec::new(),
        Some("default"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.code, CliDiagnosticCode::E103);
    assert!(diagnostic.interactive_resolution);
}

/// Wire-contract test for the E103 envelope consumed by `ato-desktop`. Pins
/// the JSON shape so a future field rename (`missing_schema` → anything else)
/// is caught in CI before the desktop loses its dynamic-config flow.
///
/// Invariants asserted here:
/// 1. `details.missing_keys` and `details.missing_schema` are both present
///    and **index-aligned** (`missing_schema[i].name == missing_keys[i]`).
/// 2. `ConfigKind` flattens into the field object — `kind = "secret"` sits
///    next to `placeholder`, `kind = "enum"` sits next to `choices`.
/// 3. Optional fields (`label`, `description`, `default`, `placeholder`)
///    are omitted when `None`, never serialized as `null`.
#[test]
fn maps_missing_required_env_error_to_e103_with_schema() {
    use capsule_core::types::{ConfigField, ConfigKind};

    let missing_schema = vec![
        ConfigField {
            name: "OPENAI_API_KEY".to_string(),
            label: Some("OpenAI API Key".to_string()),
            description: Some("Your OpenAI API key".to_string()),
            kind: ConfigKind::Secret,
            default: None,
            placeholder: Some("sk-...".to_string()),
        },
        ConfigField {
            name: "MODEL".to_string(),
            label: None,
            description: None,
            kind: ConfigKind::Enum {
                choices: vec!["gpt-4".to_string(), "gpt-5".to_string()],
            },
            default: Some("gpt-4".to_string()),
            placeholder: None,
        },
    ];
    let err = anyhow!(AtoExecutionError::missing_required_env(
        "missing required environment variables for target 'main': OPENAI_API_KEY, MODEL",
        vec!["OPENAI_API_KEY".to_string(), "MODEL".to_string()],
        missing_schema,
        Some("main"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    let envelope = diagnostic.to_json_envelope();
    let actual = serde_json::to_value(&envelope).expect("envelope must serialize to JSON value");

    let expected = serde_json::json!({
        "schema_version": "1",
        "status": "error",
        "error": {
            "code": "E103",
            "name": "missing_required_env",
            "phase": "inference",
            "classification": "manifest",
            "message": "missing required environment variables for target 'main': OPENAI_API_KEY, MODEL",
            "hint": "必要な環境変数を設定してから再実行してください。",
            "retryable": false,
            "interactive_resolution": true,
            "causes": [],
            "details": {
                "missing_keys": ["OPENAI_API_KEY", "MODEL"],
                "missing_schema": [
                    {
                        "name": "OPENAI_API_KEY",
                        "label": "OpenAI API Key",
                        "description": "Your OpenAI API key",
                        "kind": "secret",
                        "placeholder": "sk-..."
                    },
                    {
                        "name": "MODEL",
                        "kind": "enum",
                        "choices": ["gpt-4", "gpt-5"],
                        "default": "gpt-4"
                    }
                ],
                "target": "main"
            }
        }
    });

    assert_eq!(
        actual, expected,
        "E103 envelope wire contract drifted — desktop dynamic-config UI depends on this exact shape.\nactual: {}\nexpected: {}",
        serde_json::to_string_pretty(&actual).unwrap(),
        serde_json::to_string_pretty(&expected).unwrap()
    );
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
    assert_eq!(
        envelope.error.classification,
        AtoErrorClassification::Execution
    );
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

#[test]
fn preserves_execution_error_cleanup_and_manifest_metadata() {
    let err = anyhow!(AtoExecutionError::execution_contract_invalid(
        "services.main is required",
        Some("services.main"),
        Some("main"),
    )
    .with_classification(AtoErrorClassification::Manifest)
    .with_cleanup(
        CleanupStatus::Partial,
        vec![CleanupActionRecord {
            action: "remove_temp_dir".to_string(),
            status: CleanupActionStatus::Failed,
            detail: Some("permission denied".to_string()),
        }],
    )
    .with_manifest_suggestion(ManifestSuggestion {
        kind: "create_table".to_string(),
        path: "services.main".to_string(),
        operation: "create_table".to_string(),
        value: None,
        message: "Add a [services.main] table".to_string(),
    }));

    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_eq!(diagnostic.classification, AtoErrorClassification::Manifest);
    assert_eq!(diagnostic.cleanup_status, Some(CleanupStatus::Partial));
    assert_eq!(diagnostic.cleanup_actions.len(), 1);
    assert_eq!(
        diagnostic
            .manifest_suggestion
            .as_ref()
            .map(|value| value.path.as_str()),
        Some("services.main")
    );
}

#[test]
fn json_envelope_snapshot_execution_runtime_not_resolved() {
    let err = anyhow!(AtoExecutionError::runtime_not_resolved(
        "deno runtime is not resolved",
        Some("deno"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_json_envelope_snapshot(
        "execution_runtime_not_resolved",
        &diagnostic.to_json_envelope(),
    );
}

#[test]
fn json_envelope_snapshot_provisioning_engine_missing() {
    let err = anyhow!(AtoExecutionError::engine_missing(
        "nacelle engine is not installed",
        Some("nacelle"),
    ));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_json_envelope_snapshot(
        "provisioning_engine_missing",
        &diagnostic.to_json_envelope(),
    );
}

#[test]
fn json_envelope_snapshot_manifest_cleanup_enriched() {
    let err = anyhow!(AtoExecutionError::execution_contract_invalid(
        "services.main is required",
        Some("services.main"),
        Some("main"),
    )
    .with_classification(AtoErrorClassification::Manifest)
    .with_cleanup(
        CleanupStatus::Partial,
        vec![CleanupActionRecord {
            action: "remove_temp_dir".to_string(),
            status: CleanupActionStatus::Failed,
            detail: Some("permission denied".to_string()),
        }],
    )
    .with_manifest_suggestion(ManifestSuggestion {
        kind: "create_table".to_string(),
        path: "services.main".to_string(),
        operation: "create_table".to_string(),
        value: None,
        message: "Add a [services.main] table".to_string(),
    }));
    let diagnostic = from_anyhow(&err, CommandContext::Run);
    assert_json_envelope_snapshot("manifest_cleanup_enriched", &diagnostic.to_json_envelope());
}

#[test]
fn json_envelope_snapshot_internal_fallback() {
    let err = anyhow!("unexpected failure");
    let diagnostic = from_anyhow(&err, CommandContext::Other);
    assert_json_envelope_snapshot("internal_fallback", &diagnostic.to_json_envelope());
}
