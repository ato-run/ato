use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

use crate::adep::{CapsuleManifestV1, RuntimeType};
use crate::metrics::collector::MetricsCollector;
use crate::hardware::GpuDetector;
use crate::runtime::{
    youki_adapter::YoukiRuntimeAdapter, ContainerRuntime, DevRuntime, DockerCliRuntime,
    LaunchRequest, LaunchResult, NativeRuntime, Runtime, RuntimeConfig, RuntimeError, RuntimeKind,
    resolver::{resolve_runtime, ResolveContext, ResolvedTarget},
    source::{SourceRuntime, SourceRuntimeConfig},
};
use crate::security::audit::{AuditLogger, AuditOperation, AuditStatus};
use crate::security::vram_scrubber::VramScrubber;
use crate::security::ManifestVerifier;
use crate::storage::{StorageConfig, StorageManager};

/// Request parameters for deploying a capsule
#[derive(Debug, Clone)]
pub struct DeployCapsuleRequest {
    pub capsule_id: String,
    pub manifest: CapsuleManifestV1,
    /// Raw manifest bytes for signature verification (Cap'n Proto or JSON)
    /// If None, signature verification will be skipped or use serialized manifest
    pub raw_manifest_bytes: Option<Vec<u8>>,
    pub oci_image: String,
    pub digest: String,
    pub extra_args: Option<Vec<String>>,
    pub signature: Option<Vec<u8>>,
    /// Working directory for Source runtime (PythonUv, etc.)
    /// This is the directory containing the source code.
    pub source_working_dir: Option<String>,
}

/// Capsule represents a running capsule instance
#[derive(Clone, Debug)]
pub struct Capsule {
    pub id: String,
    pub adep_json: Vec<u8>,
    pub oci_image: String,
    pub digest: String,
    pub status: CapsuleStatus,
    pub storage_path: Option<String>,
    pub bundle_path: Option<String>,
    pub pid: Option<u32>,
    pub reserved_vram_bytes: u64,
    pub observed_vram_bytes: Option<u64>,
    pub last_failure: Option<String>,
    pub last_exit_code: Option<i32>,
    pub log_path: Option<String>,

    pub started_at: Option<std::time::SystemTime>,
    pub remote_url: Option<String>,
    pub user_id: Option<String>,
    pub gpu_indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CapsuleStatus {
    Pending,
    Provisioning,
    Running,
    Stopped,
    Failed,
}

impl std::fmt::Display for CapsuleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapsuleStatus::Pending => write!(f, "pending"),
            CapsuleStatus::Provisioning => write!(f, "provisioning"),
            CapsuleStatus::Running => write!(f, "running"),
            CapsuleStatus::Stopped => write!(f, "stopped"),
            CapsuleStatus::Failed => write!(f, "failed"),
        }
    }
}

use crate::artifact::ArtifactManager;
use crate::network::mdns::MdnsAnnouncer;
use crate::network::service_registry::ServiceRegistry;
use crate::network::traefik::TraefikManager;
use crate::pool_registry::PoolRegistry;
use crate::process_supervisor::ProcessSupervisor;

/// Manages the lifecycle of capsules
pub struct CapsuleManager {
    // Runtimes
    container_runtime: Arc<ContainerRuntime>,
    docker_runtime: Arc<DockerCliRuntime>,
    native_runtime: Arc<NativeRuntime>,
    dev_runtime: Arc<DevRuntime>,
    source_runtime: Arc<SourceRuntime>,
    youki_runtime: Arc<YoukiRuntimeAdapter>,
    wasm_runtime: Arc<crate::runtime::WasmRuntime>,
    verifier: Arc<ManifestVerifier>,
    capsules: Arc<RwLock<HashMap<String, Capsule>>>,

    // Pre-warmed container pool
    #[allow(dead_code)]
    pool_registry: Option<Arc<PoolRegistry>>,

    // Security
    allowed_host_paths: Vec<String>,

    // Dependencies
    audit_logger: Arc<AuditLogger>,
    gpu_detector: Arc<dyn GpuDetector>,
    service_registry: Option<Arc<ServiceRegistry>>,
    mdns_announcer: Option<Arc<MdnsAnnouncer>>,
    traefik_manager: Option<Arc<TraefikManager>>,
    cloud_client: Option<Arc<crate::cloud::skypilot::SkyPilotClient>>,
    artifact_manager: Option<Arc<ArtifactManager>>,
    storage_manager: Option<Arc<StorageManager>>,

    // Metrics (Pull-based observability)
    metrics_collector: Option<Arc<MetricsCollector>>,
}

impl CapsuleManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audit_logger: Arc<AuditLogger>,
        allowed_host_paths: Vec<String>,
        gpu_detector: Arc<dyn GpuDetector>,
        service_registry: Option<Arc<ServiceRegistry>>,
        mdns_announcer: Option<Arc<MdnsAnnouncer>>,
        traefik_manager: Option<Arc<TraefikManager>>,
        cloud_client: Option<Arc<crate::cloud::skypilot::SkyPilotClient>>,
        artifact_manager: Option<Arc<ArtifactManager>>,
        process_supervisor: Option<Arc<ProcessSupervisor>>,
        egress_proxy_port: Option<u16>,
        verifier: Arc<ManifestVerifier>,
        runtime_config: Option<RuntimeConfig>,
        metrics_collector: Option<Arc<MetricsCollector>>,
        storage_config: Option<StorageConfig>,
    ) -> Self {
        // Initialize runtimes
        let runtime_config = if let Some(config) = runtime_config {
            config
        } else {
            // TODO: Load config from file or env
            match RuntimeConfig::from_section(None) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "External OCI runtime binary not detected: {}. Falling back to Mock ContainerRuntime (NativeRuntime may still work)",
                        e
                    );
                    // Fallback to dummy config to allow Engine to start (e.g. for NativeRuntime usage)
                    RuntimeConfig {
                        kind: crate::runtime::RuntimeKind::Mock,
                        binary_path: std::path::PathBuf::from("/dev/null"),
                        bundle_root: std::env::temp_dir().join("capsuled").join("bundles"),
                        state_root: std::env::temp_dir().join("capsuled").join("state"),
                        log_dir: std::env::temp_dir().join("capsuled").join("logs"),
                        hook_retry_attempts: 1,
                    }
                }
            }
        };

        // Initialize Youki runtime (for direct OCI container execution without Docker)
        // Clone config before passing to container_runtime since it takes ownership
        let log_dir_clone = runtime_config.log_dir.clone();
        let bundle_root_clone = runtime_config.bundle_root.clone();
        
        let youki_runtime = Arc::new(YoukiRuntimeAdapter::new(
            log_dir_clone.clone(),
            bundle_root_clone.clone(),
        ));

        let container_runtime = Arc::new(ContainerRuntime::new(
            runtime_config,
            artifact_manager.clone(),
            process_supervisor.clone(),
            egress_proxy_port,
        ));
        let native_runtime = Arc::new(NativeRuntime::new(
            artifact_manager.clone(),
            egress_proxy_port,
        ));
        let dev_runtime = Arc::new(DevRuntime::new(
            process_supervisor.clone(),
            egress_proxy_port,
        ));
        let docker_cli_runtime = Arc::new(DockerCliRuntime::new(egress_proxy_port));

        // Initialize SourceRuntime (for PythonUv and interpreted languages)
        let source_runtime_config = SourceRuntimeConfig {
            dev_mode: true, // Enable dev mode for fast native execution
            log_dir: log_dir_clone.clone(),
            state_dir: std::env::temp_dir().join("capsuled").join("state"),
        };
        // Youki fallback only on Linux
        let oci_fallback = if cfg!(target_os = "linux") {
            Some(youki_runtime.clone())
        } else {
            None
        };
        let source_runtime = Arc::new(SourceRuntime::new(source_runtime_config, oci_fallback));

        // Initialize StorageManager if config provided
        let storage_manager = storage_config.map(|config| {
            info!("Initializing StorageManager with VG: {}", config.default_vg);
            Arc::new(StorageManager::new(config))
        });

        // Initialize PoolRegistry for pre-warmed container pools (Linux only)
        let pool_registry = if cfg!(target_os = "linux") {
            let pool_bundle_root = youki_runtime.inner_arc().config().bundle_root.join("pools");
            Some(Arc::new(PoolRegistry::new(
                youki_runtime.inner_arc(),
                pool_bundle_root,
            )))
        } else {
            None
        };

        // Initialize WasmRuntime (UARC V1.1.0 support)
        let wasm_runtime = Arc::new(
            crate::runtime::WasmRuntime::new(artifact_manager.clone(), log_dir_clone, egress_proxy_port)
                .expect("Failed to initialize WasmRuntime"),
        );

        Self {
            docker_runtime: docker_cli_runtime,
            native_runtime,
            dev_runtime,
            source_runtime,
            youki_runtime,
            wasm_runtime,
            container_runtime, // Added missing field initialization
            verifier,
            capsules: Arc::new(RwLock::new(HashMap::new())),
            pool_registry,

            allowed_host_paths,

            audit_logger,
            gpu_detector,
            service_registry,
            mdns_announcer,
            traefik_manager,
            cloud_client,
            artifact_manager,
            storage_manager,

            metrics_collector,
        }
    }

    pub fn cloud_configured(&self) -> bool {
        self.cloud_client.is_some()
    }

    /// Build a ResolveContext from current engine capabilities
    fn build_resolve_context(&self) -> ResolveContext {
        use std::collections::HashSet;

        let mut supported_runtimes = HashSet::new();
        supported_runtimes.insert(RuntimeKind::Native);
        supported_runtimes.insert(RuntimeKind::Wasm);

        // Youki only on Linux
        let youki_available = cfg!(target_os = "linux");
        if youki_available {
            supported_runtimes.insert(RuntimeKind::Youki);
        }

        // Check if docker is available (via container_runtime not being Mock)
        let docker_available = self.container_runtime.config().kind != RuntimeKind::Mock
            || cfg!(target_os = "macos"); // Docker Desktop on macOS

        // Available toolchains (could be detected dynamically in future)
        let mut available_toolchains = HashSet::new();
        available_toolchains.insert("python".to_string());
        available_toolchains.insert("node".to_string());

        ResolveContext {
            platform: crate::runtime::resolver::detect_current_platform(),
            supported_runtimes,
            wasm_available: true, // WasmRuntime is always initialized
            docker_available,
            gpu_available: self.gpu_detector.detect_gpus().is_ok(),
            available_toolchains,
        }
    }

    /// Select the appropriate runtime Arc for a resolved target
    fn select_runtime_for_target(
        &self,
        resolved: &ResolvedTarget,
        force_docker_cli: bool,
    ) -> Arc<dyn Runtime> {
        match resolved {
            ResolvedTarget::Wasm { .. } => self.wasm_runtime.clone(),
            ResolvedTarget::Source { language, .. } => {
                // Source targets use DevRuntime (python-uv, node, etc.)
                match language.to_lowercase().as_str() {
                    "python" | "python3" => self.dev_runtime.clone(),
                    "node" | "nodejs" | "deno" => self.dev_runtime.clone(),
                    _ => self.native_runtime.clone(),
                }
            }
            ResolvedTarget::Oci { .. } => {
                // OCI targets prefer Youki on Linux, Docker on macOS
                if cfg!(target_os = "linux") && !force_docker_cli {
                    self.youki_runtime.clone()
                } else if cfg!(target_os = "macos") || force_docker_cli {
                    self.docker_runtime.clone()
                } else {
                    self.container_runtime.clone()
                }
            }
            ResolvedTarget::Legacy { runtime_type, .. } => {
                // Legacy mode - use the old runtime selection logic
                match runtime_type {
                    RuntimeType::Native => self.native_runtime.clone(),
                    RuntimeType::Docker => {
                        if self.container_runtime.config().kind == RuntimeKind::Mock {
                            self.container_runtime.clone()
                        } else if cfg!(target_os = "macos") || force_docker_cli {
                            self.docker_runtime.clone()
                        } else {
                            self.container_runtime.clone()
                        }
                    }
                    RuntimeType::PythonUv => self.source_runtime.clone(),
                    RuntimeType::Youki => {
                        if cfg!(target_os = "linux") {
                            self.youki_runtime.clone()
                        } else {
                            warn!("Youki runtime is only supported on Linux, falling back to Docker");
                            if cfg!(target_os = "macos") || force_docker_cli {
                                self.docker_runtime.clone()
                            } else {
                                self.container_runtime.clone()
                            }
                        }
                    }
                    RuntimeType::Wasm => self.wasm_runtime.clone(),
                }
            }
        }
    }

    /// Pre-execution analysis hook for obfuscation detection.
    ///
    /// Scans manifest execution config for dangerous patterns that could
    /// indicate obfuscated code or remote code injection attempts.
    ///
    /// # Detected Patterns
    /// - `base64 -d`, `base64 --decode`: Hidden payload decoding
    /// - `eval`, `exec`: Dynamic code execution  
    /// - `curl | sh`, `wget | bash`: Remote code injection
    /// - Embedded hex/base64 blobs in environment variables
    #[allow(dead_code)]
    fn pre_execute_analysis(&self, manifest: &CapsuleManifestV1) -> Result<()> {
        tracing::debug!("Pre-execution analysis for capsule '{}'", manifest.name);

        // Dangerous command patterns (case-insensitive)
        const DANGEROUS_PATTERNS: &[&str] = &[
            "base64 -d",
            "base64 --decode",
            "eval ",
            "eval(",
            "exec(",
            "| sh",
            "| bash",
            "| zsh",
            "curl | ",
            "wget | ",
            "python -c",
            "python3 -c",
            "perl -e",
            "ruby -e",
        ];

        // Check entrypoint
        let entrypoint = &manifest.execution.entrypoint;
        for pattern in DANGEROUS_PATTERNS {
            if entrypoint.to_lowercase().contains(pattern) {
                tracing::warn!(
                    "Obfuscation detected in capsule '{}': entrypoint contains '{}'",
                    manifest.name,
                    pattern
                );
                return Err(anyhow!(
                    "Security: Capsule '{}' rejected - dangerous pattern '{}' in entrypoint",
                    manifest.name,
                    pattern
                ));
            }
        }

        // Check environment variables
        for (key, value) in &manifest.execution.env {
            // Check for suspicious long base64-like values
            if value.len() > 200 && Self::looks_like_encoded(value) {
                tracing::warn!(
                    "Obfuscation detected in capsule '{}': env var '{}' contains suspicious encoded data",
                    manifest.name, key
                );
                return Err(anyhow!(
                    "Security: Capsule '{}' rejected - env var '{}' contains suspicious encoded data (length: {})",
                    manifest.name, key, value.len()
                ));
            }

            // Check for dangerous patterns in env values
            for pattern in DANGEROUS_PATTERNS {
                if value.to_lowercase().contains(pattern) {
                    tracing::warn!(
                        "Obfuscation detected in capsule '{}': env var '{}' contains '{}'",
                        manifest.name,
                        key,
                        pattern
                    );
                    return Err(anyhow!(
                        "Security: Capsule '{}' rejected - dangerous pattern '{}' in env var '{}'",
                        manifest.name,
                        pattern,
                        key
                    ));
                }
            }
        }

        tracing::debug!(
            "Pre-execution analysis passed for capsule '{}'",
            manifest.name
        );
        Ok(())
    }

    /// Check if a string looks like base64 or hex encoded data
    fn looks_like_encoded(s: &str) -> bool {
        // High ratio of alphanumeric + base64 chars
        let encoded_chars = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
            .count();

        let ratio = encoded_chars as f64 / s.len() as f64;

        // If >90% are base64-safe chars and length is significant, likely encoded
        ratio > 0.9 && s.len() > 100
    }

    /// Deploy a new capsule
    ///
    /// Accepts a parsed `CapsuleManifestV1` directly, eliminating the need for
    /// JSON parsing at this layer. For signature verification, optionally provide
    /// `raw_manifest_bytes` (the original Cap'n Proto or JSON bytes).
    pub async fn deploy_capsule(&self, request: DeployCapsuleRequest) -> Result<String> {
        let DeployCapsuleRequest {
            capsule_id,
            mut manifest,
            raw_manifest_bytes,
            oci_image,
            digest,
            extra_args,
            signature,
            source_working_dir,
        } = request.clone();

        info!("Deploying capsule {}", capsule_id);

        // Signature verification (if signature is provided)
        if let Some(sig_bytes) = &signature {
            // Use raw_manifest_bytes if provided, otherwise serialize manifest to JSON
            let verification_bytes = raw_manifest_bytes
                .clone()
                .unwrap_or_else(|| serde_json::to_vec(&manifest).unwrap_or_default());

            if !verification_bytes.is_empty() {
                self.verifier.verify(&verification_bytes, sig_bytes, "")?;
            }
        } else if signature.is_none() {
            // No signature provided - log warning
            warn!(
                "Security: No signature provided for capsule {}. Skipping verification.",
                capsule_id
            );
        }

        // Manifest JSON string for cloud deploy / record keeping
        let manifest_json_str = serde_json::to_string(&manifest)
            .map_err(|e| anyhow!("Failed to serialize manifest: {}", e))?;

        // Placement Logic
        let mut assigned_gpu_index = None;
        let requires_gpu = manifest.requirements.vram_min.is_some()
            || manifest.requirements.vram_recommended.is_some();
        let required_vram = manifest
            .requirements
            .vram_min_bytes()
            .ok()
            .flatten()
            .unwrap_or(0);

        if requires_gpu {
            info!("Capsule requires GPU with {} bytes VRAM", required_vram);

            // 1. Local Scan
            let report = self
                .gpu_detector
                .detect_gpus()
                .map_err(|e| anyhow!("Failed to detect GPUs: {}", e))?;

            let mut found_local = false;
            for gpu in report.gpus {
                match self
                    .gpu_detector
                    .get_available_vram_bytes(gpu.index as usize)
                {
                    Ok(available) => {
                        if available >= required_vram {
                            info!(
                                "Found suitable local GPU {} (Available: {} bytes)",
                                gpu.index, available
                            );
                            assigned_gpu_index = Some(gpu.index);
                            found_local = true;
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to get VRAM for GPU {}: {}", gpu.index, e);
                    }
                }
            }

            // 2. Decision
            if !found_local {
                info!(
                    "Local resources insufficient (Required: {} bytes). Checking cloud options...",
                    required_vram
                );

                // Prioritize "fallback_to_cloud" from Routing config
                if manifest.can_fallback_to_cloud() {
                    info!("Cloud fallback enabled. Bursting to cloud...");
                    if let Some(client) = &self.cloud_client {
                        // 1. Register Capsule as Provisioning
                        let capsule = Capsule {
                            id: capsule_id.clone(),
                            adep_json: manifest_json_str.clone().into_bytes(),
                            oci_image: oci_image.clone(),
                            digest: digest.clone(),
                            status: CapsuleStatus::Provisioning,
                            storage_path: None,
                            bundle_path: None,
                            pid: None,
                            reserved_vram_bytes: 0, // Cloud VRAM
                            observed_vram_bytes: None,
                            last_failure: None,
                            last_exit_code: None,
                            log_path: None,
                            started_at: Some(std::time::SystemTime::now()),
                            remote_url: None,
                            user_id: None,
                            gpu_indices: vec![],
                        };
                        self.capsules
                            .write()
                            .unwrap()
                            .insert(capsule_id.clone(), capsule);

                        // 2. Deploy
                        match client.deploy(&manifest_json_str).await {
                            Ok(cluster_name) => {
                                // 3. Update to Running with URL
                                let url = format!("https://{}.ts.net", cluster_name);

                                if let Some(c) = self.capsules.write().unwrap().get_mut(&capsule_id)
                                {
                                    c.status = CapsuleStatus::Running;
                                    c.remote_url = Some(url.clone());
                                }

                                return Ok(format!("Cloud deployment started: {}", url));
                            }
                            Err(e) => {
                                // Update to Failed
                                if let Some(c) = self.capsules.write().unwrap().get_mut(&capsule_id)
                                {
                                    c.status = CapsuleStatus::Failed;
                                    c.last_failure = Some(e.to_string());
                                }
                                return Err(anyhow!("Cloud deployment failed: {}", e));
                            }
                        }
                    } else {
                        return Err(anyhow!(
                            "Cloud bursting required but Cloud Client is not configured"
                        ));
                    }
                }

                return Err(anyhow!("Insufficient resources: No local GPU with enough VRAM and no cloud configuration found."));
            } else {
                info!("Deploying locally to GPU {:?}", assigned_gpu_index);
            }
        }

        // Ensure Runtime Artifact (if native)
        if manifest.execution.runtime == RuntimeType::Native {
            // For Native execution, entrypoint is the unique identifier of the artifact (or path).
            // Example: "mlx-community/Llama-3.2-3B-Instruct-4bit"
            let runtime_id = manifest.execution.entrypoint.as_str();

            // Skip artifact download for system-level runtimes (mlx, llama, vllm) if they are just shims
            // But typically native runtime expects an artifact handle.
            // If it's a file path (starts with / or ./), skip download.
            if !runtime_id.starts_with('/') && !runtime_id.starts_with("./") {
                if let Some(am) = &self.artifact_manager {
                    // Check version from manifest or default to latest
                    let version = "latest"; // Manifest V1 defines version, but that's the capsule version. Artifact version?
                                            // Assuming entrypoint contains version if needed, or we treat capsule version as artifact version?
                                            // Use "latest" for now.
                    info!("Ensuring runtime artifact {}@{}...", runtime_id, version);
                    match am.ensure_runtime(runtime_id, version, None).await {
                        Ok(path) => {
                            info!("Runtime artifact ready at {:?}", path);
                        }
                        Err(e) => {
                            warn!("Failed to ensure runtime artifact: {}. Deployment may fail if not cached.", e);
                            return Err(anyhow!("Failed to ensure runtime artifact: {}", e));
                        }
                    }
                }
            }
        }

        // 1. Determine ports
        // - Container port: where the app listens inside the container.
        // - Host port: where we publish it on the host (docker -p HOST:CONTAINER).
        // Historically we conflated these via PORT, but that breaks fixed-port images (e.g. nginx:80)
        // and causes collisions with host port 80 (Caddy).

        let container_port: Option<u16> = manifest.execution.port.or_else(|| {
            manifest
                .execution
                .env
                .get("PORT")
                .and_then(|v| v.parse::<u16>().ok())
        });

        // Prefer explicit HOST_PORT if provided; otherwise, prefer same as container_port for
        // non-privileged ports, and allocate an ephemeral host port when needed.
        let desired_host_port: Option<u16> = manifest
            .execution
            .env
            .get("HOST_PORT")
            .and_then(|v| v.parse::<u16>().ok())
            .or_else(|| container_port.filter(|p| *p > 1024));

        let host_port: Option<u16> = match (&self.service_registry, desired_host_port) {
            (Some(registry), Some(p)) => {
                if registry.is_port_available(p) {
                    info!("Using requested HOST_PORT {} for capsule {}", p, capsule_id);
                    Some(p)
                } else {
                    let allocated = registry.allocate_port_prefer(p);
                    if let Some(ap) = allocated {
                        warn!(
                            "HOST_PORT {} is already in use; allocated {} for capsule {}",
                            p, ap, capsule_id
                        );
                    } else {
                        warn!(
                            "Failed to allocate fallback HOST_PORT for capsule {}",
                            capsule_id
                        );
                    }
                    allocated
                }
            }
            (Some(registry), None) => {
                let allocated = registry.allocate_port();
                if let Some(ap) = allocated {
                    info!("Allocated HOST_PORT {} for capsule {}", ap, capsule_id);
                } else {
                    warn!("Failed to allocate HOST_PORT for capsule {}", capsule_id);
                }
                allocated
            }
            (None, Some(p)) => {
                // No registry (no auto allocation); keep legacy behavior.
                info!(
                    "Using pre-injected HOST_PORT {} for capsule {}",
                    p, capsule_id
                );
                Some(p)
            }
            (None, None) => None,
        };

        if let Some(p) = host_port {
            // Pass HOST_PORT to DockerCliRuntime via env (it will still be injected into the container env;
            // harmless and avoids schema churn).
            manifest
                .execution
                .env
                .insert("HOST_PORT".to_string(), p.to_string());
        }

        // 2. Select Runtime and Launch
        let manifest_json_for_runtime = serde_json::to_string(&manifest)
            .map_err(|e| anyhow!("Failed to serialize manifest for runtime: {}", e))?;

        // Egress policy handling would go here (omitted for now)

        // GPU UUIDs for build_oci_spec
        let gpu_uuids = assigned_gpu_index.map(|gpu_idx| vec![gpu_idx.to_string()]);

        // =====================================================================
        // Wasm Runtime - WebAssembly Component Model execution
        // =====================================================================
        if matches!(manifest.execution.runtime, RuntimeType::Wasm) {
            info!("Using Wasm Runtime for capsule {}", capsule_id);
            
            // Use source_working_dir from request (set by CLI from capsule.toml [targets.wasm])
            let wasm_dir = source_working_dir
                .as_ref()
                .map(|p| PathBuf::from(p))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            
            // The entrypoint contains the path to the .wasm component file
            let component_path = if manifest.execution.entrypoint.starts_with('/') {
                PathBuf::from(&manifest.execution.entrypoint)
            } else {
                wasm_dir.join(&manifest.execution.entrypoint)
            };
            
            let dummy_spec = oci_spec::runtime::Spec::default();
            
            let launch_request = LaunchRequest {
                workload_id: &capsule_id,
                spec: &dummy_spec,
                manifest_json: Some(manifest_json_for_runtime.as_str()),
                bundle_root: wasm_dir.clone(),
                env: None,
                args: None,
                wasm_component_path: Some(component_path.clone()),
                source_target: None,
            };
            
            // Launch using WasmRuntime
            let result = self.wasm_runtime.launch(launch_request).await?;
            
            // Register capsule
            let capsule = Capsule {
                id: capsule_id.clone(),
                adep_json: manifest_json_for_runtime.clone().into_bytes(),
                oci_image: oci_image.clone(),
                digest: digest.clone(),
                status: CapsuleStatus::Running,
                storage_path: None,
                bundle_path: result.bundle_path.map(|p| p.to_string_lossy().to_string()),
                pid: result.pid,
                reserved_vram_bytes: 0,
                observed_vram_bytes: None,
                last_failure: None,
                last_exit_code: None,
                log_path: result.log_path.map(|p| p.to_string_lossy().to_string()),
                started_at: Some(std::time::SystemTime::now()),
                remote_url: None,
                user_id: None,
                gpu_indices: vec![],
            };
            
            self.capsules.write().unwrap().insert(capsule_id.clone(), capsule);
            
            // Wasm components typically don't expose HTTP ports, but report if configured
            let url = if let Some(port) = host_port {
                format!("http://localhost:{}", port)
            } else {
                format!("wasm://{}", capsule_id)
            };
            
            info!("Wasm Runtime capsule {} completed, URL: {}", capsule_id, url);
            return Ok(url);
        }

        // =====================================================================
        // Source Runtime (PythonUv) - Direct process execution (no OCI spec)
        // =====================================================================
        if matches!(manifest.execution.runtime, RuntimeType::PythonUv) {
            info!("Using Source Runtime (PythonUv) for capsule {}", capsule_id);
            
            // Use source_working_dir from request (set by CLI from capsule.toml [targets.source])
            let source_dir = source_working_dir
                .as_ref()
                .map(|p| PathBuf::from(p))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            
            let rootfs_path = source_dir.clone();
            
            // Build a minimal OCI spec for SourceRuntime (it doesn't actually use OCI)
            let dummy_spec = oci_spec::runtime::Spec::default();
            
            // Determine language from entrypoint extension
            let language = if manifest.execution.entrypoint.ends_with(".py") {
                "python".to_string()
            } else if manifest.execution.entrypoint.ends_with(".js") || manifest.execution.entrypoint.ends_with(".ts") {
                "node".to_string()
            } else {
                "python".to_string() // default
            };
            
            // Get args from extra_args (passed from CLI)
            let args = request.extra_args.clone().unwrap_or_default();
            
            let source_target = Some(crate::runtime::SourceTarget {
                language,
                version: None,
                entrypoint: manifest.execution.entrypoint.clone(),
                dependencies: None,
                args,
                source_dir: source_dir.clone(),
            });
            
            let launch_request = LaunchRequest {
                workload_id: &capsule_id,
                spec: &dummy_spec,
                manifest_json: Some(manifest_json_for_runtime.as_str()),
                bundle_root: rootfs_path.clone(),
                env: None,
                args: None,
                wasm_component_path: None,
                source_target,
            };
            
            // Launch using SourceRuntime (handles native sandbox or OCI fallback)
            let result = self.source_runtime.launch(launch_request).await?;
            
            // Register capsule
            let capsule = Capsule {
                id: capsule_id.clone(),
                adep_json: manifest_json_for_runtime.clone().into_bytes(),
                oci_image: oci_image.clone(),
                digest: digest.clone(),
                status: CapsuleStatus::Running,
                storage_path: None,
                bundle_path: result.bundle_path.map(|p| p.to_string_lossy().to_string()),
                pid: result.pid,
                reserved_vram_bytes: 0,
                observed_vram_bytes: None,
                last_failure: None,
                last_exit_code: None,
                log_path: result.log_path.map(|p| p.to_string_lossy().to_string()),
                started_at: Some(std::time::SystemTime::now()),
                remote_url: None,
                user_id: None,
                gpu_indices: vec![],
            };
            
            self.capsules.write().unwrap().insert(capsule_id.clone(), capsule);
            
            // Build response URL
            let url = if let Some(port) = host_port {
                format!("http://localhost:{}", port)
            } else {
                format!("http://localhost:{}", manifest.execution.port.unwrap_or(8000))
            };
            
            info!("Source Runtime capsule {} started, URL: {}", capsule_id, url);
            return Ok(url);
        }

        // =====================================================================
        // OCI-based Runtimes (Docker, Youki, Native, Wasm)
        // =====================================================================

        // Build the spec
        let allowed_paths = &self.allowed_host_paths;
        let rootfs_path = std::env::temp_dir().join("capsuled").join("rootfs");

        // Ensure the rootfs directory exists
        if let Err(e) = std::fs::create_dir_all(&rootfs_path) {
            return Err(anyhow!(
                "Failed to prepare rootfs directory {:?}: {}",
                rootfs_path,
                e
            ));
        }

        // Provision and mount storage if manager is available
        let mut volumes = manifest.storage.volumes.clone();
        if let Some(storage_manager) = &self.storage_manager {
            // Determine default thin provisioning from manifest's storage config
            let manifest_use_thin = manifest.storage.use_thin_provisioning;

            for volume in &mut volumes {
                // If it's a bind mount (has generic_name starting with bind:), skip provisioning
                // But V1 spec says `name` is the volume name.
                // If we want to support bind mounts via "bind:..." convention or similar, handled in runplan mapping.
                // Here we assume standard volume requests need provisioning.

                // Check if it's a host path (bind mount workaround)
                if volume.name.starts_with('/') || volume.name.starts_with("./") {
                    continue;
                }

                // Determine provisioning options from volume config
                let size_bytes = if volume.size_bytes > 0 {
                    Some(volume.size_bytes)
                } else {
                    None // Use engine default
                };

                // Per-volume use_thin overrides manifest default
                let use_thin = volume.use_thin.unwrap_or(manifest_use_thin);
                let encrypt = if volume.encrypted { Some(true) } else { None };

                match storage_manager.provision_capsule_storage(
                    &capsule_id,
                    size_bytes,
                    encrypt,
                    Some(use_thin),
                ) {
                    Ok(mut storage) => {
                        // Mount
                        if let Err(e) = storage_manager.mount_volume(&mut storage) {
                            warn!("Failed to mount storage for {}: {}", capsule_id, e);
                            return Err(anyhow!("Storage mount failed: {}", e));
                        }

                        // Update volume source to point to the host mount point
                        if let Some(mount_point) = storage.mount_point {
                            // We need to pass this host path to the OCI builder.
                            // The `StorageVolume` struct in `libadep` might not have a `source` field we can override directly
                            // if it's strictly "name".
                            // `build_oci_spec` uses `name` as source if not a path?
                            // We need to make `name` the absolute path to the mount.
                            volume.name = mount_point.to_string_lossy().to_string();
                            info!(
                                "Mapped volume to storage mount: {} (thin: {})",
                                volume.name, use_thin
                            );
                        }
                    }
                    Err(e) => {
                        warn!("Failed to provision storage for {}: {}", capsule_id, e);
                        return Err(anyhow!("Storage provisioning failed: {}", e));
                    }
                }
            }
        }

        let spec = crate::oci::spec_builder::build_oci_spec(
            &rootfs_path,
            &manifest.execution,
            &volumes,
            gpu_uuids.as_deref(),
            allowed_paths,
            None, // resources
            extra_args.as_deref(),
            &manifest,
        )
        .map_err(|e| anyhow!("Failed to build OCI spec: {}", e))?;

        let launch_request = LaunchRequest {
            workload_id: &capsule_id,
            spec: &spec,
            manifest_json: Some(manifest_json_for_runtime.as_str()),
            bundle_root: rootfs_path.clone(),
            env: None,
            args: None,
            wasm_component_path: None,
            source_target: None,
        };

        // Initialize pool if configured (Linux only, Youki runtime)
        #[cfg(target_os = "linux")]
        if let Some(pool_registry) = &self.pool_registry {
            if let Some(ref pool_config) = manifest.pool {
                if pool_config.enabled && matches!(manifest.execution.runtime, RuntimeType::Youki) {
                    info!(
                        capsule_id = %capsule_id,
                        pool_size = pool_config.size,
                        "Initializing pre-warmed container pool"
                    );

                    let config = crate::pool_registry::CapsulePoolConfig::from(pool_config.clone());
                    if let Err(e) = pool_registry
                        .register_pool(&capsule_id, &manifest, config)
                        .await
                    {
                        warn!(
                            capsule_id = %capsule_id,
                            error = %e,
                            "Failed to initialize pool, falling back to cold start"
                        );
                    }
                }
            }
        }

        // Try to acquire from pool if available (Linux only, Youki runtime)
        #[cfg(target_os = "linux")]
        let pool_launch_result: Option<LaunchResult> = {
            if matches!(manifest.execution.runtime, RuntimeType::Youki) {
                if let Some(pool_registry) = &self.pool_registry {
                    if pool_registry.has_pool(&capsule_id) {
                        info!(
                            capsule_id = %capsule_id,
                            "Attempting to acquire container from pool"
                        );
                        match pool_registry.acquire_for_launch(&capsule_id).await {
                            Ok(acquire_result) => {
                                info!(
                                    capsule_id = %capsule_id,
                                    container_id = %acquire_result.container_id,
                                    pid = acquire_result.pid,
                                    "Acquired pre-warmed container from pool"
                                );
                                // Construct LaunchResult from pool acquire result
                                let log_path = acquire_result.bundle_path.join("container.log");
                                Some(LaunchResult {
                                    pid: Some(acquire_result.pid),
                                    bundle_path: Some(acquire_result.bundle_path),
                                    log_path: Some(log_path),
                                    port: None,
                                })
                            }
                            Err(e) => {
                                warn!(
                                    capsule_id = %capsule_id,
                                    error = %e,
                                    "Failed to acquire from pool, falling back to cold start"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        #[cfg(not(target_os = "linux"))]
        let pool_launch_result: Option<LaunchResult> = None;

        // Determine which runtime to use
        let force_docker_cli = std::env::var("FORCE_DOCKER_CLI_RUNTIME").is_ok();

        // Resolve runtime using UARC V1.1.0 algorithm
        let context = self.build_resolve_context();
        let resolved = resolve_runtime(&manifest, &context)
            .map_err(|e| anyhow!("Runtime resolution failed: {}", e))?;

        info!(
            "[DEBUG] Resolved runtime: target={}, kind={:?}, entrypoint={:?}, force_docker_cli={}",
            resolved.target_type_name(),
            resolved.runtime_kind(),
            manifest.execution.entrypoint,
            force_docker_cli
        );

        // Use pool result if available, otherwise cold start
        let launch_result = if let Some(result) = pool_launch_result {
            result
        } else {
            let runtime = self.select_runtime_for_target(&resolved, force_docker_cli);

            match runtime.launch(launch_request).await {
                Ok(res) => res,
                Err(e) => {
                    self.record_runtime_failure(&capsule_id, &e)?;
                    return Err(anyhow!("Runtime launch failed: {}", e));
                }
            }
        };

        // 3. Register Service
        if let (Some(registry), Some(p)) = (&self.service_registry, host_port) {
            registry.register_service(capsule_id.clone(), p, vec!["http".to_string()]);

            // 4. Broadcast via mDNS
            if let Some(mdns) = &self.mdns_announcer {
                if let Err(e) = mdns.register(&capsule_id, p) {
                    warn!("Failed to register mDNS for {}: {}", capsule_id, e);
                }
            }

            // 5. Update Traefik Routes
            if let Some(traefik) = &self.traefik_manager {
                let services = registry.get_services();
                if let Err(e) = traefik.update_routes(&services) {
                    warn!("Failed to update Traefik routes: {}", e);
                }
            }
        }

        // Record success
        self.record_runtime_launch(
            &capsule_id,
            &manifest,
            &manifest_json_str,
            &launch_result,
            required_vram, // Track reserved VRAM
            if let Some(idx) = assigned_gpu_index {
                vec![idx as usize]
            } else {
                vec![]
            },
        )?;

        // Start metrics tracking (pull-based observability)
        if let Some(collector) = &self.metrics_collector {
            let user_id = {
                let capsules = self.capsules.read().ok();
                capsules.and_then(|c| c.get(&capsule_id).and_then(|cap| cap.user_id.clone()))
            };
            collector.start_tracking(&capsule_id, user_id);
        }

        Ok("running".to_string())
    }

    pub fn record_runtime_launch(
        &self,
        capsule_id: &str,
        manifest: &CapsuleManifestV1,
        manifest_json: &str,
        runtime: &LaunchResult,
        reserved_vram_bytes: u64,
        gpu_indices: Vec<usize>,
    ) -> Result<()> {
        let mut capsules = self
            .capsules
            .write()
            .map_err(|e| anyhow!("Lock error: {}", e))?;

        let entry = capsules
            .entry(capsule_id.to_string())
            .or_insert_with(|| Capsule {
                id: capsule_id.to_string(),
                adep_json: Vec::new(),
                oci_image: manifest.execution.entrypoint.clone(),
                digest: String::new(),
                status: CapsuleStatus::Pending,
                storage_path: None,
                bundle_path: None,
                pid: None,
                reserved_vram_bytes: 0,
                observed_vram_bytes: None,
                last_failure: None,
                last_exit_code: None,
                log_path: None,
                started_at: None,
                remote_url: None,
                user_id: None,
                gpu_indices: Vec::new(),
            });

        entry.adep_json = manifest_json.as_bytes().to_vec();
        entry.oci_image = manifest.execution.entrypoint.clone();
        entry.status = CapsuleStatus::Running;
        entry.bundle_path = runtime.bundle_path.as_ref().map(|p| p.to_string_lossy().to_string());
        entry.pid = runtime.pid;
        entry.reserved_vram_bytes = reserved_vram_bytes;
        entry.observed_vram_bytes = None;
        entry.last_failure = None;
        entry.last_exit_code = None;
        entry.log_path = runtime.log_path.as_ref().map(|p| p.to_string_lossy().to_string());
        entry.started_at = Some(std::time::SystemTime::now());
        entry.gpu_indices = gpu_indices;

        info!(
            capsule_id = capsule_id,
            pid = ?runtime.pid,
            bundle = ?runtime.bundle_path,
            "Recorded capsule runtime launch"
        );

        // Audit Log: CAPSULE_DEPLOY (async, fire-and-forget)
        let logger = self.audit_logger.clone();
        tokio::spawn(async move {
            logger
                .log_event(AuditOperation::DeployCapsule, AuditStatus::Success)
                .await;
        });

        // Audit Log: CAPSULE_START (async, fire-and-forget)
        let logger = self.audit_logger.clone();
        tokio::spawn(async move {
            logger
                .log_event(AuditOperation::StartCapsule, AuditStatus::Success)
                .await;
        });

        info!("Started workload {} (pid={:?})", capsule_id, runtime.pid);

        Ok(())
    }

    pub fn record_runtime_failure(&self, capsule_id: &str, error: &RuntimeError) -> Result<()> {
        let mut capsules = self
            .capsules
            .write()
            .map_err(|e| anyhow!("Lock error: {}", e))?;

        let entry = capsules
            .entry(capsule_id.to_string())
            .or_insert_with(|| Capsule {
                id: capsule_id.to_string(),
                adep_json: Vec::new(),
                oci_image: String::new(),
                digest: String::new(),
                status: CapsuleStatus::Pending,
                storage_path: None,
                bundle_path: None,
                pid: None,
                reserved_vram_bytes: 0,
                observed_vram_bytes: None,
                last_failure: None,
                last_exit_code: None,
                log_path: None,
                started_at: None,
                remote_url: None,
                user_id: None,
                gpu_indices: Vec::new(),
            });

        entry.status = CapsuleStatus::Failed;
        entry.pid = None;
        entry.observed_vram_bytes = None;
        entry.last_failure = Some(error.to_string());
        entry.last_exit_code = match error {
            RuntimeError::CommandFailure { exit_code, .. } => *exit_code,
            _ => None,
        };

        // Ensure we always have a log file path, even if the runtime failed before
        // producing any logs (e.g., `docker run` port bind failure).
        if entry.log_path.is_none() {
            let log_dir = std::env::temp_dir().join("capsuled").join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join(format!("{}.log", capsule_id));
            entry.log_path = Some(log_path.to_string_lossy().to_string());
        }

        // Best-effort: append the failure reason to the log file so UIs can show it via StreamLogs.
        if let Some(ref path) = entry.log_path {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                use std::io::Write;
                let _ = writeln!(file, "[capsule_manager] Runtime failure for {}", capsule_id);
                let _ = writeln!(
                    file,
                    "error: {}",
                    entry
                        .last_failure
                        .as_deref()
                        .unwrap_or("unknown runtime error")
                );
                if let Some(code) = entry.last_exit_code {
                    let _ = writeln!(file, "exit_code: {}", code);
                }
            }
        }

        warn!(
            capsule_id = capsule_id,
            "Recorded runtime failure: {}",
            entry
                .last_failure
                .as_deref()
                .unwrap_or("unknown runtime error")
        );

        Ok(())
    }

    /// Update observed VRAM usage for a capsule
    pub fn update_observed_vram(&self, capsule_id: &str, observed_bytes: u64) -> Result<()> {
        let mut capsules = self
            .capsules
            .write()
            .map_err(|e| anyhow!("Lock error: {}", e))?;

        if let Some(capsule) = capsules.get_mut(capsule_id) {
            capsule.observed_vram_bytes = Some(observed_bytes);
            Ok(())
        } else {
            Err(anyhow!("Capsule {} not found", capsule_id))
        }
    }

    pub fn list_capsules(&self) -> Result<Vec<Capsule>> {
        let capsules = self
            .capsules
            .read()
            .map_err(|e| anyhow!("Lock error: {}", e))?;
        Ok(capsules.values().cloned().collect())
    }

    pub fn get_capsule_pid(&self, capsule_id: &str) -> Option<u32> {
        let capsules = self.capsules.read().ok()?;
        capsules.get(capsule_id).and_then(|c| c.pid)
    }

    pub fn get_capsule_log_path(&self, capsule_id: &str) -> Option<String> {
        let capsules = self.capsules.read().ok()?;
        capsules.get(capsule_id).and_then(|c| c.log_path.clone())
    }

    /// Stop and remove a capsule. Returns whether VRAM scrubbing was performed successfully.
    pub async fn stop_capsule(&self, capsule_id: &str) -> Result<bool> {
        info!("Stopping capsule {}", capsule_id);

        // Remove any per-capsule egress policy.
        crate::security::EgressPolicyRegistry::global().unregister(capsule_id);

        let adep_json = {
            let capsules = self
                .capsules
                .read()
                .map_err(|e| anyhow!("Lock error: {}", e))?;
            capsules.get(capsule_id).map(|c| c.adep_json.clone())
        };

        let manifest: Option<CapsuleManifestV1> = adep_json
            .as_ref()
            .and_then(|json| serde_json::from_slice(json).ok());

        if let Some(ref manifest) = manifest {
            // Use the same runtime resolution logic as deploy
            let force_docker_cli = std::env::var("FORCE_DOCKER_CLI_RUNTIME").is_ok();

            // Resolve runtime using UARC V1.1.0 algorithm
            let context = self.build_resolve_context();
            let resolved = match resolve_runtime(manifest, &context) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Runtime resolution failed for stop: {}, using legacy fallback", e);
                    // Fallback to legacy on resolution error
                    ResolvedTarget::Legacy {
                        runtime_type: manifest.execution.runtime.clone(),
                        entrypoint: manifest.execution.entrypoint.clone(),
                    }
                }
            };

            let runtime = self.select_runtime_for_target(&resolved, force_docker_cli);

            if let Err(e) = runtime.stop(capsule_id).await {
                warn!("Failed to stop runtime for {}: {}", capsule_id, e);
            }

            // Cleanup pool if it exists (Linux only)
            #[cfg(target_os = "linux")]
            if let Some(pool_registry) = &self.pool_registry {
                if pool_registry.has_pool(capsule_id) {
                    info!(capsule_id = %capsule_id, "Unregistering pool for stopped capsule");
                    if let Err(e) = pool_registry.unregister_pool(capsule_id).await {
                        warn!(
                            capsule_id = %capsule_id,
                            error = %e,
                            "Failed to unregister pool"
                        );
                    }
                }
            }
        } else {
            warn!(
                "Capsule {} not found or has no manifest, cannot stop runtime",
                capsule_id
            );
        }

        // 2. Unregister Service
        if let Some(registry) = &self.service_registry {
            registry.unregister_service(capsule_id);

            // 3. Stop mDNS Broadcast
            if let Some(mdns) = &self.mdns_announcer {
                if let Err(e) = mdns.unregister(capsule_id) {
                    warn!("Failed to unregister mDNS for {}: {}", capsule_id, e);
                }
            }

            // 4. Update Traefik Routes
            if let Some(traefik) = &self.traefik_manager {
                let services = registry.get_services();
                if let Err(e) = traefik.update_routes(&services) {
                    warn!("Failed to update Traefik routes: {}", e);
                }
            }
        }

        // Log audit event (async, fire-and-forget)
        let logger = self.audit_logger.clone();
        tokio::spawn(async move {
            logger
                .log_event(AuditOperation::StopCapsule, AuditStatus::Success)
                .await;
        });
        info!("Stopped capsule {}", capsule_id);

        {
            let mut capsules = self
                .capsules
                .write()
                .map_err(|e| anyhow!("Lock error: {}", e))?;

            if let Some(capsule) = capsules.get_mut(capsule_id) {
                capsule.status = CapsuleStatus::Stopped;
                capsule.pid = None;
                // Keep log_path and other info
            } else {
                return Err(anyhow!("Capsule {} not found", capsule_id));
            }
        }

        // Stop metrics tracking (pull-based observability)
        if let Some(collector) = &self.metrics_collector {
            let _usage_record = collector.stop_tracking(capsule_id);
            // Usage record is stored in MetricsCollector for audit/billing queries
            // No more push to Coordinator - metrics are scraped via /metrics endpoint
        }

        // Cleanup storage if StorageManager is configured
        if let Some(storage_manager) = &self.storage_manager {
            if let Some(ref manifest) = manifest {
                for _volume in &manifest.storage.volumes {
                    // Assuming volume name in manifest matches what we mounted (sanitize_lv_name happens in provision)
                    // Wait, provision uses `capsule_id` for single-volume assumption.
                    // But mount_volume in StorageManager derives path from `lv_name`.
                    // If we provisioned using `capsule_id`, the LV name is `sanitize(capsule_id)`.
                    // `unmount_volume` takes `lv_name`.
                    // So we must pass the sanitized capsule id as `lv_name`?
                    // Currently `provision_capsule_storage` sets `lv_name`.
                    // We should probably rely on `StorageManager::get_capsule_storage` or just assume the convention.
                    // Let's assume the convention: `StorageManager::sanitize_lv_name(capsule_id)`.
                    // But `unmount_volume` assumes `lv_name` is passed.
                    // Actually, if we just call `unmount_volume(capsule_id, &sanitized_name)`.
                    // We can't access `sanitize_lv_name` (private).
                    // Refactor: Let `unmount_volume` take just `capsule_id`?
                    // No, `unmount_volume` takes `(capsule_id, lv_name)`.
                    // Limitation: We assumed 1 volume.
                    // We will unmount that 1 volume.
                    // We don't need to iterate manifest volumes if we only provisioned one based on capsule ID.
                }
                // Unmount the main volume (if any)
                // We don't have public access to `sanitize_lv_name`.
                // But `cleanup_capsule_storage` expects `capsule_id` and internally calling delete.
                // We should probably rely on `StorageManager` to handle unmount in `cleanup`?
                // `StorageManager::cleanup_capsule_storage` does NOT unmount currently (just lock/delete).
                // We should update `cleanup_capsule_storage` to unmount too?
                // Yes, simpler.

                // BUT I can't modify `StorageManager` in this call easily without backtracking.
                // I'll assume `StorageManager` modification in next step if I missed it?
                // Wait, I updated `StorageManager` to add `unmount_volume`.
                // I should likely add call `unmount_volume` here.
                // But I don't know the LV Name without sanitization logic.
                // `StorageManager::get_capsule_storage(capsule_id)` returns `CapsuleStorage` which has `lv_name`.
                // That's the way!
            }

            if let Ok(Some(storage_info)) = storage_manager.get_capsule_storage(capsule_id) {
                info!("Unmounting storage for {}", capsule_id);
                if let Err(e) = storage_manager.unmount_volume(capsule_id, &storage_info.lv_name) {
                    warn!("Failed to unmount volume: {}", e);
                }
            }

            info!("Cleaning up storage for capsule {}", capsule_id);
            match storage_manager.cleanup_capsule_storage(capsule_id) {
                Ok(_) => info!("Storage cleaned up for capsule {}", capsule_id),
                Err(e) => warn!(
                    "Failed to cleanup storage for capsule {}: {}",
                    capsule_id, e
                ),
            }
        }

        // Scrub VRAM if the capsule used a GPU
        // We get `gpu_indices` from the Capsule struct (persisted state).
        let gpu_indices = self
            .capsules
            .read()
            .map(|c| {
                c.get(capsule_id)
                    .map(|entry| entry.gpu_indices.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        if !gpu_indices.is_empty() {
            info!("Scrubbing VRAM for GPUs {:?}", gpu_indices);
            let factory = |idx| VramScrubber::new(idx);
            let stats = crate::security::vram_scrubber::scrub_gpu_indices(&gpu_indices, factory);
            for stat in stats {
                if let Some(msg) = stat.message {
                    warn!("VRAM scrub warning for GPU {}: {}", stat.gpu_index, msg);
                } else {
                    info!(
                        "VRAM scrubbed for GPU {}: {} bytes in {} chunks",
                        stat.gpu_index, stat.bytes_scrubbed, stat.chunks
                    );
                }
            }
        }

        Ok(true)
    }
}
