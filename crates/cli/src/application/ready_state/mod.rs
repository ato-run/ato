//! Ready-State use-case layer (E7): orchestrates the snapshot + capsulefs crates
//! behind the `ATO_READY_STATE_ENABLED` flag. The build seal branch
//! ([`build::seal`]) and the run restore sub-mode ([`restore::restore_and_expose`]
//! + [`runtime_adapter::RestoredRuntimeHandle`]) are wired from
//! `cli/commands/build.rs` and `application/pipeline/phases/run.rs` respectively;
//! everything here is additive and a legacy run never touches it.
//!
//! NOTE: this engine is fully unit-tested but the pipeline call sites
//! (`build.rs` seal branch, `run.rs` Execute restore sub-mode) are wired in a
//! dedicated follow-up so the change can be verified against a real `ato run`.
//! Until then the items below are exercised only by their own tests, so the
//! module carries `allow(dead_code)`.
#![allow(dead_code)]

pub(crate) mod backend;
pub(crate) mod build;
pub(crate) mod flags;
pub(crate) mod restore;
pub(crate) mod runtime_adapter;
pub(crate) mod store;

use std::path::{Path, PathBuf};

use capsule::foundation::install_lifecycle::{RunnerClassFacts, RunnerClassId};
use capsule::types::CapsuleManifest;

/// The Execute-phase sub-mode carrier: everything the run pipeline needs to
/// restore instead of cold-spawn. Present only when the flag is on, the capsule
/// is Ready-State-eligible, and a sealed artifact exists — otherwise the run
/// falls through to the legacy cold path.
pub(crate) struct ReadyStateRunPlan {
    /// The sealed artifact to restore.
    pub(crate) manifest: snapshot::ReadyStateManifest,
    /// State root holding the `ready-state/<hash>/cas` store.
    pub(crate) state_root: PathBuf,
    /// `blake3:<hex>` of the capsule manifest (the artifact key).
    pub(crate) capsule_manifest_hash: String,
    /// Whether to run the post-resume sanitizer before exposing.
    pub(crate) sanitize_after_restore: bool,
    /// This host's runner class (fed to the fail-closed restore gate).
    pub(crate) host_runner_class: Option<RunnerClassId>,
}

/// Decide whether this run is a Ready-State restore. Returns `None` (→ legacy
/// cold path) when the flag is off, the capsule is not Ready-State-eligible, or
/// no sealed artifact exists for it.
pub(crate) fn decide_ready_state_run(
    manifest: &CapsuleManifest,
    capsule_manifest_hash: &str,
    state_root: &Path,
) -> anyhow::Result<Option<ReadyStateRunPlan>> {
    if !flags::ready_state_enabled() {
        return Ok(None);
    }
    if !manifest.is_ready_state_eligible() {
        return Ok(None);
    }
    let Some(sealed) = store::load_manifest(state_root, capsule_manifest_hash)? else {
        return Ok(None);
    };
    Ok(Some(ReadyStateRunPlan {
        manifest: sealed,
        state_root: state_root.to_path_buf(),
        capsule_manifest_hash: capsule_manifest_hash.to_string(),
        sanitize_after_restore: manifest.snapshot_config().sanitize_after_restore,
        host_runner_class: Some(RunnerClassFacts::from_host().id()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible_manifest() -> CapsuleManifest {
        CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
port = 8080

[targets.app.readiness_probe]
type = "http"
path = "/health"

[snapshot]
mode = "warm"
"#,
        )
        .unwrap()
    }

    #[test]
    fn decide_returns_none_when_flag_off() {
        // ATO_READY_STATE_ENABLED is unset in the test env.
        let dir = tempfile::tempdir().unwrap();
        let plan = decide_ready_state_run(&eligible_manifest(), "blake3:x", dir.path()).unwrap();
        assert!(plan.is_none(), "flag off must mean legacy");
    }
}
