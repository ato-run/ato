//! Launching a `RuntimeLaunchSpecV1` whose realization is `process`.
//!
//! Deliberately generic. There is no Python in this module and there must not
//! be: the spec already says what to run, and a language-specific executor
//! would be a second place where runtime knowledge lives — the exact drift the
//! spec exists to prevent.
//!
//! Almost all of the work is already done elsewhere, so this is a translation:
//!
//! ```text
//! RuntimeLaunchSpecV1  --resolve-->  ResolvedRuntimeLaunchContext
//!                                             |
//!                                             v
//!                                        ProcessSpec
//!                                             |
//!                                             v
//!                       ato-adapter-process::ProcessAdapter (spawn, groups,
//!                       env_clear, terminate_process_tree)
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_process::{
    ProcessAdapter, ProcessHandle, ProcessSpec, force_kill_process_tree, process_group_is_alive,
};
use ato_ipc::runtime_launch::{
    LaunchRealizationV1, ReadinessV1, RuntimeLaunchSpecV1, StateAccessV1,
};

use super::resolved::ResolvedRuntimeLaunchContext;

/// Where a state attachment's working copy lives, relative to the workspace.
///
/// Inside the workspace on purpose. `StateAttachmentV1::mount_target` is an
/// absolute path *inside a guest* (`/data`), and a `process` realization has no
/// guest — it shares the Runner's filesystem with every other slot. Honouring
/// `/data` literally would mean writing to an arbitrary host directory, which
/// is exactly the thing a multi-tenant Runner must never do.
pub const STATE_WORKING_ROOT: &str = ".ato/state";

/// A convenience variable naming a state attachment's GUEST path.
///
/// No longer an ABI. The workload now finds its state at
/// `attachment.mount_target` — a real bind mount inside the sandbox — so a
/// ComputeSchema can declare `DATABASE_PATH=/data/app.sqlite` and mean it
/// under both the process and the future OCI realization. This variable is
/// kept because it costs nothing and helps a workload that would rather be
/// told, but nothing has to read it and nothing should depend on it.
pub fn state_path_env_name(state_key: &str) -> String {
    format!(
        "ATO_STATE_PATH_{}",
        state_key
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

/// Translate a validated spec plus its Runner-local resolution into the
/// argument the existing process adapter already understands.
pub fn process_spec_for(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
) -> Result<ProcessSpec> {
    spec.validate().context("runtime launch spec is invalid")?;

    let argv = match &spec.realization {
        LaunchRealizationV1::Process(process) => process.argv.clone(),
        LaunchRealizationV1::Oci(_) => {
            // Refused by name rather than ignored. An OCI spec that silently
            // launched as a bare process would run the workload without the
            // isolation the spec asked for.
            bail!(
                "runtime launch spec requests an `oci` realization, which this executor does not \
                 implement"
            );
        }
    };

    // The resolved cwd is absolute and already checked to be inside the
    // workspace. The adapter joins its own workspace argument, so hand it the
    // relative remainder rather than relying on `join` swallowing an absolute
    // path.
    let cwd = context
        .effective_cwd()
        .strip_prefix(context.workspace_root())
        .context("resolved cwd escaped the workspace after resolution")?
        .to_path_buf();

    let mut environment = context.environment_for_spawn();
    for attachment in context.state_attachments() {
        environment.insert(
            state_path_env_name(attachment.state_key()),
            attachment
                .working_copy_for_mount()
                .to_str()
                .context("state working copy path is not valid UTF-8")?
                .to_owned(),
        );
    }

    Ok(ProcessSpec {
        id: spec.context.run_id.clone(),
        command: argv,
        cwd,
        environment,
        isolated_group: true,
    })
}

/// The working-copy path a state attachment must be materialized into.
pub fn state_working_copy(workspace_root: &Path, state_key: &str) -> PathBuf {
    workspace_root.join(STATE_WORKING_ROOT).join(state_key)
}

/// A launched workload, held only for as long as the Run.
///
/// Not serializable and not persisted: an active process is not a durable
/// object. What survives a Run is its committed state revision, and the
/// receipt that names it.
pub struct LaunchedProcess {
    handle: ProcessHandle,
    run_id: String,
}

impl LaunchedProcess {
    pub fn pid(&self) -> u32 {
        self.handle.pid()
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Has the workload already exited on its own?
    ///
    /// A server that exits during readiness has failed; waiting for the
    /// timeout would turn a two-second failure into a sixty-second one and
    /// report the wrong cause.
    pub fn exited(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.handle
            .try_wait()
            .context("failed to poll the workload")
    }

    /// Stop the workload and prove it is gone.
    ///
    /// ```text
    /// SIGTERM the group
    ///   -> wait up to graceful_shutdown_ms
    ///   -> still alive? SIGKILL the group
    ///   -> wait up to force_kill_after_ms
    ///   -> reap
    ///   -> verify the subtree is gone
    /// ```
    ///
    /// Returning before the subtree is gone would be the dangerous kind of
    /// wrong: the caller packs state next, and a surviving child still writing
    /// to the state directory produces a torn SQLite file whose digest makes
    /// the corruption permanent.
    ///
    /// Termination targets the process GROUP throughout. A workload that
    /// forked children — a dev server spawning a reloader — would otherwise
    /// leave them behind.
    pub fn stop(mut self, lifecycle: &ato_ipc::runtime_launch::LifecycleV1) -> Result<StopOutcome> {
        let group = self.handle.process_group();
        let pid = self.handle.pid();

        if let Some(status) = self
            .handle
            .try_wait()
            .context("failed to reap the workload")?
        {
            return finish_stop(group, StopKind::AlreadyExited, Some(status));
        }

        self.handle
            .terminate()
            .context("failed to signal the workload")?;
        if let Some(status) = wait_for_exit(&mut self.handle, lifecycle.graceful_shutdown_ms)? {
            return finish_stop(group, StopKind::Graceful, Some(status));
        }

        force_kill_process_tree(pid, group).context("failed to force-stop the workload")?;
        let status = wait_for_exit(&mut self.handle, lifecycle.force_kill_after_ms.max(1))?;
        finish_stop(group, StopKind::Forced, status)
    }
}

/// How a workload ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// It had already exited before the Runner asked.
    AlreadyExited,
    /// It exited within `graceful_shutdown_ms` of SIGTERM.
    Graceful,
    /// It had to be killed.
    Forced,
}

/// The result of stopping, and the evidence that it worked.
#[derive(Debug, Clone)]
pub struct StopOutcome {
    pub kind: StopKind,
    pub exit_status: Option<std::process::ExitStatus>,
}

fn wait_for_exit(
    handle: &mut ProcessHandle,
    budget_ms: u64,
) -> Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    loop {
        if let Some(status) = handle.try_wait().context("failed to reap the workload")? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Refuse to report a stop the subtree did not honour.
fn finish_stop(
    group: u32,
    kind: StopKind,
    exit_status: Option<std::process::ExitStatus>,
) -> Result<StopOutcome> {
    if group != 0 {
        // The direct child is reaped by now; this asks about everything it
        // spawned. Give the kernel a moment to tear the group down first.
        for _ in 0..50 {
            if !process_group_is_alive(group) {
                return Ok(StopOutcome { kind, exit_status });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        anyhow::bail!(
            "process group {group} survived termination; refusing to pack state that a live \
             process may still be writing"
        );
    }
    Ok(StopOutcome { kind, exit_status })
}

/// Spawn the workload described by `spec`, contained.
///
/// Containment is a precondition, not an enhancement: a Runner that cannot
/// contain a workload refuses the Run rather than running it unconfined and
/// reporting success.
pub fn launch_process(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
) -> Result<LaunchedProcess> {
    let process_spec = process_spec_for(spec, context)?;
    super::sandbox::require_containment()?;

    // Every attachment's directory must exist before the workload starts. An
    // app that finds its state path missing does not wait for it; it either
    // fails or, worse, creates its own somewhere else.
    for attachment in context.state_attachments() {
        std::fs::create_dir_all(attachment.working_copy_for_mount()).with_context(|| {
            format!(
                "failed to create the working copy for state `{}`",
                attachment.state_key()
            )
        })?;
    }

    let shim = std::env::current_exe().context("cannot locate this Runner's own binary")?;
    let policy_path = context.workspace_root().join(".ato/sandbox-policy.json");
    let sandboxed = super::sandbox::sandboxed_command(
        context,
        &process_spec.command,
        &shim,
        &policy_path,
        true,
    )?;
    if let Some(parent) = policy_path.parent() {
        std::fs::create_dir_all(parent).context("failed to create the sandbox policy directory")?;
    }
    std::fs::write(
        &policy_path,
        serde_json::to_vec_pretty(&sandboxed.policy)
            .context("failed to serialize the sandbox policy")?,
    )
    .context("failed to write the sandbox policy")?;

    let adapter = ProcessAdapter::new(ProcessSpec {
        // The workload's cwd is set INSIDE the sandbox (`--chdir /app`), so
        // the outer command runs from the workspace root and the guest cwd is
        // the spec's, not the Runner's.
        cwd: PathBuf::new(),
        command: sandboxed.argv,
        environment: super::sandbox::guest_environment(context),
        ..process_spec
    })
    .context("sandboxed process spec is unusable")?;
    let handle = adapter
        .spawn(context.workspace_root())
        .context("failed to spawn the contained workload")?;
    Ok(LaunchedProcess {
        handle,
        run_id: spec.context.run_id.clone(),
    })
}

/// Which state attachments this Run may commit back.
///
/// A read-only attachment is never committed, no matter what the workload did
/// to its working copy — the copy is a convenience, not the record.
pub fn writable_state_keys(context: &ResolvedRuntimeLaunchContext) -> Vec<&str> {
    context
        .state_attachments()
        .iter()
        .filter(|attachment| attachment.access() == StateAccessV1::ReadWrite)
        .map(|attachment| attachment.state_key())
        .collect()
}

/// Block until the workload reports ready, or the spec's timeout expires.
///
/// Readiness is the workload's own signal, never "the process did not exit
/// yet". A `process` readiness is accepted as the weakest useful form, and the
/// spec says so.
pub fn wait_until_ready(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    launched: &mut LaunchedProcess,
    probe: &dyn ReadinessProbe,
) -> Result<()> {
    let (timeout_ms, target) = match &spec.readiness {
        ReadinessV1::Http {
            endpoint_name,
            path,
            timeout_ms,
        } => (
            *timeout_ms,
            Some((host_port_for(context, endpoint_name)?, path.clone())),
        ),
        ReadinessV1::Tcp {
            endpoint_name,
            timeout_ms,
        } => (
            *timeout_ms,
            Some((host_port_for(context, endpoint_name)?, String::new())),
        ),
        ReadinessV1::Process { timeout_ms } => (*timeout_ms, None),
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    // A `process` readiness has nothing to poll: the workload is ready because
    // it is up, which is already true here.
    let Some((port, path)) = target else {
        return Ok(());
    };
    loop {
        // Checked BEFORE the probe: a workload that exited has failed, and
        // waiting out the timeout would report a timeout instead of the real
        // cause.
        if let Some(status) = launched.exited()? {
            bail!(
                "workload for run {} exited before becoming ready ({status})",
                launched.run_id()
            );
        }
        match probe.probe(port, &path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    // Carry the last failure: "not ready" without a reason is
                    // the least useful diagnostic there is.
                    bail!(
                        "workload for run {} did not become ready within {timeout_ms}ms: {error}",
                        launched.run_id()
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn host_port_for(context: &ResolvedRuntimeLaunchContext, endpoint_name: &str) -> Result<u16> {
    context
        .endpoints()
        .iter()
        .find(|endpoint| endpoint.name == endpoint_name)
        .map(|endpoint| endpoint.host_port)
        .with_context(|| {
            format!("readiness names endpoint `{endpoint_name}`, which is not allocated")
        })
}

/// How readiness is observed. A trait so the executor is testable without a
/// listening socket.
pub trait ReadinessProbe {
    fn probe(&self, host_port: u16, path: &str) -> Result<(), String>;
}

/// The real probe: a loopback request to the port the Runner allocated.
pub struct LoopbackReadinessProbe {
    client: reqwest::blocking::Client,
}

impl LoopbackReadinessProbe {
    pub fn new(client: reqwest::blocking::Client) -> Self {
        Self { client }
    }
}

impl ReadinessProbe for LoopbackReadinessProbe {
    fn probe(&self, host_port: u16, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return std::net::TcpStream::connect(("127.0.0.1", host_port))
                .map(|_| ())
                .map_err(|error| error.to_string());
        }
        let response = self
            .client
            .get(format!("http://127.0.0.1:{host_port}{path}"))
            .send()
            .map_err(|error| error.to_string())?;
        let status = response.status();
        ensure_ready(status).map_err(|error| error.to_string())
    }
}

fn ensure_ready(status: reqwest::StatusCode) -> Result<()> {
    ensure!(status.is_success(), "readiness probe returned {status}");
    Ok(())
}

/// The non-secret facts about a launch that a receipt may record.
///
/// Built by hand rather than derived, so adding a field to the resolved
/// context cannot quietly publish it.
pub fn observed_launch(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    launched: &LaunchedProcess,
) -> BTreeMap<String, String> {
    let mut observed = BTreeMap::new();
    observed.insert("run_id".to_owned(), spec.context.run_id.clone());
    observed.insert(
        "compute_instance_id".to_owned(),
        spec.context.compute_instance_id.clone(),
    );
    observed.insert("pid".to_owned(), launched.pid().to_string());
    observed.insert(
        "secret_names".to_owned(),
        context.observed_secret_names().join(","),
    );
    observed.insert(
        "state".to_owned(),
        context
            .observed_state()
            .iter()
            .map(|state| {
                format!(
                    "{}@{}",
                    state.state_key,
                    state.revision_ref.unwrap_or("<new>")
                )
            })
            .collect::<Vec<_>>()
            .join(","),
    );
    observed
}

#[cfg(test)]
mod tests {
    use ato_ipc::runtime_launch::{LifecycleV1, OciRealizationV1};

    use super::super::resolved::{ResolvedSecret, ResolvedStateAttachment};
    use super::*;

    const PROCESS_FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-process.json"
    );
    const OCI_FIXTURE: &str =
        include_str!("../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-oci.json");

    fn spec(fixture: &str) -> RuntimeLaunchSpecV1 {
        RuntimeLaunchSpecV1::parse(fixture).expect("fixture is a valid spec")
    }

    struct Fixture {
        _workspace: tempfile::TempDir,
        context: ResolvedRuntimeLaunchContext,
    }

    fn context_with_state(access: StateAccessV1) -> Fixture {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().to_path_buf();
        let working = state_working_copy(&root, "app_data");
        let context = ResolvedRuntimeLaunchContext::new(
            root,
            "",
            BTreeMap::from([("PORT".to_owned(), "8000".to_owned())]),
            vec![ResolvedSecret::new("APP_SECRET_KEY", "hunter2")],
            vec![ResolvedStateAttachment::new(
                "app_data", None, working, "/data", access,
            )],
            vec![super::super::resolved::allocate_endpoint(
                &ato_ipc::runtime_launch::EndpointV1 {
                    name: "web".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8000),
                    allocation: ato_ipc::runtime_launch::EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                34567,
            )],
        )
        .expect("context resolves");
        Fixture {
            _workspace: workspace,
            context,
        }
    }

    #[test]
    fn an_oci_realization_is_refused_by_name() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let error = process_spec_for(&spec(OCI_FIXTURE), &fixture.context).unwrap_err();
        // Silently launching it as a bare process would run the workload
        // without the isolation the spec asked for.
        assert!(error.to_string().contains("oci"), "{error}");
    }

    #[test]
    fn the_workload_learns_its_state_path_by_name() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let process = process_spec_for(&spec(PROCESS_FIXTURE), &fixture.context).expect("spec");
        let declared = process
            .environment
            .get("ATO_STATE_PATH_APP_DATA")
            .expect("state path is exported");
        // `/data` is a path inside a GUEST. A process realization has no
        // guest, so honouring it literally would write to an arbitrary host
        // directory.
        assert_ne!(declared, "/data");
        assert!(declared.ends_with("/.ato/state/app_data"));
    }

    #[test]
    fn secrets_reach_the_spawn_environment_and_nothing_else() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let process = process_spec_for(&spec(PROCESS_FIXTURE), &fixture.context).expect("spec");
        assert_eq!(
            process
                .environment
                .get("APP_SECRET_KEY")
                .map(String::as_str),
            Some("hunter2")
        );
        // ...but not into anything a receipt records.
        let launched_observation = fixture.context.observed_secret_names();
        assert_eq!(launched_observation, vec!["APP_SECRET_KEY"]);
        assert!(!format!("{:?}", fixture.context).contains("hunter2"));
    }

    #[test]
    fn a_read_only_attachment_is_never_committed() {
        let writable = context_with_state(StateAccessV1::ReadWrite);
        assert_eq!(writable_state_keys(&writable.context), vec!["app_data"]);

        let read_only = context_with_state(StateAccessV1::ReadOnly);
        // The working copy is a convenience, not the record: whatever the
        // workload wrote into it is not a revision.
        assert!(writable_state_keys(&read_only.context).is_empty());
    }

    #[test]
    fn readiness_names_an_endpoint_that_must_be_allocated() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let mut spec = spec(PROCESS_FIXTURE);
        spec.readiness = ReadinessV1::Http {
            endpoint_name: "nonexistent".to_owned(),
            path: "/health".to_owned(),
            timeout_ms: 10,
        };
        let mut launched = LaunchedProcess {
            handle: spawn_true(),
            run_id: spec.context.run_id.clone(),
        };
        struct NeverProbed;
        impl ReadinessProbe for NeverProbed {
            fn probe(&self, _port: u16, _path: &str) -> Result<(), String> {
                panic!("an unallocated endpoint must not be probed")
            }
        }
        let error =
            wait_until_ready(&spec, &fixture.context, &mut launched, &NeverProbed).unwrap_err();
        assert!(error.to_string().contains("not allocated"), "{error}");
        let _ = launched.stop(&LifecycleV1 {
            graceful_shutdown_ms: 100,
            force_kill_after_ms: 200,
        });
    }

    #[test]
    fn a_workload_that_never_reports_ready_fails_rather_than_hangs() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let mut spec = spec(PROCESS_FIXTURE);
        spec.readiness = ReadinessV1::Http {
            endpoint_name: "web".to_owned(),
            path: "/health".to_owned(),
            timeout_ms: 60,
        };
        let mut launched = LaunchedProcess {
            handle: spawn_sleep(),
            run_id: spec.context.run_id.clone(),
        };
        struct AlwaysRefused;
        impl ReadinessProbe for AlwaysRefused {
            fn probe(&self, _port: u16, _path: &str) -> Result<(), String> {
                Err("connection refused".to_owned())
            }
        }
        let error =
            wait_until_ready(&spec, &fixture.context, &mut launched, &AlwaysRefused).unwrap_err();
        // The last probe failure is carried, because "not ready" without a
        // reason is the least useful diagnostic there is.
        assert!(error.to_string().contains("connection refused"), "{error}");
        launched
            .stop(&LifecycleV1 {
                graceful_shutdown_ms: 500,
                force_kill_after_ms: 1000,
            })
            .expect("stops");
    }

    #[test]
    fn a_launched_workload_actually_runs_and_stops() {
        if !super::super::sandbox::containment_available() {
            // Not a skip worth hiding: this Runner cannot contain a workload,
            // so it must not launch one. The behaviour under test only exists
            // on a host that can (see the staging acceptance).
            eprintln!("skipping: `bwrap` is not available, so no workload may be launched here");
            return;
        }
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let mut spec = spec(PROCESS_FIXTURE);
        spec.realization =
            LaunchRealizationV1::Process(ato_ipc::runtime_launch::ProcessRealizationV1 {
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
            });
        let launched = launch_process(&spec, &fixture.context).expect("launches");
        assert!(launched.pid() > 0);
        // The attachment directory exists BEFORE the workload starts, so an
        // app cannot mistake a missing path for an empty one.
        assert!(state_working_copy(fixture.context.workspace_root(), "app_data").is_dir());

        let observed = observed_launch(&spec, &fixture.context, &launched);
        assert_eq!(
            observed.get("state").map(String::as_str),
            Some("app_data@<new>")
        );
        assert!(!format!("{observed:?}").contains("hunter2"));

        launched
            .stop(&LifecycleV1 {
                graceful_shutdown_ms: 2_000,
                force_kill_after_ms: 4_000,
            })
            .expect("stops");
    }

    #[test]
    fn an_oci_spec_is_refused_before_anything_is_spawned() {
        let fixture = context_with_state(StateAccessV1::ReadWrite);
        let mut spec = spec(PROCESS_FIXTURE);
        spec.realization = LaunchRealizationV1::Oci(OciRealizationV1 {
            image_digest_ref: format!("sha256:{}", "ab".repeat(32)),
            argv: None,
            working_dir: None,
        });
        assert!(launch_process(&spec, &fixture.context).is_err());
    }

    fn spawn_true() -> ProcessHandle {
        ProcessAdapter::new(ProcessSpec {
            id: "probe".to_owned(),
            command: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()],
            cwd: PathBuf::new(),
            environment: BTreeMap::new(),
            isolated_group: true,
        })
        .expect("spec")
        .spawn(Path::new("/tmp"))
        .expect("spawns")
    }

    fn spawn_sleep() -> ProcessHandle {
        spawn_true()
    }
}
