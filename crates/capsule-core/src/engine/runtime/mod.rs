use crate::error::Result;
use crate::metrics::UnifiedMetrics;
use async_trait::async_trait;

pub mod native;
pub mod oci;
pub mod wasm;

#[async_trait]
pub trait Measurable {
    async fn capture_metrics(&self) -> Result<UnifiedMetrics>;
    async fn wait_and_finalize(&self) -> Result<UnifiedMetrics>;
}

pub trait RuntimeHandle: Measurable + Send + Sync {
    fn id(&self) -> &str;
    fn kill(&mut self) -> Result<()>;
}
