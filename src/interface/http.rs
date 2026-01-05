use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use crate::metrics::MetricsCollector;

/// HTTP server for serving health checks and Prometheus metrics
pub struct HttpServer {
    addr: SocketAddr,
    metrics_collector: Option<Arc<MetricsCollector>>,
}

impl HttpServer {
    /// Create a new HTTP server
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            metrics_collector: None,
        }
    }

    /// Set the metrics collector
    pub fn with_metrics(mut self, collector: Arc<MetricsCollector>) -> Self {
        self.metrics_collector = Some(collector);
        self
    }

    /// Start the HTTP server
    pub async fn start(self) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        info!("HTTP server listening on {}", self.addr);

        let metrics_collector = self.metrics_collector;

        loop {
            let (mut socket, addr) = listener.accept().await?;
            debug!("Accepted connection from {}", addr);

            let collector = metrics_collector.clone();

            tokio::spawn(async move {
                let mut buffer = [0; 1024];
                match socket.read(&mut buffer).await {
                    Ok(n) if n > 0 => {
                        let request = String::from_utf8_lossy(&buffer[..n]);
                        debug!("Received request: {}", request.lines().next().unwrap_or(""));

                        let response = handle_request(&request, collector.as_deref());
                        if let Err(e) = socket.write_all(response.as_bytes()).await {
                            error!("Failed to write response: {}", e);
                        }
                    }
                    Ok(_) => {
                        debug!("Empty request");
                    }
                    Err(e) => {
                        error!("Failed to read from socket: {}", e);
                    }
                }
            });
        }
    }
}

fn handle_request(request: &str, metrics_collector: Option<&MetricsCollector>) -> String {
    let lines: Vec<&str> = request.lines().collect();
    if lines.is_empty() {
        return http_response(400, "text/plain", "Bad Request");
    }

    let first_line = lines[0];
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        return http_response(400, "text/plain", "Bad Request");
    }

    let method = parts[0];
    let path = parts[1];

    match (method, path) {
        ("GET", "/health") => handle_health(),
        ("GET", "/ready") => handle_readiness(),
        ("GET", "/live") => handle_liveness(),
        ("GET", "/metrics") => handle_metrics(metrics_collector),
        _ => http_response(404, "text/plain", "Not Found"),
    }
}

fn handle_health() -> String {
    let body = r#"{"status":"healthy","timestamp":"#.to_string()
        + &chrono::Utc::now().to_rfc3339()
        + r#""}"#;
    http_response(200, "application/json", &body)
}

fn handle_readiness() -> String {
    // Basic readiness check - if server is responding, it's ready
    let body = r#"{"status":"ready"}"#;
    http_response(200, "application/json", body)
}

fn handle_liveness() -> String {
    // Basic liveness check - if server is responding, it's alive
    http_response(200, "text/plain", "alive")
}

fn handle_metrics(metrics_collector: Option<&MetricsCollector>) -> String {
    match metrics_collector {
        Some(collector) => match collector.gather() {
            Ok(metrics) => http_response(200, "text/plain; version=0.0.4", &metrics),
            Err(e) => {
                error!("Failed to gather metrics: {}", e);
                http_response(500, "text/plain", "Internal Server Error")
            }
        },
        None => {
            // Return empty metrics if no collector is configured
            http_response(200, "text/plain; version=0.0.4", "# No metrics available\n")
        }
    }
}

fn http_response(status: u16, content_type: &str, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };

    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n{}",
        status,
        status_text,
        content_type,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_health() {
        let response = handle_health();
        assert!(response.contains("200 OK"));
        assert!(response.contains("application/json"));
        assert!(response.contains("healthy"));
    }

    #[test]
    fn test_handle_readiness() {
        let response = handle_readiness();
        assert!(response.contains("200 OK"));
        assert!(response.contains("ready"));
    }

    #[test]
    fn test_handle_liveness() {
        let response = handle_liveness();
        assert!(response.contains("200 OK"));
        assert!(response.contains("alive"));
    }

    #[test]
    fn test_handle_metrics_no_collector() {
        let response = handle_metrics(None);
        assert!(response.contains("200 OK"));
        assert!(response.contains("No metrics available"));
    }

    #[test]
    fn test_handle_request_health() {
        let request = "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let response = handle_request(request, None);
        assert!(response.contains("200 OK"));
        assert!(response.contains("healthy"));
    }

    #[test]
    fn test_handle_request_not_found() {
        let request = "GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let response = handle_request(request, None);
        assert!(response.contains("404 Not Found"));
    }

    #[test]
    fn test_handle_request_bad_method() {
        let request = "POST /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let response = handle_request(request, None);
        assert!(response.contains("404 Not Found"));
    }
}
