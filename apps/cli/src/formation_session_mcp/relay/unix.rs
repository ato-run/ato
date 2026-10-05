//! Unix-socket transport for the same scoped, authenticated Session relay.
use std::{
    fs, io,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
};

use super::*;

pub(super) const DESCRIPTOR_SCHEMA: &str = "ato.formation-session-unix-relay/2";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnixDescriptor {
    schema: String,
    socket: PathBuf,
    #[serde(skip)]
    capability: String,
    binding: Binding,
}

struct Stream {
    stream: UnixStream,
    read_timeout: std::cell::Cell<Option<Duration>>,
    write_timeout: std::cell::Cell<Option<Duration>>,
}

impl Stream {
    fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            read_timeout: std::cell::Cell::new(None),
            write_timeout: std::cell::Cell::new(None),
        })
    }
    fn wait(&self, event: libc::c_short, timeout: Duration) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let deadline = Instant::now() + timeout;
        let mut fd = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: event,
            revents: 0,
        };
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: fd is one initialized pollfd borrowed from a live stream;
            // poll does not retain its pointer and the timeout is bounded.
            let ready = unsafe { libc::poll(&mut fd, 1, millis) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if ready > 0 {
                if fd.revents & libc::POLLNVAL != 0 {
                    return Err(io::ErrorKind::InvalidInput.into());
                }
                return Ok(());
            }
        }
    }
}
impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer)
    }
}
impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
impl RelayStream for Stream {
    fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout.get())
    }
    fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout.get())
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout.set(timeout);
        Ok(())
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout.set(timeout);
        Ok(())
    }
    fn wait_readable(&self, timeout: Duration) -> io::Result<()> {
        self.wait(libc::POLLIN, timeout)
    }
    fn wait_writable(&self, timeout: Duration) -> io::Result<()> {
        self.wait(libc::POLLOUT, timeout)
    }
}

// Only unlink the exact socket created by this broker, including publication
// failures. Never replace or remove an existing endpoint or a successor inode.
struct SocketFile {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct UnixBroker {
    descriptor: UnixDescriptor,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _socket: SocketFile,
}

impl UnixBroker {
    #[cfg(test)]
    fn start(
        server: FormationSessionMcpServer,
        descriptor_file: &Path,
        socket_file: &Path,
        expiry: Option<u64>,
    ) -> Result<Self> {
        Self::start_with_capability(
            server,
            descriptor_file,
            &descriptor_file.with_extension("capability"),
            socket_file,
            expiry,
        )
    }

    #[cfg(test)]
    fn start_with_capability(
        server: FormationSessionMcpServer,
        descriptor_file: &Path,
        capability_file: &Path,
        socket_file: &Path,
        expiry: Option<u64>,
    ) -> Result<Self> {
        Self::start_with_scope(
            server,
            descriptor_file,
            capability_file,
            socket_file,
            expiry,
            false,
        )
    }

    fn start_with_scope(
        mut server: FormationSessionMcpServer,
        descriptor_file: &Path,
        capability_file: &Path,
        socket_file: &Path,
        expiry: Option<u64>,
        read_only: bool,
    ) -> Result<Self> {
        validate_socket_path(socket_file)?;
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

        // bind is atomic and never unlinks a conflicting file/socket/symlink.
        let listener = UnixListener::bind(socket_file)?;
        let metadata = fs::symlink_metadata(socket_file)?;
        let socket = SocketFile {
            path: socket_file.into(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        fs::set_permissions(socket_file, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let descriptor = UnixDescriptor {
            schema: DESCRIPTOR_SCHEMA.into(),
            socket: socket_file.into(),
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
                && now_ms().is_ok_and(|n| n < identity.binding.expires_at_ms)
            {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Ok(mut stream) = Stream::new(stream) {
                            handle_stream(
                                &mut stream,
                                &mut server,
                                &identity.binding,
                                &identity.capability,
                            );
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
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
            _socket: socket,
        })
    }
}

impl Drop for UnixBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Descriptor is retained; dropping this broker does not cancel Search.
    }
}

/// Serve a previously created Session at one fresh Unix socket. The owner
/// connection stays outside the Producer filesystem boundary.
pub fn serve_owner(
    connection: &Path,
    descriptor: &Path,
    socket: &Path,
    expiry: Option<u64>,
) -> Result<()> {
    serve_owner_with_capability(
        connection,
        descriptor,
        &descriptor.with_extension("capability"),
        socket,
        expiry,
    )
}

pub fn serve_owner_with_capability(
    connection: &Path,
    descriptor: &Path,
    capability: &Path,
    socket: &Path,
    expiry: Option<u64>,
) -> Result<()> {
    serve_owner_with_scope(connection, descriptor, capability, socket, expiry, false)
}

pub fn serve_owner_with_scope(
    connection: &Path,
    descriptor: &Path,
    capability: &Path,
    socket: &Path,
    expiry: Option<u64>,
    read_only: bool,
) -> Result<()> {
    ensure!(
        descriptor != capability,
        "private capability destination rejected"
    );
    let broker = UnixBroker::start_with_scope(
        FormationSessionMcpServer::connect(connection)?,
        descriptor,
        capability,
        socket,
        expiry,
        read_only,
    )?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&broker.stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&broker.stop))?;
    while !broker.stop.load(Ordering::Relaxed)
        && now_ms()? < broker.descriptor.binding.expires_at_ms
    {
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

pub(super) fn run_stdio(
    bytes: &[u8],
    capability_file: &Path,
    input: impl BufRead,
    output: impl Write,
) -> Result<()> {
    let mut descriptor: UnixDescriptor = serde_json::from_slice(bytes)?;
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

fn validate_socket_path(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow!("relay socket path rejected"))?;
    // Fits both Darwin's 104-byte and Linux's 108-byte sockaddr_un; reject
    // controls and alternate spellings/symlink parents before connecting.
    ensure!(
        path.is_absolute() && text.len() <= 100 && !text.chars().any(char::is_control),
        "relay socket path rejected"
    );
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("relay socket path rejected"))?;
    ensure!(
        parent.canonicalize()? == parent
            && path.file_name().is_some()
            && path.components().all(|c| !matches!(
                c,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )),
        "relay socket path rejected"
    );
    Ok(())
}

fn validate(descriptor: &UnixDescriptor) -> Result<()> {
    ensure!(
        descriptor.schema == DESCRIPTOR_SCHEMA
            && valid_hex(&descriptor.capability)
            && !descriptor.binding.search_id.is_empty()
            && descriptor.binding.search_id.len() <= 160
            && !descriptor.binding.configuration_ref.is_empty()
            && descriptor.binding.configuration_ref.len() <= 128
            && descriptor.binding.expires_at_ms != 0,
        "relay descriptor rejected"
    );
    validate_socket_path(&descriptor.socket)
}

fn forward(descriptor: &UnixDescriptor, request: &Value) -> Result<Option<Value>> {
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
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket
        .connect_timeout(
            &socket2::SockAddr::unix(&descriptor.socket)?,
            STREAM_TIMEOUT,
        )
        .context("unix relay connect")?;
    let mut stream = Stream::new(socket.into())?;
    stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
    stream.set_write_timeout(Some(STREAM_TIMEOUT))?;
    write_frame(&mut stream, &bytes).context("unix relay write")?;
    let reply: Reply =
        serde_json::from_slice(&read_frame(&mut stream, RESPONSE_CAP).context("unix relay read")?)?;
    ensure!(reply.ok, "relay request rejected");
    verify_proof(
        &descriptor.capability,
        &reply_message(&request, &reply)?,
        reply.proof.as_deref().unwrap_or(""),
    )?;
    Ok(reply.response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formation_session_mcp::tests::fixture;
    use anyhow::Context;

    fn call(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})
    }

    fn socket_root() -> Result<tempfile::TempDir> {
        let local = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        fs::create_dir_all(&local)?;
        // Native macOS CI supplies /var/folders as TMPDIR. Keep fixtures in
        // the workspace and use a short existing ancestor .tmp when a
        // nested worktree would exceed the Unix socket path bound.
        for directory in std::iter::once(local).chain(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .map(|parent| parent.join(".tmp")),
        ) {
            if !directory.is_dir() {
                continue;
            }
            let directory = directory.canonicalize()?;
            if directory.as_os_str().as_encoded_bytes().len() + 20 > 100 {
                continue;
            }
            return Ok(tempfile::Builder::new().prefix("u").tempdir_in(directory)?);
        }
        anyhow::bail!("short workspace .tmp directory required for Unix socket fixture")
    }

    #[test]
    fn unix_expired_unknown_relay_enforces_read_only_at_dispatch() -> Result<()> {
        let deadline = now_ms()?.saturating_sub(1);
        let (root, bridge, server, _) = crate::formation_session_mcp::tests::fixture_at(deadline)?;
        bridge.update(&json!({"status":"unknown","unresolved_attempts":1,
            "attempts":[{"attempt_id":"attempt-original","status":"unknown"}]}))?;
        let sockets = socket_root()?;
        let path = root.path().join("readonly.json");
        let socket = sockets.path().join("relay.sock");
        let broker = UnixBroker::start_with_scope(
            server,
            &path,
            &path.with_extension("capability"),
            &socket,
            Some(now_ms()? + 30_000),
            true,
        )?;
        let status = forward(&broker.descriptor, &call("next", json!({})))?
            .ok_or_else(|| anyhow!("missing status"))?;
        assert_eq!(
            status["result"]["structuredContent"]["deadline_ms"],
            deadline
        );
        assert!(status["result"]["structuredContent"]["input"].is_null());
        let denied = forward(&broker.descriptor, &call("submit", json!({})))?
            .ok_or_else(|| anyhow!("missing refusal"))?;
        assert_eq!(denied["error"]["code"], -32601);
        drop(broker);
        assert!(!socket.exists());
        Ok(())
    }

    #[test]
    fn unix_reconnection_keeps_saved_reply_and_rejects_stale_or_changed_resubmission() -> Result<()>
    {
        let (root, _bridge, server, digest) = fixture()?;
        let path = root.path().join("unix-producer.json");
        let socket_root = socket_root()?;
        let socket = socket_root.path().join("relay.sock");
        let broker = UnixBroker::start(server, &path, &socket, None)?;
        let mut descriptor: UnixDescriptor =
            serde_json::from_slice(&read_descriptor_bytes(&path)?)?;
        descriptor.capability = read_capability(&path.with_extension("capability"), &descriptor)?;
        let before = forward(&descriptor, &call("next", json!({})))
            .context("unix next before response")?
            .unwrap();
        let output = r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"no_progress"}]}"#;
        let submit = call(
            "submit",
            json!({"exchange_id":"search_test_r1_s1","input_sha256":digest,"output_json":output}),
        );
        let mut request = Request {
            schema: REQUEST_SCHEMA.into(),
            nonce: random_hex()?,
            proof: String::new(),
            binding: descriptor.binding.clone(),
            request: submit.clone(),
        };
        request.proof = proof(&descriptor.capability, &request_message(&request)?)?;
        let mut stream = Stream::new(UnixStream::connect(&socket)?)?;
        write_frame(&mut stream, &serde_json::to_vec(&request)?)
            .context("unix save reply before disconnect")?;
        drop(stream); // Response is deliberately lost after publication.
        let after = forward(&descriptor, &call("next", json!({})))
            .context("unix reconcile saved response")?
            .unwrap();
        assert!(after["result"]["structuredContent"]["exchange"]["response_saved"] == true);
        let saved = fs::read(root.path().join("reasoning/r001_s001.response.json"))?;
        assert!(
            forward(&descriptor, &submit)
                .context("unix duplicate submission")?
                .unwrap()["result"]["isError"]
                == false
        );
        let mut changed = submit.clone();
        changed["params"]["arguments"]["output_json"] =
            json!(output.replace("no_progress", "insufficient_source"));
        assert!(forward(&descriptor, &changed)?.unwrap()["result"]["isError"] == true);
        changed = submit;
        changed["params"]["arguments"]["input_sha256"] =
            json!(format!("sha256:{}", "c".repeat(64)));
        assert!(forward(&descriptor, &changed)?.unwrap()["result"]["isError"] == true);
        assert!(fs::read(root.path().join("reasoning/r001_s001.response.json"))? == saved);
        for field in ["deadline_ms", "exchanges_used", "exchanges_remaining"] {
            assert!(
                before["result"]["structuredContent"][field]
                    == after["result"]["structuredContent"][field]
            );
        }
        drop(broker);
        assert!(!socket.exists() && path.exists());
        Ok(())
    }

    #[test]
    fn unix_publication_never_clobbers_endpoints_or_descriptors() -> Result<()> {
        let (root, _bridge, server, _digest) = fixture()?;
        let path = root.path().join("producer.json");
        let socket_root = socket_root()?;
        let socket = socket_root.path().join("relay.sock");
        fs::write(&path, b"retained")?;
        assert!(UnixBroker::start(server, &path, &socket, None).is_err());
        assert!(fs::read(&path)? == b"retained" && !socket.exists());
        let (_other, _bridge, server, _) = fixture()?;
        fs::write(&socket, b"existing endpoint")?;
        assert!(UnixBroker::start(server, &root.path().join("fresh.json"), &socket, None).is_err());
        assert!(fs::read(&socket)? == b"existing endpoint");
        Ok(())
    }

    #[test]
    fn unix_drop_preserves_replaced_socket_and_private_material_never_reaches_stdio() -> Result<()>
    {
        let (root, _bridge, server, _) = fixture()?;
        let owner_token =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9; 32]);
        let owner_path = server.connection_file.to_string_lossy().to_string();
        let path = root.path().join("producer.json");
        let socket_root = socket_root()?;
        let socket = socket_root.path().join("relay.sock");
        let broker = UnixBroker::start(server, &path, &socket, None)?;
        let mut output = Vec::new();
        super::super::run_relay_stdio(
            &path,
            Cursor::new(serde_json::to_vec(&call("status", json!({})))?),
            &mut output,
        )?;
        let output = String::from_utf8(output)?;
        for secret in [&owner_token, &owner_path, &broker.descriptor.capability] {
            assert!(!output.contains(secret) && !fs::read_to_string(&path)?.contains(&owner_path));
        }
        fs::remove_file(&socket)?;
        fs::write(&socket, b"successor")?;
        drop(broker);
        assert!(fs::read(&socket)? == b"successor");
        Ok(())
    }

    #[test]
    fn unix_expiry_or_invalid_path_cannot_cancel_search() -> Result<()> {
        let (root, _bridge, server, _) = fixture()?;
        let path = root.path().join("producer.json");
        let socket_root = socket_root()?;
        let socket = socket_root.path().join("relay.sock");
        let broker = UnixBroker::start(server, &path, &socket, None)?;
        let mut expired = broker.descriptor.clone();
        expired.binding.expires_at_ms = now_ms()? - 1;
        assert!(forward(&expired, &call("cancel", json!({}))).is_err());
        let mut relative = broker.descriptor.clone();
        relative.socket = PathBuf::from("relay.sock");
        assert!(forward(&relative, &call("cancel", json!({}))).is_err());
        let status = forward(&broker.descriptor, &call("status", json!({})))?.unwrap();
        assert!(status["result"]["structuredContent"]["progress"]["status"] == "pending");
        Ok(())
    }
}
