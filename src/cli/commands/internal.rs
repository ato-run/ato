use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::info;

use nacelle::launcher::source::{SourceRuntime, SourceRuntimeConfig};
use nacelle::launcher::{IsolationPolicy, LaunchRequest, Runtime, SourceTarget};

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

/// Request envelope for exec command
#[derive(Debug, Deserialize)]
pub struct ExecEnvelope {
    pub spec_version: String,
    pub workload: WorkloadSpec,
    #[serde(default)]
    #[allow(dead_code)]
    pub interactive: bool,
    /// Environment variables to pass to the workload
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
}

#[derive(Debug, Deserialize)]
pub struct WorkloadSpec {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub kind: String,
    pub manifest: Option<PathBuf>,
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
}

pub async fn execute(args: InternalArgs) -> Result<()> {
    // Internal interface must keep stdout machine-clean (JSON only).
    // Mark internal mode so shared helpers can route progress/logs to stderr.
    std::env::set_var("NACELLE_INTERNAL", "1");

    match args.command {
        InternalCommand::Features => handle_features(args.input).await,
        InternalCommand::Exec => handle_exec(args.input).await,
    }
}

async fn handle_features(input: String) -> Result<()> {
    let spec_version = parse_spec_version(&input).unwrap_or_else(|| "0.1.0".to_string());

    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let commit = std::env::var("GIT_COMMIT").ok();

    #[cfg(target_os = "macos")]
    let sandbox = vec!["macos-seatbelt".to_string()];
    #[cfg(target_os = "linux")]
    let sandbox = vec!["linux-landlock".to_string()];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let sandbox: Vec<String> = Vec::new();

    let data = FeaturesData {
        engine: EngineInfo {
            name: "nacelle".to_string(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            platform,
            commit,
        },
        capabilities: Capabilities {
            workloads: vec!["source".to_string(), "bundle".to_string()],
            languages: vec!["python".to_string()],
            sandbox,
            socket_activation: true,
            jit_provisioning: true,
        },
    };

    write_ok(spec_version, data);
    Ok(())
}

fn parse_spec_version(input: &str) -> Option<String> {
    let raw = read_input(input).ok()?;
    let env: Envelope = serde_json::from_str(&raw).ok()?;
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
fn convert_isolation_config(config: &IsolationConfig, source_dir: &PathBuf) -> IsolationPolicy {
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
    read_write_paths.push(source_dir.clone());

    IsolationPolicy {
        sandbox_enabled: config.sandbox,
        read_only_paths,
        read_write_paths,
        network_enabled: config.network.enabled,
        egress_allow: config.network.egress_allow.clone(),
    }
}

/// Handle exec command - launch a workload from manifest
async fn handle_exec(input: String) -> Result<()> {
    let raw = read_input(&input)?;
    let envelope: ExecEnvelope =
        serde_json::from_str(&raw).context("Failed to parse exec request JSON")?;

    info!("Received exec request: {:?}", envelope.workload);

    // Get manifest path
    let manifest_path = envelope
        .workload
        .manifest
        .ok_or_else(|| anyhow::anyhow!("manifest path is required"))?;

    if !manifest_path.exists() {
        anyhow::bail!("manifest not found: {}", manifest_path.display());
    }

    // Read and parse manifest
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: CapsuleManifest =
        toml::from_str(&manifest_content).context("Failed to parse manifest TOML")?;

    info!("Loaded manifest: {} v{}", manifest.name, manifest.version);

    // Determine source directory
    let source_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Resolve command from manifest (new unified logic)
    let resolution = resolve_execution_command(&manifest);
    info!(
        "Resolved command: executable='{}', args={:?}, language={:?}",
        resolution.executable, resolution.args, resolution.language
    );

    // Convert isolation config to IsolationPolicy
    let isolation_policy = convert_isolation_config(&manifest.isolation, &source_dir);
    info!(
        "Isolation policy: sandbox={}, network={}, read_only={:?}, read_write={:?}",
        isolation_policy.sandbox_enabled,
        isolation_policy.network_enabled,
        isolation_policy.read_only_paths,
        isolation_policy.read_write_paths
    );

    // Determine dev_mode: if sandbox is explicitly disabled in manifest, use dev_mode
    // Otherwise, respect the DEV_MODE env var or default to production
    let is_dev_mode = !isolation_policy.sandbox_enabled
        || std::env::var("NACELLE_DEV_MODE").is_ok();

    let source_target = SourceTarget {
        language: resolution.language.unwrap_or_else(|| "generic".to_string()),
        version: manifest.language.as_ref().and_then(|l| l.version.clone()),
        entrypoint: resolution.entrypoint_file.clone(),
        dependencies: None,
        args: vec![],
        source_dir: source_dir.clone(),
        cmd: Some(resolution.full_command),
        dev_mode: is_dev_mode,
        isolation: Some(isolation_policy),
    };

    // Create runtime config
    let config = SourceRuntimeConfig {
        dev_mode: is_dev_mode,
        log_dir: std::env::temp_dir().join("nacelle-logs"),
        state_dir: std::env::temp_dir().join("nacelle-state"),
        sidecar_config: None,
    };

    // Create runtime
    let runtime = SourceRuntime::new(config);

    // Create launch request with environment variables
    let workload_id = format!("exec-{}", std::process::id());
    let request = LaunchRequest {
        workload_id: &workload_id,
        bundle_root: source_dir.clone(),
        env: envelope.env.clone(),
        args: None,
        source_target: Some(source_target.clone()),
        socket_manager: None,
    };

    // Launch the workload
    info!("Launching workload with env: {:?}", envelope.env);
    let result = runtime
        .launch(request)
        .await
        .map_err(|e| anyhow::anyhow!("Launch failed: {:?}", e))?;

    let pid = result.pid;
    info!("Launched with PID: {:?}", pid);

    // Write success response IMMEDIATELY so Ato Desktop can capture the PID
    // This must happen before we block on waiting
    #[derive(Serialize)]
    struct ExecResult {
        pid: Option<u32>,
        log_path: Option<String>,
    }

    write_ok(
        envelope.spec_version.clone(),
        ExecResult {
            pid,
            log_path: result.log_path.as_ref().map(|p| p.display().to_string()),
        },
    );

    // Flush stdout to ensure Ato Desktop receives the response
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // SUPERVISOR MODE: Wait for child process with proper log forwarding
    // This keeps nacelle alive so:
    // 1. stderr pipe stays open for log forwarding to Ato Desktop
    // 2. Process group (PGID) is maintained for killpg()
    // 3. Ato Desktop can detect termination via pipe closure
    if let Some(child_pid) = pid {
        eprintln!(
            "[nacelle] Supervisor mode: waiting for child PID {} to terminate...",
            child_pid
        );

        // Try to get the async child handle (for dev mode)
        if let Some(mut child) = runtime.take_async_child(&workload_id).await {
            // Take stdout and stderr for forwarding
            let child_stdout = child.stdout.take();
            let child_stderr = child.stderr.take();

            // Spawn log forwarding tasks
            // Forward stdout to nacelle's stderr (so Ato Desktop sees it)
            if let Some(stdout) = child_stdout {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        eprintln!("[stdout] {}", line);
                    }
                });
            }

            // Forward stderr to nacelle's stderr
            if let Some(stderr) = child_stderr {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        eprintln!("[stderr] {}", line);
                    }
                });
            }

            // Wait for the child process asynchronously (non-blocking!)
            let exit_code = match child.wait().await {
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    eprintln!(
                        "[nacelle] Child process {} exited with code {}",
                        child_pid, code
                    );
                    code
                }
                Err(e) => {
                    eprintln!("[nacelle] Error waiting for child: {}", e);
                    1
                }
            };

            // Give log forwarding tasks a moment to flush
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Exit with the child's exit code
            std::process::exit(exit_code);
        } else {
            // Fallback for sync children (sandbox modes): poll using kill(pid, 0)
            eprintln!(
                "[nacelle] Using poll-based wait for PID {} (sync mode)",
                child_pid
            );
            loop {
                let status = unsafe { libc::kill(child_pid as i32, 0) };
                if status != 0 {
                    // Process has exited
                    eprintln!(
                        "[nacelle] Child process {} terminated (detected via poll)",
                        child_pid
                    );
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
        }
    }

    Ok(())
}
