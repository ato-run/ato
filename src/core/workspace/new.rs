//! `ato new` - create a new capsule project from scratch.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::reporters::CliReporter;

use super::init::{detect, recipe};
use capsule_core::CapsuleReporter;

pub struct NewArgs {
    pub name: String,
    pub template: Option<String>,
}

pub fn execute(
    args: NewArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let project_dir = PathBuf::from(&args.name);
    let json_mode = matches!(reporter.as_ref(), CliReporter::Json(_));

    if project_dir.exists() {
        anyhow::bail!("Directory '{}' already exists!", args.name);
    }

    let template = args.template.as_deref().unwrap_or("python");

    maybe_notify(
        &reporter,
        format!("🎉 Creating new capsule project: {}", args.name),
    )?;
    maybe_notify(&reporter, format!("   Template: {}\n", template))?;

    fs::create_dir_all(&project_dir)
        .with_context(|| format!("Failed to create directory: {}", project_dir.display()))?;

    match template {
        "python" | "py" => create_python_project(&project_dir, &args.name, reporter.clone())?,
        "node" | "nodejs" | "js" => {
            create_nodejs_project(&project_dir, &args.name, reporter.clone())?
        }
        "hono" => create_hono_project(&project_dir, &args.name, reporter.clone())?,
        "rust" | "rs" => create_rust_project(&project_dir, &args.name, reporter.clone())?,
        "go" | "golang" => create_go_project(&project_dir, &args.name, reporter.clone())?,
        "shell" | "sh" | "bash" => {
            create_shell_project(&project_dir, &args.name, reporter.clone())?
        }
        _ => {
            anyhow::bail!(
                "Unknown template: '{}'\n\
                Available templates: python, node, hono, rust, go, shell",
                template
            );
        }
    }

    create_gitignore(&project_dir, reporter.clone())?;
    create_readme(&project_dir, &args.name, template, reporter.clone())?;

    if json_mode {
        let absolute_path = fs::canonicalize(&project_dir)
            .with_context(|| format!("Failed to resolve path: {}", project_dir.display()))?;
        let payload = serde_json::json!({
            "success": true,
            "name": args.name,
            "path": absolute_path,
            "template": template,
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    maybe_notify(&reporter, "\n✨ Project created successfully!")?;
    maybe_notify(&reporter, "\nNext steps:")?;
    maybe_notify(&reporter, format!("   cd {}", args.name))?;
    maybe_notify(&reporter, "   ato dev")?;

    Ok(())
}

fn maybe_notify(
    reporter: &std::sync::Arc<crate::reporters::CliReporter>,
    message: impl Into<String>,
) -> Result<()> {
    if matches!(reporter.as_ref(), CliReporter::Json(_)) {
        return Ok(());
    }
    futures::executor::block_on(reporter.notify(message.into()))?;
    Ok(())
}

fn create_python_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let main_py = r#"#!/usr/bin/env python3
"""
Main entry point for the capsule application.
"""

def main():
    print("Hello from capsule! 🎉")
    print("Edit main.py to get started.")

if __name__ == "__main__":
    main()
"#;
    fs::write(dir.join("main.py"), main_py)?;

    fs::write(
        dir.join("requirements.txt"),
        "# Add your dependencies here\n",
    )?;

    // Generate capsule.toml via the shared detect+recipe path.
    let detected = detect::detect_project(dir)?;
    let mut info = recipe::project_info_from_detection(&detected)?;
    info.name = name.to_string();
    let manifest = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato new",
            description: "A new capsule application",
        },
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created main.py")?;
    maybe_notify(&reporter, "   ✓ Created requirements.txt")?;
    Ok(())
}

fn create_nodejs_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    // Bundle selection is opt-in: this file controls what goes into the bundle.
    // Keep it minimal (users can edit as needed).
    fs::write(dir.join(".capsuleignore"), "node_modules/\n")?;

    // Hint for detection: this is a Bun project.
    fs::write(dir.join("bun.lockb"), [])?;

    let package_json = format!(
        r#"{{
    "name": "{name}",
    "version": "0.1.0",
    "private": true,
    "type": "module",
    "packageManager": "bun@1",
    "main": "dist/server.js",
    "scripts": {{
        "dev": "bun --hot src/index.ts",
        "build": "bun build src/index.ts --outfile dist/server.js --target=bun",
        "start": "bun run dist/server.js"
    }}
}}
"#
    );
    fs::write(dir.join("package.json"), package_json)?;

    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join("dist"))?;

    let index_ts = r#"import { serve } from "bun";

serve({
    port: Number(process.env.PORT ?? 8000),
    fetch(req) {
        return new Response("Hello from capsule!\n", {
            headers: { "content-type": "text/plain; charset=utf-8" },
        });
    },
});

console.log(`Started server http://localhost:${process.env.PORT ?? 8000}`);
"#;
    fs::write(dir.join("src/index.ts"), index_ts)?;

    // Provide a ready-to-run release artifact so `ato pack` works immediately.
    // Users are expected to overwrite this by running `bun run build`.
    let dist_js = r#"import { serve } from "bun";

serve({
    port: Number(process.env.PORT ?? 8000),
    fetch(req) {
        return new Response("Hello from capsule!\n", {
            headers: { "content-type": "text/plain; charset=utf-8" },
        });
    },
});

console.log(`Started server http://localhost:${process.env.PORT ?? 8000}`);
"#;
    fs::write(dir.join("dist/server.js"), dist_js)?;

    // Generate capsule.toml via the shared detect+recipe path.
    let detected = detect::detect_project(dir)?;
    let mut info = recipe::project_info_from_detection(&detected)?;
    info.name = name.to_string();
    let manifest = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato new",
            description: "A new Bun/Node.js capsule application",
        },
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created .capsuleignore")?;
    maybe_notify(&reporter, "   ✓ Created package.json")?;
    maybe_notify(&reporter, "   ✓ Created src/index.ts")?;
    maybe_notify(&reporter, "   ✓ Created dist/server.js")?;
    Ok(())
}

fn create_hono_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    // Bundle selection is opt-in: this file controls what goes into the bundle.
    // Keep it minimal (users can edit as needed).
    fs::write(dir.join(".capsuleignore"), "node_modules/\n")?;

    // Hint for detection: this is a Bun project.
    fs::write(dir.join("bun.lockb"), [])?;

    let package_json = format!(
        r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "private": true,
  "type": "module",
    "packageManager": "bun@1",
  "main": "dist/server.js",
  "scripts": {{
    "dev": "bun --hot src/index.ts",
    "build": "bun build src/index.ts --outfile dist/server.js --target=bun",
    "start": "bun run dist/server.js"
  }},
  "dependencies": {{
    "hono": "^4"
  }}
}}
"#
    );
    fs::write(dir.join("package.json"), package_json)?;

    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join("dist"))?;

    let index_ts = r#"import { Hono } from "hono";
import { serve } from "bun";

const app = new Hono();

app.get("/", (c) => c.text("Hello from capsule!\n"));

serve({
    port: Number(process.env.PORT ?? 8000),
    fetch: app.fetch,
});

console.log(`Started server http://localhost:${process.env.PORT ?? 8000}`);
"#;
    fs::write(dir.join("src/index.ts"), index_ts)?;

    // Provide a ready-to-run release artifact so `ato pack` works immediately.
    // Users are expected to overwrite this by running `bun run build`.
    let dist_js = r#"import { Hono } from "hono";
import { serve } from "bun";

const app = new Hono();

app.get("/", (c) => c.text("Hello from capsule!\n"));

serve({
    port: Number(process.env.PORT ?? 8000),
    fetch: app.fetch,
});

console.log(`Started server http://localhost:${process.env.PORT ?? 8000}`);
"#;
    fs::write(dir.join("dist/server.js"), dist_js)?;

    // Generate capsule.toml via the shared detect+recipe path.
    let detected = detect::detect_project(dir)?;
    let mut info = recipe::project_info_from_detection(&detected)?;
    info.name = name.to_string();
    let manifest = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato new",
            description: "A new Bun/Hono capsule application",
        },
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created .capsuleignore")?;
    maybe_notify(&reporter, "   ✓ Created package.json")?;
    maybe_notify(&reporter, "   ✓ Created src/index.ts")?;
    maybe_notify(&reporter, "   ✓ Created dist/server.js")?;
    Ok(())
}

fn create_rust_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    // Bundle selection is opt-in: this file controls what goes into the bundle.
    fs::write(dir.join(".capsuleignore"), "target/\n")?;

    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
"#
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    fs::create_dir_all(dir.join("src"))?;
    let main_rs = r#"fn main() {
    println!("Hello from capsule!");
    println!("Edit src/main.rs to get started.");
}
"#;
    fs::write(dir.join("src/main.rs"), main_rs)?;

    // Generate capsule.toml via the shared detect+recipe path.
    let detected = detect::detect_project(dir)?;
    let mut info = recipe::project_info_from_detection(&detected)?;
    info.name = name.to_string();
    let manifest = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato new",
            description: "A new Rust capsule application",
        },
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created .capsuleignore")?;
    maybe_notify(&reporter, "   ✓ Created Cargo.toml")?;
    maybe_notify(&reporter, "   ✓ Created src/main.rs")?;
    Ok(())
}

fn create_go_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let go_mod = format!(
        r#"module example.com/{name}

go 1.22
"#
    );
    fs::write(dir.join("go.mod"), go_mod)?;

    let main_go = r#"package main

import "fmt"

func main() {
    fmt.Println("Hello from capsule! 🎉")
    fmt.Println("Edit main.go to get started.")
}
"#;
    fs::write(dir.join("main.go"), main_go)?;

    // Generate capsule.toml via the shared detect+recipe path.
    let detected = detect::detect_project(dir)?;
    let mut info = recipe::project_info_from_detection(&detected)?;
    info.name = name.to_string();
    let manifest = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato new",
            description: "A new Go capsule application",
        },
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created go.mod")?;
    maybe_notify(&reporter, "   ✓ Created main.go")?;
    Ok(())
}

fn create_shell_project(
    dir: &Path,
    name: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let manifest = format!(
        r#"# Capsule Manifest - Multi-Target Native v0.2
schema_version = "0.2"
name = "{name}"
version = "0.1.0"
type = "app"
default_target = "cli"

[metadata]
description = "A new capsule application"

[requirements]

[targets.cli]
runtime = "source"
entrypoint = "main.sh"

[storage]

[routing]
"#
    );
    fs::write(dir.join("capsule.toml"), manifest)?;

    let main_sh = r#"#!/bin/bash
#
# Main entry point for the capsule application.
#

echo "Hello from capsule! 🎉"
echo "Edit main.sh to get started."
"#;
    fs::write(dir.join("main.sh"), main_sh)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dir.join("main.sh"))?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(dir.join("main.sh"), perms)?;
    }

    maybe_notify(&reporter, "   ✓ Created capsule.toml")?;
    maybe_notify(&reporter, "   ✓ Created main.sh")?;
    Ok(())
}

fn create_gitignore(
    dir: &Path,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
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
    maybe_notify(&reporter, "   ✓ Created .gitignore")?;
    Ok(())
}

fn create_readme(
    dir: &Path,
    name: &str,
    template: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let quickstart: String = match template {
        "node" | "nodejs" | "js" | "hono" => r#"```bash
# Install deps (optional for this minimal template)
bun install

# Run locally (dev profile)
ato dev

# Build release artifact (recommended)
bun run build

# Create a self-extracting bundle (release profile)
ato pack --bundle

# Run bundle
./nacelle-bundle
```"#
            .to_string(),
        "rust" | "rs" => {
            format!(
                r#"```bash
# Run locally (dev profile)
ato dev

# Build release binary for bundling
cargo build --release
cp target/release/{name} ./{name}

# Create a self-extracting bundle (release profile)
ato pack --bundle

# Run bundle
./nacelle-bundle
```"#
            )
        }
        "go" | "golang" => {
            format!(
                r#"```bash
# Run locally (dev profile)
ato dev

# Build release binary for bundling
go build -o {name} .

# Create a self-extracting bundle (release profile)
ato pack --bundle

# Run bundle
./nacelle-bundle
```"#
            )
        }
        _ => r#"```bash
# Run locally (no bundling)
ato dev

# Create a self-extracting bundle
ato pack --bundle

# Run bundle
./nacelle-bundle
```"#
            .to_string(),
    };

    let content = format!(
        r#"# {name}

A capsule application built with UARC V1.1.0.

## Quick Start

{quickstart}

## Notes

- `capsule.toml` supports `execution.dev` and `execution.release` for dev vs packaging.
- `.capsuleignore` (optional) controls what gets bundled.

## Learn More

- UARC Specification: https://uarc.dev
"#
    );
    fs::write(dir.join("README.md"), content)?;
    maybe_notify(&reporter, "   ✓ Created README.md")?;
    Ok(())
}
