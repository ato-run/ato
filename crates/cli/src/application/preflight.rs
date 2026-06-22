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
//! - [`capsule::execution_plan::derive::compile_execution_plan`]
//!   only loads the manifest, applies routing logic, and constructs an
//!   `ExecutionPlan` value. It does not spawn subprocesses, write
//!   files, or contact registries.
//! - [`capsule::router::ManifestData::services`] reads the
//!   `[services]` table and returns target labels.
//! - [`crate::application::auth::consent_store::has_consent`] reads
//!   the JSONL consent log under `${ATO_HOME}/consent/`; it never
//!   writes.
//!
//! So calling this collector before the launch loop's provisioning
//! phase is safe and observably side-effect-free.
//!
//! The exception is [`preflight_oci_provider_readiness`], which is used by
//! the actual OCI launch path and may call `ensure_ready()`. On macOS/Windows
//! that can auto-start a single stopped Podman machine before launch.

#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::runtime::oci_provider::{
    OciProvider, OciProviderError, OciProviderProbe, OciProviderSelector,
};
use capsule::execution_plan::derive::compile_execution_plan;
use capsule::execution_plan::error::AtoExecutionError;
use capsule::interactive_resolution::{
    InteractiveResolutionEnvelope, InteractiveResolutionKind, ResolutionDisplay,
};
use capsule::lockfile::manifest_external_capsule_dependencies;
use capsule::router::ExecutionProfile;
use capsule::types::{ConfigField, ConfigKind, OciProviderKind, OciProviderMode, StateAttach};

use crate::app_control::sample_recipes::{
    resolve_sample_recipe_for_github, resolve_sample_recipe_for_input,
};
use crate::application::auth::consent_store::{consent_summary, has_consent};
use crate::application::graph_views::{PreflightView, build_declared_only_bundle};

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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciProviderReadinessMode {
    Required,
    BestEffort,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OciProviderReadinessRequirements {
    pub rootless: OciRootlessRequirement,
}

impl Default for OciProviderReadinessRequirements {
    fn default() -> Self {
        Self {
            rootless: OciRootlessRequirement::Any,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciRootlessRequirement {
    Any,
    Required,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OciProviderReadinessOutcome {
    Ready(OciProviderProbe),
    NotReady(OciProviderError),
}

#[allow(dead_code)]
pub(crate) async fn preflight_oci_provider_readiness<S>(
    selector: &S,
    mode: OciProviderReadinessMode,
    requirements: OciProviderReadinessRequirements,
) -> Result<OciProviderReadinessOutcome, OciProviderError>
where
    S: OciProviderSelector,
{
    let provider = selector.select_provider();
    // Auto-start a stopped machine (macOS/Windows) or verify binary is present (Linux).
    // Any ensure_ready failure is treated the same as a not-ready probe result so that
    // BestEffort callers still get OciProviderReadinessOutcome::NotReady rather than Err.
    match provider.ensure_ready().await {
        Err(err) => oci_provider_readiness_failure(mode, err),
        Ok(()) => evaluate_oci_provider_readiness(provider.probe().await, mode, requirements),
    }
}

fn evaluate_oci_provider_readiness(
    probe: Result<OciProviderProbe, OciProviderError>,
    mode: OciProviderReadinessMode,
    requirements: OciProviderReadinessRequirements,
) -> Result<OciProviderReadinessOutcome, OciProviderError> {
    let probe = match probe {
        Ok(probe) => probe,
        Err(error) => return oci_provider_readiness_failure(mode, error),
    };

    if !probe.ready {
        let error = probe
            .require_ready()
            .expect_err("non-ready OCI provider probe must produce a typed readiness error");
        return oci_provider_readiness_failure(mode, error);
    }

    if let Err(error) = validate_oci_provider_readiness_requirements(&probe, requirements) {
        return oci_provider_readiness_failure(mode, error);
    }

    Ok(OciProviderReadinessOutcome::Ready(probe))
}

fn oci_provider_readiness_failure(
    mode: OciProviderReadinessMode,
    error: OciProviderError,
) -> Result<OciProviderReadinessOutcome, OciProviderError> {
    match mode {
        OciProviderReadinessMode::Required => Err(error),
        OciProviderReadinessMode::BestEffort => Ok(OciProviderReadinessOutcome::NotReady(error)),
    }
}

fn validate_oci_provider_readiness_requirements(
    probe: &OciProviderProbe,
    requirements: OciProviderReadinessRequirements,
) -> Result<(), OciProviderError> {
    if requirements.rootless == OciRootlessRequirement::Required
        && probe.inventory.mode != OciProviderMode::Rootless
    {
        return Err(OciProviderError::CapabilityUnsupported {
            provider: oci_provider_name(probe.inventory.kind),
            capability: "rootless",
            detected: format!("{:?}", probe.inventory.mode),
        });
    }

    Ok(())
}

fn oci_provider_name(kind: OciProviderKind) -> &'static str {
    match kind {
        OciProviderKind::Podman => "podman",
        OciProviderKind::DockerCompatible => "docker-compatible",
        OciProviderKind::AtoNative => "ato-native",
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

    #[error("unsupported preflight target '{input}': {reason}")]
    UnsupportedTarget { input: String, reason: String },

    #[error("failed to materialize bundled sample recipe for '{input}': {reason}")]
    SampleRecipeMaterialize { input: String, reason: String },

    #[error("failed to load capsule manifest at {path}: {source}")]
    ManifestLoad {
        path: PathBuf,
        source: capsule::error::CapsuleError,
    },

    #[error("execution plan derivation failed for target '{target}': {source}")]
    ExecutionPlan {
        target: String,
        source: AtoExecutionError,
    },

    #[error("consent store lookup failed: {source}")]
    ConsentStore { source: AtoExecutionError },

    /// PR-3c (PR #180 review fix): the raw manifest TOML could not be
    /// projected into the bundle's dependency list. Surfaces as a hard
    /// error instead of being silently swallowed — the bundle-derived
    /// preflight view is now load-bearing for the global required_env
    /// block, so a parse failure here would otherwise produce an empty
    /// `[dependencies.*]` projection and silently skip dep-driven
    /// envelopes.
    #[error("failed to derive external dependencies from raw manifest at {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        source: capsule::error::CapsuleError,
    },
}

/// Walk an offline-resolved capsule path and collect every pending
/// pre-launch requirement. The supported target shapes are:
///
/// - a local capsule directory
/// - a local `capsule.toml` path
/// - a cached GitHub repository ref (`github.com/<owner>/<repo>` or
///   `capsule://github.com/<owner>/<repo>`)
///
/// This intentionally does **not** fetch from registries, auto-install
/// capsules, or materialize provider-backed workspaces. Unsupported
/// targets fail fast with [`PreflightError::UnsupportedTarget`] so the
/// caller cannot mistake this read-only path for normal `ato run`.
///
/// `profile` is forwarded to `compile_execution_plan` so the caller
/// can choose Dev (default) vs Prod policy. The desktop launch worker
/// passes Dev.
pub fn collect_aggregate_requirements(
    target: &str,
    profile: ExecutionProfile,
) -> Result<AggregatePreflightResult, PreflightError> {
    let manifest_path = resolve_offline_manifest_path(target)?;

    let loaded = capsule::contract::manifest::load_manifest(&manifest_path).map_err(|err| {
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
    //
    // PR #180 review fix: feed `loaded.raw` (the unmodified TOML
    // value) into `manifest_external_capsule_dependencies`, NOT a
    // re-serialization of `loaded.model`. The typed model may not
    // fully round-trip every `[dependencies.<alias>]` shape (custom
    // parameters tables, contract variants), so a manifest with deps
    // could otherwise project to an empty alias list and silently
    // skip dep-driven preflight envelopes. Errors are surfaced as
    // `PreflightError::ManifestParse` instead of `unwrap_or_default`
    // so the failure is visible.
    let manifest_dependencies =
        manifest_external_capsule_dependencies(&loaded.raw).map_err(|source| {
            PreflightError::ManifestParse {
                path: manifest_path.clone(),
                source,
            }
        })?;
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

    // 2b. State-binding requirements (#404). Each `[state.<key>]` with
    //     `attach = "explicit"` needs a user-chosen host directory before
    //     launch — the same conditions the install ledger records as
    //     `UserGrantRequired` (see `extract_state_conditions`). Auto-attach
    //     states are provisioned by Ato (ledger `Satisfied`) and are omitted.
    //     We emit a typed `StateBindingRequired { state_key, label }` so the
    //     desktop's requirement-aggregation modal can surface a "choose folder"
    //     prompt; the chosen path is resolved through the non-interactive
    //     `resolve_state_binding_from_path` seam, which keeps the raw path
    //     local-private. The label is the requirement's `purpose` (a
    //     user-facing string, never a path); no raw path is emitted here.
    //
    //     Emitted in sorted state-key order for a stable modal layout.
    let mut state_keys: Vec<&String> = manifest
        .state
        .iter()
        .filter(|(_, requirement)| requirement.attach == StateAttach::Explicit)
        .map(|(key, _)| key)
        .collect();
    state_keys.sort();
    for state_key in state_keys {
        let requirement = &manifest.state[state_key];
        let label = if requirement.purpose.trim().is_empty() {
            state_key.clone()
        } else {
            requirement.purpose.clone()
        };
        requirements.push(InteractiveResolutionEnvelope {
            kind: InteractiveResolutionKind::StateBindingRequired {
                state_key: state_key.clone(),
                label: label.clone(),
            },
            display: ResolutionDisplay {
                message: format!("Choose a local folder for '{label}'."),
                hint: Some(
                    "This app stores persistent data here. Pick a directory on this device."
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
        //
        // PR-4b: the consent envelope fields are stamped from a
        // bundle-derived `ExecutionConsentView` so the user-visible
        // E302 modal copy is fed by the view. The GATING decision
        // (skip-or-emit-envelope) stays on plan-direct `has_consent`
        // because it carries the zero-permission short-circuit that
        // the view doesn't yet model. A debug parity guard pins that
        // the view-side `has_consent_view` agrees with the plan-side
        // EXCEPT for zero-permission plans (where the plan returns
        // true via short-circuit and the view returns false because
        // no record is in the log — both reach the same "no envelope
        // pushed" outcome).
        let target_deps = capsule::lockfile::manifest_external_capsule_dependencies(&loaded.raw)
            .map_err(|source| PreflightError::ManifestParse {
                path: manifest_path.clone(),
                source,
            })?;
        let consent_input = capsule::engine::execution_graph::GraphConsentInput {
            scoped_id: plan.consent.key.scoped_id.clone(),
            version: plan.consent.key.version.clone(),
            target_label: plan.consent.key.target_label.clone(),
            policy_segment_hash: plan.consent.policy_segment_hash.clone(),
            provisioning_policy_hash: plan.consent.provisioning_policy_hash.clone(),
        };
        let bundle = crate::application::graph_views::build_declared_only_bundle_with_consent(
            &target_deps,
            Some(manifest_path.display().to_string()),
            None,
            Vec::new(),
            consent_input,
        );
        let view = crate::application::graph_views::ExecutionConsentView::from_bundle(&bundle);
        let already_consented =
            has_consent(&plan).map_err(|err| PreflightError::ConsentStore { source: err })?;
        debug_assert!(
            {
                let view_side = crate::application::auth::consent_store::has_consent_view(&view)
                    .unwrap_or(already_consented);
                // Plan-side short-circuits to true for
                // zero-permission plans; view-side has no such
                // knowledge and returns false until a record lands.
                // Treat both as agreement when plan-side is true.
                already_consented || already_consented == view_side
            },
            "PR-4b parity: has_consent_view disagrees with plan-direct has_consent \
             (outside the zero-permission short-circuit)"
        );
        if !already_consented {
            requirements.push(InteractiveResolutionEnvelope {
                kind: InteractiveResolutionKind::ConsentRequired {
                    // Envelope fields come from the bundle-derived
                    // view (same values as the plan side; the view's
                    // Option fields are guaranteed Some here because
                    // we just supplied them via GraphConsentInput).
                    scoped_id: view.scoped_id.clone().unwrap_or_default(),
                    version: view.version.clone().unwrap_or_default(),
                    target_label: view.target_label.clone().unwrap_or_default(),
                    policy_segment_hash: view.policy_segment_hash.clone().unwrap_or_default(),
                    provisioning_policy_hash: view
                        .provisioning_policy_hash
                        .clone()
                        .unwrap_or_default(),
                    // consent_summary still reads runtime policy
                    // details that aren't on the bundle; keep the
                    // plan-rich summary text so E302 modal copy is
                    // byte-equivalent to pre-PR-4b.
                    summary: consent_summary(&plan),
                },
                display: ResolutionDisplay {
                    message: format!(
                        "Approve ExecutionPlan for target '{target_label}' of \
                         {}@{}.",
                        view.scoped_id.as_deref().unwrap_or(""),
                        view.version.as_deref().unwrap_or(""),
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

/// Resolve `target` (a directory, a `capsule.toml` path, or a cached
/// GitHub repository ref) to an absolute `capsule.toml` location. Returns
/// [`PreflightError::ManifestMissing`] when no manifest is reachable.
///
/// Resolution policy:
///
/// 1. **Existing local path**: directory inputs append `capsule.toml`; file
///    inputs are used as-is. Local paths intentionally win over bundled sample
///    recipe aliases with the same name.
/// 2. **Bundled sample recipe alias / GitHub mapping**: materialize the embedded
///    recipe manifest locally and use it for preflight. This write is
///    deterministic and does not fetch from the network.
/// 3. **`capsule://github.com/<owner>/<repo>`**: look only under
///    `${ATO_HOME}/external-capsules/github/<owner>/<repo>/*`. When the
///    repo segment is pinned as `repo@<sha>`, only the exact
///    `<sha>/capsule.toml` cache entry is valid; no mtime fallback is
///    allowed. Unpinned refs may use the most recently modified cached
///    external snapshot. This works only if `ato run`/`ato-desktop` has
///    already cached the capsule once before — first-time fetching is
///    intentionally out of scope for this slice (avoiding new network/git
///    side effects in the preflight path is what makes preflight safe).
/// 4. **`github.com/<owner>/<repo>`**: normalize it exactly the way
///    `ato run` does, then reuse the same cache lookup.
/// 5. **Registry refs / provider refs**: rejected. Side-effect-free
///    preflight must not fetch from registries or materialize
///    provider-backed workspaces.
fn resolve_offline_manifest_path(target: &str) -> Result<PathBuf, PreflightError> {
    let expanded = crate::local_input::expand_local_path(target);
    if expanded.exists() {
        return resolve_existing_local_manifest(expanded);
    }

    if let Some(rest) = target.strip_prefix("capsule://github.com/") {
        if let Some(manifest) = resolve_sample_recipe_manifest_for_github_rest(rest)? {
            return Ok(manifest);
        }
        return resolve_cached_github_capsule(rest);
    }

    if let Some(resolved) = resolve_sample_recipe_for_input(target).map_err(|err| {
        PreflightError::SampleRecipeMaterialize {
            input: target.to_string(),
            reason: err.to_string(),
        }
    })? {
        return Ok(resolved.manifest_path);
    }

    match crate::application::engine::install::provider_target::classify_run_target(target, &expanded)
    {
        Ok(crate::application::engine::install::provider_target::ParsedRunTarget::GitHubRepository(
            repository,
        )) => {
            if let Some(manifest) = resolve_sample_recipe_manifest_for_github_rest(&repository)? {
                return Ok(manifest);
            }
            resolve_cached_github_capsule(&repository)
        }
        Ok(crate::application::engine::install::provider_target::ParsedRunTarget::LocalPath(_)) => {
            if !expanded.exists() {
                return Err(PreflightError::ManifestMissing {
                    path: expanded.clone(),
                });
            }
            resolve_existing_local_manifest(expanded)
        }
        Ok(crate::application::engine::install::provider_target::ParsedRunTarget::Provider(
            provider_target,
        )) => Err(PreflightError::UnsupportedTarget {
            input: target.to_string(),
            reason: format!(
                "provider-backed target '{}:{}' is not supported by side-effect-free preflight; it would need workspace materialization. Run `ato run` or `ato fetch` first.",
                provider_target.provider.as_str(),
                provider_target.ref_string
            ),
        }),
        Ok(crate::application::engine::install::provider_target::ParsedRunTarget::RegistryReference) =>
        {
            Err(PreflightError::UnsupportedTarget {
                input: target.to_string(),
                reason: "registry handles are not supported by side-effect-free preflight; install the capsule first, then run `--plan-only` against the resulting local path.".to_string(),
            })
        }
        Err(err) => Err(PreflightError::UnsupportedTarget {
            input: target.to_string(),
            reason: err.to_string(),
        }),
    }
}

fn resolve_existing_local_manifest(path: PathBuf) -> Result<PathBuf, PreflightError> {
    let manifest = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path
    };
    if !manifest.exists() {
        return Err(PreflightError::ManifestMissing { path: manifest });
    }
    Ok(manifest)
}

fn resolve_sample_recipe_manifest_for_github_rest(
    rest: &str,
) -> Result<Option<PathBuf>, PreflightError> {
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    let owner = parts[0];
    let repo = parts[1];
    if repo.contains('@') {
        return Ok(None);
    }
    Ok(resolve_sample_recipe_for_github(owner, repo)
        .map_err(|err| PreflightError::SampleRecipeMaterialize {
            input: format!("github.com/{owner}/{repo}"),
            reason: err.to_string(),
        })?
        .map(|resolved| resolved.manifest_path))
}

/// Resolve a `capsule://github.com/<owner>/<repo>` ref to a cached
/// external snapshot under `${ATO_HOME}/external-capsules/github/...`.
/// Pinned `repo@<sha>` refs require an exact `<sha>/capsule.toml`
/// hit; unpinned refs use the most recently modified cached snapshot.
/// This intentionally never fetches over the network — preflight must
/// stay side-effect-free.
fn resolve_cached_github_capsule(rest: &str) -> Result<PathBuf, PreflightError> {
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err(PreflightError::ManifestMissing {
            path: PathBuf::from(format!("capsule://github.com/{rest}")),
        });
    }
    let owner = parts[0];
    let (repo, pinned_ref) = match parts[1].split_once('@') {
        Some((repo, pinned_ref)) if !pinned_ref.is_empty() => (repo, Some(pinned_ref)),
        _ => (parts[1], None),
    };
    let ato_home = capsule::common::paths::nacelle_home_dir_or_workspace_tmp();

    // The publisher-scoped external-capsule cache. Layout:
    // `${ATO_HOME}/external-capsules/github/<owner>/<repo>/<commit>/`.
    let external_root = ato_home
        .join("external-capsules")
        .join("github")
        .join(owner)
        .join(repo);
    if let Some(pinned_ref) = pinned_ref {
        let manifest = external_root.join(pinned_ref).join("capsule.toml");
        if manifest.exists() {
            return Ok(manifest);
        }
        return Err(PreflightError::ManifestMissing { path: manifest });
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
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
        capsule::router::route_manifest(manifest_path, profile, None).map_err(|err| {
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

fn collect_global_required_env(manifest: &capsule::types::CapsuleManifest) -> Vec<String> {
    manifest.required_env.clone()
}

/// Stable-order helper for the PR-3c parity guards.
fn sorted_dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn collect_target_required_env(
    manifest: &capsule::types::CapsuleManifest,
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
    use crate::runtime::oci_provider::{
        CommandOutput, OciCommandRunner, PodmanProbePlatform, PodmanProvider,
    };
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Arc, Mutex};
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

    #[derive(Clone, Default)]
    struct FakeRunner {
        outputs: Arc<Mutex<HashMap<String, std::io::Result<CommandOutput>>>>,
    }

    impl FakeRunner {
        fn with_output(self, command: &[&str], output: CommandOutput) -> Self {
            self.outputs
                .lock()
                .unwrap()
                .insert(command.join(" "), Ok(output));
            self
        }

        fn with_error(self, command: &[&str], error: std::io::Error) -> Self {
            self.outputs
                .lock()
                .unwrap()
                .insert(command.join(" "), Err(error));
            self
        }
    }

    impl OciCommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let key = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            // Mocked outputs are reusable: `preflight_oci_provider_readiness`
            // calls `ensure_ready()` (which on Linux issues a `probe()`
            // internally) followed by another `provider.probe().await`, so
            // the same command can be observed twice in a single readiness
            // check. Treating each registered output as consume-once would
            // make the second probe collapse to NotFound → `Missing` and
            // hide the actual production behavior under a fake-runner
            // limitation. Cloning here keeps the assertions honest while
            // still surfacing genuinely unmocked commands.
            let outputs = self.outputs.lock().unwrap();
            match outputs.get(&key) {
                Some(Ok(output)) => Ok(output.clone()),
                Some(Err(err)) => Err(std::io::Error::new(err.kind(), err.to_string())),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("missing fake command: {key}"),
                )),
            }
        }
    }

    struct TestOciProviderSelector {
        runner: FakeRunner,
        platform: PodmanProbePlatform,
    }

    impl crate::runtime::oci_provider::OciProviderSelector for TestOciProviderSelector {
        type Provider = PodmanProvider<FakeRunner>;

        fn select_provider(&self) -> Self::Provider {
            PodmanProvider::with_runner(self.runner.clone(), self.platform.clone())
        }
    }

    fn output(status: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    fn selector(runner: FakeRunner, platform: PodmanProbePlatform) -> TestOciProviderSelector {
        TestOciProviderSelector { runner, platform }
    }

    #[tokio::test]
    async fn oci_provider_readiness_required_fails_when_podman_missing() {
        let selector = selector(
            FakeRunner::default().with_error(
                &["podman", "--version"],
                std::io::Error::new(std::io::ErrorKind::NotFound, "missing podman"),
            ),
            PodmanProbePlatform::Linux,
        );

        let error = preflight_oci_provider_readiness(
            &selector,
            OciProviderReadinessMode::Required,
            OciProviderReadinessRequirements::default(),
        )
        .await
        .expect_err("required readiness must fail");

        assert_eq!(error.code(), "oci_provider_missing");
    }

    #[tokio::test]
    async fn oci_provider_readiness_best_effort_reports_missing_without_failing() {
        let selector = selector(
            FakeRunner::default().with_error(
                &["podman", "--version"],
                std::io::Error::new(std::io::ErrorKind::NotFound, "missing podman"),
            ),
            PodmanProbePlatform::Linux,
        );

        let outcome = preflight_oci_provider_readiness(
            &selector,
            OciProviderReadinessMode::BestEffort,
            OciProviderReadinessRequirements::default(),
        )
        .await
        .expect("best-effort readiness should not fail parent operation");

        match outcome {
            OciProviderReadinessOutcome::NotReady(error) => {
                assert_eq!(error.code(), "oci_provider_missing");
            }
            OciProviderReadinessOutcome::Ready(probe) => {
                panic!("expected missing provider diagnostic, got ready probe: {probe:?}");
            }
        }
    }

    /// Regression cover for the macOS auto-start failure path. Prior to
    /// #328 (`fix(oci): ensure Podman machine is running before OCI session
    /// start`), `ensure_ready` on macOS returned `NotReady{ MachineNotRunning }`
    /// the moment `machine list` reported the single machine stopped. #328
    /// changed that: a single stopped machine is auto-started. The relevant
    /// failure surface for `Required` readiness is therefore no longer
    /// "machine not running" but "auto-start failed", and the typed error
    /// shape is `MachineStartFailed` (`oci_machine_start_failed`), not the
    /// older `NotReady`. This test pins the new contract so a future change
    /// to the auto-start path does not silently swallow start failures.
    #[tokio::test]
    async fn oci_provider_readiness_required_surfaces_machine_start_failure() {
        let selector = selector(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(
                        0,
                        r#"[{"Name":"podman-machine-default","MachineId":"volatile","Running":false}]"#,
                        "",
                    ),
                )
                .with_output(
                    &["podman", "machine", "start", "podman-machine-default"],
                    output(
                        1,
                        "",
                        "Error: cannot start machine: provider is unavailable\n",
                    ),
                ),
            PodmanProbePlatform::Macos,
        );

        let error = preflight_oci_provider_readiness(
            &selector,
            OciProviderReadinessMode::Required,
            OciProviderReadinessRequirements::default(),
        )
        .await
        .expect_err("required readiness must surface auto-start failures");

        assert_eq!(error.code(), "oci_machine_start_failed");
        match error {
            OciProviderError::MachineStartFailed {
                machine_name,
                reason,
            } => {
                assert_eq!(machine_name, "podman-machine-default");
                assert!(
                    reason.contains("provider is unavailable"),
                    "reason must propagate the start-command stderr: {reason:?}"
                );
            }
            other => panic!("expected MachineStartFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oci_provider_readiness_required_succeeds_when_podman_rootless_ready() {
        let selector = selector(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(0, "true\n", ""),
                ),
            PodmanProbePlatform::Linux,
        );

        let outcome = preflight_oci_provider_readiness(
            &selector,
            OciProviderReadinessMode::Required,
            OciProviderReadinessRequirements {
                rootless: OciRootlessRequirement::Required,
            },
        )
        .await
        .expect("required readiness");

        match outcome {
            OciProviderReadinessOutcome::Ready(probe) => {
                assert!(probe.ready);
                assert_eq!(probe.inventory.mode, OciProviderMode::Rootless);
            }
            OciProviderReadinessOutcome::NotReady(error) => {
                panic!("expected ready provider, got {error:?}");
            }
        }
    }

    #[tokio::test]
    async fn oci_provider_readiness_required_rootless_rejects_ambiguous_mode() {
        let selector = selector(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(0, "not-a-bool\n", ""),
                ),
            PodmanProbePlatform::Linux,
        );

        let error = preflight_oci_provider_readiness(
            &selector,
            OciProviderReadinessMode::Required,
            OciProviderReadinessRequirements {
                rootless: OciRootlessRequirement::Required,
            },
        )
        .await
        .expect_err("ambiguous rootless mode must not satisfy a rootless requirement");

        assert_eq!(error.code(), "oci_provider_capability_unsupported");
    }

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
                InteractiveResolutionKind::StateBindingRequired { state_key, .. } => {
                    state_key.as_str()
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
            kinds.contains(&"web"),
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

    /// #404: an `attach = "explicit"` `[state.<key>]` requirement is unresolved
    /// at preflight (the same conditions the install ledger marks
    /// `UserGrantRequired`), so the collector must emit a typed
    /// `StateBindingRequired { state_key, label }`. The label is the
    /// requirement's `purpose` (a user-facing string), and NO raw host path is
    /// emitted anywhere in the result.
    #[test]
    #[serial_test::serial]
    fn preflight_emits_state_binding_required_for_unresolved_state() {
        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = manifest_dir.path().join("capsule.toml");
        let manifest = r#"
schema_version = "0.3"
name           = "state-binding-app"
version        = "0.1.0"
type           = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/state-binding-app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "user documents"
attach = "explicit"
schema_id = "state-binding-app/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app/data"
"#;
        fs::write(&manifest_path, manifest).expect("write");

        let result =
            collect_aggregate_requirements(&manifest_path.to_string_lossy(), ExecutionProfile::Dev)
                .expect("collect");

        let state_reqs: Vec<(&str, &str)> = result
            .requirements
            .iter()
            .filter_map(|env| match &env.kind {
                InteractiveResolutionKind::StateBindingRequired { state_key, label } => {
                    Some((state_key.as_str(), label.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            state_reqs,
            vec![("data", "user documents")],
            "explicit-attach state must surface a StateBindingRequired with the purpose as label"
        );

        // No raw host path (the manifest's `state_bindings.target =
        // /var/lib/app/data`) may appear in the emitted state-binding
        // requirement — it carries only the logical state key + label.
        let state_envelopes: Vec<&InteractiveResolutionEnvelope> = result
            .requirements
            .iter()
            .filter(|env| {
                matches!(
                    env.kind,
                    InteractiveResolutionKind::StateBindingRequired { .. }
                )
            })
            .collect();
        let json = serde_json::to_string(&state_envelopes).expect("serialize");
        assert!(
            !json.contains("/var/lib/app/data"),
            "no host path may be emitted: {json}"
        );
        assert!(
            !json.contains('/'),
            "no path separator may be emitted: {json}"
        );
    }

    /// #404: an `attach = "auto"` state is provisioned by Ato (ledger
    /// `Satisfied`), so the collector must NOT emit a StateBindingRequired for
    /// it — only explicit-attach states need a user-chosen folder.
    #[test]
    #[serial_test::serial]
    fn preflight_omits_requirement_when_bound() {
        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = manifest_dir.path().join("capsule.toml");
        let manifest = r#"
schema_version = "0.3"
name           = "auto-state-app"
version        = "0.1.0"
type           = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/auto-state-app:latest"

[state.cache]
kind = "filesystem"
durability = "ephemeral"
purpose = "scratch cache"
attach = "auto"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "cache"
target = "/var/lib/app/cache"
"#;
        fs::write(&manifest_path, manifest).expect("write");

        let result =
            collect_aggregate_requirements(&manifest_path.to_string_lossy(), ExecutionProfile::Dev)
                .expect("collect");

        let has_state_req = result.requirements.iter().any(|env| {
            matches!(
                env.kind,
                InteractiveResolutionKind::StateBindingRequired { .. }
            )
        });
        assert!(
            !has_state_req,
            "auto-attach (Satisfied) state must not surface a StateBindingRequired"
        );
    }

    /// PR #180 review fix: a capsule manifest carrying
    /// `[dependencies.<alias>]` blocks must produce a bundle whose
    /// `derived.preflight.dependency_aliases` contains EVERY declared
    /// alias. This pins the raw-TOML input path:
    /// `manifest_external_capsule_dependencies(&loaded.raw)` reads
    /// the unmodified TOML, not a re-serialization of the typed model
    /// (which historically silently dropped aliases when typed-model
    /// round-trip was imperfect).
    #[test]
    #[serial_test::serial]
    fn bundle_dependency_aliases_carry_raw_manifest_dependencies() {
        use crate::application::graph_views::{PreflightView, build_declared_only_bundle};
        use capsule::lockfile::manifest_external_capsule_dependencies;

        let home = TempDir::new().expect("home");
        let ato_home = TempDir::new().expect("ato_home");
        let _home_guard = scoped_env("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _ato_home_guard =
            scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let manifest_dir = TempDir::new().expect("manifest_dir");
        let manifest_path = manifest_dir.path().join("capsule.toml");
        let manifest = r#"
schema_version = "0.3"
name           = "deps-fixture"
version        = "0.1.0"
type           = "app"

runtime = "source/python"
run = "main.py"

required_env = ["DB_PASSWORD"]

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  database = "appdb"

[dependencies.cache]
capsule = "capsule://ato/acme-redis@7"
contract = "service@1"
"#;
        fs::write(&manifest_path, manifest).expect("write");

        // Mirror the path the collector takes: load the manifest,
        // feed `loaded.raw` (NOT `toml::Value::try_from(&loaded.model)`)
        // into the dependency derivation.
        let loaded = capsule::contract::manifest::load_manifest(&manifest_path).expect("load");
        let manifest_dependencies =
            manifest_external_capsule_dependencies(&loaded.raw).expect("derive deps");
        assert_eq!(
            manifest_dependencies.len(),
            2,
            "raw-TOML derivation must see both `db` and `cache`; \
             got {} aliases",
            manifest_dependencies.len()
        );

        let bundle = build_declared_only_bundle(
            &manifest_dependencies,
            Some(manifest_path.display().to_string()),
            None,
            loaded.model.required_env.clone(),
        );
        let view = PreflightView::from_bundle(&bundle);

        let mut aliases = view.dependency_aliases.clone();
        aliases.sort_unstable();
        assert_eq!(
            aliases,
            vec!["cache".to_string(), "db".to_string()],
            "PR-3c: bundle-derived dependency_aliases must contain every \
             raw-TOML [dependencies.<alias>] declaration"
        );
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
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        EnvGuard { key, previous }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// #146 regression: `repo@<sha>` must resolve only from the exact
    /// commit cache entry, not by falling back to some other cached
    /// commit for the same owner/repo.
    #[test]
    #[serial_test::serial]
    fn github_cache_resolver_requires_exact_sha_match() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let repo = "MyRepo";
        let owner = "acme";
        let requested_commit = "somecommitsha";

        // Populate external-capsules cache: ~/.ato/external-capsules/github/<owner>/<repo>/<sha>/
        let cached_commit = requested_commit;
        let ext_root = ato_home
            .path()
            .join("external-capsules")
            .join("github")
            .join(owner)
            .join(repo)
            .join(cached_commit);
        std::fs::create_dir_all(&ext_root).expect("create ext_root");
        std::fs::write(ext_root.join("capsule.toml"), "[package]\nname=\"x\"\n")
            .expect("write manifest");

        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let rest = format!("{owner}/{repo}@{requested_commit}");
        let result = resolve_cached_github_capsule(&rest);

        assert!(
            result.is_ok(),
            "#146: repo@sha should resolve only from the exact cached commit (got {result:?})"
        );
        let found = result.unwrap();
        assert_eq!(found, ext_root.join("capsule.toml"));
    }

    #[test]
    #[serial_test::serial]
    fn github_cache_resolver_rejects_mismatched_sha_cache_hit() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let repo = "MyRepo";
        let owner = "acme";

        let ext_root = ato_home
            .path()
            .join("external-capsules")
            .join("github")
            .join(owner)
            .join(repo)
            .join("abc123deadbeef");
        std::fs::create_dir_all(&ext_root).expect("create ext_root");
        std::fs::write(ext_root.join("capsule.toml"), "[package]\nname=\"x\"\n")
            .expect("write manifest");

        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let result = resolve_cached_github_capsule(&format!("{owner}/{repo}@somecommitsha"));

        assert!(matches!(
            result,
            Err(PreflightError::ManifestMissing { .. })
        ));
    }

    #[test]
    #[serial_test::serial]
    fn github_cache_resolver_ignores_owner_blind_gh_run_checkout() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let repo = "MyRepo";

        let gh_run = ato_home
            .path()
            .join("tmp")
            .join("gh-run")
            .join(format!("{repo}-123"));
        std::fs::create_dir_all(&gh_run).expect("create gh-run");
        std::fs::write(gh_run.join("capsule.toml"), "[package]\nname=\"x\"\n")
            .expect("write manifest");

        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let result = resolve_cached_github_capsule(&format!("acme/{repo}"));

        assert!(matches!(
            result,
            Err(PreflightError::ManifestMissing { .. })
        ));
    }

    #[test]
    #[serial_test::serial]
    fn offline_manifest_resolver_accepts_github_run_shorthand() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let owner = "acme";
        let repo = "MyRepo";
        let cached_commit = "abc123deadbeef";
        let ext_root = ato_home
            .path()
            .join("external-capsules")
            .join("github")
            .join(owner)
            .join(repo)
            .join(cached_commit);
        std::fs::create_dir_all(&ext_root).expect("create ext_root");
        std::fs::write(ext_root.join("capsule.toml"), "[package]\nname=\"x\"\n")
            .expect("write manifest");

        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let resolved = resolve_offline_manifest_path(&format!("github.com/{owner}/{repo}"))
            .expect("github shorthand should resolve from cache");

        assert!(
            resolved.ends_with("capsule.toml"),
            "expected a path ending in capsule.toml, got {resolved:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn internal_preflight_resolves_sample_recipe_alias() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let resolved = resolve_offline_manifest_path("memos")
            .expect("sample recipe alias should materialize for preflight");

        assert!(resolved.ends_with("sample-recipes/memos/capsule.toml"));
        assert!(resolved.exists());
    }

    #[test]
    #[serial_test::serial]
    fn internal_preflight_resolves_sample_recipe_github_handle() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let resolved = resolve_offline_manifest_path("capsule://github.com/usememos/memos")
            .expect("sample recipe github handle should materialize for preflight");

        assert!(resolved.ends_with("sample-recipes/memos/capsule.toml"));
        assert!(resolved.exists());
    }

    #[test]
    #[serial_test::serial]
    fn internal_preflight_preserves_existing_local_path_precedence() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let parent = tempfile::TempDir::new().expect("parent");
        let local = parent.path().join("memos");
        std::fs::create_dir_all(&local).expect("create local alias dir");
        std::fs::write(
            local.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "local-memos"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "nginx:alpine"
port = 80
"#,
        )
        .expect("write local manifest");

        let _ato_guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let _cwd_guard = CwdGuard::enter(parent.path());

        let resolved =
            resolve_offline_manifest_path("memos").expect("local dir should win over alias");

        assert_eq!(resolved, PathBuf::from("memos").join("capsule.toml"));
        let content = std::fs::read_to_string(&resolved).expect("read resolved local manifest");
        assert!(content.contains("local-memos"));
    }

    #[test]
    #[serial_test::serial]
    fn internal_preflight_does_not_fall_back_to_cached_github_when_sample_recipe_exists() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let cached = ato_home
            .path()
            .join("external-capsules")
            .join("github")
            .join("usememos")
            .join("memos")
            .join("cached-sha");
        std::fs::create_dir_all(&cached).expect("create cache");
        std::fs::write(cached.join("capsule.toml"), "[package]\nname=\"cached\"\n")
            .expect("write cached manifest");

        let _guard = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let resolved = resolve_offline_manifest_path("github.com/usememos/memos")
            .expect("sample recipe should win over cached github snapshot");

        assert!(resolved.ends_with("sample-recipes/memos/capsule.toml"));
        assert_ne!(resolved, cached.join("capsule.toml"));
    }

    #[test]
    fn offline_manifest_resolver_rejects_registry_refs() {
        let err = resolve_offline_manifest_path("acme/demo").expect_err("registry ref must fail");
        match err {
            PreflightError::UnsupportedTarget { input, reason } => {
                assert_eq!(input, "acme/demo");
                assert!(
                    reason.contains("side-effect-free preflight"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected unsupported target error, got {other:?}"),
        }
    }

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self { previous }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }
}
