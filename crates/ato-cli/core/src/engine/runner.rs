use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{interval, timeout};

use crate::error::{CapsuleError, Result};
use crate::metrics::UnifiedMetrics;
use crate::reporter::UsageReporter;
use crate::runtime::RuntimeHandle;

#[derive(Debug, Clone)]
pub struct SessionRunnerConfig {
    pub sample_interval: Duration,
    pub timeout: Option<Duration>,
    pub finalize_timeout: Duration,
}

impl Default for SessionRunnerConfig {
    fn default() -> Self {
        Self {
            sample_interval: Duration::from_secs(5),
            timeout: None,
            finalize_timeout: Duration::from_secs(5),
        }
    }
}

pub struct SessionRunner<H, R> {
    handle: H,
    reporter: R,
    config: SessionRunnerConfig,
    last_sample: Option<UnifiedMetrics>,
}

impl<H, R> SessionRunner<H, R>
where
    H: RuntimeHandle,
    R: UsageReporter,
{
    pub fn new(handle: H, reporter: R) -> Self {
        Self {
            handle,
            reporter,
            config: SessionRunnerConfig::default(),
            last_sample: None,
        }
    }

    pub fn with_config(mut self, config: SessionRunnerConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn run(mut self) -> Result<UnifiedMetrics> {
        let timeout_opt = self.config.timeout;
        let run_future = self.run_loop();
        if let Some(deadline) = timeout_opt {
            match timeout(deadline, run_future).await {
                Ok(result) => result,
                Err(_) => self.handle_timeout().await,
            }
        } else {
            run_future.await
        }
    }

    async fn run_loop(&mut self) -> Result<UnifiedMetrics> {
        let mut ticker = interval(self.config.sample_interval);
        let wait_fut = self.handle.wait_and_finalize();
        tokio::pin!(wait_fut);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let metrics = self.capture_metrics().await?;
                    self.reporter.report_sample(&metrics).await?;
                    self.last_sample = Some(metrics);
                }
                result = &mut wait_fut => {
                    let metrics = result?;
                    self.reporter.report_final(&metrics).await?;
                    return Ok(metrics);
                }
            }
        }
    }

    async fn handle_timeout(&mut self) -> Result<UnifiedMetrics> {
        let _ = self.handle.kill();
        match timeout(
            self.config.finalize_timeout,
            self.handle.wait_and_finalize(),
        )
        .await
        {
            Ok(result) => {
                let metrics = result?;
                self.reporter.report_final(&metrics).await?;
                Ok(metrics)
            }
            Err(_) => {
                if let Some(mut fallback) = self.last_sample.clone() {
                    fallback.ended_at = Some(now_unix_secs());
                    self.reporter.report_final(&fallback).await?;
                    return Ok(fallback);
                }
                Err(CapsuleError::Timeout)
            }
        }
    }

    async fn capture_metrics(&self) -> Result<UnifiedMetrics> {
        self.handle.capture_metrics().await
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
