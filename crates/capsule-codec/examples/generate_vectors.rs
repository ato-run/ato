use std::collections::BTreeMap;

use capsule_codec::{encode_descriptor, encode_record_stream};
use capsule_protocol::{
    CapsuleDescriptor, ConnectorDef, ConnectorId, ContentRef, Direction, IoRecord, Payload,
    ProtocolId, RecordKindId, StateRef, StateTypeId,
};

fn main() {
    let descriptor = CapsuleDescriptor {
        schema_version: 1,
        base_state: StateRef {
            state_type: StateTypeId::parse("ato.state.workspace-posix-host@1").unwrap(),
            state_ref: ContentRef::parse(format!("blake3:{}", "12".repeat(32))).unwrap(),
        },
        connectors: BTreeMap::from([(
            ConnectorId::parse("terminal.main").unwrap(),
            ConnectorDef {
                protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                config_ref: None,
            },
        )]),
    };
    let records = vec![
        IoRecord {
            seq: 10,
            offset_ns: Some(0),
            observed_at_unix_ns: Some(100),
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Inline(b"echo hi\n".to_vec()),
        },
        IoRecord {
            seq: 12,
            offset_ns: Some(9),
            observed_at_unix_ns: Some(90),
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Egress,
            kind: RecordKindId::parse("output").unwrap(),
            payload: Payload::Inline(b"hi\r\n".to_vec()),
        },
    ];

    println!(
        "descriptor-v1 {}",
        hex::encode(encode_descriptor(&descriptor).unwrap())
    );
    println!(
        "records-pty-v1 {}",
        hex::encode(encode_record_stream(&descriptor, &records).unwrap())
    );
}
