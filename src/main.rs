use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use clap::{CommandFactory, Parser};
use colored::Colorize;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

fn print_animated_logo() {
    let logo = r#"
    ___    __       
   /   |  / /_____  
  / /| | / __/ __ \ 
 / ___ |/ /_/ /_/ / 
/_/  |_|\__/\____/  
"#;

    for line in logo.lines() {
        println!("{}", line.cyan().bold());
        io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    println!();
}

struct SidecarCleanup {
    sidecar: Option<common::sidecar::SidecarHandle>,
    reporter: std::sync::Arc<reporters::CliReporter>,
}

impl SidecarCleanup {
    fn new(
        sidecar: Option<common::sidecar::SidecarHandle>,
        reporter: std::sync::Arc<reporters::CliReporter>,
    ) -> Self {
        Self { sidecar, reporter }
    }

    fn stop_now(&mut self) {
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

mod cli;
mod common;
mod core;
mod services;
mod skills;
mod ui;
mod utils;

pub(crate) use cli::commands;
pub(crate) use cli::dispatch;
pub(crate) use cli::scoped_id_prompt;
pub(crate) use cli::*;
pub(crate) use core::auth;
pub(crate) use core::auth::consent_store;
pub(crate) use core::engine::build;
pub(crate) use core::engine::data_injection;
pub(crate) use core::engine::executors;
pub(crate) use core::engine::external_capsule;
pub(crate) use core::engine::install;
pub(crate) use core::engine::manager as engine_manager;
pub(crate) use core::engine::orchestrator as orchestration;
pub(crate) use core::engine::orchestrator::ingress_proxy;
pub(crate) use core::engine::runtime;
pub(crate) use core::registry;
pub(crate) use core::registry::binding;
pub(crate) use core::registry::publish;
pub(crate) use core::registry::state;
pub(crate) use core::workspace as project;
pub(crate) use install::support::*;
#[cfg(test)]
pub(crate) use orchestration::catalog_registry as catalog_registry_orchestration;
#[cfg(test)]
pub(crate) use orchestration::publish_command as publish_command_orchestration;
pub(crate) use orchestration::run_install as run_install_orchestration;
pub(crate) use publish::artifact as publish_artifact;
pub(crate) use publish::ci as publish_ci;
pub(crate) use publish::dry_run as publish_dry_run;
pub(crate) use publish::official as publish_official;
pub(crate) use publish::preflight as publish_preflight;
pub(crate) use publish::prepare as publish_prepare;
pub(crate) use publish::private as publish_private;
pub(crate) use services::inference_feedback;
pub(crate) use services::ipc;
pub(crate) use services::ipc::guest_protocol;
pub(crate) use services::preview;
pub(crate) use ui::diagnostics;
pub(crate) use ui::progressive as progressive_ui;
pub(crate) use ui::reporters;
pub(crate) use ui::terminal as tui;
pub(crate) use utils::archive as capsule_archive;
pub(crate) use utils::env;
pub(crate) use utils::error as ato_error_jsonl;
pub(crate) use utils::error as error_codes;
pub(crate) use utils::fs as fs_copy;
pub(crate) use utils::hash as artifact_hash;
pub(crate) use utils::local_input;
pub(crate) use utils::payload_guard;

fn main() {
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

fn run() -> Result<()> {
    let is_no_args = std::env::args_os().count() == 1;

    if is_no_args {
        print_animated_logo();
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
#[path = "main/tests.rs"]
mod tests;
