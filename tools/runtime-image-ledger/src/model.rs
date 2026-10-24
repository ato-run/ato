//! The `ato.runtime-image-inventory/1` data model.
//!
//! Every record here is a *declaration*: what a human or an operator says it
//! checked. Nothing in this crate observes an image, produces an SBOM, scans a
//! layer, or signs anything. The validator only rejects declarations that are
//! internally inconsistent or missing evidence that they claim.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only inventory schema this tool understands.
pub const INVENTORY_SCHEMA: &str = "ato.runtime-image-inventory/1";

/// A versioned inventory file, reviewed in the ato repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub schema: String,
    pub images: Vec<ImageRecord>,
    pub patch_candidates: Vec<PatchCandidate>,
}

/// Where the image record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedFrom {
    /// The Formation source-to-OCI lane built or froze it.
    Formation,
    /// An application author declared it in `capsule.toml`.
    Author,
    /// A hosted runner host/VM image.
    RunnerHost,
}

/// Lifecycle status of a declared image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageStatus {
    /// Proposed, not yet admitted.
    Candidate,
    /// A receipt declared full satisfaction, but not the active image.
    Verified,
    /// The image a route currently runs.
    Active,
    /// Kept only for history.
    Retired,
    /// Must not be used.
    Quarantined,
}

/// Declared SBOM artifact. The tool never generates or reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sbom {
    pub format: SbomFormat,
    /// Content digest of the SBOM artifact, `sha256:` plus 64 lowercase hex.
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbomFormat {
    #[serde(rename = "spdx-json")]
    SpdxJson,
    #[serde(rename = "cyclonedx-json")]
    CycloneDxJson,
}

/// Declared vulnerability-scan summary. The tool never runs the scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    /// Scanner name and version, as recorded by the operator.
    pub tool: String,
    pub status: ScanStatus,
    pub checked_at: String,
    pub advisory_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Passed,
    Findings,
    NotRun,
}

/// Declared provenance/signature reference. The tool never verifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub kind: ProvenanceKind,
    /// Opaque reference (bundle URL, registry reference, key id). Not a claim
    /// that this tool checked a signature.
    #[serde(rename = "ref")]
    pub ref_: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceKind {
    Sigstore,
    Ed25519,
    Registry,
    None,
}

/// The declared latest Contract K receipt for this image.
///
/// This mirrors the fields of the Rust-owned
/// `ContractVerificationReceipt` (`lib/formation/src/verify.rs`) that the
/// inventory cross-checks. It deliberately does not embed observations: the
/// receipt authority in `lib/formation/src/receipt.rs` remains the only judge
/// of whether a receipt proves K.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSummary {
    pub receipt_ref: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub fully_satisfied: bool,
    pub observed_at: String,
}

/// An accepted risk for an image with `scan.status = findings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingsWaiver {
    pub reason: String,
    pub approved_by: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRecord {
    /// `sha256:` plus 64 lowercase hex. Unique within the inventory.
    pub image_digest: String,
    pub base_digests: Vec<String>,
    /// Runtime name to exact `x.y.z` version. Ranges are rejected.
    pub runtime_versions: BTreeMap<String, String>,
    pub created_from: CreatedFrom,
    #[serde(default)]
    pub sbom: Option<Sbom>,
    #[serde(default)]
    pub scan: Option<Scan>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    pub derivation_ref: String,
    pub contract_ref: String,
    pub status: ImageStatus,
    #[serde(default)]
    pub receipt: Option<ReceiptSummary>,
    #[serde(default)]
    pub findings_waiver: Option<FindingsWaiver>,
}

/// A patch rebuild proposal from one image digest to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchCandidate {
    pub from_digest: String,
    pub to_digest: String,
    /// Non-empty list of advisories that motivate the rebuild.
    pub advisories: Vec<String>,
    pub rebuild: RebuildRecord,
    /// `true` only when the rebuild intentionally moves to a new Contract K.
    /// Without it the rebuild must keep the from-image's K.
    #[serde(default)]
    pub new_contract: bool,
    pub decision: PatchDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebuildRecord {
    pub derivation_ref: String,
    pub contract_ref: String,
    pub receipt_ref: String,
    pub fully_satisfied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchDecision {
    AutoBuild,
    Manual,
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_IMAGE: &str = r#"{
        "image_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "base_digests": ["sha256:2222222222222222222222222222222222222222222222222222222222222222"],
        "runtime_versions": {"node": "20.20.2", "python": "3.12.7"},
        "created_from": "formation",
        "sbom": {"format": "spdx-json", "digest": "sha256:3333333333333333333333333333333333333333333333333333333333333333"},
        "scan": {"tool": "trivy 0.60.0", "status": "findings", "checked_at": "2026-10-20T03:12:04Z", "advisory_refs": ["GHSA-aaaa-bbbb-cccc"]},
        "provenance": {"kind": "sigstore", "ref": "bundle:example"},
        "derivation_ref": "sha256:4444444444444444444444444444444444444444444444444444444444444444",
        "contract_ref": "sha256:5555555555555555555555555555555555555555555555555555555555555555",
        "status": "active",
        "receipt": {"receipt_ref": "sha256:6666666666666666666666666666666666666666666666666666666666666666", "contract_ref": "sha256:5555555555555555555555555555555555555555555555555555555555555555", "derivation_ref": "sha256:4444444444444444444444444444444444444444444444444444444444444444", "fully_satisfied": true, "observed_at": "2026-10-21T09:00:00Z"},
        "findings_waiver": {"reason": "advisory is not reachable", "approved_by": "koh0920", "expires_at": "2026-12-31T00:00:00Z"}
    }"#;

    fn inventory_with_image(image: &str) -> String {
        format!(
            r#"{{"schema": "ato.runtime-image-inventory/1", "images": [{image}], "patch_candidates": []}}"#
        )
    }

    fn parse_inventory(image: &str) -> Inventory {
        serde_json::from_str(&inventory_with_image(image)).expect("inventory parses")
    }

    #[test]
    fn parses_and_round_trips_every_field() {
        let inventory = parse_inventory(FULL_IMAGE);
        let image = &inventory.images[0];
        assert_eq!(image.image_digest.len(), 71);
        assert_eq!(image.runtime_versions["node"], "20.20.2");
        assert_eq!(image.created_from, CreatedFrom::Formation);
        assert_eq!(
            image.sbom.as_ref().expect("sbom").format,
            SbomFormat::SpdxJson
        );
        assert_eq!(
            image.scan.as_ref().expect("scan").status,
            ScanStatus::Findings
        );
        assert_eq!(
            image.provenance.as_ref().expect("provenance").kind,
            ProvenanceKind::Sigstore
        );
        assert_eq!(image.status, ImageStatus::Active);
        assert!(image.receipt.as_ref().expect("receipt").fully_satisfied);
        assert!(image.findings_waiver.is_some());

        let json = serde_json::to_string(&inventory).expect("serializes");
        let reparsed: Inventory = serde_json::from_str(&json).expect("round trip parses");
        assert_eq!(reparsed, inventory);
    }

    #[test]
    fn accepts_patch_candidates_and_new_contract_flag() {
        let json = r#"{
            "schema": "ato.runtime-image-inventory/1",
            "images": [],
            "patch_candidates": [{
                "from_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "to_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "advisories": ["GHSA-aaaa-bbbb-cccc"],
                "rebuild": {"derivation_ref": "sha256:3333333333333333333333333333333333333333333333333333333333333333", "contract_ref": "sha256:4444444444444444444444444444444444444444444444444444444444444444", "receipt_ref": "sha256:5555555555555555555555555555555555555555555555555555555555555555", "fully_satisfied": true},
                "new_contract": true,
                "decision": "auto_build"
            }]
        }"#;
        let inventory: Inventory = serde_json::from_str(json).expect("inventory parses");
        let patch = &inventory.patch_candidates[0];
        assert!(patch.new_contract);
        assert_eq!(patch.decision, PatchDecision::AutoBuild);
    }

    #[test]
    fn new_contract_defaults_to_false() {
        let json = r#"{
            "schema": "ato.runtime-image-inventory/1",
            "images": [],
            "patch_candidates": [{
                "from_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "to_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "advisories": ["GHSA-aaaa-bbbb-cccc"],
                "rebuild": {"derivation_ref": "sha256:3333333333333333333333333333333333333333333333333333333333333333", "contract_ref": "sha256:4444444444444444444444444444444444444444444444444444444444444444", "receipt_ref": "sha256:5555555555555555555555555555555555555555555555555555555555555555", "fully_satisfied": true},
                "decision": "manual"
            }]
        }"#;
        let inventory: Inventory = serde_json::from_str(json).expect("inventory parses");
        assert!(!inventory.patch_candidates[0].new_contract);
    }

    #[test]
    fn rejects_unknown_fields() {
        let unknown_top_level = r#"{"schema": "ato.runtime-image-inventory/1", "images": [], "patch_candidates": [], "surprise": 1}"#;
        assert!(serde_json::from_str::<Inventory>(unknown_top_level).is_err());

        let image_with_extra_field = FULL_IMAGE.replace(
            r#""status": "active","#,
            r#""status": "active", "surprise": 1,"#,
        );
        assert!(
            serde_json::from_str::<Inventory>(&inventory_with_image(&image_with_extra_field))
                .is_err()
        );

        let nested_unknown = FULL_IMAGE.replace(
            r#""kind": "sigstore","#,
            r#""kind": "sigstore", "surprise": 1,"#,
        );
        assert!(serde_json::from_str::<Inventory>(&inventory_with_image(&nested_unknown)).is_err());
    }

    #[test]
    fn rejects_unknown_variants_and_missing_fields() {
        let unknown_status = FULL_IMAGE.replace(r#""status": "active""#, r#""status": "approved""#);
        assert!(serde_json::from_str::<Inventory>(&inventory_with_image(&unknown_status)).is_err());

        let unknown_created_from =
            FULL_IMAGE.replace(r#""created_from": "formation""#, r#""created_from": "ci""#);
        assert!(
            serde_json::from_str::<Inventory>(&inventory_with_image(&unknown_created_from))
                .is_err()
        );

        let missing_digest = FULL_IMAGE.replace(
            r#""image_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111","#,
            "",
        );
        assert!(serde_json::from_str::<Inventory>(&inventory_with_image(&missing_digest)).is_err());
    }

    #[test]
    fn rejects_unknown_top_level_schema_shapes() {
        let missing_patch_candidates =
            r#"{"schema": "ato.runtime-image-inventory/1", "images": []}"#;
        assert!(serde_json::from_str::<Inventory>(missing_patch_candidates).is_err());
    }
}
