//! U10 (#877): Ready-State `mem_backend` selection **diagnostics** — the P0 step.
//!
//! When `ATO_READY_STATE_UFFD_DIAGNOSTICS=1`, `ato run` computes which `mem_backend`
//! a selector WOULD choose (via the pure U14 [`snapshot::mem_backend_selector`]) and
//! records it, then restores via **File exactly as before**. This is a pure
//! observation: no behavior change, no UFFD engaged. It is how the placement
//! contract (#816) gets connected without touching the restore path.
//!
//! Not-yet-wired inputs (always the safe value until their phase lands): hotset
//! profiles (`false` until U12) and remote read-through (`false` until P4).

use capsulefs::CasStore;
use serde::Serialize;
use snapshot::mem_backend_selector::{MemBackendInputs, decide_mem_backend};
use snapshot::{BackendCapabilities, ReadyStateManifest};
use std::path::Path;

/// The recorded diagnostics (written to `<run>/mem-backend-diagnostics.json` and
/// logged). Names mirror the selector inputs so the decision is auditable.
#[derive(Debug, Serialize)]
pub(crate) struct MemBackendDiagnostics {
    pub schema: &'static str,
    pub capsule_manifest_hash: String,
    pub backend: String,
    pub host_supports_uffd: bool,
    pub uffd_reason: Option<String>,
    pub capsule_no_bindings: bool,
    pub local_cas_has_memory: bool,
    pub memory_bytes_total: u64,
    pub hotset_profile_available: bool,
    pub remote_preview_enabled: bool,
    pub validation_mode: bool,
    /// What a selector WOULD pick (this run still restores via File).
    pub mem_backend_would_select: String,
    pub reasons: Vec<String>,
}

/// Compute + record the would-be `mem_backend` decision. Returns a one-line summary
/// for the reporter. Never fails the run — diagnostics are best-effort.
#[allow(clippy::too_many_arguments)] // each arg is a distinct selector fact
pub(crate) fn record(
    backend_id: &str,
    caps: &BackendCapabilities,
    manifest: &ReadyStateManifest,
    store: &CasStore,
    capsule_no_bindings: bool,
    capsule_manifest_hash: &str,
    validation_mode: bool,
    out_dir: &Path,
) -> String {
    let memory = manifest.layers.memory.as_ref();
    let memory_bytes_total = memory.map(|m| m.total_len).unwrap_or(0);
    // Memory is "in local CAS" only if the artifact has a memory layer AND its first
    // chunk is actually present locally (a cheap, honest liveness check).
    let local_cas_has_memory = memory
        .and_then(|m| m.chunks.first())
        .map(|c| store.has_chunk(&c.hash))
        .unwrap_or(false);

    let inputs = MemBackendInputs {
        host_supports_uffd: caps.supports_uffd_mem_backend,
        // On the restore path the runner class was already validated as eligible.
        runner_class_compatible: true,
        capsule_no_bindings,
        local_cas_has_memory,
        // Not wired yet — see module docs.
        hotset_profile_available: false,
        remote_preview_enabled: false,
        remote_available: false,
        validation_mode,
        fallback_allowed: !validation_mode,
    };
    let decision = decide_mem_backend(&inputs);
    let would = format!("{:?}", decision.choice);

    let diag = MemBackendDiagnostics {
        schema: "ato.ready_state.mem_backend_diagnostics/v1",
        capsule_manifest_hash: capsule_manifest_hash.to_string(),
        backend: backend_id.to_string(),
        host_supports_uffd: inputs.host_supports_uffd,
        uffd_reason: caps.uffd_reason.clone(),
        capsule_no_bindings,
        local_cas_has_memory,
        memory_bytes_total,
        hotset_profile_available: inputs.hotset_profile_available,
        remote_preview_enabled: inputs.remote_preview_enabled,
        validation_mode,
        mem_backend_would_select: would.clone(),
        reasons: decision.reasons.clone(),
    };

    if let Ok(json) = serde_json::to_string_pretty(&diag) {
        let _ = std::fs::create_dir_all(out_dir);
        let _ = std::fs::write(out_dir.join("mem-backend-diagnostics.json"), json);
    }
    tracing::info!(
        target: "ato::ready_state",
        mem_backend_would_select = %would,
        host_supports_uffd = inputs.host_supports_uffd,
        local_cas_has_memory,
        capsule_no_bindings,
        reasons = ?decision.reasons,
        "READY-STATE mem_backend diagnostics (would-select only; restoring via File)"
    );
    format!(
        "Ready-State mem_backend diagnostics: would select {would} ({}) — restoring via File",
        decision.reasons.last().map(String::as_str).unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_serialize_stable_schema() {
        let d = MemBackendDiagnostics {
            schema: "ato.ready_state.mem_backend_diagnostics/v1",
            capsule_manifest_hash: "blake3:x".into(),
            backend: "firecracker".into(),
            host_supports_uffd: true,
            uffd_reason: None,
            capsule_no_bindings: true,
            local_cas_has_memory: true,
            memory_bytes_total: 536870912,
            hotset_profile_available: false,
            remote_preview_enabled: false,
            validation_mode: true,
            mem_backend_would_select: "UffdLocal".into(),
            reasons: vec!["memory image available in local CAS".into()],
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("mem_backend_would_select"));
        assert!(json.contains("v1"));
    }
}
