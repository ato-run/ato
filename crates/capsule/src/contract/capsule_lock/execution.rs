//! Read-path verification for the G0-2 execution-contract lock integration.
//!
//! When a lock carries the optional [`ExecutionContractEnvelopeV1`] (D2) and/or
//! the persisted non-secret environment value payloads (D5), a reader MUST
//! re-derive and check them before trusting them:
//!
//! * [`verify_execution_envelope`] recomputes `execution_id` from the embedded
//!   identity-bearing contract ([`ExecutionContractEnvelopeV1::verify`]) and
//!   rejects a mismatch — a tampered stored id is terminal for the reader.
//! * [`verify_environment_values`] re-derives each persisted value's digest
//!   under domain `ato.environment-value/v1` from its stored payload, rejects a
//!   mismatch, rejects a secret-bearing name, and — when the envelope is also
//!   present — requires every persisted value to be committed by the execution
//!   identity (a D5 name the identity never committed is unvouched and rejected)
//!   and cross-checks its digest against the contract's
//!   `launch.environment[name].value_digest`.
//!
//! [`verify_lock_execution`] runs both. When a lock also (or instead) carries
//! the ADR-014 `program_identity` envelope, [`verify_lock_program_identity`]
//! interprets the four lock states (§5) and verifies the declaration hash and
//! the execution envelope's parent claim fail-closed. Wiring this into the
//! launch re-read (`session.rs`) and OCI runners is deferred to a later PR;
//! this is the pure verification the reader calls.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::capsule_lock::schema::{CapsuleLock, LockEnvironmentValue};
use crate::capsule_program_contract::{CapsuleProgramLinkError, verify_program_parent};
use crate::execution_contract::{ExecutionContractError, OpaqueContractDigestV1};
use crate::execution_contract_finalize::{environment_value_digest, is_sensitive_env_key};

/// Terminal read-path rejections. Any variant means the stored execution data
/// must not be trusted or republished.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LockExecutionError {
    /// The embedded envelope's stored `execution_id` did not match the canonical
    /// hash of its identity-bearing contract.
    #[error("execution_contract envelope verification failed: {0}")]
    Envelope(ExecutionContractError),
    /// A persisted env value's stored `value_digest` string was not a valid
    /// `blake3:<hex>` opaque digest.
    #[error("launch.environment[{name}].value_digest is malformed: {source}")]
    MalformedValueDigest {
        name: String,
        source: ExecutionContractError,
    },
    /// A persisted env value's re-derived digest did not match its stored one.
    #[error(
        "launch.environment[{name}].value_digest does not match the re-derived digest of the \
         stored value payload"
    )]
    ValueDigestMismatch { name: String },
    /// A persisted env value cross-checked against the envelope contract but the
    /// envelope commits a different digest for that variable.
    #[error(
        "launch.environment[{name}].value_digest disagrees with the execution contract's \
         committed environment value digest"
    )]
    EnvelopeValueDigestMismatch { name: String },
    /// A persisted (D5) env value's name is not committed by the envelope's
    /// execution identity. When the envelope is present it IS the execution
    /// identity, so every persisted non-secret value must be vouched for by a
    /// committed `launch.environment` entry; an uncommitted name is unvouched.
    #[error(
        "launch.environment[{name}] is persisted but is not committed by the execution \
         contract identity; a persisted non-secret value must be vouched for by the \
         committed execution identity"
    )]
    EnvelopeUncommittedValue { name: String },
    /// A committed (identity) non-secret env value was not persisted. When the
    /// envelope commits a non-empty environment it IS the authoritative env set,
    /// so every committed name MUST be present in D5 (D5 ⊇ committed); a committed
    /// name absent from D5 is an incomplete persisted environment.
    #[error(
        "launch.environment[{name}] is committed by the execution contract identity but is \
         not persisted; a committed non-secret environment must be persisted in full"
    )]
    EnvelopeMissingCommittedValue { name: String },
    /// The persisted `launch.environment` is not in canonical order: names must
    /// be strictly increasing (sorted and duplicate-free).
    #[error("launch.environment names must be sorted and unique")]
    NonCanonicalEnvironment,
    /// A persisted env value's name is bound as a secret via the envelope's
    /// `secret_bindings`. `secret_bindings` is the authoritative secret set, so
    /// this catches names the heuristic misses (e.g. `DATABASE_URL`).
    #[error(
        "launch.environment[{name}] is bound as a secret via the execution contract's \
         secret_bindings and must never be persisted as a non-secret value"
    )]
    SecretBoundValuePersisted { name: String },
    /// A persisted env value carried a secret-bearing name; secret values must
    /// never be persisted as non-secret values (RFC §4.3).
    #[error(
        "launch.environment[{0}] is secret-bearing and must never be persisted as a non-secret \
         value"
    )]
    SecretValuePersisted(String),
}

/// Verify the embedded execution-contract envelope (if present) by recomputing
/// `execution_id`. Absent envelope ⇒ `Ok`.
pub fn verify_execution_envelope(lock: &CapsuleLock) -> Result<(), LockExecutionError> {
    if let Some(envelope) = &lock.execution_contract {
        envelope.verify().map_err(LockExecutionError::Envelope)?;
    }
    Ok(())
}

/// Verify every persisted non-secret env value (if any) by re-deriving its
/// `value_digest`, rejecting secret names, and — when the envelope is present —
/// requiring each persisted name to be committed by the execution identity and
/// cross-checking its digest against the committed value digest.
pub fn verify_environment_values(lock: &CapsuleLock) -> Result<(), LockExecutionError> {
    let persisted: &[LockEnvironmentValue] = lock
        .launch
        .as_ref()
        .map(|launch| launch.environment.as_slice())
        .unwrap_or(&[]);

    // Persisted D5 names must be canonical: strictly increasing rejects both
    // unsorted and duplicate names in one pass.
    if persisted
        .windows(2)
        .any(|pair| pair[0].name >= pair[1].name)
    {
        return Err(LockExecutionError::NonCanonicalEnvironment);
    }

    // The committed (identity) launch, if an envelope is present. When present it
    // IS the authoritative environment set.
    let committed = lock
        .execution_contract
        .as_ref()
        .map(|envelope| &envelope.execution_contract.launch);

    for entry in persisted {
        if is_sensitive_env_key(&entry.name) {
            return Err(LockExecutionError::SecretValuePersisted(entry.name.clone()));
        }

        let stored =
            OpaqueContractDigestV1::try_from(entry.value_digest.clone()).map_err(|source| {
                LockExecutionError::MalformedValueDigest {
                    name: entry.name.clone(),
                    source,
                }
            })?;

        let rederived = environment_value_digest(&entry.value).map_err(|source| {
            LockExecutionError::MalformedValueDigest {
                name: entry.name.clone(),
                source,
            }
        })?;

        if rederived != stored {
            return Err(LockExecutionError::ValueDigestMismatch {
                name: entry.name.clone(),
            });
        }

        // Cross-check against the envelope contract's committed environment.
        //
        // When the envelope is present it IS the execution identity (RFC §4.5,
        // round-2 measured-facts-only model): every persisted (D5) non-secret
        // value MUST be vouched for by a committed `launch.environment` entry AND
        // its digest MUST agree with the committed one. Combined with the
        // completeness check below (committed ⊆ persisted) this pins set equality.
        if let Some(launch) = committed {
            // `secret_bindings` is the authoritative secret set: a persisted D5
            // value whose name is bound as a secret must never appear here, even
            // when the name heuristic does not flag it (e.g. `DATABASE_URL`).
            if launch
                .secret_bindings
                .iter()
                .any(|binding| binding == &entry.name)
            {
                return Err(LockExecutionError::SecretBoundValuePersisted {
                    name: entry.name.clone(),
                });
            }

            let committed_value = launch
                .environment
                .iter()
                .find(|variable| variable.name == entry.name)
                .ok_or_else(|| LockExecutionError::EnvelopeUncommittedValue {
                    name: entry.name.clone(),
                })?;
            if committed_value.value_digest != stored {
                return Err(LockExecutionError::EnvelopeValueDigestMismatch {
                    name: entry.name.clone(),
                });
            }
        }
    }

    // D5 completeness (Major 1). When the envelope commits a non-empty
    // environment it IS the authoritative env set, so every committed non-secret
    // name MUST be persisted (D5 ⊇ committed). An empty committed environment
    // permits `launch` absent / environment empty. Combined with the per-entry
    // `EnvelopeUncommittedValue` check (D5 ⊆ committed) this enforces set
    // equality, so cold reconstruction can recover every committed value.
    if let Some(launch) = committed
        && !launch.environment.is_empty()
    {
        let persisted_names: BTreeSet<&str> =
            persisted.iter().map(|entry| entry.name.as_str()).collect();
        if let Some(missing) = launch
            .environment
            .iter()
            .map(|variable| variable.name.as_str())
            .find(|name| !persisted_names.contains(name))
        {
            return Err(LockExecutionError::EnvelopeMissingCommittedValue {
                name: missing.to_string(),
            });
        }
    }

    Ok(())
}

/// Run both read-path checks: envelope `execution_id` re-derivation and env
/// value digest re-derivation. Reject (fail closed) on the first failure.
pub fn verify_lock_execution(lock: &CapsuleLock) -> Result<(), LockExecutionError> {
    verify_execution_envelope(lock)?;
    verify_environment_values(lock)?;
    Ok(())
}

/// Interpret the lock's Capsule Program identity states — the complete ADR-014
/// §5 four-state matrix, fail closed:
///
/// ```text
/// program_identity ABSENT + execution claim ABSENT   → Ok (true legacy)
/// program_identity ABSENT + execution claim PRESENT  → ParentEnvelopeMissing
/// program_identity PRESENT + execution ABSENT        → program self-verification only
/// program_identity PRESENT + execution PRESENT       → claim mandatory AND matching
///                                                      (ParentMissing / ParentMismatch)
/// ```
///
/// The [`VerifiedCapsuleProgramId`](crate::capsule_program_contract::VerifiedCapsuleProgramId)
/// is minted exactly once here (through the envelope's re-deriving
/// `verified_capsule_program_id`) and handed to the pairwise
/// [`verify_program_parent`] check, so the parent claim can only ever be
/// compared against a hash-checked declaration.
pub fn verify_lock_program_identity(lock: &CapsuleLock) -> Result<(), CapsuleProgramLinkError> {
    match (&lock.program_identity, &lock.execution_contract) {
        (None, Some(execution)) if execution.capsule_program_id.is_some() => {
            Err(CapsuleProgramLinkError::ParentEnvelopeMissing)
        }
        (None, _) => Ok(()),
        (Some(program), None) => {
            program.verified_capsule_program_id()?;
            Ok(())
        }
        (Some(program), Some(execution)) => {
            let verified = program.verified_capsule_program_id()?;
            verify_program_parent(&verified, execution)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::capsule_lock::schema::{LockEnvironmentValue, LockLaunchSection};
    use crate::execution_contract::{
        ContentDigest, DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentValuePayloadV1,
        EnvironmentVariableContract, ExecutionContractEnvelopeV1, ExecutionContractV1, ExecutionId,
        ExternalStateAccess, ExternalStateContract, GuestPath, GuestSurfaceContract,
        OpaqueContractDomainV1, ResolvedArtifactContract, ResolvedBuildOutputContract,
        ResolvedDependencyContract, ResolvedFilesystemContract, ResolvedLaunchContract,
        ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract, SnapshotExclusion,
        opaque_subcontract_digest,
    };
    use std::collections::BTreeMap;
    use std::num::NonZeroU16;

    fn content(byte: u8) -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32])
    }

    fn opaque(
        domain: OpaqueContractDomainV1,
        payload: &serde_json::Value,
    ) -> OpaqueContractDigestV1 {
        opaque_subcontract_digest(domain, payload).expect("digest")
    }

    fn sample_contract() -> ExecutionContractV1 {
        let placeholder = opaque(OpaqueContractDomainV1::SourceProjection, &json!({}));
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: content(1),
                projection_digest: placeholder,
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: None,
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                digest: content(2),
                dynamic_contract_digest: placeholder,
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: content(3),
                output_digest: content(4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: content(5),
                projection_digest: placeholder,
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string()],
                cwd: GuestPath::parse("/workspace").unwrap(),
                process_model_digest: placeholder,
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: environment_value_digest(&node_env_payload())
                        .expect("env value digest"),
                }],
                environment_policy_digest: placeholder,
                secret_bindings: vec![],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: content(7),
                topology_digest: placeholder,
                readonly_layers: vec![content(8)],
                writable_paths: vec![GuestPath::parse("/tmp").unwrap()],
            },
            policy: ResolvedPolicyContract {
                network_digest: placeholder,
                capability_digest: placeholder,
                filesystem_digest: placeholder,
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: Some(NonZeroU16::new(8080).unwrap()),
                features: vec![],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: GuestPath::parse("/data").unwrap(),
                access: ExternalStateAccess::ReadOnly,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
    }

    fn node_env_payload() -> EnvironmentValuePayloadV1 {
        EnvironmentValuePayloadV1::utf8("production")
    }

    fn envelope_of(contract: ExecutionContractV1) -> ExecutionContractEnvelopeV1 {
        let execution_id = contract.compute_execution_id().expect("id");
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

    fn sample_envelope() -> ExecutionContractEnvelopeV1 {
        envelope_of(sample_contract())
    }

    fn lock_with(
        execution_contract: Option<ExecutionContractEnvelopeV1>,
        launch: Option<LockLaunchSection>,
    ) -> CapsuleLock {
        CapsuleLock {
            execution_contract,
            launch,
            ..CapsuleLock::default()
        }
    }

    fn env_launch(
        name: &str,
        value: EnvironmentValuePayloadV1,
        value_digest: String,
    ) -> LockLaunchSection {
        LockLaunchSection {
            environment: vec![LockEnvironmentValue {
                name: name.to_string(),
                value,
                value_digest,
            }],
        }
    }

    #[test]
    fn absent_sections_verify_ok() {
        let lock = CapsuleLock::default();
        assert!(verify_lock_execution(&lock).is_ok());
    }

    #[test]
    fn valid_envelope_verifies() {
        let lock = lock_with(Some(sample_envelope()), None);
        assert!(verify_execution_envelope(&lock).is_ok());
    }

    #[test]
    fn tampered_execution_id_is_rejected() {
        let mut envelope = sample_envelope();
        envelope.execution_id = ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
        let lock = lock_with(Some(envelope), None);
        let error = verify_execution_envelope(&lock).expect_err("tampered id must reject");
        assert!(matches!(error, LockExecutionError::Envelope(_)));
    }

    #[test]
    fn env_value_digest_rederived_and_verified() {
        let payload = EnvironmentValuePayloadV1::utf8("production");
        let digest = environment_value_digest(&payload).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch("NODE_ENV", payload, digest.to_string())),
        );
        assert!(verify_environment_values(&lock).is_ok());
    }

    #[test]
    fn tampered_env_value_payload_is_rejected() {
        let digest =
            environment_value_digest(&EnvironmentValuePayloadV1::utf8("production")).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch(
                "NODE_ENV",
                EnvironmentValuePayloadV1::utf8("development"),
                digest.to_string(),
            )),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::ValueDigestMismatch {
                name: "NODE_ENV".to_string()
            })
        );
    }

    #[test]
    fn secret_env_value_persisted_is_rejected() {
        let digest = environment_value_digest(&EnvironmentValuePayloadV1::utf8("t")).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch(
                "API_TOKEN",
                EnvironmentValuePayloadV1::utf8("t"),
                digest.to_string(),
            )),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::SecretValuePersisted(
                "API_TOKEN".to_string()
            ))
        );
    }

    #[test]
    fn env_value_cross_checks_envelope_commitment() {
        // Same payload as the envelope commits ⇒ ok.
        let payload = node_env_payload();
        let digest = environment_value_digest(&payload).unwrap();
        let mut lock = lock_with(
            Some(sample_envelope()),
            Some(env_launch("NODE_ENV", payload, digest.to_string())),
        );
        assert!(verify_lock_execution(&lock).is_ok());

        // A different (still self-consistent) payload disagrees with the
        // envelope's committed digest ⇒ rejected.
        let other_payload = EnvironmentValuePayloadV1::utf8("staging");
        let other_digest = environment_value_digest(&other_payload).unwrap();
        lock.launch = Some(env_launch(
            "NODE_ENV",
            other_payload,
            other_digest.to_string(),
        ));
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::EnvelopeValueDigestMismatch {
                name: "NODE_ENV".to_string()
            })
        );
    }

    #[test]
    fn extra_uncommitted_env_var_is_rejected() {
        // Finding 1 hole (a): a self-consistent D5 value whose name the
        // envelope's execution identity never committed is unvouched. Its
        // payload/digest agree with each other, and it is not secret-bearing, so
        // it passes self-consistency — but the committed contract only vouches
        // for NODE_ENV, so persisting EXTRA_VAR alongside the envelope must be
        // rejected rather than accepted on self-consistency alone.
        let payload = EnvironmentValuePayloadV1::utf8("extra");
        let digest = environment_value_digest(&payload).unwrap();
        let lock = lock_with(
            Some(sample_envelope()),
            Some(env_launch("EXTRA_VAR", payload, digest.to_string())),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::EnvelopeUncommittedValue {
                name: "EXTRA_VAR".to_string()
            })
        );
    }

    #[test]
    fn lock_id_is_byte_stable_when_adding_or_removing_execution_contract() {
        // D4 — the execution_contract envelope and the launch env section are
        // excluded from lock identity (absent from CanonicalLockProjection), so
        // an existing lock's lock_id must not change when they are added or
        // removed.
        use crate::capsule_lock::{compute_lock_id, recompute_lock_id, validate_persisted_strict};

        let mut base = CapsuleLock::default();
        base.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        base.contract
            .entries
            .insert("process".to_string(), json!({"entrypoint": "main.ts"}));
        recompute_lock_id(&mut base).expect("recompute base lock_id");
        let baseline = compute_lock_id(&base).expect("baseline lock_id");

        let payload = node_env_payload();
        let mut with_sections = base.clone();
        with_sections.execution_contract = Some(sample_envelope());
        with_sections.launch = Some(LockLaunchSection {
            environment: vec![LockEnvironmentValue {
                name: "NODE_ENV".to_string(),
                value: payload.clone(),
                value_digest: environment_value_digest(&payload).unwrap().to_string(),
            }],
        });

        // lock_id is unchanged by the additive fields.
        assert_eq!(baseline, compute_lock_id(&with_sections).expect("lock_id"));
        // And the persisted lock still validates (its stored lock_id matches).
        assert!(validate_persisted_strict(&with_sections).is_ok());

        // Removing the fields returns a byte-identical projection.
        let mut removed = with_sections.clone();
        removed.execution_contract = None;
        removed.launch = None;
        assert_eq!(
            baseline,
            compute_lock_id(&removed).expect("removed lock_id")
        );
    }

    // ---- Blocker 1: signature binds execution content; lock_id does not ----

    fn base_lock() -> CapsuleLock {
        let mut base = CapsuleLock::default();
        base.resolution
            .entries
            .insert("runtime".to_string(), json!({"kind": "deno"}));
        base.contract
            .entries
            .insert("process".to_string(), json!({"entrypoint": "main.ts"}));
        crate::capsule_lock::recompute_lock_id(&mut base).expect("recompute base lock_id");
        base
    }

    #[test]
    fn signature_payload_binds_execution_sections_while_lock_id_stays_stable() {
        use crate::capsule_lock::{
            canonical_projection_bytes, canonical_signature_payload_bytes, compute_lock_id,
        };

        let base = base_lock();
        let base_lock_id = compute_lock_id(&base).unwrap();

        // (c) A legacy lock with no execution_contract / launch signs over bytes
        // byte-identical to the identity projection (verifies unchanged).
        assert_eq!(
            canonical_signature_payload_bytes(&base).unwrap(),
            canonical_projection_bytes(&base).unwrap()
        );

        let payload = node_env_payload();
        let digest = environment_value_digest(&payload).unwrap().to_string();
        let mut with_sections = base.clone();
        with_sections.execution_contract = Some(sample_envelope());
        with_sections.launch = Some(env_launch("NODE_ENV", payload, digest));

        // lock_id is unchanged by the additive sections, but the signature now
        // covers execution_contract + launch, so its bytes differ.
        assert_eq!(base_lock_id, compute_lock_id(&with_sections).unwrap());
        assert_ne!(
            canonical_signature_payload_bytes(&base).unwrap(),
            canonical_signature_payload_bytes(&with_sections).unwrap()
        );

        // (a) Swap execution_contract for a different valid envelope ⇒ lock_id
        // unchanged, signature bytes change.
        let mut other_contract = sample_contract();
        other_contract.target.architecture = "aarch64".to_string();
        let mut swapped = with_sections.clone();
        swapped.execution_contract = Some(envelope_of(other_contract));
        assert_eq!(base_lock_id, compute_lock_id(&swapped).unwrap());
        assert_ne!(
            canonical_signature_payload_bytes(&with_sections).unwrap(),
            canonical_signature_payload_bytes(&swapped).unwrap()
        );

        // (b) Change launch.environment value + digest consistently ⇒ lock_id
        // unchanged, signature bytes change.
        let other_payload = EnvironmentValuePayloadV1::utf8("staging");
        let other_digest = environment_value_digest(&other_payload)
            .unwrap()
            .to_string();
        let mut relaunched = with_sections.clone();
        relaunched.launch = Some(env_launch("NODE_ENV", other_payload, other_digest));
        assert_eq!(base_lock_id, compute_lock_id(&relaunched).unwrap());
        assert_ne!(
            canonical_signature_payload_bytes(&with_sections).unwrap(),
            canonical_signature_payload_bytes(&relaunched).unwrap()
        );
    }

    // ---- Blocker 2: read/write boundary is fail-closed via the public path ----

    #[test]
    fn load_verified_rejects_tampered_execution_id() {
        let mut envelope = sample_envelope();
        envelope.execution_id = ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
        let mut lock = base_lock();
        lock.execution_contract = Some(envelope);
        crate::capsule_lock::recompute_lock_id(&mut lock).unwrap();
        // Serialize WITHOUT the write-path verification so a tampered artifact can
        // reach the reader; the read path must reject it.
        let raw = serde_json::to_string(&lock).unwrap();
        let error =
            crate::capsule_lock::load_verified_from_str(&raw).expect_err("tampered id must reject");
        assert!(error.to_string().contains("execution verification failed"));
    }

    #[test]
    fn to_pretty_json_rejects_tampered_env_payload() {
        let good = environment_value_digest(&node_env_payload())
            .unwrap()
            .to_string();
        let mut lock = base_lock();
        // Payload does not match its stored digest.
        lock.launch = Some(env_launch(
            "NODE_ENV",
            EnvironmentValuePayloadV1::utf8("development"),
            good,
        ));
        let error =
            crate::capsule_lock::to_pretty_json(&lock).expect_err("tampered payload must reject");
        assert!(error.to_string().contains("execution verification failed"));
    }

    // ---- Blocker 3 layer 3: secret_bindings is authoritative at the D5 read ----

    #[test]
    fn secret_bound_persisted_value_is_rejected_even_when_heuristic_misses() {
        for name in ["DATABASE_URL", "AWS_ACCESS_KEY_ID"] {
            assert!(
                !is_sensitive_env_key(name),
                "{name} must dodge the heuristic"
            );
            let mut contract = sample_contract();
            contract.launch.environment = vec![];
            contract.launch.secret_bindings = vec![name.to_string()];
            let payload = EnvironmentValuePayloadV1::utf8("x");
            let digest = environment_value_digest(&payload).unwrap().to_string();
            let lock = lock_with(
                Some(envelope_of(contract)),
                Some(env_launch(name, payload, digest)),
            );
            assert_eq!(
                verify_environment_values(&lock),
                Err(LockExecutionError::SecretBoundValuePersisted {
                    name: name.to_string()
                }),
                "{name}"
            );
        }
    }

    // ---- Major 1: D5 completeness / canonical order ----

    fn contract_with_two_env_vars() -> ExecutionContractV1 {
        let mut contract = sample_contract();
        let a_digest = environment_value_digest(&EnvironmentValuePayloadV1::utf8("a")).unwrap();
        contract.launch.environment = vec![
            EnvironmentVariableContract {
                name: "A_VAR".to_string(),
                value_digest: a_digest,
            },
            EnvironmentVariableContract {
                name: "NODE_ENV".to_string(),
                value_digest: environment_value_digest(&node_env_payload()).unwrap(),
            },
        ];
        contract
    }

    #[test]
    fn missing_committed_env_value_is_rejected() {
        // Envelope commits {A_VAR, NODE_ENV}; persist only NODE_ENV ⇒ A_VAR is a
        // committed value with no persisted payload ⇒ rejected (Major 1).
        let payload = node_env_payload();
        let digest = environment_value_digest(&payload).unwrap().to_string();
        let lock = lock_with(
            Some(envelope_of(contract_with_two_env_vars())),
            Some(env_launch("NODE_ENV", payload, digest)),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::EnvelopeMissingCommittedValue {
                name: "A_VAR".to_string()
            })
        );
    }

    #[test]
    fn duplicate_persisted_env_value_is_rejected() {
        let payload = node_env_payload();
        let digest = environment_value_digest(&payload).unwrap().to_string();
        let lock = lock_with(
            None,
            Some(LockLaunchSection {
                environment: vec![
                    LockEnvironmentValue {
                        name: "NODE_ENV".to_string(),
                        value: payload.clone(),
                        value_digest: digest.clone(),
                    },
                    LockEnvironmentValue {
                        name: "NODE_ENV".to_string(),
                        value: payload,
                        value_digest: digest,
                    },
                ],
            }),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::NonCanonicalEnvironment)
        );
    }

    #[test]
    fn unsorted_persisted_env_value_is_rejected() {
        let node = node_env_payload();
        let node_digest = environment_value_digest(&node).unwrap().to_string();
        let alpha = EnvironmentValuePayloadV1::utf8("a");
        let alpha_digest = environment_value_digest(&alpha).unwrap().to_string();
        // NODE_ENV before ALPHA is not strictly increasing ⇒ rejected.
        let lock = lock_with(
            None,
            Some(LockLaunchSection {
                environment: vec![
                    LockEnvironmentValue {
                        name: "NODE_ENV".to_string(),
                        value: node,
                        value_digest: node_digest,
                    },
                    LockEnvironmentValue {
                        name: "ALPHA".to_string(),
                        value: alpha,
                        value_digest: alpha_digest,
                    },
                ],
            }),
        );
        assert_eq!(
            verify_environment_values(&lock),
            Err(LockExecutionError::NonCanonicalEnvironment)
        );
    }

    // ---- ADR-014 §5: program identity states at the lock trust boundary ----

    use crate::capsule_program_contract::{
        CapsuleProgramEnvelopeV1, CapsuleProgramId, test_capsule_program_envelope,
    };

    /// An execution envelope built from the shared seeded contract, carrying
    /// the given parent claim. The committed environment is emptied so the D5
    /// completeness check permits an absent `launch` section — these tests
    /// exercise the program-identity states only.
    fn execution_envelope_claiming(claim: Option<CapsuleProgramId>) -> ExecutionContractEnvelopeV1 {
        let mut contract = crate::execution_contract::test_execution_contract(0x42);
        contract.launch.environment = Vec::new();
        let mut envelope = envelope_of(contract);
        envelope.capsule_program_id = claim;
        envelope
    }

    /// A program envelope whose stored id is a VALID-shape id belonging to a
    /// DIFFERENT declaration — self-verification must fail.
    fn tampered_program_envelope() -> CapsuleProgramEnvelopeV1 {
        let mut program = test_capsule_program_envelope(1);
        program.capsule_program_id = test_capsule_program_envelope(2).capsule_program_id;
        program
    }

    type Chokepoint = (&'static str, fn(&CapsuleLock) -> crate::error::Result<()>);

    /// The three trust-boundary chokepoints, each driven through its public
    /// entrypoint. `load_verified_from_str` reads bytes serialized WITHOUT the
    /// write-path verification so a tampered artifact can reach the reader.
    fn chokepoints() -> [Chokepoint; 3] {
        [
            ("load_verified_from_str", |lock| {
                let raw = serde_json::to_string(lock).expect("serialize lock");
                crate::capsule_lock::load_verified_from_str(&raw).map(|_| ())
            }),
            ("to_pretty_json", |lock| {
                crate::capsule_lock::to_pretty_json(lock).map(|_| ())
            }),
            ("write_canonical_to_vec", |lock| {
                crate::capsule_lock::write_canonical_to_vec(lock).map(|_| ())
            }),
        ]
    }

    #[test]
    fn verify_lock_program_identity_covers_the_four_state_matrix() {
        // program ABSENT + claim ABSENT (execution absent entirely) ⇒ Ok.
        assert_eq!(verify_lock_program_identity(&base_lock()), Ok(()));

        // program ABSENT + execution present WITHOUT a claim ⇒ Ok (true legacy).
        let mut unclaimed = base_lock();
        unclaimed.execution_contract = Some(execution_envelope_claiming(None));
        assert_eq!(verify_lock_program_identity(&unclaimed), Ok(()));

        // program ABSENT + claim PRESENT ⇒ orphan claim, fail closed.
        let program = test_capsule_program_envelope(1);
        let program_id = program.capsule_program_id.clone();
        let mut orphan = base_lock();
        orphan.execution_contract = Some(execution_envelope_claiming(Some(program_id.clone())));
        assert_eq!(
            verify_lock_program_identity(&orphan),
            Err(CapsuleProgramLinkError::ParentEnvelopeMissing)
        );

        // program PRESENT + execution ABSENT ⇒ program self-verification only.
        let mut draft = base_lock();
        draft.program_identity = Some(program.clone());
        assert_eq!(verify_lock_program_identity(&draft), Ok(()));

        let mut tampered = base_lock();
        tampered.program_identity = Some(tampered_program_envelope());
        assert!(matches!(
            verify_lock_program_identity(&tampered),
            Err(CapsuleProgramLinkError::ProgramEnvelope(_))
        ));

        // program PRESENT + execution PRESENT ⇒ claim mandatory AND matching.
        let mut claim_missing = base_lock();
        claim_missing.program_identity = Some(program.clone());
        claim_missing.execution_contract = Some(execution_envelope_claiming(None));
        assert_eq!(
            verify_lock_program_identity(&claim_missing),
            Err(CapsuleProgramLinkError::ParentMissing)
        );

        let other_id = test_capsule_program_envelope(2).capsule_program_id;
        let mut claim_mismatched = base_lock();
        claim_mismatched.program_identity = Some(program.clone());
        claim_mismatched.execution_contract =
            Some(execution_envelope_claiming(Some(other_id.clone())));
        assert_eq!(
            verify_lock_program_identity(&claim_mismatched),
            Err(CapsuleProgramLinkError::ParentMismatch {
                claimed: other_id,
                verified: program_id.clone(),
            })
        );

        let mut claim_matching = base_lock();
        claim_matching.program_identity = Some(program);
        claim_matching.execution_contract = Some(execution_envelope_claiming(Some(program_id)));
        assert_eq!(verify_lock_program_identity(&claim_matching), Ok(()));
    }

    #[test]
    fn every_chokepoint_rejects_every_invalid_program_identity_state() {
        let program = test_capsule_program_envelope(1);
        let program_id = program.capsule_program_id.clone();
        let other_id = test_capsule_program_envelope(2).capsule_program_id;

        let mut tampered = base_lock();
        tampered.program_identity = Some(tampered_program_envelope());

        let mut claim_missing = base_lock();
        claim_missing.program_identity = Some(program.clone());
        claim_missing.execution_contract = Some(execution_envelope_claiming(None));

        let mut claim_mismatched = base_lock();
        claim_mismatched.program_identity = Some(program);
        claim_mismatched.execution_contract = Some(execution_envelope_claiming(Some(other_id)));

        let mut orphan_claim = base_lock();
        orphan_claim.execution_contract = Some(execution_envelope_claiming(Some(program_id)));

        let scenarios = [
            ("tampered program id", tampered),
            ("parent claim missing", claim_missing),
            ("parent claim mismatched", claim_mismatched),
            ("orphan parent claim", orphan_claim),
        ];

        for (scenario, lock) in &scenarios {
            for (chokepoint, run) in chokepoints() {
                let error =
                    run(lock).expect_err(&format!("{scenario} must reject via {chokepoint}"));
                assert!(
                    error
                        .to_string()
                        .contains("program identity verification failed"),
                    "{scenario} via {chokepoint}: {error}"
                );
            }
        }
    }

    #[test]
    fn every_chokepoint_accepts_a_matching_program_identity() {
        let program = test_capsule_program_envelope(1);
        let claim = program.capsule_program_id.clone();
        let mut lock = base_lock();
        lock.program_identity = Some(program);
        lock.execution_contract = Some(execution_envelope_claiming(Some(claim)));

        for (chokepoint, run) in chokepoints() {
            assert!(run(&lock).is_ok(), "matching claim must pass {chokepoint}");
        }
    }

    #[test]
    fn draft_program_identity_without_execution_passes_every_chokepoint() {
        let mut lock = base_lock();
        lock.program_identity = Some(test_capsule_program_envelope(1));

        assert_eq!(verify_lock_program_identity(&lock), Ok(()));
        for (chokepoint, run) in chokepoints() {
            assert!(run(&lock).is_ok(), "draft state must pass {chokepoint}");
        }
    }

    #[test]
    fn lock_id_is_byte_stable_when_adding_or_removing_program_identity() {
        use crate::capsule_lock::{compute_lock_id, validate_persisted_strict};

        let base = base_lock();
        let baseline = compute_lock_id(&base).expect("baseline lock_id");

        let mut with_program = base.clone();
        with_program.program_identity = Some(test_capsule_program_envelope(1));
        assert_eq!(
            baseline,
            compute_lock_id(&with_program).expect("with-program lock_id")
        );
        // The persisted lock still validates (its stored lock_id matches).
        assert!(validate_persisted_strict(&with_program).is_ok());

        let mut removed = with_program.clone();
        removed.program_identity = None;
        assert_eq!(
            baseline,
            compute_lock_id(&removed).expect("removed lock_id")
        );
    }

    #[test]
    fn signature_payload_binds_program_identity_while_legacy_bytes_stay_stable() {
        use crate::capsule_lock::{
            canonical_projection_bytes, canonical_signature_payload_bytes, compute_lock_id,
        };

        // A legacy lock (no additive sections) signs over bytes byte-identical
        // to the identity projection — unchanged by the program_identity field.
        let base = base_lock();
        assert_eq!(
            canonical_signature_payload_bytes(&base).unwrap(),
            canonical_projection_bytes(&base).unwrap()
        );

        // A lock carrying the execution sections but NO program_identity signs
        // over bytes byte-identical to the pre-ADR-014 signature projection
        // {schema_version, execution_contract, launch, resolution, contract}.
        let payload = node_env_payload();
        let digest = environment_value_digest(&payload).unwrap().to_string();
        let mut with_execution = base.clone();
        with_execution.execution_contract = Some(sample_envelope());
        with_execution.launch = Some(env_launch("NODE_ENV", payload, digest));
        let pre_change_bytes = serde_jcs::to_vec(&json!({
            "schema_version": &with_execution.schema_version,
            "execution_contract": &with_execution.execution_contract,
            "launch": &with_execution.launch,
            "resolution": &with_execution.resolution,
            "contract": &with_execution.contract,
        }))
        .expect("pre-change signature bytes");
        assert_eq!(
            canonical_signature_payload_bytes(&with_execution).unwrap(),
            pre_change_bytes
        );

        // Adding program_identity changes the signature bytes while lock_id
        // stays byte-stable.
        let baseline = compute_lock_id(&with_execution).unwrap();
        let mut with_program = with_execution.clone();
        with_program.program_identity = Some(test_capsule_program_envelope(1));
        assert_eq!(baseline, compute_lock_id(&with_program).unwrap());
        assert_ne!(
            canonical_signature_payload_bytes(&with_execution).unwrap(),
            canonical_signature_payload_bytes(&with_program).unwrap()
        );
    }
}
