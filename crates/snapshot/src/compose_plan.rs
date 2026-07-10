//! Phase 4 — Compose-to-Ato planner (roadmap: multi-image snapshot import).
//!
//! **Pure planning only.** This module converts a `docker-compose` file into
//! Ato's normalized service graph ([`ImportedServiceGraph`]). It boots no VM,
//! builds no rootfs, resolves no image digest, and touches no registry — it is
//! a deterministic YAML → graph transform. The graph it emits is the input the
//! later phases consume:
//!
//! * **Phase 5** — multi-image rootfs: each [`ImportedService`] becomes one
//!   image extracted into the shared rootfs, wired through the v1.5 supervisor.
//! * **Phase 8** — Blinko (a real multi-service compose app) is the end-to-end
//!   proof that this planner + Phase 5 import a non-trivial stack.
//!
//! ## Why a self-contained module (not `capsule`'s compose importer)
//!
//! `capsule::routing::importer::compose::import_compose` already parses a
//! Compose subset, but it targets a *different contract*: it projects to an OCI
//! `OrchestrationPlan`, maps volumes to Ato `[state.*]` bindings, and — for the
//! inputs this planner must reject — it only *warns* (relative binds, unknown
//! keys) or ignores them (`expose`, `restart`, `devices`, `cap_add`,
//! `docker.sock`, external networks). This planner is **fail-closed** on every
//! one of those, needs `expose`/`restart` (which that importer drops), and must
//! keep the graph's shape independent of `capsule` so the snapshot artifact's
//! identity is stable. Reusing it would mean projecting its lossy output *plus*
//! a second raw-YAML scan for everything it discards — more coupling and more
//! code than a focused planner. So this is a deliberate parallel module, not an
//! accidental duplicate.
//!
//! ## Fail-closed rejections
//!
//! Each of these produces a clear error rather than a silent downgrade:
//! `privileged`, any bind of `/var/run/docker.sock`, `network_mode: host`,
//! container-runtime network modes (`service:` / `container:`), `devices`,
//! `cap_add`, `build` (and thus build secrets), service/`build` `secrets`,
//! external Docker networks, arbitrary host bind mounts, and any unrecognized
//! Compose key (extension fields prefixed `x-` are ignored per the Compose
//! spec).
//!
//! ## Determinism
//!
//! Services and dependencies are sorted by name so the graph — and therefore
//! any identity hashed over it downstream — is independent of the order keys
//! appear in the source file.

use std::collections::BTreeMap;

use serde_yaml::Value;

/// Ato's normalized projection of a compose file: a canonically-ordered set of
/// services plus the dependency edges between them and the single public web
/// service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedServiceGraph {
    /// Services sorted by `name` (canonical order — see module docs).
    pub services: Vec<ImportedService>,
    /// Dependency edges sorted by `(from, to)`.
    pub dependencies: Vec<ServiceDependency>,
    /// The single service that publishes a public web port (`ports:`).
    pub public_service: String,
}

/// One normalized service in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedService {
    /// Compose service key.
    pub name: String,
    /// Declared image reference (tag or digest) — verbatim from `image:`.
    pub image_ref: String,
    /// Registry-resolved digest. **Empty at plan time**: digest resolution is a
    /// registry round-trip that belongs to the build phase (Phase 5), not the
    /// pure planner. Left `""` here and filled in when the image is pulled.
    pub resolved_digest: String,
    /// `entrypoint` ++ `command`, each normalized to exec form (a shell-form
    /// string becomes `["/bin/sh", "-c", <string>]`).
    pub command: Vec<String>,
    /// Literal environment. Keys with no value (host-passthrough form) map to an
    /// empty string; secret handling is downstream (the rootfs secret gate).
    pub env: BTreeMap<String, String>,
    /// The service's listening port: the container side of the first `ports:`
    /// entry if public, else the first `expose:` entry, else `None`.
    pub port: Option<u16>,
    /// Normalized `healthcheck:` (`None` when absent or `disable: true`).
    pub healthcheck: Option<Healthcheck>,
    /// Named / anonymous / tmpfs mounts. Host bind mounts are rejected before we
    /// get here, so this never contains an arbitrary host path.
    pub mounts: Vec<ServiceMount>,
    /// `restart:` normalized to an Ato supervisor policy.
    pub restart: RestartPolicy,
}

/// A dependency edge `from` → `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependency {
    pub from: String,
    pub to: String,
    pub kind: DependencyKind,
}

/// How a dependency must be satisfied before the dependent starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    /// Plain `depends_on` / `condition: service_started` — wait until the
    /// dependency's process is up.
    Ready,
    /// `condition: service_healthy` / `service_completed_successfully` — wait
    /// until the dependency reports success (healthy or completed).
    Success,
}

/// Normalized `healthcheck:`. Durations are kept as their raw compose strings
/// (e.g. `"30s"`) — lossless and good enough for planning; a later phase can
/// parse them when it wires a real probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Healthcheck {
    /// Command tokens with the `CMD` / `CMD-SHELL` prefix stripped.
    pub test: Vec<String>,
    pub interval: Option<String>,
    pub timeout: Option<String>,
    pub retries: Option<u32>,
    pub start_period: Option<String>,
}

/// A volume mount that survived the fail-closed filter (never a host bind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMount {
    /// Absolute path inside the container.
    pub target: String,
    /// The named-volume name, or `None` for an anonymous/tmpfs mount.
    pub source: Option<String>,
    pub kind: MountKind,
}

/// The kind of a [`ServiceMount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// A top-level named volume.
    Named,
    /// An anonymous volume (ephemeral, no declared source).
    Anonymous,
    /// An explicit `type: tmpfs` mount.
    Tmpfs,
}

/// `restart:` normalized to an Ato supervisor restart policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// `no` / absent — do not restart.
    Never,
    /// `on-failure[:N]` — restart only on non-zero exit.
    OnFailure,
    /// `always` / `unless-stopped` — keep the service running.
    Always,
}

/// Compose service keys this planner understands. Anything else (that is not an
/// `x-` extension field or an explicitly-rejected dangerous key) fails closed.
const SUPPORTED_SERVICE_KEYS: &[&str] = &[
    "image",
    "command",
    "entrypoint",
    "environment",
    "depends_on",
    "healthcheck",
    "expose",
    "ports",
    "volumes",
    "restart",
    // Recognized-but-conditional (validated, not blindly accepted):
    "privileged",   // allowed only when false
    "network_mode", // allowed only for bridge/none/default
];

/// Convert a docker-compose document into Ato's normalized [`ImportedServiceGraph`].
///
/// Pure and deterministic: same input (in any key order) → same graph. Returns
/// a human-readable error string on any unsupported or unsafe construct.
pub fn compose_to_graph(yaml: &str) -> Result<ImportedServiceGraph, String> {
    let doc: Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("failed to parse compose YAML: {e}"))?;
    let top = doc
        .as_mapping()
        .ok_or_else(|| "compose file must be a YAML mapping at the top level".to_string())?;

    reject_external_networks(top)?;

    let services_val = top
        .get(Value::from("services"))
        .ok_or_else(|| "compose file has no `services` section".to_string())?;
    let services_map = services_val
        .as_mapping()
        .ok_or_else(|| "`services` must be a mapping of service name → definition".to_string())?;
    if services_map.is_empty() {
        return Err("compose file declares no services".to_string());
    }

    // All service names first, so dependency edges can be validated.
    let mut names: Vec<String> = Vec::new();
    for (k, _) in services_map {
        let name = k
            .as_str()
            .ok_or_else(|| "service names must be strings".to_string())?;
        names.push(name.to_string());
    }

    let mut services: Vec<ImportedService> = Vec::new();
    let mut dependencies: Vec<ServiceDependency> = Vec::new();
    let mut public_services: Vec<String> = Vec::new();

    for (k, svc_val) in services_map {
        let name = k.as_str().expect("checked above").to_string();
        let svc = svc_val
            .as_mapping()
            .ok_or_else(|| format!("service '{name}': definition must be a mapping"))?;

        validate_service_keys(&name, svc)?;

        let image_ref = svc
            .get(Value::from("image"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                format!("service '{name}': missing `image` (Ato imports prebuilt images)")
            })?
            .to_string();

        let mut command = normalize_argv(svc.get(Value::from("entrypoint")));
        command.extend(normalize_argv(svc.get(Value::from("command"))));

        let env = parse_environment(&name, svc.get(Value::from("environment")))?;

        let ports = parse_port_list(&name, svc.get(Value::from("ports")), "ports")?;
        let exposed = parse_port_list(&name, svc.get(Value::from("expose")), "expose")?;
        let is_public = !ports.is_empty();
        if is_public {
            public_services.push(name.clone());
        }
        let port = ports.first().copied().or_else(|| exposed.first().copied());

        let healthcheck = parse_healthcheck(&name, svc.get(Value::from("healthcheck")))?;
        let mounts = parse_volumes(&name, svc.get(Value::from("volumes")))?;
        let restart = normalize_restart(&name, svc.get(Value::from("restart")))?;

        for (to, kind) in parse_depends_on(&name, svc.get(Value::from("depends_on")))? {
            if !names.contains(&to) {
                return Err(format!("service '{name}': depends_on unknown service '{to}'"));
            }
            dependencies.push(ServiceDependency {
                from: name.clone(),
                to,
                kind,
            });
        }

        services.push(ImportedService {
            name,
            image_ref,
            resolved_digest: String::new(),
            command,
            env,
            port,
            healthcheck,
            mounts,
            restart,
        });
    }

    let public_service = match public_services.len() {
        1 => public_services.remove(0),
        0 => {
            return Err(
                "no service publishes a public port via `ports:` — v1 requires exactly one"
                    .to_string(),
            );
        }
        _ => {
            public_services.sort();
            return Err(format!(
                "ambiguous public service: multiple services publish ports ({}) — v1 requires exactly one",
                public_services.join(", ")
            ));
        }
    };

    // Canonical order → identity is independent of source key order.
    services.sort_by(|a, b| a.name.cmp(&b.name));
    dependencies.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));

    Ok(ImportedServiceGraph {
        services,
        dependencies,
        public_service,
    })
}

/// Reject external Docker networks declared at the top level
/// (`networks.<n>.external: true` or `external: {name: ...}`).
fn reject_external_networks(top: &serde_yaml::Mapping) -> Result<(), String> {
    let Some(networks) = top.get(Value::from("networks")).and_then(Value::as_mapping) else {
        return Ok(());
    };
    for (net_name, net_def) in networks {
        let name = net_name.as_str().unwrap_or("<network>");
        if let Some(def) = net_def.as_mapping() {
            if let Some(external) = def.get(Value::from("external")) {
                let is_external = match external {
                    Value::Bool(b) => *b,
                    Value::Null => false,
                    // `external: {name: foo}` (a mapping) or any truthy scalar.
                    _ => true,
                };
                if is_external {
                    return Err(format!(
                        "external Docker network '{name}' is not supported (fail-closed)"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Fail closed on dangerous or unrecognized service keys.
fn validate_service_keys(name: &str, svc: &serde_yaml::Mapping) -> Result<(), String> {
    for (k, v) in svc {
        let key = k
            .as_str()
            .ok_or_else(|| format!("service '{name}': non-string key in definition"))?;
        match key {
            // Explicitly-rejected, each with a clear reason.
            "privileged" => {
                if truthy(v) {
                    return Err(format!(
                        "service '{name}': `privileged` mode is not supported (fail-closed)"
                    ));
                }
            }
            "network_mode" => {
                let mode = v.as_str().unwrap_or_default();
                match mode {
                    "host" => {
                        return Err(format!(
                            "service '{name}': `network_mode: host` is not supported (fail-closed)"
                        ));
                    }
                    m if m.starts_with("service:") || m.starts_with("container:") => {
                        return Err(format!(
                            "service '{name}': container-runtime network mode '{m}' is not supported (fail-closed)"
                        ));
                    }
                    "bridge" | "none" | "default" | "" => {}
                    other => {
                        return Err(format!(
                            "service '{name}': unsupported `network_mode: {other}` (fail-closed)"
                        ));
                    }
                }
            }
            "devices" => {
                return Err(format!(
                    "service '{name}': `devices` (host device passthrough) is not supported (fail-closed)"
                ));
            }
            "cap_add" => {
                return Err(format!(
                    "service '{name}': `cap_add` (added Linux capabilities) is not supported (fail-closed)"
                ));
            }
            "build" => {
                return Err(format!(
                    "service '{name}': `build` is not supported — Ato imports prebuilt images, so build secrets cannot be honored (fail-closed)"
                ));
            }
            "secrets" => {
                return Err(format!(
                    "service '{name}': `secrets` (build/compose secrets) are not supported (fail-closed)"
                ));
            }
            // Other container-runtime escape hatches.
            "pid" | "ipc" | "userns_mode" | "cgroup_parent" | "runtime" | "cap_drop"
            | "security_opt" | "sysctls" | "device_cgroup_rules" | "group_add" => {
                return Err(format!(
                    "service '{name}': `{key}` is a container-runtime dependency and is not supported (fail-closed)"
                ));
            }
            _ if key.starts_with("x-") => {} // Compose extension field — ignored.
            _ if SUPPORTED_SERVICE_KEYS.contains(&key) => {}
            other => {
                return Err(format!(
                    "service '{name}': unsupported compose key `{other}` (fail-closed)"
                ));
            }
        }
    }
    Ok(())
}

/// Normalize `command:` / `entrypoint:` to exec form. A list is taken verbatim;
/// a shell-form string becomes `["/bin/sh", "-c", <string>]` (Docker's own
/// shell-form semantics). Absent → empty.
fn normalize_argv(val: Option<&Value>) -> Vec<String> {
    match val {
        None | Some(Value::Null) => vec![],
        Some(Value::String(s)) => vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()],
        Some(Value::Sequence(seq)) => seq.iter().filter_map(scalar_to_string).collect(),
        Some(_) => vec![],
    }
}

/// Parse `environment:` (list `- KEY=VAL` / `- KEY`, or map `KEY: val`) into a
/// literal env map. A bare key (host passthrough) maps to an empty string.
fn parse_environment(name: &str, val: Option<&Value>) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    match val {
        None | Some(Value::Null) => {}
        Some(Value::Sequence(seq)) => {
            for item in seq {
                let s = item.as_str().ok_or_else(|| {
                    format!("service '{name}': environment list entries must be strings")
                })?;
                match s.split_once('=') {
                    Some((k, v)) => {
                        env.insert(k.trim().to_string(), v.to_string());
                    }
                    None => {
                        env.insert(s.trim().to_string(), String::new());
                    }
                }
            }
        }
        Some(Value::Mapping(map)) => {
            for (k, v) in map {
                let key = k
                    .as_str()
                    .ok_or_else(|| format!("service '{name}': environment keys must be strings"))?;
                let value = match v {
                    Value::Null => String::new(),
                    other => scalar_to_string(other).ok_or_else(|| {
                        format!("service '{name}': environment value for '{key}' is not a scalar")
                    })?,
                };
                env.insert(key.to_string(), value);
            }
        }
        Some(_) => {
            return Err(format!(
                "service '{name}': `environment` must be a list or a mapping"
            ));
        }
    }
    Ok(env)
}

/// Parse a `ports:` / `expose:` list into container-side port numbers. `field`
/// names which key is being parsed (for error text).
fn parse_port_list(name: &str, val: Option<&Value>, field: &str) -> Result<Vec<u16>, String> {
    let mut ports = Vec::new();
    let seq = match val {
        None | Some(Value::Null) => return Ok(ports),
        Some(Value::Sequence(seq)) => seq,
        Some(_) => {
            return Err(format!("service '{name}': `{field}` must be a list"));
        }
    };
    for entry in seq {
        let port = container_port(entry)
            .ok_or_else(|| format!("service '{name}': malformed `{field}` entry {entry:?}"))?;
        ports.push(port);
    }
    Ok(ports)
}

/// Extract the container-side port from one `ports:`/`expose:` entry.
fn container_port(val: &Value) -> Option<u16> {
    match val {
        // "port", "host:container", "ip:host:container", "port/proto"
        Value::String(s) => {
            let container = match s.split(':').collect::<Vec<_>>().as_slice() {
                [p] => p.to_string(),
                [_, p] => p.to_string(),
                [_, _, p] => p.to_string(),
                _ => return None,
            };
            let port_str = container.split('/').next().unwrap_or(&container).trim();
            // Ranges ("3000-3005") are not a single port → reject.
            if port_str.contains('-') {
                return None;
            }
            port_str.parse().ok()
        }
        Value::Number(n) => n.as_u64()?.try_into().ok(),
        // Long form: {target: 8080, published: 80, protocol: tcp}
        Value::Mapping(m) => m.get(Value::from("target")).and_then(container_port_scalar),
        _ => None,
    }
}

/// Container port for the long-form `target:` value (number or numeric string).
fn container_port_scalar(val: &Value) -> Option<u16> {
    match val {
        Value::Number(n) => n.as_u64()?.try_into().ok(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse `healthcheck:`. `disable: true` or `test: NONE` → `None`.
fn parse_healthcheck(name: &str, val: Option<&Value>) -> Result<Option<Healthcheck>, String> {
    let hc = match val {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Mapping(m)) => m,
        Some(_) => {
            return Err(format!("service '{name}': `healthcheck` must be a mapping"));
        }
    };

    if truthy(hc.get(Value::from("disable")).unwrap_or(&Value::Null)) {
        return Ok(None);
    }

    let test = match hc.get(Value::from("test")) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Sequence(seq)) => {
            let items: Vec<String> = seq.iter().filter_map(scalar_to_string).collect();
            match items.first().map(String::as_str) {
                Some("NONE") => return Ok(None),
                Some("CMD") | Some("CMD-SHELL") => items[1..].to_vec(),
                _ => items,
            }
        }
        // Bare string is shell form (Compose treats it as CMD-SHELL).
        Some(Value::String(s)) => vec![s.clone()],
        Some(_) => {
            return Err(format!(
                "service '{name}': `healthcheck.test` must be a string or list"
            ));
        }
    };
    if test.is_empty() {
        return Ok(None);
    }

    Ok(Some(Healthcheck {
        test,
        interval: hc.get(Value::from("interval")).and_then(scalar_to_string),
        timeout: hc.get(Value::from("timeout")).and_then(scalar_to_string),
        retries: hc
            .get(Value::from("retries"))
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        start_period: hc
            .get(Value::from("start_period"))
            .and_then(scalar_to_string),
    }))
}

const DOCKER_SOCK: &str = "/var/run/docker.sock";

/// Parse `volumes:`, rejecting host bind mounts (and `docker.sock` specifically).
fn parse_volumes(name: &str, val: Option<&Value>) -> Result<Vec<ServiceMount>, String> {
    let mut mounts = Vec::new();
    let seq = match val {
        None | Some(Value::Null) => return Ok(mounts),
        Some(Value::Sequence(seq)) => seq,
        Some(_) => return Err(format!("service '{name}': `volumes` must be a list")),
    };

    for entry in seq {
        match entry {
            Value::String(s) => {
                mounts.push(parse_volume_short(name, s)?);
            }
            Value::Mapping(m) => {
                mounts.push(parse_volume_long(name, m)?);
            }
            other => {
                return Err(format!("service '{name}': unsupported volume entry {other:?}"));
            }
        }
    }
    Ok(mounts)
}

/// Short-form volume: `TARGET` (anonymous) or `SOURCE:TARGET[:MODE]`.
fn parse_volume_short(name: &str, spec: &str) -> Result<ServiceMount, String> {
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    match parts.as_slice() {
        [target] => {
            if !target.starts_with('/') {
                return Err(format!(
                    "service '{name}': volume '{spec}' is neither a container path nor SOURCE:TARGET"
                ));
            }
            Ok(ServiceMount {
                target: target.to_string(),
                source: None,
                kind: MountKind::Anonymous,
            })
        }
        [source, target, ..] => {
            reject_docker_sock(name, source, target)?;
            if is_host_path(source) {
                return Err(format!(
                    "service '{name}': arbitrary bind mount '{source}:{target}' is not supported — use a named volume (fail-closed)"
                ));
            }
            Ok(ServiceMount {
                target: target.to_string(),
                source: Some((*source).to_string()),
                kind: MountKind::Named,
            })
        }
        _ => Err(format!("service '{name}': malformed volume '{spec}'")),
    }
}

/// Long-form volume: `{type, source, target, ...}`.
fn parse_volume_long(name: &str, m: &serde_yaml::Mapping) -> Result<ServiceMount, String> {
    let vtype = m
        .get(Value::from("type"))
        .and_then(Value::as_str)
        .unwrap_or("volume");
    let source = m.get(Value::from("source")).and_then(Value::as_str);
    let target = m
        .get(Value::from("target"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("service '{name}': volume long-form is missing `target`"))?;

    reject_docker_sock(name, source.unwrap_or_default(), target)?;

    match vtype {
        "bind" => Err(format!(
            "service '{name}': arbitrary bind mount (target '{target}') is not supported — use a named volume (fail-closed)"
        )),
        "tmpfs" => Ok(ServiceMount {
            target: target.to_string(),
            source: None,
            kind: MountKind::Tmpfs,
        }),
        "volume" => Ok(ServiceMount {
            target: target.to_string(),
            source: source.map(str::to_string),
            kind: if source.is_some() {
                MountKind::Named
            } else {
                MountKind::Anonymous
            },
        }),
        other => Err(format!(
            "service '{name}': unsupported volume type '{other}' (fail-closed)"
        )),
    }
}

/// Reject any mount touching the Docker socket.
fn reject_docker_sock(name: &str, source: &str, target: &str) -> Result<(), String> {
    if source == DOCKER_SOCK
        || target == DOCKER_SOCK
        || source.ends_with("/docker.sock")
        || target.ends_with("/docker.sock")
    {
        return Err(format!(
            "service '{name}': mounting the Docker socket ({DOCKER_SOCK}) is not supported (fail-closed)"
        ));
    }
    Ok(())
}

/// A bind-mount host source: absolute, relative, or home-relative.
fn is_host_path(source: &str) -> bool {
    source.starts_with('/') || source.starts_with('.') || source.starts_with('~')
}

/// Parse `depends_on:` (list or condition map) into `(target, kind)` edges.
fn parse_depends_on(
    name: &str,
    val: Option<&Value>,
) -> Result<Vec<(String, DependencyKind)>, String> {
    let mut deps = Vec::new();
    match val {
        None | Some(Value::Null) => {}
        Some(Value::Sequence(seq)) => {
            for dep in seq {
                let to = dep.as_str().ok_or_else(|| {
                    format!("service '{name}': depends_on list entries must be strings")
                })?;
                deps.push((to.to_string(), DependencyKind::Ready));
            }
        }
        Some(Value::Mapping(map)) => {
            for (dep, entry) in map {
                let to = dep
                    .as_str()
                    .ok_or_else(|| format!("service '{name}': depends_on keys must be strings"))?;
                let condition = entry
                    .as_mapping()
                    .and_then(|e| e.get(Value::from("condition")))
                    .and_then(Value::as_str);
                let kind = match condition {
                    Some("service_healthy") | Some("service_completed_successfully") => {
                        DependencyKind::Success
                    }
                    _ => DependencyKind::Ready,
                };
                deps.push((to.to_string(), kind));
            }
        }
        Some(_) => {
            return Err(format!(
                "service '{name}': `depends_on` must be a list or a mapping"
            ));
        }
    }
    Ok(deps)
}

/// Normalize `restart:` to an Ato supervisor policy.
fn normalize_restart(name: &str, val: Option<&Value>) -> Result<RestartPolicy, String> {
    let raw = match val {
        None | Some(Value::Null) => return Ok(RestartPolicy::Never),
        Some(Value::String(s)) => s.as_str(),
        Some(_) => {
            return Err(format!("service '{name}': `restart` must be a string"));
        }
    };
    match raw {
        "no" => Ok(RestartPolicy::Never),
        "always" | "unless-stopped" => Ok(RestartPolicy::Always),
        s if s == "on-failure" || s.starts_with("on-failure:") => Ok(RestartPolicy::OnFailure),
        other => Err(format!(
            "service '{name}': unsupported restart policy '{other}' (fail-closed)"
        )),
    }
}

/// Stringify a scalar YAML value (string / number / bool). Non-scalars → `None`.
fn scalar_to_string(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Truthiness for `privileged` / `disable`: only `true` counts.
fn truthy(val: &Value) -> bool {
    matches!(val, Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEB_POSTGRES: &str = r#"
services:
  web:
    image: nginx:1.25
    ports:
      - "8080:8080"
    depends_on:
      - postgres
    restart: always
  postgres:
    image: postgres:16
    expose:
      - "5432"
    environment:
      POSTGRES_DB: app
"#;

    #[test]
    fn two_service_graph_has_public_service_and_dependency() {
        let g = compose_to_graph(WEB_POSTGRES).unwrap();
        assert_eq!(g.public_service, "web");
        // Canonical order → postgres before web.
        assert_eq!(
            g.services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["postgres", "web"]
        );

        let web = g.services.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(web.image_ref, "nginx:1.25");
        assert_eq!(web.port, Some(8080));
        assert_eq!(web.restart, RestartPolicy::Always);
        assert!(web.resolved_digest.is_empty());

        let pg = g.services.iter().find(|s| s.name == "postgres").unwrap();
        assert_eq!(pg.port, Some(5432)); // from `expose`
        assert_eq!(pg.env["POSTGRES_DB"], "app");
        assert_eq!(pg.restart, RestartPolicy::Never); // absent → Never

        assert_eq!(g.dependencies.len(), 1);
        assert_eq!(g.dependencies[0].from, "web");
        assert_eq!(g.dependencies[0].to, "postgres");
        assert_eq!(g.dependencies[0].kind, DependencyKind::Ready);
    }

    #[test]
    fn depends_on_condition_maps_ready_vs_success() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    depends_on:
      db:
        condition: service_healthy
      cache:
        condition: service_started
  db:
    image: postgres
    expose: ["5432"]
  cache:
    image: redis
    expose: ["6379"]
"#;
        let g = compose_to_graph(yaml).unwrap();
        let db = g.dependencies.iter().find(|d| d.to == "db").unwrap();
        let cache = g.dependencies.iter().find(|d| d.to == "cache").unwrap();
        assert_eq!(db.kind, DependencyKind::Success);
        assert_eq!(cache.kind, DependencyKind::Ready);
    }

    #[test]
    fn completed_successfully_is_success() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    depends_on:
      migrate:
        condition: service_completed_successfully
  migrate:
    image: migrator
"#;
        let g = compose_to_graph(yaml).unwrap();
        assert_eq!(g.dependencies[0].kind, DependencyKind::Success);
    }

    #[test]
    fn input_order_independence() {
        let reordered = r#"
services:
  postgres:
    image: postgres:16
    expose:
      - "5432"
    environment:
      POSTGRES_DB: app
  web:
    image: nginx:1.25
    depends_on:
      - postgres
    ports:
      - "8080:8080"
    restart: always
"#;
        assert_eq!(
            compose_to_graph(WEB_POSTGRES).unwrap(),
            compose_to_graph(reordered).unwrap()
        );
    }

    #[test]
    fn restart_normalization() {
        let cases = [
            ("no", RestartPolicy::Never),
            ("always", RestartPolicy::Always),
            ("unless-stopped", RestartPolicy::Always),
            ("on-failure", RestartPolicy::OnFailure),
            ("on-failure:5", RestartPolicy::OnFailure),
        ];
        for (raw, expected) in cases {
            let yaml = format!(
                "services:\n  web:\n    image: nginx\n    ports: [\"80:80\"]\n    restart: {raw}\n"
            );
            let g = compose_to_graph(&yaml).unwrap();
            assert_eq!(g.services[0].restart, expected, "restart={raw}");
        }
        // Unknown policy fails closed.
        let bad = "services:\n  web:\n    image: nginx\n    ports: [\"80:80\"]\n    restart: bogus\n";
        assert!(compose_to_graph(bad).unwrap_err().contains("restart policy"));
    }

    /// Wrap a single-service snippet with a public web service so only the
    /// dangerous key under test is the cause of any rejection.
    fn with_bad(extra: &str) -> String {
        format!("services:\n  web:\n    image: nginx\n    ports: [\"80:80\"]\n{extra}")
    }

    #[test]
    fn each_rejected_key_fails_closed() {
        let cases: &[(&str, &str)] = &[
            ("    privileged: true\n", "privileged"),
            (
                "    volumes:\n      - /var/run/docker.sock:/var/run/docker.sock\n",
                "Docker socket",
            ),
            ("    network_mode: host\n", "network_mode: host"),
            ("    network_mode: \"service:other\"\n", "container-runtime network"),
            ("    devices:\n      - /dev/snd\n", "devices"),
            ("    cap_add:\n      - NET_ADMIN\n", "cap_add"),
            (
                "    build:\n      context: .\n      secrets:\n        - db_password\n",
                "build",
            ),
            ("    secrets:\n      - db_password\n", "secrets"),
            ("    volumes:\n      - ./host/data:/data\n", "bind mount"),
            ("    volumes:\n      - /abs/host:/data\n", "bind mount"),
            ("    pid: host\n", "container-runtime"),
        ];
        for (extra, needle) in cases {
            let yaml = with_bad(extra);
            let err = compose_to_graph(&yaml).expect_err(&format!("expected rejection for {extra:?}"));
            assert!(err.contains(needle), "for {extra:?} got: {err}");
        }
    }

    #[test]
    fn external_network_rejected() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
networks:
  default:
    external: true
"#;
        let err = compose_to_graph(yaml).unwrap_err();
        assert!(err.contains("external Docker network"), "{err}");
    }

    #[test]
    fn ambiguous_public_port_rejected() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
  api:
    image: api
    ports: ["3000:3000"]
"#;
        let err = compose_to_graph(yaml).unwrap_err();
        assert!(err.contains("ambiguous public service"), "{err}");
    }

    #[test]
    fn zero_public_port_rejected() {
        let yaml = r#"
services:
  worker:
    image: worker
    expose: ["9000"]
"#;
        let err = compose_to_graph(yaml).unwrap_err();
        assert!(err.contains("no service publishes a public port"), "{err}");
    }

    #[test]
    fn unknown_dependency_rejected() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    depends_on:
      - ghost
"#;
        let err = compose_to_graph(yaml).unwrap_err();
        assert!(err.contains("unknown service 'ghost'"), "{err}");
    }

    #[test]
    fn unsupported_key_fails_closed() {
        let yaml = with_bad("    labels:\n      - com.example=1\n");
        let err = compose_to_graph(&yaml).unwrap_err();
        assert!(err.contains("unsupported compose key `labels`"), "{err}");
    }

    #[test]
    fn extension_field_and_privileged_false_are_allowed() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    privileged: false
    x-custom: whatever
"#;
        let g = compose_to_graph(yaml).unwrap();
        assert_eq!(g.public_service, "web");
    }

    #[test]
    fn command_entrypoint_and_env_list_normalized() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    entrypoint: ["/entry.sh"]
    command: ["nginx", "-g", "daemon off;"]
    environment:
      - FOO=bar
      - BARE_KEY
"#;
        let g = compose_to_graph(yaml).unwrap();
        let web = &g.services[0];
        assert_eq!(web.command, vec!["/entry.sh", "nginx", "-g", "daemon off;"]);
        assert_eq!(web.env["FOO"], "bar");
        assert_eq!(web.env["BARE_KEY"], "");
    }

    #[test]
    fn shell_form_command_wrapped() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    command: "nginx -g 'daemon off;'"
"#;
        let g = compose_to_graph(yaml).unwrap();
        assert_eq!(
            g.services[0].command,
            vec!["/bin/sh", "-c", "nginx -g 'daemon off;'"]
        );
    }

    #[test]
    fn healthcheck_normalized_and_disable_honored() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 5s
  side:
    image: side
    expose: ["9000"]
    healthcheck:
      disable: true
"#;
        let g = compose_to_graph(yaml).unwrap();
        let web = g.services.iter().find(|s| s.name == "web").unwrap();
        let hc = web.healthcheck.as_ref().unwrap();
        assert_eq!(hc.test, vec!["curl", "-f", "http://localhost/"]);
        assert_eq!(hc.interval.as_deref(), Some("30s"));
        assert_eq!(hc.retries, Some(3));
        assert_eq!(hc.start_period.as_deref(), Some("5s"));
        let side = g.services.iter().find(|s| s.name == "side").unwrap();
        assert!(side.healthcheck.is_none());
    }

    #[test]
    fn named_and_tmpfs_volumes_kept() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    volumes:
      - pgdata:/var/lib/data
      - type: tmpfs
        target: /scratch
"#;
        let g = compose_to_graph(yaml).unwrap();
        let mounts = &g.services[0].mounts;
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].source.as_deref(), Some("pgdata"));
        assert_eq!(mounts[0].kind, MountKind::Named);
        assert_eq!(mounts[1].kind, MountKind::Tmpfs);
        assert_eq!(mounts[1].target, "/scratch");
    }
}
