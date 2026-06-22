use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use capsule::handle::{
    CanonicalHandle, HandleInput, InputSurface, LaunchPlan, LocalTrustDecisionRecord,
    PermissionRequestPolicy, ResolvedMetadataCacheEntry, ResolvedSnapshot, SurfaceInput,
    TrustState, classify_surface_input,
};
use capsule::handle_store::{
    load_metadata_cache, metadata_cache_is_fresh, metadata_cache_ttl_seconds, resolve_trust_state,
    store_local_trust_decision, store_metadata_cache,
};
use capsule::launch_spec::{LaunchSpecSource, derive_launch_spec};
use capsule::router::{
    ExecutionProfile, ManifestData, execution_descriptor_from_manifest_parts,
    route_manifest_with_state_overrides,
};

use super::guest_contract::{GuestContract, parse_guest_contract, preview_guest_contract};
use super::sample_recipes::{resolve_sample_recipe_for_github, resolve_sample_recipe_for_input};
use crate::install::{
    download_github_repository_at_ref, fetch_capsule_detail, fetch_capsule_manifest_toml,
    fetch_github_install_draft, parse_capsule_request,
};

const ACTION: &str = "resolve_handle";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum HandleKind {
    WebUrl,
    LocalCapsule,
    StoreCapsule,
    RemoteSourceRef,
    SampleRecipe,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum RenderStrategy {
    Web,
    Terminal,
    GuestWebview,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ResolveEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    resolution: HandleResolution,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct HandleResolution {
    pub(super) input: String,
    pub(super) normalized_handle: String,
    pub(super) kind: HandleKind,
    pub(super) render_strategy: RenderStrategy,
    pub(super) canonical_handle: Option<String>,
    pub(super) source: Option<String>,
    pub(super) trust_state: TrustState,
    pub(super) restricted: bool,
    pub(super) launch_plan: Option<LaunchPlan>,
    pub(super) snapshot: Option<ResolvedSnapshot>,
    pub(super) guest: Option<super::guest_contract::GuestContractPreview>,
    pub(super) target: Option<TargetSummary>,
    pub(super) launch: Option<LaunchPreview>,
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct TargetSummary {
    target_label: String,
    runtime: Option<String>,
    driver: Option<String>,
    language: Option<String>,
    port: Option<u16>,
    manifest_path: Option<String>,
    workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct LaunchPreview {
    working_dir: String,
    command: String,
    args: Vec<String>,
    env_vars: BTreeMap<String, String>,
    required_lockfile: Option<String>,
    runtime: Option<String>,
    driver: Option<String>,
    language: Option<String>,
    port: Option<u16>,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NormalizedHandleKind {
    WebUrl,
    LocalPath(PathBuf),
    StoreCapsule,
    RemoteSourceRef,
    SampleRecipe(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedHandle {
    input: String,
    normalized_handle: String,
    kind: NormalizedHandleKind,
    canonical: Option<CanonicalHandle>,
    cli_ref: Option<String>,
    sample_recipe_slug: Option<String>,
}

pub fn resolve_handle(
    handle: &str,
    target_label: Option<&str>,
    registry: Option<&str>,
    json: bool,
) -> Result<()> {
    let resolution = build_resolution(handle, target_label, registry)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ResolveEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::ATO_DESKTOP_PACKAGE_ID,
                action: ACTION,
                resolution,
            })?
        );
        return Ok(());
    }

    print_resolution(&resolution);
    Ok(())
}

pub(super) fn build_resolution_for_session_start(
    handle: &str,
    target_label: Option<&str>,
    registry: Option<&str>,
    use_sample_recipes: bool,
) -> Result<HandleResolution> {
    build_resolution_inner(handle, target_label, registry, use_sample_recipes)
}

pub(super) fn build_resolution(
    handle: &str,
    target_label: Option<&str>,
    registry: Option<&str>,
) -> Result<HandleResolution> {
    build_resolution_inner(handle, target_label, registry, true)
}

fn build_resolution_inner(
    handle: &str,
    target_label: Option<&str>,
    registry: Option<&str>,
    use_sample_recipes: bool,
) -> Result<HandleResolution> {
    let normalized = normalize_handle_with_options(handle, use_sample_recipes)?;

    match normalized.kind {
        NormalizedHandleKind::WebUrl => Ok(HandleResolution {
            input: normalized.input,
            normalized_handle: normalized.normalized_handle,
            kind: HandleKind::WebUrl,
            render_strategy: RenderStrategy::Web,
            canonical_handle: None,
            source: Some("web".to_string()),
            trust_state: TrustState::Unknown,
            restricted: false,
            launch_plan: None,
            snapshot: None,
            guest: None,
            target: None,
            launch: None,
            notes: Vec::new(),
        }),
        NormalizedHandleKind::SampleRecipe(manifest_path) => build_sample_recipe_resolution(
            normalized.input,
            normalized.normalized_handle,
            normalized.sample_recipe_slug,
            manifest_path,
            target_label,
        ),
        NormalizedHandleKind::RemoteSourceRef => build_github_resolution(
            normalized.input,
            normalized.normalized_handle,
            normalized
                .canonical
                .ok_or_else(|| anyhow::anyhow!("missing canonical GitHub handle"))?,
            target_label,
        ),
        NormalizedHandleKind::LocalPath(path) => build_local_resolution(
            normalized.input,
            normalized.normalized_handle,
            normalized.canonical,
            path,
            target_label,
        ),
        NormalizedHandleKind::StoreCapsule => build_store_resolution(
            normalized.input,
            normalized.normalized_handle,
            normalized
                .canonical
                .ok_or_else(|| anyhow::anyhow!("missing canonical registry handle"))?,
            target_label,
            registry,
        ),
    }
}

fn build_local_resolution(
    input: String,
    normalized_handle: String,
    canonical: Option<CanonicalHandle>,
    path: PathBuf,
    target_label: Option<&str>,
) -> Result<HandleResolution> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.clone()
    };

    if !manifest_path.exists() {
        anyhow::bail!("capsule.toml not found at {}", manifest_path.display());
    }

    let (plan, guest, mut notes) = resolve_local_plan(&manifest_path, target_label)?;
    let launch = derive_launch_spec(&plan)
        .map(build_launch_preview)
        .with_context(|| {
            format!(
                "failed to derive launch spec for {}",
                manifest_path.display()
            )
        })?;

    let snapshot = Some(ResolvedSnapshot::LocalPath {
        resolved_path: manifest_path.display().to_string(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
    });
    let trust_state = TrustState::Local;
    if let Some(canonical) = canonical.as_ref() {
        persist_metadata_cache(canonical, &normalized_handle, &plan, snapshot.clone())?;
        persist_local_trust_state(canonical, trust_state.clone(), "local-path")?;
    }

    Ok(HandleResolution {
        input,
        normalized_handle,
        kind: HandleKind::LocalCapsule,
        render_strategy: render_strategy(&plan, guest.as_ref()),
        canonical_handle: canonical.as_ref().map(CanonicalHandle::display_string),
        source: canonical
            .as_ref()
            .map(|handle| handle.source_label().to_string()),
        trust_state: trust_state.clone(),
        restricted: true,
        launch_plan: Some(default_launch_plan(
            canonical,
            snapshot.clone(),
            trust_state,
        )),
        snapshot,
        guest: guest.as_ref().map(preview_guest_contract),
        target: Some(build_target_summary(
            &plan,
            Some(manifest_path.display().to_string()),
            Some(plan.workspace_root.display().to_string()),
        )),
        launch: Some(launch),
        notes: {
            notes.shrink_to_fit();
            notes
        },
    })
}

pub(super) fn resolve_local_plan(
    manifest_path: &std::path::Path,
    target_label: Option<&str>,
) -> Result<(ManifestData, Option<GuestContract>, Vec<String>)> {
    resolve_local_plan_with_state_overrides(manifest_path, target_label, HashMap::new())
}

pub(super) fn resolve_local_plan_with_state_overrides(
    manifest_path: &std::path::Path,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
) -> Result<(ManifestData, Option<GuestContract>, Vec<String>)> {
    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read manifest at {}", manifest_path.display()))?;
    let raw_manifest: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("failed to parse manifest at {}", manifest_path.display()))?;
    let guest = parse_guest_contract(
        &raw_manifest,
        manifest_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );

    match route_manifest_with_state_overrides(
        manifest_path,
        ExecutionProfile::Release,
        target_label,
        state_source_overrides.clone(),
    ) {
        Ok(decision) => Ok((decision.plan, guest, Vec::new())),
        Err(err) => {
            let Some(driver) = experimental_guest_driver_from_error(&err) else {
                return Err(err).with_context(|| {
                    format!("failed to route manifest at {}", manifest_path.display())
                });
            };

            let plan = execution_descriptor_from_manifest_parts(
                raw_manifest,
                manifest_path.to_path_buf(),
                manifest_path
                    .parent()
                    .map(|path| path.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(".")),
                ExecutionProfile::Release,
                target_label,
                state_source_overrides,
            )
            .with_context(|| {
                format!(
                    "failed to build experimental ato-desktop execution descriptor at {}",
                    manifest_path.display()
                )
            })?;

            Ok((
                plan,
                guest,
                vec![format!(
                    "Used experimental ato-desktop guest-driver fallback for driver='{driver}'. Core manifest validation does not admit guest drivers yet."
                )],
            ))
        }
    }
}

pub(crate) fn resolve_local_plan_for_session(
    manifest_path: &std::path::Path,
    target_label: Option<&str>,
) -> Result<(ManifestData, Vec<String>)> {
    let (plan, _guest, notes) = resolve_local_plan(manifest_path, target_label)?;
    Ok((plan, notes))
}

/// Try to resolve a full execution plan from the locally-installed capsule archive when the
/// registry returns only a short lock reference (no `[targets]` table).
///
/// Returns `Some((plan, guest, notes))` if a locally installed copy exists, `None` otherwise.
fn resolve_local_plan_from_store(
    registry_manifest: &toml::Value,
    target_label: Option<&str>,
) -> Option<(ManifestData, Option<GuestContract>, Vec<String>)> {
    let publisher = registry_manifest.get("publisher")?.as_str()?;
    let slug = registry_manifest.get("name")?.as_str()?;
    let version = registry_manifest.get("version").and_then(|v| v.as_str());

    let store_root = capsule::common::paths::ato_path_or_workspace_tmp("store");

    let capsule_path = crate::install::support::resolve_installed_capsule_archive_in_store(
        &store_root.join(publisher),
        slug,
        version,
    )
    .ok()
    .flatten()?;

    let manifest_path = crate::runtime::tree::prepare_store_runtime_for_capsule(&capsule_path)
        .ok()
        .flatten()?;

    resolve_local_plan(&manifest_path, target_label).ok()
}

fn build_store_resolution(
    input: String,
    normalized_handle: String,
    canonical: CanonicalHandle,
    target_label: Option<&str>,
    registry: Option<&str>,
) -> Result<HandleResolution> {
    let cached_metadata = load_metadata_cache(&canonical)
        .with_context(|| format!("failed to load cached metadata for {normalized_handle}"))?;
    let trust_state = resolve_trust_state(&canonical, TrustState::Untrusted)
        .with_context(|| format!("failed to load trust state for {normalized_handle}"))?;
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let cli_ref = canonical
        .to_cli_ref()
        .ok_or_else(|| anyhow::anyhow!("registry handle does not support CLI resolution"))?;
    let registry_override = effective_registry_override(&canonical, registry);
    let manifest_toml = rt.block_on(fetch_capsule_manifest_toml(
        &cli_ref,
        registry_override.as_deref(),
    ))?;
    let manifest_value: toml::Value = toml::from_str(&manifest_toml)
        .with_context(|| format!("failed to parse remote manifest for {normalized_handle}"))?;

    // The registry returns a short lock reference (no `[targets]`) for presigned-upload
    // releases. Attempt to read the full manifest from the locally-installed copy instead.
    let (plan_opt, guest, extra_notes): (Option<ManifestData>, Option<GuestContract>, Vec<String>) =
        if manifest_value.get("targets").is_none() {
            match resolve_local_plan_from_store(&manifest_value, target_label) {
                Some((plan, guest, notes)) => (Some(plan), guest, notes),
                None => (
                    None,
                    None,
                    vec!["Target metadata not yet available; launch details will become concrete after installation.".to_string()],
                ),
            }
        } else {
            let guest = parse_guest_contract(&manifest_value, Path::new("."));
            let plan = execution_descriptor_from_manifest_parts(
                manifest_value,
                PathBuf::from("capsule.toml"),
                PathBuf::from("."),
                ExecutionProfile::Release,
                target_label,
                HashMap::new(),
            )
            .with_context(|| {
                format!("failed to build execution descriptor for {normalized_handle}")
            })?;
            (Some(plan), guest, vec![])
        };

    let detail = rt
        .block_on(fetch_capsule_detail(&cli_ref, registry_override.as_deref()))
        .ok();
    let snapshot = if let CanonicalHandle::RegistryCapsule { version, .. } = &canonical {
        Some(ResolvedSnapshot::RegistryRelease {
            version: version
                .clone()
                .or_else(|| detail.as_ref().and_then(|item| item.latest_version.clone()))
                .or_else(|| cached_registry_version(cached_metadata.as_ref()))
                .unwrap_or_else(|| "latest".to_string()),
            release_id: None,
            content_hash: None,
            fetched_at: chrono::Utc::now().to_rfc3339(),
        })
    } else {
        None
    };
    if let Some(plan) = plan_opt.as_ref() {
        persist_metadata_cache(&canonical, &normalized_handle, plan, snapshot.clone())?;
    }
    let mut notes = vec![
        "Remote store handles currently resolve target metadata only. Launch details become concrete after local materialization.".to_string(),
    ];
    notes.extend(extra_notes);
    if let Some(registry) = canonical
        .registry()
        .filter(|registry| registry.is_loopback())
    {
        notes.push(format!(
            "Loopback registry handle resolved via host-side developer endpoint {}.",
            registry.registry_endpoint
        ));
        notes.push(
            "Loopback registry capsules are untrusted by default; guest runtime permissions remain fail-closed until the host grants them."
                .to_string(),
        );
    }
    if let Some(cached) = cached_metadata
        .as_ref()
        .filter(|entry| metadata_cache_is_fresh(entry))
    {
        notes.push(format!(
            "Cached metadata was available from {}.",
            cached.fetched_at
        ));
    }

    Ok(HandleResolution {
        input,
        normalized_handle,
        kind: HandleKind::StoreCapsule,
        render_strategy: plan_opt
            .as_ref()
            .map(|p| render_strategy(p, guest.as_ref()))
            .unwrap_or(RenderStrategy::Terminal),
        canonical_handle: Some(canonical.display_string()),
        source: Some("registry".to_string()),
        trust_state: trust_state.clone(),
        restricted: true,
        launch_plan: Some(default_launch_plan(
            Some(canonical),
            snapshot.clone(),
            trust_state,
        )),
        snapshot,
        guest: guest.as_ref().map(preview_guest_contract),
        target: plan_opt
            .as_ref()
            .map(|p| build_target_summary(p, None, None)),
        launch: None,
        notes,
    })
}

fn build_github_resolution(
    input: String,
    normalized_handle: String,
    canonical: CanonicalHandle,
    target_label: Option<&str>,
) -> Result<HandleResolution> {
    let cached_metadata = load_metadata_cache(&canonical)
        .with_context(|| format!("failed to load cached metadata for {normalized_handle}"))?;
    let trust_state = resolve_trust_state(&canonical, TrustState::Untrusted)
        .with_context(|| format!("failed to load trust state for {normalized_handle}"))?;
    let cli_ref = canonical
        .to_cli_ref()
        .ok_or_else(|| anyhow::anyhow!("github handle does not support CLI resolution"))?;
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let draft = rt.block_on(fetch_github_install_draft(&cli_ref))?;
    let manifest_toml = if let Some(preview_toml) = draft.preview_toml.clone() {
        preview_toml
    } else if draft.capsule_toml.exists {
        let checkout = rt.block_on(download_github_repository_at_ref(
            &cli_ref,
            Some(&draft.resolved_ref.ref_name),
        ))?;
        std::fs::read_to_string(checkout.checkout_dir.join("capsule.toml")).with_context(|| {
            format!(
                "failed to read inferred repository manifest for {}",
                checkout.checkout_dir.display()
            )
        })?
    } else {
        anyhow::bail!("GitHub handle did not return previewToml or capsule.toml");
    };
    let manifest_value: toml::Value = toml::from_str(&manifest_toml)
        .with_context(|| format!("failed to parse remote manifest for {normalized_handle}"))?;
    let guest = parse_guest_contract(&manifest_value, std::path::Path::new("."));
    let plan = execution_descriptor_from_manifest_parts(
        manifest_value,
        PathBuf::from("capsule.toml"),
        PathBuf::from("."),
        ExecutionProfile::Release,
        target_label,
        HashMap::new(),
    )
    .with_context(|| format!("failed to build execution descriptor for {normalized_handle}"))?;
    let snapshot = Some(ResolvedSnapshot::GithubRepo {
        commit_sha: draft.resolved_ref.sha.clone(),
        default_branch: Some(draft.repo.default_branch.clone()),
        fetched_at: chrono::Utc::now().to_rfc3339(),
    });
    persist_metadata_cache(&canonical, &normalized_handle, &plan, snapshot.clone())?;
    let mut notes = vec![format!(
        "Resolved GitHub repository snapshot {} at {}.",
        draft.resolved_ref.ref_name, draft.resolved_ref.sha
    )];
    if let Some(cached) = cached_metadata
        .as_ref()
        .filter(|entry| metadata_cache_is_fresh(entry))
    {
        notes.push(format!(
            "Cached metadata was available from {}.",
            cached.fetched_at
        ));
    }

    Ok(HandleResolution {
        input,
        normalized_handle,
        kind: HandleKind::RemoteSourceRef,
        render_strategy: render_strategy(&plan, guest.as_ref()),
        canonical_handle: Some(canonical.display_string()),
        source: Some("github".to_string()),
        trust_state: trust_state.clone(),
        restricted: true,
        launch_plan: Some(default_launch_plan(
            Some(canonical),
            snapshot.clone(),
            trust_state,
        )),
        snapshot,
        guest: guest.as_ref().map(preview_guest_contract),
        target: Some(build_target_summary(&plan, None, None)),
        launch: None,
        notes,
    })
}

fn build_sample_recipe_resolution(
    input: String,
    normalized_handle: String,
    slug: Option<String>,
    manifest_path: PathBuf,
    target_label: Option<&str>,
) -> Result<HandleResolution> {
    let slug = slug.unwrap_or_else(|| "unknown".to_string());
    let (plan, guest, mut notes) = resolve_local_plan(&manifest_path, target_label)?;
    let launch = derive_launch_spec(&plan)
        .map(build_launch_preview)
        .with_context(|| {
            format!(
                "failed to derive launch spec for sample recipe '{}' at {}",
                slug,
                manifest_path.display()
            )
        })?;

    let manifest_rel = format!("samples/recipes/{slug}/capsule.toml");
    notes.push(format!("Resolved via bundled sample recipe '{slug}'."));

    let snapshot = Some(ResolvedSnapshot::LocalPath {
        resolved_path: manifest_path.display().to_string(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
    });
    let trust_state = TrustState::Local;

    Ok(HandleResolution {
        input,
        normalized_handle: normalized_handle.clone(),
        kind: HandleKind::SampleRecipe,
        render_strategy: render_strategy(&plan, guest.as_ref()),
        canonical_handle: Some(normalized_handle),
        source: Some("sample_recipe".to_string()),
        trust_state: trust_state.clone(),
        restricted: true,
        launch_plan: Some(default_launch_plan(None, snapshot.clone(), trust_state)),
        snapshot,
        guest: guest.as_ref().map(preview_guest_contract),
        target: Some(build_target_summary(
            &plan,
            Some(manifest_rel),
            Some(plan.workspace_root.display().to_string()),
        )),
        launch: Some(launch),
        notes: {
            notes.shrink_to_fit();
            notes
        },
    })
}

fn build_target_summary(
    plan: &ManifestData,
    manifest_path: Option<String>,
    workspace_root: Option<String>,
) -> TargetSummary {
    TargetSummary {
        target_label: plan.selected_target_label().to_string(),
        runtime: plan.execution_runtime(),
        driver: plan.execution_driver(),
        language: plan.execution_language(),
        port: plan.execution_port(),
        manifest_path,
        workspace_root,
    }
}

fn build_launch_preview(spec: capsule::launch_spec::LaunchSpec) -> LaunchPreview {
    LaunchPreview {
        working_dir: spec.working_dir.display().to_string(),
        command: spec.command,
        args: spec.args,
        env_vars: spec.env_vars.into_iter().collect(),
        required_lockfile: spec
            .required_lockfile
            .map(|path| path.display().to_string()),
        runtime: spec.runtime,
        driver: spec.driver,
        language: spec.language,
        port: spec.port,
        source: match spec.source {
            LaunchSpecSource::Entrypoint => "entrypoint".to_string(),
            LaunchSpecSource::RunCommand => "run_command".to_string(),
        },
    }
}

fn persist_metadata_cache(
    canonical: &CanonicalHandle,
    normalized_input: &str,
    plan: &ManifestData,
    snapshot: Option<ResolvedSnapshot>,
) -> Result<()> {
    let entry = ResolvedMetadataCacheEntry {
        canonical: canonical.clone(),
        normalized_input: normalized_input.to_string(),
        manifest_summary: Some(build_manifest_summary(plan)),
        snapshot,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        ttl_seconds: metadata_cache_ttl_seconds(canonical),
    };
    store_metadata_cache(&entry)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to persist metadata cache for {normalized_input}"))
}

fn persist_local_trust_state(
    canonical: &CanonicalHandle,
    trust_state: TrustState,
    reason: &str,
) -> Result<()> {
    let record = LocalTrustDecisionRecord {
        canonical: canonical.clone(),
        trust_state,
        session_scoped: false,
        recorded_at: chrono::Utc::now().to_rfc3339(),
        reason: Some(reason.to_string()),
    };
    store_local_trust_decision(&record)
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!(
                "failed to persist local trust state for {}",
                canonical.display_string()
            )
        })
}

fn build_manifest_summary(plan: &ManifestData) -> String {
    let mut parts = vec![format!("target={}", plan.selected_target_label())];
    if let Some(runtime) = plan.execution_runtime() {
        parts.push(format!("runtime={runtime}"));
    }
    if let Some(driver) = plan.execution_driver() {
        parts.push(format!("driver={driver}"));
    }
    if let Some(language) = plan.execution_language() {
        parts.push(format!("language={language}"));
    }
    parts.join(" ")
}

fn cached_registry_version(cache_entry: Option<&ResolvedMetadataCacheEntry>) -> Option<String> {
    let snapshot = cache_entry?.snapshot.as_ref()?;
    match snapshot {
        ResolvedSnapshot::RegistryRelease { version, .. } => Some(version.clone()),
        _ => None,
    }
}

fn render_strategy(plan: &ManifestData, guest: Option<&GuestContract>) -> RenderStrategy {
    if guest.is_some() {
        return RenderStrategy::GuestWebview;
    }

    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime_lower = runtime.to_ascii_lowercase();
    let driver_lower = driver.to_ascii_lowercase();

    if runtime_lower == "web" {
        return RenderStrategy::Web;
    }

    if matches!(driver_lower.as_str(), "tauri" | "electron" | "wails") {
        return RenderStrategy::GuestWebview;
    }

    // Any target that declares a port is serving HTTP — render it as
    // a web app, not a terminal stream. Without this, capsules like
    // `runtime=source, driver=node, port=3000` (a typical Node web
    // app) fall through to Terminal mode and the host shows the
    // process log instead of the served UI.
    if plan.execution_port().is_some() {
        return RenderStrategy::Web;
    }

    RenderStrategy::Terminal
}

pub(super) fn input_is_existing_local_path(input: &str) -> bool {
    let input_path = std::path::Path::new(input);
    input_path.exists() || input_path.join("capsule.toml").exists()
}

#[allow(dead_code)]
pub(super) fn normalize_handle(raw: &str) -> Result<NormalizedHandle> {
    normalize_handle_with_options(raw, true)
}

fn normalize_handle_with_options(raw: &str, use_sample_recipes: bool) -> Result<NormalizedHandle> {
    let input = raw.trim().to_string();
    if input.is_empty() {
        anyhow::bail!("handle must not be empty");
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        return Ok(NormalizedHandle {
            normalized_handle: input.clone(),
            input,
            kind: NormalizedHandleKind::WebUrl,
            canonical: None,
            cli_ref: None,
            sample_recipe_slug: None,
        });
    }

    if input.starts_with("ato://") {
        anyhow::bail!(
            "`ato://` is reserved for host routes and cannot be resolved as a capsule handle"
        );
    }

    if use_sample_recipes
        && !input.starts_with("capsule://")
        && !input.starts_with("github.com/")
        && !input.contains('/')
        && !input_is_existing_local_path(&input)
        && let Some(resolved) = resolve_sample_recipe_for_input(&input)?
    {
        return Ok(NormalizedHandle {
            input,
            normalized_handle: resolved
                .canonical_handle
                .clone()
                .unwrap_or_else(|| format!("sample-recipe://{}", resolved.slug)),
            kind: NormalizedHandleKind::SampleRecipe(resolved.manifest_path),
            canonical: None,
            cli_ref: None,
            sample_recipe_slug: Some(resolved.slug),
        });
    }

    match classify_surface_input(HandleInput {
        raw: input.clone(),
        surface: InputSurface::CliResolve,
    })
    .with_context(|| format!("unsupported handle '{input}'"))?
    {
        SurfaceInput::Capsule { canonical } => {
            let normalized_handle = canonical.display_string();
            let cli_ref = canonical.to_cli_ref();

            if use_sample_recipes
                && let CanonicalHandle::GithubRepo { owner, repo, .. } = &canonical
                && let Some(resolved) = resolve_sample_recipe_for_github(owner, repo)?
            {
                return Ok(NormalizedHandle {
                    input,
                    normalized_handle,
                    kind: NormalizedHandleKind::SampleRecipe(resolved.manifest_path),
                    canonical: Some(canonical),
                    cli_ref,
                    sample_recipe_slug: Some(resolved.slug),
                });
            }

            let kind = match &canonical {
                CanonicalHandle::GithubRepo { .. } => NormalizedHandleKind::RemoteSourceRef,
                CanonicalHandle::RegistryCapsule { .. } => NormalizedHandleKind::StoreCapsule,
                CanonicalHandle::LocalPath { path } => {
                    NormalizedHandleKind::LocalPath(path.clone())
                }
            };
            Ok(NormalizedHandle {
                normalized_handle,
                input,
                kind,
                canonical: Some(canonical),
                cli_ref,
                sample_recipe_slug: None,
            })
        }
        SurfaceInput::HostRoute { .. } => {
            anyhow::bail!("host routes cannot be resolved as capsule handles")
        }
        SurfaceInput::WebUrl { url } => Ok(NormalizedHandle {
            normalized_handle: url.clone(),
            input,
            kind: NormalizedHandleKind::WebUrl,
            canonical: None,
            cli_ref: None,
            sample_recipe_slug: None,
        }),
        SurfaceInput::SearchQuery { .. } => {
            let normalized_handle = normalize_curated_store_alias(&input);
            let canonical = capsule::handle::normalize_capsule_handle(&normalized_handle)?;
            let _ = parse_capsule_request(&normalized_handle)
                .with_context(|| format!("unsupported handle '{input}'"))?;
            Ok(NormalizedHandle {
                normalized_handle: normalized_handle.clone(),
                input,
                kind: NormalizedHandleKind::StoreCapsule,
                canonical: Some(canonical),
                cli_ref: Some(normalized_handle),
                sample_recipe_slug: None,
            })
        }
    }
}

fn experimental_guest_driver_from_error(err: &dyn std::error::Error) -> Option<&'static str> {
    let message = err.to_string().to_ascii_lowercase();
    ["tauri", "electron", "wails"]
        .into_iter()
        .find(|driver| message.contains(&format!("unsupported driver '{}'", driver)))
}

fn normalize_curated_store_alias(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.contains('/') {
        return trimmed.to_string();
    }

    let (candidate, version_suffix) = match trimmed.rsplit_once('@') {
        Some((candidate, version)) if !candidate.is_empty() && !version.trim().is_empty() => {
            (candidate.trim(), Some(version.trim()))
        }
        _ => (trimmed, None),
    };

    // Both the new `ato-desktop` alias and the legacy `desky` alias resolve
    // to the canonical control-plane package id so existing scripts /
    // bookmarks keep working post-rename.
    let canonical = if candidate.eq_ignore_ascii_case("ato-desktop")
        || candidate.eq_ignore_ascii_case("desky")
    {
        Some(super::ATO_DESKTOP_PACKAGE_ID)
    } else {
        None
    };

    match (canonical, version_suffix) {
        (Some(scoped_id), Some(version)) => format!("{}@{}", scoped_id, version),
        (Some(scoped_id), None) => scoped_id.to_string(),
        _ => trimmed.to_string(),
    }
}

fn default_launch_plan(
    canonical: Option<CanonicalHandle>,
    snapshot: Option<ResolvedSnapshot>,
    trust_state: TrustState,
) -> LaunchPlan {
    LaunchPlan {
        canonical: canonical.unwrap_or(CanonicalHandle::LocalPath {
            path: PathBuf::from("."),
        }),
        snapshot,
        trust_state,
        initial_isolation: capsule::handle::InitialIsolationPolicy::fail_closed(),
        permission_requests: PermissionRequestPolicy::jit_default(),
    }
}

fn effective_registry_override(
    canonical: &CanonicalHandle,
    registry: Option<&str>,
) -> Option<String> {
    registry
        .map(str::to_string)
        .or_else(|| canonical.registry_url_override().map(str::to_string))
}

fn print_resolution(resolution: &HandleResolution) {
    println!("Input: {}", resolution.input);
    println!("Normalized: {}", resolution.normalized_handle);
    if let Some(canonical) = &resolution.canonical_handle {
        println!("Canonical: {}", canonical);
    }
    println!("Kind: {}", handle_kind_label(&resolution.kind));
    println!(
        "Render strategy: {}",
        render_strategy_label(&resolution.render_strategy)
    );
    if let Some(source) = &resolution.source {
        println!("Source: {}", source);
    }
    println!("Trust: {:?}", resolution.trust_state);
    println!("Restricted: {}", resolution.restricted);

    if let Some(guest) = &resolution.guest {
        println!("Adapter: {}", guest.adapter);
        println!("Frontend: {}", guest.frontend_entry);
        println!("Transport: {} {}", guest.transport, guest.rpc_path);
    }

    if let Some(target) = &resolution.target {
        println!("Target: {}", target.target_label);
        if let Some(runtime) = &target.runtime {
            println!("Runtime: {}", runtime);
        }
        if let Some(driver) = &target.driver {
            println!("Driver: {}", driver);
        }
        if let Some(language) = &target.language {
            println!("Language: {}", language);
        }
        if let Some(port) = target.port {
            println!("Port: {}", port);
        }
        if let Some(manifest_path) = &target.manifest_path {
            println!("Manifest: {}", manifest_path);
        }
    }

    if let Some(launch) = &resolution.launch {
        println!("Launch command: {}", launch.command);
        if !launch.args.is_empty() {
            println!("Launch args: {}", launch.args.join(" "));
        }
        println!("Working dir: {}", launch.working_dir);
    }

    if let Some(snapshot) = &resolution.snapshot {
        println!("Snapshot: {:?}", snapshot);
    }

    for note in &resolution.notes {
        println!("Note: {}", note);
    }
}

fn handle_kind_label(kind: &HandleKind) -> &'static str {
    match kind {
        HandleKind::WebUrl => "web_url",
        HandleKind::LocalCapsule => "local_capsule",
        HandleKind::StoreCapsule => "store_capsule",
        HandleKind::RemoteSourceRef => "remote_source_ref",
        HandleKind::SampleRecipe => "sample_recipe",
    }
}

fn render_strategy_label(strategy: &RenderStrategy) -> &'static str {
    match strategy {
        RenderStrategy::Web => "web",
        RenderStrategy::Terminal => "terminal",
        RenderStrategy::GuestWebview => "guest-webview",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;

    #[test]
    fn normalize_curated_alias_preserves_version_suffix() {
        let normalized = normalize_handle("ato-desktop@1.2.3").expect("normalize alias");
        assert_eq!(normalized.normalized_handle, "ato/ato-desktop@1.2.3");
        assert!(matches!(
            normalized.kind,
            NormalizedHandleKind::StoreCapsule
        ));
    }

    #[test]
    fn normalize_legacy_desky_alias_resolves_to_new_id() {
        let normalized = normalize_handle("desky@1.2.3").expect("normalize legacy alias");
        assert_eq!(normalized.normalized_handle, "ato/ato-desktop@1.2.3");
        assert!(matches!(
            normalized.kind,
            NormalizedHandleKind::StoreCapsule
        ));
    }

    #[test]
    fn normalize_github_source_ref_marks_remote_source() {
        let normalized = normalize_handle("capsule://github.com/acme/editor").expect("normalize");
        assert_eq!(
            normalized.normalized_handle,
            "capsule://github.com/acme/editor"
        );
        assert!(matches!(
            normalized.kind,
            NormalizedHandleKind::RemoteSourceRef
        ));
    }

    #[test]
    fn normalize_loopback_registry_handle_marks_store_source() {
        let normalized =
            normalize_handle("capsule://localhost:8787/acme/editor").expect("normalize");
        assert_eq!(
            normalized.normalized_handle,
            "capsule://localhost:8787/acme/editor"
        );
        assert!(matches!(
            normalized.kind,
            NormalizedHandleKind::StoreCapsule
        ));
    }

    #[test]
    fn build_resolution_for_web_url_uses_web_strategy() {
        let resolution = build_resolution("https://ato.run", None, None).expect("resolve");
        assert_eq!(resolution.kind, HandleKind::WebUrl);
        assert_eq!(resolution.render_strategy, RenderStrategy::Web);
        assert!(resolution.target.is_none());
        assert!(resolution.launch.is_none());
    }

    #[test]
    fn build_resolution_for_local_tauri_manifest_uses_guest_webview() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desky-mock-tauri"
version = "0.1.0"
type = "app"

runtime = "source"
driver = "tauri"
run = "backend/mock-tauri""#,
        )
        .expect("write manifest");

        let resolution = build_resolution(temp.path().to_str().unwrap(), None, None)
            .expect("resolve local tauri manifest");
        assert_eq!(resolution.kind, HandleKind::LocalCapsule);
        assert_eq!(resolution.render_strategy, RenderStrategy::GuestWebview);
        assert_eq!(
            resolution
                .target
                .as_ref()
                .map(|target| target.target_label.as_str()),
            Some("app")
        );
        assert_eq!(
            resolution
                .target
                .as_ref()
                .and_then(|target| target.driver.as_deref()),
            Some("tauri")
        );
    }

    #[test]
    fn bare_alias_memos_resolves_through_sample_recipe() {
        let normalized = normalize_handle("memos").expect("normalize memos");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
            "expected SampleRecipe, got {:?}",
            normalized.kind
        );
        assert_eq!(normalized.sample_recipe_slug.as_deref(), Some("memos"));
    }

    #[test]
    fn bare_alias_uptime_kuma_resolves_through_sample_recipe() {
        let normalized = normalize_handle("uptime-kuma").expect("normalize uptime-kuma");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
            "expected SampleRecipe, got {:?}",
            normalized.kind
        );
    }

    #[test]
    fn github_memos_resolves_through_sample_recipe() {
        let normalized = normalize_handle("capsule://github.com/usememos/memos")
            .expect("normalize github memos");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
            "expected SampleRecipe, got {:?}",
            normalized.kind
        );
        assert_eq!(normalized.sample_recipe_slug.as_deref(), Some("memos"));
        assert_eq!(
            normalized.normalized_handle,
            "capsule://github.com/usememos/memos"
        );
    }

    #[test]
    fn unknown_github_falls_back_to_remote_source_ref() {
        let normalized = normalize_handle("capsule://github.com/unknown/repo")
            .expect("normalize unknown github");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::RemoteSourceRef),
            "expected RemoteSourceRef, got {:?}",
            normalized.kind
        );
        assert!(normalized.sample_recipe_slug.is_none());
    }

    #[test]
    fn local_path_not_hijacked_by_sample_recipe() {
        let temp = TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "memos"
version = "0.1.0"
type = "app"
runtime = "oci""#,
        )
        .expect("write");
        let normalized = normalize_handle(temp.path().to_str().unwrap()).expect("normalize local");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::LocalPath(_)),
            "expected LocalPath, got {:?}",
            normalized.kind
        );
    }

    #[test]
    fn existing_local_dir_named_memos_not_hijacked_by_sample_recipe() {
        let _guard = crate::tests::env_lock().lock().expect("env lock");
        let parent = TempDir::new().expect("parent tempdir");
        let memos_dir = parent.path().join("memos");
        std::fs::create_dir(&memos_dir).expect("create memos dir");
        std::fs::write(
            memos_dir.join("capsule.toml"),
            r#"schema_version = "0.3"
name = "local-memos"
version = "0.1.0"
type = "app"
runtime = "oci""#,
        )
        .expect("write");
        let orig_cwd = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(parent.path()).expect("chdir to parent");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let normalized = normalize_handle("memos")
                .expect("bare 'memos' with existing local dir should resolve");
            assert!(
                matches!(normalized.kind, NormalizedHandleKind::LocalPath(_)),
                "expected LocalPath for existing local dir, got {:?}",
                normalized.kind
            );
            assert!(normalized.sample_recipe_slug.is_none());
        }));
        let _ = std::env::set_current_dir(&orig_cwd);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn build_sample_recipe_memos_has_oci_runtime() {
        let resolution =
            build_resolution("memos", None, None).expect("resolve memos sample recipe");
        assert_eq!(resolution.kind, HandleKind::SampleRecipe);
        assert_eq!(resolution.source.as_deref(), Some("sample_recipe"));
        let target = resolution.target.as_ref().expect("target");
        assert_eq!(target.target_label, "app");
        assert_eq!(target.runtime.as_deref(), Some("oci"));
        assert_eq!(target.port, Some(5230));
        assert!(
            resolution
                .notes
                .iter()
                .any(|n| n.contains("bundled sample recipe"))
        );
    }

    #[test]
    fn build_github_memos_resolves_through_sample_recipe() {
        let resolution = build_resolution("capsule://github.com/usememos/memos", None, None)
            .expect("resolve github memos via sample recipe");
        assert_eq!(resolution.kind, HandleKind::SampleRecipe);
        assert_eq!(resolution.source.as_deref(), Some("sample_recipe"));
    }

    #[test]
    fn build_unknown_github_still_uses_fallback() {
        let result = build_resolution("capsule://github.com/unknown/repo", None, None);
        assert!(
            result.is_err(),
            "unknown GitHub repos should fail resolution (no network in tests)"
        );
    }

    // ── #377: catalog-app GitHub URLs must take the recipe path, never the
    //          raw source-build path (which would require /bin/sh on Windows).

    #[test]
    fn github_excalidraw_resolves_through_sample_recipe() {
        let normalized = normalize_handle("capsule://github.com/excalidraw/excalidraw")
            .expect("normalize github excalidraw");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
            "expected SampleRecipe (recipe path), got {:?}",
            normalized.kind
        );
        assert_eq!(normalized.sample_recipe_slug.as_deref(), Some("excalidraw"));
    }

    #[test]
    fn github_pgweb_resolves_through_sample_recipe() {
        let normalized = normalize_handle("capsule://github.com/sosedoff/pgweb")
            .expect("normalize github pgweb");
        assert!(
            matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
            "expected SampleRecipe (recipe path), got {:?}",
            normalized.kind
        );
        assert_eq!(normalized.sample_recipe_slug.as_deref(), Some("pgweb"));
    }

    /// Desktop strips the `capsule://` scheme before handing the handle to
    /// the CLI, so the bare `github.com/owner/repo` form must resolve to the
    /// same recipe as the scheme-qualified form and the bare alias. If any of
    /// these diverged, a catalog app would silently drop into the raw
    /// source-build path. (#377)
    #[test]
    fn excalidraw_resolution_is_consistent_across_forms() {
        let slug_for = |handle: &str| {
            let normalized =
                normalize_handle(handle).unwrap_or_else(|e| panic!("normalize {handle}: {e}"));
            assert!(
                matches!(normalized.kind, NormalizedHandleKind::SampleRecipe(_)),
                "{handle} expected SampleRecipe, got {:?}",
                normalized.kind
            );
            normalized.sample_recipe_slug
        };

        let alias = slug_for("excalidraw");
        let scheme = slug_for("capsule://github.com/excalidraw/excalidraw");
        let bare = slug_for("github.com/excalidraw/excalidraw");

        assert_eq!(alias.as_deref(), Some("excalidraw"));
        assert_eq!(
            alias, scheme,
            "alias and capsule:// scheme forms must resolve to the same recipe"
        );
        assert_eq!(
            scheme, bare,
            "capsule:// and bare github.com forms must resolve to the same recipe"
        );
    }
}
