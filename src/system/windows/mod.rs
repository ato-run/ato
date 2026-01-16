use async_trait::async_trait;
use std::process::Command;

use crate::system::common::{IsolationRule, SystemError};
use crate::system::NetworkSandbox;

pub mod app_container;
pub mod wfp;

pub struct WindowsSandbox {
    session: Option<wfp::WfpSession>,
    app_container: Option<app_container::AppContainerProfile>,
}

impl WindowsSandbox {
    pub fn new() -> Self {
        Self {
            session: None,
            app_container: None,
        }
    }
}

#[async_trait]
impl NetworkSandbox for WindowsSandbox {
    async fn prepare(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
        self.session = Some(wfp::WfpSession::open_dynamic()?);
        self.app_container = Some(app_container::AppContainerProfile::create_ephemeral()?);
        Err(SystemError::Unsupported(
            "Windows network sandbox not implemented".to_string(),
        ))
    }

    fn apply_to_child(&self, _cmd: &mut Command) -> Result<(), SystemError> {
        if let Some(container) = &self.app_container {
            let _ = container.apply_to_child()?;
        }
        Err(SystemError::Unsupported(
            "Windows network sandbox not implemented".to_string(),
        ))
    }

    async fn update_rules(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
        if let Some(session) = &self.session {
            let _ = session.update_filters()?;
        }
        Err(SystemError::Unsupported(
            "Windows network sandbox not implemented".to_string(),
        ))
    }
}
