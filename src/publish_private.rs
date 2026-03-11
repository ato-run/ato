use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::publish_artifact::ArtifactManifestInfo;

#[derive(Debug, Clone)]
pub struct PublishPrivateArgs {
    pub registry_url: String,
    pub artifact_path: Option<PathBuf>,
    pub force_large_payload: bool,
    pub scoped_id: Option<String>,
    pub allow_existing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishPrivateResult {
    pub scoped_id: String,
    pub version: String,
    pub artifact_url: String,
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub already_existed: bool,
    pub registry_url: String,
}

#[derive(Debug, Clone)]
pub struct PublishPrivateSummary {
    pub source: &'static str,
    pub scoped_id: String,
    pub version: String,
    pub allow_existing: bool,
}

#[derive(Debug, Clone)]
enum ResolvedPublishInput {
    Build {
        manifest_path: PathBuf,
        name: String,
        version: String,
        scoped_id: String,
    },
    Artifact {
        artifact_path: PathBuf,
        version: String,
        scoped_id: String,
    },
}

pub fn summarize(args: &PublishPrivateArgs) -> Result<PublishPrivateSummary> {
    let resolved = resolve_publish_input(args)?;
    let (source, scoped_id, version) = match resolved {
        ResolvedPublishInput::Build {
            scoped_id, version, ..
        } => ("build", scoped_id, version),
        ResolvedPublishInput::Artifact {
            scoped_id, version, ..
        } => ("artifact", scoped_id, version),
    };

    Ok(PublishPrivateSummary {
        source,
        scoped_id,
        version,
        allow_existing: args.allow_existing,
    })
}

pub fn execute(args: PublishPrivateArgs) -> Result<PublishPrivateResult> {
    let resolved = resolve_publish_input(&args)?;

    let (artifact_path, scoped_id) = match resolved {
        ResolvedPublishInput::Build {
            manifest_path,
            name,
            version,
            scoped_id,
        } => {
            let artifact_path =
                crate::publish_ci::build_capsule_artifact(&manifest_path, &name, &version)
                    .with_context(|| "Failed to build artifact for private registry publish")?;
            (artifact_path, scoped_id)
        }
        ResolvedPublishInput::Artifact {
            artifact_path,
            scoped_id,
            ..
        } => (artifact_path, scoped_id),
    };

    let uploaded =
        crate::publish_artifact::publish_artifact(crate::publish_artifact::PublishArtifactArgs {
            artifact_path,
            scoped_id,
            registry_url: args.registry_url.clone(),
            force_large_payload: args.force_large_payload,
            allow_existing: args.allow_existing,
        })?;

    Ok(PublishPrivateResult {
        scoped_id: uploaded.scoped_id,
        version: uploaded.version,
        artifact_url: uploaded.artifact_url,
        file_name: uploaded.file_name,
        sha256: uploaded.sha256,
        blake3: uploaded.blake3,
        size_bytes: uploaded.size_bytes,
        already_existed: uploaded.already_existed,
        registry_url: args.registry_url,
    })
}

fn resolve_publish_input(args: &PublishPrivateArgs) -> Result<ResolvedPublishInput> {
    if let Some(artifact_path) = &args.artifact_path {
        let info = crate::publish_artifact::inspect_artifact_manifest(artifact_path)?;
        let slug = manifest_slug(&info.name)?;
        let scoped_id = resolve_scoped_id_for_artifact(args.scoped_id.as_deref(), &info, &slug)?;
        return Ok(ResolvedPublishInput::Artifact {
            artifact_path: artifact_path.clone(),
            version: info.version,
            scoped_id,
        });
    }

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    let manifest_raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml: {}", err))?;

    let slug = manifest_slug(&manifest.name)?;
    let publisher = resolve_private_publisher(&manifest_raw);
    let scoped_id = format!("{}/{}", publisher, slug);

    Ok(ResolvedPublishInput::Build {
        manifest_path,
        name: manifest.name,
        version: manifest.version,
        scoped_id,
    })
}

fn resolve_scoped_id_for_artifact(
    override_scoped_id: Option<&str>,
    info: &ArtifactManifestInfo,
    slug: &str,
) -> Result<String> {
    if let Some(explicit) = override_scoped_id {
        let scoped = crate::install::parse_capsule_ref(explicit)?;
        if scoped.slug != slug {
            anyhow::bail!(
                "--scoped-id slug '{}' must match artifact manifest.name '{}'",
                scoped.slug,
                slug
            );
        }
        return Ok(format!("{}/{}", scoped.publisher, scoped.slug));
    }

    let publisher = info
        .repository_owner
        .as_deref()
        .map(normalize_segment)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "local".to_string());
    Ok(format!("{}/{}", publisher, slug))
}

fn resolve_private_publisher(manifest_raw: &str) -> String {
    if let Some(repo_owner) = manifest_repository_owner(manifest_raw) {
        return repo_owner;
    }

    if let Ok(origin) = crate::publish_preflight::run_git(&["remote", "get-url", "origin"]) {
        if let Some(repo) = crate::publish_preflight::normalize_origin_to_repo(&origin) {
            if let Some((owner, _)) = repo.split_once('/') {
                let normalized = normalize_segment(owner);
                if !normalized.is_empty() {
                    return normalized;
                }
            }
        }
    }

    "local".to_string()
}

fn manifest_repository_owner(manifest_raw: &str) -> Option<String> {
    let raw = crate::publish_preflight::find_manifest_repository(manifest_raw)?;
    let normalized = crate::publish_preflight::normalize_repository_value(&raw).ok()?;
    let (owner, _) = normalized.split_once('/')?;
    let owner = normalize_segment(owner);
    if owner.is_empty() {
        None
    } else {
        Some(owner)
    }
}

fn manifest_slug(raw: &str) -> Result<String> {
    let slug = raw.trim();
    if slug.is_empty() {
        anyhow::bail!("capsule.toml name is empty");
    }
    let parsed = crate::install::parse_capsule_ref(&format!("local/{}", slug))
        .with_context(|| "capsule.toml name must be lowercase kebab-case")?;
    if parsed.slug != slug {
        anyhow::bail!("capsule.toml name must be lowercase kebab-case");
    }
    Ok(slug.to_string())
}

fn normalize_segment(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;

    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
            continue;
        }

        if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use super::*;
    use tar::Builder;

    fn write_test_artifact(path: &Path, name: &str, version: &str, repository: Option<&str>) {
        let repo_line = repository
            .map(|v| format!("\n[metadata]\nrepository = \"{}\"\n", v))
            .unwrap_or_default();
        let manifest = format!(
            r#"schema_version = "0.2"
name = "{name}"
version = "{version}"
type = "app"
default_target = "cli"
{repo_line}
[targets.cli]
runtime = "source"
driver = "deno"
entrypoint = "main.ts"
"#
        );
        let mut bytes = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", manifest.as_bytes())
                .expect("append manifest");
            let mut sig_header = tar::Header::new_gnu();
            let sig = "{}";
            sig_header.set_mode(0o644);
            sig_header.set_size(sig.len() as u64);
            sig_header.set_cksum();
            builder
                .append_data(&mut sig_header, "signature.json", sig.as_bytes())
                .expect("append signature");
            builder.finish().expect("finish tar");
        }
        let mut file = std::fs::File::create(path).expect("create artifact");
        file.write_all(&bytes).expect("write artifact");
    }

    #[test]
    fn summarize_artifact_mode_does_not_require_cwd_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(
            &artifact_path,
            "demo-app",
            "1.2.3",
            Some("https://github.com/Koh0920/demo-app"),
        );

        let summary = summarize(&PublishPrivateArgs {
            registry_url: "http://127.0.0.1:8787".to_string(),
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: None,
            allow_existing: true,
        })
        .expect("summarize");

        assert_eq!(summary.source, "artifact");
        assert_eq!(summary.scoped_id, "koh0920/demo-app");
        assert_eq!(summary.version, "1.2.3");
        assert!(summary.allow_existing);
    }

    #[test]
    fn summarize_artifact_mode_prefers_explicit_scoped_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(&artifact_path, "demo-app", "1.2.3", None);

        let summary = summarize(&PublishPrivateArgs {
            registry_url: "http://127.0.0.1:8787".to_string(),
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: Some("team-x/demo-app".to_string()),
            allow_existing: false,
        })
        .expect("summarize");

        assert_eq!(summary.scoped_id, "team-x/demo-app");
    }
}
