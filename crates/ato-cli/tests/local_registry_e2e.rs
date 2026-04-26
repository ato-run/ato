#![allow(deprecated)]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use capsule_core::ato_lock::{compute_closure_digest, recompute_lock_id, to_pretty_json, AtoLock};
use capsule_core::packers::payload::compute_manifest_hash_without_signatures;
use capsule_core::types::CapsuleManifest;
use tempfile::TempDir;

struct ServerGuard {
    child: std::process::Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn local_tcp_bind_available() -> bool {
    TcpListener::bind("127.0.0.1:0").is_ok()
}

fn is_permission_denied(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
            .unwrap_or(false)
    }) || {
        let msg = err.to_string().to_ascii_lowercase();
        msg.contains("permission denied") || msg.contains("operation not permitted")
    }
}

fn start_local_registry_or_skip(
    ato: &Path,
    data_dir: &Path,
    test_name: &str,
) -> Result<Option<(ServerGuard, String)>> {
    if !local_tcp_bind_available() {
        eprintln!("skipping {test_name}: local TCP bind is not permitted in this environment");
        return Ok(None);
    }

    match start_local_registry(ato, data_dir) {
        Ok(v) => Ok(Some(v)),
        Err(err) if is_permission_denied(&err) => {
            eprintln!("skipping {test_name}: {}", err);
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn wait_for_well_known(base_url: &str) -> Result<()> {
    let url = format!("{}/.well-known/capsule.json", base_url);
    for _ in 0..60 {
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("local registry did not become ready: {}", url);
}

fn seed_minimal_deno_lockfiles(workspace_root: &Path) -> Result<()> {
    let manifest_text = std::fs::read_to_string(workspace_root.join("capsule.toml"))
        .context("read manifest for lockfile")?;
    let manifest: CapsuleManifest =
        toml::from_str(&manifest_text).context("parse manifest for lockfile")?;
    let manifest_hash = compute_manifest_hash_without_signatures(&manifest)
        .context("compute manifest hash for lockfile")?;

    std::fs::write(workspace_root.join("deno.json"), "{}")?;

    std::fs::write(
        workspace_root.join("deno.lock"),
        r#"{"version":"3","remote":{}}"#,
    )?;

    std::fs::write(
        workspace_root.join("capsule.lock.json"),
        format!(
            "version = \"1\"\n\n[meta]\ncreated_at = \"2026-03-03T07:20:13.289516+00:00\"\nmanifest_hash = \"{}\"\n\n[runtimes.deno]\nprovider = \"official\"\nversion = \"1.46.3\"\n\n[runtimes.deno.targets.aarch64-apple-darwin]\nurl = \"https://github.com/denoland/deno/releases/download/v1.46.3/deno-aarch64-apple-darwin.zip\"\nsha256 = \"e74f8ddd6d8205654905a4e42b5a605ab110722a7898aef68bc35d6e704c2946\"\n\n[targets]\n",
            manifest_hash
        ),
    )?;

    Ok(())
}

fn write_canonical_static_publish_lock(
    workspace_root: &Path,
    name: &str,
    version: &str,
) -> Result<(String, String)> {
    let closure = serde_json::json!({
        "kind": "runtime_closure",
        "status": "complete",
        "inputs": []
    });
    let closure_digest = compute_closure_digest(&closure)?
        .context("compute closure digest for canonical publish fixture")?;

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
    lock.resolution
        .entries
        .insert("closure".to_string(), closure);
    lock.contract.entries.insert(
        "process".to_string(),
        serde_json::json!({
            "driver": "static",
            "entrypoint": "dist"
        }),
    );
    lock.contract.entries.insert(
        "metadata".to_string(),
        serde_json::json!({
            "name": name,
            "version": version,
            "default_target": "site"
        }),
    );
    recompute_lock_id(&mut lock).context("recompute canonical lock id")?;
    let lock_id = lock
        .lock_id
        .as_ref()
        .map(|value| value.as_str().to_string())
        .context("canonical lock id missing after recompute")?;
    std::fs::write(
        workspace_root.join("ato.lock.json"),
        to_pretty_json(&lock).context("serialize canonical lock")?,
    )?;
    Ok((lock_id, closure_digest))
}

fn ensure_fake_runtime_shims(home_dir: &Path) -> Result<std::ffi::OsString> {
    let shims_dir = home_dir.join(".test-runtime-shims");
    std::fs::create_dir_all(&shims_dir)?;

    for binary in ["node", "bun", "python", "python3", "uv", "cargo", "wails"] {
        #[cfg(windows)]
        let shim_path = shims_dir.join(format!("{binary}.cmd"));
        #[cfg(not(windows))]
        let shim_path = shims_dir.join(binary);

        if !shim_path.exists() {
            #[cfg(windows)]
            std::fs::write(&shim_path, "@echo off\r\nexit /B 0\r\n")?;
            #[cfg(not(windows))]
            {
                std::fs::write(&shim_path, "#!/bin/sh\nexit 0\n")?;
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&shim_path)?.permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&shim_path, permissions)?;
            }
        }
    }

    #[cfg(windows)]
    let npm_path = shims_dir.join("npm.cmd");
    #[cfg(not(windows))]
    let npm_path = shims_dir.join("npm");
    if !npm_path.exists() {
        #[cfg(windows)]
        std::fs::write(&npm_path, "@echo off\r\nexit /B 0\r\n")?;
        #[cfg(not(windows))]
        {
            std::fs::write(
                &npm_path,
                r#"#!/bin/sh
set -eu
if [ "${1:-}" = "run" ] && [ "${2:-}" = "build" ]; then
  name="$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' package.json | head -n 1)"
  if [ -z "${name}" ]; then
    name="desktop-app"
  fi
  if [ -f "src-tauri/Cargo.toml" ]; then
    out="src-tauri/target/release/bundle/macos/${name}.app"
  elif [ -f "electron-builder.json" ]; then
    out="dist/${name}.app"
  elif [ -f "wails.json" ]; then
    out="build/bin/${name}.app"
  else
    out="dist/${name}.app"
  fi
  mkdir -p "${out}/Contents/MacOS"
  printf '#!/bin/sh\necho %s\n' "${name}" > "${out}/Contents/MacOS/${name}"
  chmod 755 "${out}/Contents/MacOS/${name}"
fi
exit 0
"#,
            )?;
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&npm_path)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&npm_path, permissions)?;
        }
    }

    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![shims_dir];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).context("join PATH entries for runtime shims")
}

fn run_ato_with_home(
    ato: &Path,
    args: &[&str],
    cwd: &Path,
    home_dir: &Path,
) -> Result<std::process::Output> {
    let path = ensure_fake_runtime_shims(home_dir)?;
    Command::new(ato)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home_dir)
        .env("PATH", path)
        .output()
        .with_context(|| format!("failed to run ato {:?}", args))
}

fn run_ato_with_home_allow_unsafe(
    ato: &Path,
    args: &[&str],
    cwd: &Path,
    home_dir: &Path,
) -> Result<std::process::Output> {
    let path = ensure_fake_runtime_shims(home_dir)?;
    Command::new(ato)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home_dir)
        .env("PATH", path)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .output()
        .with_context(|| format!("failed to run ato {:?}", args))
}

fn read_probe_json(path: &Path) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("expected probe output at {}", path.display()))?;
    serde_json::from_str(raw.trim())
        .with_context(|| format!("expected JSON probe in {}: {}", path.display(), raw))
}

fn start_local_registry(ato: &Path, data_dir: &Path) -> Result<(ServerGuard, String)> {
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{}", port);
    let child = Command::new(ato)
        .args([
            "registry",
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data-dir",
            data_dir.to_string_lossy().as_ref(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn local registry server")?;
    let guard = ServerGuard { child };
    wait_for_well_known(&base_url)?;
    Ok((guard, base_url))
}

fn build_publish_install(
    ato: &Path,
    project_dir: &Path,
    base_url: &str,
    scoped_id: &str,
    capsule_name: &str,
    install_cwd: &Path,
    home_dir: &Path,
) -> Result<()> {
    let build = run_ato_with_home(ato, &["build", "."], project_dir, home_dir)?;
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let capsule_path = project_dir.join(format!("{}.capsule", capsule_name));
    assert!(
        capsule_path.exists(),
        "capsule artifact not found: {}",
        capsule_path.display()
    );

    let publish = run_ato_with_home(
        ato,
        &["publish", "--registry", base_url, "--json"],
        project_dir,
        home_dir,
    )?;
    assert!(
        publish.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&publish.stderr)
    );

    let install = run_ato_with_home(
        ato,
        &["install", scoped_id, "--registry", base_url],
        install_cwd,
        home_dir,
    )?;
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    Ok(())
}

fn build_capsule_artifact(
    ato: &Path,
    project_dir: &Path,
    capsule_name: &str,
    home_dir: &Path,
) -> Result<std::path::PathBuf> {
    let build = run_ato_with_home(ato, &["build", "."], project_dir, home_dir)?;
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let capsule_path = project_dir.join(format!("{}.capsule", capsule_name));
    assert!(
        capsule_path.exists(),
        "capsule artifact not found: {}",
        capsule_path.display()
    );
    Ok(capsule_path)
}

fn publish_artifact_with_scoped_id(
    ato: &Path,
    artifact_path: &Path,
    scoped_id: &str,
    base_url: &str,
    cwd: &Path,
    home_dir: &Path,
) -> Result<()> {
    let artifact = artifact_path.to_string_lossy().to_string();
    let publish = run_ato_with_home(
        ato,
        &[
            "publish",
            "--deploy",
            "--registry",
            base_url,
            "--artifact",
            &artifact,
            "--scoped-id",
            scoped_id,
            "--json",
        ],
        cwd,
        home_dir,
    )?;
    assert!(
        publish.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&publish.stderr)
    );
    Ok(())
}

fn command_on_path(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(command);
                if candidate.is_file() {
                    return true;
                }
                #[cfg(windows)]
                {
                    let candidate = dir.join(format!("{command}.exe"));
                    if candidate.is_file() {
                        return true;
                    }
                }
                false
            })
        })
        .unwrap_or(false)
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &dest_path)?;
        } else if file_type.is_symlink() {
            let metadata = std::fs::metadata(&source_path)
                .with_context(|| format!("follow symlink {}", source_path.display()))?;
            if metadata.is_dir() {
                copy_dir_recursive(&source_path, &dest_path)?;
            } else if metadata.is_file() {
                std::fs::copy(&source_path, &dest_path).with_context(|| {
                    format!(
                        "copy symlinked sample fixture {} -> {}",
                        source_path.display(),
                        dest_path.display()
                    )
                })?;
            } else {
                anyhow::bail!(
                    "unsupported symlinked sample fixture entry: {}",
                    source_path.display()
                );
            }
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &dest_path).with_context(|| {
                format!(
                    "copy sample fixture {} -> {}",
                    source_path.display(),
                    dest_path.display()
                )
            })?;
        } else {
            anyhow::bail!(
                "unsupported entry in sample fixture: {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

fn materialize_desktop_sample(sample_name: &str, project_dir: &Path) -> Result<()> {
    let sample_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("source")
        .join("native-desktop")
        .join(sample_name);
    if !sample_root.is_dir() {
        anyhow::bail!(
            "desktop sample fixture is missing: {}",
            sample_root.display()
        );
    }
    copy_dir_recursive(&sample_root, project_dir)
}

fn assert_source_desktop_private_publish(
    ato: &Path,
    base_url: &str,
    workspace_root: &Path,
    home_dir: &Path,
    framework: &str,
    extra_publish_args: &[&str],
    expected_identity_class: &str,
) -> Result<()> {
    let mut publish_args = vec!["publish", "--registry", base_url, "--json"];
    publish_args.extend_from_slice(extra_publish_args);
    let publish = run_ato_with_home(ato, &publish_args, workspace_root, home_dir)?;
    assert!(
        publish.status.success(),
        "{framework} publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&publish.stdout),
        String::from_utf8_lossy(&publish.stderr)
    );

    let publish_json: serde_json::Value =
        serde_json::from_slice(&publish.stdout).context("parse publish json")?;
    assert_eq!(
        publish_json
            .get("publish_metadata")
            .and_then(|value| value.get("identity_class"))
            .and_then(|value| value.as_str()),
        Some(expected_identity_class),
        "{framework} publish json missing source-derived metadata: {publish_json}"
    );
    let scoped_id = publish_json
        .get("scoped_id")
        .and_then(|value| value.as_str())
        .context("publish json missing scoped_id")?;
    let mut scoped_parts = scoped_id.splitn(2, '/');
    let publisher = scoped_parts
        .next()
        .context("missing publisher in scoped_id")?;
    let slug = scoped_parts.next().context("missing slug in scoped_id")?;

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/{}/{}",
        base_url, publisher, slug
    ))
    .with_context(|| format!("fetch detail for {framework}"))?
    .json()
    .with_context(|| format!("parse detail json for {framework}"))?;
    let releases = detail
        .get("releases")
        .and_then(|value| value.as_array())
        .with_context(|| format!("releases array missing for desktop source publish: {detail}"))?;
    let release = releases
        .iter()
        .find(|value| value.get("version").and_then(|entry| entry.as_str()) == Some("1.0.0"))
        .context("desktop source publish release missing")?;
    assert_eq!(
        release
            .get("publish_metadata")
            .and_then(|value| value.get("identity_class"))
            .and_then(|value| value.as_str()),
        Some(expected_identity_class),
        "{framework} release metadata missing identity class: {release}"
    );
    assert_eq!(
        release
            .get("publish_metadata")
            .and_then(|value| value.get("provenance_limited"))
            .and_then(|value| value.as_bool()),
        Some(false),
        "{framework} release metadata should not be provenance-limited: {release}"
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_publish_artifact_without_cwd_manifest() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let build_dir = tmp.path().join("build-project");
    let publish_cwd = tmp.path().join("publish-cwd");
    std::fs::create_dir_all(&build_dir)?;
    std::fs::create_dir_all(&publish_cwd)?;

    std::fs::write(
        build_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-artifact-cwdless"
version = "1.0.0"
type = "app"

runtime = "web/static"
port = 4173
run = "dist""#,
    )?;
    std::fs::create_dir_all(build_dir.join("dist"))?;
    std::fs::write(
        build_dir.join("dist").join("index.html"),
        "<!doctype html><title>cwdless publish</title>",
    )?;

    let build = run_ato_with_home(&ato, &["build", "."], &build_dir, &home_dir)?;
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let artifact_path = build_dir.join("test-artifact-cwdless.capsule");
    assert!(artifact_path.exists(), "artifact not found");

    let Some((_guard, base_url)) =
        start_local_registry_or_skip(&ato, &data_dir, "e2e_publish_artifact_without_cwd_manifest")?
    else {
        return Ok(());
    };

    let artifact_str = artifact_path.to_string_lossy().to_string();
    let scoped_id = "team-x/test-artifact-cwdless";

    let first_publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--deploy",
            "--registry",
            &base_url,
            "--artifact",
            &artifact_str,
            "--scoped-id",
            scoped_id,
            "--json",
        ],
        &publish_cwd,
        &home_dir,
    )?;
    assert!(
        first_publish.status.success(),
        "first publish failed: {}",
        String::from_utf8_lossy(&first_publish.stderr)
    );
    let first_value: serde_json::Value =
        serde_json::from_slice(&first_publish.stdout).context("first publish json parse")?;
    let first_phases = first_value
        .get("phases")
        .and_then(|value| value.as_array())
        .context("missing publish phases")?;
    assert_eq!(
        first_phases.len(),
        6,
        "unexpected phases payload: {first_value}"
    );
    assert!(
        first_value.get("install").is_some(),
        "missing install result"
    );
    assert!(
        first_value.get("dry_run").is_some(),
        "missing dry_run result"
    );

    let second_publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--deploy",
            "--registry",
            &base_url,
            "--artifact",
            &artifact_str,
            "--scoped-id",
            scoped_id,
            "--json",
        ],
        &publish_cwd,
        &home_dir,
    )?;
    assert!(
        !second_publish.status.success(),
        "second publish without --allow-existing must fail"
    );
    let second_stdout = String::from_utf8_lossy(&second_publish.stdout);
    let second_stderr = String::from_utf8_lossy(&second_publish.stderr);
    assert!(
        second_stderr.contains("E202") || second_stdout.contains("\"code\":\"E202\""),
        "expected E202 for version conflict; stdout={} stderr={}",
        second_stdout,
        second_stderr
    );

    let third_publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--deploy",
            "--registry",
            &base_url,
            "--artifact",
            &artifact_str,
            "--scoped-id",
            scoped_id,
            "--allow-existing",
        ],
        &publish_cwd,
        &home_dir,
    )?;
    assert!(
        third_publish.status.success(),
        "third publish with --allow-existing failed: {}",
        String::from_utf8_lossy(&third_publish.stderr)
    );
    let third_stdout = String::from_utf8_lossy(&third_publish.stdout);
    assert!(
        third_stdout.contains("Existing release reused (same sha256, no new upload)."),
        "expected reused-release message in allow-existing path; stdout={}",
        third_stdout
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_publish_dry_run_artifact_reports_install_and_dry_run() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let build_dir = tmp.path().join("build-project");
    let publish_cwd = tmp.path().join("publish-cwd");
    std::fs::create_dir_all(&build_dir)?;
    std::fs::create_dir_all(&publish_cwd)?;

    std::fs::write(
        build_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-dry-run-artifact"
version = "1.0.0"
type = "app"

runtime = "web/static"
port = 4173
run = "dist""#,
    )?;
    std::fs::create_dir_all(build_dir.join("dist"))?;
    std::fs::write(
        build_dir.join("dist").join("index.html"),
        "<!doctype html><title>dry run artifact</title>",
    )?;

    let artifact_path =
        build_capsule_artifact(&ato, &build_dir, "test-dry-run-artifact", &home_dir)?;
    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_publish_dry_run_artifact_reports_install_and_dry_run",
    )?
    else {
        return Ok(());
    };

    let output = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--dry-run",
            "--json",
            "--registry",
            &base_url,
            "--artifact",
            artifact_path.to_string_lossy().as_ref(),
            "--scoped-id",
            "team-x/test-dry-run-artifact",
        ],
        &publish_cwd,
        &home_dir,
    )?;
    assert!(
        output.status.success(),
        "publish dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("dry-run json parse")?;
    let phases = value
        .get("phases")
        .and_then(|entry| entry.as_array())
        .context("missing phases payload")?;
    assert_eq!(phases.len(), 6, "unexpected phases payload: {value}");
    assert_eq!(phases[2]["status"], "ok");
    assert_eq!(phases[3]["status"], "ok");
    assert_eq!(phases[4]["status"], "ok");
    assert_eq!(phases[5]["status"], "skipped");
    assert!(value.get("install").is_some(), "missing install result");
    assert!(value.get("dry_run").is_some(), "missing dry_run result");

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_build_publish_install_search_download() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-local"
version = "1.0.0"
type = "app"

runtime = "web/static"
port = 4173
run = "dist"
build = "echo prepare"
"#,
    )?;
    std::fs::create_dir_all(project_dir.join("dist"))?;
    std::fs::write(
        project_dir.join("dist").join("index.html"),
        r#"<!doctype html><title>hello local registry</title>"#,
    )?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_build_publish_install_search_download",
    )?
    else {
        return Ok(());
    };

    build_publish_install(
        &ato,
        &project_dir,
        &base_url,
        "local/test-local",
        "test-local",
        tmp.path(),
        &home_dir,
    )?;

    let search = run_ato_with_home(
        &ato,
        &["search", "test-local", "--registry", &base_url, "--json"],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        search.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let body = String::from_utf8(search.stdout).context("search stdout utf8")?;
    let value: serde_json::Value = serde_json::from_str(&body).context("search json parse")?;
    let capsules = value
        .get("capsules")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        capsules
            .iter()
            .any(|capsule| capsule.get("slug").and_then(|v| v.as_str()) == Some("test-local")),
        "search response missing test-local capsule"
    );

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/local/test-local",
        base_url
    ))
    .context("detail endpoint call")?
    .json()
    .context("detail json parse")?;
    assert_eq!(
        detail
            .get("manifest")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("test-local"),
        "detail response should include manifest payload"
    );

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build client")?;
    let resp = client
        .get(format!(
            "{}/v1/manifest/capsules/by/local/test-local/download?version=1.0.0",
            base_url
        ))
        .send()
        .context("download endpoint call")?;
    assert_eq!(resp.status(), reqwest::StatusCode::FOUND);
    assert!(
        resp.headers().get("location").is_some(),
        "download endpoint should return Location header"
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_tauri() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!("skipping e2e_local_registry_private_publish_source_tauri: macOS only");
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("tauri-private-publish");
    materialize_desktop_sample("tauri", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_tauri",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "tauri",
        &[],
        "source_derived_unsigned_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_electron() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!("skipping e2e_local_registry_private_publish_source_electron: macOS only");
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("electron-private-publish");
    materialize_desktop_sample("electron", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_electron",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "electron",
        &[],
        "source_derived_unsigned_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_wails() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!("skipping e2e_local_registry_private_publish_source_wails: macOS only");
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("wails-private-publish");
    materialize_desktop_sample("wails", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_wails",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "wails",
        &[],
        "source_derived_unsigned_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_tauri_finalize_local() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_tauri_finalize_local: macOS only"
        );
        return Ok(());
    }
    if !command_on_path("codesign") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_tauri_finalize_local: codesign unavailable"
        );
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("tauri-private-publish");
    materialize_desktop_sample("tauri", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_tauri_finalize_local",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "tauri",
        &["--finalize-local", "--allow-external-finalize"],
        "locally_finalized_signed_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_electron_finalize_local() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_electron_finalize_local: macOS only"
        );
        return Ok(());
    }
    if !command_on_path("codesign") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_electron_finalize_local: codesign unavailable"
        );
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("electron-private-publish");
    materialize_desktop_sample("electron", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_electron_finalize_local",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "electron",
        &["--finalize-local", "--allow-external-finalize"],
        "locally_finalized_signed_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_source_wails_finalize_local() -> Result<()> {
    if !cfg!(target_os = "macos") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_wails_finalize_local: macOS only"
        );
        return Ok(());
    }
    if !command_on_path("codesign") {
        eprintln!(
            "skipping e2e_local_registry_private_publish_source_wails_finalize_local: codesign unavailable"
        );
        return Ok(());
    }

    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("wails-private-publish");
    materialize_desktop_sample("wails", &project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_source_wails_finalize_local",
    )?
    else {
        return Ok(());
    };

    assert_source_desktop_private_publish(
        &ato,
        &base_url,
        &project_dir,
        &home_dir,
        "wails",
        &["--finalize-local", "--allow-external-finalize"],
        "locally_finalized_signed_bundle",
    )
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_private_publish_prefers_canonical_lock_metadata() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("project-canonical-publish");
    std::fs::create_dir_all(project_dir.join("dist"))?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "ignored-manifest"
version = "9.9.9"
type = "app"

runtime = "web/static"
port = 4173
run = "dist""#,
    )?;
    std::fs::write(
        project_dir.join("dist").join("index.html"),
        "<!doctype html><title>canonical publish</title>",
    )?;
    let (expected_lock_id, expected_closure_digest) =
        write_canonical_static_publish_lock(&project_dir, "canonical-publish", "0.4.2")?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_private_publish_prefers_canonical_lock_metadata",
    )?
    else {
        return Ok(());
    };

    let publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--build",
            "--deploy",
            "--registry",
            &base_url,
            "--json",
        ],
        &project_dir,
        &home_dir,
    )?;
    assert!(
        publish.status.success(),
        "canonical private publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&publish.stdout),
        String::from_utf8_lossy(&publish.stderr)
    );

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/local/canonical-publish",
        base_url
    ))
    .context("canonical detail endpoint call")?
    .json()
    .context("canonical detail json parse")?;

    assert_eq!(
        detail
            .get("manifest")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str()),
        Some("canonical-publish")
    );
    let releases = detail
        .get("releases")
        .and_then(|value| value.as_array())
        .context("releases array missing for canonical publish")?;
    let release = releases
        .iter()
        .find(|value| value.get("version").and_then(|entry| entry.as_str()) == Some("0.4.2"))
        .context("canonical publish release missing")?;
    assert_eq!(
        release.get("lock_id").and_then(|value| value.as_str()),
        Some(expected_lock_id.as_str())
    );
    assert_eq!(
        release
            .get("closure_digest")
            .and_then(|value| value.as_str()),
        Some(expected_closure_digest.as_str())
    );

    let ignored = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/local/ignored-manifest",
        base_url
    ))
    .context("ignored manifest detail endpoint call")?;
    assert_eq!(ignored.status(), reqwest::StatusCode::NOT_FOUND);

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_publish_phases_preserve_readme() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("project-readme-phases");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-readme-phases"
version = "1.0.0"
type = "app"

runtime = "source/deno"
runtime_version = "1.46.3"
run = "main.ts"

[build.lifecycle]
prepare = "echo prepare"
"#,
    )?;
    std::fs::write(
        project_dir.join("main.ts"),
        r#"console.log("hello readme phases");"#,
    )?;
    std::fs::write(
        project_dir.join("README.md"),
        "# Readme Phase Test\n\nThis README must be visible in the local registry UI.\n",
    )?;
    seed_minimal_deno_lockfiles(&project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_publish_phases_preserve_readme",
    )?
    else {
        return Ok(());
    };

    let publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--prepare",
            "--build",
            "--deploy",
            "--registry",
            &base_url,
            "--json",
        ],
        &project_dir,
        &home_dir,
    )?;
    let publish_stdout = String::from_utf8_lossy(&publish.stdout);
    let publish_stderr = String::from_utf8_lossy(&publish.stderr);
    assert!(
        publish.status.success(),
        "publish phases failed: stdout={} stderr={}",
        publish_stdout,
        publish_stderr
    );

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/local/test-readme-phases",
        base_url
    ))
    .context("detail endpoint call")?
    .json()
    .context("detail json parse")?;
    assert_eq!(
        detail
            .get("readme_markdown")
            .and_then(|value| value.as_str()),
        Some("# Readme Phase Test\n\nThis README must be visible in the local registry UI.\n"),
        "detail response should surface README.md from publish phase artifact"
    );
    assert_eq!(
        detail.get("readme_source").and_then(|value| value.as_str()),
        Some("artifact"),
        "README source should indicate artifact extraction"
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_monorepo_publish_uses_parent_readme() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let repo_dir = tmp.path().join("repo");
    let project_dir = repo_dir.join("apps/file2api-monorepo");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        repo_dir.join("README.md"),
        "# File2API Monorepo\n\nParent README should be visible after publish.\n",
    )?;
    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "file2api-monorepo"
version = "1.0.0"
type = "app"

runtime = "source/deno"
runtime_version = "1.46.3"
run = "main.ts"

[metadata]
repository = "Koh0920/file2api-monorepo"

[build.lifecycle]
prepare = "echo prepare"
"#,
    )?;
    std::fs::write(
        project_dir.join("main.ts"),
        r#"console.log("hello monorepo readme");"#,
    )?;
    seed_minimal_deno_lockfiles(&project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_monorepo_publish_uses_parent_readme",
    )?
    else {
        return Ok(());
    };

    let publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--prepare",
            "--build",
            "--deploy",
            "--registry",
            &base_url,
            "--json",
        ],
        &project_dir,
        &home_dir,
    )?;
    assert!(
        publish.status.success(),
        "monorepo publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&publish.stdout),
        String::from_utf8_lossy(&publish.stderr)
    );

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/koh0920/file2api-monorepo",
        base_url
    ))
    .context("detail endpoint call")?
    .json()
    .context("detail json parse")?;
    assert_eq!(
        detail
            .get("readme_markdown")
            .and_then(|value| value.as_str()),
        Some("# File2API Monorepo\n\nParent README should be visible after publish.\n"),
        "detail response should surface parent monorepo README.md"
    );
    assert_eq!(
        detail.get("readme_source").and_then(|value| value.as_str()),
        Some("artifact"),
        "README source should indicate artifact extraction"
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_package_json_prepare_publish_exposes_readme() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("file2api-like");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "file2api-monorepo"
version = "0.1.19"
description = ""
type = "app"

runtime = "web/deno"
runtime_version = "1.46.3"
port = 4173
run = "ato-entry.ts"
[pack]
include = [
  "ato-entry.ts",
  "capsule.toml",
  "capsule.lock.json",
  "deno.json",
  "deno.lock",
  "package.json"
]

[metadata]
repository = "Koh0920/file2api"
"#,
    )?;
    std::fs::write(
        project_dir.join("package.json"),
        r#"{"scripts":{"capsule:prepare":"echo prepare"}}"#,
    )?;
    std::fs::write(
        project_dir.join("README.md"),
        "# DB-Nexus MVP\n\nThis README should remain visible after private publish.\n",
    )?;
    std::fs::write(
        project_dir.join("ato-entry.ts"),
        "console.log('file2api package-json prepare');\n",
    )?;
    seed_minimal_deno_lockfiles(&project_dir)?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_package_json_prepare_publish_exposes_readme",
    )?
    else {
        return Ok(());
    };

    let publish = run_ato_with_home(
        &ato,
        &[
            "publish",
            "--prepare",
            "--build",
            "--deploy",
            "--registry",
            &base_url,
            "--json",
        ],
        &project_dir,
        &home_dir,
    )?;
    assert!(
        publish.status.success(),
        "package.json prepare publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&publish.stdout),
        String::from_utf8_lossy(&publish.stderr)
    );

    let detail: serde_json::Value = reqwest::blocking::get(format!(
        "{}/v1/manifest/capsules/by/koh0920/file2api-monorepo",
        base_url
    ))
    .context("detail endpoint call")?
    .json()
    .context("detail json parse")?;

    assert_eq!(
        detail.get("description").and_then(|value| value.as_str()),
        Some(""),
        "description should remain empty while README is still available"
    );
    assert_eq!(
        detail.get("readme_source").and_then(|value| value.as_str()),
        Some("artifact"),
        "README source should indicate artifact extraction"
    );
    assert_eq!(
        detail
            .get("readme_markdown")
            .and_then(|value| value.as_str()),
        Some("# DB-Nexus MVP\n\nThis README should remain visible after private publish.\n"),
        "detail response should expose README content for package.json prepare flow"
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_run_seeds_execution_consent() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("project-run-consent");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-run-consent"
version = "1.0.0"
type = "app"

runtime = "source/node"
runtime_version = "20.12.0"
run = "main.js"
[network]
egress_allow = ["api.github.com"]
"#,
    )?;
    std::fs::write(
        project_dir.join("main.js"),
        r#"console.log("local registry seeded consent run");
setTimeout(() => process.exit(0), 3000);
"#,
    )?;
    std::fs::write(project_dir.join("package-lock.json"), "{}")?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_run_seeds_execution_consent",
    )?
    else {
        return Ok(());
    };

    build_publish_install(
        &ato,
        &project_dir,
        &base_url,
        "local/test-run-consent",
        "test-run-consent",
        tmp.path(),
        &home_dir,
    )?;

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("build client")?;
    let run_response = client
        .post(format!(
            "{}/v1/local/capsules/by/local/test-run-consent/run",
            base_url
        ))
        .json(&serde_json::json!({
            "confirmed": true,
        }))
        .send()
        .context("run endpoint call")?;
    assert_eq!(run_response.status(), reqwest::StatusCode::ACCEPTED);

    let mut process_id = None::<String>;
    for _ in 0..300 {
        let processes: serde_json::Value = client
            .get(format!("{}/v1/local/processes", base_url))
            .send()
            .context("process list call")?
            .json()
            .context("process list json parse")?;
        process_id = processes.as_array().and_then(|rows| {
            rows.iter().find_map(|row| {
                (row.get("scoped_id").and_then(|value| value.as_str())
                    == Some("local/test-run-consent"))
                .then(|| {
                    row.get("id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .flatten()
            })
        });
        if process_id.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let process_id = process_id.context("local registry process record missing")?;

    let mut logs = Vec::<String>::new();
    for _ in 0..100 {
        let log_payload: serde_json::Value = client
            .get(format!(
                "{}/v1/local/processes/{}/logs?tail=200",
                base_url, process_id
            ))
            .send()
            .context("process logs call")?
            .json()
            .context("process logs json parse")?;
        logs = log_payload
            .get("lines")
            .and_then(|value| value.as_array())
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if logs
            .iter()
            .any(|line| line.contains("local registry seeded consent run"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let joined_logs = logs.join("\n");
    assert!(
        !joined_logs.contains("ExecutionPlan consent missing in non-interactive mode"),
        "run should seed consent before non-interactive spawn; logs={}",
        joined_logs
    );
    assert!(
        joined_logs.contains("local registry seeded consent run"),
        "run should complete after consent seeding; logs={}",
        joined_logs
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_web_static_build_publish_install() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let static_port = if local_tcp_bind_available() {
        reserve_port()
    } else {
        eprintln!("skipping e2e_local_registry_web_static_build_publish_install: local TCP bind is not permitted in this environment");
        return Ok(());
    };
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("web-static-project");
    std::fs::create_dir_all(project_dir.join("dist"))?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        format!(
            r#"schema_version = "0.3"
name = "test-web-static"
version = "1.0.0"
type = "app"

runtime = "web/static"
port = {static_port}
run = "dist""#
        ),
    )?;
    std::fs::write(
        project_dir.join("dist").join("index.html"),
        r#"<!doctype html><title>web static</title>"#,
    )?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_web_static_build_publish_install",
    )?
    else {
        return Ok(());
    };
    build_publish_install(
        &ato,
        &project_dir,
        &base_url,
        "local/test-web-static",
        "test-web-static",
        tmp.path(),
        &home_dir,
    )?;

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("build client")?;
    let run_response = client
        .post(format!(
            "{}/v1/local/capsules/by/local/test-web-static/run",
            base_url
        ))
        .json(&serde_json::json!({
            "confirmed": true,
        }))
        .send()
        .context("run endpoint call")?;
    let run_status = run_response.status();
    let run_body = run_response.text().context("run endpoint body")?;
    assert_eq!(
        run_status,
        reqwest::StatusCode::ACCEPTED,
        "run endpoint should accept confirmed local launch: {run_body}"
    );

    // Best-effort cleanup in case background process was started.
    let _ = run_ato_with_home(&ato, &["close", "--all", "--force"], tmp.path(), &home_dir);

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_node_python_run_fail_closed() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let node_no_lock = tmp.path().join("node-no-lock");
    let node_with_lock = tmp.path().join("node-with-lock");
    let node_policy_violation = tmp.path().join("node-policy-violation");
    let python_no_lock = tmp.path().join("python-no-lock");
    let python_with_lock = tmp.path().join("python-with-lock");
    std::fs::create_dir_all(&node_no_lock)?;
    std::fs::create_dir_all(&node_with_lock)?;
    std::fs::create_dir_all(&node_policy_violation)?;
    std::fs::create_dir_all(&python_no_lock)?;
    std::fs::create_dir_all(&python_with_lock)?;

    std::fs::write(
        node_no_lock.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-node-no-lock"
version = "1.0.0"
type = "app"

runtime = "source/node"
runtime_version = "20.12.0"
run = "main.js""#,
    )?;
    std::fs::write(
        node_no_lock.join("main.js"),
        r#"console.log("node no lock");"#,
    )?;

    std::fs::write(
        node_with_lock.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-node-with-lock"
version = "1.0.0"
type = "app"

runtime = "source/node"
runtime_version = "20.12.0"
run = "main.js""#,
    )?;
    std::fs::write(
        node_with_lock.join("main.js"),
        r#"console.log("node with lock");"#,
    )?;
    std::fs::write(node_with_lock.join("package-lock.json"), "{}")?;

    std::fs::write(
        node_policy_violation.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-node-policy-violation"
version = "1.0.0"
type = "app"

runtime = "source/node"
runtime_version = "20.12.0"
run = "main.js""#,
    )?;
    std::fs::write(
        node_policy_violation.join("main.js"),
        r#"fetch("https://example.com").then((res) => console.log(res.status));"#,
    )?;
    std::fs::write(node_policy_violation.join("package-lock.json"), "{}")?;

    std::fs::write(
        python_no_lock.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-python-no-lock"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.9"
run = "main.py""#,
    )?;
    std::fs::write(python_no_lock.join("main.py"), r#"print("python no lock")"#)?;

    std::fs::write(
        python_with_lock.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "test-python-with-lock"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.9"
run = "main.py""#,
    )?;
    std::fs::write(
        python_with_lock.join("main.py"),
        r#"print("python with lock")"#,
    )?;
    std::fs::write(python_with_lock.join("uv.lock"), "# uv lock")?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_node_python_run_fail_closed",
    )?
    else {
        return Ok(());
    };

    let node_no_lock_build = run_ato_with_home(&ato, &["build", "."], &node_no_lock, &home_dir)?;
    assert!(
        !node_no_lock_build.status.success(),
        "node no lock build must fail-closed"
    );
    let node_no_lock_build_stderr = String::from_utf8_lossy(&node_no_lock_build.stderr);
    assert!(
        node_no_lock_build_stderr.contains("E102")
            || node_no_lock_build_stderr.contains("LockDraft is not ready to finalize locally"),
        "expected node no lock build to report lockdraft failure; stderr={}",
        node_no_lock_build_stderr
    );
    assert!(
        node_no_lock_build_stderr.contains("package-lock.json")
            && node_no_lock_build_stderr.contains("pnpm-lock.yaml")
            && node_no_lock_build_stderr.contains("bun.lock"),
        "expected node lockfile requirement to be surfaced during build; stderr={}",
        node_no_lock_build_stderr
    );
    build_publish_install(
        &ato,
        &node_with_lock,
        &base_url,
        "local/test-node-with-lock",
        "test-node-with-lock",
        tmp.path(),
        &home_dir,
    )?;
    let python_no_lock_build =
        run_ato_with_home(&ato, &["build", "."], &python_no_lock, &home_dir)?;
    assert!(
        !python_no_lock_build.status.success(),
        "python no lock build must fail-closed"
    );
    let python_no_lock_build_stderr = String::from_utf8_lossy(&python_no_lock_build.stderr);
    assert!(
        python_no_lock_build_stderr.contains("E102")
            || python_no_lock_build_stderr.contains("E104")
            || python_no_lock_build_stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE")
            || python_no_lock_build_stderr.contains("LockDraft is not ready to finalize locally"),
        "expected python no lock build to report lockdraft failure; stderr={}",
        python_no_lock_build_stderr
    );
    assert!(
        python_no_lock_build_stderr.contains("uv.lock"),
        "expected uv.lock requirement to be surfaced during build; stderr={}",
        python_no_lock_build_stderr
    );
    build_publish_install(
        &ato,
        &python_with_lock,
        &base_url,
        "local/test-python-with-lock",
        "test-python-with-lock",
        tmp.path(),
        &home_dir,
    )?;
    build_publish_install(
        &ato,
        &node_policy_violation,
        &base_url,
        "local/test-node-policy-violation",
        "test-node-policy-violation",
        tmp.path(),
        &home_dir,
    )?;

    let node_no_lock_run = run_ato_with_home(&ato, &["run", "."], &node_no_lock, &home_dir)?;
    assert!(
        !node_no_lock_run.status.success(),
        "node no lock run must fail-closed"
    );
    let node_no_lock_stderr = String::from_utf8_lossy(&node_no_lock_run.stderr);
    assert!(
        node_no_lock_stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE")
            || node_no_lock_stderr.contains("E104")
            || node_no_lock_stderr.contains("E102")
            || node_no_lock_stderr.contains("LockDraft is not ready to finalize locally"),
        "expected lock incomplete JSONL for node no lock; stderr={}",
        node_no_lock_stderr
    );
    assert!(
        node_no_lock_stderr.contains("package-lock.json")
            && node_no_lock_stderr.contains("pnpm-lock.yaml")
            && node_no_lock_stderr.contains("bun.lock"),
        "expected node lockfile requirement to be surfaced; stderr={}",
        node_no_lock_stderr
    );

    let node_with_lock_run = run_ato_with_home(
        &ato,
        &[
            "run",
            "local/test-node-with-lock",
            "--registry",
            &base_url,
            "--yes",
        ],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        node_with_lock_run.status.success(),
        "node with lock should run without --sandbox; stderr={}",
        String::from_utf8_lossy(&node_with_lock_run.stderr)
    );
    let node_with_lock_stderr = String::from_utf8_lossy(&node_with_lock_run.stderr);
    assert!(
        !node_with_lock_stderr.contains("ATO_ERR_POLICY_VIOLATION"),
        "node with lock should not emit policy violation; stderr={}",
        node_with_lock_stderr
    );
    assert!(
        !node_with_lock_stderr.contains("--sandbox"),
        "node with lock should not require --sandbox; stderr={}",
        node_with_lock_stderr
    );

    let node_policy_violation_run = run_ato_with_home(
        &ato,
        &[
            "run",
            "local/test-node-policy-violation",
            "--registry",
            &base_url,
            "--yes",
        ],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        !node_policy_violation_run.status.success(),
        "node permission violation case must fail-closed"
    );
    let node_policy_violation_stderr = String::from_utf8_lossy(&node_policy_violation_run.stderr);
    assert!(
        node_policy_violation_stderr.contains("ATO_ERR_POLICY_VIOLATION")
            || node_policy_violation_stderr.contains("E301")
            || node_policy_violation_stderr.contains("E302")
            || node_policy_violation_stderr.contains("PermissionDenied: Requires net access")
            || node_policy_violation_stderr.contains("NotCapable: Requires net access")
            || node_policy_violation_stderr.contains("Requires net access to \"example.com:443\""),
        "expected policy violation signal for node permission violation; stderr={}",
        node_policy_violation_stderr
    );

    let python_no_lock_run = run_ato_with_home(&ato, &["run", "."], &python_no_lock, &home_dir)?;
    assert!(
        !python_no_lock_run.status.success(),
        "python no lock run must fail-closed"
    );
    let python_no_lock_stderr = String::from_utf8_lossy(&python_no_lock_run.stderr);
    assert!(
        python_no_lock_stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE")
            || python_no_lock_stderr.contains("E104")
            || python_no_lock_stderr.contains("E102")
            || python_no_lock_stderr.contains("LockDraft is not ready to finalize locally"),
        "expected lock incomplete JSONL for python no lock; stderr={}",
        python_no_lock_stderr
    );
    assert!(
        python_no_lock_stderr.contains("uv.lock"),
        "expected uv.lock requirement to be surfaced; stderr={}",
        python_no_lock_stderr
    );

    let python_with_lock_run = run_ato_with_home(
        &ato,
        &[
            "run",
            "local/test-python-with-lock",
            "--registry",
            &base_url,
        ],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        !python_with_lock_run.status.success(),
        "python with lock without --sandbox must fail"
    );
    let python_with_lock_stderr = String::from_utf8_lossy(&python_with_lock_run.stderr);
    assert!(
        python_with_lock_stderr.contains("ATO_ERR_POLICY_VIOLATION")
            || python_with_lock_stderr.contains("E301")
            || python_with_lock_stderr.contains("E302"),
        "expected policy violation JSONL for python without --sandbox; stderr={}",
        python_with_lock_stderr
    );
    assert!(
        python_with_lock_stderr.contains("source/native|python execution requires explicit")
            && python_with_lock_stderr.contains("--sandbox"),
        "expected --sandbox requirement to be surfaced; stderr={}",
        python_with_lock_stderr
    );

    let node_with_lock_unsafe_yes_run = run_ato_with_home(
        &ato,
        &[
            "run",
            "local/test-node-with-lock",
            "--registry",
            &base_url,
            "--sandbox",
            "--yes",
        ],
        tmp.path(),
        &home_dir,
    )?;
    let node_with_lock_unsafe_yes_stderr =
        String::from_utf8_lossy(&node_with_lock_unsafe_yes_run.stderr);
    assert!(
        node_with_lock_unsafe_yes_run.status.success(),
        "node with lock should also run with --sandbox --yes; stderr={}",
        node_with_lock_unsafe_yes_stderr
    );
    assert!(
        !node_with_lock_unsafe_yes_stderr.contains("ATO_ERR_CONSENT_REQUIRED"),
        "unexpected consent-required in --sandbox --yes node run; stderr={}",
        node_with_lock_unsafe_yes_stderr
    );
    assert!(
        !node_with_lock_unsafe_yes_stderr
            .contains("source/native|python execution requires explicit --sandbox opt-in"),
        "unexpected sandbox requirement in --sandbox --yes node run; stderr={}",
        node_with_lock_unsafe_yes_stderr
    );
    assert!(
        !node_with_lock_unsafe_yes_stderr
            .contains("package-lock.json is required for source/node Tier1 execution"),
        "unexpected node lockfile error in --sandbox --yes node run; stderr={}",
        node_with_lock_unsafe_yes_stderr
    );

    let python_with_lock_unsafe_yes_run = run_ato_with_home(
        &ato,
        &[
            "run",
            "local/test-python-with-lock",
            "--registry",
            &base_url,
            "--sandbox",
            "--yes",
        ],
        tmp.path(),
        &home_dir,
    )?;
    let python_with_lock_unsafe_yes_stderr =
        String::from_utf8_lossy(&python_with_lock_unsafe_yes_run.stderr);
    assert!(
        !python_with_lock_unsafe_yes_stderr.contains("ATO_ERR_CONSENT_REQUIRED"),
        "unexpected consent-required in --sandbox --yes python run; stderr={}",
        python_with_lock_unsafe_yes_stderr
    );
    assert!(
        !python_with_lock_unsafe_yes_stderr
            .contains("source/native|python execution requires explicit --sandbox opt-in"),
        "unexpected sandbox opt-in error in --sandbox --yes python run; stderr={}",
        python_with_lock_unsafe_yes_stderr
    );
    assert!(
        !python_with_lock_unsafe_yes_stderr
            .contains("uv.lock is required for source/python execution"),
        "unexpected uv.lock error in --sandbox --yes python run; stderr={}",
        python_with_lock_unsafe_yes_stderr
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_run_exported_python_cli_preserves_arg_order() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let project_dir = tmp.path().join("tool-project");
    let export_probe_path = tmp.path().join("export-probe.json");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(
        project_dir.join("capsule.toml"),
        r#"schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

default_target = "default"

[targets.default]
runtime = "source/python"
runtime_version = "3.11.10"
run = "python3 default.py --from-default"

[targets.export]
runtime = "source/python"
runtime_version = "3.11.10"
run = "python3 tool.py --from-target"
[exports.cli.tool]
kind = "python-tool"
target = "export"
args = ["--from-export"]
"#,
    )?;
    std::fs::write(project_dir.join("default.py"), "print('default')\n")?;
    std::fs::write(
        project_dir.join("tool.py"),
        format!(
            "import json, pathlib, sys\npathlib.Path({:?}).write_text(json.dumps({{\"marker\": \"export\", \"argv\": sys.argv[1:]}}))\n",
            export_probe_path.display().to_string()
        ),
    )?;
    std::fs::write(project_dir.join("uv.lock"), "# uv lock\n")?;

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_run_exported_python_cli_preserves_arg_order",
    )?
    else {
        return Ok(());
    };

    let artifact = build_capsule_artifact(&ato, &project_dir, "tool", &home_dir)?;
    publish_artifact_with_scoped_id(
        &ato,
        &artifact,
        "local/tool",
        &base_url,
        tmp.path(),
        &home_dir,
    )?;

    let fresh_run = run_ato_with_home_allow_unsafe(
        &ato,
        &[
            "run",
            "@local/tool",
            "--registry",
            &base_url,
            "--dangerously-skip-permissions",
            "--yes",
            "--",
            "--help",
            "--user-flag",
        ],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        fresh_run.status.success(),
        "fresh exported run failed: {}",
        String::from_utf8_lossy(&fresh_run.stderr)
    );
    let fresh_probe = read_probe_json(&export_probe_path)?;
    assert_eq!(fresh_probe["marker"], "export");
    assert_eq!(
        fresh_probe["argv"],
        serde_json::json!(["--from-target", "--from-export", "--help", "--user-flag"])
    );

    let installed_run = run_ato_with_home_allow_unsafe(
        &ato,
        &[
            "run",
            "@local/tool",
            "--registry",
            &base_url,
            "--dangerously-skip-permissions",
            "--yes",
            "--",
            "--help",
        ],
        tmp.path(),
        &home_dir,
    )?;
    assert!(
        installed_run.status.success(),
        "installed exported run failed: {}",
        String::from_utf8_lossy(&installed_run.stderr)
    );
    let installed_probe = read_probe_json(&export_probe_path)?;
    assert_eq!(installed_probe["marker"], "export");
    assert_eq!(
        installed_probe["argv"],
        serde_json::json!(["--from-target", "--from-export", "--help"])
    );

    Ok(())
}

#[test]
#[serial_test::serial]
fn e2e_local_registry_release_ops_reflect_current_and_yanked_state() -> Result<()> {
    let ato = assert_cmd::cargo::cargo_bin("ato");
    let tmp = TempDir::new().context("create temp dir")?;
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let data_dir = tmp.path().join("registry-data");
    let v1_dir = tmp.path().join("release-v1");
    let v2_dir = tmp.path().join("release-v2");
    std::fs::create_dir_all(&v1_dir)?;
    std::fs::create_dir_all(&v2_dir)?;

    for (dir, version, body) in [
        (&v1_dir, "1.0.0", "console.log('release v1');"),
        (&v2_dir, "2.0.0", "console.log('release v2');"),
    ] {
        std::fs::write(
            dir.join("capsule.toml"),
            format!(
                r#"schema_version = "0.3"
name = "release-ops"
version = "{version}"
type = "app"

runtime = "web/static"
port = 4173
run = "dist""#
            ),
        )?;
        std::fs::create_dir_all(dir.join("dist"))?;
        std::fs::write(
            dir.join("dist").join("index.html"),
            format!("<!doctype html><title>{body}</title>"),
        )?;
    }

    let Some((_guard, base_url)) = start_local_registry_or_skip(
        &ato,
        &data_dir,
        "e2e_local_registry_release_ops_reflect_current_and_yanked_state",
    )?
    else {
        return Ok(());
    };

    let scoped_id = "team-x/release-ops";
    let artifact_v1 = build_capsule_artifact(&ato, &v1_dir, "release-ops", &home_dir)?;
    publish_artifact_with_scoped_id(
        &ato,
        &artifact_v1,
        scoped_id,
        &base_url,
        tmp.path(),
        &home_dir,
    )?;

    let artifact_v2 = build_capsule_artifact(&ato, &v2_dir, "release-ops", &home_dir)?;
    publish_artifact_with_scoped_id(
        &ato,
        &artifact_v2,
        scoped_id,
        &base_url,
        tmp.path(),
        &home_dir,
    )?;

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("build release ops client")?;
    let detail_url = format!("{}/v1/manifest/capsules/by/team-x/release-ops", base_url);

    let mut detail: serde_json::Value = client
        .get(&detail_url)
        .send()
        .context("detail fetch after publish")?
        .error_for_status()
        .context("detail status after publish")?
        .json()
        .context("detail json after publish")?;

    let releases = detail
        .get("releases")
        .and_then(|value| value.as_array())
        .context("releases array missing after publish")?;
    let release_v1 = releases
        .iter()
        .find(|release| release.get("version").and_then(|value| value.as_str()) == Some("1.0.0"))
        .context("release 1.0.0 missing")?;
    let release_v2 = releases
        .iter()
        .find(|release| release.get("version").and_then(|value| value.as_str()) == Some("2.0.0"))
        .context("release 2.0.0 missing")?;

    let manifest_hash_v1 = release_v1
        .get("manifest_hash")
        .and_then(|value| value.as_str())
        .context("manifest hash missing for v1")?
        .to_string();
    let manifest_hash_v2 = release_v2
        .get("manifest_hash")
        .and_then(|value| value.as_str())
        .context("manifest hash missing for v2")?
        .to_string();

    assert_eq!(
        release_v1
            .get("is_current")
            .and_then(|value| value.as_bool()),
        Some(false),
        "v1 should not be current after v2 publish"
    );
    assert_eq!(
        release_v2
            .get("is_current")
            .and_then(|value| value.as_bool()),
        Some(true),
        "v2 should be current after latest publish"
    );
    assert!(
        release_v2.get("yanked_at").is_none() || release_v2.get("yanked_at").unwrap().is_null(),
        "freshly published release must not be yanked"
    );

    let rollback_response = client
        .post(format!("{}/v1/manifest/rollback", base_url))
        .json(&serde_json::json!({
            "scoped_id": scoped_id,
            "target_manifest_hash": manifest_hash_v1,
        }))
        .send()
        .context("rollback request")?;
    assert_eq!(rollback_response.status(), reqwest::StatusCode::OK);
    let rollback_payload: serde_json::Value =
        rollback_response.json().context("rollback response json")?;
    assert_eq!(
        rollback_payload
            .get("target_manifest_hash")
            .and_then(|value| value.as_str()),
        Some(manifest_hash_v1.as_str()),
        "rollback response should echo target_manifest_hash"
    );
    assert_eq!(
        rollback_payload
            .get("manifest_hash")
            .and_then(|value| value.as_str()),
        Some(manifest_hash_v1.as_str()),
        "rollback response should expose current manifest_hash at top level"
    );
    assert_eq!(
        rollback_payload
            .get("pointer")
            .and_then(|value| value.get("manifest_hash"))
            .and_then(|value| value.as_str()),
        Some(manifest_hash_v1.as_str()),
        "rollback response pointer should match target manifest"
    );

    let epoch_pointer: serde_json::Value = client
        .post(format!("{}/v1/manifest/epoch/resolve", base_url))
        .json(&serde_json::json!({ "scoped_id": scoped_id }))
        .send()
        .context("epoch resolve request")?
        .error_for_status()
        .context("epoch resolve status")?
        .json()
        .context("epoch resolve json")?;
    assert_eq!(
        epoch_pointer
            .get("pointer")
            .and_then(|value| value.get("manifest_hash"))
            .and_then(|value| value.as_str()),
        Some(manifest_hash_v1.as_str()),
        "epoch pointer should move to the rollback target"
    );

    detail = client
        .get(&detail_url)
        .send()
        .context("detail fetch after rollback")?
        .error_for_status()
        .context("detail status after rollback")?
        .json()
        .context("detail json after rollback")?;
    let releases = detail
        .get("releases")
        .and_then(|value| value.as_array())
        .context("releases array missing after rollback")?;
    let release_v1 = releases
        .iter()
        .find(|release| release.get("version").and_then(|value| value.as_str()) == Some("1.0.0"))
        .context("release 1.0.0 missing after rollback")?;
    let release_v2 = releases
        .iter()
        .find(|release| release.get("version").and_then(|value| value.as_str()) == Some("2.0.0"))
        .context("release 2.0.0 missing after rollback")?;
    assert_eq!(
        release_v1
            .get("is_current")
            .and_then(|value| value.as_bool()),
        Some(true),
        "v1 should be current after rollback"
    );
    assert_eq!(
        release_v2
            .get("is_current")
            .and_then(|value| value.as_bool()),
        Some(false),
        "v2 should no longer be current after rollback"
    );

    let yank_response = client
        .post(format!("{}/v1/manifest/yank", base_url))
        .json(&serde_json::json!({
            "scoped_id": scoped_id,
            "target_manifest_hash": manifest_hash_v2,
        }))
        .send()
        .context("yank request")?;
    assert!(
        yank_response.status().is_success(),
        "yank failed: {}",
        yank_response.text().unwrap_or_default()
    );

    detail = client
        .get(&detail_url)
        .send()
        .context("detail fetch after yank")?
        .error_for_status()
        .context("detail status after yank")?
        .json()
        .context("detail json after yank")?;
    let releases = detail
        .get("releases")
        .and_then(|value| value.as_array())
        .context("releases array missing after yank")?;
    let release_v2 = releases
        .iter()
        .find(|release| release.get("version").and_then(|value| value.as_str()) == Some("2.0.0"))
        .context("release 2.0.0 missing after yank")?;
    assert!(
        release_v2
            .get("yanked_at")
            .and_then(|value| value.as_str())
            .is_some(),
        "yanked release should expose yanked_at"
    );
    assert_eq!(
        release_v2
            .get("is_current")
            .and_then(|value| value.as_bool()),
        Some(false),
        "yanked v2 must remain non-current after rollback"
    );

    let resolve_v2 = client
        .get(format!(
            "{}/v1/manifest/resolve/team-x/release-ops/2.0.0",
            base_url
        ))
        .send()
        .context("resolve v2 after yank")?;
    assert_eq!(resolve_v2.status(), reqwest::StatusCode::GONE);
    let resolve_v2_body: serde_json::Value = resolve_v2.json().context("resolve v2 json")?;
    assert_eq!(
        resolve_v2_body
            .get("yanked")
            .and_then(|value| value.as_bool()),
        Some(true)
    );

    let resolve_v1 = client
        .get(format!(
            "{}/v1/manifest/resolve/team-x/release-ops/1.0.0",
            base_url
        ))
        .send()
        .context("resolve v1 after rollback")?;
    assert_eq!(resolve_v1.status(), reqwest::StatusCode::OK);

    Ok(())
}
