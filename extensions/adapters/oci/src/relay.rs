//! The relay the Run starts when its daemon keeps the container network out of
//! reach.
//!
//! The image is built here, from the relay binary embedded in this crate, and
//! loaded with `docker load`. Nothing is pulled: the Run works offline, and the
//! image's identity is the digest of its own config, so two Runs on the same
//! architecture build the same image.
//!
//! The container is owned, not borrowed: it is attached to the Run's internal
//! network only, it has a read-only root, no capabilities, no new privileges, a
//! non-root user and resource limits, and it is removed only after the group's
//! services are confirmed stopped — with the container's absence verified, not
//! assumed.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::relay_protocol::{Frame, MAX_FRAME_PAYLOAD};
use anyhow::{Context, Result, bail, ensure};

use crate::DockerClient;
use crate::connector::{ConnectorKind, PortConnection, PortConnector, PortRead, PortWrite};

/// The relay binary for each architecture the daemon may report.
const RELAY_LINUX_ARM64: &[u8] = include_bytes!("../relay-bin/linux-arm64/ato-oci-relay");
const RELAY_LINUX_AMD64: &[u8] = include_bytes!("../relay-bin/linux-amd64/ato-oci-relay");

/// The sha256 of each committed relay binary, as produced by
/// `scripts/build-oci-relay.sh`.
///
/// The binary is embedded in the CLI, so this file is the provenance record:
/// the test below refuses a binary whose bytes differ from it, and CI rebuilds
/// the binary from source and checks the same file. A binary that cannot be
/// rebuilt, or a source change that was not reflected in the committed binary,
/// therefore fails a gate instead of shipping silently.
#[cfg_attr(not(test), allow(dead_code))]
const RELAY_SHA256SUMS: &str = include_str!("../relay-bin/SHA256SUMS");

/// How long one `connect` waits for the relay to accept or refuse.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the relay container gets to exit after its stream closes.
const RELAY_EXIT_GRACE: Duration = Duration::from_secs(5);
/// How much relay stderr is kept for diagnostics.
const RELAY_STDERR_MAX: usize = 4 * 1024;

/// What the Run's evidence records about this connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEvidence {
    /// The connector kind, as recorded in the receipt.
    pub connector: ConnectorKind,
    /// The image digest (`sha256:<config digest>`) the relay ran from.
    pub image_digest: String,
    /// The relay container's id.
    pub container_id: String,
}

/// A destination the relay may reach. Exactly the Ports the selected
/// Derivation declares for its services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayDestination {
    pub service: String,
    pub guest_port: u16,
}

impl RelayDestination {
    pub fn new(service: impl Into<String>, guest_port: u16) -> Self {
        Self {
            service: service.into(),
            guest_port,
        }
    }

    fn argument(&self) -> String {
        format!("{}:{}", self.service, self.guest_port)
    }
}

/// The relay image, built from this binary's embedded relay.
struct RelayImage {
    reference: String,
    digest: String,
}

/// Docker's architecture spelling, normalised to the two relay builds.
///
/// The caller passes the daemon's architecture as it observed it; this is the
/// adapter's own mapping, so the adapter does not depend on the host-facts
/// crate (an adapter may only use adapter-api and below).
fn normalize_architecture(architecture: &str) -> String {
    match architecture.trim().to_lowercase().as_str() {
        "aarch64" | "arm64" => "arm64".to_owned(),
        "x86_64" | "amd64" | "x86-64" => "amd64".to_owned(),
        other => other.to_owned(),
    }
}

fn relay_binary(architecture: &str) -> Result<&'static [u8]> {
    match architecture {
        "arm64" | "aarch64" | "arm" => Ok(RELAY_LINUX_ARM64),
        "amd64" | "x86_64" | "x86-64" => Ok(RELAY_LINUX_AMD64),
        other => bail!(
            "no relay binary for daemon architecture {other:?}; the relay is built for linux/arm64 \
             and linux/amd64"
        ),
    }
}

/// Deterministic minimal image: one static binary, nothing else.
///
/// The config digest is the image's identity, so the same architecture and
/// binary always produce the same digest and a Run can name it in its evidence.
fn build_image_tar(binary: &[u8], architecture: &str) -> Result<(Vec<u8>, String, String)> {
    let layer = layer_tar(binary)?;
    let diff_id = sha256_hex(&layer);
    let config = serde_json::json!({
        "architecture": architecture,
        "os": "linux",
        "config": {
            "Entrypoint": ["/ato-oci-relay"],
            "User": "65534:65534",
            "WorkingDir": "/"
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": [format!("sha256:{diff_id}")]
        },
        "history": [{"created_by": "ato oci relay"}]
    });
    let config_bytes = serde_json::to_vec(&config)?;
    let image_id = sha256_hex(&config_bytes);
    let reference = format!("ato-oci-relay:{}", &image_id[..12]);
    let manifest = serde_json::json!([{
        "Config": format!("{image_id}.json"),
        "RepoTags": [reference],
        "Layers": ["layer/layer.tar"]
    }]);
    let mut tar = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar);
        append_bytes(&mut builder, &format!("{image_id}.json"), &config_bytes)?;
        append_bytes(
            &mut builder,
            "manifest.json",
            &serde_json::to_vec(&manifest)?,
        )?;
        append_bytes(&mut builder, "layer/layer.tar", &layer)?;
        append_bytes(&mut builder, "layer/VERSION", b"1.0")?;
        builder.finish()?;
    }
    Ok((tar, reference, image_id))
}

fn layer_tar(binary: &[u8]) -> Result<Vec<u8>> {
    let mut layer = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut layer);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, "ato-oci-relay", binary)?;
        builder.finish()?;
    }
    Ok(layer)
}

fn append_bytes<W: Write>(builder: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Where the relay's frames go while it runs.
enum RelayInput {
    Child(Arc<Mutex<Option<ChildStdin>>>),
    /// Only the tests reach this: it drives the same multiplexer over a socket
    /// pair instead of a child process. Unix sockets, so unix only.
    #[cfg(unix)]
    #[cfg_attr(not(test), allow(dead_code))]
    Socket(Arc<Mutex<std::os::unix::net::UnixStream>>),
}

/// One relay container, and the stream that multiplexes its Ports.
pub struct DaemonRelay {
    client: DockerClient,
    container_id: String,
    image_digest: String,
    container_name: String,
    input: RelayInput,
    child: Mutex<Option<Child>>,
    pending: Arc<Mutex<BTreeMap<u32, mpsc::Sender<RelayMessage>>>>,
    next: AtomicU32,
    stopped: AtomicBool,
    stderr: Arc<Mutex<String>>,
}

#[derive(Debug)]
enum RelayMessage {
    Opened,
    Failed(String),
    Data(Vec<u8>),
    HalfClose,
    Closed,
}

impl DaemonRelay {
    /// Build and load the relay image, then start the relay on `network`.
    pub(crate) fn start(
        client: DockerClient,
        network: &str,
        destinations: &[RelayDestination],
        architecture: Option<&str>,
        run_label: &str,
    ) -> Result<Arc<Self>> {
        ensure!(
            !destinations.is_empty(),
            "a relay with no declared destination would proxy nothing"
        );
        let architecture = architecture
            .map(normalize_architecture)
            .unwrap_or_else(|| normalize_architecture(std::env::consts::ARCH));
        let image = load_image(&client, &architecture)?;

        let container_name = format!(
            "ato-relay-{}",
            &sha256_hex(format!("{run_label}:{architecture}").as_bytes())[..16]
        );
        // A leftover from an interrupted Run must not make this one unnamed.
        let _ = client
            .command()
            .args(["rm", "-f", &container_name])
            .output();

        let mut arguments: Vec<String> = vec![
            "run".to_owned(),
            "-i".to_owned(),
            "--name".to_owned(),
            container_name.clone(),
            "--network".to_owned(),
            network.to_owned(),
            "--network-alias".to_owned(),
            "ato-oci-relay".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
            "--user".to_owned(),
            "65534:65534".to_owned(),
            "--memory".to_owned(),
            "64m".to_owned(),
            "--pids-limit".to_owned(),
            "64".to_owned(),
            "--label".to_owned(),
            "ato.role=oci-relay".to_owned(),
            "--log-driver".to_owned(),
            "none".to_owned(),
            image.reference.clone(),
            "--connect-timeout-ms".to_owned(),
            "10000".to_owned(),
        ];
        if std::env::var_os("ATO_OCI_RELAY_TRACE").is_some() {
            arguments.push("--log-frames".to_owned());
        }
        for destination in destinations {
            arguments.push("--allow".to_owned());
            arguments.push(destination.argument());
        }

        let mut child = client
            .command()
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start the OCI relay container")?;
        let stdin = child.stdin.take().context("relay stdin was not piped")?;
        let stdout = child.stdout.take().context("relay stdout was not piped")?;
        let stderr = child.stderr.take().context("relay stderr was not piped")?;

        let malformed = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<BTreeMap<u32, mpsc::Sender<RelayMessage>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        spawn_reader(stdout, Arc::clone(&pending), Arc::clone(&malformed));
        let stderr_text = Arc::new(Mutex::new(String::new()));
        spawn_stderr_drain(stderr, Arc::clone(&stderr_text));

        let mut relay = Self {
            client: client.clone(),
            container_id: container_name.clone(),
            image_digest: image.digest.clone(),
            container_name,
            input: RelayInput::Child(Arc::new(Mutex::new(Some(stdin)))),
            child: Mutex::new(Some(child)),
            pending,
            next: AtomicU32::new(1),
            stopped: AtomicBool::new(false),
            stderr: stderr_text,
        };
        // The container exists a moment after `docker run` starts, so its id
        // is read rather than assumed. A relay that never appears is removed
        // here: leaving a "Created" container behind after a failed start is
        // exactly the leak the group's stop is supposed to make impossible.
        if let Err(error) = relay.wait_for_container() {
            let _ = relay.stop();
            return Err(error);
        }
        Ok(Arc::new(relay))
    }

    /// Wait, bounded, for the container to exist, and take its real id.
    fn wait_for_container(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let output = self
                .client
                .command()
                .args(["inspect", "--format", "{{.Id}}", &self.container_name])
                .output()
                .context("inspect the OCI relay container")?;
            if output.status.success() {
                let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !id.is_empty() {
                    self.container_id = id;
                    return Ok(());
                }
            }
            // A `docker run` that failed has already said why.
            let exited = self
                .child
                .lock()
                .map(|mut child| {
                    child
                        .as_mut()
                        .and_then(|child| child.try_wait().ok().flatten())
                })
                .ok()
                .flatten();
            if let Some(status) = exited {
                bail!(
                    "the OCI relay container {} did not start ({status}): {}",
                    self.container_name,
                    bounded_stderr(&self.stderr)
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "the OCI relay container {} did not appear within 30s: {}",
                    self.container_name,
                    bounded_stderr(&self.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The evidence this Run records for the connector it used.
    pub fn evidence(&self) -> RelayEvidence {
        RelayEvidence {
            connector: ConnectorKind::Relay,
            image_digest: format!("sha256:{}", self.image_digest),
            container_id: self.container_id.clone(),
        }
    }

    pub fn image_digest(&self) -> &str {
        &self.image_digest
    }

    pub fn container_id(&self) -> &str {
        &self.container_id
    }

    fn send(&self, frame: &Frame) -> Result<()> {
        let encoded = frame.encode()?;
        match &self.input {
            RelayInput::Child(stdin) => {
                let mut guard = stdin
                    .lock()
                    .map_err(|_| anyhow::anyhow!("relay stdin lock was poisoned"))?;
                let stdin = guard
                    .as_mut()
                    .context("the relay stream is already closed")?;
                stdin.write_all(&encoded)?;
                stdin.flush()?;
            }
            #[cfg(unix)]
            RelayInput::Socket(socket) => {
                let mut guard = socket
                    .lock()
                    .map_err(|_| anyhow::anyhow!("relay socket lock was poisoned"))?;
                guard.write_all(&encoded)?;
                guard.flush()?;
            }
        }
        Ok(())
    }

    /// Stop the relay and prove its container is gone.
    pub fn stop(&self) -> Result<()> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Closing the stream makes the relay exit on its own.
        if let RelayInput::Child(stdin) = &self.input
            && let Ok(mut guard) = stdin.lock()
        {
            guard.take();
        }
        {
            let mut child = self
                .child
                .lock()
                .map_err(|_| anyhow::anyhow!("relay child lock was poisoned"))?;
            if let Some(mut child) = child.take() {
                let deadline = Instant::now() + RELAY_EXIT_GRACE;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            }
        }
        let _ = self
            .client
            .command()
            .args(["rm", "-f", &self.container_name])
            .output();
        // Absence, not the command's exit status, is the evidence.
        let present = self
            .client
            .command()
            .args(["inspect", "--format", "{{.Id}}", &self.container_name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        ensure!(
            !present,
            "the OCI relay container {} still exists after removal",
            self.container_name
        );
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("relay pending lock was poisoned"))?;
            for (_, sender) in pending.iter() {
                let _ = sender.send(RelayMessage::Closed);
            }
            pending.clear();
        }
        Ok(())
    }

    fn close_connection(&self, conn: u32) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&conn);
        }
        let _ = self.send(&Frame::Close { conn });
    }

    /// Only for tests: drive the same multiplexer over a socket pair instead of
    /// a child process, so the framing is testable without Docker.
    #[cfg(unix)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_stream(
        client: DockerClient,
        socket: std::os::unix::net::UnixStream,
        container_id: &str,
    ) -> Arc<Self> {
        let reader = socket.try_clone().expect("clone relay test socket");
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        spawn_reader(
            reader,
            Arc::clone(&pending),
            Arc::new(AtomicBool::new(false)),
        );
        Arc::new(Self {
            client,
            container_id: container_id.to_owned(),
            image_digest: "test".to_owned(),
            container_name: container_id.to_owned(),
            input: RelayInput::Socket(Arc::new(Mutex::new(socket))),
            child: Mutex::new(None),
            pending,
            next: AtomicU32::new(1),
            stopped: AtomicBool::new(false),
            stderr: Arc::new(Mutex::new(String::new())),
        })
    }
}

fn bounded_stderr(stderr: &Arc<Mutex<String>>) -> String {
    stderr
        .lock()
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

fn spawn_reader(
    mut stdout: impl Read + Send + 'static,
    pending: Arc<Mutex<BTreeMap<u32, mpsc::Sender<RelayMessage>>>>,
    malformed: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("ato-oci-relay-reader".to_owned())
        .spawn(move || {
            loop {
                match Frame::decode(&mut stdout) {
                    Ok(Some(frame)) => match frame {
                        Frame::Hello { .. } => {}
                        Frame::Opened { conn } => {
                            if let Some(sender) = pending
                                .lock()
                                .ok()
                                .and_then(|pending| pending.get(&conn).cloned())
                            {
                                let _ = sender.send(RelayMessage::Opened);
                            }
                        }
                        Frame::OpenFailed { conn, reason } => {
                            let sender = pending
                                .lock()
                                .ok()
                                .and_then(|mut pending| pending.remove(&conn));
                            if let Some(sender) = sender {
                                let _ = sender.send(RelayMessage::Failed(reason));
                            }
                        }
                        Frame::Data { conn, bytes } => {
                            let sender = pending
                                .lock()
                                .ok()
                                .and_then(|pending| pending.get(&conn).cloned());
                            if let Some(sender) = sender {
                                // A closed connection is not an error for the
                                // demultiplexer: the peer's FIN and the stream's
                                // close race by design.
                                let _ = sender.send(RelayMessage::Data(bytes));
                            }
                        }
                        Frame::HalfClose { conn } => {
                            let sender = pending
                                .lock()
                                .ok()
                                .and_then(|pending| pending.get(&conn).cloned());
                            if let Some(sender) = sender {
                                let _ = sender.send(RelayMessage::HalfClose);
                            }
                        }
                        Frame::Close { conn } => {
                            let sender = pending
                                .lock()
                                .ok()
                                .and_then(|mut pending| pending.remove(&conn));
                            if let Some(sender) = sender {
                                let _ = sender.send(RelayMessage::Closed);
                            }
                        }
                        Frame::Open { .. } => malformed.store(true, Ordering::SeqCst),
                    },
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            // The relay is gone: every open connection ends, rather than
            // hanging a reader forever.
            if let Ok(mut pending) = pending.lock() {
                for (_, sender) in pending.iter() {
                    let _ = sender.send(RelayMessage::Closed);
                }
                pending.clear();
            }
        })
        .ok();
}

fn spawn_stderr_drain(mut stderr: impl Read + Send + 'static, kept: Arc<Mutex<String>>) {
    std::thread::Builder::new()
        .name("ato-oci-relay-stderr".to_owned())
        .spawn(move || {
            let mut buffer = [0u8; 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(read) => {
                        let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
                        if std::env::var_os("ATO_OCI_RELAY_TRACE").is_some() {
                            eprint!("[ato oci relay] {text}");
                        }
                        if let Ok(mut kept) = kept.lock()
                            && kept.len() < RELAY_STDERR_MAX
                        {
                            kept.push_str(&text);
                        }
                    }
                    Err(_) => return,
                }
            }
        })
        .ok();
}

impl PortConnector for DaemonRelay {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Relay
    }

    fn connect(&self, service: &str, guest_port: u16) -> Result<Box<dyn PortConnection>> {
        let conn = self.next.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("relay pending lock was poisoned"))?
            .insert(conn, sender);
        if let Err(error) = self.send(&Frame::Open {
            conn,
            service: service.to_owned(),
            port: guest_port,
        }) {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&conn);
            }
            return Err(error)
                .with_context(|| format!("open {service}:{guest_port} through relay"));
        }
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(RelayMessage::Opened) => {
                    return Ok(Box::new(RelayConnection {
                        conn,
                        pending: self.pending.clone(),
                        sender: Some(self.clone_sender()),
                        receiver: Some(receiver),
                        buffered: Vec::new(),
                    }));
                }
                Ok(RelayMessage::Failed(reason)) => {
                    self.close_connection(conn);
                    bail!("the relay refused {service}:{guest_port}: {reason}");
                }
                Ok(RelayMessage::Closed) => {
                    self.close_connection(conn);
                    bail!("the relay closed {service}:{guest_port} before it opened");
                }
                Ok(_) => continue,
                Err(_) => {
                    self.close_connection(conn);
                    bail!(
                        "the relay did not answer for {service}:{guest_port} within {}s (relay log: {})",
                        CONNECT_TIMEOUT.as_secs(),
                        bounded_stderr(&self.stderr)
                    );
                }
            }
        }
    }
}

impl DaemonRelay {
    fn clone_sender(&self) -> OutboundFrame {
        OutboundFrame(match &self.input {
            RelayInput::Child(stdin) => Outbound::Child(Arc::clone(stdin)),
            #[cfg(unix)]
            RelayInput::Socket(socket) => Outbound::Socket(Arc::clone(socket)),
        })
    }
}

/// The write side of one connection's transport, held by the connection so it
/// does not need a reference to the relay.
struct OutboundFrame(Outbound);

enum Outbound {
    Child(Arc<Mutex<Option<ChildStdin>>>),
    #[cfg(unix)]
    Socket(Arc<Mutex<std::os::unix::net::UnixStream>>),
}

impl OutboundFrame {
    fn write_frame(&self, frame: &Frame) -> std::io::Result<()> {
        let encoded = frame.encode()?;
        match &self.0 {
            Outbound::Child(stdin) => {
                let mut guard = stdin
                    .lock()
                    .map_err(|_| std::io::Error::other("relay stdin lock was poisoned"))?;
                let stdin = guard
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("the relay stream is closed"))?;
                stdin.write_all(&encoded)?;
                stdin.flush()
            }
            #[cfg(unix)]
            Outbound::Socket(socket) => {
                let mut guard = socket
                    .lock()
                    .map_err(|_| std::io::Error::other("relay socket lock was poisoned"))?;
                guard.write_all(&encoded)?;
                guard.flush()
            }
        }
    }
}

/// One multiplexed connection to a Port.
struct RelayConnection {
    conn: u32,
    pending: Arc<Mutex<BTreeMap<u32, mpsc::Sender<RelayMessage>>>>,
    sender: Option<OutboundFrame>,
    receiver: Option<mpsc::Receiver<RelayMessage>>,
    buffered: Vec<u8>,
    // Read as: the peer half-closed, so `read` reports EOF after the buffer.
}

impl Read for RelayConnection {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.buffered.is_empty() {
            let taken = self.buffered.len().min(buffer.len());
            buffer[..taken].copy_from_slice(&self.buffered[..taken]);
            self.buffered.drain(..taken);
            return Ok(taken);
        }
        let Some(receiver) = self.receiver.as_ref() else {
            return Ok(0);
        };
        loop {
            match receiver.recv() {
                Ok(RelayMessage::Data(bytes)) => {
                    let taken = bytes.len().min(buffer.len());
                    buffer[..taken].copy_from_slice(&bytes[..taken]);
                    if taken < bytes.len() {
                        self.buffered.extend_from_slice(&bytes[taken..]);
                    }
                    return Ok(taken);
                }
                Ok(RelayMessage::HalfClose) | Ok(RelayMessage::Closed) => return Ok(0),
                Ok(RelayMessage::Opened) => continue,
                Ok(RelayMessage::Failed(_)) => return Ok(0),
                Err(_) => return Ok(0),
            }
        }
    }
}

impl Write for RelayConnection {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(std::io::Error::other("the relay connection is closed"));
        };
        let chunk = &buffer[..buffer.len().min(MAX_FRAME_PAYLOAD)];
        sender.write_frame(&Frame::Data {
            conn: self.conn,
            bytes: chunk.to_vec(),
        })?;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PortConnection for RelayConnection {
    fn shutdown_write(&mut self) -> std::io::Result<()> {
        let Some(sender) = self.sender.as_ref() else {
            return Ok(());
        };
        sender.write_frame(&Frame::HalfClose { conn: self.conn })
    }

    fn split(mut self: Box<Self>) -> std::io::Result<(Box<dyn PortRead>, Box<dyn PortWrite>)> {
        let conn = self.conn;
        let pending = Arc::clone(&self.pending);
        let sender = self.sender.take();
        let receiver = self.receiver.take();
        let buffered = std::mem::take(&mut self.buffered);
        // The halves own the teardown from here.
        std::mem::forget(self);
        Ok((
            Box::new(RelayRead {
                conn,
                receiver,
                buffered,
            }),
            Box::new(RelayWrite {
                conn,
                pending,
                sender,
            }),
        ))
    }
}

/// The read half of one relay connection.
struct RelayRead {
    conn: u32,
    receiver: Option<mpsc::Receiver<RelayMessage>>,
    buffered: Vec<u8>,
}

impl PortRead for RelayRead {}

impl Read for RelayRead {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let _ = self.conn;
        if !self.buffered.is_empty() {
            let taken = self.buffered.len().min(buffer.len());
            buffer[..taken].copy_from_slice(&self.buffered[..taken]);
            self.buffered.drain(..taken);
            return Ok(taken);
        }
        let Some(receiver) = self.receiver.as_ref() else {
            return Ok(0);
        };
        loop {
            match receiver.recv() {
                Ok(RelayMessage::Data(bytes)) => {
                    let taken = bytes.len().min(buffer.len());
                    buffer[..taken].copy_from_slice(&bytes[..taken]);
                    if taken < bytes.len() {
                        self.buffered.extend_from_slice(&bytes[taken..]);
                    }
                    return Ok(taken);
                }
                Ok(RelayMessage::HalfClose) | Ok(RelayMessage::Closed) => return Ok(0),
                Ok(RelayMessage::Opened) => continue,
                Ok(RelayMessage::Failed(_)) => return Ok(0),
                Err(_) => return Ok(0),
            }
        }
    }
}

/// The write half of one relay connection, which also owns closing it.
struct RelayWrite {
    conn: u32,
    pending: Arc<Mutex<BTreeMap<u32, mpsc::Sender<RelayMessage>>>>,
    sender: Option<OutboundFrame>,
}

impl PortWrite for RelayWrite {
    fn shutdown_write(&mut self) -> std::io::Result<()> {
        let Some(sender) = self.sender.as_ref() else {
            return Ok(());
        };
        sender.write_frame(&Frame::HalfClose { conn: self.conn })
    }
}

impl Write for RelayWrite {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(std::io::Error::other("the relay connection is closed"));
        };
        let chunk = &buffer[..buffer.len().min(MAX_FRAME_PAYLOAD)];
        sender.write_frame(&Frame::Data {
            conn: self.conn,
            bytes: chunk.to_vec(),
        })?;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for RelayWrite {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.write_frame(&Frame::Close { conn: self.conn });
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.conn);
        }
    }
}

impl Drop for RelayConnection {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.write_frame(&Frame::Close { conn: self.conn });
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.conn);
        }
    }
}

/// Build, load and verify the relay image for one architecture.
fn load_image(client: &DockerClient, architecture: &str) -> Result<RelayImage> {
    let binary = relay_binary(architecture)?;
    let (tar, reference, _image_id) = build_image_tar(binary, architecture)?;
    let mut child = client
        .command()
        // `-i -` is not accepted by every Docker CLI ("open -: no such file or
        // directory"); reading the archive from stdin without `-i` is the
        // form that works across versions.
        .args(["load"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("load the OCI relay image")?;
    // The image tar can be larger than a pipe buffer, so it is written from
    // its own thread while this one drains the child's stderr. Writing it
    // inline and then waiting would deadlock on a full stderr pipe, and a
    // failed write would lose the daemon's explanation.
    let mut stdin = child
        .stdin
        .take()
        .context("docker load stdin was not piped")?;
    let writer = std::thread::Builder::new()
        .name("ato-oci-relay-load".to_owned())
        .spawn(move || stdin.write_all(&tar))
        .context("spawn the relay image writer")?;
    let output = child.wait_with_output().context("wait for docker load")?;
    let written = writer
        .join()
        .map_err(|_| anyhow::anyhow!("the relay image writer panicked"))?;
    ensure!(
        output.status.success(),
        "docker load refused the relay image ({}): {}",
        match written {
            Ok(()) => "the image was written".to_owned(),
            Err(error) => format!("writing the image failed: {error}"),
        },
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let loaded = client
        .command()
        .args(["inspect", "--format", "{{.Id}}", &reference])
        .output()
        .context("inspect the loaded relay image")?;
    ensure!(
        loaded.status.success(),
        "the relay image {reference} was loaded but is not inspectable: {}",
        String::from_utf8_lossy(&loaded.stderr).trim()
    );
    let observed = String::from_utf8_lossy(&loaded.stdout).trim().to_owned();
    // The daemon's image identity, not a digest this crate computed: a
    // containerd-backed store names the image by its manifest, while the
    // config digest names the tag. Both are content-addressed and deterministic
    // for the same tar; the one recorded is the one the daemon will report.
    let digest = observed
        .strip_prefix("sha256:")
        .unwrap_or(&observed)
        .trim()
        .to_owned();
    ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the loaded relay image reported no usable digest: {observed:?}"
    );
    Ok(RelayImage { reference, digest })
}

/// Only the pieces of `DockerClient` the relay needs, kept behind a function so
/// the module does not depend on the adapter's construction details.
#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn client_from(executable: std::path::PathBuf) -> DockerClient {
    DockerClient::ambient(executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    #[cfg(unix)]
    fn fake_relay(mut stream: UnixStream) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            Frame::Hello {
                version: crate::relay_protocol::PROTOCOL.to_owned(),
            }
            .write_to(&mut stream)
            .unwrap();
            let mut open = 0u32;
            loop {
                match Frame::decode(&mut stream) {
                    Ok(Some(Frame::Open {
                        conn,
                        service,
                        port,
                    })) => {
                        if service == "web" && port == 8080 {
                            open = conn;
                            Frame::Opened { conn }.write_to(&mut stream).unwrap();
                        } else {
                            Frame::OpenFailed {
                                conn,
                                reason: "undeclared".to_owned(),
                            }
                            .write_to(&mut stream)
                            .unwrap();
                        }
                    }
                    Ok(Some(Frame::Data { conn, bytes })) if conn == open => {
                        // Echo, with a half-close after the request.
                        Frame::Data { conn, bytes }.write_to(&mut stream).unwrap();
                    }
                    Ok(Some(Frame::HalfClose { conn })) if conn == open => {
                        Frame::HalfClose { conn }.write_to(&mut stream).unwrap();
                    }
                    Ok(Some(Frame::Close { .. })) | Ok(None) => return,
                    Ok(Some(_)) => {}
                    Err(_) => return,
                }
            }
        })
    }

    #[cfg(unix)]
    #[test]
    fn a_connection_is_multiplexed_over_the_stream_and_half_close_is_kept() {
        let (host, relay) = UnixStream::pair().unwrap();
        let server = fake_relay(relay);
        let client = client_from(std::path::PathBuf::from("/bin/false"));
        let daemon = DaemonRelay::from_stream(client, host, "relay-test");

        let mut connection = daemon.connect("web", 8080).unwrap();
        assert_eq!(daemon.kind(), ConnectorKind::Relay);
        connection.write_all(b"ping").unwrap();
        connection.flush().unwrap();
        let mut echoed = [0u8; 4];
        connection.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        connection.shutdown_write().unwrap();
        // After the half-close the peer's EOF arrives, not an error.
        let mut eof = [0u8; 1];
        assert_eq!(connection.read(&mut eof).unwrap(), 0);
        drop(connection);
        let _ = server.join();
    }

    #[cfg(unix)]
    #[test]
    fn an_undeclared_destination_is_refused_by_the_relay() {
        let (host, relay) = UnixStream::pair().unwrap();
        let server = fake_relay(relay);
        let client = client_from(std::path::PathBuf::from("/bin/false"));
        let daemon = DaemonRelay::from_stream(client, host, "relay-test");

        let error = match daemon.connect("backend", 8081) {
            Ok(_) => panic!("an undeclared destination was accepted"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("undeclared"), "{error}");
        drop(daemon);
        let _ = server.join();
    }

    #[test]
    fn the_image_is_deterministic_for_one_architecture_and_names_its_digest() {
        let binary = relay_binary("arm64").unwrap();
        let (first, reference, digest) = build_image_tar(binary, "arm64").unwrap();
        let (second, reference_again, digest_again) = build_image_tar(binary, "arm64").unwrap();
        assert_eq!(first, second);
        assert_eq!(reference, reference_again);
        assert_eq!(digest, digest_again);
        assert_eq!(reference, format!("ato-oci-relay:{}", &digest[..12]));

        // The loaded image really contains the relay binary.
        let mut archive = tar::Archive::new(&first[..]);
        let mut layer_bytes = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().to_string_lossy() == "layer/layer.tar" {
                entry.read_to_end(&mut layer_bytes).unwrap();
            }
        }
        let mut layer = tar::Archive::new(&layer_bytes[..]);
        let mut found = false;
        for entry in layer.entries().unwrap() {
            let entry = entry.unwrap();
            if entry.path().unwrap().to_string_lossy() == "ato-oci-relay" {
                found = true;
                assert_eq!(entry.header().mode().unwrap() & 0o777, 0o755);
            }
        }
        assert!(found, "the relay binary is missing from the layer");
    }

    #[test]
    fn the_embedded_relay_binaries_match_their_recorded_hashes() {
        // Parsed from the committed SHA256SUMS, which the CI rebuild job also
        // verifies against a binary it produced from source.
        let recorded: BTreeMap<&str, &str> = RELAY_SHA256SUMS
            .lines()
            .filter_map(|line| {
                let (hash, path) = line.split_once("  ")?;
                Some((path.trim(), hash.trim()))
            })
            .collect();
        for (path, binary) in [
            ("linux-arm64/ato-oci-relay", RELAY_LINUX_ARM64),
            ("linux-amd64/ato-oci-relay", RELAY_LINUX_AMD64),
        ] {
            let expected = recorded
                .get(path)
                .unwrap_or_else(|| panic!("{path} is missing from SHA256SUMS"));
            let observed = sha256_hex(binary);
            assert_eq!(
                &observed, expected,
                "{path} does not match the hash recorded in relay-bin/SHA256SUMS;                  rebuild it with scripts/build-oci-relay.sh and update the file"
            );
        }
        assert_eq!(recorded.len(), 2, "SHA256SUMS lists an unexpected entry");
    }

    #[test]
    fn an_unknown_architecture_is_refused_rather_than_guessed() {
        assert!(relay_binary("riscv64").is_err());
        assert!(relay_binary("arm64").is_ok());
        assert!(relay_binary("amd64").is_ok());
    }
}
