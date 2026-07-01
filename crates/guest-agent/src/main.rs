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
//! No secret is ever logged: responses are value-free, and the value only lands on tmpfs.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use guest_agent::vsock::{DEFAULT_VSOCK_PORT, serve_vsock};
use guest_agent::{BindingSession, BindingSink, TmpfsBindingSink};
use protocol::binding_control::AgentToHost;
use protocol::binding_control::HostToAgent;
use protocol::binding_lease::BindingName;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Handle one raw JSON control line → (response JSON, should-stop). Shared by both
/// transports so stdio and vsock behave identically.
fn dispatch<S: BindingSink>(session: &mut BindingSession<S>, line: &str) -> (String, bool) {
    match serde_json::from_str::<HostToAgent>(line) {
        Ok(msg) => {
            let is_stop = matches!(msg, HostToAgent::Stop);
            let resp = session.handle(msg, now_ms());
            (serde_json::to_string(&resp).unwrap(), is_stop)
        }
        Err(e) => {
            // Never echo the input back (it may carry a secret); report a generic error.
            let err = AgentToHost::Error { message: format!("malformed control message: {e}") };
            (serde_json::to_string(&err).unwrap(), false)
        }
    }
}

fn main() -> std::io::Result<()> {
    let mode = std::env::var("ATO_GUEST_AGENT_MODE").unwrap_or_else(|_| "stdio".to_string());

    // Required binding names from argv; secrets are delivered to the default tmpfs root
    // (`/run/ato/bindings`), or `ATO_BINDINGS_ROOT` when set (tests point it at a tmp dir).
    let required: Vec<BindingName> =
        std::env::args().skip(1).filter_map(|a| BindingName::parse(a).ok()).collect();
    let sink = match std::env::var("ATO_BINDINGS_ROOT") {
        Ok(r) if !r.is_empty() => TmpfsBindingSink::new(r),
        _ => TmpfsBindingSink::at_default(),
    };
    let mut session = BindingSession::new(required, sink);

    match mode.as_str() {
        "stdio" => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let (resp, stop) = dispatch(&mut session, &line);
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
            serve_vsock(port, |line| dispatch(&mut session, line))
        }
        other => {
            eprintln!("ato-guest-agent: unknown ATO_GUEST_AGENT_MODE={other:?} (expected stdio|vsock)");
            std::process::exit(2);
        }
    }
}
