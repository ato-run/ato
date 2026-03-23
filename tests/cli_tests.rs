#![allow(deprecated)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn run_init_in(dir: &std::path::Path) -> String {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(dir)
        .arg("init")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn write_static_publish_project(dir: &Path, name: &str, version: &str) {
    fs::create_dir_all(dir.join("dist")).expect("create dist");
    fs::write(
        dir.join("capsule.toml"),
        format!(
            r#"schema_version = "0.2"
name = "{name}"
version = "{version}"
type = "app"
default_target = "site"

[targets.site]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 4173
"#
        ),
    )
    .expect("write manifest");
    fs::write(
        dir.join("dist").join("index.html"),
        format!("<!doctype html><title>{name}</title>"),
    )
    .expect("write html");
}

fn build_capsule_for_test(project_dir: &Path, name: &str) -> PathBuf {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(project_dir)
        .args(["build", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact = project_dir.join(format!("{name}.capsule"));
    assert!(
        artifact.exists(),
        "missing artifact: {}",
        artifact.display()
    );
    artifact
}

struct MockGitHubArchiveServer {
    base_url: String,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for MockGitHubArchiveServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock GitHub archive server thread");
        }
    }
}

fn build_github_tarball(root: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{root}/{path}"),
                std::io::Cursor::new(contents.as_bytes()),
            )
            .expect("append tar entry");
    }
    builder
        .into_inner()
        .expect("finish tar builder")
        .finish()
        .expect("finish gzip encoder");
    bytes
}

fn build_github_tarball_with_global_pax_header(root: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::XGlobalHeader);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            "pax_global_header",
            std::io::Cursor::new(Vec::<u8>::new()),
        )
        .expect("append pax global header");

    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{root}/{path}"),
                std::io::Cursor::new(contents.as_bytes()),
            )
            .expect("append tar entry");
    }

    builder
        .into_inner()
        .expect("finish tar builder")
        .finish()
        .expect("finish gzip encoder");
    bytes
}

fn spawn_github_archive_server(
    expected_path: &'static str,
    archive: Vec<u8>,
) -> MockGitHubArchiveServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let addr = listener.local_addr().expect("listener addr");

    // Single-connection mock server: sufficient for the current install flow, which fetches one
    // tarball over one HTTP connection per test invocation.
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut request = [0u8; 4096];
        let size = stream.read(&mut request).expect("read request");
        let request_text = String::from_utf8_lossy(&request[..size]);
        let (status_line, response_body, content_type) =
            if request_text.starts_with(&format!("GET {expected_path} ")) {
                ("HTTP/1.1 200 OK", archive, "application/gzip")
            } else {
                (
                    "HTTP/1.1 404 Not Found",
                    b"{\"error\":\"not found\"}".to_vec(),
                    "application/json",
                )
            };
        let response = format!(
            "{status_line}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .and_then(|_| stream.write_all(&response_body))
            .expect("write response");
    });

    MockGitHubArchiveServer {
        base_url: format!("http://{}", addr),
        handle: Some(handle),
    }
}

fn extract_manifest_from_archive(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let entries = archive.entries().unwrap();
    for entry in entries {
        let mut entry = entry.unwrap();
        let entry_path = entry.path().unwrap().to_string_lossy().into_owned();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry.read_to_string(&mut manifest).unwrap();
            return manifest;
        }
    }
    panic!(
        "capsule.toml not found in installed archive: {}",
        path.display()
    );
}

fn create_fake_node_dir() -> tempfile::TempDir {
    let dir = tempdir().expect("fake node tempdir");
    #[cfg(windows)]
    let node_path = dir.path().join("node.cmd");
    #[cfg(not(windows))]
    let node_path = dir.path().join("node");

    #[cfg(windows)]
    fs::write(&node_path, "@echo off\r\nexit /B 0\r\n").expect("write fake node");
    #[cfg(not(windows))]
    {
        fs::write(&node_path, "#!/bin/sh\nexit 0\n").expect("write fake node");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&node_path)
            .expect("fake node metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&node_path, permissions).expect("chmod fake node");
    }

    dir
}

fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).expect("join PATH entries")
}

#[test]
fn test_cli_help() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Primary Commands:"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("finalize"))
        .stdout(predicate::str::contains("project"))
        .stdout(predicate::str::contains("unproject"));
}

#[test]
fn test_cli_version() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("ato"));
}

#[test]
fn test_cli_invalid_command() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("invalid-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_help_hides_legacy_commands() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(" open ").not())
        .stdout(predicate::str::contains(" pack ").not())
        .stdout(predicate::str::contains(" close ").not())
        .stdout(predicate::str::contains(" auth ").not())
        .stdout(predicate::str::contains(" setup ").not());
}

#[test]
fn test_ps_command_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("ps")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("List running capsules"));
}

#[test]
fn test_stop_command_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("stop")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Stop a running capsule"));
}

#[test]
fn test_logs_command_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("logs")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Show logs of a running capsule"));
}

#[test]
fn test_login_help_shows_optional_token() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["login", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--token <TOKEN>"))
        .stdout(predicate::str::contains("[OPTIONS]").or(predicate::str::contains("Options:")));
}

#[test]
fn test_search_help_uses_store_api_default() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tag <TAGS>"))
        .stdout(predicate::str::contains(
            "Registry URL (default: https://api.ato.run)",
        ));
}

#[test]
fn test_install_help_shows_from_gh_repo() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--from-gh-repo <REPOSITORY>"));
}

#[test]
fn test_install_rejects_slug_with_from_gh_repo() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args([
        "install",
        "koh0920/sample-capsule",
        "--from-gh-repo",
        "github.com/Koh0920/ato-cli",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("--from-gh-repo"));
}

#[test]
fn test_install_rejects_registry_with_from_gh_repo() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args([
        "install",
        "--from-gh-repo",
        "github.com/Koh0920/ato-cli",
        "--registry",
        "http://127.0.0.1:8080",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "--registry cannot be used with --from-gh-repo",
    ));
}

#[test]
fn test_install_rejects_version_with_from_gh_repo() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args([
        "install",
        "--from-gh-repo",
        "github.com/Koh0920/ato-cli",
        "--version",
        "1.2.3",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "--version cannot be used with --from-gh-repo",
    ));
}

#[test]
fn test_install_from_gh_repo_without_manifest_uses_zero_config_build_fallback() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("installed");
    let runtime_root = tmp.path().join("runtime");
    let fake_node = create_fake_node_dir();
    let archive = build_github_tarball(
        "Koh0920-demo-repo-a1b2c3",
        &[("index.js", "console.log('hello from zero config');\n")],
    );
    let server = spawn_github_archive_server("/repos/Koh0920/demo-repo/tarball", archive);

    let assert = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .env("ATO_GITHUB_API_BASE_URL", &server.base_url)
        .env("ATO_RUNTIME_ROOT", &runtime_root)
        .env("PATH", prepend_path(fake_node.path()))
        .args([
            "install",
            "--from-gh-repo",
            "https://github.com/Koh0920/demo-repo",
            "--output",
        ])
        .arg(&output_dir)
        .args(["--yes", "--no-project"])
        .assert();

    assert.success();

    let installed = output_dir
        .join("koh0920")
        .join("demo-repo")
        .join("0.1.0")
        .join("demo-repo-0.1.0.capsule");
    assert!(
        installed.exists(),
        "installed artifact missing: {}",
        installed.display()
    );

    let manifest = extract_manifest_from_archive(&installed);
    assert!(
        manifest.contains("name = \"demo-repo\""),
        "manifest={manifest}"
    );
    assert!(
        manifest.contains("entrypoint = \"index.js\""),
        "manifest={manifest}"
    );
}

#[test]
fn test_install_from_gh_repo_accepts_host_path_and_metadata_archive() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("installed");
    let runtime_root = tmp.path().join("runtime");
    let fake_node = create_fake_node_dir();
    let archive = build_github_tarball_with_global_pax_header(
        "Koh0920-demo-repo-a1b2c3",
        &[("index.js", "console.log('hello from host path');\n")],
    );
    let server = spawn_github_archive_server("/repos/Koh0920/demo-repo/tarball", archive);

    let assert = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .env("ATO_GITHUB_API_BASE_URL", &server.base_url)
        .env("ATO_RUNTIME_ROOT", &runtime_root)
        .env("PATH", prepend_path(fake_node.path()))
        .args([
            "install",
            "--from-gh-repo",
            "github.com/Koh0920/demo-repo",
            "--output",
        ])
        .arg(&output_dir)
        .args(["--yes", "--no-project"])
        .assert();

    assert.success();

    let installed = output_dir
        .join("koh0920")
        .join("demo-repo")
        .join("0.1.0")
        .join("demo-repo-0.1.0.capsule");
    assert!(
        installed.exists(),
        "installed artifact missing: {}",
        installed.display()
    );
}

#[test]
fn test_init_help_describes_agent_prompt_output() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Analyze the current project and print an agent-ready capsule.toml prompt",
        ))
        .stdout(predicate::str::contains("Usage: ato init"))
        .stdout(predicate::str::contains("<NAME>").not());
}

#[test]
fn test_init_outputs_agent_prompt_for_next_project_without_writing_manifest() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "name": "demo-next-app",
  "private": true,
  "dependencies": {
    "next": "^15.0.0",
    "react": "^19.0.0"
  },
  "scripts": {
    "dev": "next dev",
    "build": "next build",
    "start": "next start"
  }
}"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("app")).unwrap();
    fs::create_dir_all(tmp.path().join("public")).unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(
        stdout.contains("Generated an agent-ready prompt for capsule.toml creation."),
        "stdout={stdout}"
    );
    assert!(stdout.contains("Next.js"), "stdout={stdout}");
    assert!(
        stdout.contains("static export (`out/`) or a dynamic server"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("schema_version = \"0.2\""),
        "stdout={stdout}"
    );
    assert!(stdout.contains("```toml"), "stdout={stdout}");
    assert!(!tmp.path().join("capsule.toml").exists());
}

#[test]
fn test_init_detects_astro_project_and_config_facts() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "dependencies": {
    "astro": "^4.0.0"
  },
  "scripts": {
    "build": "astro build"
  }
}"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("astro.config.mjs"),
        "export default { output: 'static' };",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("dist")).unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Astro"), "stdout={stdout}");
    assert!(stdout.contains("astro.config.mjs"), "stdout={stdout}");
    assert!(stdout.contains("dist"), "stdout={stdout}");
}

#[test]
fn test_init_detects_nuxt_ambiguity() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "dependencies": {
    "nuxt": "^3.0.0"
  }
}"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("nuxt.config.ts"),
        "export default defineNuxtConfig({});",
    )
    .unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Nuxt"), "stdout={stdout}");
    assert!(
        stdout.contains("static generate build or a server deployment"),
        "stdout={stdout}"
    );
}

#[test]
fn test_init_detects_react_vite_static_facts() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "dependencies": {
    "react": "^19.0.0",
    "vite": "^5.0.0"
  },
  "scripts": {
    "build": "vite build",
    "preview": "vite preview"
  }
}"#,
    )
    .unwrap();
    fs::write(tmp.path().join("vite.config.ts"), "export default {};").unwrap();
    fs::create_dir_all(tmp.path().join("dist")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/main.ts"), "console.log('hi');").unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("React + Vite"), "stdout={stdout}");
    assert!(stdout.contains("vite.config.ts"), "stdout={stdout}");
    assert!(stdout.contains("`dist`"), "stdout={stdout}");
    assert!(stdout.contains("pure Vite static build"), "stdout={stdout}");
}

#[test]
fn test_init_detects_express_server_entry_hints() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "dependencies": {
    "express": "^4.0.0"
  },
  "scripts": {
    "start": "node dist/server.js"
  }
}"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("dist")).unwrap();
    fs::write(tmp.path().join("dist/server.js"), "console.log('server');").unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Express"), "stdout={stdout}");
    assert!(stdout.contains("dist/server.js"), "stdout={stdout}");
    assert!(stdout.contains("`npm start`"), "stdout={stdout}");
}

#[test]
fn test_init_detects_tauri_native_facts_and_ambiguity() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(
        tmp.path()
            .join("src-tauri/target/release/bundle/macos/sample.app"),
    )
    .unwrap();
    fs::write(
        tmp.path().join("src-tauri/Cargo.toml"),
        "[package]\nname = \"sample-tauri\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(tmp.path().join("tauri.conf.json"), "{}").unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Tauri"), "stdout={stdout}");
    assert!(stdout.contains("src-tauri/Cargo.toml"), "stdout={stdout}");
    assert!(stdout.contains("tauri.conf.json"), "stdout={stdout}");
    assert!(stdout.contains("sample.app"), "stdout={stdout}");
    assert!(
        stdout.contains("embedded in the desktop app"),
        "stdout={stdout}"
    );
}

#[test]
fn test_init_detects_electron_native_ambiguity() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "main": "electron/main.js",
  "devDependencies": {
    "electron": "^30.0.0",
    "electron-builder": "^24.0.0"
  }
}"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("electron-builder.yml"),
        "appId: com.example.demo\n",
    )
    .unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Electron"), "stdout={stdout}");
    assert!(stdout.contains("electron-builder.yml"), "stdout={stdout}");
    assert!(
        stdout.contains("packaged desktop artifact"),
        "stdout={stdout}"
    );
}

#[test]
fn test_init_detects_fastapi_and_module_ambiguity() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("requirements.txt"), "fastapi\nuvicorn\n").unwrap();
    fs::write(
        tmp.path().join("server.py"),
        "from fastapi import FastAPI\napp = FastAPI()\n",
    )
    .unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("FastAPI"), "stdout={stdout}");
    assert!(
        stdout.contains("uvicorn module:app")
            && stdout.contains("which module and app object should be used"),
        "stdout={stdout}"
    );
}

#[test]
fn test_init_detects_plain_rust_binary_facts() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-rust\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    let stdout = run_init_in(tmp.path());
    assert!(stdout.contains("Rust binary"), "stdout={stdout}");
    assert!(stdout.contains("Cargo.toml"), "stdout={stdout}");
    assert!(stdout.contains("src/main.rs"), "stdout={stdout}");
}

#[test]
fn test_fetch_help_shows_registry_and_version() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["fetch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("localhost:8080/slug:version"))
        .stdout(predicate::str::contains("--registry <REGISTRY>"))
        .stdout(predicate::str::contains("--version <VERSION>"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_finalize_help_shows_required_contract() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["finalize", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Path to fetched artifact directory",
        ))
        .stdout(predicate::str::contains("--allow-external-finalize"))
        .stdout(predicate::str::contains("--output-dir <OUTPUT_DIR>"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_project_help_shows_launcher_projection_contract() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["project", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Add a finalized app to launcher surfaces (experimental)",
        ))
        .stdout(predicate::str::contains("ato finalize"))
        .stdout(predicate::str::contains("--launcher-dir <LAUNCHER_DIR>"))
        .stdout(predicate::str::contains("Commands:"))
        .stdout(predicate::str::contains(
            "ls    List experimental projection state",
        ));
}

#[test]
fn test_project_ls_help_mentions_broken_projection_detection() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["project", "ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("broken projections"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_unproject_help_shows_projection_reference_contract() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["unproject", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Remove an experimental launcher projection without mutating the finalized artifact",
        ))
        .stdout(predicate::str::contains("Projection ID"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_fetch_accepts_subcommand_json_flag() {
    let tmp = tempdir().unwrap();
    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "fetch",
            "koh0920/does-not-exist",
            "--json",
            "--registry",
            "http://127.0.0.1:9",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("unexpected argument '--json'"),
        "stderr={stderr}"
    );
}

#[test]
fn test_inspect_requirements_help_shows_json_and_registry() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["inspect", "requirements", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Inspect runtime requirements from capsule.toml",
        ))
        .stdout(predicate::str::contains("publisher/slug"))
        .stdout(predicate::str::contains("--registry <REGISTRY>"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_finalize_accepts_subcommand_json_flag() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("dist");
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args([
            "finalize",
            tmp.path().to_str().unwrap(),
            "--json",
            "--output-dir",
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("unexpected argument '--json'"),
        "stderr={stderr}"
    );
}

fn write_native_build_fixture(
    root: &std::path::Path,
    executable: bool,
    include_delivery_sidecar: bool,
) {
    fs::create_dir_all(root.join("MyApp.app/Contents/MacOS")).unwrap();
    fs::write(
        root.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
    )
    .unwrap();
    if include_delivery_sidecar {
        fs::write(
            root.join("ato.delivery.toml"),
            r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "MyApp.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "MyApp.app"]
"#,
        )
        .unwrap();
    }
    let binary = root.join("MyApp.app/Contents/MacOS/MyApp");
    fs::write(&binary, b"#!/bin/sh\necho native\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(if executable { 0o755 } else { 0o644 });
        fs::set_permissions(&binary, permissions).unwrap();
    }
    #[cfg(not(unix))]
    let _ = executable;
}

fn write_native_command_build_fixture(root: &std::path::Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."
"#,
    )
    .unwrap();
    fs::write(
        root.join("ato.delivery.toml"),
        r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/MyApp.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/MyApp.app"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("build-app.sh"),
        "#!/bin/sh\nset -eu\nmkdir -p dist/MyApp.app/Contents/MacOS\nprintf '#!/bin/sh\necho native\n' > dist/MyApp.app/Contents/MacOS/MyApp\nchmod 755 dist/MyApp.app/Contents/MacOS/MyApp\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(root.join("build-app.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(root.join("build-app.sh"), permissions).unwrap();
    }
}

fn write_inline_native_command_build_fixture(root: &std::path::Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "time-management-desktop"
version = "0.1.0"
description = "Tauri desktop app for time management"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/time-management-desktop.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/time-management-desktop.app"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("build-app.sh"),
        "#!/bin/sh\nset -eu\nmkdir -p src-tauri/target/release/bundle/macos/time-management-desktop.app/Contents/MacOS\nprintf '#!/bin/sh\necho native\n' > src-tauri/target/release/bundle/macos/time-management-desktop.app/Contents/MacOS/time-management-desktop\nchmod 755 src-tauri/target/release/bundle/macos/time-management-desktop.app/Contents/MacOS/time-management-desktop\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(root.join("build-app.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(root.join("build-app.sh"), permissions).unwrap();
    }
}

#[test]
fn test_build_routes_native_delivery_projects() {
    let tmp = tempdir().unwrap();
    write_native_build_fixture(tmp.path(), true, true);
    let mut cmd = Command::cargo_bin("ato").unwrap();
    let output = cmd
        .args(["--json", "build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if cfg!(target_os = "macos") {
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let derived_from = tmp.path().canonicalize().unwrap().join("MyApp.app");
        assert!(
            stdout.contains("\"build_strategy\": \"native-delivery\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("\"schema_version\": \"0.1\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("\"target\": \"darwin/arm64\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "\"derived_from\": \"{}\"",
                derived_from.to_string_lossy()
            )),
            "stdout:\n{stdout}"
        );
        assert!(stdout.contains("\"artifact\": "), "stdout:\n{stdout}");
    } else {
        assert!(
            !output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined
                .contains("native delivery build currently supports macOS and Windows hosts only"),
            "combined output:\n{combined}"
        );
    }
}

#[test]
fn test_build_routes_native_delivery_projects_without_delivery_sidecar() {
    let tmp = tempdir().unwrap();
    write_native_build_fixture(tmp.path(), true, false);
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["--json", "build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if cfg!(target_os = "macos") {
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("\"build_strategy\": \"native-delivery\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("\"schema_version\": \"0.1\""),
            "stdout:\n{stdout}"
        );
    } else {
        assert!(
            !output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined
                .contains("native delivery build currently supports macOS and Windows hosts only"),
            "combined output:\n{combined}"
        );
    }
}

#[test]
fn test_build_strict_v3_non_app_native_target_keeps_strict_v3_error() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("source")).unwrap();
    fs::write(
        tmp.path().join("capsule.toml"),
        r#"schema_version = "0.2"
name = "strict-v3-ci-check"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "source/main.py"
"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("source/main.py"),
        "print('strict v3 check')\n",
    )
    .unwrap();

    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["build", tmp.path().to_str().unwrap(), "--strict-v3"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("strict-v3")
            || stderr.contains("Strict v3 fallback is not allowed")
            || stderr.contains("strict-manifest")
            || stderr.contains("source_digest is missing")
            || stderr.contains("E102"),
        "stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Native delivery target 'cli' entrypoint must point to a .app bundle"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn test_build_routes_native_delivery_command_mode_projects() {
    let tmp = tempdir().unwrap();
    write_native_command_build_fixture(tmp.path());
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["--json", "build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if cfg!(target_os = "macos") {
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let derived_from = tmp.path().canonicalize().unwrap().join("dist/MyApp.app");
        assert!(
            stdout.contains("\"build_strategy\": \"native-delivery\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("\"schema_version\": \"0.1\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "\"derived_from\": \"{}\"",
                derived_from.to_string_lossy()
            )),
            "stdout:\n{stdout}"
        );
        assert!(
            tmp.path()
                .join("dist/MyApp.app/Contents/MacOS/MyApp")
                .exists(),
            "built app should exist after command-mode native build"
        );
    } else {
        assert!(
            !output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined
                .contains("native delivery build currently supports macOS and Windows hosts only"),
            "combined output:\n{combined}"
        );
    }
}

#[test]
fn test_build_routes_inline_native_delivery_command_mode_projects() {
    let tmp = tempdir().unwrap();
    write_inline_native_command_build_fixture(tmp.path());
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["--json", "build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if cfg!(target_os = "macos") {
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let derived_from = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("src-tauri/target/release/bundle/macos/time-management-desktop.app");
        assert!(
            stdout.contains("\"build_strategy\": \"native-delivery\""),
            "stdout:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "\"derived_from\": \"{}\"",
                derived_from.to_string_lossy()
            )),
            "stdout:\n{stdout}"
        );
    } else {
        assert!(
            !output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined
                .contains("native delivery build currently supports macOS and Windows hosts only"),
            "combined output:\n{combined}"
        );
    }
}

#[test]
fn test_install_help_shows_native_projection_controls() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--yes"))
        .stdout(predicate::str::contains("--project"))
        .stdout(predicate::str::contains("--no-project"))
        .stdout(predicate::str::contains("local finalize / projection"));
}

#[test]
fn test_project_accepts_subcommand_json_flag() {
    let tmp = tempdir().unwrap();
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["project", tmp.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("unexpected argument '--json'"),
        "stderr={stderr}"
    );
}

#[test]
fn test_unproject_accepts_subcommand_json_flag() {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["unproject", "missing-projection", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("unexpected argument '--json'"),
        "stderr={stderr}"
    );
}

#[test]
fn test_finalize_requires_opt_in_flag() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("dist");

    let output = Command::cargo_bin("ato")
        .unwrap()
        .args([
            "finalize",
            tmp.path().to_str().unwrap(),
            "--output-dir",
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--allow-external-finalize"),
        "stderr={stderr}"
    );
}

#[test]
fn test_run_command_accepts_default_path() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required").not());
}

#[test]
fn test_run_help_shows_yes_flag() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("github.com/owner/repo"))
        .stdout(predicate::str::contains("--skill <SKILL>").not())
        .stdout(predicate::str::contains("--yes"))
        .stdout(predicate::str::contains("--registry"))
        .stdout(predicate::str::contains("default: https://api.ato.run"));
}

#[test]
fn test_run_rejects_noncanonical_github_url_input() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "https://github.com/Koh0920/demo-repo", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "ato run github.com/Koh0920/demo-repo",
        ));
}

#[test]
fn test_run_requires_yes_or_tty_for_github_repo_install() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "github.com/Koh0920/demo-repo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Interactive install confirmation requires a TTY",
        ));
}

#[test]
fn test_run_rejects_removed_skill_flags() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "--from-skill", "/tmp/SKILL.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--from-skill"));
}

#[test]
fn test_run_json_missing_manifest_requires_yes() {
    let tmp = tempdir().unwrap();

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["--json", "run"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema_version"], "1");
    assert_eq!(value["status"], "error");
    assert!(value["error"]["message"]
        .as_str()
        .expect("message string")
        .contains("requires -y/--yes"));
    assert!(!tmp.path().join("capsule.toml").exists());
}

#[test]
fn test_open_alias_is_removed() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["open", "."])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn test_build_command_with_init_flag() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("build")
        .arg("--init")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Initialize capsule.toml interactively",
        ));
}

#[test]
fn test_build_command_with_key_flag() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("build")
        .arg("--key")
        .arg("/path/to/key")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Path to signing key"));
}

#[test]
fn test_json_flag_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("--json")
        .arg("ps")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Emit machine-readable JSON output",
        ));
}

#[test]
fn test_publish_command_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["publish", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Publish capsule"))
        .stdout(predicate::str::contains("--registry <REGISTRY>"))
        .stdout(predicate::str::contains("--prepare"))
        .stdout(predicate::str::contains("--build"))
        .stdout(predicate::str::contains("--deploy"))
        .stdout(predicate::str::contains("Select Prepare as the stop point"))
        .stdout(predicate::str::contains("Select Verify as the stop point"))
        .stdout(predicate::str::contains(
            "Start at Verify using an existing .capsule artifact",
        ))
        .stdout(predicate::str::contains("--ci"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--no-tui"));
}

#[test]
fn test_registry_command_is_public() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["registry", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("serve"));
}

#[test]
fn test_registry_serve_help_has_auth_token() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["registry", "serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--auth-token <AUTH_TOKEN>"));
}

#[test]
fn test_key_command_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["key", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage signing keys"));
}

#[test]
fn test_config_engine_install_exists() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["config", "engine", "install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Download and install an engine"));
}

#[test]
fn test_legacy_setup_still_available() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Engine name to install"));
}

#[test]
fn test_build_invalid_manifest_outputs_single_json_error() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("capsule.toml"), "name =\n").unwrap();

    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["--json", "build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "unexpected stdout: {}", stdout);

    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(value["schema_version"], "1");
    assert_eq!(value["status"], "error");
    assert_eq!(value["error"]["code"], "E001");
}

#[test]
fn test_publish_json_invalid_artifact_prepare_range_uses_diagnostic_envelope() {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args([
            "publish",
            "--json",
            "--artifact",
            "demo.capsule",
            "--prepare",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema_version"], "1");
    assert_eq!(value["status"], "error");
    assert_eq!(value["error"]["code"], "E999");
    assert!(value["error"]["message"]
        .as_str()
        .expect("message string")
        .contains("cannot be combined"));
}

#[test]
fn test_publish_json_artifact_build_reports_six_phase_matrix() {
    let tmp = tempdir().unwrap();
    let build_dir = tmp.path().join("build-project");
    let publish_dir = tmp.path().join("publish-cwd");
    fs::create_dir_all(&build_dir).unwrap();
    fs::create_dir_all(&publish_dir).unwrap();
    write_static_publish_project(&build_dir, "phase-matrix-demo", "1.0.0");
    let artifact = build_capsule_for_test(&build_dir, "phase-matrix-demo");

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(&publish_dir)
        .args([
            "publish",
            "--json",
            "--artifact",
            artifact.to_string_lossy().as_ref(),
            "--build",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let phases = value["phases"].as_array().expect("phases array");
    assert_eq!(phases.len(), 6, "unexpected phases payload: {value}");
    let phase_names: Vec<&str> = phases
        .iter()
        .map(|phase| phase["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        phase_names,
        vec!["prepare", "build", "verify", "install", "dry_run", "publish"]
    );
    assert_eq!(phases[2]["status"], "ok");
    assert_eq!(phases[3]["status"], "skipped");
    assert_eq!(phases[4]["status"], "skipped");
    assert_eq!(phases[5]["status"], "skipped");
}

#[test]
fn test_publish_json_missing_manifest_uses_diagnostic_envelope() {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["publish", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema_version"], "1");
    assert_eq!(value["status"], "error");
    assert_eq!(value["error"]["code"], "E999");
    assert!(value["error"]["message"]
        .as_str()
        .expect("message string")
        .contains("capsule.toml not found in current directory"));
}

#[test]
fn test_publish_legacy_full_publish_rejected_for_private_registry() {
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args([
            "publish",
            "--json",
            "--legacy-full-publish",
            "--registry",
            "http://127.0.0.1:8787",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["error"]["code"], "E999");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("--legacy-full-publish is only available for official registry publish"));
}

#[test]
fn test_publish_phase_flags_conflict_with_ci_mode() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["publish", "--ci", "--deploy"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_source_rebuild_help_uses_ref_flag() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["source", "rebuild", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--ref <REFERENCE>"));
}

#[test]
fn test_source_rebuild_accepts_reference_alias() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args([
        "source",
        "rebuild",
        "--source-id",
        "src_123",
        "--reference",
        "main",
        "--registry",
        "http://127.0.0.1:9",
    ])
    .assert()
    .failure()
    .stderr(
        predicate::str::contains("Failed to preflight source operation").or(
            predicate::str::contains("Source operation requires authentication"),
        ),
    );
}
