use anyhow::Result;

use crate::cli::ProfileCommands;
use crate::commands;

use super::Reporter;

pub(super) fn execute_profile_command(command: ProfileCommands, reporter: Reporter) -> Result<()> {
    match command {
        ProfileCommands::Create {
            name,
            bio,
            avatar,
            key,
            output,
            website,
            github,
            twitter,
        } => commands::profile::execute_create(
            commands::profile::CreateArgs {
                name,
                bio,
                avatar,
                key,
                output,
                website,
                github,
                twitter,
            },
            reporter,
        ),
        ProfileCommands::Show { path, json } => {
            commands::profile::execute_show(commands::profile::ShowArgs { path, json }, reporter)
        }
    }
}
