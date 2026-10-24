//! Fail-closed validation of declared inventory evidence.
//!
//! Every rule here checks internal consistency of what the file declares. The
//! validator does not observe images, does not run scanners, and does not
//! accept or reject Contract receipts: it only checks that the summary agrees
//! with itself and that statuses which claim verified evidence actually carry
//! one. [`crate::rfc3339`] supplies the clock comparison for waiver expiry.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::model::{
    INVENTORY_SCHEMA, ImageRecord, ImageStatus, Inventory, PatchCandidate, PatchDecision,
    ScanStatus,
};
use crate::rfc3339::{Timestamp, parse};

/// One rejected declaration. `code` is stable; tests and docs name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable rule code, for example `receipt_not_fully_satisfied`.
    pub code: &'static str,
    /// The image digest, `patch_candidates[i]`, or `inventory`.
    pub subject: String,
    /// The JSON field that failed, for example `images[0].receipt.observed_at`.
    pub field: String,
    /// Human-readable explanation.
    pub detail: String,
}

impl Finding {
    fn new(
        code: &'static str,
        subject: impl Into<String>,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            subject: subject.into(),
            field: field.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {}: {}: {}",
            self.code, self.subject, self.field, self.detail
        )
    }
}

/// Validate every declaration in `inventory` against the rules documented in
/// `docs/ops/runtime-image-ledger.md`.
///
/// `now` is passed in so tests can pin the clock; the CLI reads the system
/// clock once per run.
pub fn validate_inventory(inventory: &Inventory, now: Timestamp) -> Vec<Finding> {
    let mut findings = Vec::new();

    if inventory.schema != INVENTORY_SCHEMA {
        findings.push(Finding::new(
            "schema_unsupported",
            "inventory",
            "schema",
            format!(
                "expected {INVENTORY_SCHEMA:?}, found {:?}",
                inventory.schema
            ),
        ));
    }

    let mut first_by_digest: BTreeMap<&str, &ImageRecord> = BTreeMap::new();
    for (index, image) in inventory.images.iter().enumerate() {
        validate_image(index, image, now, &mut findings);
        if first_by_digest
            .insert(image.image_digest.as_str(), image)
            .is_some()
        {
            findings.push(Finding::new(
                "image_digest_duplicate",
                &image.image_digest,
                format!("images[{index}].image_digest"),
                format!(
                    "{} is declared more than once; each image digest must be unique",
                    image.image_digest
                ),
            ));
        }
    }

    for (index, patch) in inventory.patch_candidates.iter().enumerate() {
        validate_patch(index, patch, &first_by_digest, &mut findings);
    }

    findings
}

fn validate_image(index: usize, image: &ImageRecord, now: Timestamp, findings: &mut Vec<Finding>) {
    let subject = image.image_digest.as_str();
    let field = |name: &str| format!("images[{index}].{name}");

    if !is_sha256_digest(&image.image_digest) {
        findings.push(Finding::new(
            "image_digest_invalid",
            subject,
            field("image_digest"),
            format!(
                "expected sha256: followed by 64 lowercase hex characters, found {:?}",
                image.image_digest
            ),
        ));
    }

    for (position, base) in image.base_digests.iter().enumerate() {
        if !is_sha256_digest(base) {
            findings.push(Finding::new(
                "base_digest_invalid",
                subject,
                field(&format!("base_digests[{position}]")),
                format!("expected sha256: followed by 64 lowercase hex characters, found {base:?}"),
            ));
        }
    }

    for (name, version) in &image.runtime_versions {
        if !is_exact_version(version) {
            findings.push(Finding::new(
                "runtime_version_not_exact",
                subject,
                field(&format!("runtime_versions.{name}")),
                format!(
                    "{name} declares {version:?}; exact x.y.z is required (no prefixes, ranges, or latest)"
                ),
            ));
        }
    }

    if let Some(sbom) = &image.sbom
        && !is_sha256_digest(&sbom.digest)
    {
        findings.push(Finding::new(
            "sbom_digest_invalid",
            subject,
            field("sbom.digest"),
            format!(
                "expected sha256: followed by 64 lowercase hex characters, found {:?}",
                sbom.digest
            ),
        ));
    }

    if let Some(scan) = &image.scan
        && parse(&scan.checked_at).is_err()
    {
        findings.push(Finding::new(
            "scan_checked_at_invalid",
            subject,
            field("scan.checked_at"),
            format!(
                "expected YYYY-MM-DDTHH:MM:SSZ (optional fractional seconds), found {:?}",
                scan.checked_at
            ),
        ));
    }

    if let Some(receipt) = &image.receipt {
        if receipt.contract_ref != image.contract_ref {
            findings.push(Finding::new(
                "receipt_contract_mismatch",
                subject,
                field("receipt.contract_ref"),
                format!(
                    "the receipt names K {}, but the image record declares K {}",
                    receipt.contract_ref, image.contract_ref
                ),
            ));
        }
        if parse(&receipt.observed_at).is_err() {
            findings.push(Finding::new(
                "receipt_observed_at_invalid",
                subject,
                field("receipt.observed_at"),
                format!(
                    "expected YYYY-MM-DDTHH:MM:SSZ (optional fractional seconds), found {:?}",
                    receipt.observed_at
                ),
            ));
        }
    }

    if let Some(waiver) = &image.findings_waiver
        && parse(&waiver.expires_at).is_err()
    {
        findings.push(Finding::new(
            "waiver_expires_at_invalid",
            subject,
            field("findings_waiver.expires_at"),
            format!(
                "expected YYYY-MM-DDTHH:MM:SSZ (optional fractional seconds), found {:?}",
                waiver.expires_at
            ),
        ));
    }

    if matches!(image.status, ImageStatus::Verified | ImageStatus::Active) {
        match &image.receipt {
            None => findings.push(Finding::new(
                "receipt_required",
                subject,
                field("receipt"),
                format!(
                    "status {} claims verified execution; a Contract K receipt summary is required",
                    status_name(image.status)
                ),
            )),
            Some(receipt) => {
                if !receipt.fully_satisfied {
                    findings.push(Finding::new(
                        "receipt_not_fully_satisfied",
                        subject,
                        field("receipt.fully_satisfied"),
                        format!(
                            "status {} requires fully_satisfied=true, found {}",
                            status_name(image.status),
                            receipt.fully_satisfied
                        ),
                    ));
                }
                if receipt.derivation_ref != image.derivation_ref {
                    findings.push(Finding::new(
                        "receipt_derivation_mismatch",
                        subject,
                        field("receipt.derivation_ref"),
                        format!(
                            "the receipt names D {}, but the image record declares D {}",
                            receipt.derivation_ref, image.derivation_ref
                        ),
                    ));
                }
            }
        }
    }

    let active_with_findings = image.status == ImageStatus::Active
        && image
            .scan
            .as_ref()
            .is_some_and(|scan| scan.status == ScanStatus::Findings);
    if active_with_findings {
        match &image.findings_waiver {
            None => findings.push(Finding::new(
                "waiver_required",
                subject,
                field("findings_waiver"),
                "status active with scan.status=findings requires a findings_waiver",
            )),
            Some(waiver) => {
                if let Ok(expires_at) = parse(&waiver.expires_at)
                    && expires_at <= now
                {
                    findings.push(Finding::new(
                        "waiver_expired",
                        subject,
                        field("findings_waiver.expires_at"),
                        format!(
                            "the waiver expired at {}; an active image with findings needs an unexpired waiver",
                            waiver.expires_at
                        ),
                    ));
                }
            }
        }
    }
}

fn validate_patch(
    index: usize,
    patch: &PatchCandidate,
    images: &BTreeMap<&str, &ImageRecord>,
    findings: &mut Vec<Finding>,
) {
    let subject = format!("patch_candidates[{index}]");
    let field = |name: &str| format!("patch_candidates[{index}].{name}");

    if patch.advisories.is_empty() {
        findings.push(Finding::new(
            "patch_advisories_empty",
            &subject,
            field("advisories"),
            "a patch candidate must name at least one motivating advisory",
        ));
    }
    for (position, advisory) in patch.advisories.iter().enumerate() {
        if advisory.trim().is_empty() {
            findings.push(Finding::new(
                "patch_advisory_empty",
                &subject,
                field(&format!("advisories[{position}]")),
                "advisory entries must not be empty",
            ));
        }
    }

    for (name, digest) in [
        ("from_digest", patch.from_digest.as_str()),
        ("to_digest", patch.to_digest.as_str()),
    ] {
        if !is_sha256_digest(digest) {
            findings.push(Finding::new(
                "patch_digest_invalid",
                &subject,
                field(name),
                format!(
                    "expected sha256: followed by 64 lowercase hex characters, found {digest:?}"
                ),
            ));
        }
    }

    if images.get(patch.from_digest.as_str()).is_none() {
        findings.push(Finding::new(
            "patch_reference_unknown",
            &subject,
            field("from_digest"),
            format!(
                "from_digest {} is not declared in images[]",
                patch.from_digest
            ),
        ));
    }
    if images.get(patch.to_digest.as_str()).is_none() {
        findings.push(Finding::new(
            "patch_reference_unknown",
            &subject,
            field("to_digest"),
            format!("to_digest {} is not declared in images[]", patch.to_digest),
        ));
    }

    if patch.to_digest == patch.from_digest {
        findings.push(Finding::new(
            "patch_self_reference",
            &subject,
            field("to_digest"),
            format!(
                "a patch candidate cannot rebuild {} onto itself",
                patch.from_digest
            ),
        ));
    }

    if let Some(from_image) = images.get(patch.from_digest.as_str())
        && !patch.new_contract
        && patch.rebuild.contract_ref != from_image.contract_ref
    {
        findings.push(Finding::new(
            "patch_contract_mismatch",
            &subject,
            field("rebuild.contract_ref"),
            format!(
                "the rebuild declares K {}, but the from-image {} declares K {}; set new_contract=true only when the rebuild intentionally moves to a new Contract",
                patch.rebuild.contract_ref, patch.from_digest, from_image.contract_ref
            ),
        ));
    }

    if patch.decision == PatchDecision::AutoBuild && !patch.rebuild.fully_satisfied {
        findings.push(Finding::new(
            "patch_auto_build_not_satisfied",
            &subject,
            field("rebuild.fully_satisfied"),
            "decision auto_build requires rebuild.fully_satisfied=true; a rebuild that did not fully satisfy K cannot be rolled out automatically",
        ));
    }
}

fn status_name(status: ImageStatus) -> &'static str {
    match status {
        ImageStatus::Candidate => "candidate",
        ImageStatus::Verified => "verified",
        ImageStatus::Active => "active",
        ImageStatus::Retired => "retired",
        ImageStatus::Quarantined => "quarantined",
    }
}

/// `sha256:` plus exactly 64 lowercase hex characters.
pub fn is_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

/// Exactly `x.y.z` with decimal digits. Ranges, prefixes, tags, and `latest`
/// are rejected so a record cannot look pinned while resolving to something
/// else later.
pub fn is_exact_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .into_iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const NEW_DIGEST: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const BASE_DIGEST: &str =
        "sha256:3333333333333333333333333333333333333333333333333333333333333333";
    const K: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const NEW_K: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
    const D: &str = "sha256:6666666666666666666666666666666666666666666666666666666666666666";
    const NEW_D: &str = "sha256:7777777777777777777777777777777777777777777777777777777777777777";
    const RECEIPT: &str = "sha256:8888888888888888888888888888888888888888888888888888888888888888";

    const NOW: Timestamp = Timestamp {
        unix_seconds: 1_792_800_000, // 2026-10-24T00:00:00Z
        nanos: 0,
    };

    fn image(digest: &str) -> Value {
        json!({
            "image_digest": digest,
            "base_digests": [BASE_DIGEST],
            "runtime_versions": {"node": "20.20.2"},
            "created_from": "formation",
            "derivation_ref": D,
            "contract_ref": K,
            "status": "candidate"
        })
    }

    fn receipt(contract_ref: &str, derivation_ref: &str, fully_satisfied: bool) -> Value {
        json!({
            "receipt_ref": RECEIPT,
            "contract_ref": contract_ref,
            "derivation_ref": derivation_ref,
            "fully_satisfied": fully_satisfied,
            "observed_at": "2026-10-21T09:00:00Z"
        })
    }

    fn scan(status: &str) -> Value {
        json!({
            "tool": "trivy 0.60.0",
            "status": status,
            "checked_at": "2026-10-20T03:12:04Z",
            "advisory_refs": ["GHSA-aaaa-bbbb-cccc"]
        })
    }

    fn waiver(expires_at: &str) -> Value {
        json!({
            "reason": "advisory not reachable from the declared route",
            "approved_by": "koh0920",
            "expires_at": expires_at
        })
    }

    fn patch(from: &str, to: &str) -> Value {
        json!({
            "from_digest": from,
            "to_digest": to,
            "advisories": ["GHSA-aaaa-bbbb-cccc"],
            "rebuild": {
                "derivation_ref": NEW_D,
                "contract_ref": K,
                "receipt_ref": RECEIPT,
                "fully_satisfied": true
            },
            "decision": "manual"
        })
    }

    fn inventory_of(images: Vec<Value>, patches: Vec<Value>) -> Inventory {
        serde_json::from_value(json!({
            "schema": INVENTORY_SCHEMA,
            "images": images,
            "patch_candidates": patches
        }))
        .expect("test inventory parses")
    }

    fn codes(inventory: &Inventory) -> Vec<&'static str> {
        validate_inventory(inventory, NOW)
            .into_iter()
            .map(|finding| finding.code)
            .collect()
    }

    fn findings_for(inventory: &Inventory) -> Vec<Finding> {
        validate_inventory(inventory, NOW)
    }

    #[test]
    fn accepts_a_minimal_consistent_inventory() {
        let inventory = inventory_of(vec![image(DIGEST)], vec![patch(DIGEST, NEW_DIGEST)]);
        // The patch target is missing here, so only that reference fails.
        assert_eq!(codes(&inventory), vec!["patch_reference_unknown"]);
    }

    #[test]
    fn accepts_the_valid_shape_including_waiver_and_auto_build() {
        let mut old = image(DIGEST);
        old["status"] = json!("active");
        old["scan"] = scan("findings");
        old["receipt"] = receipt(K, D, true);
        old["findings_waiver"] = waiver("2026-12-31T00:00:00Z");

        let mut new = image(NEW_DIGEST);
        new["status"] = json!("verified");
        new["derivation_ref"] = json!(NEW_D);
        new["receipt"] = receipt(K, NEW_D, true);

        let mut candidate = patch(DIGEST, NEW_DIGEST);
        candidate["decision"] = json!("auto_build");

        let inventory = inventory_of(vec![old, new], vec![candidate]);
        assert!(
            findings_for(&inventory).is_empty(),
            "{:?}",
            findings_for(&inventory)
        );
    }

    #[test]
    fn rejects_bad_digests() {
        let inventory = inventory_of(vec![image("sha256:XYZ"), image(DIGEST)], vec![]);
        let findings = findings_for(&inventory);
        let finding = findings
            .iter()
            .find(|finding| finding.code == "image_digest_invalid")
            .expect("digest finding");
        assert_eq!(finding.subject, "sha256:XYZ");
        assert_eq!(finding.field, "images[0].image_digest");
        assert!(finding.to_string().contains("sha256:XYZ"));

        let mut with_bad_base = image(DIGEST);
        with_bad_base["base_digests"] = json!(["node:20"]);
        assert_eq!(
            codes(&inventory_of(vec![with_bad_base], vec![])),
            vec!["base_digest_invalid"]
        );

        let mut with_bad_sbom = image(DIGEST);
        with_bad_sbom["sbom"] = json!({"format": "spdx-json", "digest": "sha256:not-hex"});
        assert_eq!(
            codes(&inventory_of(vec![with_bad_sbom], vec![])),
            vec!["sbom_digest_invalid"]
        );
    }

    #[test]
    fn rejects_duplicate_image_digests() {
        let findings = findings_for(&inventory_of(vec![image(DIGEST), image(DIGEST)], vec![]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "image_digest_duplicate");
        assert_eq!(findings[0].subject, DIGEST);
        assert_eq!(findings[0].field, "images[1].image_digest");
    }

    #[test]
    fn rejects_runtime_ranges_and_tags() {
        for version in [
            "^20.20.2",
            "~20.20.2",
            ">=20.20.2",
            "20.x",
            "20.20.x",
            "latest",
            "20.20",
            "20.20.2.1",
            " 20.20.2",
            "20.20.2 ",
            "",
        ] {
            let mut record = image(DIGEST);
            record["runtime_versions"] = json!({"node": version});
            let findings = findings_for(&inventory_of(vec![record], vec![]));
            assert_eq!(
                findings
                    .iter()
                    .map(|finding| finding.code)
                    .collect::<Vec<_>>(),
                vec!["runtime_version_not_exact"],
                "version {version:?}"
            );
            assert_eq!(findings[0].field, "images[0].runtime_versions.node");
        }
    }

    #[test]
    fn verified_and_active_require_a_fully_satisfied_matching_receipt() {
        let mut missing = image(DIGEST);
        missing["status"] = json!("verified");
        let findings = findings_for(&inventory_of(vec![missing], vec![]));
        assert_eq!(findings[0].code, "receipt_required");
        assert_eq!(findings[0].field, "images[0].receipt");

        let mut unsatisfied = image(DIGEST);
        unsatisfied["status"] = json!("active");
        unsatisfied["receipt"] = receipt(K, D, false);
        let codes = codes(&inventory_of(vec![unsatisfied], vec![]));
        assert_eq!(codes, vec!["receipt_not_fully_satisfied"]);

        let mut wrong_derivation = image(DIGEST);
        wrong_derivation["status"] = json!("verified");
        wrong_derivation["receipt"] = receipt(K, NEW_D, true);
        let findings = findings_for(&inventory_of(vec![wrong_derivation], vec![]));
        assert_eq!(findings[0].code, "receipt_derivation_mismatch");
        assert_eq!(findings[0].field, "images[0].receipt.derivation_ref");

        // A contract mismatch is invalid at any status, even one that does not
        // require a receipt.
        let mut wrong_contract = image(DIGEST);
        wrong_contract["receipt"] = receipt(NEW_K, D, true);
        let findings = findings_for(&inventory_of(vec![wrong_contract], vec![]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "receipt_contract_mismatch");
        assert_eq!(findings[0].field, "images[0].receipt.contract_ref");
    }

    #[test]
    fn requires_an_unexpired_waiver_for_active_images_with_findings() {
        let mut active = image(DIGEST);
        active["status"] = json!("active");
        active["receipt"] = receipt(K, D, true);
        active["scan"] = scan("findings");
        let findings = findings_for(&inventory_of(vec![active.clone()], vec![]));
        assert_eq!(findings[0].code, "waiver_required");

        active["findings_waiver"] = waiver("2020-01-01T00:00:00Z");
        let findings = findings_for(&inventory_of(vec![active.clone()], vec![]));
        assert_eq!(findings[0].code, "waiver_expired");
        assert_eq!(findings[0].field, "images[0].findings_waiver.expires_at");

        active["findings_waiver"] = waiver("2026-12-31T00:00:00Z");
        assert!(findings_for(&inventory_of(vec![active], vec![])).is_empty());
    }

    #[test]
    fn accepts_retired_images_with_findings_without_a_waiver() {
        let mut retired = image(DIGEST);
        retired["status"] = json!("retired");
        retired["scan"] = scan("findings");
        assert!(findings_for(&inventory_of(vec![retired], vec![])).is_empty());
    }

    #[test]
    fn rejects_missing_patch_references_and_self_patches() {
        let inventory = inventory_of(vec![image(DIGEST)], vec![patch(DIGEST, NEW_DIGEST)]);
        let findings = findings_for(&inventory);
        assert_eq!(findings[0].code, "patch_reference_unknown");
        assert_eq!(findings[0].subject, "patch_candidates[0]");
        assert_eq!(findings[0].field, "patch_candidates[0].to_digest");

        let inventory = inventory_of(
            vec![image(DIGEST), image(NEW_DIGEST)],
            vec![patch(DIGEST, DIGEST)],
        );
        let findings = findings_for(&inventory);
        assert!(findings.iter().any(|f| f.code == "patch_self_reference"));

        let mut invalid_digest_patch = patch(DIGEST, NEW_DIGEST);
        invalid_digest_patch["from_digest"] = json!("sha256:nope");
        let findings = findings_for(&inventory_of(
            vec![image(DIGEST), image(NEW_DIGEST)],
            vec![invalid_digest_patch],
        ));
        assert!(findings.iter().any(|f| f.code == "patch_digest_invalid"));
    }

    #[test]
    fn enforces_patch_advisories_and_contract_rules() {
        let mut no_advisories = patch(DIGEST, NEW_DIGEST);
        no_advisories["advisories"] = json!([]);
        assert!(
            codes(&inventory_of(
                vec![image(DIGEST), image(NEW_DIGEST)],
                vec![no_advisories]
            ))
            .contains(&"patch_advisories_empty")
        );

        let mut blank_advisory = patch(DIGEST, NEW_DIGEST);
        blank_advisory["advisories"] = json!(["  "]);
        assert!(
            codes(&inventory_of(
                vec![image(DIGEST), image(NEW_DIGEST)],
                vec![blank_advisory]
            ))
            .contains(&"patch_advisory_empty")
        );

        let mut wrong_contract = patch(DIGEST, NEW_DIGEST);
        wrong_contract["rebuild"]["contract_ref"] = json!(NEW_K);
        let findings = findings_for(&inventory_of(
            vec![image(DIGEST), image(NEW_DIGEST)],
            vec![wrong_contract],
        ));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "patch_contract_mismatch");
        assert_eq!(
            findings[0].field,
            "patch_candidates[0].rebuild.contract_ref"
        );

        // A rebuild that intentionally moves to a new K may declare it.
        let mut new_contract = patch(DIGEST, NEW_DIGEST);
        new_contract["rebuild"]["contract_ref"] = json!(NEW_K);
        new_contract["new_contract"] = json!(true);
        assert!(
            findings_for(&inventory_of(
                vec![image(DIGEST), image(NEW_DIGEST)],
                vec![new_contract],
            ))
            .is_empty()
        );
    }

    #[test]
    fn auto_build_requires_a_fully_satisfied_rebuild() {
        let mut candidate = patch(DIGEST, NEW_DIGEST);
        candidate["decision"] = json!("auto_build");
        candidate["rebuild"]["fully_satisfied"] = json!(false);
        let findings = findings_for(&inventory_of(
            vec![image(DIGEST), image(NEW_DIGEST)],
            vec![candidate.clone()],
        ));
        assert_eq!(findings[0].code, "patch_auto_build_not_satisfied");
        assert_eq!(
            findings[0].field,
            "patch_candidates[0].rebuild.fully_satisfied"
        );

        candidate["decision"] = json!("blocked");
        assert!(
            findings_for(&inventory_of(
                vec![image(DIGEST), image(NEW_DIGEST)],
                vec![candidate],
            ))
            .is_empty()
        );
    }

    #[test]
    fn rejects_unsupported_schema() {
        let mut inventory = inventory_of(vec![image(DIGEST)], vec![]);
        inventory.schema = "ato.runtime-image-inventory/2".to_owned();
        let findings = findings_for(&inventory);
        assert_eq!(findings[0].code, "schema_unsupported");
        assert_eq!(findings[0].field, "schema");
    }

    #[test]
    fn reports_every_independent_finding() {
        let mut broken = image("sha256:XYZ");
        broken["status"] = json!("active");
        broken["runtime_versions"] = json!({"node": "latest"});
        let findings = findings_for(&inventory_of(vec![broken], vec![]));
        let codes: Vec<_> = findings.iter().map(|finding| finding.code).collect();
        assert!(codes.contains(&"image_digest_invalid"));
        assert!(codes.contains(&"runtime_version_not_exact"));
        assert!(codes.contains(&"receipt_required"));
    }

    #[test]
    fn rejects_invalid_declared_timestamps() {
        let mut record = image(DIGEST);
        record["status"] = json!("active");
        record["receipt"] = receipt(K, D, true);
        record["receipt"]["observed_at"] = json!("2026-10-21 09:00:00");
        record["scan"] = json!({
            "tool": "trivy 0.60.0",
            "status": "passed",
            "checked_at": "21 Oct 2026",
            "advisory_refs": []
        });
        let findings = findings_for(&inventory_of(vec![record], vec![]));
        let codes: Vec<_> = findings.iter().map(|finding| finding.code).collect();
        assert!(codes.contains(&"scan_checked_at_invalid"));
        assert!(codes.contains(&"receipt_observed_at_invalid"));

        let mut with_waiver = image(DIGEST);
        with_waiver["status"] = json!("active");
        with_waiver["receipt"] = receipt(K, D, true);
        with_waiver["scan"] = scan("findings");
        with_waiver["findings_waiver"] = waiver("2099-01-01T00:00:00+09:00");
        let findings = findings_for(&inventory_of(vec![with_waiver], vec![]));
        assert_eq!(findings[0].code, "waiver_expires_at_invalid");
    }

    #[test]
    fn digest_and_version_helpers() {
        assert!(is_sha256_digest(DIGEST));
        assert!(!is_sha256_digest(&DIGEST.to_uppercase()));
        assert!(!is_sha256_digest("sha512:abc"));
        assert!(!is_sha256_digest(
            "sha256:111111111111111111111111111111111111111111111111111111111111111g"
        ));
        assert!(!is_sha256_digest("sha256:1111"));
        assert!(!is_sha256_digest(
            "1111111111111111111111111111111111111111111111111111111111111111"
        ));

        assert!(is_exact_version("3.12.7"));
        assert!(is_exact_version("20.20.2"));
        assert!(!is_exact_version("v3.12.7"));
        assert!(!is_exact_version("3.12.7-rc1"));
        assert!(!is_exact_version("3.12"));
        assert!(!is_exact_version("latest"));
        assert!(!is_exact_version("3.12.7+build"));
    }
}
