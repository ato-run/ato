//! Dock system capsule IPC handler.
//!
//! Handles commands sent from the `ato-dock` WebView page. The `Login`
//! command spawns `ato login --desktop` as a child process. The CLI opens
//! the OS default browser itself (reusing the same `try_open_browser`
//! helper the plain `ato login` command already uses) and polls the
//! auth_bridge for completion exactly as it does for a plain interactive
//! login — this module's only job is to watch the child's NDJSON stdout
//! for a terminal event and refresh the Dock when it's done.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use super::broker::Capability;
use crate::orchestrator::resolve_ato_binary;
use crate::proc_util::CommandNoWindowExt;

/// Source-of-truth shape for a developer-imported capsule project.
/// Drives both the cloning/validation step and how the inferred
/// manifest's `name` slug seed is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSourceKind {
    GithubRepo,
    LocalPath,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockCommand {
    Login,
}

impl DockCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            DockCommand::Login => Capability::LaunchSystemCapsule,
        }
    }
}

pub fn dispatch(cx: &mut App, _host: AnyWindowHandle, command: DockCommand) -> Result<()> {
    match command {
        DockCommand::Login => trigger_login(cx),
    }
}

// ── NDJSON event types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DesktopLoginEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    publisher_handle: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Spawn `ato login --desktop` and watch its NDJSON stdout for completion.
/// The CLI process opens the system browser itself and polls the
/// auth_bridge on its own; this function does not open any window — it
/// just notices when the child is done and refreshes the Dock.
fn trigger_login(cx: &mut App) -> Result<()> {
    let ato_bin = resolve_ato_binary().context("ato binary not found")?;
    tracing::info!(ato_bin = %ato_bin.display(), "ato_dock: spawning ato login --desktop");

    let mut child: Child = Command::new(&ato_bin)
        .no_console_window()
        .arg("login")
        .arg("--desktop")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .context("failed to spawn ato login --desktop")?;

    let stdout = child.stdout.take().context("no stdout from child")?;
    let reader = BufReader::new(stdout);

    // ── Background watcher using GPUI executors ────────────────────────────────
    // `be.spawn` runs the blocking I/O on background threads (requires Send).
    // `fe.spawn` schedules the UI update on the main GPUI thread (non-Send ok).
    // `aa` (AsyncApp, non-Send) is only used inside `fe.spawn`.
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let completion = be
            .spawn(async move { watch_login_completion(reader, child) })
            .await;
        aa.update(|cx| {
            on_login_completion(cx, completion);
        });
    })
    .detach();

    Ok(())
}

// ── Completion result ─────────────────────────────────────────────────────────

enum LoginCompletion {
    Success { publisher_handle: Option<String> },
    Failure { message: String },
}

/// Reads stdout from the child and waits for it to exit.
fn watch_login_completion(
    reader: BufReader<impl std::io::Read>,
    mut child: Child,
) -> LoginCompletion {
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(event) = serde_json::from_str::<DesktopLoginEvent>(line.trim()) else {
            continue;
        };
        match event.kind.as_str() {
            "desktop_login_completed" => {
                let _ = child.wait();
                return LoginCompletion::Success {
                    publisher_handle: event.publisher_handle,
                };
            }
            "desktop_login_failed" => {
                let _ = child.wait();
                return LoginCompletion::Failure {
                    message: event.message.unwrap_or_else(|| "login failed".to_string()),
                };
            }
            _ => {}
        }
    }

    // Process exited without a completion event.
    let exit_status = child.wait();
    match exit_status {
        Ok(s) if s.success() => LoginCompletion::Success {
            publisher_handle: None,
        },
        Ok(s) => LoginCompletion::Failure {
            message: format!("ato login exited with status {}", s),
        },
        Err(e) => LoginCompletion::Failure {
            message: format!("waiting for ato login failed: {}", e),
        },
    }
}

/// Called on the GPUI thread after the child process finishes.
fn on_login_completion(cx: &mut App, result: LoginCompletion) {
    match result {
        LoginCompletion::Success { publisher_handle } => {
            tracing::info!(
                publisher_handle = publisher_handle.as_deref().unwrap_or("(unknown)"),
                "Desktop login completed successfully"
            );
            crate::window::dock::notify_login_success(cx);
        }
        LoginCompletion::Failure { message } => {
            tracing::warn!(message, "Desktop login failed or was cancelled");
            // Bring the existing dock to front (it still shows the login page).
            let _ = crate::window::dock::open_dock_window(cx);
        }
    }
}
