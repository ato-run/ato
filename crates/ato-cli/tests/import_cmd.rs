use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::Value;

#[test]
fn import_run_executes_with_shadow_manifest() -> Result<()> {
    let root = test_root("executes")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(source.join("README.md"), "# shadow import\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "shadow-import"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("passed"));
    assert_ne!(
        output["run"]["error_class"].as_str(),
        Some("run_execution_not_wired")
    );
    assert_eq!(
        output["source"]["source_url_normalized"].as_str(),
        Some("https://github.com/ato-run/shadow-import")
    );
    assert_eq!(output["recipe"]["origin"].as_str(), Some("manual"));
    assert_eq!(
        output["recipe"]["recipe_toml"].as_str(),
        fs::read_to_string(&recipe).ok().as_deref()
    );
    Ok(())
}

#[test]
fn import_run_does_not_write_capsule_toml_to_source() -> Result<()> {
    let root = test_root("source-clean")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(source.join("app.txt"), "source bytes\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "source-clean"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    assert!(!source.join("capsule.toml").exists());
    let _output = run_import(&root, &source, Some(&recipe))?;
    assert!(
        !source.join("capsule.toml").exists(),
        "import run must write capsule.toml only to the shadow workspace"
    );
    Ok(())
}

fn run_import(root: &Path, source: &Path, recipe: Option<&Path>) -> Result<Value> {
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let mut command = Command::new(assert_cmd::cargo::cargo_bin("ato"));
    command
        .arg("import")
        .arg("github.com/ato-run/shadow-import")
        .arg("--run")
        .arg("--emit-json")
        .env("ATO_IMPORT_LOCAL_SOURCE_OVERRIDE", source)
        .env(
            "ATO_IMPORT_LOCAL_REVISION_ID",
            "1111111111111111111111111111111111111111",
        )
        .env("ATO_IMPORT_LOCAL_TREE_HASH", "blake3:test-tree")
        .env("HOME", &home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .current_dir(root);
    if let Some(recipe) = recipe {
        command.arg("--recipe").arg(recipe);
    }
    let output = command.output().context("failed to run ato import")?;
    assert!(
        output.status.success(),
        "ato import failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "ato import did not emit valid JSON\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn test_root(name: &str) -> Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let root = std::env::current_dir()?
        .join(".tmp")
        .join("import-cmd-e2e")
        .join(format!("{name}-{unique}"));
    fs::create_dir_all(&root)?;
    Ok(root)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
