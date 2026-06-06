use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use capsule_core::execution_identity::EnvOrigin;

use crate::adapters::runtime::secret_injection::RuntimeSecretEnv;
use crate::ipc::inject::IpcContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedMount {
    pub source: PathBuf,
    pub target: String,
    pub readonly: bool,
}

/// Per-endpoint preferred-port request carried from a `capsule://` port query
/// input (#548).
///
/// - `Concrete(n)` — `port[.<endpoint>]=<n>`: this exact port is the preferred
///   port for the endpoint; admission records a claim with it.
/// - `Auto` — `port[.<endpoint>]=auto`: an *explicit* request for no concrete
///   preferred port. This is distinct from "no entry": it actively **suppresses
///   the env `PORT` fallback** for that endpoint so the runtime uses its OS
///   auto-assign path and creates no concrete preferred-port claim from the
///   query. Concrete query ports still win; an endpoint with no entry at all
///   keeps the existing env-`PORT` behavior unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPreference {
    Concrete(u16),
    Auto,
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
    /// True when effective_cwd came from an explicit `ato run --cwd ...`
    /// override rather than the caller's ambient shell cwd.
    effective_cwd_is_explicit_override: bool,
    /// Filesystem root of the materialized capsule. When `effective_cwd`
    /// is outside this root (e.g. `ato run github.com/...` invoked from
    /// somewhere unrelated), the spawned process cwd defaults to
    /// `LaunchSpec.working_dir` instead so module imports / relative
    /// scripts resolve against the capsule's source tree.
    workspace_root: Option<PathBuf>,
    /// Endpoints (`host:port` strings) of capsule dependencies the
    /// orchestrator started for this consumer. Surfaced into the
    /// nacelle-side normalized manifest's `[isolation.network.egress_allow]`
    /// so the consumer's sandbox profile permits TCP back to providers
    /// like postgres on `127.0.0.1:<allocated_port>`. Without this, the
    /// consumer hits EPERM on `psycopg.connect(...)` even though the
    /// provider is happily listening on the same loopback (#17).
    dep_endpoints: Vec<String>,
    /// Egress proxy port allocated by `ato-netd` for this session.
    ///
    /// When `Some`, OCI executors must override proxy env vars to point at
    /// `http://host.containers.internal:<port>` (the loopback `127.0.0.1` used
    /// by source-native env injection cannot be reached from inside containers).
    egress_proxy_port: Option<u16>,
    /// Install profile key when this launch is an installed-app launch
    /// (`ato app run`/desktop relaunch), `None` for ephemeral `ato run`.
    ///
    /// Threaded explicitly (rather than read from the thread-local install
    /// lifecycle context) because the launch path crosses async executor
    /// boundaries where the thread-local does not reliably propagate. Used by
    /// `web_services` to scope per-install port admission claims so two
    /// installed apps that both prefer the same port get deterministically
    /// remapped instead of colliding.
    install_profile_key: Option<String>,
    /// SecretStore-backed launch-condition grants resolved for this installed
    /// relaunch (#508), injected into the spawned process env.
    ///
    /// Deliberately a **separate** channel from `injected_env`: secret values must
    /// reach the process but must NOT be observed by the execution receipt /
    /// session record, which read `merged_env*`. This field is therefore excluded
    /// from `merged_env`, `merged_env_with_origins`, and `env_permission_keys`, and
    /// is applied only at the final spawn-env construction points
    /// (`apply_allowlisted_env`, the nacelle payload, and the web-service env map).
    /// `RuntimeLaunchContext` does not derive `Serialize`, and `RuntimeSecretEnv`'s
    /// `Debug` redacts the value, so values never reach logs or serialized state.
    secret_env: Vec<RuntimeSecretEnv>,
    /// Preferred-port request per logical endpoint from a `capsule://` port query
    /// input (`port=<n>` / `port.<endpoint>=<n>` / `port[.<endpoint>]=auto`),
    /// keyed by endpoint name (`main` for the bare `port`). Used by the web-service
    /// port admission to pick the preferred port for that endpoint before
    /// consulting the claim ledger (#548).
    ///
    /// - `Concrete(n)` → that port is preferred.
    /// - `Auto` → an *explicit* "no concrete preferred port" that suppresses the
    ///   env-`PORT` fallback for that endpoint (OS auto-assign, no concrete claim).
    /// - no entry → unchanged behavior: env-`PORT` fallback applies.
    ///
    /// Empty for `ato run` (no install lifecycle), so transient launches are
    /// untouched.
    port_preferences: HashMap<String, PortPreference>,
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
                effective_cwd_is_explicit_override: false,
                workspace_root: None,
                dep_endpoints: Vec::new(),
                egress_proxy_port: None,
                install_profile_key: None,
                secret_env: Vec::new(),
                port_preferences: HashMap::new(),
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

    /// Extend the injected environment with additional key-value pairs,
    /// recording each as [`EnvOrigin::ManifestStatic`]. Takes `&mut self`
    /// so callers that already hold a prepared context can patch it in-place.
    pub fn extend_injected_env(&mut self, env: impl IntoIterator<Item = (String, String)>) {
        for (key, value) in env {
            self.injected_env_origins
                .insert(key.clone(), EnvOrigin::ManifestStatic);
            self.injected_env.insert(key, value);
        }
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

    pub fn with_effective_cwd_override(mut self, cwd: PathBuf) -> Self {
        self.effective_cwd = Some(cwd);
        self.effective_cwd_is_explicit_override = true;
        self
    }

    pub fn effective_cwd(&self) -> Option<&PathBuf> {
        self.effective_cwd.as_ref()
    }

    pub fn effective_cwd_is_explicit_override(&self) -> bool {
        self.effective_cwd_is_explicit_override
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
    }

    pub fn with_dep_endpoints(mut self, endpoints: Vec<String>) -> Self {
        self.dep_endpoints = endpoints;
        self
    }

    pub fn dep_endpoints(&self) -> &[String] {
        &self.dep_endpoints
    }

    /// Set the `ato-netd` egress proxy port for this session so OCI executors
    /// can override source-native proxy env vars to use `host.containers.internal`.
    pub fn set_egress_proxy_port(&mut self, port: u16) {
        self.egress_proxy_port = Some(port);
    }

    pub fn egress_proxy_port(&self) -> Option<u16> {
        self.egress_proxy_port
    }

    /// Mark this launch as an installed-app launch identified by
    /// `install_profile_key`. `None` keeps it ephemeral (`ato run`).
    pub fn with_install_profile_key(mut self, key: Option<String>) -> Self {
        self.install_profile_key = key;
        self
    }

    pub fn install_profile_key(&self) -> Option<&str> {
        self.install_profile_key.as_deref()
    }

    /// Attach SecretStore-backed launch-condition grants resolved for this
    /// installed relaunch (#508). These are injected into the spawned process env
    /// but excluded from receipt/session env observation — see [`Self::secret_env`].
    pub fn with_secret_env(mut self, secret_env: Vec<RuntimeSecretEnv>) -> Self {
        self.secret_env = secret_env;
        self
    }

    pub fn secret_env(&self) -> &[RuntimeSecretEnv] {
        &self.secret_env
    }

    /// Attach per-endpoint preferred-port requests from `capsule://` port query
    /// inputs (#548). Keyed by logical endpoint name (`main` for bare `port`).
    /// `port[.<endpoint>]=auto` is carried as [`PortPreference::Auto`] so it can
    /// explicitly suppress the env-`PORT` fallback for that endpoint. Empty for
    /// `ato run`, leaving transient launches untouched.
    pub fn with_port_preferences(mut self, prefs: HashMap<String, PortPreference>) -> Self {
        self.port_preferences = prefs;
        self
    }

    /// The preferred-port request a `capsule://` port query made for `endpoint`,
    /// if any. `None` when no port input named this endpoint (so the env-`PORT`
    /// fallback applies); `Some(PortPreference::Auto)` when the query explicitly
    /// asked for auto (suppressing that fallback); `Some(PortPreference::Concrete)`
    /// for a concrete requested port.
    pub fn port_preference(&self, endpoint: &str) -> Option<PortPreference> {
        self.port_preferences.get(endpoint).copied()
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

        // Secret grants last so they win for their exact env key. Applied here at
        // the spawn boundary only; never merged into the receipt-observed env.
        for secret in &self.secret_env {
            cmd.env(&secret.name, secret.value.expose());
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
    fn secret_env_reaches_command_but_is_excluded_from_receipt_env() {
        use crate::adapters::runtime::secret_injection::{RuntimeSecretEnv, SecretValue};
        let ctx = RuntimeLaunchContext::empty().with_secret_env(vec![RuntimeSecretEnv {
            name: "OPENAI_API_KEY".to_string(),
            value: SecretValue::new("sk-secret-value".to_string()),
        }]);

        // Excluded from every env surface the receipt / session observer reads.
        assert!(
            ctx.merged_env().is_empty(),
            "secret must not be in merged_env"
        );
        assert!(
            ctx.merged_env_with_origins().is_empty(),
            "secret must not be in the receipt-observed env"
        );
        assert!(ctx.env_permission_keys().is_empty());
        assert!(ctx.injected_env().is_empty());

        // But applied to the spawned command at the boundary.
        let mut cmd = std::process::Command::new("echo");
        ctx.apply_allowlisted_env(&mut cmd).unwrap();
        let value = cmd.get_envs().find_map(|(k, v)| {
            if k == "OPENAI_API_KEY" {
                v.map(|v| v.to_string_lossy().to_string())
            } else {
                None
            }
        });
        assert_eq!(value.as_deref(), Some("sk-secret-value"));
    }

    #[test]
    fn secret_env_value_is_redacted_in_context_debug() {
        use crate::adapters::runtime::secret_injection::{RuntimeSecretEnv, SecretValue};
        let ctx = RuntimeLaunchContext::empty().with_secret_env(vec![RuntimeSecretEnv {
            name: "OPENAI_API_KEY".to_string(),
            value: SecretValue::new("sk-secret-xyz".to_string()),
        }]);
        let rendered = format!("{ctx:?}");
        assert!(
            !rendered.contains("sk-secret-xyz"),
            "debug leaked the value"
        );
        assert!(
            rendered.contains("OPENAI_API_KEY"),
            "name should be visible"
        );
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
