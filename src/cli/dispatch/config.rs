use std::path::PathBuf;

use anyhow::Result;

use crate::cli::{
    ConfigCommands, ConfigEngineCommands, ConfigRegistryCommands, EngineCommands, RegistryCommands,
};

use super::engine;
use super::registry;
use super::Reporter;

pub(super) fn execute_config_command(
    command: ConfigCommands,
    nacelle: Option<PathBuf>,
    reporter: Reporter,
) -> Result<()> {
    match command {
        ConfigCommands::Engine { command } => match command {
            ConfigEngineCommands::Features => {
                engine::execute_engine_command(EngineCommands::Features, nacelle, reporter)
            }
            ConfigEngineCommands::Register {
                name,
                path,
                default,
            } => engine::execute_engine_command(
                EngineCommands::Register {
                    name,
                    path,
                    default,
                },
                nacelle,
                reporter,
            ),
            ConfigEngineCommands::Install {
                engine,
                version,
                skip_verify,
            } => engine::execute_setup_command(engine, version, skip_verify, reporter),
        },
        ConfigCommands::Registry { command } => {
            let mapped = match command {
                ConfigRegistryCommands::Resolve { domain, json } => {
                    RegistryCommands::Resolve { domain, json }
                }
                ConfigRegistryCommands::List { json } => RegistryCommands::List { json },
                ConfigRegistryCommands::ClearCache => RegistryCommands::ClearCache,
            };
            registry::execute_registry_command(mapped)
        }
    }
}
