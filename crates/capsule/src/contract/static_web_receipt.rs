//! Immutable receipt contract for `ato.static-web-bundle-receipt/v1`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::static_web_manifest::{
    StaticWebManifestV1, canonical_jcs_bytes, validate_relative_path,
};

pub const STATIC_WEB_BUNDLE_RECEIPT_V1_SCHEMA: &str = "ato.static-web-bundle-receipt/v1";
pub const STATIC_WEB_BLOB_V1_SCHEMA: &str = "ato.static-blob/v1";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StaticWebReceiptError {
    #[error("static web receipt schema must be {STATIC_WEB_BUNDLE_RECEIPT_V1_SCHEMA}")]
    InvalidSchema,
    #[error("static web receipt has an invalid field: {0}")]
    InvalidField(&'static str),
    #[error("static web receipt has an invalid blob record")]
    InvalidBlob,
    #[error("static web receipt blobs must be strictly digest-sorted and unique")]
    BlobOrder,
    #[error("static web receipt does not match its manifest: {0}")]
    ManifestMismatch(&'static str),
    #[error("failed to canonicalize static web receipt: {0}")]
    Canonicalization(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBundleReceiptV1 {
    pub schema: String,
    pub materialization_id: String,
    pub manifest_digest: String,
    pub manifest_r2_key: String,
    pub production_host_label: String,
    pub staging_host_label: String,
    pub entry_path: String,
    pub file_count: u64,
    pub total_size: u64,
    pub blobs: Vec<StaticWebBlobReceiptV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBlobReceiptV1 {
    pub digest: String,
    pub size: u64,
    pub r2_key: String,
    pub custom_metadata: StaticWebBlobMetadataV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBlobMetadataV1 {
    pub schema: String,
    pub sha256: String,
}

impl StaticWebBundleReceiptV1 {
    pub fn validate(&self) -> Result<(), StaticWebReceiptError> {
        if self.schema != STATIC_WEB_BUNDLE_RECEIPT_V1_SCHEMA {
            return Err(StaticWebReceiptError::InvalidSchema);
        }
        validate_id(&self.materialization_id)?;
        validate_digest(&self.manifest_digest)?;
        validate_relative_path(&self.entry_path)
            .map_err(|_| StaticWebReceiptError::InvalidField("entry_path"))?;
        if self.file_count == 0 || self.file_count > 10_000 || self.total_size > 1024 * 1024 * 1024
        {
            return Err(StaticWebReceiptError::InvalidField("closure limits"));
        }
        if self.manifest_r2_key != manifest_r2_key(&self.manifest_digest)?
            || self.production_host_label != host_label('p', &self.manifest_digest)?
            || self.staging_host_label != host_label('s', &self.manifest_digest)?
        {
            return Err(StaticWebReceiptError::InvalidField("derived identity"));
        }
        if self.blobs.is_empty() || self.blobs.len() > self.file_count as usize {
            return Err(StaticWebReceiptError::InvalidField("blobs"));
        }
        let mut previous = None;
        let mut unique_total = 0_u64;
        for blob in &self.blobs {
            validate_digest(&blob.digest)?;
            if previous.is_some_and(|previous: &str| previous >= blob.digest.as_str()) {
                return Err(StaticWebReceiptError::BlobOrder);
            }
            previous = Some(blob.digest.as_str());
            if blob.size > 64 * 1024 * 1024
                || blob.r2_key != blob_r2_key(&blob.digest)?
                || blob.custom_metadata.schema != STATIC_WEB_BLOB_V1_SCHEMA
                || blob.custom_metadata.sha256 != blob.digest
            {
                return Err(StaticWebReceiptError::InvalidBlob);
            }
            unique_total = unique_total
                .checked_add(blob.size)
                .ok_or(StaticWebReceiptError::InvalidField("total_size"))?;
        }
        if unique_total > self.total_size {
            return Err(StaticWebReceiptError::InvalidField("total_size"));
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, StaticWebReceiptError> {
        self.validate()?;
        canonical_jcs_bytes(self)
            .map_err(|error| StaticWebReceiptError::Canonicalization(error.to_string()))
    }

    pub fn digest(&self) -> Result<String, StaticWebReceiptError> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(self.canonical_bytes()?)
        ))
    }

    pub fn validate_for_manifest(
        &self,
        manifest: &StaticWebManifestV1,
    ) -> Result<(), StaticWebReceiptError> {
        self.validate()?;
        manifest
            .validate()
            .map_err(|_| StaticWebReceiptError::ManifestMismatch("invalid manifest"))?;
        let manifest_digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                manifest
                    .canonical_bytes()
                    .map_err(|_| StaticWebReceiptError::ManifestMismatch("canonical manifest"))?,
            )
        );
        if self.materialization_id != manifest.materialization_id {
            return Err(StaticWebReceiptError::ManifestMismatch(
                "materialization_id",
            ));
        }
        if self.manifest_digest != manifest_digest {
            return Err(StaticWebReceiptError::ManifestMismatch("manifest_digest"));
        }
        if self.entry_path != manifest.entry_path
            || self.file_count != manifest.files.len() as u64
            || self.total_size != manifest.files.values().map(|file| file.size).sum::<u64>()
        {
            return Err(StaticWebReceiptError::ManifestMismatch("summary"));
        }
        let mut expected = BTreeMap::new();
        for file in manifest.files.values() {
            expected.entry(file.blob.clone()).or_insert(file.size);
        }
        if self.blobs.len() != expected.len() {
            return Err(StaticWebReceiptError::ManifestMismatch("blob count"));
        }
        for blob in &self.blobs {
            if expected.get(&blob.digest) != Some(&blob.size) {
                return Err(StaticWebReceiptError::ManifestMismatch("blob inventory"));
            }
        }
        Ok(())
    }
}

pub fn manifest_r2_key(digest: &str) -> Result<String, StaticWebReceiptError> {
    Ok(format!(
        "static/v1/manifests/sha256/{}.json",
        digest_hex(digest)?
    ))
}

pub fn blob_r2_key(digest: &str) -> Result<String, StaticWebReceiptError> {
    Ok(format!("static/v1/blobs/sha256/{}", digest_hex(digest)?))
}

pub fn host_label(environment: char, digest: &str) -> Result<String, StaticWebReceiptError> {
    if !matches!(environment, 'p' | 's') {
        return Err(StaticWebReceiptError::InvalidField(
            "host label environment",
        ));
    }
    let bytes = hex::decode(digest_hex(digest)?)
        .map_err(|_| StaticWebReceiptError::InvalidField("digest"))?;
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buffer = 0_u16;
    let mut bits = 0_u8;
    let mut output = String::with_capacity(54);
    output.push(environment);
    output.push('-');
    for byte in bytes {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            output.push(alphabet[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        output.push(alphabet[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    Ok(output)
}

fn validate_id(value: &str) -> Result<(), StaticWebReceiptError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(StaticWebReceiptError::InvalidField("materialization_id"));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), StaticWebReceiptError> {
    let _ = digest_hex(value)?;
    Ok(())
}

fn digest_hex(value: &str) -> Result<&str, StaticWebReceiptError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(StaticWebReceiptError::InvalidField("digest"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StaticWebReceiptError::InvalidField("digest"));
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_golden_is_exact_jcs_and_has_outer_digest() {
        let receipt: StaticWebBundleReceiptV1 = serde_json::from_str(include_str!(
            "../../tests/fixtures/static-web-bundle-receipt-jcs-v1/input.json"
        ))
        .unwrap();
        assert_eq!(
            receipt.canonical_bytes().unwrap(),
            include_str!("../../tests/fixtures/static-web-bundle-receipt-jcs-v1/canonical.json")
                .trim_end_matches('\n')
                .as_bytes()
        );
        assert_eq!(
            receipt.digest().unwrap(),
            "sha256:a4b55b08a80fd3bf503be9948cd091d2a9c555b68e244147e5b78c9e6b5063d1"
        );
    }

    #[test]
    fn receipt_rejects_unknown_fields_and_duplicate_blob_digests() {
        let input =
            include_str!("../../tests/fixtures/static-web-bundle-receipt-jcs-v1/input.json");
        let mut value: serde_json::Value = serde_json::from_str(input).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<StaticWebBundleReceiptV1>(value).is_err());

        let mut receipt: StaticWebBundleReceiptV1 = serde_json::from_str(input).unwrap();
        receipt.blobs[1].digest = receipt.blobs[0].digest.clone();
        assert!(matches!(
            receipt.validate(),
            Err(StaticWebReceiptError::BlobOrder)
        ));
    }
}
