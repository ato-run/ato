//! v1.7 Dockerfile-to-Snapshot Import (ato#994) — imported-image → Ato rootfs.
//!
//! Maps the built image's runtime config (slice 3's [`DockerImageConfig`]) into
//! the SAME supervisor build types the v1.5 multi-service path emits
//! ([`SupervisorBuildSpec`]/[`ServiceBuildSpec`]), then packs the image through
//! the SAME export→inject→init→pack bash pipeline the legacy builder uses
//! (`rootfs_builder::rootfs_pack_script`) — the only difference is the acquire
//! step: the image is already built, so there is no generated Dockerfile and no
//! source copy. The restored guest therefore runs the ordinary Ato guest-agent +
//! supervisor; nothing Docker-shaped survives into the snapshot.
//!
//! v0 mapping rules (the #994 track):
//! * `ENTRYPOINT` + `CMD` → the service argv (exec-form concatenation, Docker's
//!   own semantics; shell-form was already normalized to `/bin/sh -c` by the
//!   build tool). Empty argv fails closed.
//! * `WORKDIR` → service `cwd` (default `/`).
//! * `ENV` → `base_env` / required bindings via the slice-2 secret gate
//!   ([`partition_dockerfile_env`]) — a secret-looking literal REJECTS the
//!   import; placeholders convert only under explicit policy.
//! * `EXPOSE` → the single public port (explicit override wins; exactly-one
//!   EXPOSE required otherwise — v0 imports a single public web service).
//! * `USER` → NOT honored: [`DockerImportWarning::DockerUserIgnored`].
//! * `HEALTHCHECK` → NOT honored (ReadinessSpec is port+http_path only):
//!   [`DockerImportWarning::DockerHealthcheckIgnored`]; readiness is the
//!   synthesized `GET /` unless a path is supplied.
//! * `VOLUME` → fail-closed until mapped to Ato `[state]` (ato#983 lesson:
//!   unmapped mutable state silently dies on a frozen-snapshot resume).

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::rootfs_builder::{
    PackScriptInputs, ServiceBuildSpec, SupervisorBuildSpec, rootfs_pack_script,
    shell_single_quote, supervisor_prep_and_launch,
};

use super::build::DockerImageConfig;
use super::{DockerImportWarning, EnvPartition, SecretEnvPolicy, partition_dockerfile_env};

/// The service name every v0 import emits (one single public web service).
pub const IMPORTED_SERVICE_NAME: &str = "app";

/// The fully-derived launch plan for an imported image: one public service in
/// the v1.5 supervisor shape + the import warnings the receipt records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedServicePlan {
    pub supervisor: SupervisorBuildSpec,
    /// The single proxied guest port (the public service's listener).
    pub port: u16,
    /// Readiness path (`None` = synthesized `GET /`, mirroring the legacy
    /// builder's `probe_synthesized` contract).
    pub readiness_http_path: Option<String>,
    /// Size-capped ephemeral tmpfs mounts, with optional copy-up seeding
    /// (ato#1024 generalized). Normalized + sorted by path; non-empty ONLY when
    /// the job opted in (legacy `volumes=tmpfs`, structured `volumes`, or
    /// explicit `ephemeral_mounts`). The receipt records these so it is
    /// auditable that the artifact's mutable state is deliberately ephemeral
    /// (dies on stop/resume; acceptable for the throwaway preview lane, still
    /// lossy for a durable install exactly as ato#983 warned — hence opt-in +
    /// receipt, never a default).
    pub ephemeral_mounts: Vec<EphemeralMountSpec>,
    /// ato#1026: when true, the generated init starts an `ato-guest-agent
    /// tcp-relay` from the guest's own IP:port → `127.0.0.1:port`, so an app
    /// that binds only loopback is reachable by the readiness probe and the
    /// app-proxy (both dial the guest's routable IP). Opt-in per import
    /// (`host_bind_relay`); recorded in the receipt ⇒ new artifact identity.
    pub host_bind_relay: bool,
    pub warnings: Vec<DockerImportWarning>,
}

/// How the job asked to treat IMAGE-declared VOLUMEs (ato#1024). Explicit
/// `ephemeral_mounts` are a separate, image-independent input (Phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumePolicy {
    /// Fail closed on any image VOLUME that no ephemeral mount covers (the
    /// ato#983 default, unchanged).
    #[default]
    Reject,
    /// Mount each declared image VOLUME as guest tmpfs — state is EXPECTED to
    /// die. `size_mib` caps each such mount (`None` = uncapped, legacy shape).
    Tmpfs { size_mib: Option<u32> },
}

/// How an ephemeral mount is initialized before the app runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EphemeralMountSeed {
    /// Fresh empty tmpfs (the legacy `volumes=tmpfs` behavior).
    Empty,
    /// Copy the image's existing directory contents INTO the tmpfs at boot, so
    /// an app that ships defaults under the path (e.g. `/app/config`) sees them
    /// on a writable overlay (writes still die on stop/resume).
    CopyUp,
}

/// Where an ephemeral mount came from — audit provenance in the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EphemeralMountSource {
    /// Expanded from an image-declared VOLUME under `VolumePolicy::Tmpfs`.
    ImageVolume,
    /// Declared explicitly by the job's `ephemeral_mounts`, independent of any
    /// image VOLUME.
    Explicit,
}

/// One static, recipe-owned seed file for an ephemeral mount. `source_digest`
/// is EMPTY at request/parse time and filled by build-time staging
/// (`seed_files::stage_all_mounts`) — only digest-filled specs reach the
/// receipt/identity envelope. All four fields serialize: the destination, the
/// recipe-root source path, the content blake3, and the write mode are each
/// identity inputs (a content change flips `source_digest` ⇒ a new artifact).
/// The CONTENT itself never lives on this type (never in a receipt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EphemeralSeedFile {
    /// Destination RELATIVE to the mountpoint (e.g. `config.yml`).
    pub path: String,
    /// Source RELATIVE to the recipe root (e.g. `recipe/config.yml`).
    pub source_path: String,
    /// `blake3:<hex>` of the source bytes — filled at staging, an identity input.
    pub source_digest: String,
    /// Only write when the (copy-up) seed didn't already provide the file.
    pub if_missing: bool,
}

/// One normalized ephemeral tmpfs mount. Serialized into the receipt + the
/// import identity envelope (path+seed+size_mib+source+files), so any change
/// moves the artifact identity; the normalized+sorted order (mounts by path,
/// files by destination) makes identity stable regardless of input order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EphemeralMountSpec {
    pub path: String,
    pub seed: EphemeralMountSeed,
    /// tmpfs size cap in MiB (`None` = uncapped). Enforced against the builder
    /// config caps upstream (`snapshot-builder`), `>= 1` here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_mib: Option<u32>,
    pub source: EphemeralMountSource,
    /// Static, recipe-owned seed files written into the mount at boot (after
    /// the mount + any copy-up). Skipped when EMPTY so every file-less mount
    /// keeps the exact pre-fold serialization (and every no-mount import keeps
    /// the legacy descriptor envelope byte-identical).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<EphemeralSeedFile>,
}

/// Paths an ephemeral mount may never shadow with tmpfs: mounting over these
/// would replace the OS / the app itself / an already-tmpfs mount rather than
/// the app's data directory, so they fail closed.
const TMPFS_FORBIDDEN_PREFIXES: &[&str] = &[
    "/proc", "/sys", "/dev", "/sbin", "/bin", "/usr", "/lib", "/lib64", "/etc", "/tmp", "/run",
    "/var/tmp",
];

/// Validate one ephemeral mount path. The path is rendered into the QUOTED init
/// heredoc, so the character set is restricted to what cannot break out of
/// `mount -t tmpfs tmpfs <path>` (no whitespace, quotes, or shell
/// metacharacters — fail-closed rather than escaped).
pub fn validate_ephemeral_mount_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path == "/" {
        return Err(format!(
            "ephemeral mount {path:?} is not an absolute non-root path"
        ));
    }
    if path.len() > 200 {
        return Err(format!("ephemeral mount {path:?} exceeds 200 chars"));
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    {
        return Err(format!(
            "ephemeral mount {path:?} contains characters outside [A-Za-z0-9/_.-] — refusing to render it into the guest init (fail-closed)"
        ));
    }
    if path.contains("..") {
        return Err(format!("ephemeral mount {path:?} contains '..'"));
    }
    let normalized = path.trim_end_matches('/');
    for forbidden in TMPFS_FORBIDDEN_PREFIXES {
        if normalized == *forbidden || normalized.starts_with(&format!("{forbidden}/")) {
            return Err(format!(
                "ephemeral mount {path:?} would tmpfs-shadow {forbidden} (OS/app surface, not app data) — fail-closed"
            ));
        }
    }
    Ok(())
}

/// Path with any trailing `/` stripped, for overlap/duplicate comparison
/// (`/data` and `/data/` are the same mountpoint).
fn normalize_mount_path(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    if t.is_empty() { "/" } else { t }
}

/// Fail-closed validation of the FULL normalized mount set (image-volume +
/// explicit): each path shell-safe + non-forbidden, each `size_mib >= 1`, no
/// duplicate mountpoint, no parent/child overlap between any two mounts (a
/// tmpfs over a parent would hide a sibling child mount). Per-mount seed FILES
/// are validated here too — destination mount-relative + shell-safe, source
/// recipe-root-relative (lexically; symlink/containment checks need the
/// filesystem and land in `seed_files` staging), no duplicate destination —
/// this is THE single structural gate for the whole unified mount contract.
pub fn validate_ephemeral_mounts(mounts: &[EphemeralMountSpec]) -> Result<(), String> {
    for m in mounts {
        validate_ephemeral_mount_path(&m.path)?;
        if m.size_mib == Some(0) {
            return Err(format!(
                "ephemeral mount {:?} size_mib must be >= 1",
                m.path
            ));
        }
        let mut dests: Vec<&str> = Vec::with_capacity(m.files.len());
        for f in &m.files {
            super::seed_files::validate_seed_dest(&f.path)?;
            super::seed_files::validate_seed_source(&f.source_path)?;
            if dests.contains(&f.path.as_str()) {
                return Err(format!(
                    "ephemeral mount {:?} declares seed dest {:?} twice (fail-closed)",
                    m.path, f.path
                ));
            }
            dests.push(&f.path);
        }
    }
    for i in 0..mounts.len() {
        for j in (i + 1)..mounts.len() {
            let a = normalize_mount_path(&mounts[i].path);
            let b = normalize_mount_path(&mounts[j].path);
            if a == b {
                return Err(format!("duplicate ephemeral mount path {a:?}"));
            }
            if b.starts_with(&format!("{a}/")) || a.starts_with(&format!("{b}/")) {
                return Err(format!(
                    "ephemeral mounts {a:?} and {b:?} overlap (one is nested under the other) — fail-closed"
                ));
            }
        }
    }
    Ok(())
}

/// Normalize the image-VOLUME policy + explicit mounts into the final, sorted,
/// fully-validated mount set. Explicit mounts are image-independent; under
/// `VolumePolicy::Tmpfs` each image VOLUME not already covered by an explicit
/// mount (exact mountpoint) is added as an empty ImageVolume mount; under
/// `Reject` any image VOLUME left uncovered fails closed (ato#983).
fn resolve_ephemeral_mounts(
    image_volumes: &[String],
    volume_policy: VolumePolicy,
    explicit: Vec<EphemeralMountSpec>,
) -> Result<Vec<EphemeralMountSpec>, String> {
    let covered: std::collections::BTreeSet<String> = explicit
        .iter()
        .map(|m| normalize_mount_path(&m.path).to_string())
        .collect();
    let mut mounts = explicit;
    match volume_policy {
        VolumePolicy::Reject => {
            let uncovered: Vec<&str> = image_volumes
                .iter()
                .filter(|v| !covered.contains(normalize_mount_path(v)))
                .map(|s| s.as_str())
                .collect();
            if !uncovered.is_empty() {
                return Err(format!(
                    "image declares VOLUME {} — unmapped mutable state would silently die on a \
                     frozen-snapshot resume (ato#983). Map it to Ato [state] + state_bindings, \
                     opt in to ephemeral tmpfs mapping with volumes=tmpfs (ato#1024), declare an \
                     ephemeral_mounts entry for it, or drop the VOLUME",
                    uncovered.join(", ")
                ));
            }
        }
        VolumePolicy::Tmpfs { size_mib } => {
            for v in image_volumes {
                if covered.contains(normalize_mount_path(v)) {
                    continue; // an explicit mount already owns this path (richer)
                }
                mounts.push(EphemeralMountSpec {
                    path: v.clone(),
                    seed: EphemeralMountSeed::Empty,
                    size_mib,
                    source: EphemeralMountSource::ImageVolume,
                    files: Vec::new(),
                });
            }
        }
    }
    // Sort by normalized mountpoint (and each mount's files by destination) so
    // identity is input-order-independent and the copy-up seed index is
    // deterministic.
    mounts.sort_by(|a, b| normalize_mount_path(&a.path).cmp(normalize_mount_path(&b.path)));
    for m in &mut mounts {
        m.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    validate_ephemeral_mounts(&mounts)?;
    Ok(mounts)
}

/// Derive the v0 import plan from an image config. Fail-closed on: empty argv,
/// `VOLUME` directives, secret-looking env literals (via the slice-2 gate),
/// zero or multiple `EXPOSE` ports without an explicit override.
pub fn derive_imported_service_plan(
    config: &DockerImageConfig,
    policy: SecretEnvPolicy,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
) -> Result<ImportedServicePlan, String> {
    derive_imported_service_plan_with_volumes(
        config,
        policy,
        port_override,
        readiness_http_path,
        VolumePolicy::Reject,
        false,
    )
}

/// [`derive_imported_service_plan`] with an explicit [`VolumePolicy`] (ato#1024)
/// and the ato#1026 localhost-relay opt-in. No explicit ephemeral mounts.
pub fn derive_imported_service_plan_with_volumes(
    config: &DockerImageConfig,
    policy: SecretEnvPolicy,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
    volume_policy: VolumePolicy,
    host_bind_relay: bool,
) -> Result<ImportedServicePlan, String> {
    derive_imported_service_plan_with_mounts(
        config,
        policy,
        port_override,
        readiness_http_path,
        volume_policy,
        Vec::new(),
        host_bind_relay,
    )
}

/// The richest import-plan entry point (Phase 1): image-VOLUME policy PLUS
/// explicit, image-independent ephemeral mounts (with optional copy-up seeding
/// and per-mount size caps). All forms normalize into the plan's single sorted
/// [`EphemeralMountSpec`] list.
pub fn derive_imported_service_plan_with_mounts(
    config: &DockerImageConfig,
    policy: SecretEnvPolicy,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
    volume_policy: VolumePolicy,
    explicit_mounts: Vec<EphemeralMountSpec>,
    host_bind_relay: bool,
) -> Result<ImportedServicePlan, String> {
    // Docker exec semantics: ENTRYPOINT is the argv head, CMD its default args
    // (both already exec-form in image config). Neither alone is unusual but
    // legal; both empty means the image cannot start.
    let mut cmd: Vec<String> = Vec::with_capacity(config.entrypoint.len() + config.cmd.len());
    cmd.extend(config.entrypoint.iter().cloned());
    cmd.extend(config.cmd.iter().cloned());
    if cmd.is_empty() {
        return Err(
            "image declares neither ENTRYPOINT nor CMD — nothing to start (fail-closed)".into(),
        );
    }

    let ephemeral_mounts =
        resolve_ephemeral_mounts(&config.volumes, volume_policy, explicit_mounts)?;

    let port = match (port_override, config.exposed_tcp_ports.as_slice()) {
        (Some(p), _) => p,
        (None, [single]) => *single,
        (None, []) => {
            return Err(
                "image EXPOSEs no tcp port and no explicit port was supplied — v0 imports a \
                 single public web service (fail-closed)"
                    .into(),
            );
        }
        (None, many) => {
            return Err(format!(
                "image EXPOSEs {} tcp ports ({}) — v0 imports a single public web service; \
                 supply the public port explicitly",
                many.len(),
                many.iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };

    let EnvPartition {
        mut base_env,
        bindings_env,
    } = partition_dockerfile_env(&config.env, policy)?;
    // Same invariant as the legacy multi-service derive: the public service
    // listens on the proxied port; PORT is injected idempotently (an image
    // that already sets PORT keeps its own value only if it matches).
    if let Some(declared) = base_env.get("PORT")
        && declared != &port.to_string()
    {
        return Err(format!(
            "image env PORT = {declared:?} but the public port is {port} — the public \
             service must listen on the single proxied port (fail-closed)"
        ));
    }
    base_env
        .entry("PORT".to_string())
        .or_insert_with(|| port.to_string());

    let mut warnings = Vec::new();
    if let Some(user) = config.user.as_deref() {
        // root-equivalent forms are the guest's model anyway — only a real
        // user mapping is "ignored".
        if !matches!(user, "root" | "0" | "0:0" | "root:root") {
            warnings.push(DockerImportWarning::DockerUserIgnored);
        }
    }
    if config.has_healthcheck {
        warnings.push(DockerImportWarning::DockerHealthcheckIgnored);
    }

    let mut binding_names: Vec<String> = bindings_env.values().cloned().collect();
    binding_names.sort();
    binding_names.dedup();

    let service = ServiceBuildSpec {
        name: IMPORTED_SERVICE_NAME.to_string(),
        run_once: false,
        cmd,
        cwd: config
            .working_dir
            .clone()
            .unwrap_or_else(|| "/".to_string()),
        base_env,
        env_map: bindings_env.clone(),
        public: true,
        depends_on: Vec::new(),
        aliases: Vec::new(),
        readiness_http_path: readiness_http_path.clone(),
        port: Some(port),
        volumes: Vec::new(),
    };
    let supervisor = SupervisorBuildSpec {
        stdio_mode: crate::rootfs_builder::SupervisorStdioMode::Log,
        binding_names,
        env_map: bindings_env,
        services: Some(vec![service]),
        public_service: Some(IMPORTED_SERVICE_NAME.to_string()),
        // Phase 7: a Dockerfile import declares no recipe-owned generated
        // internal bindings (those come from a capsule.toml recipe).
        generated_bindings: Vec::new(),
    };
    Ok(ImportedServicePlan {
        supervisor,
        port,
        readiness_http_path,
        ephemeral_mounts,
        host_bind_relay,
        warnings,
    })
}

/// Render one ephemeral mount into the guest init (goes into `extra_mounts`,
/// after the standard tmpfs mounts, before `cd`): the tmpfs mount, the optional
/// copy-up seeding, and the mount's static seed-file writes — all from the ONE
/// normalized plan entry. `index` uniquifies the copy-up seed staging dir;
/// `seeds` carries the build-time file CONTENT for this mount (aligned by the
/// caller). A failed mount, copy, or seed write fails guest boot (`exit 1`) —
/// these are MANAGED state mounts, never `2>/dev/null`. Paths are pre-validated
/// (`validate_ephemeral_mount_path` / `validate_seed_dest`) so plain
/// interpolation is shell-safe; file content is embedded base64 and decoded
/// in-guest, so arbitrary bytes need no shell escaping.
fn render_ephemeral_mount(
    index: usize,
    m: &EphemeralMountSpec,
    seeds: &[super::seed_files::RenderedSeedFile],
) -> String {
    let path = &m.path;
    let size_opt = m
        .size_mib
        .map(|n| format!(" -o size={n}m"))
        .unwrap_or_default();
    let mount_line = format!(
        "mount -t tmpfs{size_opt} tmpfs {path} || {{ echo \"required tmpfs mount failed: {path}\" >&2; exit 1; }}\n"
    );
    let mut out = match m.seed {
        EphemeralMountSeed::Empty => format!("mkdir -p {path}\n{mount_line}"),
        EphemeralMountSeed::CopyUp => {
            let seed = format!("/run/ato/seed/{index}");
            format!(
                "seed={seed}; mkdir -p \"$seed\"\n\
                 if [ -d {path} ]; then cp -a {path}/. \"$seed/\"; \
                 elif [ -e {path} ]; then echo \"ephemeral mountpoint is not a directory: {path}\" >&2; exit 1; fi\n\
                 mkdir -p {path}\n\
                 {mount_line}\
                 cp -a \"$seed/.\" {path}/\n"
            )
        }
    };
    for f in seeds {
        out.push_str(&super::seed_files::render_seed_file_write(f));
    }
    out
}

/// The pack script for an ALREADY-BUILT imported image: same shared pipeline as
/// the legacy builder (create → export → inject agent/supervisor.json → init →
/// ext4), with an empty acquire step, the imported tag single-quoted into
/// `TAG=`, the chosen container tool, and init cwd `/` (the imported image's
/// own WORKDIR is honored per-service via supervisor.json `cwd` — `/app` is a
/// legacy-build layout assumption that need not exist here).
pub(crate) fn imported_pack_script(
    tool: &str,
    image_tag: &str,
    plan: &ImportedServicePlan,
    seed_contents: &[super::seed_files::RenderedMountSeeds],
    size_mib: u64,
    pixel_rfb_port: Option<u16>,
) -> String {
    let (agent_prep, launch) = supervisor_prep_and_launch(
        Some(&plan.supervisor),
        plan.port,
        /* start_cmd unused in services shape */ "",
    );
    // Stage every ephemeral mountpoint (and each seed file's parent directory)
    // into the rootfs AT PACK TIME, while the ext4 tree is writable. The guest
    // root is mounted READ-ONLY: a boot-time `mkdir -p` can only no-op on a
    // directory the image already ships — an image that declares VOLUME
    // /downloads without RUN mkdir (metube, youtube-dl-server, homepage's
    // /app/config…) has NO such directory in its export, so the mount target
    // must be created here or the fail-closed mount check kills the boot.
    let mut agent_prep = agent_prep;
    // The supervisor prep ends with the supervisor.json QUOTED heredoc whose
    // terminator (`ATOSUPERVISORJSON`) must be ALONE on its line — appending
    // our mkdir right after it (no separating newline) folds the mkdir into
    // the terminator line, so the heredoc never closes, the whole pack script
    // mis-parses, and the ext4 is never written (surfacing downstream as the
    // `No such file or directory (os error 2)` from the metadata() call).
    // Guarantee the separation.
    if !agent_prep.is_empty() && !agent_prep.ends_with('\n') {
        agent_prep.push('\n');
    }
    for m in &plan.ephemeral_mounts {
        // Paths passed validate_ephemeral_mount_path (shell-safe charset).
        agent_prep.push_str(&format!("mkdir -p \"$BUILD/rootfs{}\"\n", m.path));
    }
    for sm in seed_contents {
        for f in &sm.files {
            if let Some((parent, _)) = f.abs_dest.rsplit_once('/')
                && !parent.is_empty()
            {
                agent_prep.push_str(&format!("mkdir -p \"$BUILD/rootfs{parent}\"\n"));
            }
        }
    }
    let healthcheck = plan
        .readiness_http_path
        .clone()
        .unwrap_or_else(|| "/".to_string());
    // Render each normalized ephemeral mount — the tmpfs mount, the copy-up
    // seeding, AND the mount's static seed-file writes come from the ONE plan
    // entry (`seed_contents` carries only the build-time file bytes, looked up
    // by mountpoint; alignment is validated fail-closed in
    // `pack_imported_rootfs` before this renders). Paths were validated
    // fail-closed at plan derivation, so plain interpolation into the quoted
    // init heredoc is safe. Unlike the standard /tmp /run /var/tmp compat
    // mounts, these are MANAGED state mounts — a failed mount/copy/write MUST
    // fail guest boot (exit 1), never `2>/dev/null`.
    let mut extra_mounts: String = plan
        .ephemeral_mounts
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let seeds = seed_contents
                .iter()
                .find(|s| s.path == m.path)
                .map(|s| s.files.as_slice())
                .unwrap_or(&[]);
            render_ephemeral_mount(i, m, seeds)
        })
        .collect();
    // Pixel Stream v1: a terminal-class GUI workload allocates ptys (xterm →
    // /dev/ptmx), and the minimal guest init never mounts devpts. Mount it ONLY
    // for a pixel build — fail-closed like the managed mounts above (a pixel
    // guest without ptys boots into a torn-down fixture) — so every Web import
    // keeps a byte-identical init (same rootfs digest, same identity).
    if pixel_rfb_port.is_some() {
        extra_mounts.push_str(
            "mkdir -p /dev/pts\nmount -t devpts devpts /dev/pts || { echo \"required devpts mount failed: /dev/pts\" >&2; exit 1; }\n",
        );
    }
    // ato#1026: start the localhost→guest-IP relay BEFORE the app launch. The
    // guest-agent resolves its own IP from /proc/cmdline (no shell IP-parsing,
    // no iproute2 in the app image) and the relay retries the loopback target
    // while the app finishes binding, so starting it first is safe. `port` is
    // a u16 rendered as digits — nothing to quote.
    let extra_prelaunch: String = if plan.host_bind_relay {
        format!(
            "/usr/local/bin/ato-guest-agent tcp-relay --listen-guest-port {port} \
             --target 127.0.0.1:{port} >/tmp/ato-relay.log 2>&1 &\n",
            port = plan.port
        )
    } else {
        String::new()
    };
    rootfs_pack_script(&PackScriptInputs {
        tool,
        tag_init: format!("TAG={}", shell_single_quote(image_tag)),
        acquire: String::new(),
        agent_prep,
        // Imported terminal ownership is explicit: only Pixel State appends a
        // fail-closed devpts mount through `extra_mounts` above.
        mount_supervisor_devpts: false,
        launch,
        init_cwd: "/",
        port: plan.port,
        healthcheck,
        size_mib,
        extra_mounts,
        extra_prelaunch,
    })
}

/// Pack an imported image into a bootable ext4 rootfs at `out_ext4`. Same host
/// requirements + env contract as the legacy `build_rootfs`: root (mount),
/// the chosen container tool, and `ATO_GUEST_AGENT_BIN` pointing at the
/// guest-agent binary (imports ALWAYS run the supervisor — with an empty
/// binding set the gate is vacuously bound-ready, so no-secret images start
/// immediately). The imported image is removed by the script's cleanup trap
/// (it is an ephemeral build artifact once exported).
pub fn pack_imported_rootfs(
    tool: super::BuildTool,
    image_tag: &str,
    plan: &ImportedServicePlan,
    seed_contents: &[super::seed_files::RenderedMountSeeds],
    out_ext4: &Path,
    size_mib: u64,
    pixel_rfb_port: Option<u16>,
) -> Result<u64, String> {
    validate_mount_seed_alignment(plan, seed_contents)?;
    let script = imported_pack_script(
        tool.as_str(),
        image_tag,
        plan,
        seed_contents,
        size_mib,
        pixel_rfb_port,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("ATO_OUT", out_ext4)
        .output()
        .map_err(|e| format!("spawn imported rootfs pack: {e}"))?;
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
        return Err(format!("imported rootfs pack failed: {tail}"));
    }
    std::fs::metadata(out_ext4)
        .map(|m| m.len())
        .map_err(|e| e.to_string())
}

/// Fail-closed alignment between the plan's declared seed files (identity) and
/// the staged content table (what the init will actually write). A plan mount
/// declaring N files MUST have exactly N staged contents; content for an
/// unknown mountpoint is equally fatal. Anything less would seal an artifact
/// whose identity promises files its init never writes (or vice versa).
fn validate_mount_seed_alignment(
    plan: &ImportedServicePlan,
    seed_contents: &[super::seed_files::RenderedMountSeeds],
) -> Result<(), String> {
    for s in seed_contents {
        if !plan.ephemeral_mounts.iter().any(|m| m.path == s.path) {
            return Err(format!(
                "staged seed content for {:?} has no matching ephemeral mount in the plan (fail-closed)",
                s.path
            ));
        }
    }
    for m in &plan.ephemeral_mounts {
        let staged = seed_contents
            .iter()
            .find(|s| s.path == m.path)
            .map(|s| s.files.len())
            .unwrap_or(0);
        if staged != m.files.len() {
            return Err(format!(
                "ephemeral mount {:?} declares {} seed file(s) but {} were staged — refusing to pack a rootfs whose identity and init disagree (fail-closed)",
                m.path,
                m.files.len(),
                staged
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn config() -> DockerImageConfig {
        DockerImageConfig {
            entrypoint: vec!["docker-entrypoint.sh".into()],
            cmd: vec!["node".into(), "server.js".into()],
            working_dir: Some("/srv/app".into()),
            env: BTreeMap::from([
                ("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string()),
                ("NODE_ENV".to_string(), "production".to_string()),
            ]),
            exposed_tcp_ports: vec![3000],
            user: None,
            has_healthcheck: false,
            volumes: vec![],
        }
    }

    #[test]
    fn plan_maps_entrypoint_cmd_workdir_env_expose() {
        let plan =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        assert_eq!(plan.port, 3000);
        let svcs = plan.supervisor.services.as_ref().unwrap();
        assert_eq!(svcs.len(), 1);
        let s = &svcs[0];
        assert_eq!(s.name, IMPORTED_SERVICE_NAME);
        assert_eq!(s.cmd, vec!["docker-entrypoint.sh", "node", "server.js"]);
        assert_eq!(s.cwd, "/srv/app");
        assert_eq!(s.base_env["NODE_ENV"], "production");
        assert_eq!(s.base_env["PORT"], "3000"); // injected idempotently
        assert!(s.public);
        assert_eq!(s.port, Some(3000));
        assert_eq!(
            plan.supervisor.public_service.as_deref(),
            Some(IMPORTED_SERVICE_NAME)
        );
        assert!(plan.warnings.is_empty());
        assert!(plan.supervisor.binding_names.is_empty());
    }

    #[test]
    fn empty_argv_and_volumes_fail_closed() {
        let mut c = config();
        c.entrypoint.clear();
        c.cmd.clear();
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap_err();
        assert!(err.contains("neither ENTRYPOINT nor CMD"), "{err}");

        let mut c = config();
        c.volumes = vec!["/data".into()];
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap_err();
        assert!(
            err.contains("VOLUME /data") && err.contains("[state]"),
            "{err}"
        );
    }

    // --- Phase 1 ephemeral mounts (ato#1024 generalized) ----------------------

    fn mount(
        path: &str,
        seed: EphemeralMountSeed,
        size_mib: Option<u32>,
        source: EphemeralMountSource,
    ) -> EphemeralMountSpec {
        EphemeralMountSpec {
            path: path.into(),
            seed,
            size_mib,
            source,
            files: Vec::new(),
        }
    }

    #[test]
    fn tmpfs_policy_expands_image_volumes_and_records_them() {
        let mut c = config();
        c.volumes = vec!["/data".into(), "/downloads".into()];
        let plan = derive_imported_service_plan_with_volumes(
            &c,
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Tmpfs { size_mib: None },
            false,
        )
        .unwrap();
        // Legacy volumes=tmpfs ⇒ empty seed, no size, source=ImageVolume; sorted by path.
        assert_eq!(
            plan.ephemeral_mounts,
            vec![
                mount(
                    "/data",
                    EphemeralMountSeed::Empty,
                    None,
                    EphemeralMountSource::ImageVolume
                ),
                mount(
                    "/downloads",
                    EphemeralMountSeed::Empty,
                    None,
                    EphemeralMountSource::ImageVolume
                ),
            ]
        );
        // A structured size flows through to every expanded image-VOLUME mount.
        let plan = derive_imported_service_plan_with_volumes(
            &c,
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Tmpfs {
                size_mib: Some(512),
            },
            false,
        )
        .unwrap();
        assert!(
            plan.ephemeral_mounts
                .iter()
                .all(|m| m.size_mib == Some(512))
        );
        // The default (Reject) path never populates the field.
        let plan =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        assert!(plan.ephemeral_mounts.is_empty());
    }

    #[test]
    fn explicit_mounts_are_image_independent_and_normalize() {
        // Explicit ephemeral_mounts work under the default Reject policy (the
        // image declares no VOLUME here) and normalize into the sorted list.
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![
                mount(
                    "/downloads",
                    EphemeralMountSeed::Empty,
                    Some(512),
                    EphemeralMountSource::Explicit,
                ),
                mount(
                    "/config",
                    EphemeralMountSeed::CopyUp,
                    Some(16),
                    EphemeralMountSource::Explicit,
                ),
            ],
            false,
        )
        .unwrap();
        // Sorted by mountpoint (/config before /downloads).
        assert_eq!(plan.ephemeral_mounts[0].path, "/config");
        assert_eq!(plan.ephemeral_mounts[0].seed, EphemeralMountSeed::CopyUp);
        assert_eq!(plan.ephemeral_mounts[1].path, "/downloads");
    }

    fn seed_file(dest: &str) -> EphemeralSeedFile {
        EphemeralSeedFile {
            path: dest.into(),
            source_path: format!("recipe/{dest}"),
            source_digest: String::new(),
            if_missing: false,
        }
    }

    #[test]
    fn mount_files_are_structurally_validated_in_the_single_gate() {
        // Duplicate destination within one mount.
        let mut m = mount(
            "/config",
            EphemeralMountSeed::Empty,
            None,
            EphemeralMountSource::Explicit,
        );
        m.files = vec![seed_file("config.yml"), seed_file("config.yml")];
        let err = validate_ephemeral_mounts(std::slice::from_ref(&m)).unwrap_err();
        assert!(err.contains("twice"), "{err}");

        // A dest escaping the mount and an absolute source are both rejected here
        // (lexically), before any staging touches the filesystem.
        let mut m = mount(
            "/config",
            EphemeralMountSeed::Empty,
            None,
            EphemeralMountSource::Explicit,
        );
        m.files = vec![EphemeralSeedFile {
            path: "../evil.yml".into(),
            source_path: "recipe/config.yml".into(),
            source_digest: String::new(),
            if_missing: false,
        }];
        assert!(
            validate_ephemeral_mounts(std::slice::from_ref(&m))
                .unwrap_err()
                .contains("seed file dest")
        );
        let mut m = mount(
            "/config",
            EphemeralMountSeed::Empty,
            None,
            EphemeralMountSource::Explicit,
        );
        m.files = vec![EphemeralSeedFile {
            path: "config.yml".into(),
            source_path: "/etc/passwd".into(),
            source_digest: String::new(),
            if_missing: false,
        }];
        assert!(
            validate_ephemeral_mounts(std::slice::from_ref(&m))
                .unwrap_err()
                .contains("seed file source")
        );
    }

    #[test]
    fn pack_script_renders_mount_copyup_and_seed_writes_from_one_plan() {
        let mut m = mount(
            "/config",
            EphemeralMountSeed::CopyUp,
            Some(16),
            EphemeralMountSource::Explicit,
        );
        m.files = vec![EphemeralSeedFile {
            path: "config.yml".into(),
            source_path: "recipe/config.yml".into(),
            source_digest: format!("blake3:{}", "ab".repeat(32)),
            if_missing: true,
        }];
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![m],
            false,
        )
        .unwrap();
        let seeds = vec![super::super::seed_files::RenderedMountSeeds {
            path: "/config".into(),
            files: vec![super::super::seed_files::RenderedSeedFile {
                abs_dest: "/config/config.yml".into(),
                if_missing: true,
                content: b"port: 3000\n".to_vec(),
            }],
        }];
        let script = imported_pack_script("docker", "ato-import-x", &plan, &seeds, 1024, None);
        // Mount + copy-up + guarded seed write render together, all fail-closed.
        assert!(
            script.contains("mount -t tmpfs -o size=16m tmpfs /config"),
            "{script}"
        );
        assert!(
            script.contains("required tmpfs mount failed: /config"),
            "{script}"
        );
        assert!(
            script.contains("if [ ! -e '/config/config.yml' ]; then"),
            "{script}"
        );
        assert!(
            script.contains("base64 -d > '/config/config.yml'"),
            "{script}"
        );
        assert!(script.contains("seed file write failed"), "{script}");
        assert!(
            !script.contains("2>/dev/null; mount"),
            "no silent mount failures: {script}"
        );
    }

    #[test]
    fn pack_script_stages_mountpoints_at_pack_time_for_the_ro_root() {
        // The guest root mounts READ-ONLY: a mountpoint the image does not ship
        // (VOLUME without RUN mkdir — metube /downloads, homepage /app/config)
        // cannot be created by the boot-time mkdir. The pack script must create
        // it in the staged tree while the ext4 is writable.
        let mut m = mount(
            "/downloads",
            EphemeralMountSeed::Empty,
            Some(512),
            EphemeralMountSource::Explicit,
        );
        m.files = vec![EphemeralSeedFile {
            path: "nested/dir/seed.txt".into(),
            source_path: "recipe/seed.txt".into(),
            source_digest: format!("blake3:{}", "ab".repeat(32)),
            if_missing: false,
        }];
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![m],
            false,
        )
        .unwrap();
        let seeds = vec![super::super::seed_files::RenderedMountSeeds {
            path: "/downloads".into(),
            files: vec![super::super::seed_files::RenderedSeedFile {
                abs_dest: "/downloads/nested/dir/seed.txt".into(),
                if_missing: false,
                content: b"x".to_vec(),
            }],
        }];
        let script = imported_pack_script("docker", "ato-import-x", &plan, &seeds, 1024, None);
        // Pack-time staging (runs against $BUILD/rootfs, before mkfs+copy).
        assert!(
            script.contains("mkdir -p \"$BUILD/rootfs/downloads\""),
            "{script}"
        );
        assert!(
            script.contains("mkdir -p \"$BUILD/rootfs/downloads/nested/dir\""),
            "{script}"
        );
        // The boot-time fail-closed mount check stays.
        assert!(
            script.contains("required tmpfs mount failed: /downloads"),
            "{script}"
        );
        // Completeness: the appended mkdirs must NOT fold into the
        // supervisor.json heredoc terminator — the script must reach the ext4
        // build. A truncated script (broken heredoc) is exactly the os-error-2
        // regression: the ext4 is never written and metadata() fails ENOENT.
        assert!(
            script.contains("mkfs.ext4"),
            "pack script truncated before mkfs (broken heredoc):\n{script}"
        );
        assert!(
            script.contains("ATOSUPERVISORJSON\n"),
            "supervisor.json heredoc terminator must be alone on its line:\n{script}"
        );
    }

    #[test]
    fn seed_alignment_is_fail_closed_both_directions() {
        let mut m = mount(
            "/config",
            EphemeralMountSeed::Empty,
            None,
            EphemeralMountSource::Explicit,
        );
        m.files = vec![seed_file("config.yml")];
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![m],
            false,
        )
        .unwrap();
        // Declared files but nothing staged: the identity would promise a file
        // the init never writes.
        let err = validate_mount_seed_alignment(&plan, &[]).unwrap_err();
        assert!(
            err.contains("declares 1 seed file(s) but 0 were staged"),
            "{err}"
        );
        // Staged content for a mountpoint the plan does not declare.
        let plain =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        let stray = vec![super::super::seed_files::RenderedMountSeeds {
            path: "/config".into(),
            files: vec![],
        }];
        let err = validate_mount_seed_alignment(&plain, &stray).unwrap_err();
        assert!(err.contains("no matching ephemeral mount"), "{err}");
    }

    #[test]
    fn explicit_mount_covers_an_image_volume_under_reject() {
        // An explicit ephemeral_mount for a path that IS an image VOLUME
        // satisfies the ato#983 Reject gate (no volumes=tmpfs needed) and the
        // image VOLUME is NOT double-added.
        let mut c = config();
        c.volumes = vec!["/config".into()];
        let plan = derive_imported_service_plan_with_mounts(
            &c,
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![mount(
                "/config",
                EphemeralMountSeed::CopyUp,
                Some(16),
                EphemeralMountSource::Explicit,
            )],
            false,
        )
        .unwrap();
        assert_eq!(plan.ephemeral_mounts.len(), 1);
        assert_eq!(
            plan.ephemeral_mounts[0].source,
            EphemeralMountSource::Explicit
        );
        // An UNCOVERED image VOLUME still fails closed under Reject.
        c.volumes = vec!["/config".into(), "/other".into()];
        let err = derive_imported_service_plan_with_mounts(
            &c,
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![mount(
                "/config",
                EphemeralMountSeed::CopyUp,
                None,
                EphemeralMountSource::Explicit,
            )],
            false,
        )
        .unwrap_err();
        assert!(
            err.contains("VOLUME /other") && err.contains("[state]"),
            "{err}"
        );
    }

    #[test]
    fn mount_validation_fails_closed() {
        // Bad paths (reuse validate_ephemeral_mount_path rules).
        for (bad, why) in [
            ("data", "relative"),
            ("/", "root"),
            ("/data dir", "whitespace"),
            ("/data;rm", "shell metacharacter"),
            ("/data/../etc", "dot-dot"),
            ("/etc/app", "forbidden prefix /etc"),
            ("/tmp", "already tmpfs"),
            ("/usr/local/share", "forbidden prefix /usr"),
        ] {
            let err = derive_imported_service_plan_with_mounts(
                &config(),
                SecretEnvPolicy::Reject,
                None,
                None,
                VolumePolicy::Reject,
                vec![mount(
                    bad,
                    EphemeralMountSeed::Empty,
                    None,
                    EphemeralMountSource::Explicit,
                )],
                false,
            )
            .unwrap_err();
            assert!(err.contains("ephemeral mount"), "{why}: {err}");
        }
        // size_mib = 0 rejected.
        let err = validate_ephemeral_mounts(&[mount(
            "/x",
            EphemeralMountSeed::Empty,
            Some(0),
            EphemeralMountSource::Explicit,
        )])
        .unwrap_err();
        assert!(err.contains("size_mib must be >= 1"), "{err}");
        // Duplicate mountpoint (/data and /data/ normalize equal).
        let err = validate_ephemeral_mounts(&[
            mount(
                "/data",
                EphemeralMountSeed::Empty,
                None,
                EphemeralMountSource::Explicit,
            ),
            mount(
                "/data/",
                EphemeralMountSeed::Empty,
                None,
                EphemeralMountSource::Explicit,
            ),
        ])
        .unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
        // Nested / parent-child overlap.
        let err = validate_ephemeral_mounts(&[
            mount(
                "/data",
                EphemeralMountSeed::Empty,
                None,
                EphemeralMountSource::Explicit,
            ),
            mount(
                "/data/sub",
                EphemeralMountSeed::Empty,
                None,
                EphemeralMountSource::Explicit,
            ),
        ])
        .unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn empty_seed_renders_mkdir_mount_no_cp_with_size() {
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![mount(
                "/downloads",
                EphemeralMountSeed::Empty,
                Some(512),
                EphemeralMountSource::Explicit,
            )],
            false,
        )
        .unwrap();
        let script = imported_pack_script("docker", "ato-import-x", &plan, &[], 1024, None);
        let init = script
            .split("<<'INIT'")
            .nth(1)
            .unwrap()
            .split("INIT")
            .next()
            .unwrap();
        // size= appears in the mount cmd; a required-mount failure fails boot.
        assert!(init.contains("mount -t tmpfs -o size=512m tmpfs /downloads || { echo \"required tmpfs mount failed: /downloads\" >&2; exit 1; }"), "{init}");
        assert!(init.contains("mkdir -p /downloads"), "{init}");
        // Empty seed emits NO copy-up cp and no 2>/dev/null suppression.
        assert!(
            !init.contains("cp -a"),
            "empty seed must not copy-up:\n{init}"
        );
        assert!(
            !init.contains("mount -t tmpfs -o size=512m tmpfs /downloads 2>/dev/null"),
            "no suppression:\n{init}"
        );
    }

    #[test]
    fn copy_up_renders_seed_cp_mkdir_mount_cp_in_order() {
        let plan = derive_imported_service_plan_with_mounts(
            &config(),
            SecretEnvPolicy::Reject,
            None,
            None,
            VolumePolicy::Reject,
            vec![mount(
                "/config",
                EphemeralMountSeed::CopyUp,
                Some(16),
                EphemeralMountSource::Explicit,
            )],
            false,
        )
        .unwrap();
        let script = imported_pack_script("docker", "ato-import-x", &plan, &[], 1024, None);
        let init = script
            .split("<<'INIT'")
            .nth(1)
            .unwrap()
            .split("INIT")
            .next()
            .unwrap();
        // Ordered: seed staging → copy image contents in → mkdir → mount(size=) → copy back.
        let seed_at = init.find("seed=/run/ato/seed/0").expect("seed line");
        let cp_in_at = init.find("cp -a /config/. \"$seed/\"").expect("copy-up-in");
        let not_dir_at = init
            .find("ephemeral mountpoint is not a directory: /config")
            .expect("not-a-dir guard");
        let mkdir_at = init.find("mkdir -p /config").expect("mkdir");
        let mount_at = init
            .find("mount -t tmpfs -o size=16m tmpfs /config")
            .expect("mount size=");
        let cp_back_at = init.find("cp -a \"$seed/.\" /config/").expect("copy-back");
        assert!(
            seed_at < cp_in_at
                && cp_in_at < mkdir_at
                && mkdir_at < mount_at
                && mount_at < cp_back_at,
            "wrong order:\n{init}"
        );
        assert!(not_dir_at > seed_at, "not-a-dir guard present:\n{init}");
        // Mount failure fails boot; nothing suppressed with 2>/dev/null.
        assert!(
            init.contains("|| { echo \"required tmpfs mount failed: /config\" >&2; exit 1; }"),
            "{init}"
        );
    }

    #[test]
    fn no_mounts_keeps_the_legacy_init_shape() {
        let plain =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        let plain_script = imported_pack_script("docker", "ato-import-x", &plain, &[], 1024, None);
        let init = plain_script
            .split("<<'INIT'")
            .nth(1)
            .unwrap()
            .split("INIT")
            .next()
            .unwrap();
        assert!(
            !init.contains("devpts"),
            "a Web import must not mount devpts (init byte-stability)"
        );
        assert!(
            !init.contains("cp -a \"$seed/."),
            "no copy-up when no mounts"
        );
        assert_eq!(
            init.matches("mount -t tmpfs").count(),
            3,
            "only the standard /tmp /run /var/tmp mounts"
        );
    }

    #[test]
    fn pixel_import_mounts_devpts_fail_closed() {
        // Pixel Stream v1: a terminal-class workload allocates ptys; the pixel
        // opt-in (and ONLY the pixel opt-in) mounts devpts, and a failed mount
        // fails guest boot instead of booting a pty-less fixture.
        let plan =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        let script = imported_pack_script("docker", "ato-import-x", &plan, &[], 1024, Some(5900));
        assert!(
            script.contains(
                "mount -t devpts devpts /dev/pts || { echo \"required devpts mount failed: /dev/pts\" >&2; exit 1; }"
            ),
            "{script}"
        );
    }

    // --- ato#1026 localhost→guest-IP relay opt-in ----------------------------

    #[test]
    fn host_bind_relay_renders_the_relay_line_in_init() {
        let plan = derive_imported_service_plan_with_volumes(
            &config(),
            SecretEnvPolicy::Reject,
            Some(1737),
            None,
            VolumePolicy::Reject,
            true,
        )
        .unwrap();
        assert!(plan.host_bind_relay);
        let script = imported_pack_script("docker", "ato-import-x", &plan, &[], 1024, None);
        let init = script
            .split("<<'INIT'")
            .nth(1)
            .unwrap()
            .split("INIT")
            .next()
            .unwrap();
        assert!(
            init.contains("ato-guest-agent tcp-relay --listen-guest-port 1737")
                && init.contains("--target 127.0.0.1:1737"),
            "init must start the relay for the declared port:\n{init}"
        );
        // The relay starts BEFORE the app launch (so it is listening early;
        // it also retries the target, but ordering must be prelaunch).
        let relay_at = init.find("tcp-relay").unwrap();
        let launch_at = init
            .find("ato-guest-agent ")
            .map(|_| init.rfind("ato-guest-agent").unwrap())
            .unwrap();
        assert!(
            relay_at <= launch_at,
            "relay line must precede the agent launch"
        );

        // Opt-out (default) ⇒ no relay line at all.
        let plain =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        assert!(!plain.host_bind_relay);
        assert!(
            !imported_pack_script("docker", "ato-import-x", &plain, &[], 1024, None)
                .contains("tcp-relay")
        );
    }

    #[test]
    fn port_resolution_matrix() {
        // Explicit override wins over EXPOSE.
        let plan =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, Some(8080), None)
                .unwrap();
        assert_eq!(plan.port, 8080);

        // Zero EXPOSE + no override: fail closed.
        let mut c = config();
        c.exposed_tcp_ports.clear();
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap_err();
        assert!(err.contains("EXPOSEs no tcp port"), "{err}");

        // Multiple EXPOSE + no override: fail closed, ports listed.
        let mut c = config();
        c.exposed_tcp_ports = vec![3000, 9090];
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap_err();
        assert!(err.contains("3000, 9090"), "{err}");
    }

    #[test]
    fn image_declared_port_env_must_match_public_port() {
        let mut c = config();
        c.env.insert("PORT".into(), "3000".into());
        assert!(derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).is_ok());
        c.env.insert("PORT".into(), "9999".into());
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap_err();
        assert!(err.contains("PORT = \"9999\""), "{err}");
    }

    #[test]
    fn user_and_healthcheck_emit_warnings_not_errors() {
        let mut c = config();
        c.user = Some("app".into());
        c.has_healthcheck = true;
        let plan = derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None).unwrap();
        assert_eq!(
            plan.warnings,
            vec![
                DockerImportWarning::DockerUserIgnored,
                DockerImportWarning::DockerHealthcheckIgnored
            ]
        );
        // root-equivalent USER forms are not "ignored" — no warning.
        let mut c = config();
        c.user = Some("root".into());
        assert!(
            derive_imported_service_plan(&c, SecretEnvPolicy::Reject, None, None)
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn secret_env_flows_through_the_slice2_gate() {
        let mut c = config();
        c.env.insert(
            "OPENAI_API_KEY".into(),
            "sk-abcdefghijklmnopqrstuvwx".into(),
        );
        let err =
            derive_imported_service_plan(&c, SecretEnvPolicy::ConvertPlaceholders, None, None)
                .unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
        assert!(!err.contains("sk-abcdefghijklmnopqrstuvwx"), "{err}");

        let mut c = config();
        c.env.insert("OPENAI_API_KEY".into(), "".into());
        let plan =
            derive_imported_service_plan(&c, SecretEnvPolicy::ConvertPlaceholders, None, None)
                .unwrap();
        assert_eq!(plan.supervisor.binding_names, vec!["openai_api_key"]);
        let s = &plan.supervisor.services.as_ref().unwrap()[0];
        assert_eq!(s.env_map["OPENAI_API_KEY"], "openai_api_key");
        assert!(!s.base_env.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn imported_pack_script_shares_the_pipeline_but_not_the_build() {
        let plan =
            derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        let script = imported_pack_script("podman", "ato-import-abc123", &plan, &[], 1024, None);
        // Imported tag, single-quoted; chosen tool drives create/export/cleanup.
        assert!(script.contains("TAG='ato-import-abc123'"), "{script}");
        assert!(script.contains("CID=$(podman create \"$TAG\")"), "{script}");
        assert!(script.contains("podman export \"$CID\""), "{script}");
        // No generated-Dockerfile acquire step, no source copy.
        assert!(!script.contains("docker build"), "{script}");
        assert!(!script.contains("ATO_SRC"), "{script}");
        // Supervisor staging + services-shaped supervisor.json + agent launch.
        assert!(script.contains("ATO_GUEST_AGENT_BIN"), "{script}");
        assert!(script.contains("\"services\""), "{script}");
        assert!(
            script.contains("/usr/local/bin/ato-guest-agent"),
            "{script}"
        );
        // Init cwd is `/`, not the legacy `/app`.
        assert!(script.contains("\ncd /\n"), "{script}");
        assert!(!script.contains("cd /app"), "{script}");
        // Argv cmd lands verbatim in supervisor.json (no sh -lc wrapper).
        assert!(script.contains("\"docker-entrypoint.sh\""), "{script}");
    }

    #[test]
    fn legacy_script_is_unchanged_by_the_refactor() {
        // Golden invariants of the legacy generated-Dockerfile script (the
        // refactor must keep emitting them byte-for-byte in-place).
        use crate::rootfs_builder::{SourceProbe, derive_build_spec};
        use capsule::foundation::types::manifest::CapsuleManifest;
        let m = CapsuleManifest::from_toml(
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
"#,
        )
        .unwrap();
        let spec = derive_build_spec(
            &m,
            &SourceProbe {
                has_requirements_txt: true,
                ..Default::default()
            },
        )
        .unwrap();
        let script = crate::rootfs_builder::build_rootfs_script(&spec, 512);
        assert!(script.contains("TAG=\"ato-rootfs-$$\""), "{script}");
        assert!(
            script.contains("cp -a \"$ATO_SRC/.\" \"$BUILD/\""),
            "{script}"
        );
        assert!(
            script.contains(
                "docker build -q -t \"$TAG\" \"$BUILD\" >/dev/null\nCID=$(docker create \"$TAG\")"
            ),
            "{script}"
        );
        assert!(script.contains("\ncd /app\n"), "{script}");
    }
}
