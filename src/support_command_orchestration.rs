use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use capsule_core::CapsuleReporter;
use serde_json::json;

use crate::reporters;

pub(crate) fn execute_engine_command(
    command: crate::EngineCommands,
    nacelle_override: Option<PathBuf>,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    match command {
        crate::EngineCommands::Features => {
            let nacelle =
                capsule_core::engine::discover_nacelle(capsule_core::engine::EngineRequest {
                    explicit_path: nacelle_override,
                    manifest_path: None,
                })?;
            let payload = json!({ "spec_version": "0.1.0" });
            let resp = capsule_core::engine::run_internal(&nacelle, "features", &payload)?;
            let body = serde_json::to_string_pretty(&resp)?;
            futures::executor::block_on(reporter.notify(body))?;
            Ok(())
        }
        crate::EngineCommands::Register {
            name,
            path,
            default,
        } => {
            let resolved_path = if let Some(path) = path {
                path
            } else if let Ok(env_path) = std::env::var("NACELLE_PATH") {
                PathBuf::from(env_path)
            } else {
                anyhow::bail!("Missing --path and NACELLE_PATH is not set");
            };

            let validated =
                capsule_core::engine::discover_nacelle(capsule_core::engine::EngineRequest {
                    explicit_path: Some(resolved_path),
                    manifest_path: None,
                })?;

            let mut cfg = capsule_core::config::load_config()?;
            cfg.engines.insert(
                name.clone(),
                capsule_core::config::EngineRegistration {
                    path: validated.display().to_string(),
                },
            );
            if default {
                cfg.default_engine = Some(name.clone());
            }
            capsule_core::config::save_config(&cfg)?;

            futures::executor::block_on(reporter.notify(format!(
                "✅ Registered engine '{}' -> {}",
                name,
                validated.display()
            )))?;
            if default {
                futures::executor::block_on(
                    reporter.notify("✅ Set as default engine".to_string()),
                )?;
            }
            Ok(())
        }
    }
}

pub(crate) fn execute_state_command(command: crate::StateCommands) -> Result<()> {
    match command {
        crate::StateCommands::List {
            owner_scope,
            state_name,
            json,
        } => crate::state::list_states(owner_scope.as_deref(), state_name.as_deref(), json),
        crate::StateCommands::Inspect { state_ref, json } => {
            crate::state::inspect_state(&state_ref, json)
        }
        crate::StateCommands::Register {
            manifest,
            state_name,
            path,
            json,
        } => crate::state::register_state_from_manifest(
            &manifest,
            &state_name,
            path.to_string_lossy().as_ref(),
            json,
        ),
    }
}

pub(crate) fn execute_binding_command(command: crate::BindingCommands) -> Result<()> {
    match command {
        crate::BindingCommands::List {
            owner_scope,
            service_name,
            json,
        } => crate::binding::list_bindings(owner_scope.as_deref(), service_name.as_deref(), json),
        crate::BindingCommands::Inspect { binding_ref, json } => {
            crate::binding::inspect_binding(&binding_ref, json)
        }
        crate::BindingCommands::Resolve {
            owner_scope,
            service_name,
            binding_kind,
            caller_service,
            json,
        } => crate::binding::resolve_binding(
            &owner_scope,
            &service_name,
            &binding_kind,
            caller_service.as_deref(),
            json,
        ),
        crate::BindingCommands::BootstrapTls {
            binding_ref,
            install_system_trust,
            yes,
            json,
        } => crate::binding::bootstrap_ingress_tls(&binding_ref, install_system_trust, yes, json),
        crate::BindingCommands::ServeIngress {
            binding_ref,
            manifest,
            upstream_url,
        } => {
            crate::binding::serve_ingress_binding(&binding_ref, &manifest, upstream_url.as_deref())
        }
        crate::BindingCommands::RegisterIngress {
            manifest,
            service_name,
            url,
            json,
        } => crate::binding::register_ingress_binding_from_manifest(
            &manifest,
            &service_name,
            &url,
            json,
        ),
        crate::BindingCommands::RegisterService {
            manifest,
            service_name,
            url,
            process_id,
            port,
            json,
        } => match (url.as_deref(), process_id.as_deref()) {
            (Some(url), _) => crate::binding::register_service_binding_from_manifest(
                &manifest,
                &service_name,
                url,
                json,
            ),
            (None, Some(process_id)) => crate::binding::register_service_binding_from_process(
                process_id,
                &service_name,
                port,
                json,
            ),
            (None, None) => anyhow::bail!("register-service requires either --url or --process-id"),
        },
        crate::BindingCommands::SyncProcess { process_id, json } => {
            crate::binding::sync_service_bindings_from_process(&process_id, json)
        }
    }
}

pub(crate) fn execute_setup_command(
    engine: String,
    version: Option<String>,
    skip_verify: bool,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    let capsule_reporter: &dyn CapsuleReporter = reporter.as_ref();
    let install = crate::engine_manager::install_engine_release(
        &engine,
        version.as_deref(),
        skip_verify,
        capsule_reporter,
    )?;

    futures::executor::block_on(reporter.notify(format!(
        "✅ Engine {} {} installed at {}",
        engine,
        install.version,
        install.path.display()
    )))?;

    Ok(())
}
