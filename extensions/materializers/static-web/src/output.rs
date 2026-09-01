//! Explicit, read-only extraction boundary for Static Web Bundle v1.
//!
//! The existence of a `dist/` directory never selects this lane. A future
//! builder caller must construct [`StaticWebOutputPlan`] from an explicit
//! declared output decision after the existing Vite production image build has
//! completed. This adapter only copies an already-built image/output tree; it
//! never runs a build command or a container.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::{StaticWebSecurityV1, validate_connect_source, validate_relative_path};
use anyhow::{Context, Result, bail};
use tempfile::TempDir;

const BROWSER_RUNNER_BRIDGE_PATH: &str = "__ato/browser-runner-bridge-v0.1.2.js";
const BROWSER_RUNNER_BRIDGE_JS: &[u8] = include_bytes!("../assets/browser-runner-bridge-v0.1.2.js");
const BROWSER_RUNNER_BRIDGE_TAG: &str = r#"<meta name="ato-browser-runner-controller-origins" content="https://ato.run,https://stg-app.ato.run"><meta name="ato-browser-runner-verifier-origins" content="https://api.ato.run,https://staging.api.ato.run"><meta name="ato-browser-runner-state-observation" content="dom_text"><script type="module" src="/__ato/browser-runner-bridge-v0.1.2.js"></script>"#;

/// Reserved artifact path of the Browser Instance State Bridge (State lane).
pub const INSTANCE_STATE_BRIDGE_PATH: &str = "__ato/instance-state-bridge-v1.js";
/// `id` of the injected state document the bridge hydrates from. The delivery
/// edge rewrites ONLY this element's text content; `null` is the artifact
/// placeholder, which keeps the bridge inert wherever no ComputeInstance backs
/// the request (the public Static Web lane serves these same bytes).
pub const INSTANCE_STATE_ELEMENT_ID: &str = "__ato_instance_state_v1";
const INSTANCE_STATE_BRIDGE_JS: &[u8] = include_bytes!("../assets/instance-state-bridge-v1.js");
const INSTANCE_STATE_BRIDGE_TAG: &str = r#"<script id="__ato_instance_state_v1" type="application/json">null</script><script src="/__ato/instance-state-bridge-v1.js"></script>"#;

/// Which materialized-copy instrumentation an extraction applies. Every field
/// is opt-in: the default leaves the built output byte-for-byte untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaticWebInstrumentation {
    /// `ato.browser@1` Operation lane bridge.
    pub browser_runner_bridge: bool,
    /// `ato.materialize.browser@1` State lane bridge.
    pub instance_state_bridge: bool,
}

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
    extract_static_web_output_instrumented(image_root, plan, StaticWebInstrumentation::default())
}

/// Extracts a static output tree and, when explicitly selected, instruments
/// only the materialized copy before its bytes are content-addressed.
pub fn extract_static_web_output_with_browser_runner(
    image_root: &Path,
    plan: &StaticWebOutputPlan,
    browser_runner_enabled: bool,
) -> Result<ExtractedStaticWebOutput> {
    extract_static_web_output_instrumented(
        image_root,
        plan,
        StaticWebInstrumentation {
            browser_runner_bridge: browser_runner_enabled,
            ..StaticWebInstrumentation::default()
        },
    )
}

/// Extracts a static output tree, applying every selected instrumentation to
/// the materialized copy only. The source `image_root` is never written to.
pub fn extract_static_web_output_instrumented(
    image_root: &Path,
    plan: &StaticWebOutputPlan,
    instrumentation: StaticWebInstrumentation,
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
    if instrumentation.browser_runner_bridge {
        inject_browser_runner_bridge(&output_root, &plan.entry_path)?;
    }
    // Injected LAST so it lands ahead of every other script: the State lane
    // bridge is parser-blocking and must finish hydrating before any App
    // script — or the Operation lane bridge — observes `localStorage`.
    if instrumentation.instance_state_bridge {
        inject_instance_state_bridge(&output_root, &plan.entry_path)?;
    }
    Ok(ExtractedStaticWebOutput {
        _workspace: workspace,
        output_root,
    })
}

fn inject_browser_runner_bridge(output_root: &Path, entry_path: &str) -> Result<()> {
    inject_bridge(
        output_root,
        entry_path,
        "Browser Runner",
        BROWSER_RUNNER_BRIDGE_PATH,
        BROWSER_RUNNER_BRIDGE_JS,
        BROWSER_RUNNER_BRIDGE_TAG,
    )
}

fn inject_instance_state_bridge(output_root: &Path, entry_path: &str) -> Result<()> {
    inject_bridge(
        output_root,
        entry_path,
        "Instance State",
        INSTANCE_STATE_BRIDGE_PATH,
        INSTANCE_STATE_BRIDGE_JS,
        INSTANCE_STATE_BRIDGE_TAG,
    )
}

/// Writes a reserved bridge asset and splices its tag ahead of the entry
/// document's first script. Fails closed on a reserved-path collision: the
/// built output owning that path would otherwise be silently overwritten.
fn inject_bridge(
    output_root: &Path,
    entry_path: &str,
    label: &str,
    reserved_path: &str,
    asset: &[u8],
    tag: &str,
) -> Result<()> {
    let bridge_path = output_root.join(reserved_path);
    if bridge_path.exists() {
        bail!("static output already contains reserved path {reserved_path}");
    }

    let entry = output_root.join(entry_path);
    let html = fs::read_to_string(&entry)
        .with_context(|| format!("read {label} entry HTML {}", entry.display()))?;
    let insertion = find_bridge_insertion(&html);
    let mut instrumented = String::with_capacity(html.len() + tag.len());
    instrumented.push_str(&html[..insertion]);
    instrumented.push_str(tag);
    instrumented.push_str(&html[insertion..]);

    fs::create_dir_all(bridge_path.parent().expect("bridge path has parent"))
        .context("create reserved bridge directory")?;
    fs::write(&bridge_path, asset).context("write versioned bridge artifact")?;
    fs::write(&entry, instrumented)
        .with_context(|| format!("write {label} entry HTML {}", entry.display()))?;
    Ok(())
}

fn find_bridge_insertion(html: &str) -> usize {
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
    fn browser_runner_flag_off_preserves_built_bytes() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original = b"<html><script src=\"application.js\"></script></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted =
            extract_static_web_output_with_browser_runner(image.path(), &plan(), false).unwrap();
        assert_eq!(
            fs::read(extracted.output_root().join("index.html")).unwrap(),
            original
        );
        assert!(
            !extracted
                .output_root()
                .join(BROWSER_RUNNER_BRIDGE_PATH)
                .exists()
        );
    }

    #[test]
    fn browser_runner_injection_changes_only_the_materialized_copy() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original = b"<!doctype html><html><head></head><body><script src=\"application.js\"></script></body></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted =
            extract_static_web_output_with_browser_runner(image.path(), &plan(), true).unwrap();
        let materialized = fs::read_to_string(extracted.output_root().join("index.html")).unwrap();
        assert_eq!(fs::read(output.join("index.html")).unwrap(), original);
        assert_eq!(materialized.matches(BROWSER_RUNNER_BRIDGE_TAG).count(), 1);
        assert!(
            materialized.find(BROWSER_RUNNER_BRIDGE_TAG).unwrap()
                < materialized.find("application.js").unwrap()
        );
        assert_eq!(
            fs::read(extracted.output_root().join(BROWSER_RUNNER_BRIDGE_PATH)).unwrap(),
            BROWSER_RUNNER_BRIDGE_JS
        );
    }

    #[test]
    fn browser_runner_injection_fails_closed_on_reserved_path_collision() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(output.join("__ato")).unwrap();
        fs::write(output.join("index.html"), "<html></html>").unwrap();
        fs::write(output.join(BROWSER_RUNNER_BRIDGE_PATH), "attacker bytes").unwrap();

        assert!(
            extract_static_web_output_with_browser_runner(image.path(), &plan(), true).is_err()
        );
    }

    fn instrumentation(instance_state: bool) -> StaticWebInstrumentation {
        StaticWebInstrumentation {
            browser_runner_bridge: false,
            instance_state_bridge: instance_state,
        }
    }

    #[test]
    fn instance_state_flag_off_preserves_built_bytes() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original = b"<html><script src=\"application.js\"></script></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted =
            extract_static_web_output_instrumented(image.path(), &plan(), instrumentation(false))
                .unwrap();
        assert_eq!(
            fs::read(extracted.output_root().join("index.html")).unwrap(),
            original
        );
        assert!(
            !extracted
                .output_root()
                .join(INSTANCE_STATE_BRIDGE_PATH)
                .exists()
        );
    }

    #[test]
    fn instance_state_bridge_is_parser_blocking_and_precedes_app_scripts() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        let original = b"<!doctype html><html><head></head><body><script src=\"application.js\"></script></body></html>";
        fs::write(output.join("index.html"), original).unwrap();

        let extracted =
            extract_static_web_output_instrumented(image.path(), &plan(), instrumentation(true))
                .unwrap();
        let materialized = fs::read_to_string(extracted.output_root().join("index.html")).unwrap();

        assert_eq!(fs::read(output.join("index.html")).unwrap(), original);
        assert_eq!(materialized.matches(INSTANCE_STATE_BRIDGE_TAG).count(), 1);
        // The App must never read localStorage before hydration completes.
        assert!(
            materialized.find(INSTANCE_STATE_BRIDGE_TAG).unwrap()
                < materialized.find("application.js").unwrap()
        );
        // Neither `defer` nor `async`: the tag has to block the parser.
        assert!(!INSTANCE_STATE_BRIDGE_TAG.contains("defer"));
        assert!(!INSTANCE_STATE_BRIDGE_TAG.contains("async"));
        // The state document is a placeholder in the artifact; only the
        // delivery edge that knows the ComputeInstance fills it in.
        assert!(materialized.contains(
            "<script id=\"__ato_instance_state_v1\" type=\"application/json\">null</script>"
        ));
        assert_eq!(
            fs::read(extracted.output_root().join(INSTANCE_STATE_BRIDGE_PATH)).unwrap(),
            INSTANCE_STATE_BRIDGE_JS
        );
    }

    #[test]
    fn instance_state_bridge_hydrates_before_the_operation_lane_bridge() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(&output).unwrap();
        fs::write(
            output.join("index.html"),
            b"<!doctype html><html><head></head><body><script src=\"application.js\"></script></body></html>",
        )
        .unwrap();

        let extracted = extract_static_web_output_instrumented(
            image.path(),
            &plan(),
            StaticWebInstrumentation {
                browser_runner_bridge: true,
                instance_state_bridge: true,
            },
        )
        .unwrap();
        let materialized = fs::read_to_string(extracted.output_root().join("index.html")).unwrap();

        assert!(
            materialized.find(INSTANCE_STATE_BRIDGE_TAG).unwrap()
                < materialized
                    .find(r#"<script type="module" src="/__ato/browser-runner-bridge-v0.1.2.js">"#)
                    .unwrap()
        );
        assert!(
            extracted
                .output_root()
                .join(BROWSER_RUNNER_BRIDGE_PATH)
                .exists()
        );
        assert!(
            extracted
                .output_root()
                .join(INSTANCE_STATE_BRIDGE_PATH)
                .exists()
        );
    }

    #[test]
    fn instance_state_injection_fails_closed_on_reserved_path_collision() {
        let image = tempfile::tempdir().unwrap();
        let output = image.path().join("srv/app/dist");
        fs::create_dir_all(output.join("__ato")).unwrap();
        fs::write(output.join("index.html"), "<html></html>").unwrap();
        fs::write(output.join(INSTANCE_STATE_BRIDGE_PATH), "attacker bytes").unwrap();

        assert!(
            extract_static_web_output_instrumented(image.path(), &plan(), instrumentation(true))
                .is_err()
        );
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
    fn plan_rejects_parent_traversal() {
        let mut plan = plan();
        plan.image_output_root = PathBuf::from("../dist");
        assert!(plan.validate().is_err());
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

/// The State lane's format is owned by `ato.materialize.browser@1`, but it is
/// READ by an asset shipped from this crate. These tests are the only place
/// the two are pinned to each other; without them the bridge and the
/// Materializer could drift into two incompatible "localStorage state" formats
/// with nothing failing until a user's data silently fails to restore.
#[cfg(test)]
mod state_contract_tests {
    use ato_materializer_browser::{
        BROWSER_MATERIALIZER_ID, BrowserLocalStorageEntryV1, BrowserStateV1, encode_state,
    };

    use super::{INSTANCE_STATE_BRIDGE_JS, INSTANCE_STATE_ELEMENT_ID};

    const FIXTURE: &str =
        include_str!("../tests/fixtures/instance-state-hydration-v1/canonical.json");

    #[test]
    fn state_fixture_is_canonical_browser_materialization_v1() {
        let parsed: BrowserStateV1 = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(
            parsed.local_storage,
            vec![
                BrowserLocalStorageEntryV1 {
                    key: "expenses".to_owned(),
                    value: r#"[{"id":"e1","amount":1280,"label":"Coffee"}]"#.to_owned(),
                },
                BrowserLocalStorageEntryV1 {
                    key: "ui.theme".to_owned(),
                    value: "dark".to_owned(),
                },
            ]
        );
        // Byte-exact: the edge injects these bytes verbatim, so a re-encode
        // that differs would mean the injected document is not what the
        // Materializer would have produced for the same state.
        assert_eq!(encode_state(&parsed).unwrap(), FIXTURE.as_bytes());
        assert_eq!(BROWSER_MATERIALIZER_ID, "ato.materialize.browser@1");
    }

    #[test]
    fn bridge_asset_matches_the_state_contract() {
        let bridge = std::str::from_utf8(INSTANCE_STATE_BRIDGE_JS).unwrap();
        assert!(bridge.contains(&format!("\"{INSTANCE_STATE_ELEMENT_ID}\"")));
        assert!(bridge.contains("STATE_VERSION = 1"));
        assert!(bridge.contains("local_storage"));
        assert!(bridge.contains("\"ato.browser-instance-state@1\""));
        assert!(bridge.contains("\"/__ato/instance-state/local-storage\""));
        // Inert without an instance behind the request: the same artifact
        // bytes are served on the public Static Web lane.
        assert!(bridge.contains("if (!injected) return;"));
    }
}
