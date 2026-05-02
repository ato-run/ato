use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::application::build_materialization::BuildPolicy;
use crate::application::execution_replay::{self, ReplayMode};
use crate::cli::shared::{
    CompatibilityFallbackBackend, EnforcementMode, ProviderToolchain, RunAgentMode,
};
use crate::reporters;

use super::run::{self, RunLikeCommandArgs};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayStartedView {
    execution_id: String,
    mode: &'static str,
    source: String,
    note: &'static str,
}

pub(super) fn execute_replay_command(
    id: String,
    strict: bool,
    best_effort: bool,
    json: bool,
    nacelle: Option<PathBuf>,
) -> Result<()> {
    let mode = replay_mode(strict, best_effort)?;
    let plan = execution_replay::plan_replay(&id, mode)?;
    let view = ReplayStartedView {
        execution_id: plan.receipt.execution_id.clone(),
        mode: match plan.mode {
            ReplayMode::Strict => "strict",
            ReplayMode::BestEffort => "best-effort",
        },
        source: plan.run_path.display().to_string(),
        note: match plan.mode {
            ReplayMode::Strict => "strict same-host replay delegated to normal ato run",
            ReplayMode::BestEffort => {
                "best-effort same-host replay; this does not claim an identical launch envelope"
            }
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        eprintln!("Replaying execution {}", view.execution_id);
        eprintln!("  Mode: {}", view.mode);
        eprintln!("  Source: {}", view.source);
        eprintln!("  Note: {}", view.note);
    }

    run::execute_run_like_command(RunLikeCommandArgs {
        path: plan.run_path,
        target: None,
        entry: None,
        env_file: None,
        prompt_env: false,
        args: Vec::new(),
        watch: false,
        background: false,
        nacelle,
        registry: None,
        state: Vec::new(),
        inject: Vec::new(),
        enforcement: EnforcementMode::Strict,
        sandbox_mode: false,
        unsafe_mode_legacy: false,
        unsafe_bypass_sandbox_legacy: false,
        dangerously_skip_permissions: false,
        compatibility_fallback: None::<CompatibilityFallbackBackend>,
        provider_toolchain: ProviderToolchain::Auto,
        yes: true,
        verbose: false,
        agent_mode: RunAgentMode::Auto,
        keep_failed_artifacts: false,
        auto_fix_mode: None,
        allow_unverified: false,
        build_policy: BuildPolicy::default(),
        read: Vec::new(),
        write: Vec::new(),
        read_write: Vec::new(),
        cwd: None,
        deprecation_warning: None,
        reporter: Arc::new(reporters::CliReporter::new_run(json)),
    })
}

fn replay_mode(strict: bool, best_effort: bool) -> Result<ReplayMode> {
    match (strict, best_effort) {
        (true, false) => Ok(ReplayMode::Strict),
        (false, true) => Ok(ReplayMode::BestEffort),
        (false, false) => bail!("replay requires either --strict or --best-effort"),
        (true, true) => bail!("replay modes are mutually exclusive"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_mode_requires_explicit_mode() {
        assert!(replay_mode(false, false).is_err());
        assert_eq!(
            replay_mode(true, false).expect("strict"),
            ReplayMode::Strict
        );
        assert_eq!(
            replay_mode(false, true).expect("best effort"),
            ReplayMode::BestEffort
        );
    }
}
