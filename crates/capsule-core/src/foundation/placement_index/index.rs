//! In-memory cross-device placement index and its deterministic query.
//!
//! The index is a **fast, non-authoritative map** from capsule requirements +
//! redacted provider snapshots to candidate providers. It narrows; it never
//! admits. Selecting a candidate does not reserve anything — the chosen
//! provider's local installed-state DB performs final admission later (#508).

use std::cmp::Reverse;
use std::collections::BTreeMap;

use super::model::{
    DeviceRole, GpuRequirement, GpuSummary, GpuVendor, OnlineStatus, PlacementCandidate,
    PlacementDecisionReceipt, PlacementQueryResult, PlacementRejectionReason, PlacementRequest,
    ProviderCapabilitySnapshot, ProviderId, RejectedPlacementCandidate, RequiredProjection,
};

/// In-memory placement index keyed by provider id.
///
/// Insertion order is irrelevant to query output (a `BTreeMap` keeps a stable
/// key order, and the query re-sorts candidates by an explicit comparator).
#[derive(Debug, Clone, Default)]
pub struct PlacementIndex {
    snapshots: BTreeMap<ProviderId, ProviderCapabilitySnapshot>,
}

impl PlacementIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the snapshot for a provider (keyed by `provider_id`).
    pub fn upsert_snapshot(&mut self, snapshot: ProviderCapabilitySnapshot) {
        self.snapshots
            .insert(snapshot.provider_id.clone(), snapshot);
    }

    /// Number of snapshots currently held.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Query the index for candidates satisfying `request`.
    ///
    /// `now_unix_ms` / `ttl_ms` drive staleness: a snapshot is stale when
    /// `last_seen + ttl < now`. The result's `eligible` list is ordered
    /// deterministically (best first), independent of insertion order.
    pub fn query(
        &self,
        request: &PlacementRequest,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> PlacementQueryResult {
        let mut scored: Vec<Scored> = Vec::new();
        let mut rejected: Vec<RejectedPlacementCandidate> = Vec::new();

        for snapshot in self.snapshots.values() {
            let reasons = evaluate_rejections(snapshot, request, now_unix_ms, ttl_ms);
            if reasons.is_empty() {
                scored.push(score_candidate(snapshot, request));
            } else {
                rejected.push(RejectedPlacementCandidate {
                    provider_id: snapshot.provider_id.clone(),
                    device_id: snapshot.device_id.clone(),
                    reasons,
                });
            }
        }

        // Deterministic ordering, independent of insertion order:
        //   1. more preferred-object (data-locality) hits first
        //   2. lower estimated latency (absent => worst)
        //   3. lower estimated cost (absent => worst)
        //   4. provider_id ascending (final tie-breaker)
        scored.sort_by(|a, b| a.key.cmp(&b.key));
        let eligible = scored.into_iter().map(|s| s.candidate).collect();

        // Rejected ordering is also stable: online-ish before offline/stale,
        // then provider_id. (Eligible always sorts ahead of rejected because
        // they live in separate lists.)
        rejected.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));

        PlacementQueryResult { eligible, rejected }
    }

    /// Run a query and fold it into a [`PlacementDecisionReceipt`]: the first
    /// (best) eligible candidate is selected. The receipt is a decision
    /// artifact only — it always flags that final local admission is still
    /// required.
    pub fn decide(
        &self,
        request: &PlacementRequest,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> PlacementDecisionReceipt {
        let result = self.query(request, now_unix_ms, ttl_ms);
        build_decision(result, request)
    }
}

/// Fold a query result + request into a decision receipt. Selects the first
/// eligible candidate (the query already ordered them best-first).
pub fn build_decision(
    result: PlacementQueryResult,
    request: &PlacementRequest,
) -> PlacementDecisionReceipt {
    let selected = result.eligible.into_iter().next();
    match selected {
        Some(candidate) => PlacementDecisionReceipt {
            requested_capsule: request.requested_capsule.clone(),
            selected_provider: Some(candidate.provider_id),
            rejected_candidates: result.rejected,
            selected_reason: candidate.selected_reason,
            required_projections: candidate.required_projections,
            // The whole point of the index: a selection still needs
            // provider-local admission (#508). Never authoritative.
            requires_final_local_admission: true,
        },
        None => PlacementDecisionReceipt {
            requested_capsule: request.requested_capsule.clone(),
            selected_provider: None,
            rejected_candidates: result.rejected,
            selected_reason: Vec::new(),
            required_projections: required_projections_for(request),
            // No candidate selected => nothing to admit downstream.
            requires_final_local_admission: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Internal scoring + filtering
// ---------------------------------------------------------------------------

/// Sort key for an eligible candidate. Derives `Ord` so the field order *is*
/// the precedence order. `Reverse(locality_hits)` puts more data-local
/// providers first; latency/cost use `u64::MAX` as the "absent => worst"
/// sentinel so providers with a known-low latency sort ahead of unknowns.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateSortKey {
    locality_hits: Reverse<u64>,
    latency_ms: u64,
    cost_milli_units: u64,
    provider_id: String,
}

struct Scored {
    key: CandidateSortKey,
    candidate: PlacementCandidate,
}

fn score_candidate(snapshot: &ProviderCapabilitySnapshot, request: &PlacementRequest) -> Scored {
    let local_hits: Vec<&String> = request
        .preferred_materialized_objects
        .iter()
        .filter(|hash| snapshot.materialized_objects.object_hashes.contains(hash))
        .collect();
    let locality_hits = local_hits.len() as u64;

    let mut selected_reason = vec![format!(
        "satisfies all requirements on provider {}",
        snapshot.provider_id
    )];
    if !local_hits.is_empty() {
        // Explain the locality preference by content HASH — never a path.
        let hashes = local_hits
            .iter()
            .map(|h| h.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        selected_reason.push(format!(
            "data locality: already holds {locality_hits} preferred object(s): {hashes}"
        ));
    }

    let candidate = PlacementCandidate {
        provider_id: snapshot.provider_id.clone(),
        device_id: snapshot.device_id.clone(),
        selected_reason,
        required_projections: required_projections_for(request),
        requires_final_local_admission: true,
    };

    Scored {
        key: CandidateSortKey {
            locality_hits: Reverse(locality_hits),
            latency_ms: snapshot
                .placement_hints
                .estimated_latency_ms
                .unwrap_or(u64::MAX),
            cost_milli_units: snapshot
                .placement_hints
                .estimated_cost_milli_units
                .unwrap_or(u64::MAX),
            provider_id: snapshot.provider_id.0.clone(),
        },
        candidate,
    }
}

/// The projections a selected provider would need to perform locally, derived
/// from the request. References/hashes only.
fn required_projections_for(request: &PlacementRequest) -> Vec<RequiredProjection> {
    let mut projections: Vec<RequiredProjection> = Vec::new();
    for secret in &request.required_secret_refs {
        projections.push(RequiredProjection::Secret(secret.clone()));
    }
    for hash in &request.required_materialized_objects {
        projections.push(RequiredProjection::MaterializedObject { hash: hash.clone() });
    }
    projections
}

/// Compute the full set of typed rejection reasons for a snapshot against a
/// request. An empty result means the provider is eligible.
///
/// Device-level gates (stale / offline / control-surface-only) short-circuit:
/// there is no point reporting resource mismatches for a device that cannot be
/// a realization target at all. Capability gates are collected together so a
/// single candidate can report e.g. *both* missing-GPU and insufficient-disk.
fn evaluate_rejections(
    snapshot: &ProviderCapabilitySnapshot,
    request: &PlacementRequest,
    now_unix_ms: u64,
    ttl_ms: u64,
) -> Vec<PlacementRejectionReason> {
    if is_stale(snapshot.last_seen_unix_ms, now_unix_ms, ttl_ms) {
        return vec![PlacementRejectionReason::StaleSnapshot];
    }
    if snapshot.online_status != OnlineStatus::Online {
        return vec![PlacementRejectionReason::Offline];
    }
    if snapshot.role == DeviceRole::ControlSurfaceOnly {
        return vec![PlacementRejectionReason::ControlSurfaceOnly];
    }

    let mut reasons: Vec<PlacementRejectionReason> = Vec::new();

    // Runtimes.
    for runtime in &request.required_runtimes {
        if !snapshot.runtimes.families.contains(&runtime.family) {
            reasons.push(PlacementRejectionReason::MissingRuntime {
                runtime: runtime.family.clone(),
            });
        }
    }

    // Storage.
    if let Some(required) = request.required_storage_bytes {
        let available = snapshot.resources.available_storage_bytes;
        if available < required {
            reasons.push(PlacementRejectionReason::InsufficientStorage {
                required_bytes: required,
                available_bytes: available,
            });
        }
    }

    // GPU / CUDA / VRAM.
    if let Some(gpu_req) = &request.required_gpu {
        evaluate_gpu(gpu_req, snapshot.resources.gpu.as_ref(), &mut reasons);
    }

    // Network egress.
    for net in &request.required_network {
        if !network_satisfies(
            &snapshot.network.egress_allowed,
            snapshot.network.egress_unrestricted,
            &net.host,
        ) {
            reasons.push(PlacementRejectionReason::MissingNetworkCapability {
                requirement: net.host.clone(),
            });
        }
    }

    // Secret projection.
    for secret in &request.required_secret_refs {
        let can = snapshot
            .secret_refs
            .iter()
            .any(|s| &s.secret_ref == secret && s.can_project);
        if !can {
            reasons.push(PlacementRejectionReason::MissingSecretProjection {
                secret_ref: secret.clone(),
            });
        }
    }

    // Provider capabilities.
    for capability in &request.required_provider_capabilities {
        if !snapshot.capabilities.contains(capability) {
            reasons.push(PlacementRejectionReason::MissingProviderCapability {
                capability: capability.clone(),
            });
        }
    }

    // Required materialized objects (hard).
    for hash in &request.required_materialized_objects {
        if !snapshot.materialized_objects.object_hashes.contains(hash) {
            reasons
                .push(PlacementRejectionReason::MissingMaterializedObject { hash: hash.clone() });
        }
    }

    reasons
}

fn evaluate_gpu(
    req: &GpuRequirement,
    gpu: Option<&GpuSummary>,
    reasons: &mut Vec<PlacementRejectionReason>,
) {
    let Some(gpu) = gpu else {
        reasons.push(PlacementRejectionReason::MissingGpu);
        return;
    };

    // An NVIDIA-required workload cannot run on a non-NVIDIA GPU; treat that
    // as "no suitable GPU".
    if req.require_nvidia && gpu.vendor != GpuVendor::Nvidia {
        reasons.push(PlacementRejectionReason::MissingGpu);
        // Vendor mismatch is disqualifying on its own; VRAM/CUDA are moot.
        return;
    }

    if gpu.vram_bytes < req.min_vram_bytes {
        reasons.push(PlacementRejectionReason::InsufficientGpuVram {
            required_bytes: req.min_vram_bytes,
            available_bytes: gpu.vram_bytes,
        });
    }

    if let Some(required_cuda) = &req.min_cuda_version {
        let ok = gpu
            .cuda_version
            .as_deref()
            .map(|available| version_ge(available, required_cuda))
            .unwrap_or(false);
        if !ok {
            reasons.push(PlacementRejectionReason::CudaVersionTooLow {
                required: required_cuda.clone(),
                available: gpu.cuda_version.clone(),
            });
        }
    }
}

/// `last_seen + ttl < now` => stale.
fn is_stale(last_seen_unix_ms: u64, now_unix_ms: u64, ttl_ms: u64) -> bool {
    last_seen_unix_ms.saturating_add(ttl_ms) < now_unix_ms
}

/// Egress is satisfied if the provider is unrestricted or explicitly allows
/// the host.
fn network_satisfies(egress_allowed: &[String], egress_unrestricted: bool, host: &str) -> bool {
    egress_unrestricted || egress_allowed.iter().any(|h| h == host)
}

/// Compare dotted numeric versions component-wise (missing components = 0).
/// Returns true when `available >= required`.
fn version_ge(available: &str, required: &str) -> bool {
    let a = parse_version(available);
    let b = parse_version(required);
    let n = a.len().max(b.len());
    for i in 0..n {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    true
}

fn parse_version(s: &str) -> Vec<u64> {
    s.split('.')
        .map(|part| part.trim().parse::<u64>().unwrap_or(0))
        .collect()
}
