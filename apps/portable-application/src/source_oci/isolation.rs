//! Pure isolation check for a private builder session. The facts are read by
//! the Linux session from the kernel and the daemon; the decision is here so
//! every mismatch is testable without a daemon.
use serde::Serialize;

use super::{BuildLimits, Result, err};

/// What was observed about one running private daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IsolationFacts {
    pub daemon_pid: u32,
    pub daemon_comm: String,
    /// `net:[inode]` of the daemon, of PID 1 and of the caller.
    pub daemon_netns: String,
    pub host_netns: String,
    pub caller_netns: String,
    /// Interfaces visible in the daemon's network namespace.
    pub interfaces: Vec<String>,
    /// IPv4 and IPv6 routes through any interface other than `lo`.
    pub non_loopback_routes: usize,
    pub daemon_cgroup: String,
    pub memory_max: String,
    pub pids_max: String,
    pub cpu_max: String,
    /// The socket path is a socket inside the session, and the listening
    /// socket bound there is held by the daemon this session started.
    pub socket_is_socket: bool,
    pub socket_listener_pid: Option<u32>,
    pub reported_root_dir: String,
    pub reported_cgroup_driver: String,
}

/// What the session created and therefore expects to observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IsolationExpectation {
    pub daemon_cgroup: String,
    pub root_dir: String,
}

pub fn cpu_max(limits: &BuildLimits) -> String {
    format!("{} 100000", limits.cpu_limit_millis * 100)
}

/// Every condition must hold, or the session is refused before any image is
/// loaded or any build starts.
pub fn check_isolation(
    facts: &IsolationFacts,
    expected: &IsolationExpectation,
    limits: &BuildLimits,
) -> Result<()> {
    let refuse = |detail: String| Err(err("source_oci_isolation_unconfirmed", detail));
    if facts.daemon_comm != "dockerd" {
        return refuse(format!(
            "session process is {:?}, not dockerd",
            facts.daemon_comm
        ));
    }
    if facts.daemon_netns.is_empty()
        || facts.daemon_netns == facts.host_netns
        || facts.daemon_netns == facts.caller_netns
    {
        return refuse("daemon shares a network namespace with the host or caller".into());
    }
    if facts.interfaces != ["lo"] {
        return refuse(format!(
            "daemon network namespace has {:?}",
            facts.interfaces
        ));
    }
    if facts.non_loopback_routes != 0 {
        return refuse(format!("{} non-loopback routes", facts.non_loopback_routes));
    }
    if facts.daemon_cgroup != expected.daemon_cgroup {
        return refuse(format!("daemon cgroup is {:?}", facts.daemon_cgroup));
    }
    if facts.memory_max != limits.memory_bytes.to_string()
        || facts.pids_max != limits.pids_limit.to_string()
        || facts.cpu_max != cpu_max(limits)
    {
        return refuse(format!(
            "session limits read back as memory={} pids={} cpu={}",
            facts.memory_max, facts.pids_max, facts.cpu_max
        ));
    }
    if !facts.socket_is_socket || facts.socket_listener_pid != Some(facts.daemon_pid) {
        return refuse(format!(
            "socket is not served by the session daemon (listener {:?}, daemon {})",
            facts.socket_listener_pid, facts.daemon_pid
        ));
    }
    if facts.reported_root_dir != expected.root_dir {
        return refuse(format!(
            "daemon reports data root {:?}",
            facts.reported_root_dir
        ));
    }
    if facts.reported_cgroup_driver != "cgroupfs" {
        return refuse(format!(
            "daemon cgroup driver is {:?}; build steps would leave the session cgroup",
            facts.reported_cgroup_driver
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Mutation = Box<dyn Fn(&mut IsolationFacts)>;

    fn limits() -> BuildLimits {
        BuildLimits {
            memory_bytes: 2 << 30,
            cpu_limit_millis: 2000,
            pids_limit: 512,
            disk_bytes: 4 << 30,
        }
    }
    fn expected() -> IsolationExpectation {
        IsolationExpectation {
            daemon_cgroup: "/ato-source-oci-1/daemon".into(),
            root_dir: "/srv/ato/j1/fs/data".into(),
        }
    }
    fn good() -> IsolationFacts {
        IsolationFacts {
            daemon_pid: 42,
            daemon_comm: "dockerd".into(),
            daemon_netns: "net:[2]".into(),
            host_netns: "net:[1]".into(),
            caller_netns: "net:[1]".into(),
            interfaces: vec!["lo".into()],
            non_loopback_routes: 0,
            daemon_cgroup: "/ato-source-oci-1/daemon".into(),
            memory_max: (2_u64 << 30).to_string(),
            pids_max: "512".into(),
            cpu_max: "200000 100000".into(),
            socket_is_socket: true,
            socket_listener_pid: Some(42),
            reported_root_dir: "/srv/ato/j1/fs/data".into(),
            reported_cgroup_driver: "cgroupfs".into(),
        }
    }

    #[test]
    fn the_observed_session_passes() {
        check_isolation(&good(), &expected(), &limits()).unwrap();
    }

    #[test]
    fn every_mismatch_refuses_the_session() {
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "host netns",
                Box::new(|f| f.daemon_netns = f.host_netns.clone()),
            ),
            (
                "caller netns",
                Box::new(|f| {
                    f.caller_netns = "net:[2]".into();
                }),
            ),
            (
                "extra interface",
                Box::new(|f| f.interfaces.push("eth0".into())),
            ),
            ("route", Box::new(|f| f.non_loopback_routes = 1)),
            (
                "cgroup",
                Box::new(|f| f.daemon_cgroup = "/system.slice/docker.service".into()),
            ),
            ("memory", Box::new(|f| f.memory_max = "max".into())),
            ("pids", Box::new(|f| f.pids_max = "max".into())),
            ("cpu", Box::new(|f| f.cpu_max = "max 100000".into())),
            // A socket path that is an alias of another daemon's socket: the
            // listener is not the process this session started.
            (
                "socket alias",
                Box::new(|f| f.socket_listener_pid = Some(1)),
            ),
            (
                "socket missing peer",
                Box::new(|f| f.socket_listener_pid = None),
            ),
            ("not a socket", Box::new(|f| f.socket_is_socket = false)),
            (
                "other daemon root",
                Box::new(|f| f.reported_root_dir = "/var/lib/docker".into()),
            ),
            (
                "systemd driver",
                Box::new(|f| f.reported_cgroup_driver = "systemd".into()),
            ),
            ("not dockerd", Box::new(|f| f.daemon_comm = "sh".into())),
        ];
        for (label, mutate) in cases {
            let mut facts = good();
            mutate(&mut facts);
            assert_eq!(
                check_isolation(&facts, &expected(), &limits())
                    .unwrap_err()
                    .code,
                "source_oci_isolation_unconfirmed",
                "{label}"
            );
        }
    }
}
