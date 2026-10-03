//! Shared, bounded reasoning exchanges. A model proposes; only Ato resolves
//! verified source IDs, compiles D and grants a Runtime attempt.
use super::*;
use anyhow::{Context, Result, ensure};
use ato_formation::proposal::{Proposal, ProposalBatch, REASONING_BATCH_SCHEMA, SourceReference};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const INPUT_SCHEMA: &str = "ato.formation-reasoning-input/1";
pub const SESSION_RESPONSE_SCHEMA: &str = "ato.formation-session-response/1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningProviderConfig {
    Api(deepseek::DeepSeekConfig),
    Session(SessionConfig),
}
impl ReasoningProviderConfig {
    pub fn configuration_ref(&self, budget: &budget::BudgetPlan) -> Result<String> {
        match self {
            Self::Api(c) => c.configuration_ref(budget),
            Self::Session(c) => {
                ensure!(
                    c.provider == "codex_session"
                        && c.model == "codex-session"
                        && matches!(
                            c.prompt_version.as_str(),
                            deepseek::PROMPT_VERSION_V5
                                | deepseek::PROMPT_VERSION_V6
                                | deepseek::PROMPT_VERSION_V7
                                | deepseek::PROMPT_VERSION_V8
                                | deepseek::PROMPT_VERSION_V9
                                | deepseek::PROMPT_VERSION_V10
                                | deepseek::PROMPT_VERSION_V11
                                | deepseek::PROMPT_VERSION_V12
                                | deepseek::PROMPT_VERSION_V13
                        ),
                    "invalid session provider"
                );
                budget.validate()?;
                Ok(format!(
                    "sha256:{:x}",
                    Sha256::digest(serde_jcs::to_vec(&(
                        c,
                        budget,
                        deepseek::prompt_for(&c.prompt_version)
                            .context("session prompt missing")?
                    ))?)
                ))
            }
        }
    }
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }
    fn capabilities(
        &self,
        auth: &ato_formation::proposal::ProposalAuthorization,
        runtimes: &[Value],
    ) -> Value {
        let prompt_version = match self {
            Self::Session(c) => &c.prompt_version,
            Self::Api(c) => &c.prompt_version,
        };
        if matches!(
            prompt_version.as_str(),
            deepseek::PROMPT_VERSION_V8
                | deepseek::PROMPT_VERSION_V9
                | deepseek::PROMPT_VERSION_V10
                | deepseek::PROMPT_VERSION_V11
                | deepseek::PROMPT_VERSION_V12
                | deepseek::PROMPT_VERSION_V13
        ) {
            lowering_capabilities_for(auth, runtimes, prompt_version)
        } else {
            Value::Null
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicSource {
    pub reference: SourceReference,
    pub source_relative_path: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_from: Vec<SourceReference>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub archive_digest: String,
    pub closure_ref: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextOmission {
    pub logical_id: String,
    pub reason: String,
    pub acquired_bytes: usize,
    pub transmitted_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningInput {
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub lowering_capabilities: Value,
    #[serde(default)]
    pub catalog_sources_in_inventory: bool,
    pub goal: Option<String>,
    #[serde(default)]
    pub available_variables: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_capabilities: Vec<Value>,
    #[serde(default)]
    pub max_retries: u32,
    pub schema: String,
    pub call_id: String,
    pub frozen_contract_ref: String,
    pub source_identity: SourceIdentity,
    pub inventory: Vec<PublicSource>,
    pub request: ProposalRequestV2,
    pub projection: Vec<ContextOmission>,
    pub rounds_remaining: u32,
    pub calls_remaining: u32,
    pub inspections_remaining: u32,
    pub inspection_source_bytes_remaining: u64,
    pub inspection_feedback: Vec<String>,
}
impl ReasoningInput {
    fn validate(&self, auth: &ato_formation::proposal::ProposalAuthorization) -> Result<()> {
        ensure!(
            self.lowering_capabilities.is_null()
                || self.lowering_capabilities
                    == lowering_capabilities(auth, &self.runtime_capabilities)
                || self.lowering_capabilities
                    == lowering_capabilities_for(
                        auth,
                        &self.runtime_capabilities,
                        deepseek::PROMPT_VERSION_V8
                    )
                || self.lowering_capabilities
                    == lowering_capabilities_for(
                        auth,
                        &self.runtime_capabilities,
                        deepseek::PROMPT_VERSION_V9
                    )
                || self.lowering_capabilities
                    == lowering_capabilities_for(
                        auth,
                        &self.runtime_capabilities,
                        deepseek::PROMPT_VERSION_V10
                    )
                || self.lowering_capabilities
                    == lowering_capabilities_for(
                        auth,
                        &self.runtime_capabilities,
                        deepseek::PROMPT_VERSION_V11
                    )
                || self.lowering_capabilities
                    == lowering_capabilities_for(
                        auth,
                        &self.runtime_capabilities,
                        deepseek::PROMPT_VERSION_V12
                    ),
            "lowering capability mismatch"
        );
        let mut request = self.request.clone();
        if self.catalog_sources_in_inventory {
            for operation in &mut request.operation_catalog.operations {
                if let ato_formation::proposal::OperationDomain::ExecutionPlan { sources, .. } =
                    operation
                {
                    ensure!(sources.is_empty(), "duplicate common source catalog");
                    *sources = auth
                        .execution_plan
                        .as_ref()
                        .context("source catalog missing")?
                        .catalog_sources();
                }
            }
        }
        request.validate(auth)?;
        ensure!(
            self.inventory.iter().all(|s| auth
                .execution_plan
                .as_ref()
                .and_then(|p| p.files.get(&s.reference.file_id))
                .is_some_and(
                    |f| f.digest == s.reference.digest && f.path == s.source_relative_path
                )),
            "reasoning inventory source mismatch"
        );
        Ok(())
    }
}

/// Public compiler constraints, independent of application source or known D.
fn lowering_capabilities(
    auth: &ato_formation::proposal::ProposalAuthorization,
    runtimes: &[Value],
) -> Value {
    let recipe = auth
        .execution_plan
        .as_ref()
        .and_then(|a| a.source_oci.as_ref());
    let oci_bound = runtimes
        .iter()
        .filter(|r| r["availability"]["online"] == true && r["availability"]["health"] == "ok")
        .any(|r| {
            r["environments"].as_array().into_iter().flatten().any(|e| {
                e["facts"]["formation.source_oci.available"] == "true"
                    && e["facts"]["runtime.oci"] == "true"
            })
        });
    let configured = runtimes.iter().any(|r| {
        r["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| e["facts"]["formation.source_oci.bound"] == "true")
    });
    let images = recipe
        .into_iter()
        .flat_map(|r| r.base_images.iter())
        .filter(|image| {
            runtimes
                .iter()
                .filter(|r| {
                    r["availability"]["online"] == true && r["availability"]["health"] == "ok"
                })
                .any(|r| {
                    r["environments"].as_array().into_iter().flatten().any(|e| {
                        e["facts"]["runtime.oci"] == "true"
                            && e["facts"][format!(
                                "formation.oci.image.{}",
                                image.pinned_digest.trim_start_matches("sha256:")
                            )] == "true"
                    })
                })
        })
        .map(|image| {
            if image.reference.contains('@') {
                image.reference.clone()
            } else {
                format!("{}@{}", image.reference, image.pinned_digest)
            }
        })
        .collect::<Vec<_>>();
    let toolchains = auth.execution_plan.as_ref().map(|a| &a.toolchains);
    let bound_toolchains = toolchains
        .into_iter()
        .flatten()
        .map(|(name, version)| {
            let fact = format!("toolchain.{name}.{version}");
            let available = runtimes
                .iter()
                .filter(|r| {
                    r["availability"]["online"] == true && r["availability"]["health"] == "ok"
                })
                .any(|r| {
                    r["environments"].as_array().into_iter().flatten().any(|e| {
                        e["facts"][&fact] == "present" && e["facts"]["runtime.process"] == "true"
                    })
                });
            json!({"name":name,"version":version,"bound":available})
        })
        .collect::<Vec<_>>();
    let python_bound = bound_toolchains
        .iter()
        .any(|t| t["name"] == "python" && t["bound"] == true);
    let node_bound = bound_toolchains
        .iter()
        .any(|t| t["name"] == "node" && t["bound"] == true);
    let http_operations_bound = auth
        .execution_plan
        .as_ref()
        .is_some_and(|a| a.runtime_port_operations)
        && runtimes
            .iter()
            .filter(|r| r["availability"]["online"] == true && r["availability"]["health"] == "ok")
            .any(|r| {
                r["environments"].as_array().into_iter().flatten().any(|e| {
                    e["facts"][ato_formation::port_operations::RUNTIME_CAPABILITY] == "true"
                        && e["facts"]["runtime.process"] == "true"
                })
            });
    json!({"schema":"ato.formation-lowering-capabilities/5",
        "runtime_port_operations":{"available":http_operations_bound,"routes":["node","python"],"max_operations":4,"methods":["GET","POST"],"input":"declared Runtime variable references with artifact_embedding:false; current scoped grants only", "target":"assigned frozen logical Port, relative path; no host/query/headers/response capture", "authority":"runtime ato.http@1 execute on the same logical Port in addition to bind", "when":"optional explicit source-backed GET/status guard", "no_automatic_replay":true,"limits":"same original monotonic execution deadline; bounded body and status line"},
        "static_http":{
            "available":toolchains.is_some_and(|t| t.contains_key("node") && t.contains_key("npm")),
            "entrypoint":"source manifest package.json reference",
            "static_output":"source-declared relative output directory",
            "argv":[],"guest_port":0,
            "omit_fields":["module","environment","state"],"variables":"dependency/build-phase private grants; runtime phase unavailable; public artifact bytes require owner embedding permission",
            "build_scripts":"source-owned npm script names; never commands or process argv"
        },
        "oci_image":{"available":!images.is_empty(),"proposal_field":"oci_image","images":images,"platform":recipe.map(|r|&r.platform),"command":"image default","acquisition_network":"none; approved verified archive binding","runtime_network":"internal network; no egress","variables":"runtime-phase grants only; no values in D or image"},
        "source_oci":{"supported":true,"available":recipe.is_some() && oci_bound,"configured":configured,"builder_bound":oci_bound,
            "unavailable_reason":if !configured {"source_oci_builder_unavailable"} else if !oci_bound {"runtime_unavailable"} else {"none"},
            "selection":"root Dockerfile or literal reference from acquired root declarations",
            "recipe":recipe,"single_service":true,"source_rewrite":false,
            "build_network":"explicit HTTPS host/port allowance within frozen ceiling",
            "runtime_network":"isolated internal network; no egress", "state":"explicit isolated writable VOLUME bindings", "variables":"runtime-phase grants only; dependencies/build grants unavailable; no values in D or image"},
        "http_process":{"guest_port":"1..65535","entrypoint":"actual supported source script reference, or frozen package.json for launch_script","argv":"literal arguments after the interpreter/entrypoint or npm run script","launch_script":"source-owned npm script key; frozen manifest hash checked before launch","setup_scripts":"up to four unique source-owned npm script keys; Node launch_script only; execute inside the same Runtime after state/private grants, before launch, under its network/resources/deadline"},
        "network":{"phases":["dependencies","build","runtime"],"inbound_HTTP_requires_egress":false,"within_frozen_ceiling":true},
        "HTTP_authority":{"protocol":"ato.http@1","operation":"bind","phase":"runtime","resource":"frozen K logical HTTP Port"},
        "native_dependencies":{"supported":true,"toolchains":bound_toolchains,
            "python":{"available":python_bound,"operation":"python_build_requirements","build_dependencies":"exact name/version wheels; source requirements ref","sdist_build":"dedicated build environment in Runtime sandbox","metadata_network":"dependencies","build_network":["denied","scoped_build"],"install":"offline hashed completed wheels"},
            "node":{"available":node_bound && toolchains.is_some_and(|t|t.contains_key("npm")),"operation":"npm_rebuild","after":"npm_ci","packages":"source/lock-owned explicit names","root_lifecycle":"explicit source-owned names","node_gyp_tools":["python","gcc","make"],"headers":"bound Node distribution include/node","ignore_scripts_is_runnable":false},
            "limits":"same frozen Runtime execution deadline and contained build resource limits; no ambient toolchain fallback"}
    })
}

fn lowering_capabilities_for(
    auth: &ato_formation::proposal::ProposalAuthorization,
    runtimes: &[Value],
    prompt_version: &str,
) -> Value {
    let mut capabilities = lowering_capabilities(auth, runtimes);
    if prompt_version != deepseek::PROMPT_VERSION_V13 {
        capabilities["schema"] = json!("ato.formation-lowering-capabilities/4");
        capabilities
            .as_object_mut()
            .expect("capability object")
            .remove("runtime_port_operations");
    }
    if !matches!(
        prompt_version,
        deepseek::PROMPT_VERSION_V12 | deepseek::PROMPT_VERSION_V13
    ) {
        capabilities["schema"] = json!("ato.formation-lowering-capabilities/3");
        capabilities["http_process"]
            .as_object_mut()
            .expect("process capability object")
            .remove("setup_scripts");
    }
    if matches!(
        prompt_version,
        deepseek::PROMPT_VERSION_V8 | deepseek::PROMPT_VERSION_V9 | deepseek::PROMPT_VERSION_V10
    ) {
        capabilities["schema"] = json!("ato.formation-lowering-capabilities/2");
        capabilities["static_http"]["omit_fields"] =
            json!(["module", "environment", "state", "variable_bindings"]);
        capabilities["static_http"]
            .as_object_mut()
            .expect("static capability object")
            .remove("variables");
        capabilities["source_oci"]["variables"] = json!("not yet supported by source OCI lowering");
        capabilities["oci_image"]
            .as_object_mut()
            .expect("OCI capability object")
            .remove("variables");
    }
    if matches!(
        prompt_version,
        deepseek::PROMPT_VERSION_V8 | deepseek::PROMPT_VERSION_V9
    ) {
        let object = capabilities.as_object_mut().expect("capability object");
        object.insert(
            "schema".into(),
            json!("ato.formation-lowering-capabilities/1"),
        );
        object.remove("native_dependencies");
        object.insert("http_process".into(), json!({"guest_port":"1..65535","entrypoint":"actual supported source script reference","argv":"literal arguments after the interpreter and entrypoint"}));
        object.insert(
            "unsupported_dependency_operations".into(),
            json!([
                "direct npm lifecycle/rebuild dependency lowering",
                "Python source distribution/native dependency lowering"
            ]),
        );
        if prompt_version == deepseek::PROMPT_VERSION_V8 {
            object.remove("source_oci");
            object.remove("oci_image");
        }
    }
    capabilities
}

/// Inference discovery is rooted in acquired configuration, never in the
/// presence of a directory. COPY . . and broad globs confer no read authority.
#[cfg(test)]
fn scoped_inventory(
    domain: &ato_formation::proposal::PlanAuthorization,
    acquired: &BTreeMap<String, SourceContextEntry>,
) -> Result<Vec<PublicSource>> {
    scoped_inventory_with_feedback(domain, acquired).map(|(inventory, _)| inventory)
}

fn scoped_inventory_with_feedback(
    domain: &ato_formation::proposal::PlanAuthorization,
    acquired: &BTreeMap<String, SourceContextEntry>,
) -> Result<(Vec<PublicSource>, Vec<String>)> {
    let mut feedback = Vec::new();
    let mut allowed = BTreeMap::<String, Vec<SourceReference>>::new();
    for (id, file) in &domain.files {
        if ato_formation::proposal::is_discovery_root(&file.path) {
            allowed.insert(id.clone(), vec![]);
        }
    }
    for (id, text) in acquired {
        let origin = domain.files.get(id).context("scope source missing")?;
        let parent = origin.path.rsplit_once('/').map_or("", |(p, _)| p);
        // An explicit root Python import names a module, not an invitation to
        // crawl its package. Expose only the module file or package initializer.
        if parent.is_empty() && origin.path.ends_with(".py") {
            for line in text.text.lines() {
                let line = line.trim();
                let modules: Vec<_> = if let Some(rest) = line.strip_prefix("from ") {
                    rest.split_once(" import ")
                        .map(|(m, _)| vec![m])
                        .unwrap_or_default()
                } else if let Some(rest) = line.strip_prefix("import ") {
                    rest.split(',')
                        .filter_map(|m| m.split_whitespace().next())
                        .collect()
                } else {
                    vec![]
                };
                for module in modules {
                    if module.len() > 128
                        || !module.split('.').all(|p| {
                            !p.is_empty()
                                && p.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                        })
                    {
                        continue;
                    }
                    let module = module.replace('.', "/");
                    for path in [format!("{module}.py"), format!("{module}/__init__.py")] {
                        for (target, _) in domain.files.iter().filter(|(_, f)| f.path == path) {
                            let basis = allowed.entry(target.clone()).or_default();
                            let reference = SourceReference {
                                file_id: id.clone(),
                                digest: origin.digest.clone(),
                            };
                            if !basis.contains(&reference) {
                                basis.push(reference);
                            }
                        }
                    }
                }
            }
        }
        // Only literal relative JS imports extend inspection. Resolve exact files,
        // never crawl a directory or infer dynamic require() expressions.
        if origin.path.ends_with(".js")
            || origin.path.ends_with(".mjs")
            || origin.path.ends_with(".cjs")
        {
            for line in text.text.lines() {
                let parts: Vec<_> = line.split(['\'', '"']).collect();
                for pair in parts.windows(2).step_by(2) {
                    let prefix = pair[0].trim_end();
                    let reference = pair[1];
                    if !(prefix.ends_with("require(")
                        || prefix.ends_with("from")
                        || prefix.ends_with("import"))
                        || !(reference.starts_with("./") || reference.starts_with("../"))
                        || reference.len() > 256
                        || reference.contains(['*', '\\'])
                    {
                        continue;
                    }
                    let mut segments: Vec<_> =
                        parent.split('/').filter(|s| !s.is_empty()).collect();
                    let mut valid = true;
                    for segment in reference.split('/') {
                        match segment {
                            "." | "" => {}
                            ".." => {
                                if segments.pop().is_none() {
                                    valid = false;
                                    break;
                                }
                            }
                            s => segments.push(s),
                        }
                    }
                    if !valid {
                        feedback.push(format!("source_reference_outside_root: {}", origin.path));
                        continue;
                    }
                    let path = segments.join("/");
                    let candidates = [
                        path.clone(),
                        format!("{path}.js"),
                        format!("{path}.json"),
                        format!("{path}/index.js"),
                    ];
                    let matched: Vec<_> = domain
                        .files
                        .iter()
                        .filter(|(_, f)| candidates.contains(&f.path))
                        .collect();
                    if matched.len() > 1 {
                        feedback.push(format!(
                            "source_reference_ambiguous: {} references {reference}",
                            origin.path
                        ));
                        continue;
                    }
                    for (target, _) in matched {
                        let basis = allowed.entry(target.clone()).or_default();
                        let r = SourceReference {
                            file_id: id.clone(),
                            digest: origin.digest.clone(),
                        };
                        if !basis.contains(&r) {
                            basis.push(r);
                        }
                    }
                }
            }
        }
        for token in text
            .text
            .split(|c: char| !c.is_ascii_alphanumeric() && !"._-@/*".contains(c))
        {
            let token = token
                .strip_prefix("./")
                .unwrap_or(token)
                .trim_end_matches('/');
            if token.is_empty() || token == "." || token.contains("..") || token.starts_with('/') {
                continue;
            }
            if token.contains("**") {
                if token.contains('/') {
                    feedback.push(format!(
                        "unsupported_source_glob: {} references {token}; recursive expansion is unavailable in v0", origin.path
                    ));
                }
                continue;
            }
            let path = if parent.is_empty() {
                token.to_owned()
            } else {
                format!("{parent}/{token}")
            };
            let matched: Vec<_> = domain
                .files
                .iter()
                .filter(|(_, f)| {
                    if let Some((prefix, suffix)) = path.split_once('*') {
                        // v0: one segment glob, explicit parent; never an unbounded crawl.
                        let one_segment = |candidate: &str| {
                            candidate
                                .strip_prefix(prefix)
                                .and_then(|s| s.strip_suffix(suffix))
                                .is_some_and(|s| !s.is_empty() && !s.contains('/'))
                        };
                        !prefix.is_empty()
                            && prefix.contains('/')
                            && !suffix.contains('*')
                            && (one_segment(&f.path)
                                || f.path.rsplit_once('/').is_some_and(|(directory, name)| {
                                    ato_formation::proposal::source_inspection_priority(name).0 <= 5
                                        && one_segment(directory)
                                }))
                    } else {
                        f.path == path
                            || token.contains('/')
                                && f.path.strip_prefix(&format!("{path}/")).is_some_and(|s| {
                                    !s.contains('/')
                                        && ato_formation::proposal::source_inspection_priority(s).0
                                            <= 5
                                })
                    }
                })
                .collect();
            if matched.len() > 32 {
                feedback.push(format!(
                    "source_reference_expansion_limit: {} references {token}; {} files exceed the32-file bound; no targets admitted", origin.path, matched.len()
                ));
                continue;
            }
            for (target, _) in matched {
                // Root files were already in scope; do not duplicate their
                // references as if another root file had widened the boundary.
                if ato_formation::proposal::is_discovery_root(&domain.files[target].path) {
                    continue;
                }
                let basis = allowed.entry(target.clone()).or_default();
                let reference = SourceReference {
                    file_id: id.clone(),
                    digest: origin.digest.clone(),
                };
                if !basis.contains(&reference) {
                    basis.push(reference);
                }
            }
        }
    }
    let mut files: Vec<_> = allowed.into_iter().collect();
    files.sort_by_key(|(id, _)| {
        (
            domain.files[id].path.contains('/'),
            ato_formation::proposal::source_inspection_priority(&domain.files[id].path),
            id.clone(),
        )
    });
    if files.len() > 128 {
        feedback.push(format!("source_inventory_limit: {} scoped files exceed128; inventory projection omits {} files; do not guess omitted targets", files.len(), files.len()-128));
        files.truncate(128);
    }
    feedback.sort();
    feedback.dedup();
    feedback.truncate(32);
    Ok((
        files
            .into_iter()
            .map(|(id, discovered_from)| {
                let file = &domain.files[&id];
                PublicSource {
                    reference: SourceReference {
                        file_id: id,
                        digest: file.digest.clone(),
                    },
                    source_relative_path: file.path.clone(),
                    purpose: "source".into(),
                    discovered_from,
                }
            })
            .collect(),
        feedback,
    ))
}

fn priority_context(mut entries: Vec<SourceContextEntry>, cap: usize) -> Vec<SourceContextEntry> {
    let mut remaining = cap;
    for entry in &mut entries {
        let mut end = entry
            .text
            .len()
            .min(ato_formation::proposal::MAX_SOURCE_ENTRY_BYTES)
            .min(remaining);
        while !entry.text.is_char_boundary(end) {
            end -= 1;
        }
        entry.truncated |= end != entry.text.len();
        entry.text.truncate(end);
        entry.content_sha256 = digest(entry.text.as_bytes());
        remaining -= end;
    }
    entries.retain(|e| !e.text.is_empty());
    entries.sort_by(|a, b| (a.kind, &a.logical_id).cmp(&(b.kind, &b.logical_id)));
    entries
}

#[cfg(test)]
mod autonomous_tests {
    use super::*;
    #[test]
    fn http_port_capability_requires_current_bound_runtime_and_preserves_old_prompts() {
        let mut auth:ato_formation::proposal::ProposalAuthorization=serde_json::from_value(json!({
            "execution_plan":{"files":{},"toolchains":{"python":"3.12.7"},"runtime_port_operations":true},
            "modifiable_derivation_refs":[],"source_domain":{"entrypoints":{},"modules":{}},
            "policy":{"max_proposal_rounds":3,"max_proposals":1,"timeout_ms":30000,"allow_source_text":true,"max_source_bytes":16384}
        })).unwrap();
        assert_eq!(
            lowering_capabilities(&auth, &[])["runtime_port_operations"]["available"],
            false
        );
        let runtime = json!({"availability":{"online":true,"health":"ok"},"environments":[{"facts":{"runtime.process":"true",ato_formation::port_operations::RUNTIME_CAPABILITY:"true"}}]});
        assert_eq!(
            lowering_capabilities(&auth, std::slice::from_ref(&runtime))["runtime_port_operations"]
                ["available"],
            true
        );
        let old = lowering_capabilities_for(
            &auth,
            std::slice::from_ref(&runtime),
            deepseek::PROMPT_VERSION_V12,
        );
        assert_eq!(old["schema"], "ato.formation-lowering-capabilities/4");
        assert!(old.get("runtime_port_operations").is_none());
        assert!(old["http_process"]["setup_scripts"].is_string());
        let mut offline = runtime.clone();
        offline["availability"]["online"] = json!(false);
        assert_eq!(
            lowering_capabilities(&auth, &[offline])["runtime_port_operations"]["available"],
            false
        );
        auth.execution_plan
            .as_mut()
            .unwrap()
            .runtime_port_operations = false;
        assert_eq!(
            lowering_capabilities(&auth, &[runtime])["runtime_port_operations"]["available"],
            false
        );
    }
    #[test]
    fn lowering_capabilities_are_source_independent_and_toolchain_scoped() {
        use ato_formation::proposal::{PlanAuthorization, ProposalAuthorization};
        let mut auth: ProposalAuthorization=serde_json::from_value(json!({
            "execution_plan":{"files":{},"toolchains":{"node":"22.14.0","npm":"10.9.2"}},
            "modifiable_derivation_refs":[],"source_domain":{"entrypoints":{},"modules":{}},
            "policy":{"max_proposal_rounds":3,"max_proposals":1,"timeout_ms":30000,"allow_source_text":true,"max_source_bytes":16384}
        })).unwrap();
        ReasoningProviderConfig::Session(SessionConfig {
            provider: "codex_session".into(),
            model: "codex-session".into(),
            prompt_version: deepseek::PROMPT_VERSION_V8.into(),
        })
        .configuration_ref(&budget::BudgetPlan {
            max_calls: 6,
            input_token_cap: 49152,
            output_token_cap: 2048,
            input_price: 300000,
            output_price: 1200000,
            ceiling_usd_micros: 103224,
        })
        .unwrap();
        let caps = lowering_capabilities(&auth, &[]);
        assert_eq!(caps["static_http"]["available"], true);
        assert_eq!(caps["static_http"]["guest_port"], 0);
        assert_eq!(caps["network"]["inbound_HTTP_requires_egress"], false);
        assert_eq!(caps["schema"], "ato.formation-lowering-capabilities/5");
        assert!(caps["http_process"]["setup_scripts"].is_string());
        let legacy = lowering_capabilities_for(&auth, &[], deepseek::PROMPT_VERSION_V11);
        assert_eq!(legacy["schema"], "ato.formation-lowering-capabilities/3");
        assert!(legacy["http_process"].get("setup_scripts").is_none());
        assert!(
            !serde_json::to_string(&caps)
                .unwrap()
                .contains("derivation_ref")
        );
        auth.execution_plan = Some(PlanAuthorization {
            runtime_port_operations: false,
            files: BTreeMap::new(),
            toolchains: BTreeMap::from([("python".into(), "3.12.7".into())]),
            source_oci: None,
        });
        assert_eq!(
            lowering_capabilities(&auth, &[])["static_http"]["available"],
            false
        );
    }
    #[test]
    fn oci_capabilities_distinguish_unbound_unavailable_and_executable_images() {
        let mut auth:ato_formation::proposal::ProposalAuthorization=serde_json::from_value(json!({
            "execution_plan":{"files":{},"toolchains":{"oci":"1.0.0"}},"modifiable_derivation_refs":[],"source_domain":{"entrypoints":{},"modules":{}},
            "policy":{"max_proposal_rounds":3,"max_proposals":1,"timeout_ms":30000,"allow_source_text":true,"max_source_bytes":16384}
        })).unwrap();
        assert_eq!(
            lowering_capabilities(&auth, &[])["source_oci"]["available"],
            false
        );
        auth.execution_plan.as_mut().unwrap().source_oci=Some(serde_json::from_value(json!({"schema":"ato.source-oci-recipe/1","dockerfile":"Dockerfile","platform":"linux/arm64","base_images":[{"reference":"example/app:1","pinned_digest":format!("sha256:{}","e".repeat(64))}],"build":{"memory_bytes":536870912,"cpu_limit_millis":1000,"pids_limit":128},"build_disk_bytes":536870912,"runtime":{"memory_bytes":268435456,"cpu_limit_millis":1000,"pids_limit":128},"build_timeout_seconds":60,"max_archive_bytes":1048576})).unwrap());
        let image_key = format!("formation.oci.image.{}", "e".repeat(64));
        let mut runtime = json!({"availability":{"online":true,"health":"ok"},"environments":[{"facts":{"formation.source_oci.bound":"true","formation.source_oci.available":"false","runtime.oci":"false"}}]});
        runtime["environments"][0]["facts"][&image_key] = json!("true");
        let caps = lowering_capabilities(&auth, std::slice::from_ref(&runtime));
        assert_eq!(
            caps["source_oci"]["unavailable_reason"],
            "runtime_unavailable"
        );
        assert_eq!(caps["oci_image"]["available"], false);
        runtime["environments"][0]["facts"]["runtime.oci"] = json!("true");
        let caps = lowering_capabilities(&auth, &[runtime]);
        assert_eq!(caps["source_oci"]["available"], false);
        assert_eq!(caps["oci_image"]["available"], true);
        assert_eq!(
            caps["oci_image"]["images"][0],
            format!("example/app:1@sha256:{}", "e".repeat(64))
        );
    }
    #[test]
    fn native_capabilities_require_bound_tools_and_preserve_legacy_prompt_views() {
        let auth:ato_formation::proposal::ProposalAuthorization=serde_json::from_value(json!({
            "execution_plan":{"files":{},"toolchains":{"python":"3.12.7","gcc":"13.3.0"}},"modifiable_derivation_refs":[],"source_domain":{"entrypoints":{},"modules":{}},
            "policy":{"max_proposal_rounds":3,"max_proposals":1,"timeout_ms":30000,"allow_source_text":true,"max_source_bytes":16384}
        })).unwrap();
        let caps = lowering_capabilities(&auth, &[]);
        assert_eq!(caps["native_dependencies"]["python"]["available"], false);
        let runtime = json!({"availability":{"online":true,"health":"ok"},"environments":[{"facts":{"runtime.process":"true","toolchain.python.3.12.7":"present"}}]});
        let caps = lowering_capabilities(&auth, &[runtime]);
        assert_eq!(caps["native_dependencies"]["python"]["available"], true);
        assert!(
            caps["native_dependencies"]["toolchains"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "gcc" && t["bound"] == false)
        );
        let legacy = lowering_capabilities_for(&auth, &[], deepseek::PROMPT_VERSION_V8);
        assert_eq!(legacy["schema"], "ato.formation-lowering-capabilities/1");
        assert!(legacy.get("native_dependencies").is_none() && legacy.get("oci_image").is_none());
        assert!(legacy["unsupported_dependency_operations"].is_array());
        let oci = lowering_capabilities_for(&auth, &[], deepseek::PROMPT_VERSION_V9);
        assert!(oci.get("oci_image").is_some() && oci.get("native_dependencies").is_none());
    }
    #[test]
    fn root_scope_requires_an_acquired_reference_and_preserves_its_digest() {
        use ato_formation::proposal::{PlanAuthorization, VerifiedSourceFile};
        let domain = PlanAuthorization {
            runtime_port_operations: false,
            source_oci: None,
            toolchains: BTreeMap::new(),
            files: [
                "package.json",
                "README.md",
                "src/server.js",
                "unrelated/main.py",
                "server/index.js",
            ]
            .into_iter()
            .map(|p| {
                (
                    p.into(),
                    VerifiedSourceFile {
                        path: p.into(),
                        digest: format!("sha256:{}", "a".repeat(64)),
                    },
                )
            })
            .collect(),
        };
        let root = scoped_inventory(&domain, &BTreeMap::new()).unwrap();
        assert_eq!(root.len(), 2);
        let entry = SourceContextEntry {
            kind: SourceContextKind::VerifiedFile,
            logical_id: "package.json".into(),
            text: "{\"main\":\"src/server.js\"} COPY . . This server is useful".into(),
            source_id: "package.json".into(),
            encoding: ato_formation::proposal::SourceEncoding::Utf8,
            content_sha256: "unused".into(),
            truncated: false,
        };
        let scoped =
            scoped_inventory(&domain, &BTreeMap::from([("package.json".into(), entry)])).unwrap();
        assert_eq!(scoped.len(), 3);
        assert!(
            !scoped
                .iter()
                .any(|s| s.source_relative_path.starts_with("unrelated/"))
        );
        assert_eq!(
            scoped
                .iter()
                .find(|s| s.source_relative_path == "src/server.js")
                .unwrap()
                .discovered_from[0]
                .file_id,
            "package.json"
        );
    }
    #[test]
    fn explicit_workspace_globs_are_bounded_and_recursive_refs_are_reported() {
        use ato_formation::proposal::{PlanAuthorization, VerifiedSourceFile};
        let paths = std::iter::once("package.json".to_owned())
            .chain((0..33).map(|i| format!("packages/p{i}/package.json")))
            .chain([
                "packages/one/deep/hidden.js".to_owned(),
                "unrelated/package.json".to_owned(),
            ]);
        let mut domain = PlanAuthorization {
            runtime_port_operations: false,
            source_oci: None,
            toolchains: BTreeMap::new(),
            files: paths
                .map(|path| {
                    (
                        path.clone(),
                        VerifiedSourceFile {
                            path,
                            digest: format!("sha256:{}", "a".repeat(64)),
                        },
                    )
                })
                .collect(),
        };
        let context = BTreeMap::from([(
            "package.json".into(),
            SourceContextEntry {
                kind: SourceContextKind::VerifiedFile,
                logical_id: "package.json".into(),
                text: r#"{"workspaces":["packages/*"],"config":"packages/**/config.json"}"#.into(),
                source_id: "package.json".into(),
                encoding: ato_formation::proposal::SourceEncoding::Utf8,
                content_sha256: "unused".into(),
                truncated: false,
            },
        )]);
        let (inventory, feedback) = scoped_inventory_with_feedback(&domain, &context).unwrap();
        assert_eq!(
            inventory.len(),
            1,
            "overflow must not silently choose a package"
        );
        assert!(feedback.iter().any(|s| s.contains("33 files exceed")));
        assert!(
            feedback
                .iter()
                .any(|s| s.contains("unsupported_source_glob"))
        );
        domain.files.remove("packages/p32/package.json");
        let (inventory, _) = scoped_inventory_with_feedback(&domain, &context).unwrap();
        assert_eq!(inventory.len(), 33);
        assert!(
            !inventory
                .iter()
                .any(|s| s.source_relative_path.contains("hidden")
                    || s.source_relative_path.starts_with("unrelated"))
        );
        assert!(
            inventory
                .iter()
                .filter(|s| s.source_relative_path.contains('/'))
                .all(|s| s.discovered_from[0].file_id == "package.json")
        );
    }
    #[test]
    fn explicit_python_import_exposes_only_module_or_initializer() {
        use ato_formation::proposal::{PlanAuthorization, VerifiedSourceFile};
        let domain = PlanAuthorization {
            runtime_port_operations: false,
            source_oci: None,
            toolchains: BTreeMap::new(),
            files: [
                "main.py",
                "README.md",
                "service/__init__.py",
                "service/hidden.py",
                "other/__init__.py",
            ]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    VerifiedSourceFile {
                        path: path.into(),
                        digest: format!("sha256:{}", "a".repeat(64)),
                    },
                )
            })
            .collect(),
        };
        let entry = SourceContextEntry {
            kind: SourceContextKind::VerifiedFile,
            logical_id: "main.py".into(),
            source_id: "main.py".into(),
            encoding: ato_formation::proposal::SourceEncoding::Utf8,
            content_sha256: "unused".into(),
            truncated: false,
            text: "from service import main\nimport os\n# other directory exists\n".into(),
        };
        assert_eq!(
            scoped_inventory(&domain, &BTreeMap::new())
                .unwrap()
                .iter()
                .map(|f| f.source_relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["README.md"]
        );
        let mut readme = entry.clone();
        readme.logical_id = "README.md".into();
        readme.source_id = "README.md".into();
        readme.text = "Run python main.py".into();
        let inventory = scoped_inventory(
            &domain,
            &BTreeMap::from([("README.md".into(), readme), ("main.py".into(), entry)]),
        )
        .unwrap();
        assert_eq!(inventory.len(), 3);
        let module = inventory
            .iter()
            .find(|f| f.source_relative_path == "service/__init__.py")
            .unwrap();
        assert_eq!(module.discovered_from[0].file_id, "main.py");
        assert_eq!(
            module.discovered_from[0].digest,
            domain.files["main.py"].digest
        );
        assert!(
            inventory
                .iter()
                .filter(|f| f.source_relative_path == "README.md")
                .all(|f| f.discovered_from.is_empty())
        );
    }
    #[test]
    fn relative_js_imports_use_exact_bounded_files_and_reject_ambiguity() {
        use ato_formation::proposal::{PlanAuthorization, VerifiedSourceFile};
        let mut domain = PlanAuthorization {
            runtime_port_operations: false,
            source_oci: None,
            toolchains: BTreeMap::new(),
            files: [
                "package.json",
                "server/main.js",
                "server/config.js",
                "server/private/hidden.js",
                "shared/settings.json",
            ]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    VerifiedSourceFile {
                        path: path.into(),
                        digest: format!("sha256:{}", "a".repeat(64)),
                    },
                )
            })
            .collect(),
        };
        let acquired=BTreeMap::from([("server/main.js".into(),SourceContextEntry {
            kind:SourceContextKind::VerifiedFile, logical_id:"server/main.js".into(), source_id:"server/main.js".into(),
            encoding:ato_formation::proposal::SourceEncoding::Utf8, content_sha256:"unused".into(),truncated:false,
            text:"const cfg = require(\"./config\");\nimport settings from '../shared/settings.json';\nrequire('../../outside');\nrequire(name);".into()
        })]);
        let (inventory, feedback) = scoped_inventory_with_feedback(&domain, &acquired).unwrap();
        for path in ["server/config.js", "shared/settings.json"] {
            let file = inventory
                .iter()
                .find(|s| s.source_relative_path == path)
                .unwrap();
            assert_eq!(file.discovered_from[0].file_id, "server/main.js");
        }
        assert!(
            feedback
                .iter()
                .any(|s| s.starts_with("source_reference_outside_root"))
        );
        assert!(
            !inventory
                .iter()
                .any(|s| s.source_relative_path.contains("hidden"))
        );
        domain.files.insert(
            "server/config/index.js".into(),
            VerifiedSourceFile {
                path: "server/config/index.js".into(),
                digest: format!("sha256:{}", "b".repeat(64)),
            },
        );
        let (inventory, feedback) = scoped_inventory_with_feedback(&domain, &acquired).unwrap();
        assert!(
            !inventory
                .iter()
                .any(|s| s.source_relative_path == "server/config.js")
        );
        assert!(
            feedback
                .iter()
                .any(|s| s.starts_with("source_reference_ambiguous"))
        );
    }
    #[test]
    fn exchange_call_bound_is_frozen_separately_from_rounds() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let dir = tempfile::tempdir_in(".tmp")?;
        let plan = budget::BudgetPlan {
            max_calls: 12,
            input_token_cap: 49152,
            output_token_cap: 2048,
            input_price: 300000,
            output_price: 1200000,
            ceiling_usd_micros: 206448,
        };
        let path = dir.path().join("budget.jsonl");
        let calls = budget::CallBudget::create(&path, plan.clone())?;
        for call in 0..12 {
            let cell = format!("call{call}");
            calls.reserve(&cell)?;
            calls.settle_retry(&cell, false)?;
        }
        assert!(calls.reserve("call12").is_err());
        let resumed = budget::CallBudget::reopen(&path, plan.clone())?;
        assert!(resumed.reserve("new-name").is_err());
        let mut changed = plan;
        changed.max_calls = 13;
        changed.ceiling_usd_micros = 300000;
        assert!(budget::CallBudget::reopen(&path, changed).is_err());
        Ok(())
    }
    #[test]
    fn six_saved_exchanges_refuse_a_new_round_without_a_provider_call_on_restart() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let config = ReasoningProviderConfig::Session(SessionConfig {
            provider: "codex_session".into(),
            model: "codex-session".into(),
            prompt_version: deepseek::PROMPT_VERSION_V13.into(),
        });
        let plan = budget::BudgetPlan {
            max_calls: 6, input_token_cap: 49152, output_token_cap: 2048,
            input_price: 300000, output_price: 1200000, ceiling_usd_micros: 103224,
        };
        let producer = ReasoningProducer::new(config.clone(), plan.clone(), root.path().into(), None)?;
        for exchange in 1..=6 {
            let path = root.path().join(format!("r001_s{exchange:03}.input.json"));
            assert_eq!(producer.check_exchange_budget(&path)?, exchange - 1);
            std::fs::write(path, b"saved input")?;
        }
        // Saved input recovery remains possible; creating another exchange does not.
        assert_eq!(producer.check_exchange_budget(&root.path().join("r001_s006.input.json"))?, 6);
        let next = root.path().join("r002_s001.input.json");
        assert!(producer.check_exchange_budget(&next).unwrap_err().is::<ReasoningCallBudgetExhausted>());
        drop(producer);
        let restored = ReasoningProducer::new(config, plan, root.path().into(), None)?;
        assert!(restored.check_exchange_budget(&next).unwrap_err().is::<ReasoningCallBudgetExhausted>());
        assert!(!next.exists());
        assert!(!root.path().join("r002_s001.dispatch.json").exists());
        Ok(())
    }
    #[test]
    fn operation_retries_are_initial_plus_three_and_survive_restart() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let config = ReasoningProviderConfig::Session(SessionConfig {
            provider: "codex_session".into(),
            model: "codex-session".into(),
            prompt_version: deepseek::PROMPT_VERSION_V5.into(),
        });
        let budget = budget::BudgetPlan {
            max_calls: 6,
            input_token_cap: 1024,
            output_token_cap: 1024,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 12,
        };
        let p = ReasoningProducer::new(config.clone(), budget.clone(), root.path().into(), None)?;
        let expires = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64 + 10_000;
        let mut count = 0;
        assert!(
            p.coordinator_operation(1, "claim", b"same", 3, expires, || {
                count += 1;
                anyhow::bail!("coordinator answered 503")
            })
            .is_err()
        );
        assert_eq!(count, 4);
        let p = ReasoningProducer::new(config, budget, root.path().into(), None)?;
        assert!(
            p.coordinator_operation(1, "claim", b"same", 3, expires, || {
                count += 1;
                Ok(json!({"fence":"never"}))
            })
            .is_err()
        );
        assert_eq!(count, 4);
        let result = p.coordinator_operation(2, "complete", b"fixed", 3, expires, || {
            Ok(json!({"revision":2}))
        })?;
        assert_eq!(
            result,
            p.coordinator_operation(2, "complete", b"fixed", 3, expires, || panic!(
                "completed operation redispatched"
            ))?
        );
        assert!(
            p.coordinator_operation(2, "complete", b"changed", 3, expires, || panic!(
                "operation renamed"
            ))
            .is_err()
        );
        assert!(
            p.coordinator_operation(1, "claim", b"same", 0, expires, || panic!("policy reset"))
                .is_err()
        );
        assert!(
            p.coordinator_operation(3, "claim", b"expired", 0, 0, || panic!("expired inference"))
                .is_err()
        );
        p.coordinator_operation(4, "complete", b"report", 0, 0, || {
            Ok(json!({"expired_result_reported":true}))
        })?;
        Ok(())
    }
    fn observation_producer(directory: &Path) -> Result<ReasoningProducer> {
        ReasoningProducer::new(
            ReasoningProviderConfig::Session(SessionConfig {
                provider: "codex_session".into(),
                model: "codex-session".into(),
                prompt_version: deepseek::PROMPT_VERSION_V5.into(),
            }),
            budget::BudgetPlan {
                max_calls: 6,
                input_token_cap: 1024,
                output_token_cap: 1024,
                input_price: 1,
                output_price: 1,
                ceiling_usd_micros: 12,
            },
            directory.into(),
            None,
        )
    }
    #[test]
    fn observations_retry_dropped_http_and_read_fresh_after_restart() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let server = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/status", server.local_addr()?);
        let serving = std::thread::spawn(move || -> std::io::Result<()> {
            // Drop the first response, then return different persisted states.
            for snapshot in [None, Some("running"), Some("pass")] {
                let (mut stream, _) = server.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte)?;
                    request.push(byte[0]);
                    assert!(request.len() <= 4096, "HTTP fixture request bounds");
                }
                if let Some(status) = snapshot {
                    let body = format!(r#"{{"status":"{status}"}}"#);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )?;
                }
            }
            Ok(())
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        let p = observation_producer(root.path())?;
        assert_eq!(
            p.coordinator_observation("original-search", 1, 0, || {
                Ok(client.get(&url).send()?.json()?)
            })?["status"],
            "running"
        );
        // Reopening skips completed read ledgers without replaying old status.
        let p = observation_producer(root.path())?;
        assert_eq!(
            p.coordinator_observation("original-search", 1, 0, || {
                Ok(client.get(&url).send()?.json()?)
            })?["status"],
            "pass"
        );
        serving.join().expect("HTTP fixture panicked")?;
        let saved: Value = serde_json::from_slice(&std::fs::read(
            root.path().join("r000.observe.result.json"),
        )?)?;
        assert_eq!(saved["response"], json!({"delivered":true}));
        assert!(root.path().join("r000.observe.dispatch001.json").exists());
        assert!(!root.path().join("r001.observe.dispatch001.json").exists());
        Ok(())
    }
    #[test]
    fn failed_observation_does_not_reset_custom_retry_limit() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let p = observation_producer(root.path())?;
        let mut count = 0;
        assert!(
            p.coordinator_observation("same-search", 1, 0, || {
                count += 1;
                anyhow::bail!("coordinator answered 503")
            })
            .is_err()
        );
        assert_eq!(count, 2);
        let p = observation_producer(root.path())?;
        assert!(
            p.coordinator_observation("same-search", 1, 0, || {
                count += 1;
                Ok(json!({"status":"pass"}))
            })
            .is_err()
        );
        assert_eq!(count, 2);
        assert!(
            p.coordinator_observation("same-search", 3, 0, || panic!("retry allowance reset"))
                .is_err()
        );
        assert!(
            p.coordinator_observation("different-search", 1, 0, || panic!("operation renamed"))
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn a_recovered_response_cannot_hide_a_lost_calls_unknown_usage() -> Result<()> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let config = deepseek::DeepSeekConfig {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            endpoint: "http://127.0.0.1:1".into(),
            prompt_version: deepseek::PROMPT_VERSION_V5.into(),
            max_output_tokens: 1024,
            timeout_ms: 1000,
            thinking: deepseek::ThinkingMode::Disabled,
        };
        let plan = budget::BudgetPlan {
            max_calls: 3,
            input_token_cap: 1024,
            output_token_cap: 1024,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 6,
        };
        let journal = Arc::new(budget::CallBudget::create(
            &root.path().join("calls.jsonl"),
            plan.clone(),
        )?);
        let api = Arc::new(DeepSeekCandidateProducer::new_mock(
            config.clone(),
            journal,
        )?);
        let producer = ReasoningProducer::new(
            ReasoningProviderConfig::Api(config),
            plan,
            root.path().join("producer"),
            Some(api.clone()),
        )?;
        let mut provenance = api.identity().unknown_usage();
        provenance.estimated_cost_usd_micros = Some(2);
        let error = producer
            .finish_round(
                b"{}".to_vec(),
                vec![],
                vec![ProviderCall {
                    provenance,
                    status: CallStatus::Success,
                    error_class: None,
                }],
            )
            .err()
            .context("unknown usage must stop the round")?;
        let failure = error
            .downcast_ref::<ReasoningProviderFailure>()
            .context("typed failure")?;
        assert_eq!(failure.0.class, ErrorClass::TransportError);
        assert_eq!(failure.0.provenance.usage.input_tokens, None);
        assert_eq!(failure.0.provenance.estimated_cost_usd_micros, Some(2));
        assert_eq!(
            api.credential_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepRecord {
    schema: String,
    configuration_ref: String,
    input_sha256: String,
    output_sha256: String,
    raw_output_base64: String,
    provider_call: Option<ProviderCall>,
    #[serde(default)]
    failure: Option<ErrorClass>,
    elapsed_ms: u64,
    #[serde(default)]
    reasoning_ms: u64,
    #[serde(default)]
    inspection_ms: u64,
    #[serde(default)]
    validation_ms: u64,
    inspected: Vec<SourceReference>,
    inspection_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validation_error: Option<String>,
    inspected_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionResponse {
    schema: String,
    input_sha256: String,
    // Parse proposal schema at the shared validation/repair boundary, just as
    // API outputs are parsed. Invalid authoring is not a bridge outage.
    output: Value,
}

pub struct ReasoningProducer {
    config: ReasoningProviderConfig,
    configuration_ref: String,
    budget: budget::BudgetPlan,
    directory: PathBuf,
    api: Option<Arc<DeepSeekCandidateProducer>>,
    observation_seq: Mutex<u64>,
}
#[derive(Debug)]
pub(super) struct ReasoningProviderFailure(pub GeneralFailure);
impl std::fmt::Display for ReasoningProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reasoning provider failed: {:?}", self.0.class)
    }
}
impl std::error::Error for ReasoningProviderFailure {}

/// An owner-local limit reached before a provider dispatch. It is not a call.
#[derive(Debug)]
pub(super) struct ReasoningCallBudgetExhausted;
impl std::fmt::Display for ReasoningCallBudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reasoning call budget exhausted before dispatch")
    }
}
impl std::error::Error for ReasoningCallBudgetExhausted {}

pub(super) struct RoundAnswer {
    pub output: ProducerOutput,
    pub provider_call: Option<ProviderCall>,
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub(super) fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    // Publish complete, durable bytes without replacing an existing exchange.
    // The staging file is in the same owner-owned directory as the destination.
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().context("evidence parent")?)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().context("evidence parent")?)?.sync_all()?;
    Ok(())
}
pub fn save_owner_checkpoint(path: &Path, bytes: &[u8]) -> Result<()> {
    save(path, bytes)
}
fn bounded_read(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "reasoning evidence bounds");
    Ok(bytes)
}
impl ReasoningProducer {
    fn check_exchange_budget(&self, input_path: &Path) -> Result<u32> {
        let input_count = std::fs::read_dir(&self.directory)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?
            .iter()
            .filter(|name| name.to_string_lossy().ends_with(".input.json"))
            .count();
        let provider_calls = self.api.as_ref()
            .map(|api| api.accounting_snapshot())
            .transpose()?
            .map_or(0, |snapshot| snapshot.cells.len());
        if !input_path.exists()
            && (input_count >= self.budget.max_calls as usize
                || provider_calls >= self.budget.max_calls as usize)
        {
            return Err(ReasoningCallBudgetExhausted.into());
        }
        u32::try_from(input_count).context("reasoning exchange count overflow")
    }
    /// Each successful observation reads a fresh snapshot. Only its completion
    /// is cached; failed reads retain their original retry allowance on restart.
    pub fn coordinator_observation(
        &self,
        satisfy_id: &str,
        max_retries: u32,
        expires: u64,
        mut dispatch: impl FnMut() -> Result<Value>,
    ) -> Result<Value> {
        use fs2::FileExt;
        let mut seq = self
            .observation_seq
            .lock()
            .map_err(|_| anyhow::anyhow!("coordinator observation lock poisoned"))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.directory.join("observations.lock"))?;
        lock.lock_exclusive()?;
        while self
            .directory
            .join(format!("r{:03}.observe.result.json", *seq))
            .exists()
        {
            *seq = seq
                .checked_add(1)
                .context("observation sequence exhausted")?;
        }
        let mut response = None;
        self.coordinator_operation(
            *seq,
            "observe",
            &serde_jcs::to_vec(&json!({"satisfy_id":satisfy_id}))?,
            max_retries,
            expires,
            || {
                response = Some(dispatch()?);
                Ok(json!({"delivered":true}))
            },
        )?;
        *seq = seq
            .checked_add(1)
            .context("observation sequence exhausted")?;
        response.context("coordinator observation snapshot missing")
    }
    /// Exactly one retry ledger per semantic operation, independent of worker
    /// restarts and transport names. Dispatch reservation is durable first.
    pub fn coordinator_operation(
        &self,
        seq: u64,
        kind: &str,
        request: &[u8],
        max_retries: u32,
        expires: u64,
        mut dispatch: impl FnMut() -> Result<Value>,
    ) -> Result<Value> {
        use fs2::FileExt;
        ensure!(
            matches!(kind, "start" | "claim" | "complete" | "observe"),
            "unknown coordinator operation"
        );
        let name = format!("r{seq:03}.{kind}");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.directory.join(format!("{name}.lock")))?;
        lock.lock_exclusive()?;
        ensure!(max_retries <= 16, "invalid frozen coordinator retry limit");
        let policy_path = self.directory.join(format!("{name}.policy.json"));
        let policy =
            serde_jcs::to_vec(&json!({"max_retries":max_retries,"expires_at_ms":expires}))?;
        if policy_path.exists() {
            ensure!(
                bounded_read(&policy_path, 4096)? == policy,
                "coordinator retry policy changed"
            );
        } else {
            save(&policy_path, &policy)?;
        }
        let result_path = self.directory.join(format!("{name}.result.json"));
        let request_hash = digest(request);
        if result_path.exists() {
            let result: Value = serde_json::from_slice(&bounded_read(&result_path, 64 * 1024)?)?;
            ensure!(
                result["request_sha256"] == request_hash,
                "operation identity changed"
            );
            return Ok(result["response"].clone());
        }
        for attempt in 0..=max_retries {
            let path = self
                .directory
                .join(format!("{name}.dispatch{attempt:03}.json"));
            if path.exists() {
                let old: Value = serde_json::from_slice(&bounded_read(&path, 4096)?)?;
                ensure!(
                    old["request_sha256"] == request_hash,
                    "operation identity changed"
                );
                let error_path = self
                    .directory
                    .join(format!("{name}.error{attempt:03}.json"));
                if error_path.exists() {
                    let error: Value = serde_json::from_slice(&bounded_read(&error_path, 4096)?)?;
                    ensure!(
                        error["retryable"] != false,
                        "coordinator operation permanently refused; preserved"
                    );
                }
                continue;
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            ensure!(
                matches!(kind, "complete" | "observe") || now < expires,
                "coordinator operation deadline"
            );
            save(
                &path,
                &serde_jcs::to_vec(&json!({"request_sha256":request_hash,
                "attempt":attempt,"started_at_ms":now}))?,
            )?;
            let started = Instant::now();
            match dispatch() {
                Ok(response) => {
                    save(
                        &result_path,
                        &serde_jcs::to_vec(&json!({"request_sha256":request_hash,
                        "response":response,"elapsed_ms":started.elapsed().as_millis()}))?,
                    )?;
                    return Ok(response);
                }
                Err(error) => {
                    let retryable = error.downcast_ref::<reqwest::Error>().is_some()
                        || error.to_string().starts_with("coordinator answered 5")
                        || error.to_string().starts_with("coordinator answered 429");
                    save(
                        &self
                            .directory
                            .join(format!("{name}.error{attempt:03}.json")),
                        &serde_jcs::to_vec(&json!({"retryable":retryable,
                            "elapsed_ms":started.elapsed().as_millis()}))?,
                    )?;
                    if !retryable || attempt == max_retries {
                        return Err(error);
                    }
                    let delay = 100_u64.saturating_mul(1 << attempt.min(6));
                    ensure!(
                        matches!(kind, "complete" | "observe")
                            || (SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
                                .saturating_add(delay)
                                < expires,
                        "retry backoff exceeds deadline"
                    );
                    std::thread::sleep(Duration::from_millis(delay));
                }
            }
        }
        anyhow::bail!("coordinator retry budget exhausted; same operation preserved")
    }
    pub fn new(
        config: ReasoningProviderConfig,
        budget: budget::BudgetPlan,
        directory: PathBuf,
        api: Option<Arc<DeepSeekCandidateProducer>>,
    ) -> Result<Self> {
        let configuration_ref = config.configuration_ref(&budget)?;
        ensure!(
            config.is_session() == api.is_none(),
            "reasoning transport mismatch"
        );
        std::fs::create_dir_all(&directory)?;
        let binding = serde_jcs::to_vec(&(configuration_ref.as_str(), &budget))?;
        let path = directory.join("binding.json");
        if path.exists() {
            ensure!(
                bounded_read(&path, 4096)? == binding,
                "reasoning binding changed"
            );
        } else {
            save(&path, &binding)?;
        }
        Ok(Self {
            config,
            configuration_ref,
            budget,
            directory,
            api,
            observation_seq: Mutex::new(0),
        })
    }
    pub fn configuration_ref(&self) -> &str {
        &self.configuration_ref
    }
    pub fn api_identity(&self) -> Option<ProviderIdentity> {
        self.api.as_ref().map(|p| p.identity())
    }
    fn records(&self) -> Result<Vec<(String, ReasoningInput, StepRecord)>> {
        let mut paths = std::fs::read_dir(&self.directory)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut records = Vec::new();
        for path in paths {
            let Some(name) = path
                .file_name()
                .and_then(|p| p.to_str())
                .and_then(|p| p.strip_suffix(".record.json"))
            else {
                continue;
            };
            let record: StepRecord = serde_json::from_slice(&bounded_read(&path, 32 * 1024)?)?;
            let input_bytes = bounded_read(
                &self.directory.join(format!("{name}.input.json")),
                64 * 1024,
            )?;
            let input: ReasoningInput = serde_json::from_slice(&input_bytes)?;
            let output = BASE64.decode(&record.raw_output_base64)?;
            ensure!(
                record.schema == "ato.formation-reasoning-step/1"
                    && input.schema == INPUT_SCHEMA
                    && record.configuration_ref == self.configuration_ref
                    && record.input_sha256 == digest(&input_bytes)
                    && record.output_sha256 == digest(&output)
                    && output.len() <= ato_formation::proposal::MAX_BATCH_BYTES,
                "reasoning evidence changed"
            );
            records.push((name.to_owned(), input, record));
        }
        Ok(records)
    }
    pub(super) fn saved_claim(&self, id: &str, seq: u64) -> Result<Option<Value>> {
        let path = self.directory.join(format!("r{seq:03}.claim.json"));
        if !path.exists() {
            return Ok(None);
        }
        let saved: Value = serde_json::from_slice(&bounded_read(&path, 4096)?)?;
        ensure!(
            saved["satisfy_id"] == id && saved["configuration_ref"] == self.configuration_ref,
            "reasoning claim binding changed"
        );
        Ok(Some(saved["claim"].clone()))
    }
    pub(super) fn save_claim(&self, id: &str, seq: u64, claim: &Value) -> Result<()> {
        let path = self.directory.join(format!("r{seq:03}.claim.json"));
        let value =
            json!({"satisfy_id":id,"configuration_ref":self.configuration_ref,"claim":claim});
        if path.exists() {
            ensure!(
                serde_json::from_slice::<Value>(&bounded_read(&path, 4096)?)? == value,
                "reasoning claim changed"
            );
        } else {
            save(&path, &serde_jcs::to_vec(&value)?)?;
        }
        Ok(())
    }
    fn finish_round(
        &self,
        raw: Vec<u8>,
        inspections: Vec<SourceReference>,
        calls: Vec<ProviderCall>,
    ) -> Result<RoundAnswer> {
        let final_raw = if inspections.is_empty() {
            raw
        } else {
            let mut batch: Value = serde_json::from_slice(&raw)?;
            batch["schema"] = json!(REASONING_BATCH_SCHEMA);
            batch["inspection_history"] = json!(inspections);
            serde_jcs::to_vec(&batch)?
        };
        let provider_call = if let Some(api) = &self.api {
            let mut provenance = api.identity().unknown_usage();
            if calls.iter().any(|call| {
                call.provenance.usage.input_tokens.is_none()
                    || call.provenance.usage.output_tokens.is_none()
            }) {
                // A recovered response does not reveal the lost call's usage.
                // Stop with the durable full charge instead of inventing a
                // successful round's zero-token accounting.
                provenance.estimated_cost_usd_micros = calls
                    .iter()
                    .map(|call| call.provenance.estimated_cost_usd_micros)
                    .collect::<Option<Vec<_>>>()
                    .map(|costs| costs.into_iter().fold(0u64, u64::saturating_add));
                return Err(anyhow::anyhow!(ReasoningProviderFailure(GeneralFailure {
                    class: ErrorClass::TransportError,
                    provenance,
                })));
            }
            provenance.usage = provenance::Usage {
                input_tokens: Some(
                    calls
                        .iter()
                        .filter_map(|c| c.provenance.usage.input_tokens)
                        .sum(),
                ),
                output_tokens: Some(
                    calls
                        .iter()
                        .filter_map(|c| c.provenance.usage.output_tokens)
                        .sum(),
                ),
            };
            provenance.estimated_cost_usd_micros = Some(
                calls
                    .iter()
                    .filter_map(|c| c.provenance.estimated_cost_usd_micros)
                    .sum(),
            );
            provenance.latency_ms =
                Some(calls.iter().filter_map(|c| c.provenance.latency_ms).sum());
            Some(ProviderCall {
                provenance,
                status: CallStatus::Success,
                error_class: None,
            })
        } else {
            None
        };
        let provenance = provider_call.as_ref().map_or(
            ProducerProvenance {
                provider: "fixed".into(),
                model: None,
            },
            |c| ProducerProvenance {
                provider: c.provenance.provider.clone(),
                model: Some(c.provenance.model.clone()),
            },
        );
        Ok(RoundAnswer {
            output: ProducerOutput::new(final_raw, provenance)?,
            provider_call,
        })
    }
    pub fn accounting(&self) -> Result<Value> {
        let records = self.records()?;
        let calls: Vec<_> = records.iter().map(|(name,input,r)| json!({"call_id":input.call_id,
            "input_sha256":r.input_sha256,"output_sha256":r.output_sha256,"latency_ms":r.elapsed_ms,"reasoning_ms":r.reasoning_ms,"inspection_ms":r.inspection_ms,"validation_ms":r.validation_ms,
            "provider_call":r.provider_call,"inspection_refs":r.inspected,"inspection_error":r.inspection_error,
            "input_file":format!("{name}.input.json"),"output_file":format!("{name}.record.json")})).collect();
        let transport = self
            .api
            .as_ref()
            .map(|api| api.accounting_snapshot())
            .transpose()?;
        let actual_calls = self.api.as_ref().map(|api| api.accounting()).transpose()?;
        Ok(
            json!({"provider_transport":transport,"actual_provider_calls":actual_calls,"schema":"ato.formation-reasoning-accounting/1", "provider":if self.config.is_session(){"codex_session"}else{"deepseek"},
            "calls":calls,"call_count":calls.len(),"API_calls":records.iter().filter(|(_,_,r)|r.provider_call.is_some()).count(),
            "session_token_usage":"not_exposed; no fabricated usage", "session_cost":"not_exposed", "configuration_ref":self.configuration_ref}),
        )
    }
    pub(super) fn run_round(
        &self,
        submission: &Submission,
        status: &Value,
        mut request: ProposalRequestV2,
        expires: u64,
    ) -> Result<RoundAnswer> {
        let state = submission.proposal_search(status)?;
        let local = submission
            .proposal_state
            .as_ref()
            .context("reasoning not enabled")?;
        let auth = local
            .frozen
            .policy
            .proposal
            .as_ref()
            .context("proposal domain missing")?;
        let policy = state
            .frozen
            .policy
            .exploration
            .as_ref()
            .context("exploration missing")?;
        let limits = policy
            .reasoning
            .as_ref()
            .context("reasoning limits missing")?;
        let round = request.round_seq.context("reasoning round missing")?;
        let domain = auth
            .execution_plan
            .as_ref()
            .context("execution plan domain missing")?;
        let records = self.records()?;
        ensure!(
            records
                .iter()
                .all(|(_, i, _)| i.request.search_id == request.search_id
                    && i.goal == limits.goal
                    && i.frozen_contract_ref == state.frozen.base_contract_ref
                    && i.source_identity.closure_ref == submission.request.source.closure_ref
                    && i.source_identity.archive_digest
                        == submission.request.source.archive_digest),
            "reasoning search changed"
        );
        for (_, input, _) in &records {
            input.validate(auth)?;
            for entry in &input.request.source_context {
                let verified = local.source_text(&entry.logical_id)?;
                ensure!(
                    verified.text.starts_with(&entry.text),
                    "recorded source text changed"
                );
            }
        }
        let mut acquired: BTreeMap<_, _> = local
            .source_context
            .iter()
            .map(|e| {
                (
                    e.logical_id.clone(),
                    local
                        .inspection_context
                        .get(&e.logical_id)
                        .unwrap_or(e)
                        .clone(),
                )
            })
            .collect();
        let mut inspected = BTreeMap::<String, SourceReference>::new();
        let mut latest_inspected = Vec::<String>::new();
        let mut round_inspections = Vec::new();
        let mut inspection_bytes = 0_u64;
        let mut inspection_exchanges = 0_u32;
        let mut feedback = Vec::new();
        let mut round_calls = Vec::new();
        for (_, input, r) in &records {
            if !r.inspected.is_empty() || r.inspection_error.is_some() {
                inspection_exchanges += 1;
            }
            inspection_bytes = inspection_bytes
                .checked_add(r.inspected_bytes)
                .context("inspection bytes overflow")?;
            if !r.inspected.is_empty() {
                latest_inspected = r.inspected.iter().map(|r| r.file_id.clone()).collect();
            }
            for source in &r.inspected {
                ato_formation::proposal::validate_inspection_sources(
                    auth,
                    std::slice::from_ref(source),
                    policy.max_inspections,
                    1,
                )?;
                acquired.insert(source.file_id.clone(), local.source_text(&source.file_id)?);
                inspected.insert(source.file_id.clone(), source.clone());
                if input.request.round_seq == Some(round) {
                    round_inspections.push(source.clone());
                }
            }
            if input.request.round_seq == Some(round) {
                if let Some(e) = &r.inspection_error {
                    feedback.push(e.clone());
                }
                if let Some(call) = &r.provider_call {
                    round_calls.push(call.clone());
                }
            }
        }
        // A lost completion must deliver the already recorded final answer,
        // never open another inference call after restart.
        if let Some((_, _, last)) = records
            .iter()
            .rev()
            .find(|(_, i, _)| i.request.round_seq == Some(round))
        {
            if let Some(class) = last.failure {
                let call = last
                    .provider_call
                    .as_ref()
                    .context("failed call evidence absent")?;
                return Err(anyhow::anyhow!(ReasoningProviderFailure(GeneralFailure {
                    class,
                    provenance: call.provenance.clone(),
                })));
            }
            if last.inspected.is_empty()
                && last.inspection_error.is_none()
                && last.validation_error.is_none()
            {
                return self.finish_round(
                    BASE64.decode(&last.raw_output_base64)?,
                    round_inspections,
                    round_calls,
                );
            }
        }
        let mut inspection_ms_used: u64 = records
            .iter()
            .filter(|(_, _, r)| !r.inspected.is_empty() || r.inspection_error.is_some())
            .map(|(_, _, r)| r.inspection_ms)
            .sum();
        let prior_in_round = records
            .iter()
            .filter(|(_, i, _)| i.request.round_seq == Some(round))
            .count();
        let mut step = prior_in_round as u32 + 1;
        let mut repairs = records
            .iter()
            .filter(|(_, i, r)| i.request.round_seq == Some(round) && r.validation_error.is_some())
            .count() as u32;
        for (_, i, r) in &records {
            if i.request.round_seq == Some(round)
                && let Some(error) = &r.validation_error
            {
                feedback.push(error.clone());
            }
        }
        loop {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            ensure!(now < expires, "reasoning round deadline");
            let name = format!("r{round:03}_s{step:03}");
            let call_id = format!("{}_r{round}_s{step}", request.search_id);
            let input_path = self.directory.join(format!("{name}.input.json"));
            let record_path = self.directory.join(format!("{name}.record.json"));
            let input_count = self.check_exchange_budget(&input_path)?;
            request.remaining_budget.timeout_ms = request
                .remaining_budget
                .timeout_ms
                .min(expires - now)
                .min(30_000);
            let mut entries: Vec<_> = acquired.values().cloned().collect();
            if matches!(&self.config, ReasoningProviderConfig::Session(c) if !deepseek::is_autonomous_prompt(&c.prompt_version))
                || matches!(&self.config, ReasoningProviderConfig::Api(c) if !deepseek::is_autonomous_prompt(&c.prompt_version))
            {
                entries.sort_by_key(|e| {
                    (
                        !inspected.contains_key(&e.logical_id),
                        e.kind,
                        e.logical_id.clone(),
                    )
                });
            } else {
                entries.sort_by_key(|e| {
                    let file = &domain.files[&e.logical_id];
                    let priority =
                        ato_formation::proposal::source_inspection_priority(&file.path).0;
                    (
                        if priority == 0 {
                            0
                        } else if latest_inspected.contains(&e.logical_id) {
                            1
                        } else if inspected.contains_key(&e.logical_id) {
                            2
                        } else if matches!(
                            e.kind,
                            ato_formation::proposal::SourceContextKind::Entrypoint
                        ) {
                            3
                        } else if matches!(priority, 3..=5) {
                            4
                        } else {
                            5
                        },
                        priority,
                        e.logical_id.clone(),
                    )
                });
            }
            let priority_order: Vec<_> = entries.iter().map(|e| e.logical_id.clone()).collect();
            let modern = matches!(&self.config, ReasoningProviderConfig::Session(c) if deepseek::is_autonomous_prompt(&c.prompt_version))
                || matches!(&self.config, ReasoningProviderConfig::Api(c) if deepseek::is_autonomous_prompt(&c.prompt_version));
            request.source_context = if modern {
                priority_context(
                    entries.into_iter().take(4).collect(),
                    auth.policy.max_source_bytes,
                )
            } else {
                bounded_inspection_context(
                    entries.into_iter().take(4).collect(),
                    auth.policy.max_source_bytes,
                )
            };
            let (inventory, scope_feedback) = scoped_inventory_with_feedback(domain, &acquired)?;
            let mut effective_feedback = feedback.clone();
            effective_feedback.extend(scope_feedback);
            let mut input = ReasoningInput {
                lowering_capabilities: self.config.capabilities(auth, &local.runtime_capabilities),
                catalog_sources_in_inventory: true,
                goal: limits.goal.clone(),
                available_variables: local.available_variables.clone(),
                runtime_capabilities: local.runtime_capabilities.clone(),
                max_retries: policy.formation.max_retries,
                schema: INPUT_SCHEMA.into(),
                call_id,
                frozen_contract_ref: state.frozen.base_contract_ref.clone(),
                source_identity: SourceIdentity {
                    archive_digest: submission.request.source.archive_digest.clone(),
                    closure_ref: submission.request.source.closure_ref.clone(),
                },
                inventory,
                request: request.clone(),
                projection: vec![],
                rounds_remaining: policy.formation.max_rounds.get().saturating_sub(round - 1),
                calls_remaining: self.budget.max_calls.saturating_sub(
                    self.api
                        .as_ref()
                        .map(|a| a.accounting_snapshot())
                        .transpose()?
                        .map_or(input_count, |s| s.cells.len() as u32),
                ),
                inspections_remaining: policy
                    .max_inspections
                    .saturating_sub(inspection_exchanges.max(inspected.len() as u32)),
                inspection_source_bytes_remaining: limits
                    .inspection_source_bytes
                    .saturating_sub(inspection_bytes),
                inspection_feedback: effective_feedback,
            };
            // The exact same public source refs appear once, in inventory.
            // The original full catalog is reconstructed and validated locally.
            for operation in &mut input.request.operation_catalog.operations {
                if let ato_formation::proposal::OperationDomain::ExecutionPlan { sources, .. } =
                    operation
                {
                    sources.clear();
                }
            }
            loop {
                input.projection = acquired
                    .values()
                    .filter_map(|e| {
                        let transmitted = input
                            .request
                            .source_context
                            .iter()
                            .find(|t| t.logical_id == e.logical_id)
                            .map_or(0, |t| t.text.len());
                        (transmitted < e.text.len()).then(|| ContextOmission {
                            logical_id: e.logical_id.clone(),
                            reason: if transmitted == 0 {
                                "slot_or_input_budget"
                            } else {
                                "text_prefix_budget"
                            }
                            .into(),
                            acquired_bytes: e.text.len(),
                            transmitted_bytes: transmitted,
                        })
                    })
                    .collect();
                let bytes = serde_jcs::to_vec(&input)?;
                if (bytes.len()
                    + deepseek::prompt_for(match &self.config {
                        ReasoningProviderConfig::Api(c) => &c.prompt_version,
                        ReasoningProviderConfig::Session(c) => &c.prompt_version,
                    })
                    .context("unknown prompt")?
                    .len()
                    + 1024) as u64
                    <= self.budget.input_token_cap
                {
                    break;
                }
                let entry = input
                    .request
                    .source_context
                    .iter_mut()
                    .max_by_key(|e| {
                        if modern {
                            priority_order
                                .iter()
                                .position(|id| id == &e.logical_id)
                                .unwrap_or(usize::MAX)
                        } else {
                            e.text.len()
                        }
                    })
                    .context("non-text reasoning context exceeds input budget")?;
                let mut end = entry.text.len().saturating_sub(512);
                while !entry.text.is_char_boundary(end) {
                    end -= 1;
                }
                entry.text.truncate(end);
                entry.truncated = true;
                entry.content_sha256 = digest(entry.text.as_bytes());
                input.request.source_context.retain(|e| !e.text.is_empty());
            }
            input.validate(auth)?;
            let mut bytes = serde_jcs::to_vec(&input)?;
            if input_path.exists() {
                bytes = bounded_read(&input_path, 64 * 1024)?;
                let saved: ReasoningInput = serde_json::from_slice(&bytes)?;
                ensure!(
                    saved.request.search_id == input.request.search_id
                        && saved.request.round_seq == input.request.round_seq
                        && saved.schema == INPUT_SCHEMA
                        && saved.lowering_capabilities
                            == self.config.capabilities(auth, &saved.runtime_capabilities)
                        && saved.goal == input.goal
                        && saved.call_id == input.call_id
                        && saved.frozen_contract_ref == input.frozen_contract_ref
                        && saved.max_retries == input.max_retries
                        && saved.source_identity.closure_ref == input.source_identity.closure_ref
                        && saved.source_identity.archive_digest
                            == input.source_identity.archive_digest,
                    "pending reasoning input mismatch"
                );
                saved.validate(auth)?;
                for entry in &saved.request.source_context {
                    let verified = local.source_text(&entry.logical_id)?;
                    ensure!(
                        verified.text.starts_with(&entry.text),
                        "pending source text changed"
                    );
                }
                input = saved;
            } else {
                save(&input_path, &bytes)?;
            }
            let input_sha256 = digest(&bytes);
            let call_started = Instant::now();
            let (raw, call) = if let Some(api) = &self.api {
                let answer = match api.propose_reasoning_recover(
                    &input,
                    expires,
                    policy.formation.max_retries,
                ) {
                    Ok(answer) => answer,
                    Err(failure) => {
                        let failure = *failure;
                        let call = ProviderCall {
                            provenance: failure.provenance.clone(),
                            status: if failure.class == ErrorClass::Timeout {
                                CallStatus::Timeout
                            } else {
                                CallStatus::ProviderError
                            },
                            error_class: Some(failure.class),
                        };
                        let record = StepRecord {
                            schema: "ato.formation-reasoning-step/1".into(),
                            configuration_ref: self.configuration_ref.clone(),
                            input_sha256,
                            output_sha256: digest(&[]),
                            raw_output_base64: String::new(),
                            provider_call: Some(call),
                            failure: Some(failure.class),
                            elapsed_ms: call_started.elapsed().as_millis().min(u64::MAX as u128)
                                as u64,
                            reasoning_ms: call_started.elapsed().as_millis().min(u64::MAX as u128)
                                as u64,
                            inspection_ms: 0,
                            validation_ms: 0,
                            inspected: vec![],
                            inspection_error: None,
                            validation_error: None,
                            inspected_bytes: 0,
                        };
                        save(&record_path, &serde_jcs::to_vec(&record)?)?;
                        return Err(anyhow::anyhow!(ReasoningProviderFailure(failure)));
                    }
                };
                let call = ProviderCall {
                    provenance: answer.provenance,
                    status: CallStatus::Success,
                    error_class: None,
                };
                call.validate(&api.identity())?;
                (answer.output.raw().to_vec(), Some(call))
            } else {
                let response_path = self.directory.join(format!("{name}.response.json"));
                loop {
                    if response_path.exists() {
                        break;
                    }
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
                    ensure!(
                        now < expires,
                        "session response timeout; same search and budget retained"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
                let response: SessionResponse = serde_json::from_slice(&bounded_read(
                    &response_path,
                    ato_formation::proposal::MAX_BATCH_BYTES + 4096,
                )?)?;
                ensure!(
                    response.schema == SESSION_RESPONSE_SCHEMA
                        && response.input_sha256 == input_sha256,
                    "session response input mismatch"
                );
                (serde_jcs::to_vec(&response.output)?, None)
            };
            ensure!(
                raw.len() <= ato_formation::proposal::MAX_BATCH_BYTES,
                "reasoning output bounds"
            );
            let reasoning_ms = call_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let validation_started = Instant::now();
            let parsed = serde_json::from_slice::<ProposalBatch>(&raw);
            let inspection = parsed.as_ref().ok().and_then(|b| {
                if b.schema == ato_formation::proposal::PROPOSAL_SCHEMA && b.proposals.len() == 1 {
                    if let Proposal::InspectSource { sources } = &b.proposals[0] {
                        Some(sources.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
            let mut retrieved = Vec::new();
            let mut error = None;
            let mut retrieved_bytes = 0_u64;
            let mut validation_error = None;
            let mut details = parsed.as_ref().err().map(|e| format!("JSON/schema: {e}"));
            if inspection.is_none() {
                let output = ProducerOutput::new(
                    raw.clone(),
                    ProducerProvenance {
                        provider: "fixed".into(),
                        model: None,
                    },
                )?;
                let scope_ok = parsed.as_ref().is_ok_and(|batch|batch.proposals.iter().all(|proposal| {
                    let operations=match proposal {Proposal::ProposeDerivation{operations}|Proposal::ModifyDerivation{operations,..}=>operations,_=>return true};
                    operations.iter().all(|operation|if let ato_formation::proposal::OperationInvocation::ExecutionPlan{plan}=operation {
                        let mut references=vec![&plan.entrypoint];references.extend(plan.basis.iter().map(|b|&b.source));references.extend(plan.runtime_port_operations.iter().map(|o| &o.basis));
                        for dependency in &plan.dependencies { match dependency {
                            ato_formation::proposal::DependencyOperation::PythonRequirements{requirements}|ato_formation::proposal::DependencyOperation::PythonResolveRequirements{requirements}|ato_formation::proposal::DependencyOperation::PythonBuildRequirements{requirements,..}=>references.push(requirements),
                            ato_formation::proposal::DependencyOperation::NpmCi{manifest,lockfile,..}|ato_formation::proposal::DependencyOperation::NpmRebuild{manifest,lockfile,..}=>references.extend([manifest,lockfile]),
                        }}
                        references.iter().all(|reference|input.inventory.iter().any(|s|&s.reference==*reference))
                    }else{true})
                }));
                let result = if scope_ok || parsed.is_err() {
                    CandidateRegistry::new(&local.frozen)?.validate_batch(&local.bases, &output)
                } else {
                    details=Some("Use only inventory source references; obtain explicitly referenced configuration with inspection.".into());
                    Err(ato_formation::proposal::ProposalError(
                        "source_explicit_reference_required",
                    ))
                };
                let code = match result {
                    Err(e) => Some(e.0),
                    Ok(outcomes) => outcomes.into_iter().find_map(|o| match o {
                        ProposalOutcome::Rejected(e) => Some(e.0),
                        _ => None,
                    }),
                };
                if let Some(code) = code.filter(|c| {
                    !matches!(*c, "exploration_authority_exceeded")
                        && !matches!(
                            *c,
                            "unsupported_toolchain"
                                | "source_oci_builder_unavailable"
                                | "unsupported_authority_resource"
                                | "unsupported_state_requirement"
                        )
                        && *c != "requires_binding"
                }) && repairs < policy.formation.max_retries
                    && !feedback.iter().any(|f| f == code)
                {
                    validation_error = Some(code.to_owned());
                    feedback.push(code.to_owned());
                    if matches!(
                        code,
                        "unsupported_entrypoint"
                            | "unsupported_node_entrypoint"
                            | "unsupported_static_output"
                    ) {
                        feedback.push("For static_output use the source manifest entrypoint, argv:[], guest_port:0, no module/environment/state/variable_bindings, and source-owned build_scripts. Process entrypoints must be actual supported script files. A rejected field combination does not mean the static operation is unavailable.".into());
                    }
                    if code == "execution_plan_bounds" {
                        feedback.push("Process guest_port must be nonzero. cwd is source-relative or '.'. state.id is a logical resource; state.mount is an absolute guest path under /data, /state or /app/<subdirectory>, never the resource name or a host path. access is 'read-only' or 'read-write'. Check the declared array/string bounds and reserved environment names.".into());
                    }
                    if let Some(detail) = details.take() {
                        feedback.push(detail.chars().take(512).collect());
                    }
                    repairs += 1;
                }
            }
            let validation_ms = validation_started
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            let inspection_started = Instant::now();
            if let Some(sources) = inspection {
                inspection_exchanges += 1;
                let result = (|| -> Result<_> {
                    ensure!(
                        inspection_exchanges <= policy.max_inspections,
                        "inspection_exchange_budget_exhausted"
                    );
                    ensure!(
                        inspection_ms_used < limits.inspection_timeout_ms,
                        "inspection_time_budget_exhausted"
                    );
                    let remaining = policy
                        .max_inspections
                        .saturating_sub(inspected.len() as u32);
                    let refs = ato_formation::proposal::validate_inspection_sources(
                        auth, &sources, remaining, 4,
                    )?;
                    ensure!(
                        refs.iter()
                            .all(|r| input.inventory.iter().any(|s| &s.reference == r)),
                        "source_explicit_reference_required"
                    );
                    ensure!(
                        refs.iter().all(|r| !inspected.contains_key(&r.file_id)
                            && (!acquired.contains_key(&r.file_id)
                                || input.projection.iter().any(|e| e.logical_id == r.file_id))),
                        "inspection_no_new_information"
                    );
                    for reference in &refs {
                        let text = local.source_text(&reference.file_id)?;
                        retrieved_bytes = retrieved_bytes
                            .checked_add(text.text.len() as u64)
                            .context("inspection byte overflow")?;
                    }
                    ensure!(
                        inspection_bytes + retrieved_bytes <= limits.inspection_source_bytes,
                        "inspection_source_byte_budget_exhausted"
                    );
                    ensure!(
                        u128::from(inspection_ms_used) + inspection_started.elapsed().as_millis()
                            <= u128::from(limits.inspection_timeout_ms),
                        "inspection_time_budget_exhausted"
                    );
                    Ok(refs)
                })();
                match result {
                    Ok(refs) => {
                        latest_inspected = refs.iter().map(|r| r.file_id.clone()).collect();
                        inspection_bytes += retrieved_bytes;
                        for r in &refs {
                            acquired.insert(r.file_id.clone(), local.source_text(&r.file_id)?);
                            inspected.insert(r.file_id.clone(), r.clone());
                        }
                        round_inspections.extend(refs.clone());
                        retrieved = refs;
                    }
                    Err(e) => {
                        retrieved_bytes = 0;
                        error = Some(
                            if e.to_string().starts_with("source_")
                                || e.to_string().starts_with("inspection_")
                            {
                                e.to_string()
                            } else {
                                "inspection_budget_or_progress".into()
                            },
                        );
                        feedback = vec![error.clone().unwrap_or_default()];
                    }
                }
            }
            let record = StepRecord {
                schema: "ato.formation-reasoning-step/1".into(),
                configuration_ref: self.configuration_ref.clone(),
                input_sha256,
                output_sha256: digest(&raw),
                raw_output_base64: BASE64.encode(&raw),
                provider_call: call.clone(),
                failure: None,
                elapsed_ms: call_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                reasoning_ms,
                validation_ms,
                inspection_ms: if !retrieved.is_empty() || error.is_some() {
                    inspection_started
                        .elapsed()
                        .as_millis()
                        .min(u64::MAX as u128) as u64
                } else {
                    0
                },
                inspected: retrieved.clone(),
                inspection_error: error.clone(),
                validation_error: validation_error.clone(),
                inspected_bytes: retrieved_bytes,
            };
            save(&record_path, &serde_jcs::to_vec(&record)?)?;
            inspection_ms_used = inspection_ms_used.saturating_add(record.inspection_ms);
            if let Some(call) = call {
                round_calls.push(call);
            }
            if !retrieved.is_empty() || error.is_some() || validation_error.is_some() {
                step += 1;
                continue;
            }
            return self.finish_round(raw, round_inspections, round_calls);
        }
    }
}
