//! Adapter (F1): wrap a [`snapshot::RestoredSession`] as a capsule
//! [`RuntimeHandle`] so a restored microVM reuses the existing session
//! metrics/kill/teardown machinery. Emits [`RuntimeMetadata::MicroVm`].
//!
//! This bridge lives in cli because it joins `snapshot` (RestoredSession) and
//! `capsule` (RuntimeHandle/RuntimeMetadata); neither crate should depend on the
//! other for it.

use async_trait::async_trait;
use capsule::{
    Measurable, MetricsSession, ResourceStats, Result, RuntimeHandle, RuntimeMetadata,
    UnifiedMetrics,
};
use snapshot::RestoredSession;

/// A restored Ready-State session exposed as a `RuntimeHandle`.
pub(crate) struct RestoredRuntimeHandle {
    session: RestoredSession,
    metrics: MetricsSession,
}

impl RestoredRuntimeHandle {
    pub(crate) fn new(session: RestoredSession) -> Self {
        let metrics = MetricsSession::new(session.session_id.clone());
        Self { session, metrics }
    }

    fn metadata(&self, exit_code: Option<i32>) -> RuntimeMetadata {
        RuntimeMetadata::MicroVm {
            vm_id: self.session.session_id.clone(),
            snapshot_backend: self.session.backend_id.clone(),
            exit_code,
        }
    }
}

#[async_trait]
impl Measurable for RestoredRuntimeHandle {
    async fn capture_metrics(&self) -> Result<UnifiedMetrics> {
        Ok(self
            .metrics
            .snapshot(ResourceStats::default(), self.metadata(None)))
    }

    async fn wait_and_finalize(&self) -> Result<UnifiedMetrics> {
        Ok(self
            .metrics
            .finalize(ResourceStats::default(), self.metadata(Some(0))))
    }
}

impl RuntimeHandle for RestoredRuntimeHandle {
    fn id(&self) -> &str {
        &self.session.session_id
    }

    fn kill(&mut self) -> Result<()> {
        // The disposable overlay is destroyed by SnapshotBackend::stop at
        // teardown; killing the handle is a no-op for the Fake backend.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> RestoredSession {
        RestoredSession {
            session_id: "fake-sess-1".to_string(),
            backend_id: "fake".to_string(),
            guest_port: Some(8080),
            overlay_root: std::path::PathBuf::from("/tmp/ov"),
            restored_bytes: 123,
            vmm_pid: None,
        }
    }

    #[tokio::test]
    async fn reports_microvm_metadata() {
        let h = RestoredRuntimeHandle::new(session());
        assert_eq!(h.id(), "fake-sess-1");
        let m = h.capture_metrics().await.unwrap();
        match m.metadata {
            RuntimeMetadata::MicroVm { vm_id, snapshot_backend, exit_code } => {
                assert_eq!(vm_id, "fake-sess-1");
                assert_eq!(snapshot_backend, "fake");
                assert_eq!(exit_code, None);
            }
            other => panic!("expected MicroVm, got {other:?}"),
        }
    }

    #[test]
    fn microvm_metadata_serializes_with_runtime_type_tag() {
        let h = RestoredRuntimeHandle::new(session());
        let json = serde_json::to_string(&h.metadata(Some(0))).unwrap();
        assert!(json.contains("\"runtime_type\":\"MicroVm\""), "{json}");
    }
}
