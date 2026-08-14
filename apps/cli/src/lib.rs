//! Product assembly for the Ato computation architecture.

#![deny(unsafe_op_in_unsafe_fn)]

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ato_adapter_repository::{
    CAPSULE_LOCK_FILE, CompiledRepository, LockedResolution, RepositoryOptions, compile_repository,
    compile_repository_with_lock, encode_capsule_lock, fetch_git_repository, lock_for,
    materialize_source, read_capsule_lock,
};
use ato_compose::ComposeReferences;
use ato_computation::{ComputationRef, ContentRef, SemanticsId};
use ato_kernel::{Kernel, Run};
use ato_materializer_snapshot::{
    MaterializationRef, RealizationContract, register_materialization, verify_materialization,
};
use ato_objects::{
    FsObjectStore, ObjectResolver, ReferenceRegistry, decode_bundle, encode_bundle, export_bundle,
    import_bundle, read_exact_object,
};
use ato_semantics_workspace::{
    MAX_WORKSPACE_RESIDUAL_BYTES, WORKSPACE_SEMANTICS_ID, WorkspaceReferences, WorkspaceSemantics,
    decode_workspace_residual, observe_exit,
};
use clap::{Args, Parser, Subcommand};
use nacelle::workspace_provider::NacelleWorkspaceProvider;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "ato", version, about = "Advance addressable computations")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile a repository and advance its workspace computation.
    Run(RunArgs),
    /// Pin repository resolution evidence into capsule.lock.
    Lock { target: Option<String> },
    /// Export a repository as a portable object-closure bundle.
    Encap(EncapArgs),
    /// Import and run a portable object-closure bundle.
    Decap {
        #[command(subcommand)]
        command: DecapCommand,
    },
    /// List mutable run cursors.
    Ps {
        #[arg(long)]
        json: bool,
    },
    /// Print captured output for a run cursor.
    Logs { name: String },
    /// Stop a detached run cursor.
    Stop { name: String },
    /// Capture or verify provider-owned snapshot materializations.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(default_value = ".")]
    target: String,
    #[arg(long)]
    detach: bool,
    #[arg(long, hide = true)]
    worker: bool,
    #[arg(long)]
    name: Option<String>,
    #[arg(long = "env", value_parser = parse_binding)]
    environment: Vec<(String, String)>,
    #[arg(long = "secret-ref", value_parser = parse_secret_binding)]
    secret_refs: Vec<(String, String)>,
    #[arg(long = "allow-network")]
    network_allow: Vec<String>,
    #[arg(long)]
    no_sandbox: bool,
    #[arg(last = true)]
    arguments: Vec<String>,
}

#[derive(Debug, Args)]
struct EncapArgs {
    #[arg(default_value = ".")]
    target: String,
    #[arg(short, long, default_value = "computation.capsule")]
    output: PathBuf,
}

#[derive(Subcommand)]
enum DecapCommand {
    Start {
        capsule: PathBuf,
        #[arg(long)]
        detach: bool,
        #[arg(long, hide = true)]
        worker: bool,
        #[arg(long)]
        name: Option<String>,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Attach {
        name: String,
    },
    Stop {
        name: String,
    },
}

#[derive(Subcommand)]
enum SnapshotCommand {
    Capture {
        computation: String,
        #[arg(required = true)]
        artifacts: Vec<PathBuf>,
    },
    Verify {
        materialization: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    name: String,
    head: String,
    pid: u32,
    process_start_time: String,
    process_group: u32,
    job_identity: Option<String>,
    boot_session: String,
    status: String,
    exit_code: Option<i32>,
    log: PathBuf,
}

pub fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Run(args) => run_repository(args),
        Commands::Lock { target } => lock_repository(target.as_deref().unwrap_or(".")),
        Commands::Encap(args) => encap(args),
        Commands::Decap { command } => decap(command),
        Commands::Ps { json } => list_runs(json),
        Commands::Logs { name } => print_logs(&name),
        Commands::Stop { name } => stop_run(&name),
        Commands::Snapshot { command } => snapshot(command),
    }
}

fn run_repository(args: RunArgs) -> Result<()> {
    if args.detach && !args.secret_refs.is_empty() {
        bail!("detached runs with secret bindings require a secure one-shot transport");
    }
    let source = resolve_source(&args.target)?;
    let name = safe_name(args.name.as_deref().unwrap_or("run"))?;
    if args.detach && !args.worker {
        return spawn_detached_run(&args, &name);
    }
    let _worker_job = initialize_worker_job(args.worker.then_some(name.as_str()))?;
    let objects = Arc::new(object_store()?);
    let options = RepositoryOptions {
        arguments: args.arguments,
        environment: args.environment.into_iter().collect(),
        secret_bindings: args.secret_refs.into_iter().collect(),
        network_allow: args.network_allow,
        sandbox_required: !args.no_sandbox,
        ..RepositoryOptions::default()
    };
    let compiled = match read_capsule_lock(&source)? {
        Some(lock) => compile_repository_with_lock(&source, objects.as_ref(), options, &lock)?,
        None => compile_repository(&source, objects.as_ref(), options)?,
    };
    let head = advance_workspace(compiled, objects, Some(&name))?;
    println!("{head}");
    Ok(())
}

fn lock_repository(target: &str) -> Result<()> {
    let source = resolve_source(target)?;
    let objects = Arc::new(object_store()?);
    let compiled = compile_repository(&source, objects.as_ref(), RepositoryOptions::default())?;
    let lock = lock_for(&compiled)?;
    let path = source.join(CAPSULE_LOCK_FILE);
    fs::write(&path, encode_capsule_lock(&lock)?)?;
    println!("{}", path.display());
    Ok(())
}

fn encap(args: EncapArgs) -> Result<()> {
    let source = resolve_source(&args.target)?;
    let objects = Arc::new(object_store()?);
    let compiled = compile_repository(&source, objects.as_ref(), RepositoryOptions::default())?;
    let kernel = Kernel::new(objects.clone());
    let root = kernel.seal(&compiled.computation)?;
    let references = references()?;
    let bundle = export_bundle(&root, objects.as_ref(), &references)?;
    fs::write(&args.output, encode_bundle(&bundle)?)?;
    println!("{root}");
    Ok(())
}

fn decap(command: DecapCommand) -> Result<()> {
    match command {
        DecapCommand::Start {
            capsule,
            detach,
            worker,
            name,
        } => {
            let name = safe_name(
                name.as_deref()
                    .or_else(|| capsule.file_stem().and_then(|v| v.to_str()))
                    .unwrap_or("capsule"),
            )?;
            if detach && !worker {
                return spawn_detached_decap(&capsule, &name);
            }
            let _worker_job = initialize_worker_job(worker.then_some(name.as_str()))?;
            let objects = Arc::new(object_store()?);
            let bundle = decode_bundle(&fs::read(&capsule)?)?;
            let root = import_bundle(&bundle, objects.as_ref(), &references()?)?;
            let residual = workspace_residual(&root, objects.as_ref())?;
            let compiled = CompiledRepository {
                computation: ato_objects::resolve_computation(objects.as_ref(), &root)?
                    .object()
                    .clone(),
                source: ContentRef::parse(&residual.source)?,
                evidence: ato_adapter_repository::InferenceEvidence {
                    observed_files: Vec::new(),
                    selected_toolchain: residual.toolchain.family.clone(),
                    selected_entrypoint: residual.entrypoint.clone(),
                    authoring_manifest_used: false,
                },
                resolution: LockedResolution {
                    toolchain: residual.toolchain,
                    package_manager: residual.package_manager,
                    entrypoint: residual.entrypoint,
                    working_directory: residual.working_directory,
                },
            };
            let head = advance_workspace(compiled, objects, Some(&name))?;
            println!("{head}");
            Ok(())
        }
        DecapCommand::List { json } => list_runs(json),
        DecapCommand::Attach { name } => print_logs(&name),
        DecapCommand::Stop { name } => stop_run(&name),
    }
}

fn advance_workspace(
    compiled: CompiledRepository,
    objects: Arc<FsObjectStore>,
    run_name: Option<&str>,
) -> Result<ComputationRef> {
    let workspace = materialize_run_source(
        &compiled.source,
        objects.as_ref(),
        run_name.unwrap_or("run"),
    )?;
    let provider = Arc::new(NacelleWorkspaceProvider::default());
    provider.bind_materialized_source(compiled.source, &workspace)?;
    let mut kernel = Kernel::new(objects.clone());
    kernel.register(Arc::new(WorkspaceSemantics::new(provider)))?;
    let mut run = Run {
        head: kernel.seal(&compiled.computation)?,
    };
    let record_path = run_name.map(run_record_path).transpose()?;
    if let (Some(name), Some(path)) = (run_name, record_path.as_ref()) {
        write_record(
            path,
            &RunRecord {
                name: name.to_owned(),
                head: run.head.to_string(),
                pid: std::process::id(),
                process_start_time: process_start_time(std::process::id())
                    .context("current process start time is unavailable")?,
                process_group: current_process_group()?,
                job_identity: worker_job_identity(name),
                boot_session: boot_session_identity()?,
                status: "running".to_owned(),
                exit_code: None,
                log: run_directory(name)?.join("output.log"),
            },
        )?;
    }
    let offer = kernel
        .enabled(&run.head)?
        .into_iter()
        .next()
        .context("workspace computation has no enabled transition")?;
    let result = kernel.step(&mut run, &offer);
    let exit = match result {
        Ok(_) => observe_exit(&workspace_residual(&run.head, objects.as_ref())?)
            .context("workspace computation did not expose an exit")?,
        Err(error) => {
            update_record(run_name, &run.head, "failed", None)?;
            return Err(error.into());
        }
    };
    update_record(run_name, &run.head, "exited", Some(exit))?;
    if exit != 0 {
        bail!("workspace exited with status {exit}");
    }
    Ok(run.head)
}

fn workspace_residual(
    root: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<ato_semantics_workspace::WorkspaceResidual> {
    let resolved = ato_objects::resolve_computation(objects, root)?;
    let expected = SemanticsId::parse(WORKSPACE_SEMANTICS_ID)?;
    if resolved.object().semantics != expected {
        bail!(
            "computation uses {}, expected {expected}",
            resolved.object().semantics
        );
    }
    let reference = &resolved.object().residual;
    let metadata = objects.metadata(reference)?;
    Ok(decode_workspace_residual(&read_exact_object(
        objects,
        reference,
        metadata.size,
        MAX_WORKSPACE_RESIDUAL_BYTES,
    )?)?)
}

fn materialize_run_source(
    source: &ContentRef,
    objects: &dyn ObjectResolver,
    name: &str,
) -> Result<PathBuf> {
    let run_directory = run_directory(name)?;
    fs::create_dir_all(&run_directory)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let destination = run_directory.join(format!(
        "workspace-{}-{}-{nonce}",
        &source.digest()[..12],
        std::process::id(),
    ));
    materialize_source(source, objects, &destination)?;
    Ok(destination)
}

fn snapshot(command: SnapshotCommand) -> Result<()> {
    let objects = object_store()?;
    match command {
        SnapshotCommand::Capture {
            computation,
            artifacts,
        } => {
            let computation = ComputationRef::parse(computation)?;
            let reference = register_materialization(
                &computation,
                RealizationContract::host("ato-provider-snapshot"),
                &artifacts,
                &objects,
            )?;
            println!("{}", reference.content_ref());
        }
        SnapshotCommand::Verify { materialization } => {
            let reference = MaterializationRef::parse(materialization)?;
            let computation = verify_materialization(
                &reference,
                &RealizationContract::host("ato-provider-snapshot"),
                &objects,
            )?;
            println!("{computation}");
        }
    }
    Ok(())
}

fn references() -> Result<ReferenceRegistry> {
    let mut registry = ReferenceRegistry::default();
    registry.register(Arc::new(WorkspaceReferences::default()))?;
    registry.register(Arc::new(ComposeReferences::default()))?;
    Ok(registry)
}

fn resolve_source(target: &str) -> Result<PathBuf> {
    let path = PathBuf::from(target);
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let url = if target.starts_with("github.com/") {
        format!("https://{target}.git")
    } else if target.starts_with("https://") || target.starts_with("file://") {
        target.to_owned()
    } else {
        bail!("source is neither a local path nor a supported Git URL: {target}");
    };
    let destination = ato_home()?
        .join("sources")
        .join(blake3::hash(url.as_bytes()).to_hex().to_string());
    if !destination.is_dir() {
        fs::create_dir_all(destination.parent().context("source cache has no parent")?)?;
        fetch_git_repository(&url, &destination)?;
    }
    Ok(destination)
}

fn object_store() -> Result<FsObjectStore> {
    Ok(FsObjectStore::open(ato_home()?.join("objects"))?)
}

fn ato_home() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("ATO_HOME") {
        return Ok(PathBuf::from(value));
    }
    Ok(dirs::home_dir()
        .context("home directory is unavailable")?
        .join(".ato"))
}

fn parse_binding(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "expected NAME=VALUE".to_owned())?;
    if name.is_empty() {
        return Err("binding name is empty".to_owned());
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn parse_secret_binding(value: &str) -> Result<(String, String), String> {
    let (name, binding) = parse_binding(value)?;
    if !binding.starts_with("secret://") || binding.len() == "secret://".len() {
        return Err("secret binding must be NAME=secret://...".to_owned());
    }
    Ok((name, binding))
}

fn safe_name(value: &str) -> Result<String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("run name must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(value.to_owned())
}

fn run_directory(name: &str) -> Result<PathBuf> {
    Ok(ato_home()?.join("runs").join(safe_name(name)?))
}

fn run_record_path(name: &str) -> Result<PathBuf> {
    Ok(run_directory(name)?.join("run.json"))
}

fn write_record(path: &Path, record: &RunRecord) -> Result<()> {
    fs::create_dir_all(path.parent().context("run record has no parent")?)?;
    let temporary = path.with_extension("json.new");
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_record(name: &str) -> Result<RunRecord> {
    Ok(serde_json::from_slice(&fs::read(run_record_path(name)?)?)?)
}

fn update_record(
    name: Option<&str>,
    head: &ComputationRef,
    status: &str,
    exit: Option<i32>,
) -> Result<()> {
    let Some(name) = name else {
        return Ok(());
    };
    let mut record = read_record(name)?;
    record.head = head.to_string();
    record.status = status.to_owned();
    record.exit_code = exit;
    write_record(&run_record_path(name)?, &record)
}

fn list_runs(json: bool) -> Result<()> {
    let root = ato_home()?.join("runs");
    let mut records = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path().join("run.json");
            if let Ok(bytes) = fs::read(path)
                && let Ok(mut record) = serde_json::from_slice::<RunRecord>(&bytes)
            {
                if record.status == "running" && !process_matches(&record)? {
                    record.status = "stale".to_owned();
                }
                records.push(record);
            }
        }
    }
    records.sort_by(|left, right| left.name.cmp(&right.name));
    if json {
        println!("{}", serde_json::to_string(&records)?);
    } else {
        for record in records {
            println!("{}\t{}\t{}", record.name, record.status, record.head);
        }
    }
    Ok(())
}

fn print_logs(name: &str) -> Result<()> {
    let record = read_record(name)?;
    if record.log.is_file() {
        print!("{}", fs::read_to_string(record.log)?);
    }
    Ok(())
}

fn stop_run(name: &str) -> Result<()> {
    let mut record = read_record(name)?;
    if record.status != "running" {
        return Ok(());
    }
    if !process_matches(&record)? {
        record.status = "stale".to_owned();
        write_record(&run_record_path(name)?, &record)?;
        bail!(
            "run process identity no longer matches record; refusing to stop PID {}",
            record.pid
        );
    }
    let status = stop_process_tree(&record)?;
    if !status.success() {
        bail!("failed to stop process {}", record.pid);
    }
    record.status = "stopped".to_owned();
    write_record(&run_record_path(name)?, &record)
}

fn spawn_detached_run(args: &RunArgs, name: &str) -> Result<()> {
    let log = run_directory(name)?.join("output.log");
    fs::create_dir_all(log.parent().context("log has no parent")?)?;
    let stdout = OpenOptions::new().create(true).append(true).open(&log)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["run", &args.target, "--worker", "--name", name]);
    for (key, value) in &args.environment {
        command.args(["--env", &format!("{key}={value}")]);
    }
    for (key, binding) in &args.secret_refs {
        command.args(["--secret-ref", &format!("{key}={binding}")]);
    }
    for host in &args.network_allow {
        command.args(["--allow-network", host]);
    }
    if args.no_sandbox {
        command.arg("--no-sandbox");
    }
    if !args.arguments.is_empty() {
        command.arg("--").args(&args.arguments);
    }
    configure_detached_process(&mut command);
    let child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stdout)
        .spawn()?;
    println!("started {name} (pid {})", child.id());
    Ok(())
}

fn spawn_detached_decap(capsule: &Path, name: &str) -> Result<()> {
    let log = run_directory(name)?.join("output.log");
    fs::create_dir_all(log.parent().context("log has no parent")?)?;
    let stdout = OpenOptions::new().create(true).append(true).open(&log)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["decap", "start"])
        .arg(capsule)
        .args(["--worker", "--name", name])
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stdout);
    configure_detached_process(&mut command);
    let child = command.spawn()?;
    println!("started {name} (pid {})", child.id());
    Ok(())
}

fn process_matches(record: &RunRecord) -> Result<bool> {
    if boot_session_identity()? != record.boot_session {
        return Ok(false);
    }
    Ok(process_start_time(record.pid).as_deref() == Some(record.process_start_time.as_str()))
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19).map(|value| (*value).to_owned())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_start_time(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<String> {
    let script =
        format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToUniversalTime().Ticks");
    command_output("powershell", &["-NoProfile", "-Command", &script])
}

#[cfg(target_os = "linux")]
fn boot_session_identity() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

#[cfg(target_os = "macos")]
fn boot_session_identity() -> Result<String> {
    command_output("sysctl", &["-n", "kern.boottime"])
        .context("kernel boot identity is unavailable")
}

#[cfg(windows)]
fn boot_session_identity() -> Result<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().Ticks",
        ],
    )
    .context("Windows boot identity is unavailable")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn boot_session_identity() -> Result<String> {
    bail!("boot/session identity is unavailable on this platform")
}

#[cfg(unix)]
fn current_process_group() -> Result<u32> {
    command_output(
        "ps",
        &["-o", "pgid=", "-p", &std::process::id().to_string()],
    )
    .and_then(|value| value.parse::<u32>().ok())
    .context("current process group is unavailable")
}

#[cfg(windows)]
fn current_process_group() -> Result<u32> {
    Ok(std::process::id())
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn stop_process_tree(record: &RunRecord) -> Result<std::process::ExitStatus> {
    if record.process_group != record.pid {
        bail!("run does not own an isolated process group; refusing group termination");
    }
    Ok(Command::new("kill")
        .args(["-TERM", "--", &format!("-{}", record.process_group)])
        .status()?)
}

#[cfg(windows)]
fn stop_process_tree(record: &RunRecord) -> Result<std::process::ExitStatus> {
    let expected = worker_job_identity(&record.name)
        .context("detached Windows run has no Job Object identity")?;
    if record.job_identity.as_deref() != Some(expected.as_str()) {
        bail!("run Job Object identity does not match record");
    }
    terminate_windows_job(&expected)?;
    Ok(Command::new("cmd").args(["/C", "exit", "0"]).status()?)
}

#[cfg(any(unix, windows))]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(not(windows))]
struct WorkerJob;

#[cfg(not(windows))]
fn initialize_worker_job(name: Option<&str>) -> Result<Option<WorkerJob>> {
    Ok(name.map(|_| WorkerJob))
}

#[cfg(not(windows))]
fn worker_job_identity(_name: &str) -> Option<String> {
    None
}

#[cfg(windows)]
struct WorkerJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WorkerJob {
    fn drop(&mut self) {
        // SAFETY: this guard exclusively owns the handle returned by
        // CreateJobObjectW and closes it exactly once.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn initialize_worker_job(name: Option<&str>) -> Result<Option<WorkerJob>> {
    use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::core::PCWSTR;

    let Some(name) = name else {
        return Ok(None);
    };
    let identity = worker_job_identity(name).context("invalid Windows Job Object name")?;
    let wide: Vec<u16> = identity.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated for the duration of this call and the
    // returned handle is immediately transferred to WorkerJob.
    let handle = unsafe { CreateJobObjectW(None, PCWSTR(wide.as_ptr())) }?;
    // SAFETY: both handles are valid and the Job handle remains open in the
    // returned guard for the worker's lifetime.
    unsafe { AssignProcessToJobObject(handle, GetCurrentProcess()) }?;
    Ok(Some(WorkerJob(handle)))
}

#[cfg(windows)]
fn worker_job_identity(name: &str) -> Option<String> {
    Some(format!("Local\\ato-run-{name}"))
}

#[cfg(windows)]
fn terminate_windows_job(identity: &str) -> Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::{OpenJobObjectW, TerminateJobObject};
    use windows::core::PCWSTR;

    let wide: Vec<u16> = identity.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and alive for the call.
    const JOB_OBJECT_TERMINATE_ACCESS: u32 = 0x0008;
    let handle =
        unsafe { OpenJobObjectW(JOB_OBJECT_TERMINATE_ACCESS, false, PCWSTR(wide.as_ptr())) }?;
    // SAFETY: the handle names the already identity-verified worker Job.
    let termination = unsafe { TerminateJobObject(handle, 143) };
    // SAFETY: this function owns the handle returned by OpenJobObjectW.
    let close = unsafe { CloseHandle(handle) };
    termination?;
    close?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_and_run_names_fail_closed() {
        assert_eq!(
            parse_binding("A=B").unwrap(),
            ("A".to_owned(), "B".to_owned())
        );
        assert!(parse_binding("missing").is_err());
        assert!(parse_secret_binding("API_KEY=actual-value").is_err());
        assert_eq!(
            parse_secret_binding("API_KEY=secret://profile/openai").unwrap(),
            ("API_KEY".to_owned(), "secret://profile/openai".to_owned())
        );
        assert!(safe_name("../escape").is_err());
    }

    #[test]
    fn legacy_secret_value_flag_is_rejected() {
        assert!(Cli::try_parse_from(["ato", "run", ".", "--secret", "API_KEY=value"]).is_err());
        assert!(
            Cli::try_parse_from([
                "ato",
                "run",
                ".",
                "--secret-ref",
                "API_KEY=secret://profile/openai"
            ])
            .is_ok()
        );
    }

    #[test]
    fn process_identity_rejects_a_reused_pid_record() {
        let current = RunRecord {
            name: "identity".to_owned(),
            head: format!("blake3:{}", "ab".repeat(32)),
            pid: std::process::id(),
            process_start_time: process_start_time(std::process::id()).unwrap(),
            process_group: current_process_group().unwrap(),
            job_identity: worker_job_identity("identity"),
            boot_session: boot_session_identity().unwrap(),
            status: "running".to_owned(),
            exit_code: None,
            log: PathBuf::from("unused.log"),
        };
        assert!(process_matches(&current).unwrap());

        let mut reused = current;
        reused.process_start_time.push_str("-different-process");
        assert!(!process_matches(&reused).unwrap());
    }
}
