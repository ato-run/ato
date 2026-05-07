use super::github_inference::{
    auto_fix_github_install_preview_toml, reassign_github_install_preview_toml_port,
};
use super::*;
#[cfg(target_os = "macos")]
use crate::application::producer_input::resolve_producer_authoritative_input;
#[cfg(target_os = "macos")]
use crate::publish_ci::build_capsule_artifact as build_publish_capsule_artifact;
#[cfg(target_os = "macos")]
use crate::reporters::CliReporter;
use filetime::{set_file_mtime, FileTime};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use axum::extract::{Path as AxumPath, State};
use axum::http::{header::HOST, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::{Signer as _, SigningKey};
use tokio::sync::Mutex;

const TEST_SCOPED_ID: &str = "koh0920/sample";
const TEST_VERSION: &str = "1.0.0";
const TEST_LEASE_ID: &str = "lease-test-1";

fn assert_json_object_has_keys(value: &serde_json::Value, keys: &[&str]) {
    let object = value.as_object().expect("expected JSON object");
    for key in keys {
        assert!(
            object.contains_key(*key),
            "expected key '{}' in JSON object: {object:?}",
            key
        );
    }
}

fn test_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn acquire_test_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    test_env_lock().lock().await
}

struct EnvVarGuard {
    key: String,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(&self.key, value);
        } else {
            std::env::remove_var(&self.key);
        }
    }
}

#[test]
fn normalize_github_install_preview_toml_maps_legacy_python_driver_and_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("pyproject.toml"),
        r#"[project]
version = "0.1.0"
requires-python = ">=3.12"
"#,
    )
    .expect("write pyproject");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/pip"
run = "main.py""#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#"runtime = "source/python""#));
    assert!(normalized.contains(r#"runtime_version = "3.12.0""#));
}

#[test]
fn normalize_github_install_preview_toml_maps_native_tooling_drivers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/cargo"
run = "src/main.rs""#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#"runtime = "source/native""#));
    assert!(!normalized.contains("runtime_version"));
}

#[test]
fn normalize_github_install_preview_toml_adds_runtime_version_to_v03_node_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{
  "engines": { "node": ">=20" }
}"#,
    )
    .expect("write package.json");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node server.js"
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#"schema_version = "0.3""#));
    assert!(normalized.contains(r#"runtime_version = "20.19.0""#));
    assert!(normalized.contains(r#"runtime = "source/node""#));
}

#[test]
fn normalize_github_install_preview_toml_includes_deno_import_map() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("deno.json"),
        r#"{
  "importMap": "./import_map.json",
  "tasks": {
    "start": "deno run --allow-net main.ts"
  }
}"#,
    )
    .expect("write deno.json");
    std::fs::write(tmp.path().join("import_map.json"), "{}").expect("write import_map.json");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/deno"
run = "deno task start"

[pack]
include = ["main.ts", "deno.json", "deno.lock"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#""import_map.json""#));
}

#[test]
fn normalize_github_install_preview_toml_rewrites_node_typescript_entrypoint_to_build_output() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{
  "packageManager": "pnpm@10.15.0",
  "bin": {
    "json-server": "lib/bin.js"
  },
    "files": ["lib", "views", "schema.json"],
  "scripts": {
    "build": "rm -rf lib && tsc"
  }
}"#,
    )
    .expect("write package.json");
    std::fs::write(
        tmp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .expect("write pnpm lock");
    let manifest = r#"
schema_version = "0.3"
name = "json-server"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/bin.ts fixtures/db.json"

[pack]
include = ["src/**", "fixtures/db.json", "package.json", "pnpm-lock.yaml"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains("build = \"pnpm run build\""));
    assert!(normalized.contains("run = \"node lib/bin.js fixtures/db.json\""));
    assert!(normalized.contains("\"lib/**\""));
    assert!(normalized.contains("\"schema.json\""));
}

#[test]
fn normalize_github_install_preview_toml_rewrites_tsx_run_to_dev_script_for_vite_app() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"scripts": {"dev": "vite"}}"#,
    )
    .expect("write package.json");
    std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write lock");
    let manifest = r#"
schema_version = "0.3"
name = "my-vite-app"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/main.tsx"

[pack]
include = ["src/**", "package.json", "package-lock.json"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(
        normalized.contains("run = \"npm run dev\""),
        "expected 'npm run dev' but got: {normalized}"
    );
}

#[test]
fn normalize_github_install_preview_toml_rewrites_jsx_run_to_dev_script() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"scripts": {"dev": "vite"}}"#,
    )
    .expect("write package.json");
    std::fs::write(
        tmp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .expect("write pnpm lock");
    let manifest = r#"
schema_version = "0.3"
name = "my-react-app"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/main.jsx"

[pack]
include = ["src/**", "package.json", "pnpm-lock.yaml"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(
        normalized.contains("run = \"pnpm run dev\""),
        "expected 'pnpm run dev' but got: {normalized}"
    );
}

#[test]
fn normalize_github_install_preview_toml_rewrites_ts_without_bin_to_dev_script() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No "bin" field — this is a dev-server app, not a CLI tool
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"scripts": {"dev": "react-scripts start", "build": "react-scripts build"}}"#,
    )
    .expect("write package.json");
    std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write lock");
    let manifest = r#"
schema_version = "0.3"
name = "react-player-demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/index.ts"

[pack]
include = ["src/**", "package.json", "package-lock.json"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(
        normalized.contains("run = \"npm run dev\""),
        "expected 'npm run dev' but got: {normalized}"
    );
}

#[test]
fn normalize_github_install_preview_toml_does_not_rewrite_tsx_if_no_dev_script() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"scripts": {"start": "node dist/server.js"}}"#,
    )
    .expect("write package.json");
    std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write lock");
    let manifest = r#"
schema_version = "0.3"
name = "my-app"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/main.tsx"

[pack]
include = ["src/**", "package.json", "package-lock.json"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    // Falls back to the start script when dev is missing.
    assert!(
        normalized.contains("run = \"npm run start\""),
        "expected run to be rewritten to the start script but got: {normalized}"
    );
}

#[test]
fn normalize_github_install_preview_toml_rewrites_astro_run_to_dev_script() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"scripts": {"dev": "astro dev"}}"#,
    )
    .expect("write package.json");
    std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write lock");
    let manifest = r#"
schema_version = "0.3"
name = "my-astro-site"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/pages/index.astro"

[pack]
include = ["src/**", "package.json", "package-lock.json"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(
        normalized.contains("run = \"npm run dev\""),
        "expected 'npm run dev' but got: {normalized}"
    );
}

#[test]
fn normalize_github_install_preview_toml_collapses_legacy_env_required() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "uv run app.py"
required_env = ["DATABASE_URL"]

[env]
required = ["REDIS_URL"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#"required_env = ["DATABASE_URL", "REDIS_URL"]"#));
    assert!(!normalized.contains("[env]"));
    assert!(!normalized.contains("required = ["));
}

#[test]
fn normalize_github_install_preview_toml_does_not_force_default_port_for_web_static() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "index.html"
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(!normalized.contains("port = "));
}

#[test]
fn auto_fix_github_install_preview_toml_assigns_ato_port_for_web_static() {
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "index.html"
"#;

    let fixed = auto_fix_github_install_preview_toml(manifest).expect("auto-fix");
    let parsed = fixed.parse::<toml::Value>().expect("parse fixed toml");
    let port = parsed
        .get("port")
        .and_then(toml::Value::as_integer)
        .expect("port");

    assert!((18000..=18999).contains(&port));
}

#[test]
fn reassign_github_install_preview_toml_port_replaces_existing_web_port() {
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "index.html"
port = 3000
"#;

    let fixed = reassign_github_install_preview_toml_port(manifest).expect("reassign");
    let parsed = fixed.parse::<toml::Value>().expect("parse fixed toml");
    let port = parsed
        .get("port")
        .and_then(toml::Value::as_integer)
        .expect("port");

    assert!((18000..=18999).contains(&port));
    assert_ne!(port, 3000);
}

#[test]
fn normalize_github_install_preview_toml_accepts_root_pnpm_lockfile_include() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
        .expect("write pnpm lock");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
run = "pnpm dev"
[pack]
include = ["package.json", "pnpm-lock.yaml", "src/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#""pnpm-lock.yaml""#));
}

#[test]
fn normalize_github_install_preview_toml_accepts_subdir_pnpm_lockfile_include() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app_dir = tmp.path().join("apps").join("web");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
        .expect("write pnpm lock");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
working_dir = "apps/web"
run = "pnpm dev"
[pack]
include = ["apps/web/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#"working_dir = "apps/web""#));
}

#[test]
fn normalize_github_install_preview_toml_auto_adds_subdir_lockfile_to_pack_include() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app_dir = tmp.path().join("apps").join("web");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
        .expect("write pnpm lock");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
working_dir = "apps/web"
run = "pnpm dev"
[pack]
include = ["package.json", "src/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(normalized.contains(r#""apps/web/pnpm-lock.yaml""#));
}

#[test]
fn normalize_github_install_preview_toml_resolves_multiple_lockfiles_by_priority_pnpm_over_yarn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app_dir = tmp.path().join("apps").join("web");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
        .expect("write pnpm lock");
    std::fs::write(app_dir.join("yarn.lock"), "__metadata:\n  version: 4")
        .expect("write yarn lock");
    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
working_dir = "apps/web"
run = "pnpm dev"
[pack]
include = ["package.json", "src/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    // pnpm wins by priority (pnpm > yarn > bun > npm)
    assert!(
        normalized.contains(r#""apps/web/pnpm-lock.yaml""#),
        "pnpm-lock.yaml must be auto-included by priority"
    );
    assert!(
        !normalized.contains("yarn.lock"),
        "yarn.lock must not be included when pnpm-lock.yaml takes priority"
    );
}

#[test]
fn normalize_github_install_preview_toml_resolves_multiple_lockfiles_via_package_manager_field() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app_dir = tmp.path().join("apps").join("web");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(app_dir.join("package-lock.json"), "{\"lockfileVersion\":3}")
        .expect("write npm lock");
    std::fs::write(app_dir.join("bun.lock"), "# bun lockfile v0\n").expect("write bun lock");
    // package.json explicitly declares bun as the package manager
    std::fs::write(
        app_dir.join("package.json"),
        r#"{"name":"demo","packageManager":"bun@1.1.0"}"#,
    )
    .expect("write package.json");

    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
working_dir = "apps/web"
run = "bun dev"
[pack]
include = ["apps/web/package.json", "apps/web/src/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    assert!(
        normalized.contains(r#""apps/web/bun.lock""#),
        "bun.lock must be auto-included"
    );
    assert!(
        !normalized.contains("package-lock.json"),
        "package-lock.json must not be included when bun is declared"
    );
}

#[test]
fn normalize_github_install_preview_toml_resolves_multiple_lockfiles_by_priority_when_no_package_manager_field(
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("package-lock.json"),
        "{\"lockfileVersion\":3}",
    )
    .expect("write npm lock");
    std::fs::write(tmp.path().join("bun.lock"), "# bun lockfile v0\n").expect("write bun lock");
    // package.json without packageManager field — bun.lock wins by priority (bun > npm)
    std::fs::write(tmp.path().join("package.json"), r#"{"name":"demo"}"#)
        .expect("write package.json");

    let manifest = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
run = "node src/index.js"
[pack]
include = ["package.json", "src/**"]
"#;

    let normalized =
        normalize_github_install_preview_toml(tmp.path(), manifest).expect("normalize");

    // bun.lock wins by priority; package-lock.json must NOT be added
    assert!(
        normalized.contains(r#""bun.lock""#),
        "bun.lock must be auto-included by priority"
    );
    assert!(
        !normalized.contains("package-lock.json"),
        "package-lock.json must not be included when bun.lock takes priority"
    );
}

#[test]
fn github_checkout_root_is_outside_workspace_internal_subtree() {
    use capsule_core::common::paths::path_contains_workspace_internal_subtree;
    let root = super::github_checkout_root().expect("checkout root");
    assert!(
        !path_contains_workspace_internal_subtree(&root),
        "github_checkout_root() must not be inside a workspace internal subtree; got: {}",
        root.display()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_checkout_root_uses_current_ato_home_tmp_root() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));

    let root = github_run_checkout_root().expect("gh-run root");
    assert_eq!(root, ato_home.join("tmp").join("gh-run"));
    assert!(root.exists());
}

#[test]
fn github_run_success_cleanup_removes_transient_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(checkout.join("src")).expect("mkdir checkout");
    std::fs::write(checkout.join("src/main.js"), b"console.log('ok');").expect("write file");

    remove_github_run_checkout(&checkout).expect("remove checkout");

    assert!(!checkout.exists());
    remove_github_run_checkout(&checkout).expect("remove remains idempotent");
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_sweep_removes_stale_tree_older_than_ttl() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let stale = root.join("stale-checkout");
    std::fs::create_dir_all(&stale).expect("mkdir stale checkout");

    let removed = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now() + Duration::from_secs(48 * 60 * 60),
        Duration::from_secs(24 * 60 * 60),
    )
    .expect("sweep stale checkouts");

    assert_eq!(removed, 1);
    assert!(!stale.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_sweep_preserves_fresh_tree_within_grace_period() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let fresh = root.join("fresh-checkout");
    std::fs::create_dir_all(&fresh).expect("mkdir fresh checkout");

    let removed = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now() + Duration::from_secs(60),
        Duration::from_secs(24 * 60 * 60),
    )
    .expect("sweep fresh checkouts");

    assert_eq!(removed, 0);
    assert!(fresh.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_sweep_preserves_checkout_with_live_owner_marker() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let active = root.join("active-owner-checkout");
    std::fs::create_dir_all(&active).expect("mkdir active checkout");
    write_github_run_checkout_owner_marker(&active).expect("write owner marker");

    let removed = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now() + Duration::from_secs(48 * 60 * 60),
        Duration::from_secs(24 * 60 * 60),
    )
    .expect("sweep owner-preserved checkout");

    assert_eq!(removed, 0);
    assert!(active.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_sweep_preserves_checkout_referenced_by_active_process() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let active = root.join("active-process-checkout");
    let manifest_path = active.join("nested").join("capsule.toml");
    std::fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
        .expect("mkdir active checkout");
    std::fs::write(&manifest_path, "schema_version = \"0.3\"\n").expect("write manifest");

    let process_manager = crate::runtime::process::ProcessManager::new().expect("process manager");
    process_manager
        .write_pid(&crate::runtime::process::ProcessInfo {
            id: "gh-run-live".to_string(),
            name: "gh-run-live".to_string(),
            pid: std::process::id() as i32,
            workload_pid: None,
            status: crate::runtime::process::ProcessStatus::Ready,
            runtime: "shell".to_string(),
            start_time: SystemTime::now(),
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: Some(manifest_path),
            scoped_id: None,
            target_label: None,
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        })
        .expect("write active process record");

    let removed = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now() + Duration::from_secs(48 * 60 * 60),
        Duration::from_secs(24 * 60 * 60),
    )
    .expect("sweep active-process checkout");

    assert_eq!(removed, 0);
    assert!(active.exists());
    process_manager
        .delete_pid("gh-run-live")
        .expect("cleanup pid record");
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_sweep_does_not_preserve_owner_marker_with_mismatched_start_time() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let stale = root.join("stale-reused-pid-checkout");
    std::fs::create_dir_all(&stale).expect("mkdir stale checkout");
    set_file_mtime(&stale, FileTime::from_unix_time(1, 0)).expect("age stale checkout");

    let bogus_owner = serde_json::json!({
        "owner_pid": std::process::id(),
        "owner_start_time_unix_ms": 1_u64,
    });
    std::fs::write(
        stale.join(".ato-owner.json"),
        serde_json::to_vec_pretty(&bogus_owner).expect("serialize bogus owner marker"),
    )
    .expect("write bogus owner marker");

    let removed = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now() + Duration::from_secs(48 * 60 * 60),
        Duration::from_secs(24 * 60 * 60),
    )
    .expect("sweep reused-pid checkout");

    assert_eq!(removed, 1);
    assert!(!stale.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn github_run_owner_marker_requires_matching_start_time() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ato_home = temp.path().join("ato-home");
    let _ato_home_guard = EnvVarGuard::set("ATO_HOME", Some(ato_home.to_string_lossy().as_ref()));
    let root = github_run_checkout_root().expect("gh-run root");
    let active = root.join("active-checkout");
    let reused = root.join("reused-checkout");
    std::fs::create_dir_all(&active).expect("mkdir active checkout");
    std::fs::create_dir_all(&reused).expect("mkdir reused checkout");
    write_github_run_checkout_owner_marker(&active).expect("write owner marker");
    std::fs::write(
        reused.join(".ato-owner.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "owner_pid": std::process::id(),
            "owner_start_time_unix_ms": 1_u64,
        }))
        .expect("serialize bogus owner marker"),
    )
    .expect("write bogus owner marker");

    assert!(github_run_checkout_owner_is_alive(&active));
    assert!(!github_run_checkout_owner_is_alive(&reused));
}

#[test]
fn native_install_documented_json_contract_fields_are_present() {
    let value = serde_json::to_value(InstallResult {
        capsule_id: "capsule-123".to_string(),
        scoped_id: "koh0920/sample".to_string(),
        publisher: "koh0920".to_string(),
        slug: "sample".to_string(),
        version: "1.0.0".to_string(),
        path: PathBuf::from("/tmp/sample.capsule"),
        content_hash: "blake3:artifact".to_string(),
        install_kind: InstallKind::NativeRequiresLocalDerivation,
        launchable: Some(LaunchableTarget::DerivedApp {
            path: PathBuf::from("/tmp/MyApp.app"),
        }),
        local_derivation: Some(LocalDerivationInfo {
            schema_version: "0.1".to_string(),
            performed: true,
            fetched_dir: PathBuf::from("/tmp/fetch"),
            derived_app_path: Some(PathBuf::from("/tmp/MyApp.app")),
            provenance_path: Some(PathBuf::from("/tmp/local-derivation.json")),
            parent_digest: Some("blake3:parent".to_string()),
            derived_digest: Some("blake3:derived".to_string()),
        }),
        projection: Some(ProjectionInfo {
            performed: true,
            projection_id: Some("projection-123".to_string()),
            projected_path: Some(PathBuf::from("/Applications/MyApp.app")),
            state: Some("ok".to_string()),
            schema_version: Some("0.1".to_string()),
            metadata_path: Some(PathBuf::from("/tmp/projection.json")),
        }),
        managed_environment: Some(ManagedEnvironmentInfo {
            strategy: "ato-managed".to_string(),
            target: Some("desktop".to_string()),
            services: vec!["ollama".to_string(), "opencode".to_string()],
            materialized_root: PathBuf::from("/tmp/desky-services"),
            bootstrap_state_path: PathBuf::from("/tmp/desky-bootstrap-state.json"),
            bootstrap_phase: "shell_projected".to_string(),
        }),
        promotion: Some(PromotionInfo {
            performed: true,
            preview_id: Some("preview-123".to_string()),
            source_reference: Some("github.com/octocat/hello-world".to_string()),
            source_metadata_path: Some(PathBuf::from("/tmp/preview/metadata.json")),
            source_manifest_path: Some(PathBuf::from("/tmp/preview/capsule.toml")),
            manifest_source: Some("inferred".to_string()),
            inference_mode: Some("rules".to_string()),
            resolved_ref: Some(GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "abc123".to_string(),
            }),
            derived_plan: Some(PromotionDerivedPlanSnapshot {
                runtime: Some("source".to_string()),
                driver: Some("python".to_string()),
                resolved_runtime_version: Some("3.11.10".to_string()),
                resolved_port: Some(8000),
                resolved_lock_files: vec![PathBuf::from("uv.lock")],
                resolved_pack_include: vec!["src/**".to_string()],
                warnings: vec!["generated lockfile".to_string()],
                deferred_constraints: vec!["author must commit uv.lock".to_string()],
                promotion_eligibility: "eligible".to_string(),
            }),
            promotion_metadata_path: Some(PathBuf::from("/tmp/install/promotion.json")),
            content_hash: Some("blake3:artifact".to_string()),
        }),
    })
    .expect("serialize install result");

    assert_json_object_has_keys(
        &value,
        &[
            "install_kind",
            "launchable",
            "local_derivation",
            "projection",
            "managed_environment",
            "promotion",
        ],
    );

    assert_json_object_has_keys(
        &value["local_derivation"],
        &[
            "schema_version",
            "provenance_path",
            "parent_digest",
            "derived_digest",
        ],
    );

    assert_json_object_has_keys(
        &value["projection"],
        &["schema_version", "metadata_path", "state"],
    );

    assert_json_object_has_keys(
        &value["managed_environment"],
        &[
            "strategy",
            "services",
            "materialized_root",
            "bootstrap_state_path",
            "bootstrap_phase",
        ],
    );

    assert_json_object_has_keys(
        &value["promotion"],
        &[
            "preview_id",
            "source_reference",
            "derived_plan",
            "promotion_metadata_path",
            "content_hash",
        ],
    );

    assert_json_object_has_keys(
        &value["promotion"]["derived_plan"],
        &[
            "runtime",
            "driver",
            "resolved_lock_files",
            "promotion_eligibility",
        ],
    );
}

#[tokio::test]
#[serial_test::serial]
async fn native_install_materializes_ato_managed_environment_bootstrap_state() {
    let _env_lock = acquire_test_env_lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let state_path = temp.path().join("bootstrap-state.json");
    let _state_path_guard = EnvVarGuard::set(
        "DESKY_BOOTSTRAP_STATE_PATH",
        Some(state_path.to_string_lossy().as_ref()),
    );
    let _path_guard = EnvVarGuard::set("PATH", Some(temp.path().to_string_lossy().as_ref()));

    let payload = b"placeholder-payload";
    let lock_json = serde_json::json!({
        "schema_version": 1,
        "contract": {
            "delivery": {
                "mode": "artifact-import",
                "artifact": {
                    "kind": "desktop-native",
                    "artifact_type": "app-bundle",
                    "digest": "sha256:abc",
                    "canonical_build_input": false,
                    "provenance_limited": true
                },
                "install": {
                    "environment": {
                        "strategy": "ato-managed",
                        "target": "desktop",
                        "services": [
                            {
                                "name": "ollama",
                                "from": "dependency:ollama",
                                "lifecycle": "managed"
                            },
                            {
                                "name": "opencode",
                                "from": "dependency:opencode",
                                "lifecycle": "on-demand",
                                "depends_on": ["ollama"]
                            }
                        ],
                        "bootstrap": {
                            "requires_personalization": true,
                            "model_tiers": ["fast", "balanced", "fallback"]
                        },
                        "repair": {
                            "actions": ["restart-services", "rewrite-config", "switch-model-tier"]
                        }
                    }
                },
                "projection": {}
            }
        }
    })
    .to_string();
    let artifact = build_capsule_artifact(None, Some(&lock_json), payload).expect("artifact");

    let managed = materialize_ato_managed_environment("ato/ato-desktop", &artifact, true, true)
        .expect("materialize managed environment")
        .expect("managed environment");

    assert_eq!(managed.strategy, "ato-managed");
    assert_eq!(managed.target.as_deref(), Some("desktop"));
    assert_eq!(managed.bootstrap_phase, "shell_projected");
    assert!(managed.services.iter().any(|service| service == "opencode"));
    assert!(managed.materialized_root.exists());
    assert!(managed
        .materialized_root
        .join("ollama")
        .join("service.json")
        .exists());
    assert!(managed
        .materialized_root
        .join("opencode")
        .join("run.sh")
        .exists());

    let raw = std::fs::read_to_string(&state_path).expect("read bootstrap state");
    let state: crate::app_control::StoredBootstrapState =
        serde_json::from_str(&raw).expect("parse bootstrap state");
    assert!(state.materialization.shell_installed);
    assert!(!state.materialization.opencode_installed);
    assert_eq!(state.materialization.bootstrap_phase, "shell_projected");
    assert_eq!(state.materialization.ollama_mode, "missing");
    assert_eq!(state.health.overall, "degraded");
    assert_eq!(
        state.health.services.get("ollama").map(String::as_str),
        Some("missing")
    );
    assert_eq!(
        state.health.services.get("opencode").map(String::as_str),
        Some("missing")
    );
}

#[test]
fn persist_promotion_info_writes_snapshot_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_path = temp.path().join("sample-1.0.0.capsule");
    std::fs::write(&artifact_path, b"capsule").expect("write artifact");

    let promotion = persist_promotion_info(
        &artifact_path,
        Some(&PromotionSourceInfo {
            preview_id: "preview-123".to_string(),
            source_reference: "github.com/octocat/hello-world".to_string(),
            source_metadata_path: PathBuf::from("/tmp/preview/metadata.json"),
            source_manifest_path: PathBuf::from("/tmp/preview/capsule.toml"),
            manifest_source: Some("inferred".to_string()),
            inference_mode: Some("rules".to_string()),
            resolved_ref: Some(GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "abc123".to_string(),
            }),
            derived_plan: PromotionDerivedPlanSnapshot {
                runtime: Some("source".to_string()),
                driver: Some("python".to_string()),
                resolved_runtime_version: Some("3.11.10".to_string()),
                resolved_port: Some(8000),
                resolved_lock_files: vec![PathBuf::from("uv.lock")],
                resolved_pack_include: vec!["src/**".to_string()],
                warnings: vec!["generated lockfile".to_string()],
                deferred_constraints: vec!["author must commit uv.lock".to_string()],
                promotion_eligibility: "eligible".to_string(),
            },
        }),
        "blake3:artifact",
    )
    .expect("persist promotion")
    .expect("promotion info");

    let metadata_path = temp.path().join("promotion.json");
    assert_eq!(
        promotion.promotion_metadata_path.as_deref(),
        Some(metadata_path.as_path())
    );
    assert!(metadata_path.exists());

    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).expect("read promotion metadata"))
            .expect("parse promotion metadata");
    assert_eq!(value["preview_id"], "preview-123");
    assert_eq!(value["manifest_source"], "inferred");
    assert_eq!(value["derived_plan"]["resolved_port"], 8000);
    assert_eq!(value["derived_plan"]["promotion_eligibility"], "eligible");
}

fn test_scoped_ref() -> ScopedCapsuleRef {
    parse_capsule_ref(TEST_SCOPED_ID).expect("valid scoped ref")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockScenario {
    FalsePositiveRecovery,
    MissingChunksAfterRetryFallback,
    FallbackNotImplemented,
    ManifestApiNotFound,
    ArtifactRejectsAuthorization,
    UnauthorizedManifest,
    LeaseReleaseOnFailure,
    YankedNegotiate,
    YankedManifest,
}

#[derive(Debug, Clone)]
struct MockRegistryFixture {
    scoped_id: String,
    publisher: String,
    slug: String,
    version: String,
    manifest_hash: String,
    manifest_toml: String,
    payload_tar: Vec<u8>,
    artifact_bytes: Vec<u8>,
    chunk_hashes: Vec<String>,
    chunk_bytes: HashMap<String, Vec<u8>>,
    lease_id: String,
    epoch_response: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
struct RecordedNegotiateRequest {
    has_bloom: bool,
    have_chunks_len: usize,
    reuse_lease_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct MockObservations {
    epoch_calls: usize,
    version_resolve_calls: usize,
    manifest_calls: usize,
    negotiate_calls: Vec<RecordedNegotiateRequest>,
    chunk_calls: Vec<String>,
    distribution_calls: usize,
    artifact_calls: usize,
    release_calls: Vec<String>,
}

#[derive(Debug)]
struct MockRegistryState {
    scenario: MockScenario,
    fixture: MockRegistryFixture,
    observations: MockObservations,
}

type SharedMockState = std::sync::Arc<Mutex<MockRegistryState>>;

struct MockRegistryHandle {
    base_url: String,
    state: SharedMockState,
    task: tokio::task::JoinHandle<()>,
}

impl MockRegistryHandle {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn observations(&self) -> MockObservations {
        self.state.lock().await.observations.clone()
    }
}

impl Drop for MockRegistryHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn compute_merkle_root_for_test(chunk_hashes: &[String]) -> String {
    let mut level: Vec<[u8; 32]> = chunk_hashes
        .iter()
        .map(|chunk_hash| {
            let normalized = normalize_hash_for_compare(chunk_hash);
            let bytes = hex::decode(normalized).expect("hex decode");
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            out
        })
        .collect();
    if level.is_empty() {
        return format!("blake3:{}", blake3::hash(b"").to_hex());
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut idx = 0usize;
        while idx < level.len() {
            let left = level[idx];
            let right = if idx + 1 < level.len() {
                level[idx + 1]
            } else {
                level[idx]
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(&left);
            hasher.update(&right);
            next.push(*hasher.finalize().as_bytes());
            idx += 2;
        }
        level = next;
    }
    format!("blake3:{}", hex::encode(level[0]))
}

fn build_mock_fixture(scoped_id: &str, version: &str, chunks: Vec<Vec<u8>>) -> MockRegistryFixture {
    let (publisher, slug) = scoped_id
        .split_once('/')
        .expect("scoped_id must be publisher/slug");

    let mut chunk_hashes = Vec::new();
    let mut chunk_list = Vec::new();
    let mut chunk_bytes = HashMap::new();
    let mut payload_tar = Vec::new();
    let mut offset = 0u64;
    for bytes in chunks {
        let chunk_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        chunk_hashes.push(chunk_hash.clone());
        chunk_bytes.insert(chunk_hash.clone(), bytes.clone());
        chunk_list.push(capsule_core::types::ChunkDescriptor {
            chunk_hash,
            offset,
            length: bytes.len() as u64,
            codec: "fastcdc".to_string(),
            compression: "none".to_string(),
        });
        payload_tar.extend_from_slice(&bytes);
        offset += bytes.len() as u64;
    }
    let merkle_root = compute_merkle_root_for_test(&chunk_hashes);
    let mut manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

runtime = "source"
run = "main.py""#,
    )
    .expect("manifest");
    manifest.distribution = Some(capsule_core::types::DistributionInfo {
        manifest_hash: String::new(),
        merkle_root,
        chunk_list,
        signatures: vec![],
    });
    let manifest_hash = compute_manifest_hash_without_signatures(&manifest).expect("manifest hash");
    manifest
        .distribution
        .as_mut()
        .expect("distribution")
        .manifest_hash = manifest_hash.clone();
    let manifest_toml = toml::to_string_pretty(&manifest).expect("manifest TOML");

    let payload_tar_zst = {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), DELTA_RECONSTRUCT_ZSTD_LEVEL)
            .expect("zstd encoder");
        encoder
            .write_all(&payload_tar)
            .expect("write payload tar bytes");
        encoder.finish().expect("finish zstd stream")
    };
    let artifact_bytes = build_capsule_artifact(Some(&manifest_toml), None, &payload_tar_zst)
        .expect("build artifact");

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let verifying_key = signing_key.verifying_key();
    let signer_did = public_key_to_did(&verifying_key.to_bytes());
    let issued_at = "2026-03-05T00:00:00Z";
    let key_id = "k-main";
    let unsigned_pointer = serde_json::json!({
        "scoped_id": scoped_id,
        "epoch": 1u64,
        "manifest_hash": manifest_hash,
        "prev_epoch_hash": serde_json::Value::Null,
        "issued_at": issued_at,
        "signer_did": signer_did,
        "key_id": key_id,
    });
    let canonical_pointer = serde_jcs::to_vec(&unsigned_pointer).expect("canonical pointer");
    let signature = signing_key.sign(&canonical_pointer);
    let epoch_response = serde_json::json!({
        "pointer": {
            "scoped_id": scoped_id,
            "epoch": 1u64,
            "manifest_hash": manifest_hash,
            "prev_epoch_hash": serde_json::Value::Null,
            "issued_at": issued_at,
            "signer_did": signer_did,
            "key_id": key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        },
        "public_key": BASE64.encode(verifying_key.to_bytes()),
    });

    MockRegistryFixture {
        scoped_id: scoped_id.to_string(),
        publisher: publisher.to_string(),
        slug: slug.to_string(),
        version: version.to_string(),
        manifest_hash,
        manifest_toml,
        payload_tar,
        artifact_bytes,
        chunk_hashes,
        chunk_bytes,
        lease_id: TEST_LEASE_ID.to_string(),
        epoch_response,
    }
}

fn build_payload_tar_with_source(path: &str, source: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut payload);
        let mut header = tar::Header::new_gnu();
        header.set_path(path).expect("set payload path");
        header.set_mode(0o644);
        header.set_size(source.len() as u64);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(source))
            .expect("append payload source");
        builder.finish().expect("finish payload tar");
    }
    payload
}

async fn spawn_mock_registry(
    scenario: MockScenario,
    fixture: MockRegistryFixture,
) -> MockRegistryHandle {
    let state = std::sync::Arc::new(Mutex::new(MockRegistryState {
        scenario,
        fixture,
        observations: MockObservations::default(),
    }));
    let app = Router::new()
        .route("/v1/manifest/epoch/resolve", post(mock_epoch_resolve))
        .route(
            "/v1/manifest/resolve/:publisher/:slug/:version",
            get(mock_version_resolve),
        )
        .route("/v1/manifest/documents/:manifest_hash", get(mock_manifest))
        .route("/v1/manifest/negotiate", post(mock_negotiate))
        .route("/v1/manifest/chunks/:chunk_hash", get(mock_chunk))
        .route("/v1/manifest/leases/refresh", post(mock_lease_refresh))
        .route("/v1/manifest/leases/release", post(mock_lease_release))
        .route("/v1/capsules/by/:publisher/:slug", get(mock_capsule_detail))
        .route(
            "/v1/capsules/by/:publisher/:slug/distributions",
            get(mock_distribution),
        )
        .route("/mock/artifact.capsule", get(mock_artifact))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock registry");
    let addr = listener.local_addr().expect("mock registry local addr");
    let task = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("mock registry server error: {}", err);
        }
    });

    MockRegistryHandle {
        base_url: format!("http://{}", addr),
        state,
        task,
    }
}

async fn mock_epoch_resolve(State(state): State<SharedMockState>) -> Response {
    let mut guard = state.lock().await;
    guard.observations.epoch_calls += 1;
    match guard.scenario {
        MockScenario::UnauthorizedManifest => StatusCode::UNAUTHORIZED.into_response(),
        MockScenario::FallbackNotImplemented if guard.observations.epoch_calls >= 2 => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        _ => Json(guard.fixture.epoch_response.clone()).into_response(),
    }
}

async fn mock_version_resolve(
    State(state): State<SharedMockState>,
    AxumPath((publisher, slug, version)): AxumPath<(String, String, String)>,
) -> Response {
    let mut guard = state.lock().await;
    guard.observations.version_resolve_calls += 1;
    if publisher != guard.fixture.publisher
        || slug != guard.fixture.slug
        || version != guard.fixture.version
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if matches!(
        guard.scenario,
        MockScenario::ManifestApiNotFound | MockScenario::ArtifactRejectsAuthorization
    ) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "not_found",
                "message": "Endpoint not found"
            })),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "scoped_id": guard.fixture.scoped_id,
        "version": guard.fixture.version,
        "manifest_hash": guard.fixture.manifest_hash,
        "yanked_at": serde_json::Value::Null,
    }))
    .into_response()
}

async fn mock_manifest(
    State(state): State<SharedMockState>,
    AxumPath(manifest_hash): AxumPath<String>,
) -> Response {
    let mut guard = state.lock().await;
    guard.observations.manifest_calls += 1;
    if guard.scenario == MockScenario::UnauthorizedManifest {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if guard.scenario == MockScenario::YankedManifest {
        return (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "manifest_yanked",
                "message": "Manifest has been yanked by the publisher.",
                "yanked": true
            })),
        )
            .into_response();
    }
    if normalize_hash_for_compare(&manifest_hash)
        != normalize_hash_for_compare(&guard.fixture.manifest_hash)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    (StatusCode::OK, guard.fixture.manifest_toml.clone()).into_response()
}

async fn mock_negotiate(
    State(state): State<SharedMockState>,
    Json(request): Json<ManifestNegotiateRequest>,
) -> Response {
    let mut guard = state.lock().await;
    guard
        .observations
        .negotiate_calls
        .push(RecordedNegotiateRequest {
            has_bloom: request.have_chunks_bloom.is_some(),
            have_chunks_len: request.have_chunks.len(),
            reuse_lease_id: request.reuse_lease_id.clone(),
        });
    let call_index = guard.observations.negotiate_calls.len();
    match guard.scenario {
        MockScenario::FallbackNotImplemented => StatusCode::NOT_IMPLEMENTED.into_response(),
        MockScenario::ManifestApiNotFound | MockScenario::ArtifactRejectsAuthorization => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "not_found",
                "message": "Endpoint not found"
            })),
        )
            .into_response(),
        MockScenario::UnauthorizedManifest => StatusCode::UNAUTHORIZED.into_response(),
        MockScenario::YankedNegotiate => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "manifest_yanked",
                "message": "Manifest has been yanked by the publisher.",
                "yanked": true
            })),
        )
            .into_response(),
        MockScenario::YankedManifest => Json(serde_json::json!({
            "session_id": format!("session-{}", call_index),
            "required_chunks": [],
            "required_manifests": [],
            "lease_id": guard.fixture.lease_id,
            "lease_expires_at": "2026-03-05T00:15:00Z",
        }))
        .into_response(),
        MockScenario::LeaseReleaseOnFailure => Json(serde_json::json!({
            "session_id": format!("session-{}", call_index),
            "required_chunks": [guard.fixture.chunk_hashes[0].clone()],
            "required_manifests": [],
            "lease_id": guard.fixture.lease_id,
            "lease_expires_at": "2026-03-05T00:15:00Z",
        }))
        .into_response(),
        MockScenario::FalsePositiveRecovery => {
            let lease_id = guard.fixture.lease_id.clone();
            if call_index == 1 {
                Json(serde_json::json!({
                    "session_id": "session-1",
                    "required_chunks": [guard.fixture.chunk_hashes[0].clone()],
                    "required_manifests": [],
                    "lease_id": lease_id,
                    "lease_expires_at": "2026-03-05T00:15:00Z",
                }))
                .into_response()
            } else {
                Json(serde_json::json!({
                    "session_id": "session-2",
                    "required_chunks": [guard.fixture.chunk_hashes[1].clone()],
                    "required_manifests": [],
                    "lease_id": lease_id,
                    "lease_expires_at": "2026-03-05T00:15:00Z",
                }))
                .into_response()
            }
        }
        MockScenario::MissingChunksAfterRetryFallback => {
            let lease_id = guard.fixture.lease_id.clone();
            Json(serde_json::json!({
                "session_id": format!("session-{}", call_index),
                "required_chunks": [guard.fixture.chunk_hashes[0].clone()],
                "required_manifests": [],
                "lease_id": lease_id,
                "lease_expires_at": "2026-03-05T00:15:00Z",
            }))
            .into_response()
        }
    }
}

async fn mock_chunk(
    State(state): State<SharedMockState>,
    AxumPath(chunk_hash): AxumPath<String>,
) -> Response {
    let mut guard = state.lock().await;
    guard.observations.chunk_calls.push(chunk_hash.clone());
    if guard.scenario == MockScenario::UnauthorizedManifest {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if guard.scenario == MockScenario::LeaseReleaseOnFailure {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let bytes = guard.fixture.chunk_bytes.iter().find_map(|(hash, bytes)| {
        if normalize_hash_for_compare(hash) == normalize_hash_for_compare(&chunk_hash) {
            Some(bytes.clone())
        } else {
            None
        }
    });
    match bytes {
        Some(bytes) => (StatusCode::OK, bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_lease_refresh(
    State(state): State<SharedMockState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let guard = state.lock().await;
    let lease_id = payload
        .get("lease_id")
        .and_then(|value| value.as_str())
        .unwrap_or(guard.fixture.lease_id.as_str());
    Json(serde_json::json!({
        "lease_id": lease_id,
        "expires_at": "2026-03-05T00:20:00Z",
        "chunk_count": guard.fixture.chunk_hashes.len(),
    }))
    .into_response()
}

async fn mock_lease_release(
    State(state): State<SharedMockState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let mut guard = state.lock().await;
    if let Some(lease_id) = payload.get("lease_id").and_then(|value| value.as_str()) {
        guard.observations.release_calls.push(lease_id.to_string());
    }
    StatusCode::OK.into_response()
}

async fn mock_capsule_detail(
    State(state): State<SharedMockState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
) -> Response {
    let guard = state.lock().await;
    if publisher != guard.fixture.publisher || slug != guard.fixture.slug {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(serde_json::json!({
        "id": format!("capsule-{}-{}", guard.fixture.publisher, guard.fixture.slug),
        "scoped_id": guard.fixture.scoped_id,
        "slug": guard.fixture.slug,
        "name": "Mock Capsule",
        "description": "mock description",
        "price": 0,
        "currency": "USD",
        "latestVersion": guard.fixture.version,
        "releases": [{
            "version": guard.fixture.version,
            "content_hash": compute_blake3(&guard.fixture.artifact_bytes),
            "signature_status": "verified",
        }],
    }))
    .into_response()
}

async fn mock_distribution(
    State(state): State<SharedMockState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut guard = state.lock().await;
    if publisher != guard.fixture.publisher || slug != guard.fixture.slug {
        return StatusCode::NOT_FOUND.into_response();
    }
    guard.observations.distribution_calls += 1;
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1");
    Json(serde_json::json!({
        "version": guard.fixture.version,
        "artifact_url": format!("http://{}/mock/artifact.capsule", host),
        "sha256": compute_sha256(&guard.fixture.artifact_bytes),
        "blake3": compute_blake3(&guard.fixture.artifact_bytes),
        "file_name": format!("{}-{}.capsule", guard.fixture.slug, guard.fixture.version),
    }))
    .into_response()
}

async fn mock_artifact(State(state): State<SharedMockState>, headers: HeaderMap) -> Response {
    let mut guard = state.lock().await;
    guard.observations.artifact_calls += 1;
    if guard.scenario == MockScenario::ArtifactRejectsAuthorization
        && headers.contains_key(axum::http::header::AUTHORIZATION)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "auth_header_not_allowed",
                "message": "Presigned artifact URLs must not receive Authorization headers."
            })),
        )
            .into_response();
    }
    (StatusCode::OK, guard.fixture.artifact_bytes.clone()).into_response()
}

#[test]
fn test_compute_blake3() {
    let data = b"hello world";
    let hash = compute_blake3(data);
    assert!(hash.starts_with("blake3:"));
    assert_eq!(hash.len(), 7 + 64);
}

#[test]
fn test_compute_sha256() {
    let data = b"hello world";
    let hash = compute_sha256(data);
    assert_eq!(hash.len(), 64);
}

#[test]
fn test_equals_hash() {
    let value = "b94d27b9934d3e08a52e52d7da7dabfade4f3e9e64c94f4db5d4ef7d6df4f6f6";
    assert!(equals_hash(value, value));
    assert!(equals_hash(&format!("sha256:{}", value), value));
    assert!(equals_hash(&format!("blake3:{}", value), value));
}

#[test]
fn test_normalize_hash_for_compare() {
    let value = "ABCDEF";
    assert_eq!(normalize_hash_for_compare(value), "abcdef");
    assert_eq!(normalize_hash_for_compare("sha256:ABCDEF"), "abcdef");
    assert_eq!(normalize_hash_for_compare("blake3:ABCDEF"), "abcdef");
}

#[test]
fn test_permissions_deserialization_with_aliases() {
    let payload = r#"{
            "network": {
                "egress_allow": ["api.example.com"],
                "connect_allowlist": ["wss://ws.example.com"]
            },
            "isolation": {
                "allow_env": ["OPENAI_API_KEY"]
            },
            "filesystem": {
                "read": ["/opt/data"],
                "write": ["/tmp"]
            }
        }"#;

    let permissions: CapsulePermissions = serde_json::from_str(payload).unwrap();
    let network = permissions.network.unwrap();
    assert_eq!(network.merged_endpoints().len(), 2);
    assert_eq!(
        permissions.isolation.unwrap().allow_env,
        vec!["OPENAI_API_KEY".to_string()]
    );
    let filesystem = permissions.filesystem.unwrap();
    assert_eq!(filesystem.read_only, vec!["/opt/data".to_string()]);
    assert_eq!(filesystem.read_write, vec!["/tmp".to_string()]);
}

#[test]
fn test_permissions_deserialization_missing_fields() {
    let payload = r#"{}"#;
    let permissions: CapsulePermissions = serde_json::from_str(payload).unwrap();
    assert!(permissions.network.is_none());
    assert!(permissions.isolation.is_none());
    assert!(permissions.filesystem.is_none());
}

#[test]
fn test_parse_capsule_ref_accepts_scoped_and_at_scoped() {
    let plain = parse_capsule_ref("koh0920/sample-capsule").unwrap();
    assert_eq!(plain.publisher, "koh0920");
    assert_eq!(plain.slug, "sample-capsule");
    assert_eq!(plain.scoped_id, "koh0920/sample-capsule");

    let with_at = parse_capsule_ref("@koh0920/sample-capsule").unwrap();
    assert_eq!(with_at.scoped_id, "koh0920/sample-capsule");
}

#[test]
fn test_parse_capsule_ref_rejects_slug_only() {
    assert!(parse_capsule_ref("sample-capsule").is_err());
    assert!(is_slug_only_ref("sample-capsule"));
}

#[test]
fn test_resolve_local_slug_against_fake_store() {
    // This test exercises the public API path shape but cannot substitute
    // the hard-coded `~/.ato/store` root without adding a test-only param.
    // We rely on `not_found` behaviour for arbitrary random slug values to
    // keep the test hermetic.
    let random_slug = format!("__definitely_not_installed_{}__", std::process::id());
    let resolution = resolve_local_slug(&random_slug).expect("resolve_local_slug must not error");
    assert!(matches!(resolution, LocalSlugResolution::NotFound));

    // Slashes or empty input always yield NotFound even if present.
    assert!(matches!(
        resolve_local_slug("").unwrap(),
        LocalSlugResolution::NotFound
    ));
    assert!(matches!(
        resolve_local_slug("acme/python").unwrap(),
        LocalSlugResolution::NotFound
    ));
}

#[test]
fn test_parse_capsule_request_extracts_version_suffix() {
    let parsed = parse_capsule_request("koh0920/sample-capsule@1.2.3").unwrap();
    assert_eq!(parsed.scoped_ref.scoped_id, "koh0920/sample-capsule");
    assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
}

#[test]
fn test_normalize_github_repository_accepts_url_host_path_and_owner_repo() {
    assert_eq!(
        normalize_github_repository("https://github.com/Koh0920/ato-cli.git").unwrap(),
        "Koh0920/ato-cli"
    );
    assert_eq!(
        normalize_github_repository("github.com/Koh0920/ato-cli.git").unwrap(),
        "Koh0920/ato-cli"
    );
    assert_eq!(
        normalize_github_repository("www.github.com/Koh0920/ato-cli").unwrap(),
        "Koh0920/ato-cli"
    );
    assert_eq!(
        normalize_github_repository("Koh0920/ato-cli").unwrap(),
        "Koh0920/ato-cli"
    );
}

#[test]
fn test_parse_github_run_ref_accepts_canonical_github_dot_com_input() {
    assert_eq!(
        parse_github_run_ref("github.com/Koh0920/ato-cli.git").unwrap(),
        Some("Koh0920/ato-cli".to_string())
    );
}

#[test]
fn test_parse_github_run_ref_rejects_noncanonical_github_url_input() {
    let error = parse_github_run_ref("https://github.com/Koh0920/ato-cli").unwrap_err();
    assert!(error
        .to_string()
        .contains("ato run github.com/Koh0920/ato-cli"));
}

#[test]
fn test_parse_github_run_ref_ignores_store_scoped_id_shape() {
    assert_eq!(parse_github_run_ref("koh0920/ato-cli").unwrap(), None);
}

#[test]
fn test_normalize_install_segment_slugifies_github_owner() {
    assert_eq!(normalize_install_segment("Koh_0920").unwrap(), "koh-0920");
    assert!(normalize_install_segment("___").is_err());
}

#[test]
fn test_github_api_base_url_uses_env_override() {
    let key = "ATO_GITHUB_API_BASE_URL";
    let previous = std::env::var(key).ok();
    std::env::set_var(key, "http://127.0.0.1:3000/");
    assert_eq!(github_api_base_url(), "http://127.0.0.1:3000");
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[test]
fn test_normalize_github_checkout_dir_renames_to_repo_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let extracted = temp.path().join("Koh0920-demo-abc123");
    std::fs::create_dir_all(&extracted).expect("create extracted");
    std::fs::write(extracted.join("index.js"), "console.log('hi')").expect("write fixture");
    let normalized =
        normalize_github_checkout_dir(extracted.clone(), "demo").expect("normalize checkout");
    assert_eq!(normalized, temp.path().join("demo"));
    assert!(normalized.join("index.js").exists());
    assert!(!extracted.exists());
}

#[test]
fn test_unpack_github_tarball_rejects_empty_archive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let bytes = encoder.finish().expect("finish gzip");
    let err = unpack_github_tarball(&bytes, temp.path()).expect_err("empty archive must fail");
    assert!(err.to_string().contains("GitHub archive is empty"));
}

#[test]
fn test_unpack_github_tarball_rejects_multiple_top_level_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut archive_bytes = Vec::new();
    {
        let encoder =
            flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "repo-a/index.js", std::io::Cursor::new(b"a"))
            .expect("append repo-a");

        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "repo-b/index.js", std::io::Cursor::new(b"b"))
            .expect("append repo-b");

        builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");
    }

    let err = unpack_github_tarball(&archive_bytes, temp.path()).expect_err("must reject archive");
    assert!(err.to_string().contains("multiple top-level directories"));
}

#[test]
fn test_unpack_github_tarball_ignores_global_pax_headers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut archive_bytes = Vec::new();
    {
        let encoder =
            flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "pax_global_header", std::io::Cursor::new([]))
            .expect("append pax global header");

        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "repo/index.js", std::io::Cursor::new(b"a"))
            .expect("append repo file");

        builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");
    }

    let root = unpack_github_tarball(&archive_bytes, temp.path()).expect("must unpack archive");
    assert_eq!(root, temp.path().join("repo"));
    assert_eq!(
        std::fs::read_to_string(root.join("index.js")).expect("read unpacked file"),
        "a"
    );
}

#[test]
fn test_unpack_github_tarball_rejects_path_traversal_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut tar_bytes = Vec::new();
    {
        let path = b"repo/../evil.txt";
        let content = b"x";
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path);
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000001\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
        let checksum_octal = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum_octal.as_bytes());

        tar_bytes.extend_from_slice(&header);
        tar_bytes.extend_from_slice(content);
        tar_bytes.extend_from_slice(&[0u8; 511][..511 - content.len() + 1]);
        tar_bytes.extend_from_slice(&[0u8; 1024]);
    }
    let mut archive_bytes = Vec::new();
    {
        let mut encoder =
            flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
        use std::io::Write as _;
        encoder.write_all(&tar_bytes).expect("write tar");
        encoder.finish().expect("finish gzip");
    }

    let err =
        unpack_github_tarball(&archive_bytes, temp.path()).expect_err("must reject traversal");
    assert!(err.to_string().contains("unsafe path traversal components"));
}

#[tokio::test(flavor = "current_thread")]
async fn download_github_repository_at_ref_maps_private_repo_404_to_auth_message() {
    use axum::extract::Query;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;
    use std::collections::HashMap;

    async fn github_tarball() -> impl IntoResponse {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "Not Found",
                "documentation_url": "https://docs.github.com/rest/repos/contents#download-a-repository-archive-tar",
                "status": "404"
            })),
        )
    }

    async fn store_archive(
        headers: HeaderMap,
        Query(query): Query<HashMap<String, String>>,
    ) -> impl IntoResponse {
        assert_eq!(
            headers
                .get(reqwest::header::AUTHORIZATION.as_str())
                .and_then(|v| v.to_str().ok()),
            Some("Bearer session-token-private")
        );
        assert_eq!(query.get("ref").map(String::as_str), Some("main"));
        (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "github_app_required",
                "message": "Install the ato GitHub App on the \"octocat\" account to access private repositories."
            })),
        )
    }

    let _env_lock = acquire_test_env_lock().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock github/store server");
    let addr = listener.local_addr().expect("local addr");
    let app = Router::new()
        .route(
            "/repos/octocat/private-repo/tarball/main",
            get(github_tarball),
        )
        .route(
            "/v1/github/repos/octocat/private-repo/authed/archive",
            get(store_archive),
        );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let base_url = format!("http://{}", addr);
    let _github_guard = EnvVarGuard::set("ATO_GITHUB_API_BASE_URL", Some(base_url.as_str()));
    let _store_guard = EnvVarGuard::set("ATO_STORE_API_URL", Some(base_url.as_str()));
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", Some("session-token-private"));

    let err = download_github_repository_at_ref("octocat/private-repo", Some("main"))
        .await
        .expect_err("private repo archive should surface auth guidance");
    let rendered = format!("{:#}", err);
    assert!(rendered.contains("GitHub App"));
    assert!(rendered.contains("private repositories"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn download_github_repository_at_ref_uses_github_token_for_public_archive_fetch() {
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;

    async fn github_tarball(headers: HeaderMap) -> impl IntoResponse {
        assert_eq!(
            headers
                .get(reqwest::header::AUTHORIZATION.as_str())
                .and_then(|v| v.to_str().ok()),
            Some("Bearer gh-token-public")
        );

        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(17);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    "public-repo-main/index.js",
                    std::io::Cursor::new(b"console.log('ok')"),
                )
                .expect("append tarball file");
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }

        (
            StatusCode::OK,
            [(reqwest::header::CONTENT_TYPE.as_str(), "application/gzip")],
            archive_bytes,
        )
    }

    let _env_lock = acquire_test_env_lock().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock github server");
    let addr = listener.local_addr().expect("local addr");
    let app = Router::new().route(
        "/repos/octocat/public-repo/tarball/main",
        get(github_tarball),
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let base_url = format!("http://{}", addr);
    let _github_guard = EnvVarGuard::set("ATO_GITHUB_API_BASE_URL", Some(base_url.as_str()));
    let _gh_token_guard = EnvVarGuard::set("GH_TOKEN", Some("gh-token-public"));

    let checkout = download_github_repository_at_ref("octocat/public-repo", Some("main"))
        .await
        .expect("public repo archive should download");
    assert!(checkout.checkout_dir.join("index.js").exists());

    server.abort();
}

#[test]
fn test_merge_requested_version_rejects_conflicts() {
    let err = merge_requested_version(Some("1.0.0"), Some("2.0.0")).expect_err("must fail");
    assert!(err.to_string().contains("conflicting_version_request"));
}

#[test]
fn test_epoch_guard_rejects_downgrade_without_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_path = temp.path().join("epoch-guard.json");
    enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 10, "blake3:aaaa", false)
        .expect("seed epoch");
    let err = enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 9, "blake3:bbbb", false)
        .expect_err("downgrade must fail");
    assert!(err.to_string().contains("Downgrade detected"));
}

#[test]
fn test_epoch_guard_allows_downgrade_with_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_path = temp.path().join("epoch-guard.json");
    enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 10, "blake3:aaaa", false)
        .expect("seed epoch");
    enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 9, "blake3:bbbb", true)
        .expect("downgrade should be allowed with explicit flag");
    let state = load_epoch_guard_state(&state_path).expect("state readable");
    let entry = state.capsules.get("koh0920/app").expect("entry exists");
    assert_eq!(entry.max_epoch, 10);
}

#[test]
fn test_epoch_guard_rejects_same_epoch_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_path = temp.path().join("epoch-guard.json");
    enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 7, "blake3:aaaa", false)
        .expect("seed epoch");
    let err = enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 7, "blake3:bbbb", true)
        .expect_err("same epoch conflict must fail");
    assert!(err.to_string().contains("Epoch replay mismatch"));
}

#[test]
fn test_compute_manifest_hash_without_signatures_is_stable() {
    let chunk_hash = format!("blake3:{}", blake3::hash(b"payload").to_hex());
    let mut manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

runtime = "source"
run = "main.py""#,
    )
    .expect("manifest");
    manifest.distribution = Some(capsule_core::types::DistributionInfo {
        manifest_hash: String::new(),
        merkle_root: chunk_hash.clone(),
        chunk_list: vec![capsule_core::types::ChunkDescriptor {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: 7,
            codec: "fastcdc".to_string(),
            compression: "none".to_string(),
        }],
        signatures: vec![],
    });
    let hash = compute_manifest_hash_without_signatures(&manifest).expect("hash");
    manifest
        .distribution
        .as_mut()
        .expect("distribution")
        .manifest_hash = hash.clone();
    manifest
        .distribution
        .as_mut()
        .expect("distribution")
        .signatures
        .push(capsule_core::types::SignatureEntry {
            signer_did: "did:key:zabc".to_string(),
            key_id: "k1".to_string(),
            algorithm: "ed25519".to_string(),
            signature: "AAAA".to_string(),
            signed_at: None,
        });
    let hash_with_signature = compute_manifest_hash_without_signatures(&manifest).expect("hash");
    assert_eq!(hash, hash_with_signature);
}

#[test]
fn test_verify_payload_chunks_and_merkle_root() {
    let payload = b"payload".to_vec();
    let chunk_hash = format!("blake3:{}", blake3::hash(&payload).to_hex());
    let mut manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

runtime = "source"
run = "main.py""#,
    )
    .expect("manifest");
    manifest.distribution = Some(capsule_core::types::DistributionInfo {
        manifest_hash: "blake3:dummy".to_string(),
        merkle_root: chunk_hash.clone(),
        chunk_list: vec![capsule_core::types::ChunkDescriptor {
            chunk_hash,
            offset: 0,
            length: payload.len() as u64,
            codec: "fastcdc".to_string(),
            compression: "none".to_string(),
        }],
        signatures: vec![],
    });
    verify_payload_chunks(&manifest, &payload).expect("chunks");
    verify_manifest_merkle_root(&manifest).expect("merkle");
}

#[test]
fn test_build_capsule_artifact_contains_manifest_and_payload() {
    let manifest = "schema_version = \"1\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
    let payload = b"compressed-payload";
    let artifact = build_capsule_artifact(Some(manifest), None, payload).expect("artifact");
    let mut archive = tar::Archive::new(Cursor::new(artifact));
    let mut has_manifest = false;
    let mut has_payload = false;
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().to_string();
        if path == "capsule.toml" {
            has_manifest = true;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read manifest");
            assert_eq!(bytes, manifest.as_bytes());
        } else if path == "payload.tar.zst" {
            has_payload = true;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read payload");
            assert_eq!(bytes, payload);
        }
    }
    assert!(has_manifest);
    assert!(has_payload);
}

#[test]
fn test_build_capsule_artifact_includes_capsule_toml_when_provided() {
    let payload = b"compressed-payload";
    let capsule_toml = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
    let artifact = build_capsule_artifact(Some(capsule_toml), None, payload).expect("artifact");
    let mut archive = tar::Archive::new(Cursor::new(artifact));
    let mut has_capsule_toml = false;
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().to_string();
        if path == "capsule.toml" {
            has_capsule_toml = true;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read capsule.toml");
            assert_eq!(bytes, capsule_toml.as_bytes());
        }
    }
    assert!(has_capsule_toml);
}

#[test]
fn test_build_capsule_artifact_includes_capsule_lock_when_provided() {
    let payload = b"compressed-payload";
    let capsule_lock = r#"{"schema_version":"0.1","lock_generated_at":"2026-03-05T00:00:00Z"}"#;
    let artifact = build_capsule_artifact(None, Some(capsule_lock), payload).expect("artifact");
    let mut archive = tar::Archive::new(Cursor::new(artifact));
    let mut has_capsule_lock = false;
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().to_string();
        if path == "capsule.lock.json" {
            has_capsule_lock = true;
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .expect("read capsule.lock.json");
            assert_eq!(bytes, capsule_lock.as_bytes());
        }
    }
    assert!(has_capsule_lock);
}

#[test]
fn test_reconstruct_payload_reports_missing_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cas = LocalCasIndex::open(temp.path()).expect("open cas");
    let first = b"chunk-a";
    let second = b"chunk-b";
    let first_hash = format!("blake3:{}", blake3::hash(first).to_hex());
    let second_hash = format!("blake3:{}", blake3::hash(second).to_hex());
    cas.put_verified_chunk(&first_hash, first)
        .expect("put first");

    let mut manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

runtime = "source"
run = "main.py""#,
    )
    .expect("manifest");
    manifest.distribution = Some(capsule_core::types::DistributionInfo {
        manifest_hash: "blake3:dummy".to_string(),
        merkle_root: "blake3:dummy".to_string(),
        chunk_list: vec![
            capsule_core::types::ChunkDescriptor {
                chunk_hash: first_hash.clone(),
                offset: 0,
                length: first.len() as u64,
                codec: "fastcdc".to_string(),
                compression: "none".to_string(),
            },
            capsule_core::types::ChunkDescriptor {
                chunk_hash: second_hash.clone(),
                offset: first.len() as u64,
                length: second.len() as u64,
                codec: "fastcdc".to_string(),
                compression: "none".to_string(),
            },
        ],
        signatures: vec![],
    });

    let reconstructed =
        reconstruct_payload_from_local_chunks(&cas, &manifest).expect("reconstruct");
    assert_eq!(reconstructed.missing_chunks, vec![second_hash]);
    assert_eq!(reconstructed.payload_tar, first);
}

#[tokio::test(flavor = "current_thread")]
async fn test_delta_install_false_positive_recovers_with_reuse_lease_id() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(
        TEST_SCOPED_ID,
        TEST_VERSION,
        vec![b"chunk-alpha".to_vec(), b"chunk-beta".to_vec()],
    );
    let server = spawn_mock_registry(MockScenario::FalsePositiveRecovery, fixture.clone()).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let result =
        install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
            .await
            .expect("delta install should succeed after retry");
    let artifact = match result {
        DeltaInstallResult::Artifact(artifact) => artifact,
        other => panic!("expected reconstructed artifact result, got {:?}", other),
    };
    let reconstructed_payload =
        extract_payload_tar_from_capsule(&artifact).expect("extract reconstructed payload");
    assert_eq!(reconstructed_payload, fixture.payload_tar);

    let observations = server.observations().await;
    assert_eq!(observations.negotiate_calls.len(), 2);
    assert!(observations.negotiate_calls[0].has_bloom);
    assert!(!observations.negotiate_calls[1].has_bloom);
    assert_eq!(observations.negotiate_calls[1].have_chunks_len, 1);
    assert_eq!(
        observations.negotiate_calls[1].reuse_lease_id.as_deref(),
        Some(TEST_LEASE_ID)
    );
    assert_eq!(observations.release_calls, vec![TEST_LEASE_ID.to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn test_install_app_uses_version_resolve_for_explicit_time_travel() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let output_root = tempfile::tempdir().expect("output root");
    let runtime_root = tempfile::tempdir().expect("runtime root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _runtime_guard = EnvVarGuard::set(
        "ATO_RUNTIME_ROOT",
        Some(runtime_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let payload_tar = build_payload_tar_with_source("main.py", b"print('time travel')\n");
    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![payload_tar]);
    let server = spawn_mock_registry(MockScenario::FalsePositiveRecovery, fixture).await;
    let result = install_app(
        "koh0920/sample@1.0.0",
        Some(server.base_url()),
        None,
        Some(output_root.path().to_path_buf()),
        false,
        false,
        ProjectionPreference::Skip,
        false,
        false,
        true,
        false,
    )
    .await
    .expect("explicit version install should succeed");
    assert_eq!(result.version, TEST_VERSION);

    let observations = server.observations().await;
    assert_eq!(observations.version_resolve_calls, 2);
    assert_eq!(observations.epoch_calls, 0);
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "current_thread")]
async fn repository_ato_desktop_capsule_installs_via_native_local_derivation() {
    let _env_lock = acquire_test_env_lock().await;
    let home_root = tempfile::tempdir().expect("home root");
    let output_root = tempfile::tempdir().expect("output root");
    let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repo root");
    let desktop_root = repo_root.join("crates").join("ato-desktop");

    let macos_dir = desktop_root
        .join("dist")
        .join("darwin-arm64")
        .join("Ato Desktop.app")
        .join("Contents")
        .join("MacOS");
    let has_executable = std::fs::read_dir(&macos_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
        })
        .unwrap_or(false);
    if !has_executable {
        eprintln!(
            "skipping repository_ato_desktop_capsule_installs_via_native_local_derivation: \
             Ato Desktop.app/Contents/MacOS/ has no executable; \
             build the desktop bundle first to exercise this test"
        );
        return;
    }

    let authoritative_input = resolve_producer_authoritative_input(
        &desktop_root,
        std::sync::Arc::new(CliReporter::new(false)),
        false,
    )
    .expect("authoritative input");
    let artifact_path =
        build_publish_capsule_artifact("ato-desktop", "0.1.0", Some(&authoritative_input), None)
            .expect("artifact build");
    let bytes = std::fs::read(&artifact_path).expect("artifact bytes");
    let scoped_ref = parse_capsule_ref("koh0920/ato-desktop").expect("scoped ref");

    let result = complete_install_from_bytes(
        "local:ato-desktop".to_string(),
        scoped_ref,
        "ato-desktop".to_string(),
        "0.1.0".to_string(),
        bytes,
        "ato-desktop-0.1.0.capsule".to_string(),
        InstallExecutionOptions {
            output_dir: Some(output_root.path().to_path_buf()),
            yes: true,
            projection_preference: ProjectionPreference::Skip,
            json_output: true,
            can_prompt_interactively: false,
            promotion_source: None,
            keep_progressive_flow_open: false,
        },
        InstallSource::Local("test://ato-desktop".to_string()),
    )
    .await
    .expect("native install should succeed");

    assert!(matches!(
        result.install_kind,
        InstallKind::NativeRequiresLocalDerivation
    ));
    assert!(
        result.path.is_file(),
        "installed capsule archive must exist"
    );
    let derived_app_path = result
        .local_derivation
        .as_ref()
        .and_then(|info| info.derived_app_path.as_ref())
        .expect("derived app path");
    assert!(derived_app_path.is_dir(), "derived app bundle must exist");
    assert!(matches!(
        result.launchable,
        Some(LaunchableTarget::DerivedApp { .. })
    ));
    assert!(result
        .projection
        .as_ref()
        .is_some_and(|projection| !projection.performed));
}

#[tokio::test(flavor = "current_thread")]
async fn test_install_app_fails_closed_on_negotiate_501() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let output_root = tempfile::tempdir().expect("output root");
    let runtime_root = tempfile::tempdir().expect("runtime root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _runtime_guard = EnvVarGuard::set(
        "ATO_RUNTIME_ROOT",
        Some(runtime_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
    let server = spawn_mock_registry(MockScenario::FallbackNotImplemented, fixture).await;
    let err = install_app(
        TEST_SCOPED_ID,
        Some(server.base_url()),
        Some(TEST_VERSION),
        Some(output_root.path().to_path_buf()),
        false,
        false,
        ProjectionPreference::Skip,
        true,
        false,
        true,
        false,
    )
    .await
    .expect_err("install should fail closed when negotiate is unavailable");
    assert!(err
        .to_string()
        .contains("Registry does not support the manifest negotiate API"));

    let observations = server.observations().await;
    assert_eq!(observations.negotiate_calls.len(), 1);
    assert_eq!(observations.distribution_calls, 0);
    assert_eq!(observations.artifact_calls, 0);
    assert!(observations.release_calls.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn test_install_app_unauthorized_manifest_fails_closed_without_fallback() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let output_root = tempfile::tempdir().expect("output root");
    let runtime_root = tempfile::tempdir().expect("runtime root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _runtime_guard = EnvVarGuard::set(
        "ATO_RUNTIME_ROOT",
        Some(runtime_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
    let server = spawn_mock_registry(MockScenario::UnauthorizedManifest, fixture).await;
    let err = install_app(
        TEST_SCOPED_ID,
        Some(server.base_url()),
        Some(TEST_VERSION),
        Some(output_root.path().to_path_buf()),
        false,
        false,
        ProjectionPreference::Skip,
        false,
        false,
        true,
        false,
    )
    .await
    .expect_err("install should fail closed on unauthorized manifest read");
    let rendered = format!("{:#}", err);
    assert!(
        rendered.contains(crate::error_codes::ATO_ERR_AUTH_REQUIRED)
            || rendered.contains("status=401 Unauthorized")
    );

    let observations = server.observations().await;
    assert_eq!(observations.distribution_calls, 0);
    assert_eq!(observations.artifact_calls, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn test_manifest_api_404_falls_back_to_distribution_download() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", Some("test-token"));

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
    let expected_artifact = fixture.artifact_bytes.clone();
    let expected_file_name = format!("sample-{}.capsule", TEST_VERSION);
    let server = spawn_mock_registry(MockScenario::ManifestApiNotFound, fixture).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let result = install_manifest_delta_path(
        &client,
        server.base_url(),
        &scoped_ref,
        Some(TEST_VERSION),
        None,
        None,
    )
    .await
    .expect("404 manifest endpoint should fall back to direct distribution download");

    match result {
        DeltaInstallResult::DownloadedArtifact { bytes, file_name } => {
            assert_eq!(bytes, expected_artifact);
            assert_eq!(file_name, expected_file_name);
        }
        other => panic!("expected downloaded artifact fallback, got {:?}", other),
    }

    let observations = server.observations().await;
    assert_eq!(observations.version_resolve_calls, 1);
    assert_eq!(observations.manifest_calls, 0);
    assert!(observations.negotiate_calls.is_empty());
    assert_eq!(observations.distribution_calls, 1);
    assert_eq!(observations.artifact_calls, 1);
    assert!(observations.release_calls.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn test_distribution_artifact_fallback_does_not_send_auth_to_presigned_url() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", Some("test-token"));

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
    let expected_artifact = fixture.artifact_bytes.clone();
    let server = spawn_mock_registry(MockScenario::ArtifactRejectsAuthorization, fixture).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let result = install_manifest_delta_path(
        &client,
        server.base_url(),
        &scoped_ref,
        Some(TEST_VERSION),
        None,
        None,
    )
    .await
    .expect("artifact fallback should omit bearer auth for presigned URLs");

    match result {
        DeltaInstallResult::DownloadedArtifact { bytes, .. } => {
            assert_eq!(bytes, expected_artifact);
        }
        other => panic!("expected downloaded artifact fallback, got {:?}", other),
    }

    let observations = server.observations().await;
    assert_eq!(observations.distribution_calls, 1);
    assert_eq!(observations.artifact_calls, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn test_missing_chunks_after_retry_falls_back_to_distribution_download() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", Some("test-token"));

    let fixture = build_mock_fixture(
        TEST_SCOPED_ID,
        TEST_VERSION,
        vec![b"chunk-a".to_vec(), b"chunk-b".to_vec()],
    );
    let expected_artifact = fixture.artifact_bytes.clone();
    let expected_file_name = format!("sample-{}.capsule", TEST_VERSION);
    let server = spawn_mock_registry(MockScenario::MissingChunksAfterRetryFallback, fixture).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let result =
        install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
            .await
            .expect("missing chunks after retry should fall back to direct artifact download");

    match result {
        DeltaInstallResult::DownloadedArtifact { bytes, file_name } => {
            assert_eq!(bytes, expected_artifact);
            assert_eq!(file_name, expected_file_name);
        }
        other => panic!("expected downloaded artifact fallback, got {:?}", other),
    }

    let observations = server.observations().await;
    assert_eq!(observations.negotiate_calls.len(), 2);
    assert_eq!(observations.distribution_calls, 1);
    assert_eq!(observations.artifact_calls, 1);
    assert_eq!(observations.release_calls, vec![TEST_LEASE_ID.to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn test_delta_install_releases_lease_when_chunk_download_fails() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
    let server = spawn_mock_registry(MockScenario::LeaseReleaseOnFailure, fixture).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let err =
        install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
            .await
            .expect_err("chunk failure should abort delta install");
    assert!(err
        .to_string()
        .contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));

    let observations = server.observations().await;
    assert_eq!(observations.release_calls, vec![TEST_LEASE_ID.to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn test_negotiate_yanked_fails_closed() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
    let server = spawn_mock_registry(MockScenario::YankedNegotiate, fixture).await;
    let client = reqwest::Client::new();
    let scoped_ref = test_scoped_ref();
    let err =
        install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
            .await
            .expect_err("yanked negotiate must fail closed");
    let message = err.to_string();
    assert!(message.contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
    assert!(message.to_ascii_lowercase().contains("yanked"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_manifest_yanked_fails_closed_even_with_allow_unverified() {
    let _env_lock = acquire_test_env_lock().await;
    let cas_root = tempfile::tempdir().expect("cas root");
    let output_root = tempfile::tempdir().expect("output root");
    let runtime_root = tempfile::tempdir().expect("runtime root");
    let _cas_guard = EnvVarGuard::set(
        "ATO_CAS_ROOT",
        Some(cas_root.path().to_string_lossy().as_ref()),
    );
    let _runtime_guard = EnvVarGuard::set(
        "ATO_RUNTIME_ROOT",
        Some(runtime_root.path().to_string_lossy().as_ref()),
    );
    let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

    let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
    let server = spawn_mock_registry(MockScenario::YankedManifest, fixture).await;
    let err = install_app(
        TEST_SCOPED_ID,
        Some(server.base_url()),
        Some(TEST_VERSION),
        Some(output_root.path().to_path_buf()),
        false,
        false,
        ProjectionPreference::Skip,
        true,
        false,
        true,
        false,
    )
    .await
    .expect_err("yanked manifest must fail closed");
    let message = err.to_string();
    assert!(message.contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
    assert!(message.to_ascii_lowercase().contains("yanked"));
}

#[test]
fn test_atomic_install_writes_via_tmp_and_rename() {
    let temp = tempfile::tempdir().expect("tempdir");
    let install_dir = temp.path().join("install");
    std::fs::create_dir_all(&install_dir).expect("mkdir");
    let stale = install_dir.join(".capsule.tmp.stale");
    std::fs::write(&stale, b"stale").expect("write stale");
    sweep_stale_tmp_capsules(&install_dir).expect("sweep stale");
    assert!(!stale.exists());

    let output_path = install_dir.join("sample.capsule");
    let payload = b"atomic-payload".to_vec();
    let expected = compute_blake3(&payload);
    write_capsule_atomic(&output_path, &payload, &expected).expect("atomic write");

    let written = std::fs::read(&output_path).expect("read output");
    assert_eq!(written, payload);
    let leftovers = std::fs::read_dir(&install_dir)
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".capsule.tmp.")
        })
        .count();
    assert_eq!(leftovers, 0);
}
