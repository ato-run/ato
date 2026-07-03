//! PR 4 (#863): the host-side **bound-ready gate**.
//!
//! Before user traffic is exposed, the host delivers the session's binding leases to
//! the guest-agent over an [`AgentChannel`] and blocks until the agent reports
//! **bound-ready** — else it **fails closed** (never exposes an unbound session). The
//! transport is a trait so this is testable in-process now; PR 7 supplies a real vsock
//! channel. No-binding sessions have no leases, so the gate returns immediately.

use anyhow::{Context, Result, bail};
use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::BindingLease;
use serde::Serialize;

/// Phase 8a-RunGate PR D1 (#912): the run-path binding-preview receipt. Records the
/// decision + outcome for a binding-required Ready-State run — **names and statuses
/// only, NEVER values** (the secret is delivered over vsock and lives only in guest
/// tmpfs). D1 fills the decision fields (no live delivery); D2/D3 fill
/// `binding_delivery_attempted` / `bound_ready` / `traffic_exposed_after_bound_ready`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BindingPreviewReceipt {
    pub binding_preview_enabled: bool,
    pub bindings_required: bool,
    /// Declared binding NAMES only (never values).
    pub binding_names: Vec<String>,
    pub binding_delivery_attempted: bool,
    pub bound_ready: bool,
    pub traffic_exposed_after_bound_ready: bool,
    pub binding_failure_reason: Option<String>,
    /// L3 (#912): which SecretResolver would supply the values (`env` in preview) —
    /// resolver id only, never a value. `None` when no binding delivery is planned.
    pub resolver_kind: Option<String>,
    /// v1.2 PR 2: the grant namespace (`rs-<hash16>`) the resolver read from — a
    /// manifest-hash prefix, non-secret by construction. `None` when no binding
    /// delivery is planned.
    pub grant_namespace: Option<String>,
}

impl BindingPreviewReceipt {
    /// The D1 decision: is the preview enabled, does the capsule require bindings, and
    /// which names — with delivery not yet attempted.
    pub(crate) fn decide(preview_enabled: bool, required_names: Vec<String>) -> Self {
        BindingPreviewReceipt {
            binding_preview_enabled: preview_enabled,
            bindings_required: !required_names.is_empty(),
            binding_names: required_names,
            binding_delivery_attempted: false,
            bound_ready: false,
            traffic_exposed_after_bound_ready: false,
            binding_failure_reason: None,
            resolver_kind: None,
            grant_namespace: None,
        }
    }

    /// Record the receipt: write `<out_dir>/binding-preview.json` (names only) + a
    /// structured log line. Best-effort; never fails the run.
    pub(crate) fn record(&self, out_dir: &std::path::Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(out_dir);
            let _ = std::fs::write(out_dir.join("binding-preview.json"), json);
        }
        tracing::info!(
            target: "ato::ready_state",
            preview = self.binding_preview_enabled,
            bindings_required = self.bindings_required,
            names = ?self.binding_names,
            delivery_attempted = self.binding_delivery_attempted,
            bound_ready = self.bound_ready,
            reason = ?self.binding_failure_reason,
            "READY-STATE binding preview"
        );
    }
}

/// The host's request/response channel to the guest-agent (vsock in production; an
/// in-process mock in tests).
#[allow(dead_code)] // wired into the run gate in PR C
pub(crate) trait AgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost>;
}

/// PR B (#912): the real host→guest-agent channel over Firecracker's vsock UDS.
///
/// Firecracker exposes a host Unix socket (`uds_path`); to reach a guest AF_VSOCK
/// listener on `guest_port` the host connects to that UDS and sends `CONNECT
/// <guest_port>\n`, expecting `OK <host_port>\n`, after which the stream carries the
/// newline-delimited JSON control protocol to/from the guest-agent. All reads/writes
/// are timeout-bounded; any protocol error fails closed.
#[allow(dead_code)] // wired into the run gate in PR C
pub(crate) struct FirecrackerAgentChannel {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

#[allow(dead_code)]
impl FirecrackerAgentChannel {
    /// Connect through the FC vsock UDS to the guest-agent on `guest_port`.
    pub(crate) fn connect(
        uds_path: &std::path::Path,
        guest_port: u32,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(uds_path)
            .with_context(|| format!("connect FC vsock UDS {}", uds_path.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        // Firecracker host→guest CONNECT handshake.
        writeln!(stream, "CONNECT {guest_port}")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("OK") {
            bail!("guest-agent vsock CONNECT rejected on port {guest_port}: {line:?}");
        }
        Ok(FirecrackerAgentChannel { reader, writer: stream })
    }
}

impl AgentChannel for FirecrackerAgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost> {
        use std::io::{BufRead, Write};
        let json = serde_json::to_string(&msg)?;
        writeln!(self.writer, "{json}")?;
        self.writer.flush()?;
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 || line.trim().is_empty() {
            bail!("guest-agent closed the vsock stream / empty response");
        }
        serde_json::from_str(&line).with_context(|| format!("parse agent response: {line:?}"))
    }
}

/// Deliver every lease, then poll bound-ready up to `max_polls` times. Returns `Ok`
/// only when the agent reports bound-ready; otherwise **fails closed** with the still
/// pending binding names. The caller must not expose the port unless this returns `Ok`.
#[allow(dead_code)] // wired into the run gate in PR 6
pub(crate) fn establish_bindings(
    channel: &mut dyn AgentChannel,
    leases: &[BindingLease],
    max_polls: u32,
) -> Result<()> {
    for lease in leases {
        match channel.request(HostToAgent::Deliver(lease.to_delivery()))? {
            AgentToHost::Ack { .. } => {}
            AgentToHost::Error { message } => bail!("binding delivery failed: {message}"),
            other => bail!("unexpected agent response to Deliver: {other:?}"),
        }
    }
    let mut last_pending: Vec<String> = Vec::new();
    for _ in 0..max_polls.max(1) {
        match channel.request(HostToAgent::QueryBoundReady)? {
            AgentToHost::BoundReady { ready: true, .. } => return Ok(()),
            AgentToHost::BoundReady { ready: false, pending } => {
                last_pending = pending.iter().map(|n| n.as_str().to_string()).collect();
            }
            other => bail!("unexpected agent response to QueryBoundReady: {other:?}"),
        }
    }
    // Fail closed — an unbound session must never serve.
    bail!("bound-ready gate not satisfied; pending bindings: {last_pending:?}");
}

/// PR 6: the **"post-bind state is dirty"** hard invariant (contract §"Hard
/// invariants"). Once ANY binding lease has been attached, the session/VM is **dirty**:
/// its memory and tmpfs may carry the secret, so **no** post-bind snapshot / checkpoint
/// / re-seal is ever allowed — a Ready-State seal must always come from a *pre-bind*
/// boot. This guard fails closed on any such attempt. Bindings live only in the
/// session; the on-disk artifact stays pre-bind + secret-free (#831/#834).
pub(crate) fn ensure_pre_bind_before_seal(session_is_bound: bool) -> Result<()> {
    if session_is_bound {
        bail!(
            "refusing to seal/snapshot a BOUND session: post-bind state is dirty (a lease may \
             live in VM memory or tmpfs). A Ready-State seal must come from a pre-bind boot — \
             never re-seal after binding."
        );
    }
    Ok(())
}

/// v1.2 PR 2: turn pre-resolved `(name, value)` pairs into leases. Resolution
/// happens in [`super::binding_grants::preflight_resolve`] BEFORE the restore
/// starts (aggregated, actionable grant report); the lease clock
/// (`issued/expires`) starts here at delivery time.
pub(crate) fn issue_leases(
    resolved: Vec<(String, protocol::binding_lease::SecretValue)>,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<Vec<BindingLease>> {
    use protocol::binding_lease::{BindingLeaseId, BindingName};
    let mut leases = Vec::with_capacity(resolved.len());
    for (name, value) in resolved {
        let bname = BindingName::parse(name.as_str())
            .map_err(|e| anyhow::anyhow!("invalid binding name '{name}': {e}"))?;
        leases.push(BindingLease::issue(
            BindingLeaseId::new(format!("lease-{name}")),
            bname,
            value,
            now_ms,
            ttl_ms,
        ));
    }
    Ok(leases)
}

/// PR D2: connect the guest-agent over the restored session's vsock UDS, deliver the
/// leases, and block until bound-ready — **fail closed** on any failure. The caller
/// must NOT expose traffic unless this returns `Ok`.
pub(crate) fn bind_before_expose(
    vsock_uds: &std::path::Path,
    leases: &[BindingLease],
    timeout: std::time::Duration,
) -> Result<()> {
    let mut channel = FirecrackerAgentChannel::connect(vsock_uds, 1025, timeout)
        .context("connect guest-agent over vsock")?;
    establish_bindings(&mut channel, leases, 10)
}

/// PR 5: revoke a single lease by id — the agent scrubs that binding's tmpfs file
/// immediately (e.g. a TTL that was not renewed). Returns the agent's `Scrubbed` ack.
/// Wired into the v1.2 PR 2 renewal loop: a grant revoked mid-session revokes here.
pub(crate) fn revoke_binding(
    channel: &mut dyn AgentChannel,
    id: protocol::binding_lease::BindingLeaseId,
) -> Result<()> {
    match channel.request(HostToAgent::Revoke { id })? {
        AgentToHost::Scrubbed { .. } => Ok(()),
        AgentToHost::Error { message } => bail!("revoke failed: {message}"),
        other => bail!("unexpected agent response to Revoke: {other:?}"),
    }
}

/// v1.2 PR 2 (closes L8): renew a lease — protocol-wise, **renew = Deliver again**
/// (the agent atomically replaces the tmpfs file and the presence record's
/// `expires_at_ms`). One `QueryBoundReady` after confirms the session is still
/// fully bound (a cheap health signal on every renewal tick).
pub(crate) fn renew_leases(channel: &mut dyn AgentChannel, leases: &[BindingLease]) -> Result<()> {
    establish_bindings(channel, leases, 1)
}

/// v1.2 PR 2 (closes L8): the host-side **lease renewal loop** for a foreground
/// serving session. Every tick (TTL/3, clamped to [5s, 300s]) it re-resolves
/// each grant from the selected resolver and re-delivers a fresh lease over
/// vsock; a grant revoked mid-session (`ato secrets delete …`) makes the
/// resolve fail → the loop REVOKES that lease so the agent scrubs the tmpfs
/// value immediately and bound-ready drops — the app's next secret read fails,
/// gating traffic without killing the VM. Everything here is best-effort
/// (serving must never crash on a transient store/vsock error) and value-free
/// in logs. The task runs until aborted by the serving loop.
pub(crate) fn spawn_lease_renewal(
    vsock_uds: std::path::PathBuf,
    namespace: String,
    names: Vec<String>,
    ttl_ms: u64,
) -> tokio::task::JoinHandle<()> {
    let interval = std::time::Duration::from_millis((ttl_ms / 3).clamp(5_000, 300_000));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            // Blocking store/vsock I/O — keep it off the async reactor.
            let uds = vsock_uds.clone();
            let ns = namespace.clone();
            let names = names.clone();
            let tick = tokio::task::spawn_blocking(move || renewal_tick(&uds, &ns, &names, ttl_ms));
            match tick.await {
                Ok(Ok(renewed)) => {
                    tracing::debug!(target: "ato::ready_state", renewed, "binding lease renewal tick");
                }
                Ok(Err(e)) => {
                    tracing::warn!(target: "ato::ready_state", error = %e, "binding lease renewal tick failed (will retry)");
                }
                Err(e) => {
                    tracing::warn!(target: "ato::ready_state", error = %e, "binding lease renewal task join error");
                }
            }
        }
    })
}

/// One renewal pass: resolve every grant, re-deliver the resolvable ones,
/// revoke the ones whose grant disappeared. Returns how many leases were
/// renewed. Names/reasons only in errors — never a value.
fn renewal_tick(
    vsock_uds: &std::path::Path,
    namespace: &str,
    names: &[String],
    ttl_ms: u64,
) -> Result<usize> {
    let resolver = super::secret_resolver::select_resolver(namespace)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut resolved: Vec<(String, protocol::binding_lease::SecretValue)> = Vec::new();
    let mut revoked: Vec<String> = Vec::new();
    for name in names {
        match resolver.resolve(name) {
            Ok(value) => resolved.push((name.clone(), value)),
            Err(_) => revoked.push(name.clone()),
        }
    }
    let mut channel = FirecrackerAgentChannel::connect(vsock_uds, 1025, std::time::Duration::from_secs(5))
        .context("connect guest-agent for lease renewal")?;
    for name in &revoked {
        let id = protocol::binding_lease::BindingLeaseId::new(format!("lease-{name}"));
        match revoke_binding(&mut channel, id) {
            Ok(()) => tracing::warn!(
                target: "ato::ready_state",
                binding = %name,
                "grant revoked — lease revoked, guest value scrubbed (traffic gates on next read)"
            ),
            Err(e) => tracing::warn!(
                target: "ato::ready_state",
                binding = %name,
                error = %e,
                "grant revoked but lease revoke failed (expiry will scrub lazily)"
            ),
        }
    }
    let renewed = resolved.len();
    if renewed > 0 {
        let leases = issue_leases(resolved, now_ms, ttl_ms)?;
        renew_over_channel(&mut channel, &leases, revoked.is_empty())?;
    }
    Ok(renewed)
}

/// The channel half of a renewal pass. When `assert_bound_ready` (all grants
/// still resolve) the renewal also asserts the session is fully bound; when a
/// grant was revoked, bound-ready is false BY DESIGN, so the still-granted
/// leases are re-delivered Ack-only (they must not expire too).
fn renew_over_channel(
    channel: &mut dyn AgentChannel,
    leases: &[BindingLease],
    assert_bound_ready: bool,
) -> Result<()> {
    if assert_bound_ready {
        return renew_leases(channel, leases);
    }
    for lease in leases {
        match channel.request(HostToAgent::Deliver(lease.to_delivery()))? {
            AgentToHost::Ack { .. } => {}
            AgentToHost::Error { message } => bail!("renewal delivery failed: {message}"),
            other => bail!("unexpected agent response to Deliver: {other:?}"),
        }
    }
    Ok(())
}

/// PR D3 (#912): connect the guest-agent over vsock and stop-scrub. Used by `ato stop`
/// to wipe the guest's tmpfs bindings BEFORE VM teardown. Best-effort — a connect
/// failure is returned to the caller which logs it and proceeds with teardown.
pub(crate) fn stop_scrub_over_vsock(vsock_uds: &std::path::Path) -> Result<()> {
    let mut channel = FirecrackerAgentChannel::connect(vsock_uds, 1025, std::time::Duration::from_secs(5))
        .context("connect guest-agent for stop-scrub")?;
    stop_scrub(&mut channel)
}

/// PR 5: stop-scrub — on `ato stop`, ask the agent to revoke + scrub **all** bindings
/// (tmpfs wipe) BEFORE the host tears the VM/tap/overlay down. **Never re-seals.** This
/// is best-effort: the VM teardown destroys the tmpfs regardless, so a channel error is
/// logged, not fatal.
pub(crate) fn stop_scrub(channel: &mut dyn AgentChannel) -> Result<()> {
    match channel.request(HostToAgent::Stop) {
        // The agent scrubs all and reports the (now-unbound) state back.
        Ok(AgentToHost::BoundReady { .. }) | Ok(AgentToHost::Scrubbed { .. }) => Ok(()),
        Ok(AgentToHost::Error { message }) => bail!("stop-scrub reported error: {message}"),
        Ok(other) => bail!("unexpected agent response to Stop: {other:?}"),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guest_agent::{BindingSession, TmpfsBindingSink};
    use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

    /// In-process channel: drives a real guest-agent session + tmpfs sink.
    struct MockChannel {
        session: BindingSession<TmpfsBindingSink>,
        now_ms: u64,
    }
    impl AgentChannel for MockChannel {
        fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost> {
            Ok(self.session.handle(msg, self.now_ms))
        }
    }

    fn lease(name: &str, id: &str) -> BindingLease {
        BindingLease::issue(
            BindingLeaseId::new(id),
            BindingName::parse(name).unwrap(),
            SecretValue::new(format!("secret-for-{name}")),
            1_000,
            60_000,
        )
    }

    #[test]
    fn gate_opens_when_all_required_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        let leases = vec![lease("db_url", "l1"), lease("api_key", "l2")];
        establish_bindings(&mut ch, &leases, 3).expect("gate should open");
        // both secrets are on tmpfs.
        assert_eq!(std::fs::read_to_string(dir.path().join("db_url")).unwrap(), "secret-for-db_url");
        assert!(dir.path().join("api_key").exists());
    }

    #[test]
    fn gate_fails_closed_when_a_required_binding_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        // only deliver one of the two required.
        let leases = vec![lease("db_url", "l1")];
        let err = establish_bindings(&mut ch, &leases, 3).unwrap_err().to_string();
        assert!(err.contains("api_key"), "should report the missing binding: {err}");
        assert!(err.contains("pending"), "should fail closed: {err}");
    }

    #[test]
    fn revoke_scrubs_the_binding_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(vec![BindingName::parse("db_url").unwrap()], TmpfsBindingSink::new(dir.path())),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("db_url", "l1")], 2).unwrap();
        assert!(dir.path().join("db_url").exists());
        revoke_binding(&mut ch, BindingLeaseId::new("l1")).unwrap();
        assert!(!dir.path().join("db_url").exists(), "revoke scrubbed the tmpfs file");
    }

    #[test]
    fn stop_scrub_wipes_all_bindings_before_teardown() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("db_url", "l1"), lease("api_key", "l2")], 2).unwrap();
        assert!(dir.path().join("db_url").exists() && dir.path().join("api_key").exists());
        stop_scrub(&mut ch).unwrap();
        assert!(!dir.path().join("db_url").exists(), "stop-scrub wiped db_url");
        assert!(!dir.path().join("api_key").exists(), "stop-scrub wiped api_key");
    }

    /// PR 7: the **no-secret** end-to-end invariant proof (in-process). A full bound
    /// lifecycle must leave the host artifacts secret-free and the tmpfs scrubbed:
    /// the value reaches only the guest tmpfs (never a host receipt / lease dump / the
    /// non-serializable lease), a bound session cannot be re-sealed, and stop-scrub
    /// wipes tmpfs. (The live Firecracker+vsock hardware E2E is the remaining step.)
    #[test]
    fn no_secret_e2e_host_artifacts_stay_clean_and_tmpfs_is_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "TOP-SECRET-DB-PASSWORD-42";
        let l = BindingLease::issue(
            BindingLeaseId::new("l1"),
            BindingName::parse("db_url").unwrap(),
            SecretValue::new(secret),
            1_000,
            60_000,
        );
        let mut ch = MockChannel {
            session: BindingSession::new(vec![BindingName::parse("db_url").unwrap()], TmpfsBindingSink::new(dir.path())),
            now_ms: 1_000,
        };

        // 1. Bind → secret lands ONLY on the guest tmpfs.
        establish_bindings(&mut ch, std::slice::from_ref(&l), 2).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("db_url")).unwrap(), secret);

        // 2. No host-side artifact carries the secret: the loggable receipt is
        //    value-free, the lease Debug redacts, and the lease is not even Serialize.
        let receipt = l.receipt(1_000);
        assert!(!serde_json::to_string(&receipt).unwrap().contains(secret), "host receipt leaked the secret");
        assert!(!format!("{l:?}").contains(secret), "lease Debug leaked the secret");
        assert!(!format!("{receipt:?}").contains(secret));

        // 3. A bound session may never be re-sealed.
        assert!(ensure_pre_bind_before_seal(true).is_err());

        // 4. stop-scrub wipes tmpfs.
        stop_scrub(&mut ch).unwrap();
        assert!(!dir.path().join("db_url").exists(), "tmpfs scrubbed after stop");
    }

    #[test]
    fn stop_scrub_over_vsock_unreachable_returns_err_best_effort() {
        // A missing/unreachable vsock UDS returns Err; the caller (ato stop) logs it
        // and proceeds with teardown (the VM kill scrubs guest tmpfs regardless).
        let err = stop_scrub_over_vsock(std::path::Path::new("/nonexistent/ato-vsock.sock"));
        assert!(err.is_err(), "unreachable vsock ⇒ Err (best-effort)");
    }

    #[test]
    fn issued_leases_carry_the_value_only_on_the_wire_payload() {
        let ok = issue_leases(
            vec![("api_key".to_string(), SecretValue::new("sk-secret-xyz"))],
            1000,
            60_000,
        )
        .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].to_delivery().value.expose(), "sk-secret-xyz");
        assert!(!format!("{:?}", ok[0]).contains("sk-secret-xyz"), "lease Debug must redact");
        // An invalid binding name fails closed at issuance.
        assert!(issue_leases(vec![("bad name!".to_string(), SecretValue::new("v"))], 1000, 60_000).is_err());
    }

    #[test]
    fn binding_preview_receipt_records_names_only_never_values() {
        let r = BindingPreviewReceipt::decide(true, vec!["api_key".into(), "db_url".into()]);
        assert!(r.binding_preview_enabled && r.bindings_required);
        assert_eq!(r.binding_names, vec!["api_key".to_string(), "db_url".to_string()]);
        assert!(!r.binding_delivery_attempted && !r.bound_ready);
        // no value fields exist on the receipt at all — serialize + confirm it is
        // names/flags only.
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("binding_names") && json.contains("api_key"));
        assert!(!json.contains("value"), "receipt must never carry a value: {json}");
        // no-binding capsule ⇒ not required.
        let none = BindingPreviewReceipt::decide(true, vec![]);
        assert!(!none.bindings_required);
    }

    #[test]
    fn sealing_a_bound_session_is_refused() {
        // Pre-bind boot ⇒ allowed (this is how every seal is produced).
        assert!(ensure_pre_bind_before_seal(false).is_ok());
        // Bound session ⇒ refused (post-bind state is dirty, never re-seal).
        let err = ensure_pre_bind_before_seal(true).unwrap_err().to_string();
        assert!(err.contains("post-bind state is dirty"), "{err}");
        assert!(err.to_lowercase().contains("never re-seal"), "{err}");
    }

    #[test]
    fn renewal_extends_leases_and_stays_bound_ready() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("api_key", "l1")], 3).expect("bind");
        // Renew with a fresh lease (renew = Deliver again): value replaced
        // atomically and the session stays bound-ready at the renewed clock.
        let renewed = BindingLease::issue(
            BindingLeaseId::new("l1"),
            BindingName::parse("api_key").unwrap(),
            SecretValue::new("secret-for-api_key-v2"),
            50_000,
            60_000,
        );
        renew_over_channel(&mut ch, std::slice::from_ref(&renewed), true).expect("renew");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("api_key")).unwrap(),
            "secret-for-api_key-v2"
        );
        // Past the ORIGINAL expiry (1_000+60_000) but inside the renewed one:
        ch.now_ms = 100_000;
        match ch.request(HostToAgent::QueryBoundReady).unwrap() {
            AgentToHost::BoundReady { ready, .. } => assert!(ready, "renewed lease must outlive the original TTL"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn revoked_grant_scrubs_and_remaining_leases_renew_ack_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("db_url", "l1"), lease("api_key", "l2")], 3).expect("bind");
        // Grant for db_url disappears -> the renewal loop revokes that lease…
        revoke_binding(&mut ch, BindingLeaseId::new("l1")).expect("revoke");
        assert!(!dir.path().join("db_url").exists(), "revoked value must be scrubbed");
        // …and the still-granted lease renews Ack-only (bound-ready is false by design).
        renew_over_channel(&mut ch, &[lease("api_key", "l2")], false).expect("ack-only renew");
        assert!(dir.path().join("api_key").exists());
        match ch.request(HostToAgent::QueryBoundReady).unwrap() {
            AgentToHost::BoundReady { ready, pending } => {
                assert!(!ready, "partially revoked session must not be bound-ready");
                assert_eq!(pending.len(), 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // A full renew (assert_bound_ready) must FAIL CLOSED in this state.
        assert!(renew_over_channel(&mut ch, &[lease("api_key", "l2")], true).is_err());
    }
}
