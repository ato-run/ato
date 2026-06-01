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
    /// Tool is a GitHub release asset. `tag_template` and `asset_template`
    /// may contain a `{version}` placeholder; `asset_template` may also
    /// contain `{triple}`.
    /// `repo` is `"owner/repo"`.  `triple_style` controls how the current
    /// host platform is rendered into the `{triple}` placeholder.
    GithubRelease {
        repo: &'static str,
        tag_template: &'static str,
        asset_template: &'static str,
        triple_style: TripleStyle,
    },
}

/// Controls how the host platform is mapped to the `{triple}` placeholder
/// inside a `GithubRelease` `asset_template`.
#[derive(Debug, Clone, Copy)]
pub enum TripleStyle {
    /// Standard Rust target triple: `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, etc.
    Rust,
    /// Bun-style: `darwin-aarch64`, `linux-x64`, `windows-x64`.
    Bun,
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

pub static YARN: RuntimeToolSpec = RuntimeToolSpec {
    name: "yarn",
    default_version: "1.22.22",
    roles: &[
        ToolRole::DependencyResolver,
        ToolRole::DependencyMaterializer,
        ToolRole::ScriptRunner,
    ],
    depends_on: &["node"],
    fetch: FetchKind::NpmRegistry { package: "yarn" },
    layout: ToolLayout::NodeScript {
        rel_path: "package/bin/yarn.js",
    },
};

pub static BUN: RuntimeToolSpec = RuntimeToolSpec {
    name: "bun",
    default_version: "1.1.38",
    roles: &[
        ToolRole::DependencyResolver,
        ToolRole::DependencyMaterializer,
        ToolRole::ScriptRunner,
    ],
    depends_on: &[],
    fetch: FetchKind::GithubRelease {
        repo: "oven-sh/bun",
        tag_template: "bun-v{version}",
        asset_template: "bun-{triple}.zip",
        triple_style: TripleStyle::Bun,
    },
    layout: ToolLayout::NativeBinary {
        rel_path: "bun-{triple}/bun",
    },
};

pub static UV: RuntimeToolSpec = RuntimeToolSpec {
    name: "uv",
    default_version: "0.5.21",
    roles: &[
        ToolRole::DependencyResolver,
        ToolRole::DependencyMaterializer,
        ToolRole::ScriptRunner,
    ],
    depends_on: &[],
    fetch: FetchKind::GithubRelease {
        repo: "astral-sh/uv",
        tag_template: "{version}",
        asset_template: "uv-{triple}.tar.gz",
        triple_style: TripleStyle::Rust,
    },
    layout: ToolLayout::NativeBinary { rel_path: "uv" },
};

const REGISTRY: &[&RuntimeToolSpec] = &[&PNPM, &YARN, &BUN, &UV];

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
fn should_probe_host_runtime_tool(version_override: Option<&str>) -> bool {
    version_override.is_none()
}

pub async fn ensure_runtime_tool(
    spec: &RuntimeToolSpec,
    version_override: Option<&str>,
    deps: &ToolDeps,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<ToolHandle> {
    if should_probe_host_runtime_tool(version_override)
        && let Ok(found) = which::which(spec.name)
        && let Some(dir) = found.parent()
    {
        return Ok(ToolHandle {
            bin_dir: dir.to_path_buf(),
            version: String::new(),
            binary_sha256: String::new(),
        });
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
    install_runtime_tool_archive(spec, &version, deps, &archive_bytes)
}

fn build_fetch_url(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::NpmRegistry { package } => Ok(format!(
            "https://registry.npmjs.org/{package}/-/{package}-{version}.tgz"
        )),
        FetchKind::GithubRelease {
            repo,
            tag_template,
            asset_template,
            triple_style,
        } => {
            let tag = tag_template.replace("{version}", version);
            let triple = host_triple(*triple_style)?;
            let asset = asset_template
                .replace("{version}", version)
                .replace("{triple}", &triple);
            Ok(format!(
                "https://github.com/{repo}/releases/download/{tag}/{asset}"
            ))
        }
    }
}

fn archive_filename(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::NpmRegistry { package } => Ok(format!("{package}-{version}.tgz")),
        FetchKind::GithubRelease {
            asset_template,
            triple_style,
            ..
        } => {
            let triple = host_triple(*triple_style)?;
            Ok(asset_template
                .replace("{version}", version)
                .replace("{triple}", &triple))
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
                CapsuleError::Pack(format!("Failed to open {}: {}", archive_path.display(), e))
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
        "zip" => {
            use std::io;
            let file = fs::File::open(archive_path).map_err(|e| {
                CapsuleError::Pack(format!("Failed to open {}: {}", archive_path.display(), e))
            })?;
            let mut archive = zip::ZipArchive::new(file).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to read zip {}: {}",
                    archive_path.display(),
                    e
                ))
            })?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| CapsuleError::Pack(format!("zip entry error: {}", e)))?;
                let relative = entry.enclosed_name().ok_or_else(|| {
                    CapsuleError::Pack(format!("zip entry escapes destination: {}", entry.name()))
                })?;
                ensure_safe_relative(&relative)?;
                if entry.is_symlink() {
                    return Err(CapsuleError::Pack(format!(
                        "zip entry is an unsupported symlink: {}",
                        entry.name()
                    )));
                }
                let out_path = dest.join(relative);
                if entry.is_dir() {
                    fs::create_dir_all(&out_path).map_err(|e| {
                        CapsuleError::Pack(format!(
                            "Failed to create zip dir {}: {}",
                            out_path.display(),
                            e
                        ))
                    })?;
                } else {
                    if let Some(parent) = out_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            CapsuleError::Pack(format!(
                                "Failed to create parent {}: {}",
                                parent.display(),
                                e
                            ))
                        })?;
                    }
                    let mut out_file = fs::File::create(&out_path).map_err(|e| {
                        CapsuleError::Pack(format!(
                            "Failed to create {}: {}",
                            out_path.display(),
                            e
                        ))
                    })?;
                    io::copy(&mut entry, &mut out_file).map_err(|e| {
                        CapsuleError::Pack(format!("Failed to write {}: {}", out_path.display(), e))
                    })?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Some(mode) = entry.unix_mode() {
                            fs::set_permissions(&out_path, fs::Permissions::from_mode(mode)).ok();
                        }
                    }
                }
            }
            Ok(())
        }
        other => Err(CapsuleError::Pack(format!(
            "unsupported archive type: {other}"
        ))),
    }
}

fn install_runtime_tool_archive(
    spec: &RuntimeToolSpec,
    version: &str,
    deps: &ToolDeps,
    archive_bytes: &[u8],
) -> Result<ToolHandle> {
    let tools_root = toolchain_cache_dir()?
        .join("tools")
        .join(spec.name)
        .join(version);
    let extracted_dir = tools_root.join("extracted");
    let shim_dir = tools_root.join("shim");
    let sha_path = tools_root.join("binary.sha256");
    let archive_sha256 = hex::encode(Sha256::digest(archive_bytes));

    let shim_filename = if cfg!(windows) {
        format!("{}.cmd", spec.name)
    } else {
        spec.name.to_string()
    };
    let shim_path = shim_dir.join(&shim_filename);

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

    let archive_path = tools_root.join(archive_filename(&spec.fetch, version)?);
    fs::write(&archive_path, archive_bytes).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write archive {}: {}",
            archive_path.display(),
            e
        ))
    })?;
    extract_archive(&archive_path, &extracted_dir)?;

    let target_path = extracted_dir.join(resolved_layout_path(spec)?);
    if !target_path.is_file() {
        return Err(CapsuleError::Pack(format!(
            "{} archive missing expected file {}",
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
        version: version.to_string(),
        binary_sha256: archive_sha256,
    })
}

fn resolved_layout_path(spec: &RuntimeToolSpec) -> Result<String> {
    let target_rel = match &spec.layout {
        ToolLayout::NativeBinary { rel_path } | ToolLayout::NodeScript { rel_path } => rel_path,
    };
    match &spec.fetch {
        FetchKind::GithubRelease { triple_style, .. } => {
            let triple = host_triple(*triple_style)?;
            Ok(target_rel.replace("{triple}", &triple))
        }
        _ => Ok(target_rel.to_string()),
    }
}

/// Maps the current host platform to a tool-specific triple string.
/// Used to resolve `{triple}` placeholders in `GithubRelease` asset templates.
fn host_triple(style: TripleStyle) -> Result<String> {
    match style {
        TripleStyle::Rust => {
            let triple = match (
                cfg!(target_os = "macos"),
                cfg!(target_os = "linux"),
                cfg!(target_os = "windows"),
                cfg!(target_arch = "aarch64"),
                cfg!(target_arch = "x86_64"),
            ) {
                (true, _, _, true, _) => "aarch64-apple-darwin",
                (true, _, _, _, true) => "x86_64-apple-darwin",
                (_, true, _, true, _) => "aarch64-unknown-linux-gnu",
                (_, true, _, _, true) => "x86_64-unknown-linux-gnu",
                (_, _, true, _, true) => "x86_64-pc-windows-msvc",
                _ => {
                    return Err(CapsuleError::Pack(
                        "Unsupported platform for Rust triple".to_string(),
                    ));
                }
            };
            Ok(triple.to_string())
        }
        TripleStyle::Bun => {
            let triple = match (
                cfg!(target_os = "macos"),
                cfg!(target_os = "linux"),
                cfg!(target_os = "windows"),
                cfg!(target_arch = "aarch64"),
                cfg!(target_arch = "x86_64"),
            ) {
                (true, _, _, true, _) => "darwin-aarch64",
                (true, _, _, _, true) => "darwin-x64",
                (_, true, _, true, _) => "linux-aarch64",
                (_, true, _, _, true) => "linux-x64",
                (_, _, true, _, true) => "windows-x64",
                _ => {
                    return Err(CapsuleError::Pack(
                        "Unsupported platform for Bun triple".to_string(),
                    ));
                }
            };
            Ok(triple.to_string())
        }
    }
}

fn ensure_safe_relative(path: &Path) -> Result<()> {
    if path.is_absolute() {
        return Err(CapsuleError::Pack(format!(
            "archive entry has absolute path: {}",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(CapsuleError::Pack(format!(
                    "archive entry escapes destination via '..': {}",
                    path.display()
                )));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(CapsuleError::Pack(format!(
                    "archive entry has root component: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
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
                let body = format!("@echo off\r\n\"{node_quoted}\" \"{target_quoted}\" %*\r\n");
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
    fs::set_permissions(path, perms)
        .map_err(|e| CapsuleError::Pack(format!("Failed to chmod shim {}: {}", path.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::reporter::NoOpReporter;
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;

    fn parse(toml_str: &str) -> toml::Value {
        toml_str.parse().expect("parse toml")
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    fn scoped_env(key: &'static str, value: Option<&str>) -> EnvGuard {
        let previous = std::env::var_os(key);
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
        EnvGuard { key, previous }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn unique_version(tag: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        format!("0.0.0-test-{tag}-{nanos}")
    }

    fn build_npm_tgz(rel_path: &str) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let payload = b"#!/usr/bin/env node\nconsole.log('ok');\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, rel_path, Cursor::new(payload))
            .expect("append npm entry");
        let tar = builder.into_inner().expect("finish tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).expect("write tgz");
        gz.finish().expect("finish tgz")
    }

    fn build_bun_zip() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        let path = resolved_layout_path(&BUN).expect("bun layout");
        zip.start_file(path, options).expect("start bun file");
        zip.write_all(b"bun binary").expect("write bun");
        zip.finish().expect("finish bun zip");
        cursor.into_inner()
    }

    fn build_uv_tgz() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let payload = b"#!/bin/sh\necho uv\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                resolved_layout_path(&UV).expect("uv layout"),
                Cursor::new(payload),
            )
            .expect("append uv");
        let tar = builder.into_inner().expect("finish uv tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).expect("write uv tgz");
        gz.finish().expect("finish uv tgz")
    }

    fn write_zip_archive(path: &Path, entries: &[(&str, &[u8], Option<u32>)]) {
        let file = File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, contents, mode) in entries {
            let mut options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            if let Some(mode) = mode {
                options = options.unix_permissions(*mode);
            }
            zip.start_file(*name, options).expect("start zip file");
            zip.write_all(contents).expect("write zip contents");
        }
        zip.finish().expect("finish zip");
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
    fn pinned_tool_version_disables_host_shortcut() {
        assert!(should_probe_host_runtime_tool(None));
        assert!(!should_probe_host_runtime_tool(Some("1.2.8")));
    }

    #[test]
    fn registry_contains_all_slice_b_tools() {
        assert!(lookup("pnpm").is_some());
        assert!(lookup("yarn").is_some());
        assert!(lookup("bun").is_some());
        assert!(lookup("uv").is_some());
    }

    #[test]
    fn tool_registry_builds_expected_urls_and_archive_names() {
        let cases = [
            (
                &PNPM,
                "9.12.0",
                "https://registry.npmjs.org/pnpm/-/pnpm-9.12.0.tgz",
                "pnpm-9.12.0.tgz",
            ),
            (
                &YARN,
                "1.22.22",
                "https://registry.npmjs.org/yarn/-/yarn-1.22.22.tgz",
                "yarn-1.22.22.tgz",
            ),
        ];
        for (spec, version, expected_url, expected_archive) in cases {
            assert_eq!(build_fetch_url(&spec.fetch, version).unwrap(), expected_url);
            assert_eq!(
                archive_filename(&spec.fetch, version).unwrap(),
                expected_archive
            );
        }

        let bun_triple = host_triple(TripleStyle::Bun).unwrap();
        let uv_triple = host_triple(TripleStyle::Rust).unwrap();
        assert_eq!(
            build_fetch_url(&BUN.fetch, "1.2.8").unwrap(),
            format!(
                "https://github.com/oven-sh/bun/releases/download/bun-v1.2.8/bun-{bun_triple}.zip"
            )
        );
        assert_eq!(
            archive_filename(&BUN.fetch, "1.2.8").unwrap(),
            format!("bun-{bun_triple}.zip")
        );
        assert_eq!(
            build_fetch_url(&UV.fetch, "0.5.21").unwrap(),
            format!(
                "https://github.com/astral-sh/uv/releases/download/0.5.21/uv-{uv_triple}.tar.gz"
            )
        );
        assert_eq!(
            archive_filename(&UV.fetch, "0.5.21").unwrap(),
            format!("uv-{uv_triple}.tar.gz")
        );
    }

    #[test]
    fn tool_registry_resolves_expected_binary_paths() {
        assert_eq!(resolved_layout_path(&PNPM).unwrap(), "package/bin/pnpm.cjs");
        assert_eq!(resolved_layout_path(&YARN).unwrap(), "package/bin/yarn.js");
        assert_eq!(
            resolved_layout_path(&BUN).unwrap(),
            format!("bun-{}/bun", host_triple(TripleStyle::Bun).unwrap())
        );
        assert_eq!(resolved_layout_path(&UV).unwrap(), "uv");
    }

    #[test]
    #[serial_test::serial]
    fn install_runtime_tool_archive_smoke_covers_all_slice_b_tools() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));
        let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(NoOpReporter);
        let _ = reporter;

        let pnpm_version = unique_version("pnpm");
        let pnpm_handle = install_runtime_tool_archive(
            &PNPM,
            &pnpm_version,
            &ToolDeps {
                node_bin: Some(PathBuf::from("/usr/bin/node")),
            },
            &build_npm_tgz("package/bin/pnpm.cjs"),
        )
        .expect("install pnpm");
        assert!(
            pnpm_handle
                .bin_dir
                .join(if cfg!(windows) { "pnpm.cmd" } else { "pnpm" })
                .exists()
        );
        assert!(
            ato_home
                .path()
                .join("toolchains/tools/pnpm")
                .join(&pnpm_version)
                .join("extracted")
                .join("package/bin/pnpm.cjs")
                .is_file()
        );

        let yarn_version = unique_version("yarn");
        let yarn_handle = install_runtime_tool_archive(
            &YARN,
            &yarn_version,
            &ToolDeps {
                node_bin: Some(PathBuf::from("/usr/bin/node")),
            },
            &build_npm_tgz("package/bin/yarn.js"),
        )
        .expect("install yarn");
        assert!(
            yarn_handle
                .bin_dir
                .join(if cfg!(windows) { "yarn.cmd" } else { "yarn" })
                .exists()
        );
        assert!(
            ato_home
                .path()
                .join("toolchains/tools/yarn")
                .join(&yarn_version)
                .join("extracted")
                .join("package/bin/yarn.js")
                .is_file()
        );

        let bun_version = unique_version("bun");
        let bun_handle = install_runtime_tool_archive(
            &BUN,
            &bun_version,
            &ToolDeps::default(),
            &build_bun_zip(),
        )
        .expect("install bun");
        assert!(
            bun_handle
                .bin_dir
                .join(if cfg!(windows) { "bun.cmd" } else { "bun" })
                .exists()
        );
        assert!(
            ato_home
                .path()
                .join("toolchains/tools/bun")
                .join(&bun_version)
                .join("extracted")
                .join(resolved_layout_path(&BUN).unwrap())
                .is_file()
        );

        let uv_version = unique_version("uv");
        let uv_handle =
            install_runtime_tool_archive(&UV, &uv_version, &ToolDeps::default(), &build_uv_tgz())
                .expect("install uv");
        assert!(
            uv_handle
                .bin_dir
                .join(if cfg!(windows) { "uv.cmd" } else { "uv" })
                .exists()
        );
        assert!(
            ato_home
                .path()
                .join("toolchains/tools/uv")
                .join(&uv_version)
                .join("extracted")
                .join("uv")
                .is_file()
        );
    }

    #[test]
    fn extract_archive_rejects_zip_path_traversal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("bad.zip");
        write_zip_archive(&archive, &[("../escape", b"bad", None)]);

        let err = extract_archive(&archive, temp.path()).expect_err("zip traversal must fail");
        let message = err.to_string();
        assert!(
            message.contains("escapes destination"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn extract_archive_rejects_zip_symlinks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("bad-link.zip");
        let file = File::create(&archive).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        zip.add_symlink(
            "bun-link",
            "/tmp/elsewhere",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .expect("add symlink");
        zip.finish().expect("finish zip");

        let err = extract_archive(&archive, temp.path()).expect_err("zip symlink must fail");
        let message = err.to_string();
        assert!(
            message.contains("unsupported symlink"),
            "unexpected error: {message}"
        );
    }
}
