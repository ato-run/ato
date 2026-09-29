//! `node_static_workspace@1`: an Ato-owned constructor for one owner-authorized
//! workspace of a bounded inventory. The only producer argument is an opaque
//! workspace ID; cwd, output root, argv, toolchains and network come from this
//! private authorization, never from provider text.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    ProposalError,
    python_http::{contract_requirements, validate_http_contract},
};
use crate::{
    authoring::{BindingContext, BoundContract, bind},
    capsule_toml::parse_capsule_toml,
    generation::{CompiledGeneration, logical_id},
    search::{InitialSource, SearchCandidate, execution_requirements},
};

/// Same bound as the inventory that produced it.
pub const MAX_WORKSPACE_CANDIDATES: usize = 64;
/// The port the canonical static serve step exports; K must observe it.
pub const WORKSPACE_HTTP_PORT: &str = "app.http";

/// Private owner-issued resolution; never serialize this to a provider.
/// Constructed from the bounded workspace inventory of the frozen source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeStaticWorkspaceAuthorization {
    /// One dependency installation scope: the source root.
    pub install: WorkspaceInstallScope,
    /// Static-qualified workspaces only, keyed by opaque logical ID.
    pub workspaces: BTreeMap<String, WorkspaceStaticBuild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceInstallScope {
    /// Exact.
    pub node_version: String,
    /// Exact pnpm or Yarn, resolved from the root declarations.
    pub package_manager: String,
    pub package_manager_version: String,
    pub install_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceStaticBuild {
    /// Source-relative workspace directory where the build script runs.
    pub cwd: String,
    /// Source-relative served root, inside `cwd`.
    pub output_root: String,
}

fn exact(version: &str) -> bool {
    semver::Version::parse(version)
        .is_ok_and(|v| v.pre.is_empty() && v.build.is_empty() && v.to_string() == version)
}

/// A normalized, non-empty source-relative path. No `.`, `..`, empty segments,
/// absolute paths or characters outside a narrow filename grammar.
pub fn relative_dir(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').count() <= 8
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'@'))
        })
}

impl WorkspaceInstallScope {
    /// The exact install mode is a function of the manager generation; a
    /// mismatching pair is refused rather than trusted.
    pub fn expected_install_mode(manager: &str, version: &str) -> Option<&'static str> {
        match manager {
            "pnpm" => Some("--frozen-lockfile"),
            "yarn" if version.starts_with("1.") => Some("--frozen-lockfile"),
            "yarn" if !version.starts_with("0.") => Some("--immutable"),
            _ => None,
        }
    }
}

impl NodeStaticWorkspaceAuthorization {
    pub fn validate(&self) -> Result<(), ProposalError> {
        let invalid = ProposalError("proposal_workspace_invalid");
        let install = &self.install;
        if !exact(&install.node_version)
            || !exact(&install.package_manager_version)
            || WorkspaceInstallScope::expected_install_mode(
                &install.package_manager,
                &install.package_manager_version,
            ) != Some(install.install_mode.as_str())
        {
            return Err(invalid);
        }
        if self.workspaces.is_empty() || self.workspaces.len() > MAX_WORKSPACE_CANDIDATES {
            return Err(ProposalError("proposal_domain_bounds"));
        }
        let mut cwds = BTreeSet::new();
        for (id, ws) in &self.workspaces {
            if !logical_id(id)
                || !relative_dir(&ws.cwd)
                || !relative_dir(&ws.output_root)
                || !ws
                    .output_root
                    .strip_prefix(&ws.cwd)
                    .is_some_and(|rest| rest.starts_with('/'))
                || !cwds.insert(&ws.cwd)
            {
                return Err(invalid);
            }
        }
        Ok(())
    }

    /// K must be exactly the static-surface template shape: HTTP observations
    /// on the served port and, optionally, the frozen source identity.
    pub fn validate_contract(
        &self,
        k: &BoundContract,
        source: &InitialSource,
    ) -> Result<(), ProposalError> {
        self.validate()?;
        validate_http_contract(k, source, WORKSPACE_HTTP_PORT)
    }

    pub(super) fn compile(
        &self,
        source: &InitialSource,
        k: &BoundContract,
        workspace: &WorkspaceStaticBuild,
    ) -> Result<CompiledGeneration, ProposalError> {
        self.validate_contract(k, source)?;
        let install = &self.install;
        let manager = install.package_manager.as_str();
        // All text below is Ato-owned or taken from frozen K/authorization.
        // No shell, no provider-supplied argv/cwd/output/runtime/network.
        let document = json!({
            "schema":"ato.capsule/1",
            "input":[{"id":"workspace","use":"ato.workspace@1","path":"."}],
            "runtime":[{"name":"node","version":install.node_version},
                {"name":manager,"version":install.package_manager_version}],
            "derive":{"step":[
                {"id":"install","use":"ato.process@1","op":"exec","cwd":".",
                    "argv":[manager,"install",install.install_mode],"network":"dependency-resolution"},
                {"id":"build","use":"ato.process@1","op":"exec","cwd":workspace.cwd,
                    "argv":[manager,"run","build"]},
                {"id":"site","use":"ato.browser@1","op":"serve","cwd":".","source":"workspace",
                    "root":workspace.output_root,"entry":"index.html","spa_fallback":true}]},
            "port":[{"id":WORKSPACE_HTTP_PORT,"use":"ato.http@1","from":"site"}],
            "contract":{"require":contract_requirements(k)}
        });
        let failed = |_| ProposalError("proposal_compilation_failed");
        let value = toml::Value::try_from(document).map_err(failed)?;
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

    /// Requirement/provision facts of the canonical static build route: a
    /// contained build with exact toolchains, served by the browser surface.
    pub(super) fn candidate(
        &self,
        source: &InitialSource,
        derivation_ref: String,
    ) -> SearchCandidate {
        let install = &self.install;
        let mut provisions = vec![
            format!("toolchain.node.{}", install.node_version),
            format!(
                "toolchain.{}.{}",
                install.package_manager, install.package_manager_version
            ),
        ];
        provisions.sort();
        SearchCandidate {
            derivation_ref,
            effects: "pure".into(),
            requirements: execution_requirements(false, true),
            provisions,
            materialization: source.materialization(),
        }
    }
}
