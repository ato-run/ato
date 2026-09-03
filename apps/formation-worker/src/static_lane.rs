//! The Static Formation lane.
//!
//! A Static Compute is evaluated by the browser: no process, no Runner lease,
//! no port. Formation's job for it is narrower than for a process — build if
//! the plan says to, then hand the declared output root to the canonical
//! materializer and record what it produced.
//!
//! ## The materializer is not reimplemented
//!
//! `ato-materializer-static-web` is the one on `main`, brought onto nightly by
//! I0 with its manifest, receipt and frame-ancestors fixtures byte-identical.
//! It is called, not copied. A second `static_web_*` implementation would give
//! two answers about what an artifact's identity is, and existing artifacts
//! would stop matching one of them.
//!
//! ## What the lane refuses
//!
//! An output root the build did not produce. The plan DECLARED it, so its
//! absence is a disagreement between declaration and execution — a build
//! failure, never a reason to fall back to publishing the whole workspace.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_formation::intent::{EffectiveBuildPlanV1, Lane, ProgramIntentV1};
use ato_materializer_static_web::{
    ProducedStaticWebBundle, StaticWebOutputPlan, produce_static_web_bundle,
};

/// What the Static lane produced, in the terms a FormationResult needs.
#[derive(Debug)]
pub struct StaticFormationOutput {
    pub bundle: ProducedStaticWebBundle,
    /// Content address of the manifest — the artifact's identity.
    pub manifest_digest: String,
    pub entry_path: String,
    pub spa_fallback: bool,
    pub total_bytes: u64,
}

/// Turn a built workspace into a Static Web materialization.
///
/// `materialization_id` is supplied by the caller and never invented here: the
/// producer says so, and an id minted by the producer would not be the one the
/// control plane registered.
pub fn materialize_static(
    intent: &ProgramIntentV1,
    plan: &EffectiveBuildPlanV1,
    workspace_root: &Path,
    destination_parent: &Path,
    materialization_id: &str,
    runtime_secret_canaries: &[&[u8]],
) -> Result<StaticFormationOutput> {
    if intent.lane != Lane::StaticWeb {
        bail!("materialize_static was handed a {:?} intent", intent.lane);
    }
    let declared = intent.static_output_root.clone().unwrap_or_default();
    let output_root = resolve_output_root(workspace_root, &declared)?;

    let output_plan = StaticWebOutputPlan {
        materialization_id: materialization_id.to_owned(),
        // The producer records this as provenance — WHERE inside the built
        // tree the output came from — and reads `built_output_root` for the
        // bytes. It requires a non-empty relative path, and rejects "." as a
        // non-normal component, so a site at the repository root is recorded
        // as the workspace root itself rather than as an empty string.
        image_output_root: PathBuf::from(if declared.is_empty() {
            plan.workspace_guest_root.trim_start_matches('/')
        } else {
            declared.as_str()
        }),
        entry_path: intent
            .static_entry_path
            .clone()
            .unwrap_or_else(|| "index.html".to_owned()),
        spa_fallback: intent.static_spa_fallback,
        // Empty on purpose: a connect-src is a deployment decision the control
        // plane owns, and a Formation that guessed one would publish a policy
        // nobody chose.
        connect_src: Vec::new(),
    };

    let bundle = produce_static_web_bundle(
        &output_plan,
        &output_root,
        destination_parent,
        runtime_secret_canaries,
    )
    .context("static web bundle production failed")?;

    let manifest_digest = bundle.receipt.manifest_digest.clone();
    let total_bytes = bundle
        .receipt
        .blobs
        .iter()
        .map(|blob| blob.size)
        .sum::<u64>();

    Ok(StaticFormationOutput {
        manifest_digest,
        entry_path: output_plan.entry_path,
        spa_fallback: output_plan.spa_fallback,
        total_bytes,
        bundle,
    })
}

/// The declared output root, resolved and contained.
fn resolve_output_root(workspace_root: &Path, declared: &str) -> Result<PathBuf> {
    if declared.is_empty() {
        return Ok(workspace_root.to_path_buf());
    }
    let candidate = workspace_root.join(declared);
    if !candidate.is_dir() {
        // Declaration and execution disagreeing is a build failure. Falling
        // back to the whole workspace would publish the source tree as a site.
        bail!("the plan declares static output root {declared:?}, which the build did not produce");
    }
    let root = workspace_root
        .canonicalize()
        .context("cannot resolve the workspace root")?;
    let resolved = candidate
        .canonicalize()
        .context("cannot resolve the declared static output root")?;
    if !resolved.starts_with(&root) {
        bail!("the declared static output root resolves outside the workspace");
    }
    Ok(resolved)
}

/// Whether this plan needs the build sandbox at all.
pub fn needs_build(plan: &EffectiveBuildPlanV1) -> bool {
    !plan.steps.is_empty()
}
