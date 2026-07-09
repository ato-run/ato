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
    pub warnings: Vec<DockerImportWarning>,
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

    if !config.volumes.is_empty() {
        return Err(format!(
            "image declares VOLUME {} — unmapped mutable state would silently die on a \
             frozen-snapshot resume (ato#983). Map it to Ato [state] + state_bindings \
             (import volume mapping is a later slice) or drop the VOLUME",
            config.volumes.join(", ")
        ));
    }

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
    Ok(ImportedServicePlan {
        supervisor,
        port,
        readiness_http_path,
        warnings,
    })
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
    let (agent_prep, launch) = supervisor_prep_and_launch(
        Some(&plan.supervisor),
        plan.port,
        /* start_cmd unused in services shape */ "",
    );
    let healthcheck = plan
        .readiness_http_path
        .clone()
        .unwrap_or_else(|| "/".to_string());
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
