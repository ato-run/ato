use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CapsuleError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImporterId {
    Uv,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Deno,
    Cargo,
    Go,
    Poetry,
    Tauri,
    Electron,
    Wails,
}

impl ImporterId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uv => "uv",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
            Self::Deno => "deno",
            Self::Cargo => "cargo",
            Self::Go => "go",
            Self::Poetry => "poetry",
            Self::Tauri => "tauri",
            Self::Electron => "electron",
            Self::Wails => "wails",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Lockfile,
    FrameworkConfig,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lockfile => "lockfile",
            Self::FrameworkConfig => "framework_config",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedEvidence {
    pub importer_id: ImporterId,
    pub evidence_kind: EvidenceKind,
    pub paths: Vec<PathBuf>,
    pub primary_path: PathBuf,
    pub digest: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeMissing {
    pub importer_id: ImporterId,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAmbiguity {
    pub importer_ids: Vec<ImporterId>,
    pub paths: Vec<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    Found(Vec<ImportedEvidence>),
    Missing(ProbeMissing),
    Ambiguous(ProbeAmbiguity),
    NotApplicable,
}

pub fn probe_ecosystem_lockfile_evidence(project_root: &Path) -> Result<Vec<ImportedEvidence>> {
    let mut evidence = Vec::new();
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Uv,
        EvidenceKind::Lockfile,
        &[PathBuf::from("uv.lock"), PathBuf::from("source/uv.lock")],
        Some("uv importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Npm,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("package-lock.json"),
            PathBuf::from("npm-shrinkwrap.json"),
            PathBuf::from("source/package-lock.json"),
            PathBuf::from("source/npm-shrinkwrap.json"),
        ],
        Some("npm importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Pnpm,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("pnpm-lock.yaml"),
            PathBuf::from("source/pnpm-lock.yaml"),
        ],
        Some("pnpm importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Yarn,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("yarn.lock"),
            PathBuf::from("source/yarn.lock"),
        ],
        Some("yarn importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Bun,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("bun.lock"),
            PathBuf::from("bun.lockb"),
            PathBuf::from("source/bun.lock"),
            PathBuf::from("source/bun.lockb"),
        ],
        Some("bun importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Deno,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("deno.lock"),
            PathBuf::from("source/deno.lock"),
        ],
        Some("deno importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Cargo,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("Cargo.lock"),
            PathBuf::from("src-tauri/Cargo.lock"),
            PathBuf::from("source/Cargo.lock"),
            PathBuf::from("source/src-tauri/Cargo.lock"),
        ],
        Some("cargo importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Go,
        EvidenceKind::Lockfile,
        &[PathBuf::from("go.sum"), PathBuf::from("source/go.sum")],
        Some("go importer observed an existing lockfile"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Poetry,
        EvidenceKind::Lockfile,
        &[
            PathBuf::from("poetry.lock"),
            PathBuf::from("source/poetry.lock"),
        ],
        Some("poetry importer observed an existing lockfile"),
    )?;
    evidence.sort_by(|left, right| {
        left.importer_id
            .cmp(&right.importer_id)
            .then_with(|| left.primary_path.cmp(&right.primary_path))
    });
    Ok(evidence)
}

pub fn probe_required_node_lockfile(project_root: &Path) -> Result<ProbeResult> {
    let candidates = probe_ecosystem_lockfile_evidence(project_root)?
        .into_iter()
        .filter(|evidence| {
            matches!(
                evidence.importer_id,
                ImporterId::Npm | ImporterId::Yarn | ImporterId::Pnpm | ImporterId::Bun
            )
        })
        .collect::<Vec<_>>();
    classify_required_probe(
        candidates,
        ImporterId::Npm,
        "source/node target requires one of package-lock.json, npm-shrinkwrap.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb",
        "multiple node lockfiles detected; keep only one of package-lock.json, npm-shrinkwrap.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb",
    )
}

pub fn probe_required_python_lockfile(project_root: &Path) -> Result<ProbeResult> {
    classify_single_importer(
        project_root,
        ImporterId::Uv,
        "source/python target requires uv.lock for fail-closed provisioning",
    )
}

pub fn probe_required_deno_lockfile(project_root: &Path) -> Result<ProbeResult> {
    classify_single_importer(
        project_root,
        ImporterId::Deno,
        "deno.lock is required for fail-closed provisioning",
    )
}

pub fn probe_required_cargo_lockfile(project_root: &Path) -> Result<ProbeResult> {
    classify_single_importer(
        project_root,
        ImporterId::Cargo,
        "Cargo.lock is required for fail-closed native provisioning",
    )
}

pub fn probe_native_framework_evidence(project_root: &Path) -> Result<Vec<ImportedEvidence>> {
    let mut evidence = Vec::new();
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Tauri,
        EvidenceKind::FrameworkConfig,
        &[
            PathBuf::from("src-tauri/Cargo.toml"),
            PathBuf::from("tauri.conf.json"),
            PathBuf::from("src-tauri/tauri.conf.json"),
        ],
        Some("tauri framework adapter observed native delivery build metadata"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Electron,
        EvidenceKind::FrameworkConfig,
        &[
            PathBuf::from("electron-builder.json"),
            PathBuf::from("electron-builder.yml"),
            PathBuf::from("electron-builder.yaml"),
            PathBuf::from("forge.config.js"),
            PathBuf::from("forge.config.ts"),
        ],
        Some("electron framework adapter observed desktop packaging metadata"),
    )?;
    push_if_exists(
        &mut evidence,
        project_root,
        ImporterId::Wails,
        EvidenceKind::FrameworkConfig,
        &[
            PathBuf::from("wails.json"),
            PathBuf::from("build/appicon.png"),
        ],
        Some("wails framework adapter observed desktop packaging metadata"),
    )?;
    evidence.sort_by(|left, right| {
        left.importer_id
            .cmp(&right.importer_id)
            .then_with(|| left.primary_path.cmp(&right.primary_path))
    });
    Ok(evidence)
}

fn classify_single_importer(
    project_root: &Path,
    importer_id: ImporterId,
    missing_message: &str,
) -> Result<ProbeResult> {
    let matches = probe_ecosystem_lockfile_evidence(project_root)?
        .into_iter()
        .filter(|evidence| evidence.importer_id == importer_id)
        .collect::<Vec<_>>();
    classify_required_probe(matches, importer_id, missing_message, missing_message)
}

fn classify_required_probe(
    matches: Vec<ImportedEvidence>,
    missing_importer_id: ImporterId,
    missing_message: &str,
    ambiguity_message: &str,
) -> Result<ProbeResult> {
    Ok(match matches.len() {
        0 => ProbeResult::Missing(ProbeMissing {
            importer_id: missing_importer_id,
            message: missing_message.to_string(),
        }),
        1 => ProbeResult::Found(matches),
        _ => ProbeResult::Ambiguous(ProbeAmbiguity {
            importer_ids: matches.iter().map(|value| value.importer_id).collect(),
            paths: matches
                .iter()
                .map(|value| value.primary_path.clone())
                .collect(),
            message: ambiguity_message.to_string(),
        }),
    })
}

fn push_if_exists(
    entries: &mut Vec<ImportedEvidence>,
    project_root: &Path,
    importer_id: ImporterId,
    evidence_kind: EvidenceKind,
    candidates: &[PathBuf],
    provenance_note: Option<&str>,
) -> Result<()> {
    let existing = candidates
        .iter()
        .map(|path| project_root.join(path))
        .filter(|path| path.exists() && path.is_file())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        return Ok(());
    }

    let primary_path = existing[0].clone();
    let digest = digest_paths(&existing)?;
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "paths".to_string(),
        Value::Array(
            existing
                .iter()
                .map(|path| Value::String(path.display().to_string()))
                .collect(),
        ),
    );
    metadata.insert(
        "importer_id".to_string(),
        Value::String(importer_id.as_str().to_string()),
    );
    metadata.insert(
        "evidence_kind".to_string(),
        Value::String(evidence_kind.as_str().to_string()),
    );
    entries.push(ImportedEvidence {
        importer_id,
        evidence_kind,
        paths: existing,
        primary_path,
        digest,
        metadata,
        provenance_note: provenance_note.map(str::to_string),
    });
    Ok(())
}

fn digest_paths(paths: &[PathBuf]) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    for path in paths {
        let bytes = fs::read(path).map_err(|err| {
            CapsuleError::Config(format!(
                "Failed to read importer evidence {}: {err}",
                path.display()
            ))
        })?;
        hasher.update(path.display().to_string().as_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probes_existing_uv_lock() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("uv.lock"), "version = 1").expect("write uv.lock");

        let evidence = probe_ecosystem_lockfile_evidence(tmp.path()).expect("probe");
        assert!(evidence
            .iter()
            .any(|value| value.importer_id == ImporterId::Uv));
    }

    #[test]
    fn required_node_lockfile_fails_on_ambiguity() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("package-lock.json"), "{}").expect("write npm lock");
        fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let result = probe_required_node_lockfile(tmp.path()).expect("probe");
        assert!(matches!(result, ProbeResult::Ambiguous(_)));
    }

    #[test]
    fn required_python_lockfile_reports_missing() {
        let tmp = tempdir().expect("tempdir");

        let result = probe_required_python_lockfile(tmp.path()).expect("probe");
        assert!(matches!(result, ProbeResult::Missing(_)));
    }

    #[test]
    fn probes_nested_tauri_cargo_lock() {
        let tmp = tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("src-tauri")).expect("create src-tauri");
        fs::write(tmp.path().join("src-tauri/Cargo.lock"), "version = 3\n")
            .expect("write nested cargo lock");

        let evidence = probe_ecosystem_lockfile_evidence(tmp.path()).expect("probe");
        assert!(evidence.iter().any(|value| {
            value.importer_id == ImporterId::Cargo
                && value.primary_path == tmp.path().join("src-tauri/Cargo.lock")
        }));
    }
}
