use anyhow::Result;

use crate::application::object_capsule;
use crate::cli::DecapCommands;

pub(crate) fn execute_decap_command(command: DecapCommands) -> Result<()> {
    match command {
        DecapCommands::Start {
            capsule,
            detach,
            name,
            worker,
        } => object_capsule::start(&capsule, name.as_deref(), detach, worker),
        DecapCommands::List { json } => object_capsule::list(json),
        DecapCommands::Attach { name } => object_capsule::attach(&name),
        DecapCommands::Stop { name } => object_capsule::stop(&name),
    }
}
