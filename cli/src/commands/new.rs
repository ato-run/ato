//! New command - create a new Capsule project from scratch
//!
//! Creates a minimal project structure with capsule.toml and entry file.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Arguments for the new command
pub struct NewArgs {
    /// Project name
    pub name: String,
    /// Template type (python, node, rust, shell)
    pub template: Option<String>,
}

/// Create a new Capsule project
pub fn execute(args: NewArgs) -> Result<()> {
    let project_dir = PathBuf::from(&args.name);

    if project_dir.exists() {
        anyhow::bail!("Directory '{}' already exists!", args.name);
    }

    let template = args.template.as_deref().unwrap_or("python");

    println!("🎉 Creating new Capsule project: {}", args.name);
    println!("   Template: {}\n", template);

    // Create project directory
    fs::create_dir_all(&project_dir)
        .with_context(|| format!("Failed to create directory: {}", project_dir.display()))?;

    // Generate files based on template
    match template {
        "python" | "py" => create_python_project(&project_dir, &args.name)?,
        "node" | "nodejs" | "js" => create_nodejs_project(&project_dir, &args.name)?,
        "rust" | "rs" => create_rust_project(&project_dir, &args.name)?,
        "shell" | "sh" | "bash" => create_shell_project(&project_dir, &args.name)?,
        _ => {
            anyhow::bail!(
                "Unknown template: '{}'\n\
                Available templates: python, node, rust, shell",
                template
            );
        }
    }

    // Create common files
    create_gitignore(&project_dir)?;
    create_readme(&project_dir, &args.name)?;

    println!("\n✨ Project created successfully!");
    println!("\nNext steps:");
    println!("   cd {}", args.name);
    println!("   capsule open --dev");

    Ok(())
}

fn create_python_project(dir: &PathBuf, name: &str) -> Result<()> {
    // capsule.toml
    let manifest = format!(
        r#"# Capsule Manifest - UARC V1.1.0
schema_version = "1.0"
name = "{name}"
version = "0.1.0"
type = "app"

[metadata]
description = "A new Capsule application"

[requirements]

[execution]
runtime = "source"
entrypoint = "python main.py"

[storage]

[routing]
"#
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    // main.py
    let main_py = r#"#!/usr/bin/env python3
"""
Main entry point for the Capsule application.
"""

def main():
    print("Hello from Capsule! 🎉")
    print("Edit main.py to get started.")

if __name__ == "__main__":
    main()
"#;
    fs::write(dir.join("main.py"), main_py)?;

    // requirements.txt
    fs::write(
        dir.join("requirements.txt"),
        "# Add your dependencies here\n",
    )?;

    println!("   ✓ Created capsule.toml");
    println!("   ✓ Created main.py");
    println!("   ✓ Created requirements.txt");

    Ok(())
}

fn create_nodejs_project(dir: &PathBuf, name: &str) -> Result<()> {
    // capsule.toml
    let manifest = format!(
        r#"# Capsule Manifest - UARC V1.1.0
schema_version = "1.0"
name = "{name}"
version = "0.1.0"
type = "app"

[metadata]
description = "A new Capsule application"

[requirements]

[execution]
runtime = "source"
entrypoint = "node index.js"

[storage]

[routing]
"#
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    // package.json
    let package_json = format!(
        r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "main": "index.js",
  "scripts": {{
    "start": "node index.js"
  }}
}}
"#
    );
    fs::write(dir.join("package.json"), package_json)?;

    // index.js
    let index_js = r#"/**
 * Main entry point for the Capsule application.
 */

console.log("Hello from Capsule! 🎉");
console.log("Edit index.js to get started.");
"#;
    fs::write(dir.join("index.js"), index_js)?;

    println!("   ✓ Created capsule.toml");
    println!("   ✓ Created package.json");
    println!("   ✓ Created index.js");

    Ok(())
}

fn create_rust_project(dir: &PathBuf, name: &str) -> Result<()> {
    // For Rust, we create a simple source project

    // capsule.toml
    let manifest = format!(
        r#"# Capsule Manifest - UARC V1.1.0
schema_version = "1.0"
name = "{name}"
version = "0.1.0"
type = "app"

[metadata]
description = "A new Capsule application"

[requirements]

[execution]
runtime = "source"
entrypoint = "cargo run --release"

[storage]

[routing]

# Alternative: Build to Wasm for sandboxed execution
# [targets.wasm]
# digest = "sha256:..."
# world = "wasi:cli/command"
"#
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    // Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
        name.replace("-", "_")
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    // src/main.rs
    fs::create_dir_all(dir.join("src"))?;
    let main_rs = r#"fn main() {
    println!("Hello from Capsule! 🎉");
    println!("Edit src/main.rs to get started.");
}
"#;
    fs::write(dir.join("src/main.rs"), main_rs)?;

    println!("   ✓ Created capsule.toml");
    println!("   ✓ Created Cargo.toml");
    println!("   ✓ Created src/main.rs");

    Ok(())
}

fn create_shell_project(dir: &PathBuf, name: &str) -> Result<()> {
    // capsule.toml
    let manifest = format!(
        r#"# Capsule Manifest - UARC V1.1.0
schema_version = "1.0"
name = "{name}"
version = "0.1.0"
type = "app"

[metadata]
description = "A new Capsule application"

[requirements]

[execution]
runtime = "source"
entrypoint = "bash main.sh"

[storage]

[routing]
"#
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    // main.sh
    let main_sh = r#"#!/bin/bash
#
# Main entry point for the Capsule application.
#

echo "Hello from Capsule! 🎉"
echo "Edit main.sh to get started."
"#;
    fs::write(dir.join("main.sh"), main_sh)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dir.join("main.sh"))?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(dir.join("main.sh"), perms)?;
    }

    println!("   ✓ Created capsule.toml");
    println!("   ✓ Created main.sh");

    Ok(())
}

fn create_gitignore(dir: &PathBuf) -> Result<()> {
    let content = r#"# Capsule
.capsule/
*.capsule
*.sig

# Common
.DS_Store
*.log

# Python
__pycache__/
*.py[cod]
.venv/
venv/

# Node
node_modules/

# Rust
target/
"#;
    fs::write(dir.join(".gitignore"), content)?;
    println!("   ✓ Created .gitignore");
    Ok(())
}

fn create_readme(dir: &PathBuf, name: &str) -> Result<()> {
    let content = format!(
        r#"# {name}

A Capsule application built with UARC V1.1.0.

## Quick Start

```bash
# Run in development mode
capsule open --dev

# Package for deployment
capsule pack

# Run packaged version
capsule open
```

## Project Structure

- `capsule.toml` - Capsule manifest (package config, permissions, runtime)
- Entry file depends on template (main.py, index.js, etc.)

## Learn More

- [UARC Specification](https://uarc.dev)
- [Capsule Documentation](https://docs.capsule.dev)
"#
    );
    fs::write(dir.join("README.md"), content)?;
    println!("   ✓ Created README.md");
    Ok(())
}
