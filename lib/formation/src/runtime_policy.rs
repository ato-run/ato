//! What a Runtime may execute, as opposed to what authoring may choose.
//!
//! The authoring catalog ([`crate::intent::SUPPORTED_NODE_RUNTIME`] and
//! [`crate::intent::NODE_VERSION_LADDER`]) decides what a NEW source may
//! resolve to. A sealed Derivation is different: it was formed by an earlier
//! build, it keeps the `[[runtime]]` it pinned, and nothing re-resolves it.
//! So an execution boundary needs its own statement about which pinned
//! runtimes are safe to run.
//!
//! This module is that statement for CVE-2026-111497, an out-of-bounds write
//! in the Brotli decoder Node.js ships inside `zlib`. Node fixed it in
//! 22.23.4, 24.22.1 and 26.12.1; the 18.x and 20.x lines are end-of-life and
//! will never be fixed, so every release on them is affected by construction.
//!
//! Three rules keep "cannot prove it is patched" from turning into "patched":
//!
//! - a release on an EOL line is [`RuntimePolicyVerdict::NotPatched`];
//! - a release below a known line's fix floor is
//!   [`RuntimePolicyVerdict::NotPatched`];
//! - anything this table cannot decide — a range, a partial version, a Node
//!   line it has never been told about — is
//!   [`RuntimePolicyVerdict::Unverifiable`], and execution is refused with
//!   that distinct code. Unknown is never reported as patched.
//!
//! The table here is the single authoritative copy of the patched releases;
//! the authoring catalog references it rather than repeating the literals.

use std::collections::BTreeMap;

use crate::authoring::BoundDerivation;

/// The advisory this policy answers to. Not part of the error code: another
/// CVE in the same release range must not multiply the codes.
pub const NODE_CVE_2026_111497: &str = "CVE-2026-111497";

/// The runtime key a Node process route declares in `[[runtime]]`.
pub const NODE_RUNTIME: &str = "node";

/// The lowest release of the 22.x line that contains the fix. It is also the
/// authoring catalog's first entry and default, on purpose: new work and
/// execution of old work must agree on what "patched" means.
pub const PATCHED_NODE_LOWEST: &str = "22.23.4";
/// The lowest patched release of the 24.x line.
pub const NODE_24_FIRST_PATCHED: &str = "24.22.1";
/// The lowest patched release of the 26.x line. Not provisioned by this build
/// yet (the catalog is 22/24), but a sealed Derivation may name it; calling a
/// patched release affected would be a false exposure claim.
pub const NODE_26_FIRST_PATCHED: &str = "26.12.1";

/// The Node releases this build will provision and declares as supported.
///
/// `intent::NODE_VERSION_LADDER`, `intent::SUPPORTED_NODE_RUNTIME` and
/// `intent::DEFAULT_NODE` derive from this list — they must not repeat the
/// literals.
pub const PATCHED_NODE_RELEASES: &[&str] = &[PATCHED_NODE_LOWEST, NODE_24_FIRST_PATCHED];

/// Node major versions that are end-of-life: every release on these lines is
/// affected and no fix will ever be published for them.
pub const AFFECTED_NODE_MAJORS: &[u64] = &[18, 20];

/// Per-line fix floors. A release is patched only at or above its line's
/// floor; a major absent from both this table and [`AFFECTED_NODE_MAJORS`] is
/// [`RuntimePolicyVerdict::Unverifiable`] rather than assumed safe.
pub const PATCHED_NODE_FLOORS: &[(u64, &str)] = &[
    (22, PATCHED_NODE_LOWEST),
    (24, NODE_24_FIRST_PATCHED),
    (26, NODE_26_FIRST_PATCHED),
];

/// The refusal code for a release the policy proves affected.
pub const RUNTIME_VERSION_NOT_PATCHED: &str = "runtime_version_not_patched";
/// The refusal code for a release the policy cannot classify. Fail closed:
/// an unprovable version is not executed and is never reported as patched.
pub const RUNTIME_VERSION_UNVERIFIABLE: &str = "runtime_version_unverifiable";

/// What this build can say about one declared Node release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePolicyVerdict {
    /// The release contains the CVE-2026-111497 fix.
    Patched,
    /// The release is on an EOL line, or below its line's fix floor.
    NotPatched,
    /// The declaration does not let this build prove either way.
    Unverifiable,
}

/// A policy refusal, shaped for the admission boundaries that already refuse
/// with a typed code and a message for the person reading the failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyRefusal {
    /// `runtime_version_not_patched` or `runtime_version_unverifiable`.
    pub code: &'static str,
    /// Names the CVE, the declared release and the patched releases.
    pub message: String,
}

impl RuntimePolicyRefusal {
    fn affected(version: &str) -> Self {
        Self {
            code: RUNTIME_VERSION_NOT_PATCHED,
            message: format!(
                "the route pins Node {version}, which is affected by {NODE_CVE_2026_111497} \
                 (patched releases: {})",
                patched_releases_text()
            ),
        }
    }

    fn unverifiable(version: &str) -> Self {
        Self {
            code: RUNTIME_VERSION_UNVERIFIABLE,
            message: format!(
                "the route pins Node {version:?}, which this Runtime cannot prove includes the \
                 {NODE_CVE_2026_111497} fix; a version this build cannot classify is not executed \
                 (patched releases: {})",
                patched_releases_text()
            ),
        }
    }
}

fn patched_releases_text() -> String {
    PATCHED_NODE_RELEASES.join(", ")
}

/// Classify one declared Node release.
///
/// Only an exact `major.minor.patch` can be classified. A range (`>=22`), a
/// partial version (`22`, `22.14`) and an unknown line are `Unverifiable`, not
/// `Patched`: the declaration does not let this build assert the fix is there.
/// A leading `v` is accepted because `.nvmrc`-style values carry one.
pub fn node_runtime_verdict(version: &str) -> RuntimePolicyVerdict {
    let trimmed = version.trim();
    let exact = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let Ok(parsed) = semver::Version::parse(exact) else {
        return RuntimePolicyVerdict::Unverifiable;
    };
    if AFFECTED_NODE_MAJORS.contains(&parsed.major) {
        return RuntimePolicyVerdict::NotPatched;
    }
    let Some((_, floor)) = PATCHED_NODE_FLOORS
        .iter()
        .find(|(major, _)| *major == parsed.major)
    else {
        return RuntimePolicyVerdict::Unverifiable;
    };
    let Ok(floor) = semver::Version::parse(floor) else {
        return RuntimePolicyVerdict::Unverifiable;
    };
    if parsed < floor {
        RuntimePolicyVerdict::NotPatched
    } else {
        RuntimePolicyVerdict::Patched
    }
}

fn check_node(version: &str) -> Result<(), RuntimePolicyRefusal> {
    match node_runtime_verdict(version) {
        RuntimePolicyVerdict::Patched => Ok(()),
        RuntimePolicyVerdict::NotPatched => Err(RuntimePolicyRefusal::affected(version)),
        RuntimePolicyVerdict::Unverifiable => Err(RuntimePolicyRefusal::unverifiable(version)),
    }
}

/// The policy for one sealed Derivation: every route-wide and step-scoped
/// `node` runtime must be patched. Other runtimes (Python, OCI facts, package
/// managers) are not this policy's business.
///
/// A route with no Node runtime is admitted: it cannot run Node at all.
pub fn derivation_runtime_policy(derivation: &BoundDerivation) -> Result<(), RuntimePolicyRefusal> {
    if let Some(version) = derivation.runtimes.get(NODE_RUNTIME) {
        check_node(version)?;
    }
    for step in &derivation.steps {
        if let Some(version) = step.runtimes.get(NODE_RUNTIME) {
            check_node(version)?;
        }
    }
    Ok(())
}

/// The policy for a retained candidate's recorded toolchain binding
/// (`toolchain.node.<version>`).
///
/// Replay never provisions a toolchain: it runs the one the descriptor
/// recorded, if the host still has it. So the recorded binding is an
/// execution input in its own right and is checked here even before the
/// toolchain directory is looked for.
pub fn node_toolchain_policy(
    toolchains: &BTreeMap<String, String>,
) -> Result<(), RuntimePolicyRefusal> {
    match toolchains.get(NODE_RUNTIME) {
        Some(version) => check_node(version),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derivation(
        runtimes: serde_json::Value,
        step_runtimes: serde_json::Value,
    ) -> BoundDerivation {
        let mut steps = Vec::new();
        if !step_runtimes.as_object().is_none_or(|map| map.is_empty()) {
            steps.push(serde_json::json!({
                "id": "app",
                "protocol": "ato.process@1",
                "op": "serve",
                "runtimes": step_runtimes,
            }));
        }
        serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": runtimes, "steps": steps, "ports": [], "state": [],
            "effects": "pure"
        }))
        .expect("a minimal derivation")
    }

    // ── the table itself ───────────────────────────────────────────────────

    // The authoring catalog only exists with `planning`, but this module does
    // not: the wasm receipt authority still needs the verdict.
    #[cfg(feature = "planning")]
    #[test]
    fn the_authoring_catalog_is_this_table_and_not_a_second_copy() {
        assert_eq!(crate::intent::NODE_VERSION_LADDER, PATCHED_NODE_RELEASES);
        assert_eq!(crate::intent::SUPPORTED_NODE_RUNTIME, PATCHED_NODE_RELEASES);
        assert_eq!(crate::intent::DEFAULT_NODE, PATCHED_NODE_LOWEST);
        assert!(PATCHED_NODE_RELEASES.contains(&PATCHED_NODE_LOWEST));
        for (major, floor) in PATCHED_NODE_FLOORS {
            assert_eq!(
                node_runtime_verdict(floor),
                RuntimePolicyVerdict::Patched,
                "{major}.x floor must classify patched"
            );
        }
    }

    // ── the verdict ────────────────────────────────────────────────────────

    #[test]
    fn eol_and_pre_fix_releases_are_not_patched() {
        for version in [
            "18.20.4",
            "18.0.0",
            "20.20.2",
            "22.14.0",
            "22.23.3",
            "24.18.0",
            "24.22.0",
            "26.11.0",
            // A prerelease of the fixed version does not contain the fix.
            "22.23.4-rc.1",
        ] {
            assert_eq!(
                node_runtime_verdict(version),
                RuntimePolicyVerdict::NotPatched,
                "{version} must be affected"
            );
        }
    }

    #[test]
    fn releases_at_or_above_the_fix_floor_are_patched() {
        for version in [
            "22.23.4",
            "22.24.0",
            "v22.23.4",
            " 22.23.4 ",
            "24.22.1",
            "24.99.0",
            "26.12.1",
            "26.13.0",
        ] {
            assert_eq!(
                node_runtime_verdict(version),
                RuntimePolicyVerdict::Patched,
                "{version} must be patched"
            );
        }
    }

    #[test]
    fn a_declaration_this_build_cannot_classify_is_unverifiable() {
        for version in [
            "22", "22.14", ">=22", "^22.23.4", "22.x", "", "v",
            // EOL odd lines and future majors are not in the table; assuming
            // them safe would be a claim this build cannot make.
            "21.7.0", "23.11.0", "25.0.0", "28.0.0",
        ] {
            assert_eq!(
                node_runtime_verdict(version),
                RuntimePolicyVerdict::Unverifiable,
                "{version:?} must be unverifiable"
            );
        }
    }

    // ── the Derivation policy ──────────────────────────────────────────────

    #[test]
    fn a_route_without_a_node_runtime_is_admitted() {
        for runtimes in [
            serde_json::json!({}),
            serde_json::json!({"python": "3.12.7"}),
            serde_json::json!({"oci.image": "x@sha256:abc", "oci.platform": "linux/amd64"}),
        ] {
            assert!(
                derivation_runtime_policy(&derivation(runtimes, serde_json::json!({}))).is_ok()
            );
        }
    }

    #[test]
    fn a_route_wide_affected_node_is_refused_by_name() {
        let derivation = derivation(
            serde_json::json!({"python": "3.12.7", "node": "22.14.0"}),
            serde_json::json!({}),
        );
        let refusal = derivation_runtime_policy(&derivation).expect_err("refused");
        assert_eq!(refusal.code, RUNTIME_VERSION_NOT_PATCHED);
        assert!(refusal.message.contains("CVE-2026-111497"), "{refusal:?}");
        assert!(refusal.message.contains("22.14.0"), "{refusal:?}");
        assert!(refusal.message.contains("22.23.4"), "{refusal:?}");
    }

    #[test]
    fn a_step_scoped_affected_node_is_refused_too() {
        let derivation = derivation(
            serde_json::json!({"python": "3.12.7"}),
            serde_json::json!({"node": "24.18.0"}),
        );
        let refusal = derivation_runtime_policy(&derivation).expect_err("refused");
        assert_eq!(refusal.code, RUNTIME_VERSION_NOT_PATCHED);
        assert!(refusal.message.contains("24.18.0"), "{refusal:?}");
    }

    #[test]
    fn a_patched_route_is_admitted_even_with_step_scoped_runtimes() {
        let derivation = derivation(
            serde_json::json!({"node": "22.23.4"}),
            serde_json::json!({"node": "24.22.1"}),
        );
        assert!(derivation_runtime_policy(&derivation).is_ok());
    }

    #[test]
    fn an_unverifiable_declaration_is_refused_with_its_own_code() {
        let derivation = derivation(serde_json::json!({"node": ">=22"}), serde_json::json!({}));
        let refusal = derivation_runtime_policy(&derivation).expect_err("refused");
        assert_eq!(refusal.code, RUNTIME_VERSION_UNVERIFIABLE);
        assert!(refusal.message.contains("CVE-2026-111497"), "{refusal:?}");
    }

    // ── the retained toolchain binding ─────────────────────────────────────

    #[test]
    fn a_retained_toolchain_binding_is_checked_the_same_way() {
        let mut toolchains = BTreeMap::new();
        toolchains.insert("python".to_owned(), "3.12.7".to_owned());
        assert!(node_toolchain_policy(&toolchains).is_ok());

        toolchains.insert("node".to_owned(), "22.14.0".to_owned());
        let refusal = node_toolchain_policy(&toolchains).expect_err("refused");
        assert_eq!(refusal.code, RUNTIME_VERSION_NOT_PATCHED);

        toolchains.insert("node".to_owned(), "22.23.4".to_owned());
        assert!(node_toolchain_policy(&toolchains).is_ok());
    }
}
