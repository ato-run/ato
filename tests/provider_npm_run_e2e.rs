#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use flate2::write::GzEncoder;
#[cfg(unix)]
use flate2::Compression;
#[cfg(unix)]
use serde_json::{json, Value};
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tar::Builder;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
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

#[cfg(unix)]
fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(unix)]
fn host_limited(stderr: &str) -> bool {
    stderr.contains("No compatible native sandbox backend is available")
        || stderr.contains("Sandbox unavailable")
        || stderr.contains("pfctl failed to load anchor")
}

#[cfg(unix)]
fn maybe_resolve_test_nacelle_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("NACELLE_PATH") {
        let nacelle = PathBuf::from(path);
        if nacelle.exists() {
            return Some(nacelle);
        }
    }

    let candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../nacelle/target/debug/nacelle");
    candidate.exists().then_some(candidate)
}

#[cfg(unix)]
fn require_native_provider_prerequisites() -> Option<PathBuf> {
    let Some(nacelle) = maybe_resolve_test_nacelle_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires nacelle for provider-backed npm sandbox E2E"
        );
        return None;
    };
    Some(nacelle)
}

#[cfg(unix)]
fn write_poison_node_shims(root: &Path) {
    let script = "#!/bin/sh\necho poisoned node shim >&2\nexit 97\n";
    for name in ["node", "npm", "pnpm"] {
        let path = root.join(name);
        fs::write(&path, script).expect("write poison node shim");
        let mut permissions = fs::metadata(&path).expect("shim metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("shim permissions");
    }
}

#[cfg(unix)]
fn prepend_path(dir: &Path) -> String {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths)
        .expect("join PATH entries")
        .to_string_lossy()
        .to_string()
}

#[cfg(unix)]
fn assert_success_or_skip(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && host_limited(&stderr) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={stderr}"
        );
        return false;
    }

    assert!(
        output.status.success(),
        "stdout={}; stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    true
}

#[cfg(unix)]
fn load_output_json(path: &Path, output: &std::process::Output) -> Value {
    for _ in 0..100 {
        if let Ok(raw) = fs::read(path) {
            return serde_json::from_slice(&raw).expect("parse output json");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let raw = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "read output json: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    serde_json::from_slice(&raw).expect("parse output json")
}

#[cfg(unix)]
fn provider_runs_root(home: &Path) -> PathBuf {
    home.join(".ato").join("runs").join("provider-backed")
}

#[cfg(unix)]
struct NpmPackageFixture<'a> {
    name: &'a str,
    version: &'a str,
    manifest: Value,
    files: Vec<(&'a str, &'a str)>,
}

#[cfg(unix)]
struct RegistryPackage {
    packument_path: String,
    packument_body: Vec<u8>,
    tarball_path: String,
    tarball_body: Vec<u8>,
}

#[cfg(unix)]
fn build_npm_tarball(fixture: &NpmPackageFixture<'_>) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(encoder);

    let manifest = serde_json::to_vec_pretty(&fixture.manifest).expect("serialize package.json");
    append_tar_bytes(&mut tar, "package/package.json", &manifest);
    for (relative_path, contents) in &fixture.files {
        append_tar_bytes(
            &mut tar,
            &format!("package/{relative_path}"),
            contents.as_bytes(),
        );
    }

    let encoder = tar.into_inner().expect("finish tar stream");
    encoder.finish().expect("finish gzip stream")
}

#[cfg(unix)]
fn append_tar_bytes<W: Write>(tar: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)
        .expect("append tar entry");
}

#[cfg(unix)]
fn registry_package(base_url: &str, fixture: &NpmPackageFixture<'_>) -> RegistryPackage {
    let tarball_name = format!("{}-{}.tgz", fixture.name, fixture.version);
    let tarball_path = format!("/tarballs/{tarball_name}");
    let tarball_url = format!("{base_url}{tarball_path}");
    let packument_body = serde_json::to_vec(&json!({
        "name": fixture.name,
        "dist-tags": {
            "latest": fixture.version,
        },
        "versions": {
            fixture.version: {
                "name": fixture.name,
                "version": fixture.version,
                "dist": {
                    "tarball": tarball_url,
                },
            },
        },
    }))
    .expect("serialize packument");

    RegistryPackage {
        packument_path: format!("/{}", fixture.name),
        packument_body,
        tarball_path,
        tarball_body: build_npm_tarball(fixture),
    }
}

#[cfg(unix)]
struct NpmRegistryServer {
    base_url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for NpmRegistryServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(unix)]
fn spawn_npm_registry_server(fixtures: Vec<NpmPackageFixture<'static>>) -> NpmRegistryServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind npm registry server");
    listener
        .set_nonblocking(true)
        .expect("make npm registry server nonblocking");
    let addr = listener.local_addr().expect("resolve npm registry addr");
    let base_url = format!("http://{}", addr);

    let mut responses = HashMap::new();
    for fixture in fixtures {
        let package = registry_package(&base_url, &fixture);
        responses.insert(package.packument_path, package.packument_body);
        responses.insert(package.tarball_path, package.tarball_body);
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);
    let handle = std::thread::spawn(move || {
        while !shutdown_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let started = Instant::now();
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 4096];

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

                    if let Some(body) = responses.get(&path) {
                        let content_type = if path.ends_with(".tgz") {
                            "application/octet-stream"
                        } else {
                            "application/json; charset=utf-8"
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            content_type
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(body);
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

    NpmRegistryServer {
        base_url,
        shutdown,
        handle: Some(handle),
    }
}

#[cfg(unix)]
fn demo_npm_single_bin_fixture() -> NpmPackageFixture<'static> {
    NpmPackageFixture {
        name: "demo-npm-single-bin",
        version: "1.0.0",
        manifest: json!({
            "name": "demo-npm-single-bin",
            "version": "1.0.0",
            "bin": "bin/cli.mjs",
        }),
        files: vec![(
            "bin/cli.mjs",
            r#"#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";

const argv = process.argv.slice(2);
if (argv.includes("--help")) {
  console.log("demo-npm-single-bin help");
  process.exit(0);
}

const inputPath = argv[0];
const outputIndex = argv.indexOf("-o");
if (!inputPath || outputIndex === -1 || outputIndex + 1 >= argv.length) {
  console.error("usage: demo-npm-single-bin <input> -o <output>");
  process.exit(2);
}

const outputPath = argv[outputIndex + 1];
const payload = {
  cwd: process.cwd(),
  argv,
  inputExists: fs.existsSync(inputPath),
  content: fs.readFileSync(inputPath, "utf8"),
};
fs.writeFileSync(outputPath, JSON.stringify(payload));
console.log(JSON.stringify(payload));
"#,
        )],
    }
}

#[cfg(unix)]
fn demo_npm_multi_bin_fixture() -> NpmPackageFixture<'static> {
    NpmPackageFixture {
        name: "demo-npm-multi-bin",
        version: "1.0.0",
        manifest: json!({
            "name": "demo-npm-multi-bin",
            "version": "1.0.0",
            "bin": {
                "demo-a": "bin/a.mjs",
                "demo-b": "bin/b.mjs",
            },
        }),
        files: vec![
            ("bin/a.mjs", "console.log('a');\n"),
            ("bin/b.mjs", "console.log('b');\n"),
        ],
    }
}

#[cfg(unix)]
fn demo_npm_no_bin_fixture() -> NpmPackageFixture<'static> {
    NpmPackageFixture {
        name: "demo-npm-no-bin",
        version: "1.0.0",
        manifest: json!({
            "name": "demo-npm-no-bin",
            "version": "1.0.0",
        }),
        files: vec![("index.js", "console.log('no bin');\n")],
    }
}

#[cfg(unix)]
fn demo_npm_needs_install_script_fixture() -> NpmPackageFixture<'static> {
    NpmPackageFixture {
        name: "demo-npm-needs-install-script",
        version: "1.0.0",
        manifest: json!({
            "name": "demo-npm-needs-install-script",
            "version": "1.0.0",
            "bin": "bin/cli.mjs",
            "scripts": {
                "install": "node build.mjs",
            },
        }),
        files: vec![("bin/cli.mjs", "console.log('ok');\n")],
    }
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_sandbox_preserves_caller_cwd_and_cleans_workspace() {
    let Some(nacelle) = require_native_provider_prerequisites() else {
        return;
    };

    let temp = workspace_tempdir("provider-npm-run-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-npm-home-");
    let poisoned_path = workspace_tempdir("provider-npm-path-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    write_poison_node_shims(poisoned_path.path());
    let server = spawn_npm_registry_server(vec![
        demo_npm_single_bin_fixture(),
        demo_npm_multi_bin_fixture(),
        demo_npm_no_bin_fixture(),
        demo_npm_needs_install_script_fixture(),
    ]);

    let input_path = caller_dir.join("input.txt");
    let output_path = caller_dir.join("output.json");
    fs::write(&input_path, "hello from npm provider package\n").expect("write input");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--sandbox")
        .arg("--yes")
        .arg("--nacelle")
        .arg(&nacelle)
        .arg("--read")
        .arg("./input.txt")
        .arg("--write")
        .arg("./output.json")
        .arg("npm:demo-npm-single-bin")
        .arg("--")
        .args(["./input.txt", "-o", "./output.json"])
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .env("PATH", prepend_path(poisoned_path.path()))
        .output()
        .expect("run provider-backed npm sandbox fixture");

    if !assert_success_or_skip(&output) {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("PHASE "), "stdout={stdout}");
    let stdout_json: Value = serde_json::from_slice(&output.stdout)
        .expect("default run stdout should contain only tool output json");

    let payload = load_output_json(&output_path, &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./input.txt", "-o", "./output.json"])
    );
    assert_eq!(stdout_json, payload);
    assert_eq!(payload["inputExists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from npm provider package\n")
    );
    assert!(
        !caller_dir.join(".ato").exists(),
        "provider-backed run should not create workspace-local .ato state"
    );

    let provider_runs = provider_runs_root(home.path());
    let remaining = if provider_runs.is_dir() {
        fs::read_dir(&provider_runs)
            .expect("read provider runs dir")
            .next()
            .is_some()
    } else {
        false
    };
    assert!(
        !remaining,
        "provider-backed synthetic workspaces should be cleaned up under {}",
        provider_runs.display()
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_via_pnpm_sandbox_preserves_caller_cwd_and_cleans_workspace() {
    let temp = workspace_tempdir("provider-npm-run-pnpm-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-pnpm-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("--via")
        .arg("pnpm")
        .arg("npm:demo-npm-single-bin")
        .env("HOME", home.path())
        .output()
        .expect("run provider-backed npm invalid-via fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not valid for npm:"), "stderr={stderr}");
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_multi_bin_fails_closed() {
    let temp = workspace_tempdir("provider-npm-multi-bin-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-npm-multi-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let server = spawn_npm_registry_server(vec![demo_npm_multi_bin_fixture()]);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("npm:demo-npm-multi-bin")
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .output()
        .expect("run provider-backed npm multi-bin fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("multiple bin entrypoints"),
        "stderr={stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_via_pnpm_multi_bin_fails_closed() {
    let temp = workspace_tempdir("provider-pnpm-multi-bin-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-pnpm-multi-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("--via")
        .arg("pnpm")
        .arg("npm:demo-npm-multi-bin")
        .env("HOME", home.path())
        .output()
        .expect("run provider-backed pnpm multi-bin fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not valid for npm:"), "stderr={stderr}");
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_keep_failed_artifacts_preserves_materialized_workspace() {
    let temp = workspace_tempdir("provider-npm-keep-failed-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-npm-keep-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let server = spawn_npm_registry_server(vec![demo_npm_multi_bin_fixture()]);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("--keep-failed-artifacts")
        .arg("npm:demo-npm-multi-bin")
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .output()
        .expect("run provider-backed npm keep-failed fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Kept failed provider-backed workspace for debugging"),
        "stderr={stderr}"
    );

    let provider_runs = provider_runs_root(home.path());
    let retained = fs::read_dir(&provider_runs)
        .expect("read provider runs dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1, "retained={retained:?}");

    let workspace = &retained[0];
    assert!(!workspace.join("package.json").exists());
    assert!(!workspace.join("package-lock.json").exists());
    assert!(
        !workspace.join("resolution.json").exists(),
        "multi-bin failure should stop before final resolution metadata is written"
    );
    assert!(!workspace.join("ato.lock.json").exists());

    fs::remove_dir_all(workspace).expect("cleanup retained provider workspace");
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_keep_failed_artifacts_preserves_resolution_metadata() {
    let temp = workspace_tempdir("provider-pnpm-keep-failed-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-pnpm-keep-home-");
    let poisoned_path = workspace_tempdir("provider-pnpm-keep-path-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    write_poison_node_shims(poisoned_path.path());
    let server = spawn_npm_registry_server(vec![demo_npm_single_bin_fixture()]);

    let input_path = caller_dir.join("input.txt");
    fs::write(&input_path, "hello from pnpm failure fixture\n").expect("write input");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("--keep-failed-artifacts")
        .arg("npm:demo-npm-single-bin")
        .arg("--")
        .args(["./input.txt", "-o", "./missing/output.json"])
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .env("PATH", prepend_path(poisoned_path.path()))
        .output()
        .expect("run provider-backed npm keep-failed fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Kept failed provider-backed workspace for debugging"),
        "stderr={stderr}"
    );

    let provider_runs = provider_runs_root(home.path());
    let retained = fs::read_dir(&provider_runs)
        .expect("read provider runs dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1, "retained={retained:?}");

    let metadata_path = retained[0].join("resolution.json");
    assert!(
        metadata_path.exists(),
        "metadata should exist at {}",
        metadata_path.display()
    );

    let metadata: Value =
        serde_json::from_slice(&fs::read(&metadata_path).expect("read resolution metadata"))
            .expect("parse resolution metadata");
    assert_eq!(
        metadata["requested_provider_toolchain"].as_str(),
        Some("auto")
    );
    assert_eq!(
        metadata["effective_provider_toolchain"].as_str(),
        Some("npm")
    );
    assert_eq!(metadata["provider"].as_str(), Some("npm"));
    assert!(retained[0].join("package.json").exists());
    assert!(retained[0].join("ato.lock.json").exists());

    fs::remove_dir_all(&retained[0]).expect("cleanup retained provider workspace");
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_no_bin_fails_closed() {
    let temp = workspace_tempdir("provider-npm-no-bin-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-npm-no-bin-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let server = spawn_npm_registry_server(vec![demo_npm_no_bin_fixture()]);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("npm:demo-npm-no-bin")
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .output()
        .expect("run provider-backed npm no-bin fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not expose a CLI bin entrypoint"),
        "stderr={stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_via_pnpm_no_bin_fails_closed() {
    let temp = workspace_tempdir("provider-pnpm-no-bin-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-pnpm-no-bin-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let server = spawn_npm_registry_server(vec![demo_npm_no_bin_fixture()]);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("--via")
        .arg("pnpm")
        .arg("npm:demo-npm-no-bin")
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .env("PNPM_HOME", home.path().join(".pnpm-home"))
        .env("PNPM_STORE_DIR", home.path().join(".pnpm-store"))
        .output()
        .expect("run provider-backed pnpm no-bin fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not valid for npm:"), "stderr={stderr}");
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_npm_run_install_script_package_fails_closed() {
    let temp = workspace_tempdir("provider-npm-install-script-");
    let caller_dir = temp.path().join("caller");
    let home = workspace_tempdir("provider-npm-install-script-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    let server = spawn_npm_registry_server(vec![demo_npm_needs_install_script_fixture()]);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("npm:demo-npm-needs-install-script")
        .env("HOME", home.path())
        .env("NPM_CONFIG_REGISTRY", &server.base_url)
        .env("npm_config_registry", &server.base_url)
        .env("NPM_CONFIG_CACHE", home.path().join(".npm-cache"))
        .output()
        .expect("run provider-backed npm install-script fixture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("declares install lifecycle scripts"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("--ignore-scripts"), "stderr={stderr}");
}
