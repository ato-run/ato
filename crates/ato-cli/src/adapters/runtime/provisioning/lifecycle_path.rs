use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet};
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use capsule_core::router::ManifestData;
use capsule_core::CapsuleReporter;

use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;

use super::dependency_root;

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

        entries
    }

    pub fn path_env(&self) -> Result<OsString> {
        std::env::join_paths(self.path_entries())
            .context("failed to join lifecycle PATH entries")
    }

    pub fn emit_degraded_markers(&self) {
        for toolchain in &self.provenance.ato_toolchains {
            match toolchain.source {
                ToolchainSource::HostFallback => {
                    tracing::warn!(
                        phase = self.phase.as_str(),
                        tool = %toolchain.tool,
                        requested_version = ?toolchain.requested_version,
                        logical_id = %toolchain.logical_id,
                        package_manager_source = "host_fallback",
                        "lifecycle PATH plan degraded to host tool fallback"
                    );
                }
                ToolchainSource::Unavailable => {
                    tracing::warn!(
                        phase = self.phase.as_str(),
                        tool = %toolchain.tool,
                        requested_version = ?toolchain.requested_version,
                        logical_id = %toolchain.logical_id,
                        package_manager_source = "unavailable",
                        "lifecycle PATH plan could not resolve a managed toolchain"
                    );
                }
                ToolchainSource::Managed => {}
            }
        }
    }
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
    reporter: &Arc<CliReporter>,
) -> Result<LifecyclePathPlan> {
    let dependency_root = dependency_root(plan);
    let reporter_dyn: Arc<dyn CapsuleReporter + 'static> = reporter.clone();
    let (ato_toolchain_bins, ato_toolchains) =
        collect_ato_toolchain_bins(plan, command, reporter_dyn).await?;
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

    let plan = LifecyclePathPlan {
        phase,
        ato_toolchain_bins,
        dependency_output_bins,
        minimal_system_bins,
        provenance: LifecyclePathProvenance {
            ato_toolchains,
            dependency_bins,
            minimal_system_bins: minimal_system_provenance,
        },
    };
    plan.emit_degraded_markers();
    tracing::debug!(
        phase = plan.phase.as_str(),
        ato_toolchain_bins = ?plan.ato_toolchain_bins,
        dependency_output_bins = ?plan.dependency_output_bins,
        minimal_system_bins = ?plan.minimal_system_bins,
        "resolved lifecycle PATH plan"
    );
    Ok(plan)
}

async fn collect_ato_toolchain_bins(
    plan: &ManifestData,
    command: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<(Vec<PathBuf>, Vec<ToolchainProvenance>)> {
    let mut bins = Vec::new();
    let mut provenance = Vec::new();
    let mut seen = BTreeSet::new();
    let mut managed_node_bin = None;
    let command_name = leading_command_name(command);
    let node_required = plan
        .execution_driver()
        .map(|driver| driver.trim().eq_ignore_ascii_case("node"))
        .unwrap_or(false)
        || matches!(command_name, Some("npm" | "npx" | "pnpm" | "yarn"));

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

    if let Some(tool_name) = command_name.and_then(runtime_tool_name_for_command) {
        let spec = capsule_core::tools::lookup(tool_name)
            .with_context(|| format!("runtime tool '{tool_name}' is not registered"))?;
        let requested_version = capsule_core::tools::read_tool_version(
            &plan.manifest,
            plan.selected_target_label(),
            spec.name,
        );
        let mut deps = capsule_core::tools::ToolDeps::default();
        if spec.depends_on.contains(&"node") {
            deps.node_bin = managed_node_bin.clone();
        }
        let handle = capsule_core::tools::ensure_runtime_tool(
            spec,
            requested_version.as_deref(),
            &deps,
            reporter,
        )
        .await?;
        let source = if handle.version.is_empty() && handle.binary_sha256.is_empty() {
            ToolchainSource::HostFallback
        } else {
            ToolchainSource::Managed
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
    } else if matches!(command_name, Some("npm" | "npx")) && managed_node_bin.is_some() {
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

    Ok((bins, provenance))
}

fn collect_dependency_output_bins(
    primary_root: &Path,
    lifecycle_roots: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<DependencyBinProvenance>)> {
    let mut roots = Vec::new();
    let mut seen_roots = BTreeSet::new();
    push_unique_path(&mut roots, &mut seen_roots, primary_root.to_path_buf());
    for workspace_root in detect_workspace_roots(primary_root)? {
        push_unique_path(&mut roots, &mut seen_roots, workspace_root);
    }
    for root in lifecycle_roots {
        push_unique_path(&mut roots, &mut seen_roots, root.clone());
    }

    let mut bins = Vec::new();
    let mut provenance = Vec::new();
    let mut seen_bins = BTreeSet::new();
    for root in roots {
        let bin_dir = root.join("node_modules").join(".bin");
        if !bin_dir.is_dir() {
            continue;
        }
        let fingerprint = fs::canonicalize(&bin_dir).unwrap_or_else(|_| bin_dir.clone());
        if !seen_bins.insert(fingerprint) {
            continue;
        }
        provenance.push(dependency_bin_provenance(primary_root, &root));
        bins.push(bin_dir);
    }

    Ok((bins, provenance))
}

fn dependency_bin_provenance(primary_root: &Path, root: &Path) -> DependencyBinProvenance {
    if root == primary_root {
        return DependencyBinProvenance {
            role: "root".to_string(),
            relative_path: "./node_modules/.bin".to_string(),
        };
    }

    if let Ok(relative) = root.strip_prefix(primary_root) {
        let rel = relative.to_string_lossy().replace('\\', "/");
        return DependencyBinProvenance {
            role: "workspace".to_string(),
            relative_path: format!("{rel}/node_modules/.bin"),
        };
    }

    DependencyBinProvenance {
        role: "lifecycle-root".to_string(),
        relative_path: root.join("node_modules/.bin").display().to_string(),
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
                trimmed
                    .strip_prefix("- ")
                    .map(|value| value.trim().trim_matches('\'').trim_matches('"').to_string())
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
    } else if let Some(packages) = workspaces
        .get("packages")
        .and_then(Value::as_array)
    {
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

fn logical_toolchain_id(
    tool: &str,
    version: Option<&str>,
    source: ToolchainSource,
) -> String {
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
    use tempfile::tempdir;

    #[test]
    fn install_phase_omits_dependency_output_bins_from_path_entries() {
        let plan = LifecyclePathPlan {
            phase: LifecyclePhase::Install,
            ato_toolchain_bins: vec![PathBuf::from("/managed/node/bin")],
            dependency_output_bins: vec![PathBuf::from("/repo/node_modules/.bin")],
            minimal_system_bins: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
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
                temp.path().join("apps/server/node_modules/.bin"),
                temp.path().join("apps/app/node_modules/.bin"),
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
                    relative_path: "apps/server/node_modules/.bin".to_string(),
                },
                DependencyBinProvenance {
                    role: "workspace".to_string(),
                    relative_path: "apps/app/node_modules/.bin".to_string(),
                },
                DependencyBinProvenance {
                    role: "lifecycle-root".to_string(),
                    relative_path: temp
                        .path()
                        .join("admin/node_modules/.bin")
                        .display()
                        .to_string(),
                },
            ]
        );
    }
}
