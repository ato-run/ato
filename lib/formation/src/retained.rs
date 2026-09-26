//! Content-addressed retained candidate provenance, never a Capsule identity.
//! Possession of a reference is not authorization and past PASS is not a new
//! verification. Storage/assignment checks and fresh observation remain required.
use crate::authoring::{BROWSER_PROTOCOL, BoundContract, BoundDerivation, PROCESS_PROTOCOL};
use crate::browser::{BrowserContractV0, effective_contract_ref};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const RETAINED_SCHEMA: &str = "ato.retained-candidate/1";
pub const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedArtifact {
    /// Digest of transported bytes (an archive for both explicitly named kinds).
    pub content_ref: String,
    pub bytes: u64,
    pub expanded_bytes: u64,
}

/// Only resolved runtime bindings, never a copy of canonical argv/cwd/env.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedProcessBinding {
    pub toolchains: BTreeMap<String, String>,
    pub package_manager: Option<RetainedPackageManager>,
    pub python_environment: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedPackageManager {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedShape {
    ProcessWorkspace { binding: RetainedProcessBinding },
    StaticWeb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedCandidateV1 {
    pub schema: String,
    pub shape: RetainedShape,
    pub artifact: RetainedArtifact,
    /// Historical semantics unchanged: process archive / static manifest digest.
    pub materialization_ref: String,
    pub source_closure_ref: String,
    pub derivation_ref: String,
    pub base_contract_ref: String,
    pub contract_ref: String,
    pub derivation: BoundDerivation,
    pub base_contract: BoundContract,
    pub browser_contract: Option<BrowserContractV0>,
    pub creation_attempt_id: String,
    pub validation_profile: String,
}

#[derive(Debug, thiserror::Error)]
#[error("retained candidate rejected: {0}")]
pub struct RetainedError(pub &'static str);

pub fn content_ref(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn valid_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
impl RetainedCandidateV1 {
    /// Validate provenance internally. The caller must also match the request's
    /// frozen K/D; a self-consistent different descriptor is not authorized.
    pub fn validate(&self) -> Result<(), RetainedError> {
        if self.schema != RETAINED_SCHEMA {
            return Err(RetainedError("schema"));
        }
        if self.creation_attempt_id.is_empty() || self.creation_attempt_id.len() > 256 {
            return Err(RetainedError("creation_attempt"));
        }
        for reference in [
            &self.materialization_ref,
            &self.source_closure_ref,
            &self.derivation_ref,
            &self.base_contract_ref,
            &self.contract_ref,
            &self.artifact.content_ref,
        ] {
            if !valid_ref(reference) {
                return Err(RetainedError("reference"));
            }
        }
        if self.artifact.bytes == 0
            || self.artifact.bytes > 256 * 1024 * 1024
            || self.artifact.expanded_bytes > 512 * 1024 * 1024
        {
            return Err(RetainedError("artifact_size"));
        }
        if self
            .base_contract
            .contract_ref()
            .map_err(|_| RetainedError("canonical_contract"))?
            != self.base_contract_ref
            || effective_contract_ref(&self.base_contract_ref, self.browser_contract.as_ref())
                != self.contract_ref
        {
            return Err(RetainedError("contract_mismatch"));
        }
        if self
            .derivation
            .derivation_ref()
            .map_err(|_| RetainedError("canonical_derivation"))?
            != self.derivation_ref
        {
            return Err(RetainedError("derivation_mismatch"));
        }
        let workspaces: Vec<_> = self
            .derivation
            .inputs
            .iter()
            .filter(|i| i.protocol == "ato.workspace@1")
            .collect();
        if workspaces.is_empty()
            || workspaces
                .iter()
                .any(|i| i.content_ref != self.source_closure_ref)
        {
            return Err(RetainedError("source_provenance_mismatch"));
        }
        let serves: Vec<_> = self
            .derivation
            .steps
            .iter()
            .filter(|s| s.op == "serve")
            .collect();
        let [serve] = serves.as_slice() else {
            return Err(RetainedError("serving_shape"));
        };
        match &self.shape {
            RetainedShape::StaticWeb => {
                if serve.protocol != BROWSER_PROTOCOL
                    || self.validation_profile != "ato.retained-static-web/1"
                {
                    return Err(RetainedError("static_shape"));
                }
            }
            RetainedShape::ProcessWorkspace { binding } => {
                if serve.protocol != PROCESS_PROTOCOL
                    || self.validation_profile != "ato.retained-process-workspace/1"
                    || self.materialization_ref != self.artifact.content_ref
                {
                    return Err(RetainedError("process_shape"));
                }
                // No source-controlled host paths or arbitrary toolchain family.
                for (name, version) in &binding.toolchains {
                    if !matches!(name.as_str(), "node" | "python")
                        || semver::Version::parse(version).is_err()
                    {
                        return Err(RetainedError("toolchain_binding"));
                    }
                }
                for (name, requirement) in &self.derivation.runtimes {
                    let resolved = match name.as_str() {
                        "node" | "python" => binding.toolchains.get(name),
                        "pnpm" | "yarn" => binding
                            .package_manager
                            .as_ref()
                            .filter(|m| m.name == *name)
                            .map(|m| &m.version),
                        _ => None,
                    }
                    .ok_or(RetainedError("missing_runtime_binding"))?;
                    // D remains the authority; retained physical bindings cannot
                    // substitute a different runtime for a frozen requirement.
                    let matches = resolved == requirement
                        || (name == "python"
                            && semver::VersionReq::parse(requirement.trim_start_matches("=="))
                                .ok()
                                .is_some_and(|r| {
                                    semver::Version::parse(resolved)
                                        .ok()
                                        .is_some_and(|v| r.matches(&v))
                                }));
                    if !matches {
                        return Err(RetainedError("runtime_binding_mismatch"));
                    }
                }
                if binding.python_environment && !binding.toolchains.contains_key("python") {
                    return Err(RetainedError("python_binding"));
                }
                if let Some(manager) = &binding.package_manager
                    && (!matches!(manager.name.as_str(), "npm" | "pnpm" | "yarn")
                        || semver::Version::parse(&manager.version).is_err())
                {
                    return Err(RetainedError("package_manager_binding"));
                }
            }
        }
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RetainedError> {
        self.validate()?;
        let bytes = serde_jcs::to_vec(self).map_err(|_| RetainedError("canonical_descriptor"))?;
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(RetainedError("descriptor_size"));
        }
        Ok(bytes)
    }
    pub fn retained_ref(&self) -> Result<String, RetainedError> {
        Ok(content_ref(&self.canonical_bytes()?))
    }
    pub fn parse(bytes: &[u8], expected_ref: &str) -> Result<Self, RetainedError> {
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(RetainedError("descriptor_size"));
        }
        if content_ref(bytes) != expected_ref {
            return Err(RetainedError("descriptor_digest"));
        }
        let descriptor: Self =
            serde_json::from_slice(bytes).map_err(|_| RetainedError("descriptor_json"))?;
        if descriptor.canonical_bytes()? != bytes {
            return Err(RetainedError("descriptor_not_canonical"));
        }
        Ok(descriptor)
    }
    pub fn match_assignment(&self, contract: &str, derivation: &str) -> Result<(), RetainedError> {
        self.validate()?;
        if self.contract_ref != contract || self.derivation_ref != derivation {
            return Err(RetainedError("assignment_mismatch"));
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "planning"))]
mod tests {
    use super::*;
    use crate::authoring::{BindingContext, bind};
    use crate::preset::{AppPreset, synthesize_authoring};
    fn fixture() -> RetainedCandidateV1 {
        let source = format!("sha256:{}", "1".repeat(64));
        let (k, d) = bind(
            &synthesize_authoring(AppPreset::SingleHtml),
            &BindingContext {
                source_closure_ref: &source,
            },
        )
        .unwrap();
        let kref = k.contract_ref().unwrap();
        RetainedCandidateV1 {
            schema: RETAINED_SCHEMA.into(),
            shape: RetainedShape::StaticWeb,
            artifact: RetainedArtifact {
                content_ref: content_ref(b"transport"),
                bytes: 9,
                expanded_bytes: 100,
            },
            materialization_ref: content_ref(b"manifest"),
            source_closure_ref: source,
            derivation_ref: d.derivation_ref().unwrap(),
            base_contract_ref: kref.clone(),
            contract_ref: kref,
            derivation: d,
            base_contract: k,
            browser_contract: None,
            creation_attempt_id: "original-attempt".into(),
            validation_profile: "ato.retained-static-web/1".into(),
        }
    }
    #[test]
    fn descriptor_is_canonical_and_separate_from_artifact_and_capsule() {
        let d = fixture();
        let bytes = d.canonical_bytes().unwrap();
        let reference = d.retained_ref().unwrap();
        assert_ne!(reference, d.contract_ref);
        assert_ne!(reference, d.materialization_ref);
        assert_eq!(RetainedCandidateV1::parse(&bytes, &reference).unwrap(), d);
        let spaced = serde_json::to_vec_pretty(&d).unwrap();
        assert!(RetainedCandidateV1::parse(&spaced, &content_ref(&spaced)).is_err());
    }
    #[test]
    fn changed_bytes_provenance_or_assignment_are_refused() {
        let d = fixture();
        let reference = d.retained_ref().unwrap();
        let mut altered = d.clone();
        altered.base_contract.requirements.clear();
        assert!(altered.validate().is_err());
        altered = d.clone();
        altered.derivation.steps[0].entry = Some("different.html".into());
        assert!(altered.validate().is_err());
        altered = d.clone();
        altered.source_closure_ref = content_ref(b"different");
        assert!(altered.validate().is_err());
        let mut bytes = d.canonical_bytes().unwrap();
        bytes.push(b' ');
        assert!(RetainedCandidateV1::parse(&bytes, &reference).is_err());
        assert!(
            d.match_assignment(&content_ref(b"different"), &d.derivation_ref)
                .is_err()
        );
        assert!(
            d.match_assignment(&d.contract_ref, &content_ref(b"different"))
                .is_err()
        );
    }
    #[test]
    fn artifact_kind_is_not_guessed_from_a_digest() {
        let mut d = fixture();
        d.shape = RetainedShape::ProcessWorkspace {
            binding: RetainedProcessBinding {
                toolchains: BTreeMap::new(),
                package_manager: None,
                python_environment: false,
            },
        };
        assert!(d.validate().is_err());
    }
}
