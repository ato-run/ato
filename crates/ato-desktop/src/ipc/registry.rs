//! IPC command registry — maps command names to their spec and handler.
//!
//! The registry is the single source of truth for which commands exist,
//! what visibility tier they require, and which capabilities the caller
//! must hold.
//!
//! # Design
//!
//! Commands are registered at startup as `IpcCommandSpec` entries.
//! The `IpcCommandRegistry` is built once and shared immutably.  All
//! dispatch goes through `IpcBroker` (see `broker.rs`), which calls
//! `registry.get()` before the policy check.
//!
//! # Handler type
//!
//! Handlers are `Arc<dyn IpcHandler>` to allow dynamic dispatch while
//! keeping ownership simple.  Each handler receives the resolved
//! `IpcPrincipal` and the raw `IpcRequest`; it returns an
//! `IpcResponse` synchronously.  Async commands should spawn their
//! work with `cx.spawn` and return a placeholder response immediately
//! (fire-and-forget with a subsequent `evaluate_script` callback).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::system_capsule::broker::Capability;

use super::policy::IpcVisibility;
use super::principal::IpcPrincipal;
use super::protocol::{IpcRequest, IpcResponse};

/// Synchronous handler for a single IPC command.
pub trait IpcHandler: Send + Sync + fmt::Debug {
    fn handle(&self, principal: &IpcPrincipal, request: &IpcRequest) -> IpcResponse;
}

/// Metadata for a registered IPC command.
#[derive(Debug)]
pub struct IpcCommandSpec {
    /// Dot-separated namespace.action (e.g. `"capsule.context.get"`).
    pub name: &'static str,
    /// Who may call this command.
    pub visibility: IpcVisibility,
    /// Every capability listed here must be held by the caller's principal.
    pub required_capabilities: &'static [Capability],
    /// The handler that processes the command.
    pub handler: Arc<dyn IpcHandler>,
}

/// Registry of all known IPC commands.
///
/// Built once at startup via `IpcCommandRegistry::builder()`.
#[derive(Debug, Default)]
pub struct IpcCommandRegistry {
    commands: HashMap<&'static str, IpcCommandSpec>,
}

impl IpcCommandRegistry {
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::default()
    }

    /// Look up a command spec by exact name.
    ///
    /// Returns `None` for unknown commands.  The broker converts this
    /// into an `IpcResponse::unknown_command` error.
    pub fn get(&self, command: &str) -> Option<&IpcCommandSpec> {
        self.commands.get(command)
    }

    /// Number of registered commands (useful for tests).
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Builder for `IpcCommandRegistry`.
#[derive(Debug, Default)]
pub struct RegistryBuilder {
    commands: Vec<IpcCommandSpec>,
}

impl RegistryBuilder {
    pub fn register(mut self, spec: IpcCommandSpec) -> Self {
        self.commands.push(spec);
        self
    }

    /// Register multiple command specs produced by a namespace `specs()` function.
    pub fn register_many(mut self, specs: Vec<IpcCommandSpec>) -> Self {
        self.commands.extend(specs);
        self
    }

    pub fn build(self) -> IpcCommandRegistry {
        let commands = self
            .commands
            .into_iter()
            .map(|s| (s.name, s))
            .collect();
        IpcCommandRegistry { commands }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::policy::IpcVisibility;

    #[derive(Debug)]
    struct EchoHandler;
    impl IpcHandler for EchoHandler {
        fn handle(&self, _: &IpcPrincipal, req: &IpcRequest) -> IpcResponse {
            IpcResponse::ok(req.id, req.params.clone())
        }
    }

    #[test]
    fn registered_command_is_found() {
        let registry = IpcCommandRegistry::builder()
            .register(IpcCommandSpec {
                name: "test.echo",
                visibility: IpcVisibility::PublicCapsule,
                required_capabilities: &[],
                handler: Arc::new(EchoHandler),
            })
            .build();

        assert!(registry.get("test.echo").is_some());
        assert!(registry.get("test.unknown").is_none());
        assert_eq!(registry.len(), 1);
    }
}
