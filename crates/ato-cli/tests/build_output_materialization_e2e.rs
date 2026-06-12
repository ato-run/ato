use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use capsule_core::blob::{BlobManifest, hash_tree};
use capsule_core::common::store::BlobAddress;
use serde_json::Value;
use serial_test::serial;

#[test]
#[serial]
#[cfg(unix)]
fn no_build_projects_cached_build_output_layer_before_run() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold run", &first);
    assert!(workspace.join("dist/run.sh").exists());

    fs::remove_dir_all(workspace.join("dist")).context("remove cold build output")?;
    let second = run_ato(
        &workspace,
        &home,
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_success("warm run", &second);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("build-output-layer-ok"),
        "warm stdout:\n{}\nwarm stderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(workspace.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn no_build_hard_fails_when_build_output_payload_is_corrupted() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold run", &first);
    let blob_hash = output_layer_blob_hash(&workspace)?;

    fs::remove_dir_all(workspace.join("dist")).context("remove cold build output")?;
    corrupt_payload(&home, &blob_hash)?;

    let second = run_ato(
        &workspace,
        &home,
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;

    assert_failure("warm no-build", &second);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("failed to project required build output layer"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("payload hash mismatch"),
        "stderr:\n{stderr}"
    );
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn normal_run_falls_back_to_local_build_when_projection_verification_fails() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold run", &first);
    let blob_hash = output_layer_blob_hash(&workspace)?;

    fs::remove_dir_all(workspace.join("dist")).context("remove cold build output")?;
    corrupt_payload(&home, &blob_hash)?;

    let second = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;

    assert_success("warm normal run", &second);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("build-output-layer-ok"),
        "warm stdout:\n{}\nwarm stderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("failed to project build output layer; trying remote materialization"),
        "stderr:\n{stderr}"
    );
    assert!(workspace.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn no_build_hard_fails_when_build_output_manifest_mismatches() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold run", &first);
    let blob_hash = output_layer_blob_hash(&workspace)?;

    fs::remove_dir_all(workspace.join("dist")).context("remove cold build output")?;
    corrupt_manifest(&home, &blob_hash)?;

    let second = run_ato(
        &workspace,
        &home,
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;

    assert_failure("warm no-build", &second);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("failed to project required build output layer"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("manifest hash mismatch"),
        "stderr:\n{stderr}"
    );
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn remote_export_then_remote_lookup_satisfies_no_build() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home_a = root.join("home-a");
    let workspace_a = root.join("workspace-a");
    let home_b = root.join("home-b");
    let workspace_b = root.join("workspace-b");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;
    copy_fixture(&workspace_a)?;
    copy_fixture(&workspace_b)?;

    let first = run_ato(
        &workspace_a,
        &home_a,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold source run", &first);
    export_output_layer_to_remote(&workspace_a, &home_a, &remote)?;

    let second = run_ato_with_remote(
        &workspace_b,
        &home_b,
        Some(&remote),
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_success("remote no-build run", &second);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("build-output-layer-ok"),
        "remote stdout:\n{}\nremote stderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(workspace_b.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn worker_sim_produces_remote_layer_consumed_by_lookup() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    let worker_workspace = root.join("worker");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    ato_cli::artifact_build_worker_sim::run_fixture_file_backed_artifact_build_worker(
        &fixture_dir(),
        &worker_workspace,
        &remote,
    )?;

    let output = run_ato_with_remote(
        &workspace,
        &home,
        Some(&remote),
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_success("worker remote no-build run", &output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("build-output-layer-ok"),
        "worker remote stdout:\n{}\nworker remote stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn remote_export_imports_to_local_cas() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home_a = root.join("home-a");
    let workspace_a = root.join("workspace-a");
    let home_b = root.join("home-b");
    let workspace_b = root.join("workspace-b");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;
    copy_fixture(&workspace_a)?;
    copy_fixture(&workspace_b)?;

    let first = run_ato(
        &workspace_a,
        &home_a,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold source run", &first);
    export_output_layer_to_remote(&workspace_a, &home_a, &remote)?;

    let remote_run = run_ato_with_remote(
        &workspace_b,
        &home_b,
        Some(&remote),
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_success("remote no-build run", &remote_run);

    fs::remove_dir_all(workspace_b.join("dist")).context("remove remote-projected output")?;
    let local_run = run_ato(
        &workspace_b,
        &home_b,
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_success("local no-build after remote import", &local_run);
    assert!(
        String::from_utf8_lossy(&local_run.stdout).contains("build-output-layer-ok"),
        "local stdout:\n{}\nlocal stderr:\n{}",
        String::from_utf8_lossy(&local_run.stdout),
        String::from_utf8_lossy(&local_run.stderr)
    );
    assert!(workspace_b.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn export_refuses_invalid_existing_remote_dir() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold source run", &first);

    let final_dir = remote_layer_dir(&workspace, &remote)?;
    fs::create_dir_all(&final_dir).context("create invalid remote export dir")?;
    fs::write(
        final_dir.join("layer.json"),
        b"{\"schema_version\":\"corrupt\"}\n",
    )
    .context("write corrupt remote layer")?;

    let error = export_output_layer_to_remote(&workspace, &home, &remote)
        .expect_err("invalid existing remote export must not be overwritten");
    let message = format!("{error:#}");
    assert!(
        message.contains("existing remote build output export"),
        "error:\n{message}"
    );
    assert!(
        fs::read_to_string(final_dir.join("layer.json"))?.contains("corrupt"),
        "existing invalid layer.json must remain untouched"
    );
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn export_is_idempotent_for_valid_existing_remote_dir() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let first = run_ato(
        &workspace,
        &home,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold source run", &first);

    let first_export = export_output_layer_to_remote(&workspace, &home, &remote)?;
    let second_export = export_output_layer_to_remote(&workspace, &home, &remote)?;
    assert_eq!(first_export, second_export);
    assert!(second_export.join("blob/payload/dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn remote_layer_manifest_mismatch_hard_fails_under_no_build() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home_a = root.join("home-a");
    let workspace_a = root.join("workspace-a");
    let home_b = root.join("home-b");
    let workspace_b = root.join("workspace-b");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;
    copy_fixture(&workspace_a)?;
    copy_fixture(&workspace_b)?;

    let first = run_ato(
        &workspace_a,
        &home_a,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold source run", &first);
    let remote_layer = export_output_layer_to_remote(&workspace_a, &home_a, &remote)?;
    corrupt_remote_blob_manifest(&remote_layer)?;

    let second = run_ato_with_remote(
        &workspace_b,
        &home_b,
        Some(&remote),
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_failure("remote no-build mismatch", &second);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("remote build output materialization")
            || stderr.contains("remote materialization"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("hash mismatch"), "stderr:\n{stderr}");
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn normal_run_falls_back_to_local_build_when_remote_lookup_misses() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&remote)?;
    copy_fixture(&workspace)?;

    let output = run_ato_with_remote(
        &workspace,
        &home,
        Some(&remote),
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("normal run with remote miss", &output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("build-output-layer-ok"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace.join("dist/run.sh").exists());
    Ok(())
}

#[test]
#[serial]
#[cfg(unix)]
fn remote_lookup_is_disabled_by_default() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home)?;
    copy_fixture(&workspace)?;

    let output = run_ato(
        &workspace,
        &home,
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_failure("no-build without local or remote materialization", &output);
    assert!(!workspace.join("dist").exists());
    Ok(())
}

/// A symlink injected into a remote CAS payload must be rejected at projection
/// time, even if the hash verifies correctly. Under `--no-build` this must be
/// a hard failure rather than a silent fall-through.
#[test]
#[serial]
#[cfg(unix)]
fn remote_payload_symlink_is_rejected_at_projection() -> Result<()> {
    let root = test_root()?;
    let _cleanup = Cleanup(root.clone());
    let home_a = root.join("home-a");
    let workspace_a = root.join("workspace-a");
    let home_b = root.join("home-b");
    let workspace_b = root.join("workspace-b");
    let remote = root.join("remote-mirror");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;
    copy_fixture(&workspace_a)?;
    copy_fixture(&workspace_b)?;

    // Build once so we have a real CAS blob to export.
    let first = run_ato(
        &workspace_a,
        &home_a,
        &["run", ".", "--yes", "--dangerously-skip-permissions"],
    )?;
    assert_success("cold run", &first);

    // Export the genuine blob to a remote mirror, then tamper with it.
    let remote_layer = export_output_layer_to_remote(&workspace_a, &home_a, &remote)?;
    inject_symlink_into_remote_payload_and_rehash(&remote_layer)?;

    // workspace_b has a fresh home (no local CAS hit) so the remote is the
    // only possible materialization source. Under --no-build the remote lookup
    // must succeed (hash matches) but projection must refuse the symlink.
    let output = run_ato_with_remote(
        &workspace_b,
        &home_b,
        Some(&remote),
        &[
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ],
    )?;
    assert_failure("no-build with symlink in remote payload", &output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "expected symlink rejection in stderr, got:\n{stderr}"
    );
    Ok(())
}

fn run_ato(workspace: &Path, home: &Path, args: &[&str]) -> Result<std::process::Output> {
    run_ato_with_remote(workspace, home, None, args)
}

fn run_ato_with_remote(
    workspace: &Path,
    home: &Path,
    remote: Option<&Path>,
    args: &[&str],
) -> Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato"));
    command
        .args(args)
        .current_dir(workspace)
        .env("HOME", home)
        .env("CAPSULE_ALLOW_UNSAFE", "1");
    match remote {
        Some(remote) => {
            command.env("ATO_PHASE_MATERIALIZATION_REMOTE_ROOT", remote);
        }
        None => {
            command.env_remove("ATO_PHASE_MATERIALIZATION_REMOTE_ROOT");
        }
    }
    command.output().context("run ato")
}

fn assert_success(label: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(label: &str, output: &std::process::Output) {
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn output_layer_blob_hash(workspace: &Path) -> Result<String> {
    output_layer_record(workspace)?
        .get("blob_hash")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("materialization state did not contain output_layer.blob_hash")
}

fn output_layer_record(workspace: &Path) -> Result<Value> {
    let bytes = fs::read(workspace.join(".ato/state/materializations.json"))
        .context("read materializations state")?;
    let value: Value = serde_json::from_slice(&bytes).context("parse materializations state")?;
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|artifacts| artifacts.first())
        .and_then(|artifact| artifact.get("output_layer"))
        .cloned()
        .context("materialization state did not contain output_layer")
}

fn export_output_layer_to_remote(
    workspace: &Path,
    home: &Path,
    remote_root: &Path,
) -> Result<PathBuf> {
    let _guard = HomeGuard::set(home);
    ato_cli::phase_materializer_remote::export_recorded_build_output_layer_to_remote_mirror(
        remote_root,
        workspace,
    )
}

fn remote_layer_dir(workspace: &Path, remote_root: &Path) -> Result<PathBuf> {
    let layer = output_layer_record(workspace)?;
    let materialization_key = layer
        .get("materialization_key")
        .and_then(Value::as_str)
        .context("output_layer.materialization_key missing")?;
    Ok(remote_root
        .join("build-output")
        .join(materialization_key.replace(':', "-")))
}

fn corrupt_remote_blob_manifest(remote_layer_root: &Path) -> Result<()> {
    let path = remote_layer_root.join("blob/manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).context("read manifest")?)
        .context("parse manifest")?;
    value["blob_hash"] = Value::String(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    );
    fs::write(&path, serde_json::to_vec_pretty(&value)?).context("write corrupt manifest")?;
    Ok(())
}

fn corrupt_payload(home: &Path, blob_hash: &str) -> Result<()> {
    let _guard = HomeGuard::set(home);
    let address = BlobAddress::parse(blob_hash).context("parse blob hash")?;
    fs::write(
        address.payload_dir().join("dist/run.sh"),
        b"#!/bin/sh\nprintf 'corrupted\\n'\n",
    )
    .context("corrupt payload")?;
    Ok(())
}

fn corrupt_manifest(home: &Path, blob_hash: &str) -> Result<()> {
    let _guard = HomeGuard::set(home);
    let address = BlobAddress::parse(blob_hash).context("parse blob hash")?;
    let path = address.manifest_path();
    let mut value: Value = serde_json::from_slice(&fs::read(&path).context("read manifest")?)
        .context("parse manifest")?;
    value["blob_hash"] = Value::String(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    );
    fs::write(&path, serde_json::to_vec_pretty(&value)?).context("write corrupt manifest")?;
    Ok(())
}

#[cfg(unix)]
fn inject_symlink_into_remote_payload_and_rehash(remote_layer_root: &Path) -> Result<()> {
    let payload = remote_layer_root.join("blob/payload");
    // Place a dangling absolute symlink inside dist/ — the declared output dir.
    std::os::unix::fs::symlink("/etc/passwd", payload.join("dist/injected-link"))
        .context("inject symlink into remote payload")?;

    // Recompute the tree hash with the symlink present.
    let tree = hash_tree(&payload).context("rehash payload after symlink injection")?;
    let manifest =
        BlobManifest::from_tree_hash(&tree, "test-symlink-injection", "2026-01-01T00:00:00Z");
    manifest
        .write_to(&remote_layer_root.join("blob/manifest.json"))
        .context("write updated manifest.json")?;

    // Update layer.json to reference the new hash so verification doesn't
    // fail at the manifest-check stage before projection is attempted.
    let layer_path = remote_layer_root.join("layer.json");
    let mut layer: Value =
        serde_json::from_slice(&fs::read(&layer_path).context("read layer.json")?)
            .context("parse layer.json")?;
    layer["blob_hash"] = Value::String(tree.blob_hash.clone());
    let mut bytes = serde_json::to_vec_pretty(&layer).context("serialize updated layer.json")?;
    bytes.push(b'\n');
    fs::write(&layer_path, bytes).context("write updated layer.json")?;
    Ok(())
}

fn copy_fixture(workspace: &Path) -> Result<()> {
    crate_copy_dir(&fixture_dir(), workspace)
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("vite-build-output-layer")
}

fn crate_copy_dir(source: &Path, target: &Path) -> Result<()> {
    copy_dir_entries(source, target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let build = target.join("build.sh");
        let mut permissions = fs::metadata(&build)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&build, permissions)?;
    }
    Ok(())
}

fn copy_dir_entries(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

fn test_root() -> Result<PathBuf> {
    let root = std::env::current_dir()?
        .join(".ato")
        .join("test-scratch")
        .join("build-output-materialization-e2e")
        .join(format!("{:016x}", rand::random::<u64>()));
    fs::create_dir_all(&root)?;
    Ok(root)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct HomeGuard {
    prior: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(path: &Path) -> Self {
        let prior = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", path);
        }
        Self { prior }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
