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

use crate::static_web_replay_bridge::{
    REPLAY_BRIDGE_PATH, REPLAY_BRIDGE_SCRIPT_TAG, REPLAY_BRIDGE_V0_JS,
};

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

    pub fn security(&self) -> Result<StaticWebSecurityV1> {
        StaticWebSecurityV1::producer_policy(self.connect_src.clone()).map_err(anyhow::Error::from)
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
    let replay_enabled =
        std::env::var("STATIC_WEB_REPLAY_BRIDGE_ENABLED").is_ok_and(|value| value == "true");
    extract_static_web_output_with_replay(image_root, plan, replay_enabled)
}

/// Explicit variant used by tests and staging orchestration. `false` preserves
/// the extracted bytes exactly; no replay path or HTML mutation is created.
pub fn extract_static_web_output_with_replay(
    image_root: &Path,
    plan: &StaticWebOutputPlan,
    replay_enabled: bool,
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
    if replay_enabled {
        instrument_replay_bridge(&output_root, &plan.entry_path)?;
    }
    Ok(ExtractedStaticWebOutput {
        _workspace: workspace,
        output_root,
    })
}

fn instrument_replay_bridge(output_root: &Path, entry_path: &str) -> Result<()> {
    let bridge_path = output_root.join(REPLAY_BRIDGE_PATH);
    if bridge_path.exists() {
        bail!("static output already contains reserved path {REPLAY_BRIDGE_PATH}");
    }
    let entry = output_root.join(entry_path);
    let html = fs::read_to_string(&entry)
        .with_context(|| format!("read replay entry HTML {}", entry.display()))?;
    let insertion = find_replay_insertion(&html);
    let mut instrumented = String::with_capacity(html.len() + REPLAY_BRIDGE_SCRIPT_TAG.len());
    instrumented.push_str(&html[..insertion]);
    instrumented.push_str(REPLAY_BRIDGE_SCRIPT_TAG);
    instrumented.push_str(&html[insertion..]);
    fs::create_dir_all(bridge_path.parent().expect("bridge path has parent"))
        .context("create reserved replay bridge directory")?;
    fs::write(&bridge_path, REPLAY_BRIDGE_V0_JS).context("write replay bridge adapter")?;
    fs::write(&entry, instrumented)
        .with_context(|| format!("write instrumented entry HTML {}", entry.display()))?;
    Ok(())
}

fn find_replay_insertion(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    lower
        .find("<script")
        .or_else(|| lower.find("</head>"))
        .unwrap_or(0)
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
    fn replay_flag_off_preserves_output_bytes_and_adds_nothing() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original = b"<html><script src=\"application.js\"></script></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted =
            extract_static_web_output_with_replay(image.path(), &plan(), false).unwrap();
        assert_eq!(
            fs::read(extracted.output_root().join("index.html")).unwrap(),
            original
        );
        assert!(!extracted.output_root().join(REPLAY_BRIDGE_PATH).exists());
    }

    #[test]
    fn replay_flag_on_injects_once_before_application_and_leaves_source_untouched() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original =
            "<html><head></head><body><script src=\"application.js\"></script></body></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted = extract_static_web_output_with_replay(image.path(), &plan(), true).unwrap();
        let html = fs::read_to_string(extracted.output_root().join("index.html")).unwrap();
        assert_eq!(html.matches(REPLAY_BRIDGE_SCRIPT_TAG).count(), 1);
        assert!(
            html.find(REPLAY_BRIDGE_SCRIPT_TAG).unwrap() < html.find("application.js").unwrap()
        );
        assert_eq!(
            fs::read_to_string(output.join("index.html")).unwrap(),
            original
        );
        assert_eq!(
            fs::read_to_string(extracted.output_root().join(REPLAY_BRIDGE_PATH)).unwrap(),
            REPLAY_BRIDGE_V0_JS
        );
    }

    #[test]
    fn replay_instrumentation_refuses_reserved_path_collision() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(output.join("__ato")).unwrap();
        fs::write(output.join("index.html"), "<script></script>").unwrap();
        fs::write(output.join(REPLAY_BRIDGE_PATH), "owned by app").unwrap();
        let error = extract_static_web_output_with_replay(image.path(), &plan(), true).unwrap_err();
        assert!(error.to_string().contains("reserved path"));
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
