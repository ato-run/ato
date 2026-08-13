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
use super::window_registry::SystemCapsuleWindowRegistry;
use crate::ipc::protocol::IpcResponse;

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

/// Capsule-bound IPC handler — the preferred variant for all system-capsule
/// WebViews.
///
/// Only accepts envelopes where `manifest::lookup_by_slug(envelope.capsule)`
/// resolves to `expected_capsule`.  Any envelope claiming a different capsule
/// identity is rejected with a WARN log.  This prevents a compromised system
/// capsule page from spoofing another capsule's commands.
pub fn make_ipc_handler_for_capsule(
    expected_capsule: SystemCapsuleId,
    queue: SystemBridgeQueue,
) -> impl Fn(wry::http::Request<String>) + 'static {
    make_ipc_handler_inner(Some(expected_capsule), queue)
}

fn make_ipc_handler_inner(
    expected_capsule: Option<SystemCapsuleId>,
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
        let capsule = match manifest::lookup_by_slug(envelope.capsule.as_str()) {
            Some(id) => id,
            None => {
                tracing::warn!(slug = %envelope.capsule, "system_capsule::ipc: unknown capsule slug");
                return;
            }
        };
        if let Some(expected) = expected_capsule
            && capsule != expected
        {
            tracing::warn!(
                received = %envelope.capsule,
                expected = ?expected,
                "system_capsule::ipc: cross-capsule spoof rejected"
            );
            return;
        }
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
    use crate::runtime_setup::RuntimeSetupCommand;

    // Runtime Setup is a feature shared by onboarding and settings: route by
    // command `kind`, not by capsule name, so both surfaces hit the same
    // backend. The broker still gates each request against the calling
    // capsule's manifest grant.
    if matches!(
        capsule,
        SystemCapsuleId::AtoOnboarding | SystemCapsuleId::AtoSettings
    ) && command
        .get("kind")
        .and_then(|k| k.as_str())
        .is_some_and(RuntimeSetupCommand::is_runtime_setup_kind)
    {
        return serde_json::from_value::<RuntimeSetupCommand>(command)
            .map(SystemCommand::RuntimeSetup);
    }

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
    // Capture the window id at spawn time so the check below never touches
    // the (potentially closed) AnyWindowHandle after loop termination.
    let host_window_id = host.window_id();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            // Defer while a Wry `build_as_child` is pumping the Win32 message
            // loop on the main thread (Windows): resuming and calling `update`
            // here would re-borrow the GPUI `App` and panic.
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
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
                    // Deny dispatch if *this specific host window* is no longer
                    // registered for the capsule.  Checking by window_id (rather
                    // than capsule id alone) ensures that closing one of several
                    // concurrent AtoLaunch windows does not silently deny IPC for
                    // the remaining open windows of the same capsule.
                    if !cx
                        .global::<SystemCapsuleWindowRegistry>()
                        .has_binding_for_window(capsule, host_window_id)
                    {
                        tracing::warn!(
                            ?capsule,
                            "system_capsule::ipc: denied — no window binding registered"
                        );
                        if let (Some(rid), Some(cb)) = (request_id, response_cb.as_ref()) {
                            let response = IpcResponse::error(
                                Some(rid),
                                "no_binding",
                                "no active window registered for this capsule".to_string(),
                            );
                            if let Ok(json) = serde_json::to_string(&response) {
                                cb(cx, rid, json);
                            }
                        }
                        return;
                    }
                    let result = CapabilityBroker::dispatch(cx, host, capsule, command);
                    if let (Some(rid), Some(cb)) = (request_id, response_cb.as_ref()) {
                        let response = match result {
                            Ok(()) => IpcResponse::ok(Some(rid), serde_json::Value::Null),
                            Err(ref err) => {
                                IpcResponse::error(Some(rid), "dispatch_error", format!("{err:?}"))
                            }
                        };
                        match serde_json::to_string(&response) {
                            Ok(json) => cb(cx, rid, json),
                            Err(e) => {
                                tracing::warn!(?e, "system_capsule::ipc: response serialise failed")
                            }
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

/// Run follow-up UI work after the current system-capsule IPC dispatch has
/// unwound. Wry/WebView2 can pump the native message loop during
/// `build_as_child`; starting another WebView from the same GPUI update that
/// is handling an IPC callback can re-enter GPUI while its App borrow is still
/// active on Windows.
pub fn defer_after_dispatch<F>(cx: &mut App, action: F)
where
    F: FnOnce(&mut App) + 'static,
{
    defer_after_dispatch_for(cx, Duration::from_millis(0), action);
}

pub fn defer_after_dispatch_for<F>(cx: &mut App, delay: Duration, action: F)
where
    F: FnOnce(&mut App) + 'static,
{
    let async_app = cx.to_async();
    let bg_exec = async_app.background_executor().clone();
    let update_app = async_app.clone();
    async_app
        .foreground_executor()
        .spawn(async move {
            bg_exec.timer(delay).await;
            crate::webview_init_guard::wait_until_idle(&bg_exec).await;
            update_app.update(action);
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

    #[test]
    fn cross_capsule_spoof_is_rejected_by_bound_handler() {
        // A bound dock handler must reject envelopes claiming to be ato-settings.
        let queue = new_queue();
        let handler = make_ipc_handler_for_capsule(SystemCapsuleId::AtoDock, queue.clone());
        // Synthesise a fake Wry request body claiming to be ato-settings.
        let body = r#"{"capsule":"ato-settings","command":{"kind":"close"},"requestId":1}"#;
        let fake_req = wry::http::Request::builder()
            .body(body.to_string())
            .unwrap();
        handler(fake_req);
        // The spoof must have been silently rejected — queue stays empty.
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn runtime_setup_kind_routes_to_feature_command_from_onboarding() {
        let cmd = parse_system_command(
            SystemCapsuleId::AtoOnboarding,
            serde_json::json!({ "kind": "runtime_setup_status" }),
        )
        .unwrap();
        assert!(matches!(cmd, SystemCommand::RuntimeSetup(_)));
    }

    #[test]
    fn runtime_setup_kind_routes_to_feature_command_from_settings() {
        let cmd = parse_system_command(
            SystemCapsuleId::AtoSettings,
            serde_json::json!({ "kind": "install_runtime_tools", "tools": ["node"] }),
        )
        .unwrap();
        assert!(matches!(cmd, SystemCommand::RuntimeSetup(_)));
    }

    #[test]
    fn prepare_runtime_tools_routes_to_feature_command_from_both_surfaces() {
        for capsule in [SystemCapsuleId::AtoOnboarding, SystemCapsuleId::AtoSettings] {
            let cmd = parse_system_command(
                capsule,
                serde_json::json!({ "kind": "prepare_runtime_tools", "tools": ["podman"] }),
            )
            .unwrap();
            assert!(
                matches!(cmd, SystemCommand::RuntimeSetup(_)),
                "prepare_runtime_tools from {capsule:?} should route to RuntimeSetup"
            );
        }
    }

    #[test]
    fn settings_native_command_still_routes_to_settings() {
        // A camelCase settings command must not be hijacked by the runtime
        // feature router.
        let cmd = parse_system_command(
            SystemCapsuleId::AtoSettings,
            serde_json::json!({ "kind": "loadSnapshot" }),
        )
        .unwrap();
        assert!(matches!(cmd, SystemCommand::AtoSettings(_)));
    }

    #[test]
    fn runtime_setup_kind_not_special_cased_for_other_capsules() {
        // Only onboarding/settings opt into the shared router; a store envelope
        // with a runtime kind must fall through to the store parser (and fail).
        assert!(
            parse_system_command(
                SystemCapsuleId::AtoStore,
                serde_json::json!({ "kind": "runtime_setup_status" }),
            )
            .is_err()
        );
    }

    #[test]
    fn bound_handler_accepts_own_capsule() {
        let queue = new_queue();
        let handler = make_ipc_handler_for_capsule(SystemCapsuleId::AtoOnboarding, queue.clone());
        // A valid onboarding command from the onboarding capsule.
        let body = r#"{"capsule":"ato-onboarding","command":{"kind":"complete","version":1},"requestId":2}"#;
        let fake_req = wry::http::Request::builder()
            .body(body.to_string())
            .unwrap();
        handler(fake_req);
        // The valid command must have been enqueued.
        assert_eq!(queue.lock().unwrap().len(), 1);
    }
}
