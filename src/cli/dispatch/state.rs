use anyhow::Result;

pub(super) fn execute_state_command(command: crate::StateCommands) -> Result<()> {
    match command {
        crate::StateCommands::List {
            owner_scope,
            state_name,
            json,
        } => crate::state::list_states(owner_scope.as_deref(), state_name.as_deref(), json),
        crate::StateCommands::Inspect { state_ref, json } => {
            crate::state::inspect_state(&state_ref, json)
        }
        crate::StateCommands::Register {
            manifest,
            state_name,
            path,
            json,
        } => crate::state::register_state_from_manifest(
            &manifest,
            &state_name,
            path.to_string_lossy().as_ref(),
            json,
        ),
    }
}
