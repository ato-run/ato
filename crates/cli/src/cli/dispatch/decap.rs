use anyhow::Result;

use crate::application::capsule_session;
use crate::cli::DecapCommands;

pub(crate) fn execute_decap_command(command: DecapCommands) -> Result<()> {
    match command {
        DecapCommands::Start {
            capsule,
            detach,
            name,
        } => capsule_session::start_public(&capsule, name.as_deref(), detach),
        DecapCommands::List { json } => capsule_session::list_public(json),
        DecapCommands::Attach { name } => capsule_session::attach_public(&name),
        DecapCommands::Stop { name, force } => capsule_session::stop_public(&name, force),
    }
}
