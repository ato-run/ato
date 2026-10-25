//! Publication payload preparation; storage authorization belongs to Coordinator.
use anyhow::Result;
use ato_formation::{
    browser::{BrowserContractV0, effective_contract_ref},
    intent::Lane,
    retained::*,
    source::{DownloadedArchive, SourceLimits},
};
use ato_runtime_attempt::{executor::ExecutedCandidate, plan::PlannedCandidate};
use serde::{Deserialize, Serialize};

/// Value-free provenance sufficient to finish publication after confirmed execution.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetentionProvenance {
    pub base_contract: ato_formation::authoring::BoundContract,
    pub derivation: ato_formation::authoring::BoundDerivation,
    pub browser_contract: Option<BrowserContractV0>,
    pub shape: RetainedShape,
}
impl RetentionProvenance {
    pub fn process(planned: &PlannedCandidate, browser: Option<&BrowserContractV0>) -> Self {
        Self {
            base_contract: planned.contract.clone(),
            derivation: planned.derivation.clone(),
            browser_contract: browser.cloned(),
            shape: RetainedShape::ProcessWorkspace {
                binding: RetainedProcessBinding {
                    toolchains: planned.plan.toolchains.clone(),
                    package_manager: planned.plan.package_manager.as_ref().map(|m| {
                        RetainedPackageManager {
                            name: m.name.clone(),
                            version: m.version.clone(),
                        }
                    }),
                    python_environment: planned.plan.lane == Lane::PythonProcess,
                },
            },
        }
    }
    pub fn contract_ref(&self) -> Result<String> {
        Ok(effective_contract_ref(
            &self.base_contract.contract_ref()?,
            self.browser_contract.as_ref(),
        ))
    }
    pub fn derivation_ref(&self) -> Result<String> {
        Ok(self.derivation.derivation_ref()?)
    }
}

pub(crate) fn prepare_process(
    workspace: &std::path::Path,
    provenance: &RetentionProvenance,
    attempt_id: &str,
    record: &crate::build_record::ReadyBuildRecord,
) -> Result<PreparedRetained> {
    anyhow::ensure!(
        matches!(provenance.shape, RetainedShape::ProcessWorkspace { .. }),
        "publication_shape_mismatch"
    );
    let bytes = crate::pack::pack_process_artifact_with_record(workspace, record)?;
    let materialization_ref = content_ref(&bytes);
    describe(
        bytes,
        materialization_ref,
        provenance,
        attempt_id,
        "ato.retained-process-workspace/1",
    )
}

fn describe(
    bytes: Vec<u8>,
    materialization_ref: String,
    provenance: &RetentionProvenance,
    attempt_id: &str,
    validation_profile: &str,
) -> Result<PreparedRetained> {
    let measured = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&content_ref(&bytes))?
        .verify_tree_digest(None, SourceLimits::default())?;
    let source = provenance
        .derivation
        .inputs
        .iter()
        .find(|i| i.protocol == "ato.workspace@1")
        .ok_or_else(|| anyhow::anyhow!("retained candidate has no workspace provenance"))?;
    let descriptor = RetainedCandidateV1 {
        schema: RETAINED_SCHEMA.into(),
        shape: provenance.shape.clone(),
        artifact: RetainedArtifact {
            content_ref: content_ref(&bytes),
            bytes: bytes.len() as u64,
            expanded_bytes: measured.expanded_bytes(),
        },
        materialization_ref,
        source_closure_ref: source.content_ref.clone(),
        derivation_ref: provenance.derivation_ref()?,
        base_contract_ref: provenance.base_contract.contract_ref()?,
        contract_ref: provenance.contract_ref()?,
        derivation: provenance.derivation.clone(),
        base_contract: provenance.base_contract.clone(),
        browser_contract: provenance.browser_contract.clone(),
        creation_attempt_id: attempt_id.into(),
        validation_profile: validation_profile.into(),
    };
    descriptor.validate()?;
    Ok(PreparedRetained { descriptor, bytes })
}
pub struct PreparedRetained {
    pub descriptor: RetainedCandidateV1,
    pub bytes: Vec<u8>,
}
pub fn prepare(
    executed: &ExecutedCandidate,
    planned: &PlannedCandidate,
    browser: Option<&BrowserContractV0>,
    attempt_id: &str,
) -> Result<PreparedRetained> {
    prepare_with_record(executed, planned, browser, attempt_id, None)
}

pub(crate) fn prepare_with_record(
    executed: &ExecutedCandidate,
    planned: &PlannedCandidate,
    browser: Option<&BrowserContractV0>,
    attempt_id: &str,
    record: Option<&crate::build_record::ReadyBuildRecord>,
) -> Result<PreparedRetained> {
    let (bytes, materialization_ref, shape, validation_profile) = match executed {
        ExecutedCandidate::Process { workspace_root } => {
            let bytes = if let Some(record) = record {
                crate::pack::pack_process_artifact_with_record(workspace_root, record)?
            } else {
                crate::pack::pack_process_artifact(workspace_root)?
            };
            let reference = content_ref(&bytes);
            (
                bytes,
                reference,
                RetainedShape::ProcessWorkspace {
                    binding: RetainedProcessBinding {
                        toolchains: planned.plan.toolchains.clone(),
                        package_manager: planned.plan.package_manager.as_ref().map(|m| {
                            RetainedPackageManager {
                                name: m.name.clone(),
                                version: m.version.clone(),
                            }
                        }),
                        python_environment: planned.plan.lane == Lane::PythonProcess,
                    },
                },
                "ato.retained-process-workspace/1",
            )
        }
        ExecutedCandidate::StaticWeb { output } => (
            crate::pack::pack_tree(&output.bundle.bundle_root)?,
            output.manifest_digest.clone(),
            RetainedShape::StaticWeb,
            "ato.retained-static-web/1",
        ),
    };
    describe(
        bytes,
        materialization_ref,
        &RetentionProvenance {
            base_contract: planned.contract.clone(),
            derivation: planned.derivation.clone(),
            browser_contract: browser.cloned(),
            shape,
        },
        attempt_id,
        validation_profile,
    )
}
