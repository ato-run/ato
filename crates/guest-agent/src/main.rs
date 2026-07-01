//! Ready-State **guest-agent binary** (PR 7; #863) — runs INSIDE the guest.
//!
//! It reads [`HostToAgent`] control messages as JSON lines from a stream, drives the
//! [`BindingSession`] + [`TmpfsBindingSink`] (materializing each binding at
//! `/run/ato/bindings/<name>`, `0600`), and writes [`AgentToHost`] responses back.
//!
//! The stream here is stdin/stdout (also how it is exercised in tests). The production
//! transport is an **AF_VSOCK** connection to the host — the framing is identical
//! (newline-delimited JSON), so only the socket accept differs; wiring the vsock
//! listener (and building this binary into the guest rootfs) is the remaining
//! integration step. No secret is ever logged: responses are value-free, and the value
//! only ever lands on tmpfs.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use guest_agent::{BindingSession, TmpfsBindingSink};
use protocol::binding_control::AgentToHost;
use protocol::binding_control::HostToAgent;
use protocol::binding_lease::BindingName;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn main() -> std::io::Result<()> {
    // Required binding names from argv; secrets are delivered to the default tmpfs root
    // (`/run/ato/bindings`), or `ATO_BINDINGS_ROOT` when set (tests point it at a tmp dir).
    let required: Vec<BindingName> =
        std::env::args().skip(1).filter_map(|a| BindingName::parse(a).ok()).collect();
    let sink = match std::env::var("ATO_BINDINGS_ROOT") {
        Ok(r) if !r.is_empty() => TmpfsBindingSink::new(r),
        _ => TmpfsBindingSink::at_default(),
    };
    let mut session = BindingSession::new(required, sink);

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: HostToAgent = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                // Never echo the input back (it may carry a secret); report a generic error.
                let err = AgentToHost::Error { message: format!("malformed control message: {e}") };
                writeln!(out, "{}", serde_json::to_string(&err).unwrap())?;
                out.flush()?;
                continue;
            }
        };
        let is_stop = matches!(msg, HostToAgent::Stop);
        let resp = session.handle(msg, now_ms());
        writeln!(out, "{}", serde_json::to_string(&resp).unwrap())?;
        out.flush()?;
        if is_stop {
            break; // stop-scrub done; the host tears the VM down next.
        }
    }
    Ok(())
}
