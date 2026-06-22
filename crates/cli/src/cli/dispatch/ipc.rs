use anyhow::Result;

use crate::cli::IpcCommands;
use crate::commands;

pub(super) fn execute_ipc_command(command: IpcCommands) -> Result<()> {
    match command {
        IpcCommands::Status { json } => commands::ipc::run_ipc_status(json),
        IpcCommands::Start { path, json } => commands::ipc::run_ipc_start(path, json),
        IpcCommands::Stop { name, force, json } => commands::ipc::run_ipc_stop(name, force, json),
        IpcCommands::Invoke {
            path,
            service,
            method,
            args,
            id,
            max_message_size,
            json,
        } => commands::ipc::run_ipc_invoke(path, service, method, args, id, max_message_size, json),
    }
}
