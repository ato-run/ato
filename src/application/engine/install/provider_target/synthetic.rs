use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use semver::Version;
use serde_json::json;
use tar::Archive;
use zip::ZipArchive;

use super::*;

pub(super) fn materialize_provider_run_workspace(
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
    keep_failed_artifacts: bool,
    json_output: bool,
) -> Result<ProviderRunWorkspace> {
    let workspace_root = unique_provider_workspace_root(target.provider)?;
    fs::create_dir_all(&workspace_root)
        .with_context(|| format!("failed to create {}", workspace_root.display()))?;
    let mut guard = WorkspaceGuard::new(workspace_root.clone());
    let target = target.clone();

    let result = run_blocking_provider_materialization({
        let workspace_root = workspace_root.clone();
        move || -> Result<ProviderRunWorkspace> {
            let (metadata_path, lock) = match target.provider {
                ProviderKind::PyPI => {
                    build_pypi_workspace(&workspace_root, &target, requested_toolchain)?
                }
                ProviderKind::Npm => {
                    build_npm_workspace(&workspace_root, &target, requested_toolchain)?
                }
            };

            persist_provider_authoritative_lock(&workspace_root, &metadata_path, &lock)?;

            Ok(ProviderRunWorkspace {
                target,
                workspace_root,
                resolution_metadata_path: metadata_path,
            })
        }
    });

    if result.is_ok() {
        guard.keep();
    }

    if result.is_err() && keep_failed_artifacts {
        guard.keep();
        maybe_report_kept_failed_provider_workspace(&workspace_root, json_output);
    }

    result
}

fn run_blocking_provider_materialization<F, T>(operation: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::spawn(operation)
            .join()
            .map_err(|_| anyhow::anyhow!("provider materialization thread panicked"))?;
    }

    operation()
}

fn build_pypi_workspace(
    workspace_root: &std::path::Path,
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
) -> Result<(PathBuf, AtoLock)> {
    let package_ref = parse_pypi_requirement_ref(&target.ref_string)?;
    let effective_toolchain =
        resolve_effective_provider_toolchain(target.provider, requested_toolchain)?;
    let resolved = resolve_pypi_distribution(&package_ref)?;

    let requirements_path = workspace_root.join(PROVIDER_REQUIREMENTS_FILE);
    fs::write(
        &requirements_path,
        format!("{}\n", resolved.pinned_requirement),
    )
    .with_context(|| format!("failed to write {}", requirements_path.display()))?;

    let wrapper_path = workspace_root.join("main.py");
    fs::write(
        &wrapper_path,
        python_wrapper_for_entrypoint(
            &resolved.entrypoint.entrypoint_name,
            &resolved.entrypoint.entrypoint_value,
            PROVIDER_SITE_PACKAGES_DIR,
            &package_ref,
        )?,
    )
    .with_context(|| format!("failed to write {}", wrapper_path.display()))?;

    let lock = build_provider_authoritative_lock(
        &resolved.entrypoint.package_name,
        &resolved.entrypoint.package_version,
        "main.py",
        "python",
        PROVIDER_PYTHON_RUNTIME_VERSION,
        Some(json!({
            "python": PROVIDER_PYTHON_RUNTIME_VERSION,
            "uv": PROVIDER_UV_TOOL_VERSION,
        })),
        &resolved.allow_hosts,
    );

    let resolution_metadata_path = workspace_root.join(PROVIDER_RESOLUTION_METADATA_FILE);
    let metadata = ProviderResolutionMetadata {
        provider: target.provider.as_str().to_string(),
        r#ref: package_ref.canonical_ref(),
        resolution_role: "audit_provenance_only".to_string(),
        requested_provider_toolchain: requested_toolchain.as_str().to_string(),
        effective_provider_toolchain: effective_toolchain.as_str().to_string(),
        requested_package_name: package_ref.package_name.clone(),
        requested_extras: package_ref.extras.clone(),
        resolved_package_name: resolved.entrypoint.package_name.clone(),
        resolved_package_version: resolved.entrypoint.package_version.clone(),
        selected_entrypoint: resolved.entrypoint.entrypoint_value.clone(),
        generated_capsule_root: workspace_root.display().to_string(),
        generated_manifest_path: "route_lock:derived_at_run_time".to_string(),
        generated_wrapper_path: wrapper_path.display().to_string(),
        generated_authoritative_lock_path: None,
        index_source: resolved.index_source,
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

    Ok((resolution_metadata_path, lock))
}

fn build_npm_workspace(
    workspace_root: &std::path::Path,
    target: &ProviderTargetRef,
    requested_toolchain: ProviderToolchain,
) -> Result<(PathBuf, AtoLock)> {
    let package_ref = parse_npm_package_ref(&target.ref_string)?;
    let effective_toolchain =
        resolve_effective_provider_toolchain(target.provider, requested_toolchain)?;
    let resolved = resolve_npm_distribution(&package_ref)?;

    let package_json_path = workspace_root.join(PROVIDER_PACKAGE_JSON_FILE);
    fs::write(
        &package_json_path,
        serde_json::to_string_pretty(&json!({
            "name": synthetic_workspace_package_name(&package_ref.package_name),
            "private": true,
            "dependencies": {
                package_ref.package_name.clone(): resolved.entrypoint.package_version.clone(),
            },
        }))
        .context("failed to serialize synthetic npm package.json")?
            + "\n",
    )
    .with_context(|| format!("failed to write {}", package_json_path.display()))?;

    let wrapper_path = workspace_root.join("main.mjs");
    fs::write(
        &wrapper_path,
        node_wrapper_for_bin(&format!(
            "node_modules/{}/{}",
            package_ref.package_dir().display(),
            resolved.entrypoint.entrypoint_value
        ))?,
    )
    .with_context(|| format!("failed to write {}", wrapper_path.display()))?;

    let lock = build_provider_authoritative_lock(
        &resolved.entrypoint.package_name,
        &resolved.entrypoint.package_version,
        "main.mjs",
        "node",
        PROVIDER_NODE_RUNTIME_VERSION,
        Some(json!({
            "node": PROVIDER_NODE_RUNTIME_VERSION,
        })),
        &resolved.allow_hosts,
    );

    let resolution_metadata_path = workspace_root.join(PROVIDER_RESOLUTION_METADATA_FILE);
    let metadata = ProviderResolutionMetadata {
        provider: target.provider.as_str().to_string(),
        r#ref: package_ref.canonical_ref(),
        resolution_role: "audit_provenance_only".to_string(),
        requested_provider_toolchain: requested_toolchain.as_str().to_string(),
        effective_provider_toolchain: effective_toolchain.as_str().to_string(),
        requested_package_name: package_ref.package_name.clone(),
        requested_extras: Vec::new(),
        resolved_package_name: resolved.entrypoint.package_name.clone(),
        resolved_package_version: resolved.entrypoint.package_version.clone(),
        selected_entrypoint: resolved.entrypoint.entrypoint_value.clone(),
        generated_capsule_root: workspace_root.display().to_string(),
        generated_manifest_path: "route_lock:derived_at_run_time".to_string(),
        generated_wrapper_path: wrapper_path.display().to_string(),
        generated_authoritative_lock_path: None,
        index_source: resolved.index_source,
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

    Ok((resolution_metadata_path, lock))
}

fn build_provider_authoritative_lock(
    package_name: &str,
    package_version: &str,
    entrypoint: &str,
    driver: &str,
    runtime_version: &str,
    runtime_tools: Option<serde_json::Value>,
    allow_hosts: &[String],
) -> AtoLock {
    let mut lock = AtoLock::default();
    lock.contract.entries.insert(
        "metadata".to_string(),
        json!({
            "name": package_name,
            "version": package_version,
            "capsule_type": "app",
            "default_target": "app",
        }),
    );
    lock.contract.entries.insert(
        "process".to_string(),
        json!({
            "entrypoint": entrypoint,
        }),
    );
    lock.contract.entries.insert(
        "network".to_string(),
        json!({
            "egress_allow": allow_hosts,
            "egress_id_allow": [],
        }),
    );
    lock.resolution.entries.insert(
        "runtime".to_string(),
        json!({
            "kind": "source",
            "driver": driver,
            "version": runtime_version,
            "selected_target": "app",
        }),
    );
    let mut target = serde_json::Map::new();
    target.insert("label".to_string(), json!("app"));
    target.insert("runtime".to_string(), json!("source"));
    target.insert("driver".to_string(), json!(driver));
    target.insert("entrypoint".to_string(), json!(entrypoint));
    target.insert("runtime_version".to_string(), json!(runtime_version));
    target.insert("compatible".to_string(), json!(true));
    if let Some(runtime_tools) = runtime_tools {
        target.insert("runtime_tools".to_string(), runtime_tools);
    }
    lock.resolution.entries.insert(
        "resolved_targets".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::Object(target)]),
    );
    lock.resolution.entries.insert(
        "target_selection".to_string(),
        json!({
            "default_target": "app",
            "source": "provider_synthetic_workspace",
        }),
    );
    lock.resolution.entries.insert(
        "closure".to_string(),
        json!({
            "kind": "metadata_only",
            "status": "incomplete",
            "observed_lockfiles": [],
        }),
    );
    lock
}

struct ResolvedPyPIProvider {
    entrypoint: ResolvedProviderEntrypoint,
    pinned_requirement: String,
    allow_hosts: Vec<String>,
    index_source: String,
}

struct ResolvedNpmProvider {
    entrypoint: ResolvedProviderEntrypoint,
    allow_hosts: Vec<String>,
    index_source: String,
}

fn resolve_pypi_distribution(package_ref: &ParsedPyPIRequirement) -> Result<ResolvedPyPIProvider> {
    let index_source = effective_pypi_index_source();
    let index_url = pypi_index_base_url(&index_source);
    let normalized_package = normalize_pypi_name(&package_ref.package_name);
    let simple_url = format!(
        "{}/{}/",
        index_url.trim_end_matches('/'),
        normalized_package
    );
    let body = http_client()?
        .get(&simple_url)
        .send()
        .with_context(|| format!("failed to fetch {}", simple_url))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {}", simple_url))?
        .text()
        .with_context(|| format!("failed to read {}", simple_url))?;

    let mut candidates = extract_html_links(&simple_url, &body)?
        .into_iter()
        .filter_map(|url| wheel_candidate_for_url(&normalized_package, &url))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!(
            "Package '{}' did not expose a compatible wheel on the configured simple index.",
            package_ref.package_name
        );
    }
    candidates.sort_by(|left, right| compare_version_strings(&left.version, &right.version));
    let selected = candidates
        .pop()
        .context("provider-backed PyPI resolution selected no wheel candidate")?;

    let wheel_bytes = http_client()?
        .get(&selected.url)
        .send()
        .with_context(|| format!("failed to fetch {}", selected.url))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {}", selected.url))?
        .bytes()
        .with_context(|| format!("failed to read {}", selected.url))?;
    let entrypoint = resolve_pypi_entrypoint_from_wheel(&wheel_bytes, &package_ref.package_name)?;

    let pinned_requirement = if package_ref.extras.is_empty() {
        format!(
            "{}=={}",
            package_ref.package_name, entrypoint.package_version
        )
    } else {
        format!(
            "{}[{}]=={}",
            package_ref.package_name,
            package_ref.extras.join(","),
            entrypoint.package_version
        )
    };
    let allow_hosts = dedupe_hosts([
        host_from_url(&simple_url),
        host_from_url(&selected.url),
        (index_source == "default").then(|| "files.pythonhosted.org".to_string()),
    ]);

    Ok(ResolvedPyPIProvider {
        entrypoint,
        pinned_requirement,
        allow_hosts,
        index_source,
    })
}

fn resolve_npm_distribution(package_ref: &ParsedNpmRequirement) -> Result<ResolvedNpmProvider> {
    let index_source = effective_npm_index_source();
    let registry_url = npm_registry_base_url(&index_source);
    let packument_url = format!(
        "{}/{}",
        registry_url.trim_end_matches('/'),
        encode_npm_registry_package_path(&package_ref.package_name)
    );
    let packument: serde_json::Value = http_client()?
        .get(&packument_url)
        .send()
        .with_context(|| format!("failed to fetch {}", packument_url))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {}", packument_url))?
        .json()
        .with_context(|| format!("failed to parse {}", packument_url))?;

    let version = packument
        .get("dist-tags")
        .and_then(|value| value.get("latest"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("npm provider packument is missing dist-tags.latest")?;
    let tarball_url = packument
        .get("versions")
        .and_then(|value| value.get(version))
        .and_then(|value| value.get("dist"))
        .and_then(|value| value.get("tarball"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("npm provider packument is missing versions.<version>.dist.tarball")?;

    let tarball_bytes = http_client()?
        .get(tarball_url)
        .send()
        .with_context(|| format!("failed to fetch {tarball_url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {tarball_url}"))?
        .bytes()
        .with_context(|| format!("failed to read {tarball_url}"))?;
    let manifest = npm_manifest_from_tarball(&tarball_bytes)?;
    let entrypoint = resolve_npm_entrypoint_from_manifest(manifest, &package_ref.package_name)?;
    let allow_hosts = dedupe_hosts([host_from_url(&packument_url), host_from_url(tarball_url)]);

    Ok(ResolvedNpmProvider {
        entrypoint,
        allow_hosts,
        index_source,
    })
}

fn resolve_pypi_entrypoint_from_wheel(
    wheel_bytes: &[u8],
    requested_package: &str,
) -> Result<ResolvedProviderEntrypoint> {
    let cursor = Cursor::new(wheel_bytes.to_vec());
    let mut archive = ZipArchive::new(cursor).context("failed to open wheel archive")?;
    let mut metadata = None;
    let mut entry_points = None;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .context("failed to read wheel member")?;
        let name = file.name().to_string();
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .with_context(|| format!("failed to read wheel member {name}"))?;
        if name.ends_with("/METADATA") {
            metadata = Some(contents);
        } else if name.ends_with("/entry_points.txt") {
            entry_points = Some(contents);
        }
    }

    let metadata = metadata.context("wheel metadata is missing METADATA")?;
    let distribution_name =
        metadata_header(&metadata, "Name").context("wheel metadata is missing Name header")?;
    let distribution_version = metadata_header(&metadata, "Version")
        .context("wheel metadata is missing Version header")?;
    if normalize_pypi_name(&distribution_name) != normalize_pypi_name(requested_package) {
        bail!(
            "Package '{}' resolved to unexpected distribution '{}'.",
            requested_package,
            distribution_name
        );
    }

    let mut console_scripts = entry_points
        .map(|value| parse_console_scripts(&value))
        .unwrap_or_default();
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
    let (entrypoint_name, entrypoint_value) = console_scripts.remove(0);
    validate_entrypoint_value(&entrypoint_value)?;

    Ok(ResolvedProviderEntrypoint {
        package_name: distribution_name,
        package_version: distribution_version,
        entrypoint_name,
        entrypoint_value,
    })
}

fn resolve_npm_entrypoint_from_manifest(
    manifest: NpmPackageManifest,
    requested_package: &str,
) -> Result<ResolvedProviderEntrypoint> {
    reject_npm_lifecycle_scripts(manifest.scripts.as_ref(), requested_package)?;

    let package_name = manifest
        .name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| requested_package.to_string());
    let package_version = manifest
        .version
        .filter(|value| !value.trim().is_empty())
        .context("npm provider package manifest is missing version")?;
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

    let (entrypoint_name, entrypoint_value) = entries.remove(0);
    validate_npm_bin_path(&entrypoint_value)?;
    Ok(ResolvedProviderEntrypoint {
        package_name,
        package_version,
        entrypoint_name,
        entrypoint_value,
    })
}

fn npm_manifest_from_tarball(bytes: &[u8]) -> Result<NpmPackageManifest> {
    let archive = GzDecoder::new(Cursor::new(bytes));
    let mut tar = Archive::new(archive);
    for entry in tar
        .entries()
        .context("failed to read npm tarball entries")?
    {
        let mut entry = entry.context("failed to read npm tarball entry")?;
        let path = entry.path().context("failed to read npm tarball path")?;
        if path == PathBuf::from("package/package.json") {
            let mut raw = String::new();
            entry
                .read_to_string(&mut raw)
                .context("failed to read package/package.json from npm tarball")?;
            return serde_json::from_str(&raw)
                .context("failed to parse package/package.json from npm tarball");
        }
    }
    bail!("npm tarball did not include package/package.json")
}

fn extract_html_links(base_url: &str, html: &str) -> Result<Vec<String>> {
    let base = reqwest::Url::parse(base_url)
        .with_context(|| format!("failed to parse simple index url {base_url}"))?;
    let regex = regex::Regex::new(r#"href\s*=\s*[\"']([^\"']+)[\"']"#)
        .expect("static href regex must compile");
    regex
        .captures_iter(html)
        .filter_map(|captures| captures.get(1).map(|value| value.as_str().to_string()))
        .map(|href| {
            base.join(&href)
                .map(|url| url.to_string())
                .with_context(|| format!("failed to resolve {href} against {base_url}"))
        })
        .collect()
}

struct WheelCandidate {
    version: String,
    url: String,
}

fn wheel_candidate_for_url(normalized_package: &str, url: &str) -> Option<WheelCandidate> {
    let filename = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.path_segments()?.next_back().map(str::to_string))
        .or_else(|| {
            url.split(['#', '?'])
                .next()
                .and_then(|value| value.rsplit('/').next().map(str::to_string))
        })?;
    if !filename.ends_with(".whl") {
        return None;
    }
    let stem = filename.strip_suffix(".whl")?;
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    let version = parts.get(parts.len().saturating_sub(4))?.to_string();
    let distribution = parts[..parts.len().saturating_sub(4)].join("-");
    if normalize_pypi_name(&distribution) != normalized_package {
        return None;
    }
    Some(WheelCandidate {
        version,
        url: url.to_string(),
    })
}

fn compare_version_strings(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn http_client() -> Result<Client> {
    Client::builder()
        .build()
        .context("failed to construct provider resolution HTTP client")
}

fn effective_pypi_index_source() -> String {
    current_provider_index_source(ProviderKind::PyPI)
}

fn effective_npm_index_source() -> String {
    current_provider_index_source(ProviderKind::Npm)
}

fn pypi_index_base_url(index_source: &str) -> String {
    if index_source == "default" {
        "https://pypi.org/simple".to_string()
    } else {
        index_source.to_string()
    }
}

fn npm_registry_base_url(index_source: &str) -> String {
    if index_source == "default" {
        "https://registry.npmjs.org".to_string()
    } else {
        index_source.to_string()
    }
}

fn host_from_url(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
}

fn dedupe_hosts<const N: usize>(candidates: [Option<String>; N]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut hosts = Vec::new();
    for candidate in candidates.into_iter().flatten() {
        if seen.insert(candidate.clone()) {
            hosts.push(candidate);
        }
    }
    hosts
}

fn encode_npm_registry_package_path(package_name: &str) -> String {
    package_name.replace('/', "%2f")
}

fn synthetic_workspace_package_name(package_name: &str) -> String {
    format!("ato-provider-{}", package_name.replace('/', "-"))
}

#[cfg(test)]
mod tests {
    use super::wheel_candidate_for_url;

    #[test]
    fn wheel_candidate_accepts_fragment_bearing_pypi_links() {
        let candidate = wheel_candidate_for_url(
            "markitdown",
            "https://files.pythonhosted.org/packages/6c/28/example/markitdown-0.0.1a1-py3-none-any.whl#sha256=012634784612d85dbf8b994dc8e20bfb2fb37dbc08bb130171c519a8f20b8002",
        )
        .expect("fragment-bearing wheel url should remain a candidate");

        assert_eq!(candidate.version, "0.0.1a1");
        assert!(candidate.url.contains("#sha256="));
    }
}
