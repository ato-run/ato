use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet};
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use capsule_core::CapsuleReporter;
use capsule_core::router::ManifestData;

use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;

use super::dependency_root;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecyclePhase {
    Install,
    Build,
    Run,
}

impl LifecyclePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Build => "build",
            Self::Run => "run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecyclePathPlan {
    pub phase: LifecyclePhase,
    pub ato_toolchain_bins: Vec<PathBuf>,
    pub dependency_output_bins: Vec<PathBuf>,
    pub minimal_system_bins: Vec<PathBuf>,
    /// Directories appended last for lifecycle tools ato does not manage
    /// (e.g. `cargo` from rustup) — see
    /// [`host_fallback_for_unmanaged_command`]. Always paired with a
    /// `host_fallback` provenance entry and a degraded marker.
    pub host_fallback_bins: Vec<PathBuf>,
    pub provenance: LifecyclePathProvenance,
}

impl LifecyclePathPlan {
    pub fn path_entries(&self) -> Vec<PathBuf> {
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();

        for path in &self.ato_toolchain_bins {
            push_unique_path(&mut entries, &mut seen, path.clone());
        }
        if !matches!(self.phase, LifecyclePhase::Install) {
            for path in &self.dependency_output_bins {
                push_unique_path(&mut entries, &mut seen, path.clone());
            }
        }
        for path in &self.minimal_system_bins {
            push_unique_path(&mut entries, &mut seen, path.clone());
        }
        for path in &self.host_fallback_bins {
            push_unique_path(&mut entries, &mut seen, path.clone());
        }

        entries
    }

    pub fn path_env(&self) -> Result<OsString> {
        std::env::join_paths(self.path_entries()).context("failed to join lifecycle PATH entries")
    }

    pub async fn emit_degraded_markers(&self, reporter: &Arc<CliReporter>) {
        for toolchain in &self.provenance.ato_toolchains {
            let package_manager_source = match toolchain.source {
                ToolchainSource::HostFallback => Some("host_fallback"),
                ToolchainSource::Unavailable => Some("unavailable"),
                ToolchainSource::Managed => None,
            };
            let Some(package_manager_source) = package_manager_source else {
                continue;
            };

            let requested_version = toolchain
                .requested_version
                .clone()
                .unwrap_or_else(|| "unspecified".to_string());
            let resolved_version = toolchain
                .resolved_version
                .clone()
                .or_else(|| {
                    matches!(toolchain.source, ToolchainSource::HostFallback)
                        .then(|| detect_host_tool_version(&toolchain.tool))
                        .flatten()
                })
                .unwrap_or_else(|| "unknown".to_string());
            let marker = format!(
                "degraded=true phase={} package_manager={} requested_version={} resolved_version={} package_manager_source={} logical_id={}",
                self.phase.as_str(),
                toolchain.tool,
                requested_version,
                resolved_version,
                package_manager_source,
                toolchain.logical_id,
            );
            let _ = reporter.warn(marker.clone()).await;
            tracing::warn!(
                phase = self.phase.as_str(),
                tool = %toolchain.tool,
                requested_version = %requested_version,
                resolved_version = %resolved_version,
                logical_id = %toolchain.logical_id,
                package_manager_source = package_manager_source,
                degraded = true,
                "{marker}"
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MaterializedLifecycleToolchains {
    bins: Vec<PathBuf>,
    provenance: Vec<ToolchainProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LifecyclePathProvenance {
    pub ato_toolchains: Vec<ToolchainProvenance>,
    pub dependency_bins: Vec<DependencyBinProvenance>,
    pub minimal_system_bins: Vec<SystemBinProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolchainProvenance {
    pub tool: String,
    pub requested_version: Option<String>,
    pub resolved_version: Option<String>,
    pub logical_id: String,
    pub source: ToolchainSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolchainSource {
    Managed,
    HostFallback,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyBinProvenance {
    pub role: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemBinProvenance {
    pub platform: String,
    pub path: String,
}

pub(crate) async fn build_lifecycle_path_plan(
    plan: &ManifestData,
    phase: LifecyclePhase,
    command: &str,
    lifecycle_roots: &[PathBuf],
    materialized_toolchains: MaterializedLifecycleToolchains,
    reporter: &Arc<CliReporter>,
) -> Result<LifecyclePathPlan> {
    let dependency_root = dependency_root(plan);
    let (dependency_output_bins, dependency_bins) = if matches!(phase, LifecyclePhase::Install) {
        (Vec::new(), Vec::new())
    } else {
        collect_dependency_output_bins(&dependency_root, lifecycle_roots)?
    };
    let minimal_system_bins = minimal_system_bins();
    let minimal_system_provenance = minimal_system_bins
        .iter()
        .map(|path| SystemBinProvenance {
            platform: current_platform().to_string(),
            path: path.display().to_string(),
        })
        .collect();

    let mut plan = LifecyclePathPlan {
        phase,
        ato_toolchain_bins: materialized_toolchains.bins,
        dependency_output_bins,
        minimal_system_bins,
        host_fallback_bins: Vec::new(),
        provenance: LifecyclePathProvenance {
            ato_toolchains: materialized_toolchains.provenance,
            dependency_bins,
            minimal_system_bins: minimal_system_provenance,
        },
    };
    if let Some((bin_dir, fallback_provenance)) =
        host_fallback_for_unmanaged_command(&plan, command)
    {
        plan.host_fallback_bins.push(bin_dir);
        plan.provenance.ato_toolchains.push(fallback_provenance);
    }
    plan.emit_degraded_markers(reporter).await;
    emit_command_tool_resolution_marker(&plan, command, reporter).await?;
    tracing::debug!(
        phase = plan.phase.as_str(),
        ato_toolchain_bins = ?plan.ato_toolchain_bins,
        dependency_output_bins = ?plan.dependency_output_bins,
        minimal_system_bins = ?plan.minimal_system_bins,
        "resolved lifecycle PATH plan"
    );
    Ok(plan)
}

pub(crate) fn materialize_lifecycle_toolchains(
    plan: &ManifestData,
    command: &str,
    reporter: &Arc<CliReporter>,
) -> Result<MaterializedLifecycleToolchains> {
    let mut bins = Vec::new();
    let mut provenance = Vec::new();
    let mut seen = BTreeSet::new();
    let mut managed_node_bin = None;
    let command_name = leading_command_name(command);
    let runtime_tools = lifecycle_runtime_tools(plan, command_name)?;
    let node_required = plan
        .execution_driver()
        .map(|driver| driver.trim().eq_ignore_ascii_case("node"))
        .unwrap_or(false)
        || matches!(command_name, Some("npm" | "npx" | "pnpm" | "yarn"))
        || runtime_tools
            .iter()
            .any(|tool| tool.spec.depends_on.contains(&"node"));

    if node_required {
        match runtime_manager::ensure_node_binary_with_authority(plan, None) {
            Ok(node_bin) => {
                if let Some(node_dir) = node_bin.parent() {
                    push_unique_path(&mut bins, &mut seen, node_dir.to_path_buf());
                    managed_node_bin = Some(node_bin.clone());
                    provenance.push(ToolchainProvenance {
                        tool: "node".to_string(),
                        requested_version: plan.execution_runtime_version(),
                        resolved_version: plan.execution_runtime_version(),
                        logical_id: logical_toolchain_id(
                            "node",
                            plan.execution_runtime_version().as_deref(),
                            ToolchainSource::Managed,
                        ),
                        source: ToolchainSource::Managed,
                    });
                }
            }
            Err(error) => {
                tracing::warn!(
                    tool = "node",
                    error = %error,
                    toolchain_source = "unavailable",
                    "managed node runtime is unavailable for lifecycle PATH planning"
                );
                if matches!(command_name, Some("npm" | "npx")) {
                    provenance.push(ToolchainProvenance {
                        tool: command_name.unwrap_or("npm").to_string(),
                        requested_version: None,
                        resolved_version: None,
                        logical_id: logical_toolchain_id(
                            command_name.unwrap_or("npm"),
                            None,
                            ToolchainSource::Unavailable,
                        ),
                        source: ToolchainSource::Unavailable,
                    });
                }
            }
        }
    }

    for tool in runtime_tools {
        let spec = tool.spec;
        let requested_version = tool.requested_version;
        let mut deps = capsule_core::tools::ToolDeps::default();
        if spec.depends_on.contains(&"node") {
            deps.node_bin = managed_node_bin.clone();
        }
        let reporter_dyn: Arc<dyn CapsuleReporter + 'static> = reporter.clone();
        let handle =
            ensure_runtime_tool_for_lifecycle(spec, requested_version.clone(), deps, reporter_dyn)?;
        let managed_tool_root = capsule_core::common::paths::toolchain_cache_dir()?
            .join("tools")
            .join(spec.name);
        let source = if handle.bin_dir.starts_with(&managed_tool_root) {
            ToolchainSource::Managed
        } else {
            ToolchainSource::HostFallback
        };
        push_unique_path(&mut bins, &mut seen, handle.bin_dir.clone());
        provenance.push(ToolchainProvenance {
            tool: spec.name.to_string(),
            requested_version: requested_version.clone(),
            resolved_version: if handle.version.is_empty() {
                None
            } else {
                Some(handle.version.clone())
            },
            logical_id: logical_toolchain_id(
                spec.name,
                requested_version
                    .as_deref()
                    .or_else(|| (!handle.version.is_empty()).then_some(handle.version.as_str())),
                source.clone(),
            ),
            source,
        });
    }

    if matches!(command_name, Some("npm" | "npx")) && managed_node_bin.is_some() {
        provenance.push(ToolchainProvenance {
            tool: command_name.unwrap_or("npm").to_string(),
            requested_version: None,
            resolved_version: plan.execution_runtime_version(),
            logical_id: logical_toolchain_id(
                command_name.unwrap_or("npm"),
                plan.execution_runtime_version().as_deref(),
                ToolchainSource::Managed,
            ),
            source: ToolchainSource::Managed,
        });
    }

    Ok(MaterializedLifecycleToolchains { bins, provenance })
}

#[derive(Clone)]
struct LifecycleRuntimeTool {
    spec: &'static capsule_core::tools::RuntimeToolSpec,
    requested_version: Option<String>,
}

fn lifecycle_runtime_tools(
    plan: &ManifestData,
    command_name: Option<&str>,
) -> Result<Vec<LifecycleRuntimeTool>> {
    let mut tools = Vec::new();
    let mut seen = BTreeSet::new();

    for spec in capsule_core::tools::registry() {
        let Some(requested_version) = capsule_core::tools::read_tool_version(
            &plan.manifest,
            plan.selected_target_label(),
            spec.name,
        ) else {
            continue;
        };
        seen.insert(spec.name);
        tools.push(LifecycleRuntimeTool {
            spec,
            requested_version: Some(requested_version),
        });
    }

    if let Some(tool_name) = command_name.and_then(runtime_tool_name_for_command)
        && seen.insert(tool_name)
    {
        let spec = capsule_core::tools::lookup(tool_name)
            .with_context(|| format!("runtime tool '{tool_name}' is not registered"))?;
        tools.push(LifecycleRuntimeTool {
            spec,
            requested_version: None,
        });
    }

    Ok(tools)
}

fn ensure_runtime_tool_for_lifecycle(
    spec: &'static capsule_core::tools::RuntimeToolSpec,
    requested_version: Option<String>,
    deps: capsule_core::tools::ToolDeps,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<capsule_core::tools::ToolHandle> {
    block_on_runtime_tool_materialization(async move {
        capsule_core::tools::ensure_runtime_tool(
            spec,
            requested_version.as_deref(),
            &deps,
            reporter,
        )
        .await
    })
}

fn block_on_runtime_tool_materialization<F>(future: F) -> Result<capsule_core::tools::ToolHandle>
where
    F: std::future::Future<Output = capsule_core::Result<capsule_core::tools::ToolHandle>>
        + Send
        + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let materialize = move || -> Result<capsule_core::tools::ToolHandle> {
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)?;
                runtime
                    .block_on(future)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("runtime tool materialization thread panicked"))?
        };
        return match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(materialize),
            _ => materialize(),
        };
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime
        .block_on(future)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn collect_dependency_output_bins(
    primary_root: &Path,
    lifecycle_roots: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<DependencyBinProvenance>)> {
    let mut roots = Vec::new();
    let mut seen_roots = BTreeSet::new();
    push_unique_dependency_root(
        &mut roots,
        &mut seen_roots,
        primary_root.to_path_buf(),
        DependencyRootKind::Primary,
    );
    for workspace_root in detect_workspace_roots(primary_root)? {
        push_unique_dependency_root(
            &mut roots,
            &mut seen_roots,
            workspace_root,
            DependencyRootKind::Workspace,
        );
    }
    for root in lifecycle_roots {
        push_unique_dependency_root(
            &mut roots,
            &mut seen_roots,
            root.clone(),
            DependencyRootKind::Lifecycle,
        );
    }

    let mut bins = Vec::new();
    let mut provenance = Vec::new();
    let mut seen_bins = BTreeSet::new();
    for (root, kind) in roots {
        let bin_dir = root.join("node_modules").join(".bin");
        if !bin_dir.is_dir() {
            continue;
        }
        let fingerprint = fs::canonicalize(&bin_dir).unwrap_or_else(|_| bin_dir.clone());
        if !seen_bins.insert(fingerprint) {
            continue;
        }
        provenance.push(dependency_bin_provenance(primary_root, &root, kind));
        bins.push(bin_dir);
    }

    Ok((bins, provenance))
}

#[derive(Debug, Clone, Copy)]
enum DependencyRootKind {
    Primary,
    Workspace,
    Lifecycle,
}

fn dependency_bin_provenance(
    primary_root: &Path,
    root: &Path,
    kind: DependencyRootKind,
) -> DependencyBinProvenance {
    match kind {
        DependencyRootKind::Primary => DependencyBinProvenance {
            role: "root".to_string(),
            relative_path: "./node_modules/.bin".to_string(),
        },
        DependencyRootKind::Workspace => {
            let relative = root.strip_prefix(primary_root).unwrap_or(root);
            let rel = relative.to_string_lossy().replace('\\', "/");
            DependencyBinProvenance {
                role: "workspace".to_string(),
                relative_path: format!("{rel}/node_modules/.bin"),
            }
        }
        DependencyRootKind::Lifecycle => DependencyBinProvenance {
            role: "lifecycle-root".to_string(),
            // Provenance strings are observability data; normalize to `/`
            // like the workspace arm so records read identically across
            // platforms.
            relative_path: root
                .join("node_modules/.bin")
                .display()
                .to_string()
                .replace('\\', "/"),
        },
    }
}

fn push_unique_dependency_root(
    roots: &mut Vec<(PathBuf, DependencyRootKind)>,
    seen: &mut BTreeSet<PathBuf>,
    path: PathBuf,
    kind: DependencyRootKind,
) {
    if seen.insert(path.clone()) {
        roots.push((path, kind));
    }
}

fn detect_workspace_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let Some(patterns) = detect_workspace_patterns(root)? else {
        return Ok(Vec::new());
    };
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let matcher = build_workspace_matcher(&patterns)?;
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(4)
        .into_iter()
        .filter_entry(should_walk_workspace_entry)
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        if relative_str.is_empty() || !matcher.is_match(&relative_str) {
            continue;
        }
        if !entry.path().join("package.json").is_file() {
            continue;
        }
        push_unique_path(&mut roots, &mut seen, entry.into_path());
    }

    roots.sort();
    Ok(roots)
}

fn detect_workspace_patterns(root: &Path) -> Result<Option<Vec<String>>> {
    let pnpm_workspace = root.join("pnpm-workspace.yaml");
    if pnpm_workspace.is_file() {
        let text = fs::read_to_string(&pnpm_workspace)
            .with_context(|| format!("failed to read {}", pnpm_workspace.display()))?;
        let packages: Vec<String> = text
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                trimmed.strip_prefix("- ").map(|value| {
                    value
                        .trim()
                        .trim_matches('\'')
                        .trim_matches('"')
                        .to_string()
                })
            })
            .filter(|value| !value.is_empty())
            .collect();
        if !packages.is_empty() {
            return Ok(Some(packages));
        }
    }

    let package_json = root.join("package.json");
    if !package_json.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&package_json)
        .with_context(|| format!("failed to read {}", package_json.display()))?;
    let parsed = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("failed to parse {}", package_json.display()))?;
    let Some(workspaces) = parsed.get("workspaces") else {
        return Ok(None);
    };

    let packages = if let Some(array) = workspaces.as_array() {
        array
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    } else if let Some(packages) = workspaces.get("packages").and_then(Value::as_array) {
        packages
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if packages.is_empty() {
        Ok(None)
    } else {
        Ok(Some(packages))
    }
}

fn build_workspace_matcher(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| {
            format!("invalid workspace pattern '{pattern}' in package.json/pnpm-workspace.yaml")
        })?);
    }
    builder.build().context("failed to build workspace matcher")
}

fn should_walk_workspace_entry(entry: &DirEntry) -> bool {
    let file_name = entry.file_name().to_string_lossy();
    !(file_name.starts_with('.') || file_name == "node_modules")
}

fn leading_command_name(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .next()
        .map(|token| token.trim_matches(|c: char| c == '"' || c == '\''))
        .filter(|token| !token.is_empty())
}

fn runtime_tool_name_for_command(command_name: &str) -> Option<&'static str> {
    match command_name {
        "bun" => Some("bun"),
        "pnpm" => Some("pnpm"),
        "yarn" => Some("yarn"),
        "uv" => Some("uv"),
        _ => None,
    }
}

/// Host fallback for lifecycle tools ato does not manage (e.g. `cargo`).
///
/// The sanitized lifecycle PATH carries managed toolchains, dependency
/// outputs, and the minimal system dirs only — a manifest command like
/// `cargo fetch --locked` could never resolve on rustup hosts
/// (`~/.cargo/bin` is not a system dir) and died with a bare exit 127.
/// When the command's leading tool is neither a managed runtime tool nor
/// resolvable from the planned PATH, resolve it from the parent process's
/// PATH and expose only its directory, appended after every managed entry
/// and paired with a degraded `host_fallback` marker — the same contract the
/// managed-tool host fallback already uses.
fn host_fallback_for_unmanaged_command(
    plan: &LifecyclePathPlan,
    command: &str,
) -> Option<(PathBuf, ToolchainProvenance)> {
    let command_name = leading_command_name(command)?;
    if runtime_tool_name_for_command(command_name).is_some() {
        // The managed-tool flow owns this command; never widen its PATH here.
        return None;
    }
    let path_env = plan.path_env().ok()?;
    let cwd = std::env::current_dir().ok()?;
    if which::which_in(command_name, Some(&path_env), &cwd).is_ok() {
        return None;
    }
    let host_path = which::which(command_name).ok()?;
    let bin_dir = host_path.parent()?.to_path_buf();
    let provenance = ToolchainProvenance {
        tool: command_name.to_string(),
        requested_version: None,
        resolved_version: detect_host_tool_version(command_name),
        logical_id: logical_toolchain_id(command_name, None, ToolchainSource::HostFallback),
        source: ToolchainSource::HostFallback,
    };
    Some((bin_dir, provenance))
}

fn minimal_system_bins() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut bins = Vec::new();
        if let Some(system_root) = std::env::var_os("SystemRoot").map(PathBuf::from) {
            let system32 = system_root.join("System32");
            if system32.is_dir() {
                bins.push(system32);
            }
            if system_root.is_dir() {
                bins.push(system_root);
            }
        }
        bins
    }

    #[cfg(not(windows))]
    {
        [PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
            .into_iter()
            .filter(|path| path.is_dir())
            .collect()
    }
}

fn logical_toolchain_id(tool: &str, version: Option<&str>, source: ToolchainSource) -> String {
    match source {
        ToolchainSource::Managed => format!(
            "{tool}@{}:{}",
            version.unwrap_or("unknown"),
            current_platform()
        ),
        ToolchainSource::HostFallback => format!("host-fallback:{tool}"),
        ToolchainSource::Unavailable => format!("unavailable:{tool}"),
    }
}

async fn emit_command_tool_resolution_marker(
    path_plan: &LifecyclePathPlan,
    command: &str,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let Some(command_name) = leading_command_name(command) else {
        return Ok(());
    };
    let Some(tool_name) = runtime_tool_name_for_command(command_name) else {
        return Ok(());
    };

    let path_env = path_plan.path_env()?;
    let cwd = std::env::current_dir()?;
    let Some(resolved_path) = which::which_in(tool_name, Some(path_env), cwd).ok() else {
        return Ok(());
    };
    let managed_tool_root = capsule_core::common::paths::toolchain_cache_dir()?
        .join("tools")
        .join(tool_name);
    if resolved_path.starts_with(&managed_tool_root) {
        return Ok(());
    }

    let resolved_version =
        detect_host_tool_version(tool_name).unwrap_or_else(|| "unknown".to_string());
    let marker = format!(
        "degraded=true phase={} package_manager={} requested_version=unspecified resolved_version={} package_manager_source=host_fallback resolved_path={}",
        path_plan.phase.as_str(),
        tool_name,
        resolved_version,
        resolved_path.display(),
    );
    let _ = reporter.warn(marker.clone()).await;
    tracing::warn!(
        phase = path_plan.phase.as_str(),
        package_manager = tool_name,
        resolved_version = %resolved_version,
        package_manager_source = "host_fallback",
        resolved_path = %resolved_path.display(),
        degraded = true,
        "{marker}"
    );
    Ok(())
}

fn detect_host_tool_version(tool: &str) -> Option<String> {
    let output = std::process::Command::new(tool)
        .arg("--version")
        .output()
        .ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8(output.stderr).ok()?
    } else {
        String::from_utf8(output.stdout).ok()?
    };
    text.split_whitespace()
        .map(|token| token.trim().trim_start_matches('v').trim_matches(','))
        .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .map(str::to_string)
}

fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut BTreeSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tempfile::tempdir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn build_plan(manifest_dir: &Path, manifest: &str) -> ManifestData {
        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            manifest_dir.join("capsule.toml"),
            manifest_dir.to_path_buf(),
            capsule_core::router::ExecutionProfile::Dev,
            Some("app"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn install_phase_omits_dependency_output_bins_from_path_entries() {
        let plan = LifecyclePathPlan {
            phase: LifecyclePhase::Install,
            ato_toolchain_bins: vec![PathBuf::from("/managed/node/bin")],
            dependency_output_bins: vec![PathBuf::from("/repo/node_modules/.bin")],
            minimal_system_bins: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
            host_fallback_bins: Vec::new(),
            provenance: LifecyclePathProvenance::default(),
        };

        assert_eq!(
            plan.path_entries(),
            vec![
                PathBuf::from("/managed/node/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ]
        );
    }

    #[test]
    fn build_phase_keeps_toolchains_before_dependency_output_bins() {
        let plan = LifecyclePathPlan {
            phase: LifecyclePhase::Build,
            ato_toolchain_bins: vec![
                PathBuf::from("/managed/node/bin"),
                PathBuf::from("/managed/bun/bin"),
            ],
            dependency_output_bins: vec![
                PathBuf::from("/repo/node_modules/.bin"),
                PathBuf::from("/repo/server/node_modules/.bin"),
            ],
            minimal_system_bins: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
            host_fallback_bins: Vec::new(),
            provenance: LifecyclePathProvenance::default(),
        };

        assert_eq!(
            plan.path_entries(),
            vec![
                PathBuf::from("/managed/node/bin"),
                PathBuf::from("/managed/bun/bin"),
                PathBuf::from("/repo/node_modules/.bin"),
                PathBuf::from("/repo/server/node_modules/.bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ]
        );
    }

    #[test]
    fn dependency_output_bins_cover_root_workspaces_and_lifecycle_roots() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("node_modules/.bin")).expect("root bin dir");
        fs::create_dir_all(temp.path().join("apps/server/node_modules/.bin"))
            .expect("server bin dir");
        fs::create_dir_all(temp.path().join("apps/app/node_modules/.bin")).expect("app bin dir");
        fs::create_dir_all(temp.path().join("admin/node_modules/.bin")).expect("admin bin dir");
        fs::write(
            temp.path().join("package.json"),
            r#"{
  "workspaces": ["apps/*"]
}"#,
        )
        .expect("write package.json");
        fs::write(temp.path().join("apps/server/package.json"), "{}\n").expect("server package");
        fs::write(temp.path().join("apps/app/package.json"), "{}\n").expect("app package");

        let (bins, provenance) =
            collect_dependency_output_bins(temp.path(), &[temp.path().join("admin")])
                .expect("collect bins");

        assert_eq!(
            bins,
            vec![
                temp.path().join("node_modules/.bin"),
                temp.path().join("apps/app/node_modules/.bin"),
                temp.path().join("apps/server/node_modules/.bin"),
                temp.path().join("admin/node_modules/.bin"),
            ]
        );
        assert_eq!(
            provenance,
            vec![
                DependencyBinProvenance {
                    role: "root".to_string(),
                    relative_path: "./node_modules/.bin".to_string(),
                },
                DependencyBinProvenance {
                    role: "workspace".to_string(),
                    relative_path: "apps/app/node_modules/.bin".to_string(),
                },
                DependencyBinProvenance {
                    role: "workspace".to_string(),
                    relative_path: "apps/server/node_modules/.bin".to_string(),
                },
                DependencyBinProvenance {
                    role: "lifecycle-root".to_string(),
                    relative_path: temp
                        .path()
                        .join("admin/node_modules/.bin")
                        .display()
                        .to_string()
                        .replace('\\', "/"),
                },
            ]
        );
    }

    /// Fabricates a complete, validation-passing bun 1.2.8 cache entry
    /// (shim + extracted tool entry + `binary.sha256` marker) so the
    /// materialization path is exercised hermetically — an incomplete fake
    /// would be discarded by cache validation and trigger a real download.
    fn write_complete_fake_bun_cache(ato_home: &Path) -> PathBuf {
        let tools_root = ato_home.join("toolchains/tools/bun/1.2.8");
        let extracted_dir = tools_root.join("extracted");
        let shim_dir = tools_root.join("shim");

        let entry_rel = capsule_core::tools::resolved_tool_entry_relpath(&capsule_core::tools::BUN)
            .expect("bun layout");
        let target = extracted_dir.join(entry_rel);
        fs::create_dir_all(target.parent().expect("target parent")).expect("extracted dir");
        fs::write(&target, "fake bun binary").expect("bun target");

        fs::create_dir_all(&shim_dir).expect("bun shim dir");
        let shim = shim_dir.join(if cfg!(windows) { "bun.cmd" } else { "bun" });
        fs::write(&shim, "cached bun shim").expect("bun shim");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&target, &shim] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
            }
        }

        fs::write(tools_root.join("binary.sha256"), "a".repeat(64)).expect("sha marker");
        shim_dir
    }

    #[test]
    #[serial_test::serial]
    fn runtime_tool_materialization_builds_tokio_boundary_without_outer_runtime() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let ato_home = tempdir().expect("ato home");
        let _ato_home = EnvGuard::set("ATO_HOME", ato_home.path());
        let shim_dir = write_complete_fake_bun_cache(ato_home.path());

        let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(CliReporter::new_run(false));
        let handle = ensure_runtime_tool_for_lifecycle(
            &capsule_core::tools::BUN,
            Some("1.2.8".to_string()),
            capsule_core::tools::ToolDeps::default(),
            reporter,
        )
        .expect("materialized bun");

        assert_eq!(handle.bin_dir, shim_dir);
    }

    #[test]
    #[serial_test::serial]
    fn declared_runtime_tools_are_materialized_for_shell_build_scripts() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let ato_home = tempdir().expect("ato home");
        let _ato_home = EnvGuard::set("ATO_HOME", ato_home.path());
        let shim_dir = write_complete_fake_bun_cache(ato_home.path());
        let plan = build_plan(
            ato_home.path(),
            r#"
name = "demo"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "native"
run_command = "./demo"
runtime_tools = { bun = "1.2.8" }
"#,
        );
        let reporter = Arc::new(CliReporter::new_run(false));

        let toolchains =
            materialize_lifecycle_toolchains(&plan, "set -e\n\nturbo run build:web", &reporter)
                .expect("materialize declared runtime tool");

        assert_eq!(toolchains.bins, vec![shim_dir]);
        assert_eq!(toolchains.provenance.len(), 1);
        assert_eq!(toolchains.provenance[0].tool, "bun");
        assert_eq!(
            toolchains.provenance[0].requested_version.as_deref(),
            Some("1.2.8")
        );
        assert_eq!(toolchains.provenance[0].source, ToolchainSource::Managed);
    }

    #[test]
    #[serial_test::serial]
    fn top_level_runtime_tools_bun_materialized_for_build_and_run() {
        // ato#723: a flat v0.3 manifest declaring `runtime_tools = { bun = "1.2" }`
        // at the top level must put Bun on the lifecycle PATH for BOTH the build
        // command and the run command (they share `lifecycle_runtime_tools`).
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let ato_home = tempdir().expect("ato home");
        let _ato_home = EnvGuard::set("ATO_HOME", ato_home.path());

        // Seed a *complete, valid* Bun cache for the declared pin so the shared
        // planner serves it offline (a partial pin like "1.2" is not a real
        // release tag, so any network fallback would 404). This mirrors the
        // layout `ensure_runtime_tool` validates: an executable shim, an
        // executable extracted binary under a single top-level dir, and a
        // 64-hex `binary.sha256` integrity marker.
        let tools_root = ato_home.path().join("toolchains/tools/bun/1.2");
        let shim_dir = tools_root.join("shim");
        // Place the extracted binary at the platform-resolved entry path the
        // cache validator expects (`windows-x64/bun.exe` on Windows, not a
        // unix `bun`); otherwise validation misses, the planner treats the
        // cache as incomplete, and falls back to a real network download —
        // which 404s for the partial "1.2" pin (seen only once the windows
        // test leg became blocking).
        let entry_rel = capsule_core::tools::resolved_tool_entry_relpath(&capsule_core::tools::BUN)
            .expect("bun layout");
        let extracted_bin = tools_root.join("extracted").join(&entry_rel);
        let write_executable = |path: &std::path::Path, contents: &str| {
            fs::create_dir_all(path.parent().unwrap()).expect("tool dir");
            fs::write(path, contents).expect("tool file");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(path).expect("metadata").permissions();
                perms.set_mode(0o755);
                fs::set_permissions(path, perms).expect("chmod");
            }
        };
        write_executable(
            &shim_dir.join(if cfg!(windows) { "bun.cmd" } else { "bun" }),
            "#!/bin/sh\nexec bun \"$@\"\n",
        );
        write_executable(&extracted_bin, "#!/bin/sh\necho bun\n");
        fs::write(tools_root.join("binary.sha256"), "a".repeat(64)).expect("sha");

        let plan = build_plan(
            ato_home.path(),
            r#"
schema_version = "0.3"
name = "next-bun-sqlite-app"
type = "app"

runtime = "source/node"
runtime_version = "20"
runtime_tools = { bun = "1.2" }

build = "bun install"
run = "bun run start"
port = 3000
"#,
        );

        // Sanity: the fold placed bun under the selected target so the raw
        // manifest read by lifecycle_runtime_tools resolves it.
        let tools = lifecycle_runtime_tools(&plan, Some("bun")).expect("lifecycle runtime tools");
        let bun = tools
            .iter()
            .find(|tool| tool.spec.name == "bun")
            .expect("bun spec present");
        assert_eq!(bun.requested_version.as_deref(), Some("1.2"));

        let reporter = Arc::new(CliReporter::new_run(false));

        // Build command.
        let build = materialize_lifecycle_toolchains(&plan, "bun install", &reporter)
            .expect("materialize for build");
        assert!(
            build.bins.contains(&shim_dir),
            "bun shim must be on the build PATH"
        );
        assert!(
            build
                .provenance
                .iter()
                .any(|p| p.tool == "bun" && p.requested_version.as_deref() == Some("1.2")),
            "build provenance must record the declared bun pin"
        );

        // Run command — same shared planner path.
        let run = materialize_lifecycle_toolchains(&plan, "bun run start", &reporter)
            .expect("materialize for run");
        assert!(
            run.bins.contains(&shim_dir),
            "bun shim must be on the run PATH"
        );
        assert!(
            run.provenance
                .iter()
                .any(|p| p.tool == "bun" && p.requested_version.as_deref() == Some("1.2")),
            "run provenance must record the declared bun pin"
        );
    }
}
