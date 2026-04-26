#![allow(deprecated)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use assert_cmd::Command;
use capsule_core::ato_lock::{
    recompute_lock_id, to_pretty_json, AtoLock, UnresolvedReason, UnresolvedValue,
};
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn top_level_help_hides_internal_surface() {
    let mut cmd = Command::cargo_bin("ato").expect("binary");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Primary Commands:"))
        .stdout(predicate::str::contains("Management:"))
        .stdout(predicate::str::contains("  fetch ").not())
        .stdout(predicate::str::contains("  finalize ").not())
        .stdout(predicate::str::contains("  project ").not())
        .stdout(predicate::str::contains("  unproject ").not())
        .stdout(predicate::str::contains("  key ").not())
        .stdout(predicate::str::contains("  config ").not())
        .stdout(predicate::str::contains("  gen-ci ").not())
        .stdout(predicate::str::contains("  registry ").not())
        .stdout(predicate::str::contains("  search ").not())
        .stdout(predicate::str::contains("  inspect ").not())
        .stdout(predicate::str::contains("  install ").not())
        .stdout(predicate::str::contains("  init ").not())
        .stdout(predicate::str::contains("  build ").not());
}

#[test]
fn hidden_commands_still_support_direct_help() {
    for command in ["fetch", "finalize", "config", "registry"] {
        let mut cmd = Command::cargo_bin("ato").expect("binary");
        cmd.args([command, "--help"]).assert().success();
    }
}

fn write_static_publish_project(dir: &Path, name: &str, version: &str) {
    fs::create_dir_all(dir.join("dist")).expect("create dist");
    fs::write(
        dir.join("capsule.toml"),
        format!(
            r#"schema_version = "0.3"
name = "{name}"
version = "{version}"
type = "app"

runtime = "web/static"
port = 4173
run = "dist""#
        ),
    )
    .expect("write manifest");
    fs::write(
        dir.join("dist").join("index.html"),
        format!("<!doctype html><title>{name}</title>"),
    )
    .expect("write html");
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create destination dir");
    for entry in fs::read_dir(src).expect("read source dir") {
        let entry = entry.expect("sample dir entry");
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let file_type = entry.file_type().expect("sample entry file type");
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &dest_path);
        } else if file_type.is_symlink() {
            let metadata = fs::metadata(&source_path).expect("follow symlink metadata");
            if metadata.is_dir() {
                copy_dir_recursive(&source_path, &dest_path);
            } else if metadata.is_file() {
                fs::copy(&source_path, &dest_path).expect("copy symlinked sample file");
            } else {
                panic!(
                    "unsupported symlinked sample fixture entry: {}",
                    source_path.display()
                );
            }
        } else if file_type.is_file() {
            fs::copy(&source_path, &dest_path).expect("copy sample file");
        } else {
            panic!(
                "unsupported sample fixture entry: {}",
                source_path.display()
            );
        }
    }
}

fn materialize_source_sample(relative_path: &str, dest: &Path) {
    let sample_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("source")
        .join(relative_path);
    assert!(
        sample_root.is_dir(),
        "sample fixture is missing: {}",
        sample_root.display()
    );
    copy_dir_recursive(&sample_root, dest);
}

fn inspect_diagnostics_json(target: &Path) -> serde_json::Value {
    let output = Command::cargo_bin("ato")
        .expect("binary")
        .args([
            "inspect",
            "diagnostics",
            target.to_str().expect("utf-8 target path"),
            "--json",
        ])
        .output()
        .expect("run inspect diagnostics");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("inspect diagnostics json")
}

fn write_canonical_ato_lock(dir: &Path) {
    let mut lock = AtoLock::default();
    lock.resolution.entries.insert(
        "runtime".to_string(),
        serde_json::json!({"kind": "web", "driver": "static"}),
    );
    lock.resolution.entries.insert(
        "resolved_targets".to_string(),
        serde_json::json!([
            {
                "label": "site",
                "runtime": "web",
                "driver": "static",
                "entrypoint": "dist",
                "port": 4173
            }
        ]),
    );
    lock.resolution.entries.insert(
        "closure".to_string(),
        serde_json::json!({
            "kind": "runtime_closure",
            "status": "complete",
            "inputs": []
        }),
    );
    lock.contract.entries.insert(
        "process".to_string(),
        serde_json::json!({"driver": "static", "entrypoint": "dist"}),
    );
    lock.contract.entries.insert(
        "metadata".to_string(),
        serde_json::json!({
            "name": "build-demo",
            "version": "0.1.0",
            "default_target": "site"
        }),
    );
    recompute_lock_id(&mut lock).expect("recompute lock id");
    fs::write(
        dir.join("ato.lock.json"),
        to_pretty_json(&lock).expect("serialize canonical lock"),
    )
    .expect("write canonical lock");
}

fn write_inspect_lock_workspace(dir: &Path) {
    fs::create_dir_all(dir.join(".ato/source-inference")).expect("create source inference dir");
    fs::create_dir_all(dir.join(".ato/binding")).expect("create binding dir");
    fs::write(
        dir.join("package-lock.json"),
        r#"{"name":"inspect-demo","lockfileVersion":3}"#,
    )
    .expect("write observed lockfile");

    let mut lock = AtoLock::default();
    lock.contract.entries.insert(
        "metadata".to_string(),
        serde_json::json!({
            "name": "inspect-demo",
            "version": "0.1.0",
            "default_target": "site"
        }),
    );
    lock.contract.entries.insert(
        "process".to_string(),
        serde_json::json!({
            "driver": "static",
            "entrypoint": "dist"
        }),
    );
    lock.contract.entries.insert(
        "delivery".to_string(),
        serde_json::json!({
            "mode": "source-draft",
            "artifact": {
                "kind": "desktop-native",
                "path": "dist/MyApp.app",
                "canonical_build_input": false,
                "provenance_limited": false,
                "reproducibility": "closure-incomplete-draft"
            },
            "build": {
                "kind": "native-delivery",
                "requires_build_closure": true,
                "closure_status": "incomplete"
            },
            "finalize": {
                "tool": "codesign",
                "args": ["--deep", "--force"],
                "host_local": true
            },
            "install": {
                "kind": "local-derivation",
                "host_local": true,
                "requires_local_derivation": true
            },
            "projection": {
                "kind": "launcher-surface",
                "host_local": true
            }
        }),
    );
    lock.resolution.entries.insert(
        "resolved_targets".to_string(),
        serde_json::json!([
            {
                "label": "site",
                "runtime": "web",
                "driver": "static",
                "entrypoint": "dist"
            }
        ]),
    );
    lock.resolution.entries.insert(
        "closure".to_string(),
        serde_json::json!({
            "kind": "metadata_only",
            "status": "incomplete",
            "observed_lockfiles": ["npm"]
        }),
    );
    lock.resolution.unresolved.push(UnresolvedValue {
        field: Some("resolution.closure".to_string()),
        reason: UnresolvedReason::InsufficientEvidence,
        detail: Some("closure remains metadata-only/incomplete".to_string()),
        candidates: Vec::new(),
    });
    lock.resolution.unresolved.push(UnresolvedValue {
        field: Some("resolution.runtime".to_string()),
        reason: UnresolvedReason::InsufficientEvidence,
        detail: Some("runtime selection remains unresolved".to_string()),
        candidates: Vec::new(),
    });
    lock.binding.unresolved.push(UnresolvedValue {
        field: Some("binding".to_string()),
        reason: UnresolvedReason::DeferredHostLocalBinding,
        detail: Some("host-local state binding is deferred to workspace-local seed".to_string()),
        candidates: Vec::new(),
    });
    lock.policy.unresolved.push(UnresolvedValue {
        field: Some("policy".to_string()),
        reason: UnresolvedReason::PolicyGatedResolution,
        detail: Some(
            "workspace-local policy approval is still required before execution".to_string(),
        ),
        candidates: Vec::new(),
    });
    lock.attestations.unresolved.push(UnresolvedValue {
        field: Some("attestations".to_string()),
        reason: UnresolvedReason::InsufficientEvidence,
        detail: Some("workspace-local attestation evidence has not been recorded yet".to_string()),
        candidates: Vec::new(),
    });
    recompute_lock_id(&mut lock).expect("recompute inspect lock id");
    fs::write(
        dir.join("ato.lock.json"),
        to_pretty_json(&lock).expect("serialize inspect lock"),
    )
    .expect("write inspect lock");

    fs::write(
        dir.join(".ato/source-inference/provenance.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": "init_workspace",
            "input_kind": "canonical_lock",
            "provenance": [
                {
                    "field": "contract.process",
                    "kind": "selection_gate",
                    "source_field": "site",
                    "note": "interactive selection resolved equal-ranked process ambiguity"
                },
                {
                    "field": "resolution.closure",
                    "kind": "importer_observation",
                    "source_path": dir.join("package-lock.json"),
                    "importer_id": "npm",
                    "evidence_kind": "lockfile",
                    "source_field": "npm",
                    "note": "metadata-only/incomplete observed importer evidence"
                },
                {
                    "field": "resolution.runtime",
                    "kind": "deterministic_heuristic",
                    "source_path": dir,
                    "source_field": "project_type",
                    "note": "runtime inference remained unresolved because the source evidence was incomplete"
                }
            ],
            "diagnostics": [
                {
                    "severity": "error",
                    "field": "resolution.runtime",
                    "message": "runtime must be selected before execution"
                }
            ],
            "selection_gate": {
                "field": "contract.process"
            },
            "infer": {
                "unresolved": ["resolution.runtime"]
            },
            "resolve": {
                "unresolved": ["resolution.runtime"]
            }
        }))
        .expect("serialize provenance sidecar"),
    )
    .expect("write provenance sidecar");

    fs::write(
        dir.join(".ato/source-inference/provenance-cache.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "1",
            "input_kind": "canonical_lock",
            "lock_path": dir.join("ato.lock.json"),
            "provenance_path": dir.join(".ato/source-inference/provenance.json"),
            "binding_seed_path": dir.join(".ato/binding/seed.json"),
            "lock_id": lock.lock_id.as_ref().map(|value| value.as_str()),
            "generated_at": null,
            "unresolved": [
                {
                    "field": "resolution.closure",
                    "reason": "insufficient_evidence",
                    "detail": "closure remains metadata-only/incomplete",
                    "candidates": []
                },
                {
                    "field": "resolution.runtime",
                    "reason": "insufficient_evidence",
                    "detail": "runtime selection remains unresolved",
                    "candidates": []
                }
            ],
            "field_index": [
                {
                    "field": "contract.process",
                    "kinds": ["selection_gate"],
                    "notes": ["interactive selection resolved equal-ranked process ambiguity"]
                }
            ],
            "diagnostics_count": 1
        }))
        .expect("serialize provenance cache"),
    )
    .expect("write provenance cache");

    fs::write(
        dir.join(".ato/binding/seed.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "1",
            "lock_path": dir.join("ato.lock.json"),
            "provenance_cache_path": dir.join(".ato/source-inference/provenance-cache.json"),
            "lock_id": lock.lock_id.as_ref().map(|value| value.as_str()),
            "entries": {},
            "unresolved": []
        }))
        .expect("serialize binding seed"),
    )
    .expect("write binding seed");
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

#[allow(dead_code)]
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
        .stdout(predicate::str::contains("Management:"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("decap"))
        .stdout(predicate::str::contains("encap"))
        .stdout(predicate::str::contains("ps"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("logs"))
        .stdout(predicate::str::contains("\n  fetch ").not())
        .stdout(predicate::str::contains("\n  finalize ").not())
        .stdout(predicate::str::contains("\n  project ").not())
        .stdout(predicate::str::contains("\n  unproject ").not())
        .stdout(predicate::str::contains("\n  key ").not())
        .stdout(predicate::str::contains("\n  config ").not())
        .stdout(predicate::str::contains("\n  registry ").not());
}

#[test]
fn test_validate_prefers_canonical_lock_over_manifest() {
    let tmp = tempdir().unwrap();
    write_static_publish_project(tmp.path(), "validate-demo", "0.1.0");
    write_canonical_ato_lock(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["validate", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let canonical_lock_path = fs::canonicalize(tmp.path().join("ato.lock.json")).unwrap();
    assert_eq!(value["authoritative_input"], "canonical_lock");
    assert_eq!(
        value["canonical_lock_path"],
        canonical_lock_path.display().to_string()
    );
    assert!(value.get("manifest_path").is_none());
}

#[test]
fn test_build_prefers_existing_canonical_lock_input() {
    let tmp = tempdir().unwrap();
    write_static_publish_project(tmp.path(), "build-demo", "0.1.0");
    write_canonical_ato_lock(tmp.path());
    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["build", "."])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        tmp.path().join("build-demo.capsule").exists(),
        "missing artifact: {}",
        tmp.path().join("build-demo.capsule").display()
    );
}

#[test]
fn test_inspect_lock_surface_reports_field_statuses() {
    let tmp = tempdir().unwrap();
    write_inspect_lock_workspace(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["inspect", "lock", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let fields = payload
        .get("fields")
        .and_then(|value| value.as_array())
        .expect("fields array");
    let process = fields
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("contract.process")
        })
        .expect("contract.process field");
    assert_eq!(
        process
            .get("userConfirmed")
            .and_then(|value| value.as_bool()),
        Some(true)
    );

    let closure = fields
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("resolution.closure")
        })
        .expect("resolution.closure field");
    assert_eq!(
        closure.get("observed").and_then(|value| value.as_bool()),
        Some(true)
    );
    let closure_provenance = closure
        .get("provenance")
        .and_then(|value| value.as_array())
        .expect("closure provenance");
    assert!(closure_provenance.iter().any(|value| {
        value.get("kind").and_then(|entry| entry.as_str()) == Some("importer_observation")
            && value.get("importerId").and_then(|entry| entry.as_str()) == Some("npm")
            && value.get("evidenceKind").and_then(|entry| entry.as_str()) == Some("lockfile")
    }));
    assert_eq!(
        closure.get("closureKind").and_then(|value| value.as_str()),
        Some("metadata_only")
    );
    assert_eq!(
        closure
            .get("closureStatus")
            .and_then(|value| value.as_str()),
        Some("incomplete")
    );
    assert_eq!(
        closure
            .get("closureDigestable")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    assert_eq!(
        closure.get("fallback").and_then(|value| value.as_bool()),
        Some(true)
    );
    let delivery = fields
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("contract.delivery")
        })
        .expect("contract.delivery field");
    assert_eq!(
        delivery
            .get("deliveryMode")
            .and_then(|value| value.as_str()),
        Some("source-draft")
    );

    let unresolved = payload
        .get("unresolved")
        .and_then(|value| value.as_array())
        .expect("unresolved array");
    let runtime = unresolved
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("resolution.runtime")
        })
        .expect("resolution.runtime unresolved");
    assert_eq!(
        runtime.get("reasonClass").and_then(|value| value.as_str()),
        Some("insufficient_evidence")
    );
}

#[test]
fn test_inspect_preview_surface_reports_durable_and_ephemeral_paths() {
    let tmp = tempdir().unwrap();
    write_inspect_lock_workspace(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["inspect", "preview", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload
            .get("preview")
            .and_then(|value| value.get("durableLockState"))
            .and_then(|value| value.as_str()),
        Some("present")
    );
    let run_outputs = payload
        .get("preview")
        .and_then(|value| value.get("runAttemptMaterialization"))
        .and_then(|value| value.get("outputs"))
        .and_then(|value| value.as_array())
        .expect("run outputs");
    assert!(run_outputs.iter().any(|value| {
        value
            .get("path")
            .and_then(|entry| entry.as_str())
            .map(|entry| entry.contains(".ato/runs/source-inference/<attempt>/ato.lock.json"))
            .unwrap_or(false)
    }));
}

#[test]
fn test_inspect_diagnostics_surface_links_to_inspect_and_preview() {
    let tmp = tempdir().unwrap();
    write_inspect_lock_workspace(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["inspect", "diagnostics", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let diagnostics = payload
        .get("diagnostics")
        .and_then(|value| value.as_array())
        .expect("diagnostics array");
    let runtime = diagnostics
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("resolution.runtime")
        })
        .expect("resolution.runtime diagnostic");
    assert_eq!(
        runtime
            .get("inspectCommand")
            .and_then(|value| value.as_str()),
        Some("ato inspect lock .")
    );
    assert_eq!(
        runtime
            .get("previewCommand")
            .and_then(|value| value.as_str()),
        Some("ato inspect preview .")
    );
}

#[test]
fn test_inspect_remediation_surface_prefers_lock_paths() {
    let tmp = tempdir().unwrap();
    write_inspect_lock_workspace(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["inspect", "remediation", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let suggestions = payload
        .get("suggestions")
        .and_then(|value| value.as_array())
        .expect("suggestions array");
    let runtime = suggestions
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("resolution.runtime")
        })
        .expect("resolution.runtime suggestion");
    assert_eq!(
        runtime.get("reasonClass").and_then(|value| value.as_str()),
        Some("insufficient_evidence")
    );
    assert!(runtime.get("sourceMapping").is_some());

    let binding = suggestions
        .iter()
        .find(|value| value.get("lockPath").and_then(|entry| entry.as_str()) == Some("binding"))
        .expect("binding suggestion");
    assert!(binding
        .get("recommendedAction")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.contains("workspace-local binding seed")));

    let policy = suggestions
        .iter()
        .find(|value| value.get("lockPath").and_then(|entry| entry.as_str()) == Some("policy"))
        .expect("policy suggestion");
    assert!(policy
        .get("recommendedAction")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.contains("does not change lock identity")));

    let attestations = suggestions
        .iter()
        .find(|value| {
            value.get("lockPath").and_then(|entry| entry.as_str()) == Some("attestations")
        })
        .expect("attestations suggestion");
    assert!(attestations
        .get("recommendedAction")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.contains("not part of canonical lock content")));
}

#[test]
fn test_init_rejects_existing_canonical_lock_input() {
    let tmp = tempdir().unwrap();
    write_canonical_ato_lock(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ato.lock.json already exists"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_init_materializes_durable_workspace_state_from_cli() {
    let tmp = tempdir().unwrap();
    fs::write(
        tmp.path().join("package.json"),
        r#"{
  "name": "demo-init",
  "scripts": {
    "start": "node index.js"
  }
}"#,
    )
    .unwrap();
    fs::write(tmp.path().join("index.js"), "console.log('hello');\n").unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["init", "--yes"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join("ato.lock.json").exists());
    assert!(tmp
        .path()
        .join(".ato/source-inference/provenance.json")
        .exists());
    assert!(tmp
        .path()
        .join(".ato/source-inference/provenance-cache.json")
        .exists());
    assert!(tmp.path().join(".ato/binding/seed.json").exists());
    assert!(tmp.path().join(".ato/policy/bundle.json").exists());
    assert!(tmp.path().join(".ato/attestations/store.json").exists());

    let lock: AtoLock =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("ato.lock.json")).unwrap())
            .unwrap();
    assert!(lock.binding.entries.is_empty());
    assert!(lock.attestations.entries.is_empty());

    let binding_seed: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(tmp.path().join(".ato/binding/seed.json")).unwrap(),
    )
    .unwrap();
    assert!(binding_seed.get("entries").is_none());

    let attestation_store: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(tmp.path().join(".ato/attestations/store.json")).unwrap(),
    )
    .unwrap();
    assert!(attestation_store.get("approvals").is_none());
    assert!(attestation_store.get("observations").is_none());

    let gitignore = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".ato/"));
    assert!(gitignore.contains("*.capsule"));
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
fn test_install_from_gh_repo_without_manifest_reports_fail_closed_lockfile_guidance() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("installed");
    let runtime_root = tmp.path().join("runtime");
    let fake_node = create_fake_node_dir();
    let archive = build_github_tarball(
        "Koh0920-demo-repo-a1b2c3",
        &[("index.js", "console.log('hello from zero config');\n")],
    );
    let server = spawn_github_archive_server("/repos/Koh0920/demo-repo/tarball", archive);

    let output = Command::cargo_bin("ato")
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
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Falling back to local zero-config inference"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("Missing native lockfile(s):"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("package-lock.json"), "stderr={stderr}");

    let installed = output_dir
        .join("koh0920")
        .join("demo-repo")
        .join("0.1.0")
        .join("demo-repo-0.1.0.capsule");
    assert!(!installed.exists(), "installed artifact should not exist");
}

#[test]
fn test_install_from_gh_repo_accepts_host_path_and_reports_fail_closed_lockfile_guidance() {
    let tmp = tempdir().unwrap();
    let output_dir = tmp.path().join("installed");
    let runtime_root = tmp.path().join("runtime");
    let fake_node = create_fake_node_dir();
    let archive = build_github_tarball_with_global_pax_header(
        "Koh0920-demo-repo-a1b2c3",
        &[("index.js", "console.log('hello from host path');\n")],
    );
    let server = spawn_github_archive_server("/repos/Koh0920/demo-repo/tarball", archive);

    let output = Command::cargo_bin("ato")
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
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Falling back to local zero-config inference"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("Missing native lockfile(s):"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("package-lock.json"), "stderr={stderr}");

    let installed = output_dir
        .join("koh0920")
        .join("demo-repo")
        .join("0.1.0")
        .join("demo-repo-0.1.0.capsule");
    assert!(!installed.exists(), "installed artifact should not exist");
}

#[test]
fn test_init_help_describes_durable_baseline_output() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Materialize a durable ato.lock.json baseline for a local workspace",
        ))
        .stdout(predicate::str::contains("--legacy <LEGACY>").not())
        .stdout(predicate::str::contains("--yes"))
        .stdout(predicate::str::contains("Usage: ato init"))
        .stdout(predicate::str::contains("<NAME>").not());
}

#[test]
fn test_init_materializes_durable_baseline_for_next_project_without_writing_manifest() {
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

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["init", "--yes"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Created"), "stdout={stdout}");
    assert!(stdout.contains("ato.lock.json"), "stdout={stdout}");
    assert!(tmp.path().join("ato.lock.json").exists());
    assert!(tmp
        .path()
        .join(".ato/source-inference/provenance.json")
        .exists());
    assert!(!tmp.path().join("capsule.toml").exists());
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
        .stdout(predicate::str::contains("Usage: ato project"))
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
        .stdout(predicate::str::contains("Usage: ato unproject"))
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

fn write_native_build_fixture(root: &std::path::Path, executable: bool) {
    fs::create_dir_all(root.join("MyApp.app/Contents/MacOS")).unwrap();
    fs::write(
        root.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "my-app"
version = "0.1.0"
type = "app"

runtime = "source/native"
run = "MyApp.app""#,
    )
    .unwrap();
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
        r#"schema_version = "0.3"
name = "my-app"
version = "0.1.0"
type = "app"

runtime = "source/native"
build = "sh build-app.sh"
working_dir = "."
run = "dist/MyApp.app"
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
        r#"schema_version = "0.3"
name = "time-management-desktop"
version = "0.1.0"
description = "Tauri desktop app for time management"
type = "app"

runtime = "source/native"
build = "sh build-app.sh"
working_dir = "."
run = "src-tauri/target/release/bundle/macos/time-management-desktop.app"
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
    write_native_build_fixture(tmp.path(), true);
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
                .contains("native delivery build currently supports macOS and Windows hosts only")
                || combined.contains("requires current-host target alignment"),
            "combined output:\n{combined}"
        );
    }
}

#[test]
fn test_build_rejects_source_native_delivery_sidecar() {
    let tmp = tempdir().unwrap();
    write_native_build_fixture(tmp.path(), true);
    fs::write(
        tmp.path().join("ato.delivery.toml"),
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

    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("is no longer accepted in source") && stderr.contains("projects"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn test_build_strict_v3_non_app_native_target_keeps_strict_v3_error() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("source")).unwrap();
    fs::write(
        tmp.path().join("capsule.toml"),
        r#"schema_version = "0.3"
name = "strict-v3-ci-check"
version = "0.1.0"
type = "app"

runtime = "source/native"
run = "source/main.py""#,
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
                .contains("native delivery build currently supports macOS and Windows hosts only")
                || combined.contains("requires current-host target alignment"),
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
                .contains("native delivery build currently supports macOS and Windows hosts only")
                || combined.contains("requires current-host target alignment"),
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
        .stdout(predicate::str::contains("pypi:<package>"))
        .stdout(predicate::str::contains("npm:<package>"))
        .stdout(predicate::str::contains("--via <VIA>"))
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
fn test_run_rejects_unknown_provider() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "foo:bar", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider `foo`"))
        .stderr(predicate::str::contains("pypi"))
        .stderr(predicate::str::contains("npm"));
}

#[test]
fn test_run_rejects_provider_sugar_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "pypi/markitdown", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("canonical syntax"))
        .stderr(predicate::str::contains("ato run pypi:markitdown -- ..."));
}

#[test]
fn test_run_rejects_pypi_inline_version_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "pypi:markitdown@0.1.0", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("inline version syntax"))
        .stderr(predicate::str::contains("yet"));
}

#[test]
fn test_run_rejects_pypi_direct_url_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "pypi:https://example.com/demo.whl", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("direct URL,"))
        .stderr(predicate::str::contains("yet"));
}

#[test]
fn test_run_rejects_pypi_vcs_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "pypi:git+https://example.com/demo.git", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("direct URL,"))
        .stderr(predicate::str::contains("yet"));
}

#[test]
fn test_run_rejects_via_for_local_path_targets() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", ".", "--via", "uv", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--via uv"))
        .stderr(predicate::str::contains(
            "only supported for provider-backed targets",
        ));
}

#[test]
fn test_run_rejects_via_uv_for_npm_provider_targets() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "npm:@scope/pkg", "--via", "uv", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`--via uv` is not valid for npm: targets",
        ));
}

#[test]
fn test_run_rejects_npm_inline_version_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "npm:tsx@4.9.0", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support inline versions or dist-tags",
        ));
}

#[test]
fn test_run_rejects_npm_direct_url_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "npm:https://example.com/demo.tgz", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support direct URL, git, or file references",
        ));
}

#[test]
fn test_run_rejects_npm_subpath_syntax() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "npm:@scope/pkg/bin", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support package subpaths",
        ));
}

#[test]
fn test_run_rejects_pnpm_toolchain_for_pypi_targets() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["run", "pypi:markitdown", "--via", "pnpm", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`--via pnpm` is not valid for pypi: targets",
        ))
        .stderr(predicate::str::contains("pypi + uv"));
}

#[test]
fn test_run_rejects_pnpm_toolchain_for_non_provider_targets() {
    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("capsule.toml"),
        r#"schema_version = "0.3"
name = "demo"
version = "0.1.0"

runtime = "node/node"
run = "index.mjs""#,
    )
    .unwrap();
    fs::write(temp.path().join("index.mjs"), "console.log('ok')\n").unwrap();

    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.current_dir(temp.path())
        .args(["run", ".", "--via", "pnpm", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`--via pnpm` is only supported for provider-backed targets",
        ))
        .stderr(predicate::str::contains("ato run npm:<package> -- ..."));
}

#[test]
fn test_install_rejects_provider_target_with_targeted_message() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["install", "pypi:markitdown"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run-only in this MVP"))
        .stderr(predicate::str::contains("pypi:markitdown"))
        .stderr(predicate::str::contains("ato install pypi:markitdown"));
}

#[test]
fn test_install_rejects_npm_provider_target_with_targeted_message() {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.args(["install", "npm:tsx"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run-only in this MVP"))
        .stderr(predicate::str::contains("npm:tsx"))
        .stderr(predicate::str::contains("ato install npm:tsx"));
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
fn test_run_json_missing_manifest_fails_closed_without_generating_manifest() {
    let tmp = tempdir().unwrap();

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["--json", "run"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let payload = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    let value: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(value["code"], "ATO_ERR_AMBIGUOUS_ENTRYPOINT");
    assert!(value["message"]
        .as_str()
        .expect("message string")
        .contains("selected process"));
    assert!(!tmp.path().join("capsule.toml").exists());
}

#[test]
fn test_inspect_diagnostics_tauri_minimal_has_no_diagnostics() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("tauri-minimal");
    materialize_source_sample("native-desktop/tauri/minimal", &project_dir);

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert!(diagnostics.is_empty(), "diagnostics={diagnostics:?}");
}

#[test]
fn test_inspect_diagnostics_tauri_with_lockfiles_has_no_diagnostics() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("tauri-with-lockfiles");
    materialize_source_sample("native-desktop/tauri/with-lockfiles", &project_dir);

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert!(diagnostics.is_empty(), "diagnostics={diagnostics:?}");
}

#[test]
fn test_inspect_diagnostics_tauri_ambiguous_surfaces_contract_gap() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("tauri-ambiguous");
    materialize_source_sample("native-desktop/tauri/ambiguous", &project_dir);

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert!(diagnostics.iter().any(|value| {
        value["lockPath"].as_str() == Some("contract")
            && value["reasonClass"].as_str() == Some("insufficient_evidence")
    }));
    assert!(diagnostics.iter().any(|value| {
        value["lockPath"].as_str() == Some("contract.process")
            && value["message"]
                .as_str()
                .is_some_and(|message| message.contains("could not determine a runnable process"))
    }));
}

#[test]
fn test_inspect_diagnostics_electron_minimal_reports_generic_closure_gap() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("electron-minimal");
    materialize_source_sample("native-desktop/electron/minimal", &project_dir);

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics.iter().any(|value| {
        value["lockPath"].as_str() == Some("resolution.closure")
            && value["reasonClass"].as_str() == Some("incomplete_closure")
            && value["message"]
                .as_str()
                .is_some_and(|message| message.contains("metadata_only"))
    }));
}

#[test]
fn test_inspect_diagnostics_wails_minimal_has_no_diagnostics() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("wails-minimal");
    materialize_source_sample("native-desktop/wails/minimal", &project_dir);

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert!(diagnostics.is_empty(), "diagnostics={diagnostics:?}");
}

#[test]
fn test_inspect_diagnostics_artifact_import_bundle_shape_has_no_diagnostics() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("macos-app-bundle-shape");
    materialize_source_sample(
        "native-desktop/artifact-import/macos-app-bundle-shape",
        &project_dir,
    );

    let payload = inspect_diagnostics_json(&project_dir);
    let diagnostics = payload["diagnostics"]
        .as_array()
        .expect("diagnostics array");

    assert!(diagnostics.is_empty(), "diagnostics={diagnostics:?}");
}

#[test]
fn test_legacy_open_subcommand_is_rejected() {
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
        .success();
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
        .stdout(predicate::str::contains("Usage: ato key <COMMAND>"))
        .stdout(predicate::str::contains("gen"))
        .stdout(predicate::str::contains("sign"))
        .stdout(predicate::str::contains("verify"));
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
#[serial_test::serial]
fn test_publish_json_invalid_artifact_prepare_range_uses_diagnostic_envelope() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("demo.capsule"), b"not a real capsule").unwrap();
    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "publish",
            "--json",
            "--artifact",
            "demo.capsule",
            "--prepare",
            "--registry",
            "http://127.0.0.1:9",
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
#[serial_test::serial]
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
            "--registry",
            "http://127.0.0.1:9",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
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
fn test_publish_json_failure_uses_diagnostic_envelope() {
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
    assert!(value["error"]["code"].as_str().is_some());
    assert!(!value["error"]["message"]
        .as_str()
        .expect("message string")
        .is_empty());
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

/// Regression test: `ato run .` on a directory project must NOT create
/// `<cwd>/.ato/tmp/source-inference/` run-attempt directories.
///
/// Before the fix (USE_HOME_RUN_STATE), `use_global_run_state` was `false` for
/// directory projects, causing attempt-<nanos>/ dirs to accumulate in cwd.
///
/// This test validates the display path emitted by `inspect preview` always
/// points to `~/.ato/runs/source-inference/` regardless of whether the input
/// is a canonical lock, compatibility project, or source-only directory.
#[test]
fn test_inspect_preview_run_attempt_path_uses_home_not_cwd() {
    let tmp = tempdir().unwrap();
    write_inspect_lock_workspace(tmp.path());

    let output = Command::cargo_bin("ato")
        .unwrap()
        .current_dir(tmp.path())
        .args(["inspect", "preview", ".", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let run_outputs = payload
        .get("preview")
        .and_then(|v| v.get("runAttemptMaterialization"))
        .and_then(|v| v.get("outputs"))
        .and_then(|v| v.as_array())
        .expect("run outputs array");

    // All run-attempt paths must point to ~/.ato/runs/, never to .ato/tmp/
    for entry in run_outputs {
        if let Some(path) = entry.get("path").and_then(|v| v.as_str()) {
            assert!(
                !path.contains(".ato/tmp/source-inference"),
                "run-attempt path points to cwd (regression): {path}"
            );
        }
    }

    // At least one path should reference the home runs directory
    assert!(
        run_outputs.iter().any(|v| v
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.contains(".ato/runs/source-inference"))
            .unwrap_or(false)),
        "no run-attempt path under ~/.ato/runs/source-inference/ found"
    );
}
