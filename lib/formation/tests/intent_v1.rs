//! Detection and intent compilation.
//!
//! The claims under test are mostly about what this REFUSES. A compiler that
//! guesses an entrypoint or a state path produces builds that look fine and
//! behave wrongly, and the wrongness surfaces far from its cause.

use std::collections::BTreeMap;
use std::path::Path;

use ato_formation::detect::{FieldOrigin, FieldOrigins, detect};
use ato_formation::intent::*;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
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
            .collect::<BTreeMap<_, _>>(),
    )
}

const NOTES: &[(&str, &str)] = &[
    (
        "pyproject.toml",
        "[project]\nname = \"notes\"\nrequires-python = \">=3.12\"\n",
    ),
    ("uv.lock", "version = 1\n"),
    ("app.py", "app = object()\n"),
];

fn notes_overrides() -> AuthoredOverrides {
    overrides(&[
        (
            "launch.argv",
            "/app/.venv/bin/python -m uvicorn app:app --host 0.0.0.0 --port 8000",
        ),
        ("port.http", "8000"),
        ("readiness.http_path", "/health"),
        ("state.app_data.mount", "/data"),
        ("env.DATABASE_PATH", "/data/app.sqlite"),
    ])
}

fn compile(
    dir: &Path,
    overrides: &AuthoredOverrides,
) -> (Result<ProgramIntentV1, IntentError>, FieldOrigins) {
    let evidence = detect(dir).expect("detects");
    let mut origins = FieldOrigins::new();
    let intent = compile_intent(&evidence, overrides, "/app", &mut origins);
    (intent, origins)
}

#[test]
fn detection_reads_files_and_decides_nothing() {
    let dir = tree(NOTES);
    let evidence = detect(dir.path()).expect("detects");
    let python = evidence.python.expect("python evidence");
    assert!(python.has_pyproject && python.has_uv_lock);
    // The constraint is reported VERBATIM. A detector that resolved it would be
    // deciding, and then two components would decide the same thing.
    assert_eq!(python.requires_python.as_deref(), Some(">=3.12"));
    assert_eq!(python.top_level_modules, vec!["app.py"]);
    assert!(evidence.node.is_none());
}

#[test]
fn the_same_source_compiles_to_the_same_digests() {
    let dir = tree(NOTES);
    let first = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    let second = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    assert_eq!(
        first.canonical_digest().unwrap(),
        second.canonical_digest().unwrap()
    );
    assert_eq!(
        compile_build_plan(&first, "/app", "x86_64-linux-gnu")
            .unwrap()
            .canonical_digest()
            .unwrap(),
        compile_build_plan(&second, "/app", "x86_64-linux-gnu")
            .unwrap()
            .canonical_digest()
            .unwrap()
    );
}

#[test]
fn an_entrypoint_is_never_guessed() {
    // The fixture has FastAPI's shape: a pyproject, a lock, an `app` object in
    // app.py. Everything a heuristic would need — and it still refuses.
    let dir = tree(NOTES);
    let mut without_launch = notes_overrides();
    without_launch.0.remove("launch.argv");
    let error = compile(dir.path(), &without_launch).0.unwrap_err();
    assert_eq!(error.code(), "intent_requires_authoring");
    assert!(format!("{error}").contains("launch.argv"), "{error}");
}

#[test]
fn a_port_is_declared_rather_than_read_from_a_framework() {
    let dir = tree(NOTES);
    let mut without_port = notes_overrides();
    without_port.0.remove("port.http");
    assert_eq!(
        compile(dir.path(), &without_port).0.unwrap_err().code(),
        "intent_requires_authoring"
    );
}

#[test]
fn a_state_slot_is_never_invented() {
    // Guessing one means the app writes somewhere that is silently discarded on
    // the next Run — which looks like data loss, and is.
    let dir = tree(NOTES);
    let mut without_state = notes_overrides();
    without_state.0.remove("state.app_data.mount");
    let intent = compile(dir.path(), &without_state).0.expect("compiles");
    assert!(intent.state_slots.is_empty(), "no slot was invented");

    let mut bad = notes_overrides();
    bad.0
        .insert("state.app_data.mount".to_owned(), "data/../..".to_owned());
    assert_eq!(
        compile(dir.path(), &bad).0.unwrap_err().code(),
        "intent_malformed"
    );
}

#[test]
fn authored_intent_outranks_inference() {
    let dir = tree(NOTES);
    let mut authored = notes_overrides();
    authored
        .0
        .insert("runtime.python".to_owned(), "3.11.11".to_owned());
    let (intent, origins) = compile(dir.path(), &authored);
    // `requires-python = ">=3.12"` in the source says otherwise; the author wins.
    assert_eq!(
        intent
            .expect("compiles")
            .runtime
            .get("python")
            .map(String::as_str),
        Some("3.11.11")
    );
    assert_eq!(origins.get("runtime.python"), Some(&FieldOrigin::Authored));
}

#[test]
fn provenance_says_where_every_decision_came_from() {
    let dir = tree(NOTES);
    let (_, origins) = compile(dir.path(), &notes_overrides());
    assert_eq!(origins.get("launch.argv"), Some(&FieldOrigin::Authored));
    assert_eq!(origins.get("state.app_data"), Some(&FieldOrigin::Authored));
    assert_eq!(
        origins.get("runtime.python"),
        Some(&FieldOrigin::DetectedFromSource)
    );
    assert_eq!(origins.get("lane"), Some(&FieldOrigin::DetectedFromSource));
}

#[test]
fn a_range_resolves_to_an_exact_catalog_version() {
    let dir = tree(NOTES);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    // `>=3.12` in the source; nothing downstream ever sees a constraint.
    assert_eq!(
        intent.runtime.get("python").map(String::as_str),
        Some("3.12.7")
    );
}

#[test]
fn an_unsupported_python_fails_closed() {
    let dir = tree(NOTES);
    let mut unsupported = notes_overrides();
    unsupported
        .0
        .insert("runtime.python".to_owned(), "3.7.0".to_owned());
    // Falling back to whatever the host has would make the build
    // unreproducible and the Runner's interpreter a coincidence.
    assert_eq!(
        compile(dir.path(), &unsupported).0.unwrap_err().code(),
        "intent_unsupported_python"
    );
}

#[test]
fn ambiguous_lockfiles_are_refused() {
    let dir = tree(&[
        ("pyproject.toml", "[project]\nname = \"x\"\n"),
        ("uv.lock", "version = 1\n"),
        ("poetry.lock", "\n"),
        ("app.py", "\n"),
    ]);
    // Two lockfiles are two answers, and picking one silently means the build
    // uses versions the author did not choose.
    assert_eq!(
        compile(dir.path(), &notes_overrides())
            .0
            .unwrap_err()
            .code(),
        "intent_ambiguous_lockfiles"
    );
}

#[test]
fn a_pyproject_without_a_lock_is_refused() {
    let dir = tree(&[
        ("pyproject.toml", "[project]\nname = \"x\"\n"),
        ("app.py", "\n"),
    ]);
    let error = compile(dir.path(), &notes_overrides()).0.unwrap_err();
    assert_eq!(error.code(), "intent_requires_authoring");
    assert!(format!("{error}").contains("resolves differently over time"));
}

#[test]
fn requirements_txt_is_accepted_but_recorded_as_weaker() {
    let dir = tree(&[("requirements.txt", "fastapi==0.115.6\n"), ("app.py", "\n")]);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    match &intent.dependencies {
        // Calling this reproducible would make a weaker guarantee look like the
        // strong one.
        DependencyPlan::PipRequirements { reproducibility } => {
            assert_eq!(reproducibility, "pinned-where-declared");
        }
        other => panic!("expected pip requirements, got {other:?}"),
    }
}

#[test]
fn a_dist_directory_does_not_select_the_static_output() {
    // A repository can carry a checked-in build directory, a vendored example
    // or a stale artifact. Publishing one because it looked like an output is
    // how the wrong bytes get served.
    let dir = tree(&[("dist/index.html", "<h1>stale</h1>"), ("src/main.js", "")]);
    let error = compile(dir.path(), &overrides(&[("lane", "static_web")]))
        .0
        .unwrap_err();
    assert_eq!(error.code(), "intent_requires_authoring");
    assert!(format!("{error}").contains("never selected because it exists"));

    let intent = compile(
        dir.path(),
        &overrides(&[("lane", "static_web"), ("static.output_root", "dist")]),
    )
    .0
    .expect("compiles");
    assert_eq!(intent.static_output_root.as_deref(), Some("dist"));
}

#[test]
fn a_root_html_site_needs_no_declaration() {
    let dir = tree(&[("index.html", "<h1>hi</h1>")]);
    let intent = compile(dir.path(), &AuthoredOverrides::default())
        .0
        .expect("compiles");
    assert_eq!(intent.lane, Lane::StaticWeb);
    assert_eq!(intent.static_output_root.as_deref(), Some(""));
    // A Static Compute is evaluated by the browser: no process, so no argv, and
    // inventing one would imply a Runner it must not need.
    assert!(intent.launch_argv.is_empty());
    assert!(intent.exported_ports.is_empty());
}

#[test]
fn a_source_that_matches_no_lane_is_refused() {
    let dir = tree(&[("README.md", "# nothing to build")]);
    assert_eq!(
        compile(dir.path(), &AuthoredOverrides::default())
            .0
            .unwrap_err()
            .code(),
        "intent_no_lane"
    );
}

#[test]
fn a_launch_outside_the_workspace_root_is_refused() {
    let dir = tree(NOTES);
    let mut escaping = notes_overrides();
    escaping.0.insert(
        "launch.argv".to_owned(),
        "/usr/bin/python3 -m app".to_owned(),
    );
    // It would not exist inside the sandbox, and the failure would arrive as
    // "no such file" after a build had already run.
    assert_eq!(
        compile(dir.path(), &escaping).0.unwrap_err().code(),
        "intent_malformed"
    );
}

#[test]
fn the_build_plan_pins_what_it_installs() {
    let dir = tree(NOTES);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    let plan = compile_build_plan(&intent, "/app", "x86_64-linux-gnu").expect("plans");
    // The interpreter is PROVISIONED, never taken from the host: the acceptance
    // host runs 3.14, for which pydantic-core publishes no wheel, and a plan
    // that said `python3` fell back to compiling a Rust extension and failed.
    assert_eq!(plan.steps[0].name, "provision-python");
    assert!(plan.steps[0].argv.join(" ").contains("3.12.7"));
    let sync = &plan.steps[1];
    // `--frozen`: without it `uv` may update the lock, and the build would
    // silently resolve something else than the author committed.
    assert!(sync.argv.contains(&"--frozen".to_owned()));
    assert!(
        sync.argv
            .contains(&format!("{}/bin/python3", python_home("3.12.7"))),
        "uv must be pointed at the provisioned interpreter"
    );
    assert!(
        sync.needs_network,
        "dependency resolution is declared, not assumed"
    );
    assert_eq!(plan.workspace_guest_root, "/app");
}

#[test]
fn a_pip_venv_uses_the_provisioned_interpreter() {
    let dir = tree(&[("requirements.txt", "fastapi==0.115.6\n"), ("app.py", "\n")]);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    let plan = compile_build_plan(&intent, "/app", "x86_64-linux-gnu").expect("plans");
    assert_eq!(plan.steps[0].name, "provision-python");
    let venv = &plan.steps[1];
    let script = venv.argv.join(" ");
    // Created BY the provisioned interpreter, not by the host's.
    assert!(script.contains(&format!("{}/bin/python3", python_home("3.12.7"))));
    // Self-contained: the executable is copied AND the shared libpython is
    // vendored beside it. A symlinked venv cannot survive the artifact format
    // (which refuses links), and `--copies` alone leaves the loader without
    // libpython — both observed, the second through a Runner that had already
    // materialized the workspace.
    assert!(script.contains("--copies"));
    assert!(script.contains("libpython"));
    assert!(!venv.needs_network, "creating a venv needs no network");
    assert!(plan.steps[2].needs_network);
}

#[test]
fn a_static_plan_runs_no_steps() {
    let dir = tree(&[("index.html", "<h1>hi</h1>")]);
    let intent = compile(dir.path(), &AuthoredOverrides::default())
        .0
        .expect("compiles");
    assert!(
        compile_build_plan(&intent, "/app", "x86_64-linux-gnu")
            .expect("plans")
            .steps
            .is_empty()
    );
}

#[test]
fn detection_never_follows_a_symlink() {
    let dir = tree(&[("index.html", "<h1>hi</h1>")]);
    let secret = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(secret.path(), "[project]\nrequires-python = \"3.7\"\n").expect("write");
    std::os::unix::fs::symlink(secret.path(), dir.path().join("pyproject.toml")).expect("link");
    // Following it would let a link in the source decide what the project is.
    assert!(detect(dir.path()).expect("detects").python.is_none());
}
