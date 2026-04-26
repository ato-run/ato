use anyhow::Result;

use crate::cli::PackageCommands;

use super::registry;

pub(super) fn execute_package_command(command: PackageCommands) -> Result<()> {
    match command {
        PackageCommands::Search {
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        } => registry::execute_search_command(registry::SearchCommandArgs {
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        }),
    }
}
