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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use capsule::foundation::types::manifest::{CapsuleManifest, RuntimeType};
use capsule::foundation::types::ready_state::SecretDelivery;
use protocol::binding_lease::BindingName;
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

/// v1.2 (#912): the supervisor build config emitted into the rootfs
/// (`/etc/ato/supervisor.json`) for a `delivery = "env"` secret capsule. Holds NO
/// secret value — only the `ENV_VAR → binding name` map whose tmpfs file the
/// guest-agent reads at exec. Non-secret; safe in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorBuildSpec {
    /// The binding names the guest-agent requires before it starts the workload
    /// (the run gate delivers a lease per name). = the declared `[secrets.*]` names.
    pub binding_names: Vec<String>,
    /// `ENV_VAR → binding name` (from each secret's `env` or its name).
    pub env_map: BTreeMap<String, String>,
}

/// A resolved, buildable rootfs spec. Non-secret — safe to record in a receipt.
#[derive(Debug, Clone, Serialize)]
pub struct RootfsBuildSpec {
    pub runtime: RuntimeKind,
    pub base_image: String,
    pub install_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub start_cmd: String,
    /// #932: the manifest-declared run command, verbatim — diagnostics only
    /// (`start_cmd` is what actually runs in the guest, post normalization).
    pub declared_start_cmd: String,
    pub port: u16,
    pub healthcheck: String,
    /// #932: true when the readiness probe was synthesized from the declared port
    /// (no explicit `readiness_probe.http_get` on the default target).
    pub probe_synthesized: bool,
    /// v1.2: when `Some`, this is a SUPERVISOR build — init runs the guest-agent
    /// (which starts the workload after bindings are delivered) instead of launching
    /// the app directly, and `/etc/ato/supervisor.json` is written. `None` = the v1.0
    /// no-binding path (byte-identical to before).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<SupervisorBuildSpec>,
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
    let port = target.port.ok_or("capsule default target has no port (declare `port = <n>` on the default target)")?;
    // #932: an explicit `readiness_probe.http_get` wins; otherwise SYNTHESIZE one from
    // the declared port. The capsule contract the CLI/runner path honors is "a declared
    // port ⇒ Ato synthesizes an honest readiness probe"; the snapshot boot-verify
    // probes over HTTP (GET expecting 200), so the synthesized form is `http_get "/"`
    // rather than a bare TCP connect. A capsule whose root path does not answer 200
    // still fails honestly at build_ready_state — and the synthesis is recorded in the
    // receipt (`synthesized_probe`) either way, never silently.
    let explicit_http = target
        .readiness_probe
        .as_ref()
        .and_then(|r| r.http_get.clone())
        .filter(|h| !h.trim().is_empty());
    let probe_synthesized = explicit_http.is_none();
    let healthcheck = explicit_http.unwrap_or_else(|| "/".to_string());
    let declared_start_cmd = target
        .run_command
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or("capsule default target has no run command")?;
    // #932: mirror the CLI's bare-`.py` convention (executors/source.rs
    // is_python_launch_spec): a run command that is a SINGLE token ending in `.py` —
    // the form real Store capsules use because a `python`-prefixed command
    // mis-composes in the CLI source sandbox — cannot exec as-is in the guest's
    // `sh -lc '<cmd>'`. Normalize it to `python3 <script>` (the python/static base
    // images guarantee python3). Multi-token commands are left verbatim: they are an
    // explicit shell command, not a bare entrypoint.
    let start_cmd = {
        let t = declared_start_cmd.trim();
        if !t.contains(char::is_whitespace) && t.to_ascii_lowercase().ends_with(".py") {
            format!("python3 {t}")
        } else {
            declared_start_cmd.clone()
        }
    };
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

    Ok(RootfsBuildSpec { runtime, base_image, install_cmd, build_cmd, start_cmd, declared_start_cmd, port, healthcheck, probe_synthesized, supervisor: None })
}

/// A POSIX-ish environment variable name: `^[A-Za-z_][A-Za-z0-9_]*$`. The name is
/// interpolated into the generated `supervisor.json` + the guest spawn script, so a
/// malformed name is **rejected at emission** (fail-closed), never emitted — mirroring
/// the guest-agent's own validation (#947) so a broken `supervisor.json` is never built.
fn valid_env_var_name(name: &str) -> bool {
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// v1.2 (#912): derive a **supervisor** build spec for a `delivery = "env"` secret
/// capsule. The workload is launched by the guest-agent (which starts it with the
/// composed env only after the bindings are delivered), so the rootfs runs the
/// agent-as-init and carries `/etc/ato/supervisor.json`.
///
/// Fail-closed, and it relaxes ONLY the secret gate: at least one `[secrets.*]` must
/// be declared and every one must be `delivery = "env"` — `file`/`proxy`/`fd` are NOT
/// env injection and belong to later binding paths, so they are rejected here; a
/// duplicate resolved env var is rejected (a half-used binding would desync preflight /
/// bound-ready from the actual injection); non-secret `[bindings.*]`, `[external.*]`,
/// and GPU stay rejected. All runtime/port/command detection is the exact same tested
/// logic as the no-binding path — this reuses [`derive_build_spec`] on a secret-stripped
/// manifest, so it cannot drift, then attaches the supervisor config.
pub fn derive_supervisor_build_spec(m: &CapsuleManifest, probe: &SourceProbe) -> Result<RootfsBuildSpec, String> {
    if m.secrets.is_empty() {
        return Err("supervisor build requires at least one [secrets.*] (delivery = \"env\")".into());
    }
    for (name, s) in &m.secrets {
        // Only `env` delivery is supervisor env injection. `file` is a request-time
        // read (a later file-binding path), and `proxy`/`fd` never inject an env var.
        if s.delivery != SecretDelivery::Env {
            return Err(format!(
                "secret '{name}': delivery {:?} is not supported by the v1.2 supervisor \
                 (delivery = \"env\" only; file/proxy/fd are out of scope here)",
                s.delivery
            ));
        }
    }
    // Reuse every no-binding gate + detection by deriving on a secret-stripped clone:
    // this rejects non-secret bindings / external / GPU / unsupported runtime and
    // resolves the runtime/port/command identically to the v1.0 path.
    let mut stripped = m.clone();
    stripped.secrets.clear();
    let mut spec = derive_build_spec(&stripped, probe)?;

    // ENV_VAR → binding name (the binding name IS the secret name; the run gate
    // delivers a lease per name). Env var = the secret's `env`, else its name.
    // Fail-closed on THREE axes, all at emission so a broken supervisor.json is never
    // built: (1) the secret name is used verbatim as the binding name — as the agent's
    // argv AND the `bindings_env` value the guest-agent revalidates with
    // `BindingName::parse` (#947) — so it must be a valid `BindingName` (`[a-z0-9_.-]`,
    // lowercase); a mismatch here is exactly what made the agent `exit(2)` before it
    // opened the vsock listener. (2) the resolved env var name must be a POSIX
    // identifier. (3) two secrets → one env var is rejected (a half-used binding would
    // desync preflight/bound-ready from the actual injection). Note (1) and (2) use
    // DIFFERENT alphabets on purpose: an env var is conventionally UPPERCASE, a binding
    // name is lowercase — so an uppercase env var uses a lowercase secret key plus an
    // explicit `env`, e.g. `[secrets.openai_api_key] env = "OPENAI_API_KEY"`.
    let mut env_map: BTreeMap<String, String> = BTreeMap::new();
    let mut binding_names = Vec::with_capacity(m.secrets.len());
    for (name, s) in &m.secrets {
        if let Err(e) = BindingName::parse(name.as_str()) {
            return Err(format!(
                "secret '{name}': the secret name is used as the binding name and must be a \
                 valid BindingName ({e}). If you need an uppercase environment variable, use a \
                 lowercase secret key with an explicit `env`, e.g. \
                 [secrets.openai_api_key] env = \"OPENAI_API_KEY\""
            ));
        }
        let var = s.env.clone().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| name.clone());
        if !valid_env_var_name(&var) {
            return Err(format!(
                "secret '{name}': env var name {var:?} is not a POSIX identifier \
                 (^[A-Za-z_][A-Za-z0-9_]*$)"
            ));
        }
        if let Some(prev) = env_map.insert(var.clone(), name.clone()) {
            return Err(format!(
                "secrets '{prev}' and '{name}' both resolve to env var {var:?} — \
                 duplicate env injection is ambiguous (fail-closed)"
            ));
        }
        binding_names.push(name.clone());
    }
    binding_names.sort();
    spec.supervisor = Some(SupervisorBuildSpec { binding_names, env_map });
    Ok(spec)
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
///
/// #932 `manifest_override`: the APPROVED store recipe manifest (server-resolved
/// `capsule_source_recipes.recipe_toml`, carried on the claim). When `Some`, it is
/// written as `capsule.toml` at the source root — AUTHORITATIVE over any repo file
/// (the Store-apply publish model stores the manifest server-side precisely because
/// upstream repos carry none). When `None` (raw-GitHub capsule jobs), the repo's own
/// `capsule.toml` is required, fail-closed exactly as before.
pub fn materialize_source(owner: &str, repo: &str, commit: &str, subdir: Option<&str>, manifest_override: Option<&str>, dest: &Path) -> Result<PathBuf, String> {
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

    let root = contained_source_root(dest, subdir, manifest_override.is_none())?;
    if let Some(toml) = manifest_override {
        // The recipe manifest is authoritative for Store-recipe jobs: write it at the
        // contained root (overwriting a repo capsule.toml if one exists) so every later
        // stage — manifest parse, eligibility, rootfs COPY — sees exactly the approved
        // recipe, never a divergent repo file.
        std::fs::write(root.join("capsule.toml"), toml)
            .map_err(|e| format!("write recipe manifest as capsule.toml: {e}"))?;
    }
    Ok(root)
}

/// Resolve `dest`/`subdir` to a source root that is provably **inside** the checkout.
/// Validates the subdir lexically, then canonicalizes both paths and requires containment
/// (closing symlink traversal). `require_manifest` demands a repo `capsule.toml` at the
/// root (the raw-GitHub path); recipe-manifest jobs pass `false` and write the approved
/// recipe there instead (#932). Split out so the containment logic is unit-testable
/// without a network clone.
pub(crate) fn contained_source_root(dest: &Path, subdir: Option<&str>, require_manifest: bool) -> Result<PathBuf, String> {
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
    if require_manifest && !root_canon.join("capsule.toml").exists() {
        return Err(format!(
            "no capsule.toml at resolved source root {} (and the claim carried no recipe manifest)",
            root_canon.display()
        ));
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

    // v1.2 supervisor build: init runs the guest-agent (which starts the workload with
    // the composed env after bindings arrive) instead of launching the app; the agent
    // binary + /etc/ato/supervisor.json are staged into the rootfs. `agent_prep` runs
    // after `docker export`; `launch` replaces the direct app launch. Both are empty
    // for the v1.0 no-binding path, so that script stays byte-identical.
    let (agent_prep, launch) = match &spec.supervisor {
        None => (String::new(), format!("/bin/sh -lc {start_q} >/tmp/app.log 2>&1 &")),
        Some(sup) => {
            // supervisor.json (no secret value — env var → binding name only).
            let cfg = serde_json::json!({
                "cmd": ["/bin/sh", "-lc", spec.start_cmd],
                "cwd": "/app",
                "base_env": { "PORT": spec.port.to_string() },
                "bindings_env": sup.env_map,
            });
            let cfg_json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
            // Binding names become the agent's argv — shell-quote each defensively.
            let args = sup
                .binding_names
                .iter()
                .map(|n| shell_single_quote(n))
                .collect::<Vec<_>>()
                .join(" ");
            let prep = format!(
                r#"# v1.2 supervisor: stage the guest-agent + its config into the rootfs.
: "${{ATO_GUEST_AGENT_BIN:?ATO_GUEST_AGENT_BIN must point to the guest-agent binary for a supervisor build}}"
mkdir -p "$BUILD/rootfs/usr/local/bin" "$BUILD/rootfs/etc/ato" "$BUILD/rootfs/run/ato/bindings"
cp "$ATO_GUEST_AGENT_BIN" "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
chmod 0755 "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
cat > "$BUILD/rootfs/etc/ato/supervisor.json" <<'ATOSUPERVISORJSON'
{cfg_json}
ATOSUPERVISORJSON"#
            );
            // The agent is the supervisor: vsock control plane on 1025, required
            // bindings as argv. It reads /etc/ato/supervisor.json and starts the app
            // only once every binding is delivered (bound-ready).
            let launch = format!(
                "mkdir -p /run/ato/bindings\n\
                 export ATO_GUEST_AGENT_MODE=vsock ATO_GUEST_AGENT_VSOCK_PORT=1025 ATO_BINDINGS_ROOT=/run/ato/bindings\n\
                 /usr/local/bin/ato-guest-agent {args} >/tmp/agent.log 2>&1 &"
            );
            (prep, launch)
        }
    };
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
{agent_prep}
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
{launch}
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
        agent_prep = agent_prep,
        launch = launch,
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
        assert!(!spec.probe_synthesized); // explicit http_get is honored, never replaced
        assert_eq!(spec.start_cmd, "python3 app.py");
        assert_eq!(spec.declared_start_cmd, "python3 app.py"); // multi-token: untouched
        assert!(spec.install_cmd.unwrap().contains("pip install"));
    }

    #[test]
    fn bare_py_run_command_normalizes_to_python3() {
        // #932: a single-token `.py` run command (the CLI-sandbox convention real Store
        // capsules use) execs as `python3 <script>` in the guest; the declared form is
        // preserved for the receipt.
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"app.py\""));
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.start_cmd, "python3 app.py");
        assert_eq!(spec.declared_start_cmd, "app.py");
        // A path-y bare script normalizes too.
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"scripts/serve.py\""));
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.start_cmd, "python3 scripts/serve.py");
        // Multi-token commands are explicit shell commands — verbatim, even when they
        // end in `.py` (never `python3 python app.py`).
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"python app.py\""));
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.start_cmd, "python app.py");
        // Non-python commands are untouched.
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"node server.js\""));
        let spec = derive_build_spec(&m, &SourceProbe { has_package_json: true, ..Default::default() }).unwrap();
        assert_eq!(spec.start_cmd, "node server.js");
        assert_eq!(spec.declared_start_cmd, "node server.js");
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
    fn missing_port_fails_closed_but_missing_probe_synthesizes() {
        // No port: still fail-closed — nothing to probe, nothing to proxy.
        let no_port = base_toml().replace("port = 8080\n", "");
        assert!(derive_build_spec(&parse(&no_port), &probe_python()).unwrap_err().contains("port"));
        // #932: no explicit readiness_probe but a declared port ⇒ synthesize `http_get "/"`
        // (the port⇒probe contract the CLI/runner path honors), recorded as synthesized.
        let no_hc = base_toml().replace("readiness_probe = { http_get = \"/health\" }\n", "");
        let spec = derive_build_spec(&parse(&no_hc), &probe_python()).unwrap();
        assert_eq!(spec.healthcheck, "/");
        assert!(spec.probe_synthesized);
        assert_eq!(spec.port, 8080);
    }

    #[test]
    fn materialize_rejects_a_non_pinned_commit() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "a".repeat(40);
        assert!(materialize_source("acme", "app", "main", None, None, dir.path()).unwrap_err().contains("non-pinned"));
        // path-like / invalid owner + repo are rejected before any network use.
        assert!(materialize_source("../evil", "app", &sha, None, None, dir.path()).unwrap_err().contains("owner"));
        assert!(materialize_source("acme/x", "app", &sha, None, None, dir.path()).unwrap_err().contains("owner"));
        assert!(materialize_source("acme", "a/b", &sha, None, None, dir.path()).unwrap_err().contains("repo"));
        assert!(materialize_source("acme", "..", &sha, None, None, dir.path()).unwrap_err().contains("repo"));
        assert!(materialize_source("acme", "", &sha, None, None, dir.path()).unwrap_err().contains("repo"));
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
        assert!(contained_source_root(checkout.path(), Some("app"), true).is_ok());
        // a symlink pointing outside ⇒ containment fails.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), checkout.path().join("evil")).unwrap();
            let err = contained_source_root(checkout.path(), Some("evil"), true).unwrap_err();
            assert!(err.contains("escapes the checkout"), "{err}");
        }
    }

    #[test]
    fn recipe_manifest_relaxes_the_repo_capsule_toml_requirement() {
        // #932: a checkout with NO capsule.toml resolves when the claim carries a recipe
        // manifest (require_manifest = false) — and still fail-closes without one.
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join("src")).unwrap();
        let err = contained_source_root(checkout.path(), None, true).unwrap_err();
        assert!(err.contains("no capsule.toml"), "{err}");
        assert!(err.contains("recipe manifest"), "the error must say what was missing: {err}");
        assert!(contained_source_root(checkout.path(), None, false).is_ok());
        // Containment is still enforced on the recipe path (subdir escape still rejected).
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), checkout.path().join("evil")).unwrap();
            let err = contained_source_root(checkout.path(), Some("evil"), false).unwrap_err();
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
            declared_start_cmd: "python3 app.py".into(),
            port: 8080,
            healthcheck: "/health".into(),
            probe_synthesized: false,
            supervisor: None,
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
            declared_start_cmd: evil.into(),
            port: 8080,
            healthcheck: "/health".into(),
            probe_synthesized: false,
            supervisor: None,
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

    // ── v1.2 supervisor emission ──────────────────────────────────────────────

    fn supervisor_toml() -> String {
        // Canonical form: a lowercase secret key (used verbatim as the binding name) with
        // an explicit UPPERCASE `env`. The two alphabets differ by design.
        format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n",
            base_toml()
        )
    }

    #[test]
    fn env_secret_derives_a_supervisor_spec_and_no_binding_path_still_rejects_it() {
        let m = parse(&supervisor_toml());
        // The v1.0 no-binding path still rejects any secret (unchanged contract).
        assert!(
            derive_build_spec(&m, &probe_python()).unwrap_err().contains("secrets"),
            "no-binding derive must still reject a secret capsule"
        );
        // The v1.2 supervisor path accepts it and produces the supervisor config.
        let spec = derive_supervisor_build_spec(&m, &probe_python()).expect("supervisor spec");
        let sup = spec.supervisor.as_ref().expect("supervisor present");
        // Binding name = the (lowercase) secret key; env_map is ENV_VAR → binding name.
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        assert_eq!(sup.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
        // Runtime/port/command detection is identical to the no-binding path.
        assert_eq!(spec.start_cmd, "python3 app.py");
        assert_eq!(spec.port, 8080);
    }

    #[test]
    fn supervisor_derive_rejects_uppercase_secret_key_used_as_binding_name() {
        // REGRESSION (#954 follow-up): an uppercase secret key like `OPENAI_API_KEY` is a
        // valid POSIX env var but NOT a valid BindingName (lowercase-only), so the
        // guest-agent's own `SupervisorConfig::validate` rejects the emitted
        // supervisor.json and the agent `exit(2)`s BEFORE opening the vsock listener —
        // surfacing only as a silent "vsock never acked". Reject it at emission with an
        // actionable message instead of shipping a rootfs that cannot boot.
        let toml = format!(
            "{}\n[secrets.OPENAI_API_KEY]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
        assert!(err.contains("binding name"), "{err}");
        assert!(err.contains("[secrets.openai_api_key]"), "message must suggest the lowercase form: {err}");
        // The canonical lowercase-key + explicit-uppercase-env form is accepted.
        assert!(derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).is_ok());
    }

    #[test]
    fn supervisor_derive_accepts_only_env_delivery() {
        // Only delivery = "env" injects a supervisor env var. file / proxy / fd are
        // all rejected here (file is a later request-time read path; proxy/fd never
        // inject an env var).
        for d in ["file", "proxy", "fd"] {
            let toml = supervisor_toml().replace("env = \"OPENAI_API_KEY\"", &format!("delivery = \"{d}\""));
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("delivery"), "{d}: {err}");
        }
    }

    #[test]
    fn supervisor_derive_fails_closed_on_non_secret_bindings_and_no_secrets() {
        // A non-secret binding is still rejected (state/user_files come later).
        let with_binding = format!(
            "{}\n[bindings.data]\nkind = \"state\"\nmount = \"/data\"\n",
            supervisor_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&with_binding), &probe_python())
            .unwrap_err()
            .contains("no-binding"));
        // No secrets at all → not a supervisor build.
        assert!(derive_supervisor_build_spec(&parse(&base_toml()), &probe_python())
            .unwrap_err()
            .contains("requires at least one"));
    }

    #[test]
    fn supervisor_derive_rejects_malformed_and_duplicate_env_var_names() {
        // A malformed env var name must never reach the generated supervisor.json.
        // (lowercase secret key so it passes the binding-name gate and reaches the env check)
        let bad = format!(
            "{}\n[secrets.key]\nrequired = true\nenv = \"BAD-VAR\"\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&bad), &probe_python())
            .unwrap_err()
            .contains("POSIX identifier"));
        // Two secrets resolving to the SAME env var is ambiguous → fail-closed.
        let dup = format!(
            "{}\n[secrets.key_a]\nrequired = true\nenv = \"SHARED\"\n\
             [secrets.key_b]\nrequired = true\nenv = \"SHARED\"\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&dup), &probe_python())
            .unwrap_err()
            .contains("duplicate env injection"));
        // A secret with no `env` uses its NAME; a name that isn't a POSIX identifier
        // (dot allowed in a binding name but not an env var) is rejected too.
        let name_as_var = format!(
            "{}\n[secrets.\"api.key\"]\nrequired = true\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&name_as_var), &probe_python())
            .unwrap_err()
            .contains("POSIX identifier"));
    }

    #[test]
    fn supervisor_build_script_runs_agent_as_init_and_emits_config_without_secrets() {
        let spec = derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        // init runs the guest-agent (vsock supervisor) with the (lowercase) binding name
        // as argv, NOT the app directly — and NOT the uppercase env var name.
        assert!(script.contains("/usr/local/bin/ato-guest-agent 'openai_api_key'"), "{script}");
        assert!(!script.contains("ato-guest-agent 'OPENAI_API_KEY'"), "env var must not be the binding argv");
        assert!(script.contains("ATO_GUEST_AGENT_MODE=vsock"), "agent runs in vsock mode");
        assert!(!script.contains("python3 app.py' >/tmp/app.log"), "app is not launched directly");
        // supervisor.json is staged, requires the agent binary, and carries NO value —
        // only the env var → binding name map.
        assert!(script.contains("ATO_GUEST_AGENT_BIN"), "supervisor build needs the agent binary");
        assert!(script.contains("supervisor.json"), "config is staged");
        assert!(script.contains("\"OPENAI_API_KEY\": \"openai_api_key\""), "env→binding map present");
        assert!(script.contains("<<'DOCKER'") && script.contains("<<'INIT'"), "heredocs still quoted");
    }

    #[test]
    fn no_binding_script_is_unaffected_by_the_supervisor_addition() {
        // A no-binding spec still runs the app directly and stages no agent.
        let spec = derive_build_spec(&parse(&base_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(script.contains("/bin/sh -lc 'python3 app.py' >/tmp/app.log"), "direct app launch");
        assert!(!script.contains("ato-guest-agent"), "no agent in a no-binding rootfs");
        assert!(!script.contains("supervisor.json"), "no supervisor config");
    }
}
