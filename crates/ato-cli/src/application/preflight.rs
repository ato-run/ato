//! #117 — eager pre-launch requirement collector.
//!
//! Walks an orchestration capsule's full target graph, derives an
//! ExecutionPlan per service target without running any provisioning
//! side effects (no `uv venv`, no `npm install`, no postgres provider
//! startup), checks consent state per plan, and inspects each target's
//! `required_env` (plus the manifest top-level `required_env`) for
//! missing values.
//!
//! Returns a single aggregate envelope listing every pending
//! [`InteractiveResolutionEnvelope`] (the typed shape established by
//! issues #96 / #126 / #135 / #139) so a UI shell — today
//! ato-desktop — can render one resolution modal containing all
//! per-target consents and missing-env rows at once. The caller (the
//! `ato internal preflight` plumbing command) serializes this output
//! to JSON for the desktop's launch worker to consume.
//!
//! ## Why this is side-effect-free
//!
//! Every API used here is a pure manifest computation:
//!
//! - [`capsule_core::execution_plan::derive::compile_execution_plan`]
//!   only loads the manifest, applies routing logic, and constructs an
//!   `ExecutionPlan` value. It does not spawn subprocesses, write
//!   files, or contact registries.
//! - [`capsule_core::router::ManifestData::services`] reads the
//!   `[services]` table and returns target labels.
//! - [`crate::application::auth::consent_store::has_consent`] reads
//!   the JSONL consent log under `${ATO_HOME}/consent/`; it never
//!   writes.
//!
//! So calling this collector before the launch loop's provisioning
//! phase is safe and observably side-effect-free.

#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use capsule_core::execution_plan::derive::compile_execution_plan;
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::interactive_resolution::{
    InteractiveResolutionEnvelope, InteractiveResolutionKind, ResolutionDisplay,
};
use capsule_core::lockfile::manifest_external_capsule_dependencies;
use capsule_core::router::ExecutionProfile;
use capsule_core::types::{ConfigField, ConfigKind};

use crate::application::auth::consent_store::{consent_summary, has_consent};
use crate::application::graph_views::{build_declared_only_bundle, PreflightView};

/// Top-level result emitted by the collector.
///
/// `requirements` is `Vec` (not `BTreeMap`) to preserve the
/// caller-meaningful ordering: top-level / global env first, then
/// per-target items in `[services]` order. Desktop renders sections in
/// the same order, so the user reads a stable layout across runs.
#[derive(Debug, Clone, Serialize)]
pub struct AggregatePreflightResult {
    /// Capsule identity scraped from the manifest. Pre-rendered so the
    /// caller can title the modal without re-parsing.
    pub capsule_id: String,
    pub capsule_version: String,
    /// Target labels visited during the walk. Useful for UI display
    /// and for harness consistency checks ("we expected to see N
    /// targets").
    pub visited_targets: Vec<String>,
    /// Pending requirements aggregated across every visited target.
    /// Empty list means "the launch can proceed without further user
    /// interaction" — the caller should drop the modal and start
    /// `ato run`.
    pub requirements: Vec<InteractiveResolutionEnvelope>,
}

impl AggregatePreflightResult {
    pub fn is_empty(&self) -> bool {
        self.requirements.is_empty()
    }
}

/// Errors specific to the preflight collector. Anything that prevents
/// the walk from producing a complete answer becomes one of these.
/// Manifest-load failures are surfaced as-is (so the caller can
/// distinguish "this isn't a capsule path" from "consent store is
/// broken").
#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("manifest path does not exist: {path}")]
    ManifestMissing { path: PathBuf },

    #[error("failed to load capsule manifest at {path}: {source}")]
    ManifestLoad {
        path: PathBuf,
        source: capsule_core::error::CapsuleError,
    },

    #[error("execution plan derivation failed for target '{target}': {source}")]
    ExecutionPlan {
        target: String,
        source: AtoExecutionError,
    },

    #[error("consent store lookup failed: {source}")]
    ConsentStore { source: AtoExecutionError },
}

/// Walk a local capsule path and collect every pending pre-launch
/// requirement. The `target` argument matches the same input shape
/// `ato run` and `ato inspect requirements` accept — a directory, a
/// `capsule.toml` path, or (in the future) a registry ref. This slice
/// supports local paths only; remote-resolution support tracks #117's
/// continuation.
///
/// `profile` is forwarded to `compile_execution_plan` so the caller
/// can choose Dev (default) vs Prod policy. The desktop launch worker
/// passes Dev.
pub fn collect_aggregate_requirements(
    target: &str,
    profile: ExecutionProfile,
) -> Result<AggregatePreflightResult, PreflightError> {
    let manifest_path = resolve_local_manifest_path(target)?;

    let loaded =
        capsule_core::contract::manifest::load_manifest(&manifest_path).map_err(|err| {
            PreflightError::ManifestLoad {
                path: manifest_path.clone(),
                source: err,
            }
        })?;
    let manifest = &loaded.model;

    let capsule_id = manifest.name.clone();
    let capsule_version = manifest.version.clone();

    // 1. Derive the orchestration target list. Single-target capsules
    //    fall back to `default_target` (or the routing layer's
    //    selection logic) so they degrade to the existing simple
    //    one-requirement flow without special-casing here.
    let target_labels = derive_target_labels(&manifest_path, profile)?;

    let mut requirements: Vec<InteractiveResolutionEnvelope> = Vec::new();

    // PR-3c: build a declared-only LaunchGraphBundle from the manifest
    // facts the preflight collector needs (dependency aliases for the
    // per-target walk, top-level required_env for the global block).
    // PreflightView::from_bundle is the source-of-truth surface for
    // those facts — the legacy direct manifest reads
    // (collect_global_required_env / manifest.services) are kept for
    // debug-mode parity guards so drift surfaces immediately.
    let manifest_dependencies = manifest_external_capsule_dependencies(
        &toml::Value::try_from(manifest).unwrap_or(toml::Value::Table(Default::default())),
    )
    .unwrap_or_default();
    let preflight_bundle = build_declared_only_bundle(
        &manifest_dependencies,
        Some(manifest_path.display().to_string()),
        None,
        collect_global_required_env(manifest),
    );
    let preflight_view = PreflightView::from_bundle(&preflight_bundle);

    // 2. Top-level required_env is the dep-contract resolution scope
    //    (per the manifest's own RFC §5.2 comment). For WasedaP2P this
    //    is where `PG_PASSWORD` lives — it feeds the postgres
    //    dep-contract's `credentials.password = "{env.PG_PASSWORD}"`
    //    substitution. We surface it as a SecretsRequired envelope
    //    with `target = None` so the modal can group it under a
    //    "global" header rather than misattribute it to a single
    //    target.
    let mut global_env_seen: BTreeSet<String> = BTreeSet::new();
    // PR-3c: bundle-derived view is the primary; debug-mode parity
    // pins it against the legacy direct manifest read.
    let global_required_env = preflight_view.required_env.clone();
    debug_assert_eq!(
        sorted_dedup(global_required_env.clone()),
        sorted_dedup(collect_global_required_env(manifest)),
        "PR-3c: bundle-derived required_env drifted from manifest.required_env"
    );
    if !global_required_env.is_empty() {
        let fields: Vec<ConfigField> = global_required_env
            .iter()
            .map(|name| {
                global_env_seen.insert(name.clone());
                config_field_for_env(name, "Global dependency contract environment variable")
            })
            .collect();
        requirements.push(InteractiveResolutionEnvelope {
            kind: InteractiveResolutionKind::SecretsRequired {
                target: None,
                schema: fields,
            },
            display: ResolutionDisplay {
                message: format!(
                    "Provide {} required environment variable{} before launching {capsule_id}.",
                    global_required_env.len(),
                    if global_required_env.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                hint: Some(
                    "Set these via the launching app's secret form or the shell environment."
                        .to_string(),
                ),
            },
        });
    }

    // 3. Per-target walk. For each target_label we (a) compile the
    //    ExecutionPlan to get the consent identity tuple + summary,
    //    (b) consult the consent store to decide whether to surface
    //    the consent envelope, (c) read the target's `required_env`
    //    and emit a SecretsRequired envelope for any keys not already
    //    surfaced as global.
    for target_label in &target_labels {
        let compiled = compile_execution_plan(&manifest_path, profile, Some(target_label.as_str()))
            .map_err(|err| PreflightError::ExecutionPlan {
                target: target_label.clone(),
                source: err,
            })?;
        let plan = compiled.execution_plan;

        // 3a. Per-target required_env. Any env keys already covered by
        //     the global block are skipped to avoid asking the user
        //     for the same value twice. Keys here are typically secret
        //     (e.g. `SECRET_KEY` for target=app on WasedaP2P), so we
        //     mark them `ConfigKind::Secret` to drive the masked input
        //     in the resolution modal.
        let target_required_env = collect_target_required_env(manifest, target_label);
        let target_specific: Vec<String> = target_required_env
            .into_iter()
            .filter(|key| !global_env_seen.contains(key))
            .collect();
        if !target_specific.is_empty() {
            let fields: Vec<ConfigField> = target_specific
                .iter()
                .map(|name| {
                    config_field_for_env(name, &format!("Required by target '{target_label}'"))
                })
                .collect();
            requirements.push(InteractiveResolutionEnvelope {
                kind: InteractiveResolutionKind::SecretsRequired {
                    target: Some(target_label.clone()),
                    schema: fields,
                },
                display: ResolutionDisplay {
                    message: format!(
                        "Provide {} value{} for target '{target_label}'.",
                        target_specific.len(),
                        if target_specific.len() == 1 { "" } else { "s" }
                    ),
                    hint: None,
                },
            });
        }

        // 3b. Consent. Skip if already recorded — the launch loop
        //     would skip this target's consent prompt too, so the
        //     aggregate envelope must match.
        let already_consented =
            has_consent(&plan).map_err(|err| PreflightError::ConsentStore { source: err })?;
        if !already_consented {
            requirements.push(InteractiveResolutionEnvelope {
                kind: InteractiveResolutionKind::ConsentRequired {
                    scoped_id: plan.consent.key.scoped_id.clone(),
                    version: plan.consent.key.version.clone(),
                    target_label: plan.consent.key.target_label.clone(),
                    policy_segment_hash: plan.consent.policy_segment_hash.clone(),
                    provisioning_policy_hash: plan.consent.provisioning_policy_hash.clone(),
                    summary: consent_summary(&plan),
                },
                display: ResolutionDisplay {
                    message: format!(
                        "Approve ExecutionPlan for target '{target_label}' of \
                         {}@{}.",
                        plan.consent.key.scoped_id, plan.consent.key.version
                    ),
                    hint: Some(
                        "Network / filesystem / secret policy summary follows. \
                         Approve once to record consent."
                            .to_string(),
                    ),
                },
            });
        }
    }

    Ok(AggregatePreflightResult {
        capsule_id,
        capsule_version,
        visited_targets: target_labels,
        requirements,
    })
}

/// Resolve `target` (a directory, a `capsule.toml` path, or a
/// GitHub-flavoured `capsule://github.com/<owner>/<repo>` URL) to an
/// absolute `capsule.toml` location. Returns
/// [`PreflightError::ManifestMissing`] when no manifest is reachable.
///
/// Resolution policy:
///
/// 1. **`capsule://github.com/<owner>/<repo>`**: look for a previously
///    fetched working tree under `${ATO_HOME}/tmp/gh-run/<repo>-*` and
///    `${ATO_HOME}/external-capsules/github/<owner>/<repo>/*`,
///    in that order. Use the most recently modified hit. This works
///    only if `ato run`/`ato-desktop` has already cached the capsule
///    once before — first-time fetching is intentionally out of scope
///    for this slice (avoiding new network/git side effects in the
///    preflight path is what makes preflight safe).
/// 2. **Local directory**: append `capsule.toml`.
/// 3. **Local file**: use as-is.
/// 4. **`publisher/slug` registry refs**: not supported in this slice
///    — registry resolution would re-introduce network side effects.
///    The caller should fall back to the legacy E103/E302 flow.
fn resolve_local_manifest_path(target: &str) -> Result<PathBuf, PreflightError> {
    if let Some(rest) = target.strip_prefix("capsule://github.com/") {
        return resolve_cached_github_capsule(rest);
    }
    let expanded = crate::local_input::expand_local_path(target);
    if !expanded.exists() {
        return Err(PreflightError::ManifestMissing {
            path: expanded.clone(),
        });
    }
    let manifest = if expanded.is_dir() {
        expanded.join("capsule.toml")
    } else {
        expanded
    };
    if !manifest.exists() {
        return Err(PreflightError::ManifestMissing { path: manifest });
    }
    Ok(manifest)
}

/// Resolve a `capsule://github.com/<owner>/<repo>` ref to a cached
/// working tree under `${ATO_HOME}/...`. Returns the most recently
/// modified hit so a user who's iterating on a PR sees the latest
/// cached snapshot. This intentionally never fetches over the network
/// — preflight must stay side-effect-free.
fn resolve_cached_github_capsule(rest: &str) -> Result<PathBuf, PreflightError> {
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err(PreflightError::ManifestMissing {
            path: PathBuf::from(format!("capsule://github.com/{rest}")),
        });
    }
    let owner = parts[0];
    let repo = parts[1];
    let ato_home = capsule_core::common::paths::nacelle_home_dir_or_workspace_tmp();

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Working trees fetched by `ato run` for the launch attempts the
    // user has already made. These live under `~/.ato/tmp/gh-run/<repo>-*`
    // (note: the prefix is the bare repo name, not owner/repo).
    let gh_run_root = ato_home.join("tmp").join("gh-run");
    if let Ok(entries) = std::fs::read_dir(&gh_run_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("{repo}-")) {
                candidates.push(entry.path());
            }
        }
    }

    // The publisher-scoped external-capsule cache. Layout:
    // `${ATO_HOME}/external-capsules/github/<owner>/<repo>/<commit>/`.
    let external_root = ato_home
        .join("external-capsules")
        .join("github")
        .join(owner)
        .join(repo);
    if let Ok(entries) = std::fs::read_dir(&external_root) {
        for entry in entries.flatten() {
            candidates.push(entry.path());
        }
    }

    // Most recently modified wins — recent fetches are the most
    // likely match for the user's current intent. If two candidates
    // have the same mtime we deterministically prefer the lexically
    // greater path so repeated runs are reproducible.
    candidates.sort_by(|a, b| {
        let a_mtime = a.metadata().and_then(|m| m.modified()).ok();
        let b_mtime = b.metadata().and_then(|m| m.modified()).ok();
        b_mtime.cmp(&a_mtime).then_with(|| b.cmp(a))
    });

    for candidate in candidates {
        let manifest = candidate.join("capsule.toml");
        if manifest.exists() {
            return Ok(manifest);
        }
    }

    Err(PreflightError::ManifestMissing {
        path: external_root.join("capsule.toml"),
    })
}

/// Walk the orchestration `[services]` table to extract every distinct
/// target label, in service-declaration order. For non-orchestration
/// capsules (no `[services]`) we fall back to the manifest's
/// `default_target` if present, then to a single-element vector with
/// the routing layer's selected target.
fn derive_target_labels(
    manifest_path: &Path,
    profile: ExecutionProfile,
) -> Result<Vec<String>, PreflightError> {
    // Use the routing layer to load the manifest's ExecutionDescriptor
    // (the same value `compile_execution_plan` uses internally), then
    // ask it for the `[services]` table. This is the same call site
    // the CLI's run pipeline uses for its own orchestration walk; no
    // provisioning side effects.
    let decision =
        capsule_core::router::route_manifest(manifest_path, profile, None).map_err(|err| {
            PreflightError::ExecutionPlan {
                target: "<resolution>".to_string(),
                source: AtoExecutionError::policy_violation(format!(
                    "failed to route manifest for preflight: {err}"
                )),
            }
        })?;
    let services = decision.plan.services();
    if services.is_empty() {
        // Single-target capsule. Use the routing layer's selected
        // target so the existing default_target / first-target
        // selection logic is honored — keeps the simple-flow degrade
        // requirement intact.
        return Ok(vec![decision.plan.selected_target_label().to_string()]);
    }

    // Sort by service name for stable output. The launch loop runs in
    // dependency-resolved (topological) order, but for the user-facing
    // aggregate envelope alphabetical-by-service is the easier mental
    // model — and the consent identity tuples don't depend on
    // ordering.
    let mut entries: Vec<(String, String)> = services
        .iter()
        .filter_map(|(service_name, spec)| {
            spec.target
                .as_ref()
                .map(|t| (service_name.clone(), t.clone()))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for (_, target) in entries {
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

fn collect_global_required_env(manifest: &capsule_core::types::CapsuleManifest) -> Vec<String> {
    manifest.required_env.clone()
}

/// Stable-order helper for the PR-3c parity guards.
fn sorted_dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn collect_target_required_env(
    manifest: &capsule_core::types::CapsuleManifest,
    target_label: &str,
) -> Vec<String> {
    manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target(target_label))
        .map(|target| target.required_env.clone())
        .unwrap_or_default()
}

fn config_field_for_env(name: &str, description: &str) -> ConfigField {
    ConfigField {
        name: name.to_string(),
        label: Some(name.to_string()),
        description: Some(description.to_string()),
        kind: ConfigKind::Secret,
        default: None,
        placeholder: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Writes a multi-target capsule.toml that mimics the shape of
    /// `Koh0920/WasedaP2P` (top-level `required_env`, two targets via
    /// `[services]`, per-target `required_env`). Used to assert the
    /// collector emits one envelope per missing thing rather than
    /// surfacing them sequentially.
    fn write_multi_target_fixture(dir: &Path) -> PathBuf {
        // The `[network] egress_allow = ...` block matches what
        // WasedaP2P does: each target ends up with a non-empty
        // `runtime.policy.network.allow_hosts` once the routing layer
        // applies the top-level network rules. Without this, both
        // targets would be classified as zero-permission plans and the
        // consent store auto-consents them — masking the bug we're
        // testing for here.
        let manifest = r#"
schema_version = "0.3"
name           = "preflight-test-app"
version        = "0.1.0"
type           = "app"
default_target = "app"

required_env = ["PG_PASSWORD"]

[targets.app]
runtime = "source/python"
working_dir = "."
run = "python -m app"
required_env = ["SECRET_KEY"]

[targets.web]
runtime = "source/node"
working_dir = "."
run = "node web.js"
port = 5173

[services.main]
target = "app"

[services.web]
target = "web"
depends_on = ["main"]

[network]
egress_allow = ["smtp.gmail.com"]
"#;
        let path = dir.join("capsule.toml");
        fs::write(&path, manifest).expect("write manifest");
        path
    }

    /// The collector must visit BOTH targets and return ONE
    /// aggregate envelope rather than emitting them serially via
    /// E103/E302 errors.
    ///
    /// Uses an isolated `ATO_HOME` so the user's real consent log
    /// can't influence the assertion (a previously-approved
    /// `preflight-test-app` would otherwise hide the consent
    /// requirements).
    #[test]
    #[serial_test::serial]
    fn aggregates_secrets_and_consents_for_orchestration_capsule() {
        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = write_multi_target_fixture(manifest_dir.path());
        let target_str = manifest_path.to_string_lossy().to_string();

        let result =
            collect_aggregate_requirements(&target_str, ExecutionProfile::Dev).expect("collect");

        // Two targets visited in service-name order (main → web).
        assert_eq!(result.visited_targets, vec!["app", "web"]);

        // The collector must NOT emit a separate envelope per
        // requirement — the contract is one aggregate result that
        // carries the whole list. We assert the list length and
        // contents instead of routing on individual top-level fields.
        let kinds: Vec<&str> = result
            .requirements
            .iter()
            .map(|env| match &env.kind {
                InteractiveResolutionKind::SecretsRequired { target, .. } => match target {
                    Some(t) => t.as_str(),
                    None => "<global-secrets>",
                },
                InteractiveResolutionKind::ConsentRequired { target_label, .. } => {
                    target_label.as_str()
                }
            })
            .collect();
        // Expected: global PG_PASSWORD + target=app SECRET_KEY +
        // target=app consent + target=web consent.
        assert!(
            kinds.contains(&"<global-secrets>"),
            "global secret bucket missing; got kinds={kinds:?}"
        );
        assert!(
            kinds.iter().filter(|k| **k == "app").count() >= 2,
            "expected at least two app entries (secrets + consent); got kinds={kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| *k == "web"),
            "expected web consent entry; got kinds={kinds:?}"
        );
    }

    /// Target identity tuple round-trips through the envelope so the
    /// caller can feed the values straight into
    /// `ato internal consent approve-execution-plan`. Locks the wire
    /// shape regression.
    #[test]
    #[serial_test::serial]
    fn consent_envelope_carries_identity_tuple_for_each_target() {
        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = write_multi_target_fixture(manifest_dir.path());
        let target_str = manifest_path.to_string_lossy().to_string();

        let result =
            collect_aggregate_requirements(&target_str, ExecutionProfile::Dev).expect("collect");

        for envelope in &result.requirements {
            if let InteractiveResolutionKind::ConsentRequired {
                scoped_id,
                version,
                target_label,
                policy_segment_hash,
                provisioning_policy_hash,
                summary,
            } = &envelope.kind
            {
                assert!(!scoped_id.is_empty(), "scoped_id missing");
                assert!(!version.is_empty(), "version missing");
                assert!(!target_label.is_empty(), "target_label missing");
                assert!(
                    policy_segment_hash.starts_with("blake3:"),
                    "policy_segment_hash must be blake3-prefixed: {policy_segment_hash}"
                );
                assert!(
                    provisioning_policy_hash.starts_with("blake3:"),
                    "provisioning_policy_hash must be blake3-prefixed: \
                     {provisioning_policy_hash}"
                );
                assert!(!summary.is_empty(), "summary must be pre-rendered");
            }
        }
    }

    /// Single-target capsules (no `[services]`) must degrade
    /// gracefully — the collector still returns a result, with
    /// `visited_targets` containing just the selected target. Locks
    /// the "do not regress single-target capsules" requirement from
    /// the spec.
    #[test]
    #[serial_test::serial]
    fn single_target_capsule_degrades_to_one_target_walk() {
        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = manifest_dir.path().join("capsule.toml");
        let manifest = r#"
schema_version = "0.3"
name           = "single-target-test"
version        = "0.1.0"
type           = "app"
default_target = "cli"

[targets.cli]
runtime = "source/python"
working_dir = "."
run = "python -m app"
"#;
        fs::write(&manifest_path, manifest).expect("write");

        let result =
            collect_aggregate_requirements(&manifest_path.to_string_lossy(), ExecutionProfile::Dev)
                .expect("collect");

        assert_eq!(result.visited_targets.len(), 1);
        assert_eq!(result.visited_targets[0], "cli");
    }

    /// RAII env-var scope guard. The `std::env` API is process-global
    /// and unsafe across threads; the tests run with
    /// `#[serial_test::serial]` so the guard's lifetime defines the
    /// observable scope.
    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    fn scoped_env(key: &'static str, value: Option<&str>) -> EnvGuard {
        let previous = std::env::var_os(key);
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        EnvGuard { key, previous }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
