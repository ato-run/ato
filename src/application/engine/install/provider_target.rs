use anyhow::{bail, Context, Result};
use rand::RngCore;
use regex::Regex;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use capsule_core::common::paths::ato_runs_dir;

const PROVIDER_RUN_ROOT: &str = "provider-backed";
const PROVIDER_SITE_PACKAGES_DIR: &str = ".ato/provider/site-packages";
const PROVIDER_REQUIREMENTS_FILE: &str = ".ato/provider/requirements.txt";
const PROVIDER_RESOLUTION_METADATA_FILE: &str = ".ato/provider/resolution.json";
const PROVIDER_PYTHON_RUNTIME_VERSION: &str = "3.11.10";

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
struct ResolvedConsoleScript {
    distribution_name: String,
    distribution_version: String,
    script_name: String,
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

#[derive(Debug, Serialize)]
struct ProviderResolutionMetadata {
    provider: String,
    r#ref: String,
    requested_package_name: String,
    requested_extras: Vec<String>,
    resolved_distribution_name: String,
    resolved_distribution_version: String,
    selected_entrypoint: String,
    generated_wrapper_path: String,
    index_source: String,
    effective_runtime_version: String,
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
                "unknown provider `{}`\n\nSupported providers:\n  pypi\n  npm (recognized, run not implemented yet)",
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
        ProviderKind::Npm => ref_string.to_string(),
    };

    Ok(Some(ProviderTargetRef {
        provider,
        ref_string,
    }))
}

pub(crate) fn materialize_provider_run_workspace(
    target: &ProviderTargetRef,
    keep_failed_artifacts: bool,
    json: bool,
) -> Result<ProviderRunWorkspace> {
    match target.provider {
        ProviderKind::PyPI => materialize_pypi_workspace(target, keep_failed_artifacts, json),
        ProviderKind::Npm => bail!(
            "Provider target '{}' is recognized but not implemented yet. MVP provider-backed execution currently supports `pypi:<package>` and `pypi:<package>[extra]`.",
            format_provider_target(target)
        ),
    }
}

fn materialize_pypi_workspace(
    target: &ProviderTargetRef,
    keep_failed_artifacts: bool,
    json: bool,
) -> Result<ProviderRunWorkspace> {
    let package_ref = parse_pypi_requirement_ref(&target.ref_string)?;
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
                &resolved.script_name,
                &resolved.entrypoint_value,
                ".ato/provider/site-packages",
                &package_ref,
            )?,
        )
        .with_context(|| format!("failed to write {}", wrapper_path.display()))?;
        fs::write(
            &source_wrapper_path,
            python_wrapper_for_entrypoint(
                &resolved.script_name,
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
                &resolved.distribution_version,
            ),
        )
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

        let resolution_metadata_path = workspace_root.join(PROVIDER_RESOLUTION_METADATA_FILE);
        let metadata = ProviderResolutionMetadata {
            provider: target.provider.as_str().to_string(),
            r#ref: package_ref.canonical_ref(),
            requested_package_name: package_ref.package_name.clone(),
            requested_extras: package_ref.extras.clone(),
            resolved_distribution_name: resolved.distribution_name,
            resolved_distribution_version: resolved.distribution_version,
            selected_entrypoint: resolved.entrypoint_value,
            generated_wrapper_path: wrapper_path.display().to_string(),
            index_source: current_index_source(),
            effective_runtime_version: PROVIDER_PYTHON_RUNTIME_VERSION.to_string(),
        };
        fs::write(
            &resolution_metadata_path,
            serde_json::to_string_pretty(&metadata)
                .context("failed to serialize provider resolution metadata")?
                + "\n",
        )
        .with_context(|| format!("failed to write {}", resolution_metadata_path.display()))?;

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
    let mut command = Command::new(&uv);
    command
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
    let mut command = Command::new(&uv);
    command
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
) -> Result<ResolvedConsoleScript> {
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

    Ok(ResolvedConsoleScript {
        distribution_name,
        distribution_version,
        script_name,
        entrypoint_value,
    })
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

fn capsule_manifest_for_provider_run(package_name: &str, version: &str) -> String {
    format!(
        r#"schema_version = "0.2"
name = "{package_name}"
version = "{version}"
type = "job"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "python"
runtime_version = "{PROVIDER_PYTHON_RUNTIME_VERSION}"
entrypoint = "main.py"
source_layout = "anchored_entrypoint"
"#
    )
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

fn strip_entrypoint_extras(value: &str) -> &str {
    value
        .split_once('[')
        .map(|(head, _)| head.trim_end())
        .unwrap_or(value)
}

fn current_index_source() -> String {
    env::var("UV_INDEX_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("PIP_INDEX_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "default".to_string())
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

pub(crate) fn maybe_report_kept_failed_provider_workspace(workspace_root: &Path, json: bool) {
    if json {
        return;
    }
    eprintln!(
        "⚠️  Kept failed provider-backed workspace for debugging: {}",
        workspace_root.display()
    );
}

fn format_provider_target(target: &ProviderTargetRef) -> String {
    format!("{}:{}", target.provider.as_str(), target.ref_string)
}

fn run_only_install_message(provider: Option<ProviderKind>, ref_string: &str) -> String {
    match provider {
        Some(ProviderKind::PyPI) => format!(
            "provider-backed targets are run-only in this MVP. Use `ato run pypi:{ref_string} -- ...`; `ato install pypi:{ref_string}` is not supported."
        ),
        Some(ProviderKind::Npm) => format!(
            "provider-backed targets are run-only in this MVP, and `npm:` execution is not implemented yet. `ato install npm:{ref_string}` is not supported."
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
        classify_run_target, materialize_provider_run_workspace, parse_provider_target_ref,
        parse_pypi_requirement_ref, resolve_console_script_metadata, ParsedRunTarget, ProviderKind,
        ProviderTargetRef, PROVIDER_RESOLUTION_METADATA_FILE,
    };
    use serde_json::Value;
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
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        fs::create_dir_all(&root).expect("create workspace .tmp");
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
        assert_eq!(resolved.distribution_name, "demo-provider");
        assert_eq!(resolved.distribution_version, "0.1.0");
        assert_eq!(resolved.script_name, "demo-provider");
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
        assert_eq!(
            PROVIDER_RESOLUTION_METADATA_FILE,
            ".ato/provider/resolution.json"
        );
    }

    #[test]
    #[serial]
    fn materialize_provider_workspace_writes_generated_project_and_resolution_metadata() {
        if !require_provider_materialization_prerequisites() {
            return;
        }

        let home = workspace_tempdir("provider-target-home-");
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
        let poisoned_path_dir = workspace_tempdir("provider-target-path-");
        write_poison_python_shims(poisoned_path_dir.path());
        let _env = TestEnvGuard::set(&[
            ("HOME", home.path().display().to_string()),
            (
                "UV_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            (
                "PIP_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            ("UV_INSECURE_HOST", "127.0.0.1".to_string()),
            ("PATH", prepend_path(poisoned_path_dir.path())),
        ]);

        let workspace = materialize_provider_run_workspace(
            &ProviderTargetRef {
                provider: ProviderKind::PyPI,
                ref_string: "demo-provider".to_string(),
            },
            false,
            false,
        )
        .expect("materialize provider workspace");

        assert!(
            workspace.workspace_root.join("capsule.toml").exists(),
            "capsule manifest should be generated"
        );
        assert!(
            workspace.workspace_root.join("main.py").exists(),
            "python wrapper should be generated"
        );
        assert!(
            workspace
                .workspace_root
                .join(".ato/provider/requirements.txt")
                .exists(),
            "requirements.txt should be generated"
        );
        assert!(
            workspace.workspace_root.join("uv.lock").exists(),
            "uv.lock should be generated"
        );
        let manifest_raw = fs::read_to_string(workspace.workspace_root.join("capsule.toml"))
            .expect("read manifest");
        assert!(
            manifest_raw.contains(&format!(
                "runtime_version = \"{}\"",
                super::PROVIDER_PYTHON_RUNTIME_VERSION
            )),
            "provider capsule manifest must pin the effective runtime version"
        );
        assert!(
            workspace.resolution_metadata_path.exists(),
            "resolution metadata file should be generated"
        );

        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(&workspace.resolution_metadata_path)
                .expect("read resolution metadata"),
        )
        .expect("parse resolution metadata");
        assert_eq!(metadata["provider"].as_str(), Some("pypi"));
        assert_eq!(metadata["ref"].as_str(), Some("demo-provider"));
        assert_eq!(
            metadata["requested_package_name"].as_str(),
            Some("demo-provider")
        );
        assert_eq!(metadata["requested_extras"], serde_json::json!([]));
        assert_eq!(
            metadata["resolved_distribution_version"].as_str(),
            Some("0.1.0")
        );
        assert_eq!(
            metadata["selected_entrypoint"].as_str(),
            Some("demo_provider.cli:main")
        );
        assert_eq!(
            metadata["effective_runtime_version"].as_str(),
            Some(super::PROVIDER_PYTHON_RUNTIME_VERSION)
        );
        assert!(
            workspace
                .workspace_root
                .join(".ato/provider/site-packages/demo_provider-0.1.0.dist-info")
                .exists(),
            "installed distribution metadata should be present"
        );

        fs::remove_dir_all(&workspace.workspace_root).expect("cleanup provider workspace");
    }

    #[test]
    #[serial]
    fn materialize_provider_workspace_preserves_normalized_extras_in_requirements_and_metadata() {
        if !require_provider_materialization_prerequisites() {
            return;
        }

        let home = workspace_tempdir("provider-target-extras-home-");
        let index_root = workspace_tempdir("provider-target-extras-index-");
        let poisoned_path_dir = workspace_tempdir("provider-target-extras-path-");
        write_poison_python_shims(poisoned_path_dir.path());

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
            ("HOME", home.path().display().to_string()),
            (
                "UV_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            (
                "PIP_INDEX_URL",
                format!("{}/simple", server.base_url.as_str()),
            ),
            ("UV_INSECURE_HOST", "127.0.0.1".to_string()),
            ("PATH", prepend_path(poisoned_path_dir.path())),
        ]);

        let workspace = materialize_provider_run_workspace(
            &ProviderTargetRef {
                provider: ProviderKind::PyPI,
                ref_string: "demo-provider[b,a,a]".to_string(),
            },
            false,
            false,
        )
        .expect("materialize provider workspace with extras");

        let requirements = fs::read_to_string(
            workspace
                .workspace_root
                .join(".ato/provider/requirements.txt"),
        )
        .expect("read generated requirements");
        assert_eq!(requirements, "demo-provider[a,b]\n");

        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(&workspace.resolution_metadata_path)
                .expect("read resolution metadata"),
        )
        .expect("parse resolution metadata");
        assert_eq!(metadata["ref"].as_str(), Some("demo-provider[a,b]"));
        assert_eq!(
            metadata["requested_package_name"].as_str(),
            Some("demo-provider")
        );
        assert_eq!(metadata["requested_extras"], serde_json::json!(["a", "b"]));

        fs::remove_dir_all(&workspace.workspace_root).expect("cleanup provider workspace");
    }
}
