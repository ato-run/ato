use crate::error::Result;
use crate::metrics::{MetricsSession, ResourceStats, RuntimeMetadata, UnifiedMetrics};
use crate::runtime::{Measurable, RuntimeHandle};
use async_trait::async_trait;

/// Wasm 実行のメトリクスハンドル（暫定スタブ）。
pub struct WasmHandle {
    session: MetricsSession,
    module_hash: String,
    engine: String,
}

impl WasmHandle {
    pub fn new(
        session_id: impl Into<String>,
        module_hash: impl Into<String>,
        engine: impl Into<String>,
    ) -> Self {
        Self {
            session: MetricsSession::new(session_id),
            module_hash: module_hash.into(),
            engine: engine.into(),
        }
    }

    fn metadata(&self) -> RuntimeMetadata {
        RuntimeMetadata::Wasm {
            module_hash: self.module_hash.clone(),
            engine: self.engine.clone(),
        }
    }
}

impl RuntimeHandle for WasmHandle {
    fn id(&self) -> &str {
        &self.module_hash
    }

    fn kill(&mut self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Measurable for WasmHandle {
    async fn capture_metrics(&self) -> Result<UnifiedMetrics> {
        let resources = ResourceStats {
            duration_ms: self.session.elapsed_ms(),
            ..ResourceStats::default()
        };
        Ok(self.session.snapshot(resources, self.metadata()))
    }

    async fn wait_and_finalize(&self) -> Result<UnifiedMetrics> {
        let resources = ResourceStats {
            duration_ms: self.session.elapsed_ms(),
            ..ResourceStats::default()
        };
        Ok(self.session.finalize(resources, self.metadata()))
    }
}
