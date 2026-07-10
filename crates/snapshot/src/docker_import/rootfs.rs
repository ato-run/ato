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
    /// Declared VOLUME paths mapped to guest tmpfs (ato#1024). Non-empty ONLY
    /// when the job explicitly opted in via `volumes=tmpfs` — the receipt
    /// records these so it is auditable that the artifact's mutable state is
    /// deliberately ephemeral (dies on stop/resume; acceptable for the
    /// throwaway preview lane, still lossy for a durable install exactly as
    /// ato#983 warned — hence opt-in + receipt, never a default).
    pub tmpfs_volumes: Vec<String>,
    pub warnings: Vec<DockerImportWarning>,
}

/// How the job asked to treat image-declared VOLUMEs (ato#1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumePolicy {
    /// Fail closed on any VOLUME (the ato#983 default, unchanged).
    #[default]
    Reject,
    /// Mount each declared VOLUME as guest tmpfs — state is EXPECTED to die.
    Tmpfs,
}

/// Paths a VOLUME may never shadow with tmpfs: mounting over these would
/// replace the OS / the app itself / an already-tmpfs mount rather than the
/// app's data directory, so they fail closed even under `volumes=tmpfs`.
const TMPFS_FORBIDDEN_PREFIXES: &[&str] =
    &["/proc", "/sys", "/dev", "/sbin", "/bin", "/usr", "/lib", "/lib64", "/etc", "/tmp", "/run", "/var/tmp"];

/// Validate one declared VOLUME path for tmpfs mapping. The path is rendered
/// into the QUOTED init heredoc, so the character set is restricted to what
/// cannot break out of `mount -t tmpfs tmpfs <path>` (no whitespace, quotes,
/// or shell metacharacters — fail-closed rather than escaped).
fn validate_tmpfs_volume_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path == "/" {
        return Err(format!("VOLUME {path:?} is not an absolute non-root path"));
    }
    if path.len() > 200 {
        return Err(format!("VOLUME {path:?} exceeds 200 chars"));
    }
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-')) {
        return Err(format!(
            "VOLUME {path:?} contains characters outside [A-Za-z0-9/_.-] — refusing to render it into the guest init (fail-closed)"
        ));
    }
    if path.contains("..") {
        return Err(format!("VOLUME {path:?} contains '..'"));
    }
    let normalized = path.trim_end_matches('/');
    for forbidden in TMPFS_FORBIDDEN_PREFIXES {
        if normalized == *forbidden || normalized.starts_with(&format!("{forbidden}/")) {
            return Err(format!(
                "VOLUME {path:?} would tmpfs-shadow {forbidden} (OS/app surface, not app data) — fail-closed"
            ));
        }
    }
    Ok(())
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
    derive_imported_service_plan_with_volumes(config, policy, port_override, readiness_http_path, VolumePolicy::Reject)
}

/// [`derive_imported_service_plan`] with an explicit [`VolumePolicy`] (ato#1024).
pub fn derive_imported_service_plan_with_volumes(
    config: &DockerImageConfig,
    policy: SecretEnvPolicy,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
    volume_policy: VolumePolicy,
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

    let tmpfs_volumes: Vec<String> = match (volume_policy, config.volumes.is_empty()) {
        (_, true) => Vec::new(),
        (VolumePolicy::Reject, false) => {
            return Err(format!(
                "image declares VOLUME {} — unmapped mutable state would silently die on a \
                 frozen-snapshot resume (ato#983). Map it to Ato [state] + state_bindings, \
                 opt in to ephemeral tmpfs mapping with volumes=tmpfs (ato#1024), or drop \
                 the VOLUME",
                config.volumes.join(", ")
            ));
        }
        (VolumePolicy::Tmpfs, false) => {
            for v in &config.volumes {
                validate_tmpfs_volume_path(v)?;
            }
            config.volumes.clone()
        }
    };

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
        binding_names,
        env_map: bindings_env,
        services: Some(vec![service]),
        public_service: Some(IMPORTED_SERVICE_NAME.to_string()),
    };
    Ok(ImportedServicePlan { supervisor, port, readiness_http_path, tmpfs_volumes, warnings })
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
    size_mib: u64,
) -> String {
    let (agent_prep, launch) =
        supervisor_prep_and_launch(Some(&plan.supervisor), plan.port, /* start_cmd unused in services shape */ "");
    let healthcheck = plan.readiness_http_path.clone().unwrap_or_else(|| "/".to_string());
    // ato#1024: each opted-in VOLUME becomes a guest tmpfs mount. Paths were
    // validated fail-closed at plan derivation (validate_tmpfs_volume_path),
    // so plain interpolation into the quoted init heredoc is safe here.
    let extra_mounts: String = plan
        .tmpfs_volumes
        .iter()
        .map(|v| format!("mkdir -p {v} 2>/dev/null; mount -t tmpfs tmpfs {v} 2>/dev/null\n"))
        .collect();
    rootfs_pack_script(&PackScriptInputs {
        tool,
        tag_init: format!("TAG={}", shell_single_quote(image_tag)),
        acquire: String::new(),
        agent_prep,
        launch,
        init_cwd: "/",
        port: plan.port,
        healthcheck,
        size_mib,
        extra_mounts,
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
    out_ext4: &Path,
    size_mib: u64,
) -> Result<u64, String> {
    let script = imported_pack_script(tool.as_str(), image_tag, plan, size_mib);
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

    // --- ato#1024 VOLUME→tmpfs opt-in -----------------------------------------

    #[test]
    fn tmpfs_policy_accepts_volumes_and_records_them() {
        let mut c = config();
        c.volumes = vec!["/data".into(), "/downloads".into()];
        let plan = derive_imported_service_plan_with_volumes(
            &c, SecretEnvPolicy::Reject, None, None, VolumePolicy::Tmpfs,
        )
        .unwrap();
        assert_eq!(plan.tmpfs_volumes, vec!["/data", "/downloads"]);
        // The default (Reject) path never populates the field.
        let plan = derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        assert!(plan.tmpfs_volumes.is_empty());
    }

    #[test]
    fn tmpfs_policy_validates_paths_fail_closed() {
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
            let mut c = config();
            c.volumes = vec![bad.to_string()];
            let err = derive_imported_service_plan_with_volumes(
                &c, SecretEnvPolicy::Reject, None, None, VolumePolicy::Tmpfs,
            )
            .unwrap_err();
            assert!(err.contains("VOLUME"), "{why}: {err}");
        }
    }

    #[test]
    fn tmpfs_volumes_render_mount_lines_into_init() {
        let mut c = config();
        c.volumes = vec!["/data".into()];
        let plan = derive_imported_service_plan_with_volumes(
            &c, SecretEnvPolicy::Reject, None, None, VolumePolicy::Tmpfs,
        )
        .unwrap();
        let script = imported_pack_script("docker", "ato-import-x", &plan, 1024);
        assert!(
            script.contains("mkdir -p /data 2>/dev/null; mount -t tmpfs tmpfs /data 2>/dev/null"),
            "init must mount the declared VOLUME as tmpfs:\n{script}"
        );
        // The mount lands INSIDE the quoted INIT heredoc (before `cd /`), not in
        // the host-side pack section.
        let init = script.split("<<'INIT'").nth(1).unwrap().split("INIT").next().unwrap();
        assert!(init.contains("mount -t tmpfs tmpfs /data"), "mount must be in guest init:\n{init}");

        // No volumes ⇒ byte-wise no extra lines (the legacy template shape).
        let plain = derive_imported_service_plan(&config(), SecretEnvPolicy::Reject, None, None).unwrap();
        let plain_script = imported_pack_script("docker", "ato-import-x", &plain, 1024);
        assert!(!plain_script.contains("mkdir -p /data"));
        assert_eq!(plain_script.matches("mount -t tmpfs").count(), 3, "only the standard /tmp /run /var/tmp mounts");
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
        let script = imported_pack_script("podman", "ato-import-abc123", &plan, 1024);
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
