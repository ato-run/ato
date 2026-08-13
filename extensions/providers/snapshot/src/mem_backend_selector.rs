//! U14 (#881): a **pure** memory-backend selector — the placement-contract dry-run.
//!
//! Given host capability + capsule/cache facts, decide which `mem_backend` a
//! Ready-State restore *would* use, and why. This is a **pure function** with no
//! side effects and no behavior change: U14 only records the decision in the
//! diagnostics receipt (U10). Actual selection is opt-in and gated behind an
//! explicit preview flag in U15.
//!
//! Discipline encoded here (do not relax without Phase 8 / a preview flag):
//! - **No-binding capsules only.** A binding-required capsule is never UFFD until
//!   Phase 8 `BindingLease` — it selects File (or fails closed in validation mode).
//! - **Host must truthfully support UFFD** (`BackendCapabilities.supports_uffd_mem_backend`).
//! - **Remote read-through is off unless explicitly enabled** — never auto-selected.
//! - **Local CAS must actually hold the memory** for a UFFD choice.

use serde::{Deserialize, Serialize};

/// Which `mem_backend` a restore would use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemBackendChoice {
    /// Default eager File rehydrate (also the safe fallback).
    File,
    /// UFFD demand paging from local CAS.
    UffdLocal,
    /// UFFD demand paging from local CAS with hotset prefetch.
    UffdHotset,
    /// UFFD demand paging reading through a remote CAS (opt-in only).
    UffdRemote,
    /// UFFD is not usable on this host (falls back to File where allowed).
    Unsupported,
}

/// Inputs the selector decides over (the caller — U10/U15 — fills these from
/// `BackendCapabilities`, the manifest, and cache facts).
#[derive(Debug, Clone)]
pub struct MemBackendInputs {
    /// `BackendCapabilities.supports_uffd_mem_backend` (U0).
    pub host_supports_uffd: bool,
    /// The runner class is compatible with a UFFD restore.
    pub runner_class_compatible: bool,
    /// The capsule declares NO secrets / bindings / external capabilities.
    pub capsule_no_bindings: bool,
    /// The local CAS holds the memory image chunks.
    pub local_cas_has_memory: bool,
    /// A hotset profile valid for THIS capsule/runner/memory-image is available (U12).
    pub hotset_profile_available: bool,
    /// Remote read-through was **explicitly** enabled (never inferred).
    pub remote_preview_enabled: bool,
    /// Remote CAS is reachable/configured.
    pub remote_available: bool,
    /// This is a validation run (a sealed artifact is required; must not silently
    /// cold-path).
    pub validation_mode: bool,
    /// Cold-path (File) fallback is permitted for this run.
    pub fallback_allowed: bool,
}

/// The decision + human-readable reasons (recorded in the diagnostics receipt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemBackendDecision {
    pub choice: MemBackendChoice,
    pub reasons: Vec<String>,
}

/// Pure selection. **No side effects, no behavior change** — the caller decides
/// whether to act on it (U15, opt-in). Precedence: binding-required → File/closed;
/// unsupported host → Unsupported/File; memory not local → File; else UFFD, with
/// remote only when explicitly enabled and reachable, hotset when a valid profile
/// exists, otherwise local.
pub fn decide_mem_backend(inputs: &MemBackendInputs) -> MemBackendDecision {
    let mut reasons = Vec::new();

    // Binding-required capsules are never UFFD until Phase 8 BindingLease.
    if !inputs.capsule_no_bindings {
        reasons.push(
            "capsule requires bindings → File (UFFD is no-binding-only until Phase 8)".into(),
        );
        return MemBackendDecision {
            choice: MemBackendChoice::File,
            reasons,
        };
    }
    reasons.push("capsule has no bindings".into());

    if !inputs.host_supports_uffd {
        reasons.push("host does not support UFFD mem_backend".into());
        // The choice is Unsupported; the CALLER maps it to File where
        // `fallback_allowed`, or fails closed in `validation_mode` (never silently
        // cold-paths when a sealed artifact is required).
        return MemBackendDecision {
            choice: MemBackendChoice::Unsupported,
            reasons,
        };
    }
    reasons.push("host supports UFFD".into());

    if !inputs.runner_class_compatible {
        reasons.push("runner class not UFFD-compatible → File".into());
        return MemBackendDecision {
            choice: MemBackendChoice::File,
            reasons,
        };
    }

    if !inputs.local_cas_has_memory {
        reasons.push("memory image not in local CAS → File".into());
        return MemBackendDecision {
            choice: MemBackendChoice::File,
            reasons,
        };
    }
    reasons.push("memory image available in local CAS".into());

    // UFFD-eligible. Remote only when explicitly enabled AND reachable.
    if inputs.remote_preview_enabled && inputs.remote_available {
        reasons.push("remote read-through explicitly enabled + reachable".into());
        return MemBackendDecision {
            choice: MemBackendChoice::UffdRemote,
            reasons,
        };
    }
    if inputs.remote_preview_enabled && !inputs.remote_available {
        reasons.push("remote requested but not reachable → local CAS".into());
    }

    if inputs.hotset_profile_available {
        reasons.push("valid hotset profile available → prefetch".into());
        MemBackendDecision {
            choice: MemBackendChoice::UffdHotset,
            reasons,
        }
    } else {
        reasons.push("no hotset profile → demand-only".into());
        MemBackendDecision {
            choice: MemBackendChoice::UffdLocal,
            reasons,
        }
    }
}

impl MemBackendInputs {
    /// A conservative baseline: File, everything off. Callers flip the facts they
    /// know.
    pub fn baseline() -> Self {
        MemBackendInputs {
            host_supports_uffd: false,
            runner_class_compatible: false,
            capsule_no_bindings: false,
            local_cas_has_memory: false,
            hotset_profile_available: false,
            remote_preview_enabled: false,
            remote_available: false,
            validation_mode: false,
            fallback_allowed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uffd_ready() -> MemBackendInputs {
        MemBackendInputs {
            host_supports_uffd: true,
            runner_class_compatible: true,
            capsule_no_bindings: true,
            local_cas_has_memory: true,
            hotset_profile_available: false,
            remote_preview_enabled: false,
            remote_available: false,
            validation_mode: false,
            fallback_allowed: true,
        }
    }

    #[test]
    fn binding_required_never_uffd() {
        let mut i = uffd_ready();
        i.capsule_no_bindings = false;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::File);
    }

    #[test]
    fn unsupported_host_is_unsupported() {
        let mut i = uffd_ready();
        i.host_supports_uffd = false;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::Unsupported);
    }

    #[test]
    fn memory_not_local_is_file() {
        let mut i = uffd_ready();
        i.local_cas_has_memory = false;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::File);
    }

    #[test]
    fn eligible_demand_only_is_uffd_local() {
        assert_eq!(
            decide_mem_backend(&uffd_ready()).choice,
            MemBackendChoice::UffdLocal
        );
    }

    #[test]
    fn eligible_with_hotset_is_uffd_hotset() {
        let mut i = uffd_ready();
        i.hotset_profile_available = true;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::UffdHotset);
    }

    #[test]
    fn remote_only_when_explicitly_enabled_and_reachable() {
        let mut i = uffd_ready();
        // reachable but NOT explicitly enabled → never auto-remote.
        i.remote_available = true;
        assert_eq!(
            decide_mem_backend(&i).choice,
            MemBackendChoice::UffdLocal,
            "remote must be opt-in"
        );
        // enabled + reachable → remote.
        i.remote_preview_enabled = true;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::UffdRemote);
        // enabled but unreachable → local.
        i.remote_available = false;
        assert_eq!(decide_mem_backend(&i).choice, MemBackendChoice::UffdLocal);
    }

    #[test]
    fn baseline_selects_file_family() {
        // baseline (binding unknown = false) → File.
        assert_eq!(
            decide_mem_backend(&MemBackendInputs::baseline()).choice,
            MemBackendChoice::File
        );
    }
}
