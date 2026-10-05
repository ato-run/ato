//! A fixed external broker keeps the owner's Session connection outside the
//! Native agent sandbox. The relay transports MCP only; Session authority owns
//! proposals, deduplication, budgets, deadlines and Runtime state transitions.
use std::{
    fs::File,
    io::{BufRead, Cursor, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, ensure};
use ato_formation_worker::runtime_network::proposal::reasoning::SessionAgent;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{FormationSessionMcpServer, MAX_REQUEST_BYTES};

#[cfg(unix)]
mod unix;

const DESCRIPTOR_SCHEMA: &str = "ato.formation-session-relay/2";
const REQUEST_SCHEMA: &str = "ato.formation-session-relay-request/1";
const DESCRIPTOR_CAP: usize = 8 * 1024;
const REQUEST_CAP: usize = MAX_REQUEST_BYTES + DESCRIPTOR_CAP;
const RESPONSE_CAP: usize = 512 * 1024;
const STREAM_TIMEOUT: Duration = Duration::from_secs(2);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    search_id: String,
    configuration_ref: String,
    agent: Option<SessionAgent>,
    expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    read_only: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

// A reporting capability has its own short lifetime; it never changes the
// saved Search deadline or authorizes another submission/execution.
fn relay_expiry(view: &Value, expiry: Option<u64>, read_only: bool, now: u64) -> Result<u64> {
    let deadline = view["deadline_ms"]
        .as_u64()
        .filter(|deadline| *deadline > 0)
        .ok_or_else(|| anyhow!("Session deadline unavailable"))?;
    let expires_at_ms = expiry.unwrap_or(deadline);
    ensure!(now < expires_at_ms, "relay expiry rejected");
    if read_only {
        ensure!(
            expiry.is_some()
                && expires_at_ms.saturating_sub(now) <= 120_000
                && matches!(
                    view["progress"]["status"].as_str(),
                    Some("pending" | "running" | "unknown")
                )
                && view["progress"]["unresolved_attempts"]
                    .as_u64()
                    .is_some_and(|count| count > 0)
                && view["progress"]["attempts"]
                    .as_array()
                    .is_some_and(|attempts| attempts
                        .iter()
                        .any(|attempt| attempt["status"] == "unknown"))
                && view.get("input") == Some(&Value::Null)
                && view["connected"].as_bool().is_some()
                && matches!(
                    view["next_operation"].as_str(),
                    Some(
                        "owner_reconcile"
                            | "owner_reconcile_or_assess"
                            | "owner_reconnect_or_assess"
                    )
                ),
            "reconciliation relay admission rejected"
        );
    } else {
        ensure!(expires_at_ms <= deadline, "relay expiry rejected");
    }
    Ok(expires_at_ms)
}

fn scoped_handle(
    server: &mut FormationSessionMcpServer,
    request: &Value,
    read_only: bool,
) -> Option<Value> {
    if !read_only {
        return server.handle(request);
    }
    let id = request.get("id")?.clone();
    let method = request["method"].as_str();
    if method == Some("tools/call")
        && !matches!(request["params"]["name"].as_str(), Some("status" | "next"))
    {
        return Some(crate::mcp_stdio::rpc_error(
            id,
            -32601,
            "reconciliation relay is read-only",
        ));
    }
    let mut response = server.handle(request)?;
    if method == Some("tools/list") {
        if let Some(tools) = response["result"]["tools"].as_array_mut() {
            tools.retain(|tool| matches!(tool["name"].as_str(), Some("status" | "next")));
        }
    } else if method == Some("initialize") {
        response["result"]["instructions"] = json!(
            "Read status and next to report saved UNKNOWN evidence only. This relay cannot submit, cancel, or start work. Preserve the original Search deadline and consumption; owner authority resolves the attempt."
        );
    } else if method == Some("tools/call")
        && !response["result"]["structuredContent"]["input"].is_null()
    {
        return Some(crate::mcp_stdio::rpc_error(
            id,
            -32601,
            "reconciliation relay input rejected",
        ));
    }
    Some(response)
}

// No Debug: even the limited Producer capability stays out of diagnostics.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    schema: String,
    address: SocketAddr,
    #[serde(skip)]
    capability: String,
    binding: Binding,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: String,
    nonce: String,
    proof: String,
    binding: Binding,
    request: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    ok: bool,
    response: Option<Value>,
    proof: Option<String>,
}

struct Broker {
    descriptor: Descriptor,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Broker {
    #[cfg(test)]
    fn start(
        server: FormationSessionMcpServer,
        descriptor_file: &Path,
        expiry: Option<u64>,
    ) -> Result<Self> {
        Self::start_with_capability(
            server,
            descriptor_file,
            &descriptor_file.with_extension("capability"),
            expiry,
        )
    }

    #[cfg(test)]
    fn start_with_capability(
        server: FormationSessionMcpServer,
        descriptor_file: &Path,
        capability_file: &Path,
        expiry: Option<u64>,
    ) -> Result<Self> {
        Self::start_with_scope(server, descriptor_file, capability_file, expiry, false)
    }

    fn start_with_scope(
        mut server: FormationSessionMcpServer,
        descriptor_file: &Path,
        capability_file: &Path,
        expiry: Option<u64>,
        read_only: bool,
    ) -> Result<Self> {
        // Obtain the original deadline from the same saved Session authority.
        // Publishing/reconnecting a broker never initializes a Search budget.
        let status = server
            .handle(&json!({"jsonrpc":"2.0","id":0,"method":"tools/call",
                           "params":{"name":"status","arguments":{}}}))
            .ok_or_else(|| anyhow!("Session status unavailable"))?;
        let view = &status["result"]["structuredContent"];
        ensure!(
            status["result"]["isError"] == false
                && view["search_id"] == server.binding.search_id
                && view["configuration_ref"] == server.binding.configuration_ref,
            "Session status rejected"
        );
        let expires_at_ms = relay_expiry(view, expiry, read_only, now_ms()?)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let descriptor = Descriptor {
            schema: DESCRIPTOR_SCHEMA.into(),
            address: listener.local_addr()?,
            capability: random_hex()?,
            binding: Binding {
                search_id: server.binding.search_id.clone(),
                configuration_ref: server.binding.configuration_ref.clone(),
                agent: server.binding.agent.clone(),
                expires_at_ms,
                read_only,
            },
        };
        validate(&descriptor)?;
        publish_capability(capability_file, &descriptor, &descriptor.capability)?;
        publish(descriptor_file, &descriptor)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let identity = descriptor.clone();
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed)
                && now_ms().is_ok_and(|now| now < identity.binding.expires_at_ms)
            {
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        if peer.ip().is_loopback() && stream.set_nonblocking(false).is_ok() {
                            handle_stream(
                                &mut stream,
                                &mut server,
                                &identity.binding,
                                &identity.capability,
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
            stopped.store(true, Ordering::Relaxed);
        });
        Ok(Self {
            descriptor,
            stop,
            thread: Some(thread),
        })
    }
}

// Both transports use the same authenticated frames and Session handler.
fn handle_stream(
    stream: &mut impl RelayStream,
    server: &mut FormationSessionMcpServer,
    binding: &Binding,
    capability: &str,
) {
    let reply = (|| -> Result<Reply> {
        stream.set_read_timeout(Some(STREAM_TIMEOUT))?;
        stream.set_write_timeout(Some(STREAM_TIMEOUT))?;
        let bytes = read_frame(stream, REQUEST_CAP)?;
        let request: Request = serde_json::from_slice(&bytes)?;
        ensure!(
            request.schema == REQUEST_SCHEMA
                && valid_hex(&request.nonce)
                && request.binding == *binding
                && now_ms()? < binding.expires_at_ms,
            "relay request rejected"
        );
        verify_proof(capability, &request_message(&request)?, &request.proof)?;
        // Use the existing MCP framing/handler unchanged, including
        // JSON-RPC validation and typed output_json preservation.
        let mut frame = serde_json::to_vec(&request.request)?;
        ensure!(frame.len() < MAX_REQUEST_BYTES, "relay frame rejected");
        frame.push(b'\n');
        let mut output = Vec::new();
        crate::mcp_stdio::run_stdio(
            Cursor::new(frame),
            &mut output,
            Some(MAX_REQUEST_BYTES),
            |request| scoped_handle(server, request, binding.read_only),
        )?;
        ensure!(output.len() < RESPONSE_CAP, "relay response rejected");
        let response = if output.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&output)?)
        };
        let mut reply = Reply {
            ok: true,
            response,
            proof: None,
        };
        reply.proof = Some(proof(capability, &reply_message(&request, &reply)?)?);
        Ok(reply)
    })()
    .unwrap_or(Reply {
        ok: false,
        response: None,
        proof: None,
    });
    // Never expose transport bytes, private paths or parser details.
    if let Ok(bytes) = serde_json::to_vec(&reply)
        && bytes.len() < RESPONSE_CAP
    {
        let _ = write_frame(stream, &bytes);
    }
}

trait RelayStream: Read + Write {
    fn read_timeout(&self) -> std::io::Result<Option<Duration>>;
    fn write_timeout(&self) -> std::io::Result<Option<Duration>>;
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
    fn wait_readable(&self, timeout: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
    fn wait_writable(&self, timeout: Duration) -> std::io::Result<()> {
        self.set_write_timeout(Some(timeout))
    }
}

impl RelayStream for TcpStream {
    fn read_timeout(&self) -> std::io::Result<Option<Duration>> {
        TcpStream::read_timeout(self)
    }
    fn write_timeout(&self) -> std::io::Result<Option<Duration>> {
        TcpStream::write_timeout(self)
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Keep the expired descriptor as evidence. It is never overwritten or
        // reused; the owner must explicitly publish another fresh descriptor.
    }
}

/// Owner-only publication. This is a broker for an existing scoped Session,
/// not Search creation, authentication setup, inference, or Runtime execution.
pub fn serve_owner(
    connection_file: &Path,
    descriptor_file: &Path,
    expiry: Option<u64>,
) -> Result<()> {
    serve_owner_with_capability(
        connection_file,
        descriptor_file,
        &descriptor_file.with_extension("capability"),
        expiry,
    )
}

pub fn serve_owner_with_capability(
    connection_file: &Path,
    descriptor_file: &Path,
    capability_file: &Path,
    expiry: Option<u64>,
) -> Result<()> {
    serve_owner_with_scope(
        connection_file,
        descriptor_file,
        capability_file,
        expiry,
        false,
    )
}

pub fn serve_owner_with_scope(
    connection_file: &Path,
    descriptor_file: &Path,
    capability_file: &Path,
    expiry: Option<u64>,
    read_only: bool,
) -> Result<()> {
    ensure!(
        descriptor_file != capability_file,
        "private capability destination rejected"
    );
    let broker = Broker::start_with_scope(
        FormationSessionMcpServer::connect(connection_file)?,
        descriptor_file,
        capability_file,
        expiry,
        read_only,
    )?;
    #[cfg(unix)]
    {
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&broker.stop))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&broker.stop))?;
    }
    while !broker.stop.load(Ordering::Relaxed)
        && now_ms()? < broker.descriptor.binding.expires_at_ms
    {
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(unix)]
pub use unix::serve_owner as serve_owner_unix;
#[cfg(unix)]
pub use unix::serve_owner_with_capability as serve_owner_unix_with_capability;
#[cfg(unix)]
pub use unix::serve_owner_with_scope as serve_owner_unix_with_scope;

/// Model-facing stdio process reads only its Producer descriptor. It never
/// opens the owner's connection or credentials and never retries transport.
pub fn run_relay_stdio(
    descriptor_file: &Path,
    input: impl BufRead,
    output: impl Write,
) -> Result<()> {
    run_relay_stdio_with_capability(
        descriptor_file,
        &descriptor_file.with_extension("capability"),
        input,
        output,
    )
}

pub fn run_relay_stdio_with_capability(
    descriptor_file: &Path,
    capability_file: &Path,
    input: impl BufRead,
    output: impl Write,
) -> Result<()> {
    let bytes = read_descriptor_bytes(descriptor_file)?;
    #[cfg(unix)]
    if serde_json::from_slice::<Value>(&bytes)?["schema"] == unix::DESCRIPTOR_SCHEMA {
        return unix::run_stdio(&bytes, capability_file, input, output);
    }
    let mut descriptor: Descriptor = serde_json::from_slice(&bytes)?;
    descriptor.capability = read_capability(capability_file, &descriptor)?;
    validate(&descriptor)?;
    crate::mcp_stdio::run_stdio(input, output, Some(MAX_REQUEST_BYTES), |request| {
        forward(&descriptor, request).unwrap_or_else(|_| {
            Some(crate::mcp_stdio::rpc_error(
                request.get("id").cloned().unwrap_or(Value::Null),
                -32000,
                "relay request rejected; inspect status before continuing",
            ))
        })
    })
}

fn forward(descriptor: &Descriptor, request: &Value) -> Result<Option<Value>> {
    validate(descriptor)?;
    ensure!(
        now_ms()? < descriptor.binding.expires_at_ms,
        "relay expired"
    );
    let mut request = Request {
        schema: REQUEST_SCHEMA.into(),
        nonce: random_hex()?,
        proof: String::new(),
        binding: descriptor.binding.clone(),
        request: request.clone(),
    };
    request.proof = proof(&descriptor.capability, &request_message(&request)?)?;
    let bytes = serde_json::to_vec(&request)?;
    ensure!(bytes.len() < REQUEST_CAP, "relay request bounds");
    let mut stream = TcpStream::connect_timeout(&descriptor.address, STREAM_TIMEOUT)?;
    stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
    stream.set_write_timeout(Some(STREAM_TIMEOUT))?;
    write_frame(&mut stream, &bytes)?;
    let reply: Reply = serde_json::from_slice(&read_frame(&mut stream, RESPONSE_CAP)?)?;
    ensure!(reply.ok, "relay request rejected");
    verify_proof(
        &descriptor.capability,
        &reply_message(&request, &reply)?,
        reply.proof.as_deref().unwrap_or(""),
    )?;
    Ok(reply.response)
}

fn validate(descriptor: &Descriptor) -> Result<()> {
    ensure!(
        descriptor.schema == DESCRIPTOR_SCHEMA
            && descriptor.address.ip().is_loopback()
            && descriptor.address.port() != 0
            && valid_hex(&descriptor.capability)
            && !descriptor.binding.search_id.is_empty()
            && descriptor.binding.search_id.len() <= 160
            && !descriptor.binding.configuration_ref.is_empty()
            && descriptor.binding.configuration_ref.len() <= 128
            && descriptor.binding.expires_at_ms != 0,
        "relay descriptor rejected"
    );
    Ok(())
}

fn read_descriptor_bytes(path: &Path) -> Result<Vec<u8>> {
    let expected = std::fs::symlink_metadata(path)?;
    ensure!(
        expected.file_type().is_file() && expected.len() <= DESCRIPTOR_CAP as u64,
        "relay descriptor file rejected"
    );
    let file = File::open(path)?;
    let actual = file.metadata()?;
    ensure!(
        actual.file_type().is_file(),
        "relay descriptor file rejected"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Reject a replacement/symlink race before reading any file contents.
        ensure!(
            expected.dev() == actual.dev() && expected.ino() == actual.ino(),
            "relay descriptor file changed"
        );
    }
    let mut bytes = Vec::new();
    file.take(DESCRIPTOR_CAP as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= DESCRIPTOR_CAP, "relay descriptor bounds");
    Ok(bytes)
}

#[cfg(test)]
fn read_descriptor(path: &Path) -> Result<Descriptor> {
    let mut descriptor: Descriptor = serde_json::from_slice(&read_descriptor_bytes(path)?)?;
    descriptor.capability = read_capability(&path.with_extension("capability"), &descriptor)?;
    validate(&descriptor)?;
    Ok(descriptor)
}

fn publish_capability(path: &Path, descriptor: &impl Serialize, capability: &str) -> Result<()> {
    ensure!(valid_hex(capability), "private relay capability rejected");
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&Sha256::digest(serde_json::to_vec(descriptor)?))?;
    staged.write_all(capability.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(path)
        .map_err(|_| anyhow!("private relay capability publication rejected"))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn read_capability(path: &Path, descriptor: &impl Serialize) -> Result<String> {
    let bytes = read_descriptor_bytes(path)?;
    ensure!(
        bytes.len() == 96 && bytes[..32] == Sha256::digest(serde_json::to_vec(descriptor)?)[..],
        "private relay capability binding rejected"
    );
    let capability = String::from_utf8(bytes[32..].to_vec())
        .map_err(|_| anyhow!("private relay capability rejected"))?;
    ensure!(valid_hex(&capability), "private relay capability rejected");
    Ok(capability)
}

fn publish(path: &Path, descriptor: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let bytes = serde_json::to_vec(descriptor)?;
    ensure!(bytes.len() <= DESCRIPTOR_CAP, "relay descriptor bounds");
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn read_frame(stream: &mut impl RelayStream, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let deadline = Instant::now()
        + stream
            .read_timeout()
            .context("relay timeout query")?
            .unwrap_or(STREAM_TIMEOUT);
    let mut chunk = [0; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "relay frame timeout");
        stream
            .wait_readable(socket_timeout(remaining)?)
            .context("relay remaining read timeout")?;
        let available = chunk.len().min(cap.saturating_sub(bytes.len()) + 1);
        let count = stream
            .read(&mut chunk[..available])
            .context("relay socket read")?;
        ensure!(count != 0, "relay frame incomplete");
        ensure!(Instant::now() <= deadline, "relay frame timeout");
        let end = chunk[..count].iter().position(|byte| *byte == b'\n');
        bytes.extend_from_slice(&chunk[..end.map_or(count, |index| index + 1)]);
        ensure!(bytes.len() <= cap, "relay frame bounds");
        if end.is_some() {
            return Ok(bytes);
        }
    }
}

fn write_frame(stream: &mut impl RelayStream, bytes: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(bytes.len() + 1);
    frame.extend_from_slice(bytes);
    frame.push(b'\n');
    let deadline = Instant::now() + stream.write_timeout()?.unwrap_or(STREAM_TIMEOUT);
    let mut written = 0;
    while written < frame.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "relay write timeout");
        stream.wait_writable(socket_timeout(remaining)?)?;
        let count = stream.write(&frame[written..])?;
        ensure!(count != 0, "relay write incomplete");
        written += count;
        ensure!(Instant::now() <= deadline, "relay write timeout");
    }
    Ok(())
}

// Keep timeout values at microsecond precision and retain the absolute frame
// deadline, including after each completed read/write.
fn socket_timeout(remaining: Duration) -> Result<Duration> {
    Ok(Duration::from_micros(
        u64::try_from(remaining.as_micros())?.max(1),
    ))
}

fn valid_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn random_hex() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow!("relay entropy unavailable"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn key(capability: &str) -> Result<[u8; 32]> {
    ensure!(valid_hex(capability), "relay capability rejected");
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&capability[index * 2..index * 2 + 2], 16)?;
    }
    Ok(bytes)
}

fn proof(capability: &str, message: &[u8]) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&key(capability)?)?;
    mac.update(message);
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn verify_proof(capability: &str, message: &[u8], supplied: &str) -> Result<()> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&key(capability)?)?;
    mac.update(message);
    // hmac's verification is constant time; never expose the supplied MAC.
    mac.verify_slice(&key(supplied)?)
        .map_err(|_| anyhow!("relay proof rejected"))
}

fn request_message(request: &Request) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&(
        REQUEST_SCHEMA,
        &request.nonce,
        &request.binding,
        &request.request,
    ))?)
}

fn reply_message(request: &Request, reply: &Reply) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&(
        "ato.formation-session-relay-reply/1",
        &request.nonce,
        &request.proof,
        reply.ok,
        &reply.response,
    ))?)
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formation_session_mcp::tests::fixture;

    const OUTPUT: &str = r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"no_progress"}]}"#;

    fn call(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})
    }

    fn submit(digest: &str, output_json: &str) -> Value {
        call(
            "submit",
            json!({"exchange_id":"search_test_r1_s1","input_sha256":digest,"output_json":output_json}),
        )
    }

    fn rpc(descriptor: &Descriptor, request: Value) -> Result<Value> {
        forward(descriptor, &request)?.ok_or_else(|| anyhow!("fixture response absent"))
    }

    fn wire_request(descriptor: &Descriptor, request: Value) -> Result<Request> {
        let mut result = Request {
            schema: REQUEST_SCHEMA.into(),
            nonce: random_hex()?,
            proof: String::new(),
            binding: descriptor.binding.clone(),
            request,
        };
        result.proof = proof(&descriptor.capability, &request_message(&result)?)?;
        Ok(result)
    }

    fn raw(descriptor: &Descriptor, request: &Value) -> Result<Reply> {
        let mut stream = TcpStream::connect(descriptor.address)?;
        stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
        stream.write_all(&serde_json::to_vec(request)?)?;
        stream.write_all(b"\n")?;
        Ok(serde_json::from_slice(&read_frame(
            &mut stream,
            RESPONSE_CAP,
        )?)?)
    }

    #[test]
    fn expired_unknown_can_report_but_cannot_submit_or_change_signed_scope() -> Result<()> {
        let original_deadline = now_ms()?.saturating_sub(1);
        let (root, bridge, server, digest) =
            crate::formation_session_mcp::tests::fixture_at(original_deadline)?;
        bridge.update(
            &json!({"status":"unknown","unresolved_attempts":1,"rounds_consumed":1,
            "attempts":[{"attempt_id":"attempt-original","status":"unknown"}],
            "search_budget":{"attempts":{"max":1,"used":1,"reserved":0}}}),
        )?;
        let expiry = now_ms()? + 30_000;
        let path = root.path().join("reconciliation.json");
        let broker = Broker::start_with_scope(
            server,
            &path,
            &path.with_extension("capability"),
            Some(expiry),
            true,
        )?;
        let descriptor = &broker.descriptor;
        assert!(descriptor.binding.read_only);
        let inventory = rpc(
            descriptor,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )?;
        let tools = inventory["result"]["tools"]
            .as_array()
            .ok_or_else(|| anyhow!("tools missing"))?;
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            [Some("status"), Some("next")]
        );
        let before = rpc(descriptor, call("status", json!({})))?;
        let next = rpc(descriptor, call("next", json!({})))?;
        assert!(next["result"]["structuredContent"]["input"].is_null());
        assert_eq!(
            next["result"]["structuredContent"]["deadline_ms"],
            original_deadline
        );
        for name in ["submit", "cancel"] {
            let denied = rpc(
                descriptor,
                call(
                    name,
                    if name == "submit" {
                        json!({"exchange_id":"search_test_r1_s1","input_sha256":digest,"output_json":OUTPUT})
                    } else {
                        json!({})
                    },
                ),
            )?;
            assert_eq!(denied["error"]["code"], -32601);
        }
        let mut forged = wire_request(descriptor, call("next", json!({})))?;
        forged.binding.read_only = false;
        assert!(!raw(descriptor, &serde_json::to_value(forged)?)?.ok);
        let after = rpc(descriptor, call("status", json!({})))?;
        for key in [
            "deadline_ms",
            "exchanges_used",
            "exchanges_remaining",
            "progress",
        ] {
            assert_eq!(
                before["result"]["structuredContent"][key],
                after["result"]["structuredContent"][key]
            );
        }
        assert!(
            !root
                .path()
                .join("reasoning/r001_s001.response.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn reporting_expiry_fails_closed_without_saved_unknown_or_bounded_owner_window() -> Result<()> {
        let view = json!({"deadline_ms":1,"connected":false,"input":null,"next_operation":"owner_reconnect_or_assess",
            "progress":{"status":"unknown","unresolved_attempts":1,"attempts":[{"status":"unknown"}]}});
        assert_eq!(relay_expiry(&view, Some(121_000), true, 1_000)?, 121_000);
        assert!(relay_expiry(&view, Some(121_001), true, 1_000).is_err());
        assert!(relay_expiry(&view, Some(1_000), true, 1_000).is_err());
        assert!(relay_expiry(&view, None, true, 1_000).is_err());
        assert!(relay_expiry(&view, Some(2_000), false, 1_000).is_err());
        for change in [
            json!({"status":"unknown","unresolved_attempts":0,"attempts":[{"status":"unknown"}]}),
            json!({"status":"running","unresolved_attempts":1,"attempts":[{"status":"claimed"}]}),
            json!({"status":"unsatisfied","unresolved_attempts":1,"attempts":[{"status":"unknown"}]}),
        ] {
            let mut changed = view.clone();
            changed["progress"] = change;
            assert!(relay_expiry(&changed, Some(2_000), true, 1_000).is_err());
        }
        for (field, value) in [
            ("input", json!({"instructions":"new inference"})),
            ("connected", Value::Null),
            ("next_operation", json!("submit")),
            ("deadline_ms", json!(0)),
        ] {
            let mut changed = view.clone();
            changed[field] = value;
            assert!(relay_expiry(&changed, Some(2_000), true, 1_000).is_err());
        }
        Ok(())
    }

    #[test]
    fn publication_exposes_only_frozen_scope_and_independent_producer_capability() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let owner_token =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9; 32]);
        let owner_path = server.connection_file.to_string_lossy().to_string();
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let descriptor = read_descriptor(&path)?;
        assert!(descriptor.address.ip().is_loopback());
        assert!(valid_hex(&descriptor.capability));
        let published = std::fs::read_to_string(&path)?;
        for private in [&owner_token, &owner_path] {
            assert!(!published.contains(private));
        }
        assert!(!published.contains(&descriptor.capability));
        assert!(
            serde_json::from_str::<Value>(&published)?
                .get("capability")
                .is_none()
        );
        for forbidden in [
            "access_token",
            "connection_file",
            "ticket",
            "private_grant",
            "ATO_API_TOKEN",
        ] {
            assert!(!published.contains(forbidden));
        }
        assert!(descriptor.binding == broker.descriptor.binding);
        let status = rpc(&descriptor, call("status", json!({})))?;
        assert!(status["result"]["isError"] == false);
        assert!(
            status["result"]["structuredContent"]["deadline_ms"]
                == descriptor.binding.expires_at_ms
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(std::fs::metadata(&path)?.permissions().mode() & 0o077 == 0);
        }
        Ok(())
    }

    #[test]
    fn public_descriptor_alone_and_mismatched_private_component_cannot_authorize() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let public = root.path().join("public");
        let private = root.path().join("private");
        std::fs::create_dir(&public)?;
        std::fs::create_dir(&private)?;
        let path = public.join("relay.json");
        let capability = private.join("relay.capability");
        let broker = Broker::start_with_capability(server, &path, &capability, None)?;
        assert!(read_descriptor(&path).is_err());
        let mut descriptor: Descriptor = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert!(descriptor.capability.is_empty());
        assert!(forward(&descriptor, &call("status", json!({}))).is_err());
        descriptor.capability = read_capability(&capability, &descriptor)?;
        let status = rpc(&descriptor, call("status", json!({})))?;
        assert_eq!(
            status["result"]["isError"], false,
            "public MCP error code: {}",
            status["result"]["structuredContent"]["error"]["code"]
        );
        let mut changed = broker.descriptor.clone();
        changed.binding.search_id = "another-search".into();
        assert!(read_capability(&capability, &changed).is_err());
        assert!(!serde_json::to_string(&descriptor)?.contains(&descriptor.capability));
        #[cfg(unix)]
        {
            let link = private.join("linked.capability");
            std::os::unix::fs::symlink(&capability, &link)?;
            assert!(read_capability(&link, &descriptor).is_err());
        }
        std::fs::write(&capability, vec![0; 95])?;
        assert!(read_capability(&capability, &descriptor).is_err());
        Ok(())
    }

    #[test]
    fn stdio_relay_keeps_fixed_inventory_and_never_prints_transport_material() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let owner_token =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9; 32]);
        let owner_path = server.connection_file.to_string_lossy().to_string();
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            call("status", json!({})),
            call("next", json!({})),
        ];
        let input = requests
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        let mut output = Vec::new();
        run_relay_stdio(&path, Cursor::new(input), &mut output)?;
        let output = String::from_utf8(output)?;
        let frames = output
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        assert!(frames.len() == 4);
        assert!(frames[0]["result"]["serverInfo"]["name"] == "ato-formation-session-mcp");
        let tools = frames[1]["result"]["tools"].as_array().unwrap();
        assert!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>()
                == ["status", "next", "submit"]
        );
        for secret in [&owner_token, &owner_path, &broker.descriptor.capability] {
            assert!(!output.contains(secret));
        }
        assert!(
            frames[0]["result"]["capabilities"]
                .get("resources")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn redacted_arguments_cannot_select_source_path_url_or_another_search() -> Result<()> {
        let (root, _bridge, server, digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let canary = random_hex()?;
        for name in ["status", "next"] {
            for field in [
                "source",
                "path",
                "url",
                "search_id",
                "connection",
                "private_grant",
            ] {
                let response = rpc(&broker.descriptor, call(name, json!({field:canary})))?;
                assert!(response["result"]["isError"] == true);
                assert!(!response.to_string().contains(&canary));
            }
        }
        let mut request = submit(&digest, OUTPUT);
        request["params"]["arguments"]["path"] = json!(canary);
        let response = rpc(&broker.descriptor, request)?;
        assert!(response["result"]["isError"] == true);
        assert!(!response.to_string().contains(&canary));
        let response = rpc(&broker.descriptor, call(&canary, json!({})))?;
        assert!(response["result"]["isError"] == true);
        assert!(!response.to_string().contains(&canary));
        Ok(())
    }

    #[test]
    fn dropped_reply_reconnection_and_duplicate_submit_keep_saved_response_and_budget() -> Result<()>
    {
        let (root, _bridge, server, digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let before = rpc(&broker.descriptor, call("next", json!({})))?;
        // Intentionally lose the transport result; a new relay reconciles saved status.
        let request = wire_request(&broker.descriptor, submit(&digest, OUTPUT))?;
        let mut stream = TcpStream::connect(broker.descriptor.address)?;
        stream.write_all(&serde_json::to_vec(&request)?)?;
        stream.write_all(b"\n")?;
        drop(stream);
        let after = rpc(&broker.descriptor, call("next", json!({})))?;
        assert!(after["result"]["structuredContent"]["exchange"]["response_saved"] == true);
        let response_path = root.path().join("reasoning/r001_s001.response.json");
        let saved = std::fs::read(&response_path)?;
        let second = read_descriptor(&path)?;
        assert!(rpc(&second, submit(&digest, OUTPUT))?["result"]["isError"] == false);
        assert!(std::fs::read(&response_path)? == saved);
        assert!(after["result"]["structuredContent"]["input"].is_null());
        for field in ["deadline_ms", "exchanges_used", "exchanges_remaining"] {
            assert!(
                before["result"]["structuredContent"][field]
                    == after["result"]["structuredContent"][field]
            );
        }
        let changed = OUTPUT.replace("no_progress", "insufficient_source");
        assert!(rpc(&second, submit(&digest, &changed))?["result"]["isError"] == true);
        assert!(
            rpc(
                &second,
                submit(&format!("sha256:{}", "c".repeat(64)), OUTPUT)
            )?["result"]["isError"]
                == true
        );
        assert!(std::fs::read(response_path)? == saved);
        let canceled = rpc(&second, call("cancel", json!({})))?;
        assert!(canceled["result"]["isError"] == true);
        assert!(canceled["result"]["structuredContent"]["error"]["code"] == "unknown_tool");
        let status = rpc(&second, call("status", json!({})))?;
        assert!(status["result"]["structuredContent"]["progress"]["status"] == "pending");
        Ok(())
    }

    #[test]
    fn duplicate_inner_fields_are_preserved_until_common_authority_rejects() -> Result<()> {
        let (root, _bridge, server, digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let duplicate = OUTPUT.replace(
            "\"reason\":\"no_progress\"",
            "\"reason\":\"no_progress\",\"reason\":\"insufficient_source\"",
        );
        assert!(rpc(&broker.descriptor, submit(&digest, &duplicate))?["result"]["isError"] == true);
        assert!(
            !root
                .path()
                .join("reasoning/r001_s001.response.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn auth_binding_unknown_fields_and_tampering_fail_before_authority() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let request =
            serde_json::to_value(wire_request(&broker.descriptor, call("next", json!({})))?)?;
        for field in ["proof", "nonce", "schema", "extra"] {
            let mut changed = request.clone();
            changed[field] = json!(random_hex()?);
            let rejected = raw(&broker.descriptor, &changed)?;
            assert!(!rejected.ok && rejected.response.is_none() && rejected.proof.is_none());
        }
        for field in ["search_id", "configuration_ref", "agent", "expires_at_ms"] {
            let mut changed = request.clone();
            changed["binding"][field] = match field {
                "agent" => json!({"kind":"claude_code","version":"2.1.288"}),
                "expires_at_ms" => json!(broker.descriptor.binding.expires_at_ms + 1),
                _ => json!("another-scope"),
            };
            let rejected = raw(&broker.descriptor, &changed)?;
            assert!(!rejected.ok && rejected.response.is_none());
        }
        let mut changed = request.clone();
        changed["request"]["params"]["name"] = json!("status");
        assert!(!raw(&broker.descriptor, &changed)?.ok);
        let status = rpc(&broker.descriptor, call("status", json!({})))?;
        assert!(status["result"]["structuredContent"]["progress"]["status"] == "pending");
        assert!(!serde_json::to_string(&request)?.contains(&broker.descriptor.capability));
        Ok(())
    }

    #[test]
    fn publication_is_no_clobber_and_cannot_extend_saved_deadline() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let short = now_ms()? + 30000;
        let broker = Broker::start(server, &path, Some(short))?;
        assert!(broker.descriptor.binding.expires_at_ms == short);
        let saved = std::fs::read(&path)?;
        let connection = root.path().join("agent/connection.json");
        assert!(
            Broker::start(
                FormationSessionMcpServer::connect(&connection)?,
                &path,
                None
            )
            .is_err()
        );
        assert!(std::fs::read(&path)? == saved);
        let future = root.path().join("future.json");
        assert!(
            Broker::start(
                FormationSessionMcpServer::connect(&connection)?,
                &future,
                Some(now_ms()? + 120000)
            )
            .is_err()
        );
        assert!(!future.exists());
        let expired = root.path().join("expired.json");
        assert!(
            Broker::start(
                FormationSessionMcpServer::connect(&connection)?,
                &expired,
                Some(1)
            )
            .is_err()
        );
        assert!(!expired.exists());
        Ok(())
    }

    #[test]
    fn descriptor_is_bounded_loopback_only_and_strictly_typed() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let saved = serde_json::to_value(&broker.descriptor)?;
        for (field, value) in [
            ("address", json!("192.0.2.1:1234")),
            ("address", json!("127.0.0.1:0")),
            ("capability", json!("a".repeat(63))),
            ("schema", json!("unknown")),
            ("path", json!("private")),
        ] {
            let mut changed = saved.clone();
            changed[field] = value;
            std::fs::write(&path, serde_json::to_vec(&changed)?)?;
            assert!(read_descriptor(&path).is_err());
        }
        std::fs::write(&path, vec![b'a'; DESCRIPTOR_CAP + 1])?;
        assert!(read_descriptor(&path).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("producer-link.json");
            std::os::unix::fs::symlink(&path, &link)?;
            assert!(read_descriptor(&link).is_err());
        }
        Ok(())
    }

    #[test]
    fn forged_broker_response_and_reflection_are_rejected_without_capability_disclosure()
    -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let mut descriptor = broker.descriptor.clone();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        descriptor.address = listener.local_addr()?;
        let capability = descriptor.capability.clone();
        let thread = thread::spawn(move || -> Result<bool> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(STREAM_TIMEOUT))?;
            let bytes = read_frame(&mut stream, REQUEST_CAP)?;
            let request: Request = serde_json::from_slice(&bytes)?;
            // A process reusing the old port can read request proofs, but cannot
            // sign replies or reflect a request-domain proof as a reply proof.
            let forged = Reply {
                ok: true,
                response: Some(json!({"jsonrpc":"2.0","id":1,"result":{"fully_satisfied":true}})),
                proof: Some(request.proof),
            };
            stream.write_all(&serde_json::to_vec(&forged)?)?;
            stream.write_all(b"\n")?;
            Ok(!String::from_utf8(bytes)?.contains(&capability))
        });
        assert!(forward(&descriptor, &call("status", json!({}))).is_err());
        assert!(
            thread
                .join()
                .map_err(|_| anyhow!("fixture listener failed"))??
        );
        Ok(())
    }

    #[test]
    fn disconnected_or_expired_relay_is_redacted_and_never_resubmits() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let capability = broker.descriptor.capability.clone();
        drop(broker);
        let request = serde_json::to_vec(&call("status", json!({})))?;
        let mut output = Vec::new();
        run_relay_stdio(&path, Cursor::new(request), &mut output)?;
        let response: Value = serde_json::from_slice(&output)?;
        assert!(response["error"]["code"] == -32000);
        assert!(!String::from_utf8(output)?.contains(&capability));
        let mut descriptor = read_descriptor(&path)?;
        descriptor.binding.expires_at_ms = 1;
        assert!(forward(&descriptor, &call("next", json!({}))).is_err());
        assert!(
            !root
                .path()
                .join("reasoning/r001_s001.response.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn broker_enforces_outer_frame_size_without_creating_a_response() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let mut stream = TcpStream::connect(broker.descriptor.address)?;
        stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
        stream.write_all(&vec![b' '; REQUEST_CAP + 1])?;
        let reply: Reply = serde_json::from_slice(&read_frame(&mut stream, RESPONSE_CAP)?)?;
        assert!(!reply.ok && reply.response.is_none());
        assert!(
            !root
                .path()
                .join("reasoning/r001_s001.response.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn slow_partial_frame_cannot_extend_the_absolute_read_deadline() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let mut stream = TcpStream::connect(listener.local_addr()?)?;
        let (mut peer, _) = listener.accept()?;
        let send = thread::spawn(move || {
            for _ in 0..100 {
                if peer.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        stream.set_read_timeout(Some(Duration::from_millis(80)))?;
        let started = Instant::now();
        assert!(read_frame(&mut stream, REQUEST_CAP).is_err());
        // A per-read timeout would accept drips for a full second. Use a generous
        // wall-clock bound while verifying that no successful frame is returned.
        assert!(started.elapsed() < Duration::from_millis(700));
        drop(stream);
        send.join().map_err(|_| anyhow!("fixture sender failed"))?;
        Ok(())
    }

    #[test]
    fn simultaneous_producer_connections_share_one_atomic_submission() -> Result<()> {
        let (root, _bridge, server, digest) = fixture()?;
        let path = root.path().join("producer.json");
        let broker = Broker::start(server, &path, None)?;
        let before = rpc(&broker.descriptor, call("next", json!({})))?;
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let threads = (0..2)
            .map(|_| {
                let descriptor = broker.descriptor.clone();
                let request = submit(&digest, OUTPUT);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    rpc(&descriptor, request)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for submitted in threads {
            let reply = submitted
                .join()
                .map_err(|_| anyhow!("fixture Producer failed"))??;
            assert!(reply["result"]["isError"] == false);
        }
        assert!(
            root.path()
                .join("reasoning/r001_s001.response.json")
                .exists()
        );
        let after = rpc(&broker.descriptor, call("next", json!({})))?;
        for field in ["deadline_ms", "exchanges_used", "exchanges_remaining"] {
            assert!(
                before["result"]["structuredContent"][field]
                    == after["result"]["structuredContent"][field]
            );
        }
        assert!(after["result"]["structuredContent"]["exchange"]["response_saved"] == true);
        Ok(())
    }
}
