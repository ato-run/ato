//! Cross-job acceptance fixture for a closed, Ato-managed State runtime.

use std::collections::BTreeMap;
use std::path::Path;

use capsule::protocol_bundle::{PortableCapsule, content_ref};
use capsule::protocol_runtime::{
    ConnectorRuntime, ProtocolRuntimeError, ReplayEngine, ReplayOutcome,
};
use capsule_protocol::{
    CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef, ConnectorId, Direction, IoRecord,
    Payload, ProtocolId, RecordKindId, StateRef, StateTypeId,
};

const STATE_BYTES: &[u8] = b"ato-managed-uppercase-machine-v1\n";
const STATE_TYPE: &str = "ato.state.fixture-machine@1";
const CONNECTOR_ID: &str = "fixture.bytes";

struct UppercaseMachine {
    pending: Option<Vec<u8>>,
}

impl UppercaseMachine {
    fn restored(state: &StateRef, object: &[u8]) -> Result<Self, ProtocolRuntimeError> {
        if state.state_type.as_str() != STATE_TYPE || object != STATE_BYTES {
            return Err(ProtocolRuntimeError::State(
                "fixture State runtime contract mismatch".to_owned(),
            ));
        }
        Ok(Self { pending: None })
    }

    fn continue_with(&mut self, ingress: &[u8]) -> Vec<u8> {
        ingress.iter().map(u8::to_ascii_uppercase).collect()
    }
}

impl ConnectorRuntime for UppercaseMachine {
    fn inject(&mut self, record: &IoRecord) -> Result<(), ProtocolRuntimeError> {
        let Payload::Inline(bytes) = &record.payload else {
            return Err(ProtocolRuntimeError::UnsupportedRecord(
                "fixture ingress must be inline".to_owned(),
            ));
        };
        self.pending = Some(self.continue_with(bytes));
        Ok(())
    }

    fn observe(&mut self, expected: &IoRecord) -> Result<(), ProtocolRuntimeError> {
        let seq = expected.seq;
        let Payload::Inline(expected_bytes) = &expected.payload else {
            return Err(ProtocolRuntimeError::UnsupportedRecord(
                "fixture egress must be inline".to_owned(),
            ));
        };
        if self.pending.take().as_deref() != Some(expected_bytes) {
            return Err(ProtocolRuntimeError::Diverged { seq });
        }
        Ok(())
    }
}

fn record(seq: u64, direction: Direction, bytes: &[u8]) -> IoRecord {
    IoRecord {
        seq,
        offset_ns: None,
        observed_at_unix_ns: None,
        connector: ConnectorId::parse(CONNECTOR_ID).expect("fixture connector id"),
        direction,
        kind: RecordKindId::parse("bytes").expect("fixture record kind"),
        payload: Payload::Inline(bytes.to_vec()),
    }
}

fn produce(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let state_ref = content_ref(STATE_BYTES);
    PortableCapsule {
        descriptor: CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: StateRef {
                state_type: StateTypeId::parse(STATE_TYPE)?,
                state_ref: state_ref.clone(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse(CONNECTOR_ID)?,
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.fixture-bytes@1")?,
                    config_ref: None,
                },
            )]),
        },
        records: vec![
            record(41, Direction::Ingress, b"historical"),
            record(43, Direction::Egress, b"HISTORICAL"),
        ],
        objects: BTreeMap::from([(state_ref, STATE_BYTES.to_vec())]),
    }
    .write(path)?;
    println!("produced {}", path.display());
    Ok(())
}

fn consume(path: &Path) -> Result<ReplayOutcome, Box<dyn std::error::Error>> {
    let bundle = PortableCapsule::read(path)?;
    let object = bundle
        .objects
        .get(&bundle.descriptor.base_state.state_ref)
        .ok_or("validated State object missing")?;
    let mut machine = UppercaseMachine::restored(&bundle.descriptor.base_state, object)?;
    let outcome = ReplayEngine::replay(&bundle.descriptor, &bundle.records, &mut machine)?;
    let continued = machine.continue_with(b"continue");
    if continued != b"CONTINUE" {
        return Err("continued computation returned unexpected egress".into());
    }
    println!(
        "consumed {} records; replay=HISTORICAL; continue={}",
        outcome.records_processed,
        String::from_utf8(continued)?
    );
    Ok(outcome)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: protocol_e2e_fixture <produce|consume> <bundle>")?;
    let path = args.next().ok_or("bundle path is required")?;
    if args.next().is_some() {
        return Err("unexpected trailing argument".into());
    }
    match mode.to_str() {
        Some("produce") => produce(Path::new(&path)),
        Some("consume") => consume(Path::new(&path)).map(|_| ()),
        _ => Err("mode must be `produce` or `consume`".into()),
    }
}
