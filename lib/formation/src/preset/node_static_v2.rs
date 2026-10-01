//! Source-qualified static build authoring. Every route choice is sealed in D.
use std::collections::BTreeMap;

use super::{AppPreset, PresetMismatch, synthesize_authoring};
use crate::authoring::{AuthoringDraft, AuthoringProvenance, RuntimeDraft, StepDraft, StepNetwork};
use crate::detect::{DetectorEvidence, NodeEvidence, ViteOutDir};
use crate::intent::{self, ResolvedPackageManager};

pub const NODE_STATIC_V2: &str = "node-static/v2";

/// No workspace, service or compound-script inference. npm remains v1.
pub fn synthesize_node_static_v2(
    evidence: &DetectorEvidence,
) -> Result<AuthoringDraft, PresetMismatch> {
    let refuse = |code, message: &str| PresetMismatch::new(code, message);
    let node = evidence.node.as_ref().ok_or_else(|| {
        refuse(
            "preset_node_static_v2_source",
            "A package.json is required.",
        )
    })?;
    if node.has_bun_lock {
        return Err(refuse(
            "preset_node_static_v2_bun_deferred",
            "Bun authoring is deferred.",
        ));
    }
    if node.has_workspace {
        return Err(refuse(
            "preset_node_static_v2_workspace",
            "Workspace target/build/output require explicit authoring.",
        ));
    }
    refuse_manager_config(evidence)?;
    if !evidence.present_files.iter().any(|p| p == "index.html") {
        return Err(refuse(
            "preset_node_static_v2_static_unproven",
            "A root static entry document is required.",
        ));
    }
    // Reuse the existing narrow static-artifact detector, not a second inference engine.
    // Do not normalize an absolute output path into an apparently relative one.
    if let ViteOutDir::Literal(root) = &node.vite_out_dir
        && (root.is_empty()
            || root.starts_with('/')
            || crate::projection::workspace_relative_cwd(root).map_or(true, |p| p.is_empty()))
    {
        return Err(refuse(
            "preset_node_static_v2_output",
            "Static output must be a relative workspace subtree.",
        ));
    }
    let profile = intent::detect_static_build(evidence)
        .map_err(|e| PresetMismatch::new(e.code(), e.to_string()))?
        .ok_or_else(|| refuse("preset_node_static_v2_static_unproven", "A standalone static artifact is not established; explicit service/build authoring is required."))?;
    let (manager, install_mode) = exact_manager(node, &profile.node_version)?;
    Ok(static_build_draft(
        profile.node_version,
        manager,
        install_mode,
        ".",
        profile.output_root,
    ))
}

/// Canonical v2 D for one build scope. Install always runs at the source root
/// (the one lockfile scope); the build runs in `build_cwd`, which is `.` for a
/// non-workspace source and an inventory-resolved workspace otherwise.
pub(crate) fn static_build_draft(
    node_version: String,
    manager: ResolvedPackageManager,
    install_mode: &'static str,
    build_cwd: &str,
    output_root: String,
) -> AuthoringDraft {
    // Reuse the existing K template without changing its scope. Only D changes.
    let mut draft = synthesize_authoring(AppPreset::NodeStatic);
    draft.provenance = AuthoringProvenance::PresetSynthesized {
        preset: NODE_STATIC_V2,
    };
    draft.derivation.workspace_build = None;
    draft.derivation.runtimes = vec![
        RuntimeDraft {
            name: "node".into(),
            version: node_version,
        },
        RuntimeDraft {
            name: manager.name.clone(),
            version: manager.version,
        },
    ];
    let mut serve = draft.derivation.steps.remove(0);
    serve.root = Some(output_root);
    serve.cwd = ".".into();
    let exec_in = |id: &str, args: &[&str], cwd: &str, network| StepDraft {
        id: id.into(),
        protocol: crate::authoring::PROCESS_PROTOCOL.into(),
        op: "exec".into(),
        argv: std::iter::once(manager.name.clone())
            .chain(args.iter().map(|s| (*s).into()))
            .collect(),
        cwd: cwd.into(),
        env: BTreeMap::new(),
        source: None,
        root: None,
        entry: None,
        spa_fallback: None,
        network,
    };
    draft.derivation.steps = vec![
        exec_in(
            "install",
            &["install", install_mode],
            ".",
            StepNetwork::DependencyResolution,
        ),
        exec_in("build", &["run", "build"], build_cwd, StepNetwork::Denied),
        serve,
    ];
    draft
}

/// Package-manager files that can redirect the manager or run source hooks.
/// No config interpreter exists; refuse rather than delegate version authority.
pub(crate) const MANAGER_CONFIG_FILES: &[&str] = &[
    ".yarnrc",
    ".yarnrc.yml",
    ".npmrc",
    ".pnpmfile.cjs",
    ".pnpmfile.js",
];

pub(crate) fn refuse_manager_config(evidence: &DetectorEvidence) -> Result<(), PresetMismatch> {
    if evidence
        .present_files
        .iter()
        .any(|p| MANAGER_CONFIG_FILES.contains(&p.as_str()))
    {
        return Err(PresetMismatch::new(
            "preset_node_static_v2_manager_config",
            "Package-manager configuration requires explicit authoring.",
        ));
    }
    Ok(())
}

/// The one exact pnpm/Yarn and its immutable install mode for the root lock.
pub(crate) fn exact_manager(
    node: &NodeEvidence,
    node_version: &str,
) -> Result<(ResolvedPackageManager, &'static str), PresetMismatch> {
    let refuse = |code, message: &str| PresetMismatch::new(code, message);
    let declared = BTreeMap::from([("node".to_owned(), node_version.to_owned())]);
    let resolve = |declaration: Option<String>| -> Result<ResolvedPackageManager, PresetMismatch> {
        let mut facts = node.clone();
        facts.package_manager = declaration;
        intent::resolve_toolchains(Some(&facts), &declared)
            .map_err(|e| PresetMismatch::new(e.code(), e.to_string()))?
            .1
            .ok_or_else(|| {
                refuse(
                    "preset_node_static_v2_manager",
                    "An exact pnpm or Yarn declaration is required.",
                )
            })
    };
    let dev_manager = node
        .dev_engines_package_manager
        .as_ref()
        .map(|value| {
            let name = value.get("name").and_then(|v| v.as_str());
            let version = value.get("version").and_then(|v| v.as_str());
            match (name, version) {
                (Some(name), Some(version)) => resolve(Some(format!("{name}@{version}"))),
                _ => Err(refuse(
                    "preset_node_static_v2_manager",
                    "devEngines.packageManager must name one exact manager/version.",
                )),
            }
        })
        .transpose()?;
    let manager = match (&node.package_manager, dev_manager) {
        (Some(primary), secondary) => {
            let manager = resolve(Some(primary.clone()))?;
            if secondary.as_ref().is_some_and(|other| other != &manager) {
                return Err(refuse(
                    "preset_node_static_v2_manager_conflict",
                    "packageManager and devEngines.packageManager disagree.",
                ));
            }
            manager
        }
        (None, Some(manager)) => manager,
        (None, None) => resolve(None)?,
    };
    let lock_matches = match manager.name.as_str() {
        "pnpm" => node.has_pnpm_lock && !node.has_yarn_lock,
        "yarn" => node.has_yarn_lock && !node.has_pnpm_lock,
        _ => false,
    };
    if !lock_matches || node.has_package_lock || node.has_npm_shrinkwrap {
        return Err(refuse(
            "preset_node_static_v2_lock_conflict",
            "One matching manager lockfile is required.",
        ));
    }
    if manager.name == "yarn" && manager.version.starts_with("0.") {
        return Err(refuse(
            "preset_node_static_v2_manager",
            "Only Yarn 1 or >=2 is supported.",
        ));
    }
    let install_mode = if manager.name == "yarn" && !manager.version.starts_with("1.") {
        "--immutable"
    } else {
        "--frozen-lockfile"
    };
    Ok((manager, install_mode))
}
