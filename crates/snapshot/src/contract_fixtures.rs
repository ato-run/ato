//! Shared `#[cfg(test)]` builders for a valid G0-1 [`ExecutionContractV1`] and its
//! non-identity [`ExecutionContractEnvelopeV1`].
//!
//! The capsule crate's own sample builder is `pub(in crate::contract)` and so is
//! unreachable here; rather than each snapshot test module hand-rolling a full
//! contract, they share these. The contract mirrors the capsule crate's canonical
//! sample so it passes [`ExecutionContractV1::validate`] and hashes to a stable
//! `execution_id`. Used by both the `external_state` requirement analysis tests
//! and the `acceptance` eligibility-constructor tests.

use std::num::NonZeroU16;

use capsule::execution_contract::{
    ContentDigest, DigestAlgorithm, EnvironmentVariableContract, ExecutionContractEnvelopeV1,
    ExecutionContractV1, ExecutionId, ExternalStateAccess, ExternalStateContract, GuestPath,
    GuestSurfaceContract, OpaqueContractDigestV1, ResolvedArtifactContract,
    ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
    ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract,
    SnapshotExclusion,
};
use capsule::snapshot_manifest::{
    CapturePolicyV1, PortabilityTier, RestoreContractV1, SNAPSHOT_COMPATIBILITY_V1_SCHEMA,
    SNAPSHOT_MANIFEST_V1_SCHEMA, SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA,
    SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA, SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA,
    SanitizationAttestationV1, SecretScanAttestationV1, SnapshotBackendKind,
    SnapshotCaptureProvenance, SnapshotCompatibilityContractV1, SnapshotManifestV1,
};

const EXECUTION_CONTRACT_V1_SCHEMA: &str = "ato.execution-contract/v1";

fn digest(byte: u8) -> ContentDigest {
    ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32])
}

fn opaque(byte: u8) -> OpaqueContractDigestV1 {
    OpaqueContractDigestV1::new([byte; 32])
}

fn path(value: &str) -> GuestPath {
    GuestPath::parse(value).expect("canonical guest path")
}

/// A valid G0-1 execution contract declaring exactly one `snapshot = "exclude"`
/// External State binding (`data` at `/data`, schema `"1"`, read-write). Callers
/// mutate `external_state` to exercise the requirement analysis.
pub(crate) fn sample_execution_contract() -> ExecutionContractV1 {
    ExecutionContractV1 {
        schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
        source: ResolvedSourceContract {
            digest: digest(1),
            projection_digest: opaque(0x0c),
        },
        target: ResolvedTargetContract {
            os: "linux".to_string(),
            architecture: "x86_64".to_string(),
            abi: "gnu".to_string(),
            libc: Some("glibc-2.39".to_string()),
            observable_features: std::collections::BTreeMap::new(),
        },
        runtime: ResolvedArtifactContract {
            kind: "node".to_string(),
            digest: digest(2),
            dynamic_contract_digest: opaque(0x0d),
        },
        dependencies: vec![ResolvedDependencyContract {
            name: "npm".to_string(),
            derivation_digest: digest(3),
            output_digest: digest(4),
        }],
        build_outputs: vec![ResolvedBuildOutputContract {
            name: "app".to_string(),
            digest: digest(5),
            projection_digest: opaque(0x0e),
        }],
        launch: ResolvedLaunchContract {
            argv: vec!["node".to_string(), "dist/server.js".to_string()],
            cwd: path("/workspace"),
            process_model_digest: opaque(0x0f),
            environment: vec![EnvironmentVariableContract {
                name: "NODE_ENV".to_string(),
                value_digest: opaque(6),
            }],
            environment_policy_digest: opaque(0x10),
            secret_bindings: vec!["API_TOKEN".to_string()],
        },
        filesystem: ResolvedFilesystemContract {
            view_digest: digest(7),
            topology_digest: opaque(0x11),
            readonly_layers: vec![digest(8)],
            writable_paths: vec![path("/tmp")],
        },
        policy: ResolvedPolicyContract {
            network_digest: opaque(9),
            capability_digest: opaque(10),
            filesystem_digest: opaque(11),
        },
        guest_surface: GuestSurfaceContract {
            bind_address: "0.0.0.0".to_string(),
            protocol: "ato-guest/v1".to_string(),
            port: Some(NonZeroU16::new(8080).unwrap()),
            features: vec!["bindings".to_string(), "exec".to_string()],
        },
        external_state: vec![ExternalStateContract {
            name: "data".to_string(),
            target: path("/data"),
            access: ExternalStateAccess::ReadWrite,
            schema: "1".to_string(),
            snapshot: SnapshotExclusion::Exclude,
        }],
    }
}

/// A valid `running` [`SnapshotManifestV1`] with single, distinct memory /
/// vmstate / disk layer addresses (`0x11` / `0x22` / `0x33`) so the exclusion
/// scanner can be exercised against each shared layer independently.
pub(crate) fn sample_snapshot_manifest() -> SnapshotManifestV1 {
    SnapshotManifestV1 {
        schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
        execution_id: ExecutionId::new(format!("blake3:{}", "a".repeat(64)))
            .expect("valid execution id"),
        compatibility_contract: SnapshotCompatibilityContractV1 {
            schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
            backend: SnapshotBackendKind::Firecracker,
            format_version: 2,
            vmm_identity: "firecracker-1.7".to_string(),
            state_codec: "fc-state/v2".to_string(),
            guest_kernel_identity: "vmlinux-6.1-ato".to_string(),
            cpu_template: "T2CL".to_string(),
            runner_restore_contract: "ato-restore/v1".to_string(),
            portability_tier: PortabilityTier::ClassPortable,
            compatibility_class_identity: digest(0xcc),
        },
        memory_layer_refs: vec![digest(0x11)],
        vmstate_layer_refs: vec![digest(0x22)],
        disk_layer_refs: vec![digest(0x33)],
        restore_contract: RestoreContractV1 {
            schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
            restore_protocol: "ato-restore/v1".to_string(),
            steps: vec!["network_reconnect".to_string()],
        },
        capture_policy: CapturePolicyV1::Running,
        capture_provenance: SnapshotCaptureProvenance::default(),
        sanitization_attestation: SanitizationAttestationV1 {
            schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
            steps: vec!["session_id_regenerate".to_string()],
        },
        secret_scan_attestation: SecretScanAttestationV1 {
            schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
            scanner_identity: "ato-secret-scan/1.0".to_string(),
            policy_identity: "default/v1".to_string(),
            scanned_layers: vec!["memory".to_string(), "vmstate".to_string()],
            verdict: "clean".to_string(),
        },
    }
}

/// Wrap `contract` in a non-identity envelope whose stored `execution_id` is the
/// contract's canonical hash — so [`ExecutionContractEnvelopeV1::verify`] and
/// [`ExecutionContractEnvelopeV1::verified_execution_id`] both succeed.
pub(crate) fn envelope_for(contract: ExecutionContractV1) -> ExecutionContractEnvelopeV1 {
    let execution_id = contract
        .compute_execution_id()
        .expect("valid contract hashes");
    ExecutionContractEnvelopeV1 {
        execution_contract: contract,
        execution_id,
        capsule_program_id: None,
        resolved_refs: Default::default(),
        generated_at: None,
        provenance: serde_json::Value::Null,
        diagnostics: serde_json::Value::Null,
        evidence: serde_json::Value::Null,
    }
}
