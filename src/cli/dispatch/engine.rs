use std::path::PathBuf;

use anyhow::Result;
use capsule_core::CapsuleReporter;
use serde_json::json;

use super::Reporter;

pub(super) fn execute_engine_command(
    command: crate::EngineCommands,
    nacelle_override: Option<PathBuf>,
    reporter: Reporter,
) -> Result<()> {
    match command {
        crate::EngineCommands::Features => {
            let nacelle =
                capsule_core::engine::discover_nacelle(capsule_core::engine::EngineRequest {
                    explicit_path: nacelle_override,
                    manifest_path: None,
                    workspace_root: None,
                    compat_manifest: None,
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
                    workspace_root: None,
                    compat_manifest: None,
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

pub(super) fn execute_setup_command(
    engine: String,
    version: Option<String>,
    skip_verify: bool,
    reporter: Reporter,
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
