use crate::{ConnectorId, ContentRef, RecordKindId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// External input that a historical replay may inject into computation.
    Ingress,
    /// External output that replay observes from computation; never injected.
    Egress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Inline(Vec<u8>),
    Object(ContentRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoRecord {
    /// Recorder-assigned serialization order. This is the sole ordering authority.
    pub seq: u64,
    /// Monotonic offset for optional replay pacing, never ordering.
    pub offset_ns: Option<u64>,
    /// Wall-clock audit metadata, never ordering.
    pub observed_at_unix_ns: Option<i64>,
    pub connector: ConnectorId,
    pub direction: Direction,
    pub kind: RecordKindId,
    pub payload: Payload,
}
