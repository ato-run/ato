//! Immutable build evidence transport, independent of Capsule and Run identity.
use crate::{authoring::BoundDerivation, generation::is_sha256, retained::content_ref};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const BUILD_RECORD_SCHEMA: &str = "ato.build-record/1";
pub const CHUNK_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_RECORD_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceChunk {
    pub content_ref: String,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> BuildRecordManifest {
        let reference = content_ref(b"lineage");
        let root = ".ato-dependencies/python-build-1";
        BuildRecordManifest {
            schema: BUILD_RECORD_SCHEMA.into(),
            creation_attempt_id: "attempt".into(),
            contract_ref: content_ref(b"K"),
            derivation_ref: content_ref(b"D"),
            source_closure_ref: content_ref(b"source"),
            roots: vec![EvidenceRoot {
                path: root.into(),
                requirements_ref: content_ref(b"requirements"),
                plan_ref: content_ref(b"plan"),
                provenance_ref: reference.clone(),
            }],
            files: vec![EvidenceFile {
                path: format!("{root}/provenance.json"),
                content_ref: reference.clone(),
                bytes: 7,
                chunks: vec![EvidenceChunk {
                    content_ref: reference,
                    bytes: 7,
                }],
                package: None,
                version: None,
            }],
        }
    }
    #[test]
    fn evidence_canonical_bytes_are_immutable_and_not_a_contract_identity() {
        let manifest = fixture();
        let raw = manifest.canonical_bytes().unwrap();
        let reference = manifest.record_ref().unwrap();
        assert_ne!(reference, manifest.contract_ref);
        assert_eq!(
            BuildRecordManifest::parse(&raw, &reference).unwrap(),
            manifest
        );
        let spaced = serde_json::to_vec_pretty(&manifest).unwrap();
        assert!(BuildRecordManifest::parse(&spaced, &content_ref(&spaced)).is_err());
        let mut unknown = serde_json::to_value(&manifest).unwrap();
        unknown["run_approval"] = true.into();
        let raw = serde_jcs::to_vec(&unknown).unwrap();
        assert_eq!(
            BuildRecordManifest::parse(&raw, &content_ref(&raw))
                .unwrap_err()
                .0,
            "build_record_schema"
        );
    }
    #[test]
    fn paths_counts_versions_and_order_fail_closed() {
        for path in [
            "../escape",
            "/app/secrets",
            ".ato-dependencies/python-build-1/../../escape",
            "unowned/file",
        ] {
            let mut manifest = fixture();
            manifest.files[0].path = path.into();
            assert!(manifest.validate().is_err());
        }
        let mut manifest = fixture();
        manifest.files[0].version = Some("1.0.0".into());
        assert!(manifest.validate().is_err());
        let mut manifest = fixture();
        manifest.files.push(manifest.files[0].clone());
        assert!(manifest.validate().is_err());
        let mut manifest = fixture();
        manifest.roots[0].path = ".venv".into();
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn chunks_are_bounded_ordered_and_deduplicated_without_conflicting_sizes() {
        let mut manifest = fixture();
        let mut alias = manifest.files[0].clone();
        alias.path = ".ato-dependencies/python-build-1/wheels/artifacts.json".into();
        manifest.files.push(alias);
        manifest.validate().unwrap();
        assert_eq!(manifest.stored_bytes().unwrap(), 7);
        manifest.files[1].bytes = 8;
        manifest.files[1].chunks[0].bytes = 8;
        assert_eq!(
            manifest.validate().unwrap_err().0,
            "build_record_chunk_conflict"
        );
        let mut manifest = fixture();
        manifest.files[0].bytes = CHUNK_BYTES + 7;
        manifest.files[0].chunks = vec![
            EvidenceChunk {
                content_ref: content_ref(b"large"),
                bytes: CHUNK_BYTES,
            },
            EvidenceChunk {
                content_ref: content_ref(b"tail"),
                bytes: 7,
            },
        ];
        manifest.validate().unwrap();
        manifest.files[0].chunks.reverse();
        assert!(manifest.validate().is_err());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFile {
    pub path: String,
    pub content_ref: String,
    pub bytes: u64,
    pub chunks: Vec<EvidenceChunk>,
    /// Wheel identities were sealed by the registered offline-install operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRoot {
    pub path: String,
    pub requirements_ref: String,
    pub plan_ref: String,
    pub provenance_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRecordManifest {
    pub schema: String,
    pub creation_attempt_id: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub source_closure_ref: String,
    pub roots: Vec<EvidenceRoot>,
    pub files: Vec<EvidenceFile>,
}

#[derive(Debug, thiserror::Error)]
#[error("build Record rejected: {0}")]
pub struct BuildRecordError(pub &'static str);

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

impl BuildRecordManifest {
    pub fn validate(&self) -> Result<(), BuildRecordError> {
        let invalid = || BuildRecordError("build_record_manifest_invalid");
        if self.schema != BUILD_RECORD_SCHEMA
            || self.creation_attempt_id.is_empty()
            || self.creation_attempt_id.len() > 256
            || ![
                &self.contract_ref,
                &self.derivation_ref,
                &self.source_closure_ref,
            ]
            .iter()
            .all(|r| is_sha256(r))
            || self.roots.is_empty()
            || self.roots.len() > 4
            || self.files.is_empty()
            || self.files.len() > 1024
            || self.roots.windows(2).any(|w| w[0].path >= w[1].path)
            || self.files.windows(2).any(|w| w[0].path >= w[1].path)
        {
            return Err(invalid());
        }
        for root in &self.roots {
            if !root
                .path
                .strip_prefix(".ato-dependencies/python-build-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                || ![&root.requirements_ref, &root.plan_ref, &root.provenance_ref]
                    .iter()
                    .all(|r| is_sha256(r))
                || !self.files.iter().any(|f| {
                    f.path == format!("{}/provenance.json", root.path)
                        && f.content_ref == root.provenance_ref
                })
            {
                return Err(invalid());
            }
        }
        for file in &self.files {
            if !crate::proposal::source_path(&file.path)
                || file.path.len() > 512
                || !self
                    .roots
                    .iter()
                    .any(|r| file.path.starts_with(&format!("{}/", r.path)))
                || !is_sha256(&file.content_ref)
                || file.bytes == 0
                || file.bytes > MAX_RECORD_BYTES
                || file.chunks.is_empty()
                || file.chunks.len() > 32
                || file
                    .chunks
                    .iter()
                    .any(|c| !is_sha256(&c.content_ref) || c.bytes == 0 || c.bytes > CHUNK_BYTES)
                || file.chunks.iter().map(|c| c.bytes).sum::<u64>() != file.bytes
                || file.chunks[..file.chunks.len() - 1]
                    .iter()
                    .any(|c| c.bytes != CHUNK_BYTES)
                || file.package.is_some() != file.version.is_some()
                || file.package.as_ref().is_some_and(|p| !identity(p))
                || file.version.as_ref().is_some_and(|p| !identity(p))
            {
                return Err(invalid());
            }
        }
        if self.chunks()?.len() > 1024 || self.stored_bytes()? > MAX_RECORD_BYTES {
            return Err(BuildRecordError("build_record_byte_limit"));
        }
        Ok(())
    }

    pub fn chunks(&self) -> Result<BTreeMap<String, u64>, BuildRecordError> {
        let mut chunks = BTreeMap::new();
        for chunk in self.files.iter().flat_map(|f| &f.chunks) {
            if chunks
                .insert(chunk.content_ref.clone(), chunk.bytes)
                .is_some_and(|previous| previous != chunk.bytes)
            {
                return Err(BuildRecordError("build_record_chunk_conflict"));
            }
        }
        Ok(chunks)
    }
    pub fn stored_bytes(&self) -> Result<u64, BuildRecordError> {
        Ok(self.chunks()?.values().copied().sum())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, BuildRecordError> {
        self.validate()?;
        let bytes =
            serde_jcs::to_vec(self).map_err(|_| BuildRecordError("build_record_canonical"))?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(BuildRecordError("build_record_manifest_size"));
        }
        Ok(bytes)
    }
    pub fn record_ref(&self) -> Result<String, BuildRecordError> {
        Ok(content_ref(&self.canonical_bytes()?))
    }
    pub fn parse(bytes: &[u8], reference: &str) -> Result<Self, BuildRecordError> {
        if bytes.len() > MAX_MANIFEST_BYTES || content_ref(bytes) != reference {
            return Err(BuildRecordError("build_record_manifest_digest"));
        }
        let m: Self =
            serde_json::from_slice(bytes).map_err(|_| BuildRecordError("build_record_schema"))?;
        if m.canonical_bytes()? != bytes {
            return Err(BuildRecordError("build_record_noncanonical"));
        }
        Ok(m)
    }
    pub fn match_assignment(
        &self,
        contract_ref: &str,
        d: &BoundDerivation,
        source_ref: &str,
        attempt_id: &str,
    ) -> Result<(), BuildRecordError> {
        self.validate()?;
        if self.contract_ref != contract_ref
            || self.source_closure_ref != source_ref
            || self.creation_attempt_id != attempt_id
            || self.derivation_ref
                != d.derivation_ref()
                    .map_err(|_| BuildRecordError("build_record_derivation"))?
        {
            return Err(BuildRecordError("build_record_assignment_mismatch"));
        }
        let registered = crate::proposal::python_build_outputs(d)
            .map_err(|_| BuildRecordError("build_record_plan_invalid"))?;
        let expected = registered
            .iter()
            .map(|r| (&r.root, &r.requirements_ref, &r.plan_ref))
            .collect::<BTreeSet<_>>();
        let observed = self
            .roots
            .iter()
            .map(|r| (&r.path, &r.requirements_ref, &r.plan_ref))
            .collect::<BTreeSet<_>>();
        if expected.is_empty() || expected != observed {
            return Err(BuildRecordError("build_record_root_not_owned"));
        }
        Ok(())
    }
}
