//! Ready-State runner class — the snapshot restore-compatibility contract.
//!
//! A warm/booted snapshot built for one host can only be restored on a host
//! whose VMM, CPU normalization, kernel ABI, guest kernel, base rootfs, and
//! device/network model match. [`RunnerClassFacts`] captures exactly those
//! facets; its content-addressed [`RunnerClassId`] (`blake3:<hex>` over the
//! JCS canonical form, matching [`super::hashing::canonical_hash`] and the
//! execution-identity hash family) is the contract a Ready-State Capsule pins
//! and a runner advertises.
//!
//! Granularity is deliberate (plan §5): tight enough that a restore is correct,
//! loose enough that warm-pool reuse across hosts of the same class is viable.
//! A **CPU template** (Firecracker T2/T2S/T2CL/T2A) normalizes CPUID across a
//! vendor family so the class is host-portable; without it the class pins to
//! near-identical silicon and the warm-pool hit rate collapses.
//!
//! ## Status in this milestone
//!
//! Types and hashing only. **Detection is deferred**: nothing here probes the
//! build host yet (that extends `provision_receipt` / `host_gpu` on a KVM host).
//! [`RunnerClassId`] is wired as an *optional* fold input on
//! [`LaunchTemplateKey`](super::launch_template::LaunchTemplateKey) and is
//! intended to also become a declared `execution_id` facet — when `None`
//! (every legacy launch) the launch-template digest is byte-identical to
//! before, so adding the slot is non-breaking.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::hashing::canonical_hash;

/// Host facts that determine snapshot restore compatibility (plan §5).
///
/// Ordering of fields here is irrelevant to the id (JCS sorts keys), but
/// [`RunnerClassFacts::first_divergent_field`] reports the first mismatch in a
/// fixed, human-meaningful order (coarsest → finest) so an error names the most
/// actionable difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerClassFacts {
    /// `"linux"` (the only snapshot-capable OS for now).
    pub os: String,
    /// `"x86_64"` | `"aarch64"`.
    pub arch: String,
    /// Kernel **ABI class**, not an exact patch — e.g. `"linux-6.1"`. Bucketing
    /// by ABI class keeps the warm pool from fragmenting on every patch bump.
    pub kernel_abi_class: String,
    /// Virtual machine monitor, e.g. `"firecracker"`.
    pub vmm: String,
    /// VMM version — snapshots are VMM-version sensitive, so this is part of the
    /// class, e.g. `"1.7.0"`.
    pub vmm_version: String,
    /// Snapshot wire format / version, e.g. `"fc-v2"`. A VMM upgrade that
    /// changes the format invalidates the class cleanly.
    pub snapshot_format: String,
    /// CPU template normalizing CPUID across a vendor family, e.g. `"T2CL"` /
    /// `"T2A"`. `None` means no template (then `cpu_features` pins the exact
    /// normalized feature set and portability is limited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_template: Option<String>,
    /// Normalized CPU feature set, used only when `cpu_template` is `None`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpu_features: Vec<String>,
    /// Content-addressed ref of the guest kernel image (CAS ref).
    pub guest_kernel_id: String,
    /// Content-addressed ref of the read-only base rootfs (CAS ref).
    pub rootfs_base_id: String,
    /// Device profile string, e.g. `"virtio-blk+virtio-net+vsock"`.
    pub device_profile: String,
    /// cgroup version, e.g. `"v2"`.
    pub cgroup: String,
    /// Host network model, e.g. `"tap-nat"`.
    pub network_model: String,
}

/// Field comparison order for [`RunnerClassFacts::first_divergent_field`].
/// Coarsest, most actionable differences first.
const FIELD_ORDER: &[&str] = &[
    "os",
    "arch",
    "vmm",
    "vmm_version",
    "snapshot_format",
    "kernel_abi_class",
    "cpu_template",
    "cpu_features",
    "guest_kernel_id",
    "rootfs_base_id",
    "device_profile",
    "cgroup",
    "network_model",
];

impl RunnerClassFacts {
    /// Content-addressed id of this class: `blake3:<hex>` over the JCS canonical
    /// form. Stable and key-order independent (same scheme as
    /// [`super::hashing::canonical_hash`] and execution identity), so a runner
    /// and a capsule computing the id from equal facts get the same string.
    pub fn id(&self) -> RunnerClassId {
        // canonical_hash only errors on non-canonicalizable values (NaN floats,
        // non-string map keys); RunnerClassFacts has neither, so this is total.
        let hash = canonical_hash(self).expect("RunnerClassFacts is always JCS-canonicalizable");
        RunnerClassId(hash)
    }

    /// Name of the first field (in [`FIELD_ORDER`]) that differs from `other`,
    /// or `None` if the two are equal. Drives the `first_divergent_field` of a
    /// [`RunnerClassMismatch`].
    pub fn first_divergent_field(&self, other: &Self) -> Option<&'static str> {
        for field in FIELD_ORDER {
            let differs = match *field {
                "os" => self.os != other.os,
                "arch" => self.arch != other.arch,
                "vmm" => self.vmm != other.vmm,
                "vmm_version" => self.vmm_version != other.vmm_version,
                "snapshot_format" => self.snapshot_format != other.snapshot_format,
                "kernel_abi_class" => self.kernel_abi_class != other.kernel_abi_class,
                "cpu_template" => self.cpu_template != other.cpu_template,
                "cpu_features" => self.cpu_features != other.cpu_features,
                "guest_kernel_id" => self.guest_kernel_id != other.guest_kernel_id,
                "rootfs_base_id" => self.rootfs_base_id != other.rootfs_base_id,
                "device_profile" => self.device_profile != other.device_profile,
                "cgroup" => self.cgroup != other.cgroup,
                "network_model" => self.network_model != other.network_model,
                _ => false,
            };
            if differs {
                return Some(field);
            }
        }
        None
    }

    /// Fail-closed compatibility check used at the restore Prepare gate.
    ///
    /// `self` is the class the snapshot was **built for** (expected); `actual`
    /// is the candidate restore host's class. Returns a typed
    /// [`RunnerClassMismatch`] naming the first divergent field — never a
    /// boolean, so a caller cannot accidentally treat "unknown" as compatible.
    pub fn ensure_compatible(&self, actual: &Self) -> Result<(), RunnerClassMismatch> {
        match self.first_divergent_field(actual) {
            None => Ok(()),
            Some(field) => Err(RunnerClassMismatch {
                expected: self.id(),
                actual: actual.id(),
                first_divergent_field: field.to_string(),
            }),
        }
    }
}

/// Content-addressed runner-class identifier: `blake3:<hex>`.
///
/// `#[serde(transparent)]` so it serializes as the bare hash string (a launch
/// template key / receipt facet sees a plain `blake3:<hex>`, not a wrapper
/// object).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerClassId(String);

impl RunnerClassId {
    /// Wrap a pre-computed `blake3:<hex>` id (e.g. a value pinned in a manifest
    /// or advertised by a runner). Use [`RunnerClassFacts::id`] to derive one
    /// from facts.
    pub fn from_hash(hash: impl Into<String>) -> Self {
        Self(hash.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunnerClassId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A restore was attempted against a host whose runner class does not match the
/// one the snapshot was built for. Fail-closed: surfaced at the restore Prepare
/// gate, after which `ato run` may fall back to the legacy cold path (if
/// allowed) or error with `first_divergent_field` named.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "runner class mismatch: snapshot built for {expected} cannot restore on {actual} \
     (first divergent field: {first_divergent_field})"
)]
pub struct RunnerClassMismatch {
    /// Class the snapshot was built for.
    pub expected: RunnerClassId,
    /// Class of the candidate restore host.
    pub actual: RunnerClassId,
    /// First field (in coarsest→finest order) that diverged.
    pub first_divergent_field: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> RunnerClassFacts {
        RunnerClassFacts {
            os: "linux".into(),
            arch: "aarch64".into(),
            kernel_abi_class: "linux-6.1".into(),
            vmm: "firecracker".into(),
            vmm_version: "1.7.0".into(),
            snapshot_format: "fc-v2".into(),
            cpu_template: Some("T2A".into()),
            cpu_features: vec![],
            guest_kernel_id: "blake3:kern".into(),
            rootfs_base_id: "blake3:rootfs".into(),
            device_profile: "virtio-blk+virtio-net+vsock".into(),
            cgroup: "v2".into(),
            network_model: "tap-nat".into(),
        }
    }

    #[test]
    fn id_is_blake3_and_stable() {
        let id = facts().id();
        assert!(id.as_str().starts_with("blake3:"), "{id}");
        assert_eq!(facts().id(), facts().id(), "id must be deterministic");
    }

    #[test]
    fn id_is_field_order_independent_but_content_sensitive() {
        // Equal facts → equal id (JCS sorts keys, so struct field order is moot).
        assert_eq!(facts().id(), facts().id());
        // Any changed facet → different id.
        let mut other = facts();
        other.vmm_version = "1.8.0".into();
        assert_ne!(facts().id(), other.id());
    }

    #[test]
    fn equal_facts_are_compatible() {
        assert!(facts().ensure_compatible(&facts()).is_ok());
        assert_eq!(facts().first_divergent_field(&facts()), None);
    }

    #[test]
    fn mismatch_reports_first_divergent_field_coarsest_first() {
        let expected = facts();
        let mut actual = facts();
        // Diverge on both a fine field and a coarse field; coarse wins.
        actual.network_model = "bridge".into();
        actual.arch = "x86_64".into();
        let err = expected.ensure_compatible(&actual).unwrap_err();
        assert_eq!(err.first_divergent_field, "arch");
        assert_eq!(err.expected, expected.id());
        assert_eq!(err.actual, actual.id());
    }

    #[test]
    fn cpu_template_change_is_detected() {
        let expected = facts();
        let mut actual = facts();
        actual.cpu_template = Some("T2CL".into());
        let err = expected.ensure_compatible(&actual).unwrap_err();
        assert_eq!(err.first_divergent_field, "cpu_template");
    }

    #[test]
    fn runner_class_id_serializes_transparently() {
        let id = RunnerClassId::from_hash("blake3:abc");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"blake3:abc\"");
        let back: RunnerClassId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }
}
