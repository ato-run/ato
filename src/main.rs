//! Capsuled Engine - Main Entry Point
//!
//! v2.0: Hybrid Runtime Model
//! - Development Mode: capsuled run (JIT provisioning)
//! - Production Mode: Self-extracting bundle (embedded runtime)

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;

use capsuled::api_server;
use capsuled::artifact::{manager::ArtifactConfig, ArtifactManager};
use capsuled::capsule_manager::CapsuleManager;
use capsuled::hardware;
use capsuled::job_history::SqliteJobHistoryStore;
use capsuled::network::service_registry::ServiceRegistry;
use capsuled::process_supervisor::ProcessSupervisor;
use capsuled::resource::cas::LocalCasClient;
use capsuled::runtime::{ContainerRuntime, RuntimeConfig, RuntimeKind};
use capsuled::security::audit::AuditLogger;
use capsuled::security::EgressProxy;
use capsuled::storage::StorageConfig;

const DEFAULT_HTTP_PORT: u16 = 4500;
const DEFAULT_GRPC_PORT: u16 = 50051;
const DEFAULT_AUDIT_LOG_PATH: &str = "/tmp/capsuled/logs/audit.jsonl";
const DEFAULT_AUDIT_KEY_PATH: &str = "/tmp/capsuled/keys/node_key.pem";
const DEFAULT_MODELS_CACHE_DIR: &str = "/opt/models";

/// Capsuled Engine - HTTP API Server (Phase 1.5)
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// HTTP API server port
    #[arg(short, long, default_value_t = DEFAULT_HTTP_PORT)]
    port: u16,

    /// gRPC server port
    #[arg(long, default_value_t = DEFAULT_GRPC_PORT)]
    grpc_port: u16,

    /// Path to audit log file
    #[arg(long, default_value = DEFAULT_AUDIT_LOG_PATH)]
    audit_log: String,

    /// Path to node key for audit signatures
    #[arg(long, default_value = DEFAULT_AUDIT_KEY_PATH)]
    audit_key: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // v2.0: Check if running as self-extracting bundle
    if is_self_extracting_bundle()? {
        return bootstrap_bundled_runtime().await;
    }

    // Normal engine mode
    run_engine().await
}

/// v2.0: Check if this binary contains an embedded bundle
fn is_self_extracting_bundle() -> anyhow::Result<bool> {
    let exe_path = std::env::current_exe()?;
    let file_data = std::fs::read(&exe_path)?;

    let len = file_data.len();
    let magic = b"CAPSULED_V2_BUNDLE";

    if len < magic.len() + 8 {
        return Ok(false);
    }

    let magic_start = len - magic.len() - 8;
    let found_magic = &file_data[magic_start..magic_start + magic.len()];

    Ok(found_magic == magic)
}

/// v2.0: Bootstrap and run embedded runtime
async fn bootstrap_bundled_runtime() -> anyhow::Result<()> {
    use capsuled::engine::socket::{SocketConfig, SocketManager};
    use capsuled::engine::supervisor::ProcessSupervisor;

    println!("🚀 Starting capsuled bundle...");

    // Extract bundle to temp directory
    let temp_dir = std::env::temp_dir().join(format!("capsuled-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;

    println!("📦 Extracting to {:?}...", temp_dir);

    let exe_path = std::env::current_exe()?;
    let file_data = std::fs::read(&exe_path)?;

    // Parse bundle
    let len = file_data.len();
    let magic = b"CAPSULED_V2_BUNDLE";
    let magic_start = len - magic.len() - 8;
    let size_bytes = &file_data[len - 8..len];
    let bundle_size = u64::from_le_bytes(size_bytes.try_into()?) as usize;
    let bundle_start = magic_start - bundle_size;
    let compressed = &file_data[bundle_start..magic_start];

    // Decompress
    let decompressed = zstd::decode_all(compressed)?;

    // Extract tar
    use tar::Archive;
    let mut archive = Archive::new(decompressed.as_slice());
    archive.unpack(&temp_dir)?;

    // Find entrypoint in source/
    let source_dir = temp_dir.join("source");
    let runtime_dir = temp_dir.join("runtime");

    // Look for capsule.toml to determine entrypoint
    let manifest_path = source_dir.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!("No capsule.toml found in bundle");
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest: toml::Value = toml::from_str(&manifest_content)?;

    let entrypoint = manifest
        .get("execution")
        .and_then(|e| e.get("entrypoint"))
        .and_then(|e| e.as_str())
        .ok_or_else(|| anyhow::anyhow!("No entrypoint defined in capsule.toml"))?;

    // Find Python binary in runtime
    let python_bin = find_python_binary(&runtime_dir)?;
    println!("🐍 Found Python: {:?}", python_bin);
    println!("📄 Running: {:?}", source_dir.join(entrypoint));

    // Parse port from manifest (default 8000)
    let port = manifest
        .get("execution")
        .and_then(|e| e.get("port"))
        .and_then(|p| p.as_integer())
        .map(|p| p as u16)
        .unwrap_or(8000);

    // Setup Socket Activation
    let socket_config = SocketConfig {
        port,
        host: "0.0.0.0".to_string(),
        enabled: true,
    };
    let socket_manager = SocketManager::new(socket_config)?;
    println!(
        "🔌 Socket Activation: Bound to port {} (FD {})",
        port,
        socket_manager.raw_fd()
    );

    // ⚠️ IMPORTANT: Setup signal handlers BEFORE spawning child process
    // This ensures signals are captured by our handler, not the default termination handler
    #[cfg(unix)]
    let (mut sig_term, mut sig_int) = {
        use tokio::signal::unix::{signal, SignalKind};
        let sig_term = signal(SignalKind::terminate())?;
        let sig_int = signal(SignalKind::interrupt())?;
        (sig_term, sig_int)
    };

    // Create the Supervisor (Actor-based)
    let supervisor = ProcessSupervisor::new();

    // Prepare command with Socket Activation
    let entrypoint_path = source_dir.join(entrypoint);
    let mut cmd = std::process::Command::new(&python_bin);
    cmd.arg(&entrypoint_path);
    cmd.current_dir(&source_dir);

    // Pass socket FD to child
    socket_manager.prepare_command(&mut cmd)?;

    // Set process group for signal propagation
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Spawn the child process
    let child = cmd.spawn()?;
    let child_pid = child.id();
    println!("🔗 Socket Activation: Passing FD 3 to child process");

    // Register with Supervisor
    supervisor.register("main-app".to_string(), child)?;

    // Wait for shutdown signal (SIGTERM or SIGINT)
    #[cfg(unix)]
    {
        tokio::select! {
            _ = sig_term.recv() => {
                println!("\n🛑 Received SIGTERM, shutting down gracefully...");
            }
            _ = sig_int.recv() => {
                println!("\n🛑 Received SIGINT (Ctrl+C), shutting down gracefully...");
            }
        }

        // Kill the process group directly to ensure child is terminated
        use nix::sys::signal::{self as nix_signal, Signal};
        use nix::unistd::Pid;

        println!("📤 Sending SIGTERM to process group (PID {})...", child_pid);

        // Send SIGTERM to child's process group
        let pgid = Pid::from_raw(child_pid as i32);
        if let Err(e) = nix_signal::killpg(pgid, Signal::SIGTERM) {
            warn!("Failed to send SIGTERM to process group: {}", e);
            // Fallback: try killing just the child
            if let Err(e) = nix_signal::kill(Pid::from_raw(child_pid as i32), Signal::SIGTERM) {
                warn!("Failed to send SIGTERM to child: {}", e);
            }
        }

        // Wait briefly for graceful exit
        println!("⏳ Waiting for processes to exit gracefully...");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Force kill if still running
        println!("🔨 Sending SIGKILL to ensure termination...");
        let _ = nix_signal::killpg(pgid, Signal::SIGKILL);
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        println!("\n🛑 Received shutdown signal, cleaning up...");
    }

    // Graceful shutdown via Supervisor (cleanup internal state)
    if let Err(e) = supervisor.shutdown_and_wait().await {
        // Ignore errors - the process may already be gone
        let _ = e;
    }

    // Cleanup temp directory
    if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
        warn!("Failed to cleanup temp directory: {}", e);
    }

    println!("✅ Shutdown complete");
    Ok(())
}

/// Find Python binary in extracted runtime
fn find_python_binary(runtime_dir: &PathBuf) -> anyhow::Result<PathBuf> {
    // Look for python3 or python binary
    for entry in std::fs::read_dir(runtime_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Check in bin/ subdirectory
            let bin_dir = path.join("bin");
            if bin_dir.exists() {
                for name in &["python3", "python"] {
                    let python_path = bin_dir.join(name);
                    if python_path.exists() {
                        return Ok(python_path);
                    }
                }
            }
        }
    }

    anyhow::bail!("Python binary not found in runtime directory")
}

/// Run capsuled in normal engine mode
async fn run_engine() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact(),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "capsuled=info,tower_http=debug".into()),
        )
        .init();

    let args = Args::parse();

    // UARC V1.1.0: Backend selection. Default to Source Runtime (direct execution).
    // Environment variable CAPSULED_BACKEND_MODE can override (e.g. "source", "wasm" or "oci").
    let backend_mode =
        std::env::var("CAPSULED_BACKEND_MODE").unwrap_or_else(|_| "source".to_string());
    let backend_mode = if backend_mode.is_empty() {
        "source".to_string()
    } else {
        backend_mode
    };
    info!("Backend Mode: {} (UARC V1.1.0 default)", backend_mode);

    info!(
        "=== Capsuled Engine v{} (Phase 1.5: HTTP API Only) ===",
        env!("CARGO_PKG_VERSION")
    );
    info!("HTTP API Port: {}", args.port);

    // Ensure log directory exists
    if let Some(parent) = PathBuf::from(&args.audit_log).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Ensure key directory exists
    if let Some(parent) = PathBuf::from(&args.audit_key).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Initialize AuditLogger
    let audit_logger = Arc::new(
        AuditLogger::new(
            PathBuf::from(&args.audit_log),
            PathBuf::from(&args.audit_key),
            "capsuled-engine".to_string(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize audit logger: {}", e))?,
    );

    info!("Audit logger initialized");

    info!("Audit logger initialized");

    // Initialize Egress Proxy (User-Space Filtering)
    let proxy_port = args.port + 1; // e.g. 4501
    let egress_proxy = Arc::new(EgressProxy::new(proxy_port));

    // Start Proxy
    let proxy_clone = egress_proxy.clone();
    tokio::spawn(async move {
        if let Err(e) = proxy_clone.start().await {
            warn!("Failed to start Egress Proxy: {}", e);
        }
    });

    // Detect GPU hardware
    let gpu_detector = hardware::create_gpu_detector();
    match gpu_detector.detect_gpus() {
        Ok(report) => {
            info!("GPU detection successful:");
            info!("  Total GPUs: {}", report.gpus.len());
            info!("  Total VRAM: {:.2} GB", report.total_vram_gb());
            for gpu in &report.gpus {
                info!(
                    "    GPU {}: {} ({:.2} GB)",
                    gpu.index,
                    gpu.device_name,
                    gpu.vram_gb()
                );
            }
        }
        Err(e) => {
            warn!("GPU detection failed (CPU mode): {}", e);
        }
    }

    // Initialize Service Registry (for port allocation and mDNS)
    let service_registry = Arc::new(ServiceRegistry::new(None));

    // Initialize Job History (for gRPC GetJobStatus/ListJobs)
    let data_dir = std::env::var("CAPSULED_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/capsuled"));
    std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");
    let job_history_db = data_dir.join("job_history.sqlite");
    let job_history = Arc::new(
        SqliteJobHistoryStore::new(&job_history_db)
            .expect("Failed to initialize job history store"),
    );
    info!("Job history initialized at {:?}", job_history_db);

    // Initialize Process Supervisor
    let process_supervisor = Arc::new(ProcessSupervisor::new());

    // Initialize Artifact Manager
    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let runtime_dir = home_dir.join(".gumball").join("runtimes");

    let registry_url = std::env::var("GUMBALL_REGISTRY_URL").unwrap_or_else(|_| {
        // Try to find local registry relative to current executable or workspace
        let local_registry = std::env::current_dir().ok().and_then(|cwd| {
            let candidates = vec![
                cwd.join("../../capsule-registry/registry.json"),
                cwd.join("../capsule-registry/registry.json"),
                cwd.join("capsule-registry/registry.json"),
            ];
            candidates.into_iter().find(|p| p.exists())
        });

        if let Some(path) = local_registry {
            format!("file://{}", path.to_string_lossy())
        } else {
            "https://gist.githubusercontent.com/egamikohsuke/placeholder/raw/registry.json"
                .to_string()
        }
    });

    let artifact_config = ArtifactConfig {
        registry_url,
        cache_path: runtime_dir,
        cas_root: None, // CAS integration - configure via CLI if needed
    };

    let artifact_manager = Arc::new(
        ArtifactManager::new(artifact_config)
            .await
            .expect("Failed to initialize ArtifactManager"),
    );

    // Initialize MetricsCollector (Prometheus-style Pull metrics)
    // Replaces the old UsageReporter which pushed to Coordinator
    let metrics_collector = Arc::new(capsuled::metrics::collector::MetricsCollector::new());

    // Load StorageConfig from config file if available
    let storage_config = load_storage_config();

    // Load file config (config.toml) for security allowlist + model cache dir.
    let file_cfg = load_engine_file_config();
    let mut allowed_host_paths = file_cfg
        .as_ref()
        .map(|c| c.security_allowed_paths().to_vec())
        .unwrap_or_default();

    // Env override (CSV). Example: GUMBALL_ALLOWED_HOST_PATHS="/opt/models,/mnt/cache"
    if let Ok(csv) = std::env::var("GUMBALL_ALLOWED_HOST_PATHS") {
        let parsed = capsuled::security::parse_allowed_host_paths_csv(&csv);
        if !parsed.is_empty() {
            allowed_host_paths = parsed;
        }
    }

    let models_cache_dir = file_cfg
        .as_ref()
        .and_then(|c| c.models_cache_dir())
        .unwrap_or(DEFAULT_MODELS_CACHE_DIR)
        .to_string();

    if allowed_host_paths.is_empty() {
        warn!(
            "security.allowed_host_paths is empty; FetchModel and bind-mount validation will be blocked by allowlist"
        );
    }
    info!("Models cache dir: {}", models_cache_dir);

    // Initialize ContainerRuntime (external OCI runtime) config.
    // UARC V1.1.0: Source runtime for direct execution (no OCI)
    // "docker" mode uses DockerCliRuntime (via CapsuleManager) which doesn't need youki/runc.
    let container_runtime_config = match backend_mode.as_str() {
        "native" | "direct" | "source" | "docker" => {
            info!(
                backend_mode = %backend_mode,
                "Using Source/Docker Runtime; skipping external OCI runtime binary detection"
            );
            RuntimeConfig {
                kind: RuntimeKind::Source,
                binary_path: std::path::PathBuf::from("/dev/null"),
                bundle_root: std::env::temp_dir().join("capsuled").join("bundles"),
                state_root: std::env::temp_dir().join("capsuled").join("state"),
                log_dir: std::env::temp_dir().join("capsuled").join("logs"),
                hook_retry_attempts: 1,
            }
        }
        _ => match RuntimeConfig::from_section(None) {
            Ok(c) => c,
            Err(e) => {
                error!(
                    backend_mode = %backend_mode,
                    "Failed to detect OCI runtime (youki/runc): {}. \
                     UARC V1 requires a valid OCI runtime for container support. \
                     Wasm and Source runtimes will still work.",
                    e
                );
                return Err(e.into());
            }
        },
    };

    let runtime = Arc::new(ContainerRuntime::new(
        container_runtime_config.clone(),
        Some(artifact_manager.clone()),
        Some(process_supervisor.clone()),
        Some(proxy_port),
    ));

    // Initialize ManifestVerifier
    let verifier_pubkey = std::env::var("CAPSULED_PUBKEY").ok();
    // Default to strict enforcement? No, default to permissive for now to avoid breaking existing users unless configured.
    let enforcement_enabled = std::env::var("CAPSULED_ENFORCE_SIGNATURES")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let verifier = Arc::new(capsuled::security::verifier::ManifestVerifier::new(
        verifier_pubkey,
        enforcement_enabled,
    ));

    // Initialize LocalCasClient for UARC V1.1.0 L1 Source Policy verification
    // CAS directory: ~/.capsule/cas (shared with CLI)
    let cas_client: Option<Arc<dyn capsuled::resource::cas::CasClient>> = {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let cas_root = home.join(".capsule").join("cas");
        match LocalCasClient::new(&cas_root) {
            Ok(client) => {
                info!("Initialized LocalCasClient at {:?}", cas_root);
                Some(Arc::new(client))
            }
            Err(e) => {
                warn!(
                    "Failed to initialize LocalCasClient: {}. CAS verification will be disabled.",
                    e
                );
                None
            }
        }
    };

    // Initialize CapsuleManager
    // UARC V1.1.0: Pass runtime section for allow_insecure_dev_mode
    let runtime_section = file_cfg.as_ref().and_then(|c| c.runtime.as_ref());
    let capsule_manager = Arc::new(CapsuleManager::new(
        audit_logger.clone(),
        allowed_host_paths.clone(),
        gpu_detector.clone(),
        Some(service_registry.clone()),
        None, // mDNS announcer (optional)
        // UARC V1: Traefik removed
        Some(artifact_manager.clone()),
        Some(process_supervisor.clone()),
        Some(proxy_port),
        verifier,
        Some(container_runtime_config),
        Some(metrics_collector.clone()),
        storage_config,
        cas_client, // CAS client for L1 Source Policy verification
        runtime_section,
    ));

    info!("CapsuleManager initialized");

    // Initialize AuthManager
    let auth_manager = Arc::new(capsuled::auth::AuthManager::new());

    // Start HTTP API Server
    info!("Starting HTTP API server on port {}...", args.port);

    let http_capsule_manager = capsule_manager.clone();
    let http_service_registry = service_registry.clone();
    let http_gpu_detector = gpu_detector.clone();
    let http_auth_manager = auth_manager;
    let http_metrics_collector = Some(metrics_collector.clone());

    tokio::spawn(async move {
        if let Err(e) = api_server::start_api_server(
            args.port,
            http_capsule_manager,
            http_service_registry,
            http_gpu_detector,
            http_auth_manager,
            http_metrics_collector,
        )
        .await
        {
            error!("HTTP API server error: {}", e);
        }
    });

    // TailscaleManager removed - UARC V1.1.0 uses SPIFFE ID for identity

    let allowed_host_paths_for_grpc = allowed_host_paths.clone();
    let models_cache_dir_for_grpc = models_cache_dir.clone();
    // Start gRPC Server (Phase 2)
    let grpc_port = args.grpc_port;
    let grpc_addr = format!("0.0.0.0:{}", grpc_port);

    info!("Starting gRPC server on {}...", grpc_addr);

    let grpc_capsule_manager = capsule_manager.clone();
    let grpc_service_registry = service_registry.clone();
    let grpc_gpu_detector = gpu_detector.clone();
    let grpc_artifact_manager = artifact_manager.clone();
    let grpc_job_history = job_history.clone();

    tokio::spawn(async move {
        if let Err(e) = capsuled::grpc_server::start_grpc_server(
            &grpc_addr,
            grpc_capsule_manager,
            runtime,
            allowed_host_paths_for_grpc,
            PathBuf::from(models_cache_dir_for_grpc),
            backend_mode,
            grpc_service_registry,
            grpc_gpu_detector,
            grpc_artifact_manager,
            grpc_job_history,
        )
        .await
        {
            error!("gRPC server failed: {}", e);
        }
    });

    // Keep the process running
    info!("Capsuled Engine started. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    Ok(())
}

/// Load storage configuration from config.toml or environment
fn load_storage_config() -> Option<StorageConfig> {
    // Try to load from config.toml
    let config_paths = [
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/capsuled/config.toml"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("config.toml")))
            .unwrap_or_default(),
    ];

    for path in &config_paths {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    // Parse TOML to extract storage section
                    if let Ok(table) = content.parse::<toml::Table>() {
                        if let Some(storage_table) = table.get("storage") {
                            match storage_table.clone().try_into::<StorageConfig>() {
                                Ok(config) => {
                                    if config.enabled {
                                        info!("Storage configuration loaded from {:?}", path);
                                        return Some(config);
                                    } else {
                                        info!("Storage is disabled in configuration");
                                        return None;
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to parse storage config: {}", e);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to read config file {:?}: {}", path, e);
                }
            }
        }
    }

    // Check environment variable for enabling storage
    if std::env::var("GUMBALL_STORAGE_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
    {
        info!("Storage enabled via environment variable");
        let mut config = StorageConfig {
            enabled: true,
            ..Default::default()
        };

        // Override defaults from environment
        // Note: LVM-specific options (VG, encryption) removed in SPEC V1.1.0
        // Storage now uses simple directory-based approach
        if let Ok(storage_base) = std::env::var("GUMBALL_STORAGE_BASE") {
            config.storage_base = PathBuf::from(storage_base);
        }

        return Some(config);
    }

    None
}

fn load_engine_file_config() -> Option<capsuled::config::FileConfig> {
    let config_paths = [
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/capsuled/config.toml"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("config.toml")))
            .unwrap_or_default(),
    ];

    for path in &config_paths {
        if path.exists() {
            match capsuled::config::load_config(path) {
                Ok(Some(cfg)) => return Some(cfg),
                Ok(None) => return None,
                Err(err) => {
                    warn!("Failed to load config {:?}: {}", path, err);
                    return None;
                }
            }
        }
    }

    None
}
