//! Binding requirements are semantic input; redeemed values remain Runner-private.
use crate::requirements::{ExecutionPhase, ResourceOperation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableKind {
    SigningSecret,
    ServiceCredential,
    Configuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableRequirement {
    pub name: String,
    pub kind: VariableKind,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub resource: String,
    pub operation: ResourceOperation,
    pub phase: ExecutionPhase,
    pub secret: bool,
    #[serde(default)]
    pub artifact_embedding: bool,
    /// Random 256-bit signing material, never a substitute for external authentication.
    #[serde(default)]
    pub temporary: bool,
}

pub fn validate(requirements: &[VariableRequirement]) -> Result<(), &'static str> {
    let mut names = BTreeSet::new();
    if requirements.len() > 32 {
        return Err("variable_binding_bounds");
    }
    for r in requirements {
        if r.name.is_empty()
            || r.name.len() > 64
            || !r.name.as_bytes()[0].is_ascii_uppercase()
            || !r
                .name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || r.name.starts_with("LD_")
            || r.name.starts_with("DYLD_")
            || r.name.starts_with("ATO_")
            || matches!(
                r.name.as_str(),
                "PATH"
                    | "HOME"
                    | "TMPDIR"
                    | "PYTHONPATH"
                    | "PYTHONHOME"
                    | "NODE_OPTIONS"
                    | "HTTP_PROXY"
                    | "HTTPS_PROXY"
                    | "ALL_PROXY"
                    | "NO_PROXY"
            )
            || r.purpose.is_empty()
            || r.purpose.len() > 512
            || r.purpose.contains('\0')
            || r.resource.is_empty()
            || r.resource.len() > 256
            || !r
                .resource
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
            || [&r.service, &r.endpoint, &r.account, &r.tenant]
                .iter()
                .any(|v| {
                    v.as_ref()
                        .is_some_and(|s| s.is_empty() || s.len() > 512 || s.contains('\0'))
                })
            || !names.insert((&r.name, r.phase))
        {
            return Err("variable_binding_invalid");
        }
        if r.temporary
            && (r.kind != VariableKind::SigningSecret
                || !r.secret
                || r.service.is_some()
                || r.endpoint.is_some()
                || r.account.is_some()
                || r.tenant.is_some())
        {
            return Err("temporary_variable_cannot_authenticate_external_service");
        }
        if r.secret && r.artifact_embedding {
            return Err("secret_artifact_embedding_unsupported");
        }
    }
    Ok(())
}
