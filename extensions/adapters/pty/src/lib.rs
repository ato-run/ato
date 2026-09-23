//! PTY input, resize, and signal operations. Output is written only to run logs.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, PresentationAsset, PresentationCapture, PresentationKind,
    Stylus, SupportedOperation,
};
use ato_objects::{RecordCandidate, RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const PTY_ADAPTER_ID: &str = "ato.pty@1";
pub const PTY_PROTOCOL_ID: &str = "ato.pty@1";
pub const PTY_INPUT_OPERATION: &str = "input";
pub const PTY_RESIZE_OPERATION: &str = "resize";
pub const PTY_SIGNAL_OPERATION: &str = "signal";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyAdapterConfig {
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
        }
    }

    fn supported_operations(&self) -> Vec<SupportedOperation> {
        [
            PTY_INPUT_OPERATION,
            PTY_RESIZE_OPERATION,
            PTY_SIGNAL_OPERATION,
        ]
        .into_iter()
        .map(|operation| {
            SupportedOperation::new(PTY_PROTOCOL_ID, operation, 1, BTreeSet::new())
                .expect("valid static PTY operation")
        })
        .collect()
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
        let port_id = ato_computation::PortId::parse(format!("terminal.{}", instance.instance_id))
            .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
        let stream_id = format!("pty.{}", instance.instance_id);
        let local_seq = Arc::new(AtomicU64::new(0));
        let readers = vec![
            spawn_output_reader(
                child.stdout.take().expect("piped stdout"),
                Arc::clone(&output),
                Arc::clone(&transcript),
                Arc::clone(&failure),
            ),
            spawn_output_reader(
                child.stderr.take().expect("piped stderr"),
                Arc::clone(&output),
                Arc::clone(&transcript),
                Arc::clone(&failure),
            ),
        ];
        if let Some(input) = config.initial_input {
            let event = PtyEvent::Input {
                bytes: input.as_bytes().to_vec(),
            };
            context.stylus.record(candidate(
                &port_id,
                &stream_id,
                &event,
                local_seq.fetch_add(1, Ordering::Relaxed) + 1,
            )?)?;
            context.observations.emit(observation(&port_id, &event)?)?;
            writer
                .lock()
                .map_err(|_| AdapterError::Operation("PTY writer poisoned".to_owned()))?
                .write_all(input.as_bytes())?;
        }
        Ok(Box::new(PtySession {
            instance_id: instance.instance_id.clone(),
            child,
            writer,
            output,
            transcript,
            failure,
            readers,
            stylus: Arc::clone(&context.stylus),
            observations: Arc::clone(&context.observations),
            port_id,
            stream_id,
            local_seq,
            activated: false,
        }))
    }
}

struct PtySession {
    instance_id: String,
    child: Child,
    writer: Arc<Mutex<ChildStdin>>,
    output: Arc<Mutex<VecDeque<u8>>>,
    transcript: Arc<Mutex<VecDeque<u8>>>,
    failure: Arc<Mutex<Option<String>>>,
    readers: Vec<JoinHandle<()>>,
    stylus: Arc<dyn Stylus>,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    stream_id: String,
    local_seq: Arc<AtomicU64>,
    activated: bool,
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
                Arc::clone(&self.stylus),
                Arc::clone(&self.observations),
                self.port_id.clone(),
                self.stream_id.clone(),
                Arc::clone(&self.local_seq),
                Arc::clone(&self.failure),
            );
            self.activated = true;
        }
        Ok(())
    }
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

fn candidate(
    port_id: &ato_computation::PortId,
    stream_id: &str,
    event: &PtyEvent,
    local_seq: u64,
) -> Result<RecordCandidate, AdapterError> {
    let operation = match event {
        PtyEvent::Input { .. } => PTY_INPUT_OPERATION,
        PtyEvent::Resize { .. } => PTY_RESIZE_OPERATION,
        PtyEvent::Signal { .. } => PTY_SIGNAL_OPERATION,
        PtyEvent::Output { .. } | PtyEvent::Attach | PtyEvent::Detach => {
            return Err(AdapterError::InvalidPayload(
                "PTY output and lifecycle observations are not Records".to_owned(),
            ));
        }
    };
    Ok(RecordCandidate {
        protocol_id: ato_computation::ProtocolId::parse(PTY_PROTOCOL_ID)
            .expect("valid static PTY protocol"),
        operation_id: ato_computation::OperationId::parse(operation)
            .expect("valid static PTY operation"),
        port_id: port_id.clone(),
        payload: encode_event(event)?,
        payload_version: 1,
        required_features: BTreeSet::new(),
        recorded_by: Some(PTY_ADAPTER_ID.to_owned()),
        stream: stream_id.to_owned(),
        local_seq,
        caused_by: Vec::new(),
        observed_at: observed_now(),
    })
}

fn observed_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string())
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    output: Arc<Mutex<VecDeque<u8>>>,
    transcript: Arc<Mutex<VecDeque<u8>>>,
    failure: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let bytes = buffer[..size].to_vec();
                    let _ = std::io::stdout().write_all(&bytes);
                    if let Ok(mut queue) = output.lock() {
                        queue.extend(bytes.iter().copied());
                    }
                    if let Ok(mut history) = transcript.lock() {
                        history.extend(bytes.iter().copied());
                        while history.len() > MAX_TRANSCRIPT_BYTES {
                            history.pop_front();
                        }
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

fn spawn_input_reader(
    writer: Arc<Mutex<ChildStdin>>,
    stylus: Arc<dyn Stylus>,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    stream_id: String,
    local_seq: Arc<AtomicU64>,
    failure: Arc<Mutex<Option<String>>>,
) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0_u8; 4096];
        while let Ok(size) = stdin.read(&mut buffer) {
            if size == 0 {
                break;
            }
            let bytes = buffer[..size].to_vec();
            let event = PtyEvent::Input {
                bytes: bytes.clone(),
            };
            if let Err(error) = stylus
                .record(
                    candidate(
                        &port_id,
                        &stream_id,
                        &event,
                        local_seq.fetch_add(1, Ordering::Relaxed) + 1,
                    )
                    .expect("PTY input is always a Record operation"),
                )
                .and_then(|_| {
                    observations.emit(
                        observation(&port_id, &event).expect("PTY event serialization cannot fail"),
                    )
                })
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
mod tests {
    use super::*;

    #[test]
    fn only_applicable_pty_events_become_record_candidates() {
        let port = ato_computation::PortId::parse("terminal.main").unwrap();
        let input = candidate(&port, "pty.main", &PtyEvent::Input { bytes: vec![1] }, 1).unwrap();
        assert_eq!(input.operation_id.as_str(), PTY_INPUT_OPERATION);
        assert!(matches!(
            candidate(&port, "pty.main", &PtyEvent::Output { bytes: vec![1] }, 2),
            Err(AdapterError::InvalidPayload(_))
        ));
        assert!(matches!(
            candidate(&port, "pty.main", &PtyEvent::Attach, 3),
            Err(AdapterError::InvalidPayload(_))
        ));
    }
}

impl PresentationCapture for PtySession {
    /// Projects the bounded final terminal screen. Presentation bytes are
    /// private evidence about the physical realization; they never enter
    /// Record payloads or Computation identity.
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

const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;

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
                    if suffix.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if character == '\r' {
            continue;
        }
        if character.is_control() && character != '\n' && character != '\t' {
            continue;
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn terminal_projection_strips_controls_and_bounds_to_the_last_screen() {
        let mut transcript = b"\x1b[31msecret-looking output\x1b[0m\r\n".to_vec();
        for index in 0..40 {
            transcript.extend_from_slice(format!("line-{index:02}\n").as_bytes());
        }
        let projection = terminal_screen_projection(&transcript).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&projection).unwrap();
        assert_eq!(value["schema"], "ato.terminal-screen@1");
        assert_eq!(value["columns"], 80);
        assert_eq!(value["rows"], 24);
        let text = value["text"].as_str().unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains("line-00"));
        assert!(text.contains("line-39"));
        assert!(text.lines().count() <= 24);
        assert!(projection.len() < 64 * 1024);
    }
}
