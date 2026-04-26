use anyhow::Result;

use crate::cli::InspectCommands;
use crate::commands;

pub(super) fn execute_inspect_command(command: InspectCommands, json_mode: bool) -> Result<()> {
    match command {
        InspectCommands::Requirements {
            target,
            registry,
            json,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                commands::inspect::execute_requirements(target, registry, json_mode || json)
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            })
        }
        InspectCommands::Lock { path, json } => {
            commands::inspect::execute_lock_view(path, json_mode || json).map(|_| ())
        }
        InspectCommands::Preview { path, json } => {
            commands::inspect::execute_preview_view(path, json_mode || json).map(|_| ())
        }
        InspectCommands::Diagnostics { path, json } => {
            commands::inspect::execute_diagnostics_view(path, json_mode || json).map(|_| ())
        }
        InspectCommands::Remediation { path, json } => {
            commands::inspect::execute_remediation_view(path, json_mode || json).map(|_| ())
        }
    }
}
