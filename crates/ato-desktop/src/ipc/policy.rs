//! IPC visibility tiers and policy enforcement.
//!
//! The policy layer answers two questions for every incoming request:
//!
//! 1. **Is this command reachable from a WebView transport?**
//!    `InternalOnly` commands are rejected at the transport adapter,
//!    before any registry lookup, so they never reach the broker.
//!
//! 2. **Is this principal allowed to call this command?**
//!    After a registry hit, `PolicyEngine::check` validates that the
//!    caller's visibility tier is compatible with the command's declared
//!    `IpcVisibility`, and that the principal holds every required
//!    capability.

use crate::system_capsule::broker::Capability;

use super::principal::IpcPrincipal;

/// Visibility tier for an IPC command.
///
/// Determines which principals may call a command and, critically,
/// whether the command is reachable at all from a WebView transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcVisibility {
    /// Any WebView (system or guest capsule) may call this command.
    PublicCapsule,
    /// Only built-in system-capsule WebViews may call this command.
    SystemCapsule,
    /// Never reachable from any WebView transport.
    ///
    /// The WebView transport adapter **must** reject these commands
    /// before the registry lookup, so that the rejection does not
    /// reveal the command's existence to malicious callers.
    /// Only the internal test harness dispatcher may invoke them.
    InternalOnly,
}

/// Outcome of a policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// The request is allowed to proceed.
    Allow,
    /// The command is not accessible from any WebView transport.
    ///
    /// The transport adapter should return a generic "unknown command"
    /// error rather than "forbidden" to avoid leaking existence.
    InternalOnlyCommand,
    /// The principal's tier does not match the command's visibility.
    WrongTier,
    /// The principal is missing one or more required capabilities.
    MissingCapability(Capability),
}

/// Stateless policy engine.
///
/// Instantiate once at startup and share via `Arc`; all methods are
/// pure functions of their arguments.
#[derive(Debug, Default)]
pub struct PolicyEngine;

impl PolicyEngine {
    /// Check whether `principal` may invoke a command with the given
    /// `visibility` and `required_capabilities`.
    ///
    /// Call this **after** verifying the command is not `InternalOnly`
    /// at the transport layer.  If the command is `InternalOnly` and
    /// somehow reaches this method, `PolicyOutcome::InternalOnlyCommand`
    /// is returned so the broker can surface a hard error.
    pub fn check(
        &self,
        principal: &IpcPrincipal,
        visibility: IpcVisibility,
        required_capabilities: &[Capability],
    ) -> PolicyOutcome {
        if visibility == IpcVisibility::InternalOnly {
            return PolicyOutcome::InternalOnlyCommand;
        }

        match visibility {
            IpcVisibility::PublicCapsule => {
                // Guest capsules, system capsules, and the desktop shell
                // may all call public commands.
            }
            IpcVisibility::SystemCapsule => {
                if !principal.is_any_system_capsule() && !principal.is_desktop_shell() {
                    return PolicyOutcome::WrongTier;
                }
            }
            IpcVisibility::InternalOnly => unreachable!("handled above"),
        }

        // Capability check: only meaningful for system-capsule principals
        // since we derive their allowed_capabilities from the static
        // descriptor table.  Guest capsule capability checks are handled
        // by the guest bridge layer (bridge.rs), which already validates
        // against the capsule manifest.
        if principal.is_any_system_capsule() {
            use crate::system_capsule::manifest::lookup;
            if let IpcPrincipal::SystemCapsule { id, .. } = principal {
                let descriptor = lookup(*id);
                for &cap in required_capabilities {
                    if !descriptor.allowed_capabilities.contains(&cap) {
                        return PolicyOutcome::MissingCapability(cap);
                    }
                }
            }
        }

        PolicyOutcome::Allow
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::system_capsule::broker::SystemCapsuleId;
    use crate::ipc::principal::IpcPrincipal;

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

    #[test]
    fn guest_cannot_call_system_only_command() {
        let engine = PolicyEngine::default();
        let outcome = engine.check(&guest_principal(), IpcVisibility::SystemCapsule, &[]);
        assert_eq!(outcome, PolicyOutcome::WrongTier);
    }

    #[test]
    fn system_capsule_can_call_system_only_command() {
        let engine = PolicyEngine::default();
        let outcome = engine.check(
            &system_principal(SystemCapsuleId::AtoStore),
            IpcVisibility::SystemCapsule,
            &[],
        );
        assert_eq!(outcome, PolicyOutcome::Allow);
    }

    #[test]
    fn guest_can_call_public_command() {
        let engine = PolicyEngine::default();
        let outcome = engine.check(&guest_principal(), IpcVisibility::PublicCapsule, &[]);
        assert_eq!(outcome, PolicyOutcome::Allow);
    }

    #[test]
    fn internal_only_command_always_denied() {
        let engine = PolicyEngine::default();
        for principal in [
            system_principal(SystemCapsuleId::AtoStore),
            guest_principal(),
            IpcPrincipal::DesktopShell,
        ] {
            let outcome = engine.check(&principal, IpcVisibility::InternalOnly, &[]);
            assert_eq!(outcome, PolicyOutcome::InternalOnlyCommand);
        }
    }

    #[test]
    fn system_capsule_missing_capability_denied() {
        let engine = PolicyEngine::default();
        // AtoOnboarding only has OnboardingComplete; SettingsWrite is not granted.
        let outcome = engine.check(
            &system_principal(SystemCapsuleId::AtoOnboarding),
            IpcVisibility::SystemCapsule,
            &[Capability::SettingsWrite],
        );
        assert_eq!(outcome, PolicyOutcome::MissingCapability(Capability::SettingsWrite));
    }
}
