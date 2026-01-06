//! Init command - initialize existing project as a Capsule
//!
//! Detects project type and generates capsule.toml with sensible defaults.
//! Supports: Python, Node.js, Rust, Go, Ruby, and generic projects.

use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Arguments for the init command
pub struct InitArgs {
    /// Target directory (default: current directory)
    pub path: Option<PathBuf>,
    /// Non-interactive mode (use detected defaults)
    pub yes: bool,
}

/// Detected project information
#[derive(Debug)]
struct ProjectInfo {
    name: String,
    project_type: ProjectType,
    entrypoint: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
enum ProjectType {
    Python,
    NodeJs,
    Rust,
    Go,
    Ruby,
    Unknown,
}

impl ProjectType {
    fn as_str(&self) -> &'static str {
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

/// Initialize a project as a Capsule
pub fn execute(args: InitArgs) -> Result<()> {
    let project_dir = args.path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    println!("🔍 Initializing Capsule in: {}\n", project_dir.display());

    // Check if capsule.toml already exists
    let manifest_path = project_dir.join("capsule.toml");
    if manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml already exists!\n\
            Use 'capsule open --dev' to run, or delete the file to re-initialize."
        );
    }

    // Detect project type
    let mut info = detect_project(&project_dir)?;
    println!("   Detected: {} project", info.project_type.as_str());
    
    if !info.entrypoint.is_empty() {
        println!("   Entrypoint: {}", info.entrypoint.join(" "));
    }

    // Interactive mode: confirm or customize
    if !args.yes {
        info = prompt_for_details(info)?;
    }

    // Generate capsule.toml
    let manifest_content = generate_manifest(&info);
    fs::write(&manifest_path, &manifest_content)
        .context("Failed to write capsule.toml")?;

    println!("\n✨ Created capsule.toml!");
    println!("\nNext steps:");
    println!("   capsule open --dev    # Run in development mode");
    println!("   capsule pack          # Create deployable archive");

    // Add .capsule/ to .gitignore if git repo
    if project_dir.join(".git").exists() {
        add_to_gitignore(&project_dir)?;
    }

    Ok(())
}

/// Detect project type from directory contents
fn detect_project(dir: &Path) -> Result<ProjectInfo> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-capsule")
        .to_string();

    // Python detection
    if dir.join("requirements.txt").exists() 
        || dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
    {
        let entrypoint = detect_python_entrypoint(dir);
        return Ok(ProjectInfo {
            name,
            project_type: ProjectType::Python,
            entrypoint,
        });
    }

    // Node.js detection
    if dir.join("package.json").exists() {
        let entrypoint = detect_nodejs_entrypoint(dir)?;
        return Ok(ProjectInfo {
            name,
            project_type: ProjectType::NodeJs,
            entrypoint,
        });
    }

    // Rust detection
    if dir.join("Cargo.toml").exists() {
        return Ok(ProjectInfo {
            name,
            project_type: ProjectType::Rust,
            entrypoint: vec!["cargo".to_string(), "run".to_string()],
        });
    }

    // Go detection
    if dir.join("go.mod").exists() {
        return Ok(ProjectInfo {
            name,
            project_type: ProjectType::Go,
            entrypoint: vec!["go".to_string(), "run".to_string(), ".".to_string()],
        });
    }

    // Ruby detection
    if dir.join("Gemfile").exists() {
        let entrypoint = detect_ruby_entrypoint(dir);
        return Ok(ProjectInfo {
            name,
            project_type: ProjectType::Ruby,
            entrypoint,
        });
    }

    // Unknown - try to find common entry files
    let entrypoint = detect_generic_entrypoint(dir);
    Ok(ProjectInfo {
        name,
        project_type: ProjectType::Unknown,
        entrypoint,
    })
}

fn detect_python_entrypoint(dir: &Path) -> Vec<String> {
    // Priority: main.py > app.py > __main__.py
    for candidate in ["main.py", "app.py", "run.py", "server.py"] {
        if dir.join(candidate).exists() {
            return vec!["python".to_string(), candidate.to_string()];
        }
    }
    
    // Check for __main__.py in package
    if dir.join("__main__.py").exists() {
        return vec!["python".to_string(), ".".to_string()];
    }
    
    // Check pyproject.toml for scripts
    if dir.join("pyproject.toml").exists() {
        // Could parse [tool.poetry.scripts] but keep simple for now
        return vec!["python".to_string(), "-m".to_string(), "app".to_string()];
    }
    
    vec!["python".to_string(), "main.py".to_string()]
}

fn detect_nodejs_entrypoint(dir: &Path) -> Result<Vec<String>> {
    let package_json_path = dir.join("package.json");
    let content = fs::read_to_string(&package_json_path)
        .context("Failed to read package.json")?;
    
    // Try to parse and find main or scripts.start
    if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
        // Check scripts.start first
        if let Some(scripts) = pkg.get("scripts") {
            if scripts.get("start").is_some() {
                return Ok(vec!["npm".to_string(), "start".to_string()]);
            }
        }
        
        // Check main field
        if let Some(main) = pkg.get("main").and_then(|m| m.as_str()) {
            return Ok(vec!["node".to_string(), main.to_string()]);
        }
    }
    
    // Fallback
    for candidate in ["index.js", "main.js", "app.js", "server.js"] {
        if dir.join(candidate).exists() {
            return Ok(vec!["node".to_string(), candidate.to_string()]);
        }
    }
    
    Ok(vec!["npm".to_string(), "start".to_string()])
}

fn detect_ruby_entrypoint(dir: &Path) -> Vec<String> {
    // Check for Rails
    if dir.join("config.ru").exists() {
        return vec!["bundle".to_string(), "exec".to_string(), "rackup".to_string()];
    }
    
    // Check for common entry files
    for candidate in ["app.rb", "main.rb", "server.rb"] {
        if dir.join(candidate).exists() {
            return vec!["ruby".to_string(), candidate.to_string()];
        }
    }
    
    vec!["ruby".to_string(), "app.rb".to_string()]
}

fn detect_generic_entrypoint(dir: &Path) -> Vec<String> {
    // Look for common patterns
    for (file, cmd) in [
        ("main.py", vec!["python", "main.py"]),
        ("index.js", vec!["node", "index.js"]),
        ("main.sh", vec!["bash", "main.sh"]),
        ("run.sh", vec!["bash", "run.sh"]),
    ] {
        if dir.join(file).exists() {
            return cmd.iter().map(|s| s.to_string()).collect();
        }
    }
    
    // Check for Dockerfile - might be container-based
    if dir.join("Dockerfile").exists() {
        return vec!["echo".to_string(), "Container project - specify entrypoint".to_string()];
    }
    
    vec![]
}

fn prompt_for_details(mut info: ProjectInfo) -> Result<ProjectInfo> {
    print!("\n? Package name: ({}) ", info.name);
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if !input.is_empty() {
        info.name = input.to_string();
    }
    
    if !info.entrypoint.is_empty() {
        let default_cmd = info.entrypoint.join(" ");
        print!("? Entry command: ({}) ", default_cmd);
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if !input.is_empty() {
            info.entrypoint = input.split_whitespace().map(|s| s.to_string()).collect();
        }
    } else {
        print!("? Entry command: ");
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if !input.is_empty() {
            info.entrypoint = input.split_whitespace().map(|s| s.to_string()).collect();
        }
    }
    
    Ok(info)
}

fn generate_manifest(info: &ProjectInfo) -> String {
    let entrypoint = if info.entrypoint.is_empty() {
        "echo 'Hello, Capsule!'".to_string()
    } else {
        info.entrypoint.join(" ")
    };
    
    format!(r#"# Capsule Manifest - UARC V1.1.0
# Generated by: capsule init

schema_version = "1.0"
name = "{name}"
version = "0.1.0"
type = "app"

[metadata]
description = "Capsule generated from existing {project_type} project"

[requirements]

[execution]
runtime = "source"
entrypoint = "{entrypoint}"

[storage]

[routing]
"#, 
        name = info.name, 
        project_type = info.project_type.as_str(),
        entrypoint = entrypoint
    )
}

fn add_to_gitignore(dir: &Path) -> Result<()> {
    let gitignore_path = dir.join(".gitignore");
    
    let existing = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path).unwrap_or_default()
    } else {
        String::new()
    };
    
    // Check if already present
    if existing.contains(".capsule/") || existing.contains("*.capsule") {
        return Ok(());
    }
    
    // Append to .gitignore
    let addition = "\n# Capsule\n.capsule/\n*.capsule\n*.sig\n";
    let new_content = format!("{}{}", existing.trim_end(), addition);
    
    fs::write(&gitignore_path, new_content)
        .context("Failed to update .gitignore")?;
    
    println!("   ✓ Updated .gitignore");
    
    Ok(())
}
