//! Replay an authenticated retained object through the common launcher/attempt.
//! No source fetch, detector, authoring, build, or old verification receipt.
use crate::{
    build_sandbox::NetworkPolicy,
    executor::ExecutedCandidate,
    formation_realizer::CandidateLauncher,
    plan::{BoundCandidate, PlannedCandidate},
    realize::{CandidateRealizer, RealizeFailure, Realized},
    static_lane::StaticFormationOutput,
};
use anyhow::{Context, Result, ensure};
use ato_formation::{
    execution::lower_retained,
    request::{AttemptFailure, RuntimeProfile},
    retained::{RetainedCandidateV1, RetainedShape, content_ref},
    source::{FileVerifiedArchive, SourceLimits},
};
use ato_materializer_static_web::{
    ProducedStaticWebBundle, StaticWebBundleReceiptV1, StaticWebManifestV1,
};
use std::{fs, path::Path};

/// Construct only after the transport caller checks owner, ready state, current
/// ticket assignment and fence. This type independently verifies all bytes and
/// canonical K/D provenance; a descriptor is not a capability.
pub struct RetainedCandidateRealizer<'a> {
    pub descriptor: &'a RetainedCandidateV1,
    pub archive: &'a Path,
    pub expected_contract_ref: &'a str,
    pub expected_derivation_ref: &'a str,
    pub expanded_limit: u64,
    pub shim: &'a Path,
}
impl CandidateRealizer for RetainedCandidateRealizer<'_> {
    fn admit(&self, profile: &RuntimeProfile) -> Option<AttemptFailure> {
        let refuse = |code: &str, message: String| {
            Some(AttemptFailure {
                code: code.into(),
                stage: "admission".into(),
                message,
            })
        };
        if let Err(error) = self
            .descriptor
            .match_assignment(self.expected_contract_ref, self.expected_derivation_ref)
        {
            return refuse("retained_provenance_invalid", error.to_string());
        }
        if !self.archive.is_file() {
            return refuse(
                "retained_object_missing",
                "retained object is unavailable; source fallback is forbidden".into(),
            );
        }
        if self.descriptor.artifact.expanded_bytes > self.expanded_limit {
            return refuse(
                "search_expanded_budget_exceeded",
                "retained artifact exceeds ticket cap".into(),
            );
        }
        if let RetainedShape::ProcessWorkspace { .. } = &self.descriptor.shape {
            if profile.get("formation.containment") != Some("bwrap+landlock") {
                return refuse(
                    "runtime_cannot_contain_candidate",
                    "retained process requires containment".into(),
                );
            }
            let plan = match lower_retained(self.descriptor) {
                Ok(plan) => plan,
                Err(error) => return refuse("retained_binding_invalid", error.to_string()),
            };
            // Replay never installs a missing toolchain or invokes a build.
            for directory in plan.toolchain_path {
                if !Path::new(&directory).is_dir() {
                    return refuse(
                        "retained_toolchain_unavailable",
                        format!("required toolchain is unavailable: {directory}"),
                    );
                }
            }
        }
        None
    }
    fn realize(&self, attempt_id: &str, attempt_root: &Path) -> Result<Realized, RealizeFailure> {
        let result = (|| -> Result<_> {
            self.descriptor
                .match_assignment(self.expected_contract_ref, self.expected_derivation_ref)?;
            let limits = SourceLimits {
                max_total_bytes: self.expanded_limit.min(512 * 1024 * 1024),
                ..Default::default()
            };
            let mut archive = FileVerifiedArchive::verify(
                fs::File::open(self.archive).context("retained object missing")?,
                &self.descriptor.artifact.content_ref,
                self.descriptor.artifact.bytes,
                limits,
            )?;
            ensure!(
                archive.expanded_bytes() == self.descriptor.artifact.expanded_bytes,
                "retained expanded size mismatch"
            );
            // Reuse the hardened archive boundary/path validator. The retained
            // artifact closure is NOT reinterpreted as the original source I.
            let root = archive.materialize(&attempt_root.join("retained"), "", limits)?;
            let executed = match self.descriptor.shape {
                RetainedShape::ProcessWorkspace { .. } => ExecutedCandidate::Process {
                    workspace_root: root,
                },
                RetainedShape::StaticWeb => ExecutedCandidate::StaticWeb {
                    output: Box::new(validate_static(
                        &root,
                        &self.descriptor.materialization_ref,
                    )?),
                },
            };
            let planned = PlannedCandidate {
                bound: BoundCandidate {
                    contract: self.descriptor.base_contract.clone(),
                    derivation: self.descriptor.derivation.clone(),
                    contract_ref: self.descriptor.base_contract_ref.clone(),
                    derivation_ref: self.descriptor.derivation_ref.clone(),
                },
                plan: lower_retained(self.descriptor)?,
            };
            Ok((executed, planned))
        })()
        .map_err(|error| {
            RealizeFailure::Execution(
                ato_formation::failure::FormationFailure::new(
                    "retained_artifact_invalid",
                    ato_formation::failure::FailureStage::Admission,
                    format!("{error:#}"),
                )
                .into(),
            )
        })?;
        let (executed, planned) = result;
        let mut realized = CandidateLauncher {
            planned: &planned,
            shim: self.shim,
            network: NetworkPolicy::Denied,
        }
        .realize(executed, attempt_id, attempt_root)?;
        // This is an existing immutable object, not a new publication. A fresh
        // receipt still comes exclusively from the enclosing common attempt.
        realized.kept = None;
        Ok(realized)
    }
}

fn validate_static(root: &Path, expected_manifest: &str) -> Result<StaticFormationOutput> {
    let manifest_bytes = fs::read(root.join("manifest.json"))?;
    ensure!(
        content_ref(&manifest_bytes) == expected_manifest,
        "retained manifest digest mismatch"
    );
    let manifest: StaticWebManifestV1 = serde_json::from_slice(&manifest_bytes)?;
    ensure!(
        manifest.canonical_bytes()? == manifest_bytes,
        "retained manifest is not canonical"
    );
    let receipt_bytes = fs::read(root.join("receipt.json"))?;
    let receipt: StaticWebBundleReceiptV1 = serde_json::from_slice(&receipt_bytes)?;
    receipt.validate_for_manifest(&manifest)?;
    ensure!(
        receipt.canonical_bytes()? == receipt_bytes,
        "retained artifact receipt is not canonical"
    );
    for file in manifest.files.values() {
        let hex = file
            .blob
            .strip_prefix("sha256:")
            .context("invalid blob reference")?;
        let location = root.join("blobs/sha256").join(hex);
        ensure!(
            fs::symlink_metadata(&location)?.is_file(),
            "retained blob is not a regular file"
        );
        let bytes = fs::read(&location)?;
        ensure!(
            bytes.len() as u64 == file.size && content_ref(&bytes) == file.blob,
            "retained blob digest/size mismatch"
        );
    }
    Ok(StaticFormationOutput {
        served_paths: manifest.files.keys().cloned().collect(),
        manifest_digest: expected_manifest.into(),
        entry_path: manifest.entry_path.clone(),
        spa_fallback: manifest.routing.spa_fallback,
        total_bytes: manifest.files.values().map(|f| f.size).sum(),
        bundle: ProducedStaticWebBundle {
            bundle_root: root.into(),
            manifest_bytes,
            receipt_digest: content_ref(&receipt_bytes),
            receipt_bytes,
            receipt,
        },
    })
}
