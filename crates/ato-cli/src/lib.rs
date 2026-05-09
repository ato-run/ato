//! Shared CLI entrypoints and crate-wide wiring for the `ato` binary.
//!
//! The binary target stays intentionally small so startup, error rendering, and
//! command dispatch can be exercised through this library from tests.

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use clap::{CommandFactory, Parser};

pub(crate) mod adapters;
pub(crate) mod app_control;
pub(crate) mod application;
pub(crate) mod cli;
pub(crate) mod common;
pub(crate) mod logging;
pub(crate) mod utils;

pub mod projection {
    pub use crate::application::projection::{
        project_payload, ProjectionError, ProjectionOutcome, ProjectionStrategy,
    };
}

pub mod provider_cache {
    pub use crate::application::provider_cache::{
        cache_enabled, check_and_project, compute_derivation_hash, freeze_after_install,
        ProviderCacheAction, ProviderCacheInputs, ProviderCachePlan,
    };
}

pub mod attestation {
    pub use crate::application::attestation::{issue_freeze_attestation, AttestationContext};
}

pub mod cache_admin {
    pub use crate::application::cache_admin::{
        clear_all, clear_derivation, collect_cache_stats, BlobSummary, CacheClearOutcome,
        CacheStats,
    };
}

pub mod dependency_materializer {
    pub use crate::application::dependency_materializer::{
        AttestationRef, AttestationStrategy, CacheLookupResult, CacheStrategy, DepDerivationKeyV1,
        DependencyMaterializationRequest, DependencyMaterializer, DependencyPlan,
        DependencyPortabilityClass, DependencyProjection, InstallPolicies, ManifestInputs,
        PlatformTriple, ReproducibilityMeta, RuntimeSelection, SessionDependencyMaterializer,
        SourceResolutionRecord, StoreRefRecord, VerificationResult,
    };

    pub mod freeze {
        pub use crate::application::dependency_materializer::freeze::{
            atomic_write_json, freeze_dep_tree, DerivationLock, FreezeOutcome,
        };
    }
}

/// Ensures the optional sidecar is stopped exactly once across normal exit,
/// explicit cleanup scopes, and panic-driven unwinding.
pub(crate) struct SidecarCleanup {
    sidecar: Arc<Mutex<Option<common::sidecar::SidecarHandle>>>,
    reporter: std::sync::Arc<reporters::CliReporter>,
}

impl SidecarCleanup {
    /// Wraps the sidecar handle in shared state so multiple shutdown paths can
    /// race safely without double-stopping the process.
    pub(crate) fn new(
        sidecar: Option<common::sidecar::SidecarHandle>,
        reporter: std::sync::Arc<reporters::CliReporter>,
    ) -> Self {
        Self {
            sidecar: Arc::new(Mutex::new(sidecar)),
            reporter,
        }
    }

    pub(crate) fn register_attempt_cleanup(
        &self,
        scope: &mut application::pipeline::cleanup::CleanupScope,
    ) {
        // Skip registration when no sidecar was started so the cleanup report
        // stays focused on actions that can actually run.
        if self
            .sidecar
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_none()
        {
            return;
        }

        let sidecar = Arc::clone(&self.sidecar);
        scope.register(move || stop_sidecar_cleanup_action(&sidecar));
    }

    /// Performs best-effort shutdown during teardown paths that cannot bubble
    /// errors back to the caller, such as `Drop`.
    pub(crate) fn stop_now(&mut self) {
        match stop_sidecar(&self.sidecar) {
            Ok(_) => {}
            Err(err) => {
                let _ = futures::executor::block_on(
                    self.reporter
                        .warn(format!("⚠️  Failed to stop sidecar: {}", err)),
                );
            }
        }
    }
}

fn stop_sidecar(
    sidecar: &Arc<Mutex<Option<common::sidecar::SidecarHandle>>>,
) -> anyhow::Result<bool> {
    // Cleanup must remain resilient after partial panics, so we recover the
    // inner state instead of skipping shutdown on a poisoned mutex.
    let sidecar = sidecar
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    let Some(sidecar) = sidecar else {
        return Ok(false);
    };

    sidecar.stop()?;
    Ok(true)
}

fn stop_sidecar_cleanup_action(
    sidecar: &Arc<Mutex<Option<common::sidecar::SidecarHandle>>>,
) -> capsule_core::execution_plan::error::CleanupActionRecord {
    match stop_sidecar(sidecar) {
        Ok(_) => capsule_core::execution_plan::error::CleanupActionRecord {
            action: "stop_sidecar".to_string(),
            status: capsule_core::execution_plan::error::CleanupActionStatus::Succeeded,
            detail: Some("tsnet sidecar".to_string()),
        },
        Err(error) => capsule_core::execution_plan::error::CleanupActionRecord {
            action: "stop_sidecar".to_string(),
            status: capsule_core::execution_plan::error::CleanupActionStatus::Failed,
            detail: Some(format!("tsnet sidecar: {}", error)),
        },
    }
}

impl Drop for SidecarCleanup {
    fn drop(&mut self) {
        self.stop_now();
    }
}

pub(crate) use adapters::inference_feedback;
pub(crate) use adapters::ipc;
pub(crate) use adapters::ipc::guest_protocol;
pub(crate) use adapters::output::diagnostics;
pub(crate) use adapters::output::progressive as progressive_ui;
pub(crate) use adapters::output::reporters;
pub(crate) use adapters::output::terminal as tui;
pub(crate) use adapters::preview;
pub(crate) use adapters::registry;
pub(crate) use adapters::registry::binding;
pub(crate) use adapters::registry::publish;
pub(crate) use adapters::registry::state;
pub(crate) use adapters::runtime;
pub(crate) use adapters::runtime::executors;
pub(crate) use adapters::runtime::external_capsule;
pub(crate) use application::auth;
pub(crate) use application::auth::consent_store;
pub(crate) use application::engine::build;
pub(crate) use application::engine::data_injection;
pub(crate) use application::engine::install;
pub(crate) use application::engine::manager as engine_manager;
pub(crate) use application::search;
pub(crate) use application::workspace as project;
pub(crate) use cli::commands;
pub(crate) use cli::dispatch;
pub(crate) use cli::scoped_id_prompt;
pub(crate) use cli::*;
pub(crate) use publish::artifact as publish_artifact;
pub(crate) use publish::ci as publish_ci;
pub(crate) use publish::official as publish_official;
pub(crate) use publish::preflight as publish_preflight;
pub(crate) use publish::prepare as publish_prepare;
pub(crate) use utils::archive as capsule_archive;
pub(crate) use utils::env;
pub(crate) use utils::error as ato_error_jsonl;
pub(crate) use utils::error as error_codes;
pub(crate) use utils::fs as fs_copy;
pub(crate) use utils::hash as artifact_hash;
pub(crate) use utils::local_input;
pub(crate) use utils::payload_guard;

/// Runs the CLI entry flow and converts failures into the user-facing output
/// format selected by the original raw arguments.
pub fn main_entry() {
    // Activate the tracing pipeline before any guest-runtime path can fire
    // a `tracing::*!` event. Without this, every event in the executors
    // would be a no-op (no global subscriber) and `ATO_CLI_LOG=...` would
    // silently do nothing.
    logging::init_subscriber();

    let args: Vec<String> = std::env::args().collect();
    // Detect JSON mode before Clap parsing so even parse-time failures can be
    // rendered as machine-readable diagnostics.
    let json_mode = args.iter().any(|arg| arg == "--json");
    let command_context = diagnostics::detect_command_context(&args);

    if let Err(err) = run() {
        if json_mode && commands::inspect::try_emit_json_error(&err) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        if ato_error_jsonl::try_emit_from_anyhow(&err, json_mode) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        // #126 — non-TTY callers (CI, scripted shells, AODD harnesses) must
        // be able to read the typed identity fields for any error that
        // requires interactive resolution (consent_required, missing_env,
        // auth_required, etc.). Emit the JSON envelope to stderr alongside
        // the human diagnostic when stdin or stdout is not a TTY. This is
        // additive: TTY callers and `--json` callers see no behaviour
        // change.
        let non_tty_caller = !std::io::stdin().is_terminal()
            || !std::io::stdout().is_terminal();
        if !json_mode && non_tty_caller {
            ato_error_jsonl::try_emit_interactive_resolution_envelope(&err);
        }

        let diagnostic = diagnostics::from_anyhow(&err, command_context);
        let exit_code = diagnostics::map_exit_code(&diagnostic, &err);

        if json_mode {
            if let Ok(payload) = serde_json::to_string(&diagnostic.to_json_envelope()) {
                println!("{}", payload);
            } else {
                // Fall back to a static payload because serialization failure is
                // itself an internal error and should not suppress JSON output.
                let fallback_payload = r#"{"schema_version":"1","status":"error","error":{"code":"E999","name":"internal_error","phase":"internal","message":"failed to serialize error payload","retryable":true,"interactive_resolution":false,"causes":[]}}"#;
                println!("{fallback_payload}");
            }
        } else {
            eprintln!("{:?}", miette::Report::new(diagnostic));
        }

        std::process::exit(exit_code);
    }
}

/// Parses arguments and dispatches the requested command.
///
/// When the binary is invoked without subcommands, this preserves the friendly
/// help-first UX instead of letting Clap exit through its default error path.
pub fn run() -> Result<()> {
    ato_session_core::sweep::sweep_startup_runtime_artifacts_best_effort();

    let is_no_args = std::env::args_os().count() == 1;

    if is_no_args {
        progressive_ui::print_logo(true)?;
        let mut cmd = Cli::command();
        cmd.print_help().context("failed to print CLI help")?;
        println!();
        return Ok(());
    }

    let cli = Cli::parse();
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(cli.json));

    dispatch::execute(cli, reporter)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
