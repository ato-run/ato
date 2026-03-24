use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use toml;

#[derive(Debug, Clone)]
pub struct DetectedProject {
    pub dir: PathBuf,
    pub name: String,
    pub project_type: ProjectType,
    pub node: Option<DetectedNode>,
}

#[derive(Debug, Clone, Copy)]
pub enum ProjectType {
    Python,
    NodeJs,
    Rust,
    Go,
    Ruby,
    Unknown,
}

impl ProjectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectType::Python => "Python",
            ProjectType::NodeJs => "Node.js",
            ProjectType::Rust => "Rust",
            ProjectType::Go => "Go",
            ProjectType::Ruby => "Ruby",
            ProjectType::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePackageManager {
    Bun,
    Npm,
    Pnpm,
    Yarn,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct NodeScripts {
    pub has_dev: bool,
    pub has_start: bool,
    pub has_build: bool,
}

#[derive(Debug, Clone)]
pub struct DetectedNode {
    pub package_manager: NodePackageManager,
    pub is_bun: bool,
    pub scripts: NodeScripts,
    pub main: Option<String>,
    pub has_hono: bool,
}

pub fn detect_project(dir: &Path) -> Result<DetectedProject> {
    let base_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_manifest_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "my-capsule".to_string());

    let mut name = base_name;

    if dir.join("requirements.txt").exists()
        || dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
    {
        return Ok(DetectedProject {
            dir: dir.to_path_buf(),
            name,
            project_type: ProjectType::Python,
            node: None,
        });
    }

    if dir.join("package.json").exists() {
        let node = detect_node(dir)?;
        return Ok(DetectedProject {
            dir: dir.to_path_buf(),
            name,
            project_type: ProjectType::NodeJs,
            node: Some(node),
        });
    }

    if dir.join("Cargo.toml").exists() {
        // Prefer Cargo package name when available (more accurate for Rust binary name).
        if let Some(pkg) = detect_cargo_package_name(dir) {
            name = pkg;
        }
        return Ok(DetectedProject {
            dir: dir.to_path_buf(),
            name,
            project_type: ProjectType::Rust,
            node: None,
        });
    }

    if dir.join("go.mod").exists() {
        return Ok(DetectedProject {
            dir: dir.to_path_buf(),
            name,
            project_type: ProjectType::Go,
            node: None,
        });
    }

    if dir.join("Gemfile").exists() {
        return Ok(DetectedProject {
            dir: dir.to_path_buf(),
            name,
            project_type: ProjectType::Ruby,
            node: None,
        });
    }

    Ok(DetectedProject {
        dir: dir.to_path_buf(),
        name,
        project_type: ProjectType::Unknown,
        node: None,
    })
}

fn detect_cargo_package_name(dir: &Path) -> Option<String> {
    let cargo_toml_path = dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path).ok()?;
    let value = toml::from_str::<toml::Value>(&content).ok()?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(sanitize_manifest_name)
        .filter(|s| !s.is_empty())
}

fn sanitize_manifest_name(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn detect_node(dir: &Path) -> Result<DetectedNode> {
    let package_json_path = dir.join("package.json");
    let content = fs::read_to_string(&package_json_path).context("Failed to read package.json")?;

    let bun_project = dir.join("bun.lockb").exists() || dir.join("bunfig.toml").exists();
    let pnpm_project = dir.join("pnpm-lock.yaml").exists();
    let yarn_project = dir.join("yarn.lock").exists();
    let _npm_project = dir.join("package-lock.json").exists();

    let mut pm = NodePackageManager::Unknown;
    let mut scripts = NodeScripts {
        has_dev: false,
        has_start: false,
        has_build: false,
    };
    let mut main: Option<String> = None;
    let mut has_hono = false;

    if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
        let package_manager = pkg
            .get("packageManager")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default();

        pm = if package_manager.starts_with("bun@") {
            NodePackageManager::Bun
        } else if package_manager.starts_with("pnpm@") {
            NodePackageManager::Pnpm
        } else if package_manager.starts_with("yarn@") {
            NodePackageManager::Yarn
        } else if package_manager.starts_with("npm@") {
            NodePackageManager::Npm
        } else {
            NodePackageManager::Unknown
        };

        if let Some(s) = pkg.get("scripts") {
            scripts.has_start = s.get("start").is_some();
            scripts.has_dev = s.get("dev").is_some();
            scripts.has_build = s.get("build").is_some();
        }

        main = pkg
            .get("main")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());

        let has_dep = |key: &str| -> bool {
            pkg.get(key)
                .and_then(|deps| deps.as_object())
                .map(|deps| deps.contains_key("hono"))
                .unwrap_or(false)
        };
        has_hono = has_dep("dependencies") || has_dep("devDependencies");
    }

    if pm == NodePackageManager::Unknown {
        pm = if bun_project {
            NodePackageManager::Bun
        } else if pnpm_project {
            NodePackageManager::Pnpm
        } else if yarn_project {
            NodePackageManager::Yarn
        } else {
            NodePackageManager::Npm
        };
    }

    let is_bun = pm == NodePackageManager::Bun || bun_project;

    Ok(DetectedNode {
        package_manager: pm,
        is_bun,
        scripts,
        main,
        has_hono,
    })
}
