use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub resource: HashMap<String, HashMap<String, Resource>>,
    #[serde(default)]
    pub variable: HashMap<String, Variable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Resource {
    Container(ContainerConfig),
    Compute(ComputeConfig),
    Volume(VolumeConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    #[serde(rename = "type")]
    pub var_type: String,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeConfig {
    pub vram_min: Option<String>,
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeConfig {
    pub runtime: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub image: String,
    pub env: Option<HashMap<String, String>>,
    pub network: Option<NetworkConfig>,
    pub volumes: Option<Vec<String>>,
    pub native: Option<NativeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub port: u16,
    pub cors: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeConfig {
    pub source: String,
    pub target: String,
}
