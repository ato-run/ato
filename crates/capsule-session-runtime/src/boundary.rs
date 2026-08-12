use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundaryOperationId(String);

impl BoundaryOperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, BoundaryProtocolError> {
        let value = value.into();
        if value.is_empty() || value.len() > 255 || !value.is_ascii() {
            return Err(BoundaryProtocolError::InvalidOperationId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BoundaryOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryDeliveryState {
    CandidateDurable,
    DeliveryReleased,
    Delivered,
}

#[derive(Debug, Default)]
pub struct BoundaryDeliveryLedger {
    operations: BTreeMap<BoundaryOperationId, BoundaryDeliveryState>,
}

impl BoundaryDeliveryLedger {
    pub fn candidate_durable(
        &mut self,
        operation_id: BoundaryOperationId,
    ) -> Result<(), BoundaryProtocolError> {
        if self.operations.contains_key(&operation_id) {
            return Err(BoundaryProtocolError::DuplicateOperation(operation_id));
        }
        self.operations
            .insert(operation_id, BoundaryDeliveryState::CandidateDurable);
        Ok(())
    }

    pub fn release_delivery(
        &mut self,
        operation_id: &BoundaryOperationId,
    ) -> Result<(), BoundaryProtocolError> {
        self.transition(
            operation_id,
            BoundaryDeliveryState::CandidateDurable,
            BoundaryDeliveryState::DeliveryReleased,
        )
    }

    pub fn acknowledge_delivery(
        &mut self,
        operation_id: &BoundaryOperationId,
    ) -> Result<(), BoundaryProtocolError> {
        self.transition(
            operation_id,
            BoundaryDeliveryState::DeliveryReleased,
            BoundaryDeliveryState::Delivered,
        )
    }

    pub fn has_uncertain_delivery(&self) -> bool {
        self.operations
            .values()
            .any(|state| *state == BoundaryDeliveryState::DeliveryReleased)
    }

    fn transition(
        &mut self,
        operation_id: &BoundaryOperationId,
        expected: BoundaryDeliveryState,
        next: BoundaryDeliveryState,
    ) -> Result<(), BoundaryProtocolError> {
        let state = self
            .operations
            .get_mut(operation_id)
            .ok_or_else(|| BoundaryProtocolError::UnknownOperation(operation_id.clone()))?;
        if *state != expected {
            return Err(BoundaryProtocolError::InvalidDeliveryTransition {
                operation_id: operation_id.clone(),
                from: *state,
                to: next,
            });
        }
        *state = next;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BoundaryProtocolError {
    #[error("boundary operation id must be 1..=255 ASCII bytes")]
    InvalidOperationId,
    #[error("duplicate boundary operation {0}")]
    DuplicateOperation(BoundaryOperationId),
    #[error("unknown boundary operation {0}")]
    UnknownOperation(BoundaryOperationId),
    #[error("invalid delivery transition for {operation_id}: {from:?} -> {to:?}")]
    InvalidDeliveryTransition {
        operation_id: BoundaryOperationId,
        from: BoundaryDeliveryState,
        to: BoundaryDeliveryState,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_id() -> BoundaryOperationId {
        BoundaryOperationId::parse("op-42").expect("operation id")
    }

    #[test]
    fn delivery_is_uncertain_only_after_release_and_before_acknowledgement() {
        let mut ledger = BoundaryDeliveryLedger::default();
        let operation_id = operation_id();
        ledger
            .candidate_durable(operation_id.clone())
            .expect("candidate");
        assert!(!ledger.has_uncertain_delivery());

        ledger
            .release_delivery(&operation_id)
            .expect("release delivery");
        assert!(ledger.has_uncertain_delivery());

        ledger
            .acknowledge_delivery(&operation_id)
            .expect("acknowledge");
        assert!(!ledger.has_uncertain_delivery());
    }

    #[test]
    fn delivery_cannot_be_released_before_candidate_is_durable() {
        let mut ledger = BoundaryDeliveryLedger::default();
        assert!(matches!(
            ledger.release_delivery(&operation_id()),
            Err(BoundaryProtocolError::UnknownOperation(_))
        ));
    }

    #[test]
    fn duplicate_candidate_does_not_reset_existing_delivery_state() {
        let mut ledger = BoundaryDeliveryLedger::default();
        let operation_id = operation_id();
        ledger
            .candidate_durable(operation_id.clone())
            .expect("candidate");
        ledger
            .release_delivery(&operation_id)
            .expect("release delivery");

        assert!(matches!(
            ledger.candidate_durable(operation_id.clone()),
            Err(BoundaryProtocolError::DuplicateOperation(_))
        ));
        assert!(ledger.has_uncertain_delivery());
        ledger
            .acknowledge_delivery(&operation_id)
            .expect("existing state was preserved");
    }
}
