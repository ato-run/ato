use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
struct EcosystemCase {
    name: &'static str,
    manifest: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const NODE_CASE: EcosystemCase = EcosystemCase {
    name: "node",
    manifest: r#"schema_version = "0.3"
name = "pollution-node"
version = "1.0.0"
type = "app"

runtime = "source/native"
run = "true"
"#,
    files: &[
        ("main.js", "console.log('node');\n"),
        (
            "package.json",
            r#"{"name":"pollution-node","version":"1.0.0"}"#,
        ),
        ("package-lock.json", "{}\n"),
        (".env.example", "SHOULD_NOT_COPY=1\n"),
    ],
};

const PYTHON_CASE: EcosystemCase = EcosystemCase {
    name: "python",
    manifest: r#"schema_version = "0.3"
name = "pollution-python"
version = "1.0.0"
type = "app"

runtime = "source/native"
run = "true"
"#,
    files: &[
        ("main.py", "print('python')\n"),
        (
            "pyproject.toml",
            "[project]\nname = \"pollution-python\"\nversion = \"1.0.0\"\n",
        ),
        ("uv.lock", "# uv lock\n"),
        (".env.example", "SHOULD_NOT_COPY=1\n"),
    ],
};

const RUST_CASE: EcosystemCase = EcosystemCase {
    name: "rust",
    manifest: r#"schema_version = "0.3"
name = "pollution-rust"
version = "1.0.0"
type = "app"

runtime = "source/native"
run = "true"
"#,
    files: &[
        (
            "Cargo.toml",
            "[package]\nname = \"pollution-rust\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/main.rs", "fn main() { println!(\"rust\"); }\n"),
        ("Cargo.lock", "# lock\n"),
        (".env.example", "SHOULD_NOT_COPY=1\n"),
    ],
};

const GO_CASE: EcosystemCase = EcosystemCase {
    name: "go",
    manifest: r#"schema_version = "0.3"
name = "pollution-go"
version = "1.0.0"
type = "app"

runtime = "source/native"
run = "true"
"#,
    files: &[
        ("go.mod", "module pollution-go\n\ngo 1.22\n"),
        (
            "main.go",
            "package main\n\nfunc main() { println(\"go\") }\n",
        ),
        ("go.sum", "# sum\n"),
        (".env.example", "SHOULD_NOT_COPY=1\n"),
    ],
};

#[test]
fn node_source_run_does_not_pollute_source_tree() -> Result<()> {
    assert_source_tree_unmodified_after_run(NODE_CASE)
}

#[test]
fn python_source_run_does_not_pollute_source_tree() -> Result<()> {
    assert_source_tree_unmodified_after_run(PYTHON_CASE)
}

#[test]
fn rust_source_run_does_not_pollute_source_tree() -> Result<()> {
    assert_source_tree_unmodified_after_run(RUST_CASE)
}

#[test]
fn go_source_run_does_not_pollute_source_tree() -> Result<()> {
    assert_source_tree_unmodified_after_run(GO_CASE)
}

fn assert_source_tree_unmodified_after_run(case: EcosystemCase) -> Result<()> {
    let root = test_root(case.name)?;
    let _cleanup = Cleanup(root.clone());
    let home = root.join("home");
    let source = root.join("source");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&source)?;
    fs::write(source.join("capsule.toml"), case.manifest)?;
    for (relative, contents) in case.files {
        let path = source.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }

    let before_mtime = fs::metadata(&source)?.modified()?;
    let before_hashes = file_hashes(&source)?;
    let shim_path = write_fake_runtime_shims(&root)?;

    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .args([
            "run",
            ".",
            "--yes",
            "--no-build",
            "--dangerously-skip-permissions",
        ])
        .current_dir(&source)
        .env("HOME", &home)
        .env("PATH", shim_path)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .output()
        .with_context(|| format!("failed to run ato for {}", case.name))?;
    assert!(
        output.status.success(),
        "{} run failed\nstdout:\n{}\nstderr:\n{}",
        case.name,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for forbidden in ["node_modules", ".venv", ".env", "target", "dist", "build"] {
        assert!(
            !source.join(forbidden).exists(),
            "{} created forbidden source entry {}",
            case.name,
            forbidden
        );
    }
    assert_eq!(
        before_hashes,
        file_hashes(&source)?,
        "{} changed source file bytes",
        case.name
    );
    assert_eq!(
        before_mtime,
        fs::metadata(&source)?.modified()?,
        "{} changed source directory mtime",
        case.name
    );
    Ok(())
}

fn file_hashes(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut hashes = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let bytes = fs::read(path)?;
        hashes.insert(
            path.strip_prefix(root)?.to_path_buf(),
            format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
        );
    }
    Ok(hashes)
}

fn write_fake_runtime_shims(root: &Path) -> Result<std::ffi::OsString> {
    let shims = root.join("shims");
    fs::create_dir_all(&shims)?;
    for binary in ["node", "python", "python3", "cargo", "go"] {
        let path = shims.join(binary);
        fs::write(&path, "#!/bin/sh\nexit 0\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions)?;
        }
    }
    let mut paths = vec![shims];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).context("join PATH")
}

fn test_root(name: &str) -> Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let root = std::env::current_dir()?
        .join(".tmp")
        .join("source-pollution-e2e")
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
