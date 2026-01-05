use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::{
    task::JoinHandle,
    time::{interval, sleep, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};

use crate::{
    adep::AdepManifest,
    capsule_manager::{Capsule, CapsuleManager, CapsuleStatus},
    hardware::{GpuDetectionError, GpuDetector, GpuProcessMonitor, RigHardwareReport},
    proto::onescluster::coordinator::v1::{
        coordinator_service_client::CoordinatorServiceClient, GpuInfo as ProtoGpuInfo,
        HardwareState, RigStatus, StatusReportRequest, Taint, WorkloadPhase, WorkloadStatus,
    },
};

type GrpcClient = CoordinatorServiceClient<tonic::transport::Channel>;

const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 60;

/// Periodically reports Agent hardware + workload status back to the Coordinator.
pub struct StatusReporter {
    coordinator_endpoint: String,
    interval: Duration,
    capsule_manager: Arc<CapsuleManager>,
    gpu_detector: Arc<dyn GpuDetector>,
    gpu_process_monitor: Arc<dyn GpuProcessMonitor>,
    client: Option<GrpcClient>,
    backoff: Backoff,
    taints: Vec<Taint>,
}

impl StatusReporter {
    /// Create a new status reporter.
    pub fn new(
        coordinator_endpoint: impl Into<String>,
        interval: Duration,
        capsule_manager: Arc<CapsuleManager>,
        gpu_detector: Arc<dyn GpuDetector>,
        gpu_process_monitor: Arc<dyn GpuProcessMonitor>,
        taints: Vec<Taint>,
    ) -> Self {
        let endpoint = sanitize_endpoint(coordinator_endpoint.into());
        Self {
            coordinator_endpoint: endpoint,
            interval,
            capsule_manager,
            gpu_detector,
            gpu_process_monitor,
            client: None,
            backoff: Backoff::default(),
            taints,
        }
    }

    /// Start the periodic reporting task.
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run().await {
                error!("Status reporter terminated: {err}");
            }
        })
    }

    async fn run(mut self) -> Result<(), StatusReporterError> {
        info!(
            "Starting status reporter: endpoint={} interval={}s",
            self.coordinator_endpoint,
            self.interval.as_secs()
        );

        // Send an initial report immediately so the Coordinator learns about this rig quickly.
        if let Err(err) = self.send_report().await {
            error!("Initial status report failed: {err}");
        }

        let mut ticker = interval(self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            if let Err(err) = self.send_report().await {
                warn!("Status report failed: {err}");
            }
        }
    }

    async fn send_report(&mut self) -> Result<(), StatusReporterError> {
        let report = self
            .gpu_detector
            .detect_gpus()
            .map_err(StatusReporterError::Hardware)?;

        let mut capsules = self
            .capsule_manager
            .list_capsules()
            .map_err(StatusReporterError::CapsuleList)?;

        let usage_by_pid = self.collect_gpu_process_usage();
        self.apply_observed_vram(&mut capsules, &usage_by_pid);

        let workloads = self.collect_workloads(&capsules);
        let _total_reserved_vram: u64 = workloads.iter().map(|wl| wl.reserved_vram_bytes).sum();
        let total_observed_vram: u64 = workloads
            .iter()
            .map(|wl| {
                if wl.observed_vram_bytes > 0 {
                    wl.observed_vram_bytes
                } else {
                    wl.reserved_vram_bytes
                }
            })
            .sum();
        let hardware_state = self.build_hardware_state(&report, total_observed_vram);

        let status = RigStatus {
            rig_id: report.rig_id.clone(),
            hardware: hardware_state,
            running_workloads: workloads,
            taints: self.taints.clone(),
            reported_at_unix_seconds: current_unix_timestamp(),
            is_mock: report.is_mock,
        };

        let mut client = self.ensure_client().await?;

        debug!("Sending status report to Coordinator");
        match client
            .report_status(StatusReportRequest {
                status: Some(status),
            })
            .await
        {
            Ok(_) => {
                self.backoff.reset();
                info!("Status report delivered to {}", self.coordinator_endpoint);
                Ok(())
            }
            Err(status_err) => {
                if is_retryable_status(&status_err) {
                    self.client = None;
                    let delay = self.backoff.next_delay();
                    warn!(
                        "Coordinator returned retryable error ({}). Retrying in {:?}",
                        status_err, delay
                    );
                    sleep(delay).await;
                }
                Err(StatusReporterError::Grpc(status_err))
            }
        }
    }

    fn collect_gpu_process_usage(&self) -> HashMap<u32, u64> {
        match self.gpu_process_monitor.collect_usage_bytes() {
            Ok(map) => map,
            Err(err) => {
                warn!("Failed to collect GPU process usage: {err}");
                HashMap::new()
            }
        }
    }

    fn apply_observed_vram(&self, capsules: &mut [Capsule], usage_by_pid: &HashMap<u32, u64>) {
        for capsule in capsules.iter_mut() {
            let Some(pid) = capsule.pid else { continue };
            let Some(&observed) = usage_by_pid.get(&pid) else {
                continue;
            };

            capsule.observed_vram_bytes = Some(observed);

            if let Err(err) = self
                .capsule_manager
                .update_observed_vram(&capsule.id, observed)
            {
                warn!(
                    capsule_id = %capsule.id,
                    pid,
                    error = %err,
                    "Failed to persist observed VRAM measurement"
                );
            } else {
                debug!(
                    capsule_id = %capsule.id,
                    pid,
                    observed_vram_bytes = observed,
                    "Updated capsule observed VRAM measurement"
                );
            }
        }
    }

    fn collect_workloads(&self, capsules: &[Capsule]) -> Vec<WorkloadStatus> {
        capsules
            .iter()
            .filter_map(|capsule| self.capsule_to_workload(capsule))
            .collect()
    }

    fn capsule_to_workload(&self, capsule: &Capsule) -> Option<WorkloadStatus> {
        if matches!(capsule.status, CapsuleStatus::Stopped) {
            return None;
        }

        let manifest = serde_json::from_slice::<AdepManifest>(&capsule.adep_json).ok();
        let name = manifest
            .as_ref()
            .map(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| capsule.id.clone());

        let reserved_vram_bytes = manifest
            .as_ref()
            .map(|m| m.required_vram_bytes())
            .unwrap_or(0);

        let observed_vram_bytes = capsule.observed_vram_bytes.unwrap_or(reserved_vram_bytes);

        let pid = capsule.pid.unwrap_or(0) as u64;

        let phase = match capsule.status {
            CapsuleStatus::Pending => WorkloadPhase::Pending,
            CapsuleStatus::Provisioning => WorkloadPhase::Pending,
            CapsuleStatus::Running => WorkloadPhase::Running,
            CapsuleStatus::Stopped => WorkloadPhase::Succeeded,
            CapsuleStatus::Failed => WorkloadPhase::Failed,
        } as i32;

        Some(WorkloadStatus {
            workload_id: capsule.id.clone(),
            name,
            reserved_vram_bytes,
            phase,
            pid,
            observed_vram_bytes,
        })
    }

    fn build_hardware_state(
        &self,
        report: &RigHardwareReport,
        fallback_used_vram: u64,
    ) -> Option<HardwareState> {
        if report.gpus.is_empty()
            && report.system_cuda_version.is_none()
            && report.system_driver_version.is_none()
        {
            return None;
        }

        let total_vram_bytes = report.total_vram_bytes();
        let mut used_vram_bytes = report
            .gpus
            .iter()
            .map(|gpu| {
                gpu.vram_used_bytes.unwrap_or_else(|| {
                    let available = gpu.vram_available_bytes();
                    gpu.vram_total_bytes.saturating_sub(available)
                })
            })
            .sum::<u64>();

        if used_vram_bytes == 0 {
            used_vram_bytes = fallback_used_vram;
        }

        let gpus = report
            .gpus
            .iter()
            .map(|gpu| ProtoGpuInfo {
                index: gpu.index,
                device_name: gpu.device_name.clone(),
                vram_total_bytes: gpu.vram_total_bytes,
                cuda_compute_capability: gpu.cuda_compute_capability.clone().unwrap_or_default(),
                vram_available_bytes: gpu.vram_available_bytes(),
                uuid: gpu.uuid.clone(),
            })
            .collect();

        Some(HardwareState {
            gpus,
            system_cuda_version: report.system_cuda_version.clone().unwrap_or_default(),
            system_driver_version: report.system_driver_version.clone().unwrap_or_default(),
            total_vram_bytes,
            used_vram_bytes,
        })
    }

    async fn ensure_client(&mut self) -> Result<GrpcClient, StatusReporterError> {
        loop {
            if let Some(client) = self.client.as_ref() {
                return Ok(client.clone());
            }

            match CoordinatorServiceClient::connect(self.coordinator_endpoint.clone()).await {
                Ok(client) => {
                    self.backoff.reset();
                    self.client = Some(client);
                    continue;
                }
                Err(err) => {
                    let delay = self.backoff.next_delay();
                    warn!(
                        "Failed to connect to coordinator {}: {} (retrying in {:?})",
                        self.coordinator_endpoint, err, delay
                    );
                    sleep(delay).await;
                }
            }
        }
    }

    /// Dispatch a single status report immediately. Intended for integration tests or
    /// configuration utilities that need to trigger a one-shot heartbeat without spawning
    /// the background loop.
    pub async fn send_report_once(&mut self) -> Result<(), StatusReporterError> {
        self.send_report().await
    }
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sanitize_endpoint(endpoint: String) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{}", trimmed)
    }
}

#[derive(Debug, Clone)]
struct Backoff {
    initial: Duration,
    current: Duration,
    max: Duration,
}

impl Backoff {
    fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            current: initial,
            max,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(INITIAL_BACKOFF_SECS),
            Duration::from_secs(MAX_BACKOFF_SECS),
        )
    }
}

fn is_retryable_status(status: &tonic::Status) -> bool {
    use tonic::Code;

    matches!(
        status.code(),
        Code::Unavailable
            | Code::Unknown
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::ResourceExhausted
            | Code::Aborted
            | Code::Cancelled
    )
}

#[derive(thiserror::Error, Debug)]
pub enum StatusReporterError {
    #[error("failed to detect hardware: {0}")]
    Hardware(#[from] GpuDetectionError),
    #[error("failed to list capsules: {0}")]
    CapsuleList(#[from] anyhow::Error),
    #[error("failed to reach coordinator: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("coordinator rejected status report: {0}")]
    Grpc(#[from] tonic::Status),
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::hardware::{GpuInfo, RigHardwareReport};
    use crate::security::audit::AuditLogger;
    use std::path::PathBuf;
    use std::time::Duration as StdDuration;

    fn create_test_manager() -> CapsuleManager {
        let temp_dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let log_path = temp_dir.path().join("audit.log");
        let key_path = temp_dir.path().join("node_key.pem");

        let logger =
            Arc::new(AuditLogger::new(log_path, key_path, "test-node".to_string()).unwrap());
        // let scrubber = Arc::new(GpuScrubber::new(logger.clone()));

        let gpu_detector = crate::hardware::create_gpu_detector();
        
        // UARC V1: Use Native runtime for tests (no OCI required)
        let runtime_config = crate::runtime::RuntimeConfig {
            kind: crate::runtime::RuntimeKind::Native,
            binary_path: PathBuf::from("/bin/sh"), // Native runtime doesn't use binary
            bundle_root: temp_dir.path().join("bundles"),
            state_root: temp_dir.path().join("state"),
            log_dir: temp_dir.path().join("logs"),
            hook_retry_attempts: 1,
        };

        // Create a permissive verifier for testing
        let verifier = Arc::new(crate::security::verifier::ManifestVerifier::new(None, false));

        CapsuleManager::new(
            logger,
            vec![], // allowed_host_paths
            gpu_detector,
            None, // service_registry
            None, // mdns_announcer
            None, // traefik_manager
            None, // artifact_manager
            None, // process_supervisor
            None, // egress_proxy_port
            verifier,
            Some(runtime_config),
            None, // metrics_collector
            None, // storage_config
        )
    }

    fn sample_capsule(status: CapsuleStatus) -> Capsule {
        Capsule {
            id: "capsule-1".to_string(),
            adep_json:
                br#"{"name":"demo","scheduling":{},"compute":{"image":"alpine:latest","args":[]}}"#
                    .to_vec(),
            oci_image: "alpine:latest".to_string(),
            digest: "sha256:demo".to_string(),
            status,
            storage_path: None,
            bundle_path: None,
            pid: Some(1001),
            reserved_vram_bytes: 0,
            observed_vram_bytes: None,
            last_failure: None,
            last_exit_code: None,
            log_path: None,
            started_at: None,
            remote_url: None,
        }
    }

    fn sample_report() -> RigHardwareReport {
        let mut report = RigHardwareReport::new("rig-1".to_string());
        report.gpus.push(GpuInfo {
            index: 0,
            device_name: "Mock GPU".to_string(),
            vram_total_bytes: 8 * 1_073_741_824,
            cuda_compute_capability: Some("8.0".to_string()),
            vram_used_bytes: Some(0),
            uuid: "GPU-MOCK-0".to_string(),
        });
        report
    }

    #[test]
    fn test_sanitize_endpoint() {
        assert_eq!(
            sanitize_endpoint("http://localhost:50052".to_string()),
            "http://localhost:50052"
        );
        assert_eq!(
            sanitize_endpoint("https://example".to_string()),
            "https://example"
        );
        assert_eq!(
            sanitize_endpoint("localhost:50052".to_string()),
            "http://localhost:50052"
        );
    }

    #[test]
    fn test_collect_workloads_filters_stopped() {
        let reporter = StatusReporter::new(
            "http://localhost:50052",
            Duration::from_secs(30),
            Arc::new(create_test_manager()),
            crate::hardware::create_gpu_detector(),
            crate::hardware::create_gpu_process_monitor(),
            Vec::new(),
        );

        let workloads = reporter.collect_workloads(&[
            sample_capsule(CapsuleStatus::Running),
            sample_capsule(CapsuleStatus::Stopped),
        ]);

        assert_eq!(workloads.len(), 1);
        assert_eq!(workloads[0].name, "demo");
        assert_eq!(workloads[0].phase, WorkloadPhase::Running as i32);
    }

    #[test]
    fn test_build_hardware_state_uses_fallback_vram() {
        let reporter = StatusReporter::new(
            "http://localhost:50052",
            Duration::from_secs(30),
            Arc::new(create_test_manager()),
            crate::hardware::create_gpu_detector(),
            crate::hardware::create_gpu_process_monitor(),
            Vec::new(),
        );
        let report = sample_report();

        let hardware = reporter
            .build_hardware_state(&report, 42)
            .expect("hardware should exist");

        assert_eq!(hardware.total_vram_bytes, 8 * 1_073_741_824);
        assert_eq!(hardware.used_vram_bytes, 42);
        assert_eq!(hardware.gpus.len(), 1);
    }

    #[test]
    fn test_backoff_progression_and_reset() {
        let mut backoff = Backoff::new(StdDuration::from_millis(10), StdDuration::from_millis(40));
        assert_eq!(backoff.next_delay(), StdDuration::from_millis(10));
        assert_eq!(backoff.next_delay(), StdDuration::from_millis(20));
        assert_eq!(backoff.next_delay(), StdDuration::from_millis(40));
        assert_eq!(backoff.next_delay(), StdDuration::from_millis(40));
        backoff.reset();
        assert_eq!(backoff.next_delay(), StdDuration::from_millis(10));
    }

    #[test]
    fn test_retryable_status_detection() {
        let unavailable = tonic::Status::new(tonic::Code::Unavailable, "unavailable");
        assert!(is_retryable_status(&unavailable));

        let invalid = tonic::Status::new(tonic::Code::InvalidArgument, "bad request");
        assert!(!is_retryable_status(&invalid));
    }
}
