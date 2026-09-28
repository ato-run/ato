//! A single Ato-owned, base-free constructor. No preset, known D or provider
//! TOML is consumed. The only producer argument is an authorized opaque file ID.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ProposalError;
use crate::{
    authoring::{
        BindingContext, BoundContract, HTTP_CONTRACT_VERIFIER, WORKSPACE_CONTRACT_VERIFIER, bind,
    },
    capsule_toml::parse_capsule_toml,
    generation::{CompiledGeneration, is_sha256},
    search::{InitialSource, SearchCandidate, execution_requirements},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonHttpProcess {
    pub python_version: String,
    pub http_port: String,
    pub guest_port: u16,
}
impl PythonHttpProcess {
    pub fn validate(&self) -> Result<(), ProposalError> {
        let version = semver::Version::parse(&self.python_version)
            .map_err(|_| ProposalError("proposal_runtime_invalid"))?;
        if version.major != 3
            || !version.pre.is_empty()
            || !version.build.is_empty()
            || version.to_string() != self.python_version
            || self.guest_port == 0
            || self.http_port.is_empty()
            || self.http_port.len() > 64
            || !self
                .http_port
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(ProposalError("proposal_template_invalid"));
        }
        Ok(())
    }

    /// v0 supports only exact HTTP conditions and optional workspace digest.
    /// Never drop fields, weaken K, or substitute observed values for frozen K.
    pub fn validate_contract(
        &self,
        k: &BoundContract,
        source: &InitialSource,
    ) -> Result<(), ProposalError> {
        self.validate()?;
        let invalid = ProposalError("proposal_contract_unsupported");
        if k.schema != "ato.contract/1"
            || !is_sha256(&source.closure_ref)
            || !k
                .requirements
                .iter()
                .any(|r| r.verifier == HTTP_CONTRACT_VERIFIER)
            || k.requirements.windows(2).any(|w| w[0].id >= w[1].id)
        {
            return Err(invalid);
        }
        for r in &k.requirements {
            if r.id.is_empty() {
                return Err(invalid);
            }
            match r.verifier.as_str() {
                HTTP_CONTRACT_VERIFIER
                    if r.port.as_ref() == Some(&self.http_port)
                        && r.method.as_deref() == Some("GET")
                        && r.path.as_ref().is_some_and(|p| p.starts_with('/'))
                        && r.status.is_some_and(|s| (100..=599).contains(&s))
                        && r.body_digest.as_ref().is_none_or(|d| is_sha256(d))
                        && r.input.is_none()
                        && r.digest.is_none() => {}
                WORKSPACE_CONTRACT_VERIFIER
                    if r.input.as_deref() == Some("workspace")
                        && r.digest.as_ref() == Some(&source.closure_ref)
                        && r.port.is_none()
                        && r.method.is_none()
                        && r.path.is_none()
                        && r.status.is_none()
                        && r.body_digest.is_none() => {}
                _ => return Err(invalid),
            }
        }
        Ok(())
    }

    pub(super) fn compile(
        &self,
        source: &InitialSource,
        k: &BoundContract,
        path: &str,
    ) -> Result<CompiledGeneration, ProposalError> {
        self.validate_contract(k, source)?;
        // All text below is Ato-owned or taken from frozen K/authorization.
        // Provider paths, ports, executables and runtime versions never enter.
        let requirements: Vec<Value> = k.requirements.iter().map(|r| {
            if r.verifier == HTTP_CONTRACT_VERIFIER {
                let mut expect = json!({"status": r.status.unwrap()});
                if let Some(digest) = &r.body_digest { expect["body_digest"] = json!(digest); }
                json!({"id":r.id,"use":r.verifier,"port":r.port,"method":r.method,"path":r.path,"expect":expect})
            } else {
                json!({"id":r.id,"use":r.verifier,"input":r.input,"expect":{"digest":r.digest}})
            }
        }).collect();
        let document = json!({
            "schema":"ato.capsule/1",
            "input":[{"id":"workspace","use":"ato.workspace@1","path":"."}],
            "runtime":[{"name":"python","version":self.python_version}],
            "derive":{"step":[{"id":"app","use":"ato.process@1","op":"serve","cwd":".",
                "argv":[format!("/opt/ato/toolchains/python/{}/bin/python3",self.python_version),"-B",format!("/app/{path}")]}]},
            "port":[{"id":self.http_port,"use":"ato.http@1","from":"app","guest_port":self.guest_port}],
            "contract":{"require":requirements}
        });
        let invalid = |_| ProposalError("proposal_compilation_failed");
        let value = toml::Value::try_from(document).map_err(invalid)?;
        let capsule_toml =
            toml::to_string(&value).map_err(|_| ProposalError("proposal_compilation_failed"))?;
        let draft = parse_capsule_toml(&capsule_toml)
            .map_err(|_| ProposalError("proposal_compilation_failed"))?;
        let (generated_k, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &source.closure_ref,
            },
        )
        .map_err(|_| ProposalError("proposal_compilation_failed"))?;
        if generated_k != *k {
            return Err(ProposalError("proposal_contract_mismatch"));
        }
        let derivation_ref = derivation
            .derivation_ref()
            .map_err(|_| ProposalError("proposal_canonicalization"))?;
        let base_contract_ref = k
            .contract_ref()
            .map_err(|_| ProposalError("proposal_canonicalization"))?;
        Ok(CompiledGeneration {
            capsule_toml,
            derivation,
            derivation_ref,
            base_contract_ref,
        })
    }

    pub(super) fn candidate(
        &self,
        source: &InitialSource,
        derivation_ref: String,
    ) -> SearchCandidate {
        SearchCandidate {
            derivation_ref,
            effects: "pure".into(),
            requirements: execution_requirements(true, false),
            provisions: vec![format!("toolchain.python.{}", self.python_version)],
            materialization: source.materialization(),
        }
    }
}
