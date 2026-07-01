//! Track C PR 2a (#912): **capsule.toml source → bootable ext4 rootfs** (Docker-driven).
//!
//! This is the missing materialize → build → rootfs layer: `ato build` produces a `.ato`
//! archive (not bootable) and `build_ready_state` consumes *pre-built* ext4 bytes, so the
//! Track C builder must assemble the rootfs itself. This is a **pragmatic v1**, not the
//! final Ato build semantics.
//!
//! **Docker is a build TOOL, not the trust boundary.** The trust boundary is builder-host
//! isolation + KVM/Firecracker restore + seal + the no-secret scan + runner-side artifact
//! verification. This module only turns an approved, public, no-binding capsule on a known
//! runtime into an ext4 image; everything unsupported **fails closed**.
//!
//! Split: [`derive_build_spec`] is the pure, unit-testable gate + runtime detection;
//! [`materialize_source`] (git) and [`build_rootfs`] (docker → ext4) shell out and are
//! validated on a KVM+Docker builder host.

use std::path::{Path, PathBuf};
use std::process::Command;

use capsule::foundation::types::manifest::{CapsuleManifest, RuntimeType};
use serde::Serialize;

/// The narrow runtime subset the v1 Docker builder supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    StaticWeb,
    Node,
    Python,
}

/// A cheap probe of the source tree, so [`derive_build_spec`] stays pure + testable
/// without a real checkout. Populated by [`SourceProbe::scan`] over the materialized dir.
#[derive(Debug, Clone, Default)]
pub struct SourceProbe {
    pub has_package_json: bool,
    pub has_requirements_txt: bool,
    pub has_pyproject: bool,
    pub has_index_html: bool,
    /// Any top-level `*.py` file — a python signal for stdlib-only apps that ship no
    /// requirements.txt / pyproject.toml and declare no driver.
    pub has_py_files: bool,
}

impl SourceProbe {
    pub fn scan(dir: &Path) -> Self {
        let has = |f: &str| dir.join(f).exists();
        let has_py_files = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "py")))
            .unwrap_or(false);
        SourceProbe {
            has_package_json: has("package.json"),
            has_requirements_txt: has("requirements.txt"),
            has_pyproject: has("pyproject.toml"),
            has_index_html: has("index.html") || dir.join("public").join("index.html").exists(),
            has_py_files,
        }
    }
}

/// A resolved, buildable rootfs spec. Non-secret — safe to record in a receipt.
#[derive(Debug, Clone, Serialize)]
pub struct RootfsBuildSpec {
    pub runtime: RuntimeKind,
    pub base_image: String,
    pub install_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub start_cmd: String,
    pub port: u16,
    pub healthcheck: String,
}

/// Non-secret receipt of a produced rootfs.
#[derive(Debug, Clone, Serialize)]
pub struct RootfsReceipt {
    pub spec: RootfsBuildSpec,
    pub rootfs_path: String,
    pub rootfs_bytes: u64,
}

/// Reject unsupported/unsafe capsule shapes (**fail-closed**) and detect the runtime,
/// returning a buildable spec or a structured blocker reason. Pure — the source `probe`
/// is the only non-manifest input, so this is fully unit-testable.
///
/// Rejects (Phase 8 firewall + v1 scope): any required secret / binding, any external
/// capability, GPU, a runtime outside {static web, node source, python source}, and a
/// missing port or healthcheck. The start command (`execution.entrypoint`) is required.
pub fn derive_build_spec(m: &CapsuleManifest, probe: &SourceProbe) -> Result<RootfsBuildSpec, String> {
    if m.secrets.values().any(|s| s.required) {
        return Err("capsule requires secrets (secrets.*.required)".into());
    }
    if m.bindings.values().any(|b| b.required) {
        return Err("capsule requires bindings (bindings.*.required)".into());
    }
    if !m.external.is_empty() {
        return Err("capsule requires external services (external.*)".into());
    }
    if m.build.as_ref().map(|b| b.gpu).unwrap_or(false) {
        return Err("capsule requires GPU (build.gpu)".into());
    }

    // 0.3 runtime/port/healthcheck live on the default [targets.<label>], not [execution].
    let target = m.resolve_default_target().map_err(|e| e.to_string())?;
    let port = target.port.ok_or("capsule default target has no port")?;
    let healthcheck = target
        .readiness_probe
        .as_ref()
        .and_then(|r| r.http_get.clone())
        .filter(|h| !h.trim().is_empty())
        .ok_or("capsule default target has no http readiness_probe (healthcheck)")?;
    let start_cmd = target
        .run_command
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or("capsule default target has no run command")?;
    let build_cmd = target.build_command.clone().filter(|c| !c.trim().is_empty());

    // Runtime detection: prefer the explicit driver/language on the target, fall back to
    // the source probe. Only static web + node source + python source are supported (v1).
    let rt = RuntimeType::from_target_runtime(&target.runtime).unwrap_or(RuntimeType::Source);
    let driver = target.driver.as_deref().unwrap_or("").to_ascii_lowercase();
    let lang = target.language.as_deref().unwrap_or("").to_ascii_lowercase();
    let runtime = match rt.normalize() {
        RuntimeType::Web => RuntimeKind::StaticWeb,
        RuntimeType::Source => {
            if driver == "node" || lang == "javascript" || lang == "typescript" || probe.has_package_json {
                RuntimeKind::Node
            } else if driver == "python" || lang == "python" || probe.has_requirements_txt || probe.has_pyproject || probe.has_py_files {
                RuntimeKind::Python
            } else if driver == "static" || probe.has_index_html {
                RuntimeKind::StaticWeb
            } else {
                return Err("source runtime: no node (package.json/driver) or python (requirements.txt/pyproject/driver) detected".into());
            }
        }
        other => {
            return Err(format!("unsupported runtime {other:?} (v1 supports: static web, node source, python source)"));
        }
    };

    let (base_image, install_cmd) = match runtime {
        RuntimeKind::StaticWeb => ("python:3.11-slim".to_string(), None),
        RuntimeKind::Node => (
            "node:20-slim".to_string(),
            Some(if probe.has_package_json { "npm ci --omit=dev || npm install --omit=dev".to_string() } else { "true".to_string() }),
        ),
        RuntimeKind::Python => (
            "python:3.11-slim".to_string(),
            Some(if probe.has_requirements_txt {
                "pip install --no-cache-dir -r requirements.txt".to_string()
            } else if probe.has_pyproject {
                "pip install --no-cache-dir .".to_string()
            } else {
                // stdlib-only app — nothing to install.
                "true".to_string()
            }),
        ),
    };

    Ok(RootfsBuildSpec { runtime, base_image, install_cmd, build_cmd, start_cmd, port, healthcheck })
}

/// Materialize the **server-resolved** source: shallow-clone `owner/repo`, check out the
/// pinned `commit`, and return the (optionally sub-directoried) source root. Never trusts
/// a client-provided ref — the caller passes the identity resolved from the approved
/// store record.
pub fn materialize_source(owner: &str, repo: &str, commit: &str, subdir: Option<&str>, dest: &Path) -> Result<PathBuf, String> {
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("refusing non-pinned commit {commit:?} (need a full 40-char sha)"));
    }
    let url = format!("https://github.com/{owner}/{repo}.git");
    let run = |args: &[&str], cwd: Option<&Path>| -> Result<(), String> {
        let mut c = Command::new("git");
        c.args(args);
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        let out = c.output().map_err(|e| format!("git {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    };
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    run(&["init", "-q"], Some(dest))?;
    run(&["remote", "add", "origin", &url], Some(dest))?;
    run(&["fetch", "-q", "--depth", "1", "origin", commit], Some(dest))?;
    run(&["checkout", "-q", "FETCH_HEAD"], Some(dest))?;
    let root = match subdir.filter(|s| !s.is_empty()) {
        Some(s) => dest.join(s),
        None => dest.to_path_buf(),
    };
    if !root.join("capsule.toml").exists() {
        return Err(format!("no capsule.toml at resolved source root {}", root.display()));
    }
    Ok(root)
}

/// Build a bootable ext4 rootfs from a materialized `source_dir` + a resolved `spec`,
/// writing it to `out_ext4`. Shells out to `docker` (assemble the app filesystem) and
/// `mkfs.ext4`/`mount` (pack it) — the same mechanism as `build_rootfs_ro.sh`, driven by
/// the capsule instead of a synthetic image. Requires root (mount) + docker on the host.
pub fn build_rootfs(source_dir: &Path, spec: &RootfsBuildSpec, out_ext4: &Path, size_mib: u64) -> Result<RootfsReceipt, String> {
    let script = build_rootfs_script(spec, size_mib);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("ATO_SRC", source_dir)
        .env("ATO_OUT", out_ext4)
        .output()
        .map_err(|e| format!("spawn rootfs build: {e}"))?;
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stderr).lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(format!("rootfs build failed: {tail}"));
    }
    let rootfs_bytes = std::fs::metadata(out_ext4).map_err(|e| e.to_string())?.len();
    Ok(RootfsReceipt { spec: spec.clone(), rootfs_path: out_ext4.display().to_string(), rootfs_bytes })
}

/// The bash pipeline that turns the app image into a read-only-bootable ext4. Assembles a
/// Docker image (base + copy source + install + build), exports its filesystem, packs it
/// into a fresh ext4, and installs an init that execs the capsule's start command (which
/// serves the port + healthcheck). Kept as a reviewable string; env: ATO_SRC, ATO_OUT.
fn build_rootfs_script(spec: &RootfsBuildSpec, size_mib: u64) -> String {
    let install = spec.install_cmd.clone().unwrap_or_else(|| "true".into());
    let build = spec.build_cmd.clone().unwrap_or_else(|| "true".into());
    // The init execs the capsule start command as the whole userspace (like store_bench's
    // rootfs). start_cmd serves both the app port and the healthcheck path.
    let start = spec.start_cmd.replace('\'', "'\\''");
    format!(
        r#"set -euo pipefail
TAG="ato-rootfs-$$"
BUILD=$(mktemp -d)
cp -a "$ATO_SRC/." "$BUILD/"
cat > "$BUILD/Dockerfile" <<DOCKER
FROM {base}
WORKDIR /app
COPY . /app
RUN {install}
RUN {build}
DOCKER
docker build -q -t "$TAG" "$BUILD" >/dev/null
CID=$(docker create "$TAG")
mkdir -p "$BUILD/rootfs"
docker export "$CID" | tar -x -C "$BUILD/rootfs"
docker rm -f "$CID" >/dev/null; docker rmi -f "$TAG" >/dev/null 2>&1 || true
# Read-only-bootable init (matches benchmarks/ready-state/build_rootfs_ro.sh): mount the
# pseudo + tmpfs filesystems, then run the capsule start command in the background
# (serves port {port} + healthcheck {hc}) and keep PID 1 alive.
rm -f "$BUILD/rootfs/sbin/init"
cat > "$BUILD/rootfs/sbin/init" <<INIT
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mount -t tmpfs tmpfs /tmp 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mount -t tmpfs tmpfs /var/tmp 2>/dev/null
cd /app
( {start} ) >/tmp/app.log 2>&1 &
while true; do sleep 1000; done
INIT
chmod +x "$BUILD/rootfs/sbin/init"
rm -f "$ATO_OUT"
dd if=/dev/zero of="$ATO_OUT" bs=1M count={size} status=none
mkfs.ext4 -q -F "$ATO_OUT"
MNT=$(mktemp -d)
mount -o loop "$ATO_OUT" "$MNT"
cp -a "$BUILD/rootfs/." "$MNT/"
sync; umount "$MNT"; rmdir "$MNT"; rm -rf "$BUILD"
"#,
        base = spec.base_image,
        install = install,
        build = build,
        start = start,
        port = spec.port,
        hc = spec.healthcheck,
        size = size_mib,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::foundation::types::manifest::CapsuleManifest;

    fn base_toml() -> String {
        r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python3 app.py"
port = 8080
readiness_probe = { http_get = "/health" }
"#
        .to_string()
    }

    fn parse(toml: &str) -> CapsuleManifest {
        CapsuleManifest::from_toml(toml).expect("parse capsule.toml")
    }

    fn probe_python() -> SourceProbe {
        SourceProbe { has_requirements_txt: true, ..Default::default() }
    }

    #[test]
    fn python_source_derives_a_spec() {
        let m = parse(&base_toml());
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.runtime, RuntimeKind::Python);
        assert_eq!(spec.base_image, "python:3.11-slim");
        assert_eq!(spec.port, 8080);
        assert_eq!(spec.healthcheck, "/health");
        assert_eq!(spec.start_cmd, "python3 app.py");
        assert!(spec.install_cmd.unwrap().contains("pip install"));
    }

    #[test]
    fn node_detected_from_package_json() {
        let m = parse(&base_toml().replace("python3 app.py", "node server.js"));
        let spec = derive_build_spec(&m, &SourceProbe { has_package_json: true, ..Default::default() }).unwrap();
        assert_eq!(spec.runtime, RuntimeKind::Node);
        assert_eq!(spec.base_image, "node:20-slim");
    }

    #[test]
    fn source_without_a_detectable_language_fails_closed() {
        let m = parse(&base_toml());
        let err = derive_build_spec(&m, &SourceProbe::default()).unwrap_err();
        assert!(err.contains("no node") && err.contains("python"), "{err}");
    }

    #[test]
    fn stdlib_python_detected_from_py_files_with_no_install() {
        // A python app that ships only *.py (no requirements/pyproject, no driver).
        let m = parse(&base_toml());
        let spec = derive_build_spec(&m, &SourceProbe { has_py_files: true, ..Default::default() }).unwrap();
        assert_eq!(spec.runtime, RuntimeKind::Python);
        assert_eq!(spec.install_cmd.as_deref(), Some("true")); // nothing to install
    }

    #[test]
    fn required_secret_binding_external_gpu_all_fail_closed() {
        let secret = format!("{}\n[secrets.api_key]\nrequired = true\nenv = \"API_KEY\"\ndelivery = \"proxy\"\n", base_toml());
        assert!(derive_build_spec(&parse(&secret), &probe_python()).unwrap_err().contains("secrets"));
        let binding = format!("{}\n[bindings.user_files]\nkind = \"user_files\"\nrequired = true\nscope = \"user\"\n", base_toml());
        assert!(derive_build_spec(&parse(&binding), &probe_python()).unwrap_err().contains("bindings"));
        let external = format!("{}\n[external.gpu]\ntype = \"gpu\"\nrequired = false\n", base_toml());
        assert!(derive_build_spec(&parse(&external), &probe_python()).unwrap_err().contains("external"));
    }

    #[test]
    fn missing_port_or_healthcheck_fails_closed() {
        let no_port = base_toml().replace("port = 8080\n", "");
        assert!(derive_build_spec(&parse(&no_port), &probe_python()).unwrap_err().contains("port"));
        let no_hc = base_toml().replace("readiness_probe = { http_get = \"/health\" }\n", "");
        assert!(derive_build_spec(&parse(&no_hc), &probe_python()).unwrap_err().contains("health"));
    }

    #[test]
    fn materialize_rejects_a_non_pinned_commit() {
        let dir = tempfile::tempdir().unwrap();
        let err = materialize_source("acme", "app", "main", None, dir.path()).unwrap_err();
        assert!(err.contains("non-pinned"));
    }
}
