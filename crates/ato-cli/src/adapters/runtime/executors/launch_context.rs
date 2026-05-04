use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use capsule_core::execution_identity::EnvOrigin;

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
    injected_env_origins: HashMap<String, EnvOrigin>,
    injected_mounts: Vec<InjectedMount>,
    command_args: Vec<String>,
    /// Caller's cwd when `ato run` was invoked. Used for relative-path
    /// argument resolution, grant inference, and IO candidate detection.
    /// **Not** automatically used as the spawned process cwd — see
    /// `executors::source::resolve_host_execution_cwd` for the rule:
    /// effective_cwd becomes the execution cwd only when it lives
    /// inside the materialized capsule's workspace_root (= the user is
    /// invoking from within the project tree).
    effective_cwd: Option<PathBuf>,
    /// Filesystem root of the materialized capsule. When `effective_cwd`
    /// is outside this root (e.g. `ato run github.com/...` invoked from
    /// somewhere unrelated), the spawned process cwd defaults to
    /// `LaunchSpec.working_dir` instead so module imports / relative
    /// scripts resolve against the capsule's source tree.
    workspace_root: Option<PathBuf>,
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
                injected_env_origins: HashMap::new(),
                injected_mounts: Vec::new(),
                command_args: Vec::new(),
                effective_cwd: None,
                workspace_root: None,
            }
        } else {
            Self::empty()
        }
    }

    pub fn with_injected_env(mut self, env: HashMap<String, String>) -> Self {
        self.injected_env_origins.extend(
            env.keys()
                .cloned()
                .map(|key| (key, EnvOrigin::ManifestStatic)),
        );
        self.injected_env.extend(env);
        self
    }

    pub fn with_injected_env_with_origin(
        mut self,
        env: HashMap<String, String>,
        origin: EnvOrigin,
    ) -> Self {
        self.injected_env_origins
            .extend(env.keys().cloned().map(|key| (key, origin.clone())));
        self.injected_env.extend(env);
        self
    }

    pub fn with_injected_mounts(mut self, mounts: Vec<InjectedMount>) -> Self {
        self.injected_mounts.extend(mounts);
        self
    }

    pub fn with_command_args(mut self, args: Vec<String>) -> Self {
        self.command_args = args;
        self
    }

    pub fn command_args(&self) -> &[String] {
        &self.command_args
    }

    pub fn with_effective_cwd(mut self, cwd: PathBuf) -> Self {
        self.effective_cwd = Some(cwd);
        self
    }

    pub fn effective_cwd(&self) -> Option<&PathBuf> {
        self.effective_cwd.as_ref()
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
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

    pub fn merged_env(&self) -> HashMap<String, String> {
        let mut env = self.ipc_env_vars().cloned().unwrap_or_else(HashMap::new);
        env.extend(self.injected_env.clone());
        env
    }

    pub fn merged_env_with_origins(&self) -> HashMap<String, (String, EnvOrigin)> {
        let mut env = self
            .ipc_env_vars()
            .cloned()
            .unwrap_or_else(HashMap::new)
            .into_iter()
            .map(|(key, value)| (key, (value, EnvOrigin::Host)))
            .collect::<HashMap<_, _>>();
        for (key, value) in &self.injected_env {
            let origin = self
                .injected_env_origins
                .get(key)
                .cloned()
                .unwrap_or(EnvOrigin::ManifestStatic);
            env.insert(key.clone(), (value.clone(), origin));
        }
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

    #[test]
    fn command_args_and_effective_cwd_are_preserved() {
        let cwd = PathBuf::from("/workspace/project");
        let ctx = RuntimeLaunchContext::empty()
            .with_command_args(vec!["--help".to_string()])
            .with_effective_cwd(cwd.clone());

        assert_eq!(ctx.command_args(), &["--help".to_string()]);
        assert_eq!(ctx.effective_cwd(), Some(&cwd));
    }
}
