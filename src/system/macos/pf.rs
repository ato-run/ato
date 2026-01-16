use crate::system::common::SystemError;

/// Thin placeholder for PF anchor/table management on macOS.
#[derive(Debug, Clone)]
pub struct PfAnchor;

impl PfAnchor {
    pub fn create() -> Result<Self, SystemError> {
        Err(SystemError::Unsupported(
            "PF anchor management not implemented yet".to_string(),
        ))
    }

    pub fn update_rules(&self) -> Result<(), SystemError> {
        Err(SystemError::Unsupported(
            "PF rule update not implemented yet".to_string(),
        ))
    }
}
