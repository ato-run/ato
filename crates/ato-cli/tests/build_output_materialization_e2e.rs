use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
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
        stderr.contains("failed to project build output layer; build will execute"),
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

fn run_ato(workspace: &Path, home: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_ato"))
        .args(args)
        .current_dir(workspace)
        .env("HOME", home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .output()
        .context("run ato")
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
    let bytes = fs::read(workspace.join(".ato/state/materializations.json"))
        .context("read materializations state")?;
    let value: Value = serde_json::from_slice(&bytes).context("parse materializations state")?;
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|artifacts| artifacts.first())
        .and_then(|artifact| artifact.get("output_layer"))
        .and_then(|layer| layer.get("blob_hash"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("materialization state did not contain output_layer.blob_hash")
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

fn copy_fixture(workspace: &Path) -> Result<()> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("vite-build-output-layer");
    crate_copy_dir(&fixture, workspace)
}

fn crate_copy_dir(source: &Path, target: &Path) -> Result<()> {
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
        std::env::set_var("HOME", path);
        Self { prior }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}
