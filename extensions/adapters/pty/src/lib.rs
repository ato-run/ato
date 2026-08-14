//! Byte-level PTY evidence. It deliberately does not infer shell commands.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, CaptureGate,
};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const PTY_ADAPTER_ID: &str = "ato.pty@1";
pub const PTY_PROTOCOL_ID: &str = "ato.pty@1";

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
        let failure = Arc::new(Mutex::new(None));
        let capture_gate = Arc::new(CaptureGate::default());
        let port_id = ato_computation::PortId::parse(format!("terminal.{}", instance.instance_id))
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
                Arc::clone(&failure),
                Arc::clone(&capture_gate),
            ),
            spawn_output_reader(
                child.stderr.take().expect("piped stderr"),
                Arc::clone(&context.observations),
                port_id.clone(),
                Arc::clone(&output),
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
    })
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    observations: Arc<dyn ato_adapter_api::ObservationSink>,
    port_id: ato_computation::PortId,
    output: Arc<Mutex<VecDeque<u8>>>,
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
