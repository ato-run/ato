use anyhow::Result;

use crate::cli::PackageCommands;
use crate::orchestration::catalog_registry;

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
        } => catalog_registry::execute_search_command(catalog_registry::SearchCommandArgs {
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
