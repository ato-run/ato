//! `IpcBroker` — the single dispatch chokepoint for all IPC commands.
//!
//! # Dispatch order
//!
//! 1. Registry lookup — `UnknownCommand` if missing.
//! 2. `InternalOnly` guard — `UnknownCommand` (not `Forbidden`) to avoid
//!    leaking existence to untrusted callers.
//! 3. Policy check (visibility tier + capability check).
//! 4. Handler invocation.
//!
//! # WebView transport responsibility
//!
//! The WebView transport adapter (`system_capsule/ipc.rs`) MUST screen
//! `InternalOnly` commands **before** calling `IpcBroker::dispatch`.
//! The broker also checks, so double rejection is harmless, but the
//! transport should never forward internal commands at all.

use super::policy::{IpcVisibility, PolicyOutcome};
use super::principal::IpcPrincipal;
use super::protocol::{IpcRequest, IpcResponse};
use super::registry::IpcCommandRegistry;

/// Stateless dispatch broker.
///
/// Owns an `IpcCommandRegistry` and a `PolicyEngine`.  Create one at
/// startup and share via `Arc`.
#[derive(Debug)]
pub struct IpcBroker {
    registry: IpcCommandRegistry,
    policy: super::policy::PolicyEngine,
}

impl IpcBroker {
    pub fn new(registry: IpcCommandRegistry) -> Self {
        Self {
            registry,
            policy: super::policy::PolicyEngine,
        }
    }

    /// Dispatch a single IPC request from `principal`.
    ///
    /// Always returns an `IpcResponse`.  Never panics; unknown commands,
    /// policy violations, and handler errors all produce typed responses.
    pub fn dispatch(&self, principal: &IpcPrincipal, request: &IpcRequest) -> IpcResponse {
        let Some(spec) = self.registry.get(&request.command) else {
            return IpcResponse::unknown_command(request.id, &request.command);
        };

        // InternalOnly commands must never be reachable from WebView transport.
        // Return the same error as "unknown command" to avoid leaking existence.
        if spec.visibility == IpcVisibility::InternalOnly {
            return IpcResponse::unknown_command(request.id, &request.command);
        }

        match self
            .policy
            .check(principal, spec.visibility, spec.required_capabilities)
        {
            PolicyOutcome::Allow => {}
            PolicyOutcome::InternalOnlyCommand => {
                return IpcResponse::unknown_command(request.id, &request.command);
            }
            PolicyOutcome::WrongTier => {
                return IpcResponse::forbidden(
                    request.id,
                    format!(
                        "command '{}' is not available to this caller tier",
                        request.command
                    ),
                );
            }
            PolicyOutcome::MissingCapability(cap) => {
                return IpcResponse::forbidden(
                    request.id,
                    format!(
                        "command '{}' requires capability {:?} which is not granted",
                        request.command, cap
                    ),
                );
            }
        }

        spec.handler.handle(principal, request)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::ipc::policy::IpcVisibility;
    use crate::ipc::principal::IpcPrincipal;
    use crate::ipc::registry::{IpcCommandRegistry, IpcCommandSpec, IpcHandler};
    use crate::system_capsule::broker::{Capability, SystemCapsuleId};

    #[derive(Debug)]
    struct OkHandler;
    impl IpcHandler for OkHandler {
        fn handle(&self, _: &IpcPrincipal, req: &IpcRequest) -> IpcResponse {
            IpcResponse::ok(req.id, serde_json::Value::Null)
        }
    }

    fn build_broker() -> IpcBroker {
        let registry = IpcCommandRegistry::builder()
            .register(IpcCommandSpec {
                name: "session.start",
                visibility: IpcVisibility::SystemCapsule,
                required_capabilities: &[],
                handler: Arc::new(OkHandler),
            })
            .register(IpcCommandSpec {
                name: "shell.openExternal",
                visibility: IpcVisibility::PublicCapsule,
                required_capabilities: &[],
                handler: Arc::new(OkHandler),
            })
            .register(IpcCommandSpec {
                name: "debug.reloadSystemCapsule",
                visibility: IpcVisibility::InternalOnly,
                required_capabilities: &[],
                handler: Arc::new(OkHandler),
            })
            .register(IpcCommandSpec {
                name: "settings.set",
                visibility: IpcVisibility::SystemCapsule,
                required_capabilities: &[Capability::SettingsWrite],
                handler: Arc::new(OkHandler),
            })
            .build();
        IpcBroker::new(registry)
    }

    fn system_principal(id: SystemCapsuleId) -> IpcPrincipal {
        IpcPrincipal::SystemCapsule {
            id,
            canonical_slug: "test".into(),
            materialized_root: PathBuf::from("/tmp/test"),
        }
    }

    fn guest_principal() -> IpcPrincipal {
        IpcPrincipal::Capsule {
            handle: "org/tool@0.1".into(),
            target: "wasm".into(),
            execution_id: "exec-1".into(),
            session_id: "sess-1".into(),
            origin: "capsule://atousercontent.com/org/tool".into(),
        }
    }

    fn req(command: &str) -> IpcRequest {
        IpcRequest {
            id: Some(1),
            command: command.to_string(),
            params: serde_json::Value::Null,
        }
    }

    #[test]
    fn unknown_command_returns_typed_error() {
        let broker = build_broker();
        let r = broker.dispatch(&guest_principal(), &req("doesnt.exist"));
        assert!(matches!(r, IpcResponse::Error { ref code, .. } if code == "unknown_command"));
    }

    #[test]
    fn guest_cannot_call_session_start() {
        let broker = build_broker();
        let r = broker.dispatch(&guest_principal(), &req("session.start"));
        assert!(matches!(r, IpcResponse::Error { ref code, .. } if code == "forbidden"));
    }

    #[test]
    fn system_capsule_can_call_session_start() {
        let broker = build_broker();
        let r = broker.dispatch(
            &system_principal(SystemCapsuleId::AtoStore),
            &req("session.start"),
        );
        assert!(matches!(r, IpcResponse::Ok { .. }));
    }

    #[test]
    fn guest_can_call_public_command() {
        let broker = build_broker();
        let r = broker.dispatch(&guest_principal(), &req("shell.openExternal"));
        assert!(matches!(r, IpcResponse::Ok { .. }));
    }

    #[test]
    fn internal_only_command_returns_unknown_from_webview_transport() {
        let broker = build_broker();
        // Even a system capsule principal cannot reach InternalOnly commands
        // through the broker (same error as unknown to avoid leaking existence).
        let r = broker.dispatch(
            &system_principal(SystemCapsuleId::AtoStore),
            &req("debug.reloadSystemCapsule"),
        );
        assert!(matches!(r, IpcResponse::Error { ref code, .. } if code == "unknown_command"));
    }

    #[test]
    fn missing_capability_returns_forbidden() {
        let broker = build_broker();
        // AtoOnboarding does not have SettingsWrite capability.
        let r = broker.dispatch(
            &system_principal(SystemCapsuleId::AtoOnboarding),
            &req("settings.set"),
        );
        assert!(matches!(r, IpcResponse::Error { ref code, .. } if code == "forbidden"));
    }

    #[test]
    fn spoofed_is_system_field_in_envelope_has_no_effect() {
        // The broker doesn't read the envelope at all — principal is derived
        // from Rust-side window/session context.  A guest principal stays a
        // guest no matter what the JS envelope claims.
        let broker = build_broker();
        // Simulate: JS sent { isSystem: true } but the Rust transport resolved
        // the principal as IpcPrincipal::Capsule (guest).
        let r = broker.dispatch(&guest_principal(), &req("session.start"));
        assert!(matches!(r, IpcResponse::Error { ref code, .. } if code == "forbidden"));
    }
}
