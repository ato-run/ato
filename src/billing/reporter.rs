use reqwest::Client;
use serde::Serialize;
use std::time::SystemTime;
use tracing::{error, info};

#[derive(Serialize)]
struct ReportUsageRequest {
    capsule_id: String,
    user_id: String,
    resource: String,
    amount: f64,
    start_time: String, // RFC3339
    end_time: String,   // RFC3339
}

#[derive(Clone)]
pub struct UsageReporter {
    client: Client,
    coordinator_url: String,
}

impl UsageReporter {
    pub fn new(coordinator_url: String) -> Self {
        Self {
            client: Client::new(),
            coordinator_url,
        }
    }

    pub async fn report(
        &self,
        capsule_id: String,
        user_id: String,
        start: SystemTime,
        end: SystemTime,
    ) {
        let duration = match end.duration_since(start) {
            Ok(d) => d.as_secs_f64() / 3600.0, // hours
            Err(_) => return,
        };

        if duration <= 0.0 {
            return;
        }

        let req = ReportUsageRequest {
            capsule_id: capsule_id.clone(),
            user_id: user_id.clone(),
            resource: "gpu_hour".to_string(),
            amount: duration,
            start_time: chrono::DateTime::<chrono::Utc>::from(start).to_rfc3339(),
            end_time: chrono::DateTime::<chrono::Utc>::from(end).to_rfc3339(),
        };

        let url = format!("{}/api/v1/usage/report", self.coordinator_url);

        match self.client.post(&url).json(&req).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    error!(
                        "Failed to report usage for capsule {}: status {}",
                        capsule_id,
                        resp.status()
                    );
                } else {
                    info!(
                        "Reported usage for capsule {}: {:.4} hours",
                        capsule_id, duration
                    );
                }
            }
            Err(e) => {
                error!("Failed to report usage for capsule {}: {}", capsule_id, e);
            }
        }
    }
}
