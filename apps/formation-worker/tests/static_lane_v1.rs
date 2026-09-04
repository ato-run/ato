//! The Static Formation lane.
//!
//! A Static Compute is evaluated by the browser: no process, no Runner lease,
//! no port. The lane's whole job is to hand the DECLARED output root to the
//! canonical materializer and record what it produced.
//!
//! The materializer is called, never reimplemented. A second `static_web_*`
//! implementation would give two answers about an artifact's identity, and
//! existing artifacts would stop matching one of them.

use std::path::Path;

use ato_formation::detect::{FieldOrigins, detect};
use ato_formation::intent::{AuthoredOverrides, Lane, compile_build_plan, compile_intent};
use ato_formation_worker::static_lane::{materialize_static, needs_build};

fn site(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn overrides(pairs: &[(&str, &str)]) -> AuthoredOverrides {
    AuthoredOverrides(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    )
}

fn compile(
    dir: &Path,
    over: &AuthoredOverrides,
) -> (
    ato_formation::intent::ProgramIntentV1,
    ato_formation::intent::EffectiveBuildPlanV1,
) {
    let evidence = detect(dir).expect("detects");
    let mut origins = FieldOrigins::new();
    let intent = compile_intent(&evidence, over, "/app", &mut origins).expect("compiles");
    let plan = compile_build_plan(&intent, "/app", "wasm32-browser").expect("plans");
    (intent, plan)
}

#[test]
fn a_root_html_site_forms_without_a_build() {
    let dir = site(&[
        ("index.html", "<!doctype html><h1>hello</h1>"),
        ("style.css", "body{margin:0}"),
    ]);
    let (intent, plan) = compile(dir.path(), &AuthoredOverrides::default());
    assert_eq!(intent.lane, Lane::StaticWeb);
    // No interpreter to provision and nothing to install: the lane needs no
    // build sandbox for a source that is already a site.
    assert!(!needs_build(&plan));

    let destination = tempfile::tempdir().expect("tempdir");
    let output = materialize_static(
        &intent,
        &plan,
        dir.path(),
        destination.path(),
        "swm_test_root",
        &[],
    )
    .expect("materializes");

    assert!(output.manifest_digest.starts_with("sha256:"));
    assert_eq!(output.entry_path, "index.html");
    assert!(output.total_bytes > 0);
    // The producer wrote an immutable bundle, not a deployment.
    assert!(output.bundle.bundle_root.join("blobs/sha256").is_dir());
}

#[test]
fn the_same_site_materializes_to_the_same_identity() {
    // Determinism is the property an existing artifact depends on: a rebuild
    // that produced a different manifest digest would make every stored
    // artifact look stale.
    let files: &[(&str, &str)] = &[("index.html", "<!doctype html><h1>hello</h1>")];
    let first = site(files);
    let second = site(files);
    let (intent_one, plan_one) = compile(first.path(), &AuthoredOverrides::default());
    let (intent_two, plan_two) = compile(second.path(), &AuthoredOverrides::default());

    let dest_one = tempfile::tempdir().expect("tempdir");
    let dest_two = tempfile::tempdir().expect("tempdir");
    let one = materialize_static(
        &intent_one,
        &plan_one,
        first.path(),
        dest_one.path(),
        "swm_same",
        &[],
    )
    .expect("materializes");
    let two = materialize_static(
        &intent_two,
        &plan_two,
        second.path(),
        dest_two.path(),
        "swm_same",
        &[],
    )
    .expect("materializes");
    assert_eq!(one.manifest_digest, two.manifest_digest);
}

#[test]
fn a_declared_output_root_the_build_did_not_produce_is_refused() {
    let dir = site(&[("index.html", "<!doctype html><h1>hi</h1>")]);
    let (intent, plan) = compile(
        dir.path(),
        &overrides(&[("lane", "static_web"), ("static.output_root", "dist")]),
    );
    let destination = tempfile::tempdir().expect("tempdir");
    let error = materialize_static(
        &intent,
        &plan,
        dir.path(),
        destination.path(),
        "swm_missing",
        &[],
    )
    .unwrap_err();
    // Falling back to the whole workspace would publish the source tree as a
    // site.
    assert!(format!("{error}").contains("did not produce"), "{error}");
}

#[test]
fn a_declared_output_root_is_used_when_it_exists() {
    let dir = site(&[
        ("dist/index.html", "<!doctype html><h1>built</h1>"),
        ("src/main.js", "console.log(1)"),
    ]);
    let (intent, plan) = compile(
        dir.path(),
        &overrides(&[("lane", "static_web"), ("static.output_root", "dist")]),
    );
    let destination = tempfile::tempdir().expect("tempdir");
    let output = materialize_static(
        &intent,
        &plan,
        dir.path(),
        destination.path(),
        "swm_dist",
        &[],
    )
    .expect("materializes");

    // The source tree is NOT in the bundle; only the declared output is.
    let manifest = String::from_utf8_lossy(&output.bundle.manifest_bytes).into_owned();
    assert!(manifest.contains("index.html"));
    assert!(
        !manifest.contains("main.js"),
        "the source tree must not be published"
    );
}

#[test]
fn a_process_intent_is_refused_by_this_lane() {
    let dir = site(&[
        ("requirements.txt", "fastapi==0.115.6\n"),
        ("app.py", "app = object()\n"),
    ]);
    let (intent, plan) = compile(
        dir.path(),
        &overrides(&[
            ("launch.argv", "/app/.venv/bin/python -m app"),
            ("port.http", "8000"),
        ]),
    );
    assert_eq!(intent.lane, Lane::PythonProcess);
    let destination = tempfile::tempdir().expect("tempdir");
    // Handing a process intent to the Static materializer would publish a venv
    // as a website.
    assert!(
        materialize_static(
            &intent,
            &plan,
            dir.path(),
            destination.path(),
            "swm_wrong",
            &[]
        )
        .is_err()
    );
}

#[test]
fn an_immutable_bundle_is_never_overwritten() {
    let dir = site(&[("index.html", "<!doctype html><h1>hi</h1>")]);
    let (intent, plan) = compile(dir.path(), &AuthoredOverrides::default());
    let destination = tempfile::tempdir().expect("tempdir");
    materialize_static(
        &intent,
        &plan,
        dir.path(),
        destination.path(),
        "swm_once",
        &[],
    )
    .expect("materializes");
    // A second production into the same place is refused rather than silently
    // replacing an artifact something may already reference.
    assert!(
        materialize_static(
            &intent,
            &plan,
            dir.path(),
            destination.path(),
            "swm_once",
            &[]
        )
        .is_err()
    );
}
