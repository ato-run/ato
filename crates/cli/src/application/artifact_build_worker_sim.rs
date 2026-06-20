//! File-backed local harness for artifact build producer requests.
//!
//! This module is intentionally not a remote submit surface. It copies a local
//! source fixture into a disposable worker workspace, runs the build phase
//! there, and exports the produced output layer through the existing remote
//! mirror writer.

#![allow(dead_code)] // The worker harness is exercised by its focused slice.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::application::artifact_build_producer::{
    ArtifactBuildProducerProvenance, ArtifactBuildProducerRequest, ArtifactBuildProducerResponse,
    ArtifactBuildProducerStatus, ArtifactBuildSourceRef, ArtifactPlatformProfile,
    artifact_build_request_from_observation, validate_artifact_build_producer_request,
};
use crate::application::build_materialization::{
    BuildObservation, build_observation_toolchain_fingerprint, observe,
};
use crate::application::phase_materializer::{
    build_output_contract_for_observation, capture_build_outputs,
    materialization_key_for_observation, materialization_key_path_component,
};
use crate::application::phase_materializer_remote::{
    RemoteBuildOutputProvenance, export_build_output_layer_to_remote_mirror,
};

const WORKER_SOURCE_DIR: &str = "source";
const WORKER_ATO_HOME_DIR: &str = "ato-home";

#[derive(Debug, Clone)]
pub(crate) struct ArtifactBuildWorkerSimOptions {
    pub(crate) source_root: PathBuf,
    pub(crate) command_runner: ArtifactBuildCommandRunner,
    pub(crate) provenance_created_by: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum ArtifactBuildCommandRunner {
    #[default]
    LocalShell,
}

impl ArtifactBuildCommandRunner {
    fn run_build(
        self,
        command_identity: &str,
        source_root: &Path,
        worker_workspace: &Path,
        _allowed_env_keys: &[String],
    ) -> Result<()> {
        let child_home = worker_workspace.join("home");
        let child_tmp = worker_workspace.join("tmp");
        fs::create_dir_all(&child_home)
            .with_context(|| format!("failed to create {}", child_home.display()))?;
        fs::create_dir_all(&child_tmp)
            .with_context(|| format!("failed to create {}", child_tmp.display()))?;

        let mut command = match self {
            Self::LocalShell => local_shell_command(command_identity),
        };
        command
            .current_dir(source_root)
            .env_clear()
            .env("HOME", &child_home)
            .env("TMPDIR", &child_tmp);
        set_minimal_path(&mut command);
        // allowed_env_keys are contract metadata only in the local simulation.
        // Future worker infra must supply explicit sanitized env values; we
        // must never read host std::env by key name here.

        let output = command.output().with_context(|| {
            format!("failed to run artifact build command '{command_identity}'")
        })?;
        if !output.status.success() {
            anyhow::bail!(
                "artifact build command '{}' failed with {}\nstdout:\n{}\nstderr:\n{}",
                command_identity,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
}

pub(crate) fn run_file_backed_artifact_build_worker(
    request: &ArtifactBuildProducerRequest,
    worker_workspace: &Path,
    remote_root: &Path,
    options: ArtifactBuildWorkerSimOptions,
) -> Result<ArtifactBuildProducerResponse> {
    validate_artifact_build_producer_request(request)?;
    prepare_worker_workspace(&options.source_root, worker_workspace)?;

    let worker_source_root = worker_workspace.join(WORKER_SOURCE_DIR);
    copy_source_tree(&options.source_root, &worker_source_root)?;
    let observation = observation_for_worker_source(request, &worker_source_root)?;

    options.command_runner.run_build(
        &request.build_command_identity,
        &worker_source_root,
        worker_workspace,
        &request.policy.allowed_env_keys,
    )?;

    let worker_ato_home = worker_workspace.join(WORKER_ATO_HOME_DIR);
    fs::create_dir_all(&worker_ato_home)
        .with_context(|| format!("failed to create {}", worker_ato_home.display()))?;
    let _ato_home = ScopedAtoHome::set(&worker_ato_home);
    let output_layer = capture_build_outputs(&worker_source_root, &observation)?
        .context("artifact build worker did not produce every declared build output")?;
    export_build_output_layer_to_remote_mirror(
        remote_root,
        &observation,
        &output_layer,
        RemoteBuildOutputProvenance::file_mirror_export(&options.provenance_created_by),
    )
    .context("failed to export artifact build worker output to remote mirror")?;

    Ok(ArtifactBuildProducerResponse {
        schema_version: request.schema_version.clone(),
        artifact_build_id: request.artifact_build_id.clone(),
        materialization_key: request.materialization_key.clone(),
        status: ArtifactBuildProducerStatus::Produced,
        output_layer: Some(output_layer),
        remote_layer_ref: Some(format!(
            "build-output/{}",
            materialization_key_path_component(&request.materialization_key)
        )),
        provenance: ArtifactBuildProducerProvenance {
            kind: "file-backed-local-worker-simulation".to_string(),
            producer: options.provenance_created_by,
        },
        build_log_ref: None,
        warnings: Vec::new(),
    })
}

/// Integration-test bridge for the v0 local source fixture harness.
pub fn run_fixture_file_backed_artifact_build_worker(
    source_root: &Path,
    worker_workspace: &Path,
    remote_root: &Path,
) -> Result<String> {
    let toolchain_identity = fixture_toolchain_identity();
    let observation = fixture_observation_for_source(source_root, &toolchain_identity)?;
    let request = artifact_build_request_from_observation(
        &observation,
        fixture_source_ref(),
        "blake3:worker-fixture-source".to_string(),
        "blake3:worker-fixture-recipe".to_string(),
        None,
        toolchain_identity,
        fixture_platform_profile(),
    )?;
    let response = run_file_backed_artifact_build_worker(
        &request,
        worker_workspace,
        remote_root,
        ArtifactBuildWorkerSimOptions {
            source_root: source_root.to_path_buf(),
            command_runner: ArtifactBuildCommandRunner::LocalShell,
            provenance_created_by: "ato-cli-worker-sim-test".to_string(),
        },
    )?;
    Ok(response.materialization_key)
}

fn observation_for_worker_source(
    request: &ArtifactBuildProducerRequest,
    worker_source_root: &Path,
) -> Result<BuildObservation> {
    let manifest = read_source_manifest(worker_source_root)?;
    let mut candidates = Vec::new();
    if let Some(observation) = observe_manifest(
        &manifest,
        worker_source_root,
        &request.target_label,
        &request.toolchain_identity,
        None,
    )? {
        candidates.push(observation);
    }

    let mut compatibility_manifest = manifest.clone();
    if let Some(table) = compatibility_manifest.as_table_mut() {
        table.remove("build");
    }
    if let Some(observation) = observe_manifest(
        &compatibility_manifest,
        worker_source_root,
        &request.target_label,
        &request.toolchain_identity,
        Some(&request.build_command_identity),
    )? {
        candidates.push(observation);
    }

    for observation in candidates {
        if observation_matches_request(&observation, request)? {
            return Ok(observation);
        }
    }

    anyhow::bail!(
        "artifact build request does not match copied source build observation candidates"
    )
}

fn observation_matches_request(
    observation: &BuildObservation,
    request: &ArtifactBuildProducerRequest,
) -> Result<bool> {
    if observation.command != request.build_command_identity {
        return Ok(false);
    }
    let (output_contract_digest, outputs) = build_output_contract_for_observation(observation)?;
    if output_contract_digest != request.output_contract_digest || outputs != request.outputs {
        return Ok(false);
    }
    let materialization_key = materialization_key_for_observation(observation)?;
    if materialization_key != request.materialization_key {
        return Ok(false);
    }
    Ok(true)
}

fn fixture_observation_for_source(
    source_root: &Path,
    toolchain_identity: &str,
) -> Result<BuildObservation> {
    let mut manifest = read_source_manifest(source_root)?;
    let build_command = manifest
        .get("build")
        .and_then(|build| build.get("command"))
        .and_then(toml::Value::as_str)
        .context("artifact build worker fixture does not declare [build].command")?
        .to_string();
    if let Some(table) = manifest.as_table_mut() {
        table.remove("build");
    }
    observe_manifest(
        &manifest,
        source_root,
        "main",
        toolchain_identity,
        Some(&build_command),
    )?
    .context("artifact build worker fixture did not produce a compatibility observation")
}

fn read_source_manifest(source_root: &Path) -> Result<toml::Value> {
    let manifest_path = source_root.join("capsule.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    toml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))
}

fn observe_manifest(
    manifest: &toml::Value,
    source_root: &Path,
    target_label: &str,
    toolchain_identity: &str,
    legacy_build_command: Option<&str>,
) -> Result<Option<BuildObservation>> {
    observe(
        manifest,
        target_label,
        source_root,
        source_root,
        legacy_build_command,
        toolchain_identity,
        |_| None,
    )
}

fn prepare_worker_workspace(source_root: &Path, worker_workspace: &Path) -> Result<()> {
    let canonical_source = source_root.canonicalize().with_context(|| {
        format!(
            "artifact build worker source root {} is not readable",
            source_root.display()
        )
    })?;
    if !canonical_source.is_dir() {
        anyhow::bail!(
            "artifact build worker source root {} is not a directory",
            source_root.display()
        );
    }

    let workspace_path = absolute_path(worker_workspace)?;
    if workspace_path == canonical_source || workspace_path.starts_with(&canonical_source) {
        anyhow::bail!("artifact build worker workspace must stay outside the source root");
    }

    if worker_workspace.exists() {
        if !worker_workspace.is_dir() {
            anyhow::bail!(
                "artifact build worker workspace {} is not a directory",
                worker_workspace.display()
            );
        }
        if worker_workspace
            .read_dir()
            .with_context(|| format!("failed to read {}", worker_workspace.display()))?
            .next()
            .is_some()
        {
            anyhow::bail!(
                "artifact build worker workspace {} must be empty",
                worker_workspace.display()
            );
        }
    } else {
        fs::create_dir_all(worker_workspace)
            .with_context(|| format!("failed to create {}", worker_workspace.display()))?;
    }
    Ok(())
}

fn copy_source_tree(source_root: &Path, target_root: &Path) -> Result<()> {
    fs::create_dir_all(target_root)
        .with_context(|| format!("failed to create {}", target_root.display()))?;
    for entry in WalkDir::new(source_root).follow_links(false).min_depth(1) {
        let entry = entry.with_context(|| format!("failed to walk {}", source_root.display()))?;
        let relative = entry.path().strip_prefix(source_root).with_context(|| {
            format!(
                "source entry {} escaped {}",
                entry.path().display(),
                source_root.display()
            )
        })?;
        let target = target_root.join(relative);
        if entry.file_type().is_symlink() {
            anyhow::bail!(
                "artifact build worker source fixture must not contain symlink {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy worker source {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else {
            anyhow::bail!(
                "artifact build worker source fixture has unsupported entry {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .context("failed to resolve artifact build worker workspace path")?
        .join(path))
}

#[cfg(unix)]
fn local_shell_command(command_identity: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(command_identity);
    command
}

#[cfg(windows)]
fn local_shell_command(command_identity: &str) -> Command {
    // `/D /S /C` via raw_arg: `/D` keeps a broken AutoRun script from
    // polluting output and leaking exit codes; `/S` + raw_arg keep
    // operators and quoting verbatim.
    crate::common::host_shell::windows_cmd_shell_command(command_identity)
}

#[cfg(unix)]
fn set_minimal_path(command: &mut Command) {
    command.env("PATH", "/usr/bin:/bin");
}

#[cfg(windows)]
fn set_minimal_path(command: &mut Command) {
    command.env("PATH", r"C:\Windows\System32");
}

fn fixture_source_ref() -> ArtifactBuildSourceRef {
    ArtifactBuildSourceRef::PublicGitHubCommit {
        repo: "ato-run/worker-sim-fixture".to_string(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
    }
}

fn fixture_toolchain_identity() -> String {
    build_observation_toolchain_fingerprint("source/native", "unknown", "main")
}

fn fixture_platform_profile() -> ArtifactPlatformProfile {
    ArtifactPlatformProfile {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        abi: "local-worker-sim".to_string(),
        libc_or_runtime_abi: None,
        native_addon_boundary: None,
        display: None,
    }
}

struct ScopedAtoHome {
    prior: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl ScopedAtoHome {
    fn set(path: &Path) -> Self {
        let guard = ato_home_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let prior = std::env::var_os("ATO_HOME");
        // SAFETY: `ato_home_lock` serializes all `ScopedAtoHome` instances, and
        // this fixture worker runs the build synchronously on one thread while
        // holding the guard, so no other thread reads or writes `ATO_HOME`
        // within the guard's scope.
        unsafe {
            std::env::set_var("ATO_HOME", path);
        }
        Self {
            prior,
            _guard: guard,
        }
    }
}

impl Drop for ScopedAtoHome {
    fn drop(&mut self) {
        // SAFETY: the lock guard held by this `ScopedAtoHome` is still alive
        // here, so the restore is serialized against every other instance and
        // no other thread accesses `ATO_HOME` concurrently.
        match self.prior.take() {
            Some(value) => unsafe { std::env::set_var("ATO_HOME", value) },
            None => unsafe { std::env::remove_var("ATO_HOME") },
        }
    }
}

fn ato_home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serial_test::serial;
    use tempfile::TempDir;

    use crate::application::phase_materializer_remote::lookup_remote_build_output_layer;

    #[test]
    #[serial]
    fn worker_sim_does_not_run_app_phase() {
        let temp = TempDir::new().expect("temp");
        let mut request = fixture_request();
        request.policy.run_phase_allowed = true;
        let worker_workspace = temp.path().join("worker");

        let error = run_worker(&request, &worker_workspace, &temp.path().join("remote"))
            .expect_err("run permission must be rejected");

        assert!(error.to_string().contains("run-phase"), "{error:#}");
        assert!(!worker_workspace.exists());
    }

    #[test]
    #[serial]
    fn worker_sim_rejects_persistent_state() {
        let temp = TempDir::new().expect("temp");
        let mut request = fixture_request();
        request.policy.persistent_state_allowed = true;
        let worker_workspace = temp.path().join("worker");

        let error = run_worker(&request, &worker_workspace, &temp.path().join("remote"))
            .expect_err("persistent state must be rejected");

        assert!(error.to_string().contains("persistent state"), "{error:#}");
        assert!(!worker_workspace.exists());
    }

    // The four run_worker tests below drive the sh-script fixtures that
    // emulate the (Linux) remote build worker contract; on Windows the sim's
    // minimal System32 PATH has no `sh`, so they are unix-only by design.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn worker_sim_uses_disposable_workspace() {
        let temp = TempDir::new().expect("temp");
        let request = fixture_request();
        let worker_workspace = temp.path().join("worker");

        run_worker(&request, &worker_workspace, &temp.path().join("remote")).expect("worker");

        assert!(worker_workspace.join("source/dist/run.sh").exists());
        assert!(!fixture_source_root().join("dist").exists());
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn worker_sim_exports_v1_remote_layout() {
        let temp = TempDir::new().expect("temp");
        let request = fixture_request();
        let remote_root = temp.path().join("remote");
        let response =
            run_worker(&request, &temp.path().join("worker"), &remote_root).expect("worker");
        let key_path = materialization_key_path_component(&response.materialization_key);
        let remote_layer = remote_root.join("build-output").join(key_path);
        let lookup_workspace = temp.path().join("lookup-source");
        copy_source_tree(&fixture_source_root(), &lookup_workspace).expect("copy lookup source");
        let observation =
            fixture_observation_for_source(&lookup_workspace, &fixture_toolchain_identity())
                .expect("lookup observation");
        let lookup_home = temp.path().join("lookup-ato-home");
        fs::create_dir_all(&lookup_home).expect("lookup home");
        let _ato_home = ScopedAtoHome::set(&lookup_home);
        let _remote_root =
            ScopedEnv::set_path("ATO_PHASE_MATERIALIZATION_REMOTE_ROOT", &remote_root);

        let layer = lookup_remote_build_output_layer(&lookup_workspace, &observation)
            .expect("remote lookup")
            .expect("remote layer");

        assert!(remote_layer.join("layer.json").exists());
        assert!(remote_layer.join("blob/manifest.json").exists());
        assert!(remote_layer.join("blob/payload").exists());
        assert_eq!(layer.materialization_key, response.materialization_key);
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn worker_sim_response_does_not_create_install_revision() {
        let temp = TempDir::new().expect("temp");
        let response = run_worker(
            &fixture_request(),
            &temp.path().join("worker"),
            &temp.path().join("remote"),
        )
        .expect("worker");
        let object = serde_json::to_value(response)
            .expect("response json")
            .as_object()
            .expect("response object")
            .clone();

        assert!(object.contains_key("artifact_build_id"));
        assert!(object.contains_key("materialization_key"));
        assert!(!object.contains_key("install_revision_id"));
    }

    fn run_worker(
        request: &ArtifactBuildProducerRequest,
        worker_workspace: &Path,
        remote_root: &Path,
    ) -> Result<ArtifactBuildProducerResponse> {
        run_file_backed_artifact_build_worker(
            request,
            worker_workspace,
            remote_root,
            ArtifactBuildWorkerSimOptions {
                source_root: fixture_source_root(),
                command_runner: ArtifactBuildCommandRunner::LocalShell,
                provenance_created_by: "artifact-build-worker-sim-test".to_string(),
            },
        )
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn worker_sim_does_not_inherit_host_secret_env() {
        let secret_key = "ATO_TEST_SECRET_SHOULD_NOT_LEAK";
        let secret_value = "secret-value-must-not-appear-in-build";

        // Set secret in host process env.
        unsafe {
            std::env::set_var(secret_key, secret_value);
        }

        let temp = TempDir::new().expect("temp");
        let source_root = temp.path().join("src");
        let worker_workspace = temp.path().join("worker");
        fs::create_dir_all(&source_root).expect("src dir");

        // Command writes the env var (or "not-set") to a file outside source_root.
        let out_file = temp.path().join("env-check.txt");
        let command = format!(
            "printf '%s' \"${{{}:-not-set}}\" > '{}'",
            secret_key,
            out_file.display()
        );

        // allowed_env_keys lists the key — proving contract metadata alone does
        // not cause the host value to be forwarded to the child process.
        ArtifactBuildCommandRunner::LocalShell
            .run_build(
                &command,
                &source_root,
                &worker_workspace,
                &[secret_key.to_string()],
            )
            .expect("run_build succeeded");

        let captured = fs::read_to_string(&out_file).expect("env-check.txt written by command");
        assert!(
            !captured.contains(secret_value),
            "host secret leaked into worker build process: {captured}"
        );

        unsafe {
            std::env::remove_var(secret_key);
        }
    }

    fn fixture_request() -> ArtifactBuildProducerRequest {
        let toolchain_identity = fixture_toolchain_identity();
        let observation =
            fixture_observation_for_source(&fixture_source_root(), &toolchain_identity)
                .expect("fixture observation");
        artifact_build_request_from_observation(
            &observation,
            fixture_source_ref(),
            "blake3:worker-fixture-source".to_string(),
            "blake3:worker-fixture-recipe".to_string(),
            None,
            toolchain_identity,
            fixture_platform_profile(),
        )
        .expect("fixture request")
    }

    fn fixture_source_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("vite-build-output-layer")
    }

    struct ScopedEnv {
        key: &'static str,
        prior: Option<OsString>,
    }

    impl ScopedEnv {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let prior = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, prior }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
