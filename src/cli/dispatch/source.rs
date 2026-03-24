use anyhow::Result;

use crate::cli::SourceCommands;

use super::registry;

pub(super) fn execute_source_command(command: SourceCommands) -> Result<()> {
    match command {
        SourceCommands::SyncStatus {
            source_id,
            sync_run_id,
            registry,
            json,
        } => registry::execute_source_sync_status_command(source_id, sync_run_id, registry, json),
        SourceCommands::Rebuild {
            source_id,
            reference,
            wait,
            registry,
            json,
        } => registry::execute_source_rebuild_command(source_id, reference, wait, registry, json),
    }
}
