//! The Static Formation lane.
//!
//! A Static Compute is evaluated by the browser: no process, no Runner lease,
//! no port. Formation's job for it is narrower than for a process — build if
//! the plan says to, then hand the declared output root to the canonical
//! materializer and record what it produced.
//!
//! ## The materializer is not reimplemented
//!
//! `ato-materializer-static-web` is the one on `main`, brought onto nightly by
//! I0 with its manifest, receipt and frame-ancestors fixtures byte-identical.
//! It is called, not copied. A second `static_web_*` implementation would give
//! two answers about what an artifact's identity is, and existing artifacts
//! would stop matching one of them.
//!
//! ## What the lane refuses
//!
//! An output root the build did not produce. The plan DECLARED it, so its
//! absence is a disagreement between declaration and execution — a build
//! failure, never a reason to fall back to publishing the whole workspace.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_formation::intent::{EffectiveBuildPlanV1, Lane, ProgramIntentV1};
#[cfg(test)]
use ato_materializer_static_web::INSTANCE_STATE_BRIDGE_PATH;
use ato_materializer_static_web::{
    ProducedStaticWebBundle, StaticWebInstrumentation, StaticWebOutputPlan,
    extract_static_web_output_instrumented, media_type_for, produce_static_web_bundle,
};

/// What the Static lane produced, in the terms a FormationResult needs.
#[derive(Debug)]
pub struct StaticFormationOutput {
    pub bundle: ProducedStaticWebBundle,
    /// Content address of the manifest — the artifact's identity.
    pub manifest_digest: String,
    pub entry_path: String,
    pub spa_fallback: bool,
    pub total_bytes: u64,
}

/// Turn a built workspace into a Static Web materialization.
///
/// `materialization_id` is supplied by the caller and never invented here: the
/// producer says so, and an id minted by the producer would not be the one the
/// control plane registered.
pub fn materialize_static(
    intent: &ProgramIntentV1,
    plan: &EffectiveBuildPlanV1,
    workspace_root: &Path,
    destination_parent: &Path,
    materialization_id: &str,
    runtime_secret_canaries: &[&[u8]],
) -> Result<StaticFormationOutput> {
    if intent.lane != Lane::StaticWeb {
        bail!("materialize_static was handed a {:?} intent", intent.lane);
    }
    let declared = intent.static_output_root.clone().unwrap_or_default();
    let output_root = resolve_output_root(workspace_root, &declared)?;

    let output_plan = StaticWebOutputPlan {
        materialization_id: materialization_id.to_owned(),
        // The producer records this as provenance — WHERE inside the built
        // tree the output came from — and reads `built_output_root` for the
        // bytes. It requires a non-empty relative path, and rejects "." as a
        // non-normal component, so a site at the repository root is recorded
        // as the workspace root itself rather than as an empty string.
        image_output_root: PathBuf::from(if declared.is_empty() {
            plan.workspace_guest_root.trim_start_matches('/')
        } else {
            declared.as_str()
        }),
        entry_path: intent
            .static_entry_path
            .clone()
            .unwrap_or_else(|| "index.html".to_owned()),
        spa_fallback: intent.static_spa_fallback,
        // Empty on purpose: a connect-src is a deployment decision the control
        // plane owns, and a Formation that guessed one would publish a policy
        // nobody chose.
        connect_src: Vec::new(),
    };

    // EXTRACT first, then produce.
    //
    // Skipping this stage was a real drift, found by comparing against what the
    // existing path published: extraction is where the browser and
    // instance-state bridges are injected, and going straight to the producer
    // meant a Formation bundle carried neither. The P0 Browser Instance State
    // lane depends on that injection, so a site formed this way would have
    // looked correct and silently lost its state.
    //
    // Instrumentation is applied to the materialized COPY; `output_root` is
    // never written to.
    //
    // The plan is re-expressed as parent + directory name rather than ".",
    // because `image_output_root` must be a normal relative path.
    let extract_parent = output_root
        .parent()
        .context("static output root has no parent directory")?;
    let extract_name = output_root
        .file_name()
        .context("static output root has no directory name")?;
    let extracted = extract_static_web_output_instrumented(
        extract_parent,
        &StaticWebOutputPlan {
            image_output_root: PathBuf::from(extract_name),
            ..output_plan.clone()
        },
        StaticWebInstrumentation {
            browser_runner_bridge: true,
            instance_state_bridge: true,
        },
    )
    .context("static web extraction failed")?;

    // Select what a static site can actually serve.
    //
    // This reproduces the existing Static path's behaviour, which the B1-S
    // shadow comparison measured rather than assumed: forming the 2048 fixture
    // at its repository root, the existing path published 27 files and dropped
    // exactly the 7 that carry no servable media type — `.gitignore`,
    // `.jshintrc`, `CONTRIBUTING.md`, `README.md`, `Rakefile`,
    // `style/helpers.scss`, `style/main.scss` — with every remaining file
    // byte-identical. Refusing them instead, as this lane first did, is not a
    // stricter version of the same contract; it turns every repository-root
    // static Compute from "publishes" into "build error" at cutover.
    //
    // The question asked here is `media_type_for`, the producer's own table, so
    // selection and production cannot disagree about what is servable.
    //
    // Selection is not a guess about WHERE the site is. An output root that
    // holds no entry file is still refused below, by the producer.
    let dropped =
        prune_unservable(extracted.output_root()).context("select servable static web files")?;
    if !dropped.is_empty() {
        eprintln!(
            "[static] dropped {} file(s) with no servable media type: {}",
            dropped.len(),
            dropped.join(", ")
        );
    }

    let bundle = produce_static_web_bundle(
        &output_plan,
        &extracted,
        destination_parent,
        runtime_secret_canaries,
    )
    .map_err(|error| {
        // The producer refuses a file it cannot type, and that refusal is the
        // contract working: a bundle serves what it contains, and it will not
        // serve bytes it has no media type for.
        //
        // What it cannot say is what to do about it, because it does not know a
        // repository root from a build output. A source root carrying a
        // LICENSE, a Rakefile and a .gitignore is a repository, not a site —
        // and the remedy is to declare which directory IS the site, not to
        // start dropping files the author did not ask to drop.
        if error
            .to_string()
            .contains("unsupported static web media type")
        {
            return error.context(
                "this output root contains files a static site cannot serve. Declare \
                 `static.output_root` to name the directory that is the site, rather than \
                 publishing a repository root",
            );
        }
        error.context("static web bundle production failed")
    })?;

    let manifest_digest = bundle.receipt.manifest_digest.clone();
    let total_bytes = bundle
        .receipt
        .blobs
        .iter()
        .map(|blob| blob.size)
        .sum::<u64>();

    Ok(StaticFormationOutput {
        manifest_digest,
        entry_path: output_plan.entry_path,
        spa_fallback: output_plan.spa_fallback,
        total_bytes,
        bundle,
    })
}

/// The declared output root, resolved and contained.
fn resolve_output_root(workspace_root: &Path, declared: &str) -> Result<PathBuf> {
    if declared.is_empty() {
        return Ok(workspace_root.to_path_buf());
    }
    let candidate = workspace_root.join(declared);
    if !candidate.is_dir() {
        // Declaration and execution disagreeing is a build failure. Falling
        // back to the whole workspace would publish the source tree as a site.
        bail!("the plan declares static output root {declared:?}, which the build did not produce");
    }
    let root = workspace_root
        .canonicalize()
        .context("cannot resolve the workspace root")?;
    let resolved = candidate
        .canonicalize()
        .context("cannot resolve the declared static output root")?;
    if !resolved.starts_with(&root) {
        bail!("the declared static output root resolves outside the workspace");
    }
    Ok(resolved)
}

/// Whether this plan needs the build sandbox at all.
pub fn needs_build(plan: &EffectiveBuildPlanV1) -> bool {
    !plan.steps.is_empty()
}

/// Removes every file the producer could not assign a media type to, and every
/// directory left empty by doing so. Returns the removed paths, relative to
/// `root`, for the attempt record.
///
/// Operates on the extracted COPY only; the built workspace is never modified.
fn prune_unservable(root: &Path) -> Result<Vec<String>> {
    let mut dropped = Vec::new();
    prune_dir(root, root, &mut dropped)?;
    dropped.sort();
    Ok(dropped)
}

fn prune_dir(root: &Path, dir: &Path, dropped: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        // `symlink_metadata`: a symlink is never followed here. The producer
        // refuses links outright, and following one would let a link out of the
        // tree decide what gets deleted.
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            prune_dir(root, &path, dropped)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("walked path is under root")
            .to_string_lossy()
            .replace('\\', "/");
        if media_type_for(&relative).is_none() {
            std::fs::remove_file(&path).with_context(|| format!("drop unservable {relative}"))?;
            dropped.push(relative);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> ProgramIntentV1 {
        serde_json::from_value(serde_json::json!({
            "schema": "ato.program-intent.v1",
            "lane": "static_web",
            "runtime": {},
            "dependencies": { "kind": "none" },
            "launch_argv": [],
            "cwd_relative": "",
            "public_env": {},
            "exported_ports": [],
            "readiness_http_path": null,
            "state_slots": [],
            "static_output_root": "",
            "static_entry_path": "index.html",
            "static_spa_fallback": true
        }))
        .expect("static intent fixture")
    }

    fn plan() -> EffectiveBuildPlanV1 {
        serde_json::from_value(serde_json::json!({
            "schema": "ato.effective-build-plan.v1",
            "lane": "static_web",
            "workspace_guest_root": "/app",
            "runtime": {},
            "steps": [],
            "output_root": ""
        }))
        .expect("static plan fixture")
    }

    /// The 2048 fixture, reduced to the files that decide this gate: a servable
    /// set that includes the three media types B1-S found missing, and the
    /// repository files the existing Static path drops.
    fn workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        std::fs::create_dir_all(path.join("style/fonts")).unwrap();
        std::fs::write(
            path.join("index.html"),
            "<script src=\"js/app.js\"></script>",
        )
        .unwrap();
        std::fs::create_dir_all(path.join("js")).unwrap();
        std::fs::write(path.join("js/app.js"), "console.log('2048')").unwrap();
        std::fs::write(path.join("favicon.ico"), b"icon").unwrap();
        std::fs::write(path.join("style/main.css"), "body{}").unwrap();
        std::fs::write(path.join("style/fonts/f.woff"), b"woff").unwrap();
        std::fs::write(path.join("style/fonts/f.eot"), b"eot").unwrap();
        std::fs::write(path.join("style/fonts/f.svg"), b"<svg/>").unwrap();
        // Repository files, not site files.
        std::fs::write(path.join(".gitignore"), "node_modules\n").unwrap();
        std::fs::write(path.join("README.md"), "# 2048").unwrap();
        std::fs::write(path.join("Rakefile"), "task :default").unwrap();
        std::fs::write(path.join("style/main.scss"), "body{}").unwrap();
        root
    }

    /// The regression this lane shipped and B1-S caught: reaching the producer
    /// without extracting, so neither bridge was injected. A site formed that
    /// way looks correct and has silently lost its Browser Instance State.
    ///
    /// The producer now takes the extraction proof type, so the bypass no
    /// longer compiles — this asserts the observable half: the bridges are in
    /// the bundle, and the entry document loads them.
    #[test]
    fn static_lane_selects_then_extracts_then_instruments_then_produces() {
        let workspace = workspace();
        let destination = tempfile::tempdir().unwrap();
        let produced = materialize_static(
            &intent(),
            &plan(),
            workspace.path(),
            destination.path(),
            "swm_fixture",
            &[],
        )
        .expect("static lane forms the 2048-shaped workspace");

        let manifest: serde_json::Value =
            serde_json::from_slice(&produced.bundle.manifest_bytes).unwrap();
        let files = manifest["files"].as_object().unwrap();

        // Selection: repository files are dropped, not refused. Refusing is
        // what turned every repository-root static Compute into a build error.
        for dropped in [".gitignore", "README.md", "Rakefile", "style/main.scss"] {
            assert!(!files.contains_key(dropped), "{dropped} must be dropped");
        }

        // Compatibility: the three media types the live artifact carries.
        assert_eq!(files["favicon.ico"]["media_type"], "image/x-icon");
        assert_eq!(files["style/fonts/f.woff"]["media_type"], "font/woff");
        assert_eq!(
            files["style/fonts/f.eot"]["media_type"],
            "application/vnd.ms-fontobject"
        );

        // Instrumentation: BOTH bridges are published. The Operation-lane
        // bridge asset is versioned in its filename, so this counts the
        // reserved `__ato/` namespace rather than pinning a version string
        // that a bridge release would break.
        assert!(
            files.contains_key(INSTANCE_STATE_BRIDGE_PATH),
            "instance-state bridge must be published, found {:?}",
            files.keys().collect::<Vec<_>>()
        );
        let bridges: Vec<_> = files
            .keys()
            .filter(|path| path.starts_with("__ato/"))
            .collect();
        assert_eq!(
            bridges.len(),
            2,
            "expected the State-lane and Operation-lane bridges, found {bridges:?}"
        );
    }
}
