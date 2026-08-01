use anyhow::Result;

pub(super) fn execute_app_command(command: crate::AppCommands, json_mode: bool) -> Result<()> {
    match command {
        crate::AppCommands::Installed { command } => {
            super::installed_apps::execute_installed_command(command, json_mode)
        }
        crate::AppCommands::Resolve {
            handle,
            target,
            registry,
            json,
        } => crate::app_control::resolve_handle(
            &handle,
            target.as_deref(),
            registry.as_deref(),
            json_mode || json,
        ),
        crate::AppCommands::Latest {
            handle,
            registry,
            json,
        } => crate::app_control::fetch_latest(&handle, registry.as_deref(), json_mode || json),
        crate::AppCommands::Session { command } => match command {
            crate::SessionCommands::Start {
                handle,
                target,
                community_toml_id,
                attach_state,
                from_materialized_record,
                run_config_hash,
                json,
            } => crate::app_control::start_session(
                &handle,
                target.as_deref(),
                community_toml_id.as_deref(),
                &attach_state,
                from_materialized_record.as_deref(),
                /* local_manifest_path */ None,
                run_config_hash.as_deref(),
                json_mode || json,
            ),
            crate::SessionCommands::Stop { session_id, json } => {
                crate::app_control::stop_session(&session_id, json_mode || json)
            }
            crate::SessionCommands::WatchParent {
                session_id,
                parent_pid,
                parent_start_time_unix_ms,
                poll_ms,
            } => crate::app_control::watch_parent_and_stop_session(
                &session_id,
                parent_pid,
                parent_start_time_unix_ms,
                std::time::Duration::from_millis(poll_ms.max(50)),
            ),
        },
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
