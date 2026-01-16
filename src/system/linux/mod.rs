use async_trait::async_trait;
use std::process::Command;

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

use crate::system::common::{IsolationRule, SystemError};
use crate::system::NetworkSandbox;

pub mod cgroup;
pub mod ebpf;
pub mod enforcement;

pub struct LinuxSandbox {
    enforcer: Option<ebpf::EgressEnforcerHandle>,
}

impl LinuxSandbox {
    pub fn new() -> Self {
        Self { enforcer: None }
    }
}

#[async_trait]
impl NetworkSandbox for LinuxSandbox {
    async fn prepare(&mut self, rule: IsolationRule) -> Result<(), SystemError> {
        let handle = ebpf::start_enforcer(&rule.allow_rules, &rule.dns_rules, &rule.job_id)?;
        self.enforcer = Some(handle);
        Ok(())
    }

    fn apply_to_child(&self, _cmd: &mut Command) -> Result<(), SystemError> {
        let handle = self.enforcer.as_ref().ok_or_else(|| {
            SystemError::Unsupported("LinuxSandbox::apply_to_child requires prepare".to_string())
        })?;
        let cgroup_path = handle.cgroup_path.clone();

        _cmd.pre_exec(move || {
            std::fs::write(
                cgroup_path.join("cgroup.procs"),
                std::process::id().to_string(),
            )?;
            Ok(())
        });

        Ok(())
    }

    async fn update_rules(&mut self, rule: IsolationRule) -> Result<(), SystemError> {
        if let Some(handle) = self.enforcer.as_mut() {
            handle.update_allowlist(&rule.allow_rules)?;
            Ok(())
        } else {
            Err(SystemError::Unsupported(
                "LinuxSandbox::update_rules called before prepare".to_string(),
            ))
        }
    }
}
