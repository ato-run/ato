use std::path::PathBuf;

use anyhow::Result;

use crate::cli::{
    ConfigCommands, ConfigEngineCommands, ConfigRegistryCommands, EngineCommands, RegistryCommands,
};
use crate::orchestration::{catalog_registry, support_command};

use super::Reporter;

pub(super) fn execute_config_command(
    command: ConfigCommands,
    nacelle: Option<PathBuf>,
    reporter: Reporter,
) -> Result<()> {
    match command {
        ConfigCommands::Engine { command } => match command {
            ConfigEngineCommands::Features => {
                support_command::execute_engine_command(EngineCommands::Features, nacelle, reporter)
            }
            ConfigEngineCommands::Register {
                name,
                path,
                default,
            } => support_command::execute_engine_command(
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
            } => support_command::execute_setup_command(engine, version, skip_verify, reporter),
        },
        ConfigCommands::Registry { command } => {
            let mapped = match command {
                ConfigRegistryCommands::Resolve { domain, json } => {
                    RegistryCommands::Resolve { domain, json }
                }
                ConfigRegistryCommands::List { json } => RegistryCommands::List { json },
                ConfigRegistryCommands::ClearCache => RegistryCommands::ClearCache,
            };
            catalog_registry::execute_registry_command(mapped)
        }
    }
}
