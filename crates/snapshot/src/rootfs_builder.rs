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
    // Any binding disqualifies a v1 no-binding snapshot — this is also how user-files
    // and oauth are declared (BindingKind::UserFiles / ::Oauth), so it rejects those too.
    if !m.bindings.is_empty() {
        let kinds: Vec<String> = m.bindings.values().map(|b| format!("{:?}", b.kind).to_ascii_lowercase()).collect();
        return Err(format!("capsule declares bindings ({}) — v1 is no-binding only", kinds.join(", ")));
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
    // Manifest commands must be single-line + NUL-free: they are embedded (single-quoted)
    // into a generated Dockerfile/init, and a newline could break out of the quoting or the
    // heredoc delimiter. A NUL can't survive the shell either. Fail closed.
    reject_control_chars("run command", &start_cmd)?;
    if let Some(b) = &build_cmd {
        reject_control_chars("build command", b)?;
    }

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

/// A conservative GitHub **owner** login: 1–39 chars, alphanumeric or single hyphens,
/// not starting/ending with a hyphen. Anything else (empty, `/`, `..`, path-like) fails.
pub fn valid_github_owner(owner: &str) -> bool {
    let ok_len = (1..=39).contains(&owner.len());
    let ok_chars = owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_alphanumeric());
    ok_len && ok_chars && ends(owner.bytes().next()) && ends(owner.bytes().next_back())
}

/// A conservative GitHub **repo** name: 1–100 chars of `[A-Za-z0-9._-]`, excluding the
/// pathological `.` / `..`.
pub fn valid_github_repo(repo: &str) -> bool {
    let ok_len = (1..=100).contains(&repo.len());
    let ok_chars = repo.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    ok_len && ok_chars && repo != "." && repo != ".."
}

/// Validate a relative `subdir` **before** it is joined to the checkout: reject absolute
/// paths, any `..` component, and non-normal components (root/prefix). The canonical
/// containment check after checkout closes symlink traversal.
fn validate_subdir(subdir: &str) -> Result<(), String> {
    use std::path::Component;
    let p = Path::new(subdir);
    if p.is_absolute() {
        return Err(format!("subdir {subdir:?} must be relative"));
    }
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err(format!("subdir {subdir:?} may not contain '..'")),
            Component::RootDir | Component::Prefix(_) => return Err(format!("subdir {subdir:?} has an illegal prefix")),
        }
    }
    Ok(())
}

/// Materialize the **server-resolved** source: shallow-clone `owner/repo`, check out the
/// pinned `commit`, and return the (optionally sub-directoried) source root. Never trusts
/// a client-provided ref — the caller passes the identity resolved from the approved
/// store record — and treats even that record as an input boundary: `owner`/`repo` are
/// validated as GitHub identities, `commit` must be a pinned 40-hex sha, and `subdir`
/// cannot escape the checkout (lexical + canonical containment).
pub fn materialize_source(owner: &str, repo: &str, commit: &str, subdir: Option<&str>, dest: &Path) -> Result<PathBuf, String> {
    if !valid_github_owner(owner) {
        return Err(format!("invalid github owner {owner:?}"));
    }
    if !valid_github_repo(repo) {
        return Err(format!("invalid github repo {repo:?}"));
    }
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("refusing non-pinned commit {commit:?} (need a full 40-char sha)"));
    }
    if let Some(s) = subdir.filter(|s| !s.is_empty()) {
        validate_subdir(s)?;
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

    contained_source_root(dest, subdir)
}

/// Resolve `dest`/`subdir` to a source root that is provably **inside** the checkout, with
/// a `capsule.toml`. Validates the subdir lexically, then canonicalizes both paths and
/// requires containment (closing symlink traversal). Split out so the containment logic is
/// unit-testable without a network clone.
pub(crate) fn contained_source_root(dest: &Path, subdir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(s) = subdir.filter(|s| !s.is_empty()) {
        validate_subdir(s)?;
    }
    let root = match subdir.filter(|s| !s.is_empty()) {
        Some(s) => dest.join(s),
        None => dest.to_path_buf(),
    };
    let dest_canon = dest.canonicalize().map_err(|e| format!("canonicalize checkout: {e}"))?;
    let root_canon = root.canonicalize().map_err(|e| format!("resolved source root {} not found: {e}", root.display()))?;
    if !root_canon.starts_with(&dest_canon) {
        return Err(format!("subdir escapes the checkout: {} is outside {}", root_canon.display(), dest_canon.display()));
    }
    if !root_canon.join("capsule.toml").exists() {
        return Err(format!("no capsule.toml at resolved source root {}", root_canon.display()));
    }
    Ok(root_canon)
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

/// Reject NUL bytes and line breaks in a manifest-derived command (v1 requires a single
/// shell command). A newline could escape the single-quoting / heredoc delimiter.
fn reject_control_chars(label: &str, cmd: &str) -> Result<(), String> {
    if cmd.contains('\0') {
        return Err(format!("{label} contains a NUL byte"));
    }
    if cmd.contains('\n') || cmd.contains('\r') {
        return Err(format!("{label} contains a newline (v1 requires a single-line command)"));
    }
    Ok(())
}

/// Wrap `s` as a single POSIX-shell single-quoted argument (`abc'def` → `'abc'\''def'`),
/// so a manifest-derived command is passed as ONE literal argument to `/bin/sh -lc`,
/// never re-parsed. Combined with quoted heredocs, capsule commands can never be expanded
/// by the builder-host shell.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The bash pipeline that turns the app image into a read-only-bootable ext4. Assembles a
/// Docker image (base + copy source + install + build), exports its filesystem, packs it
/// into a fresh ext4, and installs an init that runs the capsule's start command (which
/// serves the port + healthcheck). Kept as a reviewable string; env: ATO_SRC, ATO_OUT.
///
/// Security: the Dockerfile and init are written with **quoted** heredocs (`<<'DOCKER'`,
/// `<<'INIT'`) so the builder-host shell performs NO expansion of their bodies, and the
/// manifest-derived install/build/start commands are embedded as single-quoted arguments
/// to `/bin/sh -lc`. So a capsule command containing `$(...)`/backticks runs only inside
/// Docker's RUN (build) or the guest init (start) — never on the builder host.
fn build_rootfs_script(spec: &RootfsBuildSpec, size_mib: u64) -> String {
    let install_q = shell_single_quote(spec.install_cmd.as_deref().unwrap_or("true"));
    let build_q = shell_single_quote(spec.build_cmd.as_deref().unwrap_or("true"));
    let start_q = shell_single_quote(&spec.start_cmd);
    format!(
        r#"set -euo pipefail
TAG="ato-rootfs-$$"
CID=""
MNT=""
BUILD=$(mktemp -d)
# Failure-safe cleanup: on ANY exit (success or a failed build/export/mount/cp) leave no
# container, image, mount, or temp dir behind (Phase 8 orphan-hardening parity).
cleanup() {{
  [ -n "$CID" ] && docker rm -f "$CID" >/dev/null 2>&1 || true
  docker rmi -f "$TAG" >/dev/null 2>&1 || true
  if [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null; then umount "$MNT" 2>/dev/null || umount -l "$MNT" 2>/dev/null || true; fi
  [ -n "$MNT" ] && rmdir "$MNT" 2>/dev/null || true
  [ -n "$BUILD" ] && rm -rf "$BUILD" 2>/dev/null || true
}}
trap cleanup EXIT
cp -a "$ATO_SRC/." "$BUILD/"
# QUOTED heredoc: no host expansion; commands run inside Docker RUN via sh -lc '<literal>'.
cat > "$BUILD/Dockerfile" <<'DOCKER'
FROM {base}
WORKDIR /app
COPY . /app
RUN /bin/sh -lc {install_q}
RUN /bin/sh -lc {build_q}
DOCKER
docker build -q -t "$TAG" "$BUILD" >/dev/null
CID=$(docker create "$TAG")
mkdir -p "$BUILD/rootfs"
docker export "$CID" | tar -x -C "$BUILD/rootfs"
docker rm -f "$CID" >/dev/null; CID=""
# Read-only-bootable init (matches benchmarks/ready-state/build_rootfs_ro.sh): mount the
# pseudo + tmpfs filesystems, then run the capsule start command in the background
# (serves port {port} + healthcheck {hc}) and keep PID 1 alive. QUOTED heredoc: the
# start command runs only in the GUEST via sh -lc '<literal>'.
rm -f "$BUILD/rootfs/sbin/init"
cat > "$BUILD/rootfs/sbin/init" <<'INIT'
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
/bin/sh -lc {start_q} >/tmp/app.log 2>&1 &
while true; do sleep 1000; done
INIT
chmod +x "$BUILD/rootfs/sbin/init"
rm -f "$ATO_OUT"
dd if=/dev/zero of="$ATO_OUT" bs=1M count={size} status=none
mkfs.ext4 -q -F "$ATO_OUT"
MNT=$(mktemp -d)
mount -o loop "$ATO_OUT" "$MNT"
cp -a "$BUILD/rootfs/." "$MNT/"
sync; umount "$MNT"
# MNT/BUILD are removed by the EXIT trap (also on any failure above).
"#,
        base = spec.base_image,
        install_q = install_q,
        build_q = build_q,
        start_q = start_q,
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
        let sha = "a".repeat(40);
        assert!(materialize_source("acme", "app", "main", None, dir.path()).unwrap_err().contains("non-pinned"));
        // path-like / invalid owner + repo are rejected before any network use.
        assert!(materialize_source("../evil", "app", &sha, None, dir.path()).unwrap_err().contains("owner"));
        assert!(materialize_source("acme/x", "app", &sha, None, dir.path()).unwrap_err().contains("owner"));
        assert!(materialize_source("acme", "a/b", &sha, None, dir.path()).unwrap_err().contains("repo"));
        assert!(materialize_source("acme", "..", &sha, None, dir.path()).unwrap_err().contains("repo"));
        assert!(materialize_source("acme", "", &sha, None, dir.path()).unwrap_err().contains("repo"));
    }

    #[test]
    fn github_identity_validation() {
        assert!(valid_github_owner("acme") && valid_github_owner("a-b-1") && valid_github_owner("A9"));
        assert!(!valid_github_owner("") && !valid_github_owner("-a") && !valid_github_owner("a-") && !valid_github_owner("a/b") && !valid_github_owner(".."));
        assert!(valid_github_repo("my.app_1-x") && valid_github_repo("a"));
        assert!(!valid_github_repo("") && !valid_github_repo(".") && !valid_github_repo("..") && !valid_github_repo("a/b") && !valid_github_repo("a b"));
    }

    #[test]
    fn subdir_escape_is_rejected_lexically_and_canonically() {
        // Lexical: absolute + parent-dir rejected before any fs access.
        assert!(validate_subdir("/etc").unwrap_err().contains("relative"));
        assert!(validate_subdir("../x").unwrap_err().contains(".."));
        assert!(validate_subdir("a/../../b").unwrap_err().contains(".."));
        assert!(validate_subdir("sub/dir").is_ok());

        // Canonical: a symlinked subdir that resolves OUTSIDE the checkout is rejected.
        let checkout = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("capsule.toml"), b"x").unwrap();
        // in-checkout subdir with a capsule.toml is accepted.
        std::fs::create_dir_all(checkout.path().join("app")).unwrap();
        std::fs::write(checkout.path().join("app").join("capsule.toml"), b"x").unwrap();
        assert!(contained_source_root(checkout.path(), Some("app")).is_ok());
        // a symlink pointing outside ⇒ containment fails.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), checkout.path().join("evil")).unwrap();
            let err = contained_source_root(checkout.path(), Some("evil")).unwrap_err();
            assert!(err.contains("escapes the checkout"), "{err}");
        }
    }

    #[test]
    fn non_required_binding_is_also_rejected() {
        // user-files / oauth are BindingKinds; any binding (even required=false) is out.
        let uf = format!("{}\n[bindings.user_files]\nkind = \"user_files\"\nrequired = false\nscope = \"user\"\n", base_toml());
        assert!(derive_build_spec(&parse(&uf), &probe_python()).unwrap_err().contains("binding"));
        let oauth = format!("{}\n[bindings.login]\nkind = \"oauth\"\nrequired = false\nscope = \"user\"\n", base_toml());
        assert!(derive_build_spec(&parse(&oauth), &probe_python()).unwrap_err().contains("binding"));
    }

    #[test]
    fn build_script_has_a_failure_cleanup_trap() {
        let spec = RootfsBuildSpec {
            runtime: RuntimeKind::Python,
            base_image: "python:3.11-slim".into(),
            install_cmd: Some("true".into()),
            build_cmd: None,
            start_cmd: "python3 app.py".into(),
            port: 8080,
            healthcheck: "/health".into(),
        };
        let script = build_rootfs_script(&spec, 512);
        assert!(script.contains("trap cleanup EXIT"), "script must install an EXIT cleanup trap");
        assert!(script.contains("docker rm -f") && script.contains("docker rmi -f") && script.contains("umount"), "cleanup must reap container/image/mount");
    }

    #[test]
    fn manifest_commands_cannot_expand_on_the_builder_host() {
        // A malicious build/run command with a command substitution.
        let evil = "echo $(touch /tmp/ato-host-pwned)";
        let spec = RootfsBuildSpec {
            runtime: RuntimeKind::Python,
            base_image: "python:3.11-slim".into(),
            install_cmd: Some("true".into()),
            build_cmd: Some(evil.into()),
            start_cmd: evil.into(),
            port: 8080,
            healthcheck: "/health".into(),
        };
        let script = build_rootfs_script(&spec, 512);
        // Heredocs are QUOTED ⇒ the builder host performs no expansion of their bodies.
        assert!(script.contains("<<'DOCKER'") && script.contains("<<'INIT'"), "heredocs must be quoted");
        // The command appears as a single-quoted argument to sh -lc (Docker RUN + guest init),
        // never as a bare host-shell token.
        assert!(script.contains("RUN /bin/sh -lc 'echo $(touch /tmp/ato-host-pwned)'"), "build cmd must be a single-quoted Docker RUN arg");
        assert!(script.contains("/bin/sh -lc 'echo $(touch /tmp/ato-host-pwned)' >/tmp/app.log"), "start cmd must be a single-quoted guest-init arg");
        // And there is no UNquoted occurrence that the host would expand.
        assert!(!script.contains("( echo $(touch"), "must not embed the command raw");
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quotes() {
        assert_eq!(shell_single_quote("abc"), "'abc'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        // A closing-quote injection attempt stays inside one quoted argument.
        assert_eq!(shell_single_quote("'; rm -rf /"), "''\\''; rm -rf /'");
    }

    #[test]
    fn newline_or_nul_in_a_command_fails_closed() {
        let nl = base_toml().replace("run = \"python3 app.py\"", "run = \"python3 app.py\\nrm -rf /\"");
        assert!(derive_build_spec(&parse(&nl), &probe_python()).unwrap_err().contains("newline"));
    }
}
