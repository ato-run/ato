//! Typed authoring of registered process operations. Never shell generation.
use super::{ProposalError, PythonHttpProcess, python_http::contract_requirements};
use crate::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    generation::{CompiledGeneration, is_sha256},
    requirements::ExecutionRequirements,
    search::{FrozenSearchV1, InitialSource, SearchCandidate, execution_requirements},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub file_id: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedSourceFile {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanAuthorization {
    pub files: BTreeMap<String, VerifiedSourceFile>,
    pub toolchains: BTreeMap<String, String>,
}

pub fn source_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
        })
}

impl PlanAuthorization {
    pub fn validate(&self) -> Result<(), ProposalError> {
        if self.files.len() > 2048
            || self.toolchains.len() > 16
            || self.files.iter().any(|(id, file)| {
                !crate::generation::logical_id(id)
                    || !source_path(&file.path)
                    || !is_sha256(&file.digest)
            })
            || self.toolchains.iter().any(|(name, version)| {
                !matches!(name.as_str(), "python" | "node" | "npm" | "oci")
                    || semver::Version::parse(version).is_err()
            })
        {
            return Err(ProposalError("execution_plan_authorization_invalid"));
        }
        Ok(())
    }

    fn resolve(&self, reference: &SourceReference) -> Result<&str, ProposalError> {
        let file = self
            .files
            .get(&reference.file_id)
            .ok_or(ProposalError("source_inspection_required"))?;
        if file.digest != reference.digest {
            return Err(ProposalError("proposal_source_digest_mismatch"));
        }
        Ok(&file.path)
    }

    pub fn candidate(&self, source: &InitialSource, derivation_ref: String) -> SearchCandidate {
        SearchCandidate {
            derivation_ref,
            effects: "pure".into(),
            requirements: execution_requirements(true, true),
            provisions: self
                .toolchains
                .iter()
                .map(|(n, v)| format!("toolchain.{n}.{v}"))
                .collect(),
            materialization: source.materialization(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSelection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DependencyOperation {
    PythonRequirements {
        requirements: SourceReference,
    },
    NpmCi {
        manifest: SourceReference,
        lockfile: SourceReference,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementBasis {
    pub source: SourceReference,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanProposal {
    pub runtime: RuntimeSelection,
    pub entrypoint: SourceReference,
    pub argv: Vec<String>,
    pub cwd: String,
    pub guest_port: u16,
    pub dependencies: Vec<DependencyOperation>,
    pub build_scripts: Vec<String>,
    pub requirements: ExecutionRequirements,
    pub basis: Vec<RequirementBasis>,
    pub unknowns: Vec<String>,
}

impl ExecutionPlanProposal {
    pub(super) fn compile(
        &self,
        frozen: &FrozenSearchV1,
        authorization: &PlanAuthorization,
    ) -> Result<(CompiledGeneration, SearchCandidate), ProposalError> {
        authorization.validate()?;
        let exploration = frozen
            .policy
            .exploration
            .as_ref()
            .ok_or(ProposalError("exploration_policy_required"))?;
        self.requirements
            .within(&exploration.ceiling)
            .map_err(|e| ProposalError(e.0))?;
        if !matches!(self.runtime.name.as_str(), "python" | "node") {
            return Err(ProposalError("unsupported_toolchain"));
        }
        if authorization.toolchains.get(&self.runtime.name) != Some(&self.runtime.version) {
            return Err(ProposalError("runtime_toolchain_unavailable"));
        }
        if self.guest_port == 0
            || !(self.cwd == "." || source_path(&self.cwd))
            || self.argv.len() > 64
            || self.argv.iter().any(|a| a.len() > 512 || a.contains('\0'))
            || self.dependencies.len() > 4
            || self.build_scripts.len() > 4
            || self.basis.len() > 16
            || self.unknowns.len() > 16
            || self.unknowns.iter().any(|u| u.len() > 512)
            || (!self.requirements.is_empty() && self.basis.is_empty())
        {
            return Err(ProposalError("execution_plan_bounds"));
        }
        for basis in &self.basis {
            authorization.resolve(&basis.source)?;
            if basis.reason.is_empty() || basis.reason.len() > 512 {
                return Err(ProposalError("requirement_basis_invalid"));
            }
        }
        let entrypoint = authorization.resolve(&self.entrypoint)?;
        if (self.runtime.name == "python" && !entrypoint.ends_with(".py"))
            || (self.runtime.name == "node"
                && ![".js", ".mjs", ".cjs"]
                    .iter()
                    .any(|ext| entrypoint.ends_with(ext)))
        {
            return Err(ProposalError("unsupported_entrypoint"));
        }
        let source = frozen
            .initial_source
            .as_ref()
            .ok_or(ProposalError("proposal_initial_source_required"))?;
        // Logical Port is selected from frozen K, not supplied or weakened by LLM.
        let ports: std::collections::BTreeSet<_> = frozen
            .base_contract
            .requirements
            .iter()
            .filter_map(|r| r.port.as_deref())
            .collect();
        let ports = ports.iter().copied().collect::<Vec<_>>();
        let [port] = ports.as_slice() else {
            return Err(ProposalError("proposal_contract_unsupported"));
        };
        let port = port.to_string();
        PythonHttpProcess {
            python_version: "3.12.7".into(),
            http_port: port.clone(),
            guest_port: self.guest_port,
        }
        .validate_contract(&frozen.base_contract, source)?;
        let executable = format!(
            "/opt/ato/toolchains/{}/{}/bin/{}",
            self.runtime.name,
            self.runtime.version,
            if self.runtime.name == "python" {
                "python3"
            } else {
                "node"
            }
        );
        let mut steps = vec![
            json!({"id":"check-toolchain","use":"ato.process@1","op":"exec",
            "argv":[executable,"--version"],"cwd":self.cwd}),
        ];
        let mut runtimes = vec![json!({"name":self.runtime.name,"version":self.runtime.version})];
        for dependency in &self.dependencies {
            let argv = match dependency {
                DependencyOperation::PythonRequirements { requirements }
                    if self.runtime.name == "python" =>
                {
                    let path = authorization.resolve(requirements)?;
                    if !path.ends_with(".txt") {
                        return Err(ProposalError("unsupported_dependency_manifest"));
                    }
                    let minor = self
                        .runtime
                        .version
                        .rsplit_once('.')
                        .map(|(v, _)| v)
                        .ok_or(ProposalError("unsupported_toolchain"))?;
                    vec![
                        executable.clone(),
                        "-m".into(),
                        "pip".into(),
                        "install".into(),
                        "--no-input".into(),
                        "--only-binary=:all:".into(),
                        "--require-hashes".into(),
                        "--target".into(),
                        format!("/app/.venv/lib/python{minor}/site-packages"),
                        "-r".into(),
                        format!("/app/{path}"),
                    ]
                }
                DependencyOperation::NpmCi { manifest, lockfile }
                    if self.runtime.name == "node" =>
                {
                    let manifest = authorization.resolve(manifest)?;
                    let lockfile = authorization.resolve(lockfile)?;
                    let prefix = if self.cwd == "." {
                        String::new()
                    } else {
                        format!("{}/", self.cwd)
                    };
                    if manifest != format!("{prefix}package.json")
                        || lockfile != format!("{prefix}package-lock.json")
                    {
                        return Err(ProposalError("unsupported_dependency_manifest"));
                    }
                    let npm = authorization
                        .toolchains
                        .get("npm")
                        .ok_or(ProposalError("runtime_toolchain_unavailable"))?;
                    runtimes.push(json!({"name":"npm","version":npm}));
                    vec![
                        format!("/opt/ato/toolchains/npm/{npm}/bin/npm"),
                        "ci".into(),
                        "--ignore-scripts".into(),
                        "--no-audit".into(),
                        "--no-fund".into(),
                    ]
                }
                _ => return Err(ProposalError("unsupported_dependency_operation")),
            };
            steps.push(json!({"id":format!("dependencies-{}",steps.len()),"use":"ato.process@1","op":"exec",
                "argv":argv,"cwd":self.cwd,"network":"dependency-resolution"}));
        }
        for script in &self.build_scripts {
            if self.runtime.name != "node"
                || script.is_empty()
                || script.len() > 64
                || !script
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
            {
                return Err(ProposalError("unsupported_build_operation"));
            }
            let npm = authorization
                .toolchains
                .get("npm")
                .ok_or(ProposalError("runtime_toolchain_unavailable"))?;
            steps.push(json!({"id":format!("build-{}",steps.len()),"use":"ato.process@1","op":"exec",
                "argv":[format!("/opt/ato/toolchains/npm/{npm}/bin/npm"),"run",script],"cwd":self.cwd}));
        }
        let mut argv = vec![executable, format!("/app/{entrypoint}")];
        argv.extend(self.argv.iter().cloned());
        steps.push(
            json!({"id":"app","use":"ato.process@1","op":"serve","argv":argv,"cwd":self.cwd}),
        );
        let document = json!({"schema":"ato.capsule/1",
            "input":[{"id":"workspace","use":"ato.workspace@1","path":"."}],
            "runtime":runtimes,"derive":{"step":steps},
            "port":[{"id":port,"use":"ato.http@1","from":"app","guest_port":self.guest_port}],
            "contract":{"require":contract_requirements(&frozen.base_contract)},"requirements":self.requirements});
        let value = toml::Value::try_from(document)
            .map_err(|_| ProposalError("proposal_compilation_failed"))?;
        let capsule_toml =
            toml::to_string(&value).map_err(|_| ProposalError("proposal_compilation_failed"))?;
        let draft = parse_capsule_toml(&capsule_toml)
            .map_err(|_| ProposalError("proposal_compilation_failed"))?;
        let (contract, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &source.closure_ref,
            },
        )
        .map_err(|_| ProposalError("proposal_compilation_failed"))?;
        if contract != frozen.base_contract {
            return Err(ProposalError("proposal_contract_mismatch"));
        }
        let derivation_ref = derivation
            .derivation_ref()
            .map_err(|_| ProposalError("proposal_canonicalization"))?;
        let candidate = authorization.candidate(source, derivation_ref.clone());
        Ok((
            CompiledGeneration {
                capsule_toml,
                derivation,
                derivation_ref,
                base_contract_ref: frozen.base_contract_ref.clone(),
            },
            candidate,
        ))
    }
}
