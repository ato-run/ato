//! Explicit, read-only extraction boundary for Static Web Bundle v1.
//!
//! The existence of a `dist/` directory never selects this lane. A future
//! builder caller must construct [`StaticWebOutputPlan`] from an explicit
//! declared output decision after the existing Vite production image build has
//! completed. This adapter only copies an already-built image/output tree; it
//! never runs a build command or a container.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
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

    pub fn security(&self) -> Result<StaticWebSecurityV1> {
        StaticWebSecurityV1::producer_policy(self.connect_src.clone()).map_err(anyhow::Error::from)
    }

    /// Parse the API's `effective_build_plan.static_web_output` claim section.
    ///
    /// Returns `Ok(None)` when the plan has no `static_web_output` (the
    /// snapshot-only lane — a complete no-op). The `materialization_id` is
    /// always API-decided and arrives with the plan; this producer never mints
    /// one. The `image_output_root` from the plan is used verbatim (the API
    /// derives it from the authored `root` + the guest workdir).
    pub fn from_effective_build_plan_json(
        effective_build_plan: Option<&serde_json::Value>,
    ) -> Result<Option<StaticWebOutputPlan>> {
        let Some(plan) = effective_build_plan else {
            return Ok(None);
        };
        let Some(section) = plan.get("static_web_output") else {
            return Ok(None);
        };
        let materialization_id = section
            .get("materialization_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("static_web_output.materialization_id missing from the claim plan"))?
            .to_string();
        let image_output_root = section
            .get("image_output_root")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("static_web_output.image_output_root missing from the claim plan"))?
            .to_string();
        let entry_path = section
            .get("entry_path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("static_web_output.entry_path missing from the claim plan"))?
            .to_string();
        let spa_fallback = section
            .get("spa_fallback")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| anyhow!("static_web_output.spa_fallback missing from the claim plan"))?;
        let connect_src = section
            .get("connect_src")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let plan = StaticWebOutputPlan {
            materialization_id,
            image_output_root: PathBuf::from(image_output_root),
            entry_path,
            spa_fallback,
            connect_src,
        };
        plan.validate()?;
        Ok(Some(plan))
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
    // The source must be reached through REAL directories only. `symlink_metadata`
    // on the final component does not protect against an intermediate symlink
    // (e.g. `image_root/srv -> /host-sensitive` with root `srv/app/dist`): the
    // joined path resolves through it and `copy_tree_no_links` only inspects the
    // resolved subtree. Every component is therefore checked, then the canonical
    // source must remain strictly beneath the canonical image root.
    let canonical_image_root = fs::canonicalize(image_root)
        .with_context(|| format!("canonicalize static web image root {}", image_root.display()))?;
    let source = image_root.join(&plan.image_output_root);
    for component in plan.image_output_root.components() {
        let current = source
            .components()
            .take_while(|c| *c != component)
            .collect::<PathBuf>();
        let step = current.join(component.as_os_str());
        let meta = fs::symlink_metadata(&step)
            .with_context(|| format!("read static web image component {}", step.display()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            bail!(
                "static web image output path traverses a symlink or non-directory: {}",
                step.display()
            );
        }
    }
    let source_meta = fs::symlink_metadata(&source)
        .with_context(|| format!("read static web image output {}", source.display()))?;
    if source_meta.file_type().is_symlink() || !source_meta.is_dir() {
        bail!("static web image output must be a real directory");
    }
    let canonical_source = fs::canonicalize(&source)
        .with_context(|| format!("canonicalize static web image output {}", source.display()))?;
    if !canonical_source.starts_with(&canonical_image_root) {
        bail!(
            "static web image output escapes the image root: {} not under {}",
            canonical_source.display(),
            canonical_image_root.display()
        );
    }

    let workspace = tempfile::Builder::new()
        .prefix("ato-static-web-extract-")
        .tempdir()
        .context("create static web extraction workspace")?;
    let output_root = workspace.path().join("output");
    // Re-verify containment inside the copy as well, so a symlink swap between
    // the checks above and the copy cannot smuggle a file from outside the
    // image root into the immutable bundle.
    copy_tree_no_links(&source, &output_root, &canonical_image_root)?;
    Ok(ExtractedStaticWebOutput {
        _workspace: workspace,
        output_root,
    })
}

/// Whether `path` (which must already be canonicalized by the caller) is
/// strictly inside `canonical_root`. `starts_with` on Path is component-wise,
/// so a sibling that merely shares a string prefix cannot pass.
fn is_beneath(canonical_path: &Path, canonical_root: &Path) -> bool {
    canonical_path != canonical_root && canonical_path.starts_with(canonical_root)
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

fn copy_tree_no_links(source: &Path, destination: &Path, canonical_root: &Path) -> Result<()> {
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
        // TOCTOU guard: a component swapped to a symlink AFTER the type check
        // above would resolve outside the image root when canonicalized. Verify
        // the canonicalized entry remains beneath the canonical image root.
        let canonical_entry = fs::canonicalize(&source_path)
            .with_context(|| format!("canonicalize static output entry {}", source_path.display()))?;
        if !is_beneath(&canonical_entry, canonical_root) {
            bail!(
                "static output entry escapes the image root: {}",
                source_path.display()
            );
        }
        if file_type.is_dir() {
            copy_tree_no_links(&source_path, &destination_path, canonical_root)?;
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

    #[test]
    fn claim_plan_parses_only_an_explicit_static_web_section() {
        // Absent section ⇒ snapshot-only no-op.
        assert!(StaticWebOutputPlan::from_effective_build_plan_json(None).unwrap().is_none());
        assert!(StaticWebOutputPlan::from_effective_build_plan_json(
            Some(&serde_json::json!({ "schema": "ato.effective-build-plan/v1" }))
        )
        .unwrap()
        .is_none());

        // Explicit declaration ⇒ a validated plan with the API-decided id.
        let plan = StaticWebOutputPlan::from_effective_build_plan_json(Some(&serde_json::json!({
            "schema": "ato.effective-build-plan/v1",
            "static_web_output": {
                "schema": "ato.static-web-output-plan/v1",
                "materialization_id": "swm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "image_output_root": "app/dist",
                "entry_path": "index.html",
                "spa_fallback": true,
                "connect_src": ["https://api.example.com"],
                "producer_contract": "ato.static-web-producer/v1",
            }
        })))
        .expect("parses");
        let plan = plan.expect("static web section present");
        assert_eq!(plan.materialization_id, "swm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(plan.image_output_root, PathBuf::from("app/dist"));
        assert_eq!(plan.entry_path, "index.html");
        assert!(plan.spa_fallback);
        assert_eq!(plan.connect_src, ["https://api.example.com"]);

        // A declared section with an unsafe root is refused, not silently
        // accepted into the snapshot lane.
        let unsafe_plan = StaticWebOutputPlan::from_effective_build_plan_json(Some(
            &serde_json::json!({
                "schema": "ato.effective-build-plan/v1",
                "static_web_output": {
                    "schema": "ato.static-web-output-plan/v1",
                    "materialization_id": "swm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "image_output_root": "../dist",
                    "entry_path": "index.html",
                    "spa_fallback": true,
                    "connect_src": [],
                }
            }),
        ));
        assert!(unsafe_plan.is_err());
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

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_an_intermediate_symlink_escaping_the_image_root() {
        // `image_root/srv` is a symlink to a directory OUTSIDE the image root;
        // `image_output_root = srv/app/dist` would resolve through it and copy
        // host files into the bundle if only the final component were checked.
        let image = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("app/dist")).unwrap();
        fs::write(outside.path().join("app/dist/index.html"), "host-secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), image.path().join("srv")).unwrap();

        let mut escaping = plan();
        escaping.image_output_root = PathBuf::from("srv/app/dist");
        assert!(
            extract_static_web_output(image.path(), &escaping).is_err(),
            "intermediate symlink must not resolve outside the image root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_an_absolute_intermediate_symlink() {
        let image = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("dist")).unwrap();
        fs::write(outside.path().join("dist/index.html"), "host-secret").unwrap();
        // Absolute symlink target — still outside the image root.
        std::os::unix::fs::symlink(outside.path(), image.path().join("dist")).unwrap();

        let mut escaping = plan();
        escaping.image_output_root = PathBuf::from("dist");
        assert!(extract_static_web_output(image.path(), &escaping).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_a_relative_intermediate_symlink_escaping_with_parent() {
        let image = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("dist")).unwrap();
        fs::write(outside.path().join("dist/index.html"), "host-secret").unwrap();
        // `srv -> ../<outside-name>` resolves out of the image root via `..`.
        let outside_name = outside
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::os::unix::fs::symlink(format!("../{outside_name}"), image.path().join("srv"))
            .unwrap();

        let mut escaping = plan();
        escaping.image_output_root = PathBuf::from("srv/dist");
        assert!(extract_static_web_output(image.path(), &escaping).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_a_symlink_inside_the_output_tree() {
        // A symlink BELOW the selected output root is also refused (no links
        // anywhere in the closure).
        let image = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "host-secret").unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("index.html"), "built").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), output.join("leak.txt"))
            .unwrap();
        assert!(extract_static_web_output(image.path(), &plan()).is_err());
    }
}
