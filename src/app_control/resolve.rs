use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use capsule_core::handle::{
    classify_surface_input, CanonicalHandle, HandleInput, InputSurface, LaunchPlan,
    LocalTrustDecisionRecord, PermissionRequestPolicy, ResolvedMetadataCacheEntry,
    ResolvedSnapshot, SurfaceInput, TrustState,
};
use capsule_core::handle_store::{
    load_metadata_cache, metadata_cache_is_fresh, metadata_cache_ttl_seconds, resolve_trust_state,
    store_local_trust_decision, store_metadata_cache,
};
use capsule_core::launch_spec::{derive_launch_spec, LaunchSpecSource};
use capsule_core::router::{
    execution_descriptor_from_manifest_parts, route_manifest, ExecutionProfile, ManifestData,
};

use super::guest_contract::{parse_guest_contract, preview_guest_contract, GuestContract};
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
pub(super) struct HandleResolution {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedHandle {
    input: String,
    normalized_handle: String,
    kind: NormalizedHandleKind,
    canonical: Option<CanonicalHandle>,
    cli_ref: Option<String>,
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
                package_id: super::DESKY_PACKAGE_ID,
                action: ACTION,
                resolution,
            })?
        );
        return Ok(());
    }

    print_resolution(&resolution);
    Ok(())
}

pub(super) fn build_resolution(
    handle: &str,
    target_label: Option<&str>,
    registry: Option<&str>,
) -> Result<HandleResolution> {
    let normalized = normalize_handle(handle)?;

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

    match route_manifest(manifest_path, ExecutionProfile::Release, target_label) {
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
                HashMap::new(),
            )
            .with_context(|| {
                format!(
                    "failed to build experimental Desky execution descriptor at {}",
                    manifest_path.display()
                )
            })?;

            Ok((
                plan,
                guest,
                vec![format!(
                    "Used experimental Desky guest-driver fallback for driver='{driver}'. Core manifest validation does not admit guest drivers yet."
                )],
            ))
        }
    }
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
    persist_metadata_cache(&canonical, &normalized_handle, &plan, snapshot.clone())?;
    let mut notes = vec![
        "Remote store handles currently resolve target metadata only. Launch details become concrete after local materialization.".to_string(),
    ];
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
        render_strategy: render_strategy(&plan, guest.as_ref()),
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
        target: Some(build_target_summary(&plan, None, None)),
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

fn build_launch_preview(spec: capsule_core::launch_spec::LaunchSpec) -> LaunchPreview {
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

    RenderStrategy::Terminal
}

pub(super) fn normalize_handle(raw: &str) -> Result<NormalizedHandle> {
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
        });
    }

    if input.starts_with("ato://") {
        anyhow::bail!(
            "`ato://` is reserved for host routes and cannot be resolved as a capsule handle"
        );
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
        }),
        SurfaceInput::SearchQuery { .. } => {
            let normalized_handle = normalize_curated_store_alias(&input);
            let canonical = capsule_core::handle::normalize_capsule_handle(&normalized_handle)?;
            let _ = parse_capsule_request(&normalized_handle)
                .with_context(|| format!("unsupported handle '{input}'"))?;
            Ok(NormalizedHandle {
                normalized_handle: normalized_handle.clone(),
                input,
                kind: NormalizedHandleKind::StoreCapsule,
                canonical: Some(canonical),
                cli_ref: Some(normalized_handle),
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

    let canonical = if candidate.eq_ignore_ascii_case("desky") {
        Some(super::DESKY_PACKAGE_ID)
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
        initial_isolation: capsule_core::handle::InitialIsolationPolicy::fail_closed(),
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
        let normalized = normalize_handle("desky@1.2.3").expect("normalize alias");
        assert_eq!(normalized.normalized_handle, "ato/desky@1.2.3");
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
}
