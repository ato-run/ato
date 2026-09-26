//! Publication payload preparation; storage authorization belongs to Coordinator.
use anyhow::Result;
use ato_formation::{
    browser::{BrowserContractV0, effective_contract_ref},
    intent::Lane,
    retained::*,
    source::{DownloadedArchive, SourceLimits},
};
use ato_runtime_attempt::{executor::ExecutedCandidate, plan::PlannedCandidate};
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
    let (bytes, materialization_ref, shape, validation_profile) = match executed {
        ExecutedCandidate::Process { workspace_root } => {
            let bytes = crate::pack::pack_tree(workspace_root)?;
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
    let measured = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&content_ref(&bytes))?
        .verify_tree_digest(None, SourceLimits::default())?;
    let source = planned
        .derivation
        .inputs
        .iter()
        .find(|i| i.protocol == "ato.workspace@1")
        .ok_or_else(|| anyhow::anyhow!("retained candidate has no workspace provenance"))?;
    let descriptor = RetainedCandidateV1 {
        schema: RETAINED_SCHEMA.into(),
        shape,
        artifact: RetainedArtifact {
            content_ref: content_ref(&bytes),
            bytes: bytes.len() as u64,
            expanded_bytes: measured.expanded_bytes(),
        },
        materialization_ref,
        source_closure_ref: source.content_ref.clone(),
        derivation_ref: planned.derivation_ref.clone(),
        base_contract_ref: planned.contract_ref.clone(),
        contract_ref: effective_contract_ref(&planned.contract_ref, browser),
        derivation: planned.derivation.clone(),
        base_contract: planned.contract.clone(),
        browser_contract: browser.cloned(),
        creation_attempt_id: attempt_id.into(),
        validation_profile: validation_profile.into(),
    };
    descriptor.validate()?;
    Ok(PreparedRetained { descriptor, bytes })
}
