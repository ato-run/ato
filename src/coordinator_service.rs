use std::path::PathBuf;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use crate::capsule_manager::{CapsuleManager, DeployCapsuleRequest};
use crate::oci::spec_builder::build_oci_spec;
use crate::proto::onescluster::coordinator::v1::{
    agent_service_server::AgentService as AgentServiceTrait, DeployWorkloadRequest, DeployWorkloadResponse,
    FetchModelRequest, FetchModelResponse, StopWorkloadRequest, StopWorkloadResponse,
};
use crate::runtime::{ContainerRuntime, LaunchRequest, RuntimeError};
use crate::runtime::traits::Runtime;
use crate::workload::manifest_loader::{load_manifest_str, ResourceRequirements};

/// AgentService implements the Agent gRPC service
///
/// This service handles requests from the Coordinator to the Agent:
/// 1. DeployWorkload - Coordinator instructs Agent to deploy a workload
/// 2. StopWorkload - Coordinator instructs Agent to stop a workload
pub struct AgentService {
    runtime: Arc<ContainerRuntime>,
    capsule_manager: Arc<CapsuleManager>,
    allowed_host_paths: Vec<String>,
}

impl AgentService {
    pub fn new(
        capsule_manager: Arc<CapsuleManager>,
        runtime: Arc<ContainerRuntime>,
        allowed_host_paths: Vec<String>,
    ) -> Self {
        Self {
            runtime,
            capsule_manager,
            allowed_host_paths,
        }
    }
}

// Helper functions removed (no longer using Proto manifest types)

#[tonic::async_trait]
impl AgentServiceTrait for AgentService {
    /// Coordinator instructs Agent to deploy a workload
    ///
    /// Flow:
    /// 1. Receive adep_manifest_json from Coordinator
    /// 2. Parse JSON to AdepManifest
    /// 3. Call OCI spec builder (Week 3) to generate config.json
    /// 4. In production: Write config.json to disk and call runc/crun
    /// 5. For simulation: Just validate that OCI spec can be generated
    /// 6. Return success/failure
    async fn deploy_workload(
        &self,
        request: Request<DeployWorkloadRequest>,
    ) -> Result<Response<DeployWorkloadResponse>, Status> {
        let req = request.into_inner();
        if req.workload_id.trim().is_empty() {
            return Err(Status::invalid_argument("workload_id is required"));
        }
        info!(
            "🚀 Received DeployWorkload request from Coordinator for workload {}",
            req.workload_id
        );

        if req.adep_json.is_empty() {
             return Err(Status::invalid_argument(
                 "DeployWorkloadRequest.adep_json must be provided",
             ));
        }

        // Parse manifest to extract basic info for arguments
        let manifest_str = String::from_utf8(req.adep_json.clone())
            .map_err(|e| Status::invalid_argument(format!("Invalid UTF-8 in adep_json: {}", e)))?;

        let (manifest, _) = match load_manifest_str(None, &manifest_str) {
            Ok((m, r)) => (m, r),
            Err(e) => {
                return Err(Status::invalid_argument(format!(
                    "Invalid adep manifest: {}",
                    e
                )))
            }
        };

        let oci_image = manifest.execution.entrypoint.clone();
        let digest = manifest.metadata.get("digest").cloned().unwrap_or_default();

        info!(
            "🚀 Delegating deployment to CapsuleManager: id={}, image={}",
            req.workload_id, oci_image
        );

        // Build DeployCapsuleRequest - pass parsed manifest directly!
        let request = DeployCapsuleRequest {
            capsule_id: req.workload_id.clone(),
            manifest: manifest.clone(),
            raw_manifest_bytes: Some(req.adep_json.clone()), // Original bytes for signature verification
            oci_image,
            digest,
            extra_args: None,
            signature: None,
        };

        match self.capsule_manager.deploy_capsule(request).await {
            Ok(status_str) => {
                info!("✅ Deployment successful via CapsuleManager: status={}", status_str);
                
                // Construct WorkloadStatus (minimal)
                let reserved_vram = manifest.requirements.vram_min_bytes().ok().flatten().unwrap_or(0);
                let status = crate::proto::onescluster::coordinator::v1::WorkloadStatus {
                    workload_id: req.workload_id.clone(),
                    name: manifest.name,
                    reserved_vram_bytes: reserved_vram,
                    phase: crate::proto::onescluster::coordinator::v1::WorkloadPhase::Running as i32,
                    pid: 0, // PIDs are managed internally now
                    observed_vram_bytes: 0,
                };

                Ok(Response::new(DeployWorkloadResponse {
                    workload_id: status.workload_id.clone(),
                    status: Some(status),
                }))
            },
            Err(e) => {
                error!("❌ Deployment failed via CapsuleManager: {}", e);
                Err(Status::internal(format!("Deployment failed: {}", e)))
            }
        }
    }

    async fn stop_workload(
        &self,
        request: Request<StopWorkloadRequest>,
    ) -> Result<Response<StopWorkloadResponse>, Status> {
        let req = request.into_inner();
        info!("StopWorkload request: workload_id={}", req.workload_id);

        match self.capsule_manager.stop_capsule(&req.workload_id).await {
            Ok(_scrubbed) => {
                info!("Workload {} stopped successfully", req.workload_id);
                Ok(Response::new(StopWorkloadResponse {
                    workload_id: req.workload_id,
                    success: true,
                }))
            }
            Err(e) => {
                error!("Failed to stop workload {}: {}", req.workload_id, e);
                Ok(Response::new(StopWorkloadResponse {
                    workload_id: req.workload_id,
                    success: false,
                }))
            }
        }
    }

    /// Coordinator instructs Agent to fetch a model file
    async fn fetch_model(
        &self,
        request: Request<FetchModelRequest>,
    ) -> Result<Response<FetchModelResponse>, Status> {
        let req = request.into_inner();
        info!(
            "FetchModel request: url={}, dest={}",
            req.url, req.destination
        );

        // Validate URL (basic check)
        if req.url.trim().is_empty() {
            return Err(Status::invalid_argument("URL is required"));
        }

        // Validate destination path (security check is done inside downloader, but we can do it early too)
        // We'll let the downloader handle the strict validation and file operations.

        // Spawn the download task asynchronously so we don't block the gRPC thread
        // In a real system, we might want to track this task or return a job ID.
        // For MVP, we'll wait for it (since the prompt says "Simple RPC for MVP").
        // "非同期タスクとして実行し...今回はMVPとして、完了まで待つSimple RPCで構いません"
        // So we will await it here.

        match crate::downloader::download_file(&req.url, &req.destination, &self.allowed_host_paths)
            .await
        {
            Ok(bytes_downloaded) => {
                info!("Model fetched successfully: {}", req.destination);
                Ok(Response::new(FetchModelResponse {
                    success: true,
                    message: "Download completed successfully".to_string(),
                    bytes_downloaded,
                }))
            }
            Err(e) => {
                error!("Failed to fetch model: {}", e);
                Ok(Response::new(FetchModelResponse {
                    success: false,
                    message: format!("Download failed: {}", e),
                    bytes_downloaded: 0,
                }))
            }
        }
    }
}

fn resolve_rootfs_path(manifest: &AdepManifest) -> Result<PathBuf, Status> {
    if let Some(path) = manifest.metadata.get("rootfs_path") {
        let pb = PathBuf::from(path);
        if pb.exists() {
            return Ok(pb);
        }
        return Err(Status::failed_precondition(format!(
            "manifest.metadata.rootfs_path does not exist: {}",
            pb.display()
        )));
    }

    if let Ok(path) = std::env::var("CAPSULED_DEFAULT_ROOTFS") {
        let pb = PathBuf::from(path);
        if pb.exists() {
            return Ok(pb);
        }
        return Err(Status::failed_precondition(format!(
            "CAPSULED_DEFAULT_ROOTFS path does not exist: {}",
            pb.display()
        )));
    }

    Err(Status::failed_precondition(
        "rootfs_path metadata not provided and CAPSULED_DEFAULT_ROOTFS not set",
    ))
}

fn runtime_error_to_status(workload_id: &str, err: RuntimeError) -> Status {
    match err {
        RuntimeError::CommandFailure {
            operation,
            exit_code,
            stderr,
        } => {
            let message = format!(
                "Runtime '{}' failed for workload {} (exit={:?}): {}",
                operation,
                workload_id,
                exit_code,
                stderr.trim()
            );
            Status::internal(message)
        }
        RuntimeError::InvalidConfig(msg) => Status::failed_precondition(format!(
            "Runtime config error for {}: {}",
            workload_id, msg
        )),
        other => Status::internal(format!("Runtime error for {}: {}", workload_id, other)),
    }
}
