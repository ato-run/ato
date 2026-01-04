use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct CloudDeployRequest {
    pub capsule_id: String,
    pub manifest: String,
    // Additional requirements can be added here if we parse them out
    // For now, the manifest string contains everything needed by SkyPilot
}

#[derive(Debug, Deserialize)]
pub struct CloudDeployResponse {
    pub job_id: String,
    pub status: String,
    pub endpoint: Option<String>,
}
