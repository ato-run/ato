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
    CapsuleManifest, RuntimeType, ServiceSpec as ManifestServiceSpec, StateDurability,
    StateSharing, validate_and_normalize_state_mount_target, validate_state_volume_size_mb,
};
// Only referenced from #[cfg(test)] assertions below; unused in the non-test build.
#[allow(unused_imports)]
use capsule::foundation::types::manifest::DEFAULT_STATE_VOLUME_SIZE_MB;
use capsule::foundation::types::ready_state::SecretDelivery;
use protocol::binding_lease::BindingName;
use serde::Serialize;

use crate::state_volume::{drive_id as state_drive_id, volume_label};

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
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "py"))
            })
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
    /// Phase 7 (generated internal bindings): RUN-time generated internal
    /// secrets. Non-secret — the SPEC only (name/generator/bytes/scope/targets),
    /// never a value. Emitted into `supervisor.json` so the guest-agent generates
    /// each value at run and injects it into every target service; recorded in the
    /// receipt and hashed into the artifact identity (spec, not value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_bindings: Vec<GeneratedBindingBuildSpec>,
}

/// Phase 7 (generated internal bindings): one `[generated_bindings.<name>]`
/// entry, resolved for the supervisor build. Non-secret — safe in a receipt and
/// in the artifact identity. Holds NO value; the value is generated per run
/// inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeneratedBindingBuildSpec {
    /// The binding NAME (tmpfs filename each target service reads).
    pub name: String,
    /// Generator method (`random_base64`).
    pub generator: String,
    /// Bytes of OS randomness drawn before encoding.
    pub bytes: u32,
    /// Lifetime scope (`run`).
    pub scope: String,
    /// The services whose env receives this value.
    pub targets: Vec<String>,
}

/// v1.5 (ato#973): one service in a multi-service supervisor build. Non-secret —
/// safe in a receipt. `env_map` maps an env var to the binding NAME (never a value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceBuildSpec {
    /// Stable service name (unique; keys per-service logs + diagnostics).
    pub name: String,
    /// One-shot task (manifest `run_once = true`): runs to completion during
    /// the BUILD boot (`run_at: ["seal_once"]`) before dependents start; a
    /// restore resumes the sealed memory, so it never re-runs per preview.
    pub run_once: bool,
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
    /// v1.6 (ato#983): this service's durable state volumes, derived from its
    /// `state_bindings`. Empty for every service without declared state — no
    /// behavior change for existing capsules. Not yet emitted into
    /// `supervisor.json` (recorded here for the receipt; a later slice wires
    /// the actual Firecracker drive attach + guest mount).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<DurableVolumeBuildSpec>,
}

/// v1.6 (ato#983): one durable state volume a snapshot service mounts. Derived
/// from a `state_bindings` entry + its `[state.<name>]` requirement.
/// Non-secret — safe in a receipt. `target` is already validated + LEXICALLY
/// NORMALIZED (see [`validate_and_normalize_state_mount_target`]) and
/// `size_mb` already bounds-checked (see [`validate_state_volume_size_mb`]) —
/// both at manifest-validation time AND again here (defense in depth).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurableVolumeBuildSpec {
    pub state_name: String,
    /// Normalized absolute in-guest mount path, always under `/ato/state/`.
    pub target: String,
    pub size_mb: u32,
    /// v1.6 (ato#983) Slice 3: `crate::state_volume::drive_id(i)` for this
    /// volume, `i` = its index in the GLOBAL (cross-service) list of every
    /// declared volume in the capsule, sorted by `state_name` — assigned here,
    /// once, for the WHOLE capsule (not per service), so it matches EXACTLY
    /// what `firecracker.rs`'s `state_volume::prepare_volumes` (Slice 2)
    /// independently computes at attach time from the same
    /// (owner_scope, state_name) pairs sorted the same way. Diagnostic only —
    /// the guest resolves its device by `fs_label`, not this id.
    pub drive_id: String,
    /// v1.6 (ato#983) Slice 3: `crate::state_volume::volume_label(owner_scope,
    /// state_name)` — the guest resolves its actual block device with this
    /// (e.g. `blkid -L <fs_label>`), never by a `/dev/vdN` index (device
    /// enumeration order is not a contract Slice 2 makes).
    pub fs_label: String,
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
pub fn derive_build_spec(
    m: &CapsuleManifest,
    probe: &SourceProbe,
) -> Result<RootfsBuildSpec, String> {
    if m.secrets.values().any(|s| s.required) {
        return Err("capsule requires secrets (secrets.*.required)".into());
    }
    // Any binding disqualifies a v1 no-binding snapshot — this is also how user-files
    // and oauth are declared (BindingKind::UserFiles / ::Oauth), so it rejects those too.
    if !m.bindings.is_empty() {
        let kinds: Vec<String> = m
            .bindings
            .values()
            .map(|b| format!("{:?}", b.kind).to_ascii_lowercase())
            .collect();
        return Err(format!(
            "capsule declares bindings ({}) — v1 is no-binding only",
            kinds.join(", ")
        ));
    }
    if !m.external.is_empty() {
        return Err("capsule requires external services (external.*)".into());
    }
    if m.build.as_ref().map(|b| b.gpu).unwrap_or(false) {
        return Err("capsule requires GPU (build.gpu)".into());
    }

    // 0.3 runtime/port/healthcheck live on the default [targets.<label>], not [execution].
    let target = m.resolve_default_target().map_err(|e| e.to_string())?;
    let port = target
        .port
        .ok_or("capsule default target has no port (declare `port = <n>` on the default target)")?;
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
    let build_cmd = target
        .build_command
        .clone()
        .filter(|c| !c.trim().is_empty());
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
    let lang = target
        .language
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let runtime = match rt.normalize() {
        RuntimeType::Web => RuntimeKind::StaticWeb,
        RuntimeType::Source => detect_runtime_kind(&driver, &lang, probe)?,
        other => {
            return Err(format!(
                "unsupported runtime {other:?} (v1 supports: static web, node source, python source)"
            ));
        }
    };

    let (base_image, install_cmd) = base_image_and_install(runtime, probe);

    Ok(RootfsBuildSpec {
        runtime,
        base_image,
        install_cmd,
        build_cmd,
        start_cmd,
        declared_start_cmd,
        port,
        healthcheck,
        probe_synthesized,
        supervisor: None,
    })
}

/// Where a v1 build places the source in the guest, and therefore the working
/// directory its launch runs in.
///
/// The BUILD decides this, not the author: the v1 manifest has no
/// working-directory field and should not gain one, because requiring the
/// author to restate `/app` only creates a way for the manifest to be wrong
/// about where the builder put things. It is a resolved facet of the Execution
/// Contract (ADR-015 §4.1 `launch.cwd`), and this constant is the single place
/// the generated Dockerfile's `WORKDIR` and the init's `cd` agree on it.
pub const V1_GUEST_WORKING_DIRECTORY: &str = "/app";

/// argv a runtime prepends to the authored command — and the fact that a
/// producer looked.
///
/// A bare `Vec<String>` cannot carry that second half. An empty vector reads
/// both as "the runtime prepends nothing" and as "nobody measured this", and
/// the Execution Contract must never record the latter as the former:
/// `runtime.dynamic_contract` is a measured facet (ADR-015 §4.1), so an absent
/// measurement has to refuse the mint rather than mint an empty one. The only
/// way to construct this value is to say which of the two you observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ObservedInvocationPrefix(Vec<String>);

impl ObservedInvocationPrefix {
    /// The producer confirmed the runtime prepends nothing and execs the
    /// authored argv directly. A measurement, not a default.
    #[must_use]
    pub fn observed_none() -> Self {
        Self(Vec::new())
    }

    /// The producer confirmed the runtime prepends exactly `words`.
    ///
    /// Refuses an empty `words`: that is [`Self::observed_none`]'s meaning, and
    /// allowing it here would reintroduce the ambiguity the type exists to
    /// remove — a caller with nothing measured could reach the "nothing
    /// prepended" value by passing the vector it happens to hold.
    pub fn observed(words: Vec<String>) -> Result<Self, String> {
        if words.is_empty() {
            return Err(
                "an observed invocation prefix is empty; use observed_none() to record that \
                 the runtime prepends nothing"
                    .into(),
            );
        }
        Ok(Self(words))
    }

    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.0
    }

    #[must_use]
    pub fn into_words(self) -> Vec<String> {
        self.0
    }
}

/// A buildable rootfs for a `schema_version = "1"` capsule.
///
/// The difference from [`RootfsBuildSpec`] that matters is `resolved_argv`.
/// v0.3 carries a `start_cmd: String` that the init hands to `sh -lc`, so the
/// guest re-parses it and argument boundaries are whatever the shell decides.
/// v1's `[run] command` is exact argv (RFC §6.1), and the Execution Contract
/// commits it as a list — so re-joining it into a shell string here would
/// destroy the very boundaries the contract promises to have preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootfsBuildSpecV1 {
    pub runtime: RuntimeKind,
    pub base_image: String,
    pub install_cmd: Option<String>,
    /// argv the runtime prepends to the authored command.
    ///
    /// Nothing for every family in the Step-4 subset, and that is a measurement
    /// rather than a gap: v1 argv is exact, so the v0.3 bare-`.py`
    /// normalization (which silently turns `app.py` into `python3 app.py`)
    /// deliberately does NOT apply — an author who wants an interpreter names
    /// it. The field exists because a future runtime may genuinely prepend one,
    /// and the contract must be able to say so.
    pub runtime_invocation_prefix: ObservedInvocationPrefix,
    /// The complete argv init execs: `runtime_invocation_prefix` ++ the
    /// authored `[run] command`.
    pub resolved_argv: Vec<String>,
    pub port: u16,
}

/// Derive a v1 rootfs spec from the authored manifest and a source probe.
///
/// Fail-closed on everything the Step-4 subset does not cover — the subset gate
/// runs first, so a manifest with `[tools]`, `[[build.steps]]` or `[state.*]`
/// never reaches the runtime detection below.
pub fn derive_build_spec_v1(
    m: &capsule::types::manifest_v1::CapsuleManifestV1,
    probe: &SourceProbe,
) -> Result<RootfsBuildSpecV1, String> {
    m.validate_for_interactive_capture()
        .map_err(|error| error.to_string())?;

    let web = m
        .web
        .as_ref()
        .ok_or("a v1 build needs a [web] surface to serve")?;

    // v1 has no driver/language hints — the tree is the whole declaration.
    let runtime = detect_runtime_kind("", "", probe)?;
    let (base_image, install_cmd) = base_image_and_install(runtime, probe);

    let authored = &m.run.command;
    if authored.is_empty() {
        return Err("[run] command is empty; there is no argv to launch".into());
    }
    // Each word is emitted single-quoted into the generated init, so a control
    // character could break out of the quoting or the heredoc delimiter. Reject
    // at derivation rather than escaping at emission (fail-closed).
    for (index, word) in authored.iter().enumerate() {
        reject_control_chars(&format!("[run] command argv[{index}]"), word)?;
        // The Execution Contract refuses an empty or whitespace-only argv word
        // (`launch.argv` must be resolved). Refusing it here means the recipe
        // never builds an image whose identity could not be minted — the same
        // refusal, before anything is spent on it, and pointing at the manifest
        // line rather than at a contract field.
        if word.trim().is_empty() {
            return Err(format!(
                "[run] command argv[{index}] is empty; every word of an exact argv must \
                 resolve to something, so an empty argument cannot be committed"
            ));
        }
    }

    // Measured, not assumed: none of the three families in the Step-4 subset
    // wraps the authored argv — the generated init execs it directly.
    let runtime_invocation_prefix = ObservedInvocationPrefix::observed_none();
    let resolved_argv = runtime_invocation_prefix
        .words()
        .iter()
        .chain(authored.iter())
        .cloned()
        .collect();

    Ok(RootfsBuildSpecV1 {
        runtime,
        base_image,
        install_cmd,
        runtime_invocation_prefix,
        resolved_argv,
        port: web.port,
    })
}

/// The init line that launches a v1 capsule: every argv word single-quoted,
/// which the guest `/bin/sh` parses back into EXACTLY the same argv.
///
/// This is the whole reason v1 does not reuse `sh -lc '<joined>'`. Quoting each
/// word preserves boundaries — `["python3", "app one.py"]` stays two arguments
/// — whereas joining and re-splitting would turn the space into a separator and
/// launch a different program than the contract committed to.
pub(crate) fn launch_argv_line(argv: &[String]) -> String {
    let words = argv
        .iter()
        .map(|word| shell_single_quote(word))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{words} >/tmp/app.log 2>&1 &")
}

/// Assemble the v1 app image and STOP — the image survives the script under
/// `$ATO_IMAGE`, unpacked.
///
/// v0.3 builds and packs in one bash invocation because nothing ever needs to
/// look at the intermediate image. v1 does: `target.{os,architecture,abi,libc}`
/// has to be measured off the image the guest actually boots, not off the base
/// image it was derived from, and by the time the pack half has run the image
/// is gone (the pack script's EXIT trap `rmi`s it). Splitting the two is what
/// makes the measurement land on the built artifact.
///
/// `pinned_base_ref` is the base image resolved to `repo@sha256:…`, never the
/// tag the spec derived. A tag can move between the resolution that recorded
/// `runtime.digest` and the build that consumes it, and then the contract would
/// name bytes the guest never ran.
///
/// Security: same properties as [`build_rootfs_script`] — the Dockerfile is a
/// QUOTED heredoc so the builder-host shell expands nothing in its body, and
/// the manifest-derived install command is embedded as a single-quoted argument
/// to `/bin/sh -lc`, so it runs only inside Docker's RUN.
///
/// env: `ATO_SRC` (the PROJECTED source tree), `ATO_IMAGE`.
pub(crate) fn assemble_app_image_script_v1(
    spec: &RootfsBuildSpecV1,
    pinned_base_ref: &str,
    tool: &str,
) -> String {
    let install_q = shell_single_quote(spec.install_cmd.as_deref().unwrap_or("true"));
    // The projection goes in a SUBDIRECTORY of the build context, and the
    // generated Dockerfile sits beside it rather than inside it.
    //
    // v0.3 copies the source to the context root and writes its Dockerfile
    // there, which is harmless when nothing claims what the guest contains. It
    // is not harmless here: a repository carrying its own `Dockerfile` would
    // have it overwritten by the generated one and then shipped to `/app` by
    // `COPY .`, so `source.digest` would commit a tree — the one holding the
    // AUTHOR's Dockerfile — that the guest does not have. Editing that file
    // would move the execution id without changing anything the guest sees, and
    // a repository with no Dockerfile would still get one at `/app` that is not
    // in the projection. Copying `src/.` makes the guest's `{workdir}` exactly
    // the projection, byte for byte.
    format!(
        r#"set -euo pipefail
BUILD=$(mktemp -d)
cleanup() {{
  [ -n "$BUILD" ] && rm -rf "$BUILD" 2>/dev/null || true
}}
trap cleanup EXIT
mkdir -p "$BUILD/src"
cp -a "$ATO_SRC/." "$BUILD/src/"
# QUOTED heredoc: no host expansion; commands run inside Docker RUN via sh -lc '<literal>'.
cat > "$BUILD/Dockerfile" <<'DOCKER'
FROM {base}
WORKDIR {workdir}
COPY src/. {workdir}/
RUN /bin/sh -lc {install_q}
DOCKER
{tool} build -q -t "$ATO_IMAGE" "$BUILD" >/dev/null
"#,
        tool = tool,
        base = pinned_base_ref,
        workdir = V1_GUEST_WORKING_DIRECTORY,
        install_q = install_q,
    )
}

/// The timestamp every file in a v1 guest image carries.
///
/// A constant, not the clock. `filesystem.view_digest` is blake3 over the packed
/// image and is committed by the Execution Identity, so any wall-clock value
/// reaching those bytes makes the identity a function of WHEN the build ran —
/// two builds of one program source would be two executions, `capsule.lock`
/// would be rewritten every time, and two builder hosts would never agree.
///
/// `1` rather than `0` because a zero mtime reads as "unset" to some tooling,
/// and the distinction costs nothing. It is exported as `SOURCE_DATE_EPOCH`
/// for `mke2fs`, which honours it for the superblock timestamps (e2fsprogs
/// 1.45.7+), and applied to the tree with `touch` for the inode timestamps,
/// which nothing else normalizes.
pub const V1_GUEST_IMAGE_EPOCH: &str = "1";

/// Pack the already-assembled `$ATO_IMAGE` into a read-only-bootable ext4,
/// reproducibly.
///
/// Same create → export → init → pack shape as [`build_rootfs_script`], and it
/// differs from v0.3 in the two ways v1 needs:
///
/// **The init launches an argv** rather than a shell string, and there is no
/// build command — the Step-4 subset rejects `[[build.steps]]`.
///
/// **The image is a function of its inputs, not of the clock or the allocator.**
/// v0.3 does `mkfs.ext4` on an empty file, `mount -o loop`, then `cp -a`. That
/// produces a different image every run — `mkfs.ext4` writes a random UUID, a
/// random directory-hash seed and wall-clock superblock timestamps, the exported
/// rootfs carries build-time mtimes, and copying into a mounted filesystem
/// leaves allocation to the running kernel. v0.3 can afford that because nothing
/// claims what its image IS. v1 commits blake3 of these bytes as
/// `filesystem.view_digest`, so each of those becomes part of the identity.
///
/// So: timestamps are normalized to [`V1_GUEST_IMAGE_EPOCH`], the UUID and hash
/// seed are passed in (derived by the caller from the build's own inputs, so
/// they are stable per program but distinct per capsule), and the filesystem is
/// POPULATED BY `mke2fs -d` instead of by a mount-and-copy — which fixes the
/// allocation order and, incidentally, removes the loop mount from this half
/// entirely.
///
/// env: `ATO_IMAGE`, `ATO_OUT`, `ATO_FS_UUID`.
pub(crate) fn pack_app_image_script_v1(
    spec: &RootfsBuildSpecV1,
    size_mib: u64,
    tool: &str,
) -> String {
    format!(
        r#"set -euo pipefail
TAG="$ATO_IMAGE"
CID=""
BUILD=$(mktemp -d)
# Failure-safe cleanup: on ANY exit leave no container, image, or temp dir
# behind (Phase 8 orphan-hardening parity). No mount to unwind — `mke2fs -d`
# populates the filesystem without one.
cleanup() {{
  [ -n "$CID" ] && {tool} rm -f "$CID" >/dev/null 2>&1 || true
  {tool} rmi -f "$TAG" >/dev/null 2>&1 || true
  [ -n "$BUILD" ] && rm -rf "$BUILD" 2>/dev/null || true
}}
trap cleanup EXIT
CID=$({tool} create "$TAG")
mkdir -p "$BUILD/rootfs"
{tool} export "$CID" | tar -x -C "$BUILD/rootfs"
{tool} rm -f "$CID" >/dev/null; CID=""
# Read-only-bootable init (matches benchmarks/ready-state/build_rootfs_ro.sh): mount the
# pseudo + tmpfs filesystems, then run the capsule start command in the background
# (serves port {port}) and keep PID 1 alive. QUOTED heredoc: each argv word is
# single-quoted, so the guest shell parses back EXACTLY the argv the contract commits.
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
cd {init_cwd}
{launch}
while true; do sleep 1000; done
INIT
chmod +x "$BUILD/rootfs/sbin/init"
# Every inode timestamp becomes a constant. `-h` so a symlink's own timestamps
# are set rather than its target's being set twice.
find "$BUILD/rootfs" -exec touch -h -d @{epoch} {{}} +
rm -f "$ATO_OUT"
dd if=/dev/zero of="$ATO_OUT" bs=1M count={size} status=none
# `-d` populates at mkfs time: no loop mount, and the allocation order is
# mke2fs's own rather than the running kernel's. `-U` and `-E hash_seed` replace
# the two values mke2fs would otherwise draw at random.
mkfs.ext4 -q -F \
  -U "$ATO_FS_UUID" \
  -E hash_seed="$ATO_FS_UUID" \
  -d "$BUILD/rootfs" \
  "$ATO_OUT"
# The superblock clocks, set explicitly because mke2fs does NOT honour
# SOURCE_DATE_EPOCH: measured on e2fsprogs 1.47.0, two runs ten seconds apart
# produced two "Filesystem created" values ten seconds apart, and everything
# else in the superblock was already identical. Through debugfs rather than a
# byte patch because ext4 carries a superblock checksum that debugfs recomputes
# and a raw write would invalidate. `wtime` goes last: every debugfs write
# updates it.
# SOURCE_DATE_EPOCH here and not on mke2fs: mke2fs demonstrably ignores it for
# `s_mkfs_time`, but every e2fsprogs tool stamps `s_wtime` from `fs->now` when it
# flushes — so without it debugfs would overwrite the `wtime` set below with the
# clock as it closes, and the superblock checksum with it.
SOURCE_DATE_EPOCH={epoch} debugfs -w -f - "$ATO_OUT" >/dev/null 2>&1 <<'DEBUGFS'
set_super_value mkfs_time {epoch}
set_super_value lastcheck {epoch}
set_super_value mtime 0
set_super_value wtime {epoch}
quit
DEBUGFS
# BUILD is removed by the EXIT trap (also on any failure above).
"#,
        tool = tool,
        launch = launch_argv_line(&spec.resolved_argv),
        init_cwd = V1_GUEST_WORKING_DIRECTORY,
        port = spec.port,
        size = size_mib,
        epoch = V1_GUEST_IMAGE_EPOCH,
    )
}

/// A guest image that exists on the builder host and has not been packed yet.
///
/// Holding it is what lets `measure_guest_target` run against the artifact the
/// guest boots. It is not `Copy` and not `Clone`: exactly one owner is
/// responsible for either packing it (which consumes it) or discarding it, so
/// a failure between assembly and packing cannot leak an image.
#[derive(Debug, PartialEq, Eq)]
pub struct AssembledGuestImage {
    image_ref: String,
}

impl AssembledGuestImage {
    /// Take responsibility for an image that already exists under `image_ref`.
    ///
    /// The value is a claim that there is an image to pack;
    /// [`assemble_app_image_v1`] earns that claim by building one. This lets a
    /// caller that produced the image another way take the same obligation —
    /// pack it or discard it — which is what a producer standing in for docker
    /// in a test needs, and what a second assembly backend would need. It is
    /// not a way to conjure an image: nothing downstream checks that the
    /// reference resolves, so an `adopt` of a name nothing built fails at the
    /// first command that addresses it.
    #[must_use]
    pub fn adopt(image_ref: String) -> Self {
        Self { image_ref }
    }

    /// The local reference the assembled image is tagged with. Use it as
    /// `measure_guest_target`'s `image_ref`.
    #[must_use]
    pub fn image_ref(&self) -> &str {
        &self.image_ref
    }
}

/// Build the v1 app image from the PROJECTED source tree.
///
/// `projected_source` must be the materialized program-source projection — the
/// tree `source.digest` names — and not the workspace checkout. Passing the
/// checkout would put `capsule.toml` and the lock into the guest at
/// `/app`, and the contract would then commit a digest for a tree the guest
/// does not have.
///
/// Shells out to `docker`. Docker is a build tool here, not a trust boundary.
pub fn assemble_app_image_v1(
    projected_source: &Path,
    spec: &RootfsBuildSpecV1,
    pinned_base_ref: &str,
    image_ref: &str,
    tool: &str,
) -> Result<AssembledGuestImage, String> {
    // Both land in a generated script — the base ref inside a quoted heredoc,
    // the image ref as a shell variable. A newline in either could break out.
    reject_control_chars("pinned base image reference", pinned_base_ref)?;
    reject_control_chars("assembled image reference", image_ref)?;

    let script = assemble_app_image_script_v1(spec, pinned_base_ref, tool);
    run_builder_script(
        "assemble app image",
        &script,
        &[
            ("ATO_SRC", projected_source.as_os_str().to_os_string()),
            ("ATO_IMAGE", image_ref.into()),
        ],
    )?;
    Ok(AssembledGuestImage {
        image_ref: image_ref.to_string(),
    })
}

/// Pack an assembled image into `out_ext4`, returning its size in bytes.
///
/// Consumes the image: the emitted script removes it on exit, so there is
/// nothing left to discard afterwards. Requires root (mount) + docker.
pub fn pack_app_image_v1(
    image: AssembledGuestImage,
    spec: &RootfsBuildSpecV1,
    out_ext4: &Path,
    size_mib: u64,
    filesystem_uuid: &str,
    tool: &str,
) -> Result<u64, String> {
    // Interpolated nowhere, but it reaches `mke2fs` as two arguments, so a
    // malformed value is refused rather than passed on.
    if !is_uuid(filesystem_uuid) {
        return Err(format!(
            "filesystem uuid {filesystem_uuid:?} is not a canonical 8-4-4-4-12 hex UUID"
        ));
    }
    let script = pack_app_image_script_v1(spec, size_mib, tool);
    run_builder_script(
        "pack app image",
        &script,
        &[
            ("ATO_IMAGE", image.image_ref.as_str().into()),
            ("ATO_OUT", out_ext4.as_os_str().to_os_string()),
            ("ATO_FS_UUID", filesystem_uuid.into()),
        ],
    )?;
    std::fs::metadata(out_ext4)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("stat packed rootfs {}: {error}", out_ext4.display()))
}

/// Remove an assembled image that will not be packed.
///
/// Best-effort by design: this runs on the failure path, where the caller is
/// already returning a more informative error and a leaked image is a disk
/// cost rather than a correctness problem. Consuming the image means a caller
/// cannot discard one and then pack it.
pub fn discard_app_image_v1(image: AssembledGuestImage, tool: &str) {
    let _ = Command::new(tool)
        .args(["rmi", "-f", &image.image_ref])
        .output();
}

/// The filesystem UUID and directory-hash seed a v1 guest image is built with.
///
/// `mke2fs` would generate both at random, and both land in the packed bytes
/// that `filesystem.view_digest` commits — so they have to be a function of the
/// build rather than of entropy. Deriving them from inputs fixed BEFORE the pack
/// (the projected source, the pinned base image, the exact argv) keeps them
/// stable for one program and distinct between programs, which is what a
/// filesystem UUID is for: a constant shared by every capsule would make two
/// different images collide for anything resolving a device by UUID.
///
/// Domain-separated so this can never coincide with another blake3 the identity
/// also commits. It is NOT itself an identity input — the contract commits the
/// packed bytes, and this is one of the things that determines them.
#[must_use]
pub fn v1_filesystem_uuid(source_digest: &str, pinned_base_ref: &str, argv: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ato.v1-guest-image-uuid/v1\0");
    hasher.update(source_digest.as_bytes());
    hasher.update(b"\0");
    hasher.update(pinned_base_ref.as_bytes());
    for word in argv {
        hasher.update(b"\0");
        hasher.update(word.as_bytes());
    }
    let bytes = hasher.finalize();
    let hex = bytes.to_hex();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A canonical 8-4-4-4-12 lowercase-hex UUID.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(width, group)| {
                group.len() == *width
                    && group
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
}

/// Run one generated builder script under `bash -c`, reporting the stderr tail
/// on failure. The shared spawn half of [`build_rootfs`] and the two v1 halves.
fn run_builder_script(
    stage: &str,
    script: &str,
    env: &[(&str, std::ffi::OsString)],
) -> Result<(), String> {
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command
        .output()
        .map_err(|error| format!("spawn {stage}: {error}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Err(format!("{stage} failed: {tail}"))
}

/// Which runtime family a source tree belongs to.
///
/// Shared by the v0.3 recipe path and the v1 authoring surface so the two can
/// never disagree about what "a python capsule" is. `driver`/`language` are the
/// v0.3 target's explicit hints (lowercased, empty when absent); v1 has no
/// counterpart for them by design — a v1 manifest declares no runtime, so the
/// tree is the whole declaration and the probe decides alone.
pub(crate) fn detect_runtime_kind(
    driver: &str,
    language: &str,
    probe: &SourceProbe,
) -> Result<RuntimeKind, String> {
    if driver == "node"
        || language == "javascript"
        || language == "typescript"
        || probe.has_package_json
    {
        Ok(RuntimeKind::Node)
    } else if driver == "python"
        || language == "python"
        || probe.has_requirements_txt
        || probe.has_pyproject
        || probe.has_py_files
    {
        Ok(RuntimeKind::Python)
    } else if driver == "static" || probe.has_index_html {
        Ok(RuntimeKind::StaticWeb)
    } else {
        Err("source runtime: no node (package.json/driver) or python (requirements.txt/pyproject/driver) detected".into())
    }
}

/// The base image a runtime family boots on, and the install step its
/// dependency manifest implies. Both are RESOLVED values — the builder's
/// choice, not the author's — which is why the Execution Contract records the
/// base image by resolved digest rather than by the tag chosen here.
pub(crate) fn base_image_and_install(
    runtime: RuntimeKind,
    probe: &SourceProbe,
) -> (String, Option<String>) {
    match runtime {
        RuntimeKind::StaticWeb => ("python:3.11-slim".to_string(), None),
        RuntimeKind::Node => (
            "node:20-slim".to_string(),
            Some(if probe.has_package_json {
                "npm ci --omit=dev || npm install --omit=dev".to_string()
            } else {
                "true".to_string()
            }),
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
    }
}

/// A POSIX-ish environment variable name: `^[A-Za-z_][A-Za-z0-9_]*$`. The name is
/// interpolated into the generated `supervisor.json` + the guest spawn script, so a
/// malformed name is **rejected at emission** (fail-closed), never emitted — mirroring
/// the guest-agent's own validation (#947) so a broken `supervisor.json` is never built.
pub(crate) fn valid_env_var_name(name: &str) -> bool {
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
pub fn derive_supervisor_build_spec(
    m: &CapsuleManifest,
    probe: &SourceProbe,
) -> Result<RootfsBuildSpec, String> {
    // A supervisor build is warranted by EITHER an env-delivery secret (the
    // binding-lease path) OR a Phase 7 generated internal binding (the guest
    // generates + injects the value at run) — both need the guest-agent as init.
    if m.secrets.is_empty() && m.generated_bindings.is_empty() {
        return Err(
            "supervisor build requires at least one [secrets.*] (delivery = \"env\") or \
             [generated_bindings.*]"
                .into(),
        );
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
        let var = s
            .env
            .clone()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| name.clone());
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
    let mut services = derive_supervisor_services(m, &env_map, spec.port)?;

    // v1.5 per-service secret scoping (ato#982): in a MULTI-service build a secret
    // reaches only the service(s) that named it. Fail-closed: every REQUIRED secret
    // must be scoped to at least one service (an unscoped required secret would be
    // waited-for by the gate but delivered to nobody — a config error). The lease
    // set (`binding_names`, the agent's argv) then shrinks to the secrets actually
    // used, so the guest never blocks on a secret no service consumes. The legacy
    // single-service build is untouched (its sole workload gets every secret).
    if let Some(svcs) = services.as_ref() {
        let scoped: std::collections::BTreeSet<&str> = svcs
            .iter()
            .flat_map(|s| s.env_map.values())
            .map(|s| s.as_str())
            .collect();
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

    // Phase 7 (generated internal bindings): resolve `[generated_bindings.*]`.
    // Fail-closed and it INJECTS each generated value's env var into every target
    // service's `env_map` (so the guest reads the tmpfs file the guest-agent
    // materializes at run) and returns the value-free build specs for the receipt
    // + supervisor.json. Requires a multi-service build — the value is injected
    // into NAMED target services.
    let generated_bindings = derive_generated_bindings(m, services.as_mut())?;

    // app_url selection: the sole public service (exactly one, enforced in derive)
    // is the app_url / ready_url target. Recorded in the receipt; None for legacy.
    let public_service = services
        .as_ref()
        .and_then(|svcs| svcs.iter().find(|s| s.public).map(|s| s.name.clone()));
    spec.supervisor = Some(SupervisorBuildSpec {
        binding_names,
        env_map,
        services,
        public_service,
        generated_bindings,
    });
    Ok(spec)
}

/// The env var each target service uses for a generated binding: the
/// UPPERCASED, sanitized binding name (`db_password` → `DB_PASSWORD`,
/// `session.key` → `SESSION_KEY`). A single convention keeps the recipe terse;
/// the derived name is validated as a POSIX identifier before injection.
fn generated_env_var(binding_name: &str) -> String {
    binding_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Phase 7 (generated internal bindings): resolve `[generated_bindings.<name>]`
/// into value-free build specs AND inject each generated value's env var into
/// every target service's `env_map`. Fail-closed at emission (mirrors the guest
/// agent's own `SupervisorConfig::validate`, which never trusts the builder):
/// each name must be a valid [`BindingName`] (the tmpfs filename), the derived
/// env var a POSIX identifier, `bytes` bounded, targets non-empty and every one a
/// DECLARED service, and the injected env var must not collide with a secret /
/// port env the service already carries. Generated bindings require a
/// multi-service build (the value is injected into named target services).
fn derive_generated_bindings(
    m: &CapsuleManifest,
    services: Option<&mut Vec<ServiceBuildSpec>>,
) -> Result<Vec<GeneratedBindingBuildSpec>, String> {
    use capsule::foundation::types::ready_state::{GeneratedBindingScope, GeneratedGenerator};
    if m.generated_bindings.is_empty() {
        return Ok(Vec::new());
    }
    let Some(services) = services else {
        return Err(
            "[generated_bindings] requires a multi-service capsule ([services.*]) — the \
             generated value is injected into named target services"
                .into(),
        );
    };
    // Own the names so the immutable borrow of `services` is released before the
    // per-target env injection mutates `services` below.
    let declared: std::collections::BTreeSet<String> =
        services.iter().map(|s| s.name.clone()).collect();
    // Deterministic order (BTreeMap by name) so supervisor.json + the receipt are
    // reproducible from the manifest alone.
    let mut out = Vec::with_capacity(m.generated_bindings.len());
    for (name, spec) in &m.generated_bindings {
        if let Err(e) = BindingName::parse(name.as_str()) {
            return Err(format!(
                "generated binding '{name}': the name is the tmpfs binding filename and must be a \
                 valid BindingName ({e})"
            ));
        }
        if !(1..=1024).contains(&spec.bytes) {
            return Err(format!(
                "generated binding '{name}': bytes must be 1..=1024 (got {})",
                spec.bytes
            ));
        }
        if spec.targets.is_empty() {
            return Err(format!(
                "generated binding '{name}': at least one target service is required (nothing \
                 would consume the value)"
            ));
        }
        let env_var = generated_env_var(name);
        if !valid_env_var_name(&env_var) {
            return Err(format!(
                "generated binding '{name}': derived env var {env_var:?} is not a POSIX identifier"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for target in &spec.targets {
            if !declared.contains(target.as_str()) {
                return Err(format!(
                    "generated binding '{name}': target '{target}' is not a declared service"
                ));
            }
            if !seen.insert(target.as_str()) {
                return Err(format!(
                    "generated binding '{name}': duplicate target '{target}'"
                ));
            }
        }
        // Inject ENV_VAR → binding name into each target's secret injection map
        // (same `bindings_env` mechanism as a leased secret). NOT added to
        // `binding_names`: a generated binding is not leased, so the guest must
        // never WAIT for it — the guest-agent materializes it at run instead.
        for svc in services.iter_mut() {
            if !spec.targets.iter().any(|t| t == &svc.name) {
                continue;
            }
            if svc.env_map.contains_key(&env_var) || svc.base_env.contains_key(&env_var) {
                return Err(format!(
                    "generated binding '{name}': injected env {env_var:?} collides with an \
                     existing env var in service '{}'",
                    svc.name
                ));
            }
            svc.env_map.insert(env_var.clone(), name.clone());
        }
        out.push(GeneratedBindingBuildSpec {
            name: name.clone(),
            generator: match spec.generator {
                GeneratedGenerator::RandomBase64 => "random_base64".to_string(),
            },
            bytes: spec.bytes,
            scope: match spec.scope {
                GeneratedBindingScope::Run => "run".to_string(),
            },
            targets: spec.targets.clone(),
        });
    }
    Ok(out)
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
            // Phase 6 (service DAG): which declared services are one-shot tasks —
            // a dependent WAITS FOR EXIT 0 of a run_once dep (depends_on_success),
            // and for READINESS of a long-running dep (legacy depends_on).
            let run_once_names: std::collections::BTreeSet<&str> = services
                .iter()
                .filter(|s| s.run_once)
                .map(|s| s.name.as_str())
                .collect();
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
                    if s.run_once {
                        // One-shot: runs during the BUILD boot before the
                        // pre-seal snapshot; sealed memory carries its effects
                        // into every restore.
                        obj["kind"] = serde_json::json!("run_once");
                        obj["run_at"] = serde_json::json!(["seal_once"]);
                    }
                    if !s.depends_on.is_empty() {
                        let (success, ready): (Vec<&String>, Vec<&String>) = s
                            .depends_on
                            .iter()
                            .partition(|d| run_once_names.contains(d.as_str()));
                        if !ready.is_empty() {
                            obj["depends_on"] = serde_json::json!(ready);
                        }
                        if !success.is_empty() {
                            obj["depends_on_success"] = serde_json::json!(success);
                        }
                    }
                    // Readiness (so a dependent can WAIT): emit when the service has a
                    // determinable PRIMARY port (public = the proxied target port;
                    // internal = its first resolved expose port or literal env.PORT).
                    // The guest probes 127.0.0.1:<port> (plus the HTTP path when
                    // declared). No port ⇒ "ready once started" (no readiness block).
                    if let Some(rport) = s.port
                        && !s.run_once
                    {
                        let mut r = serde_json::json!({ "port": rport });
                        if let Some(path) = &s.readiness_http_path {
                            r["http_path"] = serde_json::json!(path);
                        }
                        obj["readiness"] = r;
                    }
                    obj
                })
                .collect();
            let mut obj = serde_json::json!({ "services": svc_json });
            // v1.6 (ato#983) Slice 3: durable state volumes are VM-WIDE — the
            // guest-agent mounts every one of them once at boot, before any
            // service starts (not tied to a particular service's lifecycle) —
            // so this is a top-level array, mirroring the guest's own
            // `SupervisorConfig.volumes` shape, not nested under a service.
            // Flattened across every service and ordered by drive_id (already
            // assigned in one global, deterministic pass above).
            let mut volumes: Vec<&DurableVolumeBuildSpec> =
                services.iter().flat_map(|s| &s.volumes).collect();
            volumes.sort_by(|a, b| a.drive_id.cmp(&b.drive_id));
            if !volumes.is_empty() {
                obj["volumes"] = serde_json::json!(
                    volumes
                        .iter()
                        .map(|v| serde_json::json!({
                            "state_name": v.state_name,
                            "target": v.target,
                            "drive_id": v.drive_id,
                            "fs_label": v.fs_label,
                            "size_mb": v.size_mb,
                        }))
                        .collect::<Vec<_>>()
                );
            }
            emit_generated_bindings(&mut obj, sup);
            obj
        }
        None => {
            let mut obj = serde_json::json!({
                "cmd": ["/bin/sh", "-lc", start_cmd],
                "cwd": "/app",
                "base_env": { "PORT": port.to_string() },
                "bindings_env": sup.env_map,
            });
            emit_generated_bindings(&mut obj, sup);
            obj
        }
    }
}

/// Phase 7 (generated internal bindings): emit the value-free `generated_bindings`
/// array into `supervisor.json` (mirrors the guest's `SupervisorConfig.
/// generated_bindings`, a top-level array). Only the SPEC — no value.
fn emit_generated_bindings(obj: &mut serde_json::Value, sup: &SupervisorBuildSpec) {
    if sup.generated_bindings.is_empty() {
        return;
    }
    obj["generated_bindings"] = serde_json::json!(
        sup.generated_bindings
            .iter()
            .map(|g| serde_json::json!({
                "name": g.name,
                "generator": g.generator,
                "bytes": g.bytes,
                "scope": g.scope,
                "targets": g.targets,
            }))
            .collect::<Vec<_>>()
    );
}

/// A service name: 1–63 chars of lowercase `[a-z0-9-]`, not leading/trailing `-`
/// (it keys per-service logs and may become an in-guest DNS label).
fn valid_service_name(name: &str) -> bool {
    let ok_len = (1..=63).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
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
        run_once: bool,
        author_env: BTreeMap<String, String>,
        env_map: BTreeMap<String, String>,
        public: bool,
        depends_on: Vec<String>,
        aliases: Vec<String>,
        readiness_http_path: Option<String>,
        expose: Vec<String>,
        literal_port: Option<u16>,
        volumes: Vec<DurableVolumeBuildSpec>,
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
        // Phase 6: a one-shot task is waited on for EXIT, not readiness, and can
        // never be the public web service (it exits).
        if svc.run_once {
            if svc.readiness_probe.is_some() {
                return Err(format!(
                    "service '{name}': `run_once` and `readiness_probe` are contradictory                      (a one-shot task is waited on for exit 0, not readiness)"
                ));
            }
            if svc.network.as_ref().is_some_and(|n| n.publish) {
                return Err(format!(
                    "service '{name}': a `run_once` task cannot be the public service                      (it exits; nothing would serve the proxied port)"
                ));
            }
        }
        // v1.6 (ato#983): derive this service's durable state volumes.
        // Defense-in-depth — the manifest validator (`manifest_validation.rs`)
        // already runs these exact checks (via the same shared
        // `capsule::foundation::types::manifest` helpers), but the builder never
        // trusts that validation ran; a config error here still fails closed
        // instead of silently building an unmountable/oversized volume.
        let mut volumes: Vec<DurableVolumeBuildSpec> = Vec::new();
        // Review fix (ato#991): the existing cross-service check below only
        // fires when the SAME state_name is bound by a DIFFERENT service
        // (`prev != r.name`) — the same service binding the same state_name
        // twice was silently accepted, producing two `DurableVolumeBuildSpec`
        // entries that would collide on the SAME drive_id/fs_label in the
        // global assignment pass (both keyed by that one state_name), baking
        // an ambiguous/unmountable `supervisor.json`. Reject it here, at
        // derive time, before it can reach that pass.
        let mut bound_states_this_service = std::collections::BTreeSet::new();
        for binding in &svc.state_bindings {
            let state_name = binding.state.trim();
            if !bound_states_this_service.insert(state_name.to_string()) {
                return Err(format!(
                    "service '{name}': state '{state_name}' is bound more than once"
                ));
            }
            if binding.service_target.is_some() {
                return Err(format!(
                    "service '{name}': `state_bindings.service_target` is not applicable to a \
                     single-VM snapshot service (mounts are VM-wide, not per-container); binding \
                     for state '{state_name}'"
                ));
            }
            let requirement = m.state.get(state_name).ok_or_else(|| {
                format!("service '{name}': state '{state_name}' is not declared under [state]")
            })?;
            if requirement.durability != StateDurability::Persistent {
                return Err(format!(
                    "service '{name}': state '{state_name}' must have durability=\"persistent\" to \
                     be used as a snapshot durable-volume binding"
                ));
            }
            if requirement.sharing == StateSharing::SameCapsule {
                return Err(format!(
                    "service '{name}': state '{state_name}' has sharing=\"same-capsule\", not \
                     supported for a snapshot durable-volume binding in this release"
                ));
            }
            let size_mb = validate_state_volume_size_mb(state_name, requirement.size_mb)
                .map_err(|e| format!("service '{name}': {e}"))?;
            let target = validate_and_normalize_state_mount_target(&binding.target)
                .map_err(|e| format!("service '{name}': {e}"))?;
            // drive_id/fs_label are placeholders here — assigned in one global
            // (cross-service) pass below, sorted by state_name across the
            // WHOLE capsule, to match firecracker.rs's attach-time ordering.
            volumes.push(DurableVolumeBuildSpec {
                state_name: state_name.to_string(),
                target,
                size_mb,
                drive_id: String::new(),
                fs_label: String::new(),
            });
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
                return Err(format!(
                    "service '{name}': depends_on '{dep}' is not a declared service"
                ));
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
                return Err(format!(
                    "service '{name}': env var {k:?} is not a POSIX identifier"
                ));
            }
            base_env.insert(k, v);
        }
        // The runner proxies exactly ONE guest port — the build target's `port`. The
        // PUBLIC service MUST listen there, so a service-declared `PORT` that differs
        // is a config error (build would succeed, restore would succeed, but the
        // proxy would front a port nothing is on → false/never ready). Fail closed
        // rather than silently honour the target port and ignore the author's `PORT`.
        // (Injected below in build_supervisor_json when absent.)
        if public
            && let Some(declared) = base_env.get("PORT")
            && declared != &target_port.to_string()
        {
            return Err(format!(
                "public service '{name}': env PORT = {declared:?} but the build target \
                 port is {target_port} — the public service must listen on the single \
                 proxied port. Drop the explicit PORT (it is injected) or set it to {target_port}"
            ));
        }
        // DNS-safe aliases (they may become in-guest DNS labels in the aliasing slice).
        let aliases = svc
            .network
            .as_ref()
            .map(|n| n.aliases.clone())
            .unwrap_or_default();
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
            run_once: svc.run_once,
            author_env: base_env,
            env_map,
            public,
            depends_on,
            aliases,
            readiness_http_path,
            expose,
            literal_port,
            volumes,
        });
    }

    // v1.6 (ato#983): cross-service durable-state checks, defense-in-depth
    // alongside the manifest validator's equivalent (which may not have run —
    // this builder never trusts that it did). A state name must have exactly
    // one owning service, and two volumes' mount targets must never be
    // identical or nested under one another (both would make a later slice's
    // mount step try to mount two devices onto/under the same directory).
    {
        let mut state_owner: BTreeMap<String, String> = BTreeMap::new();
        let mut all_targets: Vec<(String, String, String)> = Vec::new();
        for r in &collected {
            for v in &r.volumes {
                if let Some(prev) = state_owner.insert(v.state_name.clone(), r.name.clone())
                    && prev != r.name
                {
                    return Err(format!(
                        "state '{}' is bound by both service '{prev}' and '{}' — a snapshot \
                         durable-volume binding has exactly one owning service",
                        v.state_name, r.name
                    ));
                }
                all_targets.push((r.name.clone(), v.state_name.clone(), v.target.clone()));
            }
        }
        for i in 0..all_targets.len() {
            for j in (i + 1)..all_targets.len() {
                let (service_a, state_a, target_a) = &all_targets[i];
                let (service_b, state_b, target_b) = &all_targets[j];
                let path_a = Path::new(target_a);
                let path_b = Path::new(target_b);
                if path_a == path_b || path_a.starts_with(path_b) || path_b.starts_with(path_a) {
                    return Err(format!(
                        "target '{target_a}' (service '{service_a}', state '{state_a}') conflicts \
                         with target '{target_b}' (service '{service_b}', state '{state_b}') — \
                         durable mount targets must not be identical or nested under one another"
                    ));
                }
            }
        }
    }

    // v1.6 (ato#983) Slice 3: assign each volume's drive_id + fs_label in ONE
    // GLOBAL pass — sorted by state_name across every service in the capsule,
    // exactly the same rule `state_volume::prepare_volumes` (firecracker.rs,
    // Slice 2) applies to the flattened cross-service list it attaches at
    // build/restore time. Computing it here, from the manifest alone (no
    // host/runtime input), guarantees the id/label BAKED into supervisor.json
    // below is the SAME one the host actually attaches — the two are never
    // computed from a different ordering rule that could drift apart.
    if collected.iter().any(|r| !r.volumes.is_empty()) {
        let owner_scope = m.persistent_state_owner_scope().ok_or_else(|| {
            "capsule declares durable state volumes but has no name/state_owner_scope to derive \
             a stable identity from"
                .to_string()
        })?;
        let mut state_names: Vec<String> = collected
            .iter()
            .flat_map(|r| &r.volumes)
            .map(|v| v.state_name.clone())
            .collect();
        state_names.sort();
        let index_of: BTreeMap<String, usize> = state_names
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, i))
            .collect();
        for r in &mut collected {
            for v in &mut r.volumes {
                let i = index_of[&v.state_name];
                v.drive_id = state_drive_id(i);
                v.fs_label = volume_label(&owner_scope, &v.state_name);
            }
        }
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
                return Err(format!(
                    "service '{}': duplicate expose placeholder {ph:?}",
                    r.name
                ));
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
            next = next
                .checked_add(1)
                .ok_or("ran out of ports allocating service expose placeholders")?;
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
            let port = if r.public && i == 0 {
                target_port
            } else {
                alloc()?
            };
            resolved_ports.insert((r.name.clone(), ph.clone()), port);
            if i == 0 {
                primary.entry(r.name.clone()).or_insert(port);
            }
        }
        // Primary fallback: a public service always listens on the target port; an
        // internal service with no expose but a literal env.PORT uses that.
        if r.public {
            primary.insert(r.name.clone(), target_port);
        } else if !primary.contains_key(&r.name)
            && let Some(p) = r.literal_port
        {
            primary.insert(r.name.clone(), p);
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
            base_env
                .entry("PORT".to_string())
                .or_insert_with(|| target_port.to_string());
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
            run_once: r.run_once,
            cmd: r.cmd.clone(),
            cwd: "/app".into(),
            base_env,
            env_map: r.env_map.clone(),
            public: r.public,
            depends_on: r.depends_on.clone(),
            aliases: r.aliases.clone(),
            readiness_http_path: r.readiness_http_path.clone(),
            port: primary.get(&r.name).copied(),
            volumes: r.volumes.clone(),
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
const RESERVED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "ip6-localhost",
    "ip6-loopback",
];

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
    format!("127.0.0.1 localhost {joined}\n::1 localhost ip6-localhost ip6-loopback\n")
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
    let ok_chars = owner
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_alphanumeric());
    ok_len && ok_chars && ends(owner.bytes().next()) && ends(owner.bytes().next_back())
}

/// A conservative GitHub **repo** name: 1–100 chars of `[A-Za-z0-9._-]`, excluding the
/// pathological `.` / `..`.
pub fn valid_github_repo(repo: &str) -> bool {
    let ok_len = (1..=100).contains(&repo.len());
    let ok_chars = repo
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    ok_len && ok_chars && repo != "." && repo != ".."
}

/// Validate a relative `subdir` **before** it is joined to the checkout: reject absolute
/// paths, any `..` component, and non-normal components (root/prefix). The canonical
/// containment check after checkout closes symlink traversal.
///
/// `subdir` names a path inside a cloned git repository — always POSIX-style,
/// regardless of the platform this builder itself runs on. Parsed manually as
/// a plain string rather than via `std::path::Path`/`Component`: on Windows,
/// `Path::new("/etc").is_absolute()` is FALSE (Windows absolute paths need a
/// drive letter/UNC prefix) while `.components()` still yields a rooted
/// component — the two host-platform-dependent checks disagree with each
/// other and with POSIX semantics, wrongly admitting `/etc` past the
/// "must be relative" gate on a non-Linux build host.
pub(crate) fn validate_subdir(subdir: &str) -> Result<(), String> {
    if subdir.starts_with('/') {
        return Err(format!("subdir {subdir:?} must be relative"));
    }
    for component in subdir.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(format!("subdir {subdir:?} may not contain '..'")),
            _ => {}
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
pub fn materialize_source(
    owner: &str,
    repo: &str,
    commit: &str,
    subdir: Option<&str>,
    manifest_override: Option<&str>,
    dest: &Path,
) -> Result<PathBuf, String> {
    validate_source_identity(owner, repo, commit)?;
    if let Some(s) = subdir.filter(|s| !s.is_empty()) {
        validate_subdir(s)?;
    }
    git_checkout_pinned(owner, repo, commit, dest)?;

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

/// SOURCE_MATERIALIZATION_SPEC step 1: shallow-checkout the pinned public commit and
/// return the (optionally sub-directoried) source root **without** requiring or writing
/// a `capsule.toml`. The `source_materialize` job freezes the repo tree exactly as it
/// is (there is no recipe to apply and no manifest to resolve), so unlike
/// [`materialize_source`] it neither demands the repo carry a `capsule.toml` nor writes
/// one. Same identity validation, full-SHA pin, and lexical+canonical containment as the
/// recipe lane — it reuses the same [`git_checkout_pinned`] + [`contained_source_root`]
/// helpers, only passing `require_manifest = false`.
///
/// The returned root carries no `.git` ([`remove_checkout_git_metadata`]): the archive
/// this feeds must hash reproducibly and, under ADR-014 §1, a working tree is not a
/// pinned source materialization.
pub fn checkout_source_tree(
    owner: &str,
    repo: &str,
    commit: &str,
    subdir: Option<&str>,
    dest: &Path,
) -> Result<PathBuf, String> {
    validate_source_identity(owner, repo, commit)?;
    if let Some(s) = subdir.filter(|s| !s.is_empty()) {
        validate_subdir(s)?;
    }
    git_checkout_pinned(owner, repo, commit, dest)?;
    contained_source_root(dest, subdir, false)
}

/// Validate the server-resolved source identity as an input boundary: `owner`/`repo`
/// as conservative GitHub identities and `commit` as a pinned 40-hex sha (never a
/// branch/tag). Shared by [`materialize_source`] and [`checkout_source_tree`] so both
/// lanes enforce the same fail-closed gate before any network use.
fn validate_source_identity(owner: &str, repo: &str, commit: &str) -> Result<(), String> {
    if !valid_github_owner(owner) {
        return Err(format!("invalid github owner {owner:?}"));
    }
    if !valid_github_repo(repo) {
        return Err(format!("invalid github repo {repo:?}"));
    }
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "refusing non-pinned commit {commit:?} (need a full 40-char sha)"
        ));
    }
    Ok(())
}

/// Shallow-clone `owner/repo` and check out the pinned `commit` into `dest`. The pure
/// git steps shared by [`materialize_source`] (recipe lane) and [`checkout_source_tree`]
/// (source_materialize lane); callers validate the identity (`validate_source_identity`)
/// and any subdir before calling, and resolve/contain the source root afterward.
///
/// The checkout's own `.git` is removed before returning ([`remove_checkout_git_metadata`]),
/// so what every caller receives is a materialized source tree, never a working tree.
fn git_checkout_pinned(owner: &str, repo: &str, commit: &str, dest: &Path) -> Result<(), String> {
    let url = format!("https://github.com/{owner}/{repo}.git");
    let run = |args: &[&str], cwd: Option<&Path>| -> Result<(), String> {
        let mut c = Command::new("git");
        c.args(args);
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        let out = c.output().map_err(|e| format!("git {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    };
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    run(&["init", "-q"], Some(dest))?;
    run(&["remote", "add", "origin", &url], Some(dest))?;
    run(
        &["fetch", "-q", "--depth", "1", "origin", commit],
        Some(dest),
    )?;
    run(&["checkout", "-q", "FETCH_HEAD"], Some(dest))?;
    remove_checkout_git_metadata(dest)
}

/// Delete the `.git` that [`git_checkout_pinned`]'s `git init` created, turning the
/// checkout into a plain materialized source tree. Fail-closed: an IO error here is an
/// error, never a silently retained working tree.
///
/// Removing it is required, not hygiene:
///
/// * `.git` content is **not reproducible** — `.git/index` records per-file stat data
///   (inode, mtime), so two checkouts of the same commit on the same host differ. A1v2
///   (`materialized_source_tree_hash`) hashes a ROOT `.git` as an ordinary directory
///   (only a NESTED one is a submodule signal), so leaving it made an identity-bearing
///   value — `ExecutionContractV1.source.digest`, and the `source_materialize` job's
///   reported tree hash — depend on when and where the checkout ran. The subdir case hid
///   this; `subdirectory` is optional, so the no-subdir case is reachable by design.
/// * ADR-014 §1 refuses a root-level `.git` of ANY node type as a pinned source
///   materialization, so a `.tar.zst` frozen from such a tree can never yield a
///   `capsule_program_id`.
/// * The recipe lane `cp -a`s this tree into the rootfs build context, so `.git` would
///   also bloat the image and ship repo metadata into the guest.
///
/// Only the ROOT entry is removed. A NESTED `.git` stays: A1v2 rejects it as a submodule
/// / embedded-repo signal, and stripping it would hide that. A `--depth 1` fetch +
/// `checkout` without `--recurse-submodules` never creates one — a gitlink materializes
/// as an empty directory — so this is a fail-closed invariant, not a case to clean up.
///
/// Same shape as the CLI's GitHub import path, which already removes `.git` after its
/// own pinned checkout before hashing (`crates/cli/src/cli/dispatch/import_cmd.rs`), and
/// what `capsule::source_identity::materialized_tree_hash` has always demanded of its
/// callers ("callers must remove `.git` metadata before invoking this function").
///
/// **Cache invalidation:** a no-subdir source archived or hashed before this change gets
/// a DIFFERENT A1v2 digest afterwards. Those digests were never reproducible in the first
/// place — that is the defect — so a digest change here is the fix landing, not drift.
fn remove_checkout_git_metadata(dest: &Path) -> Result<(), String> {
    let git = dest.join(".git");
    let meta = match std::fs::symlink_metadata(&git) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("inspect {}: {e}", git.display())),
    };
    // `git init` writes a directory; a gitfile/symlink `.git` is not something this
    // helper's own checkout produces, but ADR-014 rejects every node type, so remove
    // whatever is there rather than leaving a shape the gate would refuse.
    if meta.file_type().is_dir() {
        std::fs::remove_dir_all(&git)
    } else {
        std::fs::remove_file(&git)
    }
    .map_err(|e| format!("remove checkout git metadata {}: {e}", git.display()))
}

/// Resolve `dest`/`subdir` to a source root that is provably **inside** the checkout.
/// Validates the subdir lexically, then canonicalizes both paths and requires containment
/// (closing symlink traversal). `require_manifest` demands a repo `capsule.toml` at the
/// root (the raw-GitHub path); recipe-manifest jobs pass `false` and write the approved
/// recipe there instead (#932). Split out so the containment logic is unit-testable
/// without a network clone.
pub(crate) fn contained_source_root(
    dest: &Path,
    subdir: Option<&str>,
    require_manifest: bool,
) -> Result<PathBuf, String> {
    if let Some(s) = subdir.filter(|s| !s.is_empty()) {
        validate_subdir(s)?;
    }
    let root = match subdir.filter(|s| !s.is_empty()) {
        Some(s) => dest.join(s),
        None => dest.to_path_buf(),
    };
    let dest_canon = dest
        .canonicalize()
        .map_err(|e| format!("canonicalize checkout: {e}"))?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("resolved source root {} not found: {e}", root.display()))?;
    if !root_canon.starts_with(&dest_canon) {
        return Err(format!(
            "subdir escapes the checkout: {} is outside {}",
            root_canon.display(),
            dest_canon.display()
        ));
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
pub fn build_rootfs(
    source_dir: &Path,
    spec: &RootfsBuildSpec,
    out_ext4: &Path,
    size_mib: u64,
) -> Result<RootfsReceipt, String> {
    let script = build_rootfs_script(spec, size_mib);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("ATO_SRC", source_dir)
        .env("ATO_OUT", out_ext4)
        .output()
        .map_err(|e| format!("spawn rootfs build: {e}"))?;
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("rootfs build failed: {tail}"));
    }
    let rootfs_bytes = std::fs::metadata(out_ext4)
        .map_err(|e| e.to_string())?
        .len();
    Ok(RootfsReceipt {
        spec: spec.clone(),
        rootfs_path: out_ext4.display().to_string(),
        rootfs_bytes,
    })
}

/// Reject NUL bytes and line breaks in a value interpolated into a builder-generated
/// script (manifest-derived commands, import readiness paths). A newline could escape
/// the single-quoting / heredoc delimiter — or a `#` comment — and run on the builder
/// host. Pub so the snapshot-builder daemon applies the same gate to job params
/// (ato#1002).
pub fn reject_control_chars(label: &str, value: &str) -> Result<(), String> {
    if value.contains('\0') {
        return Err(format!("{label} contains a NUL byte"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(format!(
            "{label} contains a newline (single-line value required)"
        ));
    }
    Ok(())
}

/// Wrap `s` as a single POSIX-shell single-quoted argument (`abc'def` → `'abc'\''def'`),
/// so a manifest-derived command is passed as ONE literal argument to `/bin/sh -lc`,
/// never re-parsed. Combined with quoted heredocs, capsule commands can never be expanded
/// by the builder-host shell.
pub(crate) fn shell_single_quote(s: &str) -> String {
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
pub(crate) fn build_rootfs_script(spec: &RootfsBuildSpec, size_mib: u64) -> String {
    let install_q = shell_single_quote(spec.install_cmd.as_deref().unwrap_or("true"));
    let build_q = shell_single_quote(spec.build_cmd.as_deref().unwrap_or("true"));

    let (agent_prep, launch) =
        supervisor_prep_and_launch(spec.supervisor.as_ref(), spec.port, &spec.start_cmd);
    // The legacy acquire step: copy the materialized source into the build dir and
    // assemble the app image from a GENERATED Dockerfile. The Docker-import path
    // (ato#994) replaces exactly this step with an already-built imported image —
    // everything downstream (create → export → inject → init → pack) is shared via
    // `rootfs_pack_script`, emitted byte-identically for this legacy path.
    let acquire = format!(
        r#"cp -a "$ATO_SRC/." "$BUILD/"
# QUOTED heredoc: no host expansion; commands run inside Docker RUN via sh -lc '<literal>'.
cat > "$BUILD/Dockerfile" <<'DOCKER'
FROM {base}
WORKDIR /app
COPY . /app
RUN /bin/sh -lc {install_q}
RUN /bin/sh -lc {build_q}
DOCKER
docker build -q -t "$TAG" "$BUILD" >/dev/null
"#,
        base = spec.base_image,
        install_q = install_q,
        build_q = build_q,
    );
    rootfs_pack_script(&PackScriptInputs {
        tool: "docker",
        tag_init: "TAG=\"ato-rootfs-$$\"".into(),
        acquire,
        agent_prep,
        launch,
        init_cwd: "/app",
        port: spec.port,
        healthcheck: spec.healthcheck.clone(),
        size_mib,
        // Legacy build: no VOLUME mapping / no relay — empty keeps the emitted
        // script byte-identical to the pre-ato#1024/#1026 template.
        extra_mounts: String::new(),
        extra_prelaunch: String::new(),
    })
}

/// v1.2 supervisor staging + launch lines, shared between the legacy
/// generated-Dockerfile path and the Docker-import path (ato#994): init runs the
/// guest-agent (which starts the workload with the composed env after bindings
/// arrive) instead of launching the app; the agent binary +
/// /etc/ato/supervisor.json are staged into the rootfs. `agent_prep` runs after
/// the image export; `launch` replaces the direct app launch. Both are empty/
/// direct-launch for the v1.0 no-binding path, so that script stays
/// byte-identical.
pub(crate) fn supervisor_prep_and_launch(
    supervisor: Option<&SupervisorBuildSpec>,
    port: u16,
    start_cmd: &str,
) -> (String, String) {
    let start_q = shell_single_quote(start_cmd);
    match supervisor {
        None => (
            String::new(),
            format!("/bin/sh -lc {start_q} >/tmp/app.log 2>&1 &"),
        ),
        Some(sup) => {
            // supervisor.json (no secret value — env var → binding name only).
            let cfg = build_supervisor_json(sup, port, start_cmd);
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
            // v1.6 (ato#983) Slice 3 fix: the rootfs is mounted READ-ONLY at
            // boot (Ready-State's whole model), so the guest-agent's
            // `mkdir -p <target>` before mounting a durable volume can NEVER
            // create a NEW directory there — only reuse one that already
            // exists. Every declared durable-volume target must therefore be
            // baked into the rootfs at BUILD time, here, alongside the other
            // static paths (`/etc/ato`, `/run/ato/bindings`) already staged
            // below. `target` is quoted as `"$BUILD"'<literal>'` (variable
            // expansion for `$BUILD` only, everything else single-quoted) —
            // never double-quoted-and-interpolated — because a manifest
            // author's `state_bindings.target` is validated as a clean path
            // shape (components, no `.`/`..`, under `/ato/state/`) but NOT
            // for shell metacharacters within a component; single-quoting is
            // what actually closes that off, matching the existing
            // `reject_control_chars` discipline applied to run/build commands
            // elsewhere in this same builder.
            let state_dirs_prep = {
                let mut targets: Vec<&str> = sup
                    .services
                    .iter()
                    .flatten()
                    .flat_map(|s| &s.volumes)
                    .map(|v| v.target.as_str())
                    .collect();
                targets.sort_unstable();
                targets.dedup();
                if targets.is_empty() {
                    String::new()
                } else {
                    let args: Vec<String> = targets
                        .iter()
                        .map(|t| {
                            format!("\"$BUILD\"{}", shell_single_quote(&format!("/rootfs{t}")))
                        })
                        .collect();
                    format!("\nmkdir -p {}", args.join(" "))
                }
            };
            let prep = format!(
                r#"# v1.2 supervisor: stage the guest-agent + its config into the rootfs.
: "${{ATO_GUEST_AGENT_BIN:?ATO_GUEST_AGENT_BIN must point to the guest-agent binary for a supervisor build}}"
mkdir -p "$BUILD/rootfs/usr/local/bin" "$BUILD/rootfs/etc/ato" "$BUILD/rootfs/run/ato/bindings"
cp "$ATO_GUEST_AGENT_BIN" "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
chmod 0755 "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
cat > "$BUILD/rootfs/etc/ato/supervisor.json" <<'ATOSUPERVISORJSON'
{cfg_json}
ATOSUPERVISORJSON{hosts_prep}{state_dirs_prep}"#
            );
            // The agent is the supervisor: vsock control plane on 1025, required
            // bindings as argv. It reads /etc/ato/supervisor.json and starts the app
            // only once every binding is delivered (bound-ready).
            let launch = format!(
                "mkdir -p /run/ato/bindings\n\
                 export ATO_GUEST_AGENT_MODE=vsock ATO_GUEST_AGENT_VSOCK_PORT=1025 ATO_BINDINGS_ROOT=/run/ato/bindings\n\
                 /usr/local/bin/ato-guest-agent {args} 2>&1 | tee /tmp/agent.log > /dev/console &"
            );
            (prep, launch)
        }
    }
}

/// Inputs for the shared export→inject→init→pack pipeline. `acquire` is the
/// only step that differs between the legacy generated-Dockerfile build
/// (cp source + heredoc Dockerfile + `docker build`) and the Docker-import
/// path (image already built — empty acquire, literal `TAG`).
pub(crate) struct PackScriptInputs<'a> {
    /// Container CLI to drive: `docker` (legacy builder hosts) or `podman`.
    pub tool: &'a str,
    /// The `TAG=…` line: process-unique for legacy, the imported tag for import.
    pub tag_init: String,
    /// Image-acquisition section (may be empty). Must end with a newline when
    /// non-empty — it sits between `trap cleanup EXIT` and `CID=$(… create …)`.
    pub acquire: String,
    pub agent_prep: String,
    pub launch: String,
    /// Init's working directory before launch: `/app` for legacy builds (the
    /// generated Dockerfile put the app there), `/` for imported images (their
    /// own WORKDIR is honored per-service via supervisor.json `cwd`).
    pub init_cwd: &'a str,
    pub port: u16,
    pub healthcheck: String,
    pub size_mib: u64,
    /// Extra mount lines rendered into init after the standard tmpfs mounts
    /// (ato#1024 VOLUME→tmpfs mapping). MUST be empty for the legacy build
    /// (keeps its emitted script byte-identical) and each non-empty line must
    /// already be shell-safe — paths are validated fail-closed upstream
    /// (`validate_tmpfs_volume_path`), never escaped here.
    pub extra_mounts: String,
    /// Extra init lines rendered AFTER `cd <init_cwd>` and BEFORE the launch
    /// (ato#1026 localhost relay). MUST be empty for the legacy build (keeps
    /// its emitted script byte-identical); content is generated, never a
    /// user-controlled literal.
    pub extra_prelaunch: String,
}

/// The bash pipeline that turns an app image into a read-only-bootable ext4:
/// (acquire →) create → export → inject agent/config → install init → pack.
/// Kept as a reviewable string; env: ATO_SRC (legacy acquire only), ATO_OUT.
/// See `build_rootfs_script` for the legacy assembly (emitted byte-identically
/// to the pre-#994 single template) and `docker_import::rootfs` for the import
/// assembly.
pub(crate) fn rootfs_pack_script(i: &PackScriptInputs<'_>) -> String {
    format!(
        r#"set -euo pipefail
{tag_init}
CID=""
MNT=""
BUILD=$(mktemp -d)
# Failure-safe cleanup: on ANY exit (success or a failed build/export/mount/cp) leave no
# container, image, mount, or temp dir behind (Phase 8 orphan-hardening parity).
cleanup() {{
  [ -n "$CID" ] && {tool} rm -f "$CID" >/dev/null 2>&1 || true
  {tool} rmi -f "$TAG" >/dev/null 2>&1 || true
  if [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null; then umount "$MNT" 2>/dev/null || umount -l "$MNT" 2>/dev/null || true; fi
  [ -n "$MNT" ] && rmdir "$MNT" 2>/dev/null || true
  [ -n "$BUILD" ] && rm -rf "$BUILD" 2>/dev/null || true
}}
trap cleanup EXIT
{acquire}CID=$({tool} create "$TAG")
mkdir -p "$BUILD/rootfs"
{tool} export "$CID" | tar -x -C "$BUILD/rootfs"
{tool} rm -f "$CID" >/dev/null; CID=""
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
{extra_mounts}cd {init_cwd}
{extra_prelaunch}{launch}
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
        tag_init = i.tag_init,
        tool = i.tool,
        acquire = i.acquire,
        agent_prep = i.agent_prep,
        launch = i.launch,
        init_cwd = i.init_cwd,
        port = i.port,
        hc = i.healthcheck,
        size = i.size_mib,
        extra_mounts = i.extra_mounts,
        extra_prelaunch = i.extra_prelaunch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::foundation::types::manifest::CapsuleManifest;

    // --- v1 authoring surface ---------------------------------------------------

    const V1_MINIMAL: &str = r#"
schema_version = "1"
name = "demo"
version = "0.1.0"

[run]
command = ["python3", "app.py"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["curl", "-fsS", "http://127.0.0.1:8080/"]
"#;

    fn v1(text: &str) -> capsule::types::manifest_v1::CapsuleManifestV1 {
        capsule::types::manifest_v1::CapsuleManifestV1::from_toml(text).expect("v1 manifest parses")
    }

    fn python_probe() -> SourceProbe {
        SourceProbe {
            has_py_files: true,
            ..SourceProbe::default()
        }
    }

    #[test]
    fn a_v1_spec_resolves_the_argv_the_author_wrote() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");

        assert_eq!(spec.runtime, RuntimeKind::Python);
        assert_eq!(spec.base_image, "python:3.11-slim");
        assert_eq!(spec.port, 8080);
        // Nothing prepended, and that is a measurement rather than a gap.
        assert_eq!(
            spec.runtime_invocation_prefix,
            ObservedInvocationPrefix::observed_none()
        );
        assert!(spec.runtime_invocation_prefix.words().is_empty());
        assert_eq!(spec.resolved_argv, ["python3", "app.py"]);
    }

    /// v0.3 rewrites a bare `app.py` into `python3 app.py` because its command
    /// is a shell string that has to exec somehow. v1 argv is exact, so the same
    /// input must survive untouched — an author who wants an interpreter names
    /// one, and inventing it here would put a word into the Execution Identity
    /// that nobody wrote.
    #[test]
    fn a_v1_bare_script_argv_is_not_rewritten_the_way_v0_3_rewrites_it() {
        let bare = V1_MINIMAL.replace(r#"["python3", "app.py"]"#, r#"["app.py"]"#);
        let spec = derive_build_spec_v1(&v1(&bare), &python_probe()).expect("derives");
        assert_eq!(spec.resolved_argv, ["app.py"]);

        // The v0.3 path, same input shape, DOES rewrite it — so this is a real
        // divergence between the two surfaces, not an untested coincidence.
        let legacy = derive_build_spec(
            &CapsuleManifest::from_toml(&base_toml().replace("python3 app.py", "app.py"))
                .expect("v0.3 manifest parses"),
            &python_probe(),
        )
        .expect("derives");
        assert_eq!(legacy.start_cmd, "python3 app.py");
    }

    /// The launch line must round-trip through the guest shell to the SAME
    /// argv, including a word containing a space, a single quote, and an empty
    /// word. This is asserted by actually running `/bin/sh` over the emitted
    /// line rather than by eyeballing the quoting.
    #[cfg(unix)]
    #[test]
    fn the_emitted_launch_line_parses_back_to_the_exact_argv() {
        let argv: Vec<String> = vec![
            "python3".into(),
            "my app.py".into(),
            "it's".into(),
            String::new(),
            "--flag=a b".into(),
        ];
        let line = launch_argv_line(&argv);
        let words = line
            .strip_suffix(" >/tmp/app.log 2>&1 &")
            .expect("the launch line ends with the redirect");

        // `printf '%s\0'` writes each argument the shell parsed, NUL-separated,
        // so the boundaries are readable without guessing.
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s\\0' {words}"))
            .output()
            .expect("run /bin/sh");
        assert!(out.status.success(), "{:?}", out);
        let mut parsed: Vec<String> = String::from_utf8(out.stdout)
            .expect("utf8")
            .split('\0')
            .map(String::from)
            .collect();
        parsed.pop(); // trailing empty piece after the final NUL

        assert_eq!(parsed, argv);
    }

    /// A newline in an argv word could break out of the single quoting or the
    /// heredoc delimiter, so it is refused at derivation rather than escaped at
    /// emission.
    #[test]
    fn a_control_character_in_the_argv_is_refused() {
        let evil = V1_MINIMAL.replace(
            r#"["python3", "app.py"]"#,
            r#"["python3", "app.py\nINIT\nrm -rf /"]"#,
        );
        let error = derive_build_spec_v1(&v1(&evil), &python_probe()).expect_err("must refuse");
        assert!(error.contains("newline"), "{error}");
    }

    /// The generated init and the generated Dockerfile must agree on where the
    /// source lives — a `WORKDIR` the init does not `cd` into would launch the
    /// argv from the wrong directory while the contract recorded the other one.
    #[test]
    fn the_v1_script_puts_the_source_and_the_launch_in_the_same_directory() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let assemble = assemble_app_image_script_v1(&spec, PINNED_BASE, "docker");
        let pack = pack_app_image_script_v1(&spec, 512, "docker");

        assert!(
            assemble.contains(&format!("WORKDIR {V1_GUEST_WORKING_DIRECTORY}")),
            "{assemble}"
        );
        assert!(
            pack.contains(&format!("cd {V1_GUEST_WORKING_DIRECTORY}")),
            "{pack}"
        );
        // And it launches the argv directly — no `sh -lc` re-parsing.
        assert!(pack.contains("'python3' 'app.py' >/tmp/app.log"), "{pack}");
        assert!(
            !pack.contains("/bin/sh -lc 'python3 app.py'"),
            "the v1 init must not re-parse a joined command: {pack}"
        );
    }

    const PINNED_BASE: &str = "docker.io/library/python@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// The image is built FROM the digest-pinned reference the lane resolved,
    /// never from the tag the spec derived. A tag can move between the
    /// resolution that recorded `runtime.digest` and this build, and then the
    /// contract would name bytes the guest never ran.
    #[test]
    fn the_assembled_image_is_built_from_the_pinned_reference() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let script = assemble_app_image_script_v1(&spec, PINNED_BASE, "docker");

        assert!(script.contains(&format!("FROM {PINNED_BASE}")), "{script}");
        assert_eq!(spec.base_image, "python:3.11-slim");
        assert!(
            !script.contains("FROM python:3.11-slim"),
            "the mutable tag must not reach the Dockerfile: {script}"
        );
    }

    /// The author's own `Dockerfile` must reach the guest unchanged, and the
    /// generated one must not reach it at all.
    ///
    /// Writing the generated Dockerfile into the copied tree — which is what
    /// v0.3 does, harmlessly, because it claims nothing about the guest's
    /// contents — would overwrite an author's file and then ship Ato's
    /// three-liner to `/app`. `source.digest` would then commit a tree the
    /// guest does not have.
    #[test]
    fn the_generated_dockerfile_never_enters_the_guest() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let script = assemble_app_image_script_v1(&spec, PINNED_BASE, "docker");

        // The projection lands in a subdirectory; the Dockerfile sits beside it.
        assert!(
            script.contains(r#"cp -a "$ATO_SRC/." "$BUILD/src/""#),
            "{script}"
        );
        assert!(script.contains(r#"cat > "$BUILD/Dockerfile""#), "{script}");
        assert!(
            !script.contains(r#"cp -a "$ATO_SRC/." "$BUILD/""#),
            "the projection must not be copied to the context root, where the \
             generated Dockerfile would overwrite the author's: {script}"
        );
        // And only the projection is copied into the guest.
        assert!(
            script.contains(&format!("COPY src/. {V1_GUEST_WORKING_DIRECTORY}/")),
            "{script}"
        );
        assert!(
            !script.contains(&format!("COPY . {V1_GUEST_WORKING_DIRECTORY}")),
            "COPY . would ship the generated Dockerfile to the guest: {script}"
        );
    }

    /// Every step must drive the SAME container tool.
    ///
    /// The resolution and the measurement go through one tool's local image
    /// store; building through another's would look up a digest in a store that
    /// does not hold the image the build produced. The scripts used to hardcode
    /// `docker` while the CLI probed for `podman` first — a host with podman and
    /// no docker would have failed at the build, and a host with both would have
    /// measured one image and packed another.
    #[test]
    fn every_step_drives_the_probed_tool() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        for tool in ["docker", "podman"] {
            let assemble = assemble_app_image_script_v1(&spec, PINNED_BASE, tool);
            let pack = pack_app_image_script_v1(&spec, 512, tool);
            assert!(
                assemble.contains(&format!("{tool} build -q -t")),
                "{assemble}"
            );
            for verb in ["create", "export", "rm -f", "rmi -f"] {
                assert!(
                    pack.contains(&format!("{tool} {verb}")),
                    "{tool} {verb}: {pack}"
                );
            }
            if tool != "docker" {
                assert!(!assemble.contains("docker "), "{assemble}");
                assert!(!pack.contains("docker "), "{pack}");
            }
        }
    }

    /// The pack half must not leave a source of entropy or a clock read in the
    /// bytes the Execution Identity commits.
    ///
    /// Asserted against the emitted script because `mkfs.ext4` does not run on
    /// every host this suite does. That makes this a check on the RECIPE, not a
    /// proof of byte-equality — the proof is two real builds agreeing, which is
    /// `scripts/ready-state/verify-v1-pack-reproducible.sh` on a Linux host.
    #[test]
    fn the_v1_pack_leaves_no_clock_or_entropy_in_the_image() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let script = pack_app_image_script_v1(&spec, 512, "docker");

        // The two values mke2fs would otherwise draw at random.
        assert!(script.contains(r#"-U "$ATO_FS_UUID""#), "{script}");
        assert!(
            script.contains(r#"-E hash_seed="$ATO_FS_UUID""#),
            "{script}"
        );
        // The superblock clocks. mke2fs does NOT honour SOURCE_DATE_EPOCH —
        // measured on e2fsprogs 1.47.0, where two runs ten seconds apart
        // produced two "Filesystem created" values ten seconds apart — so they
        // are set afterwards through debugfs, which recomputes the superblock
        // checksum that a raw byte patch would invalidate.
        for field in ["mkfs_time", "lastcheck", "wtime"] {
            assert!(
                script.contains(&format!("set_super_value {field} {V1_GUEST_IMAGE_EPOCH}")),
                "{field}: {script}"
            );
        }
        // SOURCE_DATE_EPOCH belongs on debugfs and NOT on mke2fs, and the
        // difference is measured rather than stylistic: mke2fs ignores it (its
        // `s_mkfs_time` was wall-clock with it set), while debugfs stamps
        // `s_wtime` from `fs->now` as it flushes and would otherwise overwrite
        // the value it was just told to set. Putting it back on mke2fs would
        // read as a control that works.
        assert!(
            script.contains(&format!("SOURCE_DATE_EPOCH={V1_GUEST_IMAGE_EPOCH} debugfs")),
            "{script}"
        );
        assert!(
            !script.contains("SOURCE_DATE_EPOCH={V1_GUEST_IMAGE_EPOCH} mkfs"),
            "{script}"
        );
        assert!(
            script.contains(&format!(r#"-exec touch -h -d @{V1_GUEST_IMAGE_EPOCH}"#)),
            "{script}"
        );
        // And the allocation order: populated by mke2fs, not by the running
        // kernel through a loop mount.
        assert!(script.contains(r#"-d "$BUILD/rootfs""#), "{script}");
        assert!(
            !script.contains("mount -o loop"),
            "a loop mount leaves allocation to the running kernel: {script}"
        );
        assert!(!script.contains(r#"cp -a "$BUILD/rootfs/.""#), "{script}");
    }

    /// v0.3 keeps its mount-and-copy pack, byte for byte. It makes no claim
    /// about what its image IS, so the determinism work does not apply to it —
    /// and changing a working producer to share one would be a regression risk
    /// taken for nothing.
    #[test]
    fn the_v03_pack_is_unchanged() {
        let manifest = CapsuleManifest::from_toml(&base_toml()).expect("v0.3 manifest");
        let script = build_rootfs_script(
            &derive_build_spec(&manifest, &python_probe()).expect("derives"),
            512,
        );
        assert!(script.contains("mkfs.ext4 -q -F \"$ATO_OUT\""), "{script}");
        assert!(script.contains("mount -o loop"), "{script}");
        assert!(!script.contains("SOURCE_DATE_EPOCH"), "{script}");
    }

    /// The UUID is a function of the build's inputs: stable for one program,
    /// distinct between programs. A constant shared by every capsule would make
    /// two different images collide for anything resolving a device by UUID;
    /// a random one would put entropy into the identity.
    #[test]
    fn the_filesystem_uuid_is_derived_from_the_builds_own_inputs() {
        let argv = vec!["python3".to_string(), "app.py".to_string()];
        let uuid =
            |source: &str, base: &str, argv: &[String]| v1_filesystem_uuid(source, base, argv);

        let baseline = uuid("sha256:aa", PINNED_BASE, &argv);
        assert_eq!(baseline, uuid("sha256:aa", PINNED_BASE, &argv), "stable");
        assert!(is_uuid(&baseline), "{baseline}");

        // Each input moves it.
        assert_ne!(baseline, uuid("sha256:bb", PINNED_BASE, &argv));
        assert_ne!(baseline, uuid("sha256:aa", "docker.io/x@sha256:ff", &argv));
        assert_ne!(
            baseline,
            uuid("sha256:aa", PINNED_BASE, &["python3".to_string()])
        );
        // And argv boundaries are not lost to concatenation.
        assert_ne!(
            uuid("sha256:aa", PINNED_BASE, &["a".into(), "b".into()]),
            uuid("sha256:aa", PINNED_BASE, &["ab".into()])
        );
    }

    /// A malformed UUID is refused rather than handed to `mke2fs`.
    #[test]
    fn the_pack_refuses_a_uuid_it_did_not_derive() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let out = tempfile::tempdir().expect("tempdir");
        let error = pack_app_image_v1(
            AssembledGuestImage::adopt("ato-v1-test".into()),
            &spec,
            &out.path().join("guest.img"),
            512,
            "not-a-uuid",
            "docker",
        )
        .expect_err("refused");
        assert!(error.contains("canonical"), "{error}");
        assert!(is_uuid("0123abcd-4567-89ef-0123-456789abcdef"));
        assert!(
            !is_uuid("0123ABCD-4567-89ef-0123-456789abcdef"),
            "uppercase"
        );
        assert!(!is_uuid("0123abcd-4567-89ef-0123-456789abcde"), "short");
    }

    /// The assemble half must leave the image behind — the whole reason the
    /// pipeline is split is so `measure_guest_target` can inspect the artifact
    /// the guest boots, and an `rmi` here would delete it first.
    #[test]
    fn assembling_does_not_remove_the_image_but_packing_does() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");

        let assemble = assemble_app_image_script_v1(&spec, PINNED_BASE, "docker");
        assert!(
            !assemble.contains("rmi"),
            "the assembled image must survive for measurement: {assemble}"
        );

        // The pack half is the last user, so it is the one that cleans up.
        let pack = pack_app_image_script_v1(&spec, 512, "docker");
        assert!(pack.contains("rmi -f \"$TAG\""), "{pack}");
        assert!(pack.contains("TAG=\"$ATO_IMAGE\""), "{pack}");
    }

    /// The two halves must name the same image, or the pack would export
    /// whatever else happened to carry that tag.
    #[test]
    fn both_halves_address_the_image_through_the_same_variable() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        assert!(
            assemble_app_image_script_v1(&spec, PINNED_BASE, "docker")
                .contains("docker build -q -t \"$ATO_IMAGE\""),
        );
        assert!(pack_app_image_script_v1(&spec, 512, "docker").contains("TAG=\"$ATO_IMAGE\""));
    }

    /// The base reference is interpolated into the generated Dockerfile, so a
    /// newline in it could add a `RUN` line the author never wrote. It is
    /// refused before any script is generated, not escaped at emission — and
    /// the refusal happens before docker is ever spawned.
    #[test]
    fn a_control_character_in_an_image_reference_is_refused() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let source = tempfile::tempdir().expect("tempdir");

        let error = assemble_app_image_v1(
            source.path(),
            &spec,
            "python@sha256:aaa\nRUN rm -rf /",
            "ato-v1-test",
            "docker",
        )
        .expect_err("a newline in the base ref is refused");
        assert!(error.contains("newline"), "{error}");

        let error = assemble_app_image_v1(
            source.path(),
            &spec,
            PINNED_BASE,
            "ato-v1-test\nRUN rm -rf /",
            "docker",
        )
        .expect_err("a newline in the image ref is refused");
        assert!(error.contains("newline"), "{error}");
    }

    /// The image reference reaches the script only as an environment variable
    /// inside double quotes — it is never interpolated into the script text, so
    /// a metacharacter in a tag cannot become shell syntax.
    #[test]
    fn the_image_reference_is_never_interpolated_into_the_script() {
        let spec = derive_build_spec_v1(&v1(V1_MINIMAL), &python_probe()).expect("derives");
        let assemble = assemble_app_image_script_v1(&spec, PINNED_BASE, "docker");
        let pack = pack_app_image_script_v1(&spec, 512, "docker");
        // Both scripts are generated without knowing the reference at all.
        for script in [&assemble, &pack] {
            assert!(script.contains("$ATO_IMAGE"), "{script}");
        }
    }

    /// An observed prefix and an unmeasured one must not be spellable the same
    /// way: the empty vector is reachable only through the constructor that
    /// says the runtime prepends nothing.
    #[test]
    fn an_empty_observed_prefix_must_name_which_emptiness_it_is() {
        assert!(ObservedInvocationPrefix::observed(Vec::new()).is_err());
        assert_eq!(
            ObservedInvocationPrefix::observed(vec!["uv".into(), "run".into()])
                .expect("a real prefix")
                .words(),
            ["uv", "run"]
        );
        assert!(ObservedInvocationPrefix::observed_none().words().is_empty());
    }

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
        SourceProbe {
            has_requirements_txt: true,
            ..Default::default()
        }
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
        let m =
            parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"scripts/serve.py\""));
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.start_cmd, "python3 scripts/serve.py");
        // Multi-token commands are explicit shell commands — verbatim, even when they
        // end in `.py` (never `python3 python app.py`).
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"python app.py\""));
        let spec = derive_build_spec(&m, &probe_python()).unwrap();
        assert_eq!(spec.start_cmd, "python app.py");
        // Non-python commands are untouched.
        let m = parse(&base_toml().replace("run = \"python3 app.py\"", "run = \"node server.js\""));
        let spec = derive_build_spec(
            &m,
            &SourceProbe {
                has_package_json: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(spec.start_cmd, "node server.js");
        assert_eq!(spec.declared_start_cmd, "node server.js");
    }

    #[test]
    fn node_detected_from_package_json() {
        let m = parse(&base_toml().replace("python3 app.py", "node server.js"));
        let spec = derive_build_spec(
            &m,
            &SourceProbe {
                has_package_json: true,
                ..Default::default()
            },
        )
        .unwrap();
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
        let spec = derive_build_spec(
            &m,
            &SourceProbe {
                has_py_files: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(spec.runtime, RuntimeKind::Python);
        assert_eq!(spec.install_cmd.as_deref(), Some("true")); // nothing to install
    }

    #[test]
    fn required_secret_binding_external_gpu_all_fail_closed() {
        let secret = format!(
            "{}\n[secrets.api_key]\nrequired = true\nenv = \"API_KEY\"\ndelivery = \"proxy\"\n",
            base_toml()
        );
        assert!(
            derive_build_spec(&parse(&secret), &probe_python())
                .unwrap_err()
                .contains("secrets")
        );
        let binding = format!(
            "{}\n[bindings.user_files]\nkind = \"user_files\"\nrequired = true\nscope = \"user\"\n",
            base_toml()
        );
        assert!(
            derive_build_spec(&parse(&binding), &probe_python())
                .unwrap_err()
                .contains("bindings")
        );
        let external = format!(
            "{}\n[external.gpu]\ntype = \"gpu\"\nrequired = false\n",
            base_toml()
        );
        assert!(
            derive_build_spec(&parse(&external), &probe_python())
                .unwrap_err()
                .contains("external")
        );
    }

    #[test]
    fn missing_port_fails_closed_but_missing_probe_synthesizes() {
        // No port: still fail-closed — nothing to probe, nothing to proxy.
        let no_port = base_toml().replace("port = 8080\n", "");
        assert!(
            derive_build_spec(&parse(&no_port), &probe_python())
                .unwrap_err()
                .contains("port")
        );
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
        assert!(
            materialize_source("acme", "app", "main", None, None, dir.path())
                .unwrap_err()
                .contains("non-pinned")
        );
        // path-like / invalid owner + repo are rejected before any network use.
        assert!(
            materialize_source("../evil", "app", &sha, None, None, dir.path())
                .unwrap_err()
                .contains("owner")
        );
        assert!(
            materialize_source("acme/x", "app", &sha, None, None, dir.path())
                .unwrap_err()
                .contains("owner")
        );
        assert!(
            materialize_source("acme", "a/b", &sha, None, None, dir.path())
                .unwrap_err()
                .contains("repo")
        );
        assert!(
            materialize_source("acme", "..", &sha, None, None, dir.path())
                .unwrap_err()
                .contains("repo")
        );
        assert!(
            materialize_source("acme", "", &sha, None, None, dir.path())
                .unwrap_err()
                .contains("repo")
        );
    }

    #[test]
    fn checkout_source_tree_validates_identity_before_any_network() {
        // The source_materialize lane shares the recipe lane's fail-closed identity
        // gate: a non-pinned commit and path-like owner/repo are rejected before git
        // ever runs, and (unlike materialize_source) no capsule.toml is required.
        let dir = tempfile::tempdir().unwrap();
        let sha = "a".repeat(40);
        assert!(
            checkout_source_tree("acme", "app", "main", None, dir.path())
                .unwrap_err()
                .contains("non-pinned")
        );
        assert!(
            checkout_source_tree("../evil", "app", &sha, None, dir.path())
                .unwrap_err()
                .contains("owner")
        );
        assert!(
            checkout_source_tree("acme", "a/b", &sha, None, dir.path())
                .unwrap_err()
                .contains("repo")
        );
        assert!(
            checkout_source_tree("acme", "app", &sha, Some("../escape"), dir.path())
                .unwrap_err()
                .contains("..")
        );
    }

    /// Commit a `git init`ed tree with pinned identity/dates and no ambient config, so
    /// the only thing that can differ between two such repos is `.git`'s own
    /// machine-dependent state (chiefly `.git/index`'s per-file stat data).
    fn commit_local_repo(dir: &Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create source parent dir");
            }
            std::fs::write(&path, body).expect("write source file");
        }
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                // Isolate from the developer's global/system config (signing, hooks,
                // autocrlf, default branch) so both repos are built identically.
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "ato")
                .env("GIT_AUTHOR_EMAIL", "ato@example.invalid")
                .env("GIT_COMMITTER_NAME", "ato")
                .env("GIT_COMMITTER_EMAIL", "ato@example.invalid")
                .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00+0000")
                .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00+0000")
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "pinned"]);
    }

    #[test]
    fn pinned_checkouts_of_identical_content_hash_identically() {
        // The regression this closes: `git_checkout_pinned` used to leave its own `.git`
        // in the tree callers receive, and A1v2 hashes a ROOT `.git` as an ordinary
        // directory — so `materialized_source_tree_hash`, an identity-bearing value
        // (`ExecutionContractV1.source.digest`, the source_materialize ack), differed
        // between two checkouts of byte-identical source.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let files = [
            ("capsule.toml", "name = \"demo\"\n"),
            ("app.py", "print('hi')\n"),
            ("static/index.html", "<h1>hi</h1>\n"),
        ];
        commit_local_repo(a.path(), &files);
        commit_local_repo(b.path(), &files);
        assert!(a.path().join(".git").is_dir() && b.path().join(".git").is_dir());

        remove_checkout_git_metadata(a.path()).unwrap();
        remove_checkout_git_metadata(b.path()).unwrap();

        let ha = capsule::blob::materialized_source_tree_hash(a.path()).unwrap();
        let hb = capsule::blob::materialized_source_tree_hash(b.path()).unwrap();
        assert_eq!(
            ha, hb,
            "two checkouts of the same content must yield the same A1v2 tree hash"
        );
        assert!(ha.starts_with("sha256:"), "{ha}");
    }

    #[test]
    fn root_git_metadata_removal_is_what_makes_the_hash_agree() {
        // The same proof without git: two trees whose SOURCE bytes match but whose root
        // `.git` differs (as real ones always do — `.git/index` carries per-file stat
        // data) hash DIFFERENTLY before the removal and identically after, so the
        // regression test above cannot pass vacuously.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for (dir, index_bytes) in [(a.path(), "stat-data-a"), (b.path(), "stat-data-b")] {
            std::fs::write(dir.join("app.py"), "print('hi')\n").unwrap();
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(dir.join(".git").join("index"), index_bytes).unwrap();
            std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        }
        let before_a = capsule::blob::materialized_source_tree_hash(a.path()).unwrap();
        let before_b = capsule::blob::materialized_source_tree_hash(b.path()).unwrap();
        assert_ne!(
            before_a, before_b,
            "a retained root .git must be what perturbs the hash"
        );

        remove_checkout_git_metadata(a.path()).unwrap();
        remove_checkout_git_metadata(b.path()).unwrap();
        assert_eq!(
            capsule::blob::materialized_source_tree_hash(a.path()).unwrap(),
            capsule::blob::materialized_source_tree_hash(b.path()).unwrap()
        );
    }

    #[test]
    fn source_root_carries_no_git_metadata_with_or_without_a_subdir() {
        // Both source roots `contained_source_root` can return must be `.git`-free: the
        // checkout root itself (no subdir — reachable for any capsule at the repo root)
        // and a subdir root. A NESTED `.git` is left alone: A1v2 rejects it as a
        // submodule signal and removing it would hide that.
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(root.join("app").join("vendor").join(".git")).unwrap();
        std::fs::write(root.join("app").join("capsule.toml"), b"x").unwrap();

        remove_checkout_git_metadata(root).unwrap();

        let no_subdir = contained_source_root(root, None, false).unwrap();
        assert!(!no_subdir.join(".git").exists());
        let subdir = contained_source_root(root, Some("app"), true).unwrap();
        assert!(!subdir.join(".git").exists());
        assert!(
            subdir.join("vendor").join(".git").is_dir(),
            "a nested .git is a submodule signal A1v2 must still see"
        );

        // Idempotent + absent-is-fine: re-running on a tree with no `.git` is not an error.
        remove_checkout_git_metadata(root).unwrap();
    }

    #[test]
    fn github_identity_validation() {
        assert!(
            valid_github_owner("acme") && valid_github_owner("a-b-1") && valid_github_owner("A9")
        );
        assert!(
            !valid_github_owner("")
                && !valid_github_owner("-a")
                && !valid_github_owner("a-")
                && !valid_github_owner("a/b")
                && !valid_github_owner("..")
        );
        assert!(valid_github_repo("my.app_1-x") && valid_github_repo("a"));
        assert!(
            !valid_github_repo("")
                && !valid_github_repo(".")
                && !valid_github_repo("..")
                && !valid_github_repo("a/b")
                && !valid_github_repo("a b")
        );
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
        assert!(
            err.contains("recipe manifest"),
            "the error must say what was missing: {err}"
        );
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
        let uf = format!(
            "{}\n[bindings.user_files]\nkind = \"user_files\"\nrequired = false\nscope = \"user\"\n",
            base_toml()
        );
        assert!(
            derive_build_spec(&parse(&uf), &probe_python())
                .unwrap_err()
                .contains("binding")
        );
        let oauth = format!(
            "{}\n[bindings.login]\nkind = \"oauth\"\nrequired = false\nscope = \"user\"\n",
            base_toml()
        );
        assert!(
            derive_build_spec(&parse(&oauth), &probe_python())
                .unwrap_err()
                .contains("binding")
        );
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
        assert!(
            script.contains("trap cleanup EXIT"),
            "script must install an EXIT cleanup trap"
        );
        assert!(
            script.contains("docker rm -f")
                && script.contains("docker rmi -f")
                && script.contains("umount"),
            "cleanup must reap container/image/mount"
        );
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
        assert!(
            script.contains("<<'DOCKER'") && script.contains("<<'INIT'"),
            "heredocs must be quoted"
        );
        // The command appears as a single-quoted argument to sh -lc (Docker RUN + guest init),
        // never as a bare host-shell token.
        assert!(
            script.contains("RUN /bin/sh -lc 'echo $(touch /tmp/ato-host-pwned)'"),
            "build cmd must be a single-quoted Docker RUN arg"
        );
        assert!(
            script.contains("/bin/sh -lc 'echo $(touch /tmp/ato-host-pwned)' >/tmp/app.log"),
            "start cmd must be a single-quoted guest-init arg"
        );
        // And there is no UNquoted occurrence that the host would expand.
        assert!(
            !script.contains("( echo $(touch"),
            "must not embed the command raw"
        );
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
        let nl = base_toml().replace(
            "run = \"python3 app.py\"",
            "run = \"python3 app.py\\nrm -rf /\"",
        );
        assert!(
            derive_build_spec(&parse(&nl), &probe_python())
                .unwrap_err()
                .contains("newline")
        );
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
            derive_build_spec(&m, &probe_python())
                .unwrap_err()
                .contains("secrets"),
            "no-binding derive must still reject a secret capsule"
        );
        // The v1.2 supervisor path accepts it and produces the supervisor config.
        let spec = derive_supervisor_build_spec(&m, &probe_python()).expect("supervisor spec");
        let sup = spec.supervisor.as_ref().expect("supervisor present");
        // Binding name = the (lowercase) secret key; env_map is ENV_VAR → binding name.
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        assert_eq!(
            sup.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
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
        assert!(
            err.contains("[secrets.openai_api_key]"),
            "message must suggest the lowercase form: {err}"
        );
        // The canonical lowercase-key + explicit-uppercase-env form is accepted.
        assert!(derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).is_ok());
    }

    #[test]
    fn supervisor_derive_accepts_only_env_delivery() {
        // Only delivery = "env" injects a supervisor env var. file / proxy / fd are
        // all rejected here (file is a later request-time read path; proxy/fd never
        // inject an env var).
        for d in ["file", "proxy", "fd"] {
            let toml = supervisor_toml()
                .replace("env = \"OPENAI_API_KEY\"", &format!("delivery = \"{d}\""));
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
        assert!(
            derive_supervisor_build_spec(&parse(&with_binding), &probe_python())
                .unwrap_err()
                .contains("no-binding")
        );
        // No secrets at all → not a supervisor build.
        assert!(
            derive_supervisor_build_spec(&parse(&base_toml()), &probe_python())
                .unwrap_err()
                .contains("requires at least one")
        );
    }

    #[test]
    fn supervisor_derive_rejects_malformed_and_duplicate_env_var_names() {
        // A malformed env var name must never reach the generated supervisor.json.
        // (lowercase secret key so it passes the binding-name gate and reaches the env check)
        let bad = format!(
            "{}\n[secrets.key]\nrequired = true\nenv = \"BAD-VAR\"\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&bad), &probe_python())
                .unwrap_err()
                .contains("POSIX identifier")
        );
        // Two secrets resolving to the SAME env var is ambiguous → fail-closed.
        let dup = format!(
            "{}\n[secrets.key_a]\nrequired = true\nenv = \"SHARED\"\n\
             [secrets.key_b]\nrequired = true\nenv = \"SHARED\"\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&dup), &probe_python())
                .unwrap_err()
                .contains("duplicate env injection")
        );
        // A secret with no `env` uses its NAME; a name that isn't a POSIX identifier
        // (dot allowed in a binding name but not an env var) is rejected too.
        let name_as_var = format!("{}\n[secrets.\"api.key\"]\nrequired = true\n", base_toml());
        assert!(
            derive_supervisor_build_spec(&parse(&name_as_var), &probe_python())
                .unwrap_err()
                .contains("POSIX identifier")
        );
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
        assert_eq!(
            api.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
        assert!(
            !api.env_map.contains_key("REDIS_PASSWORD"),
            "api must NOT get redis's secret"
        );
        assert_eq!(
            redis.env_map.get("REDIS_PASSWORD").map(String::as_str),
            Some("redis_password")
        );
        assert!(
            !redis.env_map.contains_key("OPENAI_API_KEY"),
            "redis must NOT get api's secret"
        );
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
        assert_eq!(
            sup.binding_names,
            vec!["openai_api_key"],
            "unused optional secret dropped from lease set"
        );
        let redis = sup
            .services
            .as_ref()
            .unwrap()
            .iter()
            .find(|s| s.name == "redis")
            .unwrap();
        assert!(
            redis.env_map.is_empty(),
            "redis scopes no secret → empty injection map"
        );
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
        assert_eq!(
            services
                .iter()
                .filter(|s| s.port == Some(spec.port))
                .count(),
            1
        );

        // expose resolution + cross-injection: redis's port is allocated (≠ target),
        // web can reach it by REDIS_REDIS_PORT, and it matches redis's own REDIS_PORT.
        let rport = redis.base_env.get("REDIS_PORT").expect("redis own port");
        assert_ne!(rport, "8080");
        assert_eq!(web.base_env.get("REDIS_REDIS_PORT"), Some(rport));
        assert_eq!(
            worker.base_env.get("REDIS_REDIS_PORT"),
            Some(rport),
            "worker reaches redis too"
        );

        // service aliasing: /etc/hosts maps every name + alias to loopback.
        let hosts = build_etc_hosts(services);
        for h in ["web", "redis", "cache", "worker"] {
            assert!(hosts.contains(h), "hosts missing {h}");
        }

        // per-service secret scoping: least privilege, no cross-delivery.
        assert_eq!(
            web.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
        assert!(
            !web.env_map.contains_key("REDIS_PASSWORD"),
            "web must not get redis's secret"
        );
        assert_eq!(
            redis.env_map.get("REDIS_PASSWORD").map(String::as_str),
            Some("redis_password")
        );
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
        assert!(
            redisj["readiness"].get("http_path").is_none(),
            "redis readiness is TCP-accept"
        );
        // No secret VALUE anywhere in the emitted config (names only).
        assert!(
            !json.to_string().contains("sk-")
                && !json.to_string().to_lowercase().contains("password=")
        );

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
        assert!(
            err.contains("required secret 'openai_api_key'")
                && err.contains("not used by any service"),
            "{err}"
        );
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
        assert!(
            err.contains("'ghost_key'") && err.contains("not a declared"),
            "{err}"
        );
    }

    #[test]
    fn legacy_single_service_still_receives_every_secret() {
        // The legacy single-service build (no [services]) is unchanged: the sole
        // workload gets every declared secret, and binding_names is the full set.
        let spec =
            derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(sup.services.is_none(), "legacy single-service build");
        assert_eq!(
            sup.binding_names,
            vec!["openai_api_key"],
            "legacy keeps every declared secret"
        );
        assert_eq!(
            sup.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
    }

    #[test]
    fn supervisor_build_script_runs_agent_as_init_and_emits_config_without_secrets() {
        let spec =
            derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        // init runs the guest-agent (vsock supervisor) with the (lowercase) binding name
        // as argv, NOT the app directly — and NOT the uppercase env var name.
        assert!(
            script.contains("/usr/local/bin/ato-guest-agent 'openai_api_key'"),
            "{script}"
        );
        assert!(
            !script.contains("ato-guest-agent 'OPENAI_API_KEY'"),
            "env var must not be the binding argv"
        );
        assert!(
            script.contains("ATO_GUEST_AGENT_MODE=vsock"),
            "agent runs in vsock mode"
        );
        assert!(
            !script.contains("python3 app.py' >/tmp/app.log"),
            "app is not launched directly"
        );
        // supervisor.json is staged, requires the agent binary, and carries NO value —
        // only the env var → binding name map.
        assert!(
            script.contains("ATO_GUEST_AGENT_BIN"),
            "supervisor build needs the agent binary"
        );
        assert!(script.contains("supervisor.json"), "config is staged");
        assert!(
            script.contains("\"OPENAI_API_KEY\": \"openai_api_key\""),
            "env→binding map present"
        );
        assert!(
            script.contains("<<'DOCKER'") && script.contains("<<'INIT'"),
            "heredocs still quoted"
        );
    }

    /// v1.6 (ato#983) Slice 3 regression, found live on real KVM hardware: the
    /// rootfs is mounted READ-ONLY at boot, so the guest-agent's
    /// `mkdir -p <target>` before mounting a durable volume can never CREATE
    /// a new directory there — it must already exist, baked in at build time.
    #[test]
    fn durable_state_target_directory_is_baked_into_the_rootfs_at_build_time() {
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [state.dbdata]\nkind = \"filesystem\"\ndurability = \"persistent\"\n\
             purpose = \"x\"\nattach = \"explicit\"\nschema_id = \"sha256:{}\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n\
             state_bindings = [{{ state = \"dbdata\", target = \"/ato/state/dbdata\" }}]\n\
             [services.api.network]\npublish = true\n",
            base_toml(),
            "0".repeat(64)
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(
            script.contains(r#"mkdir -p "$BUILD"'/rootfs/ato/state/dbdata'"#),
            "durable-state target must be mkdir'd into the rootfs before it's sealed read-only: {script}"
        );
    }

    #[test]
    fn no_durable_state_script_stays_byte_identical_no_extra_mkdir() {
        // A supervisor build with NO durable-state binding must not gain any
        // new mkdir line for it.
        let toml = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n\
             [services.api.network]\npublish = true\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(
            !script.contains("/ato/state"),
            "no durable state declared, no /ato/state mkdir expected: {script}"
        );
    }

    #[test]
    fn no_binding_script_is_unaffected_by_the_supervisor_addition() {
        // A no-binding spec still runs the app directly and stages no agent.
        let spec = derive_build_spec(&parse(&base_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(
            script.contains("/bin/sh -lc 'python3 app.py' >/tmp/app.log"),
            "direct app launch"
        );
        assert!(
            !script.contains("ato-guest-agent"),
            "no agent in a no-binding rootfs"
        );
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
        assert_eq!(
            api.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
        assert_eq!(redis.name, "redis");
        assert!(!redis.public, "redis is internal (no publish)");

        // Emitted supervisor.json: services[] shape, PUBLIC service gets PORT,
        // internal one does not, and NO secret value appears.
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let arr = json["services"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "api");
        assert_eq!(arr[0]["base_env"]["PORT"], spec.port.to_string());
        assert!(
            arr[1]["base_env"].get("PORT").is_none(),
            "internal service has no PORT injected"
        );
        assert_eq!(arr[0]["bindings_env"]["OPENAI_API_KEY"], "openai_api_key");
        assert!(
            !json.to_string().contains("sk-"),
            "no secret value in the emitted config"
        );
    }

    // ── Phase 7 (generated internal bindings) ──

    #[test]
    fn run_once_service_emits_dag_fields_and_dependents_wait_for_success() {
        // A migration-style one-shot + a public web that depends on it and on a
        // long-running db — the dependent's wait splits by dep kind.
        let toml = format!(
            "{}\n[services.web]\nentrypoint = \"python3 web.py\"\nsecrets = [\"api_key\"]\n\
             depends_on = [\"migrate\", \"db\"]\n\
             [services.web.network]\npublish = true\n\
             [services.db]\nentrypoint = \"python3 db.py\"\n\
             [services.db.expose]\n\
             [services.migrate]\nentrypoint = \"python3 migrate.py\"\nrun_once = true\n\
             [secrets.api_key]\nrequired = true\nenv = \"API_KEY\"\n",
            base_toml()
        );
        // db needs an expose-able port to be waitable; reuse the simplest shape:
        let toml = toml.replace("[services.db.expose]\n", "");
        let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python())
            .expect("run_once manifest derives");
        let sup = spec.supervisor.as_ref().unwrap();
        let migrate = sup
            .services
            .as_ref()
            .unwrap()
            .iter()
            .find(|s| s.name == "migrate")
            .unwrap();
        assert!(migrate.run_once);
        let json = build_supervisor_json(sup, spec.port, "");
        let arr = json["services"].as_array().unwrap();
        let m = arr.iter().find(|s| s["name"] == "migrate").unwrap();
        assert_eq!(m["kind"], "run_once");
        assert_eq!(m["run_at"], serde_json::json!(["seal_once"]));
        assert!(
            m.get("readiness").is_none(),
            "one-shot has no readiness block"
        );
        let w = arr.iter().find(|s| s["name"] == "web").unwrap();
        assert_eq!(w["depends_on_success"], serde_json::json!(["migrate"]));
        assert_eq!(w["depends_on"], serde_json::json!(["db"]));
    }

    #[test]
    fn run_once_contradictions_fail_closed_in_the_manifest() {
        // readiness_probe on a run_once task.
        let toml = format!(
            "{}\n[services.app2]\nentrypoint = \"x\"\nsecrets = [\"api_key\"]\nrun_once = true\n\
             readiness_probe = {{ http_get = \"/\" }}\n\
             [secrets.api_key]\nrequired = true\nenv = \"API_KEY\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
        assert!(err.contains("contradictory"), "{err}");
        // public run_once task.
        let toml = format!(
            "{}\n[services.app2]\nentrypoint = \"x\"\nsecrets = [\"api_key\"]\nrun_once = true\n\
             [services.app2.network]\npublish = true\n\
             [secrets.api_key]\nrequired = true\nenv = \"API_KEY\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
        assert!(err.contains("cannot be the public service"), "{err}");
    }

    fn generated_bindings_toml() -> String {
        // A public `api` + an internal `postgres`, both fed the SAME generated
        // internal DB password. One external secret too (realistic: an api key on
        // the lease path coexists with an internal generated credential).
        format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"python3 api.py\"\nsecrets = [\"openai_api_key\"]\n\
             [services.api.network]\npublish = true\n\
             [services.postgres]\nentrypoint = \"postgres\"\n\
             [generated_bindings.db_password]\ngenerator = \"random_base64\"\nbytes = 32\n\
             scope = \"run\"\ntargets = [\"api\", \"postgres\"]\n",
            base_toml()
        )
    }

    #[test]
    fn generated_binding_injects_env_records_spec_and_never_a_value() {
        let spec =
            derive_supervisor_build_spec(&parse(&generated_bindings_toml()), &probe_python())
                .expect("supervisor spec with generated binding");
        let sup = spec.supervisor.as_ref().unwrap();
        // The SPEC is recorded (name/generator/bytes/scope/targets) — no value.
        assert_eq!(sup.generated_bindings.len(), 1);
        let g = &sup.generated_bindings[0];
        assert_eq!(g.name, "db_password");
        assert_eq!(g.generator, "random_base64");
        assert_eq!(g.bytes, 32);
        assert_eq!(g.scope, "run");
        assert_eq!(g.targets, vec!["api", "postgres"]);
        // A generated binding is NOT leased — the guest must never wait for it, so
        // it stays out of the agent argv (binding_names = the external secret only).
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        // BOTH target services get the injected env → the same binding name → the
        // same run-time value (one shared tmpfs file). postgres also gets it even
        // though it scopes no [secrets.*].
        let services = sup.services.as_ref().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        let postgres = services.iter().find(|s| s.name == "postgres").unwrap();
        assert_eq!(
            api.env_map.get("DB_PASSWORD").map(String::as_str),
            Some("db_password")
        );
        assert_eq!(
            postgres.env_map.get("DB_PASSWORD").map(String::as_str),
            Some("db_password")
        );
        assert_eq!(
            api.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
        assert!(
            !postgres.env_map.contains_key("OPENAI_API_KEY"),
            "postgres scopes no external secret"
        );

        // Emitted supervisor.json carries the value-free generated_bindings spec at
        // the top level (mirrors the guest's SupervisorConfig.generated_bindings).
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let gb = json["generated_bindings"]
            .as_array()
            .expect("generated_bindings array");
        assert_eq!(gb.len(), 1);
        assert_eq!(gb[0]["name"], "db_password");
        assert_eq!(gb[0]["generator"], "random_base64");
        assert_eq!(gb[0]["bytes"], 32);
        assert_eq!(gb[0]["scope"], "run");
        assert_eq!(gb[0]["targets"][1], "postgres");
        // Nothing in the whole emitted config resembles a value (no base64 padding,
        // no secret bytes) — the value is generated per run inside the guest only.
        assert!(
            !gb[0].as_object().unwrap().contains_key("value"),
            "spec must never carry a value"
        );
    }

    #[test]
    fn generated_binding_spec_is_identity_stable_two_builds_same_bytes() {
        // Two builds of the same manifest emit BYTE-IDENTICAL supervisor.json —
        // the SPEC is in the artifact (so identity is stable), the VALUE never is
        // (it is generated per run). Different runs of the identical artifact then
        // get different values without changing artifact identity.
        let a = derive_supervisor_build_spec(&parse(&generated_bindings_toml()), &probe_python())
            .unwrap();
        let b = derive_supervisor_build_spec(&parse(&generated_bindings_toml()), &probe_python())
            .unwrap();
        let ja = build_supervisor_json(a.supervisor.as_ref().unwrap(), a.port, &a.start_cmd);
        let jb = build_supervisor_json(b.supervisor.as_ref().unwrap(), b.port, &b.start_cmd);
        assert_eq!(
            ja.to_string(),
            jb.to_string(),
            "same spec ⇒ identical supervisor.json (identity stable)"
        );
    }

    #[test]
    fn generated_binding_fail_closed_rules() {
        // Unknown target service.
        let bad_target =
            generated_bindings_toml().replace("[\"api\", \"postgres\"]", "[\"api\", \"nope\"]");
        assert!(
            derive_supervisor_build_spec(&parse(&bad_target), &probe_python())
                .unwrap_err()
                .contains("not a declared service")
        );
        // bytes out of range.
        let bad_bytes = generated_bindings_toml().replace("bytes = 32", "bytes = 99999");
        assert!(
            derive_supervisor_build_spec(&parse(&bad_bytes), &probe_python())
                .unwrap_err()
                .contains("bytes")
        );
        // Uppercase binding name is not a valid BindingName.
        let bad_name = generated_bindings_toml().replace(
            "[generated_bindings.db_password]",
            "[generated_bindings.DB_PASSWORD]",
        );
        assert!(
            derive_supervisor_build_spec(&parse(&bad_name), &probe_python())
                .unwrap_err()
                .contains("BindingName")
        );
        // Injected env collides with a service's existing secret env (SHARED name
        // via an explicit env on the secret): the api secret env = DB_PASSWORD.
        let collide = format!(
            "{}\n[secrets.db_password_secret]\nrequired = true\nenv = \"DB_PASSWORD\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"db_password_secret\"]\n\
             [services.api.network]\npublish = true\n\
             [services.postgres]\nentrypoint = \"postgres\"\n\
             [generated_bindings.db_password]\ngenerator = \"random_base64\"\nbytes = 32\n\
             targets = [\"api\"]\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&collide), &probe_python())
                .unwrap_err()
                .contains("collides")
        );
    }

    #[test]
    fn generated_binding_requires_a_multi_service_capsule() {
        // A generated binding with NO [services.*] fails closed — the value is
        // injected into named target services.
        let single = format!(
            "{}\n[generated_bindings.db_password]\ngenerator = \"random_base64\"\nbytes = 32\n\
             targets = [\"app\"]\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&single), &probe_python())
                .unwrap_err()
                .contains("multi-service")
        );
    }

    // ── v1.5 (ato#973): app_url selection ──

    #[test]
    fn app_url_selection_records_the_public_service_and_targets_its_port() {
        // api(public) + redis(internal) → the app_url target is api, on the proxied
        // target port; redis is never the URL target and its port is not exposed.
        let spec =
            derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        // The receipt records WHICH service the public URL points at.
        assert_eq!(
            sup.public_service.as_deref(),
            Some("api"),
            "public service recorded for app_url"
        );
        // That service owns the proxied target port (= the ready_url/app_url port).
        let services = sup.services.as_ref().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert!(api.public);
        assert_eq!(
            api.port,
            Some(spec.port),
            "public service listens on the proxied port"
        );
        // The internal service is never selected, and it does not own the target port.
        let redis = services.iter().find(|s| s.name == "redis").unwrap();
        assert!(!redis.public);
        assert_ne!(sup.public_service.as_deref(), Some("redis"));
        assert_ne!(
            redis.port,
            Some(spec.port),
            "internal service is not on the public port"
        );
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
        let redis = sup
            .services
            .as_ref()
            .unwrap()
            .iter()
            .find(|s| s.name == "redis")
            .unwrap();
        // redis's exposed port is a real allocated loopback port, but it is NOT the
        // public/proxied port, and redis is not the URL target.
        let rport = redis
            .base_env
            .get("REDIS_PORT")
            .unwrap()
            .parse::<u16>()
            .unwrap();
        assert_ne!(
            rport, spec.port,
            "internal expose port is not the public port"
        );
        assert!(
            !redis.aliases.is_empty(),
            "redis has an alias (cache) — internal, not a URL target"
        );
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
        let err =
            derive_supervisor_build_spec(&parse(&internal_on_target), &probe_python()).unwrap_err();
        assert!(
            err.contains("proxied public port") && err.contains("redis"),
            "{err}"
        );

        // (2) Two internal services declaring the SAME concrete literal port → rejected.
        let dup_literal = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.env]\nPORT = \"9001\"\n\
             [services.worker]\nentrypoint = \"w\"\n[services.worker.env]\nPORT = \"9001\"\n",
            base_toml()
        );
        let err = derive_supervisor_build_spec(&parse(&dup_literal), &probe_python()).unwrap_err();
        assert!(
            err.contains("single owner") && err.contains("9001"),
            "{err}"
        );

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
        assert_eq!(
            services
                .iter()
                .filter(|s| s.port == Some(spec.port))
                .count(),
            1,
            "one target-port owner"
        );
        let owner = services.iter().find(|s| s.port == Some(spec.port)).unwrap();
        assert!(
            owner.public && owner.name == "api",
            "the target-port owner is the public service"
        );
    }

    #[test]
    fn legacy_single_service_has_no_recorded_public_service() {
        // A legacy single-service build: no [services] → services None, and
        // public_service is None (the sole workload is implicitly the URL target).
        let spec =
            derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(sup.services.is_none(), "legacy single-service build");
        assert!(
            sup.public_service.is_none(),
            "no explicit public service selection for legacy"
        );
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
        assert!(
            script.contains("rootfs/etc/hosts"),
            "multi-service bakes /etc/hosts"
        );
        assert!(
            script.contains("cache"),
            "alias present in the baked hosts file"
        );

        // A duplicate hostname (alias equals another service's name) is fail-closed.
        let dup = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.redis]\nentrypoint = \"r\"\n[services.redis.network]\naliases = [\"api\"]\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&dup), &probe_python())
                .unwrap_err()
                .contains("claimed by both")
        );
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
        let spec =
            derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(
            !script.contains("rootfs/etc/hosts"),
            "single-service must not bake /etc/hosts"
        );
    }

    #[test]
    fn multi_service_build_script_emits_services_and_no_legacy_top_level_cmd() {
        let spec =
            derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let script = build_rootfs_script(&spec, 512);
        assert!(
            script.contains("\"services\""),
            "emits a services[] supervisor.json"
        );
        assert!(
            script.contains("\"name\": \"api\"") && script.contains("\"name\": \"redis\""),
            "{script}"
        );
        assert!(
            script.contains("/usr/local/bin/ato-guest-agent 'openai_api_key'"),
            "agent argv = binding"
        );
        assert!(
            !script.contains("sk-"),
            "no secret value in the rootfs script"
        );
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
        let json = build_supervisor_json(
            spec.supervisor.as_ref().unwrap(),
            spec.port,
            &spec.start_cmd,
        );
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
        let rport = redis
            .base_env
            .get("REDIS_PORT")
            .expect("own placeholder injected");
        assert_ne!(rport, "8080");
        let rport: u16 = rport.parse().unwrap();
        assert!(rport >= 8091, "allocated from the service port base");
        // redis's primary/readiness port is that allocated port.
        assert_eq!(redis.port, Some(rport));

        // api can reach redis on loopback via the cross-referenced env var.
        assert_eq!(
            api.base_env.get("REDIS_REDIS_PORT").map(String::as_str),
            Some(rport.to_string().as_str())
        );
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
        assert_eq!(
            web.base_env.get("HTTP_PORT").map(String::as_str),
            Some("8080"),
            "first expose = target port"
        );
        assert_eq!(
            web.base_env.get("PORT").map(String::as_str),
            Some("8080"),
            "public always gets PORT=target"
        );
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
        assert!(
            derive_supervisor_build_spec(&parse(&toml), &probe_python())
                .unwrap_err()
                .contains("POSIX identifier")
        );
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
        assert!(
            derive_supervisor_build_spec(&parse(&dup), &probe_python())
                .unwrap_err()
                .contains("duplicate expose placeholder")
        );

        // (2) Two (service, placeholder) pairs generate the SAME cross-ref var:
        //     `a-b`+`C` and `a`+`B_C` both → `A_B_C`. Rejected.
        let alias = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.web]\nentrypoint = \"w\"\n[services.web.network]\npublish = true\n\
             [services.a-b]\nentrypoint = \"x\"\nexpose = [\"C\"]\n\
             [services.a]\nentrypoint = \"y\"\nexpose = [\"B_C\"]\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&alias), &probe_python())
                .unwrap_err()
                .contains("cross-reference env var")
        );

        // (3a) Own placeholder env var already declared by the author → rejected.
        let own = expose_collision_toml(
            "[services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n\
             [services.redis.env]\nREDIS_PORT = \"1234\"\n",
        );
        assert!(
            derive_supervisor_build_spec(&parse(&own), &probe_python())
                .unwrap_err()
                .contains("collides with an existing")
        );

        // (3b) Generated cross-reference var already declared by the author → rejected.
        let xref = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\n\
             [services.api.env]\nREDIS_REDIS_PORT = \"1234\"\n\
             [services.redis]\nentrypoint = \"r\"\nexpose = [\"REDIS_PORT\"]\n",
            base_toml()
        );
        assert!(
            derive_supervisor_build_spec(&parse(&xref), &probe_python())
                .unwrap_err()
                .contains("collides with an existing")
        );
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
            svc.iter()
                .find(|s| s.name == "redis")
                .unwrap()
                .port
                .unwrap()
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
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{extra}",
                base_toml()
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains(needle), "expected {needle:?} in: {err}");
        };
        // No public service.
        bad(
            "[services.api]\nentrypoint = \"python3 api.py\"\n",
            "no public service",
        );
        // Two public services.
        bad(
            "[services.a]\nentrypoint = \"a\"\n[services.a.network]\npublish = true\n\
             [services.b]\nentrypoint = \"b\"\n[services.b.network]\npublish = true\n",
            "exactly one may be public",
        );
        // Empty entrypoint.
        bad(
            "[services.api]\nentrypoint = \"\"\n[services.api.network]\npublish = true\n",
            "`entrypoint` is empty",
        );
        // depends_on to an unknown service.
        bad(
            "[services.api]\nentrypoint = \"a\"\ndepends_on = [\"ghost\"]\n[services.api.network]\npublish = true\n",
            "not a declared service",
        );
        // v1.6 (ato#983): a state_bindings entry naming an UNDECLARED state is
        // still rejected (state_bindings itself is no longer a blanket
        // container-only reject — see `durable_state_binding_rules` below).
        bad(
            "[services.api]\nentrypoint = \"a\"\nstate_bindings = [{ state = \"d\", target = \"/ato/state/d\" }]\n[services.api.network]\npublish = true\n",
            "not declared under [state]",
        );
        // Container-only field: egress_proxy opt-out.
        bad(
            "[services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n[services.api.network]\npublish = true\negress_proxy = false\n",
            "egress_proxy = false",
        );
    }

    /// v1.6 (ato#983) Slice 1: durable state volume derivation + fail-closed rules.
    #[test]
    fn durable_state_binding_rules() {
        // A valid, persistent+exclusive `[state.<name>]` schema_id (64 hex chars —
        // this builder doesn't format-check it, but it's kept realistic anyway;
        // the manifest-validation layer does check the format).
        fn schema_id_64() -> String {
            format!("sha256:{}", "0".repeat(64))
        }
        fn state_decl(name: &str, extra: &str) -> String {
            format!(
                "[state.{name}]\nkind = \"filesystem\"\ndurability = \"persistent\"\n\
                 purpose = \"test state\"\nattach = \"explicit\"\nschema_id = \"{}\"\n{extra}",
                schema_id_64()
            )
        }
        let ok = |extra_state: &str, extra_service: &str| -> Result<RootfsBuildSpec, String> {
            // NOTE: `extra_service` (state_bindings) MUST come before
            // `[services.api.network]` — TOML scopes bare keys to the most
            // recently opened table, so a `state_bindings = [...]` line after
            // `[services.api.network]` would silently become a key of THAT
            // sub-table instead of `[services.api]`.
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
                 {extra_state}\n[services.api]\nentrypoint = \"a\"\n\
                 secrets = [\"openai_api_key\"]\n{extra_service}\
                 [services.api.network]\npublish = true\n",
                base_toml()
            );
            derive_supervisor_build_spec(&parse(&toml), &probe_python())
        };
        let bad_state = |extra_state: &str, extra_service: &str, needle: &str| {
            let err = ok(extra_state, extra_service).unwrap_err();
            assert!(err.contains(needle), "expected {needle:?} in: {err}");
        };

        // Accepted: default size_mb.
        let spec = ok(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
        )
        .unwrap();
        {
            let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
            let api = services.iter().find(|s| s.name == "api").unwrap();
            assert_eq!(api.volumes.len(), 1);
            assert_eq!(api.volumes[0].state_name, "dbdata");
            assert_eq!(api.volumes[0].target, "/ato/state/dbdata");
            assert_eq!(api.volumes[0].size_mb, DEFAULT_STATE_VOLUME_SIZE_MB);
            // v1.6 Slice 3: drive_id/fs_label are assigned globally, from the
            // manifest's name (no explicit state_owner_scope declared here).
            assert_eq!(api.volumes[0].drive_id, "state0");
            assert_eq!(
                api.volumes[0].fs_label.len(),
                16,
                "ext4 label must be 16 bytes"
            );
            assert!(api.volumes[0].fs_label.starts_with("AS"));
        }
        // And it DOES appear in the receipt when non-empty.
        let receipt = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            receipt["supervisor"]["services"][0]["volumes"][0]["state_name"],
            "dbdata"
        );
        // v1.6 Slice 3: NOW emitted into the guest-facing supervisor.json too —
        // as a VM-WIDE top-level array (mounts happen once at boot, before any
        // service; not nested under the owning service).
        let sup_json = build_supervisor_json(
            spec.supervisor.as_ref().unwrap(),
            spec.port,
            &spec.start_cmd,
        );
        assert!(
            sup_json["services"][0].get("volumes").is_none(),
            "volumes are top-level, not per-service"
        );
        assert_eq!(sup_json["volumes"][0]["state_name"], "dbdata");
        assert_eq!(sup_json["volumes"][0]["target"], "/ato/state/dbdata");
        assert_eq!(sup_json["volumes"][0]["drive_id"], "state0");
        assert_eq!(
            sup_json["volumes"][0]["size_mb"],
            DEFAULT_STATE_VOLUME_SIZE_MB
        );

        // Accepted: custom in-bounds size_mb, and lexical normalization (trailing
        // slash + repeated slash) both collapse to the same target string.
        let spec = ok(
            &state_decl("dbdata", "size_mb = 4096\n"),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state//dbdata/\" }]\n",
        )
        .unwrap();
        let services = spec.supervisor.unwrap().services.unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert_eq!(api.volumes[0].size_mb, 4096);
        assert_eq!(api.volumes[0].target, "/ato/state/dbdata");

        // size_mb = 0 rejected.
        bad_state(
            &state_decl("dbdata", "size_mb = 0\n"),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
            "out of range",
        );
        // size_mb below MIN (nonzero) rejected.
        bad_state(
            &state_decl("dbdata", "size_mb = 1\n"),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
            "out of range",
        );
        // size_mb above MAX rejected.
        bad_state(
            &state_decl("dbdata", "size_mb = 4294967295\n"),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
            "out of range",
        );

        // durability = "ephemeral" rejected for a snapshot durable-volume binding.
        bad_state(
            "[state.dbdata]\nkind = \"filesystem\"\ndurability = \"ephemeral\"\npurpose = \"x\"\n",
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
            "durability=\"persistent\"",
        );

        // sharing = "same-capsule" rejected in this MVP.
        bad_state(
            &state_decl("dbdata", "sharing = \"same-capsule\"\n"),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\" }]\n",
            "same-capsule",
        );

        // service_target set is rejected (not applicable to a single-VM service).
        bad_state(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/dbdata\", service_target = \"api\" }]\n",
            "service_target",
        );

        // target outside /ato/state/ rejected.
        bad_state(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"/var/lib/dbdata\" }]\n",
            "must be under",
        );
        // target exactly "/ato/state" rejected (needs a real subpath).
        bad_state(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state\" }]\n",
            "not '/ato/state' itself",
        );
        // target with a ".." component rejected.
        bad_state(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"/ato/state/../etc\" }]\n",
            "'.', '..'",
        );
        // relative target rejected.
        bad_state(
            &state_decl("dbdata", ""),
            "state_bindings = [{ state = \"dbdata\", target = \"ato/state/dbdata\" }]\n",
            "absolute path",
        );

        // Same state name bound by two different services → rejected (one owner).
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{}\n\
                 [services.api]\nentrypoint = \"a\"\n\
                 state_bindings = [{{ state = \"dbdata\", target = \"/ato/state/dbdata\" }}]\n\
                 [services.api.network]\npublish = true\n\
                 [services.worker]\nentrypoint = \"w\"\n\
                 state_bindings = [{{ state = \"dbdata\", target = \"/ato/state/dbdata2\" }}]\n",
                base_toml(),
                state_decl("dbdata", "")
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("exactly one owning service"), "{err}");
        }

        // Review fix (ato#991): the SAME service binding the SAME state name
        // twice (even at different targets) must also be rejected — left
        // unchecked, both entries would collide on the same drive_id/fs_label
        // in the global assignment pass, baking an ambiguous supervisor.json.
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{}\n\
                 [services.api]\nentrypoint = \"a\"\n\
                 state_bindings = [\
                     {{ state = \"dbdata\", target = \"/ato/state/dbdata\" }}, \
                     {{ state = \"dbdata\", target = \"/ato/state/dbdata2\" }}\
                 ]\n\
                 [services.api.network]\npublish = true\n",
                base_toml(),
                state_decl("dbdata", "")
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("bound more than once"), "{err}");
        }

        // Two DIFFERENT state names whose targets are identical → rejected.
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{}\n{}\n\
                 [services.api]\nentrypoint = \"a\"\n\
                 state_bindings = [{{ state = \"dba\", target = \"/ato/state/shared\" }}]\n\
                 [services.api.network]\npublish = true\n\
                 [services.worker]\nentrypoint = \"w\"\n\
                 state_bindings = [{{ state = \"dbb\", target = \"/ato/state/shared\" }}]\n",
                base_toml(),
                state_decl("dba", ""),
                state_decl("dbb", "")
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("must not be identical or nested"), "{err}");
        }

        // Two DIFFERENT state names whose targets prefix-overlap → rejected.
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{}\n{}\n\
                 [services.api]\nentrypoint = \"a\"\n\
                 state_bindings = [{{ state = \"dba\", target = \"/ato/state/db\" }}]\n\
                 [services.api.network]\npublish = true\n\
                 [services.worker]\nentrypoint = \"w\"\n\
                 state_bindings = [{{ state = \"dbb\", target = \"/ato/state/db/backup\" }}]\n",
                base_toml(),
                state_decl("dba", ""),
                state_decl("dbb", "")
            );
            let err = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap_err();
            assert!(err.contains("must not be identical or nested"), "{err}");
        }

        // Multiple independent per-service volumes, non-colliding, all accepted.
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n{}\n{}\n\
                 [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n\
                 state_bindings = [{{ state = \"dba\", target = \"/ato/state/api-data\" }}]\n\
                 [services.api.network]\npublish = true\n\
                 [services.worker]\nentrypoint = \"w\"\n\
                 state_bindings = [{{ state = \"dbb\", target = \"/ato/state/worker-data\" }}]\n",
                base_toml(),
                state_decl("dba", ""),
                state_decl("dbb", "")
            );
            let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
            let services = spec.supervisor.unwrap().services.unwrap();
            let api = services.iter().find(|s| s.name == "api").unwrap();
            let worker = services.iter().find(|s| s.name == "worker").unwrap();
            assert_eq!(api.volumes.len(), 1);
            assert_eq!(worker.volumes.len(), 1);
            assert_eq!(api.volumes[0].target, "/ato/state/api-data");
            assert_eq!(worker.volumes[0].target, "/ato/state/worker-data");
            // v1.6 Slice 3: drive_id assigned GLOBALLY across services, sorted
            // by state_name ("dba" < "dbb") — not per-service, not declaration
            // order (worker is declared after api but "dbb" > "dba").
            assert_eq!(api.volumes[0].drive_id, "state0");
            assert_eq!(worker.volumes[0].drive_id, "state1");
            assert_ne!(api.volumes[0].fs_label, worker.volumes[0].fs_label);
        }

        // No declared state at all → volumes stays empty (no behavior change), and
        // the RECEIPT (not the guest-facing supervisor.json, which never carries
        // `volumes` in this slice regardless) omits the empty `volumes` array via
        // `skip_serializing_if` — proving no downstream receipt consumer sees a
        // new field for a capsule that declares no state.
        {
            let toml = format!(
                "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
                 [services.api]\nentrypoint = \"a\"\nsecrets = [\"openai_api_key\"]\n\
                 [services.api.network]\npublish = true\n",
                base_toml()
            );
            let spec = derive_supervisor_build_spec(&parse(&toml), &probe_python()).unwrap();
            let services = spec.supervisor.as_ref().unwrap().services.as_ref().unwrap();
            assert!(
                services
                    .iter()
                    .find(|s| s.name == "api")
                    .unwrap()
                    .volumes
                    .is_empty()
            );
            let receipt = serde_json::to_value(&spec).unwrap();
            assert!(
                receipt["supervisor"]["services"][0]
                    .get("volumes")
                    .is_none(),
                "volumes must be omitted from the receipt when empty: {receipt}"
            );
        }
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
        assert!(
            err.contains("proxied port") && err.contains("3000"),
            "{err}"
        );

        // Declaring the SAME port is fine (redundant but honest).
        let same = format!(
            "{}\n[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n\
             [services.api]\nentrypoint = \"node server.js\"\nsecrets = [\"openai_api_key\"]\n[services.api.env]\nPORT = \"8080\"\n\
             [services.api.network]\npublish = true\n",
            base_toml()
        );
        let spec = derive_supervisor_build_spec(&parse(&same), &probe_python()).unwrap();
        let json = build_supervisor_json(
            spec.supervisor.as_ref().unwrap(),
            spec.port,
            &spec.start_cmd,
        );
        assert_eq!(json["services"][0]["base_env"]["PORT"], "8080");

        // Absent PORT → the builder injects the target port.
        let spec =
            derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        let json = build_supervisor_json(
            spec.supervisor.as_ref().unwrap(),
            spec.port,
            &spec.start_cmd,
        );
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
        assert!(
            derive_supervisor_build_spec(&parse(&bad_alias), &probe_python())
                .unwrap_err()
                .contains("DNS-safe")
        );

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
        let spec =
            derive_supervisor_build_spec(&parse(&multi_service_toml()), &probe_python()).unwrap();
        assert_eq!(spec.port, 8080, "port comes from the build target");
        let sup = spec.supervisor.as_ref().unwrap();
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        let api = &json["services"].as_array().unwrap()[0];
        assert_eq!(api["name"], "api");
        assert_eq!(
            api["base_env"]["PORT"], "8080",
            "public service listens on the proxied port"
        );
    }

    #[test]
    fn single_service_supervisor_json_stays_byte_identical() {
        // A legacy (no [services]) supervisor build must emit the OLD top-level shape.
        let spec =
            derive_supervisor_build_spec(&parse(&supervisor_toml()), &probe_python()).unwrap();
        let sup = spec.supervisor.as_ref().unwrap();
        assert!(
            sup.services.is_none(),
            "no [services] ⇒ legacy single-service build"
        );
        let json = build_supervisor_json(sup, spec.port, &spec.start_cmd);
        assert!(
            json.get("services").is_none(),
            "legacy build emits top-level cmd, not services[]"
        );
        assert_eq!(
            json["cmd"],
            serde_json::json!(["/bin/sh", "-lc", "python3 app.py"])
        );
        assert_eq!(json["base_env"]["PORT"], spec.port.to_string());
        assert_eq!(json["bindings_env"]["OPENAI_API_KEY"], "openai_api_key");
    }
}
