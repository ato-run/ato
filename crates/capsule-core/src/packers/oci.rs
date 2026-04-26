use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::error::{CapsuleError, Result};
use crate::router::ManifestData;

#[derive(Debug, Clone)]
pub struct OciPackResult {
    pub image: String,
    pub archive: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum OciEngine {
    Docker,
    Podman,
}

pub fn pack(
    plan: &ManifestData,
    output: Option<PathBuf>,
    reporter: &dyn crate::reporter::CapsuleReporter,
) -> Result<OciPackResult> {
    let engine = detect_engine()?;
    let image = resolve_image(plan)?;

    let dockerfile = resolve_dockerfile(plan);
    let context_dir = resolve_context(plan);

    if dockerfile.is_some() || plan.build_context().is_some() {
        let dockerfile = dockerfile.unwrap_or_else(|| plan.manifest_dir.join("Dockerfile"));
        let mut cmd = Command::new(engine_binary(&engine));
        cmd.arg("build")
            .arg("-t")
            .arg(&image)
            .arg("-f")
            .arg(&dockerfile)
            .arg(&context_dir)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        if let Some(target) = plan.build_target() {
            cmd.arg("--target").arg(target);
        }

        let status = cmd.status().map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to run {} build: {}",
                engine_binary(&engine),
                e
            ))
        })?;
        if !status.success() {
            return Err(CapsuleError::Pack("OCI build failed".to_string()));
        }
    } else if !image_exists(&engine, &image)? {
        return Err(CapsuleError::NotFound(
            "OCI image not found locally and no build context provided".to_string(),
        ));
    }

    let archive = if let Some(path) = output {
        save_image(&engine, &image, &path)?;
        Some(path)
    } else {
        None
    };

    if archive.is_some() {
        futures::executor::block_on(reporter.notify(format!("✅ OCI image saved: {}", image)))?;
    } else {
        futures::executor::block_on(reporter.notify(format!("✅ OCI image ready: {}", image)))?;
    }

    Ok(OciPackResult { image, archive })
}

fn detect_engine() -> Result<OciEngine> {
    if which::which("docker").is_ok() {
        return Ok(OciEngine::Docker);
    }
    if which::which("podman").is_ok() {
        return Ok(OciEngine::Podman);
    }
    Err(CapsuleError::ContainerEngine(
        "No OCI engine found (docker/podman)".to_string(),
    ))
}

fn engine_binary(engine: &OciEngine) -> &'static str {
    match engine {
        OciEngine::Docker => "docker",
        OciEngine::Podman => "podman",
    }
}

fn resolve_image(plan: &ManifestData) -> Result<String> {
    if let Some(image) = plan.build_image() {
        let tag = plan
            .build_tag()
            .or_else(|| plan.manifest_version())
            .or_else(|| Some("latest".to_string()));
        return Ok(with_tag(image, tag));
    }
    if let Some(image) = plan.targets_oci_image() {
        return Ok(image);
    }
    if let Some(image) = plan.execution_image() {
        return Ok(image);
    }

    let name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    let tag = plan
        .build_tag()
        .or_else(|| plan.manifest_version())
        .unwrap_or_else(|| "latest".to_string());
    Ok(format!("capsule/{}:{}", name, tag))
}

fn with_tag(image: String, tag: Option<String>) -> String {
    if let Some(tag) = tag {
        if image.contains(':') {
            image
        } else {
            format!("{}:{}", image, tag)
        }
    } else {
        image
    }
}

fn resolve_dockerfile(plan: &ManifestData) -> Option<PathBuf> {
    if let Some(path) = plan.build_dockerfile() {
        return Some(plan.resolve_path(&path));
    }

    let default = if plan.build_gpu() {
        let cuda = plan.manifest_dir.join("Dockerfile.cuda");
        if cuda.exists() {
            return Some(cuda);
        }
        plan.manifest_dir.join("Dockerfile")
    } else {
        plan.manifest_dir.join("Dockerfile")
    };

    if default.exists() {
        Some(default)
    } else {
        None
    }
}

fn resolve_context(plan: &ManifestData) -> PathBuf {
    plan.build_context()
        .map(|p| plan.resolve_path(&p))
        .unwrap_or_else(|| plan.manifest_dir.clone())
}

fn image_exists(engine: &OciEngine, image: &str) -> Result<bool> {
    let output = Command::new(engine_binary(engine))
        .arg("inspect")
        .arg(image)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| CapsuleError::Pack(format!("OCI inspect failed: {}", e)))?;

    Ok(output.success())
}

fn save_image(engine: &OciEngine, image: &str, output: &PathBuf) -> Result<()> {
    let status = Command::new(engine_binary(engine))
        .arg("save")
        .arg(image)
        .arg("-o")
        .arg(output)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to run {} save: {}",
                engine_binary(engine),
                e
            ))
        })?;

    if !status.success() {
        return Err(CapsuleError::Pack("OCI image save failed".to_string()));
    }

    Ok(())
}
