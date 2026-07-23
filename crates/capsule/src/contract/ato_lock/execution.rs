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
//! [`verify_lock_execution`] runs both. Wiring this into the launch re-read
//! (`session.rs`) and OCI runners is deferred to a later PR; this is the pure
//! verification the reader calls.

use thiserror::Error;

use crate::ato_lock::schema::AtoLock;
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
pub fn verify_execution_envelope(lock: &AtoLock) -> Result<(), LockExecutionError> {
    if let Some(envelope) = &lock.execution_contract {
        envelope.verify().map_err(LockExecutionError::Envelope)?;
    }
    Ok(())
}

/// Verify every persisted non-secret env value (if any) by re-deriving its
/// `value_digest`, rejecting secret names, and — when the envelope is present —
/// requiring each persisted name to be committed by the execution identity and
/// cross-checking its digest against the committed value digest.
pub fn verify_environment_values(lock: &AtoLock) -> Result<(), LockExecutionError> {
    let Some(launch) = &lock.launch else {
        return Ok(());
    };

    for entry in &launch.environment {
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
        // value MUST be vouched for by a committed `launch.environment` entry
        // AND its digest MUST agree with the committed one. A D5 name the
        // identity never committed is rejected here (hole (a)) — a persisted
        // value with no committed identity is unvouched.
        //
        // The reverse direction is intentionally lenient (hole (b)): the
        // committed identity MAY declare an env var whose value was not
        // persisted in D5 (e.g. it was not measured / not stored), so a
        // committed name absent from D5 is NOT verified or required here.
        if let Some(envelope) = &lock.execution_contract {
            let committed = envelope
                .execution_contract
                .launch
                .environment
                .iter()
                .find(|variable| variable.name == entry.name)
                .ok_or_else(|| LockExecutionError::EnvelopeUncommittedValue {
                    name: entry.name.clone(),
                })?;
            if committed.value_digest != stored {
                return Err(LockExecutionError::EnvelopeValueDigestMismatch {
                    name: entry.name.clone(),
                });
            }
        }
    }

    Ok(())
}

/// Run both read-path checks: envelope `execution_id` re-derivation and env
/// value digest re-derivation. Reject (fail closed) on the first failure.
pub fn verify_lock_execution(lock: &AtoLock) -> Result<(), LockExecutionError> {
    verify_execution_envelope(lock)?;
    verify_environment_values(lock)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ato_lock::schema::{LockEnvironmentValue, LockLaunchSection};
    use crate::execution_contract::{
        ContentDigest, DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentVariableContract,
        ExecutionContractEnvelopeV1, ExecutionContractV1, ExecutionId, ExternalStateAccess,
        ExternalStateContract, GuestPath, GuestSurfaceContract, OpaqueContractDomainV1,
        ResolvedArtifactContract, ResolvedBuildOutputContract, ResolvedDependencyContract,
        ResolvedFilesystemContract, ResolvedLaunchContract, ResolvedPolicyContract,
        ResolvedSourceContract, ResolvedTargetContract, SnapshotExclusion,
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
                    value_digest: opaque(
                        OpaqueContractDomainV1::EnvironmentValue,
                        &json!({"raw": "production"}),
                    ),
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

    fn sample_envelope() -> ExecutionContractEnvelopeV1 {
        let contract = sample_contract();
        let execution_id = contract.compute_execution_id().expect("id");
        ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id,
            resolved_refs: Default::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        }
    }

    fn lock_with(
        execution_contract: Option<ExecutionContractEnvelopeV1>,
        launch: Option<LockLaunchSection>,
    ) -> AtoLock {
        AtoLock {
            execution_contract,
            launch,
            ..AtoLock::default()
        }
    }

    fn env_launch(name: &str, value: serde_json::Value, value_digest: String) -> LockLaunchSection {
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
        let lock = AtoLock::default();
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
        let payload = json!({"raw": "production"});
        let digest = environment_value_digest(&payload).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch("NODE_ENV", payload, digest.to_string())),
        );
        assert!(verify_environment_values(&lock).is_ok());
    }

    #[test]
    fn tampered_env_value_payload_is_rejected() {
        let digest = environment_value_digest(&json!({"raw": "production"})).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch(
                "NODE_ENV",
                json!({"raw": "development"}),
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
        let digest = environment_value_digest(&json!("t")).unwrap();
        let lock = lock_with(
            None,
            Some(env_launch("API_TOKEN", json!("t"), digest.to_string())),
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
        let payload = json!({"raw": "production"});
        let digest = environment_value_digest(&payload).unwrap();
        let mut lock = lock_with(
            Some(sample_envelope()),
            Some(env_launch("NODE_ENV", payload, digest.to_string())),
        );
        assert!(verify_lock_execution(&lock).is_ok());

        // A different (still self-consistent) payload disagrees with the
        // envelope's committed digest ⇒ rejected.
        let other_payload = json!({"raw": "staging"});
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
        let payload = json!({"raw": "extra"});
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
        use crate::ato_lock::{compute_lock_id, recompute_lock_id, validate_persisted_strict};

        let mut base = AtoLock::default();
        base.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        base.contract
            .entries
            .insert("process".to_string(), json!({"entrypoint": "main.ts"}));
        recompute_lock_id(&mut base).expect("recompute base lock_id");
        let baseline = compute_lock_id(&base).expect("baseline lock_id");

        let payload = json!({"raw": "production"});
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
}
