use serde::{Deserialize, Serialize};

use crate::error::{CapsuleError, Result};

pub const SCHEMA_VERSION: u32 = 3;
pub const BLAKE3_PREFIX: &str = "blake3:";
pub const PAYLOAD_MANIFEST_PATH: &str = "payload.v3.manifest.json";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PayloadManifest {
    pub schema_version: u32,
    pub artifact_hash: String,
    pub cdc_params: CdcParams,
    pub total_raw_size: u64,
    pub chunks: Vec<ChunkMeta>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CdcParams {
    pub algorithm: String,
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
    pub seed: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChunkMeta {
    pub raw_hash: String,
    pub raw_size: u32,
    pub zstd_size_hint: Option<u32>,
}

impl PayloadManifest {
    pub fn new(chunks: Vec<ChunkMeta>) -> Self {
        let total_raw_size = chunks.iter().map(|chunk| chunk.raw_size as u64).sum();
        Self {
            schema_version: SCHEMA_VERSION,
            artifact_hash: String::new(),
            cdc_params: CdcParams::default_fastcdc(),
            total_raw_size,
            chunks,
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_core()?;
        validate_blake3_digest("artifact_hash", &self.artifact_hash)
    }

    pub(crate) fn validate_core(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(CapsuleError::Config(format!(
                "payload schema_version must be {}, got {}",
                SCHEMA_VERSION, self.schema_version
            )));
        }

        self.cdc_params.validate()?;
        for chunk in &self.chunks {
            chunk.validate()?;
        }

        let computed_total: u64 = self.chunks.iter().map(|chunk| chunk.raw_size as u64).sum();
        if computed_total != self.total_raw_size {
            return Err(CapsuleError::Config(format!(
                "payload total_raw_size mismatch: expected {}, got {}",
                computed_total, self.total_raw_size
            )));
        }

        Ok(())
    }
}

impl CdcParams {
    pub fn default_fastcdc() -> Self {
        Self {
            algorithm: "fastcdc".to_string(),
            min_size: 2 * 1024 * 1024,
            avg_size: 4 * 1024 * 1024,
            max_size: 8 * 1024 * 1024,
            seed: 0,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.algorithm != "fastcdc" {
            return Err(CapsuleError::Config(format!(
                "unsupported cdc algorithm: {}",
                self.algorithm
            )));
        }
        if self.min_size == 0 || self.avg_size == 0 || self.max_size == 0 {
            return Err(CapsuleError::Config(
                "cdc sizes must be non-zero".to_string(),
            ));
        }
        if !(self.min_size <= self.avg_size && self.avg_size <= self.max_size) {
            return Err(CapsuleError::Config(format!(
                "invalid cdc sizes: min={} avg={} max={}",
                self.min_size, self.avg_size, self.max_size
            )));
        }
        Ok(())
    }
}

impl ChunkMeta {
    pub fn validate(&self) -> Result<()> {
        validate_blake3_digest("chunk.raw_hash", &self.raw_hash)
    }
}

pub fn blake3_digest(bytes: &[u8]) -> String {
    format!("{}{}", BLAKE3_PREFIX, blake3::hash(bytes).to_hex())
}

pub fn parse_blake3_digest(value: &str) -> Result<&str> {
    validate_blake3_digest("digest", value)?;
    Ok(value.strip_prefix(BLAKE3_PREFIX).unwrap_or_default())
}

pub fn validate_blake3_digest(label: &str, value: &str) -> Result<()> {
    if !value.starts_with(BLAKE3_PREFIX) {
        return Err(CapsuleError::Config(format!(
            "{} must start with '{}': {}",
            label, BLAKE3_PREFIX, value
        )));
    }

    let hex = value.strip_prefix(BLAKE3_PREFIX).unwrap_or_default();
    if hex.len() != 64 {
        return Err(CapsuleError::Config(format!(
            "{} must contain 64 lowercase hex chars, got {}",
            label,
            hex.len()
        )));
    }

    if !hex
        .chars()
        .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
    {
        return Err(CapsuleError::Config(format!(
            "{} must be lowercase hex: {}",
            label, value
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{blake3_digest, ChunkMeta, PayloadManifest};

    #[test]
    fn validate_rejects_invalid_schema_version() {
        let mut manifest = PayloadManifest::new(Vec::new());
        manifest.schema_version = 2;
        manifest.artifact_hash = blake3_digest(b"manifest");
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_digest() {
        let mut manifest = PayloadManifest::new(Vec::new());
        manifest.artifact_hash = "blake3:XYZ".to_string();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn validate_rejects_size_mismatch() {
        let mut manifest = PayloadManifest::new(vec![ChunkMeta {
            raw_hash: blake3_digest(b"abc"),
            raw_size: 3,
            zstd_size_hint: Some(10),
        }]);
        manifest.total_raw_size = 2;
        manifest.artifact_hash = blake3_digest(b"manifest");
        assert!(manifest.validate().is_err());
    }
}
