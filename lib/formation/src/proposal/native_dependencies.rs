//! Generic dependency operations, lowered to the existing contained exec path.
use super::{
    DependencyOperation, ExecutionPlanProposal, NativeBuildNetwork, PlanAuthorization,
    ProposalError, RuntimeSelection, SourceReference,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The same canonical registered operations supply scheduler claims and the
/// Runtime attestation. Tools are prebound requirements, never provisions.
pub fn runtime_requirements(
    d: &crate::authoring::BoundDerivation,
) -> Vec<crate::search::Requirement> {
    let mut facts = BTreeSet::new();
    for step in &d.steps {
        if step.protocol != "ato.process@1" || step.op != "exec" || step.argv.len() != 5 {
            continue;
        }
        let schema = if step.argv[2] == include_str!("python-native-operation.py")
            || step.argv[2] == include_str!("python-native-dependencies.py")
        {
            "ato.python-build-plan/1"
        } else if step.argv[2] == include_str!("node-native-dependencies.cjs") {
            "ato.npm-native-plan/1"
        } else {
            continue;
        };
        let Ok(plan) = serde_json::from_str::<Value>(&step.argv[4]) else {
            continue;
        };
        if plan["schema"] != schema {
            continue;
        }
        let Ok(tools) = serde_json::from_value::<Vec<RuntimeSelection>>(plan["toolchains"].clone())
        else {
            continue;
        };
        for tool in tools {
            facts.insert(format!("toolchain.{}.{}", tool.name, tool.version));
        }
    }
    facts
        .into_iter()
        .map(|fact| crate::search::Requirement {
            fact,
            one_of: Some(vec!["present".into()]),
        })
        .collect()
}

fn step(steps: &mut Vec<Value>, argv: Vec<String>, cwd: &str, network: &str) {
    steps.push(
        json!({"id":format!("native-{}",steps.len()),"use":"ato.process@1",
        "op":"exec","argv":argv,"cwd":cwd,"network":network}),
    );
}

fn build_network(network: NativeBuildNetwork) -> &'static str {
    match network {
        NativeBuildNetwork::Denied => "denied",
        NativeBuildNetwork::ScopedBuild => "scoped-build",
    }
}

fn tools(
    tools: &[RuntimeSelection],
    authorization: &PlanAuthorization,
) -> Result<Value, ProposalError> {
    let mut names = BTreeSet::new();
    if tools.len() > 4
        || tools.iter().any(|t| {
            !matches!(t.name.as_str(), "python" | "gcc" | "make" | "pkg-config")
                || !names.insert(&t.name)
                || authorization.toolchains.get(&t.name) != Some(&t.version)
        })
    {
        return Err(ProposalError("native_toolchain_unavailable"));
    }
    Ok(json!(tools))
}

fn npm_refs<'a>(
    proposal: &ExecutionPlanProposal,
    authorization: &'a PlanAuthorization,
    manifest: &SourceReference,
    lockfile: &SourceReference,
) -> Result<(&'a str, &'a str), ProposalError> {
    let prefix = if proposal.cwd == "." {
        String::new()
    } else {
        format!("{}/", proposal.cwd)
    };
    let (manifest_path, lock_path) = (
        authorization.resolve(manifest)?,
        authorization.resolve(lockfile)?,
    );
    if manifest_path != format!("{prefix}package.json")
        || lock_path != format!("{prefix}package-lock.json")
    {
        return Err(ProposalError("unsupported_dependency_manifest"));
    }
    Ok((manifest_path, lock_path))
}

pub(super) fn compile_operation(
    operation: &DependencyOperation,
    proposal: &ExecutionPlanProposal,
    authorization: &PlanAuthorization,
    executable: &str,
    steps: &mut Vec<Value>,
) -> Result<bool, ProposalError> {
    match operation {
        DependencyOperation::PythonBuildRequirements {
            requirements,
            build_dependencies,
            toolchains,
            network,
        } if proposal.runtime.name == "python" => {
            let path = authorization.resolve(requirements)?;
            let mut names = BTreeSet::new();
            if !path.ends_with(".txt")
                || build_dependencies.is_empty()
                || build_dependencies.len() > 32
                || build_dependencies.iter().any(|d| {
                    d.name.is_empty()
                        || !d.name.as_bytes()[0].is_ascii_alphanumeric()
                        || !d.name.as_bytes()[d.name.len() - 1].is_ascii_alphanumeric()
                        || d.name.len() > 128
                        || !d
                            .name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                        || !names.insert(d.name.to_ascii_lowercase().replace('_', "-"))
                        || d.version.is_empty()
                        || !d.version.as_bytes()[0].is_ascii_digit()
                        || d.version.len() > 64
                        || !d
                            .version
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
                })
            {
                return Err(ProposalError("python_build_dependencies_invalid"));
            }
            let root = format!("/app/.ato-dependencies/python-build-{}", steps.len());
            let plan = json!({"schema":"ato.python-build-plan/1","python_version":proposal.runtime.version,"requirements":format!("/app/{path}"),
                "requirements_sha256":requirements.digest,"root":root,"build_dependencies":build_dependencies,
                "toolchains":tools(toolchains,authorization)?,"build_network":build_network(*network),
                "lock_operation":include_str!("python-lock-operation.py")});
            for (mode, phase) in [
                ("check", "denied"),
                ("build-dependencies", "scoped-dependencies"),
                ("prepare", "denied"),
                ("acquire", "scoped-dependencies"),
                ("build", build_network(*network)),
                ("seal", "denied"),
            ] {
                step(
                    steps,
                    vec![
                        executable.into(),
                        "-I".into(),
                        "-c".into(),
                        include_str!("python-native-operation.py").into(),
                        mode.into(),
                        plan.to_string(),
                    ],
                    &proposal.cwd,
                    phase,
                );
            }
            let minor = proposal
                .runtime
                .version
                .rsplit_once('.')
                .ok_or(ProposalError("unsupported_toolchain"))?
                .0;
            step(
                steps,
                vec![
                    executable.into(),
                    "-I".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "--no-input".into(),
                    "--no-index".into(),
                    "--no-compile".into(),
                    "--only-binary=:all:".into(),
                    "--require-hashes".into(),
                    "--find-links".into(),
                    format!("{root}/wheels"),
                    "--target".into(),
                    format!("/app/.venv/lib/python{minor}/site-packages"),
                    "-r".into(),
                    format!("{root}/wheels/requirements.lock"),
                ],
                &proposal.cwd,
                "denied",
            );
            step(
                steps,
                vec![
                    executable.into(),
                    "-I".into(),
                    "-c".into(),
                    include_str!("python-native-operation.py").into(),
                    "cleanup".into(),
                    plan.to_string(),
                ],
                &proposal.cwd,
                "denied",
            );
            Ok(true)
        }
        DependencyOperation::NpmRebuild {
            manifest,
            lockfile,
            packages,
            root_lifecycle,
            toolchains,
            network,
        } if proposal.runtime.name == "node" => {
            npm_refs(proposal, authorization, manifest, lockfile)?;
            let acquired = proposal.dependencies.iter().position(|d| matches!(d, DependencyOperation::NpmCi {manifest:m,lockfile:l,..} if m==manifest && l==lockfile));
            let selected = proposal
                .dependencies
                .iter()
                .position(|d| std::ptr::eq(d, operation));
            let mut names = BTreeSet::new();
            if acquired.is_none()
                || selected.is_none()
                || acquired >= selected
                || packages.len() > 128
                || packages.iter().any(|p| {
                    p.is_empty()
                        || p.len() > 214
                        || p.starts_with('-')
                        || !p
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"@/._-".contains(&b))
                        || !names.insert(p)
                })
                || root_lifecycle.len() > 7
                || root_lifecycle.iter().any(|s| {
                    !matches!(
                        s.as_str(),
                        "preinstall"
                            | "install"
                            | "postinstall"
                            | "prepublish"
                            | "preprepare"
                            | "prepare"
                            | "postprepare"
                    )
                })
                || root_lifecycle.iter().collect::<BTreeSet<_>>().len() != root_lifecycle.len()
            {
                return Err(ProposalError("npm_lifecycle_plan_invalid"));
            }
            let plan = npm_plan(
                proposal,
                authorization,
                manifest,
                lockfile,
                NpmNativeSelection {
                    packages,
                    root_lifecycle,
                    toolchains,
                    network: *network,
                },
            )?;
            step(
                steps,
                vec![
                    executable.into(),
                    "-e".into(),
                    include_str!("node-native-dependencies.cjs").into(),
                    "rebuild".into(),
                    plan.to_string(),
                ],
                &proposal.cwd,
                build_network(*network),
            );
            Ok(true)
        }
        DependencyOperation::PythonBuildRequirements { .. }
        | DependencyOperation::NpmRebuild { .. } => {
            Err(ProposalError("unsupported_dependency_operation"))
        }
        _ => Ok(false),
    }
}

#[derive(Default)]
struct NpmNativeSelection<'a> {
    packages: &'a [String],
    root_lifecycle: &'a [String],
    toolchains: &'a [RuntimeSelection],
    network: NativeBuildNetwork,
}

fn npm_plan(
    proposal: &ExecutionPlanProposal,
    authorization: &PlanAuthorization,
    manifest: &SourceReference,
    lockfile: &SourceReference,
    selection: NpmNativeSelection<'_>,
) -> Result<Value, ProposalError> {
    let NpmNativeSelection {
        packages,
        root_lifecycle,
        toolchains,
        network,
    } = selection;
    let (manifest_path, lock_path) = npm_refs(proposal, authorization, manifest, lockfile)?;
    let production_only = proposal
        .dependencies
        .iter()
        .find_map(|d| match d {
            DependencyOperation::NpmCi {
                manifest: m,
                lockfile: l,
                production_only,
            } if m == manifest && l == lockfile => Some(*production_only),
            _ => None,
        })
        .unwrap_or(false);
    if !authorization.toolchains.contains_key("npm") {
        return Err(ProposalError("runtime_toolchain_unavailable"));
    }
    Ok(
        json!({"schema":"ato.npm-native-plan/1","manifest":format!("/app/{manifest_path}"),"lockfile":format!("/app/{lock_path}"),
        "manifest_sha256":manifest.digest,"lockfile_sha256":lockfile.digest,"npm":format!("/opt/ato/toolchains/node/{}/bin/npm",proposal.runtime.version),"npm_version":authorization.toolchains["npm"],
        "node_root":format!("/opt/ato/toolchains/node/{}",proposal.runtime.version),"node_version":proposal.runtime.version,"receipt_directory":format!("/app/{}/.ato-npm-native",proposal.cwd),"packages":packages,"root_lifecycle":root_lifecycle,
        "toolchains":tools(toolchains,authorization)?,"build_network":build_network(network),"production_only":production_only}),
    )
}

pub(super) fn compile_npm_audit(
    proposal: &ExecutionPlanProposal,
    authorization: &PlanAuthorization,
    executable: &str,
    steps: &mut Vec<Value>,
) -> Result<(), ProposalError> {
    // Only the inspected, source-owned root is admitted in v0. One acquisition
    // and at most one rebuild prevent ambiguous completion receipts.
    let acquisitions = proposal
        .dependencies
        .iter()
        .filter_map(|d| match d {
            DependencyOperation::NpmCi {
                manifest, lockfile, ..
            } => Some((manifest, lockfile)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let rebuilds = proposal
        .dependencies
        .iter()
        .filter(|d| matches!(d, DependencyOperation::NpmRebuild { .. }))
        .count();
    if acquisitions.len() != 1 || rebuilds > 1 {
        return Err(ProposalError("npm_lifecycle_plan_invalid"));
    }
    let (manifest, lockfile) = acquisitions[0];
    let plan = npm_plan(
        proposal,
        authorization,
        manifest,
        lockfile,
        NpmNativeSelection::default(),
    )?;
    step(
        steps,
        vec![
            executable.into(),
            "-e".into(),
            include_str!("node-native-dependencies.cjs").into(),
            "audit".into(),
            plan.to_string(),
        ],
        &proposal.cwd,
        "denied",
    );
    Ok(())
}

pub(super) fn compile_npm_initialize(
    proposal: &ExecutionPlanProposal,
    authorization: &PlanAuthorization,
    executable: &str,
    manifest: &SourceReference,
    lockfile: &SourceReference,
    steps: &mut Vec<Value>,
) -> Result<(), ProposalError> {
    let plan = npm_plan(
        proposal,
        authorization,
        manifest,
        lockfile,
        NpmNativeSelection::default(),
    )?;
    step(
        steps,
        vec![
            executable.into(),
            "-e".into(),
            include_str!("node-native-dependencies.cjs").into(),
            "initialize".into(),
            plan.to_string(),
        ],
        &proposal.cwd,
        "denied",
    );
    Ok(())
}
