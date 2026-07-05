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

use capsule::foundation::types::manifest::{
    CapsuleManifest, RuntimeType, ServiceSpec as ManifestServiceSpec,
};
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
    /// The binding names the guest-agent requires before it starts the workload(s)
    /// (the run gate delivers a lease per name). = the declared `[secrets.*]` names.
    pub binding_names: Vec<String>,
    /// `ENV_VAR → binding name` (from each secret's `env` or its name). For the
    /// LEGACY single-service build this is the sole workload's injection map; for a
    /// MULTI-service build (`services` is `Some`) each service carries its own.
    pub env_map: BTreeMap<String, String>,
    /// v1.5 (ato#973): when `Some`, this is a MULTI-service build — the emitted
    /// `supervisor.json` carries a `services[]` list (the guest supervisor starts
    /// the whole group under one bound-ready/revoke/rotation gate). `None` = the
    /// legacy single-service build, whose emitted `supervisor.json` stays
    /// byte-identical to before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceBuildSpec>>,
    /// v1.5 app_url selection (ato#973): the ONE service that owns the proxied
    /// public port — the app_url / ready_url target. Exactly the service with
    /// `network.publish = true` (0 or 2+ are rejected at derive time). Recorded so
    /// the receipt/diagnostic names which service the public URL points at. `None`
    /// for the legacy single-service build (its sole service is implicitly public).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_service: Option<String>,
}

/// v1.5 (ato#973): one service in a multi-service supervisor build. Non-secret —
/// safe in a receipt. `env_map` maps an env var to the binding NAME (never a value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceBuildSpec {
    /// Stable service name (unique; keys per-service logs + diagnostics).
    pub name: String,
    /// The workload argv (already wrapped `["/bin/sh", "-lc", <entrypoint>]`).
    pub cmd: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Non-secret env applied before bindings (e.g. `PORT`).
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR → binding name` for this service's secret injection.
    pub env_map: BTreeMap<String, String>,
    /// Whether this service is the PUBLIC one (exposed via the runner proxy).
    /// Exactly one service in a build is public; the rest are internal.
    pub public: bool,
    /// Declared start-ordering hints (recorded now; the readiness graph enforces
    /// ordering in a later slice — today every service starts together).
    pub depends_on: Vec<String>,
    /// Extra in-guest DNS aliases for this service (service aliasing; recorded for
    /// the aliasing slice). Validated DNS-safe at derive time.
    pub aliases: Vec<String>,
    /// The service's declared HTTP readiness path (`readiness_probe.http_get`),
    /// recorded for the readiness-graph slice. `None` = no HTTP probe declared.
    pub readiness_http_path: Option<String>,
    /// The service's PRIMARY listen port (public = the proxied target port;
    /// internal = its first resolved `expose` port or literal `env.PORT`). Used
    /// for the readiness probe. `None` = no determinable port (ready-once-started).
    pub port: Option<u16>,
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
    // v1.5 (ato#973): when the capsule declares `[services.<name>]`, derive a
    // MULTI-service supervisor build; otherwise keep the legacy single-service
    // shape (`services = None` → byte-identical emitted supervisor.json).
    let services = derive_supervisor_services(m, &env_map, spec.port)?;

    // v1.5 per-service secret scoping (ato#982): in a MULTI-service build a secret
    // reaches only the service(s) that named it. Fail-closed: every REQUIRED secret
    // must be scoped to at least one service (an unscoped required secret would be
    // waited-for by the gate but delivered to nobody — a config error). The lease
    // set (`binding_names`, the agent's argv) then shrinks to the secrets actually
    // used, so the guest never blocks on a secret no service consumes. The legacy
    // single-service build is untouched (its sole workload gets every secret).
    if let Some(svcs) = services.as_ref() {
        let scoped: std::collections::BTreeSet<&str> =
            svcs.iter().flat_map(|s| s.env_map.values()).map(|s| s.as_str()).collect();
        for (name, s) in &m.secrets {
            if s.required && !scoped.contains(name.as_str()) {
                return Err(format!(
                    "required secret '{name}' is not used by any service — scope it to a \
                     service with `secrets = [\"{name}\"]` (least privilege) or drop it"
                ));
            }
        }
        binding_names.retain(|n| scoped.contains(n.as_str()));
    }

    // app_url selection: the sole public service (exactly one, enforced in derive)
    // is the app_url / ready_url target. Recorded in the receipt; None for legacy.
    let public_service = services
        .as_ref()
        .and_then(|svcs| svcs.iter().find(|s| s.public).map(|s| s.name.clone()));
    spec.supervisor = Some(SupervisorBuildSpec { binding_names, env_map, services, public_service });
    Ok(spec)
}

/// Build the `/etc/ato/supervisor.json` value from the supervisor spec. A
/// MULTI-service build emits a `services[]` list (the guest group supervisor,
/// ato#974); a legacy single-service build emits the byte-identical top-level
/// shape. The PUBLIC service inherits the derived `PORT` so its listener matches
/// the single proxied guest port; internal services keep only their own env. No
/// secret value ever appears — only `ENV_VAR → binding name`.
fn build_supervisor_json(
    sup: &SupervisorBuildSpec,
    port: u16,
    start_cmd: &str,
) -> serde_json::Value {
    match &sup.services {
        Some(services) => {
            let svc_json: Vec<serde_json::Value> = services
                .iter()
                .map(|s| {
                    let mut obj = serde_json::json!({
                        "name": s.name,
                        "cmd": s.cmd,
                        "cwd": s.cwd,
                        "base_env": s.base_env,
                        "bindings_env": s.env_map,
                    });
                    if !s.depends_on.is_empty() {
                        obj["depends_on"] = serde_json::json!(s.depends_on);
                    }
                    // Readiness (so a dependent can WAIT): emit when the service has a
                    // determinable PRIMARY port (public = the proxied target port;
                    // internal = its first resolved expose port or literal env.PORT).
                    // The guest probes 127.0.0.1:<port> (plus the HTTP path when
                    // declared). No port ⇒ "ready once started" (no readiness block).
                    if let Some(rport) = s.port {
                        let mut r = serde_json::json!({ "port": rport });
                        if let Some(path) = &s.readiness_http_path {
                            r["http_path"] = serde_json::json!(path);
                        }
                        obj["readiness"] = r;
                    }
                    obj
                })
                .collect();
            serde_json::json!({ "services": svc_json })
        }
        None => serde_json::json!({
            "cmd": ["/bin/sh", "-lc", start_cmd],
            "cwd": "/app",
            "base_env": { "PORT": port.to_string() },
            "bindings_env": sup.env_map,
        }),
    }
}

/// A service name: 1–63 chars of lowercase `[a-z0-9-]`, not leading/trailing `-`
/// (it keys per-service logs and may become an in-guest DNS label).
fn valid_service_name(name: &str) -> bool {
    let ok_len = (1..=63).contains(&name.len());
    let ok_chars = name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    ok_len && ok_chars && ends(name.bytes().next()) && ends(name.bytes().next_back())
}

/// v1.5 (ato#973): derive the multi-service build from `[services.<name>]`, or
/// `Ok(None)` when the capsule declares no services (legacy single-service path).
///
/// Adopts the PROCESS-relevant subset of the existing manifest service schema
/// (entrypoint / env / depends_on / readiness_probe / network.publish /
/// network.aliases) and FAIL-CLOSES on everything that only makes sense for the
/// OCI/container orchestration path — a single Firecracker VM cannot honour a
/// cross-container volume mount, an egress-proxy opt-out, or a service-to-service
/// ACL, so declaring them here is a config error, never silently ignored.
///
/// `common_secret_env_map` is the global `[secrets.*]` env injection (ENV_VAR →
/// secret name). v1.5 per-service secret scoping (ato#982): each service receives
/// ONLY the secrets it names in `secrets = [...]` — this map is filtered per
/// service, not cloned wholesale. The caller then shrinks the required binding
/// names (agent argv) to the secrets actually scoped, and rejects a required
/// secret that no service uses.
fn derive_supervisor_services(
    m: &CapsuleManifest,
    common_secret_env_map: &BTreeMap<String, String>,
    target_port: u16,
) -> Result<Option<Vec<ServiceBuildSpec>>, String> {
    let Some(services) = m.services.as_ref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    // NOTE on the target: a multi-service capsule still carries a `[targets.<label>]`
    // — it is the build/runtime ANCHOR (runtime detection, base image, the single
    // proxied guest PORT). Its `run` seeds the derivation; the RUNTIME processes come
    // from `[services]` (emitted as the guest supervisor's `services[]`). They are
    // complementary, not competing. (Schema 0.3 has no `[execution]` section — a
    // legacy top-level entrypoint cannot even parse here.)

    // v1.5 expose port resolution: an intermediate captured per service, then the
    // ports its `expose` placeholders name are ALLOCATED deterministically and
    // injected (own placeholders unprefixed; every service's ports cross-referenced
    // as `<SERVICE>_<PLACEHOLDER>` so a dependent can reach it on loopback).
    struct Resolved {
        name: String,
        cmd: Vec<String>,
        author_env: BTreeMap<String, String>,
        env_map: BTreeMap<String, String>,
        public: bool,
        depends_on: Vec<String>,
        aliases: Vec<String>,
        readiness_http_path: Option<String>,
        expose: Vec<String>,
        literal_port: Option<u16>,
    }
    let mut collected: Vec<Resolved> = Vec::with_capacity(services.len());
    let mut public_count = 0usize;
    // Deterministic order (BTree by name) so the emitted supervisor.json + receipt
    // are reproducible regardless of the source map's iteration order.
    let ordered: BTreeMap<&String, &ManifestServiceSpec> = services.iter().collect();
    let names: std::collections::BTreeSet<&str> = ordered.keys().map(|k| k.as_str()).collect();

    for (name, svc) in &ordered {
        if !valid_service_name(name) {
            return Err(format!(
                "service '{name}': name must be 1–63 chars of lowercase [a-z0-9-], \
                 not leading/trailing '-'"
            ));
        }
        if svc.entrypoint.trim().is_empty() {
            return Err(format!("service '{name}': `entrypoint` is empty"));
        }
        // Reject container-only fields (single-VM snapshot cannot honour them).
        if !svc.state_bindings.is_empty() {
            return Err(format!(
                "service '{name}': `state_bindings` is a container/volume feature not \
                 supported in a single-VM snapshot (v1.6 persistent state is out of scope here)"
            ));
        }
        if let Some(net) = &svc.network {
            if !net.allow_from.is_empty() {
                return Err(format!(
                    "service '{name}': `network.allow_from` (service-to-service ACL) is not \
                     supported in a single-VM snapshot"
                ));
            }
            if !net.egress_proxy {
                return Err(format!(
                    "service '{name}': `network.egress_proxy = false` is a container opt-out \
                     not supported in a single-VM snapshot"
                ));
            }
        }
        // depends_on references must exist (validate the graph now; ordering is
        // enforced by the later readiness-graph slice).
        let depends_on = svc.depends_on.clone().unwrap_or_default();
        for dep in &depends_on {
            if !names.contains(dep.as_str()) {
                return Err(format!("service '{name}': depends_on '{dep}' is not a declared service"));
            }
            if dep == *name {
                return Err(format!("service '{name}': depends_on itself"));
            }
        }
        let public = svc.network.as_ref().is_some_and(|n| n.publish);
        if public {
            public_count += 1;
        }
        // Non-secret env → base_env (validated as POSIX identifiers, like secrets).
        let mut base_env: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in svc.env.clone().unwrap_or_default() {
            if !valid_env_var_name(&k) {
                return Err(format!("service '{name}': env var {k:?} is not a POSIX identifier"));
            }
            base_env.insert(k, v);
        }
        // The runner proxies exactly ONE guest port — the build target's `port`. The
        // PUBLIC service MUST listen there, so a service-declared `PORT` that differs
        // is a config error (build would succeed, restore would succeed, but the
        // proxy would front a port nothing is on → false/never ready). Fail closed
        // rather than silently honour the target port and ignore the author's `PORT`.
        // (Injected below in build_supervisor_json when absent.)
        if public {
            if let Some(declared) = base_env.get("PORT") {
                if declared != &target_port.to_string() {
                    return Err(format!(
                        "public service '{name}': env PORT = {declared:?} but the build target \
                         port is {target_port} — the public service must listen on the single \
                         proxied port. Drop the explicit PORT (it is injected) or set it to {target_port}"
                    ));
                }
            }
        }
        // DNS-safe aliases (they may become in-guest DNS labels in the aliasing slice).
        let aliases = svc.network.as_ref().map(|n| n.aliases.clone()).unwrap_or_default();
        for alias in &aliases {
            if !valid_service_name(alias) {
                return Err(format!(
                    "service '{name}': alias '{alias}' must be a DNS-safe label \
                     (1–63 chars of lowercase [a-z0-9-], not leading/trailing '-')"
                ));
            }
        }
        let readiness_http_path = svc
            .readiness_probe
            .as_ref()
            .and_then(|p| p.http_get.clone())
            .filter(|s| !s.trim().is_empty());
        // `expose` placeholders → env var names the builder resolves to concrete
        // ports. Each must be a POSIX identifier (it becomes an env var).
        let expose = svc.expose.clone().unwrap_or_default();
        for ph in &expose {
            if !valid_env_var_name(ph) {
                return Err(format!(
                    "service '{name}': expose placeholder {ph:?} is not a POSIX identifier"
                ));
            }
        }
        // A literal `env.PORT` (the author hardcoded a port) is reserved so
        // allocation never collides with it.
        let literal_port = base_env.get("PORT").and_then(|p| p.parse::<u16>().ok());

        // v1.5 per-service secret scoping (ato#982): a secret reaches ONLY the
        // service(s) that name it in `secrets = [...]`. Build this service's
        // injection map as the subset of the global env_map whose secret name is
        // declared here. Fail-closed: a referenced secret must actually exist.
        // `common_secret_env_map` is ENV_VAR → secret name.
        let wanted: std::collections::BTreeSet<&str> =
            svc.secrets.iter().flatten().map(|s| s.as_str()).collect();
        for secret in &wanted {
            if !common_secret_env_map.values().any(|n| n == secret) {
                return Err(format!(
                    "service '{name}': secrets entry '{secret}' is not a declared \
                     [secrets.*] — declare it or remove the reference"
                ));
            }
        }
        let env_map: BTreeMap<String, String> = common_secret_env_map
            .iter()
            .filter(|(_, secret)| wanted.contains(secret.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        collected.push(Resolved {
            name: (*name).clone(),
            cmd: vec!["/bin/sh".into(), "-lc".into(), svc.entrypoint.clone()],
            author_env: base_env,
            env_map,
            public,
            depends_on,
            aliases,
            readiness_http_path,
            expose,
            literal_port,
        });
    }

    // Exactly one PUBLIC service — the run gate proxies exactly one guest port.
    match public_count {
        1 => {}
        0 => {
            return Err(
                "no public service: exactly one service must set `network.publish = true` \
                 (the one exposed via the runner proxy)"
                    .into(),
            );
        }
        n => {
            return Err(format!(
                "{n} services set `network.publish = true`; exactly one may be public in a \
                 single-VM snapshot (the rest are internal)"
            ));
        }
    }

    // Fail-closed: no DUPLICATE expose placeholder within a service — the second
    // would silently overwrite the first's allocated port.
    for r in &collected {
        let mut seen = std::collections::BTreeSet::new();
        for ph in &r.expose {
            if !seen.insert(ph.as_str()) {
                return Err(format!("service '{}': duplicate expose placeholder {ph:?}", r.name));
            }
        }
    }
    // Fail-closed: the GENERATED cross-reference env var names
    // (`<SERVICE>_<PLACEHOLDER>`) must be unique across all pairs — a service name
    // with a `-` and a placeholder with a `_` can otherwise alias (`a-b`+`C` and
    // `a`+`B_C` both → `A_B_C`), silently losing one port reference.
    {
        let mut xref: BTreeMap<String, (String, String)> = BTreeMap::new();
        for r in &collected {
            for ph in &r.expose {
                let var = format!("{}_{}", env_prefix(&r.name), ph);
                if let Some(prev) = xref.insert(var.clone(), (r.name.clone(), ph.clone())) {
                    return Err(format!(
                        "expose placeholders collide: {}.{} and {}.{} both generate the \
                         cross-reference env var {var:?} — rename one placeholder to disambiguate",
                        prev.0, prev.1, r.name, ph
                    ));
                }
            }
        }
    }

    // ── app_url selection (ato#973): the proxied target port has EXACTLY ONE owner
    // — the public service. An author-declared literal `env.PORT` must not let an
    // internal service claim it (bind collision / URL-vs-owner drift), and two
    // services must not declare the same concrete literal port. `expose`-allocated
    // ports are already collision-free (see `reserved`); this closes the remaining
    // author-declared-literal ambiguity so `public_service` is auditable as the
    // sole target-port owner. ──
    let public_name = collected
        .iter()
        .find(|r| r.public)
        .map(|r| r.name.clone())
        .expect("exactly one public service (checked above)");
    let mut port_owner: BTreeMap<u16, String> = BTreeMap::new();
    port_owner.insert(target_port, public_name.clone()); // the public service owns it
    for r in &collected {
        let Some(p) = r.literal_port else { continue };
        if r.public {
            // The public service's literal PORT was already validated == target_port
            // and it is the registered owner — nothing more to check.
            continue;
        }
        if p == target_port {
            return Err(format!(
                "internal service '{}': env PORT = {p} is the proxied public port owned by \
                 service '{public_name}' — only the public service may listen on the target \
                 port. Give '{}' a different port (or expose a placeholder)",
                r.name, r.name
            ));
        }
        if let Some(prev) = port_owner.insert(p, r.name.clone()) {
            return Err(format!(
                "services '{prev}' and '{}' both declare env PORT = {p} — each concrete \
                 listen port must have a single owner in a single-VM snapshot",
                r.name
            ));
        }
    }

    // ── Allocate the ports each service's `expose` placeholders name ──
    // Reserved: the proxied target port + every literal `env.PORT`. The PUBLIC
    // service's FIRST expose placeholder resolves to the target port; every other
    // placeholder gets the next free port from a FIXED base. The base is a
    // constant (not ambient env) so the sealed rootfs is reproducible from the
    // manifest alone — the same manifest never allocates differently on two hosts.
    const SERVICE_PORT_BASE: u16 = 8091;
    let mut reserved: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();
    reserved.insert(target_port);
    for r in &collected {
        if let Some(p) = r.literal_port {
            reserved.insert(p);
        }
    }
    let mut next = SERVICE_PORT_BASE;
    let mut alloc = || -> Result<u16, String> {
        loop {
            let p = next;
            next = next.checked_add(1).ok_or("ran out of ports allocating service expose placeholders")?;
            if !reserved.contains(&p) {
                reserved.insert(p);
                return Ok(p);
            }
        }
    };
    // `(service, placeholder) → port` and each service's primary port.
    let mut resolved_ports: BTreeMap<(String, String), u16> = BTreeMap::new();
    let mut primary: BTreeMap<String, u16> = BTreeMap::new();
    for r in &collected {
        for (i, ph) in r.expose.iter().enumerate() {
            let port = if r.public && i == 0 { target_port } else { alloc()? };
            resolved_ports.insert((r.name.clone(), ph.clone()), port);
            if i == 0 {
                primary.entry(r.name.clone()).or_insert(port);
            }
        }
        // Primary fallback: a public service always listens on the target port; an
        // internal service with no expose but a literal env.PORT uses that.
        if r.public {
            primary.insert(r.name.clone(), target_port);
        } else if !primary.contains_key(&r.name) {
            if let Some(p) = r.literal_port {
                primary.insert(r.name.clone(), p);
            }
        }
    }

    // ── Build final services: inject own placeholders (unprefixed) + every
    // service's resolved ports as `<SERVICE>_<PLACEHOLDER>` for cross-service
    // reachability on loopback. EVERY injected var is checked against what is
    // already present (author env or a prior injection) — a collision is a config
    // error, never a silent override (Ato fail-closed). ──
    fn insert_checked(
        env: &mut BTreeMap<String, String>,
        service: &str,
        key: String,
        val: String,
    ) -> Result<(), String> {
        if env.contains_key(&key) {
            return Err(format!(
                "service '{service}': injected port env {key:?} collides with an existing \
                 env var — rename the expose placeholder or drop the conflicting env"
            ));
        }
        env.insert(key, val);
        Ok(())
    }
    let mut out: Vec<ServiceBuildSpec> = Vec::with_capacity(collected.len());
    for r in &collected {
        let mut base_env = r.author_env.clone();
        // The public service always listens on the proxied target port. A public
        // service that declared `env.PORT` was already validated == target_port, so
        // set it idempotently BEFORE the checked injections (an expose placeholder
        // literally named "PORT" would then correctly collide).
        if r.public {
            base_env.entry("PORT".to_string()).or_insert_with(|| target_port.to_string());
        }
        // Own placeholders, unprefixed, so THIS service binds there.
        for ph in &r.expose {
            let p = resolved_ports[&(r.name.clone(), ph.clone())];
            // A public first placeholder resolves to the target port; if it is
            // literally "PORT" the idempotent public-PORT set above already holds
            // the same value, so allow that exact match rather than false-collide.
            if r.public && ph == "PORT" && base_env.get("PORT") == Some(&p.to_string()) {
                continue;
            }
            insert_checked(&mut base_env, &r.name, ph.clone(), p.to_string())?;
        }
        // Cross-references: every service's ports, as <SERVICE>_<PLACEHOLDER>.
        for ((svc, ph), port) in &resolved_ports {
            let var = format!("{}_{}", env_prefix(svc), ph);
            insert_checked(&mut base_env, &r.name, var, port.to_string())?;
        }
        out.push(ServiceBuildSpec {
            name: r.name.clone(),
            cmd: r.cmd.clone(),
            cwd: "/app".into(),
            base_env,
            env_map: r.env_map.clone(),
            public: r.public,
            depends_on: r.depends_on.clone(),
            aliases: r.aliases.clone(),
            readiness_http_path: r.readiness_http_path.clone(),
            port: primary.get(&r.name).copied(),
        });
    }

    // v1.5 service aliasing (ato#973): every service NAME and its aliases become an
    // in-guest hostname resolving to loopback (see build_etc_hosts), so a service
    // reaches another by NAME (`redis:$REDIS_REDIS_PORT`). Every hostname must be
    // UNIQUE across the whole set — a name/alias claimed by two services (or twice)
    // would be an ambiguous DNS entry. Fail-closed here (the alias's own DNS-safe
    // shape was already checked per service).
    let mut host_owner: BTreeMap<String, String> = BTreeMap::new();
    for s in &out {
        for host in std::iter::once(&s.name).chain(s.aliases.iter()) {
            // Reserved loopback names are baked unconditionally by build_etc_hosts —
            // a service name or alias that shadows one is an ambiguous DNS entry.
            if RESERVED_HOSTNAMES.contains(&host.as_str()) {
                return Err(format!(
                    "service '{}': hostname '{host}' is reserved for the loopback entry \
                     and cannot be a service name or alias",
                    s.name
                ));
            }
            if let Some(prev) = host_owner.insert(host.clone(), s.name.clone()) {
                return Err(format!(
                    "hostname '{host}' is claimed by both service '{prev}' and '{}' — a service \
                     name and every alias must be unique across the capsule",
                    s.name
                ));
            }
        }
    }

    Ok(Some(out))
}

/// Hostnames `build_etc_hosts` bakes unconditionally for the loopback entries — a
/// service name or alias may not shadow one (ambiguous DNS). `localhost.localdomain`
/// is reserved defensively even though it is not emitted.
const RESERVED_HOSTNAMES: &[&str] =
    &["localhost", "localhost.localdomain", "ip6-localhost", "ip6-loopback"];

/// v1.5 (ato#973): the `/etc/hosts` a multi-service guest is built with, so a
/// service reaches another by NAME on loopback. Every service name and alias maps
/// to `127.0.0.1` (single VM ⇒ everything is loopback). Uniqueness (and that no
/// name shadows a reserved loopback host) is enforced by the caller
/// (`derive_supervisor_services`). Deterministic (BTree-ordered names).
fn build_etc_hosts(services: &[ServiceBuildSpec]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for s in services {
        names.push(&s.name);
        for a in &s.aliases {
            names.push(a);
        }
    }
    names.sort_unstable();
    let joined = names.join(" ");
    format!(
        "127.0.0.1 localhost {joined}\n::1 localhost ip6-localhost ip6-loopback\n"
    )
}

/// Uppercase a service name into an env-var prefix: `-` → `_`, then uppercase
/// (`my-api` → `MY_API`). Service names are DNS-safe labels, so the result is a
/// valid POSIX identifier prefix.
fn env_prefix(service: &str) -> String {
    service.replace('-', "_").to_ascii_uppercase()
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
            let cfg = build_supervisor_json(sup, spec.port, &spec.start_cmd);
            let cfg_json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
            // Binding names become the agent's argv — shell-quote each defensively.
            let args = sup
                .binding_names
                .iter()
                .map(|n| shell_single_quote(n))
                .collect::<Vec<_>>()
                .join(" ");
            // v1.5 service aliasing: a MULTI-service build bakes /etc/hosts so each
            // service name + alias resolves to loopback (single-service builds stay
            // byte-identical — no hosts write).
            let hosts_prep = match &sup.services {
                Some(services) => {
                    let hosts = build_etc_hosts(services);
                    format!(
                        "\n# v1.5 service aliasing: names + aliases → loopback.\n\
                         cat > \"$BUILD/rootfs/etc/hosts\" <<'ATOETCHOSTS'\n{hosts}ATOETCHOSTS"
                    )
                }
                None => String::new(),
            };
            let prep = format!(
                r#"# v1.2 supervisor: stage the guest-agent + its config into the rootfs.
: "${{ATO_GUEST_AGENT_BIN:?ATO_GUEST_AGENT_BIN must point to the guest-agent binary for a supervisor build}}"
mkdir -p "$BUILD/rootfs/usr/local/bin" "$BUILD/rootfs/etc/ato" "$BUILD/rootfs/run/ato/bindings"
cp "$ATO_GUEST_AGENT_BIN" "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
chmod 0755 "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
cat > "$BUILD/rootfs/etc/ato/supervisor.json" <<'ATOSUPERVISORJSON'
{cfg_json}
ATOSUPERVISORJSON{hosts_prep}"#
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

    // ── v1.5 per-service secret scoping (ato#982) ──

    #[test]
    fn a_secret_reaches_only_the_services_that_scope_it() {
        // Two secrets; api scopes the openai key, redis scopes the redis password.
        // Neither service sees the other's secret (least privilege).
        let toml = format!(
            "{}\n\
             [secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [secrets.redis_password]\nrequired = true\nenv = \"REDIS_PASSWORD\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\nsecrets = [\"redis_password\"]\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        let services = sup.services.as_ref().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        let redis = services.iter().find(|s| s.name == "redis").unwrap();
        // api gets ONLY the openai key; redis gets ONLY the redis password.
        assert_eq!(api.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
        assert!(!api.env_map.contains_key("REDIS_PASSWORD"), "api must NOT get redis's secret");
        assert_eq!(redis.env_map.get("REDIS_PASSWORD").map(String::as_str), Some("redis_password"));
        assert!(!redis.env_map.contains_key("OPENAI_API_KEY"), "redis must NOT get api's secret");
        // Both required secrets are scoped, so both are still waited-for by the gate.
        assert_eq!(sup.binding_names, vec!["openai_api_key", "redis_password"]);
    }

    #[test]
    fn an_unscoped_secret_is_not_delivered_and_shrinks_the_lease_set() {
        // redis needs no secret; the (optional) unused secret is delivered to nobody
        // and drops out of binding_names (the guest never blocks on it).
        let toml = format!(
            "{}\n\
             [secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [secrets.unused_key]\nrequired = false\nenv = \"UNUSED\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        // The optional unused secret is not waited-for; redis carries no secret.
        assert_eq!(sup.binding_names, vec!["openai_api_key"], "unused optional secret dropped from lease set");
        let redis = sup.services.as_ref().unwrap().iter().find(|s| s.name == "redis").unwrap();
        assert!(redis.env_map.is_empty(), "redis scopes no secret → empty injection map");
    }

    // ── v1.5 multi-service COMPOSITION fixture (ato#984) ──
    // One realistic capsule exercising every v1.5 piece together, so a regression
    // that only shows up when the pieces COMPOSE (not in a per-slice unit) fails
    // here. A public `web` + an internal `redis` + an internal `worker` that
    // depends on redis; expose ports, an alias, a readiness probe, and per-service
    // secret scoping all in one manifest. (CI-runnable — no KVM/Docker; the live
    // seal→restore of a fixture like this runs via the runner smoke on a real host.)

    fn compose_fixture_toml() -> String {
        format!(
            "{}\n\
             [secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [secrets.redis_password]\nrequired = true\nenv = \"REDIS_PASSWORD\"\n\
             [services.web]\nentrypoint = \"node web.js\"\ndepends_on = [\"redis\", \"worker\"]\n\
             secrets = [\"openai_api_key\"]\n\
             [services.web.network]\npublish = true\n\
             [services.web.readiness_probe]\nhttp_get = \"/healthz\"\n\
             [services.redis]\nentrypoint = \"redis-server\"\nexpose = [\"REDIS_PORT\"]\n\
             secrets = [\"redis_password\"]\n\
             [services.redis.network]\naliases = [\"cache\"]\n\
             [services.worker]\nentrypoint = \"node worker.js\"\ndepends_on = [\"redis\"]\n",
            base_toml() // target port 8080
        )
    }

    #[test]
    fn multi_service_fixture_composes_every_v15_piece() {
        let spec = derive_supervisor_build_spec(&parse(&compose_fixture_toml()), &probe_python())
            .expect("compose fixture derives");
        let sup = spec.supervisor.as_ref().unwrap();
        let services = sup.services.as_ref().unwrap();
        let by = |n: &str| services.iter().find(|s| s.name == n).unwrap();
        let (web, redis, worker) = (by("web"), by("redis"), by("worker"));

        // app_url selection: web is the sole public service + the target-port owner.
        assert_eq!(sup.public_service.as_deref(), Some("web"));
        assert_eq!(web.port, Some(spec.port));
        assert_eq!(services.iter().filter(|s| s.port == Some(spec.port)).count(), 1);

        // expose resolution + cross-injection: redis's port is allocated (≠ target),
        // web can reach it by REDIS_REDIS_PORT, and it matches redis's own REDIS_PORT.
        let rport = redis.base_env.get("REDIS_PORT").expect("redis own port");
        assert_ne!(rport, "8080");
        assert_eq!(web.base_env.get("REDIS_REDIS_PORT"), Some(rport));
        assert_eq!(worker.base_env.get("REDIS_REDIS_PORT"), Some(rport), "worker reaches redis too");

        // service aliasing: /etc/hosts maps every name + alias to loopback.
        let hosts = build_etc_hosts(services);
        for h in ["web", "redis", "cache", "worker"] {
            assert!(hosts.contains(h), "hosts missing {h}");
        }

        // per-service secret scoping: least privilege, no cross-delivery.
        assert_eq!(web.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
        assert!(!web.env_map.contains_key("REDIS_PASSWORD"), "web must not get redis's secret");
        assert_eq!(redis.env_map.get("REDIS_PASSWORD").map(String::as_str), Some("redis_password"));
        assert!(!redis.env_map.contains_key("OPENAI_API_KEY"));
        assert!(worker.env_map.is_empty(), "worker scopes no secret");
        // Both required secrets are scoped → both waited-for by the gate.
        assert_eq!(sup.binding_names, vec!["openai_api_key", "redis_password"]);

        // readiness graph + emission: the emitted supervisor.json carries depends_on
        // and readiness, web depends on redis + worker, and the group is startable.
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let arr = json["services"].as_array().unwrap();
        let webj = arr.iter().find(|s| s["name"] == "web").unwrap();
        let redisj = arr.iter().find(|s| s["name"] == "redis").unwrap();
        let workerj = arr.iter().find(|s| s["name"] == "worker").unwrap();
        // web waits on BOTH redis and worker; worker waits on redis — the real
        // readiness graph, not just a single edge.
        assert_eq!(webj["depends_on"], serde_json::json!(["redis", "worker"]));
        assert_eq!(webj["readiness"]["port"], spec.port);
        assert_eq!(webj["readiness"]["http_path"], "/healthz");
        assert_eq!(workerj["depends_on"], serde_json::json!(["redis"]));
        // redis is depended-on, so a dependent WAITS for it — its readiness probes
        // its own resolved REDIS_PORT (TCP-accept: no http_path), the very port
        // web/worker were cross-injected with. This is what makes the wait reach it.
        let redis_port: u16 = redis.base_env.get("REDIS_PORT").unwrap().parse().unwrap();
        assert_eq!(redisj["readiness"]["port"], redis_port);
        assert!(redisj["readiness"].get("http_path").is_none(), "redis readiness is TCP-accept");
        // No secret VALUE anywhere in the emitted config (names only).
        assert!(!json.to_string().contains("sk-") && !json.to_string().to_lowercase().contains("password="));

        // The whole build script assembles (multi-service supervisor.json + /etc/hosts).
        let script = build_rootfs_script(&spec, 1024);
        assert!(script.contains("\"services\"") && script.contains("rootfs/etc/hosts"));
    }

    #[test]
    fn a_required_secret_scoped_to_no_service_is_rejected() {
        // openai_api_key is required but no service names it → fail-closed.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
        assert!(err.contains("required secret 'openai_api_key'") && err.contains("not used by any service"), "{err}");
    }

    #[test]
    fn a_service_scoping_an_undeclared_secret_is_rejected() {
        // api references a secret that does not exist in [secrets.*] → fail-closed.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\", \"ghost_key\"]\n[services.api.network]\npublish = true\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
        assert!(err.contains("'ghost_key'") && err.contains("not a declared"), "{err}");
    }

    #[test]
    fn legacy_single_service_still_receives_every_secret() {
        // The legacy single-service build (no [services]) is unchanged: the sole
        // workload gets every declared secret, and binding_names is the full set.
        let spec = derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(sup.services.is_none(), "legacy single-service build");
        assert_eq!(sup.binding_names, vec!["openai_api_key"], "legacy keeps every declared secret");
        assert_eq!(sup.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
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

    // ── v1.5 (ato#973): multi-service supervisor build ──

    /// base target + secret + a two-service graph: a PUBLIC api that depends on an
    /// INTERNAL redis.
    fn multi_service_toml() -> String {
        format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"python3 api.py\"\ndepends_on = [\"redis\"]\n\
             secrets = [\"openai_api_key\"]\n\
             [services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"redis-server\"\n",
            base_toml()
        )
    }

    #[test]
    fn multi_service_derives_a_service_group_with_one_public_service() {
        let spec = derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python())
            .expect("multi-service supervisor spec");
        let sup = spec.supervisor.as_ref().unwrap();
        // Required bindings (agent argv) are still the global secret set.
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        let services = sup.services.as_ref().expect("services list present");
        assert_eq!(services.len(), 2);
        // Deterministic order (BTree by name): api before redis.
        let api = &services[0];
        let redis = &services[1];
        assert_eq!(api.name, "api");
        assert!(api.public, "api declared network.publish = true");
        assert_eq!(api.cmd, vec!["/bin/sh", "-lc", "python3 api.py"]);
        assert_eq!(api.depends_on, vec!["redis"]);
        assert_eq!(api.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
        assert_eq!(redis.name, "redis");
        assert!(!redis.public, "redis is internal (no publish)");

        // Emitted supervisor.json: services[] shape, PUBLIC service gets PORT,
        // internal one does not, and NO secret value appears.
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let arr = json["services"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "api");
        assert_eq!(arr[0]["base_env"]["PORT"], spec.port.to_string());
        assert!(arr[1]["base_env"].get("PORT").is_none(), "internal service has no PORT injected");
        assert_eq!(arr[0]["bindings_env"]["OPENAI_API_KEY"], "openai_api_key");
        assert!(!json.to_string().contains("sk-"), "no secret value in the emitted config");
    }

    // ── v1.5 (ato#973): app_url selection ──

    #[test]
    fn app_url_selection_records_the_public_service_and_targets_its_port() {
        // api(public) + redis(internal) → the app_url target is api, on the proxied
        // target port; redis is never the URL target and its port is not exposed.
        let spec = derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        // The receipt records WHICH service the public URL points at.
        assert_eq!(sup.public_service.as_deref(), Some("api"), "public service recorded for app_url");
        // That service owns the proxied target port (= the ready_url/app_url port).
        let services = sup.services.as_ref().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert!(api.public);
        assert_eq!(api.port, Some(spec.port), "public service listens on the proxied port");
        // The internal service is never selected, and it does not own the target port.
        let redis = services.iter().find(|s| s.name == "redis").unwrap();
        assert!(!redis.public);
        assert_ne!(sup.public_service.as_deref(), Some("redis"));
        assert_ne!(redis.port, Some(spec.port), "internal service is not on the public port");
        // The recorded name is exactly the one public service.
        assert_eq!(services.iter().filter(|s| s.public).count(), 1);
    }

    #[test]
    fn app_url_selection_ignores_internal_expose_ports_and_aliases() {
        // An internal service EXPOSES a port and declares an alias; neither becomes
        // the app_url target — only the public service's proxied port is the URL.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n\
             [services.redis.network]\naliases = [\"cache\"]\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert_eq!(sup.public_service.as_deref(), Some("api"));
        let redis = sup.services.as_ref().unwrap().iter().find(|s| s.name == "redis").unwrap();
        // redis's exposed port is a real allocated loopback port, but it is NOT the
        // public/proxied port, and redis is not the URL target.
        let rport = redis.base_env.get("REDIS_PORT").unwrap().parse::<u16>().unwrap();
        assert_ne!(rport, spec.port, "internal expose port is not the public port");
        assert!(!redis.aliases.is_empty(), "redis has an alias (cache) — internal, not a URL target");
        assert!(!redis.public);
    }

    #[test]
    fn only_the_public_service_may_own_the_target_port() {
        // (1) An internal service declaring env PORT == the target port is rejected —
        //     the proxied port has a single owner (the public service).
        let internal_on_target = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.env]\nPORT = \"8080\"\n",
            base_toml() // target port 8080
        );
        let err = derive_supervisor_build_spec(&parse(&internal_on_target), &probe_python()).unwrap_err();
        assert!(err.contains("proxied public port") && err.contains("redis"), "{err}");

        // (2) Two internal services declaring the SAME concrete literal port → rejected.
        let dup_literal = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.env]\nPORT = \"9001\"\n\
             [services.worker]\nentrypoint = \"w\"\n[services.worker.env]\nPORT = \"9001\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&dup_literal), &probe_python()).unwrap_err();
        assert!(err.contains("single owner") && err.contains("9001"), "{err}");

        // (3) The public service remains the ONLY service whose primary port == target;
        //     an internal service on a DIFFERENT literal port is fine.
        let ok = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.env]\nPORT = \"6379\"\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&ok), &probe_python()).unwrap();
        let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
        assert_eq!(services.iter().filter(|s| s.port == Some(spec.port)).count(), 1, "one target-port owner");
        let owner = services.iter().find(|s| s.port == Some(spec.port)).unwrap();
        assert!(owner.public && owner.name == "api", "the target-port owner is the public service");
    }

    #[test]
    fn legacy_single_service_has_no_recorded_public_service() {
        // A legacy single-service build: no [services] → services None, and
        // public_service is None (the sole workload is implicitly the URL target).
        let spec = derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(sup.services.is_none(), "legacy single-service build");
        assert!(sup.public_service.is_none(), "no explicit public service selection for legacy");
    }

    #[test]
    fn multi_service_bakes_etc_hosts_with_names_and_aliases_and_rejects_duplicates() {
        // api (public) + redis (internal) with an alias "cache". The build bakes an
        // /etc/hosts mapping every name + alias to loopback, so api reaches redis by
        // name (`redis:$REDIS_REDIS_PORT` or `cache:$...`).
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\ndepends_on = [\"redis\"]\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"redis-server\"\nexpose = [\"REDIS_PORT\"]\n\
             [services.redis.network]\naliases = [\"cache\"]\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
        let hosts = build_etc_hosts(services);
        assert!(hosts.contains("127.0.0.1 localhost"));
        for h in ["api", "redis", "cache"] {
            assert!(hosts.contains(h), "hosts missing {h}: {hosts}");
        }
        // The rootfs script bakes /etc/hosts for the multi-service build.
        let script = build_rootfs_script(&spec, 512);
        assert!(script.contains("rootfs/etc/hosts"), "multi-service bakes /etc/hosts");
        assert!(script.contains("cache"), "alias present in the baked hosts file");

        // A duplicate hostname (alias equals another service's name) is fail-closed.
        let dup = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.network]\naliases = [\"api\"]\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&dup), &probe_python())
            .unwrap_err()
            .contains("claimed by both"));
    }

    #[test]
    fn etc_hosts_content_is_exact_and_deterministic() {
        // Two internal services + a public one, aliases included. The baked file must
        // be EXACTLY this (loopback line = sorted names+aliases, then the ::1 line).
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.network]\naliases = [\"cache\"]\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
        assert_eq!(
            build_etc_hosts(services),
            "127.0.0.1 localhost api cache redis\n::1 localhost ip6-localhost ip6-loopback\n"
        );
    }

    #[test]
    fn reserved_loopback_hostnames_are_rejected_as_service_names_or_aliases() {
        let reject = |body: &str| {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{body}",
                base_toml()
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("reserved for the loopback"), "{err}");
        };
        // alias = "localhost"
        reject(
            "[services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\naliases = [\"localhost\"]\n",
        );
        // service NAME = "localhost"
        reject(
            "[services.localhost]\nentrypoint = \"a\"\n[services.localhost.network]\npublish = true\n",
        );
        // alias = "ip6-localhost"
        reject(
            "[services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\naliases = [\"ip6-localhost\"]\n",
        );
        // A normal alias still works.
        let ok = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\naliases = [\"web\"]\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&ok), &probe_python()).is_ok());
    }

    #[test]
    fn single_service_supervisor_build_does_not_bake_etc_hosts() {
        // A legacy single-service supervisor build stays byte-identical: no /etc/hosts.
        let spec = derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(!script.contains("rootfs/etc/hosts"), "single-service must not bake /etc/hosts");
    }

    #[test]
    fn multi_service_build_script_emits_services_and_no_legacy_top_level_cmd() {
        let spec = derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(script.contains("\"services\""), "emits a services[] supervisor.json");
        assert!(script.contains("\"name\": \"api\"") && script.contains("\"name\": \"redis\""), "{script}");
        assert!(script.contains("/usr/local/bin/ato-guest-agent 'openai_api_key'"), "agent argv = binding");
        assert!(!script.contains("sk-"), "no secret value in the rootfs script");
    }

    #[test]
    fn emitted_supervisor_json_carries_depends_on_and_readiness() {
        // api (public, /health) depends_on redis (internal, PORT 6379). The emitted
        // supervisor.json must carry the graph so the guest can order + wait.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"python3 api.py\"\ndepends_on = [\"redis\"]\nsecrets = [\"openai_api_key\"]\n\
             [services.api.network]\npublish = true\n\
             [services.api.readiness_probe]\nhttp_get = \"/health\"\n\
             [services.redis]\nentrypoint = \"redis-server\"\n[services.redis.env]\nPORT = \"6379\"\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let json = build_supervisor_json(spec.supervisor.as_ref().unwrap(), spec.port, &spec.start_cmd);
        let arr = json["services"].as_array().unwrap();
        let api = arr.iter().find(|s| s["name"] == "api").unwrap();
        let redis = arr.iter().find(|s| s["name"] == "redis").unwrap();
        // api depends on redis and probes its own public port over HTTP /health.
        assert_eq!(api["depends_on"], serde_json::json!(["redis"]));
        assert_eq!(api["readiness"]["port"], spec.port); // public → target port
        assert_eq!(api["readiness"]["http_path"], "/health");
        // redis is internal: readiness on its own PORT, TCP-accept (no http_path).
        assert_eq!(redis["readiness"]["port"], 6379);
        assert!(redis["readiness"].get("http_path").is_none());
        assert!(redis.get("depends_on").is_none(), "no deps ⇒ field omitted");
    }

    #[test]
    fn expose_placeholders_resolve_to_ports_and_cross_reference_across_services() {
        // api (public) depends on redis (internal) which EXPOSES its port via a
        // placeholder rather than hardcoding it. The builder allocates redis's port
        // and makes it reachable from api as REDIS_REDIS_PORT + gives redis its own
        // REDIS_PORT.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"node server.js\"\ndepends_on = [\"redis\"]\nsecrets = [\"openai_api_key\"]\n\
             [services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"redis-server\"\nexpose = [\"REDIS_PORT\"]\n",
            base_toml() // target port 8080
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        let redis = services.iter().find(|s| s.name == "redis").unwrap();

        // redis's placeholder resolved to a concrete allocated port (not the target).
        let rport = redis.base_env.get("REDIS_PORT").expect("own placeholder injected");
        assert_ne!(rport, "8080");
        let rport: u16 = rport.parse().unwrap();
        assert!(rport >= 8091, "allocated from the service port base");
        // redis's primary/readiness port is that allocated port.
        assert_eq!(redis.port, Some(rport));

        // api can reach redis on loopback via the cross-referenced env var.
        assert_eq!(api.base_env.get("REDIS_REDIS_PORT").map(String::as_str), Some(rport.to_string().as_str()));
        // The public service still listens on the proxied target port.
        assert_eq!(api.base_env.get("PORT").map(String::as_str), Some("8080"));
        assert_eq!(api.port, Some(8080));
    }

    #[test]
    fn public_service_first_expose_placeholder_is_the_target_port() {
        // A public service may itself use `expose` — its FIRST placeholder is the
        // proxied target port; an additional one gets an allocated port.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.web]\nentrypoint = \"node web.js\"\nexpose = [\"HTTP_PORT\", \"METRICS_PORT\"]\nsecrets = [\"openai_api_key\"]\n\
             [services.web.network]\npublish = true\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let web = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap()[0].clone();
        assert_eq!(web.base_env.get("HTTP_PORT").map(String::as_str), Some("8080"), "first expose = target port");
        assert_eq!(web.base_env.get("PORT").map(String::as_str), Some("8080"), "public always gets PORT=target");
        let metrics: u16 = web.base_env.get("METRICS_PORT").unwrap().parse().unwrap();
        assert_ne!(metrics, 8080, "second expose is a distinct allocated port");
        assert_eq!(web.port, Some(8080));
    }

    #[test]
    fn expose_placeholder_must_be_a_posix_identifier() {
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nexpose = [\"bad-name\"]\n[services.api.network]\npublish = true\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&toml), &probe_python())
            .unwrap_err()
            .contains("POSIX identifier"));
    }

    /// Helper: a two-service capsule (public api + one internal service) with the
    /// internal service's body spliced in, for the collision fail-closed cases.
    fn expose_collision_toml(internal: &str) -> String {
        format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n{internal}",
            base_toml()
        )
    }

    #[test]
    fn expose_port_injection_is_fail_closed_on_every_collision() {
        // (1) Duplicate placeholder within a service → rejected (would overwrite).
        let dup = expose_collision_toml(
            "[services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\", \"REDIS_PORT\"]\n",
        );
        assert!(derive_supervisor_build_spec(&parse(&dup), &probe_python())
            .unwrap_err()
            .contains("duplicate expose placeholder"));

        // (2) Two (service, placeholder) pairs generate the SAME cross-ref var:
        //     `a-b`+`C` and `a`+`B_C` both → `A_B_C`. Rejected.
        let alias = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.web]\nentrypoint = \"w\"\n[services.web.network]\npublish = true\n\
             [services.a-b]\nentrypoint = \"x\"\nexpose = [\"C\"]\n\
             [services.a]\nentrypoint = \"y\"\nexpose = [\"B_C\"]\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&alias), &probe_python())
            .unwrap_err()
            .contains("cross-reference env var"));

        // (3a) Own placeholder env var already declared by the author → rejected.
        let own = expose_collision_toml(
            "[services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n\
             [services.redis.env]\nREDIS_PORT = \"1234\"\n",
        );
        assert!(derive_supervisor_build_spec(&parse(&own), &probe_python())
            .unwrap_err()
            .contains("collides with an existing"));

        // (3b) Generated cross-reference var already declared by the author → rejected.
        let xref = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.api.env]\nREDIS_REDIS_PORT = \"1234\"\n\
             [services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&xref), &probe_python())
            .unwrap_err()
            .contains("collides with an existing"));
    }

    #[test]
    fn port_allocation_is_deterministic_and_independent_of_ambient_env() {
        // The base is a fixed constant, not ambient env — the same manifest always
        // allocates the same ports (reproducible sealed rootfs). Setting the old
        // env var must have NO effect.
        let toml = expose_collision_toml(
            "[services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n",
        );
        let port_of = || {
            let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
            let svc = spec.supervisor.unwrap().services.unwrap();
            svc.iter().find(|s| s.name == "redis").unwrap().port.unwrap()
        };
        let a = port_of();
        // SAFETY: single-threaded test; the value is ignored by the allocator now.
        unsafe { std::env::set_var("ATO_SERVICE_PORT_BASE", "20000") };
        let b = port_of();
        unsafe { std::env::remove_var("ATO_SERVICE_PORT_BASE") };
        assert_eq!(a, b, "allocation must not depend on ambient env");
        assert_eq!(a, 8091, "fixed base");
    }

    #[test]
    fn multi_service_fail_closed_rules() {
        let bad = |extra: &str, needle: &str| {
            let toml = format!("{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{extra}", base_toml());
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains(needle), "expected {needle:?} in: {err}");
        };
        // No public service.
        bad("[services.api]\nentrypoint = \"python3 api.py\"\n", "no public service");
        // Two public services.
        bad(
            "[services.a]\nentrypoint = \"a\"\n[services.a.network]\npublish = true\n\
             [services.b]\nentrypoint = \"b\"\n[services.b.network]\npublish = true\n",
            "exactly one may be public",
        );
        // Empty entrypoint.
        bad("[services.api]\nentrypoint = \"\"\n[services.api.network]\npublish = true\n", "`entrypoint` is empty");
        // depends_on to an unknown service.
        bad(
            "[services.api]\nentrypoint = \"a\"\ndepends_on = [\"ghost\"]\n[services.api.network]\npublish = true\n",
            "not a declared service",
        );
        // Container-only field: state_bindings.
        bad(
            "[services.api]\nentrypoint = \"a\"\nstate_bindings = [{ state = \"d\", target = \"/x\" }]\n[services.api.network]\npublish = true\n",
            "state_bindings",
        );
        // Container-only field: egress_proxy opt-out.
        bad(
            "[services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\negress_proxy = false\n",
            "egress_proxy = false",
        );
    }

    #[test]
    fn public_service_port_must_not_diverge_from_the_target_port() {
        // A public service that declares a DIFFERENT PORT than the proxied target
        // port would listen where the proxy is NOT — fail closed.
        let mismatch = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"node server.js\"\nsecrets = [\"openai_api_key\"]\n[services.api.env]\nPORT = \"3000\"\n\
             [services.api.network]\npublish = true\n",
            base_toml() // target port = 8080
        );
        let err = derive_supervisor_build_spec(&parse(&mismatch), &probe_python()).unwrap_err();
        assert!(err.contains("proxied port") && err.contains("3000"), "{err}");

        // Declaring the SAME port is fine (redundant but honest).
        let same = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"node server.js\"\nsecrets = [\"openai_api_key\"]\n[services.api.env]\nPORT = \"8080\"\n\
             [services.api.network]\npublish = true\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&same), &probe_python()).unwrap();
        let json = build_supervisor_json(spec.supervisor.as_ref().unwrap(), spec.port, &spec.start_cmd);
        assert_eq!(json["services"][0]["base_env"]["PORT"], "8080");

        // Absent PORT → the builder injects the target port.
        let spec = derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let json = build_supervisor_json(spec.supervisor.as_ref().unwrap(), spec.port, &spec.start_cmd);
        assert_eq!(json["services"][0]["base_env"]["PORT"], "8080");

        // An INTERNAL service may listen wherever it likes — its PORT is untouched.
        let internal_port = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"redis-server\"\n[services.redis.env]\nPORT = \"6379\"\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&internal_port), &probe_python()).unwrap();
        let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
        let redis = services.iter().find(|s| s.name == "redis").unwrap();
        assert_eq!(redis.base_env.get("PORT").map(String::as_str), Some("6379"));
    }

    #[test]
    fn service_aliases_must_be_dns_safe_and_readiness_path_is_recorded() {
        let bad_alias = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\naliases = [\"Bad_Alias\"]\n",
            base_toml()
        );
        assert!(derive_supervisor_build_spec(&parse(&bad_alias), &probe_python())
            .unwrap_err()
            .contains("DNS-safe"));

        // A declared HTTP readiness path is recorded on the service spec.
        let with_probe = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.api.readiness_probe]\nhttp_get = \"/healthz\"\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&with_probe), &probe_python()).unwrap();
        let api = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap()[0].clone();
        assert_eq!(api.readiness_http_path.as_deref(), Some("/healthz"));
    }

    #[test]
    fn multi_service_coexists_with_the_build_target_which_supplies_the_public_port() {
        // The [targets.app] anchor supplies runtime/port; [services] supply the
        // runtime processes. The PUBLIC service inherits that derived port.
        let spec = derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        assert_eq!(spec.port, 8080, "port comes from the build target");
        let sup = spec.supervisor.as_ref().unwrap();
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let api = &json["services"].as_array().unwrap()[0];
        assert_eq!(api["name"], "api");
        assert_eq!(api["base_env"]["PORT"], "8080", "public service listens on the proxied port");
    }

    #[test]
    fn single_service_supervisor_json_stays_byte_identical() {
        // A legacy (no [services]) supervisor build must emit the OLD top-level shape.
        let spec = derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(sup.services.is_none(), "no [services] ⇒ legacy single-service build");
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        assert!(json.get("services").is_none(), "legacy build emits top-level cmd, not services[]");
        assert_eq!(json["cmd"], serde_json::json!(["/bin/sh", "-lc", "python3 app.py"]));
        assert_eq!(json["base_env"]["PORT"], spec.port.to_string());
        assert_eq!(json["bindings_env"]["OPENAI_API_KEY"], "openai_api_key");
    }
}
