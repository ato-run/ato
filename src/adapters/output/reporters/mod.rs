use async_trait::async_trait;
use std::io::{IsTerminal, Write};

use capsule_core::{CapsuleReporter, UnifiedMetrics, UsageReporter};

use crate::application::ports::output::OutputPort;

#[derive(Debug, Clone)]
pub enum CliReporter {
    Text(TextReporter),
    Json(JsonReporter),
}

impl CliReporter {
    pub fn new(json: bool) -> Self {
        Self::new_with_stream(json, TextStream::Stdout)
    }

    pub fn new_run(json: bool) -> Self {
        Self::new_with_stream(json, TextStream::Stderr)
    }

    fn new_with_stream(json: bool, stream: TextStream) -> Self {
        if json {
            Self::Json(JsonReporter)
        } else {
            Self::Text(TextReporter { stream })
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
            Self::Text(reporter) => reporter.report_sample(metrics).await,
            Self::Json(reporter) => reporter.report_sample(metrics).await,
        }
    }

    async fn report_final(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.report_final(metrics).await,
            Self::Json(reporter) => reporter.report_final(metrics).await,
        }
    }
}

#[async_trait]
impl CapsuleReporter for CliReporter {
    async fn notify(&self, message: String) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.notify(message).await,
            Self::Json(reporter) => reporter.notify(message).await,
        }
    }

    async fn warn(&self, message: String) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.warn(message).await,
            Self::Json(reporter) => reporter.warn(message).await,
        }
    }

    async fn progress_start(&self, label: String, total: Option<u64>) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.progress_start(label, total).await,
            Self::Json(reporter) => reporter.progress_start(label, total).await,
        }
    }

    async fn progress_inc(&self, amount: u64) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.progress_inc(amount).await,
            Self::Json(reporter) => reporter.progress_inc(amount).await,
        }
    }

    async fn progress_finish(&self, message: Option<String>) -> anyhow::Result<()> {
        match self {
            Self::Text(reporter) => reporter.progress_finish(message).await,
            Self::Json(reporter) => reporter.progress_finish(message).await,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TextStream {
    Stdout,
    Stderr,
}

impl TextStream {
    fn print_line(self, message: &str) -> anyhow::Result<()> {
        match self {
            Self::Stdout => println!("{}", message),
            Self::Stderr => eprintln!("{}", message),
        }
        Ok(())
    }

    fn write_progress_start(self, label: &str, total: Option<u64>) -> anyhow::Result<()> {
        match self {
            Self::Stdout => {
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
            }
            Self::Stderr => {
                if std::io::stderr().is_terminal() {
                    if let Some(total) = total {
                        eprint!("\r{} ({} bytes)", label, total);
                    } else {
                        eprint!("\r{}", label);
                    }
                    std::io::stderr().flush()?;
                } else if let Some(total) = total {
                    eprintln!("{} ({} bytes)", label, total);
                } else {
                    eprintln!("{}", label);
                }
            }
        }
        Ok(())
    }

    fn write_progress_finish(self, message: Option<String>) -> anyhow::Result<()> {
        match self {
            Self::Stdout => {
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
            }
            Self::Stderr => {
                if std::io::stderr().is_terminal() {
                    eprint!("\r\x1B[2K");
                    if let Some(message) = message {
                        eprintln!("{}", message);
                    } else {
                        std::io::stderr().flush()?;
                    }
                } else if let Some(message) = message {
                    eprintln!("{}", message);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TextReporter {
    stream: TextStream,
}

#[async_trait]
impl UsageReporter for TextReporter {
    async fn report_sample(&self, _metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        Ok(())
    }

    async fn report_final(&self, metrics: &UnifiedMetrics) -> anyhow::Result<()> {
        self.stream.print_line(&format!(
            "📈 Metrics: session={}, duration_ms={}, peak_memory_bytes={}",
            metrics.session_id, metrics.resources.duration_ms, metrics.resources.peak_memory_bytes
        ))
    }
}

#[async_trait]
impl CapsuleReporter for TextReporter {
    async fn notify(&self, message: String) -> anyhow::Result<()> {
        self.stream.print_line(&message)
    }

    async fn warn(&self, message: String) -> anyhow::Result<()> {
        eprintln!("{}", message);
        Ok(())
    }

    async fn progress_start(&self, label: String, total: Option<u64>) -> anyhow::Result<()> {
        self.stream.write_progress_start(&label, total)
    }

    async fn progress_inc(&self, _amount: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn progress_finish(&self, message: Option<String>) -> anyhow::Result<()> {
        self.stream.write_progress_finish(message)
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
