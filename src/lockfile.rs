use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use url::Url;

use crate::common::paths::toolchain_cache_dir;
use crate::launcher::source::RuntimeFetcher;

#[derive(Debug, Deserialize)]
pub struct CapsuleLock {
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub tools: Option<ToolSection>,
    #[serde(default)]
    pub runtimes: Option<RuntimeSection>,
    #[serde(default)]
    pub targets: HashMap<String, TargetEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ToolSection {
    #[serde(default)]
    pub uv: Option<ToolTargets>,
    #[serde(default)]
    pub pnpm: Option<ToolTargets>,
    #[serde(default)]
    pub yarn: Option<ToolTargets>,
    #[serde(default)]
    pub bun: Option<ToolTargets>,
}

#[derive(Debug, Deserialize)]
pub struct ToolTargets {
    pub targets: HashMap<String, UrlEntry>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeSection {
    #[serde(default)]
    pub python: Option<RuntimeEntry>,
    #[serde(default)]
    pub node: Option<RuntimeEntry>,
    #[serde(default)]
    pub java: Option<RuntimeEntry>,
    #[serde(default)]
    pub dotnet: Option<RuntimeEntry>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeEntry {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub targets: HashMap<String, UrlEntry>,
}

#[derive(Debug, Deserialize)]
pub struct TargetEntry {
    #[serde(default)]
    pub python_lockfile: Option<String>,
    #[serde(default)]
    pub node_lockfile: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<UrlEntry>,
    #[serde(default)]
    pub compiled: Option<CompiledEntry>,
}

#[derive(Debug, Deserialize)]
pub struct CompiledEntry {
    pub artifacts: UrlEntry,
}

#[derive(Debug, Deserialize)]
pub struct UrlEntry {
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default, rename = "type")]
    pub artifact_type: Option<String>,
}

struct StagedArtifacts {
    python_cache_dir: Option<PathBuf>,
    pnpm_store_dir: Option<PathBuf>,
    yarn_cache_dir: Option<PathBuf>,
    bun_cache_dir: Option<PathBuf>,
}

pub async fn hydrate_bundle(bundle_root: &Path) -> Result<(), String> {
    let lock = match load_lockfile(bundle_root)? {
        Some(lock) => lock,
        None => return Ok(()),
    };
    let target_key = platform_target_key()?;
    let target_triple = platform_triple()?;
    let target = match lock.targets.get(&target_key) {
        Some(target) => target,
        None => return Ok(()),
    };
    let staged = stage_bundle_artifacts(bundle_root, &target_key, target)?;

    let source_dir = bundle_root.join("source");
    if target.python_lockfile.is_some() {
        hydrate_python(
            &lock,
            target,
            &source_dir,
            bundle_root,
            &target_triple,
            staged.python_cache_dir.as_deref(),
        )
        .await?;
    }
    if target.node_lockfile.is_some() {
        hydrate_node(
            &lock,
            target,
            &source_dir,
            bundle_root,
            &target_triple,
            staged.pnpm_store_dir.as_deref(),
            staged.yarn_cache_dir.as_deref(),
            staged.bun_cache_dir.as_deref(),
        )
        .await?;
    }

    Ok(())
}

pub fn enforce_lockfile_allowlist(bundle_root: &Path) -> Result<(), String> {
    let path = bundle_root.join("capsule.lock");
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read capsule.lock: {}", e))?;
    let lock: CapsuleLock =
        toml::from_str(&content).map_err(|e| format!("Failed to parse capsule.lock: {}", e))?;
    let Some(allowlist) = lock.allowlist.as_ref().filter(|l| !l.is_empty()) else {
        return Ok(());
    };

    for url in lock.collect_urls() {
        let parsed = Url::parse(&url)
            .map_err(|e| format!("Invalid URL in capsule.lock ({}): {}", url, e))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("URL missing host in capsule.lock: {}", url))?;
        if !allowlist.iter().any(|allowed| allowed == host) {
            return Err(format!("URL not in allowlist: {}", url));
        }
    }

    Ok(())
}

fn load_lockfile(bundle_root: &Path) -> Result<Option<CapsuleLock>, String> {
    let path = bundle_root.join("capsule.lock");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read capsule.lock: {}", e))?;
    let lock: CapsuleLock =
        toml::from_str(&content).map_err(|e| format!("Failed to parse capsule.lock: {}", e))?;
    Ok(Some(lock))
}

fn stage_bundle_artifacts(
    bundle_root: &Path,
    target_key: &str,
    target: &TargetEntry,
) -> Result<StagedArtifacts, String> {
    let cache_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir: {}", e))?;

    if target.artifacts.is_empty() {
        return Ok(StagedArtifacts {
            python_cache_dir: None,
            pnpm_store_dir: None,
            yarn_cache_dir: None,
            bun_cache_dir: None,
        });
    }

    let artifacts_root = bundle_root.join("artifacts").join(target_key);
    if !artifacts_root.exists() {
        return Err(format!(
            "capsule.lock lists artifacts but {} is missing",
            artifacts_root.display()
        ));
    }

    let mut python_cache_dir = None;
    let mut pnpm_store_dir = None;
    let mut yarn_cache_dir = None;
    let mut bun_cache_dir = None;

    for artifact in &target.artifacts {
        let rel_path = artifact_relative_path(artifact)?;
        let source_path = artifacts_root.join(&rel_path);
        if !source_path.exists() {
            return Err(format!(
                "Artifact not found in bundle: {}",
                source_path.display()
            ));
        }
        let dest_path = cache_dir.join(&rel_path);
        copy_artifact(&source_path, &dest_path)?;

        let artifact_type = artifact.artifact_type.as_deref();
        let filename_hint = artifact.filename.as_deref();
        let is_uv_cache = artifact_type == Some("uv-cache")
            || filename_hint == Some("uv-cache")
            || rel_path.as_os_str() == "uv-cache";
        let is_pnpm_store = artifact_type == Some("pnpm-store")
            || filename_hint == Some("pnpm-store")
            || rel_path.as_os_str() == "pnpm-store";
        let is_yarn_cache = artifact_type == Some("yarn-cache")
            || filename_hint == Some("yarn-cache")
            || rel_path.as_os_str() == "yarn-cache";
        let is_bun_cache = artifact_type == Some("bun-cache")
            || filename_hint == Some("bun-cache")
            || rel_path.as_os_str() == "bun-cache";

        if python_cache_dir.is_none() && is_uv_cache {
            python_cache_dir = Some(cache_dir.join(&rel_path));
        }
        if pnpm_store_dir.is_none() && is_pnpm_store {
            pnpm_store_dir = Some(cache_dir.join(&rel_path));
        }
        if yarn_cache_dir.is_none() && is_yarn_cache {
            yarn_cache_dir = Some(cache_dir.join(&rel_path));
        }
        if bun_cache_dir.is_none() && is_bun_cache {
            bun_cache_dir = Some(cache_dir.join(&rel_path));
        }
    }

    Ok(StagedArtifacts {
        python_cache_dir,
        pnpm_store_dir,
        yarn_cache_dir,
        bun_cache_dir,
    })
}

async fn hydrate_python(
    lock: &CapsuleLock,
    target: &TargetEntry,
    source_dir: &Path,
    bundle_root: &Path,
    target_triple: &str,
    python_cache_dir: Option<&Path>,
) -> Result<(), String> {
    let uv = ensure_uv(lock, target_triple).await?;
    let python_path = ensure_python(lock).await?;
    let cache_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir: {}", e))?;
    let uv_cache_dir = python_cache_dir.unwrap_or(cache_dir.as_path());
    if let Some(parent) = uv_cache_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create uv cache dir: {}", e))?;
    }

    if let Some(lockfile) = target.python_lockfile.as_ref() {
        let lock_src = bundle_root.join(lockfile);
        let lock_dest = source_dir.join("uv.lock");
        if lock_src.exists() {
            std::fs::copy(&lock_src, &lock_dest)
                .map_err(|e| format!("Failed to copy uv.lock: {}", e))?;
        }
    }

    let pyproject_path = source_dir.join("pyproject.toml");
    let lock_path = source_dir.join("uv.lock");
    if pyproject_path.exists() {
        let mut cmd = Command::new(&uv);
        cmd.args(["sync", "--frozen", "--no-install-project"])
            .current_dir(source_dir);
        cmd.env("UV_CACHE_DIR", uv_cache_dir);
        if let Some(python_path) = python_path.as_ref() {
            cmd.env("UV_PYTHON", python_path);
        }
        if python_cache_dir.is_some() {
            cmd.arg("--offline");
        }
        cmd.env("UV_PROJECT_ENVIRONMENT", ".venv");
        run_command(cmd, "uv sync")?;
    } else {
        if !lock_path.exists() {
            return Err("uv.lock missing for Python hydration".to_string());
        }
        let venv_path = source_dir.join(".venv");
        let mut venv_cmd = Command::new(&uv);
        if python_cache_dir.is_some() {
            venv_cmd.arg("--offline");
        }
        venv_cmd
            .arg("venv")
            .arg("--allow-existing")
            .current_dir(source_dir);
        if let Some(python_path) = python_path.as_ref() {
            venv_cmd.arg("--python").arg(python_path);
        }
        venv_cmd.arg(&venv_path);
        venv_cmd.env("UV_CACHE_DIR", uv_cache_dir);
        run_command(venv_cmd, "uv venv")?;

        let venv_python = if cfg!(target_os = "windows") {
            venv_path.join("Scripts").join("python.exe")
        } else {
            venv_path.join("bin").join("python")
        };
        let mut cmd = Command::new(&uv);
        if python_cache_dir.is_some() {
            cmd.arg("--offline");
        }
        cmd.args([
            "pip",
            "sync",
            lock_path.to_string_lossy().as_ref(),
            "--python",
            venv_python.to_string_lossy().as_ref(),
        ])
        .current_dir(source_dir);
        cmd.env("UV_CACHE_DIR", uv_cache_dir);
        run_command(cmd, "uv pip sync")?;
    }
    Ok(())
}

async fn hydrate_node(
    lock: &CapsuleLock,
    target: &TargetEntry,
    source_dir: &Path,
    bundle_root: &Path,
    target_triple: &str,
    pnpm_store_dir: Option<&Path>,
    yarn_cache_dir: Option<&Path>,
    bun_cache_dir: Option<&Path>,
) -> Result<(), String> {
    let lockfile_path = match target.node_lockfile.as_deref() {
        Some(p) => p,
        None => return Ok(()),
    };
    let lockfile_name = std::path::Path::new(lockfile_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pnpm-lock.yaml");

    let lock_src = bundle_root.join(lockfile_path);
    let lock_dest = source_dir.join(lockfile_name);
    if lock_src.exists() {
        std::fs::copy(&lock_src, &lock_dest)
            .map_err(|e| format!("Failed to copy {}: {}", lockfile_name, e))?;
    }

    let cache_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir: {}", e))?;

    if lockfile_name == "yarn.lock" {
        let node_path = ensure_node(lock).await?;
        let yarn_cmd = ensure_yarn_classic(lock, &node_path, target_triple).await?;
        let mut install_cmd = Command::new(&yarn_cmd.program);
        install_cmd
            .args(&yarn_cmd.args_prefix)
            .args(["install", "--frozen-lockfile"])
            .current_dir(source_dir);
        if let Some(cache) = yarn_cache_dir {
            install_cmd.args(["--cache-folder", cache.to_string_lossy().as_ref()]);
        }
        inject_node_path(&mut install_cmd, &node_path);
        run_command(install_cmd, "yarn install")?;
    } else if lockfile_name == "bun.lock" || lockfile_name == "bun.lockb" {
        let bun_path = ensure_bun(lock, target_triple).await?;
        let mut install_cmd = Command::new(&bun_path);
        install_cmd
            .args(["install", "--frozen-lockfile"])
            .current_dir(source_dir);
        if let Some(cache) = bun_cache_dir {
            install_cmd.env("BUN_INSTALL_CACHE_DIR", cache);
        }
        run_command(install_cmd, "bun install")?;
    } else {
        // pnpm (default)
        let node_path = ensure_node(lock).await?;
        let pnpm_cmd = ensure_pnpm(lock, &node_path, target_triple).await?;
        let store_dir = pnpm_store_dir
            .map(|dir| dir.to_path_buf())
            .unwrap_or_else(|| cache_dir.join("pnpm-store"));
        std::fs::create_dir_all(&store_dir)
            .map_err(|e| format!("Failed to create pnpm store dir: {}", e))?;
        let mut install_cmd = Command::new(&pnpm_cmd.program);
        install_cmd
            .args(&pnpm_cmd.args_prefix)
            .args([
                "install",
                "--ignore-scripts",
                "--frozen-lockfile",
                "--force",
            ])
            .current_dir(source_dir);
        inject_node_path(&mut install_cmd, &node_path);
        if pnpm_store_dir.is_none() {
            let mut fetch_cmd = Command::new(&pnpm_cmd.program);
            fetch_cmd
                .args(&pnpm_cmd.args_prefix)
                .args(["fetch", "--store-dir", store_dir.to_string_lossy().as_ref()])
                .current_dir(source_dir);
            inject_node_path(&mut fetch_cmd, &node_path);
            run_command(fetch_cmd, "pnpm fetch")?;
        }
        install_cmd.args(["--store-dir", store_dir.to_string_lossy().as_ref()]);
        if pnpm_store_dir.is_some() {
            install_cmd.arg("--offline");
        }
        run_command(install_cmd, "pnpm install")?;
    }
    Ok(())
}

struct PnpmCommand {
    program: PathBuf,
    args_prefix: Vec<String>,
}

async fn ensure_uv(lock: &CapsuleLock, target_triple: &str) -> Result<PathBuf, String> {
    if let Ok(found) = which::which("uv") {
        return Ok(found);
    }
    let uv = lock
        .tools
        .as_ref()
        .and_then(|t| t.uv.as_ref())
        .and_then(|u| u.targets.get(target_triple))
        .ok_or_else(|| "uv tool entry missing from capsule.lock".to_string())?;
    let tools_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("tools")
        .join("uv");
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("Failed to create uv tools dir: {}", e))?;
    let archive_path = tools_dir.join("uv.tar.gz");
    download_file(&uv.url, &archive_path).await?;
    extract_tgz(&archive_path, &tools_dir)?;
    find_binary_recursive(&tools_dir, &["uv", "uv.exe"])
        .ok_or_else(|| "uv binary not found after extraction".to_string())
}

async fn ensure_python(lock: &CapsuleLock) -> Result<Option<PathBuf>, String> {
    let Some(version) = lock
        .runtimes
        .as_ref()
        .and_then(|r| r.python.as_ref())
        .and_then(|p| p.version.clone())
    else {
        return Ok(None);
    };
    let fetcher =
        RuntimeFetcher::new().map_err(|e| format!("Failed to init runtime fetcher: {}", e))?;
    let python = fetcher
        .ensure_python(&version)
        .await
        .map_err(|e| format!("Failed to download python runtime: {}", e))?;
    Ok(Some(python))
}

async fn ensure_node(lock: &CapsuleLock) -> Result<PathBuf, String> {
    if let Ok(found) = which::which("node") {
        return Ok(found);
    }
    let version = lock
        .runtimes
        .as_ref()
        .and_then(|r| r.node.as_ref())
        .and_then(|n| n.version.clone())
        .unwrap_or_else(|| "20".to_string());
    let fetcher =
        RuntimeFetcher::new().map_err(|e| format!("Failed to init runtime fetcher: {}", e))?;
    fetcher
        .ensure_node(&version)
        .await
        .map_err(|e| format!("Failed to download node runtime: {}", e))
}

async fn ensure_pnpm(
    lock: &CapsuleLock,
    node_path: &Path,
    target_triple: &str,
) -> Result<PnpmCommand, String> {
    if let Ok(found) = which::which("pnpm") {
        return Ok(PnpmCommand {
            program: found,
            args_prefix: Vec::new(),
        });
    }
    let pnpm = lock
        .tools
        .as_ref()
        .and_then(|t| t.pnpm.as_ref())
        .and_then(|p| p.targets.get(target_triple))
        .ok_or_else(|| "pnpm tool entry missing from capsule.lock".to_string())?;
    let tools_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("tools")
        .join("pnpm");
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("Failed to create pnpm tools dir: {}", e))?;
    let archive_path = tools_dir.join("pnpm.tgz");
    download_file(&pnpm.url, &archive_path).await?;
    extract_tgz(&archive_path, &tools_dir)?;
    let script = tools_dir.join("package").join("bin").join("pnpm.cjs");
    if !script.exists() {
        return Err("pnpm.cjs not found after extraction".to_string());
    }
    Ok(PnpmCommand {
        program: node_path.to_path_buf(),
        args_prefix: vec![script.to_string_lossy().to_string()],
    })
}

async fn ensure_yarn_classic(
    lock: &CapsuleLock,
    node_path: &Path,
    target_triple: &str,
) -> Result<PnpmCommand, String> {
    if let Ok(found) = which::which("yarn") {
        return Ok(PnpmCommand {
            program: found,
            args_prefix: Vec::new(),
        });
    }
    let yarn = lock
        .tools
        .as_ref()
        .and_then(|t| t.yarn.as_ref())
        .and_then(|y| y.targets.get(target_triple))
        .ok_or_else(|| "yarn tool entry missing from capsule.lock".to_string())?;
    let tools_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("tools")
        .join("yarn-classic");
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("Failed to create yarn tools dir: {}", e))?;
    let archive_path = tools_dir.join("yarn.tgz");
    download_file(&yarn.url, &archive_path).await?;
    extract_tgz(&archive_path, &tools_dir)?;
    let script = tools_dir.join("package").join("bin").join("yarn.js");
    if !script.exists() {
        return Err("yarn.js not found after extraction".to_string());
    }
    Ok(PnpmCommand {
        program: node_path.to_path_buf(),
        args_prefix: vec![script.to_string_lossy().to_string()],
    })
}

async fn ensure_bun(lock: &CapsuleLock, target_triple: &str) -> Result<PathBuf, String> {
    if let Ok(found) = which::which("bun") {
        return Ok(found);
    }
    let bun = lock
        .tools
        .as_ref()
        .and_then(|t| t.bun.as_ref())
        .and_then(|b| b.targets.get(target_triple))
        .ok_or_else(|| "bun tool entry missing from capsule.lock".to_string())?;
    let tools_dir = toolchain_cache_dir()
        .map_err(|e| format!("Failed to resolve toolchain cache: {}", e))?
        .join("tools")
        .join("bun");
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("Failed to create bun tools dir: {}", e))?;
    let archive_path = tools_dir.join("bun.zip");
    download_file(&bun.url, &archive_path).await?;
    extract_zip(&archive_path, &tools_dir)?;
    let bun_bin = find_binary_recursive(&tools_dir, &["bun", "bun.exe"])
        .ok_or_else(|| "bun binary not found after extraction".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bun_bin)
            .map_err(|e| format!("Failed to stat bun binary: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bun_bin, perms)
            .map_err(|e| format!("Failed to chmod bun binary: {}", e))?;
    }
    Ok(bun_bin)
}

fn inject_node_path(cmd: &mut Command, node_path: &Path) {
    if let Some(parent) = node_path.parent() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = parent.as_os_str().to_os_string();
        new_path.push(std::path::MAIN_SEPARATOR_STR);
        new_path.push(current);
        cmd.env("PATH", new_path);
    }
}

async fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to download {}: {}", url, e))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download {} (status {})",
            url,
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read {}: {}", url, e))?;
    std::fs::write(dest, &bytes)
        .map_err(|e| format!("Failed to write {}: {}", dest.display(), e))?;
    Ok(())
}

fn extract_tgz(archive_path: &Path, dest: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("Failed to open archive {}: {}", archive_path.display(), e))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(dest)
        .map_err(|e| format!("Failed to extract archive: {}", e))?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("Failed to open zip {}: {}", archive.display(), e))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read zip {}: {}", archive.display(), e))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry {}: {}", i, e))?;
        let out_path = dest.join(entry.name());
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed to create dir {}: {}", out_path.display(), e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create dir {}: {}", parent.display(), e))?;
            }
            let mut out_file = std::fs::File::create(&out_path)
                .map_err(|e| format!("Failed to create {}: {}", out_path.display(), e))?;
            std::io::copy(&mut entry, &mut out_file)
                .map_err(|e| format!("Failed to extract {}: {}", out_path.display(), e))?;
        }
    }
    Ok(())
}

fn find_binary_recursive(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if candidates.contains(&name) {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn run_command(mut cmd: Command, operation: &str) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("{} failed: {}", operation, e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{} failed: {}", operation, stderr.trim()));
    }
    Ok(())
}

fn artifact_relative_path(entry: &UrlEntry) -> Result<PathBuf, String> {
    if let Some(filename) = entry.filename.as_ref().filter(|name| !name.is_empty()) {
        return Ok(PathBuf::from(filename));
    }
    let parsed =
        Url::parse(&entry.url).map_err(|e| format!("Invalid artifact URL {}: {}", entry.url, e))?;
    let name = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .ok_or_else(|| format!("Artifact URL missing filename: {}", entry.url))?;
    Ok(PathBuf::from(name))
}

fn copy_artifact(source: &Path, destination: &Path) -> Result<(), String> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
        std::fs::copy(source, destination)
            .map_err(|e| format!("Failed to copy {}: {}", source.display(), e))?;
        Ok(())
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("Failed to create {}: {}", destination.display(), e))?;
    for entry in std::fs::read_dir(source)
        .map_err(|e| format!("Failed to read {}: {}", source.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        let dest_path = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)
                .map_err(|e| format!("Failed to copy {}: {}", path.display(), e))?;
        }
    }
    Ok(())
}

fn platform_target_key() -> Result<String, String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err("Unsupported OS".to_string());
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err("Unsupported architecture".to_string());
    };
    Ok(format!("{}-{}", os, arch))
}

fn platform_triple() -> Result<String, String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err("Unsupported OS".to_string());
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err("Unsupported architecture".to_string());
    };

    let triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return Err("Unsupported platform".to_string()),
    };
    Ok(triple.to_string())
}

impl CapsuleLock {
    fn collect_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(tools) = &self.tools {
            if let Some(uv) = &tools.uv {
                urls.extend(uv.targets.values().map(|u| u.url.clone()));
            }
            if let Some(pnpm) = &tools.pnpm {
                urls.extend(pnpm.targets.values().map(|u| u.url.clone()));
            }
            if let Some(yarn) = &tools.yarn {
                urls.extend(yarn.targets.values().map(|u| u.url.clone()));
            }
            if let Some(bun) = &tools.bun {
                urls.extend(bun.targets.values().map(|u| u.url.clone()));
            }
        }
        if let Some(runtimes) = &self.runtimes {
            if let Some(python) = &runtimes.python {
                urls.extend(python.targets.values().map(|u| u.url.clone()));
            }
            if let Some(node) = &runtimes.node {
                urls.extend(node.targets.values().map(|u| u.url.clone()));
            }
            if let Some(java) = &runtimes.java {
                urls.extend(java.targets.values().map(|u| u.url.clone()));
            }
            if let Some(dotnet) = &runtimes.dotnet {
                urls.extend(dotnet.targets.values().map(|u| u.url.clone()));
            }
        }
        for target in self.targets.values() {
            urls.extend(target.artifacts.iter().map(|a| a.url.clone()));
            if let Some(compiled) = &target.compiled {
                urls.push(compiled.artifacts.url.clone());
            }
        }
        urls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_rejects_unknown_host() {
        let lock = CapsuleLock {
            allowlist: Some(vec!["example.com".to_string()]),
            tools: None,
            runtimes: None,
            targets: HashMap::from([(
                "linux-x86_64".to_string(),
                TargetEntry {
                    python_lockfile: None,
                    node_lockfile: None,
                    artifacts: vec![UrlEntry {
                        url: "https://nodejs.org/dist/v1.0.0/node.tar.gz".to_string(),
                        sha256: None,
                        filename: None,
                        artifact_type: None,
                    }],
                    compiled: None,
                },
            )]),
        };

        let urls = lock.collect_urls();
        assert!(urls.iter().any(|u| u.contains("nodejs.org")));
    }
}
