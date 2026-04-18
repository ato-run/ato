use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;

use crate::error::{CapsuleError, Result};
use crate::lockfile::CAPSULE_LOCK_FILE_NAME;
use crate::manifest;
use crate::packers::payload;
use crate::reporter::CapsuleReporter;

const LOCKFILE_VERSION: &str = "1";
const DEFAULT_UV_VERSION: &str = "0.4.19";
const DEFAULT_PNPM_VERSION: &str = "9.9.0";
const DEFAULT_YARN_CLASSIC_VERSION: &str = "1.22.22";
const DEFAULT_BUN_VERSION: &str = "1.2.10";
const DEFAULT_NODE_VERSION: &str = "20.12.0";
const DEFAULT_PYTHON_VERSION: &str = "3.11.9";
pub const DEFAULT_DENO_VERSION: &str = "1.46.3";

const DEFAULT_ALLOWLIST: &[&str] = &["nodejs.org", "registry.npmjs.org", "github.com"];

#[derive(Debug, Serialize)]
pub struct CapsuleLock {
    pub version: String,
    pub meta: LockMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<LockTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<LockRuntimes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<HashMap<String, LockTarget>>,
}

#[derive(Debug, Serialize)]
pub struct LockMeta {
    pub created_at: String,
    pub manifest_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct LockTools {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv: Option<LockTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pnpm: Option<LockTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yarn: Option<LockTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bun: Option<LockTool>,
}

#[derive(Debug, Serialize)]
pub struct LockTool {
    pub targets: HashMap<String, LockToolArtifact>,
}

#[derive(Debug, Serialize)]
pub struct LockToolArtifact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LockRuntimes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python: Option<LockRuntime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deno: Option<LockRuntime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<LockRuntime>,
}

#[derive(Debug, Serialize)]
pub struct LockRuntime {
    pub provider: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<HashMap<String, LockRuntimeArtifact>>,
}

#[derive(Debug, Serialize)]
pub struct LockRuntimeArtifact {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LockTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_lockfile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_lockfile: Option<String>,
}

pub fn write_lockfile(
    manifest_path: &Path,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    let loaded = manifest::load_manifest(manifest_path)?;
    let manifest_dir = loaded.dir.clone();
    let allowlist = read_allowlist(&loaded.raw)
        .unwrap_or_else(|| DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect());
    let languages = detect_languages(&loaded.raw, &loaded.path, &loaded.dir);
    let (os, arch) = detect_platform()?;
    let target_key = format!("{}-{}", os, arch);
    let triple = target_triple(&os, &arch)?;

    let mut tools = LockTools {
        uv: None,
        pnpm: None,
        yarn: None,
        bun: None,
    };
    if languages.contains("python") {
        let uv_url = format!(
            "https://github.com/astral-sh/uv/releases/download/{}/uv-{}.tar.gz",
            DEFAULT_UV_VERSION, triple
        );
        let mut targets = HashMap::new();
        targets.insert(
            triple.to_string(),
            LockToolArtifact {
                version: Some(DEFAULT_UV_VERSION.to_string()),
                url: uv_url,
                sha256: None,
            },
        );
        tools.uv = Some(LockTool { targets });
    }
    if languages.contains("node") {
        let pnpm_url = format!(
            "https://registry.npmjs.org/pnpm/-/pnpm-{}.tgz",
            DEFAULT_PNPM_VERSION
        );
        let mut targets = HashMap::new();
        targets.insert(
            triple.to_string(),
            LockToolArtifact {
                version: Some(DEFAULT_PNPM_VERSION.to_string()),
                url: pnpm_url,
                sha256: None,
            },
        );
        tools.pnpm = Some(LockTool { targets });
    }

    let mut runtimes = LockRuntimes {
        python: None,
        deno: None,
        node: None,
    };
    if languages.contains("python") {
        let mut targets = HashMap::new();
        let python_url = format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20241002/cpython-{}+20241002-{}-install_only.tar.gz",
            DEFAULT_PYTHON_VERSION,
            triple
        );
        targets.insert(
            triple.to_string(),
            LockRuntimeArtifact {
                url: python_url,
                sha256: None,
            },
        );
        runtimes.python = Some(LockRuntime {
            provider: "python-build-standalone".to_string(),
            version: DEFAULT_PYTHON_VERSION.to_string(),
            targets: Some(targets),
        });
    }
    if languages.contains("node") {
        let mut targets = HashMap::new();
        let node_url = format!(
            "https://nodejs.org/dist/v{}/node-v{}-{}-{}.tar.gz",
            DEFAULT_NODE_VERSION,
            DEFAULT_NODE_VERSION,
            match os.as_str() {
                "macos" => "darwin",
                "linux" => "linux",
                _ => "linux",
            },
            match arch.as_str() {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                _ => "x64",
            }
        );
        targets.insert(
            triple.to_string(),
            LockRuntimeArtifact {
                url: node_url,
                sha256: None,
            },
        );
        runtimes.node = Some(LockRuntime {
            provider: "official".to_string(),
            version: DEFAULT_NODE_VERSION.to_string(),
            targets: Some(targets),
        });
    }
    if languages.contains("deno") {
        let mut targets = HashMap::new();
        let deno_version = selected_runtime_version(&loaded.raw)
            .unwrap_or_else(|| DEFAULT_DENO_VERSION.to_string());
        let deno_target = match (os.as_str(), arch.as_str()) {
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported Deno platform: {} {}",
                    os, arch
                )))
            }
        };
        let deno_url = format!(
            "https://github.com/denoland/deno/releases/download/v{}/deno-{}.zip",
            deno_version, deno_target
        );
        targets.insert(
            triple.to_string(),
            LockRuntimeArtifact {
                url: deno_url,
                sha256: None,
            },
        );
        runtimes.deno = Some(LockRuntime {
            provider: "official".to_string(),
            version: deno_version,
            targets: Some(targets),
        });
    }

    let mut targets = HashMap::new();
    let mut target = LockTarget {
        python_lockfile: None,
        node_lockfile: None,
    };
    if languages.contains("python") {
        let uv_lock = manifest_dir.join("uv.lock");
        if uv_lock.exists() {
            target.python_lockfile = Some("uv.lock".to_string());
        }
    }
    if languages.contains("node") {
        let pnpm_lock = manifest_dir.join("pnpm-lock.yaml");
        let yarn_lock = manifest_dir.join("yarn.lock");
        let bun_lock = manifest_dir.join("bun.lock");
        let bun_lockb = manifest_dir.join("bun.lockb");
        if pnpm_lock.exists() {
            target.node_lockfile = Some("pnpm-lock.yaml".to_string());
        } else if yarn_lock.exists() {
            target.node_lockfile = Some("yarn.lock".to_string());
            let yarn_url = format!(
                "https://registry.npmjs.org/yarn/-/yarn-{}.tgz",
                DEFAULT_YARN_CLASSIC_VERSION
            );
            let mut targets = HashMap::new();
            targets.insert(
                triple.to_string(),
                LockToolArtifact {
                    version: Some(DEFAULT_YARN_CLASSIC_VERSION.to_string()),
                    url: yarn_url,
                    sha256: None,
                },
            );
            tools.yarn = Some(LockTool { targets });
        } else if bun_lock.exists() || bun_lockb.exists() {
            let lockfile_name = if bun_lockb.exists() {
                "bun.lockb"
            } else {
                "bun.lock"
            };
            target.node_lockfile = Some(lockfile_name.to_string());
            if let Some(bun_triple) = bun_platform_triple(&triple) {
                let bun_url = format!(
                    "https://github.com/oven-sh/bun/releases/download/bun-v{}/bun-{}.zip",
                    DEFAULT_BUN_VERSION, bun_triple
                );
                let mut targets = HashMap::new();
                targets.insert(
                    triple.to_string(),
                    LockToolArtifact {
                        version: Some(DEFAULT_BUN_VERSION.to_string()),
                        url: bun_url,
                        sha256: None,
                    },
                );
                tools.bun = Some(LockTool { targets });
            }
        }
    }
    if target.python_lockfile.is_some() || target.node_lockfile.is_some() {
        targets.insert(target_key, target);
    }

    let lockfile = CapsuleLock {
        version: LOCKFILE_VERSION.to_string(),
        meta: LockMeta {
            created_at: reproducible_created_at().to_rfc3339(),
            manifest_hash: payload::compute_manifest_hash_without_signatures(&loaded.model)
                .map_err(|e| {
                    CapsuleError::Pack(format!("Failed to compute manifest hash: {}", e))
                })?,
            allowlist: Some(allowlist.clone()),
        },
        tools: if tools.uv.is_some()
            || tools.pnpm.is_some()
            || tools.yarn.is_some()
            || tools.bun.is_some()
        {
            Some(tools)
        } else {
            None
        },
        runtimes: if runtimes.python.is_some() || runtimes.node.is_some() || runtimes.deno.is_some()
        {
            Some(runtimes)
        } else {
            None
        },
        targets: if targets.is_empty() {
            None
        } else {
            Some(targets)
        },
    };

    warn_on_allowlist(&lockfile, &allowlist, reporter.clone())?;

    let lock_path = manifest_dir.join(CAPSULE_LOCK_FILE_NAME);
    let content = serde_json::to_vec_pretty(&lockfile).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to serialize {}: {}",
            CAPSULE_LOCK_FILE_NAME, e
        ))
    })?;
    std::fs::write(&lock_path, content).map_err(|e| {
        CapsuleError::Pack(format!("Failed to write {}: {}", CAPSULE_LOCK_FILE_NAME, e))
    })?;

    Ok(lock_path)
}

fn detect_platform() -> Result<(String, String)> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err(CapsuleError::Pack("Unsupported OS".to_string()));
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err(CapsuleError::Pack("Unsupported architecture".to_string()));
    };

    Ok((os.to_string(), arch.to_string()))
}

fn reproducible_created_at() -> chrono::DateTime<Utc> {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    chrono::DateTime::<Utc>::from_timestamp(epoch, 0)
        .unwrap_or_else(|| chrono::DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch"))
}

fn target_triple(os: &str, arch: &str) -> Result<String> {
    let triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => {
            return Err(CapsuleError::Pack(format!(
                "Unsupported platform: {} {}",
                os, arch
            )))
        }
    };
    Ok(triple.to_string())
}

fn bun_platform_triple(rust_triple: &str) -> Option<&'static str> {
    match rust_triple {
        "aarch64-apple-darwin" => Some("darwin-aarch64"),
        "x86_64-apple-darwin" => Some("darwin-x86_64"),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => Some("linux-x64"),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => Some("linux-aarch64"),
        "x86_64-pc-windows-msvc" => Some("windows-x64.exe"),
        _ => None,
    }
}

fn detect_languages(
    manifest: &toml::Value,
    manifest_path: &Path,
    manifest_dir: &Path,
) -> HashSet<String> {
    let mut langs = HashSet::new();
    if let Some(language) = manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|t| t.get("language"))
        .and_then(|v| v.as_str())
    {
        langs.insert(language.to_string());
    }
    if manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|t| t.get("driver"))
        .and_then(|v| v.as_str())
        .map(|v| v.eq_ignore_ascii_case("deno"))
        .unwrap_or(false)
    {
        langs.insert("deno".to_string());
    }

    let entrypoint = manifest
        .get("execution")
        .and_then(|e| e.get("entrypoint"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if entrypoint.ends_with(".py") || entrypoint == "python" || entrypoint == "python3" {
        langs.insert("python".to_string());
    }
    if entrypoint.ends_with(".js")
        || entrypoint.ends_with(".mjs")
        || entrypoint.ends_with(".cjs")
        || entrypoint.ends_with(".ts")
        || entrypoint == "node"
    {
        langs.insert("node".to_string());
    }

    if langs.is_empty() {
        let _ = manifest_path;
        let _ = manifest_dir;
    }

    langs
}

fn read_allowlist(manifest: &toml::Value) -> Option<Vec<String>> {
    let list = manifest
        .get("runtime")
        .and_then(|v| v.get("allowlist"))
        .and_then(|v| v.as_array())?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect::<Vec<_>>();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn warn_on_allowlist(
    lockfile: &CapsuleLock,
    allowlist: &[String],
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<()> {
    let urls = collect_urls(lockfile);
    for url in urls {
        if !is_allowed(&url, allowlist) {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Allowlist mismatch in {}: {}",
                CAPSULE_LOCK_FILE_NAME, url
            )))?;
        }
    }
    Ok(())
}

fn collect_urls(lockfile: &CapsuleLock) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(tools) = &lockfile.tools {
        if let Some(uv) = &tools.uv {
            out.extend(uv.targets.values().map(|t| t.url.clone()));
        }
        if let Some(pnpm) = &tools.pnpm {
            out.extend(pnpm.targets.values().map(|t| t.url.clone()));
        }
    }
    if let Some(runtimes) = &lockfile.runtimes {
        if let Some(py) = &runtimes.python {
            if let Some(targets) = &py.targets {
                out.extend(targets.values().map(|t| t.url.clone()));
            }
        }
        if let Some(node) = &runtimes.node {
            if let Some(targets) = &node.targets {
                out.extend(targets.values().map(|t| t.url.clone()));
            }
        }
        if let Some(deno) = &runtimes.deno {
            if let Some(targets) = &deno.targets {
                out.extend(targets.values().map(|t| t.url.clone()));
            }
        }
    }
    out
}

fn selected_runtime_version(manifest: &toml::Value) -> Option<String> {
    manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|t| t.get("runtime_version"))
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn is_allowed(url: &str, allowlist: &[String]) -> bool {
    if allowlist.iter().any(|entry| entry == "*") {
        return true;
    }
    let host = extract_host(url);
    for entry in allowlist {
        if entry.contains("://") {
            if url.starts_with(entry) {
                return true;
            }
            continue;
        }
        if let Some(host) = &host {
            if host == entry || host.ends_with(&format!(".{}", entry)) {
                return true;
            }
        }
    }
    false
}

fn extract_host(url: &str) -> Option<String> {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = without_scheme.split('/').next()?;
    let host = host_port.split('@').next_back().unwrap_or(host_port);
    Some(host.split(':').next().unwrap_or(host).to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::write_lockfile;

    #[test]
    fn writes_lockfile_with_allowlist() {
        let temp = TempDir::new().unwrap();
        let manifest_path = temp.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"schema_version = "0.3"
name = "lockfile-test"
version = "0.1.0"
type = "app"

runtime = "source"
run = "python"
[runtime]
allowlist = ["nodejs.org", "github.com"]
"#,
        )
        .unwrap();

        let reporter = Arc::new(crate::reporter::NoOpReporter);
        let lock_path = write_lockfile(&manifest_path, reporter).unwrap();
        let content = std::fs::read_to_string(lock_path).unwrap();
        assert!(content.contains("allowlist"));
        assert!(content.contains("version"));
    }
}
