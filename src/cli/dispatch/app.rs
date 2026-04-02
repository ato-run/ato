use anyhow::Result;

pub(super) fn execute_app_command(command: crate::AppCommands, json_mode: bool) -> Result<()> {
    match command {
        crate::AppCommands::Status { package_id, json } => {
            crate::app_control::status(&package_id, json_mode || json)
        }
        crate::AppCommands::Bootstrap {
            package_id,
            finalize,
            workspace,
            model_tier,
            privacy_mode,
            json,
        } => crate::app_control::bootstrap(
            &package_id,
            finalize,
            workspace.as_deref(),
            model_tier,
            privacy_mode,
            json_mode || json,
        ),
        crate::AppCommands::Repair {
            package_id,
            action,
            json,
        } => crate::app_control::repair(&package_id, action, json_mode || json),
    }
}
