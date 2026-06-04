//! Provider capability snapshot **publisher** (#509-2).
//!
//! This is the boundary that turns provider-local facts into a redacted
//! [`ProviderCapabilitySnapshot`] suitable for the cross-device placement
//! index. It is deliberately the *only* sanctioned way to mint a snapshot for
//! publication, so the redaction and normalization rules live in one place.
//!
//! ## What it does
//!
//! - **Normalizes** list-shaped facets (runtime families, provider
//!   capabilities, materialized-object hashes, secret projection refs, network
//!   egress hosts) by sorting and de-duplicating them, so two callers that
//!   discovered the same facts in a different order publish byte-identical
//!   snapshots.
//! - **Guards redaction**: the builder is fallible and rejects inputs whose
//!   redacted-by-name fields smell like a secret *value* or a raw sensitive
//!   *path*. The model is already redacted by construction; this is
//!   defense-in-depth so a careless caller cannot smuggle a value/path through
//!   a `String` field.
//!
//! ## What it deliberately does NOT do (out of scope for this slice)
//!
//! - No real host probing (GPU/disk/runtime detection).
//! - No cloud API publish, no mobile/client sync.
//! - No #508 installed-state DB integration — the DB is, later, an *optional*
//!   summary input that fills `materialized_objects` / storage availability /
//!   resource-claim summaries. This builder does not depend on it.
//! - No #501 capability-vocabulary finalization — capability ids stay opaque.
//! - No launch/install wiring.

use super::model::{
    DeviceId, DeviceRole, MaterializedObjectSummary, NetworkCapabilitySummary, OnlineStatus,
    PlacementHints, PlatformSummary, ProviderCapabilityId, ProviderCapabilitySnapshot, ProviderId,
    ProviderKind, ResourceSummary, RuntimeSummary, SecretProjectionSummary,
};

/// Provider-local facts handed to [`build_provider_capability_snapshot`].
///
/// Mirrors [`ProviderCapabilitySnapshot`] field-for-field: the publisher's job
/// is normalization + redaction guarding, not field invention. Producers (a
/// desktop/cloud/mobile companion, or — later — a #508 installed-state summary)
/// populate what they know; unknown list facets are simply empty.
#[derive(Debug, Clone)]
pub struct ProviderSnapshotInput {
    pub device_id: DeviceId,
    pub provider_id: ProviderId,
    pub provider_kind: ProviderKind,
    pub role: DeviceRole,
    pub online_status: OnlineStatus,
    pub last_seen_unix_ms: u64,
    pub platform: PlatformSummary,
    pub resources: ResourceSummary,
    pub runtimes: RuntimeSummary,
    pub network: NetworkCapabilitySummary,
    pub capabilities: Vec<ProviderCapabilityId>,
    pub materialized_objects: MaterializedObjectSummary,
    pub secret_refs: Vec<SecretProjectionSummary>,
    pub placement_hints: PlacementHints,
}

/// Why the publisher refused to build a snapshot.
///
/// The redaction guard is the load-bearing reason this is fallible: the index
/// must never carry a secret value or a raw sensitive path, so the publisher
/// rejects rather than silently sanitizes (silent sanitizing would hide a
/// producer bug).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotBuildError {
    /// A field that should hold only a redacted *reference* looks like it
    /// carries a secret **value**.
    SuspectedSecretValue { field: String, value_hint: String },
    /// A field that should hold only a hash/identifier looks like it carries a
    /// raw sensitive **path**.
    SuspectedRawPath { field: String, value_hint: String },
}

impl std::fmt::Display for SnapshotBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SuspectedSecretValue { field, value_hint } => write!(
                f,
                "refusing to publish: field {field} looks like a secret value ({value_hint})"
            ),
            Self::SuspectedRawPath { field, value_hint } => write!(
                f,
                "refusing to publish: field {field} looks like a raw path ({value_hint})"
            ),
        }
    }
}

impl std::error::Error for SnapshotBuildError {}

/// Build a redacted, normalized [`ProviderCapabilitySnapshot`] from
/// provider-local facts.
///
/// Returns [`SnapshotBuildError`] when a redacted-by-name field smells like a
/// secret value or a raw sensitive path. On success the snapshot's list facets
/// are sorted and de-duplicated, so the output is deterministic regardless of
/// the order facts were discovered in.
///
/// > Note: this returns a `Result` rather than the bare snapshot sketched in
/// > the issue. Making the redaction guard part of the type — "you cannot mint
/// > a snapshot from a value/path" — is the point of the boundary, so the
/// > builder is fallible.
pub fn build_provider_capability_snapshot(
    input: ProviderSnapshotInput,
) -> Result<ProviderCapabilitySnapshot, SnapshotBuildError> {
    let ProviderSnapshotInput {
        device_id,
        provider_id,
        provider_kind,
        role,
        online_status,
        last_seen_unix_ms,
        platform,
        resources,
        runtimes,
        network,
        capabilities,
        materialized_objects,
        secret_refs,
        placement_hints,
    } = input;

    // ---- Redaction guard (defense-in-depth) --------------------------------

    // Materialized objects are identified by content hash only; a path here is
    // a producer bug.
    for hash in &materialized_objects.object_hashes {
        reject_if_pathlike("materialized_objects.object_hashes", hash)?;
    }
    // Secret refs carry a reference *name* and a scope label — never a value,
    // never a path.
    for projection in &secret_refs {
        let name = projection.secret_ref.reference_name();
        reject_if_secret_value("secret_refs.secret_ref", name)?;
        reject_if_pathlike("secret_refs.secret_ref", name)?;
        reject_if_secret_value("secret_refs.scope", &projection.scope)?;
        reject_if_pathlike("secret_refs.scope", &projection.scope)?;
    }
    // Network egress is a host pattern, not a path.
    for host in &network.egress_allowed {
        reject_if_pathlike("network.egress_allowed", host)?;
    }

    // ---- Normalization (sort + dedup => deterministic snapshot) ------------

    let runtimes = RuntimeSummary {
        families: sorted_dedup(runtimes.families),
    };
    let mut capabilities = capabilities;
    capabilities.sort();
    capabilities.dedup();

    let materialized_objects = MaterializedObjectSummary {
        object_hashes: sorted_dedup(materialized_objects.object_hashes),
        // `object_count` / `total_bytes` are provider-reported aggregates over
        // the full materialized set, not necessarily 1:1 with the listed
        // hashes; pass them through untouched.
        object_count: materialized_objects.object_count,
        total_bytes: materialized_objects.total_bytes,
    };

    let network = NetworkCapabilitySummary {
        egress_allowed: sorted_dedup(network.egress_allowed),
        egress_unrestricted: network.egress_unrestricted,
    };

    let secret_refs = sorted_dedup_secret_refs(secret_refs);

    Ok(ProviderCapabilitySnapshot {
        device_id,
        provider_id,
        provider_kind,
        role,
        online_status,
        last_seen_unix_ms,
        platform,
        resources,
        runtimes,
        network,
        capabilities,
        materialized_objects,
        secret_refs,
        placement_hints,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sorted_dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// Sort secret projections by `(reference_name, scope)` then drop exact
/// duplicates. Keeps the publication order independent of discovery order.
fn sorted_dedup_secret_refs(
    mut refs: Vec<SecretProjectionSummary>,
) -> Vec<SecretProjectionSummary> {
    refs.sort_by(|a, b| {
        a.secret_ref
            .reference_name()
            .cmp(b.secret_ref.reference_name())
            .then_with(|| a.scope.cmp(&b.scope))
            .then_with(|| a.can_project.cmp(&b.can_project))
    });
    refs.dedup();
    refs
}

/// Substrings that mark a *secret value* (not a reference name).
const SECRET_VALUE_MARKERS: &[&str] = &[
    "sk-live",
    "sk_live",
    "sk-live-secret",
    "OPENAI_API_KEY_VALUE",
    "AKIA",
    "ghp_",
    "-----BEGIN",
];

/// Substrings / shapes that mark a *raw sensitive path*.
const PATH_MARKERS: &[&str] = &[
    "/Users/",
    "C:\\Users\\",
    "/home/",
    ".aws/credentials",
    ".ssh/",
    "id_rsa",
    ".env",
];

fn looks_like_secret_value(s: &str) -> bool {
    SECRET_VALUE_MARKERS.iter().any(|marker| s.contains(marker))
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.contains('\\') || PATH_MARKERS.iter().any(|marker| s.contains(marker))
}

fn reject_if_secret_value(field: &str, value: &str) -> Result<(), SnapshotBuildError> {
    if looks_like_secret_value(value) {
        return Err(SnapshotBuildError::SuspectedSecretValue {
            field: field.to_string(),
            value_hint: redacted_hint(value),
        });
    }
    Ok(())
}

fn reject_if_pathlike(field: &str, value: &str) -> Result<(), SnapshotBuildError> {
    if looks_like_path(value) {
        return Err(SnapshotBuildError::SuspectedRawPath {
            field: field.to_string(),
            value_hint: redacted_hint(value),
        });
    }
    Ok(())
}

/// A short, non-leaking hint for error messages — never echoes the full
/// suspect value (which might itself be a secret/path) into logs.
fn redacted_hint(value: &str) -> String {
    let prefix: String = value.chars().take(4).collect();
    format!("{prefix}… (len {})", value.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::placement_index::{GpuSummary, GpuVendor, RedactedSecretRef};

    const GB: u64 = 1024 * 1024 * 1024;

    /// Forbidden substrings reused from the #509 redaction contract.
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

    /// A valid cloud-GPU realizer input. List facets are intentionally given
    /// out of order / with duplicates so normalization is exercised.
    fn cloud_gpu_input() -> ProviderSnapshotInput {
        ProviderSnapshotInput {
            device_id: DeviceId::new("ato-cloud-gpu"),
            provider_id: ProviderId::new("ato-cloud-gpu"),
            provider_kind: ProviderKind::CloudGpu,
            role: DeviceRole::Realizer,
            online_status: OnlineStatus::Online,
            last_seen_unix_ms: 1_000_000_000_000,
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
                families: vec![
                    "python".to_string(),
                    "oci".to_string(),
                    "python".to_string(), // dup
                ],
            },
            network: NetworkCapabilitySummary {
                egress_allowed: vec![
                    "huggingface.co".to_string(),
                    "api.openai.com".to_string(),
                    "huggingface.co".to_string(), // dup
                ],
                egress_unrestricted: false,
            },
            capabilities: vec![
                ProviderCapabilityId::new("realize.oci"),
                ProviderCapabilityId::new("realize.native"),
                ProviderCapabilityId::new("realize.oci"), // dup
            ],
            materialized_objects: MaterializedObjectSummary {
                object_hashes: vec![
                    "sha256:bbb".to_string(),
                    "sha256:aaa".to_string(),
                    "sha256:bbb".to_string(), // dup
                ],
                object_count: 2,
                total_bytes: 4 * GB,
            },
            secret_refs: vec![
                SecretProjectionSummary {
                    secret_ref: RedactedSecretRef::new("OPENAI_API_KEY"),
                    scope: "project".to_string(),
                    can_project: true,
                },
                SecretProjectionSummary {
                    secret_ref: RedactedSecretRef::new("HF_TOKEN"),
                    scope: "project".to_string(),
                    can_project: true,
                },
                // Exact duplicate of the first.
                SecretProjectionSummary {
                    secret_ref: RedactedSecretRef::new("OPENAI_API_KEY"),
                    scope: "project".to_string(),
                    can_project: true,
                },
            ],
            placement_hints: PlacementHints {
                estimated_latency_ms: Some(40),
                estimated_cost_milli_units: Some(120),
            },
        }
    }

    fn mobile_input() -> ProviderSnapshotInput {
        ProviderSnapshotInput {
            device_id: DeviceId::new("iphone"),
            provider_id: ProviderId::new("iphone"),
            provider_kind: ProviderKind::Mobile,
            role: DeviceRole::ControlSurfaceOnly,
            online_status: OnlineStatus::Online,
            last_seen_unix_ms: 1_000_000_000_000,
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
            capabilities: vec![ProviderCapabilityId::new("control.surface")],
            materialized_objects: MaterializedObjectSummary::default(),
            secret_refs: Vec::new(),
            placement_hints: PlacementHints::default(),
        }
    }

    fn assert_no_forbidden(json: &str) {
        for needle in FORBIDDEN_SUBSTRINGS {
            assert!(
                !json.contains(needle),
                "forbidden substring {needle:?} leaked into published snapshot: {json}"
            );
        }
    }

    #[test]
    fn publisher_sorts_and_dedups_snapshot_fields() {
        let snap = build_provider_capability_snapshot(cloud_gpu_input()).expect("valid input");

        assert_eq!(snap.runtimes.families, vec!["oci", "python"]);
        assert_eq!(
            snap.capabilities,
            vec![
                ProviderCapabilityId::new("realize.native"),
                ProviderCapabilityId::new("realize.oci"),
            ]
        );
        assert_eq!(
            snap.materialized_objects.object_hashes,
            vec!["sha256:aaa", "sha256:bbb"]
        );
        assert_eq!(
            snap.network.egress_allowed,
            vec!["api.openai.com", "huggingface.co"]
        );
        // Secret refs sorted by name, duplicate OPENAI_API_KEY dropped.
        let names: Vec<&str> = snap
            .secret_refs
            .iter()
            .map(|s| s.secret_ref.reference_name())
            .collect();
        assert_eq!(names, vec!["HF_TOKEN", "OPENAI_API_KEY"]);

        // Provider-reported aggregates pass through untouched.
        assert_eq!(snap.materialized_objects.object_count, 2);
        assert_eq!(snap.materialized_objects.total_bytes, 4 * GB);
    }

    #[test]
    fn publisher_does_not_accept_or_emit_secret_values() {
        // A ref name that smells like an actual secret value is rejected.
        let mut input = cloud_gpu_input();
        input.secret_refs[0].secret_ref = RedactedSecretRef::new("sk-live-secret-abc123");
        let err = build_provider_capability_snapshot(input).unwrap_err();
        assert!(matches!(
            err,
            SnapshotBuildError::SuspectedSecretValue { .. }
        ));

        // A clean build emits no secret value either.
        let snap = build_provider_capability_snapshot(cloud_gpu_input()).expect("valid input");
        let json = serde_json::to_string_pretty(&snap).expect("serialize");
        assert_no_forbidden(&json);
        // The redacted ref *name* is allowed to appear (it is not a value).
        assert!(json.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn publisher_does_not_emit_raw_sensitive_paths() {
        // A materialized "hash" that is actually a path is rejected.
        let mut input = cloud_gpu_input();
        input.materialized_objects.object_hashes =
            vec!["/Users/alice/.cache/ato/objects/abc".to_string()];
        let err = build_provider_capability_snapshot(input).unwrap_err();
        assert!(matches!(err, SnapshotBuildError::SuspectedRawPath { .. }));

        // A path smuggled through network egress is rejected too.
        let mut input2 = cloud_gpu_input();
        input2.network.egress_allowed = vec!["/home/bob/socket".to_string()];
        assert!(matches!(
            build_provider_capability_snapshot(input2).unwrap_err(),
            SnapshotBuildError::SuspectedRawPath { .. }
        ));

        // Clean build emits no path.
        let snap = build_provider_capability_snapshot(cloud_gpu_input()).expect("valid input");
        let json = serde_json::to_string_pretty(&snap).expect("serialize");
        assert_no_forbidden(&json);
    }

    #[test]
    fn publisher_builds_control_surface_snapshot_for_mobile() {
        let snap = build_provider_capability_snapshot(mobile_input()).expect("valid input");
        assert_eq!(snap.role, DeviceRole::ControlSurfaceOnly);
        assert_eq!(snap.provider_kind, ProviderKind::Mobile);
        assert!(snap.resources.gpu.is_none());
        assert!(snap.secret_refs.is_empty());
    }

    #[test]
    fn publisher_builds_realizer_snapshot_for_cloud_gpu() {
        let snap = build_provider_capability_snapshot(cloud_gpu_input()).expect("valid input");
        assert_eq!(snap.role, DeviceRole::Realizer);
        assert_eq!(snap.provider_kind, ProviderKind::CloudGpu);
        let gpu = snap.resources.gpu.expect("cloud gpu present");
        assert_eq!(gpu.vendor, GpuVendor::Nvidia);
        assert_eq!(gpu.vram_bytes, 24 * GB);
        assert_eq!(gpu.cuda_version.as_deref(), Some("12.4"));
    }

    #[test]
    fn publisher_snapshot_is_deterministic_for_reordered_inputs() {
        let forward = build_provider_capability_snapshot(cloud_gpu_input()).expect("valid");

        // Reverse every list facet of the input.
        let mut reordered = cloud_gpu_input();
        reordered.runtimes.families.reverse();
        reordered.capabilities.reverse();
        reordered.materialized_objects.object_hashes.reverse();
        reordered.network.egress_allowed.reverse();
        reordered.secret_refs.reverse();
        let reversed = build_provider_capability_snapshot(reordered).expect("valid");

        assert_eq!(forward, reversed, "snapshot must not depend on input order");
        assert_eq!(
            serde_json::to_string(&forward).unwrap(),
            serde_json::to_string(&reversed).unwrap(),
            "serialized snapshot must be byte-identical regardless of input order",
        );
    }
}
