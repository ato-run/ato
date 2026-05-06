//! Unified runtime tools registry.
//!
//! This module is the single source of truth for **execution tools** —
//! `pnpm`, `yarn`, `bun`, `uv`, `git`, etc. Adding a new tool means adding
//! one `RuntimeToolSpec` to the registry; the fetch/extract/shim plumbing is
//! shared.
//!
//! Naming caveat: the legacy manifest schema spells per-target tool pins as
//! `runtime_tools.<name>`. That term is preserved for backwards
//! compatibility, but it is misleading — the items in this registry are
//! *not* runtimes. Runtimes (node / python / deno / wasmtime) execute the
//! program; the tools here only prepare or launch the execution world. The
//! distinction matters because conflating them collapses the
//! Node/Python/Deno/Wasm/Native driver model.
//!
//! Manifest surface (canonical):
//! ```toml
//! [[tools]]
//! name = "pnpm"
//! version = "9.12.0"
//! ```
//! Lock surface (map keyed by tool name) lives in
//! [`crate::contract::lockfile::ToolSection`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::bootstrap::{BootstrapBoundary, BootstrapVerificationKind};
use crate::common::paths::toolchain_cache_dir;
use crate::error::{CapsuleError, Result};
use crate::reporter::CapsuleReporter;

/// Logical role a tool plays in the execution model. Roles live in the
/// registry so the manifest stays free of role declarations: the user writes
/// `[[tools]] name = "pnpm"`, the registry decides what pnpm is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRole {
    /// Decides which versions of which packages are needed (lockfile producer).
    DependencyResolver,
    /// Materializes the resolved set into the workspace (`node_modules`, `venv`).
    DependencyMaterializer,
    /// Invokes user-defined scripts (`pnpm run`, `npm run`, `uv run`).
    ScriptRunner,
    /// Compiles source into artefacts (`cargo`, `cmake`, `node-gyp`, `maturin`).
    BuildTool,
    /// Acquires source from external location (`git`).
    SourceMaterializer,
    /// Host-side UI / editor. **Not** part of execution identity; expressed as
    /// a capability bridge instead of a packed dependency.
    HostIntegration,
}

#[derive(Debug, Clone, Copy)]
pub enum FetchKind {
    /// Tool is published as an npm tarball at
    /// `https://registry.npmjs.org/<package>/-/<package>-<version>.tgz`.
    NpmRegistry { package: &'static str },
    /// Tool is a GitHub release asset. `asset_template` may contain
    /// `{version}` and `{triple}` placeholders.
    /// Slice A: declared but not yet resolved at runtime (no consumer).
    GithubRelease {
        repo: &'static str,
        asset_template: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum ToolLayout {
    /// Native executable inside the archive at `rel_path`.
    NativeBinary { rel_path: &'static str },
    /// Node.js script (e.g. `bin/pnpm.cjs`) that needs `node` to invoke.
    NodeScript { rel_path: &'static str },
}

#[derive(Debug, Clone)]
pub struct RuntimeToolSpec {
    pub name: &'static str,
    pub default_version: &'static str,
    pub roles: &'static [ToolRole],
    /// Names of other tools/runtimes whose binary path must be supplied via
    /// [`ToolDeps`] before this tool can be invoked. For pnpm this is `node`.
    pub depends_on: &'static [&'static str],
    pub fetch: FetchKind,
    pub layout: ToolLayout,
}

pub static PNPM: RuntimeToolSpec = RuntimeToolSpec {
    name: "pnpm",
    default_version: "9.9.0",
    roles: &[
        ToolRole::DependencyResolver,
        ToolRole::DependencyMaterializer,
        ToolRole::ScriptRunner,
    ],
    depends_on: &["node"],
    fetch: FetchKind::NpmRegistry { package: "pnpm" },
    layout: ToolLayout::NodeScript {
        rel_path: "package/bin/pnpm.cjs",
    },
};

const REGISTRY: &[&RuntimeToolSpec] = &[&PNPM];

pub fn registry() -> &'static [&'static RuntimeToolSpec] {
    REGISTRY
}

pub fn lookup(name: &str) -> Option<&'static RuntimeToolSpec> {
    REGISTRY.iter().copied().find(|spec| spec.name == name)
}

/// Per-call dependency injection. Keeping resolution in the caller avoids a
/// dependency back into the runtime manager (ato-cli) from capsule-core.
#[derive(Debug, Default, Clone)]
pub struct ToolDeps {
    pub node_bin: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ToolHandle {
    /// Directory to prepend to PATH so the tool name resolves to the shim.
    pub bin_dir: PathBuf,
    pub version: String,
    /// SHA-256 of the downloaded archive. Empty when the host tool was used
    /// directly. Suitable for recording into
    /// `capsule.lock.json::tools.<name>.binary_sha256`.
    pub binary_sha256: String,
}

/// Reads the requested tool version from the manifest with this dispatch:
///
/// 1. If the top-level `tools` key is a TOML *array* → treat as the canonical
///    `[[tools]]` form and search by `name`.
/// 2. If `tools` is a TOML *table* → treat as the transitional `[tools.<name>]`
///    alias.
/// 3. If `tools` is absent → fall back to the legacy
///    `targets.<target_label>.runtime_tools.<name>` entry.
///
/// `[[tools]]` and `[tools.<name>]` cannot coexist in TOML, so this is a type
/// dispatch rather than a precedence list. When `tools` is present but the
/// requested name is missing, we deliberately do **not** fall back to legacy
/// — the user opted into the new schema.
pub fn read_tool_version(
    manifest: &toml::Value,
    target_label: &str,
    tool_name: &str,
) -> Option<String> {
    if let Some(tools) = manifest.get("tools") {
        if let Some(arr) = tools.as_array() {
            for entry in arr {
                let name_matches = entry
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|n| n == tool_name);
                if name_matches {
                    return entry
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string);
                }
            }
            return None;
        }
        if let Some(tbl) = tools.as_table() {
            return tbl
                .get(tool_name)
                .and_then(|entry| entry.get("version"))
                .and_then(toml::Value::as_str)
                .map(str::to_string);
        }
    }
    manifest
        .get("targets")
        .and_then(|t| t.get(target_label))
        .and_then(|t| t.get("runtime_tools"))
        .and_then(|rt| rt.get(tool_name))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

/// Provisions a runtime tool and returns a directory containing a shim that
/// resolves the tool name on PATH. If the tool is already on the host PATH the
/// containing directory is returned unchanged (no download).
pub async fn ensure_runtime_tool(
    spec: &RuntimeToolSpec,
    version_override: Option<&str>,
    deps: &ToolDeps,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<ToolHandle> {
    if let Ok(found) = which::which(spec.name) {
        if let Some(dir) = found.parent() {
            return Ok(ToolHandle {
                bin_dir: dir.to_path_buf(),
                version: String::new(),
                binary_sha256: String::new(),
            });
        }
    }

    let version = version_override.unwrap_or(spec.default_version).to_string();
    // Trust attribution for the network fetch. Used by future trust-boundary
    // integrations; harmless to construct now.
    let _boundary =
        BootstrapBoundary::network_tool(spec.name, BootstrapVerificationKind::ChecksumUnavailable);

    let tools_root = toolchain_cache_dir()?
        .join("tools")
        .join(spec.name)
        .join(&version);
    let extracted_dir = tools_root.join("extracted");
    let shim_dir = tools_root.join("shim");
    let sha_path = tools_root.join("binary.sha256");

    let shim_filename = if cfg!(windows) {
        format!("{}.cmd", spec.name)
    } else {
        spec.name.to_string()
    };
    let shim_path = shim_dir.join(&shim_filename);

    if shim_path.exists() {
        let cached_sha = fs::read_to_string(&sha_path).unwrap_or_default();
        return Ok(ToolHandle {
            bin_dir: shim_dir,
            version,
            binary_sha256: cached_sha.trim().to_string(),
        });
    }

    fs::create_dir_all(&tools_root).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool dir {}: {}",
            tools_root.display(),
            e
        ))
    })?;
    fs::create_dir_all(&extracted_dir).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool extract dir {}: {}",
            extracted_dir.display(),
            e
        ))
    })?;
    fs::create_dir_all(&shim_dir).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool shim dir {}: {}",
            shim_dir.display(),
            e
        ))
    })?;

    let url = build_fetch_url(&spec.fetch, &version)?;
    reporter
        .notify(format!("⬇️  Downloading {} {}", spec.name, version))
        .await?;
    let archive_bytes = download_bytes(&url).await?;
    let archive_sha256 = hex::encode(Sha256::digest(&archive_bytes));

    let archive_path = tools_root.join(archive_filename(&spec.fetch, &version));
    fs::write(&archive_path, &archive_bytes).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write archive {}: {}",
            archive_path.display(),
            e
        ))
    })?;
    extract_archive(&archive_path, &extracted_dir)?;

    let target_rel = match &spec.layout {
        ToolLayout::NativeBinary { rel_path } | ToolLayout::NodeScript { rel_path } => rel_path,
    };
    let target_path = extracted_dir.join(target_rel);
    if !target_path.exists() {
        return Err(CapsuleError::Pack(format!(
            "{} archive missing expected entry {}",
            spec.name,
            target_path.display()
        )));
    }

    write_shim(spec, deps, &target_path, &shim_path)?;

    fs::write(&sha_path, &archive_sha256).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write archive hash {}: {}",
            sha_path.display(),
            e
        ))
    })?;

    Ok(ToolHandle {
        bin_dir: shim_dir,
        version,
        binary_sha256: archive_sha256,
    })
}

fn build_fetch_url(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::NpmRegistry { package } => Ok(format!(
            "https://registry.npmjs.org/{package}/-/{package}-{version}.tgz"
        )),
        FetchKind::GithubRelease { .. } => Err(CapsuleError::Pack(
            "GithubRelease fetch is reserved for the next slice (yarn/bun/deno); \
             no consumer exists in Slice A"
                .to_string(),
        )),
    }
}

fn archive_filename(fetch: &FetchKind, version: &str) -> String {
    match fetch {
        FetchKind::NpmRegistry { package } => format!("{package}-{version}.tgz"),
        FetchKind::GithubRelease { asset_template, .. } => {
            asset_template.replace("{version}", version)
        }
    }
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(CapsuleError::Network)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(CapsuleError::Network)?;
    if !response.status().is_success() {
        return Err(CapsuleError::Network(
            response.error_for_status().unwrap_err(),
        ));
    }
    let bytes = response.bytes().await.map_err(CapsuleError::Network)?;
    Ok(bytes.to_vec())
}

fn extract_archive(archive_path: &Path, dest: &Path) -> Result<()> {
    let ext = archive_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    match ext {
        "tgz" | "gz" => {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let file = fs::File::open(archive_path).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to open {}: {}",
                    archive_path.display(),
                    e
                ))
            })?;
            Archive::new(GzDecoder::new(file))
                .unpack(dest)
                .map_err(|e| {
                    CapsuleError::Pack(format!(
                        "Failed to extract {}: {}",
                        archive_path.display(),
                        e
                    ))
                })
        }
        other => Err(CapsuleError::Pack(format!(
            "unsupported archive type: {other}"
        ))),
    }
}

fn write_shim(
    spec: &RuntimeToolSpec,
    deps: &ToolDeps,
    target_path: &Path,
    shim_path: &Path,
) -> Result<()> {
    let target_quoted = target_path.display().to_string().replace('"', "\\\"");
    match &spec.layout {
        ToolLayout::NodeScript { .. } => {
            // Fall back to bare `node` if the caller did not supply a node_bin.
            // The shim still works as long as `node` is on PATH at invocation
            // time (which preflight guarantees by prepending the managed Node
            // bin dir).
            let node_quoted = deps
                .node_bin
                .as_ref()
                .map(|p| p.display().to_string().replace('"', "\\\""))
                .unwrap_or_else(|| "node".to_string());
            #[cfg(unix)]
            {
                let body =
                    format!("#!/bin/sh\nexec \"{node_quoted}\" \"{target_quoted}\" \"$@\"\n");
                write_executable(shim_path, body.as_bytes())?;
            }
            #[cfg(windows)]
            {
                let body =
                    format!("@echo off\r\n\"{node_quoted}\" \"{target_quoted}\" %*\r\n");
                fs::write(shim_path, body).map_err(|e| {
                    CapsuleError::Pack(format!(
                        "Failed to write shim {}: {}",
                        shim_path.display(),
                        e
                    ))
                })?;
            }
        }
        ToolLayout::NativeBinary { .. } => {
            #[cfg(unix)]
            {
                let body = format!("#!/bin/sh\nexec \"{target_quoted}\" \"$@\"\n");
                write_executable(shim_path, body.as_bytes())?;
            }
            #[cfg(windows)]
            {
                let body = format!("@echo off\r\n\"{target_quoted}\" %*\r\n");
                fs::write(shim_path, body).map_err(|e| {
                    CapsuleError::Pack(format!(
                        "Failed to write shim {}: {}",
                        shim_path.display(),
                        e
                    ))
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, bytes).map_err(|e| {
        CapsuleError::Pack(format!("Failed to write shim {}: {}", path.display(), e))
    })?;
    let mut perms = fs::metadata(path)
        .map_err(|e| CapsuleError::Pack(format!("Failed to stat shim {}: {}", path.display(), e)))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| {
        CapsuleError::Pack(format!("Failed to chmod shim {}: {}", path.display(), e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> toml::Value {
        toml_str.parse().expect("parse toml")
    }

    #[test]
    fn read_tool_version_array_form() {
        let manifest = parse(
            r#"
            [[tools]]
            name = "pnpm"
            version = "9.12.0"

            [[tools]]
            name = "uv"
            version = "0.5.10"
            "#,
        );
        assert_eq!(
            read_tool_version(&manifest, "main", "pnpm").as_deref(),
            Some("9.12.0")
        );
        assert_eq!(
            read_tool_version(&manifest, "main", "uv").as_deref(),
            Some("0.5.10")
        );
        assert_eq!(read_tool_version(&manifest, "main", "yarn"), None);
    }

    #[test]
    fn read_tool_version_table_form_alias() {
        let manifest = parse(
            r#"
            [tools.pnpm]
            version = "9.12.0"
            "#,
        );
        assert_eq!(
            read_tool_version(&manifest, "main", "pnpm").as_deref(),
            Some("9.12.0")
        );
    }

    #[test]
    fn read_tool_version_legacy_runtime_tools() {
        let manifest = parse(
            r#"
            [targets.main.runtime_tools]
            pnpm = "9.12.0"
            "#,
        );
        assert_eq!(
            read_tool_version(&manifest, "main", "pnpm").as_deref(),
            Some("9.12.0")
        );
    }

    #[test]
    fn read_tool_version_canonical_blocks_legacy_fallback() {
        // Once the user adopts [[tools]], legacy is deliberately ignored.
        let manifest = parse(
            r#"
            [[tools]]
            name = "pnpm"
            version = "9.12.0"

            [targets.main.runtime_tools]
            yarn = "1.22.0"
            "#,
        );
        assert_eq!(
            read_tool_version(&manifest, "main", "yarn"),
            None,
            "legacy runtime_tools must be ignored once [[tools]] is present"
        );
    }

    #[test]
    fn registry_contains_pnpm_only_in_slice_a() {
        assert_eq!(registry().len(), 1);
        assert_eq!(lookup("pnpm").map(|s| s.name), Some("pnpm"));
        assert!(lookup("yarn").is_none());
    }
}
