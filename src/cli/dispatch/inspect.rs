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
    }
}
