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
        asset_template_windows: Option<&'static str>,
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
        asset_template_windows: None,
        triple_style: TripleStyle::Bun,
    },
    layout: ToolLayout::NativeBinary {
        // Bun's Windows zip ships `bun-windows-x64/bun.exe`; without the
        // `{exe_suffix}` placeholder the Windows entry could never resolve.
        rel_path: "bun-{triple}/bun{exe_suffix}",
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
        asset_template_windows: Some("uv-{triple}.zip"),
        triple_style: TripleStyle::Rust,
    },
    layout: ToolLayout::NativeBinary {
        rel_path: "uv{exe_suffix}",
    },
};

// Deno is intentionally not a `RuntimeToolSpec` tool. It is a primary runtime,
// modeled in `RuntimeSection` (`runtimes.deno` in the lockfile) and handled by
// the Deno runtime path (`resolve_deno_runtime`, `generate_deno_lock`). This
// registry is only for auxiliary package/runtime tools — uv, pnpm, yarn, bun.
// Deno's absence here is not a bug and does not mean Deno is unsupported.
// See docs/dev-notes/runtime-vs-runtime-tools.md and #470.
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
    /// SHA-256 of the downloaded archive/distribution bytes.
    ///
    /// Empty when the host tool was used directly (no managed provisioning) or
    /// when served from cache (the archive bytes are not retained). Maps to
    /// `capsule.lock.json::tools.<name>.targets.<triple>.sha256`.
    pub archive_sha256: String,
    /// SHA-256 of the resolved executable/tool-entry file after extraction.
    ///
    /// This is the **resolved binary** hash, distinct from
    /// [`Self::archive_sha256`]. Empty when the host tool was used directly.
    /// Maps to `capsule.lock.json::tools.<name>.targets.<triple>.binary_sha256`.
    /// Historically this field carried the archive hash; that was the bug fixed
    /// in #469.
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
            // Host PATH tool: Ato did not provision it, so neither hash is
            // known. Left empty so callers serialize neither into the lockfile.
            archive_sha256: String::new(),
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
    fs::create_dir_all(&tools_root).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool dir {}: {}",
            tools_root.display(),
            e
        ))
    })?;
    // Serialize validation + rebuild of one cache entry across processes.
    // Without this, a concurrent session can delete/rewrite `extracted/`
    // while another is executing the binary from it — on Windows that
    // surfaces as "Access is denied" from half-written or vanishing tools.
    let _install_lock = acquire_tool_cache_lock(spec, &version, &tools_root)?;

    let extracted_dir = tools_root.join("extracted");
    let shim_dir = tools_root.join("shim");
    let sha_path = tools_root.join("binary.sha256");
    let shim_path = shim_dir.join(shim_filename(spec));

    if shim_path.exists() {
        match validate_tool_cache(spec, &shim_path, &extracted_dir, &sha_path) {
            Ok(binary_sha256) => {
                tracing::debug!(
                    tool = spec.name,
                    version = %version,
                    cache = %tools_root.display(),
                    "reusing validated runtime tool cache entry"
                );
                return Ok(ToolHandle {
                    bin_dir: shim_dir,
                    version,
                    // Cache hit: the archive bytes are not retained, so the
                    // archive hash is unknown here. `validate_tool_cache`
                    // recomputes the resolved binary hash from the target file.
                    archive_sha256: String::new(),
                    binary_sha256,
                });
            }
            Err(err) => {
                discard_tool_cache_entry(&extracted_dir, &shim_dir, &sha_path);
                tracing::warn!(
                    tool = spec.name,
                    version = %version,
                    cache = %tools_root.display(),
                    expected_executable = %resolved_layout_path(spec).unwrap_or_default(),
                    error = %err,
                    "runtime tool cache entry is invalid; cache not reused, rebuilding"
                );
            }
        }
    }

    let url = build_fetch_url(&spec.fetch, &version)?;
    reporter
        .notify(format!("⬇️  Downloading {} {}", spec.name, version))
        .await?;
    let archive_bytes = download_bytes(&url).await?;
    install_runtime_tool_archive_locked(spec, &version, deps, &archive_bytes)
}

fn shim_filename(spec: &RuntimeToolSpec) -> String {
    if cfg!(windows) {
        format!("{}.cmd", spec.name)
    } else {
        spec.name.to_string()
    }
}

/// Takes an exclusive advisory lock on the cache entry for one
/// `<tool>/<version>`. The lock is released when the returned handle drops.
fn acquire_tool_cache_lock(
    spec: &RuntimeToolSpec,
    version: &str,
    tools_root: &Path,
) -> Result<fs::File> {
    use fs2::FileExt;

    let lock_path = tools_root.join(".install.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to open tool cache lock {}: {}",
                lock_path.display(),
                e
            ))
        })?;
    if let Err(err) = lock.try_lock_exclusive() {
        if err.kind() != fs2::lock_contended_error().kind() {
            return Err(CapsuleError::Pack(format!(
                "Failed to lock tool cache {}: {}",
                lock_path.display(),
                err
            )));
        }
        tracing::info!(
            tool = spec.name,
            version = %version,
            "waiting for a concurrent install of this runtime tool to finish"
        );
        lock.lock_exclusive().map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to lock tool cache {}: {}",
                lock_path.display(),
                e
            ))
        })?;
    }
    Ok(lock)
}

fn discard_tool_cache_entry(extracted_dir: &Path, shim_dir: &Path, sha_path: &Path) {
    // The sha marker goes first: with it gone, a partially-deleted entry can
    // never validate as complete.
    fs::remove_file(sha_path).ok();
    fs::remove_dir_all(extracted_dir).ok();
    fs::remove_dir_all(shim_dir).ok();
}

fn build_fetch_url(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::NpmRegistry { package } => Ok(format!(
            "https://registry.npmjs.org/{package}/-/{package}-{version}.tgz"
        )),
        FetchKind::GithubRelease {
            repo, tag_template, ..
        } => {
            let tag = tag_template.replace("{version}", version);
            let asset = resolved_asset_name(fetch, version)?;
            Ok(format!(
                "https://github.com/{repo}/releases/download/{tag}/{asset}"
            ))
        }
    }
}

fn archive_filename(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::NpmRegistry { package } => Ok(format!("{package}-{version}.tgz")),
        FetchKind::GithubRelease { .. } => resolved_asset_name(fetch, version),
    }
}

/// Selects and expands the correct asset template for the given platform.
/// Extracted to allow platform-injected testing on non-Windows CI runners.
#[cfg_attr(not(test), allow(dead_code))]
fn apply_asset_template(
    asset_template: &str,
    asset_template_windows: Option<&str>,
    version: &str,
    triple: &str,
    is_windows: bool,
) -> String {
    let template = if is_windows {
        asset_template_windows.unwrap_or(asset_template)
    } else {
        asset_template
    };
    template
        .replace("{version}", version)
        .replace("{triple}", triple)
}

/// Expands `{triple}` and `{exe_suffix}` placeholders in a layout rel_path.
/// Extracted to allow platform-injected testing on non-Windows CI runners.
fn apply_layout_template(rel_path: &str, triple: &str, is_windows: bool) -> String {
    let exe_suffix = if is_windows { ".exe" } else { "" };
    rel_path
        .replace("{triple}", triple)
        .replace("{exe_suffix}", exe_suffix)
}

fn resolved_asset_name(fetch: &FetchKind, version: &str) -> Result<String> {
    match fetch {
        FetchKind::GithubRelease {
            asset_template,
            asset_template_windows,
            triple_style,
            ..
        } => {
            let triple = host_triple(*triple_style)?;
            Ok(apply_asset_template(
                asset_template,
                *asset_template_windows,
                version,
                &triple,
                cfg!(windows),
            ))
        }
        FetchKind::NpmRegistry { package } => Ok(format!("{package}-{version}.tgz")),
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

/// Lock-acquiring wrapper around [`install_runtime_tool_archive_locked`] for
/// callers that do not already hold the per-entry install lock.
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
    fs::create_dir_all(&tools_root).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool dir {}: {}",
            tools_root.display(),
            e
        ))
    })?;
    let _install_lock = acquire_tool_cache_lock(spec, version, &tools_root)?;
    install_runtime_tool_archive_locked(spec, version, deps, archive_bytes)
}

/// Installs a downloaded tool archive into the cache entry for `version`.
///
/// The archive is extracted and validated inside a `partial/` staging
/// directory first; only a fully-validated build is promoted (via rename)
/// into the canonical `extracted/` and `shim/` directories. `binary.sha256`
/// is written last as the completion marker, so an interrupted install can
/// never validate as complete on the next run. Callers must hold the
/// per-entry install lock.
fn install_runtime_tool_archive_locked(
    spec: &RuntimeToolSpec,
    version: &str,
    deps: &ToolDeps,
    archive_bytes: &[u8],
) -> Result<ToolHandle> {
    let tools_root = toolchain_cache_dir()?
        .join("tools")
        .join(spec.name)
        .join(version);
    stage_and_promote_tool_archive(spec, version, deps, archive_bytes, &tools_root).map_err(
        |err| {
            CapsuleError::Pack(format!(
                "{} {} runtime tool install (cache rebuild) failed: expected executable '{}' under {}: {}",
                spec.name,
                version,
                resolved_layout_path(spec).unwrap_or_default(),
                tools_root.display(),
                err
            ))
        },
    )
}

fn stage_and_promote_tool_archive(
    spec: &RuntimeToolSpec,
    version: &str,
    deps: &ToolDeps,
    archive_bytes: &[u8],
    tools_root: &Path,
) -> Result<ToolHandle> {
    let extracted_dir = tools_root.join("extracted");
    let shim_dir = tools_root.join("shim");
    let sha_path = tools_root.join("binary.sha256");
    let staging_root = tools_root.join("partial");
    let staging_extracted = staging_root.join("extracted");
    let staging_shim = staging_root.join("shim");
    let archive_sha256 = hex::encode(Sha256::digest(archive_bytes));

    fs::create_dir_all(tools_root).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create tool dir {}: {}",
            tools_root.display(),
            e
        ))
    })?;
    // A leftover staging dir is a previous interrupted install — never reuse it.
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root).map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to discard stale staging dir {}: {}",
                staging_root.display(),
                e
            ))
        })?;
    }
    for dir in [&staging_extracted, &staging_shim] {
        fs::create_dir_all(dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to create {}: {}", dir.display(), e))
        })?;
    }

    let archive_path = staging_root.join(archive_filename(&spec.fetch, version)?);
    fs::write(&archive_path, archive_bytes).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write archive {}: {}",
            archive_path.display(),
            e
        ))
    })?;
    extract_archive(&archive_path, &staging_extracted)?;

    let staged_target = resolve_tool_target(spec, &staging_extracted)?;

    // The shim must reference the canonical (post-promote) target path, not
    // the staging path it was validated at.
    let relative_target = staged_target
        .strip_prefix(&staging_extracted)
        .map_err(|_| {
            CapsuleError::Pack(format!(
                "{} staged tool entry {} escaped staging dir {}",
                spec.name,
                staged_target.display(),
                staging_extracted.display()
            ))
        })?
        .to_path_buf();
    let final_target = extracted_dir.join(&relative_target);
    let shim_path = staging_shim.join(shim_filename(spec));
    write_shim(spec, deps, &final_target, &shim_path)?;

    // Hash of the resolved executable, not the archive. The cache-side
    // `binary.sha256` file stores this resolved hash so its name matches its
    // contents (older builds wrote the archive hash here — the #469 bug).
    let binary_sha256 = sha256_file(&staged_target)?;

    // Promote. Removing the sha marker first means an interruption between
    // the renames leaves an entry that fails validation and gets rebuilt
    // instead of being half-reused.
    fs::remove_file(&sha_path).ok();
    for dir in [&extracted_dir, &shim_dir] {
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to replace stale cache dir {}: {}",
                    dir.display(),
                    e
                ))
            })?;
        }
    }
    fs::rename(&staging_extracted, &extracted_dir).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to promote {} -> {}: {}",
            staging_extracted.display(),
            extracted_dir.display(),
            e
        ))
    })?;
    fs::rename(&staging_shim, &shim_dir).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to promote {} -> {}: {}",
            staging_shim.display(),
            shim_dir.display(),
            e
        ))
    })?;
    fs::write(&sha_path, &binary_sha256).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write binary hash {}: {}",
            sha_path.display(),
            e
        ))
    })?;
    // Only the downloaded archive bytes remain in staging at this point.
    fs::remove_dir_all(&staging_root).ok();

    Ok(ToolHandle {
        bin_dir: shim_dir,
        version: version.to_string(),
        archive_sha256,
        binary_sha256,
    })
}

/// SHA-256 of a file's bytes, hex-encoded. Used for the resolved tool-entry
/// hash (#469).
fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to read {} for hashing: {}",
            path.display(),
            e
        ))
    })?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Validates all cache integrity conditions for a managed runtime tool.
/// Returns the **resolved binary** SHA-256 on success.
///
/// Checks:
/// 1. shim file is present and executable (Unix)
/// 2. extracted target is present and executable for NativeBinary (via resolve_tool_target)
/// 3. binary.sha256 is present, non-empty, and a valid 64-char hex string
///
/// The cache-side `binary.sha256` file is still required as an integrity marker
/// of a completed install, but its *value* is no longer trusted as the binary
/// identity: older caches stored the archive hash there under the same name, so
/// the resolved binary hash is recomputed from the actual target file and that
/// value is returned. See #469.
fn validate_tool_cache(
    spec: &RuntimeToolSpec,
    shim_path: &Path,
    extracted_dir: &Path,
    sha_path: &Path,
) -> Result<String> {
    validate_cached_shim(spec, shim_path)?;
    let target_path = resolve_tool_target(spec, extracted_dir)?;
    validate_cached_sha(sha_path)?;
    sha256_file(&target_path)
}

fn validate_cached_shim(spec: &RuntimeToolSpec, shim_path: &Path) -> Result<()> {
    if !shim_path.is_file() {
        return Err(CapsuleError::Pack(format!(
            "{} cached shim is missing: {}",
            spec.name,
            shim_path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(shim_path)
            .map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to stat shim {}: {}",
                    shim_path.display(),
                    e
                ))
            })?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(CapsuleError::Pack(format!(
                "{} cached shim is not executable: {}",
                spec.name,
                shim_path.display()
            )));
        }
    }
    Ok(())
}

fn validate_cached_sha(sha_path: &Path) -> Result<String> {
    let raw = fs::read_to_string(sha_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "binary.sha256 missing or unreadable {}: {}",
            sha_path.display(),
            e
        ))
    })?;
    let sha = raw.trim().to_string();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CapsuleError::Pack(format!(
            "binary.sha256 is not a valid SHA-256 hex string (got {:?})",
            if sha.len() > 20 {
                format!("{}…", &sha[..20])
            } else {
                sha.clone()
            }
        )));
    }
    Ok(sha)
}

fn resolve_tool_target(spec: &RuntimeToolSpec, extracted_dir: &Path) -> Result<PathBuf> {
    let layout_path = resolved_layout_path(spec)?;
    let target_path = extracted_dir.join(&layout_path);
    let mut searched = vec![target_path.clone()];

    if target_path.is_file() {
        validate_tool_target(spec, &target_path)?;
        return Ok(target_path);
    }

    if matches!(spec.layout, ToolLayout::NativeBinary { .. }) {
        let file_name = Path::new(&layout_path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                CapsuleError::Pack(format!("{} has invalid binary layout path", spec.name))
            })?;
        let platform_dir_candidate = find_single_top_level_binary(extracted_dir, file_name)?;
        if let Some(candidate) = platform_dir_candidate {
            searched.push(candidate.clone());
            validate_tool_target(spec, &candidate)?;
            copy_tool_target_to_canonical(&candidate, &target_path)?;
            validate_tool_target(spec, &target_path)?;
            return Ok(target_path);
        }
    }

    Err(CapsuleError::Pack(format!(
        "{} archive missing expected file {}; searched: {}",
        spec.name,
        target_path.display(),
        searched
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn find_single_top_level_binary(extracted_dir: &Path, file_name: &str) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(extracted_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CapsuleError::Pack(format!(
                "Failed to inspect tool extract dir {}: {}",
                extracted_dir.display(),
                e
            )));
        }
    };

    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to inspect tool extract dir {}: {}",
                extracted_dir.display(),
                e
            ))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let candidate = path.join(file_name);
        if candidate.is_file() {
            matches.push(candidate);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(CapsuleError::Pack(format!(
            "ambiguous native tool binary layout under {}: {}",
            extracted_dir.display(),
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn copy_tool_target_to_canonical(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CapsuleError::Pack(format!("Failed to create {}: {}", parent.display(), e))
        })?;
    }
    fs::copy(source, target).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to normalize tool binary {} -> {}: {}",
            source.display(),
            target.display(),
            e
        ))
    })?;
    #[cfg(unix)]
    {
        let perms = fs::metadata(source)
            .map_err(|e| CapsuleError::Pack(format!("Failed to stat {}: {}", source.display(), e)))?
            .permissions();
        fs::set_permissions(target, perms).map_err(|e| {
            CapsuleError::Pack(format!("Failed to chmod {}: {}", target.display(), e))
        })?;
    }
    Ok(())
}

fn validate_tool_target(spec: &RuntimeToolSpec, path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(CapsuleError::Pack(format!(
            "{} archive missing expected file {}",
            spec.name,
            path.display()
        )));
    }

    // A zero-length tool entry is never valid; on Windows executing one
    // surfaces as an opaque "Access is denied".
    let metadata = fs::metadata(path)
        .map_err(|e| CapsuleError::Pack(format!("Failed to stat {}: {}", path.display(), e)))?;
    if metadata.len() == 0 {
        return Err(CapsuleError::Pack(format!(
            "{} tool entry is empty (0 bytes): {}",
            spec.name,
            path.display()
        )));
    }

    if matches!(spec.layout, ToolLayout::NativeBinary { .. }) {
        validate_native_executable(spec.name, path)?;
    }

    Ok(())
}

#[cfg(unix)]
fn validate_native_executable(tool_name: &str, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .map_err(|e| CapsuleError::Pack(format!("Failed to stat {}: {}", path.display(), e)))?
        .permissions()
        .mode();
    if mode & 0o111 == 0 {
        return Err(CapsuleError::Pack(format!(
            "{} archive file is not executable: {}",
            tool_name,
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_native_executable(_tool_name: &str, _path: &Path) -> Result<()> {
    Ok(())
}

fn resolved_layout_path(spec: &RuntimeToolSpec) -> Result<String> {
    let target_rel = match &spec.layout {
        ToolLayout::NativeBinary { rel_path } | ToolLayout::NodeScript { rel_path } => rel_path,
    };
    match &spec.fetch {
        FetchKind::GithubRelease { triple_style, .. } => {
            let triple = host_triple(*triple_style)?;
            Ok(apply_layout_template(target_rel, &triple, cfg!(windows)))
        }
        _ => Ok(apply_layout_template(target_rel, "", cfg!(windows))),
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

    fn build_uv_tgz_at(rel_path: &str, mode: u32) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let payload = b"#!/bin/sh\necho uv\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder
            .append_data(&mut header, rel_path, Cursor::new(payload))
            .expect("append uv");
        let tar = builder.into_inner().expect("finish uv tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).expect("write uv tgz");
        gz.finish().expect("finish uv tgz")
    }

    fn build_uv_archive() -> Vec<u8> {
        #[cfg(windows)]
        {
            build_uv_archive_with_payload(b"uv binary")
        }
        #[cfg(not(windows))]
        {
            build_uv_archive_with_payload(b"#!/bin/sh\necho uv\n")
        }
    }

    fn build_uv_archive_with_payload(payload: &[u8]) -> Vec<u8> {
        #[cfg(windows)]
        {
            let mut cursor = Cursor::new(Vec::new());
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .unix_permissions(0o755);
            let path = resolved_layout_path(&UV).expect("uv layout");
            zip.start_file(path, options).expect("start uv file");
            zip.write_all(payload).expect("write uv");
            zip.finish().expect("finish uv zip");
            cursor.into_inner()
        }
        #[cfg(not(windows))]
        {
            let mut builder = tar::Builder::new(Vec::new());
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
    }

    /// Builds a nested-layout archive where the uv binary is inside a
    /// `uv-{triple}/` subdirectory (the layout astral-sh/uv uses on some
    /// releases).  On Windows the archive is a .zip containing `uv.exe`;
    /// on Unix it is a .tar.gz containing `uv`.
    fn build_uv_nested_archive(triple: &str) -> Vec<u8> {
        #[cfg(windows)]
        {
            let nested_path = format!("uv-{triple}/uv.exe");
            let mut cursor = std::io::Cursor::new(Vec::new());
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .unix_permissions(0o755);
            zip.start_file(&nested_path, options)
                .expect("start nested uv");
            zip.write_all(b"uv binary").expect("write nested uv");
            zip.finish().expect("finish nested zip");
            return cursor.into_inner();
        }
        #[cfg(not(windows))]
        {
            build_uv_tgz_at(&format!("uv-{triple}/uv"), 0o755)
        }
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
    fn deno_is_runtime_modeled_not_runtime_tool_spec() {
        // Deno is a primary runtime (modeled in RuntimeSection / `runtimes.deno`),
        // not a RuntimeToolSpec tool. Its absence from REGISTRY is intentional and
        // must not be read as "Deno unsupported". See #470 and
        // docs/dev-notes/runtime-vs-runtime-tools.md.
        assert!(
            lookup("deno").is_none(),
            "deno must not be a RuntimeToolSpec tool — it is runtime-modeled"
        );
        // The auxiliary runtime tools remain registry-modeled.
        assert!(lookup("uv").is_some());
        assert!(lookup("pnpm").is_some());
        assert!(lookup("yarn").is_some());
        assert!(lookup("bun").is_some());
        // The registry is exactly these four tools.
        assert_eq!(registry().len(), 4);
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
            if cfg!(windows) {
                format!(
                    "https://github.com/astral-sh/uv/releases/download/0.5.21/uv-{uv_triple}.zip"
                )
            } else {
                format!(
                    "https://github.com/astral-sh/uv/releases/download/0.5.21/uv-{uv_triple}.tar.gz"
                )
            }
        );
        assert_eq!(
            archive_filename(&UV.fetch, "0.5.21").unwrap(),
            if cfg!(windows) {
                format!("uv-{uv_triple}.zip")
            } else {
                format!("uv-{uv_triple}.tar.gz")
            }
        );
    }

    #[test]
    fn tool_registry_resolves_expected_binary_paths() {
        assert_eq!(resolved_layout_path(&PNPM).unwrap(), "package/bin/pnpm.cjs");
        assert_eq!(resolved_layout_path(&YARN).unwrap(), "package/bin/yarn.js");
        assert_eq!(
            resolved_layout_path(&BUN).unwrap(),
            format!(
                "bun-{}/bun{}",
                host_triple(TripleStyle::Bun).unwrap(),
                if cfg!(windows) { ".exe" } else { "" }
            )
        );
        assert_eq!(
            resolved_layout_path(&UV).unwrap(),
            if cfg!(windows) { "uv.exe" } else { "uv" }
        );
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
        let uv_handle = install_runtime_tool_archive(
            &UV,
            &uv_version,
            &ToolDeps::default(),
            &build_uv_archive(),
        )
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
                .join(resolved_layout_path(&UV).unwrap())
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
    #[serial_test::serial]
    fn install_uv_archive_normalizes_single_platform_directory() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-nested");
        let triple = host_triple(TripleStyle::Rust).expect("host triple");
        let handle = install_runtime_tool_archive(
            &UV,
            &version,
            &ToolDeps::default(),
            &build_uv_nested_archive(&triple),
        )
        .expect("install nested uv");

        let canonical_uv = ato_home
            .path()
            .join("toolchains/tools/uv")
            .join(&version)
            .join("extracted")
            .join(resolved_layout_path(&UV).expect("uv layout"));
        assert!(
            canonical_uv.is_file(),
            "expected uv binary at canonical path {canonical_uv:?}"
        );
        assert!(
            handle.bin_dir.join("uv").is_file() || handle.bin_dir.join("uv.cmd").is_file(),
            "shim must exist in bin_dir {:?}",
            handle.bin_dir
        );
    }

    // ── validate_tool_cache unit tests ───────────────────────────────────
    // These test the helper directly; ensure_runtime_tool calls the same helper
    // for the cache hit branch.

    struct FakeCacheDir {
        // Held for RAII only: dropping it removes the temp dir.
        _root: tempfile::TempDir,
        extracted_dir: PathBuf,
        shim_dir: PathBuf,
        sha_path: PathBuf,
        shim_path: PathBuf,
    }

    impl FakeCacheDir {
        fn new(tag: &str) -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let version = unique_version(tag);
            let tools_root = root.path().join("toolchains/tools/uv").join(version);
            let extracted_dir = tools_root.join("extracted");
            let shim_dir = tools_root.join("shim");
            let sha_path = tools_root.join("binary.sha256");
            let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
            let shim_path = shim_dir.join(shim_name);
            FakeCacheDir {
                _root: root,
                extracted_dir,
                shim_dir,
                sha_path,
                shim_path,
            }
        }

        fn write_shim(&self, mode: u32) {
            fs::create_dir_all(&self.shim_dir).expect("shim_dir");
            fs::write(&self.shim_path, b"#!/bin/sh\nexec uv \"$@\"\n").expect("shim");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.shim_path, fs::Permissions::from_mode(mode))
                    .expect("chmod shim");
            }
            let _ = mode;
        }

        fn write_target(&self, mode: u32) {
            let layout = resolved_layout_path(&UV).expect("uv layout");
            let target = self.extracted_dir.join(&layout);
            fs::create_dir_all(target.parent().unwrap()).expect("extracted_dir");
            fs::write(&target, b"#!/bin/sh\necho uv\n").expect("target");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(mode))
                    .expect("chmod target");
            }
            let _ = mode;
        }

        fn write_sha(&self, content: &str) {
            fs::create_dir_all(self.sha_path.parent().unwrap()).expect("sha parent");
            fs::write(&self.sha_path, content).expect("sha file");
        }

        fn validate(&self) -> Result<String> {
            validate_tool_cache(&UV, &self.shim_path, &self.extracted_dir, &self.sha_path)
        }
    }

    #[test]
    fn cache_validation_succeeds_on_complete_install() {
        let cache = FakeCacheDir::new("valid");
        cache.write_shim(0o755);
        cache.write_target(0o755);
        cache.write_sha(&"a".repeat(64));
        assert!(cache.validate().is_ok(), "complete cache should be valid");
    }

    #[test]
    fn cache_validation_rejects_missing_shim() {
        let cache = FakeCacheDir::new("no-shim");
        cache.write_target(0o755);
        cache.write_sha(&"a".repeat(64));
        // shim_path doesn't exist
        let err = cache.validate().expect_err("missing shim must fail");
        assert!(err.to_string().contains("shim"), "error: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn cache_validation_rejects_non_executable_shim() {
        let cache = FakeCacheDir::new("shim-noexec");
        cache.write_shim(0o644); // not executable
        cache.write_target(0o755);
        cache.write_sha(&"a".repeat(64));
        let err = cache.validate().expect_err("non-executable shim must fail");
        assert!(err.to_string().contains("executable"), "error: {err}");
    }

    #[test]
    fn cache_validation_rejects_missing_target() {
        let cache = FakeCacheDir::new("no-target");
        cache.write_shim(0o755);
        // extracted_dir exists but is empty (no target binary)
        fs::create_dir_all(&cache.extracted_dir).expect("extracted_dir");
        cache.write_sha(&"a".repeat(64));
        let err = cache.validate().expect_err("missing target must fail");
        assert!(
            err.to_string().contains("missing") || err.to_string().contains("searched"),
            "error: {err}"
        );
    }

    #[test]
    fn cache_validation_rejects_missing_sha() {
        let cache = FakeCacheDir::new("no-sha");
        cache.write_shim(0o755);
        cache.write_target(0o755);
        // sha_path not created
        let err = cache.validate().expect_err("missing sha must fail");
        assert!(
            err.to_string().contains("sha256") || err.to_string().contains("unreadable"),
            "error: {err}"
        );
    }

    #[test]
    fn cache_validation_rejects_empty_sha() {
        let cache = FakeCacheDir::new("empty-sha");
        cache.write_shim(0o755);
        cache.write_target(0o755);
        cache.write_sha("");
        let err = cache.validate().expect_err("empty sha must fail");
        assert!(err.to_string().contains("sha256"), "error: {err}");
    }

    #[test]
    fn cache_validation_rejects_invalid_sha() {
        let cache = FakeCacheDir::new("bad-sha");
        cache.write_shim(0o755);
        cache.write_target(0o755);
        cache.write_sha("not-a-hex-string");
        let err = cache.validate().expect_err("invalid sha must fail");
        assert!(err.to_string().contains("sha256"), "error: {err}");
    }

    #[test]
    #[serial_test::serial]
    fn corrupt_cache_is_discarded_by_reinstall() {
        // Verifies the reinstall path: install → corrupt sha → reinstall via archive.
        // (The re-download leg of ensure_runtime_tool requires HTTP; this test covers
        // the cache-discard + archive-install path exercised after discard.)
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-reinstall");

        // First install: valid
        let handle1 =
            install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &build_uv_archive())
                .expect("first install");

        let tools_root = ato_home.path().join("toolchains/tools/uv").join(&version);
        let sha_path = tools_root.join("binary.sha256");
        let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
        let shim_path = handle1.bin_dir.join(shim_name);

        // Corrupt: truncate sha file
        fs::write(&sha_path, b"bad").expect("corrupt sha");

        // validate_tool_cache must now fail
        assert!(
            validate_tool_cache(&UV, &shim_path, &tools_root.join("extracted"), &sha_path).is_err(),
            "corrupted cache must fail validation"
        );

        // Discard stale state (mirrors ensure_runtime_tool's Err branch)
        fs::remove_dir_all(tools_root.join("extracted")).ok();
        fs::remove_dir_all(tools_root.join("shim")).ok();
        fs::remove_file(&sha_path).ok();
        assert!(!shim_path.exists(), "shim must be gone after discard");

        // Reinstall via archive (the download-then-install leg)
        let handle2 =
            install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &build_uv_archive())
                .expect("reinstall after discard");
        assert_eq!(handle2.version, handle1.version);
        assert!(
            handle2.bin_dir.join(shim_name).is_file(),
            "shim must be restored after reinstall"
        );
    }

    #[test]
    #[serial_test::serial]
    fn stale_partial_staging_dir_is_discarded_not_reused() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-partial");
        let tools_root = ato_home.path().join("toolchains/tools/uv").join(&version);
        let staging_root = tools_root.join("partial");

        // Simulate a previous interrupted install: junk in the staging dir.
        fs::create_dir_all(staging_root.join("extracted")).expect("stale staging");
        fs::write(
            staging_root.join("extracted/garbage"),
            b"truncated download",
        )
        .expect("junk");

        let handle =
            install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &build_uv_archive())
                .expect("install over stale staging");

        assert!(
            !staging_root.exists(),
            "staging dir must be removed after a completed install"
        );
        assert!(
            !tools_root.join("extracted/garbage").exists(),
            "stale staging contents must never be promoted"
        );
        let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
        let binary_sha256 = validate_tool_cache(
            &UV,
            &handle.bin_dir.join(shim_name),
            &tools_root.join("extracted"),
            &tools_root.join("binary.sha256"),
        )
        .expect("promoted cache entry must validate");
        assert_eq!(binary_sha256, handle.binary_sha256);
    }

    #[test]
    #[serial_test::serial]
    fn install_replaces_corrupt_canonical_entry() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-corrupt");
        let tools_root = ato_home.path().join("toolchains/tools/uv").join(&version);

        // A canonical entry whose shim exists but whose extracted target is
        // missing (the shape an interrupted legacy install left behind).
        let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
        fs::create_dir_all(tools_root.join("shim")).expect("shim dir");
        fs::write(tools_root.join("shim").join(shim_name), b"stale shim").expect("stale shim");
        fs::create_dir_all(tools_root.join("extracted")).expect("extracted dir");

        let handle =
            install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &build_uv_archive())
                .expect("install over corrupt entry");

        validate_tool_cache(
            &UV,
            &handle.bin_dir.join(shim_name),
            &tools_root.join("extracted"),
            &tools_root.join("binary.sha256"),
        )
        .expect("rebuilt cache entry must validate");
        let shim_body = fs::read(handle.bin_dir.join(shim_name)).expect("shim body");
        assert_ne!(shim_body, b"stale shim", "stale shim must be replaced");
    }

    #[test]
    #[serial_test::serial]
    fn empty_tool_entry_in_archive_fails_install_with_context() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-empty");
        let err = install_runtime_tool_archive(
            &UV,
            &version,
            &ToolDeps::default(),
            &build_uv_archive_with_payload(b""),
        )
        .expect_err("zero-byte tool entry must fail validation");
        let message = err.to_string();
        assert!(message.contains("uv"), "tool name missing: {message}");
        assert!(
            message.contains("empty (0 bytes)"),
            "empty-entry cause missing: {message}"
        );
        assert!(
            message.contains("toolchains"),
            "cache path missing: {message}"
        );

        // The failed install must not leave a canonical entry that validates.
        let tools_root = ato_home.path().join("toolchains/tools/uv").join(&version);
        let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
        assert!(
            validate_tool_cache(
                &UV,
                &tools_root.join("shim").join(shim_name),
                &tools_root.join("extracted"),
                &tools_root.join("binary.sha256"),
            )
            .is_err(),
            "failed install must not produce a valid cache entry"
        );
    }

    #[test]
    #[serial_test::serial]
    fn concurrent_installs_of_same_entry_serialize_on_the_cache_lock() {
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-race");
        let archive = build_uv_archive();
        let results = std::thread::scope(|scope| {
            let handles = [
                scope.spawn(|| {
                    install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &archive)
                }),
                scope.spawn(|| {
                    install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &archive)
                }),
            ];
            handles.map(|handle| handle.join().expect("install thread"))
        });
        for result in results {
            result.expect("concurrent install must succeed");
        }

        let tools_root = ato_home.path().join("toolchains/tools/uv").join(&version);
        let shim_name = if cfg!(windows) { "uv.cmd" } else { "uv" };
        validate_tool_cache(
            &UV,
            &tools_root.join("shim").join(shim_name),
            &tools_root.join("extracted"),
            &tools_root.join("binary.sha256"),
        )
        .expect("cache entry must be valid after concurrent installs");
        assert!(
            !tools_root.join("partial").exists(),
            "no staging dir may survive concurrent installs"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cache_validation_on_windows_does_not_require_unix_exec_bits() {
        // Plain fs::write produces no Unix permission bits on Windows; the
        // validator must accept the entry based on presence + content alone.
        let cache = FakeCacheDir::new("win-noexec");
        cache.write_shim(0o644);
        cache.write_target(0o644);
        cache.write_sha(&"a".repeat(64));
        assert!(cache.validate().is_ok(), "complete cache should be valid");
    }

    #[test]
    fn bun_windows_layout_resolves_to_exe() {
        let ToolLayout::NativeBinary { rel_path } = &BUN.layout else {
            panic!("BUN must use NativeBinary layout");
        };
        assert_eq!(
            apply_layout_template(rel_path, "windows-x64", true),
            "bun-windows-x64/bun.exe"
        );
        assert_eq!(
            apply_layout_template(rel_path, "linux-x64", false),
            "bun-linux-x64/bun"
        );
    }

    #[test]
    #[serial_test::serial]
    fn managed_install_records_resolved_binary_hash_not_archive_hash() {
        // #469 guard: ToolHandle.binary_sha256 must be the resolved tool-entry
        // file hash, and archive_sha256 the archive bytes hash — never conflated.
        let ato_home = tempfile::tempdir().expect("ato_home");
        let _home = scoped_env("ATO_HOME", Some(ato_home.path().to_string_lossy().as_ref()));

        let version = unique_version("uv-hash");
        let archive = build_uv_archive();
        let handle = install_runtime_tool_archive(&UV, &version, &ToolDeps::default(), &archive)
            .expect("install uv");

        // archive_sha256 == hash of the downloaded archive bytes.
        let expected_archive = hex::encode(Sha256::digest(&archive));
        assert_eq!(
            handle.archive_sha256, expected_archive,
            "archive_sha256 must be the archive bytes hash"
        );

        // binary_sha256 == hash of the resolved tool-entry file, not the archive.
        let target = ato_home
            .path()
            .join("toolchains/tools/uv")
            .join(&version)
            .join("extracted")
            .join(resolved_layout_path(&UV).unwrap());
        let expected_binary = hex::encode(Sha256::digest(fs::read(&target).expect("read target")));
        assert_eq!(
            handle.binary_sha256, expected_binary,
            "binary_sha256 must be the resolved target file hash"
        );

        // The #469 bug was these two being the same value.
        assert_ne!(
            handle.archive_sha256, handle.binary_sha256,
            "archive and resolved-binary hashes must not be conflated"
        );

        // The cache-side `binary.sha256` file stores the resolved binary hash.
        let sha_path = ato_home
            .path()
            .join("toolchains/tools/uv")
            .join(&version)
            .join("binary.sha256");
        let cached = fs::read_to_string(&sha_path).expect("read cache sha");
        assert_eq!(
            cached.trim(),
            handle.binary_sha256,
            "cache file must store the resolved binary hash, not the archive hash"
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

    // ── Platform-injected asset resolution tests ──────────────────────────
    // These tests exercise apply_asset_template / apply_layout_template with
    // explicit triples so that Linux/macOS CI runners can verify Windows
    // asset paths without cfg!(windows) being true.

    #[test]
    fn uv_windows_asset_name_resolves_to_zip() {
        let FetchKind::GithubRelease {
            asset_template,
            asset_template_windows,
            ..
        } = &UV.fetch
        else {
            panic!("UV must use GithubRelease");
        };
        let name = apply_asset_template(
            asset_template,
            *asset_template_windows,
            "0.5.21",
            "x86_64-pc-windows-msvc",
            true,
        );
        assert_eq!(name, "uv-x86_64-pc-windows-msvc.zip");
    }

    #[test]
    fn uv_linux_asset_name_resolves_to_targz() {
        let FetchKind::GithubRelease {
            asset_template,
            asset_template_windows,
            ..
        } = &UV.fetch
        else {
            panic!("UV must use GithubRelease");
        };
        let name = apply_asset_template(
            asset_template,
            *asset_template_windows,
            "0.5.21",
            "x86_64-unknown-linux-gnu",
            false,
        );
        assert_eq!(name, "uv-x86_64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn uv_macos_asset_name_resolves_to_targz() {
        let FetchKind::GithubRelease {
            asset_template,
            asset_template_windows,
            ..
        } = &UV.fetch
        else {
            panic!("UV must use GithubRelease");
        };
        let name = apply_asset_template(
            asset_template,
            *asset_template_windows,
            "0.5.21",
            "aarch64-apple-darwin",
            false,
        );
        assert_eq!(name, "uv-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn uv_windows_layout_resolves_to_exe() {
        let ToolLayout::NativeBinary { rel_path } = &UV.layout else {
            panic!("UV must use NativeBinary layout");
        };
        let path = apply_layout_template(rel_path, "x86_64-pc-windows-msvc", true);
        assert_eq!(path, "uv.exe");
    }

    #[test]
    fn uv_unix_layout_resolves_to_plain_binary() {
        let ToolLayout::NativeBinary { rel_path } = &UV.layout else {
            panic!("UV must use NativeBinary layout");
        };
        let path = apply_layout_template(rel_path, "x86_64-unknown-linux-gnu", false);
        assert_eq!(path, "uv");
    }

    #[test]
    fn uv_windows_build_url_uses_zip() {
        let FetchKind::GithubRelease {
            repo, tag_template, ..
        } = &UV.fetch
        else {
            panic!("UV must use GithubRelease");
        };
        let version = "0.5.21";
        let tag = tag_template.replace("{version}", version);
        let asset = apply_asset_template(
            "uv-{triple}.tar.gz",
            Some("uv-{triple}.zip"),
            version,
            "x86_64-pc-windows-msvc",
            true,
        );
        let url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
        assert!(
            url.ends_with(".zip"),
            "Windows uv URL must use .zip, got: {url}"
        );
        assert!(
            url.contains("x86_64-pc-windows-msvc"),
            "Windows triple missing: {url}"
        );
        assert_eq!(
            url,
            "https://github.com/astral-sh/uv/releases/download/0.5.21/uv-x86_64-pc-windows-msvc.zip"
        );
    }
}
