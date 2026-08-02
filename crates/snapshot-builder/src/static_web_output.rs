//! Explicit, read-only extraction boundary for Static Web Bundle v1.
//!
//! The existence of a `dist/` directory never selects this lane. A future
//! builder caller must construct [`StaticWebOutputPlan`] from an explicit
//! declared output decision after the existing Vite production image build has
//! completed. This adapter only copies an already-built image/output tree; it
//! never runs a build command or a container.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use capsule::contract::static_web_manifest::{
    StaticWebSecurityV1, validate_connect_source, validate_relative_path,
};
use tempfile::TempDir;

/// An explicit materialization decision for immutable static output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticWebOutputPlan {
    /// Supplied by the materialization owner; this producer never invents it.
    pub materialization_id: String,
    /// Relative path inside a mounted/exported built image root.
    pub image_output_root: PathBuf,
    pub entry_path: String,
    pub spa_fallback: bool,
    /// Exact public origins. Frame ancestors are fixed by the v1 contract.
    pub connect_src: Vec<String>,
}

impl StaticWebOutputPlan {
    pub fn validate(&self) -> Result<()> {
        if self.materialization_id.is_empty()
            || self.materialization_id.len() > 128
            || !self
                .materialization_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            bail!("static web materialization_id must match [A-Za-z0-9_-]{{1,128}}");
        }
        validate_output_root(&self.image_output_root)?;
        validate_relative_path(&self.entry_path).map_err(anyhow::Error::from)?;
        for origin in &self.connect_src {
            validate_connect_source(origin).map_err(anyhow::Error::from)?;
        }
        if has_duplicates(&self.connect_src) {
            bail!("static web connect_src contains a duplicate origin");
        }
        Ok(())
    }

    pub fn security(&self) -> StaticWebSecurityV1 {
        StaticWebSecurityV1::producer_policy(self.connect_src.clone())
    }
}

/// A temporary, independent copy of built static output. Dropping this value
/// removes the extraction workspace, including any image-export sentinel.
#[derive(Debug)]
pub struct ExtractedStaticWebOutput {
    _workspace: TempDir,
    output_root: PathBuf,
}

impl ExtractedStaticWebOutput {
    pub fn output_root(&self) -> &Path {
        &self.output_root
    }
}

/// Copies only the explicitly selected output tree from an already mounted or
/// exported image root. This function is deliberately read-only with respect to
/// `image_root`; production integration may provide the image root via Docker
/// create/export, but must not execute the container or rebuild Vite here.
pub fn extract_static_web_output(
    image_root: &Path,
    plan: &StaticWebOutputPlan,
) -> Result<ExtractedStaticWebOutput> {
    plan.validate()?;
    let source = image_root.join(&plan.image_output_root);
    let source_meta = fs::symlink_metadata(&source)
        .with_context(|| format!("read static web image output {}", source.display()))?;
    if source_meta.file_type().is_symlink() || !source_meta.is_dir() {
        bail!("static web image output must be a real directory");
    }

    let workspace = tempfile::Builder::new()
        .prefix("ato-static-web-extract-")
        .tempdir()
        .context("create static web extraction workspace")?;
    let output_root = workspace.path().join("output");
    copy_tree_no_links(&source, &output_root)?;
    Ok(ExtractedStaticWebOutput {
        _workspace: workspace,
        output_root,
    })
}

fn validate_output_root(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("static web image_output_root must be a non-empty relative path");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("static web image_output_root contains a non-normal component");
        }
    }
    Ok(())
}

fn has_duplicates(values: &[String]) -> bool {
    let mut unique = std::collections::BTreeSet::new();
    values.iter().any(|value| !unique.insert(value))
}

fn copy_tree_no_links(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("create extracted output {}", destination.display()))?;
    let mut entries = fs::read_dir(source)
        .with_context(|| format!("read static output directory {}", source.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("enumerate static output directory {}", source.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type {}", source_path.display()))?;
        if file_type.is_symlink() {
            bail!(
                "static output contains a symlink: {}",
                source_path.display()
            );
        }
        if file_type.is_dir() {
            copy_tree_no_links(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            reject_hard_link(&fs::metadata(&source_path)?, &source_path)?;
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "copy static output {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else {
            bail!(
                "static output contains a non-regular file: {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn reject_hard_link(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.nlink() != 1 {
        bail!(
            "static output contains a hard-linked file: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hard_link(_metadata: &fs::Metadata, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> StaticWebOutputPlan {
        StaticWebOutputPlan {
            materialization_id: "mat_fixture".into(),
            image_output_root: PathBuf::from("srv/app/dist"),
            entry_path: "index.html".into(),
            spa_fallback: true,
            connect_src: vec!["https://api.example.com".into()],
        }
    }

    #[test]
    fn extraction_is_read_only_and_workspace_is_removed() {
        let image = tempfile::tempdir().unwrap();
        let source = image.path().join("srv/app/dist/assets");
        fs::create_dir_all(&source).unwrap();
        fs::write(image.path().join("srv/app/dist/index.html"), "built").unwrap();
        fs::write(source.join("app.js"), "console.log(1)").unwrap();
        let sentinel = image.path().join("image-sentinel");
        fs::write(&sentinel, "must survive").unwrap();

        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        assert_eq!(
            fs::read_to_string(extracted.output_root().join("index.html")).unwrap(),
            "built"
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "must survive");
        let workspace = extracted.output_root().parent().unwrap().to_path_buf();
        drop(extracted);
        assert!(!workspace.exists());
    }

    #[test]
    fn plan_never_infers_a_dist_directory() {
        let mut explicit = plan();
        explicit.image_output_root = PathBuf::from("dist");
        explicit.validate().unwrap();
        explicit.image_output_root = PathBuf::from("../dist");
        assert!(explicit.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_hard_linked_closure_members() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("index.html"), "built").unwrap();
        fs::hard_link(output.join("index.html"), output.join("duplicate.html")).unwrap();
        assert!(extract_static_web_output(image.path(), &plan()).is_err());
    }
}
