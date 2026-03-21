use anyhow::Result;

use crate::project;

use super::Reporter;

pub(super) fn execute_scaffold_command(
    command: crate::cli::ScaffoldCommands,
    reporter: Reporter,
) -> Result<()> {
    match command {
        crate::cli::ScaffoldCommands::Docker {
            manifest,
            output,
            output_dir,
            force,
        } => project::scaffold::execute_docker(
            project::scaffold::ScaffoldDockerArgs {
                manifest_path: manifest,
                output_dir,
                output,
                force,
            },
            reporter,
        ),
    }
}
