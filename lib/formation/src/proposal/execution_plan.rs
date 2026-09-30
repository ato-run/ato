//! Typed authoring of registered process operations. Never shell generation.
use super::{
    ProposalError,
    python_http::{contract_requirements, validate_http_contract},
};
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

/// Bounded public discovery hints, never the private authorization path map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
    #[serde(flatten)]
    pub reference: SourceReference,
    pub purpose: String,
}

pub fn source_inspection_priority(path: &str) -> (u8, usize) {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let depth = path.bytes().filter(|b| *b == b'/').count();
    let test = path
        .split('/')
        .any(|p| matches!(p, "test" | "tests" | "__tests__" | "cypress" | "fixtures"));
    let priority = if test {
        6
    } else {
        match name.as_str() {
            "package.json" | "pyproject.toml" | "setup.py" => 0,
            "readme.md" | "readme.rst" | "readme" => 1,
            "__main__.py" | "main.py" | "server.py" | "server.js" | "server.mjs" | "index.js"
            | "app.py" | "main.js" => 2,
            "configuration.mjs" | "config.mjs" | "configuration.js" | "config.js"
            | "settings.py" | "gulpfile.js" | "gruntfile.js" | "vite.config.js"
            | "vite.config.ts" | "webpack.config.js" | "rollup.config.js" => 3,
            "package-lock.json" | "requirements.txt" => 4,
            "dockerfile" => 5,
            _ if name.ends_with(".py")
                || name.ends_with(".mjs")
                || name.ends_with(".cjs")
                || name.ends_with(".js") =>
            {
                6
            }
            _ => 7,
        }
    };
    (priority, depth)
}

impl PlanAuthorization {
    pub fn catalog_sources(&self) -> Vec<CatalogSource> {
        let mut sources: Vec<_> = self.files.iter().collect();
        sources.sort_by_key(|(id, file)| (source_inspection_priority(&file.path), *id));
        sources
            .into_iter()
            .take(32)
            .map(|(id, file)| CatalogSource {
                reference: SourceReference {
                    file_id: id.clone(),
                    digest: file.digest.clone(),
                },
                purpose: [
                    "manifest",
                    "readme",
                    "entrypoint",
                    "configuration",
                    "lockfile",
                    "dockerfile",
                    "source",
                    "other",
                ][source_inspection_priority(&file.path).0 as usize]
                    .into(),
            })
            .collect()
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_oci: Option<crate::source_oci_plan::SourceOciRecipe>,
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

/// Source-owned secret/config files never enter model context or plan IDs.
pub fn source_file_allowed(path: &str) -> bool {
    source_path(path) && !credential_path(path)
}

fn credential_path(path: &str) -> bool {
    path.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        part == ".env"
            || part.starts_with(".env.")
            || part.starts_with(".dev.vars")
            || matches!(
                part.as_str(),
                ".ssh"
                    | ".aws"
                    | ".azure"
                    | ".kube"
                    | ".npmrc"
                    | ".pypirc"
                    | "credentials"
                    | "secrets"
            )
            || part.ends_with(".pem")
            || part.ends_with(".key")
    })
}

impl PlanAuthorization {
    pub fn validate(&self) -> Result<(), ProposalError> {
        if let Some(recipe) = &self.source_oci {
            recipe.validate().map_err(ProposalError)?;
        }
        if self.files.len() > 2048
            || self.toolchains.len() > 16
            || self.files.iter().any(|(id, file)| {
                !crate::generation::logical_id(id)
                    || !source_path(&file.path)
                    || credential_path(&file.path)
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        production_only: bool,
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
    /// Source-owned build output served by the existing browser adapter. The
    /// entrypoint ref must be the manifest, never a browser script run as Node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub argv: Vec<String>,
    pub cwd: String,
    /// Static serving allocates its port through the browser adapter. Process
    /// routes must still provide a nonzero source-derived guest port.
    #[serde(default)]
    pub guest_port: u16,
    pub dependencies: Vec<DependencyOperation>,
    pub build_scripts: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<PlanStateRequirement>,
    pub requirements: ExecutionRequirements,
    pub basis: Vec<RequirementBasis>,
    pub unknowns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStateRequirement {
    pub id: String,
    pub mount: String,
    pub access: crate::authoring::StateAccess,
}

pub fn isolated_state_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Guest-owned paths only. A state mount can never mask the policy, shim,
/// toolchain, kernel interfaces or system libraries.
pub fn isolated_state_mount(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(source_path)
        && ["/data", "/state", "/app"]
            .iter()
            .any(|root| path == *root && *root != "/app" || path.starts_with(&format!("{root}/")))
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
        if !matches!(self.runtime.name.as_str(), "python" | "node" | "oci") {
            return Err(ProposalError("unsupported_toolchain"));
        }
        if authorization.toolchains.get(&self.runtime.name) != Some(&self.runtime.version) {
            return Err(ProposalError("runtime_toolchain_unavailable"));
        }
        if (self.guest_port == 0 && self.static_output.is_none())
            || !(self.cwd == "." || source_path(&self.cwd))
            || self.argv.len() > 64
            || self.argv.iter().any(|a| a.len() > 512 || a.contains('\0'))
            || self.dependencies.len() > 4
            || self.build_scripts.len() > 4
            || self.state.len() > 8
            || self
                .state
                .iter()
                .any(|s| !isolated_state_id(&s.id) || !isolated_state_mount(&s.mount))
            || self.environment.len() > 32
            || self.environment.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 64
                    || value.len() > 1024
                    || value.contains('\0')
                    || !key
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    || key.starts_with("LD_")
                    || key.starts_with("DYLD_")
                    || key.starts_with("ATO_")
                    || matches!(
                        key.as_str(),
                        "PATH"
                            | "PYTHONHOME"
                            | "PYTHONPATH"
                            | "NODE_OPTIONS"
                            | "HTTP_PROXY"
                            | "HTTPS_PROXY"
                            | "ALL_PROXY"
                            | "NO_PROXY"
                    )
                    || key.contains("SECRET")
                    || key.contains("TOKEN")
                    || key.contains("PASSWORD")
                    || key.ends_with("_KEY")
            })
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
        if self.runtime.name == "oci" {
            return self.compile_oci(frozen, authorization, entrypoint);
        }
        if let Some(output) = &self.static_output {
            let manifest = if self.cwd == "." {
                "package.json".to_owned()
            } else {
                format!("{}/package.json", self.cwd)
            };
            if self.runtime.name != "node"
                || entrypoint != manifest
                || !source_path(output)
                || self.module.is_some()
                || !self.argv.is_empty()
                || !self.environment.is_empty()
                || !self.state.is_empty()
            {
                return Err(ProposalError("unsupported_static_output"));
            }
        } else if (self.runtime.name == "python" && !entrypoint.ends_with(".py"))
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
        validate_http_contract(&frozen.base_contract, source, &port)?;
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
            "argv":[executable,"--version"],"cwd":self.cwd,"network":"scoped-dependencies"}),
        ];
        let runtimes = vec![json!({"name":self.runtime.name,"version":self.runtime.version})];
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
                DependencyOperation::NpmCi {
                    manifest,
                    lockfile,
                    production_only,
                } if self.runtime.name == "node" => {
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
                    authorization
                        .toolchains
                        .get("npm")
                        .ok_or(ProposalError("runtime_toolchain_unavailable"))?;
                    let mut argv = vec![
                        format!("/opt/ato/toolchains/node/{}/bin/npm", self.runtime.version),
                        "ci".into(),
                        "--ignore-scripts".into(),
                        "--no-audit".into(),
                        "--no-fund".into(),
                    ];
                    if *production_only {
                        argv.push("--omit=dev".into());
                    }
                    argv
                }
                _ => return Err(ProposalError("unsupported_dependency_operation")),
            };
            steps.push(json!({"id":format!("dependencies-{}",steps.len()),"use":"ato.process@1","op":"exec",
                "argv":argv,"cwd":self.cwd,"network":"scoped-dependencies"}));
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
            authorization
                .toolchains
                .get("npm")
                .ok_or(ProposalError("runtime_toolchain_unavailable"))?;
            steps.push(json!({"id":format!("build-{}",steps.len()),"use":"ato.process@1","op":"exec",
                "argv":[format!("/opt/ato/toolchains/node/{}/bin/npm", self.runtime.version),"run",script],"cwd":self.cwd,"network":"scoped-build"}));
        }
        let serving_port = if let Some(root) = &self.static_output {
            steps.push(
                json!({"id":"app","use":"ato.browser@1","op":"serve","cwd":".",
                "source":"workspace","root":root,"entry":"index.html","spa_fallback":false}),
            );
            json!({"id":port,"use":"ato.http@1","from":"app"})
        } else {
            let mut argv = match &self.module {
                None => vec![executable, format!("/app/{entrypoint}")],
                Some(module) => {
                    if self.runtime.name != "python"
                        || module.is_empty()
                        || module.len() > 128
                        || !module.split('.').all(|s| {
                            !s.is_empty()
                                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        })
                    {
                        return Err(ProposalError("unsupported_module_entrypoint"));
                    }
                    let prefix = if self.cwd == "." {
                        String::new()
                    } else {
                        format!("{}/", self.cwd)
                    };
                    let module_path = format!("{prefix}{}", module.replace('.', "/"));
                    if entrypoint != format!("{module_path}.py")
                        && entrypoint != format!("{module_path}/__main__.py")
                    {
                        return Err(ProposalError("proposal_module_source_mismatch"));
                    }
                    vec![executable, "-m".into(), module.clone()]
                }
            };
            argv.extend(self.argv.iter().cloned());
            steps.push(
            json!({"id":"app","use":"ato.process@1","op":"serve","argv":argv,"cwd":self.cwd,"env":self.environment}),
        );
            json!({"id":port,"use":"ato.http@1","from":"app","guest_port":self.guest_port})
        };
        let document = json!({"schema":"ato.capsule/1",
            "input":[{"id":"workspace","use":"ato.workspace@1","path":"."}],
            "runtime":runtimes,"derive":{"step":steps},
            "port":[serving_port],
            "contract":{"require":contract_requirements(&frozen.base_contract)},"requirements":self.requirements,
            "state":self.state.iter().map(|s| json!({"id":s.id,"use":crate::authoring::STATE_FILESYSTEM_PROTOCOL,"mount":s.mount,"access":s.access})).collect::<Vec<_>>()});
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
        let mut candidate = authorization.candidate(source, derivation_ref.clone());
        candidate.provisions = derivation
            .runtimes
            .iter()
            .map(|(name, version)| format!("toolchain.{name}.{version}"))
            .collect();
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
    fn compile_oci(
        &self,
        frozen: &FrozenSearchV1,
        authorization: &PlanAuthorization,
        entrypoint: &str,
    ) -> Result<(CompiledGeneration, SearchCandidate), ProposalError> {
        if entrypoint != "Dockerfile"
            || self.cwd != "."
            || !self.argv.is_empty()
            || self.module.is_some()
            || !self.environment.is_empty()
            || !self.dependencies.is_empty()
            || !self.build_scripts.is_empty()
        {
            return Err(ProposalError("unsupported_source_oci_selection"));
        }
        let recipe = authorization
            .source_oci
            .as_ref()
            .ok_or(ProposalError("source_oci_builder_unavailable"))?;
        let source = frozen
            .initial_source
            .as_ref()
            .ok_or(ProposalError("proposal_initial_source_required"))?;
        let ports: std::collections::BTreeSet<_> = frozen
            .base_contract
            .requirements
            .iter()
            .filter_map(|r| r.port.as_deref())
            .collect();
        let ports = ports.into_iter().collect::<Vec<_>>();
        let [port] = ports.as_slice() else {
            return Err(ProposalError("proposal_contract_unsupported"));
        };
        validate_http_contract(&frozen.base_contract, source, port)?;
        let document = json!({"schema":"ato.capsule/1",
            "input":[{"id":"workspace","use":"ato.workspace@1","path":"."}],
            "derive":{"step":[{"id":"app","use":crate::source_oci_plan::OCI_PROTOCOL,"op":"serve","source":"workspace"}]},
            "port":[{"id":port,"use":"ato.http@1","from":"app","guest_port":self.guest_port}],
            "source_oci":recipe,"requirements":self.requirements,
            "state":self.state.iter().map(|s| json!({"id":s.id,"use":crate::authoring::STATE_FILESYSTEM_PROTOCOL,"mount":s.mount,"access":s.access})).collect::<Vec<_>>(),
            "contract":{"require":contract_requirements(&frozen.base_contract)}});
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
        let mut candidate = authorization.candidate(source, derivation_ref.clone());
        candidate.provisions.clear();
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
