//! Per-namespace IPC command specs.
//!
//! Each sub-module owns the `IpcCommandSpec` definitions for one namespace
//! (`capsule.*`, `shell.*`, …).  The `build_default_registry()` function
//! aggregates them into a single `IpcCommandRegistry` that is shared by
//! `IpcBroker` at runtime.
//!
//! # Handler stubs
//!
//! In Phases 5–7 the handlers return a `not_implemented` error — real
//! execution still flows through the legacy `CapabilityBroker`.  Each spec
//! exists here to be the single source of truth for:
//! - the canonical `namespace.action` command name,
//! - the visibility tier (`PublicCapsule` / `SystemCapsule` / `InternalOnly`),
//! - the required capabilities.
//!
//! Phase 8 (window rollout) will replace the stubs with real implementations
//! as each window is migrated.

pub mod account;
pub mod capsule;
pub mod debug;
pub mod onboarding;
pub mod registry;
pub mod session;
pub mod settings;
pub mod shell;

use std::sync::Arc;

use crate::ipc::policy::IpcVisibility;
use crate::ipc::principal::IpcPrincipal;
use crate::ipc::protocol::{IpcRequest, IpcResponse};
use crate::ipc::registry::{IpcCommandRegistry, IpcCommandSpec, IpcHandler};
use crate::system_capsule::broker::Capability;

/// Placeholder handler returned for all Phase 5 command specs.
///
/// Real handlers will replace this during Phase 8 window rollout.
#[derive(Debug)]
pub(crate) struct StubHandler;

impl IpcHandler for StubHandler {
    fn handle(&self, _: &IpcPrincipal, req: &IpcRequest) -> IpcResponse {
        IpcResponse::error(
            req.id,
            "not_implemented".to_string(),
            format!("command '{}' is not yet wired to a handler", req.command),
        )
    }
}

pub(crate) fn stub() -> Arc<StubHandler> {
    Arc::new(StubHandler)
}

pub(crate) fn spec(
    name: &'static str,
    visibility: IpcVisibility,
    required_capabilities: &'static [Capability],
) -> IpcCommandSpec {
    IpcCommandSpec {
        name,
        visibility,
        required_capabilities,
        handler: stub(),
    }
}

/// Build the default `IpcCommandRegistry` with all known commands registered.
///
/// Call once at startup and share the resulting registry (wrapped in
/// `Arc<IpcBroker>`) across all windows.
pub fn build_default_registry() -> IpcCommandRegistry {
    IpcCommandRegistry::builder()
        .register_many(capsule::specs())
        .register_many(shell::specs())
        .register_many(session::specs())
        .register_many(registry::specs())
        .register_many(settings::specs())
        .register_many(account::specs())
        .register_many(onboarding::specs())
        .register_many(debug::specs())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::policy::IpcVisibility;

    #[test]
    fn default_registry_contains_expected_public_commands() {
        let reg = build_default_registry();
        for name in ["capsule.context.get", "shell.openExternal"] {
            let spec = reg
                .get(name)
                .unwrap_or_else(|| panic!("missing command: {name}"));
            assert_eq!(
                spec.visibility,
                IpcVisibility::PublicCapsule,
                "{name} should be PublicCapsule"
            );
        }
    }

    #[test]
    fn default_registry_contains_expected_system_commands() {
        let reg = build_default_registry();
        for name in [
            "session.start",
            "session.stop",
            "registry.search",
            "settings.get",
            "settings.set",
            "account.login",
            "onboarding.complete",
        ] {
            let spec = reg
                .get(name)
                .unwrap_or_else(|| panic!("missing command: {name}"));
            assert_eq!(
                spec.visibility,
                IpcVisibility::SystemCapsule,
                "{name} should be SystemCapsule"
            );
        }
    }

    #[test]
    fn default_registry_contains_internal_only_commands() {
        let reg = build_default_registry();
        let spec = reg
            .get("debug.reloadSystemCapsule")
            .expect("missing debug.reloadSystemCapsule");
        assert_eq!(spec.visibility, IpcVisibility::InternalOnly);
    }
}
