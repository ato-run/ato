//! Shared seal+scan orchestration for the Ready-State backends.
//!
//! `seal_and_scan` runs the no-secret policy in ONE place so the Fake and
//! Firecracker backends cannot drift, in two phases:
//!
//! **Phase 1 — gate (NO storing):** declared markers are scanned on the FULL
//! bytes of EVERY layer, and provider/env are scanned on the small build-authored
//! layers (app/dependency). If either fails, the build is rejected **before any
//! layer is written to CAS** — a rejected build never persists secret-bearing
//! bytes. Never cached.
//!
//! **Phase 2 — store + advisory:** each layer is stored into CapsuleFS; the large
//! opaque layers (rootfs/runtime/vmstate/memory) get provider/env/entropy as
//! ADVISORY only — consulted from the content-addressed [`ScanCache`] (hit ⇒ skip
//! the re-scan; the dominant win for byte-identical bases) and bounded by an
//! advisory byte budget on a miss so a 512 MB RAM image never blocks the build.
//! High-entropy is advisory everywhere. Coverage is recorded honestly.
//!
//! Takes layer byte-SLICES (not an owned `BuildLayers`) so the caller need not
//! clone hundreds of MB of rootfs/memory.

use capsulefs::{
    store_blob, BlobManifest, CasStore, ChunkingKind, LayerKind, MEMORY_PAGE_CHUNK_SIZE,
};

use crate::backend::SnapshotError;
use crate::bench;
use crate::manifest::{LayerScanCoverage, ReadyStateLayers};
use crate::scan_cache::ScanCache;
use crate::scanner::{self, FindingKind, ScanReport, SecretFinding, POLICY_VERSION, SCANNER_VERSION};

/// Default advisory byte budget (8 MiB): small layers (incl. all test fixtures)
/// scan fully; large opaque layers are capped so the build does not block.
/// `0` = unbounded. Override with `ATO_SCAN_ADVISORY_BUDGET_BYTES`.
pub const DEFAULT_ADVISORY_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Advisory byte budget from `ATO_SCAN_ADVISORY_BUDGET_BYTES` (default
/// [`DEFAULT_ADVISORY_BUDGET_BYTES`]; `0` = unbounded).
pub fn advisory_budget_from_env() -> usize {
    std::env::var("ATO_SCAN_ADVISORY_BUDGET_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ADVISORY_BUDGET_BYTES)
}

/// Borrowed view of the six sealed layers (no owned copies).
pub struct SealLayersRef<'a> {
    pub rootfs: &'a [u8],
    pub runtime: Option<&'a [u8]>,
    pub dependency: Option<&'a [u8]>,
    pub app: Option<&'a [u8]>,
    pub vmstate: &'a [u8],
    pub memory: &'a [u8],
}

/// Output of [`seal_and_scan`] (only returned when the gate passes).
pub struct SealOutput {
    pub layers: ReadyStateLayers,
    pub report: ScanReport,
    pub coverage: Vec<LayerScanCoverage>,
    pub sealed_bytes: u64,
}

struct Acc {
    heuristic: Vec<SecretFinding>,
    coverage: Vec<LayerScanCoverage>,
    sealed_bytes: u64,
}

fn coverage_row(layer: &'static str, hex: String, blocking: bool, capped: bool, source: &str) -> LayerScanCoverage {
    LayerScanCoverage {
        layer: layer.to_string(),
        content_hash: hex,
        scanner_version: SCANNER_VERSION.to_string(),
        policy_version: POLICY_VERSION.to_string(),
        declared_checked: true,
        blocking_checks: if blocking { vec!["provider".into(), "env".into()] } else { Vec::new() },
        advisory_checks: if blocking {
            vec!["entropy".into()]
        } else {
            vec!["provider".into(), "env".into(), "entropy".into()]
        },
        coverage: if capped { "budget_capped".into() } else { "full".into() },
        source: source.to_string(),
    }
}

/// Store one app/dependency layer (gate findings already computed in Phase 1).
fn store_blocking(
    store: &CasStore,
    layer: &'static str,
    bytes: &[u8],
    kind: LayerKind,
    findings: Vec<SecretFinding>,
    acc: &mut Acc,
) -> Result<BlobManifest, SnapshotError> {
    let blob = store_blob(store, kind, bytes, ChunkingKind::ContentDefined)?;
    acc.sealed_bytes += blob.total_len;
    let hex = blob.id().hex().to_string();
    acc.heuristic.extend(findings);
    acc.coverage.push(coverage_row(layer, hex, true, false, "scanned"));
    Ok(blob)
}

/// Store one large opaque layer + run the ADVISORY scan via cache/budget.
#[allow(clippy::too_many_arguments)]
fn store_opaque(
    store: &CasStore,
    cache: &ScanCache,
    budget: usize,
    layer: &'static str,
    bytes: &[u8],
    kind: LayerKind,
    chunking: ChunkingKind,
    prestored: Option<BlobManifest>,
    acc: &mut Acc,
) -> Result<BlobManifest, SnapshotError> {
    let blob = match prestored {
        Some(b) => b,
        None => store_blob(store, kind, bytes, chunking)?,
    };
    acc.sealed_bytes += blob.total_len;
    let hex = blob.id().hex().to_string();
    let (findings, capped, source) = match cache.get(&hex, layer) {
        Some(hit) => {
            bench::count("scan.cache.hit", 1);
            bench::count("scan.bytes_cache_skipped", bytes.len() as u64);
            (hit.findings, hit.capped, "cache_hit")
        }
        None => {
            bench::count("scan.cache.miss", 1);
            let (f, c) = scanner::scan_layer_budgeted(layer, bytes, budget);
            let scanned = if budget == 0 { bytes.len() } else { bytes.len().min(budget) };
            bench::count("scan.bytes_scanned", scanned as u64);
            cache.put(&hex, c, &f);
            (f, c, "scanned")
        }
    };
    acc.heuristic.extend(findings);
    acc.coverage.push(coverage_row(layer, hex, false, capped, source));
    Ok(blob)
}

/// The no-secret GATE over a set of layers, WITHOUT storing anything: declared
/// markers on every provided layer + provider/env blocking on `app`/`dependency`.
/// Fails closed (`SecretFoundInSnapshot` / `SecretScanFindings`). This is the
/// single source of the gate policy — used both by [`preflight_gate`] (before
/// Firecracker stores the rootfs / boots) and by [`seal_and_scan`]'s Phase 1, so
/// the two backends cannot drift.
pub fn gate_layers(
    declared_layers: &[&[u8]],
    app: &[u8],
    dependency: &[u8],
    markers: &[String],
) -> Result<(), SnapshotError> {
    let mut declared: Vec<String> = Vec::new();
    for bytes in declared_layers {
        for h in scanner::declared_hits_in(bytes, markers) {
            if !declared.contains(&h) {
                declared.push(h);
            }
        }
    }
    if !declared.is_empty() {
        return Err(SnapshotError::SecretFoundInSnapshot(declared));
    }
    let blocking: Vec<SecretFinding> = scanner::scan_layer("app", app)
        .into_iter()
        .chain(scanner::scan_layer("dependency", dependency))
        .filter(|f| matches!(f.kind, FindingKind::ProviderKeyPrefix | FindingKind::EnvAssignment))
        .collect();
    if !blocking.is_empty() {
        return Err(SnapshotError::SecretScanFindings(blocking));
    }
    Ok(())
}

/// Preflight gate for the Firecracker build path: run the no-secret gate over the
/// INPUT layers (rootfs/runtime/dependency/app — vmstate/memory don't exist until
/// after boot+snapshot) BEFORE the rootfs is stored into CAS / the stable rootfs
/// image is written / the VM is booted. A rejected build therefore never writes
/// secret-bearing rootfs bytes to disk. The full six-layer gate still runs in
/// [`seal_and_scan`] after the snapshot (catching vmstate/memory before they are
/// stored).
pub fn preflight_gate(
    rootfs: &[u8],
    runtime: Option<&[u8]>,
    dependency: Option<&[u8]>,
    app: Option<&[u8]>,
    markers: &[String],
) -> Result<(), SnapshotError> {
    gate_layers(
        &[rootfs, runtime.unwrap_or(&[]), dependency.unwrap_or(&[]), app.unwrap_or(&[])],
        app.unwrap_or(&[]),
        dependency.unwrap_or(&[]),
        markers,
    )
}

/// Store + scan all present layers under the layer-scoped no-secret policy.
/// `rootfs_prestored` lets the Firecracker backend pass the rootfs blob it
/// already stored (for the stable drive path) so it is not stored twice.
/// Returns `Err` (nothing stored) when the Phase-1 gate rejects the build.
pub fn seal_and_scan(
    store: &CasStore,
    layers: SealLayersRef<'_>,
    declared_markers: &[String],
    cache: &ScanCache,
    advisory_budget: usize,
    rootfs_prestored: Option<BlobManifest>,
) -> Result<SealOutput, SnapshotError> {
    let app_bytes = layers.app.unwrap_or(&[]);
    let dep_bytes = layers.dependency.unwrap_or(&[]);

    // ── Phase 1: GATE (no storing) — shared with the Firecracker preflight ──
    // declared markers on EVERY layer; provider/env on app+dependency.
    gate_layers(
        &[
            layers.rootfs,
            layers.runtime.unwrap_or(&[]),
            dep_bytes,
            app_bytes,
            layers.vmstate,
            layers.memory,
        ],
        app_bytes,
        dep_bytes,
        declared_markers,
    )?;
    // app/dep findings (incl. advisory entropy) reused when storing them below.
    let app_findings = scanner::scan_layer("app", app_bytes);
    let dep_findings = scanner::scan_layer("dependency", dep_bytes);

    // ── Phase 2: STORE + advisory scan ──────────────────────────────────────
    let cd = ChunkingKind::ContentDefined;
    let page = ChunkingKind::PageAligned { page_size: MEMORY_PAGE_CHUNK_SIZE as u64 };
    let mut acc = Acc { heuristic: Vec::new(), coverage: Vec::new(), sealed_bytes: 0 };

    let rootfs = store_opaque(store, cache, advisory_budget, "rootfs", layers.rootfs, LayerKind::Rootfs, cd, rootfs_prestored, &mut acc)?;
    let runtime = match layers.runtime {
        Some(b) => Some(store_opaque(store, cache, advisory_budget, "runtime", b, LayerKind::Runtime, cd, None, &mut acc)?),
        None => None,
    };
    let dependency = match layers.dependency {
        Some(b) => Some(store_blocking(store, "dependency", b, LayerKind::Dependency, dep_findings, &mut acc)?),
        None => None,
    };
    let app = match layers.app {
        Some(b) => Some(store_blocking(store, "app", b, LayerKind::App, app_findings, &mut acc)?),
        None => None,
    };
    let vmstate = store_opaque(store, cache, advisory_budget, "vmstate", layers.vmstate, LayerKind::VmState, cd, None, &mut acc)?;
    let memory = store_opaque(store, cache, advisory_budget, "memory", layers.memory, LayerKind::Memory, page, None, &mut acc)?;

    Ok(SealOutput {
        layers: ReadyStateLayers {
            rootfs: Some(rootfs),
            runtime,
            dependency,
            app,
            vmstate: Some(vmstate),
            memory: Some(memory),
        },
        // Gate already passed → declared_hits empty; heuristic holds the advisory
        // findings (+ any app/dep entropy) for the proof's advisory list.
        report: ScanReport { declared_hits: Vec::new(), heuristic: acc.heuristic },
        coverage: acc.coverage,
        sealed_bytes: acc.sealed_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER_IN_APP: &[u8] = b"token sk-proj-ABCDEFGHIJ1234567890abcdef end";

    #[test]
    fn preflight_passes_on_clean_layers() {
        assert!(preflight_gate(b"clean rootfs", Some(b"runtime"), None, Some(b"app code"), &[]).is_ok());
    }

    #[test]
    fn preflight_rejects_declared_marker_in_rootfs() {
        let markers = vec!["TOPSECRET-VALUE".to_string()];
        let err = preflight_gate(b"...embedded TOPSECRET-VALUE here...", None, None, None, &markers).unwrap_err();
        assert!(matches!(err, SnapshotError::SecretFoundInSnapshot(_)), "{err:?}");
    }

    #[test]
    fn preflight_rejects_provider_key_in_app() {
        let err = preflight_gate(b"clean", None, None, Some(PROVIDER_IN_APP), &[]).unwrap_err();
        assert!(matches!(err, SnapshotError::SecretScanFindings(_)), "{err:?}");
    }

    #[test]
    fn preflight_ignores_provider_key_in_rootfs() {
        // provider/env in the opaque rootfs layer is ADVISORY, not a preflight block.
        assert!(preflight_gate(PROVIDER_IN_APP, None, None, None, &[]).is_ok());
    }

    #[test]
    fn gate_layers_matches_preflight_semantics() {
        // declared on a non-app layer blocks; provider on app blocks; clean passes.
        let m = vec!["XSECRET".to_string()];
        assert!(gate_layers(&[b"has XSECRET", b""], b"", b"", &m).is_err());
        assert!(gate_layers(&[b"a", b"b"], PROVIDER_IN_APP, b"", &[]).is_err());
        assert!(gate_layers(&[b"a", b"b"], b"clean app", b"clean dep", &[]).is_ok());
    }
}
