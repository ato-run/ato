use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::info;

use nacelle::internal_api::{
    validate_spec_version, NacelleEvent, CURRENT_SPEC_VERSION, NEXT_SPEC_VERSION,
};
use nacelle::launcher::environment::{
    prepare_environment, DerivedOutputMountSpec, EnvironmentPrepareRequest, EnvironmentWorkspace,
    OverlayMountSpec, RuntimeArtifactReference,
};
use nacelle::launcher::source::{
    NativeSandboxCapabilityReport, SourceRuntime, SourceRuntimeConfig,
};
use nacelle::launcher::{InjectedMount, IsolationPolicy, LaunchRequest, Runtime, SourceTarget};
use nacelle::system::sandbox::{default_shell, validate_shell};

/// Minimal manifest structure for parsing capsule.toml
#[derive(Debug, Deserialize)]
struct CapsuleManifest {
    name: String,
    version: String,
    #[serde(default)]
    execution: ExecutionConfig,
    /// Language configuration (optional, for JIT provisioning)
    #[serde(default)]
    language: Option<LanguageConfig>,
    /// Isolation/Sandbox configuration
    #[serde(default)]
    isolation: IsolationConfig,
    /// Optional readiness probe for NDJSON event streaming.
    #[serde(default)]
    readiness_probe: Option<ReadinessProbeConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct ExecutionConfig {
    /// Primary entrypoint - can be a file (./app.py) or a command (npm)
    #[serde(default)]
    entrypoint: String,
    /// Additional command arguments (e.g., "run dev" for npm)
    #[serde(default)]
    command: Option<String>,
    /// Explicit runtime type (source, wasm, oci)
    #[serde(default)]
    #[allow(dead_code)]
    runtime: Option<String>,
}

/// Isolation/Sandbox configuration from capsule.toml
#[derive(Debug, Deserialize, Default, Clone)]
struct IsolationConfig {
    /// Enable sandbox (default: true in production)
    #[serde(default = "default_sandbox_enabled")]
    sandbox: bool,
    /// Filesystem permissions
    #[serde(default)]
    filesystem: FilesystemPermissions,
    /// Network permissions
    #[serde(default)]
    network: NetworkPermissions,
}

fn default_sandbox_enabled() -> bool {
    true
}

/// Filesystem access permissions
#[derive(Debug, Deserialize, Default, Clone)]
struct FilesystemPermissions {
    /// Paths with read-only access
    #[serde(default)]
    read_only: Vec<String>,
    /// Paths with read-write access
    #[serde(default)]
    read_write: Vec<String>,
}

/// Network access permissions
#[derive(Debug, Deserialize, Default, Clone)]
struct NetworkPermissions {
    /// Enable network access (default: true)
    #[serde(default = "default_network_enabled")]
    enabled: bool,
    /// Allowed egress domains (allowlist mode)
    #[serde(default)]
    egress_allow: Vec<String>,
}

fn default_network_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
struct ReadinessProbeConfig {
    port: String,
    #[serde(default)]
    http_get: Option<String>,
    #[serde(default)]
    tcp_connect: Option<String>,
    #[serde(default = "default_readiness_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_readiness_interval_ms")]
    interval_ms: u64,
}

fn default_readiness_timeout_ms() -> u64 {
    30_000
}

fn default_readiness_interval_ms() -> u64 {
    200
}

/// Language configuration for JIT provisioning
#[derive(Debug, Deserialize)]
struct LanguageConfig {
    /// Language name (python, node, etc.)
    #[serde(default)]
    language: Option<String>,
    /// Version constraint
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Envelope {
    pub spec_version: String,
}

/// Terminal configuration for interactive PTY sessions
#[derive(Debug, Deserialize, Clone)]
pub struct TerminalConfig {
    /// Initial terminal columns
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// Initial terminal rows
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Shell executable path; validated against allowlist
    pub shell: Option<String>,
    /// Environment variable filter mode: "safe" | "minimal" | "passthrough"
    #[serde(default = "default_env_filter")]
    pub env_filter: String,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}
fn default_env_filter() -> String {
    "safe".to_string()
}

/// Request envelope for exec command
#[derive(Debug, Deserialize)]
pub struct ExecEnvelope {
    pub spec_version: String,
    pub workload: WorkloadSpec,
    /// When true, nacelle spawns the workload in an interactive PTY session
    #[serde(default)]
    pub interactive: bool,
    /// Terminal configuration (only meaningful when interactive = true)
    #[serde(default)]
    pub terminal: Option<TerminalConfig>,
    /// Environment variables to pass to the workload
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
    /// IPC environment variables injected by ato-cli (IPC Broker).
    /// nacelle transparently passes these to the child process without
    /// interpreting them (Smart Build, Dumb Runtime).
    #[serde(default)]
    pub ipc_env: Option<Vec<(String, String)>>,
    /// IPC socket paths that must be allowed through the Sandbox.
    /// ato-cli generates these paths; nacelle adds them to the
    /// Sandbox policy's read-write list.
    #[serde(default)]
    pub ipc_socket_paths: Option<Vec<String>>,
    /// Requested working directory for the process.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Additional host mounts injected by ato-cli.
    #[serde(default)]
    pub mounts: Vec<ExecMount>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct ExecEnvelopeV2 {
    pub spec_version: String,
    pub workload: WorkloadSpecV2,
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub ipc_env: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub ipc_socket_paths: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct WorkloadSpecV2 {
    #[serde(rename = "type")]
    pub kind: String,
    pub environment_spec: CapsuleEnvironmentSpec,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct CapsuleEnvironmentSpec {
    pub lower_source: LowerSourceSpec,
    #[serde(default)]
    pub upper_overlays: Vec<OverlayMountSpec>,
    #[serde(default)]
    pub derived_outputs: Vec<DerivedOutputMountSpec>,
    #[serde(default)]
    pub runtime_artifacts: Vec<RuntimeArtifactReference>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct LowerSourceSpec {
    pub manifest: PathBuf,
}

#[derive(Debug)]
enum ParsedExecRequest {
    V1(ExecEnvelope),
    V2(ExecEnvelopeV2),
}

#[derive(Debug, Serialize)]
struct ExecResult {
    pid: Option<u32>,
    log_path: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExecMount {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Deserialize)]
pub struct WorkloadSpec {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub kind: String,
    pub manifest: Option<PathBuf>,
    /// Explicit command to run inside the sandbox (shell workload only).
    /// When present with `type: "shell"`, nacelle runs this command instead
    /// of launching an interactive shell. Allows non-interactive sandboxed
    /// execution of share-run entries (e.g. `["python", "main.py"]`).
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct InternalArgs {
    pub input: String,
    pub command: InternalCommand,
}

#[derive(Debug)]
pub enum InternalCommand {
    Features,
    Exec,
    Pack,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ErrorBody {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Response<T> {
    ok: bool,
    spec_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    data: Option<T>,
}

/// Streaming event emitted on stdout during exec for ato-cli consumption.
///
/// After the initial `Response<ExecResult>` (which carries the PID), nacelle
/// may emit zero or more `NacelleEvent` lines (one JSON object per line).
/// ato-cli reads these to track IPC readiness, service health, etc.
///
/// # Wire Format
/// ```json
/// {"event":"ipc_ready","service":"llm-service","endpoint":"unix:///tmp/capsule-ipc/llm.sock","port":54321}
/// ```
#[derive(Debug, Serialize)]
struct FeaturesData {
    engine: EngineInfo,
    capabilities: Capabilities,
}

#[derive(Debug, Serialize)]
struct EngineInfo {
    name: String,
    engine_version: String,
    platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct Capabilities {
    workloads: Vec<String>,
    languages: Vec<String>,
    sandbox: Vec<String>,
    socket_activation: bool,
    jit_provisioning: bool,
    /// Whether this engine supports IPC socket path injection into Sandbox.
    /// When true, ato-cli can pass `ipc_socket_paths` in the exec request
    /// and nacelle will add them to the Sandbox policy's allow-list.
    ipc_sandbox: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FeatureCapabilityReport {
    languages: Vec<String>,
    sandbox: Vec<String>,
    ipc_sandbox: bool,
}

#[derive(Debug)]
enum ManagedChild {
    Async(tokio::process::Child),
    Sync(std::process::Child),
}

#[derive(Debug)]
enum ReadinessOutcome {
    Ready,
    Exited(std::process::ExitStatus),
}

pub async fn execute(args: InternalArgs) -> Result<()> {
    // Internal interface must keep stdout machine-clean (JSON only).
    // Mark internal mode so shared helpers can route progress/logs to stderr.
    std::env::set_var("NACELLE_INTERNAL", "1");

    let raw = read_input(&args.input)?;
    let spec_version =
        parse_spec_version_from_raw(&raw).unwrap_or_else(|| CURRENT_SPEC_VERSION.to_string());

    let result = match args.command {
        InternalCommand::Features => handle_features(&raw).await,
        InternalCommand::Exec => handle_exec(&raw).await,
        InternalCommand::Pack => handle_pack(&raw).await,
    };

    if let Err(err) = result {
        write_error(
            spec_version,
            classify_error_code(&err),
            err.to_string(),
            None,
        );
        return Err(err);
    }

    Ok(())
}

async fn handle_features(raw: &str) -> Result<()> {
    let spec_version =
        parse_spec_version_from_raw(raw).unwrap_or_else(|| CURRENT_SPEC_VERSION.to_string());
    validate_spec_version(&spec_version).map_err(anyhow::Error::msg)?;

    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let commit = std::env::var("GIT_COMMIT").ok();
    let report = current_feature_capability_report();

    let data = FeaturesData {
        engine: EngineInfo {
            name: "nacelle".to_string(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            platform,
            commit,
        },
        capabilities: capabilities_from_report(report),
    };

    write_ok(spec_version, data);
    Ok(())
}

async fn handle_pack(raw: &str) -> Result<()> {
    let spec_version =
        parse_spec_version_from_raw(raw).unwrap_or_else(|| CURRENT_SPEC_VERSION.to_string());
    validate_spec_version(&spec_version).map_err(anyhow::Error::msg)?;
    anyhow::bail!("internal pack is not supported by nacelle. Packaging/build is owned by ato-cli");
}

fn current_feature_capability_report() -> FeatureCapabilityReport {
    let NativeSandboxCapabilityReport {
        backends,
        ipc_sandbox,
    } = SourceRuntime::native_sandbox_capability_report();

    FeatureCapabilityReport {
        languages: SourceRuntime::supported_languages(),
        sandbox: backends,
        ipc_sandbox,
    }
}

fn capabilities_from_report(report: FeatureCapabilityReport) -> Capabilities {
    let ipc_sandbox = !report.sandbox.is_empty() && report.ipc_sandbox;

    Capabilities {
        workloads: vec!["source".to_string(), "bundle".to_string()],
        languages: report.languages,
        sandbox: report.sandbox,
        socket_activation: true,
        jit_provisioning: true,
        ipc_sandbox,
    }
}

fn parse_spec_version_from_raw(raw: &str) -> Option<String> {
    let env: Envelope = serde_json::from_str(raw).ok()?;
    Some(env.spec_version)
}

fn read_input(input: &str) -> Result<String> {
    if input == "-" {
        let mut buf = String::new();
        let mut stdin = std::io::stdin();
        stdin
            .read_to_string(&mut buf)
            .context("Failed to read stdin")?;
        if buf.trim().is_empty() {
            return Ok("{}".to_string());
        }
        return Ok(buf);
    }

    let path = PathBuf::from(input);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read input file: {}", path.display()))?;
    Ok(content)
}

fn write_ok<T: Serialize>(spec_version: String, data: T) {
    let resp = Response {
        ok: true,
        spec_version,
        error: None,
        data: Some(data),
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}

fn write_error(
    spec_version: String,
    code: &str,
    message: String,
    details: Option<serde_json::Value>,
) {
    let resp: Response<serde_json::Value> = Response {
        ok: false,
        spec_version,
        error: Some(ErrorBody {
            code: code.to_string(),
            message,
            details,
        }),
        data: None,
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}

fn classify_error_code(err: &anyhow::Error) -> &'static str {
    let text = err.to_string();
    if text.contains("Unsupported spec_version") || text.contains("internal pack is not supported")
    {
        "UNSUPPORTED"
    } else if text.contains("Failed to parse")
        || text.contains("manifest path is required")
        || text.contains("manifest not found")
        || text.contains("Invalid readiness probe port")
    {
        "INVALID_INPUT"
    } else if text.contains("Policy") || text.contains("sandbox") {
        "POLICY_VIOLATION"
    } else {
        "INTERNAL"
    }
}

impl ManagedChild {
    fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        match self {
            ManagedChild::Async(child) => child.stdout.take(),
            ManagedChild::Sync(_) => None,
        }
    }

    fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        match self {
            ManagedChild::Async(child) => child.stderr.take(),
            ManagedChild::Sync(_) => None,
        }
    }

    async fn poll_exit(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            ManagedChild::Async(child) => child.try_wait(),
            ManagedChild::Sync(child) => child.try_wait(),
        }
    }

    async fn kill(&mut self) -> std::io::Result<()> {
        match self {
            ManagedChild::Async(child) => child.kill().await,
            ManagedChild::Sync(child) => child.kill(),
        }
    }
}

fn start_log_forwarding(child: &mut ManagedChild) {
    if let Some(stdout) = child.take_stdout() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("[stdout] {}", line);
            }
        });
    }

    if let Some(stderr) = child.take_stderr() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("[stderr] {}", line);
            }
        });
    }
}

fn emit_service_exited(service: &str, status: &std::process::ExitStatus) {
    NacelleEvent::ServiceExited {
        service: service.to_string(),
        exit_code: status.code(),
    }
    .emit();
}

fn emit_service_ready(service: &str) {
    NacelleEvent::IpcReady {
        service: service.to_string(),
        endpoint: "command://ready".to_string(),
        port: None,
    }
    .emit();
}

fn readiness_endpoint(
    probe: &ReadinessProbeConfig,
    ipc_socket_paths: &[PathBuf],
) -> (String, Option<u16>) {
    if let Some(socket_path) = ipc_socket_paths.first() {
        return (format!("unix://{}", socket_path.display()), None);
    }

    let port = probe.port.trim().parse::<u16>().ok();
    let host = probe.tcp_connect.as_deref().unwrap_or("127.0.0.1").trim();

    if host.contains(':') {
        return (format!("tcp://{}", host), port);
    }

    match port {
        Some(port) => (format!("tcp://{}:{}", host, port), Some(port)),
        None => (format!("tcp://{}", host), None),
    }
}

async fn wait_for_readiness_or_exit(
    child: &mut ManagedChild,
    probe: &ReadinessProbeConfig,
    ipc_socket_paths: &[PathBuf],
) -> Result<ReadinessOutcome> {
    use tokio::time::Instant;

    let deadline = Instant::now() + Duration::from_millis(probe.timeout_ms);
    let interval = Duration::from_millis(probe.interval_ms);

    loop {
        if let Some(status) = child.poll_exit().await? {
            return Ok(ReadinessOutcome::Exited(status));
        }

        if readiness_probe_ok(probe, ipc_socket_paths).await? {
            return Ok(ReadinessOutcome::Ready);
        }

        if Instant::now() >= deadline {
            let _ = child.kill().await;
            anyhow::bail!("Readiness probe timed out");
        }

        tokio::time::sleep(interval).await;
    }
}

async fn wait_for_child_exit(child: &mut ManagedChild) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.poll_exit().await? {
            return Ok(status);
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn readiness_probe_ok(
    probe: &ReadinessProbeConfig,
    ipc_socket_paths: &[PathBuf],
) -> Result<bool> {
    if !ipc_socket_paths.is_empty() {
        return Ok(ipc_socket_paths.iter().any(|path| path.exists()));
    }

    let port: u16 = probe
        .port
        .trim()
        .parse()
        .with_context(|| format!("Invalid readiness probe port: {}", probe.port))?;

    if let Some(http_get) = &probe.http_get {
        return Ok(readiness_http_ok(http_get, port).await);
    }

    let host = probe.tcp_connect.as_deref().unwrap_or("127.0.0.1").trim();
    Ok(readiness_tcp_ok(host, port).await)
}

async fn readiness_tcp_ok(host: &str, port: u16) -> bool {
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };

    timeout(Duration::from_secs(1), TcpStream::connect(addr))
        .await
        .ok()
        .and_then(|result| result.ok())
        .is_some()
}

async fn readiness_http_ok(http_get: &str, port: u16) -> bool {
    use tokio::time::timeout;

    let url = if http_get.starts_with("http://") || http_get.starts_with("https://") {
        http_get.to_string()
    } else if http_get.starts_with('/') {
        format!("http://127.0.0.1:{port}{http_get}")
    } else {
        format!("http://127.0.0.1:{port}/{http_get}")
    };

    let client = reqwest::Client::new();
    let fut = async {
        let resp = client.get(url).send().await.ok()?;
        Some(resp.status().is_success())
    };

    timeout(Duration::from_secs(2), fut)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Result of command resolution
struct CommandResolution {
    /// The executable/binary to run (e.g., "npm", "python3", "node")
    executable: String,
    /// Arguments to pass to the executable
    args: Vec<String>,
    /// The full command as a vector (executable + args)
    full_command: Vec<String>,
    /// Detected or explicit language (for toolchain selection)
    language: Option<String>,
    /// The entrypoint file (for display purposes)
    entrypoint_file: String,
}

/// Resolve execution command from manifest
///
/// This function handles various capsule.toml formats:
/// 1. `entrypoint = "npm"`, `command = "run dev"` → ["npm", "run", "dev"]
/// 2. `entrypoint = "python3 server.py"` → ["python3", "server.py"]
/// 3. `entrypoint = "./app.py"` → ["python3", "./app.py"] (detected from extension)
/// 4. `entrypoint = "./index.js"` → ["node", "./index.js"]
///
/// Language detection priority:
/// 1. Explicit `[language]` section in manifest
/// 2. Detected from executable name (npm → node, python3 → python)
/// 3. Detected from file extension (.py → python, .js → node)
/// 4. "generic" as fallback (let the command run as-is)
fn resolve_execution_command(manifest: &CapsuleManifest) -> CommandResolution {
    let entrypoint = &manifest.execution.entrypoint;
    let command = manifest.execution.command.as_deref();

    // Tokenize entrypoint (handles quoted strings properly)
    let mut tokens = shell_words_split(entrypoint);

    // Append command tokens if present
    if let Some(cmd) = command {
        if !cmd.trim().is_empty() {
            tokens.extend(shell_words_split(cmd));
        }
    }

    // Handle empty case
    if tokens.is_empty() {
        return CommandResolution {
            executable: "sh".to_string(),
            args: vec![],
            full_command: vec!["sh".to_string()],
            language: None,
            entrypoint_file: String::new(),
        };
    }

    let executable = tokens[0].clone();
    let args: Vec<String> = tokens[1..].to_vec();

    // Detect language from various sources
    let language = detect_language(manifest, &executable, &args);

    // Determine entrypoint file for display
    let entrypoint_file = find_entrypoint_file(&tokens);

    CommandResolution {
        executable: executable.clone(),
        args,
        full_command: tokens,
        language,
        entrypoint_file,
    }
}

/// Simple shell-words split (handles basic quoting)
fn shell_words_split(s: &str) -> Vec<String> {
    // Try proper shell_words parsing, fallback to whitespace split
    shell_words::split(s).unwrap_or_else(|_| s.split_whitespace().map(String::from).collect())
}

/// Detect language from manifest, executable name, or file extension
fn detect_language(
    manifest: &CapsuleManifest,
    executable: &str,
    args: &[String],
) -> Option<String> {
    // Priority 1: Explicit [language] section
    if let Some(ref lang_config) = manifest.language {
        if let Some(ref lang) = lang_config.language {
            return Some(normalize_language(lang));
        }
    }

    // Priority 2: Detect from executable name
    if let Some(lang) = detect_language_from_executable(executable) {
        return Some(lang);
    }

    // Priority 3: Detect from file extension in args
    for arg in args {
        if let Some(lang) = detect_language_from_extension(arg) {
            return Some(lang);
        }
    }

    // Priority 4: Detect from entrypoint if it's a file
    if let Some(lang) = detect_language_from_extension(executable) {
        return Some(lang);
    }

    // No language detected - will run as "generic" (raw command execution)
    None
}

/// Normalize language name to canonical form
fn normalize_language(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "python3" | "python" | "py" => "python".to_string(),
        "node" | "nodejs" | "js" => "node".to_string(),
        "ruby" | "rb" => "ruby".to_string(),
        "deno" | "ts" => "deno".to_string(),
        other => other.to_string(),
    }
}

/// Detect language from executable/command name
fn detect_language_from_executable(executable: &str) -> Option<String> {
    let basename = std::path::Path::new(executable)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| executable.to_string());

    match basename.to_lowercase().as_str() {
        // Python ecosystem
        "python" | "python3" | "python3.11" | "python3.12" | "pip" | "uv" => {
            Some("python".to_string())
        }
        // Node.js ecosystem
        "node" | "npm" | "yarn" | "pnpm" | "npx" => Some("node".to_string()),
        // Other runtimes
        "bun" => Some("bun".to_string()),
        "deno" => Some("deno".to_string()),
        "ruby" | "bundle" | "rake" => Some("ruby".to_string()),
        "go" => Some("go".to_string()),
        "cargo" | "rustc" => Some("rust".to_string()),
        _ => None,
    }
}

/// Detect language from file extension
fn detect_language_from_extension(path: &str) -> Option<String> {
    let path = std::path::Path::new(path);
    let ext = path.extension()?.to_string_lossy().to_lowercase();

    match ext.as_str() {
        "py" => Some("python".to_string()),
        "js" | "mjs" | "cjs" => Some("node".to_string()),
        "ts" | "tsx" => Some("node".to_string()), // TypeScript via node/bun/deno
        "rb" => Some("ruby".to_string()),
        "go" => Some("go".to_string()),
        "rs" => Some("rust".to_string()),
        _ => None,
    }
}

/// Find the most likely entrypoint file from command tokens
fn find_entrypoint_file(tokens: &[String]) -> String {
    // Look for file-like arguments (has extension or starts with ./)
    for token in tokens.iter().skip(1) {
        if token.starts_with("./") || token.starts_with("../") {
            return token.clone();
        }
        if std::path::Path::new(token).extension().is_some() {
            return token.clone();
        }
    }
    // Fallback: use first token
    tokens.first().cloned().unwrap_or_default()
}

/// Convert IsolationConfig from capsule.toml to IsolationPolicy for runtime
fn convert_isolation_config(
    config: &IsolationConfig,
    source_dir: &std::path::Path,
) -> IsolationPolicy {
    // Convert string paths to PathBuf, resolving relative paths against source_dir
    let resolve_path = |path: &str| -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            source_dir.join(p)
        }
    };

    // Always include source_dir as read-only at minimum
    let mut read_only_paths: Vec<PathBuf> = config
        .filesystem
        .read_only
        .iter()
        .map(|p| resolve_path(p))
        .collect();

    // Add essential system paths for read-only access
    read_only_paths.extend([
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/etc/ssl"),
        #[cfg(target_os = "macos")]
        PathBuf::from("/System"),
        #[cfg(target_os = "macos")]
        PathBuf::from("/Library"),
    ]);

    // Convert read-write paths
    let mut read_write_paths: Vec<PathBuf> = config
        .filesystem
        .read_write
        .iter()
        .map(|p| resolve_path(p))
        .collect();

    // Always allow tmp directories
    read_write_paths.extend([
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        #[cfg(target_os = "macos")]
        PathBuf::from("/private/tmp"),
    ]);

    // Source directory should be read-write by default (for dev mode)
    // In production, this could be read-only with explicit write paths
    read_write_paths.push(source_dir.to_path_buf());

    IsolationPolicy {
        sandbox_enabled: config.sandbox,
        read_only_paths,
        read_write_paths,
        network_enabled: config.network.enabled,
        egress_allow: config.network.egress_allow.clone(),
        egress_id_allow: vec![],
    }
}

fn merge_workload_env(
    env: Option<Vec<(String, String)>>,
    ipc_env: Option<Vec<(String, String)>>,
) -> Vec<(String, String)> {
    let mut merged_env = env.unwrap_or_default();
    if let Some(ipc_env) = ipc_env {
        merged_env.extend(ipc_env);
    }
    merged_env
}

fn parse_ipc_socket_paths(paths: Option<Vec<String>>) -> Vec<PathBuf> {
    paths
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn emit_execution_completed(
    service: &str,
    prepared: &EnvironmentWorkspace,
    exit_code: Option<i32>,
) -> Result<()> {
    NacelleEvent::ExecutionCompleted {
        service: service.to_string(),
        run_id: prepared.run_id.clone(),
        derived_output_path: prepared.primary_derived_output_path(),
        exported_artifacts: prepared.exported_artifacts()?,
        cleanup_policy_applied: prepared.cleanup_policy().as_str().to_string(),
        exit_code,
    }
    .emit();
    Ok(())
}

fn prepare_v1_launch(envelope: ExecEnvelope) -> Result<EnvironmentWorkspace> {
    let manifest_path = envelope
        .workload
        .manifest
        .ok_or_else(|| anyhow::anyhow!("manifest path is required"))?;
    let merged_env = merge_workload_env(envelope.env, envelope.ipc_env);
    let ipc_socket_paths = parse_ipc_socket_paths(envelope.ipc_socket_paths);
    let injected_mounts = envelope
        .mounts
        .into_iter()
        .map(|mount| InjectedMount {
            source: PathBuf::from(mount.source),
            target: PathBuf::from(mount.target),
            readonly: mount.readonly,
        })
        .collect();

    EnvironmentWorkspace::for_manifest(
        format!("exec-{}", std::process::id()),
        envelope.spec_version,
        manifest_path,
        envelope.cwd.map(PathBuf::from),
        merged_env,
        ipc_socket_paths,
        injected_mounts,
    )
}

fn prepare_v2_launch(envelope: ExecEnvelopeV2) -> Result<EnvironmentWorkspace> {
    if envelope.workload.kind != "source" {
        anyhow::bail!("unsupported v2 workload type: {}", envelope.workload.kind);
    }

    prepare_environment(EnvironmentPrepareRequest {
        run_id: format!("exec-{}", std::process::id()),
        spec_version: envelope.spec_version,
        manifest_path: envelope.workload.environment_spec.lower_source.manifest,
        requested_cwd: envelope.cwd,
        env: merge_workload_env(envelope.env, envelope.ipc_env),
        ipc_socket_paths: parse_ipc_socket_paths(envelope.ipc_socket_paths),
        injected_mounts: vec![],
        overlays: envelope.workload.environment_spec.upper_overlays,
        derived_outputs: envelope.workload.environment_spec.derived_outputs,
        runtime_artifacts: envelope.workload.environment_spec.runtime_artifacts,
    })
}

/// Handle exec command - launch a workload from manifest
async fn handle_exec(raw: &str) -> Result<()> {
    let request = parse_exec_request(raw)?;

    match request {
        ParsedExecRequest::V1(envelope) => handle_exec_v1(envelope).await,
        ParsedExecRequest::V2(envelope) => handle_exec_v2(envelope).await,
    }
}

fn parse_exec_request(raw: &str) -> Result<ParsedExecRequest> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("Failed to parse exec request JSON")?;
    let spec_version = value
        .get("spec_version")
        .and_then(|value| value.as_str())
        .unwrap_or(CURRENT_SPEC_VERSION);
    validate_spec_version(spec_version).map_err(anyhow::Error::msg)?;

    if spec_version == NEXT_SPEC_VERSION {
        let envelope: ExecEnvelopeV2 =
            serde_json::from_value(value).context("Failed to deserialize exec request v2 JSON")?;
        return Ok(ParsedExecRequest::V2(envelope));
    }

    let envelope: ExecEnvelope =
        serde_json::from_value(value).context("Failed to deserialize exec request JSON")?;
    Ok(ParsedExecRequest::V1(envelope))
}

async fn handle_exec_v2(envelope: ExecEnvelopeV2) -> Result<()> {
    validate_spec_version(&envelope.spec_version).map_err(anyhow::Error::msg)?;
    let prepared = prepare_v2_launch(envelope)?;
    execute_prepared_launch(prepared, false, None).await
}

async fn handle_exec_v1(envelope: ExecEnvelope) -> Result<()> {
    validate_spec_version(&envelope.spec_version).map_err(anyhow::Error::msg)?;
    // Route shell workloads (no manifest) directly without prepare_v1_launch
    if envelope.workload.kind == "shell" && envelope.workload.manifest.is_none() {
        return handle_exec_v1_shell(envelope).await;
    }
    let interactive = envelope.interactive;
    let terminal = envelope.terminal.clone();
    let prepared = prepare_v1_launch(envelope)?;
    execute_prepared_launch(prepared, interactive, terminal).await
}

/// Launch an interactive shell session without a capsule manifest.
///
/// Used when ato-desktop or capsule-core requests a shell session via:
/// `{ "workload": { "type": "shell" }, "interactive": true, "terminal": { ... } }`
///
/// Also supports non-interactive command execution with an explicit `cmd`:
/// `{ "workload": { "type": "shell", "cmd": ["python", "main.py"] }, "cwd": "/workspace" }`
///
/// Bypasses capsule manifest requirements but still routes through nacelle's
/// sandbox (shell allowlist, env filter, output sanitizer, Seatbelt/bwrap/Landlock).
async fn handle_exec_v1_shell(envelope: ExecEnvelope) -> Result<()> {
    let has_cmd = envelope.workload.cmd.is_some();
    anyhow::ensure!(
        envelope.interactive || has_cmd,
        "type:shell workload requires interactive:true or a cmd"
    );

    let terminal = envelope.terminal.unwrap_or(TerminalConfig {
        cols: 80,
        rows: 24,
        shell: None,
        env_filter: "safe".to_string(),
    });

    // Resolve and validate shell
    let shell = match &terminal.shell {
        Some(s) => {
            validate_shell(s).with_context(|| format!("shell {s} is not in the allowlist"))?;
            s.clone()
        }
        None => default_shell(),
    };

    let source_dir = envelope.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    });

    let env_pairs = merge_workload_env(envelope.env, envelope.ipc_env);

    let source_target = SourceTarget {
        language: "shell".to_string(),
        entrypoint: shell.clone(),
        cmd: envelope.workload.cmd.clone(),
        source_dir: source_dir.clone(),
        dev_mode: true,
        isolation: None,
        interactive: envelope.interactive,
        terminal_cols: terminal.cols,
        terminal_rows: terminal.rows,
        terminal_shell: terminal.shell,
        terminal_env_filter: terminal.env_filter,
        ..SourceTarget::default()
    };

    if has_cmd {
        info!(cmd = ?envelope.workload.cmd, cwd = %source_dir.display(), "Launching shell command");
    } else {
        info!("Launching interactive shell session");
    }
    let config = SourceRuntimeConfig::default();
    let runtime = SourceRuntime::new(config);
    let run_id = format!("shell-{}", std::process::id());
    let request = LaunchRequest {
        workload_id: &run_id,
        bundle_root: source_dir,
        env: if env_pairs.is_empty() {
            None
        } else {
            Some(env_pairs)
        },
        args: None,
        source_target: Some(source_target),
        socket_manager: None,
    };

    runtime
        .launch(request)
        .await
        .map_err(|e| anyhow::anyhow!("shell launch failed: {e}"))?;

    // Non-interactive shell workloads (e.g. `ato run <share-url>` → `sh -lc "python main.py"`)
    // run via launch_direct on macOS / launch_with_bubblewrap on linux, both of which spawn the
    // child with piped stdout/stderr. Without explicit forwarding + wait, nacelle returns
    // exit 0 before the child produces any output and the piped FDs are dropped when nacelle
    // exits. Wait for the child and stream its output through nacelle's own stdio so callers
    // that spawn nacelle with Stdio::inherit() (e.g. ato-cli share executor) see the output.
    if has_cmd && !envelope.interactive {
        if let Some(mut child) = runtime.take_async_child(&run_id).await {
            use tokio::io::{copy, stderr as tokio_stderr, stdout as tokio_stdout};
            let child_stdout = child.stdout.take();
            let child_stderr = child.stderr.take();
            let stdout_task = child_stdout.map(|mut s| {
                tokio::spawn(async move {
                    let mut out = tokio_stdout();
                    let _ = copy(&mut s, &mut out).await;
                })
            });
            let stderr_task = child_stderr.map(|mut s| {
                tokio::spawn(async move {
                    let mut err = tokio_stderr();
                    let _ = copy(&mut s, &mut err).await;
                })
            });
            let status = child
                .wait()
                .await
                .map_err(|e| anyhow::anyhow!("shell wait failed: {e}"))?;
            if let Some(t) = stdout_task {
                let _ = t.await;
            }
            if let Some(t) = stderr_task {
                let _ = t.await;
            }
            if !status.success() {
                let code = status.code().unwrap_or(1);
                anyhow::bail!("shell workload exited with status {code}");
            }
        }
    }
    Ok(())
}

async fn execute_prepared_launch(
    prepared: EnvironmentWorkspace,
    interactive: bool,
    terminal: Option<TerminalConfig>,
) -> Result<()> {
    info!("Received exec request for run {}", prepared.run_id);

    let manifest_content = fs::read_to_string(&prepared.manifest_path).with_context(|| {
        format!(
            "Failed to read manifest: {}",
            prepared.manifest_path.display()
        )
    })?;
    let manifest: CapsuleManifest =
        toml::from_str(&manifest_content).context("Failed to parse manifest TOML")?;
    info!("Loaded manifest: {} v{}", manifest.name, manifest.version);

    let resolution = resolve_execution_command(&manifest);
    info!(
        "Resolved command: executable='{}', args={:?}, language={:?}",
        resolution.executable, resolution.args, resolution.language
    );

    let isolation_policy = convert_isolation_config(&manifest.isolation, &prepared.source_dir);
    let is_dev_mode =
        !isolation_policy.sandbox_enabled || std::env::var("NACELLE_DEV_MODE").is_ok();

    let (term_cols, term_rows, term_shell, term_env_filter) = terminal
        .as_ref()
        .map(|t| (t.cols, t.rows, t.shell.clone(), t.env_filter.clone()))
        .unwrap_or((80, 24, None, "safe".to_string()));

    let source_target = SourceTarget {
        language: resolution.language.unwrap_or_else(|| "generic".to_string()),
        version: manifest
            .language
            .as_ref()
            .and_then(|language| language.version.clone()),
        entrypoint: resolution.entrypoint_file.clone(),
        dependencies: None,
        args: vec![],
        source_dir: prepared.source_dir.clone(),
        requested_cwd: prepared.requested_cwd.clone(),
        cmd: Some(resolution.full_command),
        dev_mode: is_dev_mode,
        isolation: Some(isolation_policy),
        ipc_socket_paths: prepared.ipc_socket_paths.clone(),
        injected_mounts: prepared.injected_mounts.clone(),
        interactive,
        terminal_cols: term_cols,
        terminal_rows: term_rows,
        terminal_shell: term_shell,
        terminal_env_filter: term_env_filter,
    };

    let config: SourceRuntimeConfig = prepared.runtime_config(is_dev_mode);
    let runtime = SourceRuntime::new(config);

    let request = LaunchRequest {
        workload_id: &prepared.run_id,
        bundle_root: prepared.source_dir.clone(),
        env: if prepared.env.is_empty() {
            None
        } else {
            Some(prepared.env.clone())
        },
        args: None,
        source_target: Some(source_target.clone()),
        socket_manager: None,
    };

    let result = runtime
        .launch(request)
        .await
        .map_err(|err| anyhow::anyhow!("Launch failed: {:?}", err))?;

    write_ok(
        prepared.spec_version.clone(),
        ExecResult {
            pid: result.pid,
            log_path: result
                .log_path
                .as_ref()
                .map(|path| path.display().to_string()),
        },
    );

    use std::io::Write;
    let _ = std::io::stdout().flush();

    if let Some(child_pid) = result.pid {
        eprintln!(
            "[nacelle] Supervisor mode: waiting for child PID {} to terminate...",
            child_pid
        );

        let mut child = if let Some(child) = runtime.take_async_child(&prepared.run_id).await {
            ManagedChild::Async(child)
        } else if let Some(child) = runtime.take_child(&prepared.run_id) {
            ManagedChild::Sync(child)
        } else {
            anyhow::bail!("Internal exec lost child handle for PID {}", child_pid);
        };

        start_log_forwarding(&mut child);

        if let Some(probe) = manifest.readiness_probe.as_ref() {
            match wait_for_readiness_or_exit(&mut child, probe, &source_target.ipc_socket_paths)
                .await?
            {
                ReadinessOutcome::Ready => {
                    let (endpoint, port) =
                        readiness_endpoint(probe, &source_target.ipc_socket_paths);
                    NacelleEvent::IpcReady {
                        service: manifest.name.clone(),
                        endpoint,
                        port,
                    }
                    .emit();
                }
                ReadinessOutcome::Exited(status) => {
                    emit_service_exited(&manifest.name, &status);
                    prepared.sync_derived_outputs()?;
                    emit_execution_completed(&manifest.name, &prepared, status.code())?;
                    prepared.cleanup();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    std::process::exit(status.code().unwrap_or(1));
                }
            }
        } else {
            emit_service_ready(&manifest.name);
        }

        let status = wait_for_child_exit(&mut child).await?;
        emit_service_exited(&manifest.name, &status);
        prepared.sync_derived_outputs()?;
        emit_execution_completed(&manifest.name, &prepared, status.code())?;
        prepared.cleanup();
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(status.code().unwrap_or(1));
    }

    prepared.sync_derived_outputs()?;
    emit_execution_completed(&manifest.name, &prepared, None)?;
    prepared.cleanup();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests (Phase 13a: IPC support)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_envelope_without_ipc_fields() {
        let json = r#"{
            "spec_version": "0.1.0",
            "workload": { "type": "source", "manifest": "/app/capsule.toml" },
            "env": [["FOO", "bar"]]
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.spec_version, "0.1.0");
        assert!(envelope.ipc_env.is_none());
        assert!(envelope.ipc_socket_paths.is_none());
        assert_eq!(envelope.env.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_exec_envelope_with_ipc_env() {
        let json = r#"{
            "spec_version": "0.1.0",
            "workload": { "type": "source", "manifest": "/app/capsule.toml" },
            "ipc_env": [
                ["CAPSULE_IPC_GREETER_URL", "unix:///tmp/capsule-ipc/greeter.sock"],
                ["CAPSULE_IPC_GREETER_TOKEN", "tok_abc123"]
            ]
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        let ipc_env = envelope.ipc_env.unwrap();
        assert_eq!(ipc_env.len(), 2);
        assert_eq!(ipc_env[0].0, "CAPSULE_IPC_GREETER_URL");
        assert_eq!(ipc_env[0].1, "unix:///tmp/capsule-ipc/greeter.sock");
        assert_eq!(ipc_env[1].0, "CAPSULE_IPC_GREETER_TOKEN");
        assert_eq!(ipc_env[1].1, "tok_abc123");
    }

    #[test]
    fn test_exec_envelope_with_ipc_socket_paths() {
        let json = r#"{
            "spec_version": "0.1.0",
            "workload": { "type": "source", "manifest": "/app/capsule.toml" },
            "ipc_socket_paths": [
                "/tmp/capsule-ipc/greeter.sock",
                "/tmp/capsule-ipc/db-service.sock"
            ]
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        let paths = envelope.ipc_socket_paths.unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "/tmp/capsule-ipc/greeter.sock");
        assert_eq!(paths[1], "/tmp/capsule-ipc/db-service.sock");
    }

    #[test]
    fn test_exec_envelope_full_ipc_request() {
        let json = r#"{
            "spec_version": "0.1.0",
            "workload": { "type": "source", "manifest": "/app/capsule.toml" },
            "env": [["APP_PORT", "3000"]],
            "ipc_env": [
                ["CAPSULE_IPC_GREETER_URL", "unix:///tmp/capsule-ipc/greeter.sock"],
                ["CAPSULE_IPC_GREETER_TOKEN", "tok_abc123"]
            ],
            "ipc_socket_paths": [
                "/tmp/capsule-ipc/greeter.sock"
            ]
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        // Regular env
        assert_eq!(envelope.env.as_ref().unwrap().len(), 1);
        // IPC env
        assert_eq!(envelope.ipc_env.as_ref().unwrap().len(), 2);
        // IPC socket paths
        assert_eq!(envelope.ipc_socket_paths.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_capabilities_includes_ipc_sandbox() {
        let caps = capabilities_from_report(FeatureCapabilityReport {
            languages: SourceRuntime::supported_languages(),
            sandbox: vec!["macos-seatbelt".to_string()],
            ipc_sandbox: true,
        });

        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("\"ipc_sandbox\":true"));
    }

    #[test]
    fn test_capabilities_fail_closed_when_backend_unavailable() {
        let caps = capabilities_from_report(FeatureCapabilityReport {
            languages: SourceRuntime::supported_languages(),
            sandbox: Vec::new(),
            ipc_sandbox: true,
        });

        assert!(caps.sandbox.is_empty());
        assert!(!caps.ipc_sandbox);
    }

    #[test]
    fn test_readiness_probe_deserialization_defaults() {
        let manifest: CapsuleManifest = toml::from_str(
            r#"
name = "probe-app"
version = "0.1.0"

[execution]
entrypoint = "python3 server.py"

[readiness_probe]
port = "43123"
http_get = "/health"
"#,
        )
        .unwrap();

        let probe = manifest.readiness_probe.unwrap();
        assert_eq!(probe.port, "43123");
        assert_eq!(probe.http_get.as_deref(), Some("/health"));
        assert_eq!(probe.timeout_ms, 30_000);
        assert_eq!(probe.interval_ms, 200);
    }

    #[test]
    fn test_classify_internal_pack_as_unsupported() {
        let err = anyhow::anyhow!(
            "internal pack is not supported by nacelle. Packaging/build is owned by ato-cli"
        );
        assert_eq!(classify_error_code(&err), "UNSUPPORTED");
    }

    #[test]
    fn test_nacelle_event_ipc_ready_serialization() {
        let event = NacelleEvent::IpcReady {
            service: "llm-service".to_string(),
            endpoint: "unix:///tmp/capsule-ipc/llm.sock".to_string(),
            port: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"ipc_ready\""));
        assert!(json.contains("\"service\":\"llm-service\""));
        assert!(json.contains("\"endpoint\":\"unix:///tmp/capsule-ipc/llm.sock\""));
        // port is None, should be omitted
        assert!(!json.contains("\"port\""));
    }

    #[test]
    fn test_nacelle_event_ipc_ready_with_port() {
        let event = NacelleEvent::IpcReady {
            service: "db-service".to_string(),
            endpoint: "tcp://127.0.0.1:54321".to_string(),
            port: Some(54321),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"port\":54321"));
    }

    #[test]
    fn test_nacelle_event_service_exited() {
        let event = NacelleEvent::ServiceExited {
            service: "my-service".to_string(),
            exit_code: Some(1),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"service_exited\""));
        assert!(json.contains("\"exit_code\":1"));
    }

    #[test]
    fn test_nacelle_event_deserialization() {
        let json =
            r#"{"event":"ipc_ready","service":"greeter","endpoint":"unix:///tmp/test.sock"}"#;
        let event: NacelleEvent = serde_json::from_str(json).unwrap();
        match event {
            NacelleEvent::IpcReady {
                service,
                endpoint,
                port,
            } => {
                assert_eq!(service, "greeter");
                assert_eq!(endpoint, "unix:///tmp/test.sock");
                assert!(port.is_none());
            }
            _ => panic!("Expected IpcReady event"),
        }
    }

    #[test]
    fn test_shell_workload_with_cmd() {
        let json = r#"{
            "spec_version": "1.0",
            "workload": { "type": "shell", "cmd": ["python", "main.py"] },
            "interactive": true,
            "terminal": { "cols": 120, "rows": 40, "env_filter": "safe" },
            "cwd": "/tmp/workspace",
            "env": [["PYTHONPATH", "."]]
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.workload.kind, "shell");
        assert_eq!(
            envelope.workload.cmd.as_ref().unwrap(),
            &vec!["python".to_string(), "main.py".to_string()]
        );
        assert!(envelope.interactive);
        assert_eq!(envelope.cwd.as_deref(), Some("/tmp/workspace"));
    }

    #[test]
    fn test_shell_workload_cmd_without_interactive() {
        let json = r#"{
            "spec_version": "1.0",
            "workload": { "type": "shell", "cmd": ["python", "main.py"] },
            "cwd": "/tmp/workspace"
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.workload.kind, "shell");
        assert!(envelope.workload.cmd.is_some());
        assert!(!envelope.interactive);
        assert!(envelope.workload.manifest.is_none());
    }

    #[test]
    fn test_shell_workload_without_cmd_defaults_none() {
        let json = r#"{
            "spec_version": "1.0",
            "workload": { "type": "shell" },
            "interactive": true
        }"#;

        let envelope: ExecEnvelope = serde_json::from_str(json).unwrap();
        assert!(envelope.workload.cmd.is_none());
        assert!(envelope.interactive);
    }
}
