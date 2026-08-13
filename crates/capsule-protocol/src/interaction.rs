use crate::{ContentRef, Direction, InteractionKindId, PortId};

/// An observed interaction at a computation Port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionRecord {
    /// Recorder-assigned serialization order. This is the sole ordering authority.
    pub seq: u64,
    /// Monotonic offset for optional replay pacing, never ordering.
    pub offset_ns: Option<u64>,
    /// Wall-clock audit metadata, never ordering.
    pub observed_at_unix_ns: Option<i64>,
    pub port: PortId,
    pub direction: Direction,
    pub kind: InteractionKindId,
    pub payload: InteractionPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionPayload {
    Inline(Vec<u8>),
    Object(ContentRef),
}
