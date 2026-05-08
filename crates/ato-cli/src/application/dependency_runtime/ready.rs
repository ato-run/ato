//! Ready probe runtime (RFC §7.6).
//!
//! v1 implements the `tcp`, `probe`, and `postgres` variants. `http`
//! and `unix_socket` are reserved-only and rejected at lock time
//! (`capsule-core::foundation::dependency_contracts::verify_and_lock`),
//! so we do not need runtime support for them here.
//!
//! The `postgres` probe replaces a per-binary `pg_isready` dependency
//! with a native "is the server accepting connections?" check. See
//! [`postgres_accepting_connections`] for the wire-level details and
//! [`ReadyProbeKind::Postgres`] for the readiness contract.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReadyError {
    #[error("ready probe timed out after {elapsed:?}: {detail}")]
    Timeout { elapsed: Duration, detail: String },

    #[error("ready probe failed during execution: {detail}")]
    ExecutionFailed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// Ready check passed. Returns the elapsed duration since the probe
    /// loop started.
    Ready { elapsed: Duration, attempts: u32 },
}

#[derive(Debug, Clone)]
pub enum ReadyProbeKind {
    /// Open a short TCP connection to `host:port`. Success on connect.
    Tcp { host: String, port: u16 },
    /// Run a command. Success on exit code 0.
    Probe { argv: Vec<String> },
    /// Native Postgres "accepting connections" probe — the orchestration-
    /// layer replacement for spawning `pg_isready` (which is not part of
    /// the relocatable artifact we ship). Sends a minimal `StartupMessage`
    /// and treats the first backend response as the readiness verdict.
    ///
    /// **Semantics: server is accepting connections, not query-ready for
    /// the consumer's credentials.** The probe deliberately does not
    /// send a password — we are not authenticating the consumer's app,
    /// we are confirming postmaster is past startup. Authentication
    /// challenges and auth-class `ErrorResponse` (SQLSTATE class 28) are
    /// treated as `ready`; only `57P03` (`cannot_connect_now`) and
    /// transport-level failures are treated as `not ready`.
    Postgres {
        host: String,
        port: u16,
        /// Username to send in the StartupMessage. Used only to give
        /// the server a label for its own logs — the probe does not
        /// authenticate. Conventionally "postgres".
        user: String,
        /// Database name in the StartupMessage. Same caveat as `user`.
        database: String,
    },
}

/// Wait for a ready probe to succeed or time out.
///
/// `interval` is the minimum spacing between successive attempts. If the
/// caller passes `Duration::ZERO`, the probe loop spins as fast as the
/// kernel allows; production callers should pass at least 100 ms.
pub fn wait_for_ready(
    kind: &ReadyProbeKind,
    timeout: Duration,
    interval: Duration,
) -> Result<ReadyOutcome, ReadyError> {
    let started = Instant::now();
    let mut attempts: u32 = 0;
    let mut last_failure = String::from("not attempted");
    loop {
        if started.elapsed() >= timeout {
            return Err(ReadyError::Timeout {
                elapsed: started.elapsed(),
                detail: format!("last failure: {last_failure}"),
            });
        }
        attempts += 1;
        let outcome = attempt(kind);
        match outcome {
            Ok(()) => {
                return Ok(ReadyOutcome::Ready {
                    elapsed: started.elapsed(),
                    attempts,
                });
            }
            Err(err) => {
                last_failure = err;
            }
        }
        if started.elapsed() >= timeout {
            return Err(ReadyError::Timeout {
                elapsed: started.elapsed(),
                detail: format!("last failure: {last_failure}"),
            });
        }
        if !interval.is_zero() {
            std::thread::sleep(interval);
        }
    }
}

fn attempt(kind: &ReadyProbeKind) -> Result<(), String> {
    match kind {
        ReadyProbeKind::Tcp { host, port } => {
            let addr = format!("{host}:{port}");
            // 1 second connect timeout per attempt; the outer loop owns
            // the overall timeout.
            match TcpStream::connect_timeout(
                &addr
                    .parse()
                    .map_err(|err: std::net::AddrParseError| err.to_string())?,
                Duration::from_secs(1),
            ) {
                Ok(stream) => {
                    drop(stream);
                    Ok(())
                }
                Err(err) => Err(format!("tcp connect to {addr}: {err}")),
            }
        }
        ReadyProbeKind::Probe { argv } => {
            if argv.is_empty() {
                return Err("probe argv is empty".to_string());
            }
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::piped());
            let output = cmd
                .output()
                .map_err(|err| format!("spawn {} failed: {err}", argv[0]))?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!(
                    "probe {} exited {}: {}",
                    argv[0],
                    output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    stderr.trim()
                ))
            }
        }
        ReadyProbeKind::Postgres {
            host,
            port,
            user,
            database,
        } => postgres_accepting_connections(host, *port, user, database),
    }
}

// ────────────────────────────────────────────────────────────────────
// Native Postgres "accepting connections" probe
// ────────────────────────────────────────────────────────────────────

/// Per-attempt connect timeout. The outer `wait_for_ready` loop owns
/// the overall budget; this caps how long a single attempt waits for
/// the TCP connect to either succeed or fail with ECONNREFUSED.
const POSTGRES_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-attempt read timeout. After we send the StartupMessage we wait
/// at most this long for the first backend message before declaring
/// the attempt failed. Postgres on a healthy host responds in low ms;
/// 2 s is generous enough to absorb a slow CI container without making
/// the outer loop sluggish.
const POSTGRES_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Postgres protocol-version constant for the v3 protocol used since
/// 7.4. High 16 bits = major (3), low 16 bits = minor (0).
const POSTGRES_PROTOCOL_VERSION: u32 = 3 << 16;

/// Run one accepting-connections attempt. The result string is what
/// the outer retry loop records as `last_failure` on a non-ready
/// outcome — be specific so a final timeout has a useful tail.
fn postgres_accepting_connections(
    host: &str,
    port: u16,
    user: &str,
    database: &str,
) -> Result<(), String> {
    let addr = resolve_socket_addr(host, port)?;
    let mut stream = TcpStream::connect_timeout(&addr, POSTGRES_CONNECT_TIMEOUT)
        .map_err(|err| format!("connect {addr}: {err}"))?;
    stream
        .set_read_timeout(Some(POSTGRES_READ_TIMEOUT))
        .map_err(|err| format!("set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(POSTGRES_READ_TIMEOUT))
        .map_err(|err| format!("set write timeout: {err}"))?;

    let startup = build_startup_message(user, database);
    stream
        .write_all(&startup)
        .map_err(|err| format!("send StartupMessage: {err}"))?;

    let msg = read_first_backend_message(&mut stream)?;
    classify_first_message(&msg)
}

fn resolve_socket_addr(host: &str, port: u16) -> Result<std::net::SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let candidates = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("resolve {host}:{port}: {err}"))?;
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("resolve {host}:{port}: empty address list"))
}

/// Build a Postgres v3 `StartupMessage` for the probe. The payload is
/// only `user`, `database`, and the trailing NUL. We deliberately
/// omit application-time parameters (e.g. `application_name`) so the
/// message stays minimal — the goal is only to elicit the first
/// backend response.
fn build_startup_message(user: &str, database: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(64);
    push_cstring(&mut payload, "user");
    push_cstring(&mut payload, user);
    push_cstring(&mut payload, "database");
    push_cstring(&mut payload, database);
    payload.push(0); // terminator after the last pair
    let total_len = (4 + 4 + payload.len()) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(&POSTGRES_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

fn push_cstring(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

#[derive(Debug)]
enum BackendFirstMessage {
    /// `R` tag, auth-type 0.
    AuthenticationOk,
    /// `R` tag, auth-type 3.
    AuthenticationCleartextPassword,
    /// `R` tag, auth-type 5.
    AuthenticationMd5Password,
    /// `R` tag, auth-type 10.
    AuthenticationSasl,
    /// `R` tag, auth-type 11.
    AuthenticationSaslContinue,
    /// `R` tag, auth-type 12.
    AuthenticationSaslFinal,
    /// `R` tag, an auth-type Postgres has but the probe doesn't
    /// special-case (e.g. GSS / SSPI). Treated as accepting because
    /// the server reached the auth phase.
    AuthenticationOther(u32),
    /// `Z` tag — only emitted after auth in real flows, but a
    /// permissive trust-auth server can send it as the first
    /// message. Treat as ready.
    ReadyForQuery,
    /// `E` tag — `sqlstate` field comes from the `C` field; `message`
    /// from the `M` field. Both may be absent on a malformed reply.
    ErrorResponse {
        sqlstate: Option<String>,
        message: Option<String>,
    },
    /// Tag we don't expect at this stage — surfaces as a probe error
    /// so the caller can investigate. Carries the raw tag byte.
    UnexpectedTag(u8),
}

fn read_first_backend_message(stream: &mut TcpStream) -> Result<BackendFirstMessage, String> {
    let mut tag_buf = [0u8; 1];
    stream
        .read_exact(&mut tag_buf)
        .map_err(|err| format!("read message tag: {err}"))?;
    let tag = tag_buf[0];

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|err| format!("read message length: {err}"))?;
    let length = i32::from_be_bytes(len_buf);
    if length < 4 {
        return Err(format!(
            "postgres protocol violation: length {length} < 4 for tag {}",
            char::from(tag)
        ));
    }
    let body_len = (length as usize) - 4;
    // Cap the body size we will allocate. The protocol allows up to
    // i32::MAX but a healthy first response is well under 1 KB; cap
    // at 1 MiB so a hostile server cannot exhaust memory.
    if body_len > 1024 * 1024 {
        return Err(format!(
            "postgres protocol violation: implausible message length {length}"
        ));
    }
    let mut body = vec![0u8; body_len];
    if body_len > 0 {
        stream
            .read_exact(&mut body)
            .map_err(|err| format!("read message body ({body_len} bytes): {err}"))?;
    }

    match tag {
        b'R' => parse_authentication_message(&body),
        b'Z' => Ok(BackendFirstMessage::ReadyForQuery),
        b'E' => Ok(parse_error_response(&body)),
        other => Ok(BackendFirstMessage::UnexpectedTag(other)),
    }
}

fn parse_authentication_message(body: &[u8]) -> Result<BackendFirstMessage, String> {
    if body.len() < 4 {
        return Err(format!(
            "postgres protocol violation: AuthenticationXxx body too short ({} bytes)",
            body.len()
        ));
    }
    let auth_type = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    Ok(match auth_type {
        0 => BackendFirstMessage::AuthenticationOk,
        3 => BackendFirstMessage::AuthenticationCleartextPassword,
        5 => BackendFirstMessage::AuthenticationMd5Password,
        10 => BackendFirstMessage::AuthenticationSasl,
        11 => BackendFirstMessage::AuthenticationSaslContinue,
        12 => BackendFirstMessage::AuthenticationSaslFinal,
        other => BackendFirstMessage::AuthenticationOther(other),
    })
}

fn parse_error_response(body: &[u8]) -> BackendFirstMessage {
    let mut sqlstate = None;
    let mut message = None;
    let mut i = 0;
    while i < body.len() {
        let tag = body[i];
        if tag == 0 {
            break;
        }
        i += 1;
        let start = i;
        while i < body.len() && body[i] != 0 {
            i += 1;
        }
        let value = std::str::from_utf8(&body[start..i])
            .ok()
            .map(|s| s.to_string());
        if i < body.len() {
            i += 1; // consume NUL
        }
        match tag {
            b'C' => sqlstate = value,
            b'M' => message = value,
            _ => {} // ignore other fields (severity, file, …)
        }
    }
    BackendFirstMessage::ErrorResponse { sqlstate, message }
}

fn classify_first_message(msg: &BackendFirstMessage) -> Result<(), String> {
    match msg {
        BackendFirstMessage::AuthenticationOk
        | BackendFirstMessage::AuthenticationCleartextPassword
        | BackendFirstMessage::AuthenticationMd5Password
        | BackendFirstMessage::AuthenticationSasl
        | BackendFirstMessage::AuthenticationSaslContinue
        | BackendFirstMessage::AuthenticationSaslFinal
        | BackendFirstMessage::ReadyForQuery => Ok(()),
        BackendFirstMessage::AuthenticationOther(t) => {
            // The server reached auth — accepting connections. Record
            // the type so an operator can diagnose unusual configs.
            tracing::debug!(auth_type = *t, "postgres probe: unusual auth type, treating as ready");
            Ok(())
        }
        BackendFirstMessage::ErrorResponse { sqlstate, message } => {
            let detail = message.as_deref().unwrap_or("(no message)");
            match sqlstate.as_deref() {
                // Class 28 — invalid_authorization_specification (28000),
                // invalid_password (28P01), and similar. Auth failure is
                // a strong signal that the server is up and accepting
                // connections — the failure mode we are filtering for
                // is "consumer auth correctness", not "server up".
                Some(code) if is_auth_class(code) => Ok(()),
                // 57P03 cannot_connect_now — the server's own
                // "I'm not ready, retry me" code. Don't promote to ready.
                Some("57P03") => Err(format!(
                    "postgres still starting (sqlstate=57P03): {detail}"
                )),
                Some(code) => Err(format!(
                    "postgres error response (sqlstate={code}): {detail}"
                )),
                None => Err(format!(
                    "postgres error response without sqlstate: {detail}"
                )),
            }
        }
        BackendFirstMessage::UnexpectedTag(tag) => Err(format!(
            "postgres protocol violation: unexpected first-message tag {tag} ({})",
            char::from(*tag)
        )),
    }
}

fn is_auth_class(sqlstate: &str) -> bool {
    sqlstate.len() == 5 && sqlstate.starts_with("28")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn tcp_probe_succeeds_when_listener_is_up() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        // Hand the listener off to a thread that accepts forever — this
        // keeps the port live for the duration of the probe.
        let listener = Arc::new(listener);
        let l = listener.clone();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let kind = ReadyProbeKind::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        let outcome = wait_for_ready(&kind, Duration::from_secs(2), Duration::from_millis(20))
            .expect("ready");
        match outcome {
            ReadyOutcome::Ready { attempts, .. } => {
                assert!(attempts >= 1);
            }
        }
    }

    #[test]
    fn tcp_probe_times_out_when_nothing_listens() {
        // Pick a port that nothing is listening on. We bind & immediately
        // drop a listener to grab a port, then probe that port (which is
        // now closed in TIME_WAIT-ish state).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let kind = ReadyProbeKind::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        let err = wait_for_ready(&kind, Duration::from_millis(300), Duration::from_millis(50))
            .expect_err("must time out");
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn probe_command_succeeds_on_exit_zero() {
        let kind = ReadyProbeKind::Probe {
            argv: vec!["/usr/bin/true".to_string()],
        };
        wait_for_ready(&kind, Duration::from_secs(1), Duration::from_millis(20)).expect("ready");
    }

    #[test]
    fn probe_command_times_out_on_nonzero_exit() {
        let kind = ReadyProbeKind::Probe {
            argv: vec!["/usr/bin/false".to_string()],
        };
        let err = wait_for_ready(&kind, Duration::from_millis(300), Duration::from_millis(50))
            .expect_err("must time out");
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn probe_with_empty_argv_returns_failure_then_timeout() {
        let kind = ReadyProbeKind::Probe { argv: vec![] };
        let err = wait_for_ready(&kind, Duration::from_millis(100), Duration::from_millis(20))
            .expect_err("must time out");
        // The retry loop wraps the empty-argv error as a Timeout because
        // it never succeeds within the deadline.
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }

    // ────────────────────────────────────────────────────────────
    // Postgres "accepting connections" probe
    // ────────────────────────────────────────────────────────────
    //
    // These tests stand up a tiny synchronous fake server that accepts
    // exactly one connection, reads the StartupMessage, and writes a
    // canned response. The probe under test must classify the response
    // per the contract documented on `ReadyProbeKind::Postgres`.

    use std::io::Read as _;
    use std::io::Write as _;

    /// Spawn a fake server that accepts connections in a loop and
    /// replies to each StartupMessage with `response`. Looping is
    /// important because the outer `wait_for_ready` retry policy can
    /// run multiple attempts within its budget — if we only accepted
    /// one, the second attempt would see ECONNREFUSED and overwrite
    /// the `last_failure` we are trying to assert against.
    fn spawn_fake_postgres(response: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || loop {
            let (mut stream, _) = match listener.accept() {
                Ok(p) => p,
                Err(_) => return,
            };
            let response = response.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut len_buf = [0u8; 4];
                if stream.read_exact(&mut len_buf).is_err() {
                    return;
                }
                let length = i32::from_be_bytes(len_buf);
                if length >= 4 {
                    let mut rest = vec![0u8; (length - 4) as usize];
                    let _ = stream.read_exact(&mut rest);
                }
                let _ = stream.write_all(&response);
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(50));
            });
        });
        port
    }

    fn auth_message(auth_type: u32, extra_payload: &[u8]) -> Vec<u8> {
        let mut msg = vec![b'R'];
        let length = (4 + 4 + extra_payload.len()) as u32;
        msg.extend_from_slice(&length.to_be_bytes());
        msg.extend_from_slice(&auth_type.to_be_bytes());
        msg.extend_from_slice(extra_payload);
        msg
    }

    fn error_response(sqlstate: &str, message: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(b'C');
        payload.extend_from_slice(sqlstate.as_bytes());
        payload.push(0);
        payload.push(b'M');
        payload.extend_from_slice(message.as_bytes());
        payload.push(0);
        payload.push(0); // terminator
        let length = (4 + payload.len()) as u32;
        let mut msg = vec![b'E'];
        msg.extend_from_slice(&length.to_be_bytes());
        msg.extend_from_slice(&payload);
        msg
    }

    fn pg_probe(port: u16) -> ReadyProbeKind {
        ReadyProbeKind::Postgres {
            host: "127.0.0.1".to_string(),
            port,
            user: "postgres".to_string(),
            database: "postgres".to_string(),
        }
    }

    #[test]
    fn postgres_probe_not_ready_when_connection_refused() {
        // Bind+drop to grab a port nothing is listening on.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let err = wait_for_ready(
            &pg_probe(port),
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .expect_err("must time out");
        match err {
            ReadyError::Timeout { detail, .. } => {
                assert!(
                    detail.contains("connect") || detail.to_lowercase().contains("refused"),
                    "expected connect/refused in detail, got: {detail}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn postgres_probe_not_ready_when_server_never_responds() {
        // A listener that accepts but never writes — exercises the
        // per-attempt read timeout. Outer budget is short so the loop
        // runs only one or two attempts.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            // Hold the connection open without writing for longer than
            // the outer timeout. Drop closes silently.
            if let Ok((stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(5));
                drop(stream);
            }
        });
        let err = wait_for_ready(
            &pg_probe(port),
            // POSTGRES_READ_TIMEOUT is 2s; cap the outer at 2.5s.
            Duration::from_millis(2500),
            Duration::from_millis(100),
        )
        .expect_err("must time out");
        match err {
            ReadyError::Timeout { detail, .. } => {
                assert!(
                    detail.to_lowercase().contains("read")
                        || detail.to_lowercase().contains("timed out")
                        || detail.to_lowercase().contains("timeout"),
                    "expected read/timeout in detail, got: {detail}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn postgres_probe_ready_on_authentication_cleartext_password() {
        let port = spawn_fake_postgres(auth_message(3, &[]));
        let outcome = wait_for_ready(
            &pg_probe(port),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .expect("ready");
        match outcome {
            ReadyOutcome::Ready { attempts, .. } => {
                assert!(attempts >= 1);
            }
        }
    }

    #[test]
    fn postgres_probe_ready_on_authentication_sasl() {
        // AuthenticationSASL carries a NUL-terminated mechanism list
        // ending in an extra NUL. Minimal valid payload: just "\0".
        let mech_list = b"SCRAM-SHA-256\0\0";
        let port = spawn_fake_postgres(auth_message(10, mech_list));
        wait_for_ready(
            &pg_probe(port),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .expect("ready");
    }

    #[test]
    fn postgres_probe_ready_on_authentication_ok() {
        let port = spawn_fake_postgres(auth_message(0, &[]));
        wait_for_ready(
            &pg_probe(port),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .expect("ready");
    }

    #[test]
    fn postgres_probe_ready_on_invalid_password_28p01() {
        // Auth failure means the server reached the auth phase and is
        // accepting connections. The consumer's credentials being
        // wrong is the consumer's failure, not the provider's.
        let port = spawn_fake_postgres(error_response("28P01", "password authentication failed"));
        wait_for_ready(
            &pg_probe(port),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .expect("ready");
    }

    #[test]
    fn postgres_probe_not_ready_on_cannot_connect_now_57p03() {
        // 57P03 is the server's own retry signal — we must not promote
        // it to ready or we will declare a still-starting server up.
        let port = spawn_fake_postgres(error_response(
            "57P03",
            "the database system is starting up",
        ));
        let err = wait_for_ready(
            &pg_probe(port),
            // Single-attempt budget — we only stand up one fake server,
            // so the loop cannot retry past the first attempt anyway.
            Duration::from_millis(800),
            Duration::from_millis(50),
        )
        .expect_err("must time out");
        match err {
            ReadyError::Timeout { detail, .. } => {
                assert!(
                    detail.contains("57P03") || detail.contains("starting"),
                    "expected 57P03 / starting in detail, got: {detail}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn postgres_probe_typed_error_on_malformed_response() {
        // Tag 'X' (any tag) with length 2 — protocol violation
        // (length must be at least 4 to cover the length field
        // itself). The probe must surface this as a recognizable
        // protocol-violation message in last_failure.
        let bogus = vec![b'X', 0, 0, 0, 2];
        let port = spawn_fake_postgres(bogus);
        let err = wait_for_ready(
            &pg_probe(port),
            Duration::from_millis(800),
            Duration::from_millis(50),
        )
        .expect_err("must time out");
        match err {
            ReadyError::Timeout { detail, .. } => {
                assert!(
                    detail.contains("protocol violation") || detail.contains("length"),
                    "expected protocol-violation detail, got: {detail}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn postgres_probe_ready_on_ready_for_query_first() {
        // 'Z' tag with length=5 and status byte 'I' (idle). Permissive
        // trust-auth servers can emit ReadyForQuery as the first message.
        let mut msg = vec![b'Z'];
        msg.extend_from_slice(&5u32.to_be_bytes());
        msg.push(b'I');
        let port = spawn_fake_postgres(msg);
        wait_for_ready(
            &pg_probe(port),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .expect("ready");
    }

    #[test]
    fn build_startup_message_round_trips_user_and_database() {
        let bytes = build_startup_message("postgres", "demo");
        // Layout: [len:4][protocol:4][cstring*..][NUL]
        let length = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(length as usize, bytes.len());
        let protocol = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(protocol, POSTGRES_PROTOCOL_VERSION);
        let payload = &bytes[8..];
        assert!(payload.windows(5).any(|w| w == b"user\0"));
        assert!(payload.windows(9).any(|w| w == b"postgres\0"));
        assert!(payload.windows(9).any(|w| w == b"database\0"));
        assert!(payload.windows(5).any(|w| w == b"demo\0"));
        assert_eq!(payload.last(), Some(&0u8), "must terminate with NUL");
    }

    #[test]
    fn is_auth_class_accepts_28000_and_28p01() {
        assert!(is_auth_class("28000"));
        assert!(is_auth_class("28P01"));
        // Same prefix but wrong length:
        assert!(!is_auth_class("28"));
        // Different class:
        assert!(!is_auth_class("57P03"));
    }
}
