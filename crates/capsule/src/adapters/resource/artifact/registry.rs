use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub runtimes: HashMap<String, RuntimeDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDefinition {
    pub versions: HashMap<String, HashMap<String, ArtifactVersion>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub url: String,
    pub sha256: String,
    pub binary_path: String,
}
