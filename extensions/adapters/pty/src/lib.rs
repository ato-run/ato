//! Byte-level PTY evidence. It deliberately does not infer shell commands.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, CaptureGate, PresentationAsset, PresentationCapture,
    PresentationKind,
};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const PTY_ADAPTER_ID: &str = "ato.pty@1";
pub const PTY_PROTOCOL_ID: &str = "ato.pty@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyAdapterConfig {
    pub port_id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub initial_input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PtyEvent {
    Input { bytes: Vec<u8> },
    Output { bytes: Vec<u8> },
    Resize { columns: u16, rows: u16 },
    Signal { name: String },
    Attach,
    Detach,
}

pub fn encode_event(event: &PtyEvent) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(event)
}

pub fn decode_event(bytes: &[u8]) -> Result<PtyEvent, serde_json::Error> {
    let event = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&event)? != bytes {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PTY event is not canonical JCS",
        )));
    }
    Ok(event)
}

#[derive(Default)]
pub struct PtyAdapter;

impl AdapterFactory for PtyAdapter {
    fn id(&self) -> &str {
        PTY_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
            capture_consistency: ato_adapter_api::CaptureConsistency::AdapterMediated,
        }
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        let config: PtyAdapterConfig = serde_json::from_value(instance.config.clone())?;
        let program = config
            .command
            .first()
            .ok_or_else(|| AdapterError::InvalidConfig("PTY command is empty".to_owned()))?;
        let mut command = Command::new(program);
        command
            .args(&config.command[1..])
            .current_dir(context.runtime.workspace.join(config.cwd))
            .env_clear()
            .envs(explicit_base_environment())
            .envs(config.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let writer =
            Arc::new(Mutex::new(child.stdin.take().ok_or_else(|| {
                AdapterError::Operation("PTY stdin unavailable".to_owned())
            })?));
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let transcript = Arc::new(Mutex::new(VecDeque::new()));
        let failure = Arc::new(Mutex::new(None));
        let capture_gate = Arc::new(CaptureGate::default());
        let port_id = ato_computation::PortId::parse(&config.port_id)
            .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
        context
            .observations
            .emit(observation(&port_id, &PtyEvent::Attach)?)?;
        let readers = vec![
            spawn_output_reader(
                child.stdout.take().expect("piped stdout"),
                Arc::clone(&context.observations),
                port_id.clone(),
                Arc::clone(&output),
                Arc::clone(&transcript),
                Arc::clone(&failure),
                Arc::clone(&capture_gate),
            ),
            spawn_output_reader(
                child.stderr.take().expect("piped stderr"),
                Arc::clone(&context.observations),
                port_id.clone(),
                Arc::clone(&output),
                Arc::clone(&transcript),
                Arc::clone(&failure),
                Arc::clone(&capture_gate),
            ),
        ];
        if let Some(input) = config.initial_input {
            let event = PtyEvent::Input {
                bytes: input.as_bytes().to_vec(),
            };
            context.observations.emit(observation(&port_id, &event)?)?;
            writer
                .lock()
                .map_err(|_| AdapterError::Operation("PTY writer poisoned".to_owned()))?
                .write_all(input.as_bytes())?;
        }
        let gateway = std::env::var("ATO_PTY_GATEWAY_LISTEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                let listen: SocketAddr = value.parse().map_err(|error| {
                    AdapterError::InvalidConfig(format!("invalid PTY gateway address: {error}"))
                })?;
                spawn_terminal_gateway(
                    listen,
                    Arc::clone(&writer),
                    Arc::clone(&context.observations),
                    port_id.clone(),
                    Arc::clone(&failure),
                    Arc::clone(&capture_gate),
                    Arc::clone(&transcript),
                )
                .map(Some)
            })
            .transpose()?
            .flatten();
        Ok(Box::new(PtySession {
            instance_id: instance.instance_id.clone(),
            child,
            writer,
            output,
            failure,
            readers,
            observations: Arc::clone(&context.observations),
            port_id,
            activated: false,
            capture_gate,
            gateway,
            transcript,
        }))
    }
}

struct PtySession {
    instance_id: String,
    child: Child,
    writer: Arc<Mutex<ChildStdin>>,
    output: Arc<Mutex<VecDeque<u8>>>,
    failure: Arc<Mutex<Option<String>>>,
    readers: Vec<JoinHandle<()>>,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    activated: bool,
    capture_gate: Arc<CaptureGate>,
    gateway: Option<TerminalGateway>,
    transcript: Arc<Mutex<VecDeque<u8>>>,
}

impl AttachedAdapter for PtySession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        PTY_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&PtyAdapter)
    }

    fn presentation_capture(&mut self) -> Option<&mut dyn PresentationCapture> {
        Some(self)
    }

    fn apply(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes =
            read_exact_object(context.objects, &record.payload_ref, metadata.size, 1 << 20)?;
        let event =
            decode_event(&bytes).map_err(|error| AdapterError::Operation(error.to_string()))?;
        match event {
            PtyEvent::Input { bytes } => self
                .writer
                .lock()
                .map_err(|_| AdapterError::Operation("PTY writer poisoned".to_owned()))?
                .write_all(&bytes)
                .map_err(AdapterError::from),
            PtyEvent::Output { bytes } => self.verify_output(&bytes),
            PtyEvent::Resize { .. }
            | PtyEvent::Signal { .. }
            | PtyEvent::Attach
            | PtyEvent::Detach => Ok(()),
        }
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        AttachedAdapter::apply(self, record, context)
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.capture_gate.resume()?;
        self.gateway.take();
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        for reader in self.readers.drain(..) {
            reader
                .join()
                .map_err(|_| AdapterError::Operation("PTY reader panicked".to_owned()))?;
        }
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| AdapterError::Operation("PTY failure state poisoned".to_owned()))?
            .take()
        {
            return Err(AdapterError::Operation(error));
        }
        Ok(())
    }

    fn wait(&mut self) -> Result<(), AdapterError> {
        let status = self.child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(AdapterError::Operation(format!(
                "PTY process exited with {status}"
            )))
        }
    }

    fn activate(&mut self) -> Result<(), AdapterError> {
        if !self.activated {
            spawn_input_reader(
                Arc::clone(&self.writer),
                Arc::clone(&self.observations),
                self.port_id.clone(),
                Arc::clone(&self.failure),
                Arc::clone(&self.capture_gate),
            );
            self.activated = true;
        }
        Ok(())
    }

    fn pause_for_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.capture_gate.pause_and_drain()
    }

    fn resume_after_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.capture_gate.resume()
    }
}

impl PresentationCapture for PtySession {
    fn capture_final(
        &mut self,
        _context: &AdapterContext<'_>,
    ) -> Result<Vec<PresentationAsset>, AdapterError> {
        let transcript: Vec<u8> = self
            .transcript
            .lock()
            .map_err(|_| AdapterError::Operation("PTY transcript was poisoned".to_owned()))?
            .iter()
            .copied()
            .collect();
        let bytes = terminal_screen_projection(&transcript)?;
        Ok(vec![PresentationAsset {
            kind: PresentationKind::TerminalFinal,
            content_type: "application/vnd.ato.terminal-screen+json".to_owned(),
            width: None,
            height: None,
            sequence: 0,
            bytes,
        }])
    }
}

fn terminal_screen_projection(transcript: &[u8]) -> Result<Vec<u8>, AdapterError> {
    const COLUMNS: usize = 80;
    const ROWS: usize = 24;
    const MAX_TEXT_BYTES: usize = 64 * 1024;

    let printable = strip_terminal_controls(&String::from_utf8_lossy(transcript));
    let lines: Vec<&str> = printable.lines().collect();
    let start = lines.len().saturating_sub(ROWS);
    let mut text = lines[start..]
        .iter()
        .map(|line| line.chars().take(COLUMNS).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    while text.len() > MAX_TEXT_BYTES {
        let next = text
            .char_indices()
            .nth(1024)
            .map_or(text.len(), |(index, _)| index);
        text.drain(..next);
    }
    serde_jcs::to_vec(&serde_json::json!({
        "schema": "ato.terminal-screen@1",
        "columns": COLUMNS,
        "rows": ROWS,
        "text": text,
    }))
    .map_err(AdapterError::from)
}

fn strip_terminal_controls(input: &str) -> String {
    let mut result = String::new();
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            if characters.peek() == Some(&'[') {
                characters.next();
                for suffix in characters.by_ref() {
                    if ('@'..='~').contains(&suffix) {
                        break;
                    }
                }
            }
            continue;
        }
        match character {
            '\n' | '\t' => result.push(character),
            '\r' => {}
            value if !value.is_control() => result.push(value),
            _ => {}
        }
    }
    result
}

impl PtySession {
    fn verify_output(&self, expected: &[u8]) -> Result<(), AdapterError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut actual = Vec::new();
        while actual.len() < expected.len() && Instant::now() < deadline {
            if let Some(byte) = self
                .output
                .lock()
                .map_err(|_| AdapterError::Operation("PTY output queue poisoned".to_owned()))?
                .pop_front()
            {
                actual.push(byte);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if actual == expected {
            Ok(())
        } else {
            Err(AdapterError::Operation(format!(
                "PTY replay output mismatch (expected {} bytes {:?}, got {} bytes {:?})",
                expected.len(),
                String::from_utf8_lossy(expected),
                actual.len(),
                String::from_utf8_lossy(&actual)
            )))
        }
    }
}

fn observation(
    port_id: &ato_computation::PortId,
    event: &PtyEvent,
) -> Result<ato_adapter_api::AdapterObservation, AdapterError> {
    Ok(ato_adapter_api::AdapterObservation {
        adapter_id: PTY_ADAPTER_ID.to_owned(),
        protocol_id: ato_computation::ProtocolId::parse(PTY_PROTOCOL_ID)
            .expect("valid static PTY protocol"),
        port_id: port_id.clone(),
        direction: match event {
            PtyEvent::Input { .. } => ato_objects::Direction::Inbound,
            PtyEvent::Output { .. } => ato_objects::Direction::Outbound,
            _ => ato_objects::Direction::Internal,
        },
        payload: encode_event(event)?,
        caused_by: Vec::new(),
        effect: match event {
            PtyEvent::Input { .. } | PtyEvent::Resize { .. } | PtyEvent::Signal { .. } => {
                ato_adapter_api::ObservationEffect::Evolution
            }
            _ => ato_adapter_api::ObservationEffect::Evidence,
        },
        presentation_hint: ato_adapter_api::PresentationHint::None,
    })
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    output: Arc<Mutex<VecDeque<u8>>>,
    transcript: Arc<Mutex<VecDeque<u8>>>,
    failure: Arc<Mutex<Option<String>>>,
    capture_gate: Arc<CaptureGate>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let _permit = match capture_gate.enter() {
                        Ok(permit) => permit,
                        Err(error) => {
                            if let Ok(mut slot) = failure.lock() {
                                *slot = Some(error.to_string());
                            }
                            break;
                        }
                    };
                    let bytes = buffer[..size].to_vec();
                    let _ = std::io::stdout().write_all(&bytes);
                    if let Ok(mut queue) = output.lock() {
                        queue.extend(bytes.iter().copied());
                    }
                    if let Ok(mut history) = transcript.lock() {
                        history.extend(bytes.iter().copied());
                        while history.len() > 1024 * 1024 {
                            history.pop_front();
                        }
                    }
                    if let Err(error) = observations.emit(
                        observation(&port_id, &PtyEvent::Output { bytes })
                            .expect("PTY event serialization cannot fail"),
                    ) && let Ok(mut slot) = failure.lock()
                    {
                        *slot = Some(error.to_string());
                    }
                }
                Err(error) => {
                    if let Ok(mut slot) = failure.lock() {
                        *slot = Some(error.to_string());
                    }
                    break;
                }
            }
        }
    })
}

struct TerminalGateway {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for TerminalGateway {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_terminal_gateway(
    listen: SocketAddr,
    writer: Arc<Mutex<ChildStdin>>,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    failure: Arc<Mutex<Option<String>>>,
    capture_gate: Arc<CaptureGate>,
    transcript: Arc<Mutex<VecDeque<u8>>>,
) -> Result<TerminalGateway, AdapterError> {
    let listener = TcpListener::bind(listen)?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !thread_stop.load(std::sync::atomic::Ordering::Acquire) {
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(error) => {
                    set_failure(&failure, error.to_string());
                    break;
                }
            };
            // Accepted sockets can inherit nonblocking mode on some hosts.
            // The synchronous WebSocket handshake must not observe a transient
            // WouldBlock and reset an otherwise valid client connection.
            if let Err(error) = stream.set_nonblocking(false) {
                set_failure(&failure, error.to_string());
                continue;
            }
            let mut socket = match tungstenite::accept(stream) {
                Ok(socket) => socket,
                Err(error) => {
                    set_failure(&failure, error.to_string());
                    continue;
                }
            };
            let _ = socket
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(25)));
            let initial: Vec<u8> = transcript
                .lock()
                .map(|history| history.iter().copied().collect())
                .unwrap_or_default();
            if !initial.is_empty()
                && socket
                    .send(tungstenite::Message::Binary(initial.into()))
                    .is_err()
            {
                continue;
            }
            let mut sent = transcript.lock().map_or(0, |history| history.len());
            loop {
                if thread_stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = socket.close(None);
                    break;
                }
                match socket.read() {
                    Ok(tungstenite::Message::Binary(bytes)) => {
                        if let Err(error) =
                            gateway_input(&bytes, &writer, &observations, &port_id, &capture_gate)
                        {
                            set_failure(&failure, error.to_string());
                            break;
                        }
                    }
                    Ok(tungstenite::Message::Text(text)) => {
                        if let Err(error) = gateway_input(
                            text.as_bytes(),
                            &writer,
                            &observations,
                            &port_id,
                            &capture_gate,
                        ) {
                            set_failure(&failure, error.to_string());
                            break;
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => break,
                    Ok(tungstenite::Message::Ping(value)) => {
                        let _ = socket.send(tungstenite::Message::Pong(value));
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(tungstenite::Error::ConnectionClosed) => break,
                    Err(error) => {
                        set_failure(&failure, error.to_string());
                        break;
                    }
                }
                let pending: Vec<u8> = transcript.lock().map_or_else(
                    |_| Vec::new(),
                    |history| {
                        if sent > history.len() {
                            sent = 0;
                        }
                        let bytes = history.iter().skip(sent).copied().collect();
                        sent = history.len();
                        bytes
                    },
                );
                if !pending.is_empty()
                    && socket
                        .send(tungstenite::Message::Binary(pending.into()))
                        .is_err()
                {
                    break;
                }
            }
        }
    });
    Ok(TerminalGateway {
        stop,
        thread: Some(thread),
    })
}

fn gateway_input(
    bytes: &[u8],
    writer: &Arc<Mutex<ChildStdin>>,
    observations: &Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: &ato_computation::PortId,
    capture_gate: &Arc<CaptureGate>,
) -> Result<(), AdapterError> {
    let _permit = capture_gate.enter()?;
    let bytes = bytes.to_vec();
    observations.emit(observation(
        port_id,
        &PtyEvent::Input {
            bytes: bytes.clone(),
        },
    )?)?;
    writer
        .lock()
        .map_err(|_| AdapterError::Operation("PTY writer poisoned".to_owned()))?
        .write_all(&bytes)?;
    Ok(())
}

fn set_failure(failure: &Arc<Mutex<Option<String>>>, message: String) {
    if let Ok(mut slot) = failure.lock() {
        *slot = Some(message);
    }
}

fn spawn_input_reader(
    writer: Arc<Mutex<ChildStdin>>,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    failure: Arc<Mutex<Option<String>>>,
    capture_gate: Arc<CaptureGate>,
) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0_u8; 4096];
        while let Ok(size) = stdin.read(&mut buffer) {
            if size == 0 {
                break;
            }
            let bytes = buffer[..size].to_vec();
            let _permit = match capture_gate.enter() {
                Ok(permit) => permit,
                Err(error) => {
                    if let Ok(mut slot) = failure.lock() {
                        *slot = Some(error.to_string());
                    }
                    break;
                }
            };
            if let Err(error) = observations
                .emit(
                    observation(
                        &port_id,
                        &PtyEvent::Input {
                            bytes: bytes.clone(),
                        },
                    )
                    .expect("PTY event serialization cannot fail"),
                )
                .and_then(|_| {
                    writer
                        .lock()
                        .map_err(|_| AdapterError::Operation("PTY writer poisoned".to_owned()))?
                        .write_all(&bytes)
                        .map_err(AdapterError::from)
                })
            {
                if let Ok(mut slot) = failure.lock() {
                    *slot = Some(error.to_string());
                }
                break;
            }
        }
    });
}

fn explicit_base_environment() -> BTreeMap<String, String> {
    ["PATH", "SYSTEMROOT", "WINDIR"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_owned(), value))
        })
        .collect()
}

#[cfg(test)]
mod gateway_tests {
    use std::process::Stdio;

    use ato_adapter_api::IgnoreObservations;

    use super::*;

    #[test]
    fn terminal_projection_is_bounded_and_removes_ansi_controls() {
        let mut transcript = b"\x1b[31msecret-looking output\x1b[0m\r\n".to_vec();
        for index in 0..40 {
            transcript.extend_from_slice(format!("line-{index:02}\n").as_bytes());
        }
        let projection = terminal_screen_projection(&transcript).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&projection).unwrap();
        let text = value["text"].as_str().unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains("line-00"));
        assert!(text.contains("line-39"));
        assert_eq!(value["rows"], 24);
        assert_eq!(value["columns"], 80);
        assert!(projection.len() < 64 * 1024);
    }

    #[test]
    fn terminal_gateway_replays_diagnostic_and_accepts_input() {
        let mut child = Command::new("sh")
            .args(["-c", "cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let writer = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let mut output = child.stdout.take().unwrap();
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let transcript = Arc::new(Mutex::new(VecDeque::from(
            b"rustc error: expected expression\r\n".to_vec(),
        )));
        let failure = Arc::new(Mutex::new(None));
        let gateway = spawn_terminal_gateway(
            address,
            writer,
            Arc::new(IgnoreObservations),
            ato_computation::PortId::parse("terminal").unwrap(),
            Arc::clone(&failure),
            Arc::new(CaptureGate::default()),
            transcript,
        )
        .unwrap();
        let (mut socket, _) = tungstenite::connect(format!("ws://{address}/")).unwrap();
        let diagnostic = socket.read().unwrap().into_data();
        assert!(String::from_utf8_lossy(&diagnostic).contains("rustc error"));
        socket
            .send(tungstenite::Message::Binary(
                b"echo fixed\n".to_vec().into(),
            ))
            .unwrap();
        let mut accepted = [0_u8; 11];
        output.read_exact(&mut accepted).unwrap();
        assert_eq!(&accepted, b"echo fixed\n");
        socket.close(None).unwrap();
        drop(gateway);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(failure.lock().unwrap().is_none());
    }
}
