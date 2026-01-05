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
use capsuled::runtime::{ContainerRuntime, RuntimeConfig, RuntimeKind};
use capsuled::security::audit::AuditLogger;
use capsuled::security::EgressProxy;
use capsuled::storage::StorageConfig;

mod headscale;
use headscale::{HeadscaleClient, HeadscaleConfig};

const DEFAULT_HTTP_PORT: u16 = 4500;
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

    /// Path to audit log file
    #[arg(long, default_value = DEFAULT_AUDIT_LOG_PATH)]
    audit_log: String,

    /// Path to node key for audit signatures
    #[arg(long, default_value = DEFAULT_AUDIT_KEY_PATH)]
    audit_key: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    // Phase 2: backend selection (Hybrid). Default to Native/DirectRuntime.
    let backend_mode =
        std::env::var("CAPSULED_BACKEND_MODE").unwrap_or_else(|_| "native".to_string());
    let backend_mode = if backend_mode.is_empty() {
        "native".to_string()
    } else {
        backend_mode
    };
    info!("Backend Mode: {}", backend_mode);

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

    // Initialize Headscale if configured
    let headscale_config = load_headscale_config()?;
    if !headscale_config.server_url.is_empty() {
        let headscale = HeadscaleClient::new(headscale_config);

        match headscale.connect().await {
            Ok(info) => {
                tracing::info!(
                    tailnet_ip = %info.ip,
                    hostname = %info.hostname,
                    peers = info.peers.len(),
                    "Connected to Tailnet"
                );

                // Update machine registration with Tailnet IP
                // update_machine_tailnet_ip(&info.ip).await?;
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to connect to Tailnet, running in local mode");
            }
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
    // In Native/DirectRuntime mode we intentionally skip binary detection to avoid noisy/misleading warnings.
    let container_runtime_config = match backend_mode.as_str() {
        "native" | "direct" => {
            info!(
                backend_mode = %backend_mode,
                "Using internal NativeRuntime; skipping external OCI runtime binary detection"
            );
            RuntimeConfig {
                kind: RuntimeKind::Native,
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

    // Initialize CapsuleManager
    let capsule_manager = Arc::new(CapsuleManager::new(
        audit_logger.clone(),
        allowed_host_paths.clone(),
        gpu_detector.clone(),
        Some(service_registry.clone()),
        None, // mDNS announcer (optional)
        None, // Traefik manager (optional)
        Some(artifact_manager.clone()),
        Some(process_supervisor.clone()),
        Some(proxy_port),
        verifier,
        Some(container_runtime_config),
        Some(metrics_collector.clone()),
        storage_config,
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

    // Initialize WasmHost
    // Use minimal valid WASM header (magic + version) as fallback
    let wasm_bytes = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let wasm_host = Arc::new(
        capsuled::wasm_host::AdepLogicHost::new(&wasm_bytes)
            .unwrap_or_else(|e| panic!("Failed to initialize WasmHost: {}", e)),
    );

    // Initialize TailscaleManager
    let tailscale_manager =
        Arc::new(capsuled::network::tailscale::TailscaleManager::start(None, None, None));

    let allowed_host_paths_for_grpc = allowed_host_paths.clone();
    let models_cache_dir_for_grpc = models_cache_dir.clone();
    // Start gRPC Server (Phase 2)
    let grpc_port = 50051;
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
            wasm_host,
            runtime,
            allowed_host_paths_for_grpc,
            PathBuf::from(models_cache_dir_for_grpc),
            backend_mode,
            tailscale_manager,
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

fn load_headscale_config() -> anyhow::Result<HeadscaleConfig> {
    // Load from environment or config file
    Ok(HeadscaleConfig {
        server_url: std::env::var("GUMBALL_HEADSCALE_URL").unwrap_or_default(),
        auth_key: std::env::var("GUMBALL_HEADSCALE_KEY").ok(),
        ..Default::default()
    })
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
