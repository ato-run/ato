//! Historical v1 fixture adapters. Never compiled into an active executor.
#![allow(dead_code)]
use ato_formation::{
    authoring::BoundDerivation,
    execution::{BuildAction, ExecutionPlan},
    intent::{EffectiveBuildPlanV1, Lane, ProgramIntentV1},
};
use ato_runtime_attempt::{
    build::{BuildAttempt, BuildOutcome},
    build_sandbox::BuildSandbox,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn derivation(intent: Option<&ProgramIntentV1>) -> BoundDerivation {
    let static_web = intent.is_some_and(|i| i.lane == Lane::StaticWeb);
    serde_json::from_value(serde_json::json!({
        "schema":"ato.derivation/1","inputs":[],"runtimes":{},"ports":[],"state":[],"effects":"pure",
        "steps":[{"id":"serve","protocol":if static_web {"ato.browser@1"} else {"ato.process@1"},"op":"serve",
          "argv":intent.map(|i|i.launch_argv.clone()).unwrap_or_default(),
          "cwd":intent.map(|i|i.cwd_relative.clone()).unwrap_or_default(),
          "env":intent.map(|i|i.public_env.clone()).unwrap_or_default(),
          "root":intent.and_then(|i|i.static_output_root.clone()),
          "entry":intent.and_then(|i|i.static_entry_path.clone()),
          "spa_fallback":intent.map(|i|i.static_spa_fallback)
        }]
    })).unwrap()
}
pub fn plan(old: &EffectiveBuildPlanV1) -> ExecutionPlan {
    ExecutionPlan {
        lane: old.lane,
        serving_step: 0,
        workspace_guest_root: old.workspace_guest_root.clone(),
        toolchains: old.runtime.clone(),
        package_manager: None,
        actions: old
            .steps
            .iter()
            .cloned()
            .map(BuildAction::Prerequisite)
            .collect(),
        toolchain_path: old.toolchain_path.clone(),
        environment_bindings: BTreeMap::new(),
    }
}
pub fn run_build(
    old: &EffectiveBuildPlanV1,
    attempt: BuildAttempt,
    sandbox: &BuildSandbox<'_>,
) -> anyhow::Result<BuildOutcome> {
    ato_runtime_attempt::build::run_build(&plan(old), &derivation(None), attempt, sandbox)
}
pub fn output_root(outcome: &BuildOutcome, old: &EffectiveBuildPlanV1) -> anyhow::Result<PathBuf> {
    ato_runtime_attempt::build::output_root(outcome, &old.output_root)
}
pub fn needs_build(old: &EffectiveBuildPlanV1) -> bool {
    !old.steps.is_empty()
}
pub fn materialize_static(
    intent: &ProgramIntentV1,
    old: &EffectiveBuildPlanV1,
    workspace: &Path,
    parent: &Path,
    id: &str,
    secrets: &[&[u8]],
) -> anyhow::Result<ato_runtime_attempt::static_lane::StaticFormationOutput> {
    ato_runtime_attempt::static_lane::materialize_static(
        &derivation(Some(intent)),
        &plan(old),
        workspace,
        parent,
        id,
        secrets,
    )
}
