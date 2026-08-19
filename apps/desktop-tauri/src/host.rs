//! The desktop host: typed commands that delegate Capsule operations to the
//! `ato` CLI and inspect the active Run.
//!
//! Every command verifies its caller label before acting; the capability file
//! is defense in depth, not the only check. Computation is never advanced by
//! this shell — the CLI is the sole owner.

use ato_host_control::{CommandSpec, CompletedCommand, NativeHost, RunnerHost};
use ato_ipc::computation::{ComputationCommand, ComputationCommandResult};
use ato_ipc::desktop_control::{DesktopRunStatus, DesktopRunView};
use ato_ipc::session_surface::WEB_SURFACE_PROFILE;
use serde::Serialize;
use tauri::Url;

use crate::{binary, navigation, windows};

/// Static shell metadata surfaced to the launcher UI.
#[derive(Serialize)]
pub struct DesktopInfo {
    pub version: String,
    pub platform: String,
}

/// Map a typed [`ComputationCommand`] to the exact `ato` argv. This is the
/// only boundary where a command becomes a process — arbitrary shell strings
/// are never accepted.
pub fn argv_for(command: &ComputationCommand) -> Vec<String> {
    match command {
        ComputationCommand::Init {
            capsule,
            initial_only,
        } => {
            let mut args = vec!["init".to_owned(), capsule.clone()];
            if *initial_only {
                args.push("--initial-only".to_owned());
            }
            args
        }
        ComputationCommand::Resume { selector, branch } => {
            let mut args = vec!["resume".to_owned(), selector.clone()];
            if let Some(branch) = branch {
                args.extend(["--branch".to_owned(), branch.clone()]);
            }
            args
        }
        ComputationCommand::Stop { capsule } => vec!["stop".to_owned(), capsule.clone()],
        ComputationCommand::Encap { selector, output } => vec![
            "encap".to_owned(),
            selector.clone(),
            "-o".to_owned(),
            output.clone(),
        ],
        ComputationCommand::RunPortable { capsule_file } => {
            vec!["run".to_owned(), capsule_file.clone()]
        }
    }
}

fn combined_output(completed: &CompletedCommand) -> String {
    let mut text = String::from_utf8_lossy(&completed.stdout).into_owned();
    if !completed.stderr.is_empty() {
        text.push_str(&String::from_utf8_lossy(&completed.stderr));
    }
    text
}

fn cli_host() -> Result<NativeHost, String> {
    let program = binary::resolve_ato_binary().map_err(|error| error.to_string())?;
    Ok(NativeHost::new(move |_name| Ok(program.clone())))
}

fn run_cli(args: Vec<String>) -> Result<CompletedCommand, String> {
    let host = cli_host()?;
    let program = host
        .resolve_binary(binary::ato_binary_name())
        .map_err(|error| error.to_string())?;
    host.run_to_completion(&CommandSpec {
        program,
        args,
        env: vec![],
    })
    .map_err(|error| error.to_string())
}

/// Inspect the active Run by asking the CLI, and decode its JSON response.
fn inspect(project: &str) -> Result<DesktopRunView, String> {
    let completed = run_cli(vec![
        "__desktop".to_owned(),
        "inspect".to_owned(),
        project.to_owned(),
    ])?;
    if !completed.success() {
        let stderr = String::from_utf8_lossy(&completed.stderr).into_owned();
        return Err(if stderr.is_empty() {
            combined_output(&completed)
        } else {
            stderr
        });
    }
    serde_json::from_slice(&completed.stdout).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn desktop_info() -> DesktopInfo {
    DesktopInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
    }
}

#[tauri::command]
pub async fn computation_execute(
    window: tauri::WebviewWindow,
    command: ComputationCommand,
) -> Result<ComputationCommandResult, String> {
    windows::verify_main_caller(window.label())?;
    let args = argv_for(&command);
    let completed = tauri::async_runtime::spawn_blocking(move || run_cli(args))
        .await
        .map_err(|error| error.to_string())??;
    Ok(ComputationCommandResult {
        success: completed.success(),
        output: combined_output(&completed),
    })
}

#[tauri::command]
pub async fn run_inspect(
    window: tauri::WebviewWindow,
    project: String,
) -> Result<DesktopRunView, String> {
    windows::verify_main_caller(window.label())?;
    tauri::async_runtime::spawn_blocking(move || inspect(&project))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn pick_project(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Option<String>, String> {
    windows::verify_main_caller(window.label())?;
    use tauri_plugin_dialog::DialogExt;
    let (sender, receiver) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = sender.send(path);
    });
    let path = receiver
        .recv()
        .map_err(|_| "folder picker was closed".to_owned())?;
    Ok(path
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn open_home(window: tauri::WebviewWindow, app: tauri::AppHandle) -> Result<(), String> {
    windows::verify_main_caller(window.label())?;
    crate::build_home_window(&app).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn open_web_surface(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    project: String,
) -> Result<(), String> {
    windows::verify_main_caller(window.label())?;
    let view = tauri::async_runtime::spawn_blocking(move || inspect(&project))
        .await
        .map_err(|error| error.to_string())??;

    if view.status != DesktopRunStatus::Active {
        return Err(format!("Run is not active (status {:?})", view.status));
    }
    let [surface] = view.surfaces.as_slice() else {
        return Err(format!(
            "expected exactly one Web surface, found {}",
            view.surfaces.len()
        ));
    };
    let url_string = match surface {
        ato_ipc::desktop_control::DesktopSurfaceView::Web { url, profile } => {
            if profile != WEB_SURFACE_PROFILE {
                return Err(format!("unsupported surface profile {profile}"));
            }
            url.clone()
        }
        ato_ipc::desktop_control::DesktopSurfaceView::Terminal { .. } => {
            return Err("Terminal surfaces are not openable yet".to_owned());
        }
    };
    let url: Url = url_string
        .parse()
        .map_err(|error| format!("invalid surface URL: {error}"))?;
    if !navigation::is_loopback_surface_url(&url) {
        return Err("surface URL is not a loopback HTTP(S) origin".to_owned());
    }
    let origin = navigation::url_origin(&url);
    let label = windows::app_window_label(&view.project);
    crate::open_app_window(&app, &label, url, &origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_maps_to_exact_argv() {
        assert_eq!(
            argv_for(&ComputationCommand::Init {
                capsule: "demo".to_owned(),
                initial_only: false,
            }),
            vec!["init", "demo"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Init {
                capsule: "demo".to_owned(),
                initial_only: true,
            }),
            vec!["init", "demo", "--initial-only"]
        );
    }

    #[test]
    fn resume_maps_to_exact_argv_with_optional_branch() {
        assert_eq!(
            argv_for(&ComputationCommand::Resume {
                selector: "demo@main".to_owned(),
                branch: None,
            }),
            vec!["resume", "demo@main"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Resume {
                selector: "demo@main#1".to_owned(),
                branch: Some("experiment".to_owned()),
            }),
            vec!["resume", "demo@main#1", "--branch", "experiment"]
        );
    }

    #[test]
    fn stop_encap_and_run_map_to_exact_argv() {
        assert_eq!(
            argv_for(&ComputationCommand::Stop {
                capsule: "demo".to_owned()
            }),
            vec!["stop", "demo"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Encap {
                selector: "demo@main".to_owned(),
                output: "out.capsule".to_owned(),
            }),
            vec!["encap", "demo@main", "-o", "out.capsule"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::RunPortable {
                capsule_file: "x.capsule".to_owned(),
            }),
            vec!["run", "x.capsule"]
        );
    }
}
