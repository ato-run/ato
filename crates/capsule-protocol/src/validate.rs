use std::collections::BTreeSet;

use thiserror::Error;

use crate::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorId, IoRecord};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DescriptorError {
    #[error("unsupported Capsule Protocol schema version {actual}; expected {expected}")]
    UnsupportedSchema { actual: u16, expected: u16 },
}

impl CapsuleDescriptor {
    pub fn validate(&self) -> Result<(), DescriptorError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(DescriptorError::UnsupportedSchema {
                actual: self.schema_version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StreamValidationError {
    #[error("record references undefined connector `{0}`")]
    UndefinedConnector(ConnectorId),
    #[error("record seq {actual} is not greater than previous seq {previous}")]
    NonIncreasingSequence { previous: u64, actual: u64 },
}

/// Incrementally validates one record at a time without retaining payloads.
pub struct StreamValidator {
    connectors: BTreeSet<ConnectorId>,
    previous_seq: Option<u64>,
}

impl StreamValidator {
    pub fn new(descriptor: &CapsuleDescriptor) -> Result<Self, DescriptorError> {
        descriptor.validate()?;
        Ok(Self {
            connectors: descriptor.connectors.keys().cloned().collect(),
            previous_seq: None,
        })
    }

    pub fn accept(&mut self, record: &IoRecord) -> Result<(), StreamValidationError> {
        if !self.connectors.contains(&record.connector) {
            return Err(StreamValidationError::UndefinedConnector(
                record.connector.clone(),
            ));
        }
        if let Some(previous) = self.previous_seq
            && record.seq <= previous
        {
            return Err(StreamValidationError::NonIncreasingSequence {
                previous,
                actual: record.seq,
            });
        }
        self.previous_seq = Some(record.seq);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::*;

    fn descriptor() -> CapsuleDescriptor {
        CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.workspace-posix@1").unwrap(),
                state_ref: ContentRef::parse(format!("blake3:{}", "00".repeat(32))).unwrap(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("terminal.main").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                    config_ref: None,
                },
            )]),
        }
    }

    fn record(seq: u64, timestamp: Option<i64>) -> IoRecord {
        IoRecord {
            seq,
            offset_ns: None,
            observed_at_unix_ns: timestamp,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Inline(b"hello".to_vec()),
        }
    }

    #[test]
    fn accepts_empty_stream_arbitrary_first_seq_gaps_and_clock_regression() {
        let mut validator = StreamValidator::new(&descriptor()).unwrap();
        validator.accept(&record(100, Some(1_000))).unwrap();
        validator.accept(&record(102, Some(900))).unwrap();
    }

    #[test]
    fn rejects_duplicate_and_decreasing_seq() {
        let mut validator = StreamValidator::new(&descriptor()).unwrap();
        validator.accept(&record(100, None)).unwrap();
        assert!(matches!(
            validator.accept(&record(100, None)),
            Err(StreamValidationError::NonIncreasingSequence { .. })
        ));

        let mut validator = StreamValidator::new(&descriptor()).unwrap();
        validator.accept(&record(100, None)).unwrap();
        assert!(matches!(
            validator.accept(&record(99, None)),
            Err(StreamValidationError::NonIncreasingSequence { .. })
        ));
    }

    #[test]
    fn rejects_undefined_connector() {
        let mut record = record(1, None);
        record.connector = ConnectorId::parse("terminal.other").unwrap();
        let mut validator = StreamValidator::new(&descriptor()).unwrap();
        assert!(matches!(
            validator.accept(&record),
            Err(StreamValidationError::UndefinedConnector(_))
        ));
    }
}
