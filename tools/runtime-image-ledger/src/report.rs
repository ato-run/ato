//! Human-readable and JSON rendering of the declared ledger.
//!
//! The text report always names missing evidence as `missing`. It never prints
//! a green/valid-looking marker for evidence that is simply absent, and it
//! repeats that nothing here was observed by this tool.

use std::fmt::Write as _;

use anyhow::Result;
use serde::Serialize;

use crate::model::{
    ImageRecord, ImageStatus, Inventory, PatchCandidate, PatchDecision, ProvenanceKind, SbomFormat,
    ScanStatus,
};
use crate::validate::Finding;

const DIGEST_WIDTH: usize = 71;

#[derive(Serialize)]
struct ReportJson<'a> {
    schema: &'a str,
    path: &'a str,
    inventory_sha256: &'a str,
    valid: bool,
    findings: &'a [Finding],
    images: &'a [ImageRecord],
    patch_candidates: &'a [PatchCandidate],
}

/// Machine-readable report. `valid` is the validation result; `images` and
/// `patch_candidates` are the declarations verbatim.
pub fn render_json(
    inventory: &Inventory,
    findings: &[Finding],
    path: &str,
    inventory_sha256: &str,
) -> Result<String> {
    let report = ReportJson {
        schema: &inventory.schema,
        path,
        inventory_sha256,
        valid: findings.is_empty(),
        findings,
        images: &inventory.images,
        patch_candidates: &inventory.patch_candidates,
    };
    let mut rendered = serde_json::to_string_pretty(&report)?;
    rendered.push('\n');
    Ok(rendered)
}

/// Text report: one row per image with the declared evidence, then one block
/// per patch candidate with its decision.
pub fn render_text(
    inventory: &Inventory,
    findings: &[Finding],
    path: &str,
    inventory_sha256: &str,
) -> String {
    let mut out = String::new();
    let validation = if findings.is_empty() {
        "valid".to_owned()
    } else {
        format!("invalid ({} finding(s))", findings.len())
    };
    let _ = writeln!(out, "inventory: {path}");
    let _ = writeln!(out, "schema: {}", inventory.schema);
    let _ = writeln!(out, "inventory_sha256: {inventory_sha256}");
    let _ = writeln!(out, "validation: {validation}");
    let _ = writeln!(
        out,
        "images: {}  patch_candidates: {}",
        inventory.images.len(),
        inventory.patch_candidates.len()
    );

    let _ = writeln!(out);
    let _ = writeln!(out, "IMAGES ({})", inventory.images.len());
    let _ = writeln!(
        out,
        "{:<DIGEST_WIDTH$}  {:<11}  {:<9}  {:<14}  {:<11}  RECEIPT",
        "DIGEST", "STATUS", "SCAN", "SBOM", "PROVENANCE"
    );
    for image in &inventory.images {
        let _ = writeln!(
            out,
            "{:<DIGEST_WIDTH$}  {:<11}  {:<9}  {:<14}  {:<11}  {}",
            image.image_digest,
            status_label(image.status),
            scan_label(image),
            sbom_label(image),
            provenance_label(image),
            receipt_label(image),
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "PATCH CANDIDATES ({})",
        inventory.patch_candidates.len()
    );
    for (index, patch) in inventory.patch_candidates.iter().enumerate() {
        let _ = writeln!(out, "patch_candidates[{index}]");
        let _ = writeln!(out, "  FROM: {}", patch.from_digest);
        let _ = writeln!(out, "  TO:   {}", patch.to_digest);
        let _ = writeln!(
            out,
            "  DECISION: {}  REBUILD_FULLY_SATISFIED: {}  NEW_CONTRACT: {}  ADVISORIES: {}",
            decision_label(patch.decision),
            patch.rebuild.fully_satisfied,
            patch.new_contract,
            patch.advisories.len(),
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "NOTE: declared evidence only. This tool does not scan, sign, or verify any"
    );
    let _ = writeln!(
        out,
        "image, and it is not the Contract K authority. Receipt rows are declared"
    );
    let _ = writeln!(
        out,
        "summaries; only lib/formation/src/receipt.rs (accept_receipt /"
    );
    let _ = writeln!(
        out,
        "accept_verified_route) accepts receipts. `missing` means the inventory"
    );
    let _ = writeln!(
        out,
        "declares no evidence for that item; it is not a passing result."
    );

    out
}

fn status_label(status: ImageStatus) -> &'static str {
    match status {
        ImageStatus::Candidate => "candidate",
        ImageStatus::Verified => "verified",
        ImageStatus::Active => "active",
        ImageStatus::Retired => "retired",
        ImageStatus::Quarantined => "quarantined",
    }
}

fn scan_label(image: &ImageRecord) -> &'static str {
    match &image.scan {
        None => "missing",
        Some(scan) => match scan.status {
            ScanStatus::Passed => "passed",
            ScanStatus::Findings => "findings",
            ScanStatus::NotRun => "not_run",
        },
    }
}

fn sbom_label(image: &ImageRecord) -> &'static str {
    match &image.sbom {
        None => "missing",
        Some(sbom) => match sbom.format {
            SbomFormat::SpdxJson => "spdx-json",
            SbomFormat::CycloneDxJson => "cyclonedx-json",
        },
    }
}

fn provenance_label(image: &ImageRecord) -> &'static str {
    match &image.provenance {
        None => "missing",
        Some(provenance) => match provenance.kind {
            ProvenanceKind::Sigstore => "sigstore",
            ProvenanceKind::Ed25519 => "ed25519",
            ProvenanceKind::Registry => "registry",
            ProvenanceKind::None => "none",
        },
    }
}

fn receipt_label(image: &ImageRecord) -> &'static str {
    match &image.receipt {
        None => "missing",
        Some(receipt) => {
            if receipt.fully_satisfied {
                "satisfied"
            } else {
                "not_satisfied"
            }
        }
    }
}

fn decision_label(decision: PatchDecision) -> &'static str {
    match decision {
        PatchDecision::AutoBuild => "auto_build",
        PatchDecision::Manual => "manual",
        PatchDecision::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::INVENTORY_SCHEMA;
    use crate::rfc3339::Timestamp;
    use crate::validate::validate_inventory;

    fn inventory() -> Inventory {
        serde_json::from_str(
            r#"{
                "schema": "ato.runtime-image-inventory/1",
                "images": [
                    {
                        "image_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                        "base_digests": [],
                        "runtime_versions": {"node": "20.20.2"},
                        "created_from": "formation",
                        "derivation_ref": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                        "contract_ref": "sha256:3333333333333333333333333333333333333333333333333333333333333333",
                        "status": "candidate"
                    }
                ],
                "patch_candidates": []
            }"#,
        )
        .expect("test inventory")
    }

    #[test]
    fn text_report_names_missing_evidence_as_missing() {
        let inventory = inventory();
        let findings = validate_inventory(&inventory, Timestamp::from_unix_seconds(1_792_800_000));
        let rendered = render_text(
            &inventory,
            &findings,
            "fixtures/valid-inventory.json",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(rendered.contains("validation: valid"));
        assert!(rendered.contains("candidate"));
        // Four evidence columns are absent, and the footer explains "missing".
        assert_eq!(rendered.matches("missing").count(), 5);
        assert!(rendered.contains("does not scan, sign, or verify"));
        assert!(rendered.contains("not the Contract K authority"));
        assert!(!rendered.contains("PASS"));
    }

    #[test]
    fn json_report_is_parseable_and_carries_findings() {
        let inventory = inventory();
        let findings = validate_inventory(&inventory, Timestamp::from_unix_seconds(1_792_800_000));
        let rendered = render_json(
            &inventory,
            &findings,
            "fixtures/valid-inventory.json",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("renders");
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["schema"], INVENTORY_SCHEMA);
        assert_eq!(value["valid"], true);
        assert_eq!(value["findings"].as_array().expect("findings").len(), 0);
        assert_eq!(
            value["images"][0]["image_digest"]
                .as_str()
                .expect("digest")
                .len(),
            71
        );
    }
}
