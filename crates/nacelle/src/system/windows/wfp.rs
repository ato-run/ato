use crate::system::common::SystemError;

/// Thin placeholder for a Windows Filtering Platform session.
#[derive(Debug, Clone)]
pub struct WfpSession;

impl WfpSession {
    pub fn open_dynamic() -> Result<Self, SystemError> {
        Err(SystemError::Unsupported(
            "WFP session not implemented yet".to_string(),
        ))
    }

    pub fn update_filters(&self) -> Result<(), SystemError> {
        Err(SystemError::Unsupported(
            "WFP filter update not implemented yet".to_string(),
        ))
    }
}
