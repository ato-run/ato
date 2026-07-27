//! Turning a v1 build's output into an input this builder may act on.
//!
//! # What this proves, and what it deliberately does not
//!
//! A v1 `ato build` publishes three things: a `capsule.lock` carrying a minted
//! Execution Contract, a packed guest image, and a
//! [`V1MaterializationReceipt`] naming the image it wrote. This module is the
//! gate those three pass through before any later phase is allowed to touch
//! them.
//!
//! It proves exactly one thing: **the lock, the receipt and the file on disk
//! describe one build, and that build is inside the v1 subset this builder
//! knows how to handle.**
//!
//! It does NOT prove that the packed filesystem contains the contents
//! `filesystem.view_digest` commits. Nothing short of unpacking or booting the
//! image can show that, and both happen later (7B reads `/proc/self/cmdline`
//! and `/proc/self/cwd` from the running guest; 7C re-verifies across a
//! disposable restore). A receipt is a producer's attestation, and this module
//! checks that the attestation is internally consistent and matches the bytes
//! present — not that the producer measured honestly.
//!
//! Naming follows that boundary on purpose: this is `VerifiedV1BuildInput`,
//! not "verified guest".
//!
//! # Why the receipt's paths are not used to find the files
//!
//! A receipt records where the PRODUCER wrote its outputs. By the time a
//! builder reads it the artifact has usually been copied into a job directory,
//! so those paths name files on another host. The caller therefore supplies the
//! paths it actually holds, and the receipt's own paths are carried through as
//! provenance only. A receipt whose `guest_image` disagrees with where the file
//! now lives is not an error — it is the normal case.
//!
//! # Fail-closed
//!
//! Every uncertainty is a refusal, never a downgrade:
//!
//! * the lock does not read back, or carries no contract ⇒ refuse;
//! * the contract does not re-derive its own `execution_id` ⇒ refuse. The
//!   identity handed on comes from `verified_execution_id`, which recomputes
//!   the canonical hash, so it is never merely read out of the stored field;
//! * the contract declares a facet outside the ADR-015 §7 subset ⇒ refuse, by
//!   name, with the reason — **never** ignore the facet and proceed;
//! * the receipt disagrees with the lock about any shared value ⇒ refuse;
//! * the artifact's length or digest disagrees with the receipt ⇒ refuse;
//! * the producer did not claim `trusted_load_verified` ⇒ refuse.
//!
//! # Order
//!
//! Checks run cheapest-first *except* where a later check would otherwise
//! report a confusing cause: the lock is verified before the receipt is read,
//! so a stale receipt is reported against a contract already known good, and
//! the artifact is hashed last because it is the only step that reads
//! gigabytes.

use std::path::{Path, PathBuf};

use capsule::capsule_lock;
use capsule::execution_contract::{
    ContentDigest, ExecutionContractError, ExecutionContractV1, ExecutionId, GuestPath,
    ResolvedTargetContract,
};
use snapshot::v1_materialization::{
    GuestArtifactDigest, GuestFilesystemViewDigest, V1MaterializationReceipt,
    measure_guest_artifact, target_triple,
};

/// Why an intake was refused.
///
/// One variant per distinct thing that can be wrong with the world. Collapsing
/// them would make every one read as "the build is bad", when the operator's
/// next action differs completely between "the lock is stale", "this capsule
/// uses a feature the subset does not cover" and "the image on disk is not the
/// one the build wrote".
#[derive(Debug, thiserror::Error)]
pub enum V1IntakeRefusal {
    #[error("the lock at {path} could not be read back: {reason}")]
    LockUnreadable { path: PathBuf, reason: String },

    #[error(
        "the lock at {path} carries no execution contract, so this build minted no Execution \
         Identity and cannot be taken as a v1 input"
    )]
    LockCarriesNoExecutionContract { path: PathBuf },

    /// The contract does not hash to the `execution_id` stored beside it.
    ///
    /// Not reachable through [`V1BuildIntake::verify`] today: the trusted load
    /// runs the same recomputation and reports it as [`Self::LockUnreadable`]
    /// first. It exists because the identity this module hands on is taken from
    /// its own recomputation rather than from the stored field, and that call
    /// has to be allowed to fail somewhere. Kept rather than unwrapped so a
    /// future loader that stops verifying envelopes does not turn a silent
    /// assumption into a wrong Execution Identity.
    #[error("the execution contract in {path} does not verify: {source}")]
    ContractVerificationFailed {
        path: PathBuf,
        #[source]
        source: ExecutionContractError,
    },

    /// A facet the contract declares that this builder has no way to honour.
    ///
    /// Refused rather than ignored: a facet silently dropped here becomes a
    /// guest that runs without something its identity says it has.
    #[error("this build declares {feature}, which this builder cannot honour: {why}")]
    UnsupportedFacet {
        feature: &'static str,
        why: &'static str,
    },

    #[error(
        "the receipt disagrees with the lock about {field}: the receipt says {receipt}, the lock \
         says {lock}"
    )]
    ReceiptDisagreesWithLock {
        field: &'static str,
        receipt: String,
        lock: String,
    },

    #[error(
        "the receipt does not claim trusted_load_verified, so the producer never confirmed its own \
         lock read back as the execution it minted"
    )]
    ReceiptNotTrustedLoadVerified,

    #[error("the receipt's {field} is not a well-formed value: {value}")]
    MalformedReceiptValue { field: &'static str, value: String },

    #[error("the guest artifact at {path} could not be read: {reason}")]
    ArtifactUnreadable { path: PathBuf, reason: String },

    #[error(
        "the guest artifact at {path} is {actual} bytes, but the receipt names a {receipt}-byte \
         artifact"
    )]
    ArtifactLengthMismatch {
        path: PathBuf,
        receipt: u64,
        actual: u64,
    },

    #[error(
        "the guest artifact at {path} digests to {measured}, but the receipt names {receipt} — \
         this is not the file that build wrote"
    )]
    ArtifactDigestMismatch {
        path: PathBuf,
        receipt: String,
        measured: String,
    },

    #[error(
        "the receipt's runtime ref {runtime} is not pinned to a digest, so it cannot name the \
         runtime the identity commits"
    )]
    RuntimeRefNotPinned { runtime: String },

    #[error(
        "the receipt's runtime ref {runtime} does not name the runtime artifact the contract \
         commits ({contract_digest})"
    )]
    RuntimeRefDisagreesWithContract {
        runtime: String,
        contract_digest: String,
    },

    #[error("this build targets {detail}, which this builder cannot boot")]
    UnsupportedGuestTarget { detail: String },
}

/// The guest artifact, once its bytes have been shown to be the ones the
/// receipt names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGuestArtifact {
    path: PathBuf,
    bytes: u64,
    digest: GuestArtifactDigest,
}

impl VerifiedGuestArtifact {
    /// Where the artifact actually is — the caller's path, not the receipt's.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The MATERIALIZATION digest. Never an identity input.
    pub fn digest(&self) -> GuestArtifactDigest {
        self.digest
    }
}

/// The launch contract, once it has been shown to be resolved.
///
/// `argv` is exact: it is the vector the kernel is to be handed, including
/// `argv[0]`. No interpreter is inferred and no word is rewritten — ADR-015
/// §9.4 records why the v0.3 bare-`.py` rewrite deliberately does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedLaunchContract {
    argv: Vec<String>,
    cwd: GuestPath,
}

impl VerifiedLaunchContract {
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Where the guest is to start.
    ///
    /// Canonical and absolute by construction (`GuestPath` refuses anything
    /// else). Whether the directory EXISTS in the guest is not knowable here —
    /// that is proven at boot by reading `/proc/self/cwd`.
    pub fn cwd(&self) -> &GuestPath {
        &self.cwd
    }
}

/// The guest target, once it has been shown to be one this builder can boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGuestTarget {
    target: ResolvedTargetContract,
}

impl VerifiedGuestTarget {
    pub fn os(&self) -> &str {
        &self.target.os
    }

    pub fn architecture(&self) -> &str {
        &self.target.architecture
    }

    pub fn abi(&self) -> &str {
        &self.target.abi
    }

    pub fn libc(&self) -> Option<&str> {
        self.target.libc.as_deref()
    }

    pub fn as_contract(&self) -> &ResolvedTargetContract {
        &self.target
    }
}

/// The receipt, once every value it shares with the lock has been shown to
/// agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMaterializationReceipt {
    receipt: V1MaterializationReceipt,
}

impl VerifiedMaterializationReceipt {
    pub fn as_receipt(&self) -> &V1MaterializationReceipt {
        &self.receipt
    }

    /// Where the PRODUCER wrote the image. Provenance only — see the module
    /// doc on why this is not where the file is looked for.
    pub fn producer_guest_image_path(&self) -> &Path {
        &self.receipt.guest_image
    }
}

/// A v1 build output this builder has verified and may act on.
///
/// Every field is private and there is no public constructor, so the only way
/// to obtain one is [`V1BuildIntake::verify`]. A later phase that holds one of
/// these does not have to re-derive the verification, and cannot be handed an
/// unchecked lock or an unmeasured artifact by mistake.
///
/// (This crate is a binary, so the usual `compile_fail` doctest proving the
/// struct literal is unreachable cannot run. The property is instead structural:
/// no field below is `pub`, and no `impl` outside this module can name them.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedV1BuildInput {
    execution_id: ExecutionId,
    guest_artifact: VerifiedGuestArtifact,
    launch: VerifiedLaunchContract,
    target: VerifiedGuestTarget,
    filesystem_view_digest: GuestFilesystemViewDigest,
    materialization_receipt: VerifiedMaterializationReceipt,
    contract: ExecutionContractV1,
}

impl VerifiedV1BuildInput {
    /// The Execution Identity, re-derived from the contract's canonical bytes
    /// rather than read from the lock's stored field.
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn guest_artifact(&self) -> &VerifiedGuestArtifact {
        &self.guest_artifact
    }

    pub fn launch(&self) -> &VerifiedLaunchContract {
        &self.launch
    }

    pub fn target(&self) -> &VerifiedGuestTarget {
        &self.target
    }

    /// The IDENTITY-bearing digest of the guest's contents.
    ///
    /// Distinct from [`VerifiedGuestArtifact::digest`], which names the packed
    /// file. The types differ so the two cannot be interchanged.
    pub fn filesystem_view_digest(&self) -> GuestFilesystemViewDigest {
        self.filesystem_view_digest
    }

    pub fn materialization_receipt(&self) -> &VerifiedMaterializationReceipt {
        &self.materialization_receipt
    }

    pub fn contract(&self) -> &ExecutionContractV1 {
        &self.contract
    }
}

/// A v1 build output awaiting verification.
pub struct V1BuildIntake {
    lock_path: PathBuf,
    guest_image_path: PathBuf,
    receipt: V1MaterializationReceipt,
}

impl V1BuildIntake {
    /// Take the three things a v1 build publishes.
    ///
    /// `lock_path` and `guest_image_path` are where the CALLER holds the files
    /// now; the receipt's own paths are not used to locate anything.
    pub fn from_build_output(
        lock_path: PathBuf,
        guest_image_path: PathBuf,
        receipt: V1MaterializationReceipt,
    ) -> Self {
        Self {
            lock_path,
            guest_image_path,
            receipt,
        }
    }

    /// Verify, or refuse. See the module doc for exactly which guarantee this
    /// produces.
    pub fn verify(&self) -> Result<VerifiedV1BuildInput, V1IntakeRefusal> {
        // 1. Read the lock through the trusted-load path. This alone covers the
        //    lock schema version, `lock_id`, the D5 environment-value
        //    re-derivation and the ADR-014 program-identity link. Hand-rolling
        //    any of it here would be a second opinion that can drift from the
        //    one every other reader gets.
        let lock = capsule_lock::load_verified_from_path(&self.lock_path).map_err(|source| {
            V1IntakeRefusal::LockUnreadable {
                path: self.lock_path.clone(),
                reason: source.to_string(),
            }
        })?;

        let Some(envelope) = lock.execution_contract.as_ref() else {
            return Err(V1IntakeRefusal::LockCarriesNoExecutionContract {
                path: self.lock_path.clone(),
            });
        };

        // 2. Take the identity from a recomputation of the contract's canonical
        //    bytes, NOT from the envelope's stored field. The trusted load
        //    above already refused a lock whose stored id disagreed, so this
        //    cannot normally fail — the point is not the check, it is where the
        //    value in `VerifiedV1BuildInput` comes from. An id that was only
        //    ever read out of a field would make this type's central claim rest
        //    on another function having been called first.
        //
        //    `verified_execution_id` also runs `ExecutionContractV1::validate`,
        //    which is where `launch.argv` being non-empty with no blank word,
        //    `readonly_layers` being non-empty, the schema string, and the
        //    env-name/secret-binding disjointness are all enforced — so those
        //    are checked here without being restated.
        let verified_id = envelope.verified_execution_id().map_err(|source| {
            V1IntakeRefusal::ContractVerificationFailed {
                path: self.lock_path.clone(),
                source,
            }
        })?;
        let execution_id = verified_id.as_execution_id().clone();
        let contract = &envelope.execution_contract;

        // 3. Refuse every facet outside the ADR-015 §7 subset, by name.
        refuse_facets_outside_the_v1_subset(contract)?;

        // 4. Refuse a target this builder cannot boot.
        let target = verify_guest_target(&contract.target)?;

        // 5. Cross-check the receipt against the contract.
        let receipt = self.verify_receipt_against_contract(contract, execution_id.as_str())?;

        // 6. Measure the artifact last: it is the only step that reads the
        //    whole image.
        let guest_artifact = self.verify_guest_artifact()?;

        Ok(VerifiedV1BuildInput {
            execution_id,
            guest_artifact,
            launch: VerifiedLaunchContract {
                argv: contract.launch.argv.clone(),
                cwd: contract.launch.cwd.clone(),
            },
            target,
            filesystem_view_digest: GuestFilesystemViewDigest::from_contract(contract),
            materialization_receipt: receipt,
            contract: contract.clone(),
        })
    }

    fn verify_receipt_against_contract(
        &self,
        contract: &ExecutionContractV1,
        lock_execution_id: &str,
    ) -> Result<VerifiedMaterializationReceipt, V1IntakeRefusal> {
        let receipt = &self.receipt;

        // The producer's own read-back claim. Absent, this receipt describes a
        // build that never confirmed it published what it minted.
        if !receipt.trusted_load_verified {
            return Err(V1IntakeRefusal::ReceiptNotTrustedLoadVerified);
        }

        if receipt.execution_id != lock_execution_id {
            return Err(V1IntakeRefusal::ReceiptDisagreesWithLock {
                field: "execution_id",
                receipt: receipt.execution_id.clone(),
                lock: lock_execution_id.to_string(),
            });
        }

        // The IDENTITY digest. Compared as a parsed digest, not as text, so a
        // differently-spelled but equal value cannot read as a mismatch.
        let receipt_view = parse_digest("filesystem_view_digest", &receipt.filesystem_view_digest)?;
        if receipt_view != contract.filesystem.view_digest {
            return Err(V1IntakeRefusal::ReceiptDisagreesWithLock {
                field: "filesystem.view_digest",
                receipt: receipt_view.to_string(),
                lock: contract.filesystem.view_digest.to_string(),
            });
        }

        let receipt_source = parse_digest("source_digest", &receipt.source_digest)?;
        if receipt_source != contract.source.digest {
            return Err(V1IntakeRefusal::ReceiptDisagreesWithLock {
                field: "source.digest",
                receipt: receipt_source.to_string(),
                lock: contract.source.digest.to_string(),
            });
        }

        let contract_target = target_triple(&contract.target);
        if receipt.target != contract_target {
            return Err(V1IntakeRefusal::ReceiptDisagreesWithLock {
                field: "target",
                receipt: receipt.target.clone(),
                lock: contract_target,
            });
        }

        verify_runtime_ref(&receipt.runtime, contract)?;

        // Parsed here so a malformed value is refused as part of the receipt
        // check rather than surfacing later as an artifact mismatch.
        let _ = parse_digest("guest_image_digest", &receipt.guest_image_digest)?;

        Ok(VerifiedMaterializationReceipt {
            receipt: receipt.clone(),
        })
    }

    fn verify_guest_artifact(&self) -> Result<VerifiedGuestArtifact, V1IntakeRefusal> {
        let metadata = std::fs::metadata(&self.guest_image_path).map_err(|source| {
            V1IntakeRefusal::ArtifactUnreadable {
                path: self.guest_image_path.clone(),
                reason: source.to_string(),
            }
        })?;

        // Cheap first: a length mismatch is the common truncation case and
        // costs one stat rather than a full read.
        if metadata.len() != self.receipt.guest_image_bytes {
            return Err(V1IntakeRefusal::ArtifactLengthMismatch {
                path: self.guest_image_path.clone(),
                receipt: self.receipt.guest_image_bytes,
                actual: metadata.len(),
            });
        }

        let measured = measure_guest_artifact(&self.guest_image_path).map_err(|source| {
            V1IntakeRefusal::ArtifactUnreadable {
                path: self.guest_image_path.clone(),
                reason: source.to_string(),
            }
        })?;
        let expected = parse_digest("guest_image_digest", &self.receipt.guest_image_digest)?;

        if measured.as_content_digest() != expected {
            return Err(V1IntakeRefusal::ArtifactDigestMismatch {
                path: self.guest_image_path.clone(),
                receipt: expected.to_string(),
                measured: measured.to_string(),
            });
        }

        Ok(VerifiedGuestArtifact {
            path: self.guest_image_path.clone(),
            bytes: metadata.len(),
            digest: measured,
        })
    }
}

fn parse_digest(field: &'static str, value: &str) -> Result<ContentDigest, V1IntakeRefusal> {
    ContentDigest::try_from(value.to_string()).map_err(|_| V1IntakeRefusal::MalformedReceiptValue {
        field,
        value: value.to_string(),
    })
}

/// Refuse everything outside the ADR-015 §7 vertical subset.
///
/// The wire format makes this a total rule: ADR-015 §6.3 forbids an empty
/// collection on the wire, so every one of these facets is either absent or
/// non-empty. "Present" therefore means "declared", with no empty-versus-absent
/// ambiguity to get wrong.
///
/// `ato#1089` tracks the producers these facets are waiting on. Until they
/// exist, a contract declaring one is refused here rather than accepted with
/// the facet quietly dropped.
fn refuse_facets_outside_the_v1_subset(
    contract: &ExecutionContractV1,
) -> Result<(), V1IntakeRefusal> {
    if !contract.dependencies.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "dependencies[]",
            why: "per-dependency derivation and output digests have no producer yet, so this \
                  builder cannot confirm the dependency it would launch is the one the identity \
                  commits",
        });
    }
    if !contract.build_outputs.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "build_outputs[]",
            why: "a build output has no guest placement in this contract version, so its \
                  projection could not be checked against the guest",
        });
    }
    if !contract.external_state.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "external_state[]",
            why: "RFC §8.3 refuses a running capture of a workload with external state — it needs \
                  `workload_idle`, which is a separate lifecycle",
        });
    }
    if !contract.launch.secret_bindings.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "launch.secret_bindings",
            why: "a restore-time binding would be sealed into bytes many users restore, so a \
                  contract declaring one cannot be captured live",
        });
    }
    if !contract.target.observable_features.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "target.observable_features",
            why: "no producer measures observable CPU/platform features yet, so this builder \
                  cannot confirm the host it would boot on provides them",
        });
    }
    if !contract.guest_surface.features.is_empty() {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "guest_surface.features",
            why: "ADR-015 §6.2 omits guest-surface features from the v1 subset, so a contract \
                  declaring one describes a surface this builder does not implement",
        });
    }
    // `validate` already proved this is non-empty; the subset additionally
    // ships exactly one layer, and more than one would mean a mount topology
    // this builder does not assemble.
    if contract.filesystem.readonly_layers.len() != 1 {
        return Err(V1IntakeRefusal::UnsupportedFacet {
            feature: "filesystem.readonly_layers",
            why: "the v1 subset ships exactly one rootfs layer; a multi-layer topology has no \
                  assembler in this builder",
        });
    }
    Ok(())
}

/// Refuse a target this builder cannot boot.
///
/// Mirrors the producer's `verify_target_agrees_with_runtime`: the ABI must be
/// the one the measured libc implies, never a default. An unclassifiable libc
/// is a refusal on both sides.
fn verify_guest_target(
    target: &ResolvedTargetContract,
) -> Result<VerifiedGuestTarget, V1IntakeRefusal> {
    if target.os != "linux" {
        return Err(V1IntakeRefusal::UnsupportedGuestTarget {
            detail: format!("os {}", target.os),
        });
    }
    let libc = target.libc.as_deref();
    let consistent = match (target.abi.as_str(), libc) {
        ("gnu", Some(libc)) => libc.starts_with("glibc"),
        ("musl", Some(libc)) => libc.starts_with("musl"),
        _ => false,
    };
    if !consistent {
        return Err(V1IntakeRefusal::UnsupportedGuestTarget {
            detail: format!("abi {} with libc {:?}", target.abi, libc),
        });
    }
    Ok(VerifiedGuestTarget {
        target: target.clone(),
    })
}

/// The receipt's runtime ref must be digest-pinned, and that digest must be the
/// runtime artifact the contract commits.
///
/// The ref itself is NOT an identity input — two refs resolving to one artifact
/// are the same execution, which is why it lives on the receipt rather than in
/// the contract. What matters is that it names the same artifact.
fn verify_runtime_ref(
    runtime_ref: &str,
    contract: &ExecutionContractV1,
) -> Result<(), V1IntakeRefusal> {
    let Some((_, digest)) = runtime_ref.split_once('@') else {
        return Err(V1IntakeRefusal::RuntimeRefNotPinned {
            runtime: runtime_ref.to_string(),
        });
    };
    let pinned = ContentDigest::try_from(digest.to_string()).map_err(|_| {
        V1IntakeRefusal::RuntimeRefNotPinned {
            runtime: runtime_ref.to_string(),
        }
    })?;
    if pinned != contract.runtime.digest {
        return Err(V1IntakeRefusal::RuntimeRefDisagreesWithContract {
            runtime: runtime_ref.to_string(),
            contract_digest: contract.runtime.digest.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::capsule_lock::CapsuleLock;
    use capsule::execution_contract::{
        DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, ExecutionContractEnvelopeV1,
        GuestSurfaceContract, OpaqueContractDomainV1, ResolvedArtifactContract,
        ResolvedFilesystemContract, ResolvedLaunchContract, ResolvedPolicyContract,
        ResolvedSourceContract, opaque_subcontract_digest,
    };
    use std::collections::BTreeMap;

    const ARTIFACT_BYTES: &[u8] = b"a packed guest image, as far as these tests are concerned";

    fn placeholder_opaque() -> capsule::execution_contract::OpaqueContractDigestV1 {
        opaque_subcontract_digest(
            OpaqueContractDomainV1::SourceProjection,
            &serde_json::json!({}),
        )
        .expect("placeholder opaque digest")
    }

    fn runtime_digest() -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Sha256, [2u8; 32])
    }

    /// A contract inside the §7 subset: no dependencies, no build outputs, no
    /// external state, no secret bindings, exactly one readonly layer.
    fn subset_contract() -> ExecutionContractV1 {
        let placeholder = placeholder_opaque();
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: ContentDigest::new(DigestAlgorithm::Sha256, [1u8; 32]),
                projection_digest: placeholder,
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc-2.39".to_string()),
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "python".to_string(),
                digest: runtime_digest(),
                dynamic_contract_digest: placeholder,
            },
            dependencies: Vec::new(),
            build_outputs: Vec::new(),
            launch: ResolvedLaunchContract {
                argv: vec![
                    "python3".to_string(),
                    "server.py".to_string(),
                    "--label".to_string(),
                    "step 6".to_string(),
                ],
                cwd: GuestPath::parse("/app").expect("cwd"),
                process_model_digest: placeholder,
                environment: Vec::new(),
                environment_policy_digest: placeholder,
                secret_bindings: Vec::new(),
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
                topology_digest: placeholder,
                readonly_layers: vec![ContentDigest::new(DigestAlgorithm::Blake3, [8u8; 32])],
                writable_paths: Vec::new(),
            },
            policy: ResolvedPolicyContract {
                network_digest: placeholder,
                capability_digest: placeholder,
                filesystem_digest: placeholder,
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato.web-surface.v1".to_string(),
                port: std::num::NonZeroU16::new(8080),
                features: Vec::new(),
            },
            external_state: Vec::new(),
        }
    }

    /// A build output on disk: a published lock, a packed artifact, and the
    /// receipt naming it — the exact three things slice 7A takes in.
    struct BuildOutput {
        _dir: tempfile::TempDir,
        lock_path: PathBuf,
        guest_image_path: PathBuf,
        receipt: V1MaterializationReceipt,
    }

    impl BuildOutput {
        fn publish(contract: ExecutionContractV1) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let lock_path = dir.path().join("capsule.lock");
            let guest_image_path = dir.path().join("guest.ext4");

            let execution_id = contract
                .compute_execution_id()
                .expect("compute execution id");
            let envelope = ExecutionContractEnvelopeV1 {
                execution_contract: contract.clone(),
                execution_id: execution_id.clone(),
                capsule_program_id: None,
                resolved_refs: Default::default(),
                generated_at: None,
                provenance: serde_json::Value::Null,
                diagnostics: serde_json::Value::Null,
                evidence: serde_json::Value::Null,
            };
            let lock = CapsuleLock {
                execution_contract: Some(envelope),
                ..CapsuleLock::default()
            };
            capsule_lock::write_pretty_to_path(&lock, &lock_path).expect("publish lock");

            std::fs::write(&guest_image_path, ARTIFACT_BYTES).expect("write artifact");
            let artifact_digest = measure_guest_artifact(&guest_image_path).expect("measure");

            let receipt = V1MaterializationReceipt {
                execution_id: execution_id.as_str().to_string(),
                lock: lock_path.clone(),
                guest_image: guest_image_path.clone(),
                guest_image_bytes: ARTIFACT_BYTES.len() as u64,
                guest_image_digest: artifact_digest.to_string(),
                filesystem_view_digest: contract.filesystem.view_digest.to_string(),
                source_digest: contract.source.digest.to_string(),
                runtime: format!("python@{}", contract.runtime.digest),
                target: target_triple(&contract.target),
                trusted_load_verified: true,
            };

            Self {
                _dir: dir,
                lock_path,
                guest_image_path,
                receipt,
            }
        }

        fn intake(&self) -> V1BuildIntake {
            V1BuildIntake::from_build_output(
                self.lock_path.clone(),
                self.guest_image_path.clone(),
                self.receipt.clone(),
            )
        }

        fn intake_with(&self, mutate: impl FnOnce(&mut V1MaterializationReceipt)) -> V1BuildIntake {
            let mut receipt = self.receipt.clone();
            mutate(&mut receipt);
            V1BuildIntake::from_build_output(
                self.lock_path.clone(),
                self.guest_image_path.clone(),
                receipt,
            )
        }
    }

    // -- the accepting case ------------------------------------------------

    /// A build inside the subset, with a receipt that agrees and an artifact
    /// whose bytes are the ones it names, verifies.
    ///
    /// Worth stating as a test rather than assuming: a gate that refuses
    /// everything is trivially "fail-closed" and completely useless.
    #[test]
    fn a_v1_build_output_that_agrees_with_itself_verifies() {
        let build = BuildOutput::publish(subset_contract());

        let verified = build.intake().verify().expect("verify");

        let contract = subset_contract();
        assert_eq!(
            verified.execution_id().as_str(),
            contract.compute_execution_id().expect("id").as_str()
        );
        assert_eq!(
            verified.launch().argv(),
            ["python3", "server.py", "--label", "step 6"]
        );
        assert_eq!(verified.launch().cwd().as_str(), "/app");
        assert_eq!(verified.target().os(), "linux");
        assert_eq!(verified.target().abi(), "gnu");
        assert_eq!(
            verified.guest_artifact().bytes(),
            ARTIFACT_BYTES.len() as u64
        );
    }

    /// The word carrying a space survives as ONE argument.
    ///
    /// The same property ADR-015 §9.5 gates the KVM E2E on, asserted here at
    /// the intake boundary so a regression is caught without hardware.
    #[test]
    fn an_argv_word_containing_a_space_stays_one_word() {
        let build = BuildOutput::publish(subset_contract());
        let verified = build.intake().verify().expect("verify");
        assert_eq!(verified.launch().argv()[3], "step 6");
    }

    /// The identity digest and the artifact digest are different values, and
    /// the verified input keeps them apart.
    ///
    /// This is the substitution ADR-015 §9.2 exists to prevent: committing the
    /// packed image's digest would make an `apt upgrade` on a builder change
    /// every capsule's identity. The types differ so the swap cannot compile,
    /// and this test pins that they carry the values their names claim.
    #[test]
    fn the_identity_digest_and_the_materialization_digest_are_not_interchanged() {
        let build = BuildOutput::publish(subset_contract());
        let verified = build.intake().verify().expect("verify");

        assert_eq!(
            verified.filesystem_view_digest().as_content_digest(),
            subset_contract().filesystem.view_digest
        );
        assert_eq!(
            verified.guest_artifact().digest(),
            measure_guest_artifact(&build.guest_image_path).expect("measure")
        );
        assert_ne!(
            verified.filesystem_view_digest().as_content_digest(),
            verified.guest_artifact().digest().as_content_digest()
        );
    }

    /// The artifact is located where the CALLER says, not where the receipt
    /// says — the receipt's path names a file on the producer's host.
    #[test]
    fn the_artifact_is_found_at_the_callers_path_not_the_receipts() {
        let build = BuildOutput::publish(subset_contract());
        let elsewhere = build._dir.path().join("copied-into-the-jobdir.ext4");
        std::fs::copy(&build.guest_image_path, &elsewhere).expect("copy");

        let mut receipt = build.receipt.clone();
        receipt.guest_image = PathBuf::from("/on/another/host/guest.ext4");

        let verified =
            V1BuildIntake::from_build_output(build.lock_path.clone(), elsewhere.clone(), receipt)
                .verify()
                .expect("verify");

        assert_eq!(verified.guest_artifact().path(), elsewhere);
        assert_eq!(
            verified
                .materialization_receipt()
                .producer_guest_image_path(),
            Path::new("/on/another/host/guest.ext4")
        );
    }

    // -- unsupported facets, refused by name -------------------------------

    /// Every facet outside the §7 subset is refused BY NAME.
    ///
    /// The dangerous alternative is accepting the contract and ignoring the
    /// facet: the guest then runs without something its own identity says it
    /// has, and the `execution_id` still matches, so nothing downstream can
    /// notice. `ato#1089` tracks the producers these are waiting on.
    #[test]
    fn each_facet_outside_the_v1_subset_is_refused_by_name() {
        use capsule::execution_contract::{
            ExternalStateAccess, ExternalStateContract, ResolvedBuildOutputContract,
            ResolvedDependencyContract, SnapshotExclusion,
        };

        /// A facet name paired with the edit that puts that facet into a
        /// contract which is otherwise inside the subset.
        type DeclareFacet = (&'static str, Box<dyn Fn(&mut ExecutionContractV1)>);

        let cases: Vec<DeclareFacet> = vec![
            (
                "dependencies[]",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.dependencies.push(ResolvedDependencyContract {
                        name: "pip".to_string(),
                        derivation_digest: ContentDigest::new(DigestAlgorithm::Blake3, [3u8; 32]),
                        output_digest: ContentDigest::new(DigestAlgorithm::Blake3, [4u8; 32]),
                    });
                }),
            ),
            (
                "build_outputs[]",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.build_outputs.push(ResolvedBuildOutputContract {
                        name: "app".to_string(),
                        digest: ContentDigest::new(DigestAlgorithm::Blake3, [5u8; 32]),
                        projection_digest: placeholder_opaque(),
                    });
                }),
            ),
            (
                "external_state[]",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.external_state.push(ExternalStateContract {
                        name: "data".to_string(),
                        target: GuestPath::parse("/data").expect("path"),
                        access: ExternalStateAccess::ReadWrite,
                        schema: "1".to_string(),
                        snapshot: SnapshotExclusion::Exclude,
                    });
                }),
            ),
            (
                "launch.secret_bindings",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.launch.secret_bindings.push("API_TOKEN".to_string());
                }),
            ),
            (
                "target.observable_features",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.target
                        .observable_features
                        .insert("avx512".to_string(), "required".to_string());
                }),
            ),
            (
                "guest_surface.features",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.guest_surface.features.push("exec".to_string());
                }),
            ),
            (
                "filesystem.readonly_layers",
                Box::new(|c: &mut ExecutionContractV1| {
                    c.filesystem
                        .readonly_layers
                        .push(ContentDigest::new(DigestAlgorithm::Blake3, [9u8; 32]));
                }),
            ),
        ];

        for (expected_feature, mutate) in cases {
            let mut contract = subset_contract();
            mutate(&mut contract);
            let build = BuildOutput::publish(contract);

            match build.intake().verify() {
                Err(V1IntakeRefusal::UnsupportedFacet { feature, why }) => {
                    assert_eq!(
                        feature, expected_feature,
                        "refused the wrong facet for {expected_feature}"
                    );
                    assert!(
                        !why.is_empty(),
                        "{expected_feature} was refused without a reason"
                    );
                }
                other => panic!("{expected_feature} was not refused as unsupported: {other:?}"),
            }
        }
    }

    /// The subset gate subsumes the live-capture eligibility rule.
    ///
    /// `requires_restore_time_bindings_for_live_workload` is the existing
    /// answer to "may this be captured while running". This asserts the two
    /// cannot drift apart: anything that rule refuses, the subset gate refuses
    /// too.
    #[test]
    fn the_subset_gate_refuses_everything_the_live_capture_rule_refuses() {
        let mut with_state = subset_contract();
        with_state
            .launch
            .secret_bindings
            .push("API_TOKEN".to_string());
        assert!(
            snapshot::external_state::requires_restore_time_bindings_for_live_workload(&with_state)
        );
        assert!(refuse_facets_outside_the_v1_subset(&with_state).is_err());

        let clean = subset_contract();
        assert!(
            !snapshot::external_state::requires_restore_time_bindings_for_live_workload(&clean)
        );
        assert!(refuse_facets_outside_the_v1_subset(&clean).is_ok());
    }

    // -- the lock ----------------------------------------------------------

    /// A lock that carries no contract minted no identity, so there is nothing
    /// to verify against.
    #[test]
    fn a_lock_without_an_execution_contract_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("capsule.lock");
        capsule_lock::write_pretty_to_path(&CapsuleLock::default(), &lock_path).expect("publish");
        let guest = dir.path().join("guest.ext4");
        std::fs::write(&guest, ARTIFACT_BYTES).expect("write");

        let receipt = BuildOutput::publish(subset_contract()).receipt.clone();
        let intake = V1BuildIntake::from_build_output(lock_path, guest, receipt);

        assert!(matches!(
            intake.verify(),
            Err(V1IntakeRefusal::LockCarriesNoExecutionContract { .. })
        ));
    }

    /// A lock that is not there at all is a refusal, not a panic.
    #[test]
    fn a_missing_lock_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = V1BuildIntake::from_build_output(
            build._dir.path().join("no-such.lock"),
            build.guest_image_path.clone(),
            build.receipt.clone(),
        );
        assert!(matches!(
            intake.verify(),
            Err(V1IntakeRefusal::LockUnreadable { .. })
        ));
    }

    /// A contract whose stored `execution_id` is not the one its own bytes
    /// hash to is refused.
    ///
    /// Reported as `LockUnreadable`, not `ContractVerificationFailed`: the
    /// trusted load recomputes the id before this module sees the envelope, so
    /// tampering is caught one layer down. Asserted precisely rather than as
    /// "some refusal" so that if a future loader stops verifying envelopes,
    /// this test changes shape and says so instead of quietly passing on the
    /// other arm.
    #[test]
    fn a_contract_that_does_not_hash_to_its_stored_execution_id_is_refused() {
        let build = BuildOutput::publish(subset_contract());

        // Rewrite the published lock so the contract says something different
        // from what the recorded id commits. `write_pretty_to_path` would
        // refuse to produce this, which is why it is edited as text.
        let raw = std::fs::read_to_string(&build.lock_path).expect("read lock");
        let tampered = raw.replace("server.py", "other.py");
        assert_ne!(raw, tampered, "the fixture did not contain the edited word");
        std::fs::write(&build.lock_path, tampered).expect("write lock");

        match build.intake().verify() {
            Err(V1IntakeRefusal::LockUnreadable { .. }) => {}
            other => panic!("expected the trusted load to refuse the tampered lock, got {other:?}"),
        }
    }

    /// The identity handed on is the one the contract's bytes hash to.
    ///
    /// Pins that `VerifiedV1BuildInput::execution_id` comes from a
    /// recomputation and not from the envelope's stored field — the two agree
    /// on a good lock, so only an independent recomputation here can tell them
    /// apart.
    #[test]
    fn the_execution_id_handed_on_is_recomputed_from_the_contract() {
        let build = BuildOutput::publish(subset_contract());
        let verified = build.intake().verify().expect("verify");

        let recomputed = subset_contract()
            .compute_execution_id()
            .expect("recompute execution id");
        assert_eq!(verified.execution_id(), &recomputed);
    }

    // -- the receipt -------------------------------------------------------

    /// A receipt naming a different execution than the lock is refused.
    ///
    /// This is the stale-receipt case: both halves verify individually and
    /// describe perfectly good builds — just not the same one.
    #[test]
    fn a_receipt_naming_another_execution_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| {
            r.execution_id = format!("blake3:{}", "f".repeat(64));
        });

        match intake.verify() {
            Err(V1IntakeRefusal::ReceiptDisagreesWithLock { field, .. }) => {
                assert_eq!(field, "execution_id");
            }
            other => panic!("expected an execution_id disagreement, got {other:?}"),
        }
    }

    /// A receipt disagreeing about the IDENTITY digest is refused.
    #[test]
    fn a_receipt_disagreeing_about_the_view_digest_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| {
            r.filesystem_view_digest =
                ContentDigest::new(DigestAlgorithm::Blake3, [0xAAu8; 32]).to_string();
        });

        match intake.verify() {
            Err(V1IntakeRefusal::ReceiptDisagreesWithLock { field, .. }) => {
                assert_eq!(field, "filesystem.view_digest");
            }
            other => panic!("expected a view_digest disagreement, got {other:?}"),
        }
    }

    /// A receipt disagreeing about the source is refused.
    #[test]
    fn a_receipt_disagreeing_about_the_source_digest_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| {
            r.source_digest = ContentDigest::new(DigestAlgorithm::Sha256, [0xBBu8; 32]).to_string();
        });

        match intake.verify() {
            Err(V1IntakeRefusal::ReceiptDisagreesWithLock { field, .. }) => {
                assert_eq!(field, "source.digest");
            }
            other => panic!("expected a source.digest disagreement, got {other:?}"),
        }
    }

    /// A receipt disagreeing about the target triple is refused.
    #[test]
    fn a_receipt_disagreeing_about_the_target_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| r.target = "linux/arm64/gnu".to_string());

        match intake.verify() {
            Err(V1IntakeRefusal::ReceiptDisagreesWithLock { field, .. }) => {
                assert_eq!(field, "target");
            }
            other => panic!("expected a target disagreement, got {other:?}"),
        }
    }

    /// A producer that never confirmed its own read-back is not trusted.
    #[test]
    fn a_receipt_without_trusted_load_verified_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| r.trusted_load_verified = false);
        assert!(matches!(
            intake.verify(),
            Err(V1IntakeRefusal::ReceiptNotTrustedLoadVerified)
        ));
    }

    /// A digest that is not a digest is refused as malformed, never coerced.
    #[test]
    fn a_receipt_carrying_a_malformed_digest_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| r.filesystem_view_digest = "not-a-digest".to_string());

        match intake.verify() {
            Err(V1IntakeRefusal::MalformedReceiptValue { field, .. }) => {
                assert_eq!(field, "filesystem_view_digest");
            }
            other => panic!("expected a malformed value, got {other:?}"),
        }
    }

    // -- the runtime ref ---------------------------------------------------

    /// An unpinned runtime ref cannot name the artifact the identity commits.
    #[test]
    fn an_unpinned_runtime_ref_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| r.runtime = "python:3.12".to_string());
        assert!(matches!(
            intake.verify(),
            Err(V1IntakeRefusal::RuntimeRefNotPinned { .. })
        ));
    }

    /// A pinned ref naming a DIFFERENT artifact than the contract is refused.
    ///
    /// A guest serving the right port while running another runtime is exactly
    /// the failure an Execution Identity exists to make impossible.
    #[test]
    fn a_runtime_ref_pinned_to_another_artifact_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        let intake = build.intake_with(|r| {
            r.runtime = format!(
                "python@{}",
                ContentDigest::new(DigestAlgorithm::Sha256, [0xCCu8; 32])
            );
        });
        assert!(matches!(
            intake.verify(),
            Err(V1IntakeRefusal::RuntimeRefDisagreesWithContract { .. })
        ));
    }

    // -- the artifact ------------------------------------------------------

    /// An artifact whose bytes are not the ones the receipt names is refused.
    #[test]
    fn an_artifact_whose_bytes_are_not_the_ones_the_receipt_names_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        // Same length, different content — so this can only be caught by the
        // digest, not by the cheap length check.
        let mut swapped = ARTIFACT_BYTES.to_vec();
        let last = swapped.len() - 1;
        swapped[last] ^= 0xFF;
        std::fs::write(&build.guest_image_path, &swapped).expect("rewrite");

        match build.intake().verify() {
            Err(V1IntakeRefusal::ArtifactDigestMismatch { .. }) => {}
            other => panic!("expected a digest mismatch, got {other:?}"),
        }
    }

    /// A truncated artifact is refused on length, before the whole image is
    /// read.
    #[test]
    fn a_truncated_artifact_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        std::fs::write(&build.guest_image_path, &ARTIFACT_BYTES[..8]).expect("truncate");

        match build.intake().verify() {
            Err(V1IntakeRefusal::ArtifactLengthMismatch {
                receipt, actual, ..
            }) => {
                assert_eq!(receipt, ARTIFACT_BYTES.len() as u64);
                assert_eq!(actual, 8);
            }
            other => panic!("expected a length mismatch, got {other:?}"),
        }
    }

    /// A missing artifact is a refusal, not a panic.
    #[test]
    fn a_missing_artifact_is_refused() {
        let build = BuildOutput::publish(subset_contract());
        std::fs::remove_file(&build.guest_image_path).expect("remove");
        assert!(matches!(
            build.intake().verify(),
            Err(V1IntakeRefusal::ArtifactUnreadable { .. })
        ));
    }

    // -- the target --------------------------------------------------------

    /// A non-Linux guest has no boot path here.
    #[test]
    fn a_non_linux_target_is_refused() {
        let mut contract = subset_contract();
        contract.target.os = "darwin".to_string();
        let build = BuildOutput::publish(contract);
        assert!(matches!(
            build.intake().verify(),
            Err(V1IntakeRefusal::UnsupportedGuestTarget { .. })
        ));
    }

    /// An ABI the measured libc does not imply is refused rather than
    /// defaulted — the same rule the producer applies (ADR-015 §9.4).
    #[test]
    fn an_abi_that_disagrees_with_the_libc_is_refused() {
        let mut contract = subset_contract();
        contract.target.abi = "musl".to_string();
        contract.target.libc = Some("glibc-2.39".to_string());
        let build = BuildOutput::publish(contract);
        assert!(matches!(
            build.intake().verify(),
            Err(V1IntakeRefusal::UnsupportedGuestTarget { .. })
        ));
    }

    /// A target with no libc at all cannot resolve an ABI, so it is refused.
    #[test]
    fn a_target_without_a_libc_is_refused() {
        let mut contract = subset_contract();
        contract.target.libc = None;
        let build = BuildOutput::publish(contract);
        assert!(matches!(
            build.intake().verify(),
            Err(V1IntakeRefusal::UnsupportedGuestTarget { .. })
        ));
    }
}
