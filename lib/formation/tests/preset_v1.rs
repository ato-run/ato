//! App Presets — the small set of shapes Ato knows how to turn into an App.
//!
//! These tests are mostly about what a preset REFUSES, and about the promise
//! that a version's meaning never moves. Measured on staging before this
//! existed: one `.html` file produced a 6 GB Python VM image, because the model
//! could only express "something that runs" and the inference machine filled
//! in the rest.

use ato_formation::detect::{FieldOrigins, detect};
use ato_formation::intent::*;
use ato_formation::preset::*;
use std::collections::BTreeMap;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn preset_of(files: &[(&str, &str)]) -> Result<AppPreset, PresetMismatch> {
    let dir = tree(files);
    select_preset(&detect(dir.path()).expect("detect"))
}

const PACKAGE_JSON: &str = r#"{"name":"a","scripts":{"build":"vite build"}}"#;

#[test]
fn a_lone_html_file_is_the_narrowest_preset() {
    // Not `static-files/v1` with one file in it. Both describe the same
    // artifact; the narrower one promises less and can be relied on more.
    assert_eq!(
        preset_of(&[("expense.html", "<h1>hi</h1>")]),
        Ok(AppPreset::SingleHtml)
    );
    // The name does not have to be `index.html` — `expense.html` is what an
    // editor exports, and demanding the rename is Ato asking to be
    // accommodated.
    assert_eq!(
        preset_of(&[("index.html", "<h1>hi</h1>")]),
        Ok(AppPreset::SingleHtml)
    );
}

#[test]
fn a_readme_beside_it_does_not_change_what_the_app_is() {
    assert_eq!(
        preset_of(&[
            ("expense.html", "<h1>hi</h1>"),
            ("README.md", "# notes"),
            (".gitignore", "node_modules"),
        ]),
        Ok(AppPreset::SingleHtml),
    );
}

#[test]
fn a_tree_with_an_index_is_a_static_website() {
    assert_eq!(
        preset_of(&[
            ("index.html", "<h1>hi</h1>"),
            ("app.js", "//"),
            ("style.css", "body{}"),
        ]),
        Ok(AppPreset::StaticFiles),
    );
}

#[test]
fn several_pages_and_no_index_is_refused_in_words_a_person_can_act_on() {
    let error = preset_of(&[("one.html", "<h1>1</h1>"), ("two.html", "<h1>2</h1>")])
        .expect_err("no entry to pick");
    assert_eq!(error.code, "preset_static_files_needs_index");
    // The message says what to do, not what our dispatch failed to match.
    assert!(error.message.contains("index.html"), "{}", error.message);
    assert!(!error.message.contains("lane"), "{}", error.message);
}

#[test]
fn a_project_is_a_built_web_app_only_when_it_can_be_built_reproducibly() {
    assert_eq!(
        preset_of(&[("package.json", PACKAGE_JSON), ("package-lock.json", "{}")]),
        Ok(AppPreset::NodeStatic),
    );

    // No lockfile: `npm ci` cannot run, and `npm install` would resolve
    // something different on a different day.
    let error = preset_of(&[("package.json", PACKAGE_JSON)]).expect_err("no lockfile");
    assert_eq!(error.code, "preset_node_static_needs_lockfile");
    assert!(
        error.message.contains("package-lock.json"),
        "{}",
        error.message
    );

    // No build script: there is nothing to run.
    let error = preset_of(&[
        ("package.json", r#"{"name":"a"}"#),
        ("package-lock.json", "{}"),
    ])
    .expect_err("no build script");
    assert_eq!(error.code, "preset_node_static_needs_build_script");
    assert!(error.message.contains("dist/"), "{}", error.message);
}

#[test]
fn nothing_recognisable_is_refused_without_naming_our_dispatch() {
    let error = preset_of(&[("notes.txt", "hello")]).expect_err("not an app");
    assert_eq!(error.code, "preset_no_match");
    assert!(error.message.contains("HTML file"), "{}", error.message);
}

#[test]
fn no_framework_is_ever_detected() {
    // React, Vue, Svelte, Astro — all one preset, deliberately. A framework
    // branch is how the inference machine grows back.
    for framework in ["react", "vue", "svelte", "astro", "next"] {
        let package = format!(
            r#"{{"name":"a","dependencies":{{"{framework}":"1"}},"scripts":{{"build":"x"}}}}"#
        );
        assert_eq!(
            preset_of(&[
                ("package.json", package.as_str()),
                ("package-lock.json", "{}"),
            ]),
            Ok(AppPreset::NodeStatic),
            "{framework} must not get its own branch",
        );
    }
}

// ── the version contract ────────────────────────────────────────────────────

#[test]
fn a_presets_meaning_is_pinned_by_its_version() {
    // If `node-static/v1` means `npm ci -> npm run build -> dist/`, it means
    // that forever: every artifact already formed under it keeps meaning what
    // it meant. A different rule is `v2`, never a quiet redefinition.
    assert_eq!(AppPreset::NodeStatic.id(), "node-static/v1");
    assert_eq!(NODE_STATIC_OUTPUT_ROOT, "dist");
    assert_eq!(AppPreset::SingleHtml.id(), "single-html/v1");
    assert_eq!(AppPreset::StaticFiles.id(), "static-files/v1");
    assert_eq!(CANONICAL_ENTRY, "index.html");
}

#[test]
fn the_security_matrix_is_a_property_of_the_preset() {
    // Not of a scan of the source. A build-free preset can take a public
    // untrusted upload; one that installs from a registry cannot.
    assert!(!AppPreset::SingleHtml.resolves_dependencies());
    assert!(!AppPreset::StaticFiles.resolves_dependencies());
    assert!(AppPreset::NodeStatic.resolves_dependencies());
}

#[test]
fn node_is_never_the_word_shown_to_a_person() {
    // It is the tool that BUILDS the App, not what the App runs on.
    for preset in [
        AppPreset::SingleHtml,
        AppPreset::StaticFiles,
        AppPreset::NodeStatic,
    ] {
        let label = preset.label();
        assert!(!label.to_lowercase().contains("node"), "{label}");
        assert!(!label.to_lowercase().contains("npm"), "{label}");
    }
    assert_eq!(AppPreset::NodeStatic.label(), "Built web app");
}

// ── presets compile through the ordinary intent path ────────────────────────

fn intent_for(preset: AppPreset, files: &[(&str, &str)]) -> ProgramIntentV1 {
    let dir = tree(files);
    let evidence = detect(dir.path()).expect("detect");
    let mut origins = FieldOrigins::default();
    compile_intent_for_preset(
        preset,
        &evidence,
        &AuthoredOverrides(BTreeMap::new()),
        "/app",
        &mut origins,
    )
    .expect("preset compiles")
}

#[test]
fn single_html_builds_nothing_and_runs_nothing() {
    // The whole point. Before presets, this shape produced a 6 GB VM image.
    let intent = intent_for(AppPreset::SingleHtml, &[("index.html", "<h1>hi</h1>")]);
    assert_eq!(intent.lane, Lane::StaticWeb);
    assert!(intent.static_build.is_none(), "single-html/v1 never builds");
    assert!(intent.launch_argv.is_empty(), "single-html/v1 never runs");
    assert!(
        intent.exported_ports.is_empty(),
        "single-html/v1 has no port"
    );
    assert_eq!(intent.static_entry_path.as_deref(), Some("index.html"));

    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    assert!(plan.steps.is_empty(), "no build steps at all");
}

#[test]
fn static_files_publishes_the_tree_it_was_given() {
    let intent = intent_for(
        AppPreset::StaticFiles,
        &[("index.html", "<h1>hi</h1>"), ("app.js", "//")],
    );
    assert!(intent.static_build.is_none());
    assert_eq!(intent.static_output_root.as_deref(), Some(""));
    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    assert!(plan.steps.is_empty());
}

#[test]
fn node_static_is_npm_ci_then_npm_run_build_into_dist() {
    let intent = intent_for(
        AppPreset::NodeStatic,
        &[("package.json", PACKAGE_JSON), ("package-lock.json", "{}")],
    );
    let build = intent.static_build.as_ref().expect("built-static");
    assert_eq!(build.build_script, "build");
    assert_eq!(build.output_root, "dist");
    assert!(build.lockfile_pinned);

    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    let names: Vec<&str> = plan.steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "provision-node",
            "install-node-dependencies",
            "static-build"
        ],
    );
    assert!(plan.steps[1].argv.join(" ").contains("npm ci"));
    assert!(plan.steps[2].argv.join(" ").contains("npm run build"));
    assert_eq!(plan.output_root, "dist");
}

#[test]
fn an_explicit_override_beats_the_preset() {
    // A preset is a set of defaults. Somebody who states a value meant it.
    let dir = tree(&[
        ("index.html", "<h1>hi</h1>"),
        ("site/index.html", "<h1>x</h1>"),
    ]);
    let evidence = detect(dir.path()).expect("detect");
    let mut origins = FieldOrigins::default();
    let overrides = AuthoredOverrides(BTreeMap::from([(
        "static.output_root".to_owned(),
        "site".to_owned(),
    )]));
    let intent = compile_intent_for_preset(
        AppPreset::StaticFiles,
        &evidence,
        &overrides,
        "/app",
        &mut origins,
    )
    .expect("compiles");
    assert_eq!(intent.static_output_root.as_deref(), Some("site"));
}
