use crate::system::common::SystemError;

/// Thin placeholder for AppContainer profile management.
#[derive(Debug, Clone)]
pub struct AppContainerProfile;

impl AppContainerProfile {
    pub fn create_ephemeral() -> Result<Self, SystemError> {
        Err(SystemError::Unsupported(
            "AppContainer profile creation not implemented yet".to_string(),
        ))
    }

    pub fn apply_to_child(&self) -> Result<(), SystemError> {
        Err(SystemError::Unsupported(
            "AppContainer apply_to_child not implemented yet".to_string(),
        ))
    }
}
