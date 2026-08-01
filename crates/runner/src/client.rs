//! Typed clients for the `ato` CLI's installed-app and session surfaces.
//!
//! These clients own command construction and JSON/error handling only. The
//! CLI remains the lifecycle owner; callers never read or mutate `~/.ato`
//! directly. Long-running processes still go through [`crate::ProcessSupervisor`].

use std::path::PathBuf;

use serde_json::Value;

use crate::{CommandSpec, HostError, RunnerHost};

const MAX_STDERR_BYTES: usize = 8 * 1024;

/// Failures surfaced by a typed CLI client.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Resolving or running the bundled `ato` binary failed.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The CLI completed but rejected the operation.
    #[error("ato command exited with code {exit_code}: {stderr}")]
    CommandFailed { exit_code: i32, stderr: String },
    /// A successful `--json` command returned an invalid payload.
    #[error("ato command returned invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// The host shell could not focus the requested session window.
    #[error("could not focus session: {0}")]
    Focus(String),
}

/// Supported install input classes. Local filesystem paths stay explicitly
/// typed so a remote string cannot accidentally enter the local install lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    Store(String),
    GitHub(String),
    Local(PathBuf),
}

/// Client for installed-app lifecycle commands.
pub struct InstalledAppsClient<'a, H: RunnerHost> {
    host: &'a H,
}

impl<'a, H: RunnerHost> InstalledAppsClient<'a, H> {
    pub fn new(host: &'a H) -> Self {
        Self { host }
    }

    pub fn list(&self) -> Result<Value, ClientError> {
        self.run_json(["app", "installed", "list", "--json"])
    }

    pub fn inspect(&self, install_profile_key: &str) -> Result<Value, ClientError> {
        self.run_json(["app", "installed", "inspect", install_profile_key, "--json"])
    }

    pub fn install(&self, source: &InstallSource) -> Result<Value, ClientError> {
        let mut args = vec!["install".to_string()];
        match source {
            InstallSource::Store(capsule_ref) => args.push(capsule_ref.clone()),
            InstallSource::GitHub(repository) => {
                args.extend(["--from-gh-repo".into(), repository.clone()]);
            }
            InstallSource::Local(path) => {
                args.extend(["--from-local".into(), path.display().to_string()]);
            }
        }
        args.extend(["-y".into(), "--no-project".into(), "--json".into()]);
        self.run_json(args)
    }

    pub fn update(&self, install_profile_key: &str) -> Result<Value, ClientError> {
        self.run_json(["update", install_profile_key, "-y", "--json"])
    }

    pub fn rollback(
        &self,
        install_profile_key: &str,
        revision: Option<&str>,
    ) -> Result<Value, ClientError> {
        let mut args = vec!["rollback".to_string(), install_profile_key.to_string()];
        if let Some(revision) = revision {
            args.push(revision.to_string());
        }
        args.push("--json".to_string());
        self.run_json(args)
    }

    pub fn remove(&self, install_profile_key: &str) -> Result<Value, ClientError> {
        self.remove_with_state_policy(install_profile_key, false)
    }

    pub fn remove_with_state_policy(
        &self,
        install_profile_key: &str,
        purge_state: bool,
    ) -> Result<Value, ClientError> {
        let mut args = vec![
            "app".to_string(),
            "installed".to_string(),
            "remove".to_string(),
            install_profile_key.to_string(),
        ];
        if purge_state {
            args.push("--purge-state".to_string());
        }
        args.push("--json".to_string());
        self.run_json(args)
    }

    fn run_json<I, S>(&self, args: I) -> Result<Value, ClientError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        run_ato_json(self.host, args)
    }
}

/// Client for runtime session commands. Window focus is injected because it is
/// host-shell presentation policy, not an `ato` CLI lifecycle operation.
pub struct SessionClient<'a, H: RunnerHost, F> {
    host: &'a H,
    focus: F,
}

impl<'a, H, F> SessionClient<'a, H, F>
where
    H: RunnerHost,
    F: Fn(&str) -> Result<(), String>,
{
    pub fn new(host: &'a H, focus: F) -> Self {
        Self { host, focus }
    }

    pub fn list(&self) -> Result<Value, ClientError> {
        run_ato_json(self.host, ["ps", "--json"])
    }

    pub fn launch(&self, install_profile_key: &str) -> Result<Value, ClientError> {
        run_ato_json(
            self.host,
            [
                "launch",
                install_profile_key,
                "-y",
                "--detached-session",
                "--json",
            ],
        )
    }

    pub fn stop(&self, session_id: &str) -> Result<Value, ClientError> {
        run_ato_json(self.host, ["app", "session", "stop", session_id, "--json"])
    }

    pub fn focus(&self, session_id: &str) -> Result<(), ClientError> {
        (self.focus)(session_id).map_err(ClientError::Focus)
    }
}

fn run_ato_json<H, I, S>(host: &H, args: I) -> Result<Value, ClientError>
where
    H: RunnerHost,
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let ato = host.resolve_binary("ato")?;
    let completed = host.run_to_completion(&CommandSpec {
        program: ato,
        args: args.into_iter().map(Into::into).collect(),
        env: Vec::new(),
    })?;
    if !completed.success() {
        return Err(ClientError::CommandFailed {
            exit_code: completed.exit_code,
            stderr: bounded_lossy(&completed.stderr),
        });
    }
    Ok(serde_json::from_slice(&completed.stdout)?)
}

fn bounded_lossy(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_STDERR_BYTES;
    let visible = &bytes[..bytes.len().min(MAX_STDERR_BYTES)];
    let mut text = String::from_utf8_lossy(visible).trim().to_string();
    if truncated {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::{ChildId, CompletedCommand, ManagedChild, SpawnSpec};

    use super::*;

    struct FakeChild;

    impl ManagedChild for FakeChild {
        fn id(&self) -> ChildId {
            ChildId(1)
        }

        fn is_alive(&self) -> bool {
            false
        }

        fn terminate_group(&mut self) -> Result<(), HostError> {
            Ok(())
        }
    }

    struct FakeHost {
        commands: Mutex<Vec<CommandSpec>>,
        completed: Mutex<Vec<CompletedCommand>>,
    }

    impl FakeHost {
        fn with_outputs(outputs: impl IntoIterator<Item = CompletedCommand>) -> Self {
            let mut completed: Vec<_> = outputs.into_iter().collect();
            completed.reverse();
            Self {
                commands: Mutex::new(Vec::new()),
                completed: Mutex::new(completed),
            }
        }

        fn successful_json(json: &str) -> Self {
            Self::with_outputs([CompletedCommand {
                exit_code: 0,
                stdout: json.as_bytes().to_vec(),
                stderr: Vec::new(),
            }])
        }

        fn args(&self) -> Vec<Vec<String>> {
            self.commands
                .lock()
                .unwrap()
                .iter()
                .map(|spec| spec.args.clone())
                .collect()
        }
    }

    impl RunnerHost for FakeHost {
        type Child = FakeChild;

        fn resolve_binary(&self, name: &str) -> Result<PathBuf, HostError> {
            Ok(PathBuf::from(format!("/bundle/{name}")))
        }

        fn spawn(&self, _spec: &SpawnSpec) -> Result<Self::Child, HostError> {
            Ok(FakeChild)
        }

        fn run_to_completion(&self, spec: &CommandSpec) -> Result<CompletedCommand, HostError> {
            self.commands.lock().unwrap().push(spec.clone());
            self.completed
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| HostError::Run("no fake output queued".into()))
        }
    }

    #[test]
    fn installed_list_uses_future_cli_read_contract() {
        let host = FakeHost::successful_json(r#"{"apps":[]}"#);
        let response = InstalledAppsClient::new(&host).list().unwrap();

        assert_eq!(response["apps"], serde_json::json!([]));
        assert_eq!(
            host.args(),
            vec![vec!["app", "installed", "list", "--json"]]
        );
    }

    #[test]
    fn install_source_selects_the_explicit_local_lane() {
        let host = FakeHost::successful_json(r#"{"ok":true}"#);
        InstalledAppsClient::new(&host)
            .install(&InstallSource::Local(PathBuf::from("/chosen/app")))
            .unwrap();

        assert_eq!(
            host.args(),
            vec![vec![
                "install",
                "--from-local",
                "/chosen/app",
                "-y",
                "--no-project",
                "--json",
            ]]
        );
    }

    #[test]
    fn launch_uses_detached_session_json_contract() {
        let host = FakeHost::successful_json(r#"{"session":{"session_id":"s1"}}"#);
        let client = SessionClient::new(&host, |_| Ok(()));
        let response = client.launch("ipk_123").unwrap();

        assert_eq!(response["session"]["session_id"], "s1");
        assert_eq!(
            host.args(),
            vec![vec![
                "launch",
                "ipk_123",
                "-y",
                "--detached-session",
                "--json",
            ]]
        );
    }

    #[test]
    fn focus_is_delegated_to_the_shell_boundary() {
        let host = FakeHost::with_outputs([]);
        let focused = Mutex::new(Vec::new());
        let client = SessionClient::new(&host, |id| {
            focused.lock().unwrap().push(id.to_string());
            Ok(())
        });

        client.focus("session-1").unwrap();
        assert_eq!(*focused.lock().unwrap(), vec!["session-1"]);
        assert!(host.args().is_empty(), "focus must not invoke the ato CLI");
    }

    #[test]
    fn nonzero_exit_is_not_parsed_as_json() {
        let host = FakeHost::with_outputs([CompletedCommand {
            exit_code: 9,
            stdout: br#"{"ignored":true}"#.to_vec(),
            stderr: b"operation rejected".to_vec(),
        }]);
        let err = InstalledAppsClient::new(&host).list().unwrap_err();

        assert!(matches!(
            err,
            ClientError::CommandFailed {
                exit_code: 9,
                ref stderr
            } if stderr == "operation rejected"
        ));
    }

    #[test]
    fn successful_invalid_json_is_a_protocol_error() {
        let host = FakeHost::successful_json("not-json");
        let err = SessionClient::new(&host, |_| Ok(())).list().unwrap_err();
        assert!(matches!(err, ClientError::InvalidJson(_)));
    }

    #[test]
    fn stderr_diagnostic_is_bounded() {
        let stderr = vec![b'x'; MAX_STDERR_BYTES + 50];
        let text = bounded_lossy(&stderr);
        assert!(text.ends_with('…'));
        assert!(text.len() <= MAX_STDERR_BYTES + '…'.len_utf8());
    }
}
