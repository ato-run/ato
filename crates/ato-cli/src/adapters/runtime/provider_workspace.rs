use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use capsule_core::ato_lock::AtoLock;
use capsule_core::router::ManifestData;
use sha2::Digest;

use crate::application::dependency_materializer::{
    digest_file, AttestationStrategy, CacheStrategy, DependencyMaterializationRequest,
    DependencyMaterializer, InstallPolicies, ManifestInputs, PlatformTriple, RuntimeSelection,
    SessionDependencyMaterializer,
};
use crate::runtime::manager;

const PROVIDER_RESOLUTION_FILE: &str = "resolution.json";

pub(crate) fn is_provider_workspace(path: &Path) -> bool {
    provider_resolution_metadata_path(path)
        .map(|metadata| metadata.exists())
        .unwrap_or(false)
}

pub(crate) fn provider_resolution_metadata_path(path: &Path) -> Option<PathBuf> {
    let direct = path.join(PROVIDER_RESOLUTION_FILE);
    if direct.exists() {
        return Some(direct);
    }

    path.parent()
        .map(|parent| parent.join(PROVIDER_RESOLUTION_FILE))
        .filter(|candidate| candidate.exists())
}

pub(crate) fn ensure_provider_node_execution_inputs(
    plan: &ManifestData,
    authoritative_lock: Option<&AtoLock>,
) -> Result<()> {
    if !is_provider_workspace(&plan.manifest_dir) {
        return Ok(());
    }

    let authoritative_lock = authoritative_lock.context(
        "provider-backed npm execution requires authoritative lock before dependency materialization",
    )?;
    let package_json = plan.manifest_dir.join("package.json");
    if !package_json.exists() {
        bail!(
            "provider-backed npm workspace is missing package.json: {}",
            package_json.display()
        );
    }
    materialize_provider_dependency_boundary(plan, "npm", "node", Some(authoritative_lock))?;

    let node_bin = manager::ensure_node_binary_with_authority(plan, Some(authoritative_lock))?;
    let npm_invocation = resolve_npm_invocation(&node_bin)?;
    let package_lock = plan.manifest_dir.join("package-lock.json");
    if !package_lock.exists() {
        run_npm(
            &npm_invocation,
            &plan.manifest_dir,
            &[
                "install",
                "--package-lock-only",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--silent",
            ],
        )
        .context("failed to derive package-lock.json from authoritative npm provider lock")?;
    }

    let node_modules = plan.manifest_dir.join("node_modules");
    if !node_modules.exists() {
        run_npm(
            &npm_invocation,
            &plan.manifest_dir,
            &[
                "ci",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--silent",
            ],
        )
        .context("failed to materialize node_modules from derived package-lock.json")?;
    }

    Ok(())
}

pub(crate) fn ensure_provider_python_execution_inputs(
    plan: &ManifestData,
    authoritative_lock: Option<&AtoLock>,
) -> Result<()> {
    if !is_provider_workspace(&plan.manifest_dir) {
        return Ok(());
    }

    let authoritative_lock = authoritative_lock.context(
        "provider-backed PyPI execution requires authoritative lock before dependency materialization",
    )?;
    let requirements = plan.manifest_dir.join("requirements.txt");
    if !requirements.exists() {
        bail!(
            "provider-backed PyPI workspace is missing requirements.txt: {}",
            requirements.display()
        );
    }
    materialize_provider_dependency_boundary(plan, "pypi", "python", Some(authoritative_lock))?;

    let python_bin = manager::ensure_python_binary_with_authority(plan, Some(authoritative_lock))?;
    let uv_bin = manager::ensure_uv_binary_with_authority(plan, Some(authoritative_lock))?;
    let uv_lock = plan.manifest_dir.join("uv.lock");
    if !uv_lock.exists() {
        run_command(
            Command::new(&uv_bin)
                .arg("pip")
                .arg("compile")
                .arg("requirements.txt")
                .arg("-o")
                .arg("uv.lock")
                .arg("--python")
                .arg(&python_bin)
                .current_dir(&plan.manifest_dir),
            &format!("derive {}", uv_lock.display()),
        )?;
    }

    let site_packages = plan.manifest_dir.join("site-packages");
    if !site_packages.exists() {
        fs::create_dir_all(&site_packages)
            .with_context(|| format!("failed to create {}", site_packages.display()))?;
        run_command(
            Command::new(&uv_bin)
                .arg("pip")
                .arg("sync")
                .arg("uv.lock")
                .arg("--python")
                .arg(&python_bin)
                .arg("--target")
                .arg(&site_packages)
                .current_dir(&plan.manifest_dir),
            &format!("materialize {}", site_packages.display()),
        )?;
    }

    Ok(())
}

fn materialize_provider_dependency_boundary(
    plan: &ManifestData,
    ecosystem: &str,
    runtime_name: &str,
    authoritative_lock: Option<&AtoLock>,
) -> Result<()> {
    let lockfile_digest = first_digest(
        &plan.manifest_dir,
        &[
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
            "uv.lock",
            "requirements.txt",
        ],
    )?;
    let lock_digest = authoritative_lock
        .map(serde_jcs::to_vec)
        .transpose()
        .context("failed to canonicalize provider authoritative lock")?
        .map(|bytes| format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes))));
    let materializer = SessionDependencyMaterializer::new();
    let request = DependencyMaterializationRequest {
        session_id: format!("provider-{ecosystem}"),
        capsule_id: plan
            .manifest
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or("provider-workspace")
            .to_string(),
        source_root: plan.manifest_dir.clone(),
        workspace_root: plan.workspace_root.clone(),
        ecosystem: ecosystem.to_string(),
        package_manager: Some(if ecosystem == "npm" { "npm" } else { "uv" }.to_string()),
        package_manager_version: None,
        runtime: RuntimeSelection {
            name: runtime_name.to_string(),
            version: None,
        },
        manifests: ManifestInputs {
            lockfile_digest: lockfile_digest.or(lock_digest),
            package_manifest_digest: first_digest(
                &plan.manifest_dir,
                &["package.json", "pyproject.toml", "requirements.txt"],
            )?,
            workspace_manifest_digest: digest_file(&plan.manifest_dir.join("capsule.toml"))?,
            path_dependency_digest: None,
        },
        policies: InstallPolicies {
            lifecycle_script_policy: "sandbox".to_string(),
            registry_policy: "default".to_string(),
            network_policy: "default".to_string(),
            env_allowlist_digest: None,
        },
        platform: PlatformTriple::current(),
        cache_strategy: CacheStrategy::None,
        attestation_strategy: AttestationStrategy::None,
    };
    let projection = materializer.materialize(&request)?;
    let verification = materializer.verify(&projection)?;
    if !verification.ok {
        bail!("{}", verification.messages.join("; "));
    }
    Ok(())
}

fn first_digest(root: &Path, names: &[&str]) -> Result<Option<String>> {
    for name in names {
        if let Some(digest) = digest_file(&root.join(name))? {
            return Ok(Some(digest));
        }
    }
    Ok(None)
}

enum NpmInvocation {
    Program(PathBuf),
    NodeCli { node: PathBuf, cli: PathBuf },
}

fn resolve_npm_invocation(node_bin: &Path) -> Result<NpmInvocation> {
    let node_dir = node_bin
        .parent()
        .context("node binary must have a parent directory")?;

    let install_root = node_dir
        .parent()
        .context("node binary install root is missing")?;
    let cli = install_root
        .join("lib")
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js");
    if cli.exists() {
        return Ok(NpmInvocation::NodeCli {
            node: node_bin.to_path_buf(),
            cli,
        });
    }

    for candidate in [node_dir.join("npm"), node_dir.join("npm.cmd")] {
        if candidate.exists() {
            return Ok(NpmInvocation::Program(candidate));
        }
    }

    bail!(
        "bundled node runtime does not include an npm executable near {}",
        node_bin.display()
    )
}

fn run_npm(invocation: &NpmInvocation, cwd: &Path, args: &[&str]) -> Result<()> {
    let mut command = match invocation {
        NpmInvocation::Program(program) => Command::new(program),
        NpmInvocation::NodeCli { node, cli } => {
            let mut command = Command::new(node);
            command.arg(cli);
            command
        }
    };
    let path = env::var_os("PATH").unwrap_or_default();
    let bin_dir = match invocation {
        NpmInvocation::Program(program) => program.parent().map(Path::to_path_buf),
        NpmInvocation::NodeCli { node, .. } => node.parent().map(Path::to_path_buf),
    };
    if let Some(bin_dir) = bin_dir {
        let mut paths = vec![bin_dir];
        paths.extend(env::split_paths(&path));
        command.env(
            "PATH",
            env::join_paths(paths).context("join PATH for bundled npm")?,
        );
    }
    for arg in args {
        command.arg(arg);
    }
    command.current_dir(cwd);
    run_command(&mut command, &format!("npm {}", args.join(" ")))
}

fn run_command(command: &mut Command, label: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "{} failed with status {}: {}",
        label,
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}
