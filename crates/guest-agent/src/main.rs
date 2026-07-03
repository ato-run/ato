//! Ready-State **guest-agent binary** (#863) — runs INSIDE the guest.
//!
//! Reads [`HostToAgent`] control messages as newline-delimited JSON, drives the
//! [`BindingSession`] + [`TmpfsBindingSink`] (materializing each binding at
//! `/run/ato/bindings/<name>`, `0600`), and writes value-free [`AgentToHost`]
//! responses back. Two transports, identical framing:
//!
//! - `ATO_GUEST_AGENT_MODE=stdio` (default): over stdin/stdout (tests + the smoke).
//! - `ATO_GUEST_AGENT_MODE=vsock` (PR B): AF_VSOCK listener on
//!   `ATO_GUEST_AGENT_VSOCK_PORT` (default 1025) — the host connects through
//!   Firecracker's vsock UDS. This is the production guest transport.
//!
//! v1.2 (supervisor mode): when `/etc/ato/supervisor.json` is present, the agent
//! also owns the WORKLOAD process — it starts it (with the env composed from the
//! delivered bindings) once the session is bound-ready, and stops it on
//! `StopWorkload`. This is how a secret reaches an env-delivery workload without a
//! host-side environ rewrite (impossible for a snapshotted process).
//!
//! No secret is ever logged: responses are value-free, and the value only lands on
//! tmpfs / the started child's environment.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use guest_agent::supervisor::{
    bindings_root, config_path, ChildWorkload, Supervisor, SupervisorConfig, Workload,
};
use guest_agent::vsock::{serve_vsock, DEFAULT_VSOCK_PORT};
use guest_agent::{BindingSession, BindingSink, TmpfsBindingSink};
use protocol::binding_control::AgentToHost;
use protocol::binding_control::HostToAgent;
use protocol::binding_lease::BindingName;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The agent runtime: the binding session plus an optional workload supervisor. The
/// supervisor is `None` for a no-binding / non-env-delivery (v1.0) capsule, in which
/// case the agent only delivers/scrubs bindings and the init launches the app itself.
struct AgentRuntime<S: BindingSink, W: Workload> {
    session: BindingSession<S>,
    supervisor: Option<Supervisor<W>>,
}

impl<S: BindingSink, W: Workload> AgentRuntime<S, W> {
    /// Handle one control message → (response JSON, should-stop). `StopWorkload` is
    /// routed to the supervisor (not the binding session); every other message goes
    /// to the session, after which — if the session is now bound-ready — the
    /// supervisor starts the workload exactly once. A supervisor start failure is
    /// reported as an agent `Error` (fail-closed: never claim ready without a
    /// running workload).
    fn dispatch(&mut self, line: &str) -> (String, bool) {
        let msg = match serde_json::from_str::<HostToAgent>(line) {
            Ok(m) => m,
            Err(e) => {
                // Never echo the input back (it may carry a secret).
                return (
                    serde_json::to_string(&AgentToHost::Error {
                        message: format!("malformed control message: {e}"),
                    })
                    .unwrap(),
                    false,
                );
            }
        };

        if let HostToAgent::StopWorkload = msg {
            let resp = match self.supervisor.as_mut() {
                Some(sup) => match sup.stop_workload() {
                    Ok(was_running) => AgentToHost::WorkloadStopped { was_running },
                    Err(e) => AgentToHost::Error { message: format!("stop workload: {e}") },
                },
                None => AgentToHost::WorkloadStopped { was_running: false },
            };
            return (serde_json::to_string(&resp).unwrap(), false);
        }

        let is_stop = matches!(msg, HostToAgent::Stop);
        let now = now_ms();
        let mut resp = self.session.handle(msg, now);

        // Drive the workload after the binding state settles.
        if let Some(sup) = self.supervisor.as_mut() {
            if is_stop {
                let _ = sup.stop_workload();
            } else if let Err(e) = sup.on_bound_ready(self.session.bound_ready(now)) {
                // The bindings are present but the workload failed to start — do not
                // let the host believe the session is serving.
                resp = AgentToHost::Error { message: format!("supervisor start: {e}") };
            }
        }
        (serde_json::to_string(&resp).unwrap(), is_stop)
    }
}

fn main() -> std::io::Result<()> {
    let mode = std::env::var("ATO_GUEST_AGENT_MODE").unwrap_or_else(|_| "stdio".to_string());

    // Required binding names from argv; secrets are delivered to the default tmpfs root
    // (`/run/ato/bindings`), or `ATO_BINDINGS_ROOT` when set (tests point it at a tmp dir).
    let required: Vec<BindingName> =
        std::env::args().skip(1).filter_map(|a| BindingName::parse(a).ok()).collect();
    let root = bindings_root();
    let sink = TmpfsBindingSink::new(&root);
    let session = BindingSession::new(required, sink);

    // Supervisor: present only when /etc/ato/supervisor.json exists. A malformed
    // config for a supervisor capsule fails closed (the agent exits) rather than
    // launching the workload unbound.
    let supervisor = match SupervisorConfig::load(&config_path()) {
        Ok(Some(cfg)) => Some(Supervisor::new(cfg, root.clone(), ChildWorkload::default())),
        Ok(None) => None,
        Err(e) => {
            eprintln!("ato-guest-agent: {e}");
            std::process::exit(2);
        }
    };

    let mut runtime = AgentRuntime { session, supervisor };

    match mode.as_str() {
        "stdio" => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let (resp, stop) = runtime.dispatch(&line);
                writeln!(out, "{resp}")?;
                out.flush()?;
                if stop {
                    break;
                }
            }
            Ok(())
        }
        "vsock" => {
            let port = std::env::var("ATO_GUEST_AGENT_VSOCK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_VSOCK_PORT);
            eprintln!("ato-guest-agent: vsock listening on port {port}");
            serve_vsock(port, |line| runtime.dispatch(line))
        }
        other => {
            eprintln!("ato-guest-agent: unknown ATO_GUEST_AGENT_MODE={other:?} (expected stdio|vsock)");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guest_agent::supervisor::SupervisorConfig;
    use protocol::binding_lease::{BindingLease, BindingLeaseId, SecretValue};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    /// A workload that records the env of each start + how many stops — shared via Rc
    /// so the test can inspect it after moving it into the Supervisor.
    #[derive(Clone, Default)]
    struct SpyWorkload(Rc<SpyState>);
    #[derive(Default)]
    struct SpyState {
        running: RefCell<bool>,
        starts: RefCell<Vec<BTreeMap<String, String>>>,
        stops: RefCell<u32>,
    }
    impl Workload for SpyWorkload {
        fn start(&mut self, _cmd: &[String], _cwd: &str, env: &BTreeMap<String, String>) -> std::io::Result<()> {
            self.0.starts.borrow_mut().push(env.clone());
            *self.0.running.borrow_mut() = true;
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<bool> {
            let was = *self.0.running.borrow();
            *self.0.running.borrow_mut() = false;
            *self.0.stops.borrow_mut() += 1;
            Ok(was)
        }
        fn is_running(&self) -> bool {
            *self.0.running.borrow()
        }
    }

    fn deliver_line(name: &str, secret: &str) -> String {
        // dispatch() stamps `now` from the real wall clock, so the lease must expire
        // in the real future — expires_at_ms = issued + ttl. Use a far-future value
        // (leases are unix-millis vs the guest's real clock).
        let lease = BindingLease::issue(
            BindingLeaseId::new(format!("lease-{name}")),
            BindingName::parse(name).unwrap(),
            SecretValue::new(secret),
            0,
            100_000_000_000_000, // ~year 5138
        );
        serde_json::to_string(&HostToAgent::Deliver(lease.to_delivery())).unwrap()
    }

    fn runtime_with_supervisor(
        dir: &std::path::Path,
    ) -> (AgentRuntime<TmpfsBindingSink, SpyWorkload>, Rc<SpyState>) {
        let spy = SpyWorkload::default();
        let state = spy.0.clone();
        let cfg = SupervisorConfig {
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
        };
        let session = BindingSession::new(vec![BindingName::parse("openai").unwrap()], TmpfsBindingSink::new(dir));
        let sup = Supervisor::new(cfg, dir.to_path_buf(), spy);
        (AgentRuntime { session, supervisor: Some(sup) }, state)
    }

    #[test]
    fn build_flow_delivers_placeholder_starts_workload_then_stopworkload_idles_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());

        // Deliver the PLACEHOLDER binding → session bound-ready → supervisor starts
        // the workload with the placeholder env.
        let (resp, _stop) = rt.dispatch(&deliver_line("openai", "ATO-PLACEHOLDER-nonce"));
        assert!(resp.contains("ack"), "deliver acked: {resp}");
        assert!(spy.running.borrow().to_owned(), "workload started on bound-ready");
        assert_eq!(spy.starts.borrow()[0].get("OPENAI_API_KEY").map(String::as_str), Some("ATO-PLACEHOLDER-nonce"));

        // Host sends StopWorkload before the pre-bind snapshot → workload idled.
        let (resp, stop) = rt.dispatch(&serde_json::to_string(&HostToAgent::StopWorkload).unwrap());
        assert!(!stop);
        assert!(resp.contains("workload_stopped"), "{resp}");
        assert!(resp.contains("\"was_running\":true"), "{resp}");
        assert!(!spy.running.borrow().to_owned(), "workload idle for the snapshot");
        assert_eq!(*spy.stops.borrow(), 1);
    }

    #[test]
    fn restore_flow_restarts_workload_with_the_real_value() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        // Restore delivers the REAL value → bound-ready → workload starts with it.
        rt.dispatch(&deliver_line("openai", "sk-REAL-KEY"));
        assert!(spy.running.borrow().to_owned());
        assert_eq!(spy.starts.borrow().last().unwrap().get("OPENAI_API_KEY").map(String::as_str), Some("sk-REAL-KEY"));
        // The placeholder is never seen here (fresh restore); exactly one start.
        assert_eq!(spy.starts.borrow().len(), 1);
    }

    #[test]
    fn stopworkload_without_a_supervisor_is_a_clean_no_op() {
        // A no-binding capsule has no supervisor; StopWorkload must not error.
        let mut rt: AgentRuntime<TmpfsBindingSink, ChildWorkload> = AgentRuntime {
            session: BindingSession::new(vec![], TmpfsBindingSink::at_default()),
            supervisor: None,
        };
        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::StopWorkload).unwrap());
        assert!(resp.contains("workload_stopped") && resp.contains("\"was_running\":false"), "{resp}");
    }

    #[test]
    fn malformed_line_never_echoes_input() {
        let mut rt: AgentRuntime<TmpfsBindingSink, ChildWorkload> = AgentRuntime {
            session: BindingSession::new(vec![], TmpfsBindingSink::at_default()),
            supervisor: None,
        };
        let (resp, _) = rt.dispatch("{\"kind\":\"deliver\",\"secret\":\"leak-me\"}");
        assert!(resp.contains("malformed"), "{resp}");
        assert!(!resp.contains("leak-me"), "a malformed control line must never be echoed");
    }
}
