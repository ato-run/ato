//! Bounded workspace inventory: source tree in, workspace FACTS out.
//!
//! This is not monorepo understanding. It expands only the root
//! `package.json` `workspaces` declaration with a narrow glob grammar, reads
//! each matched workspace's own manifest without executing anything, and
//! reports whether that workspace qualifies for the existing narrow static
//! build rule. It never selects a workspace: selection is an owner-authorized
//! typed operation over opaque IDs (`node_static_workspace@1`).
//!
//! No network, no package scripts, no source code evaluation. Symlinks are
//! never followed. Anything outside the grammar is refused, not approximated.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{DetectorEvidence, NodeEvidence, ViteOutDir, detect};
use crate::intent::{self, SERVER_FRAMEWORKS};
use crate::preset::{PresetMismatch, node_static_v2};
use crate::proposal::{
    MAX_WORKSPACE_CANDIDATES, NodeStaticWorkspaceAuthorization, WorkspaceInstallScope,
    WorkspaceStaticBuild,
};

pub const WORKSPACE_INVENTORY_SCHEMA: &str = "ato.formation-workspace-inventory/1";
/// Declared patterns read from the root manifest.
pub const MAX_WORKSPACE_PATTERNS: usize = 32;
/// Path segments per pattern.
pub const MAX_PATTERN_SEGMENTS: usize = 4;
/// Directory entries examined while expanding `*` segments, in total.
pub const MAX_SCANNED_ENTRIES: usize = 4096;
/// Any manifest this inventory parses.
pub const MAX_PACKAGE_JSON_BYTES: u64 = 1024 * 1024;

/// Refusals of the whole inventory. No candidate is reported on any of them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InventoryError {
    #[error("the root package.json declares no workspaces")]
    Absent,
    #[error("pnpm workspace configuration is not read by this inventory")]
    PnpmDeferred,
    #[error("the workspaces declaration is not a string array or {{ packages = [...] }}")]
    DeclarationUnsupported,
    #[error("workspace pattern {0:?} is outside the literal / whole-segment `*` grammar")]
    PatternUnsupported(String),
    #[error("workspace pattern or match {0:?} leaves the source closure or crosses a symlink")]
    PathEscape(String),
    #[error("the workspace inventory exceeds its bounds")]
    Bounds,
    #[error("the root package.json cannot be read: {0}")]
    Unreadable(String),
}
impl InventoryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Absent => "workspace_absent",
            Self::PnpmDeferred => "workspace_pnpm_deferred",
            Self::DeclarationUnsupported => "workspace_declaration_unsupported",
            Self::PatternUnsupported(_) => "workspace_pattern_unsupported",
            Self::PathEscape(_) => "workspace_path_escape",
            Self::Bounds => "workspace_inventory_bounds",
            Self::Unreadable(_) => "workspace_root_unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceInventory {
    pub schema: String,
    /// Declared patterns, verbatim.
    pub patterns: Vec<String>,
    /// The one root installation scope, or why it cannot be authored exactly.
    pub installation: Installation,
    /// Every matched workspace, sorted by path, with typed qualification.
    pub workspaces: Vec<WorkspaceFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Installation {
    Exact { scope: WorkspaceInstallScope },
    Refused { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFact {
    /// Source-relative. Private to Ato; providers see only opaque IDs.
    pub relative_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    pub qualification: Qualification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Qualification {
    StaticQualified { output_root: String },
    Excluded { code: String },
}

impl WorkspaceInventory {
    /// Owner-authorizable domain: static-qualified workspaces only, under an
    /// exact installation scope. IDs are ordinal in sorted path order and are
    /// meaningful only inside this frozen source.
    pub fn authorization(&self) -> Option<NodeStaticWorkspaceAuthorization> {
        let Installation::Exact { scope } = &self.installation else {
            return None;
        };
        let workspaces: std::collections::BTreeMap<_, _> = self
            .workspaces
            .iter()
            .filter_map(|w| match &w.qualification {
                Qualification::StaticQualified { output_root } => Some(WorkspaceStaticBuild {
                    cwd: w.relative_root.clone(),
                    output_root: output_root.clone(),
                }),
                Qualification::Excluded { .. } => None,
            })
            .enumerate()
            .map(|(n, build)| (format!("w_{n}"), build))
            .collect();
        let authorization = NodeStaticWorkspaceAuthorization {
            install: scope.clone(),
            workspaces,
        };
        authorization.validate().ok().map(|()| authorization)
    }
}

fn read_manifest(path: &Path) -> Result<serde_json::Value, &'static str> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "workspace_package_json_invalid")?;
    if !meta.file_type().is_file() {
        return Err("workspace_symlink_input");
    }
    if meta.len() > MAX_PACKAGE_JSON_BYTES {
        return Err("workspace_package_json_bounds");
    }
    let value: serde_json::Value = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("workspace_package_json_invalid")?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("workspace_package_json_invalid")
    }
}

/// Literal names use the same narrow grammar the authorization accepts.
fn literal_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'@'))
}

fn parse_pattern(raw: &str) -> Result<Vec<String>, InventoryError> {
    let escape = || InventoryError::PathEscape(raw.to_owned());
    if raw.starts_with('/') || raw.contains('\\') || raw.contains('\0') {
        return Err(escape());
    }
    let trimmed = raw.strip_prefix("./").unwrap_or(raw);
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let segments: Vec<&str> = trimmed.split('/').collect();
    if segments.contains(&"..") {
        return Err(escape());
    }
    if segments.is_empty() || segments.len() > MAX_PATTERN_SEGMENTS {
        return Err(InventoryError::PatternUnsupported(raw.to_owned()));
    }
    segments
        .into_iter()
        .map(|segment| {
            if segment == "*" || literal_segment(segment) {
                Ok(segment.to_owned())
            } else {
                Err(InventoryError::PatternUnsupported(raw.to_owned()))
            }
        })
        .collect()
}

/// Expand one parsed pattern to directories, never following a symlink.
fn expand(
    root: &Path,
    raw: &str,
    segments: &[String],
    scanned: &mut usize,
) -> Result<Vec<String>, InventoryError> {
    let escape = |p: &str| InventoryError::PathEscape(format!("{raw} -> {p}"));
    let mut frontier = vec![String::new()];
    for segment in segments {
        let mut next = Vec::new();
        for parent in &frontier {
            let names: Vec<String> = if segment == "*" {
                let dir = root.join(parent);
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                let mut names = Vec::new();
                for entry in entries {
                    *scanned += 1;
                    if *scanned > MAX_SCANNED_ENTRIES {
                        return Err(InventoryError::Bounds);
                    }
                    let entry = entry.map_err(|_| InventoryError::Bounds)?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    // Globs do not match dot-directories or installed trees.
                    if name.starts_with('.') || name == "node_modules" {
                        continue;
                    }
                    names.push(name);
                }
                names.sort();
                names
            } else {
                vec![segment.clone()]
            };
            for name in names {
                let rel = if parent.is_empty() {
                    name.clone()
                } else {
                    format!("{parent}/{name}")
                };
                let Ok(meta) = std::fs::symlink_metadata(root.join(&rel)) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    return Err(escape(&rel));
                }
                if meta.is_dir() {
                    next.push(rel);
                }
            }
        }
        frontier = next;
    }
    Ok(frontier)
}

fn installation(evidence: &DetectorEvidence, node: &NodeEvidence) -> Installation {
    let refused = |m: PresetMismatch| Installation::Refused {
        code: m.code.to_owned(),
    };
    if node.has_bun_lock {
        return Installation::Refused {
            code: "preset_node_static_v2_bun_deferred".into(),
        };
    }
    if let Err(m) = node_static_v2::refuse_manager_config(evidence) {
        return refused(m);
    }
    let node_version = match intent::resolve_node_version(node) {
        Ok(v) => v,
        Err(e) => {
            return Installation::Refused {
                code: e.code().into(),
            };
        }
    };
    match node_static_v2::exact_manager(node, &node_version) {
        Ok((manager, mode)) => Installation::Exact {
            scope: WorkspaceInstallScope {
                node_version,
                package_manager: manager.name,
                package_manager_version: manager.version,
                install_mode: mode.into(),
            },
        },
        Err(m) => refused(m),
    }
}

fn declares_node(node: &NodeEvidence) -> bool {
    node.node_version_file.is_some()
        || node.node_version_alt_file.is_some()
        || node.volta_node.is_some()
        || node.engines_node.is_some()
}

/// Why a workspace is not a standalone static build candidate, or its
/// source-relative output root. Reuses the root-level static-build detector.
fn qualify(
    root: &Path,
    relative_root: &str,
    manifest: &serde_json::Value,
    installation: &Installation,
) -> Result<String, &'static str> {
    let dir = root.join(relative_root);
    if !crate::proposal::relative_dir(relative_root) {
        return Err("workspace_path_unsupported");
    }
    // Every file the detector reads must be a regular file of this directory.
    for name in [".nvmrc", ".node-version", "index.html"]
        .into_iter()
        .chain([
            "vite.config.ts",
            "vite.config.js",
            "vite.config.mts",
            "vite.config.mjs",
        ])
    {
        if std::fs::symlink_metadata(dir.join(name)).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("workspace_symlink_input");
        }
    }
    let evidence = detect(&dir).map_err(|_| "workspace_package_json_invalid")?;
    let node = evidence
        .node
        .as_ref()
        .ok_or("workspace_package_json_invalid")?;
    if node.has_package_lock
        || node.has_npm_shrinkwrap
        || node.has_pnpm_lock
        || node.has_yarn_lock
        || node.has_bun_lock
    {
        return Err("workspace_nested_lock");
    }
    if node_static_v2::refuse_manager_config(&evidence).is_err() {
        return Err("workspace_manager_config");
    }
    if node.has_workspace {
        return Err("workspace_nested_workspaces");
    }
    if let Installation::Exact { scope } = installation {
        let pin = |name: &str, version: &str| {
            name == scope.package_manager
                && version.split('+').next() == Some(scope.package_manager_version.as_str())
        };
        if node.package_manager.as_deref().is_some_and(|declared| {
            let (name, version) = declared.split_once('@').unwrap_or((declared, ""));
            !pin(name, version)
        }) || node
            .dev_engines_package_manager
            .as_ref()
            .is_some_and(|value| {
                !pin(
                    value
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default(),
                    value
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default(),
                )
            })
        {
            return Err("workspace_manager_conflict");
        }
        if declares_node(node)
            && intent::resolve_node_version(node).ok().as_ref() != Some(&scope.node_version)
        {
            return Err("workspace_node_version_conflict");
        }
    }
    if manifest
        .get("scripts")
        .and_then(|s| s.get("build"))
        .and_then(|b| b.as_str())
        .is_none()
    {
        return Err("workspace_build_script_missing");
    }
    if node
        .dependency_names
        .iter()
        .any(|name| SERVER_FRAMEWORKS.contains(&name.as_str()))
    {
        return Err("workspace_server_dependency");
    }
    if let ViteOutDir::Literal(out) = &node.vite_out_dir
        && (out.is_empty()
            || out.starts_with('/')
            || crate::projection::workspace_relative_cwd(out).map_or(true, |p| p.is_empty()))
    {
        return Err("workspace_output_ambiguous");
    }
    let profile = match intent::detect_static_build(&evidence) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err("workspace_static_unproven"),
        Err(intent::IntentError::RequiresAuthoring { .. }) => {
            return Err("workspace_output_ambiguous");
        }
        Err(_) => return Err("workspace_static_unproven"),
    };
    if !evidence.present_files.iter().any(|f| f == "index.html") {
        return Err("workspace_entry_missing");
    }
    let output = crate::projection::workspace_relative_cwd(&profile.output_root)
        .map_err(|_| "workspace_output_ambiguous")?;
    let output_root = format!("{relative_root}/{output}");
    if output.is_empty() || !crate::proposal::relative_dir(&output_root) {
        return Err("workspace_output_ambiguous");
    }
    Ok(output_root)
}

/// Build the bounded inventory for `root`, a frozen source closure.
pub fn inventory(root: &Path) -> Result<WorkspaceInventory, InventoryError> {
    let root: PathBuf = root.to_path_buf();
    let manifest = read_manifest(&root.join("package.json"))
        .map_err(|code| InventoryError::Unreadable(code.into()))?;
    let evidence = detect(&root).map_err(|e| InventoryError::Unreadable(e.to_string()))?;
    let node = evidence
        .node
        .as_ref()
        .ok_or_else(|| InventoryError::Unreadable("no package.json".into()))?;
    if evidence
        .present_files
        .iter()
        .any(|f| f == "pnpm-workspace.yaml")
    {
        return Err(InventoryError::PnpmDeferred);
    }
    let declared = manifest.get("workspaces").ok_or(InventoryError::Absent)?;
    let list = match declared {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) if map.len() == 1 => map
            .get("packages")
            .and_then(|p| p.as_array())
            .ok_or(InventoryError::DeclarationUnsupported)?,
        _ => return Err(InventoryError::DeclarationUnsupported),
    };
    if list.is_empty() || list.len() > MAX_WORKSPACE_PATTERNS {
        return Err(InventoryError::Bounds);
    }
    let patterns: Vec<String> = list
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_owned)
                .ok_or(InventoryError::DeclarationUnsupported)
        })
        .collect::<Result<_, _>>()?;
    let mut matched = std::collections::BTreeSet::new();
    let mut scanned = 0;
    for raw in &patterns {
        let segments = parse_pattern(raw)?;
        for dir in expand(&root, raw, &segments, &mut scanned)? {
            // Package managers treat a match without a manifest as no workspace.
            if std::fs::symlink_metadata(root.join(&dir).join("package.json")).is_ok() {
                matched.insert(dir);
            }
        }
    }
    if matched.len() > MAX_WORKSPACE_CANDIDATES {
        return Err(InventoryError::Bounds);
    }
    let installation = installation(&evidence, node);
    let workspaces = matched
        .into_iter()
        .map(|relative_root| {
            let manifest = read_manifest(&root.join(&relative_root).join("package.json"));
            let package_name = manifest
                .as_ref()
                .ok()
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str())
                .filter(|n| n.len() <= 214)
                .map(str::to_owned);
            let qualification = manifest
                .and_then(|m| qualify(&root, &relative_root, &m, &installation))
                .map_or_else(
                    |code| Qualification::Excluded { code: code.into() },
                    |output_root| Qualification::StaticQualified { output_root },
                );
            WorkspaceFact {
                relative_root,
                package_name,
                qualification,
            }
        })
        .collect();
    Ok(WorkspaceInventory {
        schema: WORKSPACE_INVENTORY_SCHEMA.into(),
        patterns,
        installation,
        workspaces,
    })
}

#[cfg(test)]
mod tests {
    use crate::authoring::{BindingContext, bind};
    use crate::intent::ResolvedPackageManager;
    use crate::preset::node_static_v2::static_build_draft;
    use crate::proposal::*;
    use crate::search::{FrozenSearchV1, InitialSource};

    /// The typed operation and the known-D v2 builder share one canonical D
    /// shape: same install/build/serve, only the Ato-resolved scope differs.
    #[test]
    fn proposal_d_equals_the_v2_builder_for_the_same_resolved_scope() {
        let closure = format!("sha256:{}", "a".repeat(64));
        let ctx = BindingContext {
            source_closure_ref: &closure,
        };
        for (cwd, out) in [("apps/web", "apps/web/dist"), ("site", "site/build")] {
            let draft = static_build_draft(
                "20.20.2".into(),
                ResolvedPackageManager {
                    name: "yarn".into(),
                    version: "1.22.22".into(),
                },
                "--frozen-lockfile",
                cwd,
                out.into(),
            );
            let (k, expected) = bind(&draft, &ctx).unwrap();
            let auth = NodeStaticWorkspaceAuthorization {
                install: WorkspaceInstallScope {
                    node_version: "20.20.2".into(),
                    package_manager: "yarn".into(),
                    package_manager_version: "1.22.22".into(),
                    install_mode: "--frozen-lockfile".into(),
                },
                workspaces: [(
                    "w_0".to_owned(),
                    WorkspaceStaticBuild {
                        cwd: cwd.into(),
                        output_root: out.into(),
                    },
                )]
                .into(),
            };
            let mut state: crate::search::SearchStateV1 = serde_json::from_str(include_str!(
                "../tests/fixtures/search-state/d1-failed.json"
            ))
            .unwrap();
            let frozen: &mut FrozenSearchV1 = &mut state.frozen;
            frozen.candidates.clear();
            frozen.base_contract_ref = k.contract_ref().unwrap();
            frozen.contract_ref = frozen.base_contract_ref.clone();
            frozen.base_contract = k;
            frozen.initial_source = Some(InitialSource {
                closure_ref: closure.clone(),
                archive_digest: closure.clone(),
            });
            frozen.policy.proposal = Some(ProposalAuthorization {
                execution_plan: None,
                modifiable_derivation_refs: vec![],
                source_domain: SourceDomain {
                    entrypoints: Default::default(),
                    modules: Default::default(),
                },
                python_http_process: None,
                node_static_workspace: Some(auth),
                policy: CandidateProducerPolicy {
                    max_proposal_rounds: 1,
                    max_proposals: 1,
                    timeout_ms: 1000,
                    allow_source_text: false,
                    max_source_bytes: 0,
                },
            });
            let batch = br#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"node_static_workspace@1","workspace_id":"w_0"}]}]}"#;
            let out = CandidateRegistry::new(frozen)
                .unwrap()
                .validate_batch(
                    &Default::default(),
                    &ProducerOutput::new(
                        batch.to_vec(),
                        ProducerProvenance {
                            provider: "fixed".into(),
                            model: None,
                        },
                    )
                    .unwrap(),
                )
                .unwrap();
            let [ProposalOutcome::Admitted(candidate)] = out.as_slice() else {
                panic!("{out:?}")
            };
            assert_eq!(candidate.compiled().derivation, expected);
            assert_eq!(
                candidate.compiled().derivation_ref,
                expected.derivation_ref().unwrap()
            );
        }
    }
}
