use anyhow::Result;

use crate::cli::KeyCommands;
use crate::commands;

use super::Reporter;

pub(super) fn execute_key_command(command: KeyCommands, reporter: Reporter) -> Result<()> {
    match command {
        KeyCommands::Gen { out, force, json } => {
            commands::keygen::execute(commands::keygen::KeygenArgs { out, force, json }, reporter)
        }
        KeyCommands::Sign { target, key, out } => {
            commands::sign::execute(commands::sign::SignArgs { target, key, out }, reporter)
        }
        KeyCommands::Verify {
            target,
            sig,
            signer,
            json,
        } => commands::verify::execute(
            commands::verify::VerifyArgs {
                target,
                sig,
                signer,
                json,
            },
            reporter,
        ),
    }
}
