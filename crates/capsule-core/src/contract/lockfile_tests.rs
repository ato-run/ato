use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::{routing::get, Json, Router};
use serde_json::json;
use tempfile::TempDir;

use crate::packers::runtime_fetcher::RuntimeFetcher;
use crate::reporter::CapsuleReporter;

use super::lockfile_runtime::{
    deno_artifact_filename, required_env_keys_from_manifest, run_command_inner, uv_artifact_url,
};
use super::lockfile_support::{
    capsule_error_pack, create_atomic_temp_file, write_atomic_bytes_with_os_lock,
};
use super::{
    ensure_lockfile, ensure_lockfile_for_compat_input, generate_lockfile,
    lockfile_has_required_platform_coverage, lockfile_inputs_snapshot_path, lockfile_output_path,
    lockfile_runtime_platforms, lockfile_runtime_target_labels,
    orchestration_service_target_labels, read_lockfile, read_runtime_tools,
    required_runtime_version, resolve_external_capsule_dependencies,
    semantic_manifest_hash_from_text, tool_capsule_env_bindings,
    verify_lockfile_external_dependencies, verify_lockfile_manifest, CapsuleLock, LockMeta,
    LockedCapsuleDependency, LockedToolCapsule, LockedToolExports, RuntimeArtifact, RuntimeEntry,
    RuntimeSection, ToolArtifact, ToolSection, ToolTargets, CAPSULE_LOCK_FILE_NAME,
    ENV_STORE_API_URL, LOCKFILE_INPUT_SNAPSHOT_NAME, SUPPORTED_RUNTIME_PLATFORMS, UV_VERSION,
};

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn serialize_lockfile_with_allowlist() {
    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-01-20T00:00:00Z".to_string(),
            manifest_hash: "sha256:deadbeef".to_string(),
        },
        allowlist: Some(vec!["nodejs.org".to_string()]),
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: None,
        runtimes: None,
        targets: HashMap::new(),
    };

    let json = serde_json::to_string(&lockfile).unwrap();
    let parsed: CapsuleLock = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.allowlist.unwrap()[0], "nodejs.org");
}

#[test]
fn verify_lockfile_manifest_hash() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let lockfile_path = temp.path().join(CAPSULE_LOCK_FILE_NAME);
    let manifest_text = r#"schema_version = "0.3"
name = "demo"
version = "1.0.0"
type = "app"
"#;
    fs::write(&manifest_path, manifest_text).unwrap();

    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-01-20T00:00:00Z".to_string(),
            manifest_hash: semantic_manifest_hash_from_text(manifest_text).unwrap(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: None,
        runtimes: None,
        targets: HashMap::new(),
    };

    let json = serde_json::to_vec_pretty(&lockfile).unwrap();
    fs::write(&lockfile_path, json).unwrap();

    verify_lockfile_manifest(&manifest_path, &lockfile_path).unwrap();
}

#[test]
fn verify_lockfile_external_dependencies_matches_manifest() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "web"

[targets.web]
external_dependencies = [
    { alias = "auth", source = "capsule://store/acme/auth-svc", source_type = "store", injection_bindings = { MODEL_DIR = "https://data.tld/weights.zip" } }
]
"#,
    )
    .unwrap();

    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-01-20T00:00:00Z".to_string(),
            manifest_hash: "sha256:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: vec![LockedCapsuleDependency {
            name: "auth".to_string(),
            source: "capsule://store/acme/auth-svc".to_string(),
            source_type: "store".to_string(),
            contract: None,
            injection_bindings: BTreeMap::from([(
                "MODEL_DIR".to_string(),
                "https://data.tld/weights.zip".to_string(),
            )]),
            parameters: BTreeMap::new(),
            credentials: BTreeMap::new(),
            identity_exports: BTreeMap::new(),
            resolved_version: Some("1.2.3".to_string()),
            digest: Some("blake3:deadbeef".to_string()),
            sha256: Some("sha256:beadfeed".to_string()),
            artifact_url: Some("https://example.test/auth.capsule".to_string()),
        }],
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: None,
        runtimes: None,
        targets: HashMap::new(),
    };

    verify_lockfile_external_dependencies(&manifest, &lockfile).unwrap();
}

#[test]
fn manifest_external_capsule_dependencies_reads_top_level_dependency_contracts() {
    let manifest: toml::Value = toml::from_str(
        r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  database = "appdb"

  [dependencies.db.credentials]
  password = "{{env.PG_PASSWORD}}"
"#,
    )
    .unwrap();

    let dependencies = super::manifest_external_capsule_dependencies(&manifest).unwrap();
    assert_eq!(dependencies.len(), 1);
    assert_eq!(dependencies[0].alias, "db");
    assert_eq!(dependencies[0].source_type, "store");
    assert_eq!(dependencies[0].contract.as_deref(), Some("service@1"));
    assert_eq!(
        dependencies[0].parameters.get("database"),
        Some(&crate::types::ParamValue::String("appdb".to_string()))
    );
    assert_eq!(
        dependencies[0]
            .credentials
            .get("password")
            .map(ToString::to_string)
            .as_deref(),
        Some("{{env.PG_PASSWORD}}")
    );
}

#[test]
fn verify_lockfile_external_dependencies_matches_top_level_dependency_contracts() {
    let manifest: toml::Value = toml::from_str(
        r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  database = "appdb"
"#,
    )
    .unwrap();

    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-01-20T00:00:00Z".to_string(),
            manifest_hash: "sha256:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: vec![LockedCapsuleDependency {
            name: "db".to_string(),
            source: "capsule://ato/acme-postgres@16".to_string(),
            source_type: "store".to_string(),
            contract: Some("service@1".to_string()),
            injection_bindings: BTreeMap::new(),
            parameters: BTreeMap::from([(
                "database".to_string(),
                crate::types::ParamValue::String("appdb".to_string()),
            )]),
            credentials: BTreeMap::new(),
            identity_exports: BTreeMap::new(),
            resolved_version: Some("16.0.0".to_string()),
            digest: Some("blake3:deadbeef".to_string()),
            sha256: Some("sha256:beadfeed".to_string()),
            artifact_url: Some("https://example.test/postgres.capsule".to_string()),
        }],
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: None,
        runtimes: None,
        targets: HashMap::new(),
    };

    verify_lockfile_external_dependencies(&manifest, &lockfile).unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn resolve_external_capsule_dependencies_reads_store_distribution() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let address = listener.local_addr().expect("local addr");
    let app = Router::new().route(
        "/v1/capsules/by/acme/auth-svc/distributions",
        get(|| async {
            Json(json!({
                "version": "1.2.3",
                "artifact_url": "https://registry.test/auth-svc-1.2.3.capsule",
                "sha256": "sha256:beadfeed",
                "blake3": "blake3:deadbeef"
            }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve app");
    });
    let _guard = EnvGuard::set(ENV_STORE_API_URL, &format!("http://{}", address));

    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "web"

[targets.web]
external_dependencies = [
    { alias = "auth", source = "capsule://store/acme/auth-svc", source_type = "store", injection_bindings = { MODEL_DIR = "https://data.tld/weights.zip" } }
]
"#,
    )
    .unwrap();

    let resolved = resolve_external_capsule_dependencies(&manifest)
        .await
        .expect("resolve dependencies");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "auth");
    assert_eq!(
        resolved[0]
            .injection_bindings
            .get("MODEL_DIR")
            .map(String::as_str),
        Some("https://data.tld/weights.zip")
    );
    assert_eq!(resolved[0].resolved_version.as_deref(), Some("1.2.3"));
    assert_eq!(resolved[0].digest.as_deref(), Some("blake3:deadbeef"));
}

#[test]
fn deno_artifact_filename_uses_release_target_triplets() {
    assert_eq!(
        deno_artifact_filename("macos", "aarch64").unwrap(),
        "deno-aarch64-apple-darwin.zip"
    );
    assert_eq!(
        deno_artifact_filename("linux", "x86_64").unwrap(),
        "deno-x86_64-unknown-linux-gnu.zip"
    );
    assert_eq!(
        deno_artifact_filename("windows", "x86_64").unwrap(),
        "deno-x86_64-pc-windows-msvc.zip"
    );
    assert!(deno_artifact_filename("windows", "aarch64").is_err());
}

#[test]
fn runtime_tools_are_read_from_selected_target() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_tools = { node = "20.11.0", python = "3.11.7" }
"#,
    )
    .unwrap();

    let tools = read_runtime_tools(&manifest);
    assert_eq!(tools.get("node"), Some(&"20.11.0".to_string()));
    assert_eq!(tools.get("python"), Some(&"3.11.7".to_string()));
}

#[test]
fn orchestration_service_targets_are_collected() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "dashboard"

[targets.dashboard]
runtime = "web"
driver = "node"

[targets.control_plane]
runtime = "source"
driver = "python"

[services.main]
target = "dashboard"
depends_on = ["control_plane"]

[services.control_plane]
target = "control_plane"
"#,
    )
    .unwrap();

    let mut labels = orchestration_service_target_labels(&manifest);
    labels.sort();
    assert_eq!(
        labels,
        vec!["control_plane".to_string(), "dashboard".to_string()]
    );
}

#[test]
fn lockfile_runtime_targets_include_named_targets_without_services() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"

[targets.web]
runtime = "source"
driver = "node"
runtime_version = "20.12.0"
"#,
    )
    .unwrap();

    let mut labels = lockfile_runtime_target_labels(&manifest);
    labels.sort();
    assert_eq!(labels, vec!["app".to_string(), "web".to_string()]);
}

#[test]
fn required_runtime_version_for_web_deno_target() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
"#,
    )
    .unwrap();

    let version = required_runtime_version(&manifest).unwrap();
    assert_eq!(version.as_deref(), Some("1.46.3"));
}

#[test]
fn web_targets_include_all_supported_runtime_platforms_in_lockfile() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
"#,
    )
    .unwrap();

    let platforms = lockfile_runtime_platforms(&manifest).unwrap();
    assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
    for expected in SUPPORTED_RUNTIME_PLATFORMS {
        assert!(platforms.contains(expected));
    }
}

#[test]
fn source_managed_runtime_targets_include_all_supported_runtime_platforms_in_lockfile() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
"#,
    )
    .unwrap();

    let platforms = lockfile_runtime_platforms(&manifest).unwrap();
    assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
    for expected in SUPPORTED_RUNTIME_PLATFORMS {
        assert!(platforms.contains(expected));
    }
}

#[test]
fn source_targets_with_runtime_tools_include_all_supported_runtime_platforms_in_lockfile() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "source"
driver = "node"
runtime_version = "20.11.0"
runtime_tools = { python = "3.11.7" }
"#,
    )
    .unwrap();

    let platforms = lockfile_runtime_platforms(&manifest).unwrap();
    assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
    for expected in SUPPORTED_RUNTIME_PLATFORMS {
        assert!(platforms.contains(expected));
    }
}

#[test]
fn stale_universal_lockfile_is_detected_when_runtime_targets_are_host_only() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
    )
    .unwrap();

    let host_only_targets = HashMap::from([(
        "aarch64-apple-darwin".to_string(),
        RuntimeArtifact {
            url: "https://example.com/runtime.tar.gz".to_string(),
            sha256: "deadbeef".to_string(),
        },
    )]);
    let host_only_tool_targets = HashMap::from([(
        "aarch64-apple-darwin".to_string(),
        ToolArtifact {
            url: "https://example.com/uv.tar.gz".to_string(),
            sha256: Some("deadbeef".to_string()),
            version: Some("0.4.19".to_string()),
        },
    )]);
    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-03-08T00:00:00Z".to_string(),
            manifest_hash: "blake3:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: Some(ToolSection {
            uv: Some(ToolTargets {
                targets: host_only_tool_targets,
            }),
            pnpm: None,
            yarn: None,
            bun: None,
        }),
        runtimes: Some(RuntimeSection {
            python: None,
            deno: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "1.46.3".to_string(),
                targets: host_only_targets.clone(),
            }),
            node: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "20.11.0".to_string(),
                targets: host_only_targets,
            }),
            java: None,
            dotnet: None,
        }),
        targets: HashMap::new(),
    };

    assert!(!lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
}

#[test]
fn universal_lockfile_passes_when_all_runtime_targets_are_present() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
    )
    .unwrap();

    let runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                RuntimeArtifact {
                    url: format!(
                        "https://example.com/{}/runtime.tar.gz",
                        platform.target_triple
                    ),
                    sha256: "deadbeef".to_string(),
                },
            )
        })
        .collect();
    let tool_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: format!("https://example.com/{}/uv.tar.gz", platform.target_triple),
                    sha256: Some("deadbeef".to_string()),
                    version: Some("0.4.19".to_string()),
                },
            )
        })
        .collect();
    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-03-08T00:00:00Z".to_string(),
            manifest_hash: "blake3:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: Some(ToolSection {
            uv: Some(ToolTargets {
                targets: tool_targets,
            }),
            pnpm: None,
            yarn: None,
            bun: None,
        }),
        runtimes: Some(RuntimeSection {
            python: Some(RuntimeEntry {
                provider: "python-build-standalone".to_string(),
                version: "3.11.10".to_string(),
                targets: runtime_targets.clone(),
            }),
            deno: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "1.46.3".to_string(),
                targets: runtime_targets.clone(),
            }),
            node: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "20.11.0".to_string(),
                targets: runtime_targets,
            }),
            java: None,
            dotnet: None,
        }),
        targets: HashMap::new(),
    };

    assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
}

#[test]
fn universal_lockfile_allows_deno_without_windows_arm64_target() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
    )
    .unwrap();

    let common_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                RuntimeArtifact {
                    url: format!(
                        "https://example.com/{}/runtime.tar.gz",
                        platform.target_triple
                    ),
                    sha256: "deadbeef".to_string(),
                },
            )
        })
        .collect();
    let deno_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .filter(|platform| deno_artifact_filename(platform.os, platform.arch).is_ok())
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                RuntimeArtifact {
                    url: format!("https://example.com/{}/deno.zip", platform.target_triple),
                    sha256: "deadbeef".to_string(),
                },
            )
        })
        .collect();
    let tool_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: format!("https://example.com/{}/uv.tar.gz", platform.target_triple),
                    sha256: Some("deadbeef".to_string()),
                    version: Some("0.4.19".to_string()),
                },
            )
        })
        .collect();
    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-03-08T00:00:00Z".to_string(),
            manifest_hash: "blake3:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: Some(ToolSection {
            uv: Some(ToolTargets {
                targets: tool_targets,
            }),
            pnpm: None,
            yarn: None,
            bun: None,
        }),
        runtimes: Some(RuntimeSection {
            python: Some(RuntimeEntry {
                provider: "python-build-standalone".to_string(),
                version: "3.11.10".to_string(),
                targets: common_runtime_targets.clone(),
            }),
            deno: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "1.46.3".to_string(),
                targets: deno_runtime_targets,
            }),
            node: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "20.11.0".to_string(),
                targets: common_runtime_targets,
            }),
            java: None,
            dotnet: None,
        }),
        targets: HashMap::new(),
    };

    assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
}

#[test]
fn universal_lockfile_allows_python_without_windows_arm64_target() {
    let manifest: toml::Value = toml::from_str(
        r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
    )
    .unwrap();

    let python_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .filter(|platform| {
            RuntimeFetcher::get_python_download_url("3.11.10", platform.os, platform.arch).is_ok()
        })
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                RuntimeArtifact {
                    url: format!(
                        "https://example.com/{}/python.tar.gz",
                        platform.target_triple
                    ),
                    sha256: "deadbeef".to_string(),
                },
            )
        })
        .collect();
    let common_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                RuntimeArtifact {
                    url: format!(
                        "https://example.com/{}/runtime.tar.gz",
                        platform.target_triple
                    ),
                    sha256: "deadbeef".to_string(),
                },
            )
        })
        .collect();
    let uv_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .filter(|platform| uv_artifact_url(platform.target_triple).is_some())
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: uv_artifact_url(platform.target_triple).unwrap(),
                    sha256: Some("deadbeef".to_string()),
                    version: Some(UV_VERSION.to_string()),
                },
            )
        })
        .collect();
    let lockfile = CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-03-08T00:00:00Z".to_string(),
            manifest_hash: "blake3:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules: Default::default(),
        tools: Some(ToolSection {
            uv: Some(ToolTargets {
                targets: uv_targets,
            }),
            pnpm: None,
            yarn: None,
            bun: None,
        }),
        runtimes: Some(RuntimeSection {
            python: Some(RuntimeEntry {
                provider: "python-build-standalone".to_string(),
                version: "3.11.10".to_string(),
                targets: python_targets,
            }),
            deno: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "1.46.3".to_string(),
                targets: common_runtime_targets.clone(),
            }),
            node: Some(RuntimeEntry {
                provider: "official".to_string(),
                version: "20.11.0".to_string(),
                targets: common_runtime_targets,
            }),
            java: None,
            dotnet: None,
        }),
        targets: HashMap::new(),
    };

    assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
}

#[test]
fn uv_artifact_url_uses_zip_for_windows_x64_and_skips_windows_arm64() {
    assert_eq!(
        uv_artifact_url("x86_64-pc-windows-msvc").as_deref(),
        Some("https://github.com/astral-sh/uv/releases/download/0.4.19/uv-x86_64-pc-windows-msvc.zip")
    );
    assert!(uv_artifact_url("aarch64-pc-windows-msvc").is_none());
    assert_eq!(
        uv_artifact_url("x86_64-unknown-linux-gnu").as_deref(),
        Some("https://github.com/astral-sh/uv/releases/download/0.4.19/uv-x86_64-unknown-linux-gnu.tar.gz")
    );
}

#[test]
fn required_env_keys_from_manifest_collects_modern_and_legacy() {
    let manifest: toml::Value = toml::from_str(
        r#"
[targets.default]
runtime = "web"
driver = "deno"
required_env = ["API_TOKEN", " ACCOUNT_ID ", ""]
env = { ATO_ORCH_REQUIRED_ENVS = "LEGACY_ONE, LEGACY_TWO,API_TOKEN" }
"#,
    )
    .unwrap();

    let keys = required_env_keys_from_manifest(&manifest);
    assert_eq!(
        keys,
        vec![
            "ACCOUNT_ID".to_string(),
            "API_TOKEN".to_string(),
            "LEGACY_ONE".to_string(),
            "LEGACY_TWO".to_string(),
        ]
    );
}

#[test]
fn atomic_write_replaces_file_without_temp_leaks() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join(CAPSULE_LOCK_FILE_NAME);

    write_atomic_bytes_with_os_lock(&target, b"first", "test lockfile", capsule_error_pack)
        .unwrap();
    write_atomic_bytes_with_os_lock(&target, b"second", "test lockfile", capsule_error_pack)
        .unwrap();

    let written = fs::read_to_string(&target).unwrap();
    assert_eq!(written, "second");

    let leftovers = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".capsule.lock.tmp-"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "temp files leaked: {:?}", leftovers);
}

#[test]
fn atomic_temp_file_is_created_in_target_directory() {
    let temp = TempDir::new().unwrap();
    let tmp_path = create_atomic_temp_file(
        temp.path(),
        CAPSULE_LOCK_FILE_NAME,
        "test temp file",
        &capsule_error_pack,
    )
    .unwrap();

    assert_eq!(tmp_path.parent(), Some(temp.path()));
    assert!(tmp_path.exists());
    let _ = fs::remove_file(tmp_path);
}

#[test]
fn ensure_lockfile_reuses_when_inputs_unchanged() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "source/main.sh""#;
    fs::write(&manifest_path, manifest_text).unwrap();
    fs::create_dir_all(temp.path().join("source")).unwrap();
    fs::write(temp.path().join("source/main.sh"), "echo demo").unwrap();

    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let first = rt
        .block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter.clone(),
            false,
        ))
        .unwrap();
    let first_lock = read_lockfile(&first).unwrap();

    std::thread::sleep(Duration::from_millis(20));

    let second = rt
        .block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter,
            false,
        ))
        .unwrap();
    let second_lock = read_lockfile(&second).unwrap();

    assert_eq!(first_lock.meta.created_at, second_lock.meta.created_at);
    assert!(lockfile_inputs_snapshot_path(temp.path()).exists());
}

#[test]
fn ensure_lockfile_for_compat_input_does_not_materialize_bridge_manifest() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("main.sh"), "echo demo\n").unwrap();

    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "main.sh""#;
    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let bridge = crate::router::CompatManifestBridge::from_manifest_value(&manifest_raw).unwrap();
    let compat_input =
        crate::router::CompatProjectInput::from_bridge(temp.path().to_path_buf(), bridge).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let lock_path = rt
        .block_on(ensure_lockfile_for_compat_input(
            &compat_input,
            reporter,
            false,
        ))
        .unwrap();

    assert_eq!(lock_path, lockfile_output_path(temp.path()));
    assert!(lock_path.exists());
    assert!(!temp.path().join(CAPSULE_LOCK_FILE_NAME).exists());
    assert!(!temp.path().join(LOCKFILE_INPUT_SNAPSHOT_NAME).exists());
    assert!(!temp.path().join("capsule.toml").exists());
    assert!(!temp
        .path()
        .join(".tmp")
        .join("compat-manifest-bridge")
        .join("capsule.toml")
        .exists());
}

#[test]
fn ensure_lockfile_accepts_existing_deno_lock() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/deno"
runtime_version = "1.46.3"
run = "main.ts""#;
    fs::write(&manifest_path, manifest_text).unwrap();
    fs::write(temp.path().join("main.ts"), "console.log('demo')").unwrap();
    fs::write(
        temp.path().join("deno.lock"),
        r#"{"version":"4","specifiers":{},"packages":{}}"#,
    )
    .unwrap();

    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let lock_path = rt
        .block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter,
            false,
        ))
        .unwrap();

    assert_eq!(lock_path, lockfile_output_path(temp.path()));
    assert!(lock_path.exists());
    assert!(lockfile_inputs_snapshot_path(temp.path()).exists());
    assert!(!temp.path().join(CAPSULE_LOCK_FILE_NAME).exists());
    assert!(!temp.path().join(LOCKFILE_INPUT_SNAPSHOT_NAME).exists());
}

#[test]
fn ensure_lockfile_accepts_existing_uv_lock() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.10"
dependencies = "requirements.txt"
run = "uv run python3 main.py""#;
    fs::write(&manifest_path, manifest_text).unwrap();
    fs::write(temp.path().join("main.py"), "print('demo')\n").unwrap();
    fs::write(temp.path().join("requirements.txt"), "fastapi==0.115.0\n").unwrap();
    fs::write(
        temp.path().join("uv.lock"),
        "version = 1\nrevision = 1\nrequires-python = \">=3.11\"\n",
    )
    .unwrap();

    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let lock_path = rt
        .block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter,
            false,
        ))
        .unwrap();

    assert_eq!(lock_path, lockfile_output_path(temp.path()));
    assert!(lock_path.exists());
    assert!(lockfile_inputs_snapshot_path(temp.path()).exists());
    assert!(!temp.path().join(CAPSULE_LOCK_FILE_NAME).exists());
    assert!(!temp.path().join(LOCKFILE_INPUT_SNAPSHOT_NAME).exists());
}

#[test]
fn ensure_lockfile_accepts_existing_pnpm_lock() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/bin.ts fixtures/db.json"

[pack]
include = ["src/**", "fixtures/db.json", "package.json", "pnpm-lock.yaml"]
"#;
    fs::write(&manifest_path, manifest_text).unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::create_dir_all(temp.path().join("fixtures")).unwrap();
    fs::write(temp.path().join("src/bin.ts"), "console.log('demo')").unwrap();
    fs::write(temp.path().join("fixtures/db.json"), "{}\n").unwrap();
    fs::write(
        temp.path().join("package.json"),
        r#"{"name":"demo","packageManager":"pnpm@10.0.0"}"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();

    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let lock_path = rt
        .block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter,
            false,
        ))
        .unwrap();

    assert_eq!(lock_path, lockfile_output_path(temp.path()));
    assert!(lock_path.exists());
    assert!(lockfile_inputs_snapshot_path(temp.path()).exists());
    assert!(!temp.path().join(CAPSULE_LOCK_FILE_NAME).exists());
    assert!(!temp.path().join(LOCKFILE_INPUT_SNAPSHOT_NAME).exists());
}

#[test]
fn generate_lockfile_does_not_include_ambient_tools_for_native_target() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "main.sh""#;
    fs::write(&manifest_path, manifest_text).unwrap();
    fs::write(temp.path().join("main.sh"), "echo demo").unwrap();

    let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let lockfile = rt
        .block_on(generate_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            temp.path(),
            reporter,
            false,
        ))
        .unwrap();

    assert!(lockfile.tools.is_none());
}

#[tokio::test]
async fn run_command_inner_rejects_relative_program() {
    let cmd = std::process::Command::new("echo");
    let err = run_command_inner(cmd).await.expect_err("must fail closed");
    assert!(err
        .to_string()
        .contains("Refusing to execute non-absolute command path"));
}

// ──────────────────────────────────────────────────────────────────────────
// Tool-capsule lockfile section (#71)
// ──────────────────────────────────────────────────────────────────────────

fn empty_lockfile_with_tool_capsules(
    tool_capsules: BTreeMap<String, LockedToolCapsule>,
) -> CapsuleLock {
    CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: "2026-05-06T00:00:00Z".to_string(),
            manifest_hash: "sha256:deadbeef".to_string(),
        },
        allowlist: None,
        capsule_dependencies: Vec::new(),
        injected_data: HashMap::new(),
        tool_capsules,
        tools: None,
        runtimes: None,
        targets: HashMap::new(),
    }
}

#[test]
fn lockfile_serializes_tool_capsule_entries() {
    let mut tool_capsules = BTreeMap::new();
    tool_capsules.insert(
        "postgres".to_string(),
        LockedToolCapsule {
            resolved: "capsule://ato.run/tools/postgresql-binaries@16.4.0".to_string(),
            platform: "darwin-arm64".to_string(),
            artifact_hash: "sha256:0123456789abcdef".to_string(),
            artifact_url: Some(
                "https://tools.ato.run/postgresql-16.4-darwin-arm64.tar.zst".to_string(),
            ),
            exports: LockedToolExports {
                binaries: BTreeMap::from([
                    ("initdb".to_string(), "bin/initdb".to_string()),
                    ("pg_isready".to_string(), "bin/pg_isready".to_string()),
                ]),
                paths: BTreeMap::from([("share_dir".to_string(), "share".to_string())]),
            },
            bind_env: BTreeMap::from([
                ("initdb".to_string(), "ATO_TOOL_INITDB".to_string()),
                ("pg_isready".to_string(), "ATO_TOOL_PG_ISREADY".to_string()),
            ]),
        },
    );

    let lockfile = empty_lockfile_with_tool_capsules(tool_capsules);
    let json = serde_json::to_string(&lockfile).unwrap();

    assert!(json.contains("\"tool_capsules\""));
    assert!(json.contains("postgresql-binaries@16.4.0"));
    assert!(json.contains("\"darwin-arm64\""));
    assert!(json.contains("\"ATO_TOOL_INITDB\""));

    let parsed: CapsuleLock = serde_json::from_str(&json).unwrap();
    let postgres = parsed
        .tool_capsules
        .get("postgres")
        .expect("postgres entry");
    assert_eq!(postgres.platform, "darwin-arm64");
    assert_eq!(
        postgres
            .exports
            .binaries
            .get("pg_isready")
            .map(String::as_str),
        Some("bin/pg_isready")
    );
    assert_eq!(
        postgres.bind_env.get("initdb").map(String::as_str),
        Some("ATO_TOOL_INITDB")
    );
}

#[test]
fn lockfile_omits_tool_capsules_when_empty() {
    let lockfile = empty_lockfile_with_tool_capsules(BTreeMap::new());
    let json = serde_json::to_string(&lockfile).unwrap();
    assert!(
        !json.contains("\"tool_capsules\""),
        "empty tool_capsules must be omitted from serialized lockfile, got: {json}"
    );
}

#[test]
fn locked_tool_capsule_env_bindings_use_explicit_map() {
    let entry = LockedToolCapsule {
        resolved: "capsule://ato.run/tools/postgresql-binaries@16.4.0".to_string(),
        platform: "darwin-arm64".to_string(),
        artifact_hash: "sha256:abc".to_string(),
        artifact_url: None,
        exports: LockedToolExports {
            binaries: BTreeMap::from([
                ("initdb".to_string(), "bin/initdb".to_string()),
                ("pg_isready".to_string(), "bin/pg_isready".to_string()),
            ]),
            paths: BTreeMap::from([("root".to_string(), ".".to_string())]),
        },
        bind_env: BTreeMap::from([
            ("initdb".to_string(), "ATO_TOOL_INITDB".to_string()),
            ("root".to_string(), "ATO_TOOL_POSTGRES_ROOT".to_string()),
        ]),
    };

    let projected = std::path::Path::new("/runs/run-1/tools/postgres");
    let bindings = entry.env_bindings("postgres", projected);
    let map: std::collections::HashMap<_, _> = bindings.into_iter().collect();

    // Explicit bind_env wins where present.
    assert_eq!(
        map.get("ATO_TOOL_INITDB").map(|p| p.as_path()),
        Some(std::path::Path::new(
            "/runs/run-1/tools/postgres/bin/initdb"
        ))
    );
    assert_eq!(
        map.get("ATO_TOOL_POSTGRES_ROOT").map(|p| p.as_path()),
        Some(std::path::Path::new("/runs/run-1/tools/postgres/."))
    );
    // Falls back to ATO_TOOL_<ALIAS>_<EXPORT> for unbound exports.
    assert_eq!(
        map.get("ATO_TOOL_POSTGRES_PG_ISREADY").map(|p| p.as_path()),
        Some(std::path::Path::new(
            "/runs/run-1/tools/postgres/bin/pg_isready"
        ))
    );
}

#[test]
fn locked_tool_capsule_env_bindings_normalize_hyphens() {
    let entry = LockedToolCapsule {
        resolved: "capsule://ato.run/tools/some-tool@1.0.0".to_string(),
        platform: "linux-x86_64".to_string(),
        artifact_hash: "sha256:def".to_string(),
        artifact_url: None,
        exports: LockedToolExports {
            binaries: BTreeMap::from([("multi-word".to_string(), "bin/multi-word".to_string())]),
            paths: BTreeMap::new(),
        },
        bind_env: BTreeMap::new(),
    };

    let projected = std::path::Path::new("/runs/run-1/tools/my-tool");
    let bindings = entry.env_bindings("my-tool", projected);

    assert_eq!(bindings.len(), 1);
    let (name, path) = &bindings[0];
    // Hyphens in alias and export normalize to underscores in env-var names.
    assert_eq!(name, "ATO_TOOL_MY_TOOL_MULTI_WORD");
    assert_eq!(
        path,
        std::path::Path::new("/runs/run-1/tools/my-tool/bin/multi-word")
    );
}

#[test]
fn tool_capsule_env_bindings_merges_across_aliases() {
    use crate::common::paths::AtoRunLayout;

    let mut tool_capsules = BTreeMap::new();
    tool_capsules.insert(
        "postgres".to_string(),
        LockedToolCapsule {
            resolved: "capsule://ato.run/tools/postgresql-binaries@16.4.0".to_string(),
            platform: "darwin-arm64".to_string(),
            artifact_hash: "sha256:abc".to_string(),
            artifact_url: None,
            exports: LockedToolExports {
                binaries: BTreeMap::from([("initdb".to_string(), "bin/initdb".to_string())]),
                paths: BTreeMap::new(),
            },
            bind_env: BTreeMap::from([("initdb".to_string(), "ATO_TOOL_INITDB".to_string())]),
        },
    );
    tool_capsules.insert(
        "redis".to_string(),
        LockedToolCapsule {
            resolved: "capsule://ato.run/tools/redis-binaries@7.2.0".to_string(),
            platform: "darwin-arm64".to_string(),
            artifact_hash: "sha256:def".to_string(),
            artifact_url: None,
            exports: LockedToolExports {
                binaries: BTreeMap::from([("redis-cli".to_string(), "bin/redis-cli".to_string())]),
                paths: BTreeMap::new(),
            },
            bind_env: BTreeMap::new(),
        },
    );
    let lock = empty_lockfile_with_tool_capsules(tool_capsules);
    let layout = AtoRunLayout::for_root(std::path::PathBuf::from("/runs/run-1"));

    let bindings = tool_capsule_env_bindings(&lock, &layout).expect("no conflicts");

    assert_eq!(
        bindings.get("ATO_TOOL_INITDB").map(|p| p.as_path()),
        Some(std::path::Path::new(
            "/runs/run-1/tools/postgres/bin/initdb"
        ))
    );
    assert_eq!(
        bindings
            .get("ATO_TOOL_REDIS_REDIS_CLI")
            .map(|p| p.as_path()),
        Some(std::path::Path::new(
            "/runs/run-1/tools/redis/bin/redis-cli"
        ))
    );
    assert_eq!(bindings.len(), 2);
}

#[test]
fn tool_capsule_env_bindings_detects_conflicts() {
    use crate::common::paths::AtoRunLayout;

    let conflict_env = "ATO_TOOL_SHARED";
    let mut tool_capsules = BTreeMap::new();
    tool_capsules.insert(
        "alpha".to_string(),
        LockedToolCapsule {
            resolved: "capsule://ato.run/tools/alpha@1.0.0".to_string(),
            platform: "darwin-arm64".to_string(),
            artifact_hash: "sha256:a".to_string(),
            artifact_url: None,
            exports: LockedToolExports {
                binaries: BTreeMap::from([("foo".to_string(), "bin/foo".to_string())]),
                paths: BTreeMap::new(),
            },
            bind_env: BTreeMap::from([("foo".to_string(), conflict_env.to_string())]),
        },
    );
    tool_capsules.insert(
        "beta".to_string(),
        LockedToolCapsule {
            resolved: "capsule://ato.run/tools/beta@1.0.0".to_string(),
            platform: "darwin-arm64".to_string(),
            artifact_hash: "sha256:b".to_string(),
            artifact_url: None,
            exports: LockedToolExports {
                binaries: BTreeMap::from([("bar".to_string(), "bin/bar".to_string())]),
                paths: BTreeMap::new(),
            },
            bind_env: BTreeMap::from([("bar".to_string(), conflict_env.to_string())]),
        },
    );
    let lock = empty_lockfile_with_tool_capsules(tool_capsules);
    let layout = AtoRunLayout::for_root(std::path::PathBuf::from("/runs/run-1"));

    let err = tool_capsule_env_bindings(&lock, &layout).expect_err("expected conflict");
    assert_eq!(err.env_name, conflict_env);
    assert!(matches!(err.first_alias.as_str(), "alpha" | "beta"));
    assert!(matches!(err.second_alias.as_str(), "alpha" | "beta"));
    assert_ne!(err.first_alias, err.second_alias);
}

#[test]
fn lockfile_accepts_legacy_lockfile_without_tool_capsules() {
    // Legacy lockfile JSON written before the tool_capsules field existed.
    let legacy = r#"{
        "version": "1",
        "meta": { "created_at": "2026-01-01T00:00:00Z", "manifest_hash": "sha256:deadbeef" },
        "capsule_dependencies": [],
        "targets": {}
    }"#;
    let parsed: CapsuleLock = serde_json::from_str(legacy).expect("parse legacy lockfile");
    assert!(parsed.tool_capsules.is_empty());
}
