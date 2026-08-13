use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    CURRENT_COMPUTATION_SCHEMA_VERSION, CURRENT_SCHEMA_VERSION, CapsuleDescriptor,
    ComputationDescriptor, ConnectorId, Direction, InteractionRecord, IoRecord, PortId, PortMode,
};

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
pub enum ComputationDescriptorError {
    #[error("unsupported computation schema version {actual}; expected {expected}")]
    UnsupportedSchema { actual: u16, expected: u16 },
}

impl ComputationDescriptor {
    pub fn validate(&self) -> Result<(), ComputationDescriptorError> {
        if self.schema_version != CURRENT_COMPUTATION_SCHEMA_VERSION {
            return Err(ComputationDescriptorError::UnsupportedSchema {
                actual: self.schema_version,
                expected: CURRENT_COMPUTATION_SCHEMA_VERSION,
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

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InteractionStreamValidationError {
    #[error("interaction references undefined port `{0}`")]
    UndefinedPort(PortId),
    #[error("interaction seq {actual} is not greater than previous seq {previous}")]
    NonIncreasingSequence { previous: u64, actual: u64 },
    #[error("{direction:?} interaction is forbidden by mode {mode:?} on port `{port}`")]
    DirectionForbidden {
        port: PortId,
        mode: PortMode,
        direction: Direction,
    },
}

/// Incrementally validates native computation interactions without retaining
/// payloads or confusing a semantic Port with its runtime binding.
pub struct InteractionStreamValidator {
    ports: std::collections::BTreeMap<PortId, PortMode>,
    previous_seq: Option<u64>,
}

impl InteractionStreamValidator {
    pub fn new(descriptor: &ComputationDescriptor) -> Result<Self, ComputationDescriptorError> {
        descriptor.validate()?;
        Ok(Self {
            ports: descriptor
                .ports
                .iter()
                .map(|(id, definition)| (id.clone(), definition.mode))
                .collect(),
            previous_seq: None,
        })
    }

    pub fn accept(
        &mut self,
        record: &InteractionRecord,
    ) -> Result<(), InteractionStreamValidationError> {
        let mode =
            self.ports.get(&record.port).copied().ok_or_else(|| {
                InteractionStreamValidationError::UndefinedPort(record.port.clone())
            })?;
        let permitted = matches!(
            (mode, record.direction),
            (PortMode::Duplex, _)
                | (PortMode::IngressOnly, Direction::Ingress)
                | (PortMode::EgressOnly, Direction::Egress)
        );
        if !permitted {
            return Err(InteractionStreamValidationError::DirectionForbidden {
                port: record.port.clone(),
                mode,
                direction: record.direction,
            });
        }
        if let Some(previous) = self.previous_seq
            && record.seq <= previous
        {
            return Err(InteractionStreamValidationError::NonIncreasingSequence {
                previous,
                actual: record.seq,
            });
        }
        self.previous_seq = Some(record.seq);
        Ok(())
    }
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

    fn computation_descriptor(mode: PortMode) -> ComputationDescriptor {
        ComputationDescriptor {
            schema_version: CURRENT_COMPUTATION_SCHEMA_VERSION,
            root: ComputationRef {
                computation_type: ComputationTypeId::parse("example.computation.greeter@1")
                    .unwrap(),
                computation_ref: ContentRef::parse(format!("blake3:{}", "11".repeat(32))).unwrap(),
            },
            ports: BTreeMap::from([(
                PortId::parse("greeter.name").unwrap(),
                PortDef {
                    protocol: ProtocolId::parse("example.greeter.text@1").unwrap(),
                    mode,
                    config_ref: None,
                },
            )]),
            trace_from: None,
        }
    }

    fn interaction(seq: u64, direction: Direction) -> InteractionRecord {
        InteractionRecord {
            seq,
            offset_ns: None,
            observed_at_unix_ns: None,
            port: PortId::parse("greeter.name").unwrap(),
            direction,
            kind: InteractionKindId::parse("text").unwrap(),
            payload: InteractionPayload::Inline(b"Alice".to_vec()),
        }
    }

    #[test]
    fn validates_port_mode_and_order() {
        let mut validator =
            InteractionStreamValidator::new(&computation_descriptor(PortMode::IngressOnly))
                .unwrap();
        validator
            .accept(&interaction(4, Direction::Ingress))
            .unwrap();
        assert!(matches!(
            validator.accept(&interaction(5, Direction::Egress)),
            Err(InteractionStreamValidationError::DirectionForbidden { .. })
        ));

        let mut validator =
            InteractionStreamValidator::new(&computation_descriptor(PortMode::Duplex)).unwrap();
        validator
            .accept(&interaction(4, Direction::Ingress))
            .unwrap();
        assert!(matches!(
            validator.accept(&interaction(4, Direction::Egress)),
            Err(InteractionStreamValidationError::NonIncreasingSequence { .. })
        ));
    }
}
