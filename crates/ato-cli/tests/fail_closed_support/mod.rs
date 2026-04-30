#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tar::Builder;

use capsule_core::execution_plan::derive::compile_execution_plan;
use capsule_core::router::ExecutionProfile;
use sha2::Digest;
use tempfile::TempDir;

pub fn ato_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ato"))
}

pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) {
    if !dst.exists() {
        fs::create_dir_all(dst).expect("failed to create destination fixture directory");
    }

    for entry in fs::read_dir(src).expect("failed to read source fixture directory") {
        let entry = entry.expect("failed to read fixture entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());

        if from.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            fs::copy(&from, &to).expect("failed to copy fixture file");
        }
    }
}

pub fn write_capsule_lock(workspace_root: &Path, fixture_name: &str) {
    let manifest_path = workspace_root.join("capsule.toml");
    let manifest_text = fs::read_to_string(&manifest_path).expect("failed to read manifest");
    let manifest: capsule_core::types::CapsuleManifest =
        toml::from_str(&manifest_text).expect("failed to parse manifest");
    let hash = capsule_core::packers::payload::compute_manifest_hash_without_signatures(&manifest)
        .expect("failed to compute manifest hash");

    let mut lock_content = serde_json::json!({
        "version": "1",
        "meta": {
            "created_at": "2026-02-23T00:00:00Z",
            "manifest_hash": hash,
        },
        "targets": {},
    });

    if fixture_name == "future-glibc-capsule" {
        lock_content["targets"]["x86_64-unknown-linux-gnu"] = serde_json::json!({
            "constraints": { "glibc": "glibc-999.0" }
        });
    }
    if fixture_name == "glibc-mismatch-capsule" {
        lock_content["targets"]["x86_64-unknown-linux-gnu"] = serde_json::json!({
            "constraints": { "glibc": "glibc-2.17" }
        });
    }

    fs::write(
        workspace_root.join("capsule.lock.json"),
        serde_json::to_vec_pretty(&lock_content).expect("serialize capsule.lock.json"),
    )
    .expect("failed to write capsule.lock.json");
}

pub fn prepare_fixture_workspace(fixture_name: &str) -> (TempDir, PathBuf) {
    let source = fixture_dir(fixture_name);
    let temp = TempDir::new().expect("failed to create fixture workspace");
    let workspace_root = temp.path().join(fixture_name);
    copy_dir_recursive(&source, &workspace_root);
    let lock_path = workspace_root.join("capsule.lock.json");
    if lock_path.exists() {
        let manifest_text = fs::read_to_string(workspace_root.join("capsule.toml"))
            .expect("failed to read manifest");
        let manifest: capsule_core::types::CapsuleManifest =
            toml::from_str(&manifest_text).expect("failed to parse manifest");
        let rendered = capsule_core::lockfile::render_lockfile_for_manifest(&lock_path, &manifest)
            .expect("failed to re-render existing capsule.lock.json");
        fs::write(&lock_path, rendered).expect("failed to update existing capsule.lock.json");
    } else {
        write_capsule_lock(&workspace_root, fixture_name);
    }

    if fixture_name == "glibc-mismatch-capsule" {
        write_mock_elf_with_dt_verneed(&workspace_root.join("app.bin"), "GLIBC_2.99");
    }

    (temp, workspace_root)
}

pub fn write_mock_elf_with_dt_verneed(path: &Path, required_glibc: &str) {
    const ELF_HEADER_SIZE: usize = 64;
    const PROGRAM_HEADER_SIZE: usize = 56;
    const PROGRAM_HEADERS: usize = 2;
    const DYNAMIC_OFFSET: usize = 0x100;
    const DYNAMIC_SIZE: usize = 32;
    const STRING_OFFSET: usize = 0x200;
    const DT_VERNEED: u64 = 0x6fff_fffe;
    const FILE_SIZE: usize = 0x280;

    let mut bytes = vec![0u8; FILE_SIZE];
    let file_size_u64 = FILE_SIZE as u64;

    bytes[0] = 0x7f;
    bytes[1] = b'E';
    bytes[2] = b'L';
    bytes[3] = b'F';
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;

    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
    bytes[40..48].copy_from_slice(&0u64.to_le_bytes());
    bytes[48..52].copy_from_slice(&0u32.to_le_bytes());
    bytes[52..54].copy_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
    bytes[54..56].copy_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
    bytes[56..58].copy_from_slice(&(PROGRAM_HEADERS as u16).to_le_bytes());

    let ph0 = ELF_HEADER_SIZE;
    bytes[ph0..ph0 + 4].copy_from_slice(&2u32.to_le_bytes());
    bytes[ph0 + 4..ph0 + 8].copy_from_slice(&0u32.to_le_bytes());
    bytes[ph0 + 8..ph0 + 16].copy_from_slice(&(DYNAMIC_OFFSET as u64).to_le_bytes());
    bytes[ph0 + 16..ph0 + 24].copy_from_slice(&(DYNAMIC_OFFSET as u64).to_le_bytes());
    bytes[ph0 + 24..ph0 + 32].copy_from_slice(&(DYNAMIC_OFFSET as u64).to_le_bytes());
    bytes[ph0 + 32..ph0 + 40].copy_from_slice(&(DYNAMIC_SIZE as u64).to_le_bytes());
    bytes[ph0 + 40..ph0 + 48].copy_from_slice(&(DYNAMIC_SIZE as u64).to_le_bytes());
    bytes[ph0 + 48..ph0 + 56].copy_from_slice(&8u64.to_le_bytes());

    let ph1 = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;
    bytes[ph1..ph1 + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[ph1 + 4..ph1 + 8].copy_from_slice(&5u32.to_le_bytes());
    bytes[ph1 + 8..ph1 + 16].copy_from_slice(&0u64.to_le_bytes());
    bytes[ph1 + 16..ph1 + 24].copy_from_slice(&0u64.to_le_bytes());
    bytes[ph1 + 24..ph1 + 32].copy_from_slice(&0u64.to_le_bytes());
    bytes[ph1 + 32..ph1 + 40].copy_from_slice(&file_size_u64.to_le_bytes());
    bytes[ph1 + 40..ph1 + 48].copy_from_slice(&file_size_u64.to_le_bytes());
    bytes[ph1 + 48..ph1 + 56].copy_from_slice(&0x1000u64.to_le_bytes());

    bytes[DYNAMIC_OFFSET..DYNAMIC_OFFSET + 8].copy_from_slice(&DT_VERNEED.to_le_bytes());
    bytes[DYNAMIC_OFFSET + 8..DYNAMIC_OFFSET + 16]
        .copy_from_slice(&(STRING_OFFSET as u64).to_le_bytes());

    bytes[DYNAMIC_OFFSET + 16..DYNAMIC_OFFSET + 24].copy_from_slice(&0u64.to_le_bytes());
    bytes[DYNAMIC_OFFSET + 24..DYNAMIC_OFFSET + 32].copy_from_slice(&0u64.to_le_bytes());

    let marker = required_glibc.as_bytes();
    let end = STRING_OFFSET + marker.len();
    bytes[STRING_OFFSET..end].copy_from_slice(marker);
    bytes[end] = 0;

    fs::write(path, &bytes).expect("failed to write mock ELF fixture");
}

pub fn prepare_consent_home(fixture_root: &Path) -> TempDir {
    let home = TempDir::new().expect("failed to create temporary HOME");
    let consent_dir = home.path().join(".ato").join("consent");
    fs::create_dir_all(&consent_dir).expect("failed to create consent dir");

    let manifest_path = fixture_root.join("capsule.toml");
    let compiled = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, None)
        .expect("failed to compile execution plan for fixture");
    let plan = compiled.execution_plan;

    let record = serde_json::json!({
        "scoped_id": plan.consent.key.scoped_id,
        "version": plan.consent.key.version,
        "target_label": plan.consent.key.target_label,
        "policy_segment_hash": plan.consent.policy_segment_hash,
        "provisioning_policy_hash": plan.consent.provisioning_policy_hash,
        "approved_at": "2026-02-23T00:00:00Z"
    });

    fs::write(
        consent_dir.join("executionplan_v1.jsonl"),
        format!("{}\n", record),
    )
    .expect("failed to seed consent store");

    home
}

pub fn run_with_seeded_consent(
    fixture_name: &str,
    args: &[&str],
    extra_envs: &[(&str, &str)],
) -> Output {
    let (_workspace, fixture) = prepare_fixture_workspace(fixture_name);
    let home = prepare_consent_home(&fixture);

    let mut cmd = ato_cmd();
    cmd.arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());

    for arg in args {
        cmd.arg(arg);
    }
    for (key, value) in extra_envs {
        cmd.env(key, value);
    }

    cmd.output().expect("failed to execute ato")
}

pub fn run_without_seeded_consent(
    fixture_name: &str,
    args: &[&str],
    extra_envs: &[(&str, &str)],
) -> Output {
    let (_workspace, fixture) = prepare_fixture_workspace(fixture_name);
    let home = TempDir::new().expect("failed to create temporary HOME");

    let mut cmd = ato_cmd();
    cmd.arg("run")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());

    for arg in args {
        cmd.arg(arg);
    }
    for (key, value) in extra_envs {
        cmd.env(key, value);
    }

    cmd.output().expect("failed to execute ato")
}

pub fn find_built_capsule_path(workspace_root: &Path) -> PathBuf {
    let mut found: Vec<PathBuf> = Vec::new();

    let capsule_dir = workspace_root.join(".ato");
    if capsule_dir.is_dir() {
        found.extend(
            fs::read_dir(&capsule_dir)
                .expect("failed to read .ato directory")
                .filter_map(|entry| entry.ok().map(|v| v.path()))
                .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("capsule")),
        );
    }

    found.extend(
        fs::read_dir(workspace_root)
            .expect("failed to read workspace root")
            .filter_map(|entry| entry.ok().map(|v| v.path()))
            .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("capsule")),
    );

    found.sort();
    found
        .into_iter()
        .next()
        .expect("built capsule archive not found")
}

pub fn resolve_test_nacelle_path() -> PathBuf {
    if let Ok(path) = std::env::var("NACELLE_PATH") {
        let nacelle = PathBuf::from(path);
        if nacelle.exists() {
            return nacelle;
        }
    }

    let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../nacelle/target/debug/nacelle")
        .canonicalize()
        .expect("failed to resolve default nacelle path for tests");
    assert!(
        candidate.exists(),
        "nacelle binary not found for tests: {}",
        candidate.display()
    );
    candidate
}

pub fn tamper_lock_manifest_hash(workspace_root: &Path) {
    let lock_path = workspace_root.join("capsule.lock.json");
    let raw = fs::read_to_string(&lock_path).expect("failed to read capsule.lock.json");
    let tampered = raw.replace(
        "\"manifest_hash\": \"blake3:",
        "\"manifest_hash\": \"blake3:deadbeef",
    );
    fs::write(&lock_path, tampered).expect("failed to tamper capsule.lock.json");
}

pub fn add_egress_allow_host(workspace_root: &Path, host: &str) {
    let manifest_path = workspace_root.join("capsule.toml");
    let raw = fs::read_to_string(&manifest_path).expect("failed to read capsule.toml");

    let marker = "egress_allow = [";
    let start = raw
        .find(marker)
        .expect("egress_allow declaration not found in fixture manifest");
    let list_start = start + marker.len();
    let end_rel = raw[list_start..]
        .find(']')
        .expect("egress_allow closing bracket not found");
    let list_end = list_start + end_rel;

    let current = raw[list_start..list_end].trim();
    let quoted_host = format!("\"{}\"", host);
    if current.contains(&quoted_host) {
        return;
    }

    let next_list = if current.is_empty() {
        quoted_host
    } else {
        format!("{}, {}", current, quoted_host)
    };

    let mut updated = String::new();
    updated.push_str(&raw[..list_start]);
    updated.push_str(&next_list);
    updated.push_str(&raw[list_end..]);

    fs::write(&manifest_path, updated).expect("failed to update egress_allow list");
}

#[cfg(unix)]
pub fn write_mock_nacelle_without_sandbox(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
set -eu

if [ "${1:-}" = "internal" ] && [ "${2:-}" = "--input" ] && [ "${3:-}" = "-" ] && [ "${4:-}" = "features" ]; then
    while IFS= read -r _line; do :; done || true
  printf '%s\n' '{"data":{"capabilities":{"sandbox":[]}}}'
  exit 0
fi

echo "unsupported invocation" >&2
exit 2
"#;
    fs::write(path, script).expect("failed to write mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod mock nacelle script");
}

#[cfg(unix)]
pub fn write_mock_uv(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "uv 0.0.0-test"
  exit 0
fi
exit 0
"#;
    fs::write(path, script).expect("failed to write mock uv script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat mock uv script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod mock uv script");
}

#[cfg(unix)]
pub fn write_mock_nacelle(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "internal" ]; then
  shift
  if [ "${1:-}" = "--input" ]; then
    shift
    if [ "${1:-}" = "-" ]; then
      shift
    fi
  fi
  sub="${1:-}"
  cat >/dev/null || true
  case "$sub" in
    features)
      printf '%s\n' '{"ok":true,"spec_version":"0.1.0","engine":{"name":"nacelle","engine_version":"test","platform":"test"},"capabilities":{"workloads":["source","bundle"],"languages":["python"],"sandbox":["mock-sandbox"],"socket_activation":true,"jit_provisioning":true,"ipc_sandbox":true}}'
      ;;
    exec)
      printf '%s\n' '{"ok":true,"spec_version":"0.1.0","pid":1,"log_path":null}'
      ;;
    *)
      echo "unknown internal subcommand: $sub" >&2
      exit 1
      ;;
  esac
  exit 0
fi

echo "mock nacelle only supports internal mode" >&2
exit 1
"#;

    fs::write(path, script).expect("failed to write mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod mock nacelle script");
}

#[cfg(unix)]
pub fn write_mock_nacelle_ready(path: &Path, log_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "internal" ]; then
    shift
    if [ "${{1:-}}" = "--input" ]; then
        shift
        if [ "${{1:-}}" = "-" ]; then
            shift
        fi
    fi
    sub="${{1:-}}"
    cat >/dev/null || true
    case "$sub" in
        features)
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","engine":{{"name":"nacelle","engine_version":"test","platform":"test"}},"capabilities":{{"workloads":["source","bundle"],"languages":["sh"],"sandbox":["mock-sandbox"],"socket_activation":true,"jit_provisioning":true,"ipc_sandbox":true}}}}'
            ;;
        exec)
            : > "{log_path}"
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","pid":{},"log_path":"{log_path}"}}'
            printf '%s\n' '{{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/mock.sock"}}'
            trap 'exit 0' TERM INT
            while true; do sleep 1; done
            ;;
        *)
            echo "unknown internal subcommand: $sub" >&2
            exit 1
            ;;
    esac
    exit 0
fi

echo "mock nacelle only supports internal mode" >&2
exit 1
"#,
        std::process::id(),
        log_path = log_path.display()
    );

    fs::write(path, script).expect("failed to write ready mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat ready mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod ready mock nacelle script");
}

#[cfg(unix)]
pub fn write_mock_nacelle_fail_before_ready(path: &Path, log_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "internal" ]; then
    shift
    if [ "${{1:-}}" = "--input" ]; then
        shift
        if [ "${{1:-}}" = "-" ]; then
            shift
        fi
    fi
    sub="${{1:-}}"
    cat >/dev/null || true
    case "$sub" in
        features)
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","engine":{{"name":"nacelle","engine_version":"test","platform":"test"}},"capabilities":{{"workloads":["source","bundle"],"languages":["sh"],"sandbox":["mock-sandbox"],"socket_activation":true,"jit_provisioning":true,"ipc_sandbox":true}}}}'
            ;;
        exec)
            : > "{log_path}"
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","pid":{},"log_path":"{log_path}"}}'
            printf '%s\n' '{{"event":"service_exited","service":"main","exit_code":42}}'
            exit 42
            ;;
        *)
            echo "unknown internal subcommand: $sub" >&2
            exit 1
            ;;
    esac
    exit 0
fi

echo "mock nacelle only supports internal mode" >&2
exit 1
"#,
        std::process::id(),
        log_path = log_path.display()
    );

    fs::write(path, script).expect("failed to write failure mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat failure mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod failure mock nacelle script");
}

#[cfg(unix)]
pub fn write_mock_nacelle_ready_then_exit(path: &Path, log_path: &Path, exit_code: i32) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "internal" ]; then
    shift
    if [ "${{1:-}}" = "--input" ]; then
        shift
        if [ "${{1:-}}" = "-" ]; then
            shift
        fi
    fi
    sub="${{1:-}}"
    cat >/dev/null || true
    case "$sub" in
        features)
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","engine":{{"name":"nacelle","engine_version":"test","platform":"test"}},"capabilities":{{"workloads":["source","bundle"],"languages":["sh"],"sandbox":["mock-sandbox"],"socket_activation":true,"jit_provisioning":true,"ipc_sandbox":true}}}}'
            ;;
        exec)
            : > "{log_path}"
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","pid":{},"log_path":"{log_path}"}}'
            printf '%s\n' '{{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/mock.sock"}}'
            sleep 1
            printf '%s\n' '{{"event":"service_exited","service":"main","exit_code":{exit_code}}}'
            exit {exit_code}
            ;;
        *)
            echo "unknown internal subcommand: $sub" >&2
            exit 1
            ;;
    esac
    exit 0
fi

echo "mock nacelle only supports internal mode" >&2
exit 1
"#,
        std::process::id(),
        log_path = log_path.display(),
        exit_code = exit_code,
    );

    fs::write(path, script).expect("failed to write ready-then-exit mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat ready-then-exit mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to chmod ready-then-exit mock nacelle script");
}

#[cfg(unix)]
pub fn write_mock_nacelle_starting_then_exit(
    path: &Path,
    log_path: &Path,
    sleep_secs: u64,
    exit_code: i32,
) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "internal" ]; then
    shift
    if [ "${{1:-}}" = "--input" ]; then
        shift
        if [ "${{1:-}}" = "-" ]; then
            shift
        fi
    fi
    sub="${{1:-}}"
    cat >/dev/null || true
    case "$sub" in
        features)
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","engine":{{"name":"nacelle","engine_version":"test","platform":"test"}},"capabilities":{{"workloads":["source","bundle"],"languages":["sh"],"sandbox":["mock-sandbox"],"socket_activation":true,"jit_provisioning":true,"ipc_sandbox":true}}}}'
            ;;
        exec)
            : > "{log_path}"
            printf '%s\n' '{{"ok":true,"spec_version":"0.1.0","pid":{},"log_path":"{log_path}"}}'
            sleep {sleep_secs}
            exit {exit_code}
            ;;
        *)
            echo "unknown internal subcommand: $sub" >&2
            exit 1
            ;;
    esac
    exit 0
fi

echo "mock nacelle only supports internal mode" >&2
exit 1
"#,
        std::process::id(),
        log_path = log_path.display(),
        sleep_secs = sleep_secs,
        exit_code = exit_code,
    );

    fs::write(path, script).expect("failed to write starting-then-exit mock nacelle script");
    let mut perms = fs::metadata(path)
        .expect("failed to stat starting-then-exit mock nacelle script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .expect("failed to chmod starting-then-exit mock nacelle script");
}

#[cfg(unix)]
pub fn host_nacelle_release_archive_name() -> String {
    let target = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else {
        panic!("unsupported target triple for mock nacelle release test");
    };

    format!("nacelle-{target}.tar.xz")
}

#[cfg(unix)]
pub fn write_mock_nacelle_release(root: &Path, version: &str) -> PathBuf {
    let version_dir = root.join(version);
    fs::create_dir_all(&version_dir).expect("failed to create mock release dir");

    let staged_binary_path = version_dir.join("nacelle");
    write_mock_nacelle(&staged_binary_path);

    let archive_name = host_nacelle_release_archive_name();
    let archive_path = version_dir.join(&archive_name);
    let archive_file =
        fs::File::create(&archive_path).expect("failed to create mock release archive");
    let encoder = xz2::write::XzEncoder::new(archive_file, 6);
    let mut archive = Builder::new(encoder);
    archive
        .append_path_with_name(&staged_binary_path, "nacelle")
        .expect("failed to append mock nacelle binary to archive");
    let encoder = archive
        .into_inner()
        .expect("failed to finish mock release tar archive");
    encoder
        .finish()
        .expect("failed to finish mock release xz stream");

    let bytes = fs::read(&archive_path).expect("failed to read mock nacelle release archive");
    let sha256 = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    fs::write(
        version_dir.join(format!("{archive_name}.sha256")),
        format!("{sha256}  {archive_name}\n"),
    )
    .expect("failed to write mock nacelle checksum");

    archive_path
}

pub struct StaticFileServer {
    pub base_url: String,
    hits: Arc<Mutex<std::collections::HashMap<String, usize>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StaticFileServer {
    pub fn request_count(&self, path: &str) -> usize {
        self.hits
            .lock()
            .expect("request count lock poisoned")
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for StaticFileServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn spawn_static_file_server(root: PathBuf) -> StaticFileServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind static file server");
    listener
        .set_nonblocking(true)
        .expect("failed to make static file server nonblocking");
    let addr = listener
        .local_addr()
        .expect("failed to resolve static file server addr");

    let hits = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let hits_thread = Arc::clone(&hits);
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);

    let handle = std::thread::spawn(move || {
        while !shutdown_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let started = Instant::now();
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];

                    while started.elapsed() < Duration::from_secs(2) {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                req.extend_from_slice(&buf[..n]);
                                if req.windows(4).any(|w| w == b"\r\n\r\n") {
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

                    let request = String::from_utf8_lossy(&req);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/")
                        .to_string();

                    {
                        let mut guard = hits_thread.lock().expect("request count lock poisoned");
                        *guard.entry(path.clone()).or_insert(0) += 1;
                    }

                    let relative = path.trim_start_matches('/');
                    let file_path = root.join(relative);
                    let file_path = if file_path.is_dir() {
                        file_path.join("index.html")
                    } else {
                        file_path
                    };
                    if let Ok(body) = fs::read(&file_path) {
                        let content_type = match file_path
                            .extension()
                            .and_then(|value| value.to_str())
                            .unwrap_or_default()
                        {
                            "html" => "text/html; charset=utf-8",
                            "whl" => "application/octet-stream",
                            _ => "application/octet-stream",
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            content_type
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(&body);
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

    StaticFileServer {
        base_url: format!("http://{}", addr),
        hits,
        shutdown,
        handle: Some(handle),
    }
}

pub fn spawn_redirect_server(location: &str) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind redirect server");
    let port = listener
        .local_addr()
        .expect("failed to resolve redirect server addr")
        .port();

    let location = location.to_string();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let started = Instant::now();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];

            while started.elapsed() < Duration::from_secs(2) {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") {
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

            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                location
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (port, handle)
}

pub fn spawn_plain_http_server(body: &str) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind plain http server");
    let port = listener
        .local_addr()
        .expect("failed to resolve plain http server addr")
        .port();
    let payload = body.to_string();

    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let started = Instant::now();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];

            while started.elapsed() < Duration::from_secs(2) {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") {
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

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (port, handle)
}

pub fn extract_policy_violation_target(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if value.get("code").and_then(|v| v.as_str()) != Some("ATO_ERR_POLICY_VIOLATION") {
            continue;
        }
        if let Some(target) = value.get("target").and_then(|v| v.as_str()) {
            return Some(target.to_string());
        }
    }
    None
}

pub fn normalize_host_from_target(target: &str) -> String {
    target
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(target)
        .split(':')
        .next()
        .unwrap_or(target)
        .to_string()
}
