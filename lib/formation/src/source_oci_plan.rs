//! Registered source-to-OCI adapter recipe. Physical archive paths and
//! exploration grants are bindings, never part of this reusable D.
use crate::generation::is_sha256;
use serde::{Deserialize, Serialize};

pub const OCI_PROTOCOL: &str = "ato.oci@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenBaseImage {
    pub reference: String,
    pub pinned_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciLimits {
    pub memory_bytes: u64,
    pub cpu_limit_millis: u64,
    pub pids_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOciRecipe {
    pub schema: String,
    pub dockerfile: String,
    pub platform: String,
    pub base_images: Vec<FrozenBaseImage>,
    pub build: OciLimits,
    pub build_disk_bytes: u64,
    pub runtime: OciLimits,
    pub build_timeout_seconds: u64,
    pub max_archive_bytes: u64,
}

impl SourceOciRecipe {
    pub fn validate(&self) -> Result<(), &'static str> {
        let image = |i: &FrozenBaseImage| {
            !i.reference.is_empty()
                && i.reference.len() <= 512
                && !i.reference.contains([' ', '\n', '\r', '$', '=', '\\'])
                && is_sha256(&i.pinned_digest)
                && i.reference
                    .rsplit_once('@')
                    .is_none_or(|(_, d)| d == i.pinned_digest)
        };
        let limits = |l: &OciLimits| {
            l.memory_bytes >= 256 * 1024 * 1024
                && (1..=64_000).contains(&l.cpu_limit_millis)
                && (64..=4096).contains(&l.pids_limit)
        };
        if self.schema != "ato.source-oci-recipe/1"
            || self.dockerfile != "Dockerfile"
            || !matches!(self.platform.as_str(), "linux/amd64" | "linux/arm64")
            || self.base_images.len() > 8
            || !self.base_images.iter().all(image)
            || !self
                .base_images
                .windows(2)
                .all(|p| p[0].reference < p[1].reference)
            || !limits(&self.build)
            || !limits(&self.runtime)
            || !(512 * 1024 * 1024..=16 * 1024 * 1024 * 1024).contains(&self.build_disk_bytes)
            || !(1..=3600).contains(&self.build_timeout_seconds)
            || !(1..=512 * 1024 * 1024).contains(&self.max_archive_bytes)
        {
            return Err("source_oci_recipe_invalid");
        }
        Ok(())
    }
}
