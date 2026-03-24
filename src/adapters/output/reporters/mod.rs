use async_trait::async_trait;
use std::io::{IsTerminal, Write};

use capsule_core::{CapsuleReporter, UnifiedMetrics, UsageReporter};

use crate::application::ports::output::OutputPort;

#[derive(Debug, Clone)]
pub enum CliReporter {
    Stdout(StdoutReporter),
    Json(JsonReporter),
}

impl CliReporter {
    pub fn new(json: bool) -> Self {
        if json {
            Self::Json(JsonReporter)
        } else {
            Self::Stdout(StdoutReporter)
        }
    }
}

#[async_trait]
impl OutputPort for CliReporter {
    fn is_json(&self) -> bool {
        matches!(self, Self::Json(_))
    }
}

#[async_trait]
impl UsageReporter for CliReporter {
    async fn report_sample(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.report_sample(metrics).await,
            Self::Json(reporter) => reporter.report_sample(metrics).await,
        }
    }

    async fn report_final(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.report_final(metrics).await,
            Self::Json(reporter) => reporter.report_final(metrics).await,
        }
    }
}

#[async_trait]
impl CapsuleReporter for CliReporter {
    async fn notify(&self, message: String) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.notify(message).await,
            Self::Json(reporter) => reporter.notify(message).await,
        }
    }

    async fn warn(&self, message: String) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.warn(message).await,
            Self::Json(reporter) => reporter.warn(message).await,
        }
    }

    async fn progress_start(&self, label: String, total: Option<u64>) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.progress_start(label, total).await,
            Self::Json(reporter) => reporter.progress_start(label, total).await,
        }
    }

    async fn progress_inc(&self, amount: u64) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.progress_inc(amount).await,
            Self::Json(reporter) => reporter.progress_inc(amount).await,
        }
    }

    async fn progress_finish(&self, message: Option<String>) -> anyhow::Result<()> {
        match self {
            Self::Stdout(reporter) => reporter.progress_finish(message).await,
            Self::Json(reporter) => reporter.progress_finish(message).await,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct StdoutReporter;

#[async_trait]
impl UsageReporter for StdoutReporter {
    async fn report_sample(&self, _metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        Ok(())
    }

    async fn report_final(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        println!(
            "📈 Metrics: session={}, duration_ms={}, peak_memory_bytes={}",
            metrics.session_id, metrics.resources.duration_ms, metrics.resources.peak_memory_bytes
        );
        Ok(())
    }
}

#[async_trait]
impl CapsuleReporter for StdoutReporter {
    async fn notify(&self, message: String) -> anyhow::Result<()> {
        println!("{}", message);
        Ok(())
    }

    async fn warn(&self, message: String) -> anyhow::Result<()> {
        eprintln!("{}", message);
        Ok(())
    }

    async fn progress_start(&self, label: String, total: Option<u64>) -> anyhow::Result<()> {
        if std::io::stdout().is_terminal() {
            if let Some(total) = total {
                print!("\r{} ({} bytes)", label, total);
            } else {
                print!("\r{}", label);
            }
            std::io::stdout().flush()?;
        } else if let Some(total) = total {
            println!("{} ({} bytes)", label, total);
        } else {
            println!("{}", label);
        }
        Ok(())
    }

    async fn progress_inc(&self, _amount: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn progress_finish(&self, message: Option<String>) -> anyhow::Result<()> {
        if std::io::stdout().is_terminal() {
            print!("\r\x1B[2K");
            if let Some(message) = message {
                println!("{}", message);
            } else {
                std::io::stdout().flush()?;
            }
        } else if let Some(message) = message {
            println!("{}", message);
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct JsonReporter;

#[async_trait]
impl UsageReporter for JsonReporter {
    async fn report_sample(&self, _metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        Ok(())
    }

    async fn report_final(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "metrics",
            "session_id": metrics.session_id,
            "duration_ms": metrics.resources.duration_ms,
            "peak_memory_bytes": metrics.resources.peak_memory_bytes,
            "started_at": metrics.started_at,
            "ended_at": metrics.ended_at,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }
}

#[async_trait]
impl CapsuleReporter for JsonReporter {
    async fn notify(&self, message: String) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "notify",
            "message": message,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }

    async fn warn(&self, message: String) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "warn",
            "message": message,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }

    async fn progress_start(&self, label: String, total: Option<u64>) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "progress_start",
            "label": label,
            "total": total,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }

    async fn progress_inc(&self, amount: u64) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "progress_inc",
            "amount": amount,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }

    async fn progress_finish(&self, message: Option<String>) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "type": "progress_finish",
            "message": message,
        });
        println!("{}", serde_json::to_string(&payload)?);
        Ok(())
    }
}
