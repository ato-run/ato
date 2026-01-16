use async_trait::async_trait;
use std::process::Command;

use crate::system::common::{IsolationRule, SystemError};
use crate::system::NetworkSandbox;

pub mod pf;
pub mod user_group;

pub struct MacosSandbox {
    anchor: Option<pf::PfAnchor>,
    group: Option<user_group::GroupIdentity>,
}

impl MacosSandbox {
    pub fn new() -> Self {
        Self {
            anchor: None,
            group: None,
        }
    }
}

impl Default for MacosSandbox {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NetworkSandbox for MacosSandbox {
    async fn prepare(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
        self.anchor = Some(pf::PfAnchor::create()?);
        self.group = Some(user_group::GroupIdentity::acquire_ephemeral()?);
        Err(SystemError::Unsupported(
            "macOS network sandbox not implemented".to_string(),
        ))
    }

    fn apply_to_child(&self, _cmd: &mut Command) -> Result<(), SystemError> {
        Err(SystemError::Unsupported(
            "macOS network sandbox not implemented".to_string(),
        ))
    }

    async fn update_rules(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
        if let Some(anchor) = &self.anchor {
            let _ = anchor.update_rules()?;
        }
        Err(SystemError::Unsupported(
            "macOS network sandbox not implemented".to_string(),
        ))
    }
}
