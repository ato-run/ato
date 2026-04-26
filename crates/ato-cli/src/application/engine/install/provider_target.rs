#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use rand::RngCore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

use crate::ProviderToolchain;
use capsule_core::ato_lock::AtoLock;
use capsule_core::common::paths::ato_runs_dir;
use capsule_core::input_resolver::ATO_LOCK_FILE_NAME;
use capsule_core::python_runtime::{normalized_python_runtime_version, python_selector_env};

mod synthetic;

const PROVIDER_RUN_ROOT: &str = "provider-backed";
const PROVIDER_PACKAGE_JSON_FILE: &str = "package.json";
const PROVIDER_PACKAGE_LOCK_FILE: &str = "package-lock.json";
const PROVIDER_PNPM_LOCK_FILE: &str = "pnpm-lock.yaml";
const PROVIDER_BUN_LOCK_FILE: &str = "bun.lock";
const PROVIDER_BUN_LOCKB_FILE: &str = "bun.lockb";
const PROVIDER_NODE_MODULES_DIR: &str = "node_modules";
const PROVIDER_SITE_PACKAGES_DIR: &str = "site-packages";
const PROVIDER_REQUIREMENTS_FILE: &str = "requirements.txt";
const PROVIDER_RESOLUTION_METADATA_FILE: &str = "resolution.json";
const PROVIDER_PYTHON_RUNTIME_VERSION: &str = "3.11.10";
const PROVIDER_NODE_RUNTIME_VERSION: &str = "20.11.0";
const PROVIDER_UV_TOOL_VERSION: &str = "0.4.19";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderKind {
    PyPI,
    Npm,
}

impl ProviderKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PyPI => "pypi",
            Self::Npm => "npm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderTargetRef {
    pub(crate) provider: ProviderKind,
    pub(crate) ref_string: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedRunTarget {
    LocalPath(PathBuf),
    GitHubRepository(String),
    Provider(ProviderTargetRef),
    RegistryReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRunWorkspace {
    pub(crate) target: ProviderTargetRef,
    pub(crate) workspace_root: PathBuf,
    pub(crate) resolution_metadata_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedProviderEntrypoint {
    package_name: String,
    package_version: String,
    entrypoint_name: String,
    entrypoint_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPyPIRequirement {
    package_name: String,
    extras: Vec<String>,
}

impl ParsedPyPIRequirement {
    fn canonical_ref(&self) -> String {
        if self.extras.is_empty() {
            self.package_name.clone()
        } else {
            format!("{}[{}]", self.package_name, self.extras.join(","))
        }
    }

    fn requirement_spec(&self) -> String {
        self.canonical_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedNpmRequirement {
    package_name: String,
}

impl ParsedNpmRequirement {
    fn canonical_ref(&self) -> String {
        self.package_name.clone()
    }

    fn package_dir(&self) -> PathBuf {
        let mut path = PathBuf::new();
        for segment in self.package_name.split('/') {
            path.push(segment);
        }
        path
    }
}

#[derive(Debug, Serialize)]
struct ProviderResolutionMetadata {
    provider: String,
    r#ref: String,
    resolution_role: String,
    requested_provider_toolchain: String,
    effective_provider_toolchain: String,
    requested_package_name: String,
    requested_extras: Vec<String>,
    resolved_package_name: String,
    resolved_package_version: String,
    selected_entrypoint: String,
    generated_capsule_root: String,
    generated_manifest_path: String,
    generated_wrapper_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_authoritative_lock_path: Option<String>,
    index_source: String,
    requested_runtime_version: String,
    effective_runtime_version: String,
    materialization_runtime_selector: String,
}

#[derive(Debug, Deserialize)]
struct NpmPackageManifest {
    name: Option<String>,
    version: Option<String>,
    #[serde(default)]
    bin: Option<serde_json::Value>,
    #[serde(default)]
    scripts: Option<serde_json::Map<String, serde_json::Value>>,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeProviderLockfileKind {
    PackageLock,
    PnpmLock,
    BunLock,
}

struct WorkspaceGuard {
    root: PathBuf,
    keep: bool,
}

impl WorkspaceGuard {
    fn new(root: PathBuf) -> Self {
        Self { root, keep: false }
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn classify_run_target(raw: &str, expanded_local: &Path) -> Result<ParsedRunTarget> {
    if crate::local_input::should_treat_input_as_local(raw, expanded_local) {
        return Ok(ParsedRunTarget::LocalPath(expanded_local.to_path_buf()));
    }

    if let Some(repository) = super::parse_github_run_ref(raw)? {
        return Ok(ParsedRunTarget::GitHubRepository(repository));
    }

    if let Some((provider, ref_string)) = detect_provider_sugar(raw) {
        bail!(
            "Provider-backed targets must use canonical syntax '<provider>:<ref>'. Re-run with: ato run {}:{} -- ...",
            provider.as_str(),
            ref_string
        );
    }

    if let Some(provider_target) = parse_provider_target_ref(raw)? {
        return Ok(ParsedRunTarget::Provider(provider_target));
    }

    Ok(ParsedRunTarget::RegistryReference)
}

pub(crate) fn provider_install_error(input: &str) -> Result<Option<String>> {
    if let Some((provider, ref_string)) = detect_provider_sugar(input) {
        return Ok(Some(run_only_install_message(Some(provider), &ref_string)));
    }

    if let Some(provider_target) = parse_provider_target_ref(input)? {
        return Ok(Some(run_only_install_message(
            Some(provider_target.provider),
            &provider_target.ref_string,
        )));
    }

    Ok(None)
}

pub(crate) fn parse_provider_target_ref(input: &str) -> Result<Option<ProviderTargetRef>> {
    let raw = input.trim();
    let Some((provider_raw, ref_raw)) = raw.split_once(':') else {
        return Ok(None);
    };

    if !is_valid_provider_identifier(provider_raw) {
        return Ok(None);
    }

    let provider = match provider_raw.to_ascii_lowercase().as_str() {
        "pypi" => ProviderKind::PyPI,
        "npm" => ProviderKind::Npm,
        unknown => {
            bail!(
                "unknown provider `{}`\n\nSupported providers:\n  pypi\n  npm",
                unknown
            )
        }
    };

    let ref_string = ref_raw.trim();
    if ref_string.is_empty() {
        bail!(
            "Provider-backed targets must include a non-empty ref. Use: ato run {}:<ref> -- ...",
            provider.as_str()
        );
    }

    let ref_string = match provider {
        ProviderKind::PyPI => parse_pypi_requirement_ref(ref_string)?.canonical_ref(),
        ProviderKind::Npm => parse_npm_package_ref(ref_string)?.canonical_ref(),
    };

    Ok(Some(ProviderTargetRef {
        provider,
        ref_string,
    }))
}

pub(crate) fn materialize_provider_run_workspace(
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
    keep_failed_artifacts: bool,
    json: bool,
) -> Result<ProviderRunWorkspace> {
    synthetic::materialize_provider_run_workspace(
        target,
        requested_toolchain,
        keep_failed_artifacts,
        json,
    )
}

fn materialize_pypi_workspace(
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
    keep_failed_artifacts: bool,
    json: bool,
) -> Result<ProviderRunWorkspace> {
    let package_ref = parse_pypi_requirement_ref(&target.ref_string)?;
    let effective_toolchain =
        resolve_effective_provider_toolchain(target.provider, requested_toolchain)?;
    let workspace_root = unique_provider_workspace_root(target.provider)?;
    fs::create_dir_all(&workspace_root)
        .with_context(|| format!("failed to create {}", workspace_root.display()))?;
    let mut guard = WorkspaceGuard::new(workspace_root.clone());
    let result = (|| -> Result<ProviderRunWorkspace> {
        let site_packages_dir = workspace_root.join(PROVIDER_SITE_PACKAGES_DIR);
        fs::create_dir_all(&site_packages_dir)
            .with_context(|| format!("failed to create {}", site_packages_dir.display()))?;

        let requirements_path = workspace_root.join(PROVIDER_REQUIREMENTS_FILE);
        if let Some(parent) = requirements_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(
            &requirements_path,
            format!("{}\n", package_ref.requirement_spec()),
        )
        .with_context(|| format!("failed to write {}", requirements_path.display()))?;

        compile_provider_lockfile(&workspace_root)?;
        let source_dir = workspace_root.join("source");
        fs::create_dir_all(&source_dir)
            .with_context(|| format!("failed to create {}", source_dir.display()))?;
        fs::copy(workspace_root.join("uv.lock"), source_dir.join("uv.lock"))
            .with_context(|| format!("failed to mirror lockfile into {}", source_dir.display()))?;
        sync_provider_site_packages(&workspace_root, &site_packages_dir)?;

        let resolved =
            resolve_console_script_metadata(&site_packages_dir, &package_ref.package_name)?;
        let wrapper_path = workspace_root.join("main.py");
        let source_wrapper_path = source_dir.join("main.py");
        fs::write(
            &wrapper_path,
            python_wrapper_for_entrypoint(
                &resolved.entrypoint_name,
                &resolved.entrypoint_value,
                ".ato/provider/site-packages",
                &package_ref,
            )?,
        )
        .with_context(|| format!("failed to write {}", wrapper_path.display()))?;
        fs::write(
            &source_wrapper_path,
            python_wrapper_for_entrypoint(
                &resolved.entrypoint_name,
                &resolved.entrypoint_value,
                "../.ato/provider/site-packages",
                &package_ref,
            )?,
        )
        .with_context(|| format!("failed to write {}", source_wrapper_path.display()))?;

        let manifest_path = workspace_root.join("capsule.toml");
        fs::write(
            &manifest_path,
            capsule_manifest_for_provider_run(
                &package_ref.package_name,
                &resolved.package_version,
                "python",
                PROVIDER_PYTHON_RUNTIME_VERSION,
                "main.py",
            ),
        )
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

        let resolution_metadata_path = workspace_root.join(PROVIDER_RESOLUTION_METADATA_FILE);
        let metadata = ProviderResolutionMetadata {
            provider: target.provider.as_str().to_string(),
            r#ref: package_ref.canonical_ref(),
            resolution_role: "audit_provenance_only".to_string(),
            requested_provider_toolchain: requested_toolchain.as_str().to_string(),
            effective_provider_toolchain: effective_toolchain.as_str().to_string(),
            requested_package_name: package_ref.package_name.clone(),
            requested_extras: package_ref.extras.clone(),
            resolved_package_name: resolved.package_name,
            resolved_package_version: resolved.package_version,
            selected_entrypoint: resolved.entrypoint_value,
            generated_capsule_root: workspace_root.display().to_string(),
            generated_manifest_path: manifest_path.display().to_string(),
            generated_wrapper_path: wrapper_path.display().to_string(),
            generated_authoritative_lock_path: None,
            index_source: current_provider_index_source(target.provider),
            requested_runtime_version: PROVIDER_PYTHON_RUNTIME_VERSION.to_string(),
            effective_runtime_version: PROVIDER_PYTHON_RUNTIME_VERSION.to_string(),
            materialization_runtime_selector: normalized_python_runtime_version(Some(
                PROVIDER_PYTHON_RUNTIME_VERSION,
            ))
            .unwrap_or_else(|| PROVIDER_PYTHON_RUNTIME_VERSION.to_string()),
        };
        fs::write(
            &resolution_metadata_path,
            serde_json::to_string_pretty(&metadata)
                .context("failed to serialize provider resolution metadata")?
                + "\n",
        )
        .with_context(|| format!("failed to write {}", resolution_metadata_path.display()))?;

        debug!(
            selected_python_runtime = PROVIDER_PYTHON_RUNTIME_VERSION,
            materialized_python_selector = PROVIDER_PYTHON_RUNTIME_VERSION,
            workspace_root = %workspace_root.display(),
            "Materialized provider-backed PyPI workspace"
        );
        guard.keep();

        Ok(ProviderRunWorkspace {
            target: target.clone(),
            workspace_root: workspace_root.clone(),
            resolution_metadata_path,
        })
    })();

    if result.is_err() && keep_failed_artifacts {
        guard.keep();
        maybe_report_kept_failed_provider_workspace(&workspace_root, json);
    }

    result
}

fn materialize_npm_workspace(
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
    keep_failed_artifacts: bool,
    json: bool,
) -> Result<ProviderRunWorkspace> {
    let package_ref = parse_npm_package_ref(&target.ref_string)?;
    let effective_toolchain =
        resolve_effective_provider_toolchain(target.provider, requested_toolchain)?;
    let workspace_root = unique_provider_workspace_root(target.provider)?;
    fs::create_dir_all(&workspace_root)
        .with_context(|| format!("failed to create {}", workspace_root.display()))?;
    let mut guard = WorkspaceGuard::new(workspace_root.clone());
    let result = (|| -> Result<ProviderRunWorkspace> {
        let provider_dir = workspace_root.join(".ato/provider");
        fs::create_dir_all(&provider_dir)
            .with_context(|| format!("failed to create {}", provider_dir.display()))?;

        let package_json_path = workspace_root.join(PROVIDER_PACKAGE_JSON_FILE);
        if let Some(parent) = package_json_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(
            &package_json_path,
            serde_json::to_string_pretty(&json!({
                "name": "ato-provider-run",
                "private": true,
                "dependencies": {
                    package_ref.package_name.clone(): "*",
                },
            }))
            .context("failed to serialize synthetic npm package.json")?
                + "\n",
        )
        .with_context(|| format!("failed to write {}", package_json_path.display()))?;

        let lockfile_path = install_node_provider_package(
            &provider_dir,
            workspace_root.as_path(),
            effective_toolchain,
        )?;

        let source_dir = workspace_root.join("source");
        fs::create_dir_all(&source_dir)
            .with_context(|| format!("failed to create {}", source_dir.display()))?;
        let lockfile_name = lockfile_path
            .file_name()
            .context("provider-backed node lockfile must have a filename")?;
        fs::copy(&lockfile_path, source_dir.join(lockfile_name)).with_context(|| {
            format!(
                "failed to mirror node lockfile into {}",
                source_dir.display()
            )
        })?;

        let installed_package_dir = workspace_root
            .join(PROVIDER_NODE_MODULES_DIR)
            .join(package_ref.package_dir());
        let resolved = resolve_npm_bin_metadata(&installed_package_dir, &package_ref.package_name)?;
        let root_bin_relative = installed_package_dir
            .join(&resolved.entrypoint_value)
            .strip_prefix(&workspace_root)
            .context("npm bin path must stay under synthetic workspace root")?
            .to_string_lossy()
            .to_string();

        let wrapper_path = workspace_root.join("main.mjs");
        let source_wrapper_path = source_dir.join("main.mjs");
        fs::write(&wrapper_path, node_wrapper_for_bin(&root_bin_relative)?)
            .with_context(|| format!("failed to write {}", wrapper_path.display()))?;
        fs::write(
            &source_wrapper_path,
            node_wrapper_for_bin(&format!("../{root_bin_relative}"))?,
        )
        .with_context(|| format!("failed to write {}", source_wrapper_path.display()))?;

        let manifest_path = workspace_root.join("capsule.toml");
        fs::write(
            &manifest_path,
            capsule_manifest_for_provider_run(
                &package_ref.package_name,
                &resolved.package_version,
                "node",
                PROVIDER_NODE_RUNTIME_VERSION,
                "main.mjs",
            ),
        )
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

        let resolution_metadata_path = workspace_root.join(PROVIDER_RESOLUTION_METADATA_FILE);
        let metadata = ProviderResolutionMetadata {
            provider: target.provider.as_str().to_string(),
            r#ref: package_ref.canonical_ref(),
            resolution_role: "audit_provenance_only".to_string(),
            requested_provider_toolchain: requested_toolchain.as_str().to_string(),
            effective_provider_toolchain: effective_toolchain.as_str().to_string(),
            requested_package_name: package_ref.package_name.clone(),
            requested_extras: Vec::new(),
            resolved_package_name: resolved.package_name,
            resolved_package_version: resolved.package_version,
            selected_entrypoint: resolved.entrypoint_value,
            generated_capsule_root: workspace_root.display().to_string(),
            generated_manifest_path: manifest_path.display().to_string(),
            generated_wrapper_path: wrapper_path.display().to_string(),
            generated_authoritative_lock_path: None,
            index_source: current_provider_index_source(target.provider),
            requested_runtime_version: PROVIDER_NODE_RUNTIME_VERSION.to_string(),
            effective_runtime_version: PROVIDER_NODE_RUNTIME_VERSION.to_string(),
            materialization_runtime_selector: PROVIDER_NODE_RUNTIME_VERSION.to_string(),
        };
        fs::write(
            &resolution_metadata_path,
            serde_json::to_string_pretty(&metadata)
                .context("failed to serialize provider resolution metadata")?
                + "\n",
        )
        .with_context(|| format!("failed to write {}", resolution_metadata_path.display()))?;

        debug!(
            selected_node_runtime = PROVIDER_NODE_RUNTIME_VERSION,
            effective_provider_toolchain = effective_toolchain.as_str(),
            workspace_root = %workspace_root.display(),
            "Materialized provider-backed npm workspace"
        );
        guard.keep();

        Ok(ProviderRunWorkspace {
            target: target.clone(),
            workspace_root: workspace_root.clone(),
            resolution_metadata_path,
        })
    })();

    if result.is_err() && keep_failed_artifacts {
        guard.keep();
        maybe_report_kept_failed_provider_workspace(&workspace_root, json);
    }

    result
}

fn compile_provider_lockfile(workspace_root: &Path) -> Result<()> {
    let uv = find_uv_binary()?;
    let selector_env = python_selector_env(Some(PROVIDER_PYTHON_RUNTIME_VERSION));
    let mut command = Command::new(&uv);
    command
        .envs(selector_env.iter())
        .args([
            "pip",
            "compile",
            PROVIDER_REQUIREMENTS_FILE,
            "-o",
            "uv.lock",
            "--managed-python",
        ])
        .arg(format!("--python={PROVIDER_PYTHON_RUNTIME_VERSION}"))
        .current_dir(workspace_root);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{}`", uv.display()))?;

    if output.status.success() {
        return Ok(());
    }

    bail!(
        "failed to generate uv.lock for provider-backed PyPI run (status {}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn sync_provider_site_packages(workspace_root: &Path, site_packages_dir: &Path) -> Result<()> {
    let uv = find_uv_binary()?;
    let selector_env = python_selector_env(Some(PROVIDER_PYTHON_RUNTIME_VERSION));
    let mut command = Command::new(&uv);
    command
        .envs(selector_env.iter())
        .args(["pip", "sync", "uv.lock", "--managed-python"])
        .arg(format!("--python={PROVIDER_PYTHON_RUNTIME_VERSION}"))
        .args(["--target", site_packages_dir.to_string_lossy().as_ref()])
        .current_dir(workspace_root);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{}`", uv.display()))?;

    if output.status.success() {
        return Ok(());
    }

    bail!(
        "failed to materialize bundled site-packages for provider-backed PyPI run (status {}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn resolve_console_script_metadata(
    site_packages_dir: &Path,
    requested_package: &str,
) -> Result<ResolvedProviderEntrypoint> {
    let normalized_requested = normalize_pypi_name(requested_package);
    let mut matches = Vec::new();

    let entries = fs::read_dir(site_packages_dir)
        .with_context(|| format!("failed to read {}", site_packages_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !file_name.ends_with(".dist-info") {
            continue;
        }

        let metadata_path = path.join("METADATA");
        if !metadata_path.exists() {
            continue;
        }

        let metadata = fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?;
        let Some(name) = metadata_header(&metadata, "Name") else {
            continue;
        };
        if normalize_pypi_name(&name) != normalized_requested {
            continue;
        }

        let version = metadata_header(&metadata, "Version").unwrap_or_else(|| "0.0.0".to_string());
        let entry_points_path = path.join("entry_points.txt");
        let console_scripts = if entry_points_path.exists() {
            parse_console_scripts(
                &fs::read_to_string(&entry_points_path)
                    .with_context(|| format!("failed to read {}", entry_points_path.display()))?,
            )
        } else {
            Vec::new()
        };

        matches.push((name, version, console_scripts));
    }

    if matches.is_empty() {
        bail!(
            "Package '{}' did not resolve to installed distribution metadata. Provider-backed MVP currently supports package-name-only PyPI resolution.",
            requested_package
        );
    }
    if matches.len() > 1 {
        bail!(
            "Package '{}' resolved to multiple installed distributions. Explicit entrypoint selection is not supported yet.",
            requested_package
        );
    }

    let (distribution_name, distribution_version, mut console_scripts) = matches.remove(0);
    if console_scripts.is_empty() {
        bail!(
            "Package '{}' does not expose a console script entrypoint. Explicit entrypoint selection is not supported yet.",
            requested_package
        );
    }
    if console_scripts.len() > 1 {
        let names = console_scripts
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Package '{}' exposes multiple console script entrypoints ({names}). Explicit entrypoint selection is not supported yet.",
            requested_package
        );
    }

    let (script_name, entrypoint_value) = console_scripts.remove(0);
    validate_entrypoint_value(&entrypoint_value)?;

    Ok(ResolvedProviderEntrypoint {
        package_name: distribution_name,
        package_version: distribution_version,
        entrypoint_name: script_name,
        entrypoint_value,
    })
}

fn resolve_effective_provider_toolchain(
    provider: ProviderKind,
    requested: ProviderToolchain,
) -> Result<ProviderToolchain> {
    match (provider, requested) {
        (ProviderKind::PyPI, ProviderToolchain::Auto | ProviderToolchain::Uv) => {
            Ok(ProviderToolchain::Uv)
        }
        (ProviderKind::PyPI, invalid) => bail!(
            "`--via {}` is not valid for pypi: targets in this MVP. Supported combinations:\n  pypi + auto\n  pypi + uv",
            invalid.as_str()
        ),
        (ProviderKind::Npm, ProviderToolchain::Auto | ProviderToolchain::Npm) => {
            Ok(ProviderToolchain::Npm)
        }
        (ProviderKind::Npm, invalid) => bail!(
            "`--via {}` is not valid for npm: targets in this MVP. Supported combinations:\n  npm + auto\n  npm + npm",
            invalid.as_str()
        ),
    }
}

fn install_node_provider_package(
    provider_dir: &Path,
    workspace_root: &Path,
    toolchain: ProviderToolchain,
) -> Result<PathBuf> {
    let (program, args, expected_lockfile_kind) = node_provider_install_command(toolchain)?;
    let output = Command::new(&program)
        .args(args)
        .current_dir(provider_dir)
        .output()
        .with_context(|| format!("failed to execute `{}`", program.display()))?;

    if output.status.success() {
        return resolve_node_provider_lockfile_path(workspace_root, expected_lockfile_kind);
    }

    bail!(
        "failed to materialize provider-backed {} package (status {}): {}",
        toolchain.as_str(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn node_provider_install_command(
    toolchain: ProviderToolchain,
) -> Result<(PathBuf, Vec<&'static str>, NodeProviderLockfileKind)> {
    match toolchain {
        ProviderToolchain::Npm => Ok((
            find_npm_binary()?,
            npm_install_command_args().to_vec(),
            NodeProviderLockfileKind::PackageLock,
        )),
        ProviderToolchain::Pnpm => Ok((
            find_pnpm_binary()?,
            pnpm_install_command_args().to_vec(),
            NodeProviderLockfileKind::PnpmLock,
        )),
        ProviderToolchain::Bun => Ok((
            find_bun_binary()?,
            bun_install_command_args().to_vec(),
            NodeProviderLockfileKind::BunLock,
        )),
        other => bail!(
            "Node provider toolchain '{}' is not materializable in this MVP.",
            other.as_str()
        ),
    }
}

fn npm_install_command_args() -> [&'static str; 5] {
    [
        "install",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--silent",
    ]
}

fn pnpm_install_command_args() -> [&'static str; 5] {
    [
        "install",
        "--ignore-scripts",
        "--ignore-workspace",
        "--no-frozen-lockfile",
        "--silent",
    ]
}

fn bun_install_command_args() -> [&'static str; 3] {
    ["install", "--ignore-scripts", "--silent"]
}

fn resolve_node_provider_lockfile_path(
    workspace_root: &Path,
    expected_lockfile_kind: NodeProviderLockfileKind,
) -> Result<PathBuf> {
    let path = match expected_lockfile_kind {
        NodeProviderLockfileKind::PackageLock => workspace_root.join(PROVIDER_PACKAGE_LOCK_FILE),
        NodeProviderLockfileKind::PnpmLock => workspace_root.join(PROVIDER_PNPM_LOCK_FILE),
        NodeProviderLockfileKind::BunLock => {
            let bun_lock = workspace_root.join(PROVIDER_BUN_LOCK_FILE);
            if bun_lock.exists() {
                bun_lock
            } else {
                workspace_root.join(PROVIDER_BUN_LOCKB_FILE)
            }
        }
    };

    if path.exists() {
        return Ok(path);
    }

    bail!(
        "failed to materialize provider-backed node package: expected lockfile {} was not generated",
        path.display()
    );
}

fn resolve_npm_bin_metadata(
    installed_package_dir: &Path,
    requested_package: &str,
) -> Result<ResolvedProviderEntrypoint> {
    let manifest_path = installed_package_dir.join("package.json");
    if !manifest_path.exists() {
        bail!(
            "Package '{}' was not materialized under node_modules. Provider-backed npm execution currently supports registry package-name resolution only.",
            requested_package
        );
    }

    let manifest: NpmPackageManifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

    reject_npm_lifecycle_scripts(manifest.scripts.as_ref(), requested_package)?;

    let package_name = manifest
        .name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| requested_package.to_string());
    let package_version = manifest
        .version
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "0.0.0".to_string());
    let Some(bin) = manifest.bin else {
        bail!(
            "Package '{}' does not expose a CLI bin entrypoint. Explicit entrypoint selection is not supported yet.",
            requested_package
        );
    };

    let mut entries = match bin {
        serde_json::Value::String(path) => vec![(default_npm_bin_name(&package_name), path)],
        serde_json::Value::Object(map) => map
            .into_iter()
            .filter_map(|(name, value)| value.as_str().map(|path| (name, path.to_string())))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    if entries.is_empty() {
        bail!(
            "Package '{}' does not expose a CLI bin entrypoint. Explicit entrypoint selection is not supported yet.",
            requested_package
        );
    }
    if entries.len() > 1 {
        let canonical = default_npm_bin_name(&package_name);
        if let Some(pos) = entries.iter().position(|(name, _)| name == &canonical) {
            entries = vec![entries.remove(pos)];
        } else {
            let names = entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Package '{}' exposes multiple bin entrypoints ({names}). Explicit entrypoint selection is not supported yet.",
                requested_package
            );
        }
    }

    let (entrypoint_name, entrypoint_value) = entries.remove(0);
    validate_npm_bin_path(&entrypoint_value)?;
    let bin_path = installed_package_dir.join(&entrypoint_value);
    if !bin_path.exists() {
        bail!(
            "Package '{}' publishes bin '{}' -> '{}' but the file is missing after `npm install --ignore-scripts`. Packages that require install scripts are not supported in this MVP.",
            requested_package,
            entrypoint_name,
            entrypoint_value
        );
    }

    Ok(ResolvedProviderEntrypoint {
        package_name,
        package_version,
        entrypoint_name,
        entrypoint_value,
    })
}

fn reject_npm_lifecycle_scripts(
    scripts: Option<&serde_json::Map<String, serde_json::Value>>,
    requested_package: &str,
) -> Result<()> {
    let Some(scripts) = scripts else {
        return Ok(());
    };

    let lifecycle = ["preinstall", "install", "postinstall"]
        .into_iter()
        .filter(|name| {
            scripts
                .get(*name)
                .and_then(|value| value.as_str())
                .is_some()
        })
        .collect::<Vec<_>>();
    if lifecycle.is_empty() {
        return Ok(());
    }

    bail!(
        "Package '{}' declares install lifecycle scripts ({}). MVP provider-backed npm execution always runs `npm install --ignore-scripts` and rejects packages that require install scripts.",
        requested_package,
        lifecycle.join(", ")
    );
}

fn default_npm_bin_name(package_name: &str) -> String {
    package_name
        .rsplit('/')
        .next()
        .unwrap_or(package_name)
        .to_string()
}

fn validate_npm_bin_path(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Resolved npm bin path must not be empty");
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        bail!("Resolved npm bin path '{}' must be relative", raw);
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => bail!(
                "Resolved npm bin path '{}' must stay within the package root",
                raw
            ),
        }
    }
    Ok(())
}

fn parse_console_scripts(raw: &str) -> Vec<(String, String)> {
    let mut in_console_scripts = false;
    let mut entries = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_console_scripts = trimmed.eq_ignore_ascii_case("[console_scripts]");
            continue;
        }
        if !in_console_scripts {
            continue;
        }

        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        entries.push((name.to_string(), value.to_string()));
    }

    entries
}

fn metadata_header(raw: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    raw.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .map(str::to_string)
    })
}

fn validate_entrypoint_value(value: &str) -> Result<()> {
    let cleaned = strip_entrypoint_extras(value);
    let Some((module_name, attr_path)) = cleaned.split_once(':') else {
        bail!(
            "Resolved console script entrypoint '{}' is not a callable module reference.",
            value
        );
    };
    if module_name.trim().is_empty() || attr_path.trim().is_empty() {
        bail!(
            "Resolved console script entrypoint '{}' is not a callable module reference.",
            value
        );
    }
    Ok(())
}

fn python_wrapper_for_entrypoint(
    script_name: &str,
    entrypoint_value: &str,
    site_packages_relative: &str,
    package_ref: &ParsedPyPIRequirement,
) -> Result<String> {
    let cleaned = strip_entrypoint_extras(entrypoint_value);
    let (module_name, attr_path) = cleaned
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("entrypoint must include module and callable"))?;
    let script_name_literal =
        serde_json::to_string(script_name).context("failed to encode script name literal")?;
    let module_literal =
        serde_json::to_string(module_name).context("failed to encode module literal")?;
    let attr_literal =
        serde_json::to_string(attr_path).context("failed to encode attribute literal")?;
    let site_packages_literal = serde_json::to_string(site_packages_relative)
        .context("failed to encode site-packages literal")?;
    let package_literal = serde_json::to_string(&package_ref.package_name)
        .context("failed to encode package name literal")?;
    let extras_literal =
        serde_json::to_string(&package_ref.extras).context("failed to encode extras literal")?;

    Ok(format!(
        r#"#!/usr/bin/env python3
from __future__ import annotations

import importlib
import sys
import traceback
from pathlib import Path

_SITE_PACKAGES = (Path(__file__).resolve().parent / {site_packages_literal}).resolve()
_REQUESTED_PACKAGE_NAME = {package_literal}
_REQUESTED_EXTRAS = {extras_literal}
sys.path.insert(0, str(_SITE_PACKAGES))
sys.argv[0] = {script_name_literal}


def _load_entrypoint():
    module = importlib.import_module({module_literal})
    value = module
    for attribute in {attr_literal}.split("."):
        value = getattr(value, attribute)
    return value


def _maybe_print_known_hint(exc):
    if _REQUESTED_PACKAGE_NAME != "markitdown":
        return
    if "pdf" in _REQUESTED_EXTRAS:
        return

    diagnostic = "".join(traceback.format_exception_only(type(exc), exc))
    if (
        "MissingDependencyException" not in diagnostic
        or "PdfConverter" not in diagnostic
        or "[pdf]" not in diagnostic
    ):
        return

    sys.stderr.write("hint: markitdown[pdf] extra may be required for PDF input.\n")
    sys.stderr.write("Try: ato run pypi:markitdown[pdf] -- ...\n")
    sys.stderr.flush()


if __name__ == "__main__":
    try:
        entrypoint = _load_entrypoint()
        result = entrypoint()
        raise SystemExit(result if isinstance(result, int) else 0)
    except BaseException as exc:
        _maybe_print_known_hint(exc)
        raise
"#
    ))
}

fn node_wrapper_for_bin(relative_bin_path: &str) -> Result<String> {
    let relative_literal = serde_json::to_string(relative_bin_path)
        .context("failed to encode npm bin path literal")?;
    Ok(format!(
        r#"#!/usr/bin/env node
import {{ spawnSync }} from "node:child_process";
import path from "node:path";
import {{ fileURLToPath }} from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const binPath = path.resolve(here, {relative_literal});
const result = spawnSync(process.execPath, [binPath, ...process.argv.slice(2)], {{
  stdio: "inherit",
  cwd: process.cwd(),
  env: process.env,
}});

if (result.error) {{
  throw result.error;
}}

process.exit(result.status ?? 1);
"#
    ))
}

fn capsule_manifest_for_provider_run(
    package_name: &str,
    version: &str,
    driver: &str,
    runtime_version: &str,
    entrypoint: &str,
) -> String {
    let manifest_name = normalize_provider_manifest_name(package_name);
    format!(
        r#"schema_version = "0.3"
name = "{manifest_name}"
version = "{version}"
type = "job"

runtime = "source/{driver}"
runtime_version = "{runtime_version}"
source_layout = "anchored_entrypoint"
run = "{entrypoint}""#
    )
}

fn normalize_provider_manifest_name(package_name: &str) -> String {
    package_name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_pypi_requirement_ref(raw_ref: &str) -> Result<ParsedPyPIRequirement> {
    let trimmed = raw_ref.trim();
    if trimmed.contains('@') {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed PyPI execution does not support inline version syntax yet.",
            raw_ref
        );
    }
    if trimmed.contains(';') {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed PyPI execution does not support environment markers yet.",
            raw_ref
        );
    }
    if trimmed.contains("://")
        || trimmed.starts_with("git+")
        || trimmed.starts_with("file:")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(char::is_whitespace)
    {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed PyPI execution does not support direct URL, VCS, or path references yet.",
            raw_ref
        );
    }

    let Some(captures) = pypi_requirement_regex().captures(trimmed) else {
        if trimmed.contains('[') || trimmed.contains(']') {
            bail!(
                "Unsupported provider target '{}'. Invalid extras syntax; use `pypi:<name>[extra]` or `pypi:<name>[a,b]`.",
                raw_ref
            );
        }
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed PyPI execution accepts package names and extras matching [A-Za-z0-9._-]+.",
            raw_ref
        );
    };

    let package_name = normalize_pypi_name(
        captures
            .name("name")
            .expect("regex must capture package name")
            .as_str(),
    );
    let extras = captures
        .name("extras")
        .map(|value| canonicalize_pypi_extras(value.as_str()))
        .transpose()?
        .unwrap_or_default();

    Ok(ParsedPyPIRequirement {
        package_name,
        extras,
    })
}

fn canonicalize_pypi_extras(raw_extras: &str) -> Result<Vec<String>> {
    let mut extras = raw_extras
        .split(',')
        .map(normalize_pypi_name)
        .collect::<Vec<_>>();
    if extras.iter().any(|value| value.is_empty()) {
        bail!("PyPI extras must not be empty");
    }
    extras.sort();
    extras.dedup();
    Ok(extras)
}

fn normalize_pypi_name(raw: &str) -> String {
    pypi_normalize_regex()
        .replace_all(&raw.trim().to_ascii_lowercase(), "-")
        .to_string()
}

fn parse_npm_package_ref(raw_ref: &str) -> Result<ParsedNpmRequirement> {
    let trimmed = raw_ref.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) || trimmed.contains('\\') {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed npm execution accepts only `npm:<package>` or `npm:@scope/package`.",
            raw_ref
        );
    }
    if trimmed.starts_with("file:")
        || trimmed.starts_with("git+")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.contains("://")
    {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed npm execution does not support direct URL, git, or file references yet.",
            raw_ref
        );
    }

    let package_name = if let Some(scoped) = trimmed.strip_prefix('@') {
        let Some((scope, remainder)) = scoped.split_once('/') else {
            bail!(
                "Unsupported provider target '{}'. Scoped npm refs must use `npm:@scope/package`.",
                raw_ref
            );
        };
        if remainder.contains('/') {
            bail!(
                "Unsupported provider target '{}'. MVP provider-backed npm execution does not support package subpaths.",
                raw_ref
            );
        }
        if remainder.contains('@') {
            bail!(
                "Unsupported provider target '{}'. MVP provider-backed npm execution does not support inline versions or dist-tags yet.",
                raw_ref
            );
        }
        validate_npm_name_segment(scope, raw_ref)?;
        validate_npm_name_segment(remainder, raw_ref)?;
        format!(
            "@{}/{}",
            scope.to_ascii_lowercase(),
            remainder.to_ascii_lowercase()
        )
    } else {
        if trimmed.contains('/') {
            bail!(
                "Unsupported provider target '{}'. MVP provider-backed npm execution does not support package subpaths.",
                raw_ref
            );
        }
        if trimmed.contains('@') {
            bail!(
                "Unsupported provider target '{}'. MVP provider-backed npm execution does not support inline versions or dist-tags yet.",
                raw_ref
            );
        }
        validate_npm_name_segment(trimmed, raw_ref)?;
        trimmed.to_ascii_lowercase()
    };

    Ok(ParsedNpmRequirement { package_name })
}

fn validate_npm_name_segment(segment: &str, raw_ref: &str) -> Result<()> {
    if segment.is_empty()
        || !segment.chars().all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || matches!(value, '.' | '_' | '-')
        })
        || !segment
            .chars()
            .next()
            .map(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
            .unwrap_or(false)
    {
        bail!(
            "Unsupported provider target '{}'. MVP provider-backed npm execution accepts package names matching [a-z0-9._-]+.",
            raw_ref
        );
    }
    Ok(())
}

fn strip_entrypoint_extras(value: &str) -> &str {
    value
        .split_once('[')
        .map(|(head, _)| head.trim_end())
        .unwrap_or(value)
}

fn current_provider_index_source(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::PyPI => env::var("UV_INDEX_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                env::var("PIP_INDEX_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "default".to_string()),
        ProviderKind::Npm => env::var("NPM_CONFIG_REGISTRY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                env::var("npm_config_registry")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "default".to_string()),
    }
}

fn unique_provider_workspace_root(provider: ProviderKind) -> Result<PathBuf> {
    let base = ato_runs_dir().join(PROVIDER_RUN_ROOT);
    fs::create_dir_all(&base).with_context(|| format!("failed to create {}", base.display()))?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random = rand::thread_rng().next_u64();
    Ok(base.join(format!("{}-{millis:x}-{random:x}", provider.as_str())))
}

fn find_uv_binary() -> Result<PathBuf> {
    which::which("uv").context("provider-backed PyPI execution requires `uv` on PATH")
}

fn find_npm_binary() -> Result<PathBuf> {
    which::which("npm").context("provider-backed npm execution requires `npm` on PATH")
}

fn find_pnpm_binary() -> Result<PathBuf> {
    which::which("pnpm").context("provider-backed pnpm execution requires `pnpm` on PATH")
}

fn find_bun_binary() -> Result<PathBuf> {
    which::which("bun").context("provider-backed bun execution requires `bun` on PATH")
}

pub(crate) fn persist_provider_authoritative_lock(
    workspace_root: &Path,
    resolution_metadata_path: &Path,
    lock: &AtoLock,
) -> Result<PathBuf> {
    let lock_path = workspace_root.join(ATO_LOCK_FILE_NAME);
    capsule_core::ato_lock::write_pretty_to_path(lock, &lock_path)
        .with_context(|| format!("failed to write {}", lock_path.display()))?;
    record_provider_authoritative_lock_path(resolution_metadata_path, &lock_path)?;
    Ok(lock_path)
}

fn record_provider_authoritative_lock_path(
    resolution_metadata_path: &Path,
    lock_path: &Path,
) -> Result<()> {
    let raw = fs::read_to_string(resolution_metadata_path)
        .with_context(|| format!("failed to read {}", resolution_metadata_path.display()))?;
    let mut metadata: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse provider resolution metadata {}",
            resolution_metadata_path.display()
        )
    })?;
    let object = metadata.as_object_mut().context(
        "provider resolution metadata must be a JSON object before authority lock annotation",
    )?;
    object.insert(
        "generated_authoritative_lock_path".to_string(),
        serde_json::Value::String(lock_path.display().to_string()),
    );
    fs::write(
        resolution_metadata_path,
        serde_json::to_string_pretty(&metadata)
            .context("failed to serialize provider resolution metadata")?
            + "\n",
    )
    .with_context(|| format!("failed to write {}", resolution_metadata_path.display()))?;
    Ok(())
}

pub(crate) fn maybe_report_kept_failed_provider_workspace(workspace_root: &Path, json: bool) {
    if json {
        return;
    }
    eprintln!(
        "⚠️  Kept failed provider-backed workspace for debugging: {}",
        workspace_root.display()
    );
}

fn run_only_install_message(provider: Option<ProviderKind>, ref_string: &str) -> String {
    match provider {
        Some(ProviderKind::PyPI) => format!(
            "provider-backed targets are run-only in this MVP. Use `ato run pypi:{ref_string} -- ...`; `ato install pypi:{ref_string}` is not supported."
        ),
        Some(ProviderKind::Npm) => format!(
            "provider-backed targets are run-only in this MVP. Use `ato run npm:{ref_string} -- ...`; `ato install npm:{ref_string}` is not supported."
        ),
        None => "provider-backed targets are run-only in this MVP.".to_string(),
    }
}

fn detect_provider_sugar(input: &str) -> Option<(ProviderKind, String)> {
    let raw = input.trim();
    let (provider_raw, ref_string) = raw.split_once('/')?;
    let provider = match provider_raw.to_ascii_lowercase().as_str() {
        "pypi" => ProviderKind::PyPI,
        "npm" => ProviderKind::Npm,
        _ => return None,
    };
    let ref_string = ref_string.trim_matches('/');
    if ref_string.is_empty() {
        return None;
    }
    Some((provider, ref_string.to_string()))
}

fn is_valid_provider_identifier(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '-')
}

fn pypi_requirement_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^(?P<name>[A-Za-z0-9][A-Za-z0-9._-]*)(?:\[(?P<extras>[A-Za-z0-9][A-Za-z0-9._-]*(?:,[A-Za-z0-9][A-Za-z0-9._-]*)*)\])?$",
        )
        .expect("valid regex")
    })
}

fn pypi_normalize_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"[-_.]+").expect("valid regex"))
}

#[cfg(test)]
mod tests {
    use super::{
        capsule_manifest_for_provider_run, classify_run_target, materialize_provider_run_workspace,
        npm_install_command_args, parse_npm_package_ref, parse_provider_target_ref,
        parse_pypi_requirement_ref, pnpm_install_command_args, resolve_console_script_metadata,
        resolve_effective_provider_toolchain, resolve_npm_bin_metadata, ParsedRunTarget,
        ProviderKind, ProviderTargetRef, PROVIDER_RESOLUTION_METADATA_FILE,
    };
    use crate::ProviderToolchain;
    use capsule_core::ato_lock;
    use serde_json::{json, Value};
    use serial_test::serial;
    use std::fs;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use zip::{write::FileOptions, ZipWriter};

    struct TestWheelSpec<'a> {
        package_name: &'a str,
        version: &'a str,
        module_name: &'a str,
        module_files: Vec<(&'a str, &'a str)>,
        metadata_lines: Vec<String>,
        console_script_name: Option<&'a str>,
        console_script_entrypoint: Option<&'a str>,
    }

    fn workspace_tempdir(prefix: &str) -> TempDir {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".ato")
            .join("test-scratch");
        fs::create_dir_all(&root).expect("create workspace .ato/test-scratch");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(root)
            .expect("create workspace tempdir")
    }

    fn strict_ci() -> bool {
        std::env::var("ATO_STRICT_CI")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    fn require_provider_materialization_prerequisites() -> bool {
        if super::find_uv_binary().is_ok() {
            return true;
        }

        assert!(
            !strict_ci(),
            "strict CI requires uv for provider-backed materialization tests"
        );
        false
    }

    struct TestEnvGuard {
        original: Vec<(&'static str, Option<String>)>,
    }

    impl TestEnvGuard {
        fn set(entries: &[(&'static str, String)]) -> Self {
            let mut original = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                original.push((*key, std::env::var(key).ok()));
                std::env::set_var(key, value);
            }
            Self { original }
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.original.drain(..).rev() {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }

    fn write_poison_python_shims(root: &Path) {
        #[cfg(windows)]
        {
            let script = "@echo off\r\necho poisoned python shim>&2\r\nexit /b 97\r\n";
            fs::write(root.join("python.cmd"), script).expect("write python shim");
            fs::write(root.join("python3.cmd"), script).expect("write python3 shim");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let script = "#!/bin/sh\necho poisoned python shim >&2\nexit 97\n";
            for name in ["python", "python3"] {
                let path = root.join(name);
                fs::write(&path, script).expect("write python shim");
                let mut permissions = fs::metadata(&path).expect("shim metadata").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions).expect("shim permissions");
            }
        }
    }

    fn prepend_path(dir: &Path) -> String {
        let original = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&original));
        std::env::join_paths(paths)
            .expect("join PATH entries")
            .to_string_lossy()
            .to_string()
    }

    struct TestStaticFileServer {
        base_url: String,
        shutdown: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for TestStaticFileServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(self.base_url.trim_start_matches("http://"));
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn spawn_test_static_file_server(root: PathBuf) -> TestStaticFileServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind static file server");
        listener
            .set_nonblocking(true)
            .expect("make static file server nonblocking");
        let addr = listener
            .local_addr()
            .expect("resolve static file server addr");
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = Arc::clone(&shutdown);

        let handle = std::thread::spawn(move || {
            while !shutdown_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let started = Instant::now();
                        let mut request = Vec::new();
                        let mut buffer = [0u8; 1024];

                        while started.elapsed() < Duration::from_secs(2) {
                            match stream.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(read) => {
                                    request.extend_from_slice(&buffer[..read]);
                                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(err)
                                    if err.kind() == std::io::ErrorKind::WouldBlock
                                        || err.kind() == std::io::ErrorKind::TimedOut =>
                                {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => break,
                            }
                        }

                        let path = String::from_utf8_lossy(&request)
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .split('?')
                            .next()
                            .unwrap_or("/")
                            .to_string();
                        let relative = path.trim_start_matches('/');
                        let file_path = root.join(relative);
                        let file_path = if file_path.is_dir() {
                            file_path.join("index.html")
                        } else {
                            file_path
                        };

                        if let Ok(body) = fs::read(&file_path) {
                            let content_type = match file_path
                                .extension()
                                .and_then(|value| value.to_str())
                                .unwrap_or_default()
                            {
                                "html" => "text/html; charset=utf-8",
                                "whl" => "application/octet-stream",
                                _ => "application/octet-stream",
                            };
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                                body.len(),
                                content_type
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.write_all(&body);
                        } else {
                            let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                            let _ = stream.write_all(response.as_bytes());
                        }
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        TestStaticFileServer {
            base_url: format!("http://{}", addr),
            shutdown,
            handle: Some(handle),
        }
    }

    fn write_test_wheel(root: &Path, spec: TestWheelSpec<'_>) -> String {
        let normalized = spec.package_name.replace('-', "_");
        let wheel_name = format!("{normalized}-{}-py3-none-any.whl", spec.version);
        let wheel_path = root.join("packages").join(&wheel_name);
        fs::create_dir_all(wheel_path.parent().expect("wheel parent"))
            .expect("create packages dir");

        let file = File::create(&wheel_path).expect("create wheel");
        let mut zip = ZipWriter::new(file);
        let options: FileOptions<()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        let mut metadata = format!(
            "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
            spec.package_name, spec.version
        );
        for line in &spec.metadata_lines {
            metadata.push_str(line);
            metadata.push('\n');
        }
        let wheel = "\
Wheel-Version: 1.0\n\
Generator: ato-cli-test\n\
Root-Is-Purelib: true\n\
Tag: py3-none-any\n";
        let entry_points = spec
            .console_script_name
            .zip(spec.console_script_entrypoint)
            .map(|(name, entrypoint)| format!("[console_scripts]\n{name} = {entrypoint}\n"));
        let has_explicit_init = spec
            .module_files
            .iter()
            .any(|(relative_path, _)| *relative_path == "__init__.py");
        let mut record_lines = vec![
            format!("{normalized}-{}.dist-info/METADATA,,", spec.version),
            format!("{normalized}-{}.dist-info/WHEEL,,", spec.version),
            format!("{normalized}-{}.dist-info/RECORD,,", spec.version),
        ];

        if !has_explicit_init {
            record_lines.push(format!("{}/__init__.py,,", spec.module_name));
            zip.start_file(format!("{}/__init__.py", spec.module_name), options)
                .expect("start __init__.py");
            zip.write_all(b"").expect("write package __init__.py");
        }

        for (relative_path, contents) in spec.module_files {
            record_lines.push(format!("{}/{relative_path},,", spec.module_name));
            zip.start_file(format!("{}/{}", spec.module_name, relative_path), options)
                .expect("start module file");
            zip.write_all(contents.as_bytes())
                .expect("write module file");
        }

        zip.start_file(
            format!("{normalized}-{}.dist-info/METADATA", spec.version),
            options,
        )
        .expect("start METADATA");
        zip.write_all(metadata.as_bytes()).expect("write METADATA");

        zip.start_file(
            format!("{normalized}-{}.dist-info/WHEEL", spec.version),
            options,
        )
        .expect("start WHEEL");
        zip.write_all(wheel.as_bytes()).expect("write WHEEL");

        if let Some(entry_points) = entry_points {
            record_lines.push(format!(
                "{normalized}-{}.dist-info/entry_points.txt,,",
                spec.version
            ));
            zip.start_file(
                format!("{normalized}-{}.dist-info/entry_points.txt", spec.version),
                options,
            )
            .expect("start entry_points.txt");
            zip.write_all(entry_points.as_bytes())
                .expect("write entry_points.txt");
        }

        zip.start_file(
            format!("{normalized}-{}.dist-info/RECORD", spec.version),
            options,
        )
        .expect("start RECORD");
        zip.write_all((record_lines.join("\n") + "\n").as_bytes())
            .expect("write RECORD");

        zip.finish().expect("finish wheel");
        wheel_name
    }

    fn write_simple_index(root: &Path, packages: &[(&str, Vec<String>)]) {
        let simple_dir = root.join("simple");
        fs::create_dir_all(&simple_dir).expect("create simple root index dir");
        let mut root_index = String::new();
        for (package_name, wheel_names) in packages {
            let package_dir = simple_dir.join(package_name);
            fs::create_dir_all(&package_dir).expect("create simple package dir");
            root_index.push_str(&format!(
                "<!doctype html><a href=\"{package_name}/\">{package_name}</a>\n"
            ));
            let package_index = wheel_names
                .iter()
                .map(|wheel_name| {
                    format!(
                        "<!doctype html><a href=\"../../packages/{wheel_name}\">{wheel_name}</a>\n"
                    )
                })
                .collect::<String>();
            fs::write(package_dir.join("index.html"), package_index)
                .expect("write package simple index");
        }
        fs::write(simple_dir.join("index.html"), root_index).expect("write simple root index");
    }

    fn write_distribution(
        site_packages: &Path,
        dist_name: &str,
        version: &str,
        entry_points: Option<&str>,
    ) {
        let normalized = dist_name.replace('-', "_");
        let package_dir = site_packages.join(normalized.clone());
        fs::create_dir_all(&package_dir).expect("create package dir");
        fs::write(
            package_dir.join("cli.py"),
            "raise RuntimeError('package code must not be imported during probe')\n",
        )
        .expect("write package");

        let dist_info = site_packages.join(format!("{normalized}-{version}.dist-info"));
        fs::create_dir_all(&dist_info).expect("create dist info");
        fs::write(
            dist_info.join("METADATA"),
            format!("Metadata-Version: 2.1\nName: {dist_name}\nVersion: {version}\n"),
        )
        .expect("write metadata");
        if let Some(entry_points) = entry_points {
            fs::write(dist_info.join("entry_points.txt"), entry_points)
                .expect("write entry points");
        }
    }

    fn write_npm_package_manifest(
        package_root: &Path,
        manifest: serde_json::Value,
        bin_files: &[(&str, &str)],
    ) {
        fs::create_dir_all(package_root).expect("create npm package root");
        fs::write(
            package_root.join("package.json"),
            serde_json::to_string_pretty(&manifest).expect("serialize npm package manifest"),
        )
        .expect("write npm package manifest");
        for (relative_path, contents) in bin_files {
            let path = package_root.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create npm bin parent");
            }
            fs::write(path, contents).expect("write npm bin file");
        }
    }

    #[test]
    fn classify_run_target_parses_pypi_provider_target() {
        let parsed = classify_run_target("pypi:markitdown", Path::new("pypi:markitdown"))
            .expect("parse run target");
        assert_eq!(
            parsed,
            ParsedRunTarget::Provider(super::ProviderTargetRef {
                provider: ProviderKind::PyPI,
                ref_string: "markitdown".to_string(),
            })
        );
    }

    #[test]
    fn classify_run_target_prefers_windows_drive_letter_paths() {
        let raw = r"C:\tool.py";
        let parsed =
            classify_run_target(raw, Path::new(raw)).expect("windows path should stay local");
        assert_eq!(parsed, ParsedRunTarget::LocalPath(PathBuf::from(raw)));
    }

    #[test]
    fn parse_provider_target_ref_rejects_unknown_provider() {
        let err = parse_provider_target_ref("foo:bar").expect_err("unknown provider must fail");
        assert!(err.to_string().contains("unknown provider `foo`"));
    }

    #[test]
    fn parse_provider_target_ref_accepts_npm_shape() {
        let target = parse_provider_target_ref("npm:@scope/pkg")
            .expect("parse")
            .expect("provider target");
        assert_eq!(target.provider, ProviderKind::Npm);
        assert_eq!(target.ref_string, "@scope/pkg");
    }

    #[test]
    fn parse_npm_package_ref_accepts_unscoped_package() {
        let target = parse_npm_package_ref("tsx").expect("parse npm package");
        assert_eq!(target.package_name, "tsx");
    }

    #[test]
    fn capsule_manifest_for_provider_run_normalizes_scoped_package_name() {
        let manifest = capsule_manifest_for_provider_run(
            "@biomejs/biome",
            "1.0.0",
            "node",
            "20.11.0",
            "main.mjs",
        );
        assert!(manifest.contains("name = \"biomejs-biome\""));
    }

    #[test]
    fn parse_npm_package_ref_rejects_inline_version() {
        let err = parse_npm_package_ref("tsx@4.9.0").expect_err("version must fail");
        assert!(err
            .to_string()
            .contains("does not support inline versions or dist-tags"));
    }

    #[test]
    fn parse_npm_package_ref_rejects_direct_url() {
        let err = parse_npm_package_ref("https://example.com/demo.tgz").expect_err("url must fail");
        assert!(err
            .to_string()
            .contains("does not support direct URL, git, or file references"));
    }

    #[test]
    fn parse_npm_package_ref_rejects_subpath() {
        let err = parse_npm_package_ref("@scope/pkg/bin").expect_err("subpath must fail");
        assert!(err
            .to_string()
            .contains("does not support package subpaths"));
    }

    #[test]
    fn parse_provider_target_ref_normalizes_pypi_extras() {
        let target = parse_provider_target_ref("pypi:demo[b,a,a]")
            .expect("parse")
            .expect("provider target");
        assert_eq!(target.ref_string, "demo[a,b]");
    }

    #[test]
    fn parse_provider_target_ref_accepts_pypi_extras() {
        let target = parse_provider_target_ref("pypi:markitdown[pdf]")
            .expect("parse")
            .expect("provider target");
        assert_eq!(target.provider, ProviderKind::PyPI);
        assert_eq!(target.ref_string, "markitdown[pdf]");
    }

    #[test]
    fn parse_pypi_requirement_ref_rejects_invalid_extras_syntax() {
        let err = parse_pypi_requirement_ref("demo[pdf,,ocr]")
            .expect_err("invalid extras syntax must fail");
        assert!(err.to_string().contains("Invalid extras syntax"));
    }

    #[test]
    fn parse_pypi_requirement_ref_rejects_version_suffix() {
        let err = parse_pypi_requirement_ref("demo@1.2.3").expect_err("version must fail");
        assert!(err
            .to_string()
            .contains("does not support inline version syntax"));
    }

    #[test]
    fn parse_pypi_requirement_ref_rejects_direct_urls() {
        let err =
            parse_pypi_requirement_ref("https://example.com/demo.whl").expect_err("url must fail");
        assert!(err
            .to_string()
            .contains("does not support direct URL, VCS, or path references"));
    }

    #[test]
    fn npm_install_command_uses_ignore_scripts() {
        assert_eq!(
            npm_install_command_args(),
            [
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--silent"
            ]
        );
    }

    #[test]
    fn pnpm_install_command_uses_ignore_scripts() {
        assert_eq!(
            pnpm_install_command_args(),
            [
                "install",
                "--ignore-scripts",
                "--ignore-workspace",
                "--no-frozen-lockfile",
                "--silent"
            ]
        );
    }

    #[test]
    fn resolve_effective_provider_toolchain_rejects_pnpm_for_npm_targets() {
        let err = resolve_effective_provider_toolchain(ProviderKind::Npm, ProviderToolchain::Pnpm)
            .expect_err("pnpm should be rejected for npm targets in this MVP");
        assert!(err.to_string().contains("npm + npm"));
    }

    #[test]
    fn resolve_effective_provider_toolchain_rejects_pnpm_for_pypi_targets() {
        let err = resolve_effective_provider_toolchain(ProviderKind::PyPI, ProviderToolchain::Pnpm)
            .expect_err("pnpm should be rejected for pypi targets");
        assert!(err.to_string().contains("pypi + uv"));
    }

    #[test]
    fn resolve_npm_bin_metadata_accepts_single_bin_string() {
        let temp = workspace_tempdir("provider-target-npm-single-bin-");
        write_npm_package_manifest(
            temp.path(),
            json!({
                "name": "demo-npm-single-bin",
                "version": "1.0.0",
                "bin": "bin/cli.mjs",
            }),
            &[("bin/cli.mjs", "console.log('ok');\n")],
        );

        let resolved =
            resolve_npm_bin_metadata(temp.path(), "demo-npm-single-bin").expect("resolve npm bin");
        assert_eq!(resolved.package_name, "demo-npm-single-bin");
        assert_eq!(resolved.package_version, "1.0.0");
        assert_eq!(resolved.entrypoint_name, "demo-npm-single-bin");
        assert_eq!(resolved.entrypoint_value, "bin/cli.mjs");
    }

    #[test]
    fn resolve_npm_bin_metadata_rejects_multiple_bins() {
        let temp = workspace_tempdir("provider-target-npm-multi-bin-");
        write_npm_package_manifest(
            temp.path(),
            json!({
                "name": "demo-npm-multi-bin",
                "version": "1.0.0",
                "bin": {
                    "demo-a": "bin/a.mjs",
                    "demo-b": "bin/b.mjs",
                },
            }),
            &[("bin/a.mjs", "a\n"), ("bin/b.mjs", "b\n")],
        );

        let err = resolve_npm_bin_metadata(temp.path(), "demo-npm-multi-bin")
            .expect_err("multiple bins must fail");
        assert!(err.to_string().contains("multiple bin entrypoints"));
    }

    #[test]
    fn resolve_npm_bin_metadata_selects_matching_bin_from_multiple() {
        let temp = workspace_tempdir("provider-target-npm-multi-bin-match-");
        write_npm_package_manifest(
            temp.path(),
            json!({
                "name": "cowsay",
                "version": "1.5.0",
                "bin": {
                    "cowsay":   "cli.js",
                    "cowthink": "cli.js",
                },
            }),
            &[("cli.js", "#!/usr/bin/env node\n")],
        );

        let resolved =
            resolve_npm_bin_metadata(temp.path(), "cowsay").expect("should resolve to cowsay bin");
        assert_eq!(resolved.entrypoint_name, "cowsay");
    }

    #[test]
    fn resolve_npm_bin_metadata_rejects_install_lifecycle_scripts() {
        let temp = workspace_tempdir("provider-target-npm-install-script-");
        write_npm_package_manifest(
            temp.path(),
            json!({
                "name": "demo-npm-needs-install-script",
                "version": "1.0.0",
                "bin": "bin/cli.mjs",
                "scripts": {
                    "install": "node build.mjs",
                },
            }),
            &[("bin/cli.mjs", "console.log('ok');\n")],
        );

        let err = resolve_npm_bin_metadata(temp.path(), "demo-npm-needs-install-script")
            .expect_err("install script package must fail");
        assert!(err
            .to_string()
            .contains("declares install lifecycle scripts"));
        assert!(err.to_string().contains("--ignore-scripts"));
    }

    #[test]
    fn resolve_console_script_metadata_uses_distribution_metadata_only() {
        let temp = workspace_tempdir("provider-target-metadata-only-");
        write_distribution(
            temp.path(),
            "demo-provider",
            "0.1.0",
            Some("[console_scripts]\ndemo-provider = demo_provider.cli:main\n"),
        );

        let resolved = resolve_console_script_metadata(temp.path(), "demo-provider")
            .expect("resolve entrypoint");
        assert_eq!(resolved.package_name, "demo-provider");
        assert_eq!(resolved.package_version, "0.1.0");
        assert_eq!(resolved.entrypoint_name, "demo-provider");
        assert_eq!(resolved.entrypoint_value, "demo_provider.cli:main");
    }

    #[test]
    fn resolve_console_script_metadata_rejects_multiple_entrypoints() {
        let temp = workspace_tempdir("provider-target-entrypoints-");
        write_distribution(
            temp.path(),
            "demo-provider",
            "0.1.0",
            Some(
                "[console_scripts]\ndemo-provider = demo_provider.cli:main\nother = demo_provider.cli:other\n",
            ),
        );

        let err = resolve_console_script_metadata(temp.path(), "demo-provider")
            .expect_err("multiple entrypoints must fail");
        assert!(err
            .to_string()
            .contains("multiple console script entrypoints"));
    }

    #[test]
    fn resolution_metadata_path_is_stable() {
        assert_eq!(PROVIDER_RESOLUTION_METADATA_FILE, "resolution.json");
    }

    #[test]
    #[serial]
    fn materialize_provider_workspace_writes_generated_project_and_resolution_metadata() {
        let index_root = workspace_tempdir("provider-target-index-");
        let cli_source = "raise RuntimeError('package code must not be imported during probe')\n";
        let wheel_name = write_test_wheel(
            index_root.path(),
            TestWheelSpec {
                package_name: "demo-provider",
                version: "0.1.0",
                module_name: "demo_provider",
                module_files: vec![("cli.py", cli_source)],
                metadata_lines: Vec::new(),
                console_script_name: Some("demo-provider"),
                console_script_entrypoint: Some("demo_provider.cli:main"),
            },
        );
        write_simple_index(
            index_root.path(),
            &[("demo-provider", vec![wheel_name.clone()])],
        );
        let server = spawn_test_static_file_server(index_root.path().to_path_buf());
        let _env = TestEnvGuard::set(&[
            (
                "UV_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            (
                "PIP_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
        ]);

        let workspace = materialize_provider_run_workspace(
            &ProviderTargetRef {
                provider: ProviderKind::PyPI,
                ref_string: "demo-provider".to_string(),
            },
            ProviderToolchain::Auto,
            false,
            false,
        )
        .expect("materialize provider workspace");

        assert!(
            workspace.workspace_root.join("ato.lock.json").exists(),
            "authoritative lock should be generated"
        );
        assert!(
            workspace.workspace_root.join("main.py").exists(),
            "python wrapper should be generated"
        );
        assert!(
            workspace.workspace_root.join("requirements.txt").exists(),
            "requirements.txt should be generated"
        );
        assert!(
            !workspace.workspace_root.join("uv.lock").exists(),
            "uv.lock should remain a derived execution input"
        );
        assert!(
            workspace.resolution_metadata_path.exists(),
            "resolution metadata file should be generated"
        );

        let lock =
            ato_lock::load_unvalidated_from_path(&workspace.workspace_root.join("ato.lock.json"))
                .expect("load provider authoritative lock");
        assert_eq!(
            lock.contract.entries["metadata"]["default_target"].as_str(),
            Some("app")
        );
        assert_eq!(
            lock.resolution.entries["runtime"]["driver"].as_str(),
            Some("python")
        );
        assert_eq!(
            lock.resolution.entries["resolved_targets"][0]["entrypoint"].as_str(),
            Some("main.py")
        );

        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(&workspace.resolution_metadata_path)
                .expect("read resolution metadata"),
        )
        .expect("parse resolution metadata");
        assert_eq!(metadata["provider"].as_str(), Some("pypi"));
        assert_eq!(metadata["ref"].as_str(), Some("demo-provider"));
        assert_eq!(
            metadata["requested_provider_toolchain"].as_str(),
            Some("auto")
        );
        assert_eq!(
            metadata["effective_provider_toolchain"].as_str(),
            Some("uv")
        );
        assert_eq!(
            metadata["requested_package_name"].as_str(),
            Some("demo-provider")
        );
        assert_eq!(metadata["requested_extras"], serde_json::json!([]));
        assert_eq!(metadata["resolved_package_version"].as_str(), Some("0.1.0"));
        assert_eq!(
            metadata["selected_entrypoint"].as_str(),
            Some("demo_provider.cli:main")
        );
        assert_eq!(
            metadata["requested_runtime_version"].as_str(),
            Some(super::PROVIDER_PYTHON_RUNTIME_VERSION)
        );
        assert_eq!(
            metadata["effective_runtime_version"].as_str(),
            Some(super::PROVIDER_PYTHON_RUNTIME_VERSION)
        );
        assert_eq!(
            metadata["materialization_runtime_selector"].as_str(),
            Some(super::PROVIDER_PYTHON_RUNTIME_VERSION)
        );
        assert_eq!(
            metadata["resolution_role"].as_str(),
            Some("audit_provenance_only")
        );
        assert!(
            metadata["generated_authoritative_lock_path"]
                .as_str()
                .is_some(),
            "persisted authoritative lock path should be recorded"
        );

        fs::remove_dir_all(&workspace.workspace_root).expect("cleanup provider workspace");
    }

    #[test]
    #[serial]
    fn materialize_provider_workspace_preserves_normalized_extras_in_requirements_and_metadata() {
        let index_root = workspace_tempdir("provider-target-extras-index-");

        let helper_wheel = write_test_wheel(
            index_root.path(),
            TestWheelSpec {
                package_name: "demo-provider-pdf-helper",
                version: "0.1.0",
                module_name: "demo_provider_pdf_helper",
                module_files: vec![("__init__.py", "PDF_HELPER = True\n")],
                metadata_lines: Vec::new(),
                console_script_name: None,
                console_script_entrypoint: None,
            },
        );
        let provider_wheel = write_test_wheel(
            index_root.path(),
            TestWheelSpec {
                package_name: "demo-provider",
                version: "0.1.0",
                module_name: "demo_provider",
                module_files: vec![("cli.py", "def main():\n    return 0\n")],
                metadata_lines: vec![
                    "Provides-Extra: a".to_string(),
                    "Provides-Extra: b".to_string(),
                    "Requires-Dist: demo-provider-pdf-helper; extra == 'a'".to_string(),
                ],
                console_script_name: Some("demo-provider"),
                console_script_entrypoint: Some("demo_provider.cli:main"),
            },
        );
        write_simple_index(
            index_root.path(),
            &[
                ("demo-provider", vec![provider_wheel.clone()]),
                ("demo-provider-pdf-helper", vec![helper_wheel.clone()]),
            ],
        );
        let server = spawn_test_static_file_server(index_root.path().to_path_buf());
        let _env = TestEnvGuard::set(&[
            (
                "UV_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            (
                "PIP_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
        ]);

        let workspace = materialize_provider_run_workspace(
            &ProviderTargetRef {
                provider: ProviderKind::PyPI,
                ref_string: "demo-provider[b,a,a]".to_string(),
            },
            ProviderToolchain::Auto,
            false,
            false,
        )
        .expect("materialize provider workspace with extras");

        let requirements = fs::read_to_string(workspace.workspace_root.join("requirements.txt"))
            .expect("read generated requirements");
        assert_eq!(requirements, "demo-provider[a,b]==0.1.0\n");

        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(&workspace.resolution_metadata_path)
                .expect("read resolution metadata"),
        )
        .expect("parse resolution metadata");
        assert_eq!(metadata["ref"].as_str(), Some("demo-provider[a,b]"));
        assert_eq!(
            metadata["requested_provider_toolchain"].as_str(),
            Some("auto")
        );
        assert_eq!(
            metadata["effective_provider_toolchain"].as_str(),
            Some("uv")
        );
        assert_eq!(
            metadata["requested_package_name"].as_str(),
            Some("demo-provider")
        );
        assert_eq!(metadata["requested_extras"], serde_json::json!(["a", "b"]));
        assert_eq!(
            metadata["materialization_runtime_selector"].as_str(),
            Some(super::PROVIDER_PYTHON_RUNTIME_VERSION)
        );

        fs::remove_dir_all(&workspace.workspace_root).expect("cleanup provider workspace");
    }
}
