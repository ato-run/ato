//! Explicit portable encoding for [`capsule_protocol`] domain values.
//!
//! Wire DTOs are private and converted field-by-field. Domain types therefore
//! cannot accidentally acquire an implicit JSON or CBOR compatibility surface.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use capsule_protocol::{
    CapsuleDescriptor, ConnectorDef, ConnectorId, ContentRef, Direction, IoRecord, Payload,
    ProtocolId, RecordKindId, StateRef, StateTypeId, StreamValidator,
};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use thiserror::Error;

pub const WIRE_VERSION: u16 = 1;
pub const MAX_INLINE_PAYLOAD: usize = 1024 * 1024;
pub const MAX_RECORDS: usize = 1_000_000;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("CBOR encode failed: {0}")]
    Encode(String),
    #[error("CBOR decode failed: {0}")]
    Decode(String),
    #[error("unsupported wire version {0}")]
    UnsupportedWireVersion(u16),
    #[error("invalid Capsule Protocol value: {0}")]
    InvalidValue(String),
    #[error("inline payload is {actual} bytes; maximum is {maximum}")]
    InlinePayloadTooLarge { actual: usize, maximum: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordStreamStats {
    pub record_count: usize,
    pub encoded_bytes: u64,
}

pub struct RecordStreamEncoder<W> {
    validator: StreamValidator,
    writer: W,
    count: usize,
    encoded_bytes: u64,
}

impl<W: Write> RecordStreamEncoder<W> {
    pub fn new(descriptor: &CapsuleDescriptor, writer: W) -> Result<Self, CodecError> {
        Ok(Self {
            validator: StreamValidator::new(descriptor)
                .map_err(|error| CodecError::InvalidValue(error.to_string()))?,
            writer,
            count: 0,
            encoded_bytes: 0,
        })
    }

    pub fn push(&mut self, record: &IoRecord) -> Result<(), CodecError> {
        if self.count >= MAX_RECORDS {
            return Err(record_count_error());
        }
        self.validator
            .accept(record)
            .map_err(|error| CodecError::InvalidValue(error.to_string()))?;
        validate_payload(&record.payload)?;
        let mut writer = CountingWriter {
            inner: &mut self.writer,
            written: &mut self.encoded_bytes,
        };
        ciborium::ser::into_writer(&WireIoRecordV1::from(record), &mut writer)
            .map_err(|error| CodecError::Encode(error.to_string()))?;
        self.count += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<RecordStreamStats, CodecError> {
        Ok(RecordStreamStats {
            record_count: self.count,
            encoded_bytes: self.encoded_bytes,
        })
    }
}

pub struct RecordStreamDecoder<R> {
    validator: StreamValidator,
    reader: R,
    remaining_bytes: u64,
    count: usize,
    initial_bytes: u64,
}

impl<R: Read> RecordStreamDecoder<R> {
    pub fn new(
        descriptor: &CapsuleDescriptor,
        reader: R,
        encoded_bytes: u64,
    ) -> Result<Self, CodecError> {
        Ok(Self {
            validator: StreamValidator::new(descriptor)
                .map_err(|error| CodecError::InvalidValue(error.to_string()))?,
            reader,
            remaining_bytes: encoded_bytes,
            count: 0,
            initial_bytes: encoded_bytes,
        })
    }

    pub fn next_record(&mut self) -> Result<Option<IoRecord>, CodecError> {
        if self.remaining_bytes == 0 {
            return Ok(None);
        }
        if self.count >= MAX_RECORDS {
            return Err(record_count_error());
        }
        let mut reader = RemainingReader {
            inner: &mut self.reader,
            remaining: &mut self.remaining_bytes,
        };
        let wire: WireIoRecordV1 = ciborium::de::from_reader(&mut reader)
            .map_err(|error| CodecError::Decode(error.to_string()))?;
        let record = IoRecord::try_from(wire)?;
        self.validator
            .accept(&record)
            .map_err(|error| CodecError::InvalidValue(error.to_string()))?;
        validate_payload(&record.payload)?;
        self.count += 1;
        Ok(Some(record))
    }

    pub fn stats(&self) -> RecordStreamStats {
        RecordStreamStats {
            record_count: self.count,
            encoded_bytes: self.initial_bytes - self.remaining_bytes,
        }
    }
}

struct CountingWriter<'a, W> {
    inner: &'a mut W,
    written: &'a mut u64,
}

impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(bytes)?;
        *self.written = self
            .written
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("record stream size overflow"))?;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct RemainingReader<'a, R> {
    inner: &'a mut R,
    remaining: &'a mut u64,
}

impl<R: Read> Read for RemainingReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let limit = usize::try_from((*self.remaining).min(bytes.len() as u64))
            .map_err(|_| std::io::Error::other("record stream size does not fit usize"))?;
        if limit == 0 {
            return Ok(0);
        }
        let count = self.inner.read(&mut bytes[..limit])?;
        *self.remaining -= count as u64;
        Ok(count)
    }
}

// Arrays, not CBOR maps: field order and integer representation are explicit.
#[derive(Serialize, Deserialize)]
struct WireDescriptorV1(u16, u16, WireStateRef, Vec<(String, WireConnectorDef)>);

#[derive(Serialize, Deserialize)]
struct WireStateRef(String, String);

#[derive(Serialize, Deserialize)]
struct WireConnectorDef(String, Option<String>);

#[derive(Serialize, Deserialize)]
struct WireIoRecordV1(
    u16,
    u64,
    Option<u64>,
    Option<i64>,
    String,
    u8,
    String,
    WirePayload,
);

#[derive(Serialize, Deserialize)]
struct WirePayload(u8, ByteBuf, Option<String>);

pub fn encode_descriptor(descriptor: &CapsuleDescriptor) -> Result<Vec<u8>, CodecError> {
    descriptor
        .validate()
        .map_err(|error| CodecError::InvalidValue(error.to_string()))?;
    let wire = WireDescriptorV1::from(descriptor);
    encode_item(&wire)
}

pub fn decode_descriptor(bytes: &[u8]) -> Result<CapsuleDescriptor, CodecError> {
    decode_descriptor_reader(Cursor::new(bytes), bytes.len() as u64)
}

pub fn decode_descriptor_reader<R: Read>(
    reader: R,
    encoded_bytes: u64,
) -> Result<CapsuleDescriptor, CodecError> {
    let mut reader = reader.take(encoded_bytes);
    let wire: WireDescriptorV1 = ciborium::de::from_reader(&mut reader)
        .map_err(|error| CodecError::Decode(error.to_string()))?;
    if reader.limit() != 0 {
        return Err(CodecError::Decode(
            "trailing bytes after CBOR item".to_owned(),
        ));
    }
    CapsuleDescriptor::try_from(wire)
}

pub fn encode_record_stream(
    descriptor: &CapsuleDescriptor,
    records: &[IoRecord],
) -> Result<Vec<u8>, CodecError> {
    let mut output = Vec::new();
    {
        let mut encoder = RecordStreamEncoder::new(descriptor, &mut output)?;
        for record in records {
            encoder.push(record)?;
        }
        encoder.finish()?;
    }
    Ok(output)
}

pub fn decode_record_stream(
    descriptor: &CapsuleDescriptor,
    bytes: &[u8],
) -> Result<Vec<IoRecord>, CodecError> {
    let mut decoder = RecordStreamDecoder::new(descriptor, Cursor::new(bytes), bytes.len() as u64)?;
    let mut records = Vec::new();
    while let Some(record) = decoder.next_record()? {
        records.push(record);
    }
    Ok(records)
}

fn record_count_error() -> CodecError {
    CodecError::InvalidValue(format!("record count exceeds {MAX_RECORDS}"))
}

fn encode_item<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|error| CodecError::Encode(error.to_string()))?;
    Ok(bytes)
}

fn validate_wire_version(version: u16) -> Result<(), CodecError> {
    if version == WIRE_VERSION {
        Ok(())
    } else {
        Err(CodecError::UnsupportedWireVersion(version))
    }
}

fn validate_payload(payload: &Payload) -> Result<(), CodecError> {
    if let Payload::Inline(bytes) = payload
        && bytes.len() > MAX_INLINE_PAYLOAD
    {
        return Err(CodecError::InlinePayloadTooLarge {
            actual: bytes.len(),
            maximum: MAX_INLINE_PAYLOAD,
        });
    }
    Ok(())
}

impl From<&CapsuleDescriptor> for WireDescriptorV1 {
    fn from(value: &CapsuleDescriptor) -> Self {
        Self(
            WIRE_VERSION,
            value.schema_version,
            WireStateRef::from(&value.base_state),
            value
                .connectors
                .iter()
                .map(|(id, definition)| (id.to_string(), WireConnectorDef::from(definition)))
                .collect(),
        )
    }
}

impl TryFrom<WireDescriptorV1> for CapsuleDescriptor {
    type Error = CodecError;

    fn try_from(value: WireDescriptorV1) -> Result<Self, Self::Error> {
        validate_wire_version(value.0)?;
        let mut connectors = BTreeMap::new();
        let mut previous_id = None;
        for (id, definition) in value.3 {
            let id = ConnectorId::parse(id).map_err(invalid_identifier)?;
            if previous_id.as_ref().is_some_and(|previous| previous >= &id) {
                return Err(CodecError::InvalidValue(format!(
                    "connector id `{id}` is not in strictly ascending order"
                )));
            }
            let definition = ConnectorDef::try_from(definition)?;
            previous_id = Some(id.clone());
            connectors.insert(id, definition);
        }
        let descriptor = Self {
            schema_version: value.1,
            base_state: StateRef::try_from(value.2)?,
            connectors,
        };
        descriptor
            .validate()
            .map_err(|error| CodecError::InvalidValue(error.to_string()))?;
        Ok(descriptor)
    }
}

impl From<&StateRef> for WireStateRef {
    fn from(value: &StateRef) -> Self {
        Self(value.state_type.to_string(), value.state_ref.to_string())
    }
}

impl TryFrom<WireStateRef> for StateRef {
    type Error = CodecError;

    fn try_from(value: WireStateRef) -> Result<Self, Self::Error> {
        Ok(Self {
            state_type: StateTypeId::parse(value.0).map_err(invalid_identifier)?,
            state_ref: ContentRef::parse(value.1).map_err(invalid_identifier)?,
        })
    }
}

impl From<&ConnectorDef> for WireConnectorDef {
    fn from(value: &ConnectorDef) -> Self {
        Self(
            value.protocol.to_string(),
            value.config_ref.as_ref().map(ToString::to_string),
        )
    }
}

impl TryFrom<WireConnectorDef> for ConnectorDef {
    type Error = CodecError;

    fn try_from(value: WireConnectorDef) -> Result<Self, Self::Error> {
        Ok(Self {
            protocol: ProtocolId::parse(value.0).map_err(invalid_identifier)?,
            config_ref: value
                .1
                .map(ContentRef::parse)
                .transpose()
                .map_err(invalid_identifier)?,
        })
    }
}

impl From<&IoRecord> for WireIoRecordV1 {
    fn from(value: &IoRecord) -> Self {
        Self(
            WIRE_VERSION,
            value.seq,
            value.offset_ns,
            value.observed_at_unix_ns,
            value.connector.to_string(),
            match value.direction {
                Direction::Ingress => 0,
                Direction::Egress => 1,
            },
            value.kind.to_string(),
            WirePayload::from(&value.payload),
        )
    }
}

impl TryFrom<WireIoRecordV1> for IoRecord {
    type Error = CodecError;

    fn try_from(value: WireIoRecordV1) -> Result<Self, Self::Error> {
        validate_wire_version(value.0)?;
        Ok(Self {
            seq: value.1,
            offset_ns: value.2,
            observed_at_unix_ns: value.3,
            connector: ConnectorId::parse(value.4).map_err(invalid_identifier)?,
            direction: match value.5 {
                0 => Direction::Ingress,
                1 => Direction::Egress,
                other => {
                    return Err(CodecError::InvalidValue(format!(
                        "unknown direction tag {other}"
                    )));
                }
            },
            kind: RecordKindId::parse(value.6).map_err(invalid_identifier)?,
            payload: Payload::try_from(value.7)?,
        })
    }
}

impl From<&Payload> for WirePayload {
    fn from(value: &Payload) -> Self {
        match value {
            Payload::Inline(bytes) => Self(0, ByteBuf::from(bytes.clone()), None),
            Payload::Object(reference) => Self(1, ByteBuf::new(), Some(reference.to_string())),
        }
    }
}

impl TryFrom<WirePayload> for Payload {
    type Error = CodecError;

    fn try_from(value: WirePayload) -> Result<Self, Self::Error> {
        match (value.0, value.1.into_vec(), value.2) {
            (0, bytes, None) => Ok(Self::Inline(bytes)),
            (1, bytes, Some(reference)) if bytes.is_empty() => Ok(Self::Object(
                ContentRef::parse(reference).map_err(invalid_identifier)?,
            )),
            (tag, _, _) => Err(CodecError::InvalidValue(format!(
                "malformed payload with tag {tag}"
            ))),
        }
    }
}

fn invalid_identifier(error: capsule_protocol::IdentifierError) -> CodecError {
    CodecError::InvalidValue(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> CapsuleDescriptor {
        CapsuleDescriptor {
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
        }
    }

    fn records() -> Vec<IoRecord> {
        vec![
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
        ]
    }

    #[test]
    fn descriptor_round_trip_is_byte_stable() {
        let first = encode_descriptor(&descriptor()).unwrap();
        let golden =
            hex::decode(include_str!("../tests/vectors/descriptor-v1.cbor.hex").trim()).unwrap();
        assert_eq!(first, golden);
        let decoded = decode_descriptor(&first).unwrap();
        assert_eq!(decoded, descriptor());
        assert_eq!(encode_descriptor(&decoded).unwrap(), first);
    }

    #[test]
    fn descriptor_reader_decodes_exact_member_without_buffering_wrapper() {
        let bytes = encode_descriptor(&descriptor()).unwrap();
        assert_eq!(
            decode_descriptor_reader(bytes.as_slice(), bytes.len() as u64).unwrap(),
            descriptor()
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            decode_descriptor_reader(trailing.as_slice(), trailing.len() as u64),
            Err(CodecError::Decode(message)) if message.contains("trailing bytes")
        ));
    }

    #[test]
    fn cbor_sequence_streams_multiple_records_without_losing_order() {
        let bytes = encode_record_stream(&descriptor(), &records()).unwrap();
        let golden =
            hex::decode(include_str!("../tests/vectors/records-pty-v1.cborseq.hex").trim())
                .unwrap();
        assert_eq!(bytes, golden);
        let decoded = decode_record_stream(&descriptor(), &bytes).unwrap();
        assert_eq!(decoded, records());
        assert_eq!(
            encode_record_stream(&descriptor(), &decoded).unwrap(),
            bytes
        );
    }

    #[test]
    fn streaming_codec_matches_golden_vector_byte_for_byte() {
        let descriptor = descriptor();
        let records = records();
        let golden =
            hex::decode(include_str!("../tests/vectors/records-pty-v1.cborseq.hex").trim())
                .unwrap();
        let mut encoded = Vec::new();
        let stats = {
            let mut encoder = RecordStreamEncoder::new(&descriptor, &mut encoded).unwrap();
            for record in &records {
                encoder.push(record).unwrap();
            }
            encoder.finish().unwrap()
        };
        assert_eq!(encoded, golden);
        assert_eq!(stats.record_count, records.len());
        assert_eq!(stats.encoded_bytes, golden.len() as u64);

        let mut decoder =
            RecordStreamDecoder::new(&descriptor, encoded.as_slice(), encoded.len() as u64)
                .unwrap();
        let mut decoded = Vec::new();
        while let Some(record) = decoder.next_record().unwrap() {
            decoded.push(record);
        }
        assert_eq!(decoded, records);
        assert_eq!(decoder.stats(), stats);
    }

    #[test]
    fn streaming_decoder_rejects_trailing_malformed_record() {
        let descriptor = descriptor();
        let mut bytes = encode_record_stream(&descriptor, &records()).unwrap();
        bytes.push(0x9f);
        let mut decoder =
            RecordStreamDecoder::new(&descriptor, bytes.as_slice(), bytes.len() as u64).unwrap();
        assert!(decoder.next_record().unwrap().is_some());
        assert!(decoder.next_record().unwrap().is_some());
        assert!(matches!(decoder.next_record(), Err(CodecError::Decode(_))));
    }

    #[test]
    fn streaming_encoder_rejects_record_after_normative_limit() {
        let descriptor = descriptor();
        let mut output = Vec::new();
        let mut encoder = RecordStreamEncoder::new(&descriptor, &mut output).unwrap();
        encoder.count = MAX_RECORDS;
        assert!(matches!(
            encoder.push(&records()[0]),
            Err(CodecError::InvalidValue(message)) if message.contains("record count exceeds")
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn empty_sequence_is_valid() {
        assert_eq!(
            encode_record_stream(&descriptor(), &[]).unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(
            decode_record_stream(&descriptor(), &[]).unwrap(),
            Vec::<IoRecord>::new()
        );
    }

    #[test]
    fn oversized_inline_payload_is_rejected() {
        let mut records = records();
        records[0].payload = Payload::Inline(vec![0; MAX_INLINE_PAYLOAD + 1]);
        assert!(matches!(
            encode_record_stream(&descriptor(), &records),
            Err(CodecError::InlinePayloadTooLarge { .. })
        ));
    }

    #[test]
    fn duplicate_wire_connector_is_rejected_instead_of_overwritten() {
        let wire = WireDescriptorV1(
            WIRE_VERSION,
            1,
            WireStateRef(
                "ato.state.workspace-posix-host@1".to_owned(),
                format!("blake3:{}", "12".repeat(32)),
            ),
            vec![
                (
                    "terminal.main".to_owned(),
                    WireConnectorDef("ato.io.pty@1".to_owned(), None),
                ),
                (
                    "terminal.main".to_owned(),
                    WireConnectorDef("ato.io.pty@1".to_owned(), None),
                ),
            ],
        );
        let bytes = encode_item(&wire).unwrap();
        assert!(matches!(
            decode_descriptor(&bytes),
            Err(CodecError::InvalidValue(message)) if message.contains("strictly ascending")
        ));
    }

    #[test]
    fn unsorted_wire_connectors_are_rejected() {
        let wire = WireDescriptorV1(
            WIRE_VERSION,
            1,
            WireStateRef(
                "ato.state.workspace-posix-host@1".to_owned(),
                format!("blake3:{}", "12".repeat(32)),
            ),
            vec![
                (
                    "terminal.z".to_owned(),
                    WireConnectorDef("ato.io.pty@1".to_owned(), None),
                ),
                (
                    "terminal.a".to_owned(),
                    WireConnectorDef("ato.io.pty@1".to_owned(), None),
                ),
            ],
        );
        let bytes = encode_item(&wire).unwrap();
        assert!(matches!(
            decode_descriptor(&bytes),
            Err(CodecError::InvalidValue(message)) if message.contains("strictly ascending")
        ));
    }

    #[test]
    fn normative_cddl_identifier_patterns_match_the_domain_contract() {
        let cddl = include_str!("../../../docs/rfcs/accepted/protocol/CAPSULE_CBOR_V1.cddl");
        let normalized_patterns = cddl.replace("\\\\", "\\");
        assert!(normalized_patterns.contains(capsule_protocol::COMPONENT_ID_PATTERN));
        assert!(normalized_patterns.contains(capsule_protocol::VERSIONED_ID_PATTERN));
        assert!(cddl.contains("strictly ascending connector-id byte order"));
    }
}
