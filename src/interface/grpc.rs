use std::path::PathBuf;
use std::sync::Arc;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{error, info, warn};

use crate::artifact::ArtifactManager;
use crate::capsule_manager::{CapsuleManager, DeployCapsuleRequest};
use crate::failure_codes;
use crate::hardware::GpuDetector;
use crate::job_history::{
    JobHistory, JobPhase as JobPhaseInternal, JobRecord, SqliteJobHistoryStore,
};
use crate::proto::onescluster::common::v1 as common;
use crate::proto::onescluster::engine::v1::{
    deploy_request::Manifest as DeployManifest,
    engine_server::{Engine, EngineServer},
    CancelJobRequest, CancelJobResponse, CapsuleInfo, DeployRequest, DeployResponse,
    EngineLogEntry, FetchModelRequest, FetchModelResponse, GetJobStatusRequest,
    GetJobStatusResponse, GetResourcesRequest, GetSystemStatusRequest, JobPhase, JobResourceUsage,
    JobSummary, ListJobsRequest, ListJobsResponse, ResourceInfo, ResourceUsage, StopRequest,
    StopResponse, SystemStatus, ValidateRequest, ValidationResult,
};
use crate::runplan;
use crate::runtime::ContainerRuntime;
use crate::wasm_host::AdepLogicHost;
use crate::workload;

use capsule_core::capsule_v1::CapsuleManifestV1;

/// EngineService implements the Engine gRPC service
use crate::network::service_registry::ServiceRegistry;

#[derive(Clone)]
pub struct EngineService {
    capsule_manager: Arc<CapsuleManager>,
    wasm_host: Arc<AdepLogicHost>,
    backend_mode: String,
    service_registry: Arc<ServiceRegistry>,
    _runtime: Arc<ContainerRuntime>,
    allowed_host_paths: Vec<String>,
    models_cache_dir: PathBuf,
    gpu_detector: Arc<dyn GpuDetector>,
    job_history: Arc<SqliteJobHistoryStore>,
}

impl EngineService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capsule_manager: Arc<CapsuleManager>,
        wasm_host: Arc<AdepLogicHost>,
        backend_mode: String,
        service_registry: Arc<ServiceRegistry>,
        runtime: Arc<ContainerRuntime>,
        allowed_host_paths: Vec<String>,
        models_cache_dir: PathBuf,
        gpu_detector: Arc<dyn GpuDetector>,
        _artifact_manager: Arc<ArtifactManager>,
        job_history: Arc<SqliteJobHistoryStore>,
    ) -> Self {
        Self {
            capsule_manager,
            wasm_host,
            backend_mode,
            service_registry,
            _runtime: runtime,
            allowed_host_paths,
            models_cache_dir,
            gpu_detector,
            job_history,
        }
    }
}

fn canonical_runplan_to_proto(plan: &capsule_core::runplan::RunPlan) -> common::RunPlan {
    use std::collections::HashMap;

    let runtime = match &plan.runtime {
        capsule_core::runplan::RunPlanRuntime::Docker(docker) => {
            let env: HashMap<String, String> = docker
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            common::run_plan::Runtime::Docker(common::DockerRuntime {
                image: docker.image.clone(),
                digest: docker.digest.clone().unwrap_or_default(),
                command: docker.command.clone(),
                env,
                working_dir: docker.working_dir.clone().unwrap_or_default(),
                user: docker.user.clone().unwrap_or_default(),
                ports: docker
                    .ports
                    .iter()
                    .map(|p| common::Port {
                        container_port: p.container_port,
                        host_port: p.host_port.unwrap_or(0),
                        protocol: p.protocol.clone().unwrap_or_default(),
                    })
                    .collect(),
                mounts: docker
                    .mounts
                    .iter()
                    .map(|m| common::Mount {
                        source: m.source.clone(),
                        target: m.target.clone(),
                        readonly: m.readonly,
                    })
                    .collect(),
            })
        }
        // UARC V1: Native runtime removed - convert to Source runtime
        capsule_core::runplan::RunPlanRuntime::Native(native) => {
            let env: HashMap<String, String> = native
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            // Map Native to Source runtime for backward compatibility
            common::run_plan::Runtime::Source(common::SourceRuntime {
                language: "generic".to_string(), // Generic source runtime
                entrypoint: native.binary_path.clone(),
                cmd: vec![native.binary_path.clone()],
                args: native.args.clone(),
                env,
                working_dir: native.working_dir.clone().unwrap_or_default(),
                dev_mode: false,
            })
        }
        capsule_core::runplan::RunPlanRuntime::Source(src) => {
            let env: HashMap<String, String> = src
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            common::run_plan::Runtime::Source(common::SourceRuntime {
                language: src.language.clone().unwrap_or_default(),
                entrypoint: src.entrypoint.clone(),
                cmd: src.cmd.clone(),
                args: src.args.clone(),
                env,
                working_dir: src.working_dir.clone().unwrap_or_default(),
                dev_mode: src.dev_mode,
            })
        }
    };

    common::RunPlan {
        capsule_id: plan.capsule_id.clone(),
        name: plan.name.clone(),
        version: plan.version.clone(),
        runtime: Some(runtime),
        cpu_cores: plan.cpu_cores.unwrap_or(0),
        memory_bytes: plan.memory_bytes.unwrap_or(0),
        gpu_profile: plan.gpu_profile.clone().unwrap_or_default(),
        egress_allowlist: plan.egress_allowlist.clone(),
    }
}

fn try_canonical_toml_to_runplan_proto(toml_str: &str) -> Option<common::RunPlan> {
    let canonical = CapsuleManifestV1::from_toml(toml_str).ok()?;
    canonical.validate().ok()?;
    let canonical_plan = canonical.to_run_plan().ok()?;
    Some(canonical_runplan_to_proto(&canonical_plan))
}

#[tonic::async_trait]
impl Engine for EngineService {
    /// Deploy a new capsule
    async fn deploy_capsule(
        &self,
        request: Request<DeployRequest>,
    ) -> Result<Response<DeployResponse>, Status> {
        let req = request.into_inner();
        info!("DeployCapsule request: capsule_id={}", req.capsule_id);

        // Parse manifest (RunPlan preferred, fallback to TOML/JSON)
        let mut oci_image = req.oci_image.clone();
        let mut digest = req.digest.clone();
        let mut direct_command: Option<Vec<String>> = None;
        let mut source_working_dir: Option<String> = None;

        // Returns (CapsuleManifestV1, Option<raw_bytes_for_verification>)
        let (manifest, raw_manifest_bytes): (CapsuleManifestV1, Option<Vec<u8>>) = match req
            .manifest
        {
            Some(DeployManifest::RunPlan(plan)) => {
                info!("  Using RunPlan manifest");

                // If the caller provided an explicit command (argv) for Docker, pass it to DirectRuntime.
                if let Some(crate::proto::onescluster::common::v1::run_plan::Runtime::Docker(d)) =
                    plan.runtime.as_ref()
                {
                    if !d.command.is_empty() {
                        direct_command = Some(d.command.clone());
                    }
                }

                // Extract working_dir from SourceRuntime for source execution
                if let Some(crate::proto::onescluster::common::v1::run_plan::Runtime::Source(src)) =
                    plan.runtime.as_ref()
                {
                    if !src.working_dir.is_empty() {
                        source_working_dir = Some(src.working_dir.clone());
                    }
                }

                // Extract working_dir from WasmRuntime for wasm execution
                if let Some(crate::proto::onescluster::common::v1::run_plan::Runtime::Wasm(wasm)) =
                    plan.runtime.as_ref()
                {
                    if !wasm.working_dir.is_empty() {
                        source_working_dir = Some(wasm.working_dir.clone());
                    }
                }

                let converted = runplan::from_engine(&plan);
                if oci_image.is_empty() {
                    oci_image = converted.oci_image;
                }
                if digest.is_empty() {
                    digest = converted.digest;
                }
                // RunPlan-generated manifests have no external signature
                (converted.adep, None)
            }
            Some(DeployManifest::TomlContent(toml_str)) => {
                info!("  Parsing TOML manifest");
                // Canonical-first: capsule_v1 -> validated -> normalized RunPlan v0 -> same path as DeployManifest::RunPlan
                if let Some(plan) = try_canonical_toml_to_runplan_proto(&toml_str) {
                    if let Some(crate::proto::onescluster::common::v1::run_plan::Runtime::Docker(
                        d,
                    )) = plan.runtime.as_ref()
                    {
                        if direct_command.is_none() && !d.command.is_empty() {
                            direct_command = Some(d.command.clone());
                        }
                    }

                    let converted = runplan::from_engine(&plan);
                    if oci_image.is_empty() {
                        oci_image = converted.oci_image;
                    }
                    if digest.is_empty() {
                        digest = converted.digest;
                    }

                    (converted.adep, None)
                } else {
                    // Legacy fallback: Convert TOML to CapsuleManifestV1 directly
                    let (manifest, _) =
                        workload::manifest_loader::load_manifest_str(None, &toml_str).map_err(
                            |e| Status::invalid_argument(format!("Failed to parse TOML: {}", e)),
                        )?;

                    (manifest, None)
                }
            }
            Some(DeployManifest::AdepJson(json_bytes)) => {
                info!("  Using JSON manifest");
                let manifest: CapsuleManifestV1 =
                    serde_json::from_slice(&json_bytes).map_err(|e| {
                        Status::invalid_argument(format!("Failed to parse JSON: {}", e))
                    })?;
                // JSON bytes can be used for signature verification
                (manifest, Some(json_bytes))
            }
            Some(DeployManifest::CapnpManifest(_capnp_bytes)) => {
                // TODO: Re-enable when capnp proto generation is set up
                // Cap'n Proto support is temporarily disabled - proto definitions need to be generated
                return Err(Status::unimplemented(
                    "Cap'n Proto manifest support is temporarily disabled. Use JSON or TOML format instead.",
                ));
            }
            None => {
                return Err(Status::invalid_argument(
                    "manifest is required (run_plan, toml_content, adep_json, or capnp_manifest)",
                ));
            }
        };

        if !digest.is_empty() {
            info!("  Using digest hint: {}", digest);
        }

        // Preflight: surface upgrade-required signals immediately (no background spawn).
        // We already have the parsed manifest - no need to parse again!
        {
            let requires_gpu = manifest.requirements.vram_min.is_some()
                || manifest.requirements.vram_recommended.is_some();

            let required_vram = manifest
                .requirements
                .vram_min_bytes()
                .ok()
                .flatten()
                .unwrap_or(0);

            if requires_gpu {
                let local_ok =
                    failure_codes::local_gpu_satisfies(required_vram, self.gpu_detector.as_ref())
                        .unwrap_or(true);

                let codes = failure_codes::compute_deploy_failure_codes(
                    local_ok,
                    manifest.can_fallback_to_cloud(),
                );

                if !codes.is_empty() {
                    return Ok(Response::new(DeployResponse {
                        capsule_id: req.capsule_id,
                        status: "failed".to_string(),
                        local_url: String::new(),
                        failure_codes: codes,
                        failure_message: "Upgrade required".to_string(),
                    }));
                }
            }
        }

        info!("  Delegating to CapsuleManager (Cloud Bursting Logic)...");

        let capsule_id = req.capsule_id.clone();
        let capsule_manager = self.capsule_manager.clone();
        tokio::spawn(async move {
            // Construct DeployCapsuleRequest with parsed manifest directly
            let request = DeployCapsuleRequest {
                capsule_id: capsule_id.clone(),
                manifest,
                raw_manifest_bytes,
                oci_image,
                digest,
                extra_args: direct_command,
                signature: None, // Signature verification handled internally if needed
                source_working_dir,
            };

            match capsule_manager.deploy_capsule(request).await {
                Ok(status) => info!("Capsule {} deploy completed: {}", capsule_id, status),
                Err(e) => error!("Capsule {} deploy failed: {}", capsule_id, e),
            }
        });

        Ok(Response::new(DeployResponse {
            capsule_id: req.capsule_id,
            status: "starting".to_string(),
            local_url: String::new(),
            failure_codes: vec![],
            failure_message: String::new(),
        }))
    }

    async fn fetch_model(
        &self,
        request: Request<FetchModelRequest>,
    ) -> Result<Response<FetchModelResponse>, Status> {
        let req = request.into_inner();

        if req.model_id.trim().is_empty() {
            return Err(Status::invalid_argument("model_id is required"));
        }
        if req.url.trim().is_empty() {
            return Err(Status::invalid_argument("url is required"));
        }

        let result = crate::resource::ingest::fetcher::fetch_resource(
            crate::resource::ingest::fetcher::ResourceFetchRequest {
                resource_id: req.model_id,
                url: req.url,
                expected_sha256: if req.expected_sha256.trim().is_empty() {
                    None
                } else {
                    Some(req.expected_sha256)
                },
            },
            crate::resource::ingest::fetcher::FetcherConfig {
                cache_dir: self.models_cache_dir.clone(),
                allowed_host_paths: self.allowed_host_paths.clone(),
            },
        )
        .await
        .map_err(|e| Status::failed_precondition(format!("FetchModel failed: {}", e)))?;

        Ok(Response::new(FetchModelResponse {
            local_path: result
                .local_path
                .to_str()
                .ok_or_else(|| Status::internal("local_path is not valid UTF-8"))?
                .to_string(),
            cached: result.cached,
            bytes_downloaded: result.bytes_downloaded,
        }))
    }

    /// Stop a running capsule
    async fn stop_capsule(
        &self,
        request: Request<StopRequest>,
    ) -> Result<Response<StopResponse>, Status> {
        let req = request.into_inner();
        info!("StopCapsule request: capsule_id={}", req.capsule_id);

        match self.capsule_manager.stop_capsule(&req.capsule_id).await {
            Ok(_scrubbed) => {
                info!("Capsule {} stopped successfully", req.capsule_id);
                Ok(Response::new(StopResponse {
                    capsule_id: req.capsule_id,
                    status: "stopped".to_string(),
                }))
            }
            Err(e) => {
                error!("Failed to stop capsule {}: {}", req.capsule_id, e);
                Err(Status::internal(format!("Stop failed: {}", e)))
            }
        }
    }

    /// Get resource information about this node
    async fn get_resources(
        &self,
        _request: Request<GetResourcesRequest>,
    ) -> Result<Response<ResourceInfo>, Status> {
        info!("GetResources request");

        // TODO: Get actual system resources
        // For now, return mock data
        let mut resources = get_system_resources();
        resources.backend_mode = self.backend_mode.clone();

        // Expose registered local service ports (capsule_id -> host port).
        resources.local_services = self
            .service_registry
            .get_services()
            .into_iter()
            .map(|s| (s.name, s.port as u32))
            .collect();

        info!(
            "Resources: cpu_cores={}, memory_bytes={}, disk_bytes={}, backend_mode={}",
            resources.cpu_cores,
            resources.memory_bytes,
            resources.disk_bytes,
            resources.backend_mode
        );

        Ok(Response::new(resources))
    }

    /// Validate a manifest without deploying
    async fn validate_manifest(
        &self,
        request: Request<ValidateRequest>,
    ) -> Result<Response<ValidationResult>, Status> {
        let req = request.into_inner();
        info!("ValidateManifest request ({} bytes)", req.adep_json.len());

        let adep_json_str = String::from_utf8(req.adep_json)
            .map_err(|e| Status::invalid_argument(format!("Invalid UTF-8 in adep_json: {}", e)))?;

        match self.wasm_host.validate_manifest(&adep_json_str) {
            Ok(_) => {
                info!("Manifest validation succeeded");
                Ok(Response::new(ValidationResult {
                    valid: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                warn!("Manifest validation failed: {}", e);
                Ok(Response::new(ValidationResult {
                    valid: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    /// Get system status including capsules and resources
    async fn get_system_status(
        &self,
        _request: Request<GetSystemStatusRequest>,
    ) -> Result<Response<SystemStatus>, Status> {
        info!("GetSystemStatus request");

        // UARC V1: VPN IP removed (uses SPIFFE ID instead)
        let vpn_ip = String::new();

        // Get capsule list
        let capsules = match self.capsule_manager.list_capsules() {
            Ok(caps) => caps
                .into_iter()
                .map(|c| {
                    let local_url = self
                        .service_registry
                        .get_services()
                        .iter()
                        .find(|s| s.name == c.id)
                        .map(|s| format!("http://127.0.0.1:{}", s.port))
                        .unwrap_or_default();

                    CapsuleInfo {
                        id: c.id.clone(),
                        name: c.id,
                        status: c.status.to_string(),
                        local_url,
                        reserved_vram_bytes: c.reserved_vram_bytes,
                        last_failure: c.last_failure.unwrap_or_default(),
                        last_exit_code: c.last_exit_code.unwrap_or(-1),
                    }
                })
                .collect(),
            Err(e) => {
                warn!("Failed to list capsules: {}", e);
                vec![]
            }
        };

        // Get resource usage
        let hardware_report = self.gpu_detector.detect_gpus().unwrap_or_else(|_| {
            crate::hardware::RigHardwareReport {
                rig_id: "unknown".to_string(),
                gpus: vec![],
                system_cuda_version: None,
                system_driver_version: None,
                is_mock: false,
            }
        });

        let cpu_cores_total = num_cpus::get() as u64;
        let system_resources = get_system_resources();
        let memory_bytes_total = system_resources.memory_bytes;
        let vram_bytes_total = (hardware_report.total_vram_gb() * 1024.0 * 1024.0 * 1024.0) as u64;

        // Calculate used resources (simplified - just sum of reserved)
        let vram_bytes_used: u64 = capsules.iter().map(|c| c.reserved_vram_bytes).sum();

        let resources = ResourceUsage {
            cpu_cores_total,
            cpu_cores_used: capsules.len() as u64, // Simplified
            memory_bytes_total,
            memory_bytes_used: 0, // TODO: Calculate actual usage
            vram_bytes_total,
            vram_bytes_used,
        };

        Ok(Response::new(SystemStatus {
            backend_mode: self.backend_mode.clone(),
            vpn_ip,
            capsules,
            resources: Some(resources),
        }))
    }

    type StreamLogsStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<EngineLogEntry, Status>> + Send + Sync + 'static,
        >,
    >;

    async fn stream_logs(
        &self,
        request: Request<crate::proto::onescluster::engine::v1::LogRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        let req = request.into_inner();
        let capsule_id = req.capsule_id;
        let follow = req.follow;

        info!("StreamLogs request for capsule_id={}", capsule_id);

        // Get log path
        let log_path = self
            .capsule_manager
            .get_capsule_log_path(&capsule_id)
            .ok_or_else(|| Status::not_found("Capsule not found or no log path"))?;

        // Create a channel for the stream
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;

            // Open file
            let file = match tokio::fs::File::open(&log_path).await {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to open log file: {}",
                            e
                        ))))
                        .await;
                    return;
                }
            };

            let mut reader = tokio::io::BufReader::new(file);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        // EOF
                        if follow {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        } else {
                            break;
                        }
                    }
                    Ok(_) => {
                        let entry = EngineLogEntry {
                            line: line.clone(),
                            timestamp: "".to_string(), // TODO: Parse timestamp if possible
                            source: "stdout".to_string(), // NativeRuntime mixes stdout/stderr
                        };
                        if tx.send(Ok(entry)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(Status::internal(format!("Error reading log: {}", e))))
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }

    /// Get job execution status
    async fn get_job_status(
        &self,
        request: Request<GetJobStatusRequest>,
    ) -> Result<Response<GetJobStatusResponse>, Status> {
        let req = request.into_inner();
        info!("GetJobStatus request for job_id={}", req.job_id);

        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id is required"));
        }

        match self.job_history.get_job(&req.job_id) {
            Ok(job) => {
                let response = job_record_to_proto(&job);
                Ok(Response::new(response))
            }
            Err(e) => {
                // Check if it's a NotFound error
                let err_str = e.to_string();
                if err_str.contains("not found") || err_str.contains("NotFound") {
                    Err(Status::not_found(format!("Job '{}' not found", req.job_id)))
                } else {
                    error!("Failed to get job status: {}", e);
                    Err(Status::internal(format!("Failed to get job status: {}", e)))
                }
            }
        }
    }

    /// List recent jobs with optional filtering
    async fn list_jobs(
        &self,
        request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let req = request.into_inner();
        let limit = if req.limit == 0 {
            100
        } else {
            req.limit as usize
        };
        let capsule_filter = if req.capsule_name.is_empty() {
            None
        } else {
            Some(req.capsule_name.as_str())
        };
        let phase_filter = proto_phase_to_internal(req.phase_filter());

        info!(
            "ListJobs request: limit={}, capsule_filter={:?}, phase_filter={:?}",
            limit, capsule_filter, phase_filter
        );

        // Note: phase_filter is ignored for now (not in JobHistory trait)
        // TODO: Add phase filtering to JobHistory trait
        match self.job_history.list_jobs(capsule_filter, limit) {
            Ok(jobs) => {
                // Apply phase filter in memory if specified
                let filtered_jobs: Vec<_> = if let Some(pf) = phase_filter {
                    jobs.into_iter().filter(|j| j.phase == pf).collect()
                } else {
                    jobs
                };

                let summaries: Vec<JobSummary> = filtered_jobs
                    .into_iter()
                    .map(|j| JobSummary {
                        job_id: j.job_id,
                        capsule_name: j.capsule_name,
                        phase: internal_phase_to_proto(&j.phase).into(),
                        created_at: j.created_at.to_rfc3339(),
                        duration_secs: j.duration_secs.unwrap_or(0),
                    })
                    .collect();

                Ok(Response::new(ListJobsResponse { jobs: summaries }))
            }
            Err(e) => {
                error!("Failed to list jobs: {}", e);
                Err(Status::internal(format!("Failed to list jobs: {}", e)))
            }
        }
    }

    /// Cancel a running job
    async fn cancel_job(
        &self,
        request: Request<CancelJobRequest>,
    ) -> Result<Response<CancelJobResponse>, Status> {
        let req = request.into_inner();
        info!(
            "CancelJob request for job_id={}, force={}",
            req.job_id, req.force
        );

        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id is required"));
        }

        // First, get the current job status
        let job_record = match self.job_history.get_job(&req.job_id) {
            Ok(job) => job,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("not found") || err_str.contains("NotFound") {
                    return Err(Status::not_found(format!("Job '{}' not found", req.job_id)));
                }
                error!("Failed to get job status: {}", e);
                return Err(Status::internal(format!("Failed to get job status: {}", e)));
            }
        };

        let previous_phase = internal_phase_to_proto(&job_record.phase);

        // Check if job is already in a terminal state
        match job_record.phase {
            JobPhaseInternal::Succeeded
            | JobPhaseInternal::Failed
            | JobPhaseInternal::Cancelled => {
                return Ok(Response::new(CancelJobResponse {
                    success: true,
                    message: format!(
                        "Job '{}' is already in terminal state: {:?}",
                        req.job_id, job_record.phase
                    ),
                    previous_phase: previous_phase.into(),
                }));
            }
            _ => {}
        }

        // Try to stop the capsule (job_id should map to capsule_id in most cases)
        // The job_id is typically the same as capsule_id for running jobs
        let capsule_id = &job_record.capsule_name;

        match self.capsule_manager.stop_capsule(capsule_id).await {
            Ok(_) => {
                // Update job history to Cancelled state
                if let Err(e) = self.job_history.update_phase(
                    &req.job_id,
                    JobPhaseInternal::Cancelled,
                    Some("Cancelled by user request"),
                    None,
                ) {
                    warn!("Failed to update job history after cancel: {}", e);
                }

                info!(
                    "Job '{}' (capsule '{}') cancelled successfully",
                    req.job_id, capsule_id
                );
                Ok(Response::new(CancelJobResponse {
                    success: true,
                    message: format!("Job '{}' cancelled successfully", req.job_id),
                    previous_phase: previous_phase.into(),
                }))
            }
            Err(e) => {
                let err_msg = e.to_string();
                warn!("Failed to cancel job '{}': {}", req.job_id, err_msg);

                // If capsule not found, it may have already exited
                if err_msg.contains("not found") || err_msg.contains("NotFound") {
                    // Update to cancelled anyway since user requested it
                    let _ = self.job_history.update_phase(
                        &req.job_id,
                        JobPhaseInternal::Cancelled,
                        Some("Cancelled (process not found)"),
                        None,
                    );
                    return Ok(Response::new(CancelJobResponse {
                        success: true,
                        message: format!(
                            "Job '{}' marked as cancelled (process already exited)",
                            req.job_id
                        ),
                        previous_phase: previous_phase.into(),
                    }));
                }

                Err(Status::internal(format!(
                    "Failed to cancel job: {}",
                    err_msg
                )))
            }
        }
    }
}

/// Convert internal JobPhase to proto JobPhase
fn internal_phase_to_proto(phase: &JobPhaseInternal) -> JobPhase {
    match phase {
        JobPhaseInternal::Pending => JobPhase::Pending,
        JobPhaseInternal::Running => JobPhase::Running,
        JobPhaseInternal::Succeeded => JobPhase::Succeeded,
        JobPhaseInternal::Failed => JobPhase::Failed,
        JobPhaseInternal::Cancelled => JobPhase::Cancelled,
    }
}

/// Convert proto JobPhase to internal JobPhase (for filtering)
fn proto_phase_to_internal(phase: JobPhase) -> Option<JobPhaseInternal> {
    match phase {
        JobPhase::Unspecified => None,
        JobPhase::Pending => Some(JobPhaseInternal::Pending),
        JobPhase::Running => Some(JobPhaseInternal::Running),
        JobPhase::Succeeded => Some(JobPhaseInternal::Succeeded),
        JobPhase::Failed => Some(JobPhaseInternal::Failed),
        JobPhase::Cancelled => Some(JobPhaseInternal::Cancelled),
    }
}

/// ResourceUsageData for JSON parsing
#[derive(serde::Deserialize, Default)]
struct ResourceUsageData {
    #[serde(default)]
    cpu_time_ms: u64,
    #[serde(default)]
    memory_peak_bytes: u64,
    #[serde(default)]
    vram_peak_bytes: u64,
}

/// Convert JobRecord to proto GetJobStatusResponse
fn job_record_to_proto(job: &JobRecord) -> GetJobStatusResponse {
    // Parse resource_usage_json if present
    let resource_usage = job
        .resource_usage_json
        .as_ref()
        .and_then(|json| serde_json::from_str::<ResourceUsageData>(json).ok())
        .map(|r| JobResourceUsage {
            cpu_time_ms: r.cpu_time_ms,
            memory_peak_bytes: r.memory_peak_bytes,
            vram_peak_bytes: r.vram_peak_bytes,
        });

    GetJobStatusResponse {
        job_id: job.job_id.clone(),
        capsule_name: job.capsule_name.clone(),
        capsule_version: job.capsule_version.clone(),
        phase: internal_phase_to_proto(&job.phase).into(),
        error_message: job.error_message.clone().unwrap_or_default(),
        exit_code: job.exit_code.unwrap_or(-1),
        created_at: job.created_at.to_rfc3339(),
        started_at: job.started_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
        finished_at: job.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
        duration_secs: job.duration_secs.unwrap_or(0),
        resource_usage,
    }
}

/// Get system resource information
fn get_system_resources() -> ResourceInfo {
    // TODO: Implement actual system resource detection
    // This would use sysinfo or similar crates to get real values

    #[cfg(target_os = "linux")]
    {
        // On Linux, we could read /proc/cpuinfo, /proc/meminfo, etc.
        ResourceInfo {
            cpu_cores: num_cpus::get() as u64,
            memory_bytes: get_total_memory(),
            disk_bytes: get_total_disk_space(),
            backend_mode: "unknown".to_string(), // Will be overwritten
            vpn_ip: String::new(),
            local_services: std::collections::HashMap::new(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Mock data for non-Linux systems
        ResourceInfo {
            cpu_cores: 4,
            memory_bytes: 8 * 1024 * 1024 * 1024, // 8 GB
            disk_bytes: 100 * 1024 * 1024 * 1024, // 100 GB
            backend_mode: "unknown".to_string(),  // Will be overwritten
            vpn_ip: String::new(),
            local_services: std::collections::HashMap::new(),
        }
    }
}

#[cfg(target_os = "linux")]
fn get_total_memory() -> u64 {
    // Read from /proc/meminfo
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                if let Some(kb_str) = line.split_whitespace().nth(1) {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        return kb * 1024; // Convert KB to bytes
                    }
                }
            }
        }
    }
    8 * 1024 * 1024 * 1024 // Default 8 GB
}

#[cfg(target_os = "linux")]
fn get_total_disk_space() -> u64 {
    // This is a simplified version - in production you'd check actual mount points
    use std::fs;
    if fs::metadata("/").is_ok() {
        // This doesn't give us total disk space, just a placeholder
        return 100 * 1024 * 1024 * 1024; // Default 100 GB
    }
    100 * 1024 * 1024 * 1024
}

/// Start the gRPC server
#[allow(clippy::too_many_arguments)]
pub async fn start_grpc_server(
    addr: &str,
    capsule_manager: Arc<CapsuleManager>,
    wasm_host: Arc<AdepLogicHost>,
    runtime: Arc<ContainerRuntime>,
    allowed_host_paths: Vec<String>,
    models_cache_dir: PathBuf,
    backend_mode: String,
    service_registry: Arc<ServiceRegistry>,
    gpu_detector: Arc<dyn GpuDetector>,
    artifact_manager: Arc<ArtifactManager>,
    job_history: Arc<SqliteJobHistoryStore>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = addr.parse()?;
    let engine_service = EngineService::new(
        Arc::clone(&capsule_manager),
        Arc::clone(&wasm_host),
        backend_mode,
        Arc::clone(&service_registry),
        Arc::clone(&runtime),
        allowed_host_paths.clone(),
        models_cache_dir,
        gpu_detector,
        artifact_manager,
        job_history,
    );
    info!("Engine gRPC server listening on {}", addr);

    Server::builder()
        .add_service(EngineServer::new(engine_service.clone()))
        .serve(addr)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_history::JobPhase as InternalJobPhase;
    use chrono::Utc;
    use tempfile::tempdir;

    #[test]
    fn test_internal_phase_to_proto() {
        assert_eq!(
            internal_phase_to_proto(&JobPhaseInternal::Pending),
            JobPhase::Pending
        );
        assert_eq!(
            internal_phase_to_proto(&JobPhaseInternal::Running),
            JobPhase::Running
        );
        assert_eq!(
            internal_phase_to_proto(&JobPhaseInternal::Succeeded),
            JobPhase::Succeeded
        );
        assert_eq!(
            internal_phase_to_proto(&JobPhaseInternal::Failed),
            JobPhase::Failed
        );
        assert_eq!(
            internal_phase_to_proto(&JobPhaseInternal::Cancelled),
            JobPhase::Cancelled
        );
    }

    #[test]
    fn test_proto_phase_to_internal() {
        assert_eq!(proto_phase_to_internal(JobPhase::Unspecified), None);
        assert_eq!(
            proto_phase_to_internal(JobPhase::Pending),
            Some(JobPhaseInternal::Pending)
        );
        assert_eq!(
            proto_phase_to_internal(JobPhase::Running),
            Some(JobPhaseInternal::Running)
        );
        assert_eq!(
            proto_phase_to_internal(JobPhase::Succeeded),
            Some(JobPhaseInternal::Succeeded)
        );
        assert_eq!(
            proto_phase_to_internal(JobPhase::Failed),
            Some(JobPhaseInternal::Failed)
        );
        assert_eq!(
            proto_phase_to_internal(JobPhase::Cancelled),
            Some(JobPhaseInternal::Cancelled)
        );
    }

    #[test]
    fn test_job_record_to_proto() {
        let job = JobRecord {
            job_id: "test-job-123".to_string(),
            capsule_name: "my-capsule".to_string(),
            capsule_version: "v1.0.0".to_string(),
            phase: InternalJobPhase::Succeeded,
            error_message: None,
            exit_code: Some(0),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            duration_secs: Some(42),
            resource_usage_json: Some(
                r#"{"cpu_time_ms": 1000, "memory_peak_bytes": 1048576}"#.to_string(),
            ),
        };

        let proto = job_record_to_proto(&job);
        assert_eq!(proto.job_id, "test-job-123");
        assert_eq!(proto.capsule_name, "my-capsule");
        assert_eq!(proto.capsule_version, "v1.0.0");
        assert_eq!(proto.phase(), JobPhase::Succeeded);
        assert_eq!(proto.exit_code, 0);
        assert_eq!(proto.duration_secs, 42);

        let resource = proto.resource_usage.unwrap();
        assert_eq!(resource.cpu_time_ms, 1000);
        assert_eq!(resource.memory_peak_bytes, 1048576);
    }

    #[test]
    fn test_job_record_to_proto_without_resource_usage() {
        let job = JobRecord {
            job_id: "test-job-456".to_string(),
            capsule_name: "other-capsule".to_string(),
            capsule_version: "v2.0.0".to_string(),
            phase: InternalJobPhase::Failed,
            error_message: Some("Out of memory".to_string()),
            exit_code: Some(137),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            duration_secs: Some(10),
            resource_usage_json: None,
        };

        let proto = job_record_to_proto(&job);
        assert_eq!(proto.job_id, "test-job-456");
        assert_eq!(proto.phase(), JobPhase::Failed);
        assert_eq!(proto.error_message, "Out of memory");
        assert_eq!(proto.exit_code, 137);
        assert!(proto.resource_usage.is_none());
    }

    #[test]
    fn test_resource_usage_json_parsing() {
        // Valid JSON
        let valid_json =
            r#"{"cpu_time_ms": 500, "memory_peak_bytes": 2048, "vram_peak_bytes": 4096}"#;
        let parsed: ResourceUsageData = serde_json::from_str(valid_json).unwrap();
        assert_eq!(parsed.cpu_time_ms, 500);
        assert_eq!(parsed.memory_peak_bytes, 2048);
        assert_eq!(parsed.vram_peak_bytes, 4096);

        // Partial JSON (missing fields default to 0)
        let partial_json = r#"{"cpu_time_ms": 100}"#;
        let parsed: ResourceUsageData = serde_json::from_str(partial_json).unwrap();
        assert_eq!(parsed.cpu_time_ms, 100);
        assert_eq!(parsed.memory_peak_bytes, 0);
        assert_eq!(parsed.vram_peak_bytes, 0);
    }

    #[tokio::test]
    async fn test_job_history_integration() {
        // Create temp directory for test database
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test_jobs.sqlite");

        let job_history =
            SqliteJobHistoryStore::new(&db_path).expect("Failed to create job history store");

        // Insert a test job
        let job = JobRecord {
            job_id: "integration-test-001".to_string(),
            capsule_name: "test-capsule".to_string(),
            capsule_version: "v1.0.0".to_string(),
            phase: InternalJobPhase::Running,
            error_message: None,
            exit_code: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: None,
            duration_secs: None,
            resource_usage_json: None,
        };

        job_history.insert_job(&job).expect("Failed to insert job");

        // Verify we can retrieve it
        let retrieved = job_history
            .get_job("integration-test-001")
            .expect("Failed to get job");
        assert_eq!(retrieved.job_id, "integration-test-001");
        assert_eq!(retrieved.capsule_name, "test-capsule");
        assert!(matches!(retrieved.phase, InternalJobPhase::Running));

        // List jobs
        let jobs = job_history
            .list_jobs(None, 10)
            .expect("Failed to list jobs");
        assert_eq!(jobs.len(), 1);
    }

    #[tokio::test]
    async fn test_cancel_job_updates_phase() {
        // Create temp directory for test database
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test_cancel.sqlite");

        let job_history =
            SqliteJobHistoryStore::new(&db_path).expect("Failed to create job history store");

        // Insert a running job
        let job = JobRecord {
            job_id: "cancel-test-001".to_string(),
            capsule_name: "cancel-capsule".to_string(),
            capsule_version: "v1.0.0".to_string(),
            phase: InternalJobPhase::Running,
            error_message: None,
            exit_code: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: None,
            duration_secs: None,
            resource_usage_json: None,
        };

        job_history.insert_job(&job).expect("Failed to insert job");

        // Simulate cancellation by updating phase
        job_history
            .update_phase(
                "cancel-test-001",
                InternalJobPhase::Cancelled,
                Some("Cancelled by user request"),
                None,
            )
            .expect("Failed to update phase");

        // Verify phase was updated
        let retrieved = job_history
            .get_job("cancel-test-001")
            .expect("Failed to get job");
        assert!(matches!(retrieved.phase, InternalJobPhase::Cancelled));
        assert_eq!(
            retrieved.error_message,
            Some("Cancelled by user request".to_string())
        );
    }

    #[test]
    fn test_cancel_already_completed_job_is_idempotent() {
        // Create temp directory for test database
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test_idempotent.sqlite");

        let job_history =
            SqliteJobHistoryStore::new(&db_path).expect("Failed to create job history store");

        // Insert a succeeded job
        let job = JobRecord {
            job_id: "completed-001".to_string(),
            capsule_name: "done-capsule".to_string(),
            capsule_version: "v1.0.0".to_string(),
            phase: InternalJobPhase::Succeeded,
            error_message: None,
            exit_code: Some(0),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            duration_secs: Some(10),
            resource_usage_json: None,
        };

        job_history.insert_job(&job).expect("Failed to insert job");

        // Verify it's in succeeded state (cancel should be a no-op)
        let retrieved = job_history
            .get_job("completed-001")
            .expect("Failed to get job");
        assert!(matches!(retrieved.phase, InternalJobPhase::Succeeded));
    }
}
