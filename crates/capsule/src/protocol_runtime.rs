//! Runtime bridge for historical Capsule Protocol replay.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use capsule_protocol::{
    CapsuleDescriptor, ConnectorId, Direction, IoRecord, Payload, RecordKindId,
};
use capsule_session_runtime::{HistoricalReplayVerdict, RecordFrontier};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rand::RngCore;
use thiserror::Error;

const PTY_CONNECTOR_ID: &str = "terminal.main";
const PTY_PROTOCOL_ID: &str = "ato.io.pty@1";
const MARKER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ProtocolRuntimeError {
    #[error("runtime I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("PTY failed: {0}")]
    Pty(String),
    #[error("unsupported connector record: {0}")]
    UnsupportedRecord(String),
    #[error("state runtime failed: {0}")]
    State(String),
    #[error("replay diverged at seq {seq}")]
    Diverged { seq: u64 },
    #[error("replay timed out waiting for PTY completion marker")]
    Timeout,
    #[error("replay recovery frontier {from:?} is after target {through:?}")]
    InvalidReplayRange {
        from: RecordFrontier,
        through: RecordFrontier,
    },
}

pub trait StateRuntime {
    type RestoredState;

    fn restore(
        &self,
        state: &capsule_protocol::StateRef,
    ) -> Result<Self::RestoredState, ProtocolRuntimeError>;
}

pub struct WorkspaceStateRuntime<'a> {
    pub object: &'a [u8],
    pub destination: &'a Path,
}

impl StateRuntime for WorkspaceStateRuntime<'_> {
    type RestoredState = std::path::PathBuf;

    fn restore(
        &self,
        state: &capsule_protocol::StateRef,
    ) -> Result<Self::RestoredState, ProtocolRuntimeError> {
        crate::protocol_bundle::restore_workspace_state(state, self.object, self.destination)
            .map_err(|error| ProtocolRuntimeError::State(error.to_string()))?;
        Ok(self.destination.to_path_buf())
    }
}

pub trait ConnectorRuntime {
    fn inject(&mut self, record: &IoRecord) -> Result<(), ProtocolRuntimeError>;
    fn observe(&mut self, expected: &IoRecord) -> Result<(), ProtocolRuntimeError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub records_processed: usize,
    pub historical_verdict: HistoricalReplayVerdict,
}

pub struct ReplayEngine;

impl ReplayEngine {
    pub fn replay(
        descriptor: &CapsuleDescriptor,
        records: &[IoRecord],
        connector: &mut impl ConnectorRuntime,
    ) -> Result<ReplayOutcome, ProtocolRuntimeError> {
        let through = records.last().map_or(RecordFrontier::Origin, |record| {
            RecordFrontier::Through(record.seq)
        });
        Self::replay_between(
            descriptor,
            records,
            RecordFrontier::Origin,
            through,
            connector,
        )
    }

    /// Replays exactly the records in `(from, through]`.
    ///
    /// `from` is the common recovery frontier already represented by State and
    /// every Connector. Excluding it prevents checkpointed effects from being
    /// applied twice.
    pub fn replay_between(
        descriptor: &CapsuleDescriptor,
        records: &[IoRecord],
        from: RecordFrontier,
        through: RecordFrontier,
        connector: &mut impl ConnectorRuntime,
    ) -> Result<ReplayOutcome, ProtocolRuntimeError> {
        if from > through {
            return Err(ProtocolRuntimeError::InvalidReplayRange { from, through });
        }
        let mut validator = capsule_protocol::StreamValidator::new(descriptor)
            .map_err(|error| ProtocolRuntimeError::UnsupportedRecord(error.to_string()))?;
        let mut records_processed = 0;
        for record in records
            .iter()
            .filter(|record| from.replay_contains(through, record.seq))
        {
            validator
                .accept(record)
                .map_err(|error| ProtocolRuntimeError::UnsupportedRecord(error.to_string()))?;
            match record.direction {
                Direction::Ingress => connector.inject(record)?,
                Direction::Egress => connector.observe(record)?,
            }
            records_processed += 1;
        }
        Ok(ReplayOutcome {
            records_processed,
            historical_verdict: HistoricalReplayVerdict::Verified { from, through },
        })
    }
}

pub fn pty_descriptor_connector() -> (ConnectorId, capsule_protocol::ConnectorDef) {
    (
        ConnectorId::parse(PTY_CONNECTOR_ID).expect("static connector id"),
        capsule_protocol::ConnectorDef {
            protocol: capsule_protocol::ProtocolId::parse(PTY_PROTOCOL_ID)
                .expect("static protocol id"),
            config_ref: None,
        },
    )
}

pub fn record_pty_command(
    workspace: &Path,
    argv: &[String],
) -> Result<Vec<IoRecord>, ProtocolRuntimeError> {
    if argv.is_empty() {
        return Err(ProtocolRuntimeError::UnsupportedRecord(
            "capture command must not be empty".to_owned(),
        ));
    }
    let command = shell_join(argv);
    let started = Instant::now();
    let wall_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok());
    let mut session = PtyConnector::open(workspace)?;
    let ingress = IoRecord {
        seq: 1,
        offset_ns: Some(0),
        observed_at_unix_ns: wall_time,
        connector: ConnectorId::parse(PTY_CONNECTOR_ID).expect("static connector id"),
        direction: Direction::Ingress,
        kind: RecordKindId::parse("stdin").expect("static record kind"),
        payload: Payload::Inline(format!("{command}\n").into_bytes()),
    };
    session.inject(&ingress)?;
    let output = session.take_pending_output()?;
    std::io::stdout().write_all(&output)?;
    std::io::stdout().flush()?;
    session.shutdown()?;
    Ok(vec![
        ingress,
        IoRecord {
            seq: 2,
            offset_ns: Some(started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)),
            observed_at_unix_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_nanos()).ok()),
            connector: ConnectorId::parse(PTY_CONNECTOR_ID).expect("static connector id"),
            direction: Direction::Egress,
            kind: RecordKindId::parse("output").expect("static record kind"),
            payload: Payload::Inline(output),
        },
    ])
}

pub struct PtyConnector {
    master: Option<Box<dyn MasterPty + Send>>,
    output_receiver: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    pending_output: Option<Vec<u8>>,
}

impl PtyConnector {
    pub fn open(workspace: &Path) -> Result<Self, ProtocolRuntimeError> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| ProtocolRuntimeError::Pty(error.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| ProtocolRuntimeError::Pty(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| ProtocolRuntimeError::Pty(error.to_string()))?;
        let mut command = shell_command();
        command.cwd(workspace);
        command.env("TERM", "dumb");
        command.env("NO_COLOR", "1");
        command.env("CLICOLOR", "0");
        command.env("PS1", "");
        command.env("ENV", "");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| ProtocolRuntimeError::Pty(error.to_string()))?;
        drop(pair.slave);
        let (sender, output_receiver) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        if sender.send(Ok(chunk[..count].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        let mut connector = Self {
            master: Some(pair.master),
            output_receiver,
            reader_thread: Some(reader_thread),
            writer,
            child,
            pending_output: None,
        };
        connector.initialize()?;
        Ok(connector)
    }

    fn initialize(&mut self) -> Result<(), ProtocolRuntimeError> {
        const READY_MARKER: &[u8] = b"__ATO_PROTOCOL_READY__";
        // Disable terminal echo before recording. Otherwise the shell echoes
        // the completion command (including its random marker), making an
        // otherwise deterministic egress differ between producer and consumer.
        self.writer.write_all(initialization_command())?;
        self.writer.flush()?;
        let _ = read_until_marker(&self.output_receiver, READY_MARKER)?;
        Ok(())
    }

    pub fn read_command_output(&mut self) -> Result<Vec<u8>, ProtocolRuntimeError> {
        let marker = random_completion_marker();
        let command = completion_command(&marker);
        self.writer.write_all(command.as_bytes())?;
        self.writer.flush()?;
        let output = read_until_marker(&self.output_receiver, marker.as_bytes())?;
        Ok(output)
    }

    fn take_pending_output(&mut self) -> Result<Vec<u8>, ProtocolRuntimeError> {
        self.pending_output.take().ok_or_else(|| {
            ProtocolRuntimeError::UnsupportedRecord("ingress produced no pending output".to_owned())
        })
    }

    pub fn continue_interactive(mut self) -> Result<(), ProtocolRuntimeError> {
        let receiver = self.output_receiver;
        let output_thread = std::thread::spawn(move || {
            let mut stdout = std::io::stdout().lock();
            while let Ok(chunk) = receiver.recv() {
                stdout.write_all(&chunk?)?;
                stdout.flush()?;
            }
            Ok::<(), std::io::Error>(())
        });
        std::io::copy(&mut std::io::stdin().lock(), &mut self.writer)?;
        self.writer.flush()?;
        let _ = self.child.wait();
        drop(self.master.take());
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
        output_thread
            .join()
            .map_err(|_| ProtocolRuntimeError::Pty("PTY output thread panicked".to_owned()))?
            .map_err(ProtocolRuntimeError::Io)?;
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), ProtocolRuntimeError> {
        self.writer.write_all(b"exit\n")?;
        self.writer.flush()?;
        let _ = self.child.wait();
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
        Ok(())
    }
}

impl ConnectorRuntime for PtyConnector {
    fn inject(&mut self, record: &IoRecord) -> Result<(), ProtocolRuntimeError> {
        if record.connector.as_str() != PTY_CONNECTOR_ID
            || record.kind.as_str() != "stdin"
            || record.direction != Direction::Ingress
        {
            return Err(ProtocolRuntimeError::UnsupportedRecord(format!(
                "seq {} is not PTY stdin ingress",
                record.seq
            )));
        }
        let Payload::Inline(bytes) = &record.payload else {
            return Err(ProtocolRuntimeError::UnsupportedRecord(
                "PTY stdin must be inline".to_owned(),
            ));
        };
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        self.pending_output = Some(self.read_command_output()?);
        Ok(())
    }

    fn observe(&mut self, expected: &IoRecord) -> Result<(), ProtocolRuntimeError> {
        if expected.connector.as_str() != PTY_CONNECTOR_ID
            || expected.kind.as_str() != "output"
            || expected.direction != Direction::Egress
        {
            return Err(ProtocolRuntimeError::UnsupportedRecord(format!(
                "seq {} is not PTY output egress",
                expected.seq
            )));
        }
        let Payload::Inline(expected_bytes) = &expected.payload else {
            return Err(ProtocolRuntimeError::UnsupportedRecord(
                "PTY output must be inline".to_owned(),
            ));
        };
        let actual = self.pending_output.take().ok_or_else(|| {
            ProtocolRuntimeError::UnsupportedRecord("egress had no preceding ingress".to_owned())
        })?;
        if &actual != expected_bytes {
            return Err(ProtocolRuntimeError::Diverged { seq: expected.seq });
        }
        std::io::stdout().write_all(&actual)?;
        std::io::stdout().flush()?;
        Ok(())
    }
}

fn read_until_marker(
    receiver: &mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    marker: &[u8],
) -> Result<Vec<u8>, ProtocolRuntimeError> {
    let deadline = Instant::now() + MARKER_TIMEOUT;
    let mut collected = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(Ok(chunk)) => {
                collected.extend_from_slice(&chunk);
                if find_subslice(&collected, marker).is_some() {
                    return Ok(trim_at_marker(collected, marker));
                }
            }
            Ok(Err(error)) => return Err(ProtocolRuntimeError::Io(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(ProtocolRuntimeError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ProtocolRuntimeError::Pty(
                    "PTY reader disconnected before completion marker".to_owned(),
                ));
            }
        }
    }
}

fn trim_at_marker(mut bytes: Vec<u8>, marker: &[u8]) -> Vec<u8> {
    if let Some(index) = find_subslice(&bytes, marker) {
        bytes.truncate(index);
    }
    bytes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn random_completion_marker() -> String {
    let mut nonce = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    completion_marker(nonce)
}

fn completion_marker(nonce: [u8; 16]) -> String {
    format!("__ATO_PROTOCOL_DONE_{}__", hex::encode(nonce))
}

#[cfg(unix)]
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            let escaped = argument
                .replace('^', "^^")
                .replace('&', "^&")
                .replace('|', "^|")
                .replace('<', "^<")
                .replace('>', "^>")
                .replace('(', "^(")
                .replace(')', "^)")
                .replace('%', "^%")
                .replace('!', "^^!")
                .replace('"', "\\\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(unix)]
fn initialization_command() -> &'static [u8] {
    b"stty -echo\nprintf '__ATO_PROTOCOL_%s__\\n' 'READY'\n"
}

#[cfg(windows)]
fn initialization_command() -> &'static [u8] {
    b"@echo off\r\necho __ATO_PROTOCOL_READY__\r\n"
}

#[cfg(unix)]
fn completion_command(marker: &str) -> String {
    format!("printf '\\n%s\\n' '{marker}'\n")
}

#[cfg(windows)]
fn completion_command(marker: &str) -> String {
    format!("echo {marker}\r\n")
}

#[cfg(unix)]
fn shell_command() -> CommandBuilder {
    CommandBuilder::new("/bin/sh")
}

#[cfg(windows)]
fn shell_command() -> CommandBuilder {
    CommandBuilder::new("cmd.exe")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use capsule_protocol::{ConnectorDef, ProtocolId, StateRef, StateTypeId};

    struct EchoConnector {
        pending: Option<Vec<u8>>,
        injected: usize,
        observed: usize,
    }

    impl ConnectorRuntime for EchoConnector {
        fn inject(&mut self, record: &IoRecord) -> Result<(), ProtocolRuntimeError> {
            let Payload::Inline(bytes) = &record.payload else {
                unreachable!()
            };
            self.pending = Some(bytes.iter().map(u8::to_ascii_uppercase).collect());
            self.injected += 1;
            Ok(())
        }

        fn observe(&mut self, expected: &IoRecord) -> Result<(), ProtocolRuntimeError> {
            let Payload::Inline(expected) = &expected.payload else {
                unreachable!()
            };
            if self.pending.take().as_deref() != Some(expected) {
                return Err(ProtocolRuntimeError::Diverged {
                    seq: expected.len() as u64,
                });
            }
            self.observed += 1;
            Ok(())
        }
    }

    fn record(seq: u64, direction: Direction, payload: &[u8]) -> IoRecord {
        IoRecord {
            seq,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("test.echo").unwrap(),
            direction,
            kind: RecordKindId::parse("data").unwrap(),
            payload: Payload::Inline(payload.to_vec()),
        }
    }

    #[test]
    fn replay_injects_ingress_and_observes_egress_on_distinct_paths() {
        let descriptor = CapsuleDescriptor {
            schema_version: 1,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.test@1").unwrap(),
                state_ref: capsule_protocol::ContentRef::parse(format!(
                    "blake3:{}",
                    "00".repeat(32)
                ))
                .unwrap(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("test.echo").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.test.echo@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        let records = vec![
            record(5, Direction::Ingress, b"hello"),
            record(8, Direction::Egress, b"HELLO"),
        ];
        let mut connector = EchoConnector {
            pending: None,
            injected: 0,
            observed: 0,
        };
        let outcome = ReplayEngine::replay(&descriptor, &records, &mut connector).unwrap();
        assert_eq!(outcome.records_processed, 2);
        assert_eq!(
            outcome.historical_verdict,
            HistoricalReplayVerdict::Verified {
                from: RecordFrontier::Origin,
                through: RecordFrontier::Through(8),
            }
        );
        assert_eq!(connector.injected, 1);
        assert_eq!(connector.observed, 1);
    }

    #[test]
    fn replay_from_checkpoint_excludes_records_already_present_in_state() {
        let descriptor = CapsuleDescriptor {
            schema_version: 1,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.test@1").unwrap(),
                state_ref: capsule_protocol::ContentRef::parse(format!(
                    "blake3:{}",
                    "00".repeat(32)
                ))
                .unwrap(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("test.echo").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.test.echo@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        let records = vec![
            record(5, Direction::Ingress, b"already-applied"),
            record(8, Direction::Egress, b"ALREADY-APPLIED"),
            record(10, Direction::Ingress, b"new"),
            record(12, Direction::Egress, b"NEW"),
        ];
        let mut connector = EchoConnector {
            pending: None,
            injected: 0,
            observed: 0,
        };

        let outcome = ReplayEngine::replay_between(
            &descriptor,
            &records,
            RecordFrontier::Through(8),
            RecordFrontier::Through(12),
            &mut connector,
        )
        .unwrap();

        assert_eq!(outcome.records_processed, 2);
        assert_eq!(connector.injected, 1);
        assert_eq!(connector.observed, 1);
    }

    #[test]
    fn empty_record_stream_is_a_zero_step_replay() {
        let descriptor = CapsuleDescriptor {
            schema_version: 1,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.test@1").unwrap(),
                state_ref: capsule_protocol::ContentRef::parse(format!(
                    "blake3:{}",
                    "00".repeat(32)
                ))
                .unwrap(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("test.echo").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.test.echo@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        let mut connector = EchoConnector {
            pending: None,
            injected: 0,
            observed: 0,
        };

        let outcome = ReplayEngine::replay(&descriptor, &[], &mut connector).unwrap();

        assert_eq!(outcome.records_processed, 0);
        assert_eq!(
            outcome.historical_verdict,
            HistoricalReplayVerdict::Verified {
                from: RecordFrontier::Origin,
                through: RecordFrontier::Origin,
            }
        );
        assert_eq!(connector.injected, 0);
        assert_eq!(connector.observed, 0);
    }

    #[test]
    fn completion_marker_uses_the_full_128_bit_nonce() {
        let marker = completion_marker([0xab; 16]);
        assert_eq!(
            marker,
            "__ATO_PROTOCOL_DONE_abababababababababababababababab__"
        );
        assert_ne!(marker, completion_marker([0xac; 16]));
    }
}
