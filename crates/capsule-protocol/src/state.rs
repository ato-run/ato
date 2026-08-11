use crate::{ContentRef, StateTypeId};

/// A content-addressed computation state whose type defines continuation semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRef {
    pub state_type: StateTypeId,
    pub state_ref: ContentRef,
}
