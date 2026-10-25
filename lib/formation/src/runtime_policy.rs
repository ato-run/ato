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
use std::path::Path;

use crate::authoring::BoundDerivation;

/// The advisory this policy answers to. Not part of the error code: another
/// CVE in the same release range must not multiply the codes.
pub const NODE_CVE_2026_111497: &str = "CVE-2026-111497";

/// The runtime key a Node process route declares in `[[runtime]]`.
pub const NODE_RUNTIME: &str = "node";

/// Where provisioned Node toolchains live, with a trailing separator so a
/// bare root (`/opt/ato/toolchains/node`) is not mistaken for a release.
pub const NODE_TOOLCHAIN_PREFIX: &str = "/opt/ato/toolchains/node/";

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

    fn unproven_executable(executable: &Path) -> Self {
        Self {
            code: RUNTIME_VERSION_UNVERIFIABLE,
            message: format!(
                "the route declares a Node runtime but would execute {}: a Node binary outside \
                 {NODE_TOOLCHAIN_PREFIX}<patched release> cannot be proven to include the \
                 {NODE_CVE_2026_111497} fix (patched releases: {})",
                executable.display(),
                patched_releases_text()
            ),
        }
    }

    fn missing_executable(executable: &Path) -> Self {
        Self {
            code: RUNTIME_VERSION_UNVERIFIABLE,
            message: format!(
                "the route declares a Node runtime but its executable {} does not resolve; a \
                 Node this Runtime cannot inspect is not executed (patched releases: {})",
                executable.display(),
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
/// A declaration is not the only way a Derivation names Node. `argv` may be an
/// absolute `/opt/ato/toolchains/node/<release>/bin/node`, and an authored
/// `env.PATH` may put one first. Both are deterministic references to a
/// toolchain, so both are classified too: a Derivation that declares a patched
/// runtime must not execute an affected one through either.
///
/// A route with no Node runtime and no toolchain reference is admitted: it
/// cannot run Node at all.
pub fn derivation_runtime_policy(derivation: &BoundDerivation) -> Result<(), RuntimePolicyRefusal> {
    if let Some(version) = derivation.runtimes.get(NODE_RUNTIME) {
        check_node(version)?;
    }
    for step in &derivation.steps {
        if let Some(version) = step.runtimes.get(NODE_RUNTIME) {
            check_node(version)?;
        }
        for argument in &step.argv {
            string_runtime_policy(argument)?;
        }
        // Every value, not only PATH: an environment value that references an
        // affected toolchain is the same execution input, and scanning all of
        // them is the cheaper mistake.
        for value in step.env.values() {
            string_runtime_policy(value)?;
        }
    }
    Ok(())
}

/// Does this Derivation declare that it runs Node?
///
/// Used to decide whether executing a `node`-named binary outside a known
/// toolchain is "unverifiable" rather than "not this policy's business".
pub fn derivation_declares_node_runtime(derivation: &BoundDerivation) -> bool {
    derivation.runtimes.contains_key(NODE_RUNTIME)
        || derivation
            .steps
            .iter()
            .any(|step| step.runtimes.contains_key(NODE_RUNTIME))
}

/// The release named by the first `/opt/ato/toolchains/node/<release>` in
/// `value`, if any. The release segment ends at the next path separator,
/// whitespace, a PATH `:`, or a shell/JSON quote, so
/// `/opt/ato/toolchains/node/22.14.0/bin/node` yields `22.14.0`.
pub fn node_toolchain_version(value: &str) -> Option<&str> {
    let (index, _) = value.match_indices(NODE_TOOLCHAIN_PREFIX).next()?;
    let after = &value[index + NODE_TOOLCHAIN_PREFIX.len()..];
    let end = after
        .find(|character: char| {
            character == '/'
                || character == ':'
                || character.is_whitespace()
                || matches!(character, ';' | '"' | '\'' | '\0')
        })
        .unwrap_or(after.len());
    Some(&after[..end])
}

/// Classify every Node-toolchain reference inside one string.
///
/// A string may be a single path, a PATH list, or a shell-ish value; any
/// occurrence counts. An empty release segment (`/opt/ato/toolchains/node/`)
/// is unverifiable, and an affected one is refused, never skipped.
pub fn string_runtime_policy(value: &str) -> Result<(), RuntimePolicyRefusal> {
    for (index, _) in value.match_indices(NODE_TOOLCHAIN_PREFIX) {
        let after = &value[index + NODE_TOOLCHAIN_PREFIX.len()..];
        let end = after
            .find(|character: char| {
                character == '/'
                    || character == ':'
                    || character.is_whitespace()
                    || matches!(character, ';' | '"' | '\'' | '\0')
            })
            .unwrap_or(after.len());
        check_node(&after[..end])?;
    }
    Ok(())
}

/// Classify every value of an environment a process would be spawned with.
pub fn environment_runtime_policy(
    environment: &BTreeMap<String, String>,
) -> Result<(), RuntimePolicyRefusal> {
    for value in environment.values() {
        string_runtime_policy(value)?;
    }
    Ok(())
}

/// The policy for the executable a process launch would actually exec.
///
/// `lexical` is the path the spawner would try (absolute, or resolved against
/// PATH); `canonical` is that path with symlinks resolved, when it exists.
/// Both are checked: a symlink can point at an affected toolchain even when
/// the name written in the PATH entry looks innocent.
///
/// When a Node runtime is implied and the target is a `node`/`nodejs` binary,
/// it must live under a `/opt/ato/toolchains/node/<patched>` directory. A
/// `node` from anywhere else — `/usr/bin/node`, a workspace copy, a symlink to
/// either — is unverifiable, not patched.
pub fn executable_runtime_policy(
    lexical: &Path,
    canonical: Option<&Path>,
    node_implied: bool,
) -> Result<(), RuntimePolicyRefusal> {
    let lexical_text = lexical.to_string_lossy();
    string_runtime_policy(&lexical_text)?;
    if let Some(canonical) = canonical {
        string_runtime_policy(&canonical.to_string_lossy())?;
    }
    if !node_implied {
        return Ok(());
    }
    let effective = canonical.unwrap_or(lexical);
    let file_name = effective.file_name().and_then(|name| name.to_str());
    if !matches!(file_name, Some("node" | "nodejs")) {
        return Ok(());
    }
    if canonical.is_none() {
        return Err(RuntimePolicyRefusal::missing_executable(lexical));
    }
    if node_toolchain_version(&effective.to_string_lossy()).is_none() {
        return Err(RuntimePolicyRefusal::unproven_executable(effective));
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

    // ── toolchain references in argv / PATH / the executable ───────────────

    fn route_with_step(step: serde_json::Value) -> BoundDerivation {
        serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": {"node": "22.23.4"}, "steps": [step],
            "ports": [], "state": [], "effects": "pure"
        }))
        .expect("a derivation with one step")
    }

    fn serve_step(extra: serde_json::Value) -> serde_json::Value {
        let mut step = serde_json::json!({
            "id": "app", "protocol": "ato.process@1", "op": "serve", "argv": ["node", "app.js"]
        });
        if let Some(map) = extra.as_object() {
            for (key, value) in map {
                step[key] = value.clone();
            }
        }
        step
    }

    #[test]
    fn a_toolchain_reference_in_argv_is_classified() {
        let affected = route_with_step(serve_step(serde_json::json!({
            "argv": ["/opt/ato/toolchains/node/22.14.0/bin/node", "app.js"]
        })));
        let refusal = derivation_runtime_policy(&affected).expect_err("affected argv");
        assert_eq!(refusal.code, RUNTIME_VERSION_NOT_PATCHED);
        assert!(refusal.message.contains("22.14.0"), "{refusal:?}");

        let patched = route_with_step(serve_step(serde_json::json!({
            "argv": ["/opt/ato/toolchains/node/22.23.4/bin/node", "app.js"]
        })));
        assert!(derivation_runtime_policy(&patched).is_ok());

        let unverifiable = route_with_step(serve_step(serde_json::json!({
            "argv": ["/opt/ato/toolchains/node/22/bin/node", "app.js"]
        })));
        assert_eq!(
            derivation_runtime_policy(&unverifiable)
                .expect_err("unverifiable argv")
                .code,
            RUNTIME_VERSION_UNVERIFIABLE
        );
    }

    #[test]
    fn a_toolchain_reference_in_an_authored_path_is_classified() {
        let affected = route_with_step(serve_step(serde_json::json!({
            "env": {
                "PATH": "/opt/ato/toolchains/node/22.14.0/bin:/usr/local/bin:/usr/bin:/bin"
            }
        })));
        assert_eq!(
            derivation_runtime_policy(&affected)
                .expect_err("affected PATH")
                .code,
            RUNTIME_VERSION_NOT_PATCHED
        );

        let patched = route_with_step(serve_step(serde_json::json!({
            "env": {
                "PATH": "/opt/ato/toolchains/node/22.23.4/bin:/usr/local/bin:/usr/bin:/bin"
            }
        })));
        assert!(derivation_runtime_policy(&patched).is_ok());

        // A non-PATH variable is the same execution input.
        let in_other_value = route_with_step(serve_step(serde_json::json!({
            "env": {"NODE_HOME": "/opt/ato/toolchains/node/20.20.2"}
        })));
        assert_eq!(
            derivation_runtime_policy(&in_other_value)
                .expect_err("affected env value")
                .code,
            RUNTIME_VERSION_NOT_PATCHED
        );
    }

    #[test]
    fn a_toolchain_reference_is_a_declaration_too() {
        // No `node` runtime key at all, only the absolute reference: the gate
        // must still see what would actually run.
        let derivation: BoundDerivation = serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": {"python": "3.12.7"},
            "steps": [{
                "id": "app", "protocol": "ato.process@1", "op": "serve",
                "argv": ["/opt/ato/toolchains/node/18.20.4/bin/node", "app.js"]
            }],
            "ports": [], "state": [], "effects": "pure"
        }))
        .unwrap();
        assert_eq!(
            derivation_runtime_policy(&derivation)
                .expect_err("undeclared affected Node")
                .code,
            RUNTIME_VERSION_NOT_PATCHED
        );
        assert!(!derivation_declares_node_runtime(&derivation));
    }

    #[test]
    fn the_release_segment_stops_at_a_separator_or_a_colon() {
        assert_eq!(
            node_toolchain_version("/opt/ato/toolchains/node/22.14.0/bin/node"),
            Some("22.14.0")
        );
        assert_eq!(
            node_toolchain_version("PATH=/opt/ato/toolchains/node/v22.23.4/bin:/usr/bin"),
            Some("v22.23.4")
        );
        assert_eq!(
            node_toolchain_version("/opt/ato/toolchains/node/22.14.0"),
            Some("22.14.0")
        );
        assert_eq!(
            node_toolchain_version("/opt/ato/toolchains/node/"),
            Some("")
        );
        assert_eq!(node_toolchain_version("/usr/bin/node"), None);
        // The bare root is not a release reference.
        assert_eq!(node_toolchain_version("/opt/ato/toolchains/node"), None);
    }

    #[test]
    fn an_environment_is_scanned_value_by_value() {
        let mut environment = BTreeMap::new();
        environment.insert(
            "PATH".to_owned(),
            "/opt/ato/toolchains/python/3.12.7/bin".to_owned(),
        );
        assert!(environment_runtime_policy(&environment).is_ok());
        environment.insert(
            "PATH".to_owned(),
            "/opt/ato/toolchains/node/24.18.0/bin:/usr/bin".to_owned(),
        );
        assert_eq!(
            environment_runtime_policy(&environment)
                .expect_err("affected PATH")
                .code,
            RUNTIME_VERSION_NOT_PATCHED
        );
    }

    #[test]
    fn a_node_outside_the_toolchain_cannot_prove_patched() {
        // The normal case: a patched toolchain path is fine.
        assert!(
            executable_runtime_policy(
                Path::new("/opt/ato/toolchains/node/22.23.4/bin/node"),
                Some(Path::new("/opt/ato/toolchains/node/22.23.4/bin/node")),
                true
            )
            .is_ok()
        );
        // An affected path is refused even when a symlink hides it.
        assert_eq!(
            executable_runtime_policy(
                Path::new("/usr/local/bin/node"),
                Some(Path::new("/opt/ato/toolchains/node/22.14.0/bin/node")),
                true
            )
            .expect_err("symlink to affected")
            .code,
            RUNTIME_VERSION_NOT_PATCHED
        );
        // A system Node, or a workspace copy, cannot be proven patched.
        assert_eq!(
            executable_runtime_policy(
                Path::new("/usr/bin/node"),
                Some(Path::new("/usr/bin/node")),
                true
            )
            .expect_err("system node")
            .code,
            RUNTIME_VERSION_UNVERIFIABLE
        );
        // A missing executable the spawner would try is fail-closed.
        assert_eq!(
            executable_runtime_policy(Path::new("node"), None, true)
                .expect_err("missing node")
                .code,
            RUNTIME_VERSION_UNVERIFIABLE
        );
        // No Node is implied: a non-node executable is not this policy's.
        assert!(
            executable_runtime_policy(
                Path::new("/usr/bin/python3"),
                Some(Path::new("/usr/bin/python3")),
                false
            )
            .is_ok()
        );
        // A non-node basename under a Node toolchain is still classified by
        // path (here: patched), and an affected one is refused.
        assert!(
            executable_runtime_policy(
                Path::new("/opt/ato/toolchains/node/22.23.4/bin/npm"),
                None,
                true
            )
            .is_ok()
        );
        assert_eq!(
            executable_runtime_policy(
                Path::new("/opt/ato/toolchains/node/22.14.0/bin/npm"),
                None,
                true
            )
            .expect_err("affected npm")
            .code,
            RUNTIME_VERSION_NOT_PATCHED
        );
    }
}
