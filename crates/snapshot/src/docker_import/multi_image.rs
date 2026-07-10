//! Phase 5 — **multi-image rootfs** (ato#994 follow-on / compose import).
//!
//! v0/v1.7 imports a SINGLE image onto one `/` (see [`super::rootfs`]). A compose
//! app (web + postgres + redis…) is several images that must NOT be overlaid onto
//! one filesystem — their `/usr`, `/etc`, `/var/lib` would clobber each other. This
//! module packs MULTIPLE images into ONE ext4 rootfs, each exported INDEPENDENTLY
//! into its own subtree:
//!
//! ```text
//! /opt/ato/services/web/rootfs/       <- image A, exported whole
//! /opt/ato/services/postgres/rootfs/  <- image B, exported whole
//! /opt/ato/services/redis/rootfs/     <- image C, exported whole
//! ```
//!
//! The base rootfs (the ext4 `/`) carries only the Ato guest-agent + supervisor.json
//! + `/etc/hosts`; the agent starts each service in its OWN mount namespace and
//! `chroot`s it into that subtree (the guest-agent side —
//! `ServiceSpec.rootfs` + `spawn_script`'s `unshare`/`chroot` wrapper).
//!
//! **The graph is the canonical [`crate::compose_plan`] model** (Step 3 of the
//! import roadmap): this module no longer defines its own service/graph shape.
//! What the compose file DECLARES (names, edges, ports, mounts, public service,
//! service-vs-run_once) comes from [`ImportedServiceGraph`]; what only the BUILD
//! phase knows (the per-service image tag, the image config's WORKDIR /
//! ENTRYPOINT+CMD / EXPOSE, and the secret-gated env split) rides in
//! [`ServiceBuildFacts`]. [`MultiImagePackPlan::new`] joins + validates the two
//! fail-closed; everything downstream (supervisor.json, /etc/hosts, the pack
//! script) renders from that one plan.
//!
//! **v1 scope / fail-closed rules**
//! * Service discovery is `/etc/hosts` aliases on the SHARED guest network namespace
//!   — every service reaches another by NAME on loopback (`127.0.0.1 postgres`), so
//!   each long-running service must own a UNIQUE listen port. Two services on the
//!   SAME port are REJECTED. (Per-service network namespaces + veth — which WOULD
//!   allow same-port services — are future work.)
//! * Exactly one PUBLIC service (enforced at compose parse), unique service names
//!   (parse) that are also DNS-safe labels (here), dependency cycles rejected at
//!   parse.
//! * Compose volumes (named / anonymous / tmpfs) all become EPHEMERAL tmpfs inside
//!   the owning service's subtree — state dies on stop/resume BY DESIGN (the
//!   throwaway preview lane; durable state is the persistent-state track).
//! * A [`ServiceKind::RunOnce`] task (a `service_completed_successfully` target,
//!   e.g. a migration container) needs no port and gets `run_at: ["seal_once"]` —
//!   it runs during the BUILD boot, before the pre-seal snapshot; a restore resumes
//!   the sealed memory, so its effects persist into every preview.
//!
//! Like [`super::rootfs`] this module is PURE generation + a thin executor: the pack
//! script is a reviewable bash string, unit-tested by asserting on the emitted text
//! (no Docker/mkfs needed in CI); [`pack_multi_image_rootfs`] shells it out on a real
//! builder host.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::compose_plan::{DependencyKind, ImportedServiceGraph, ServiceKind};
use crate::rootfs_builder::shell_single_quote;

/// Root under which each service's independently-exported image subtree lives.
pub const SERVICES_ROOT: &str = "/opt/ato/services";

/// The per-service rootfs subtree path inside the packed ext4:
/// `/opt/ato/services/<name>/rootfs`. `name` is a DNS-safe label (validated by
/// [`MultiImagePackPlan::new`]) so it is a single safe path component.
pub fn service_rootfs_dir(name: &str) -> String {
    format!("{SERVICES_ROOT}/{name}/rootfs")
}

/// Hostnames [`build_etc_hosts`] bakes unconditionally for the loopback entries —
/// a service name may not shadow one (ambiguous DNS).
const RESERVED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "ip6-localhost",
    "ip6-loopback",
];

/// Build-phase facts for ONE service — everything the pack needs that the compose
/// file cannot declare (it exists only once the image is pulled/built and its
/// config inspected + secret-gated). Non-secret: `bindings_env` maps an env var
/// to a binding NAME, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceBuildFacts {
    /// The already-built/pulled image tag (or pinned digest ref) for THIS
    /// service — what the pack script `create`s + `export`s.
    pub image_tag: String,
    /// The image config's ENTRYPOINT ++ CMD (exec form) — the argv fallback when
    /// the compose service declares no `entrypoint`/`command` override.
    pub image_cmd: Vec<String>,
    /// The image config's WORKDIR (`/` default).
    pub cwd: String,
    /// Secret-gated plain env for this service (the caller merges compose `env`
    /// over the image env and runs the partition — compose wins on key clash).
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> binding name` for this service's secret injection.
    pub bindings_env: BTreeMap<String, String>,
    /// The image config's single EXPOSE port, if any — the port fallback when
    /// the compose service declares neither `ports:` nor `expose:`.
    pub exposed_port: Option<u16>,
}

/// The joined, validated pack input: the canonical compose graph + per-service
/// build facts. Construction ([`Self::new`]) is the single fail-closed gate;
/// every renderer below takes the plan, so an unvalidated combination can never
/// reach a shell-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiImagePackPlan<'g> {
    pub graph: &'g ImportedServiceGraph,
    facts: BTreeMap<String, ServiceBuildFacts>,
    /// Effective argv per service (compose override or image fallback).
    cmds: BTreeMap<String, Vec<String>>,
    /// Effective listen port per long-running service (run_once tasks have none).
    ports: BTreeMap<String, u16>,
    /// Declared HTTP readiness path for the PUBLIC service (import request param;
    /// `None` = TCP-accept).
    public_readiness_http_path: Option<String>,
}

/// A service name: 1–63 chars of lowercase `[a-z0-9-]`, not leading/trailing `-`
/// (keys the rootfs subtree, per-service logs, `/etc/hosts`).
fn valid_service_name(name: &str) -> bool {
    let ok_len = (1..=63).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    ok_len && ok_chars && ends(name.bytes().next()) && ends(name.bytes().next_back())
}

impl<'g> MultiImagePackPlan<'g> {
    /// Join the canonical graph with the per-service build facts, fail-closed.
    ///
    /// The compose parse already rejected duplicate names, zero/many public
    /// services, unknown/self `depends_on`, and dependency cycles — this gate
    /// adds what only the joined view can check: facts present for EXACTLY the
    /// graph's services, DNS-safe names (they become paths + hostnames), a
    /// non-empty effective argv, a determinable UNIQUE port per long-running
    /// service, and mount targets that are safe to render.
    pub fn new(
        graph: &'g ImportedServiceGraph,
        facts: BTreeMap<String, ServiceBuildFacts>,
        public_readiness_http_path: Option<String>,
    ) -> Result<Self, String> {
        if graph.services.is_empty() {
            return Err("multi-image import declares no services (fail-closed)".into());
        }
        let names: BTreeSet<&str> = graph.services.iter().map(|s| s.name.as_str()).collect();
        if let Some(extra) = facts.keys().find(|k| !names.contains(k.as_str())) {
            return Err(format!(
                "build facts for '{extra}' have no matching service in the graph (fail-closed)"
            ));
        }

        let mut cmds: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut ports: BTreeMap<String, u16> = BTreeMap::new();
        let mut port_owner: BTreeMap<u16, String> = BTreeMap::new();

        for svc in &graph.services {
            if !valid_service_name(&svc.name) {
                return Err(format!(
                    "service '{}': name must be 1–63 chars of lowercase [a-z0-9-], not \
                     leading/trailing '-' (it keys the rootfs subtree and /etc/hosts)",
                    svc.name
                ));
            }
            if RESERVED_HOSTNAMES.contains(&svc.name.as_str()) {
                return Err(format!(
                    "service '{}': hostname is reserved for the loopback entry",
                    svc.name
                ));
            }
            let f = facts.get(&svc.name).ok_or_else(|| {
                format!(
                    "service '{}': no build facts (image not built/pulled?) — fail-closed",
                    svc.name
                )
            })?;
            if f.image_tag.trim().is_empty() {
                return Err(format!("service '{}': empty image tag", svc.name));
            }
            let cmd = if svc.command.is_empty() {
                f.image_cmd.clone()
            } else {
                svc.command.clone()
            };
            if cmd.is_empty() {
                return Err(format!(
                    "service '{}': neither the compose service nor its image declares a \
                     command (nothing to start)",
                    svc.name
                ));
            }
            cmds.insert(svc.name.clone(), cmd);

            // Mount targets render into the init — same shell-safety bar as the
            // single-image ephemeral mounts.
            for m in &svc.mounts {
                super::rootfs::validate_ephemeral_mount_path(&m.target)
                    .map_err(|e| format!("service '{}': volume target: {e}", svc.name))?;
            }

            let effective_port = svc.port.or(f.exposed_port);
            match (svc.kind, effective_port) {
                (ServiceKind::RunOnce, _) => {
                    // A one-shot task listens on nothing; any declared port is
                    // ignored for the same-port rule (it exits).
                }
                (ServiceKind::Service, Some(p)) => {
                    if let Some(prev) = port_owner.insert(p, svc.name.clone()) {
                        return Err(format!(
                            "services '{prev}' and '{}' both listen on port {p} — a v1 \
                             multi-image snapshot shares one network namespace, so every \
                             service needs a UNIQUE port (per-service network namespaces + \
                             veth are future work)",
                            svc.name
                        ));
                    }
                    ports.insert(svc.name.clone(), p);
                }
                (ServiceKind::Service, None) => {
                    return Err(format!(
                        "service '{}': no determinable port (no compose ports:/expose: and \
                         the image EXPOSEs nothing) — a long-running service must be \
                         waitable (fail-closed)",
                        svc.name
                    ));
                }
            }
        }
        Ok(MultiImagePackPlan {
            graph,
            facts,
            cmds,
            ports,
            public_readiness_http_path,
        })
    }

    /// The effective argv for a service (compose override or image fallback).
    pub fn cmd(&self, name: &str) -> &[String] {
        self.cmds.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The effective listen port of a long-running service (`None` for run_once).
    pub fn port(&self, name: &str) -> Option<u16> {
        self.ports.get(name).copied()
    }

    /// The union of every service's required binding names (the leases the guest
    /// agent waits on), sorted + deduped — the agent's argv.
    pub fn binding_names(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for f in self.facts.values() {
            for name in f.bindings_env.values() {
                set.insert(name.clone());
            }
        }
        set.into_iter().collect()
    }

    fn facts_of(&self, name: &str) -> &ServiceBuildFacts {
        // Guarded by `new` (facts exist for every graph service).
        self.facts.get(name).expect("validated plan")
    }
}

/// The `/etc/hosts` a multi-image guest is built with: every service name maps to
/// `127.0.0.1` (single VM ⇒ everything is loopback), so a compose service reaches
/// another by NAME. Deterministic (graph services are name-sorted).
pub fn build_etc_hosts(plan: &MultiImagePackPlan<'_>) -> String {
    let names: Vec<&str> = plan
        .graph
        .services
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    let joined = names.join(" ");
    format!("127.0.0.1 localhost {joined}\n::1 localhost ip6-localhost ip6-loopback\n")
}

/// Build the multi-image `/etc/ato/supervisor.json`: a `services[]` list in the
/// guest-agent schema, each carrying its per-service `rootfs` subtree
/// ([`service_rootfs_dir`]), effective argv, cwd, env split, readiness, and the
/// Phase-6 DAG fields — `depends_on_ready` from Ready/Healthy edges,
/// `depends_on_success` from Completed edges, and `kind: run_once` +
/// `run_at: ["seal_once"]` for one-shot tasks. No secret value ever appears —
/// only `ENV_VAR -> binding name`.
pub fn build_supervisor_json(plan: &MultiImagePackPlan<'_>) -> serde_json::Value {
    // Edges grouped by dependent, split by wait kind.
    let mut ready_deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut success_deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for d in &plan.graph.dependencies {
        match d.kind {
            DependencyKind::Ready | DependencyKind::Healthy => {
                ready_deps
                    .entry(d.from.as_str())
                    .or_default()
                    .push(d.to.as_str());
            }
            DependencyKind::Completed => {
                success_deps
                    .entry(d.from.as_str())
                    .or_default()
                    .push(d.to.as_str());
            }
        }
    }
    let svc_json: Vec<serde_json::Value> = plan
        .graph
        .services
        .iter()
        .map(|s| {
            let f = plan.facts_of(&s.name);
            let mut obj = serde_json::json!({
                "name": s.name,
                "cmd": plan.cmd(&s.name),
                "cwd": f.cwd,
                "base_env": f.base_env,
                "bindings_env": f.bindings_env,
                "rootfs": service_rootfs_dir(&s.name),
            });
            if let Some(deps) = ready_deps.get(s.name.as_str()) {
                let mut deps: Vec<&str> = deps.clone();
                deps.sort_unstable();
                obj["depends_on_ready"] = serde_json::json!(deps);
            }
            if let Some(deps) = success_deps.get(s.name.as_str()) {
                let mut deps: Vec<&str> = deps.clone();
                deps.sort_unstable();
                obj["depends_on_success"] = serde_json::json!(deps);
            }
            match s.kind {
                ServiceKind::RunOnce => {
                    // One-shot task: runs during the BUILD boot (before the
                    // pre-seal snapshot); a restore resumes the sealed memory,
                    // so it never re-runs per preview.
                    obj["kind"] = serde_json::json!("run_once");
                    obj["run_at"] = serde_json::json!(["seal_once"]);
                }
                ServiceKind::Service => {
                    // Long-running: a readiness block so dependents can WAIT.
                    // The public service carries the declared HTTP path; internal
                    // services are TCP-accept.
                    if let Some(port) = plan.port(&s.name) {
                        let mut r = serde_json::json!({ "port": port });
                        if s.name == plan.graph.public_service
                            && let Some(path) = &plan.public_readiness_http_path
                        {
                            r["http_path"] = serde_json::json!(path);
                        }
                        obj["readiness"] = r;
                    }
                }
            }
            obj
        })
        .collect();
    serde_json::json!({ "services": svc_json })
}

/// The bash pipeline that packs MULTIPLE already-built images into ONE bootable
/// ext4: for each service, `create` a container from its image and `export` its
/// whole filesystem INDEPENDENTLY into `/opt/ato/services/<name>/rootfs`; then stage
/// the guest-agent + supervisor.json + `/etc/hosts` into the base rootfs and install
/// an init that runs the agent. Kept as a reviewable string; env: ATO_OUT,
/// ATO_GUEST_AGENT_BIN. The images are removed by the cleanup trap.
///
/// Security: image tags are single-quoted; service names are DNS-safe labels
/// (validated) so they are safe path components; volume targets passed the
/// ephemeral-mount path validator so plain interpolation is safe; the
/// supervisor.json + /etc/hosts bodies are written with QUOTED heredocs so the
/// builder host performs no expansion.
pub(crate) fn multi_image_pack_script(
    tool: &str,
    plan: &MultiImagePackPlan<'_>,
    size_mib: u64,
) -> String {
    let services = &plan.graph.services;
    // Per-service export blocks (each image → its own subtree, never overlaid).
    let mut export_blocks = String::new();
    for s in services {
        let subtree = format!("$BUILD/rootfs{}", service_rootfs_dir(&s.name));
        export_blocks.push_str(&format!(
            "mkdir -p \"{subtree}\"\n\
             CID=$({tool} create {tag})\n\
             CIDS=\"$CIDS $CID\"\n\
             {tool} export \"$CID\" | tar -x -C \"{subtree}\"\n\
             {tool} rm -f \"$CID\" >/dev/null 2>&1 || true; CID=\"\"\n",
            subtree = subtree,
            tool = tool,
            tag = shell_single_quote(&plan.facts_of(&s.name).image_tag),
        ));
    }
    // Cleanup reaps every created container (runtime CIDS) + every imported image.
    let rmi_tags: String = services
        .iter()
        .map(|s| shell_single_quote(&plan.facts_of(&s.name).image_tag))
        .collect::<Vec<_>>()
        .join(" ");
    // Compose volumes → EPHEMERAL tmpfs inside the owning service's subtree,
    // mounted by the init before the agent starts. MANAGED state mounts: a
    // failed mount fails guest boot (exit 1), never 2>/dev/null. Targets were
    // validated shell-safe in `MultiImagePackPlan::new`.
    let mut volume_mounts = String::new();
    let mut staged_mountpoints = String::new();
    for s in services {
        for m in &s.mounts {
            let inside = format!("{}{}", service_rootfs_dir(&s.name), m.target);
            // The guest root mounts READ-ONLY: create the mountpoint in the
            // STAGED tree at pack time (an image that declares VOLUME without
            // RUN mkdir ships no directory); the boot-time mkdir stays as a
            // no-op and the mount check stays fail-closed.
            staged_mountpoints.push_str(&format!("mkdir -p \"$BUILD/rootfs{inside}\"\n"));
            volume_mounts.push_str(&format!(
                "mkdir -p {inside}\nmount -t tmpfs tmpfs {inside} || {{ echo \"required tmpfs mount failed: {inside}\" >&2; exit 1; }}\n"
            ));
        }
    }
    // supervisor.json (services[] with per-service rootfs) + /etc/hosts.
    let cfg = build_supervisor_json(plan);
    let cfg_json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
    let hosts = build_etc_hosts(plan);
    // Agent argv = the union of required binding names (leases to wait on).
    let args = plan
        .binding_names()
        .iter()
        .map(|n| shell_single_quote(n))
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        r#"set -euo pipefail
: "${{ATO_GUEST_AGENT_BIN:?ATO_GUEST_AGENT_BIN must point to the guest-agent binary for a multi-image build}}"
CID=""
CIDS=""
MNT=""
BUILD=$(mktemp -d)
# Failure-safe cleanup: on ANY exit leave no container, image, mount, or temp dir.
cleanup() {{
  for c in $CIDS; do {tool} rm -f "$c" >/dev/null 2>&1 || true; done
  {tool} rmi -f {rmi_tags} >/dev/null 2>&1 || true
  if [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null; then umount "$MNT" 2>/dev/null || umount -l "$MNT" 2>/dev/null || true; fi
  [ -n "$MNT" ] && rmdir "$MNT" 2>/dev/null || true
  [ -n "$BUILD" ] && rm -rf "$BUILD" 2>/dev/null || true
}}
trap cleanup EXIT
mkdir -p "$BUILD/rootfs"
# Export each image INDEPENDENTLY into its own subtree (never overlay onto one /).
{export_blocks}# RO-root: create every volume mountpoint while the staged tree is writable.
{staged_mountpoints}# Stage the guest-agent + its config + /etc/hosts into the BASE rootfs.
mkdir -p "$BUILD/rootfs/usr/local/bin" "$BUILD/rootfs/etc/ato" "$BUILD/rootfs/etc" "$BUILD/rootfs/run/ato/bindings"
cp "$ATO_GUEST_AGENT_BIN" "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
chmod 0755 "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
cat > "$BUILD/rootfs/etc/ato/supervisor.json" <<'ATOSUPERVISORJSON'
{cfg_json}
ATOSUPERVISORJSON
# Service discovery: every service name resolves to loopback (shared netns).
cat > "$BUILD/rootfs/etc/hosts" <<'ATOETCHOSTS'
{hosts}ATOETCHOSTS
# Read-only-bootable init: mount the base pseudo/tmpfs filesystems + the compose
# volumes (ephemeral tmpfs inside each service subtree), then run the
# guest-agent (it starts each service in its OWN mount namespace + chroot into
# /opt/ato/services/<svc>/rootfs). QUOTED heredoc — no host expansion.
rm -f "$BUILD/rootfs/sbin/init"
mkdir -p "$BUILD/rootfs/sbin"
cat > "$BUILD/rootfs/sbin/init" <<'INIT'
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mount -t tmpfs tmpfs /tmp 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
{volume_mounts}mkdir -p /run/ato/bindings
export ATO_GUEST_AGENT_MODE=vsock ATO_GUEST_AGENT_VSOCK_PORT=1025 ATO_BINDINGS_ROOT=/run/ato/bindings
/usr/local/bin/ato-guest-agent {args} 2>&1 | tee /tmp/agent.log > /dev/console &
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
        tool = tool,
        rmi_tags = rmi_tags,
        export_blocks = export_blocks,
        staged_mountpoints = staged_mountpoints,
        volume_mounts = volume_mounts,
        cfg_json = cfg_json,
        hosts = hosts,
        args = args,
        size = size_mib,
    )
}

/// Pack a validated multi-image plan into a bootable ext4 rootfs at `out_ext4`.
/// Same host requirements + env contract as [`super::rootfs::pack_imported_rootfs`]:
/// root (mount), the chosen container tool, and `ATO_GUEST_AGENT_BIN`. The plan
/// was validated at construction ([`MultiImagePackPlan::new`]) so a bad compose
/// never reaches a shell-out.
pub fn pack_multi_image_rootfs(
    tool: super::BuildTool,
    plan: &MultiImagePackPlan<'_>,
    out_ext4: &Path,
    size_mib: u64,
) -> Result<u64, String> {
    let script = multi_image_pack_script(tool.as_str(), plan, size_mib);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("ATO_OUT", out_ext4)
        .output()
        .map_err(|e| format!("spawn multi-image rootfs pack: {e}"))?;
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
        return Err(format!("multi-image rootfs pack failed: {tail}"));
    }
    std::fs::metadata(out_ext4)
        .map(|m| m.len())
        .map_err(|e| e.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::compose_plan::compose_to_graph;

    /// The canonical three-service fixture: public web + postgres + a one-shot
    /// migration task (the Blinko shape).
    const COMPOSE: &str = r#"
services:
  web:
    image: ghost:5
    ports: ["8080:2368"]
    depends_on:
      postgres:
        condition: service_healthy
      migrate:
        condition: service_completed_successfully
  postgres:
    image: postgres:16
    expose: ["5432"]
    volumes:
      - dbdata:/var/lib/postgresql/data
  migrate:
    image: ghost:5
    command: ["node", "migrate.js"]
    depends_on:
      postgres:
        condition: service_healthy
volumes:
  dbdata:
"#;

    fn facts_for(graph: &ImportedServiceGraph) -> BTreeMap<String, ServiceBuildFacts> {
        graph
            .services
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    ServiceBuildFacts {
                        image_tag: format!("ato-import-{}", s.name),
                        image_cmd: vec!["docker-entrypoint.sh".into(), "start".into()],
                        cwd: "/srv".into(),
                        base_env: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
                        bindings_env: BTreeMap::new(),
                        exposed_port: None,
                    },
                )
            })
            .collect()
    }

    fn plan_of(graph: &ImportedServiceGraph) -> MultiImagePackPlan<'_> {
        MultiImagePackPlan::new(graph, facts_for(graph), Some("/health".into()))
            .expect("valid plan")
    }

    // --- canonical graph → pack plan -------------------------------------------

    #[test]
    fn canonical_graph_joins_with_build_facts_into_a_plan() {
        let g = compose_to_graph(COMPOSE).unwrap();
        let plan = plan_of(&g);
        // Compose command override wins; image fallback applies otherwise.
        assert_eq!(plan.cmd("migrate"), ["node", "migrate.js"]);
        assert_eq!(plan.cmd("web"), ["docker-entrypoint.sh", "start"]);
        // Ports: web from ports:, postgres from expose:, migrate none (run_once).
        assert_eq!(plan.port("web"), Some(2368));
        assert_eq!(plan.port("postgres"), Some(5432));
        assert_eq!(plan.port("migrate"), None);
    }

    #[test]
    fn missing_build_facts_fail_closed() {
        let g = compose_to_graph(COMPOSE).unwrap();
        let mut facts = facts_for(&g);
        facts.remove("postgres");
        let err = MultiImagePackPlan::new(&g, facts, None).unwrap_err();
        assert!(err.contains("no build facts"), "{err}");
        // …and stray facts for an undeclared service too.
        let mut facts = facts_for(&g);
        facts.insert("ghost".into(), ServiceBuildFacts::default());
        let err = MultiImagePackPlan::new(&g, facts, None).unwrap_err();
        assert!(err.contains("no matching service"), "{err}");
    }

    #[test]
    fn a_service_with_no_determinable_port_is_rejected() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    depends_on: [worker]
  worker:
    image: worker
"#;
        let g = compose_to_graph(yaml).unwrap();
        // worker: no ports/expose in compose and no image EXPOSE in facts.
        let err = MultiImagePackPlan::new(&g, facts_for(&g), None).unwrap_err();
        assert!(err.contains("no determinable port"), "{err}");
        // The image EXPOSE fallback fixes it.
        let mut facts = facts_for(&g);
        facts.get_mut("worker").unwrap().exposed_port = Some(9000);
        assert!(MultiImagePackPlan::new(&g, facts, None).is_ok());
    }

    #[test]
    fn same_port_services_are_rejected() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports: ["80:8080"]
  api:
    image: api
    expose: ["8080"]
"#;
        let g = compose_to_graph(yaml).unwrap();
        let err = MultiImagePackPlan::new(&g, facts_for(&g), None).unwrap_err();
        assert!(err.contains("both listen on port 8080"), "{err}");
    }

    #[test]
    fn plan_is_input_order_independent() {
        // The same services in a different declaration order produce an
        // identical supervisor.json + /etc/hosts + pack script.
        let reordered = r#"
services:
  migrate:
    image: ghost:5
    command: ["node", "migrate.js"]
    depends_on:
      postgres:
        condition: service_healthy
  postgres:
    image: postgres:16
    expose: ["5432"]
    volumes:
      - dbdata:/var/lib/postgresql/data
  web:
    image: ghost:5
    ports: ["8080:2368"]
    depends_on:
      postgres:
        condition: service_healthy
      migrate:
        condition: service_completed_successfully
volumes:
  dbdata:
"#;
        let a = compose_to_graph(COMPOSE).unwrap();
        let b = compose_to_graph(reordered).unwrap();
        let pa = plan_of(&a);
        let pb = plan_of(&b);
        assert_eq!(build_supervisor_json(&pa), build_supervisor_json(&pb));
        assert_eq!(build_etc_hosts(&pa), build_etc_hosts(&pb));
        assert_eq!(
            multi_image_pack_script("docker", &pa, 2048),
            multi_image_pack_script("docker", &pb, 2048)
        );
    }

    #[test]
    fn dependency_start_order_is_stable_through_the_plan() {
        let g = compose_to_graph(COMPOSE).unwrap();
        // postgres first (no deps), then migrate (needs postgres), then web.
        assert_eq!(g.start_order(), vec!["postgres", "migrate", "web"]);
    }

    // --- supervisor.json --------------------------------------------------------

    #[test]
    fn supervisor_json_carries_rootfs_dag_and_run_once() {
        let g = compose_to_graph(COMPOSE).unwrap();
        let json = build_supervisor_json(&plan_of(&g));
        let arr = json["services"].as_array().unwrap();
        // Graph services are name-sorted: migrate, postgres, web.
        assert_eq!(arr[0]["name"], "migrate");
        assert_eq!(arr[1]["name"], "postgres");
        assert_eq!(arr[2]["name"], "web");
        // Per-service chroot subtree.
        assert_eq!(arr[1]["rootfs"], "/opt/ato/services/postgres/rootfs");
        // The one-shot migration task: kind + seal_once timing + readiness-less.
        assert_eq!(arr[0]["kind"], "run_once");
        assert_eq!(arr[0]["run_at"], serde_json::json!(["seal_once"]));
        assert!(arr[0].get("readiness").is_none());
        assert_eq!(arr[0]["depends_on_ready"], serde_json::json!(["postgres"]));
        // web: readiness w/ the declared public HTTP path + split dependencies.
        assert_eq!(arr[2]["readiness"]["port"], 2368);
        assert_eq!(arr[2]["readiness"]["http_path"], "/health");
        assert_eq!(arr[2]["depends_on_ready"], serde_json::json!(["postgres"]));
        assert_eq!(arr[2]["depends_on_success"], serde_json::json!(["migrate"]));
        // postgres: internal TCP-accept readiness, no http path.
        assert_eq!(arr[1]["readiness"]["port"], 5432);
        assert!(arr[1]["readiness"].get("http_path").is_none());
        // No secret value anywhere.
        assert!(!json.to_string().contains("sk-"));
    }

    // --- /etc/hosts + pack script ----------------------------------------------

    #[test]
    fn etc_hosts_maps_every_service_to_loopback() {
        let g = compose_to_graph(COMPOSE).unwrap();
        let hosts = build_etc_hosts(&plan_of(&g));
        assert_eq!(
            hosts,
            "127.0.0.1 localhost migrate postgres web\n::1 localhost ip6-localhost ip6-loopback\n"
        );
    }

    #[test]
    fn pack_script_exports_each_image_into_its_own_subtree() {
        let g = compose_to_graph(COMPOSE).unwrap();
        let script = multi_image_pack_script("docker", &plan_of(&g), 2048);
        for name in ["migrate", "postgres", "web"] {
            assert!(
                script.contains(&format!("$BUILD/rootfs/opt/ato/services/{name}/rootfs")),
                "{name} subtree missing:\n{script}"
            );
            assert!(
                script.contains(&format!("'ato-import-{name}'")),
                "{name} tag"
            );
        }
        // RO-root: the volume mountpoint is created in the STAGED tree at pack time.
        assert!(
            script.contains(
                "mkdir -p \"$BUILD/rootfs/opt/ato/services/postgres/rootfs/var/lib/postgresql/data\""
            ),
            "{script}"
        );
        // Compose volume → fail-closed tmpfs inside the OWNING service's subtree.
        assert!(
            script.contains(
                "mount -t tmpfs tmpfs /opt/ato/services/postgres/rootfs/var/lib/postgresql/data"
            ),
            "{script}"
        );
        assert!(script.contains("required tmpfs mount failed"), "{script}");
        // Base rootfs carries the agent + supervisor.json + /etc/hosts.
        assert!(script.contains("supervisor.json"), "{script}");
        assert!(script.contains("ATOETCHOSTS"), "{script}");
        // Cleanup reaps images.
        assert!(
            script.contains("rmi -f 'ato-import-migrate' 'ato-import-postgres' 'ato-import-web'"),
            "{script}"
        );
    }
}
