//! Bounded, read-only repository evidence. This is not a second execution model:
//! only the canonical authoring compiler may turn evidence into D requirements.
//! File contents, credentials, shell commands and README instructions never leave
//! this scanner. A declaration is source evidence, never runtime verification.
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MAX_FILE_BYTES: u64 = 64 * 1024;
pub const MAX_FACTS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSignal {
    Python,
    Node,
    Rust,
    Oci,
    MultipleComponents,
    Web,
    NativeGui,
    Terminal,
    Gpu,
    Model,
    ExternalService,
    PersistentStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    /// A structured manifest declaration. Its presence was checked, not its behavior.
    Declaration,
    /// Lexical evidence only; may describe an example or optional feature.
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFact {
    pub signal: ExecutionSignal,
    pub file: String,
    pub strength: EvidenceStrength,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryEvidence {
    pub schema: String,
    pub facts: Vec<ExecutionFact>,
    /// Names only, never commands or credential values. These need authoring
    /// and existing ProposalValidator admission before they can become a route.
    pub entrypoint_names: Vec<String>,
    pub credential_names: Vec<String>,
    pub declared_container_ports: Vec<u16>,
    /// Files which could not be scanned completely. No truncated text is used.
    pub unscanned_files: Vec<String>,
    /// Source inspection cannot establish Surface readiness or C' |= K.
    pub runtime_verified: bool,
}

const FILES: &[&str] = &[
    "index.html",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "requirements.txt",
    "Dockerfile",
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
    "README.md",
    "README.rst",
    ".env.example",
    ".env.sample",
];

fn symbol(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn credential_name(name: &str) -> bool {
    symbol(name)
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && ["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD"]
            .iter()
            .any(|s| name.ends_with(s))
}

/// The root must be an already-authorized source directory. This function never
/// traverses child directories, follows source-file links, or reads `.env`.
pub fn inspect(root: &Path) -> std::io::Result<RepositoryEvidence> {
    let root_meta = std::fs::symlink_metadata(root)?;
    if !root_meta.is_dir() || root_meta.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "source root must be a regular directory",
        ));
    }
    let mut report = RepositoryEvidence {
        schema: "ato.formation-repository-evidence/1".into(),
        facts: Vec::new(),
        entrypoint_names: Vec::new(),
        credential_names: Vec::new(),
        declared_container_ports: Vec::new(),
        unscanned_files: Vec::new(),
        runtime_verified: false,
    };
    let mut facts = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut credentials = BTreeSet::new();
    let mut ports = BTreeSet::new();
    for file in FILES {
        let path = root.join(file);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            report.unscanned_files.push((*file).into());
            continue;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let Ok(text) = String::from_utf8(bytes) else {
            report.unscanned_files.push((*file).into());
            continue;
        };
        if text.len() as u64 > MAX_FILE_BYTES {
            report.unscanned_files.push((*file).into());
            continue;
        }
        let mut add = |signal, strength| {
            facts.insert(ExecutionFact {
                signal,
                file: (*file).into(),
                strength,
            });
        };
        use EvidenceStrength::{Declaration, Hint};
        use ExecutionSignal::*;
        match *file {
            "index.html" => add(Web, Declaration),
            "package.json" => {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    report.unscanned_files.push((*file).into());
                    continue;
                };
                add(Node, Declaration);
                for section in ["scripts", "bin"] {
                    if let Some(map) = value.get(section).and_then(|v| v.as_object()) {
                        names.extend(map.keys().filter(|k| symbol(k)).cloned());
                    }
                }
                for section in ["dependencies", "devDependencies", "peerDependencies"] {
                    if let Some(map) = value.get(section).and_then(|v| v.as_object()) {
                        for dependency in map.keys() {
                            dependency_signals(dependency, &mut add);
                        }
                    }
                }
            }
            "pyproject.toml" | "Cargo.toml" => {
                let Ok(value) = text.parse::<toml::Value>() else {
                    report.unscanned_files.push((*file).into());
                    continue;
                };
                add(
                    if *file == "Cargo.toml" { Rust } else { Python },
                    Declaration,
                );
                if let Some(table) = value
                    .get("project")
                    .and_then(|v| v.get("scripts"))
                    .and_then(|v| v.as_table())
                {
                    names.extend(table.keys().filter(|k| symbol(k)).cloned());
                }
                if let Some(dependencies) = value
                    .get("project")
                    .and_then(|v| v.get("dependencies"))
                    .and_then(|v| v.as_array())
                {
                    for dependency in dependencies.iter().filter_map(|v| v.as_str()) {
                        dependency_signals(dependency, &mut add);
                    }
                }
                for section in ["dependencies", "dev-dependencies"] {
                    if let Some(table) = value.get(section).and_then(|v| v.as_table()) {
                        for dependency in table.keys() {
                            dependency_signals(dependency, &mut add);
                        }
                    }
                }
            }
            "Dockerfile" => {
                add(Oci, Declaration);
                for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
                    let mut fields = line.split_whitespace();
                    match fields.next().unwrap_or("").to_ascii_uppercase().as_str() {
                        "EXPOSE" => {
                            for port in fields {
                                if let Ok(port) =
                                    port.strip_suffix("/tcp").unwrap_or(port).parse::<u16>()
                                    && port != 0
                                {
                                    ports.insert(port);
                                }
                            }
                        }
                        "VOLUME" => add(PersistentStorage, Declaration),
                        _ => {}
                    }
                }
            }
            "compose.yaml" | "compose.yml" | "docker-compose.yaml" | "docker-compose.yml" => {
                // Compose interpolation/includes/extends require its own
                // resolver. A marker does not claim a resolved service DAG.
                add(Oci, Hint);
                add(MultipleComponents, Hint);
            }
            ".env.example" | ".env.sample" => {
                for line in text.lines() {
                    let name = line
                        .trim()
                        .strip_prefix("export ")
                        .unwrap_or(line.trim())
                        .split_once('=')
                        .map(|(k, _)| k.trim());
                    if let Some(name) = name.filter(|n| credential_name(n)) {
                        credentials.insert(name.to_owned());
                        add(ExternalService, Hint);
                    }
                }
            }
            "requirements.txt" => {
                add(Python, Declaration);
                for line in text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.starts_with('#') && !l.starts_with('-'))
                {
                    dependency_signals(line, &mut add);
                }
            }
            _ => {}
        }
        if !file.starts_with(".env") {
            // Only fixed enum markers escape lexical scanning; raw source is
            // never returned. README is advisory and cannot grant execution.
            let lower = text.to_ascii_lowercase();
            for (tokens, signal) in [
                (&["cuda", "nvidia", "--gpus"][..], Gpu),
                (&["huggingface", "safetensors", "model weights"][..], Model),
                (&["x11", "wayland", "tigervnc", "novnc"][..], NativeGui),
                (&["terminal", "curses", "ratatui"][..], Terminal),
            ] {
                if tokens.iter().any(|t| lower.contains(t)) {
                    add(signal, Hint);
                }
            }
        }
    }
    report.facts = facts.into_iter().take(MAX_FACTS).collect();
    report.entrypoint_names = names.into_iter().take(64).collect();
    report.credential_names = credentials.into_iter().take(32).collect();
    report.declared_container_ports = ports.into_iter().take(32).collect();
    report.unscanned_files.sort();
    Ok(report)
}

fn dependency_signals(dependency: &str, add: &mut impl FnMut(ExecutionSignal, EvidenceStrength)) {
    let name = dependency
        .split(['<', '>', '=', '[', ' ', ';'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    use ExecutionSignal::*;
    let signal = match name.as_str() {
        "torch" | "tensorflow" | "jax" | "cupy" => Some(Gpu),
        "transformers" | "diffusers" | "huggingface-hub" | "huggingface_hub" => Some(Model),
        "pyqt5" | "pyqt6" | "pyside6" | "gtk" | "gtk4" | "egui" | "eframe" | "electron" => {
            Some(NativeGui)
        }
        "ratatui" | "crossterm" | "textual" | "rich" | "blessed" => Some(Terminal),
        "fastapi" | "flask" | "gradio" | "streamlit" | "express" | "next" | "vite" | "axum"
        | "actix-web" => Some(Web),
        "openai" | "anthropic" | "zep-cloud" | "zep_cloud" => Some(ExternalService),
        _ => None,
    };
    // Optional CPU paths and unused dependencies exist. A package name alone
    // must never be promoted into a mandatory GPU or provider selection.
    if let Some(signal) = signal {
        add(signal, EvidenceStrength::Hint);
    }
}
