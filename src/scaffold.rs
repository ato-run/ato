use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::CapsuleReporter;

pub struct ScaffoldDockerArgs {
    pub manifest_path: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub force: bool,
}

pub fn execute_docker(
    args: ScaffoldDockerArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let manifest_path = args.manifest_path.canonicalize().with_context(|| {
        format!(
            "Failed to resolve manifest path: {}",
            args.manifest_path.display()
        )
    })?;

    let project_dir = manifest_path
        .parent()
        .context("Failed to determine manifest directory")?
        .to_path_buf();

    if args.output.is_some() && args.output_dir.is_some() {
        anyhow::bail!("Use either --output or --output-dir (not both)");
    }

    let (output_dir, dockerfile_path) = if let Some(output) = args.output.as_ref() {
        let output_path = if output.is_absolute() {
            output.clone()
        } else {
            project_dir.join(output)
        };
        let dir = output_path
            .parent()
            .context("Failed to determine output file directory")?
            .to_path_buf();
        (dir, output_path)
    } else {
        let dir = args.output_dir.unwrap_or(project_dir);
        (dir.clone(), dir.join("Dockerfile"))
    };

    let content = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Failed to parse TOML: {}", manifest_path.display()))?;

    let gpu = manifest
        .get("build")
        .and_then(|b| b.get("gpu"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let dockerignore_path = output_dir.join(".dockerignore");

    let dockerfile = if gpu {
        dockerfile_gpu_template()
    } else {
        dockerfile_distroless_template()
    };

    write_if_allowed(&dockerfile_path, &dockerfile, args.force)?;
    write_if_allowed(&dockerignore_path, &dockerignore_template(), args.force)?;

    futures::executor::block_on(reporter.notify("✅ Scaffolded Docker files".to_string()))?;
    futures::executor::block_on(reporter.notify(format!("- {}", dockerfile_path.display())))?;
    futures::executor::block_on(reporter.notify(format!("- {}", dockerignore_path.display())))?;

    if gpu {
        futures::executor::block_on(
            reporter.notify("\nGPU mode detected: build.gpu=true".to_string()),
        )?;
        futures::executor::block_on(
            reporter
                .notify("Run with: docker run --rm --gpus all -p 8000:8000 <image>".to_string()),
        )?;
    }

    Ok(())
}

fn write_if_allowed(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "Refusing to overwrite existing file: {} (use --force)",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    fs::write(path, content)
        .with_context(|| format!("Failed to write file: {}", path.display()))?;
    Ok(())
}

fn dockerfile_distroless_template() -> String {
    // NOTE:
    // - Distroless images have no shell; keep the final stage simple.
    // - The bundle must be built for Linux (the container's target OS/arch).
    // - The self-extracting bundle writes to a temp directory (typically /tmp).
    r#"# syntax=docker/dockerfile:1

# Thin Capsule runner (CPU) on Distroless
#
# Expected workflow:
#   1) Build a Linux self-extracting bundle (nacelle-bundle) via: ato pack
#   2) docker build -t my-capsule .
#   3) docker run --rm -p 8000:8000 my-capsule

FROM gcr.io/distroless/base-debian12:nonroot

WORKDIR /app

# Copy the self-extracting bundle produced by `ato pack`.
# (Name is configurable; update as needed.)
COPY --chown=nonroot:nonroot nacelle-bundle /app/nacelle-bundle

ENV PORT=8000
EXPOSE 8000

ENTRYPOINT ["/app/nacelle-bundle"]
"#
    .to_string()
}

fn dockerfile_gpu_template() -> String {
    // GPU images: use NVIDIA CUDA runtime base.
    // The NVIDIA Container Toolkit on the host interprets NVIDIA_* env vars.
    r#"# syntax=docker/dockerfile:1

# Thin Capsule runner (GPU) on CUDA Runtime
#
# Expected workflow:
#   1) Build a Linux self-extracting bundle (nacelle-bundle) via: ato pack
#   2) docker build -t my-capsule-gpu .
#   3) docker run --rm --gpus all -p 8000:8000 my-capsule-gpu

FROM nvidia/cuda:12.4.1-runtime-ubuntu22.04

WORKDIR /app

# Create a non-root user (distroless-style UID)
RUN useradd -m -u 65532 -s /usr/sbin/nologin nonroot \
    && mkdir -p /app \
    && chown -R nonroot:nonroot /app

COPY nacelle-bundle /app/nacelle-bundle
RUN chown nonroot:nonroot /app/nacelle-bundle

USER nonroot

# NVIDIA Container Toolkit environment variables (OCI spec)
ENV NVIDIA_VISIBLE_DEVICES=all
ENV NVIDIA_DRIVER_CAPABILITIES=compute,utility

ENV PORT=8000
EXPOSE 8000

ENTRYPOINT ["/app/nacelle-bundle"]
"#
    .to_string()
}

fn dockerignore_template() -> String {
    // Prefer a small build context; whitelist common manifest/artifact files.
    r#"# Ignore everything by default
**

# Allow the scaffold + capsule metadata
!Dockerfile
!.dockerignore
!capsule.toml

# Allow the self-extracting bundle artifact
!nacelle-bundle

# Optional: include lockfiles/dep manifests if you extend the Dockerfile
!requirements.txt
!pyproject.toml
!poetry.lock
!uv.lock
!package.json
!package-lock.json
!pnpm-lock.yaml
!yarn.lock
!bun.lockb
"#
    .to_string()
}
