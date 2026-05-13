//! IPC envelope parser for system-capsule WebViews.
//!
//! The HTML pages (`assets/system/<name>/...`) emit a typed JSON
//! envelope of the shape:
//!
//! ```json
//! { "capsule": "ato-windows",
//!   "command": { "kind": "activate_window", "windowId": 42 } }
//! ```
//!
//! `make_ipc_handler` parses one such envelope per IPC call, resolves
//! the capsule slug to a `SystemCapsuleId`, parses the `command` value
//! into the matching `*Command` enum, and pushes a typed
//! `(SystemCapsuleId, SystemCommand)` pair onto a shared queue. A
//! foreground drain loop (`spawn_drain_loop`) trampolines onto the
//! GPUI main thread and hands each pair to `CapabilityBroker::dispatch`,
//! which validates capability allowlist before routing to the
//! per-capsule handler.
//!
//! This module replaces the per-window dispatcher pattern in
//! `crate::window::web_bridge` for system-capsule WebViews. The old
//! `web_bridge.rs` stays as-is to serve any other Wry consumers (none
//! at the moment of Stage B — kept for one release before removal).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use super::ato_identity::IdentityCommand;
use super::ato_launch::LaunchCommand;
use super::ato_settings::SettingsCommand;
use super::ato_store::StoreCommand;
use super::ato_web_viewer::WebViewerCommand;
use super::ato_windows::WindowsCommand;
use super::broker::{CapabilityBroker, SystemCapsuleId, SystemCommand};

#[derive(Debug, Deserialize)]
struct Envelope {
    /// Slug — must match one of `ato-windows`, `ato-store`,
    /// `ato-settings`, `ato-web-viewer`. Unknown slugs are dropped
    /// at the IPC boundary with a warn-level log.
    capsule: String,
    /// Per-capsule command payload. Parsed lazily once the capsule
    /// slug is resolved.
    command: serde_json::Value,
}

pub type SystemBridgeQueue = Arc<Mutex<Vec<(SystemCapsuleId, SystemCommand)>>>;

pub fn new_queue() -> SystemBridgeQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Build the closure handed to `WebViewBuilder::with_ipc_handler`.
/// Runs on whatever thread Wry chooses; only touches the queue.
/// Errors are logged at WARN and dropped so a malformed message
/// never propagates beyond the bridge boundary.
pub fn make_ipc_handler(
    queue: SystemBridgeQueue,
) -> impl Fn(wry::http::Request<String>) + 'static {
    move |request: wry::http::Request<String>| {
        let body = request.body();
        let envelope: Envelope = match serde_json::from_str(body) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, body = %body, "system_capsule::ipc: unparseable envelope");
                return;
            }
        };
        let capsule = match envelope.capsule.as_str() {
            "ato-windows" => SystemCapsuleId::AtoWindows,
            "ato-store" => SystemCapsuleId::AtoStore,
            "ato-settings" => SystemCapsuleId::AtoSettings,
            "ato-web-viewer" => SystemCapsuleId::AtoWebViewer,
            "ato-launch" => SystemCapsuleId::AtoLaunch,
            "ato-identity" => SystemCapsuleId::AtoIdentity,
            other => {
                tracing::warn!(slug = %other, "system_capsule::ipc: unknown capsule slug");
                return;
            }
        };
        let command_result = match capsule {
            SystemCapsuleId::AtoWindows => serde_json::from_value::<WindowsCommand>(envelope.command)
                .map(SystemCommand::AtoWindows),
            SystemCapsuleId::AtoStore => serde_json::from_value::<StoreCommand>(envelope.command)
                .map(SystemCommand::AtoStore),
            SystemCapsuleId::AtoSettings => {
                serde_json::from_value::<SettingsCommand>(envelope.command)
                    .map(SystemCommand::AtoSettings)
            }
            SystemCapsuleId::AtoWebViewer => {
                serde_json::from_value::<WebViewerCommand>(envelope.command)
                    .map(SystemCommand::AtoWebViewer)
            }
            SystemCapsuleId::AtoLaunch => serde_json::from_value::<LaunchCommand>(envelope.command)
                .map(SystemCommand::AtoLaunch),
            SystemCapsuleId::AtoIdentity => {
                serde_json::from_value::<IdentityCommand>(envelope.command)
                    .map(SystemCommand::AtoIdentity)
            }
        };
        match command_result {
            Ok(cmd) => {
                if let Ok(mut q) = queue.lock() {
                    q.push((capsule, cmd));
                }
            }
            Err(err) => {
                tracing::warn!(?capsule, error = %err, "system_capsule::ipc: command parse failed");
            }
        }
    }
}

/// Spawn the foreground drain loop that pulls typed `(capsule,
/// command)` pairs off the queue and dispatches each through
/// `CapabilityBroker::dispatch`. Trampolines onto the GPUI main
/// thread so the broker has full `&mut App` access. Terminates when
/// the host window closes (probe via `host.update`).
pub fn spawn_drain_loop(
    cx: &mut App,
    queue: SystemBridgeQueue,
    host: AnyWindowHandle,
) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            let drained: Vec<(SystemCapsuleId, SystemCommand)> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                let host_alive: bool =
                    aa.update(|cx| host.update(cx, |_, _, _| ()).is_ok());
                if !host_alive {
                    return;
                }
                continue;
            }
            for (capsule, command) in drained {
                aa.update(|cx| {
                    if let Err(err) = CapabilityBroker::dispatch(cx, host, capsule, command) {
                        tracing::warn!(?err, ?capsule, "system_capsule::ipc: broker dispatch failed");
                    }
                });
            }
        }
    })
    .detach();
}
