use async_trait::async_trait;

use crate::error::Result;
use crate::metrics::UnifiedMetrics;

#[async_trait]
pub trait UsageReporter: Send + Sync {
    async fn report_sample(&self, metrics: &UnifiedMetrics) -> Result<()>;
    async fn report_final(&self, metrics: &UnifiedMetrics) -> Result<()>;
}

#[async_trait]
pub trait CapsuleReporter: Send + Sync {
    async fn notify(&self, message: String) -> Result<()>;
    async fn warn(&self, message: String) -> Result<()>;
    async fn progress_start(&self, label: String, total: Option<u64>) -> Result<()>;
    async fn progress_inc(&self, amount: u64) -> Result<()>;
    async fn progress_finish(&self, message: Option<String>) -> Result<()>;
}

#[derive(Debug, Default, Clone)]
pub struct NoOpReporter;

#[async_trait]
impl UsageReporter for NoOpReporter {
    async fn report_sample(&self, _metrics: &UnifiedMetrics) -> Result<()> {
        Ok(())
    }

    async fn report_final(&self, _metrics: &UnifiedMetrics) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl CapsuleReporter for NoOpReporter {
    async fn notify(&self, _message: String) -> Result<()> {
        Ok(())
    }

    async fn warn(&self, _message: String) -> Result<()> {
        Ok(())
    }

    async fn progress_start(&self, _label: String, _total: Option<u64>) -> Result<()> {
        Ok(())
    }

    async fn progress_inc(&self, _amount: u64) -> Result<()> {
        Ok(())
    }

    async fn progress_finish(&self, _message: Option<String>) -> Result<()> {
        Ok(())
    }
}
