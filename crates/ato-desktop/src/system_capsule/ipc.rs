//! IPC envelope parser for system-capsule WebViews.
//!
//! The HTML pages (`assets/system/<name>/...`) emit a typed JSON
//! envelope of the shape:
//!
//! ```json
//! { "capsule": "ato-windows",
//!   "command": { "kind": "activate_window", "windowId": 42 },
//!   "requestId": 1 }
//! ```
//!
//! The `requestId` field is optional.  When present the host responds via
//! `window.__atoIpcResolve(id, responseJson)`, which the JS preload
//! (`SYSTEM_IPC_INIT_SCRIPT`) connects to a `Promise`-based API.
//!
//! `make_ipc_handler` parses one such envelope per IPC call, resolves
//! the capsule slug to a `SystemCapsuleId` via `manifest::lookup_by_slug`
//! (never from a local hard-coded match on the JS-supplied string), parses
//! the `command` value into the matching `*Command` enum, and pushes a typed
//! `(SystemCapsuleId, SystemCommand, Option<u64>)` tuple onto a shared queue.
//! A foreground drain loop (`spawn_drain_loop`) trampolines onto the GPUI
//! main thread and hands each tuple to `CapabilityBroker::dispatch`, then
//! delivers a typed `IpcResponse` back to the JS caller when a `request_id`
//! is present.
//!
//! This module replaces the per-window dispatcher pattern in
//! `crate::window::web_bridge` for system-capsule WebViews.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use super::ato_dock::DockCommand;
use super::ato_identity::IdentityCommand;
use super::ato_import::ImportCommand;
use super::ato_launch::LaunchCommand;
use super::ato_onboarding::OnboardingCommand;
use super::ato_settings::SettingsCommand;
use super::ato_start::AtoStartCommand;
use super::ato_store::StoreCommand;
use super::ato_web_viewer::WebViewerCommand;
use super::ato_windows::WindowsCommand;
use super::broker::{CapabilityBroker, SystemCapsuleId, SystemCommand};
use super::manifest;
use crate::ipc::protocol::IpcResponse;

/// JS preload injected into every system-capsule WebView via
/// `WebViewBuilder::with_initialization_script`.
///
/// Installs:
/// - `window.__atoPendingIpc`: `Map<requestId, {resolve, reject}>`
/// - `window.__atoIpcResolve(id, responseJson)`: called by Rust
/// - `window.__ATO_IPC__.invoke(capsule, command, params)`: returns `Promise`
pub const SYSTEM_IPC_INIT_SCRIPT: &str = r#"(function () {
  if (!window.__atoPendingIpc) {
    window.__atoPendingIpc = new Map();
  }
  window.__atoIpcResolve = function (requestId, responseJson) {
    var pending = window.__atoPendingIpc.get(requestId);
    if (!pending) return;
    window.__atoPendingIpc.delete(requestId);
    try {
      var response = JSON.parse(responseJson);
      if (response.status === 'ok') {
        pending.resolve(response.payload);
      } else {
        pending.reject(response);
      }
    } catch (e) {
      pending.reject({ status: 'error', code: 'parse_error', message: String(e) });
    }
  };
  if (!window.__ATO_IPC__) {
    var _nextRequestId = 1;
    window.__ATO_IPC__ = {
      invoke: function (capsule, command, params) {
        return new Promise(function (resolve, reject) {
          var requestId = _nextRequestId++;
          window.__atoPendingIpc.set(requestId, { resolve: resolve, reject: reject });
          window.ipc.postMessage(JSON.stringify({
            capsule: capsule,
            command: command,
            params: params || {},
            requestId: requestId
          }));
        });
      }
    };
  }
})();"#;

#[derive(Debug, Deserialize)]
struct Envelope {
    /// Slug — resolved via `manifest::lookup_by_slug`, which accepts both
    /// canonical short slugs (`"store"`) and legacy `ato-*` aliases.
    /// Unknown slugs are dropped at the IPC boundary with a warn-level log.
    capsule: String,
    /// Per-capsule command payload.  Parsed lazily once the capsule slug
    /// is resolved.
    command: serde_json::Value,
    /// Optional correlation id.  When present the host delivers a typed
    /// `IpcResponse` back to the JS caller via `window.__atoIpcResolve`.
    #[serde(rename = "requestId")]
    request_id: Option<u64>,
}

/// Each queue entry carries the resolved capsule id, the parsed command, and
/// the optional correlation id for the typed-response path.
pub type SystemBridgeQueue = Arc<Mutex<Vec<(SystemCapsuleId, SystemCommand, Option<u64>)>>>;

pub fn new_queue() -> SystemBridgeQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Callback invoked on the GPUI main thread to deliver a typed response back
/// to the JS caller.  Receives the full `&mut App` so callers can reach their
/// `WebView` entity via GPUI globals/handles and call `evaluate_script`.
///
/// The `u64` is the `request_id` and the `String` is the serialised
/// `IpcResponse` JSON.
pub type IpcResponseCallback = Box<dyn Fn(&mut App, u64, String) + 'static>;

/// Build the closure handed to `WebViewBuilder::with_ipc_handler`.
/// Runs on whatever thread Wry chooses; only touches the queue.
/// Errors are logged at WARN and dropped so a malformed message
/// never propagates beyond the bridge boundary.
pub fn make_ipc_handler(queue: SystemBridgeQueue) -> impl Fn(wry::http::Request<String>) + 'static {
    move |request: wry::http::Request<String>| {
        let body = request.body();
        let envelope: Envelope = match serde_json::from_str(body) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, body = %body, "system_capsule::ipc: unparseable envelope");
                return;
            }
        };
        let capsule = match manifest::lookup_by_slug(envelope.capsule.as_str()) {
            Some(id) => id,
            None => {
                tracing::warn!(slug = %envelope.capsule, "system_capsule::ipc: unknown capsule slug");
                return;
            }
        };
        let command_result = parse_system_command(capsule, envelope.command);
        match command_result {
            Ok(cmd) => {
                if let Ok(mut q) = queue.lock() {
                    q.push((capsule, cmd, envelope.request_id));
                }
            }
            Err(err) => {
                tracing::warn!(?capsule, error = %err, "system_capsule::ipc: command parse failed");
            }
        }
    }
}

fn parse_system_command(
    capsule: SystemCapsuleId,
    command: serde_json::Value,
) -> Result<SystemCommand, serde_json::Error> {
    match capsule {
        SystemCapsuleId::AtoWindows => {
            serde_json::from_value::<WindowsCommand>(command).map(SystemCommand::AtoWindows)
        }
        SystemCapsuleId::AtoStore => {
            serde_json::from_value::<StoreCommand>(command).map(SystemCommand::AtoStore)
        }
        SystemCapsuleId::AtoSettings => {
            serde_json::from_value::<SettingsCommand>(command).map(SystemCommand::AtoSettings)
        }
        SystemCapsuleId::AtoWebViewer => {
            serde_json::from_value::<WebViewerCommand>(command).map(SystemCommand::AtoWebViewer)
        }
        SystemCapsuleId::AtoLaunch => {
            serde_json::from_value::<LaunchCommand>(command).map(SystemCommand::AtoLaunch)
        }
        SystemCapsuleId::AtoIdentity => {
            serde_json::from_value::<IdentityCommand>(command).map(SystemCommand::AtoIdentity)
        }
        SystemCapsuleId::AtoStart => {
            serde_json::from_value::<AtoStartCommand>(command).map(SystemCommand::AtoStart)
        }
        SystemCapsuleId::AtoDock => {
            serde_json::from_value::<DockCommand>(command).map(SystemCommand::AtoDock)
        }
        SystemCapsuleId::AtoOnboarding => {
            serde_json::from_value::<OnboardingCommand>(command).map(SystemCommand::AtoOnboarding)
        }
        SystemCapsuleId::AtoImport => {
            serde_json::from_value::<ImportCommand>(command).map(SystemCommand::AtoImport)
        }
    }
}

/// Spawn the foreground drain loop (fire-and-forget variant).
///
/// Backward-compatible shim — all existing call sites continue to compile
/// unchanged.  Use [`spawn_drain_loop_with_response`] when the window needs
/// to deliver typed `IpcResponse` JSON back to the JS caller.
pub fn spawn_drain_loop(cx: &mut App, queue: SystemBridgeQueue, host: AnyWindowHandle) {
    spawn_drain_loop_inner(cx, queue, host, None);
}

/// Spawn the foreground drain loop with a typed-response callback.
///
/// `response_cb` receives `(&mut App, request_id, response_json)` on the GPUI
/// main thread after each dispatched command that carries a `request_id`.
/// Callers typically implement this by calling
/// `webview.evaluate_script(&format!("window.__atoIpcResolve({}, ...)", id))`.
pub fn spawn_drain_loop_with_response(
    cx: &mut App,
    queue: SystemBridgeQueue,
    host: AnyWindowHandle,
    response_cb: IpcResponseCallback,
) {
    spawn_drain_loop_inner(cx, queue, host, Some(response_cb));
}

fn spawn_drain_loop_inner(
    cx: &mut App,
    queue: SystemBridgeQueue,
    host: AnyWindowHandle,
    response_cb: Option<IpcResponseCallback>,
) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            let drained: Vec<(SystemCapsuleId, SystemCommand, Option<u64>)> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                let host_alive: bool = aa.update(|cx| host.update(cx, |_, _, _| ()).is_ok());
                if !host_alive {
                    return;
                }
                continue;
            }
            for (capsule, command, request_id) in drained {
                aa.update(|cx| {
                    let result = CapabilityBroker::dispatch(cx, host, capsule, command);
                    if let (Some(rid), Some(cb)) = (request_id, response_cb.as_ref()) {
                        let response = match result {
                            Ok(()) => IpcResponse::ok(Some(rid), serde_json::Value::Null),
                            Err(ref err) => IpcResponse::error(
                                Some(rid),
                                "dispatch_error",
                                format!("{err:?}"),
                            ),
                        };
                        match serde_json::to_string(&response) {
                            Ok(json) => cb(cx, rid, json),
                            Err(e) => tracing::warn!(?e, "system_capsule::ipc: response serialise failed"),
                        }
                    } else if let Err(err) = result {
                        tracing::warn!(
                            ?err,
                            ?capsule,
                            "system_capsule::ipc: broker dispatch failed"
                        );
                    }
                });
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_legacy_slug_resolves() {
        assert_eq!(
            manifest::lookup_by_slug("ato-onboarding"),
            Some(SystemCapsuleId::AtoOnboarding)
        );
    }

    #[test]
    fn store_canonical_slug_resolves() {
        assert_eq!(
            manifest::lookup_by_slug("store"),
            Some(SystemCapsuleId::AtoStore)
        );
    }

    #[test]
    fn unknown_slug_returns_none() {
        assert!(manifest::lookup_by_slug("not-a-real-capsule").is_none());
    }
}
