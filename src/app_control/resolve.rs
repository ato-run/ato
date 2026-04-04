use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use capsule_core::launch_spec::{derive_launch_spec, LaunchSpecSource};
use capsule_core::router::{
    execution_descriptor_from_manifest_parts, route_manifest, ExecutionProfile, ManifestData,
};

use super::guest_contract::{parse_guest_contract, preview_guest_contract, GuestContract};
use crate::install::{fetch_capsule_manifest_toml, parse_capsule_request};
use crate::local_input::{expand_local_path, should_treat_input_as_local};

const ACTION: &str = "resolve_handle";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HandleKind {
    WebUrl,
    LocalCapsule,
    StoreCapsule,
    RemoteSourceRef,
    StateUri,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum RenderStrategy {
    Web,
    Terminal,
    GuestWebview,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ResolveEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    resolution: HandleResolution,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct HandleResolution {
    input: String,
    normalized_handle: String,
    kind: HandleKind,
    render_strategy: RenderStrategy,
    guest: Option<super::guest_contract::GuestContractPreview>,
    target: Option<TargetSummary>,
    launch: Option<LaunchPreview>,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct TargetSummary {
    target_label: String,
    runtime: Option<String>,
    driver: Option<String>,
    language: Option<String>,
    port: Option<u16>,
    manifest_path: Option<String>,
    workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct LaunchPreview {
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
    StateUri,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedHandle {
    input: String,
    normalized_handle: String,
    kind: NormalizedHandleKind,
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

fn build_resolution(
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
            guest: None,
            target: None,
            launch: None,
            notes: Vec::new(),
        }),
        NormalizedHandleKind::StateUri => Ok(HandleResolution {
            input: normalized.input,
            normalized_handle: normalized.normalized_handle,
            kind: HandleKind::StateUri,
            render_strategy: RenderStrategy::Unsupported,
            guest: None,
            target: None,
            launch: None,
            notes: vec![
                "mag:// state views are not implemented yet; keep this handle in Desky as a future state-view route.".to_string(),
            ],
        }),
        NormalizedHandleKind::RemoteSourceRef => Ok(HandleResolution {
            input: normalized.input,
            normalized_handle: normalized.normalized_handle,
            kind: HandleKind::RemoteSourceRef,
            render_strategy: RenderStrategy::Unsupported,
            guest: None,
            target: None,
            launch: None,
            notes: vec![
                "Remote source handles are not resolved yet. Materialize or infer a capsule first, then retry with a local path or store-scoped handle.".to_string(),
            ],
        }),
        NormalizedHandleKind::LocalPath(path) => build_local_resolution(
            normalized.input,
            normalized.normalized_handle,
            path,
            target_label,
        ),
        NormalizedHandleKind::StoreCapsule => build_store_resolution(
            normalized.input,
            normalized.normalized_handle,
            target_label,
            registry,
        ),
    }
}

fn build_local_resolution(
    input: String,
    normalized_handle: String,
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
        .with_context(|| format!("failed to derive launch spec for {}", manifest_path.display()))?;

    Ok(HandleResolution {
        input,
        normalized_handle,
        kind: HandleKind::LocalCapsule,
        render_strategy: render_strategy(&plan, guest.as_ref()),
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
    let raw = std::fs::read_to_string(manifest_path).with_context(|| {
        format!("failed to read manifest at {}", manifest_path.display())
    })?;
    let raw_manifest: toml::Value = toml::from_str(&raw).with_context(|| {
        format!("failed to parse manifest at {}", manifest_path.display())
    })?;
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
    target_label: Option<&str>,
    registry: Option<&str>,
) -> Result<HandleResolution> {
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let manifest_toml = rt.block_on(fetch_capsule_manifest_toml(&normalized_handle, registry))?;
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

    Ok(HandleResolution {
        input,
        normalized_handle,
        kind: HandleKind::StoreCapsule,
        render_strategy: render_strategy(&plan, guest.as_ref()),
        guest: guest.as_ref().map(preview_guest_contract),
        target: Some(build_target_summary(&plan, None, None)),
        launch: None,
        notes: vec![
            "Remote store handles currently resolve target metadata only. Launch details become concrete after local materialization.".to_string(),
        ],
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
        });
    }

    if input.starts_with("mag://") {
        return Ok(NormalizedHandle {
            normalized_handle: input.clone(),
            input,
            kind: NormalizedHandleKind::StateUri,
        });
    }

    if let Some(rest) = input.strip_prefix("capsule://store/") {
        return Ok(NormalizedHandle {
            normalized_handle: normalize_curated_store_alias(rest),
            input,
            kind: NormalizedHandleKind::StoreCapsule,
        });
    }

    if let Some(rest) = input.strip_prefix("capsule://github.com/") {
        return Ok(NormalizedHandle {
            normalized_handle: format!("github.com/{rest}"),
            input,
            kind: NormalizedHandleKind::RemoteSourceRef,
        });
    }

    let expanded_path = expand_local_path(&input);
    if should_treat_input_as_local(&input, &expanded_path) {
        let canonical = expanded_path.canonicalize().with_context(|| {
            format!("failed to resolve local path '{}'", expanded_path.display())
        })?;
        return Ok(NormalizedHandle {
            normalized_handle: canonical.display().to_string(),
            input,
            kind: NormalizedHandleKind::LocalPath(canonical),
        });
    }

    if input.starts_with("github.com/") {
        return Ok(NormalizedHandle {
            normalized_handle: input.clone(),
            input,
            kind: NormalizedHandleKind::RemoteSourceRef,
        });
    }

    let normalized_handle = normalize_curated_store_alias(&input);
    let _ = parse_capsule_request(&normalized_handle)
        .with_context(|| format!("unsupported handle '{input}'"))?;

    Ok(NormalizedHandle {
        normalized_handle,
        input,
        kind: NormalizedHandleKind::StoreCapsule,
    })
}

pub(super) fn normalize_local_handle(raw: &str) -> Result<PathBuf> {
    let normalized = normalize_handle(raw)?;
    match normalized.kind {
        NormalizedHandleKind::LocalPath(path) => Ok(path),
        _ => anyhow::bail!(
            "session start currently supports local capsule paths only; got '{}'",
            raw
        ),
    }
}

pub(super) fn derive_local_launch_plan(
    path: &std::path::Path,
    target_label: Option<&str>,
) -> Result<(PathBuf, ManifestData, capsule_core::launch_spec::LaunchSpec, Vec<String>)> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    };
    let (plan, _guest, notes) = resolve_local_plan(&manifest_path, target_label)?;
    let launch = derive_launch_spec(&plan)
        .with_context(|| format!("failed to derive launch spec for {}", manifest_path.display()))?;
    Ok((manifest_path, plan, launch, notes))
}

fn experimental_guest_driver_from_error(err: &anyhow::Error) -> Option<&'static str> {
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

fn print_resolution(resolution: &HandleResolution) {
    println!("Input: {}", resolution.input);
    println!("Normalized: {}", resolution.normalized_handle);
    println!("Kind: {}", handle_kind_label(&resolution.kind));
    println!("Render strategy: {}", render_strategy_label(&resolution.render_strategy));

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
        HandleKind::StateUri => "state_uri",
    }
}

fn render_strategy_label(strategy: &RenderStrategy) -> &'static str {
    match strategy {
        RenderStrategy::Web => "web",
        RenderStrategy::Terminal => "terminal",
        RenderStrategy::GuestWebview => "guest-webview",
        RenderStrategy::Unsupported => "unsupported",
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
        assert!(matches!(normalized.kind, NormalizedHandleKind::StoreCapsule));
    }

    #[test]
    fn normalize_github_source_ref_marks_remote_source() {
        let normalized = normalize_handle("capsule://github.com/acme/editor").expect("normalize");
        assert_eq!(normalized.normalized_handle, "github.com/acme/editor");
        assert!(matches!(normalized.kind, NormalizedHandleKind::RemoteSourceRef));
    }

    #[test]
    fn build_resolution_for_web_url_uses_web_strategy() {
        let resolution = build_resolution("https://store.ato.run", None, None).expect("resolve");
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
            r#"schema_version = "0.2"
name = "desky-mock-tauri"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "tauri"
entrypoint = "backend/mock-tauri"
"#,
        )
        .expect("write manifest");

        let resolution = build_resolution(temp.path().to_str().unwrap(), None, None)
            .expect("resolve local tauri manifest");
        assert_eq!(resolution.kind, HandleKind::LocalCapsule);
        assert_eq!(resolution.render_strategy, RenderStrategy::GuestWebview);
        assert_eq!(resolution.target.as_ref().map(|target| target.target_label.as_str()), Some("desktop"));
        assert_eq!(resolution.launch.as_ref().map(|launch| launch.command.as_str()), Some("backend/mock-tauri"));
    }
}
