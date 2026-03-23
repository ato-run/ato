use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use clap::{CommandFactory, Parser};

pub(crate) mod adapters;
pub(crate) mod application;
pub(crate) mod cli;
pub(crate) mod common;
pub(crate) mod utils;

pub(crate) struct SidecarCleanup {
    sidecar: Option<common::sidecar::SidecarHandle>,
    reporter: std::sync::Arc<reporters::CliReporter>,
}

impl SidecarCleanup {
    pub(crate) fn new(
        sidecar: Option<common::sidecar::SidecarHandle>,
        reporter: std::sync::Arc<reporters::CliReporter>,
    ) -> Self {
        Self { sidecar, reporter }
    }

    pub(crate) fn stop_now(&mut self) {
        if let Some(sidecar) = self.sidecar.take() {
            if let Err(err) = sidecar.stop() {
                let _ = futures::executor::block_on(
                    self.reporter
                        .warn(format!("⚠️  Failed to stop sidecar: {}", err)),
                );
            }
        }
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
pub(crate) use cli::orchestration;
pub(crate) use cli::orchestration::ingress_proxy;
pub(crate) use cli::scoped_id_prompt;
pub(crate) use cli::*;
pub(crate) use install::support::*;
#[cfg(test)]
pub(crate) use orchestration::catalog_registry as catalog_registry_orchestration;
#[cfg(test)]
pub(crate) use orchestration::publish_command as publish_command_orchestration;
pub(crate) use orchestration::run_install as run_install_orchestration;
pub(crate) use publish::artifact as publish_artifact;
pub(crate) use publish::ci as publish_ci;
pub(crate) use publish::official as publish_official;
pub(crate) use publish::preflight as publish_preflight;
pub(crate) use publish::prepare as publish_prepare;
pub(crate) use publish::private as publish_private;
pub(crate) use utils::archive as capsule_archive;
pub(crate) use utils::env;
pub(crate) use utils::error as ato_error_jsonl;
pub(crate) use utils::error as error_codes;
pub(crate) use utils::fs as fs_copy;
pub(crate) use utils::hash as artifact_hash;
pub(crate) use utils::local_input;
pub(crate) use utils::payload_guard;

pub fn main_entry() {
    let args: Vec<String> = std::env::args().collect();
    let json_mode = args.iter().any(|arg| arg == "--json");
    let command_context = diagnostics::detect_command_context(&args);

    if let Err(err) = run() {
        if json_mode && commands::inspect::try_emit_json_error(&err) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        if ato_error_jsonl::try_emit_from_anyhow(&err, json_mode) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        let diagnostic = diagnostics::from_anyhow(&err, command_context);
        let exit_code = diagnostics::map_exit_code(&diagnostic, &err);

        if json_mode {
            if let Ok(payload) = serde_json::to_string(&diagnostic.to_json_envelope()) {
                println!("{}", payload);
            } else {
                let fallback_payload = r#"{"schema_version":"1","status":"error","error":{"code":"E999","name":"internal_error","phase":"internal","message":"failed to serialize error payload","retryable":true,"interactive_resolution":false,"causes":[]}}"#;
                println!("{fallback_payload}");
            }
        } else {
            eprintln!("{:?}", miette::Report::new(diagnostic));
        }

        std::process::exit(exit_code);
    }
}

pub fn run() -> Result<()> {
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
