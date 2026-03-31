use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

use crate::ipc::inject::IpcContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedMount {
    pub source: PathBuf,
    pub target: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeLaunchContext {
    ipc: Option<IpcContext>,
    injected_env: HashMap<String, String>,
    injected_mounts: Vec<InjectedMount>,
    command_args: Vec<String>,
}

impl RuntimeLaunchContext {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_ipc(ipc: IpcContext) -> Self {
        if ipc.has_ipc() || !ipc.env_vars.is_empty() {
            Self {
                ipc: Some(ipc),
                injected_env: HashMap::new(),
                injected_mounts: Vec::new(),
                command_args: Vec::new(),
            }
        } else {
            Self::empty()
        }
    }

    pub fn with_injected_env(mut self, env: HashMap<String, String>) -> Self {
        self.injected_env.extend(env);
        self
    }

    pub fn with_injected_mounts(mut self, mounts: Vec<InjectedMount>) -> Self {
        self.injected_mounts.extend(mounts);
        self
    }

    pub fn with_command_args(mut self, args: Vec<String>) -> Self {
        self.command_args.extend(args);
        self
    }

    pub fn ipc(&self) -> Option<&IpcContext> {
        self.ipc.as_ref()
    }

    pub fn ipc_env_vars(&self) -> Option<&HashMap<String, String>> {
        self.ipc().map(|ipc| &ipc.env_vars)
    }

    pub fn socket_paths(&self) -> Option<&HashMap<String, PathBuf>> {
        self.ipc().map(|ipc| &ipc.socket_paths)
    }

    pub fn injected_env(&self) -> &HashMap<String, String> {
        &self.injected_env
    }

    pub fn injected_mounts(&self) -> &[InjectedMount] {
        &self.injected_mounts
    }

    pub fn command_args(&self) -> &[String] {
        &self.command_args
    }

    pub fn merged_env(&self) -> HashMap<String, String> {
        let mut env = self.ipc_env_vars().cloned().unwrap_or_else(HashMap::new);
        env.extend(self.injected_env.clone());
        env
    }

    pub fn env_permission_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.merged_env().into_keys().collect();
        keys.sort();
        keys.dedup();
        keys
    }

    pub fn apply_allowlisted_env(&self, cmd: &mut Command) -> Result<()> {
        if let Some(env) = self.ipc_env_vars() {
            for (key, value) in env {
                if key.starts_with("CAPSULE_IPC_") || key == "ATO_BRIDGE_TOKEN" {
                    cmd.env(key, value);
                    continue;
                }

                return Err(
                    capsule_core::execution_plan::error::AtoExecutionError::policy_violation(
                        format!("session_token env '{}' is not allowlisted", key),
                    )
                    .into(),
                );
            }
        }

        for (key, value) in &self.injected_env {
            cmd.env(key, value);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeLaunchContext;
    use crate::ipc::inject::IpcContext;
    use std::path::PathBuf;

    #[test]
    fn empty_context_does_not_apply_env() {
        let ctx = RuntimeLaunchContext::empty();
        let mut cmd = std::process::Command::new("echo");
        ctx.apply_allowlisted_env(&mut cmd).unwrap();
        assert!(ctx.ipc().is_none());
        assert!(ctx.injected_env().is_empty());
    }

    #[test]
    fn non_allowlisted_env_is_rejected() {
        let ctx = RuntimeLaunchContext::from_ipc(IpcContext {
            env_vars: [("BAD_ENV".to_string(), "value".to_string())]
                .into_iter()
                .collect(),
            ..IpcContext::default()
        });
        let mut cmd = std::process::Command::new("echo");
        let err = ctx
            .apply_allowlisted_env(&mut cmd)
            .expect_err("must reject");
        assert!(err.to_string().contains("not allowlisted"));
    }

    #[test]
    fn injected_env_is_merged_and_applied() {
        let ctx = RuntimeLaunchContext::empty().with_injected_env(
            [("ATO_SERVICE_DB_HOST".to_string(), "127.0.0.1".to_string())]
                .into_iter()
                .collect(),
        );
        let mut cmd = std::process::Command::new("echo");
        ctx.apply_allowlisted_env(&mut cmd).unwrap();

        let value = cmd
            .get_envs()
            .find_map(|(key, value)| {
                if key == "ATO_SERVICE_DB_HOST" {
                    value.map(|v| v.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .expect("injected env must be present");

        assert_eq!(value, "127.0.0.1");
        assert_eq!(ctx.env_permission_keys(), vec!["ATO_SERVICE_DB_HOST"]);
    }

    #[test]
    fn injected_mounts_are_preserved() {
        let mount = super::InjectedMount {
            source: PathBuf::from("/tmp/model"),
            target: "/var/run/ato/injected/MODEL_DIR".to_string(),
            readonly: true,
        };
        let ctx = RuntimeLaunchContext::empty().with_injected_mounts(vec![mount.clone()]);
        assert_eq!(ctx.injected_mounts(), &[mount]);
    }
}
