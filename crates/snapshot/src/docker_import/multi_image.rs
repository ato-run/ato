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
//! **v1 scope / fail-closed rules**
//! * Service discovery is `/etc/hosts` aliases on the SHARED guest network namespace
//!   — every service reaches another by NAME on loopback (`127.0.0.1 postgres`), so
//!   each service must own a UNIQUE listen port. Two services on the SAME port are
//!   REJECTED: a shared network namespace cannot give them distinct loopback ports.
//!   (Per-service network namespaces + veth — which WOULD allow same-port services —
//!   are future work.)
//! * Exactly one PUBLIC service (the runner proxies one guest port), unique service
//!   names + aliases, `depends_on` must reference declared services (no self-loop).
//!
//! Like [`super::rootfs`] this module is PURE generation + a thin executor: the pack
//! script is a reviewable bash string, unit-tested by asserting on the emitted text
//! (no Docker/mkfs needed in CI); [`pack_multi_image_rootfs`] shells it out on a real
//! builder host.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::rootfs_builder::shell_single_quote;

/// Root under which each service's independently-exported image subtree lives.
pub const SERVICES_ROOT: &str = "/opt/ato/services";

/// The per-service rootfs subtree path inside the packed ext4:
/// `/opt/ato/services/<name>/rootfs`. `name` is a DNS-safe label (validated by
/// [`ImportedServiceGraph::validate`]) so it is a single safe path component.
pub fn service_rootfs_dir(name: &str) -> String {
    format!("{SERVICES_ROOT}/{name}/rootfs")
}

/// Hostnames [`build_etc_hosts`] bakes unconditionally for the loopback entries —
/// a service name or alias may not shadow one (ambiguous DNS).
const RESERVED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "ip6-localhost",
    "ip6-loopback",
];

// TODO reconcile with Phase 4 on merge: Phase 4 introduces the canonical
// `ImportedServiceGraph` / `ImportedService` (compose file → per-service image
// build plan). Until it lands, Phase 5 defines the MINIMAL shape it needs to pack
// + launch a multi-image rootfs. When Phase 4 merges, replace these with its types
// (the fields below are the process-relevant subset Phase 5 consumes).
/// One imported compose service: its independently-built image + the derived launch
/// plan (argv, cwd, env split, unique port, discovery aliases). Non-secret —
/// `bindings_env` maps an env var to a binding NAME, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedService {
    /// Stable, DNS-safe service name (keys the rootfs subtree, logs, `/etc/hosts`).
    pub name: String,
    /// The already-built image tag for THIS service's image (exported into the
    /// service's own subtree). Each service has its own image.
    pub image_tag: String,
    /// The workload argv (ENTRYPOINT + CMD concatenated, exec-form).
    pub cmd: Vec<String>,
    /// Working directory INSIDE this service's rootfs (its image WORKDIR; `/` default).
    pub cwd: String,
    /// Non-secret env applied before bindings.
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> binding name` for this service's secret injection.
    pub bindings_env: BTreeMap<String, String>,
    /// This service's UNIQUE loopback listen port (shared netns ⇒ ports must differ).
    pub port: u16,
    /// Exactly one service is PUBLIC (exposed via the runner proxy); the rest are
    /// internal, reachable only by other services on loopback by name.
    pub public: bool,
    /// Services this one must start after (each must be READY first).
    pub depends_on: Vec<String>,
    /// Extra in-guest DNS aliases (compose `redis` ⇒ also reachable as `cache`).
    pub aliases: Vec<String>,
    /// Declared HTTP readiness path; `None` = TCP-accept is enough.
    pub readiness_http_path: Option<String>,
    /// Ephemeral tmpfs mount paths inside this service's rootfs (ato#1024 opt-in).
    pub tmpfs_volumes: Vec<String>,
}

/// A resolved multi-image import: the set of compose services, each its own image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedServiceGraph {
    pub services: Vec<ImportedService>,
}

/// A service name / DNS alias: 1–63 chars of lowercase `[a-z0-9-]`, not
/// leading/trailing `-` (keys the rootfs subtree, per-service logs, `/etc/hosts`).
fn valid_service_name(name: &str) -> bool {
    let ok_len = (1..=63).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    ok_len && ok_chars && ends(name.bytes().next()) && ends(name.bytes().next_back())
}

impl ImportedServiceGraph {
    /// Fail-closed validation of a multi-image compose graph. Rejects: an empty
    /// graph, an empty/duplicate/invalid service name or empty argv, zero or >1
    /// public service, an unknown/self `depends_on`, a hostname (name or alias)
    /// claimed twice or shadowing a reserved loopback name, and — the v1 network
    /// constraint — two services declaring the SAME listen port.
    pub fn validate(&self) -> Result<(), String> {
        if self.services.is_empty() {
            return Err("multi-image import declares no services (fail-closed)".into());
        }
        let names: BTreeSet<&str> = self.services.iter().map(|s| s.name.as_str()).collect();
        if names.len() != self.services.len() {
            return Err("duplicate service name in the multi-image graph".into());
        }
        let mut public_count = 0usize;
        let mut port_owner: BTreeMap<u16, String> = BTreeMap::new();
        let mut host_owner: BTreeMap<String, String> = BTreeMap::new();
        for svc in &self.services {
            if !valid_service_name(&svc.name) {
                return Err(format!(
                    "service '{}': name must be 1–63 chars of lowercase [a-z0-9-], not \
                     leading/trailing '-'",
                    svc.name
                ));
            }
            if svc.cmd.is_empty() {
                return Err(format!(
                    "service '{}': empty argv (nothing to start)",
                    svc.name
                ));
            }
            if svc.public {
                public_count += 1;
            }
            for dep in &svc.depends_on {
                if dep == &svc.name {
                    return Err(format!("service '{}': depends_on itself", svc.name));
                }
                if !names.contains(dep.as_str()) {
                    return Err(format!(
                        "service '{}': depends_on '{dep}' is not a declared service",
                        svc.name
                    ));
                }
            }
            // v1 same-port rejection: a shared network namespace cannot give two
            // services distinct loopback ports. (Per-service netns + veth would —
            // future work; noted so the message points at the real fix.)
            if let Some(prev) = port_owner.insert(svc.port, svc.name.clone()) {
                return Err(format!(
                    "services '{prev}' and '{}' both listen on port {} — a v1 multi-image \
                     snapshot shares one network namespace, so every service needs a UNIQUE \
                     port (per-service network namespaces + veth are future work)",
                    svc.name, svc.port
                ));
            }
            // Discovery hostnames: name + aliases, DNS-safe + unique + not reserved.
            for host in std::iter::once(&svc.name).chain(svc.aliases.iter()) {
                if !valid_service_name(host) {
                    return Err(format!(
                        "service '{}': hostname/alias '{host}' must be a DNS-safe label \
                         (1–63 chars of lowercase [a-z0-9-], not leading/trailing '-')",
                        svc.name
                    ));
                }
                if RESERVED_HOSTNAMES.contains(&host.as_str()) {
                    return Err(format!(
                        "service '{}': hostname '{host}' is reserved for the loopback entry",
                        svc.name
                    ));
                }
                if let Some(prev) = host_owner.insert(host.clone(), svc.name.clone()) {
                    return Err(format!(
                        "hostname '{host}' is claimed by both service '{prev}' and '{}' — a \
                         service name and every alias must be unique across the capsule",
                        svc.name
                    ));
                }
            }
        }
        match public_count {
            1 => Ok(()),
            0 => Err(
                "no public service: exactly one service must be public (the one \
                      exposed via the runner proxy)"
                    .into(),
            ),
            n => Err(format!(
                "{n} services are public; exactly one may be public in a single-VM \
                 multi-image snapshot (the rest are internal)"
            )),
        }
    }

    /// The services in deterministic (name-sorted) order — the order the pack script
    /// exports them and the supervisor.json lists them, so both are reproducible.
    fn ordered(&self) -> Vec<&ImportedService> {
        let mut v: Vec<&ImportedService> = self.services.iter().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// The PUBLIC service's name (exactly one — enforced by [`validate`]).
    pub fn public_service(&self) -> Option<&str> {
        self.services
            .iter()
            .find(|s| s.public)
            .map(|s| s.name.as_str())
    }

    /// The union of every service's required binding names (the leases the guest
    /// agent waits on), sorted + deduped — the agent's argv.
    pub fn binding_names(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for svc in &self.services {
            for name in svc.bindings_env.values() {
                set.insert(name.clone());
            }
        }
        set.into_iter().collect()
    }
}

/// The `/etc/hosts` a multi-image guest is built with: every service name + alias
/// maps to `127.0.0.1` (single VM ⇒ everything is loopback), so a compose
/// service reaches another by NAME. Uniqueness + reserved-name safety are enforced
/// by [`ImportedServiceGraph::validate`]. Deterministic (name-sorted).
pub fn build_etc_hosts(graph: &ImportedServiceGraph) -> String {
    let mut names: Vec<&str> = Vec::new();
    for svc in &graph.services {
        names.push(&svc.name);
        for a in &svc.aliases {
            names.push(a);
        }
    }
    names.sort_unstable();
    let joined = names.join(" ");
    format!("127.0.0.1 localhost {joined}\n::1 localhost ip6-localhost ip6-loopback\n")
}

/// Build the multi-image `/etc/ato/supervisor.json`: a `services[]` list in the
/// guest-agent schema, each carrying its per-service `rootfs` subtree
/// ([`service_rootfs_dir`]), argv, cwd, env split, readiness, and depends_on. The
/// guest-agent starts each service in its own mount namespace + chroot. No secret
/// value ever appears — only `ENV_VAR -> binding name`.
pub fn build_supervisor_json(graph: &ImportedServiceGraph) -> serde_json::Value {
    let svc_json: Vec<serde_json::Value> = graph
        .ordered()
        .iter()
        .map(|s| {
            let mut obj = serde_json::json!({
                "name": s.name,
                "cmd": s.cmd,
                "cwd": s.cwd,
                "base_env": s.base_env,
                "bindings_env": s.bindings_env,
                "rootfs": service_rootfs_dir(&s.name),
            });
            if !s.depends_on.is_empty() {
                let mut deps = s.depends_on.clone();
                deps.sort();
                obj["depends_on"] = serde_json::json!(deps);
            }
            // Every service has a unique determinable port, so every service gets a
            // readiness block (a dependent can WAIT for it). The public/declared
            // HTTP path is included when set; otherwise TCP-accept.
            let mut r = serde_json::json!({ "port": s.port });
            if let Some(path) = &s.readiness_http_path {
                r["http_path"] = serde_json::json!(path);
            }
            obj["readiness"] = r;
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
/// (validated) so they are safe path components; the supervisor.json + /etc/hosts
/// bodies are written with QUOTED heredocs so the builder host performs no
/// expansion.
pub(crate) fn multi_image_pack_script(
    tool: &str,
    graph: &ImportedServiceGraph,
    size_mib: u64,
) -> String {
    let services = graph.ordered();
    // Per-service export blocks (each image → its own subtree, never overlaid).
    let mut export_blocks = String::new();
    for s in &services {
        let subtree = format!("$BUILD/rootfs{}", service_rootfs_dir(&s.name));
        export_blocks.push_str(&format!(
            "mkdir -p \"{subtree}\"\n\
             CID=$({tool} create {tag})\n\
             CIDS=\"$CIDS $CID\"\n\
             {tool} export \"$CID\" | tar -x -C \"{subtree}\"\n\
             {tool} rm -f \"$CID\" >/dev/null 2>&1 || true; CID=\"\"\n",
            subtree = subtree,
            tool = tool,
            tag = shell_single_quote(&s.image_tag),
        ));
    }
    // Cleanup reaps every created container (runtime CIDS) + every imported image.
    let rmi_tags: String = services
        .iter()
        .map(|s| shell_single_quote(&s.image_tag))
        .collect::<Vec<_>>()
        .join(" ");
    // supervisor.json (services[] with per-service rootfs) + /etc/hosts.
    let cfg = build_supervisor_json(graph);
    let cfg_json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
    let hosts = build_etc_hosts(graph);
    // Agent argv = the union of required binding names (leases to wait on).
    let args = graph
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
{export_blocks}# Stage the guest-agent + its config + /etc/hosts into the BASE rootfs.
mkdir -p "$BUILD/rootfs/usr/local/bin" "$BUILD/rootfs/etc/ato" "$BUILD/rootfs/etc" "$BUILD/rootfs/run/ato/bindings"
cp "$ATO_GUEST_AGENT_BIN" "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
chmod 0755 "$BUILD/rootfs/usr/local/bin/ato-guest-agent"
cat > "$BUILD/rootfs/etc/ato/supervisor.json" <<'ATOSUPERVISORJSON'
{cfg_json}
ATOSUPERVISORJSON
# Service discovery: every service name + alias resolves to loopback (shared netns).
cat > "$BUILD/rootfs/etc/hosts" <<'ATOETCHOSTS'
{hosts}ATOETCHOSTS
# Read-only-bootable init: mount the base pseudo/tmpfs filesystems, then run the
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
mkdir -p /run/ato/bindings
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
        cfg_json = cfg_json,
        hosts = hosts,
        args = args,
        size = size_mib,
    )
}

/// Pack a validated multi-image graph into a bootable ext4 rootfs at `out_ext4`.
/// Same host requirements + env contract as [`super::rootfs::pack_imported_rootfs`]:
/// root (mount), the chosen container tool, and `ATO_GUEST_AGENT_BIN`. The graph is
/// validated first (fail-closed) so a bad compose never reaches a shell-out.
pub fn pack_multi_image_rootfs(
    tool: super::BuildTool,
    graph: &ImportedServiceGraph,
    out_ext4: &Path,
    size_mib: u64,
) -> Result<u64, String> {
    graph.validate()?;
    let script = multi_image_pack_script(tool.as_str(), graph, size_mib);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, port: u16, public: bool) -> ImportedService {
        ImportedService {
            name: name.to_string(),
            image_tag: format!("ato-import-{name}"),
            cmd: vec![format!("{name}-server")],
            cwd: "/".to_string(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            port,
            public,
            depends_on: Vec::new(),
            aliases: Vec::new(),
            readiness_http_path: None,
            tmpfs_volumes: Vec::new(),
        }
    }

    fn web_redis_postgres() -> ImportedServiceGraph {
        let mut web = svc("web", 8080, true);
        web.cmd = vec!["node".into(), "server.js".into()];
        web.cwd = "/srv/app".into();
        web.depends_on = vec!["postgres".into(), "redis".into()];
        web.readiness_http_path = Some("/healthz".into());
        let mut redis = svc("redis", 6379, false);
        redis.aliases = vec!["cache".into()];
        let postgres = svc("postgres", 5432, false);
        ImportedServiceGraph {
            services: vec![web, redis, postgres],
        }
    }

    #[test]
    fn service_rootfs_dir_is_the_per_service_subtree_under_services_root() {
        assert_eq!(service_rootfs_dir("web"), "/opt/ato/services/web/rootfs");
        assert_eq!(
            service_rootfs_dir("postgres"),
            "/opt/ato/services/postgres/rootfs"
        );
        assert!(service_rootfs_dir("redis").starts_with(SERVICES_ROOT));
    }

    #[test]
    fn a_valid_multi_service_graph_validates() {
        assert!(web_redis_postgres().validate().is_ok());
    }

    #[test]
    fn two_services_on_the_same_port_are_rejected() {
        // v1: shared netns ⇒ ports must be unique.
        let g = ImportedServiceGraph {
            services: vec![svc("web", 8080, true), svc("api", 8080, false)],
        };
        let err = g.validate().unwrap_err();
        assert!(err.contains("both listen on port 8080"), "{err}");
        assert!(err.contains("UNIQUE port"), "{err}");
        assert!(
            err.contains("future work"),
            "message points at the real fix: {err}"
        );
    }

    #[test]
    fn zero_or_multiple_public_services_are_rejected() {
        let none = ImportedServiceGraph {
            services: vec![svc("a", 1, false), svc("b", 2, false)],
        };
        assert!(none.validate().unwrap_err().contains("no public service"));
        let two = ImportedServiceGraph {
            services: vec![svc("a", 1, true), svc("b", 2, true)],
        };
        assert!(two.validate().unwrap_err().contains("exactly one"));
    }

    #[test]
    fn duplicate_name_unknown_dep_and_self_dep_fail_closed() {
        let dup = ImportedServiceGraph {
            services: vec![svc("a", 1, true), svc("a", 2, false)],
        };
        assert!(
            dup.validate()
                .unwrap_err()
                .contains("duplicate service name")
        );

        let mut s = svc("web", 8080, true);
        s.depends_on = vec!["ghost".into()];
        assert!(
            ImportedServiceGraph { services: vec![s] }
                .validate()
                .unwrap_err()
                .contains("ghost")
        );

        let mut s = svc("web", 8080, true);
        s.depends_on = vec!["web".into()];
        assert!(
            ImportedServiceGraph { services: vec![s] }
                .validate()
                .unwrap_err()
                .contains("depends_on itself")
        );
    }

    #[test]
    fn a_hostname_claimed_twice_or_shadowing_loopback_is_rejected() {
        // redis alias collides with the postgres service name.
        let mut redis = svc("redis", 6379, false);
        redis.aliases = vec!["postgres".into()];
        let g = ImportedServiceGraph {
            services: vec![svc("web", 8080, true), redis, svc("postgres", 5432, false)],
        };
        assert!(g.validate().unwrap_err().contains("claimed by both"));

        // An alias shadowing `localhost` is rejected.
        let mut web = svc("web", 8080, true);
        web.aliases = vec!["localhost".into()];
        let g = ImportedServiceGraph {
            services: vec![web],
        };
        assert!(g.validate().unwrap_err().contains("reserved"));
    }

    #[test]
    fn etc_hosts_maps_every_name_and_alias_to_loopback() {
        let hosts = build_etc_hosts(&web_redis_postgres());
        for h in ["web", "redis", "cache", "postgres", "localhost"] {
            assert!(hosts.contains(h), "hosts missing {h}:\n{hosts}");
        }
        assert!(hosts.contains("127.0.0.1 localhost"), "{hosts}");
        assert!(
            hosts.contains("::1 localhost ip6-localhost ip6-loopback"),
            "{hosts}"
        );
    }

    #[test]
    fn supervisor_json_carries_a_per_service_rootfs_and_readiness_and_deps() {
        let g = web_redis_postgres();
        let json = build_supervisor_json(&g);
        let arr = json["services"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // Deterministic name-sorted order: postgres, redis, web.
        assert_eq!(arr[0]["name"], "postgres");
        assert_eq!(arr[2]["name"], "web");
        // Each service is chrooted into its OWN subtree.
        assert_eq!(arr[2]["rootfs"], "/opt/ato/services/web/rootfs");
        assert_eq!(arr[0]["rootfs"], "/opt/ato/services/postgres/rootfs");
        // web depends_on (sorted) + HTTP readiness; internal services TCP-accept.
        assert_eq!(
            arr[2]["depends_on"],
            serde_json::json!(["postgres", "redis"])
        );
        assert_eq!(arr[2]["readiness"]["port"], 8080);
        assert_eq!(arr[2]["readiness"]["http_path"], "/healthz");
        assert_eq!(arr[1]["readiness"]["port"], 6379);
        assert!(
            arr[1]["readiness"].get("http_path").is_none(),
            "internal svc is TCP-accept"
        );
        assert_eq!(g.public_service(), Some("web"));
    }

    #[test]
    fn pack_script_exports_each_image_into_its_own_isolated_subtree() {
        let g = web_redis_postgres();
        let script = multi_image_pack_script("podman", &g, 2048);
        // Each image is created + exported INDEPENDENTLY into its own subtree —
        // never a single overlaid `/`.
        for name in ["web", "redis", "postgres"] {
            let subtree = format!("$BUILD/rootfs/opt/ato/services/{name}/rootfs");
            assert!(
                script.contains(&format!("mkdir -p \"{subtree}\"")),
                "no subtree for {name}"
            );
            assert!(
                script.contains(&format!("podman export \"$CID\" | tar -x -C \"{subtree}\"")),
                "no independent export for {name}:\n{script}"
            );
            assert!(
                script.contains(&format!("podman create 'ato-import-{name}'")),
                "no create for {name}"
            );
        }
        // Three distinct create/export pairs (one per image), not one shared export.
        assert_eq!(script.matches("create 'ato-import-").count(), 3);
        assert_eq!(script.matches("tar -x -C").count(), 3);
        // Base rootfs carries the agent + supervisor.json (services[]) + /etc/hosts.
        assert!(script.contains("ATO_GUEST_AGENT_BIN"), "{script}");
        assert!(
            script.contains("/usr/local/bin/ato-guest-agent"),
            "{script}"
        );
        assert!(
            script.contains("\"services\""),
            "supervisor.json is services-shaped"
        );
        assert!(
            script.contains("etc/hosts"),
            "bakes /etc/hosts for discovery"
        );
        assert!(script.contains("127.0.0.1 localhost"), "{script}");
        // Failure-safe cleanup reaps containers + images.
        assert!(script.contains("trap cleanup EXIT"), "{script}");
        assert!(
            script.contains("rmi -f 'ato-import-"),
            "cleanup removes imported images"
        );
    }

    #[test]
    fn pack_script_agent_argv_is_the_union_of_binding_names() {
        let mut web = svc("web", 8080, true);
        web.bindings_env = BTreeMap::from([("OPENAI_API_KEY".into(), "openai_api_key".into())]);
        let mut redis = svc("redis", 6379, false);
        redis.bindings_env = BTreeMap::from([("REDIS_PASSWORD".into(), "redis_password".into())]);
        let g = ImportedServiceGraph {
            services: vec![web, redis],
        };
        assert_eq!(g.binding_names(), vec!["openai_api_key", "redis_password"]);
        let script = multi_image_pack_script("docker", &g, 1024);
        assert!(
            script.contains("ato-guest-agent 'openai_api_key' 'redis_password'"),
            "{script}"
        );
    }
}
