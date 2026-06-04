//! Focused unit tests for the cross-device placement index (#509).
//!
//! These pin the core idea — capsule requirements + redacted snapshots =>
//! candidate providers — plus the non-negotiable guarantees: redaction by
//! construction, TTL/staleness handling, typed rejection reasons, deterministic
//! ordering, and the explicit "final local admission still required" marker.

use super::*;

const GB: u64 = 1024 * 1024 * 1024;
const NOW: u64 = 1_000_000_000_000;
const TTL: u64 = 60_000; // 60s

/// Substrings that must never appear in any serialized snapshot / result /
/// receipt. Secret values and raw sensitive local paths.
const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "sk-live-secret",
    "OPENAI_API_KEY_VALUE",
    "/Users/",
    "C:\\Users\\",
    "/home/",
    ".aws/credentials",
    ".env",
    "id_rsa",
];

fn assert_no_forbidden(json: &str, what: &str) {
    for needle in FORBIDDEN_SUBSTRINGS {
        assert!(
            !json.contains(needle),
            "forbidden substring {needle:?} leaked into {what}: {json}"
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario fixtures
//
// Capsule requires: NVIDIA GPU, CUDA >= 12, VRAM >= 12GB, OPENAI_API_KEY
// projection, egress to api.openai.com and huggingface.co, disk 20GB.
// ---------------------------------------------------------------------------

fn scenario_request() -> PlacementRequest {
    PlacementRequest {
        requested_capsule: "publisher/llm-trainer".to_string(),
        required_storage_bytes: Some(20 * GB),
        required_runtimes: vec![RuntimeRequirement::new("python")],
        required_gpu: Some(GpuRequirement {
            require_nvidia: true,
            min_vram_bytes: 12 * GB,
            min_cuda_version: Some("12".to_string()),
        }),
        required_network: vec![
            NetworkRequirement::new("api.openai.com"),
            NetworkRequirement::new("huggingface.co"),
        ],
        required_secret_refs: vec![RedactedSecretRef::new("OPENAI_API_KEY")],
        required_provider_capabilities: Vec::new(),
        required_materialized_objects: Vec::new(),
        preferred_materialized_objects: Vec::new(),
    }
}

fn openai_secret_projection() -> SecretProjectionSummary {
    SecretProjectionSummary {
        secret_ref: RedactedSecretRef::new("OPENAI_API_KEY"),
        scope: "project".to_string(),
        can_project: true,
    }
}

/// macbook: online, darwin arm64, no NVIDIA GPU (Apple integrated), 8GB disk.
fn macbook_snapshot() -> ProviderCapabilitySnapshot {
    ProviderCapabilitySnapshot {
        device_id: DeviceId::new("macbook"),
        provider_id: ProviderId::new("macbook"),
        provider_kind: ProviderKind::DesktopWorkstation,
        role: DeviceRole::Realizer,
        online_status: OnlineStatus::Online,
        last_seen_unix_ms: NOW,
        platform: PlatformSummary {
            os: "darwin".to_string(),
            arch: "arm64".to_string(),
        },
        resources: ResourceSummary {
            available_storage_bytes: 8 * GB,
            total_memory_bytes: 16 * GB,
            gpu: Some(GpuSummary {
                vendor: GpuVendor::Apple,
                vram_bytes: 8 * GB,
                cuda_version: None,
            }),
        },
        runtimes: RuntimeSummary {
            families: vec!["python".to_string(), "node".to_string()],
        },
        network: NetworkCapabilitySummary {
            egress_allowed: Vec::new(),
            egress_unrestricted: true,
        },
        capabilities: Vec::new(),
        materialized_objects: MaterializedObjectSummary::default(),
        secret_refs: vec![openai_secret_projection()],
        placement_hints: PlacementHints::default(),
    }
}

/// ato-cloud-gpu: online, linux x86_64, NVIDIA, CUDA 12.4, VRAM 24GB, 200GB
/// disk, unrestricted egress, can project OPENAI_API_KEY.
fn cloud_gpu_snapshot() -> ProviderCapabilitySnapshot {
    ProviderCapabilitySnapshot {
        device_id: DeviceId::new("ato-cloud-gpu"),
        provider_id: ProviderId::new("ato-cloud-gpu"),
        provider_kind: ProviderKind::CloudGpu,
        role: DeviceRole::Realizer,
        online_status: OnlineStatus::Online,
        last_seen_unix_ms: NOW,
        platform: PlatformSummary {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
        resources: ResourceSummary {
            available_storage_bytes: 200 * GB,
            total_memory_bytes: 128 * GB,
            gpu: Some(GpuSummary {
                vendor: GpuVendor::Nvidia,
                vram_bytes: 24 * GB,
                cuda_version: Some("12.4".to_string()),
            }),
        },
        runtimes: RuntimeSummary {
            families: vec!["python".to_string(), "oci".to_string()],
        },
        network: NetworkCapabilitySummary {
            egress_allowed: vec!["api.openai.com".to_string(), "huggingface.co".to_string()],
            egress_unrestricted: false,
        },
        capabilities: vec![ProviderCapabilityId::new("realize.oci")],
        materialized_objects: MaterializedObjectSummary::default(),
        secret_refs: vec![openai_secret_projection()],
        placement_hints: PlacementHints {
            estimated_latency_ms: Some(40),
            estimated_cost_milli_units: Some(120),
        },
    }
}

/// home-desktop: NVIDIA, CUDA 12.2, VRAM 16GB — but offline AND stale.
fn home_desktop_snapshot() -> ProviderCapabilitySnapshot {
    ProviderCapabilitySnapshot {
        device_id: DeviceId::new("home-desktop"),
        provider_id: ProviderId::new("home-desktop"),
        provider_kind: ProviderKind::HomeDesktop,
        role: DeviceRole::Realizer,
        online_status: OnlineStatus::Offline,
        last_seen_unix_ms: NOW - TTL - 1, // stale
        platform: PlatformSummary {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
        resources: ResourceSummary {
            available_storage_bytes: 500 * GB,
            total_memory_bytes: 64 * GB,
            gpu: Some(GpuSummary {
                vendor: GpuVendor::Nvidia,
                vram_bytes: 16 * GB,
                cuda_version: Some("12.2".to_string()),
            }),
        },
        runtimes: RuntimeSummary {
            families: vec!["python".to_string()],
        },
        network: NetworkCapabilitySummary {
            egress_allowed: Vec::new(),
            egress_unrestricted: true,
        },
        capabilities: Vec::new(),
        materialized_objects: MaterializedObjectSummary::default(),
        secret_refs: vec![openai_secret_projection()],
        placement_hints: PlacementHints::default(),
    }
}

/// iphone: online, but control-surface-only — never a realization target.
fn iphone_snapshot() -> ProviderCapabilitySnapshot {
    ProviderCapabilitySnapshot {
        device_id: DeviceId::new("iphone"),
        provider_id: ProviderId::new("iphone"),
        provider_kind: ProviderKind::Mobile,
        role: DeviceRole::ControlSurfaceOnly,
        online_status: OnlineStatus::Online,
        last_seen_unix_ms: NOW,
        platform: PlatformSummary {
            os: "ios".to_string(),
            arch: "arm64".to_string(),
        },
        resources: ResourceSummary {
            available_storage_bytes: 64 * GB,
            total_memory_bytes: 8 * GB,
            gpu: None,
        },
        runtimes: RuntimeSummary::default(),
        network: NetworkCapabilitySummary {
            egress_allowed: Vec::new(),
            egress_unrestricted: true,
        },
        capabilities: Vec::new(),
        materialized_objects: MaterializedObjectSummary::default(),
        secret_refs: Vec::new(),
        placement_hints: PlacementHints::default(),
    }
}

fn full_scenario_index() -> PlacementIndex {
    let mut index = PlacementIndex::new();
    index.upsert_snapshot(macbook_snapshot());
    index.upsert_snapshot(cloud_gpu_snapshot());
    index.upsert_snapshot(home_desktop_snapshot());
    index.upsert_snapshot(iphone_snapshot());
    index
}

fn reasons_for<'a>(
    result: &'a PlacementQueryResult,
    provider: &str,
) -> &'a [PlacementRejectionReason] {
    result
        .rejected
        .iter()
        .find(|r| r.provider_id.as_str() == provider)
        .map(|r| r.reasons.as_slice())
        .unwrap_or_else(|| panic!("provider {provider} not in rejected list"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_scenario_selects_cloud_gpu_and_rejects_the_rest() {
    let index = full_scenario_index();
    let request = scenario_request();
    let receipt = index.decide(&request, NOW, TTL);

    assert_eq!(
        receipt.selected_provider,
        Some(ProviderId::new("ato-cloud-gpu")),
        "the cloud GPU provider is the only one satisfying every requirement"
    );
    assert!(
        receipt.requires_final_local_admission,
        "a selection must still require provider-local admission (#508)"
    );

    // OPENAI_API_KEY appears as a redacted required projection, never a value.
    assert!(
        receipt
            .required_projections
            .contains(&RequiredProjection::Secret(RedactedSecretRef::new(
                "OPENAI_API_KEY"
            ))),
        "required projections must record the redacted secret ref"
    );

    // The three non-candidates are all rejected with typed reasons.
    let rejected: Vec<&str> = receipt
        .rejected_candidates
        .iter()
        .map(|r| r.provider_id.as_str())
        .collect();
    assert!(rejected.contains(&"macbook"));
    assert!(rejected.contains(&"home-desktop"));
    assert!(rejected.contains(&"iphone"));
}

#[test]
fn placement_index_filters_gpu_disk_runtime_secret_and_network_requirements() {
    let index = full_scenario_index();
    let result = index.query(&scenario_request(), NOW, TTL);

    // macbook: no NVIDIA GPU AND insufficient disk — both reasons reported.
    let macbook = reasons_for(&result, "macbook");
    assert!(
        macbook.contains(&PlacementRejectionReason::MissingGpu),
        "macbook must be rejected for lacking an NVIDIA GPU; got {macbook:?}"
    );
    assert!(
        macbook.contains(&PlacementRejectionReason::InsufficientStorage {
            required_bytes: 20 * GB,
            available_bytes: 8 * GB,
        }),
        "macbook must be rejected for insufficient disk; got {macbook:?}"
    );

    // Exactly one eligible provider: the cloud GPU.
    assert_eq!(result.eligible.len(), 1);
    assert_eq!(
        result.eligible[0].provider_id,
        ProviderId::new("ato-cloud-gpu")
    );
}

#[test]
fn missing_runtime_secret_and_network_produce_typed_reasons() {
    // A provider that is online + realizer + has the GPU/disk, but is missing
    // the runtime family, the secret projection, and a network egress.
    let mut snap = cloud_gpu_snapshot();
    snap.provider_id = ProviderId::new("narrow-cloud");
    snap.device_id = DeviceId::new("narrow-cloud");
    snap.runtimes = RuntimeSummary {
        families: vec!["node".to_string()], // no python
    };
    snap.secret_refs = Vec::new(); // cannot project OPENAI_API_KEY
    snap.network = NetworkCapabilitySummary {
        egress_allowed: vec!["api.openai.com".to_string()], // missing huggingface.co
        egress_unrestricted: false,
    };

    let mut index = PlacementIndex::new();
    index.upsert_snapshot(snap);
    let result = index.query(&scenario_request(), NOW, TTL);

    let reasons = reasons_for(&result, "narrow-cloud");
    assert!(reasons.contains(&PlacementRejectionReason::MissingRuntime {
        runtime: "python".to_string()
    }));
    assert!(
        reasons.contains(&PlacementRejectionReason::MissingSecretProjection {
            secret_ref: RedactedSecretRef::new("OPENAI_API_KEY"),
        })
    );
    assert!(
        reasons.contains(&PlacementRejectionReason::MissingNetworkCapability {
            requirement: "huggingface.co".to_string()
        })
    );
}

#[test]
fn snapshot_serialization_does_not_include_secret_values_or_raw_paths() {
    // A fully-populated snapshot — secret refs, materialized objects, the lot.
    let mut snap = cloud_gpu_snapshot();
    snap.materialized_objects = MaterializedObjectSummary {
        object_hashes: vec!["sha256:abc123".to_string(), "sha256:def456".to_string()],
        object_count: 2,
        total_bytes: 4 * GB,
    };

    let snap_json = serde_json::to_string_pretty(&snap).expect("serialize snapshot");
    assert_no_forbidden(&snap_json, "snapshot");

    // The whole query result and decision receipt must be clean too.
    let index = full_scenario_index();
    let result = index.query(&scenario_request(), NOW, TTL);
    let result_json = serde_json::to_string_pretty(&result).expect("serialize result");
    assert_no_forbidden(&result_json, "query result");

    let receipt = index.decide(&scenario_request(), NOW, TTL);
    let receipt_json = serde_json::to_string_pretty(&receipt).expect("serialize receipt");
    assert_no_forbidden(&receipt_json, "decision receipt");

    // Sanity: the redacted ref name itself IS present (it is not a secret
    // value), proving we asserted absence of values, not of all references.
    assert!(receipt_json.contains("OPENAI_API_KEY"));
}

#[test]
fn stale_snapshot_is_rejected_with_typed_reason() {
    // Same provider, but last seen just past the TTL while still "Online".
    let mut snap = cloud_gpu_snapshot();
    snap.online_status = OnlineStatus::Online;
    snap.last_seen_unix_ms = NOW - TTL - 1;

    let mut index = PlacementIndex::new();
    index.upsert_snapshot(snap);
    let result = index.query(&scenario_request(), NOW, TTL);

    assert!(result.eligible.is_empty());
    let reasons = reasons_for(&result, "ato-cloud-gpu");
    assert_eq!(reasons, &[PlacementRejectionReason::StaleSnapshot]);

    // And exactly at the TTL boundary it is NOT stale (`last_seen + ttl == now`).
    let mut fresh = cloud_gpu_snapshot();
    fresh.last_seen_unix_ms = NOW - TTL;
    let mut index2 = PlacementIndex::new();
    index2.upsert_snapshot(fresh);
    let result2 = index2.query(&scenario_request(), NOW, TTL);
    assert_eq!(
        result2.eligible.len(),
        1,
        "boundary last_seen+ttl==now is fresh"
    );
}

#[test]
fn offline_provider_is_rejected_with_typed_reason() {
    let mut snap = cloud_gpu_snapshot();
    snap.online_status = OnlineStatus::Offline;
    snap.last_seen_unix_ms = NOW; // fresh, so the reason is Offline not Stale

    let mut index = PlacementIndex::new();
    index.upsert_snapshot(snap);
    let result = index.query(&scenario_request(), NOW, TTL);

    let reasons = reasons_for(&result, "ato-cloud-gpu");
    assert_eq!(reasons, &[PlacementRejectionReason::Offline]);
}

#[test]
fn control_surface_only_device_is_not_realization_candidate() {
    // The iphone is online and fresh, yet must never be a realization target.
    let mut index = PlacementIndex::new();
    index.upsert_snapshot(iphone_snapshot());
    let result = index.query(&scenario_request(), NOW, TTL);

    assert!(result.eligible.is_empty());
    let reasons = reasons_for(&result, "iphone");
    assert_eq!(reasons, &[PlacementRejectionReason::ControlSurfaceOnly]);
}

#[test]
fn gpu_vram_and_cuda_shortfalls_have_distinct_typed_reasons() {
    // VRAM too low.
    let mut low_vram = cloud_gpu_snapshot();
    low_vram.provider_id = ProviderId::new("low-vram");
    low_vram.device_id = DeviceId::new("low-vram");
    low_vram.resources.gpu = Some(GpuSummary {
        vendor: GpuVendor::Nvidia,
        vram_bytes: 8 * GB,
        cuda_version: Some("12.4".to_string()),
    });

    // CUDA too low.
    let mut low_cuda = cloud_gpu_snapshot();
    low_cuda.provider_id = ProviderId::new("low-cuda");
    low_cuda.device_id = DeviceId::new("low-cuda");
    low_cuda.resources.gpu = Some(GpuSummary {
        vendor: GpuVendor::Nvidia,
        vram_bytes: 24 * GB,
        cuda_version: Some("11.8".to_string()),
    });

    let mut index = PlacementIndex::new();
    index.upsert_snapshot(low_vram);
    index.upsert_snapshot(low_cuda);
    let result = index.query(&scenario_request(), NOW, TTL);

    assert!(reasons_for(&result, "low-vram").contains(
        &PlacementRejectionReason::InsufficientGpuVram {
            required_bytes: 12 * GB,
            available_bytes: 8 * GB,
        }
    ));
    assert!(reasons_for(&result, "low-cuda").contains(
        &PlacementRejectionReason::CudaVersionTooLow {
            required: "12".to_string(),
            available: Some("11.8".to_string()),
        }
    ));
}

#[test]
fn query_order_is_deterministic_independent_of_snapshot_insert_order() {
    // Three eligible cloud providers distinguished only by latency hint.
    let mut a = cloud_gpu_snapshot();
    a.provider_id = ProviderId::new("cloud-a");
    a.device_id = DeviceId::new("cloud-a");
    a.placement_hints.estimated_latency_ms = Some(80);

    let mut b = cloud_gpu_snapshot();
    b.provider_id = ProviderId::new("cloud-b");
    b.device_id = DeviceId::new("cloud-b");
    b.placement_hints.estimated_latency_ms = Some(20);

    let mut c = cloud_gpu_snapshot();
    c.provider_id = ProviderId::new("cloud-c");
    c.device_id = DeviceId::new("cloud-c");
    c.placement_hints.estimated_latency_ms = Some(50);

    let request = scenario_request();

    // Build the index in several different insertion orders.
    let orders: Vec<Vec<ProviderCapabilitySnapshot>> = vec![
        vec![a.clone(), b.clone(), c.clone()],
        vec![c.clone(), b.clone(), a.clone()],
        vec![b.clone(), c.clone(), a.clone()],
    ];

    let mut seen_order: Option<Vec<String>> = None;
    for snaps in orders {
        let mut index = PlacementIndex::new();
        for s in snaps {
            index.upsert_snapshot(s);
        }
        let result = index.query(&request, NOW, TTL);
        let ids: Vec<String> = result
            .eligible
            .iter()
            .map(|c| c.provider_id.0.clone())
            .collect();
        // Ordered by ascending latency: b(20) < c(50) < a(80).
        assert_eq!(ids, vec!["cloud-b", "cloud-c", "cloud-a"]);
        match &seen_order {
            None => seen_order = Some(ids),
            Some(prev) => assert_eq!(prev, &ids, "ordering must not depend on insertion order"),
        }
    }
}

#[test]
fn decision_receipt_records_rejected_candidates_and_selected_reason() {
    let index = full_scenario_index();
    let receipt = index.decide(&scenario_request(), NOW, TTL);

    assert_eq!(
        receipt.selected_provider,
        Some(ProviderId::new("ato-cloud-gpu"))
    );
    assert!(
        !receipt.selected_reason.is_empty(),
        "a selection must carry an explanatory reason"
    );
    assert_eq!(
        receipt.rejected_candidates.len(),
        3,
        "macbook, home-desktop, iphone must all be recorded as rejected"
    );
    for rejected in &receipt.rejected_candidates {
        assert!(
            !rejected.reasons.is_empty(),
            "every rejected candidate must carry at least one typed reason"
        );
    }
}

#[test]
fn selected_candidate_requires_final_local_admission() {
    let index = full_scenario_index();
    let request = scenario_request();

    // Both the per-candidate marker and the receipt-level flag say so.
    let result = index.query(&request, NOW, TTL);
    assert_eq!(result.eligible.len(), 1);
    assert!(
        result.eligible[0].requires_final_local_admission,
        "an eligible candidate is a narrowing result, not an admission"
    );

    let receipt = index.decide(&request, NOW, TTL);
    assert!(receipt.requires_final_local_admission);
}

#[test]
fn no_eligible_candidate_means_no_selection_and_no_admission_marker() {
    // Only the iphone (control-surface-only) is present.
    let mut index = PlacementIndex::new();
    index.upsert_snapshot(iphone_snapshot());
    let receipt = index.decide(&scenario_request(), NOW, TTL);

    assert_eq!(receipt.selected_provider, None);
    assert!(
        !receipt.requires_final_local_admission,
        "no selection => nothing to admit downstream"
    );
    assert_eq!(receipt.rejected_candidates.len(), 1);
}

#[test]
fn materialized_object_hash_can_make_provider_preferred_without_exposing_path() {
    // Two otherwise-identical eligible cloud providers; one already holds the
    // preferred object (by content hash). Data locality must prefer it — and
    // the preference must be expressed by HASH, never a local path.
    let object_hash = "sha256:deadbeefcafe".to_string();

    let mut has_object = cloud_gpu_snapshot();
    has_object.provider_id = ProviderId::new("cloud-warm");
    has_object.device_id = DeviceId::new("cloud-warm");
    has_object.materialized_objects = MaterializedObjectSummary {
        object_hashes: vec![object_hash.clone()],
        object_count: 1,
        total_bytes: 2 * GB,
    };

    let mut no_object = cloud_gpu_snapshot();
    no_object.provider_id = ProviderId::new("cloud-cold");
    no_object.device_id = DeviceId::new("cloud-cold");
    // Same latency/cost so the ONLY differentiator is data locality.
    no_object.placement_hints = has_object.placement_hints.clone();

    let mut request = scenario_request();
    request.preferred_materialized_objects = vec![object_hash.clone()];

    // Insert cold first so a wrong impl that keeps insertion order would fail.
    let mut index = PlacementIndex::new();
    index.upsert_snapshot(no_object);
    index.upsert_snapshot(has_object);

    let receipt = index.decide(&request, NOW, TTL);
    assert_eq!(
        receipt.selected_provider,
        Some(ProviderId::new("cloud-warm")),
        "the provider already holding the object should be preferred"
    );

    // The preference is explained, and nothing exposes a path.
    let receipt_json = serde_json::to_string_pretty(&receipt).expect("serialize receipt");
    assert_no_forbidden(&receipt_json, "preferred-object receipt");
    assert!(
        receipt_json.contains(&object_hash),
        "locality is expressed by content hash"
    );
}
