//! Detection: source tree in, EVIDENCE out. Nothing else.
//!
//! A detector may read files. It may not reach the network, install a
//! dependency, run a build, start a process or create a Run — and it does not
//! decide anything either. It reports what it saw, and the intent compiler
//! decides, so the two can be argued about separately.
//!
//! ## What is deliberately NOT inferred
//!
//! FastAPI. The ASGI application object. A uvicorn entrypoint. A SQLite path.
//! A `/data` state slot.
//!
//! Each is a guess that looks harmless and is not. Guessing a state path means
//! an app writes somewhere that is silently discarded on the next Run; guessing
//! an entrypoint means launching something the author never asked to launch.
//! B1 requires these to be authored, and refuses rather than assumes.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// What a detector saw. Facts, not conclusions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorEvidence {
    /// Repository-relative paths that exist, sorted.
    pub present_files: Vec<String>,
    /// Python-specific observations, when any Python marker is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonEvidence>,
    /// Static-web observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_web: Option<StaticWebEvidence>,
    /// Node observations. Collected, but B1 implements no Node lane — recording
    /// it now means the evidence format does not change when the lane lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeEvidence>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonEvidence {
    pub has_pyproject: bool,
    pub has_uv_lock: bool,
    pub has_requirements_txt: bool,
    pub has_poetry_lock: bool,
    pub has_pipfile_lock: bool,
    /// Verbatim contents of `.python-version`, trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_version_file: Option<String>,
    /// Verbatim `project.requires-python`, unparsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_python: Option<String>,
    /// Top-level `.py` files, sorted.
    pub top_level_modules: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebEvidence {
    /// An HTML file at the repository root.
    pub has_root_index_html: bool,
    /// Directories that LOOK like build outputs. Evidence only — see below.
    pub candidate_output_dirs: Vec<String>,
    pub has_package_json: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEvidence {
    pub has_package_json: bool,
    pub has_package_lock: bool,
    pub has_pnpm_lock: bool,
    pub has_yarn_lock: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_version_file: Option<String>,
}

const PYTHON_MARKERS: &[&str] = &[
    "pyproject.toml",
    "requirements.txt",
    "uv.lock",
    "poetry.lock",
    "Pipfile.lock",
    ".python-version",
];

const OUTPUT_DIR_CANDIDATES: &[&str] = &["dist", "build", "out", "public", "_site"];

/// Read a source tree and report what is there.
///
/// Only the root is inspected for markers. A `pyproject.toml` six directories
/// down belongs to a vendored package, not to this project, and treating it as
/// a signal is how a repository gets built as something it is not.
pub fn detect(root: &Path) -> std::io::Result<DetectorEvidence> {
    let mut present = Vec::new();
    let mut top_level_modules = Vec::new();
    let mut candidate_output_dirs = Vec::new();

    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // `symlink_metadata`: a link is not followed. Following one would let a
        // link in the source decide what the project is.
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if OUTPUT_DIR_CANDIDATES.contains(&name.as_str()) {
                candidate_output_dirs.push(name.clone());
            }
            continue;
        }
        if name.ends_with(".py") {
            top_level_modules.push(name.clone());
        }
        present.push(name);
    }
    present.sort();
    top_level_modules.sort();
    candidate_output_dirs.sort();

    let has = |name: &str| present.iter().any(|entry| entry == name);
    let read_trimmed = |name: &str| -> Option<String> {
        std::fs::read_to_string(root.join(name))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    };

    let python = PYTHON_MARKERS.iter().any(|marker| has(marker)).then(|| {
        let requires_python = read_trimmed("pyproject.toml")
            .as_deref()
            .and_then(extract_requires_python);
        PythonEvidence {
            has_pyproject: has("pyproject.toml"),
            has_uv_lock: has("uv.lock"),
            has_requirements_txt: has("requirements.txt"),
            has_poetry_lock: has("poetry.lock"),
            has_pipfile_lock: has("Pipfile.lock"),
            python_version_file: read_trimmed(".python-version"),
            requires_python,
            top_level_modules: top_level_modules.clone(),
        }
    });

    let static_web = Some(StaticWebEvidence {
        has_root_index_html: has("index.html"),
        candidate_output_dirs: candidate_output_dirs.clone(),
        has_package_json: has("package.json"),
    });

    let node = has("package.json").then(|| NodeEvidence {
        has_package_json: true,
        has_package_lock: has("package-lock.json"),
        has_pnpm_lock: has("pnpm-lock.yaml"),
        has_yarn_lock: has("yarn.lock"),
        node_version_file: read_trimmed(".nvmrc"),
    });

    Ok(DetectorEvidence {
        present_files: present,
        python,
        static_web,
        node,
    })
}

/// Pull `requires-python` out of a pyproject without a TOML parser.
///
/// Deliberately shallow: this is EVIDENCE, reported verbatim and unparsed. The
/// intent compiler decides what a constraint means, and a detector that
/// interpreted it would be deciding.
fn extract_requires_python(pyproject: &str) -> Option<String> {
    let mut in_project = false;
    for line in pyproject.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_project = line == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(rest) = line.strip_prefix("requires-python") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            let value = value.trim_matches(['"', '\''].as_ref());
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Where each field of an intent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldOrigin {
    Authored,
    DetectedFromSource,
    PolicyDefault,
}

/// The origins, gathered as the compiler decides each field.
pub type FieldOrigins = BTreeMap<String, FieldOrigin>;
