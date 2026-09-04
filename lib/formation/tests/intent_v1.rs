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
fn the_artifact_carries_dependencies_and_not_the_interpreter() {
    let dir = tree(&[("requirements.txt", "fastapi==0.115.6\n"), ("app.py", "\n")]);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    let plan = compile_build_plan(&intent, "/app", "x86_64-linux-gnu").expect("plans");

    assert_eq!(plan.steps[0].name, "provision-python");
    let layout = plan.steps[1].argv.join(" ");
    // Nothing from the venv's bin/ survives: the interpreter is a runtime
    // requirement shared across every app on the host, and vendoring it into a
    // per-app artifact took the workspace past the control plane's request cap
    // and past a Worker's memory.
    assert!(layout.contains("--without-pip"));
    assert!(layout.contains("rm -rf /app/.venv/bin"));

    let install = &plan.steps[2];
    // Installed BY the provisioned interpreter, INTO the workspace.
    assert_eq!(
        install.argv[0],
        format!("{}/bin/python3", python_home("3.12.7"))
    );
    assert!(install.argv.contains(&"--target".to_owned()));
    assert!(
        install.argv.iter().any(|a| a.contains("site-packages")),
        "dependencies must land in the workspace"
    );
    // Bytecode is derived and regenerates on first import; shipping it would
    // roughly double the artifact for nothing.
    assert!(install.argv.contains(&"--no-compile".to_owned()));
    assert!(install.needs_network);
}

#[test]
fn the_launch_is_told_where_its_dependencies_are() {
    let dir = tree(&[("requirements.txt", "fastapi==0.115.6\n"), ("app.py", "\n")]);
    let intent = compile(dir.path(), &notes_overrides()).0.expect("compiles");
    // The provisioned interpreter knows nothing about this workspace, so it is
    // told rather than expected to guess.
    assert_eq!(
        intent.public_env.get("PYTHONPATH").map(String::as_str),
        Some("/app/.venv/lib/python3.12/site-packages")
    );
}

#[test]
fn a_launch_may_name_the_provisioned_interpreter() {
    let dir = tree(&[("requirements.txt", "fastapi==0.115.6\n"), ("app.py", "\n")]);
    let mut over = notes_overrides();
    over.0.insert(
        "launch.argv".to_owned(),
        format!("{}/bin/python3 -m uvicorn app:app", python_home("3.12.7")),
    );
    // It lives outside the workspace on purpose: it is shared across every app
    // on the host, not part of any one of them.
    assert!(compile(dir.path(), &over).0.is_ok());

    // Anything else absolute is still refused — it would not exist inside the
    // sandbox.
    over.0.insert(
        "launch.argv".to_owned(),
        "/usr/bin/python3 -m app".to_owned(),
    );
    assert_eq!(
        compile(dir.path(), &over).0.unwrap_err().code(),
        "intent_malformed"
    );
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

// ─── Static Build Profile v1 ────────────────────────────────────────────────

/// Fixture B, reduced to what decides the profile. The checked-in `dist/` is a
/// DIFFERENT version of the app from the source — the same trap the real
/// `ato-e2e-static-spa@1e1be10` sets, and the reason "a dist exists" can never
/// mean "no build needed".
fn vite_fixture(extra: &[(&str, &str)]) -> tempfile::TempDir {
    let mut files = vec![
        (
            "package.json",
            r#"{"name":"f","private":true,"type":"module",
                "scripts":{"dev":"vite","build":"vite build","preview":"vite preview"},
                "devDependencies":{"vite":"^7.1.11"}}"#,
        ),
        (
            "vite.config.js",
            "import { defineConfig } from \"vite\";\n\
             export default defineConfig({ build: { outDir: \"dist\" } });\n",
        ),
        (
            "index.html",
            "<script type=\"module\" src=\"/src/app.js\"></script>",
        ),
        ("src/app.js", "console.log('NEW_BUILD_SENTINEL')"),
        // Stale, committed, and a different app.
        ("dist/index.html", "<script src=\"/app.js\"></script>"),
        ("dist/app.js", "console.log('OLD_PREBUILT_SENTINEL')"),
    ];
    files.extend_from_slice(extra);
    tree(&files)
}

fn static_intent(dir: &Path, pairs: &[(&str, &str)]) -> Result<ProgramIntentV1, IntentError> {
    let evidence = detect(dir).expect("detect");
    let mut origins = FieldOrigins::default();
    compile_intent(&evidence, &overrides(pairs), "/app", &mut origins)
}

#[test]
fn vite_source_is_built_static_and_never_publishes_the_checked_in_dist() {
    let dir = vite_fixture(&[]);
    let intent = static_intent(dir.path(), &[]).expect("vite fixture compiles");
    let build = intent
        .static_build
        .as_ref()
        .expect("a vite build/preview pair is a built-static site");

    assert_eq!(build.package_manager, PackageManager::Npm);
    assert_eq!(build.node_version, DEFAULT_NODE);
    assert_eq!(build.build_script, "build");
    assert_eq!(build.output_root, "dist");
    // No lockfile in this fixture: the plan must not imply a pinned graph.
    assert!(!build.lockfile_pinned);
    assert_eq!(
        intent.runtime.get("node").map(String::as_str),
        Some(DEFAULT_NODE)
    );
    // The output root is where the BUILD writes. That it coincides with the
    // stale committed directory is exactly why the build step matters.
    assert_eq!(intent.static_output_root.as_deref(), Some("dist"));

    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    let names: Vec<&str> = plan.steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "provision-node",
            "install-node-dependencies",
            "static-build"
        ],
        "a built-static plan provisions, installs, then builds"
    );
    assert_eq!(plan.output_root, "dist");

    let joined = plan.steps[2].argv.join(" ");
    assert!(
        joined.contains("npm run build"),
        "runs the package's own script: {joined}"
    );
    assert!(
        joined.contains(&format!("{}/bin", node_home(DEFAULT_NODE))),
        "the build runs on the PROVISIONED toolchain, not the host's node: {joined}"
    );
    // The build resolves nothing from the network; only the install does.
    assert!(plan.steps[1].needs_network);
    assert!(!plan.steps[2].needs_network);
}

#[test]
fn a_lockfile_selects_the_reproducible_install_and_its_package_manager() {
    for (lockfile, expected, command) in [
        ("package-lock.json", PackageManager::Npm, "npm ci"),
        (
            "pnpm-lock.yaml",
            PackageManager::Pnpm,
            "pnpm install --frozen-lockfile",
        ),
        (
            "yarn.lock",
            PackageManager::Yarn,
            "yarn install --frozen-lockfile",
        ),
    ] {
        let dir = vite_fixture(&[(lockfile, "{}")]);
        let intent = static_intent(dir.path(), &[]).expect("compiles");
        let build = intent.static_build.as_ref().expect("built-static");
        assert_eq!(build.package_manager, expected, "{lockfile}");
        assert!(build.lockfile_pinned, "{lockfile}");
        let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
        assert!(
            plan.steps[1].argv.join(" ").contains(command),
            "{lockfile} must install with {command}"
        );
    }
}

#[test]
fn two_package_managers_disagreeing_is_ambiguous_and_fails_closed() {
    let dir = vite_fixture(&[("package-lock.json", "{}"), ("pnpm-lock.yaml", "{}")]);
    let error = static_intent(dir.path(), &[]).expect_err("two lockfiles cannot both be right");
    assert_eq!(error.code(), "intent_ambiguous_lockfiles");
}

#[test]
fn corepack_package_manager_outranks_the_lockfiles() {
    let dir = vite_fixture(&[
        ("package-lock.json", "{}"),
        (
            "package.json",
            r#"{"name":"f","private":true,"packageManager":"pnpm@9.1.0",
                "scripts":{"build":"vite build","preview":"vite preview"}}"#,
        ),
    ]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert_eq!(
        intent.static_build.as_ref().expect("built").package_manager,
        PackageManager::Pnpm
    );
}

#[test]
fn node_version_comes_from_the_source_and_never_from_the_host() {
    for (file, contents, expected) in [
        (".nvmrc", "v22.14.0", "22.14.0"),
        (".node-version", "18.20.4", "18.20.4"),
    ] {
        let dir = vite_fixture(&[(file, contents)]);
        let intent = static_intent(dir.path(), &[]).expect("compiles");
        assert_eq!(intent.static_build.expect("built").node_version, expected);
    }

    // A RANGE resolves through the ladder, and never survives as a range.
    let dir = vite_fixture(&[(
        "package.json",
        r#"{"name":"f","private":true,"engines":{"node":">=22"},
            "scripts":{"build":"vite build","preview":"vite preview"}}"#,
    )]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert_eq!(intent.static_build.expect("built").node_version, "22.14.0");

    // A range no provisioned version satisfies is a refusal, not a default.
    let dir = vite_fixture(&[(
        "package.json",
        r#"{"name":"f","private":true,"engines":{"node":">=99"},
            "scripts":{"build":"vite build","preview":"vite preview"}}"#,
    )]);
    assert_eq!(
        static_intent(dir.path(), &[])
            .expect_err("unsatisfiable")
            .code(),
        "intent_unsupported_node"
    );
}

#[test]
fn a_literal_out_dir_override_is_honored_and_an_unreadable_one_refuses() {
    let dir = vite_fixture(&[(
        "vite.config.js",
        "export default { build: { outDir: \"site\" } };\n",
    )]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert_eq!(intent.static_build.expect("built").output_root, "site");

    // Computed: publishing `dist` here would serve a directory the build never
    // wrote — stale or empty, and reported as success.
    let dir = vite_fixture(&[(
        "vite.config.js",
        "const target = process.env.OUT;\nexport default { build: { outDir: target } };\n",
    )]);
    assert_eq!(
        static_intent(dir.path(), &[])
            .expect_err("computed outDir")
            .code(),
        "intent_requires_authoring"
    );
}

#[test]
fn source_static_stays_source_static() {
    // 2048's shape: a root index.html, no package.json at all.
    let dir = tree(&[("index.html", "<h1>2048</h1>"), ("js/app.js", "//")]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert!(
        intent.static_build.is_none(),
        "no build was declared, so none is run"
    );
    assert!(intent.runtime.is_empty());
    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    assert!(plan.steps.is_empty(), "a source-static site builds nothing");
}

#[test]
fn a_server_in_the_dependency_set_is_not_a_static_site() {
    let dir = vite_fixture(&[(
        "package.json",
        r#"{"name":"f","private":true,"dependencies":{"express":"^4"},
            "scripts":{"build":"vite build","preview":"vite preview"}}"#,
    )]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert!(
        intent.static_build.is_none(),
        "a package that depends on a server serves itself; it is not a built-static site"
    );
}

#[test]
fn a_compound_build_script_is_not_interpreted() {
    let dir = vite_fixture(&[(
        "package.json",
        r#"{"name":"f","private":true,
            "scripts":{"build":"vite build && cp -r extra dist","preview":"vite preview"}}"#,
    )]);
    let intent = static_intent(dir.path(), &[]).expect("compiles");
    assert!(
        intent.static_build.is_none(),
        "a build script with shell operators is a program, not a declaration"
    );
}

#[test]
fn authored_static_build_overrides_the_inference_in_both_directions() {
    let dir = vite_fixture(&[]);
    let intent = static_intent(dir.path(), &[("static.build", "none")]).expect("compiles");
    assert!(intent.static_build.is_none());

    // `required` with nothing to infer refuses rather than inventing a build.
    let plain = tree(&[("index.html", "<h1>hi</h1>")]);
    assert_eq!(
        static_intent(plain.path(), &[("static.build", "required")])
            .expect_err("nothing to build")
            .code(),
        "intent_requires_authoring"
    );
}

// ─── marker-less Python, under an authored lane ─────────────────────────────

#[test]
fn an_authored_python_lane_accepts_a_stdlib_program_with_no_dependency_marker() {
    // `ato-e2e-compute-server`'s shape: one module, nothing else.
    let dir = tree(&[("server.py", "import http.server\n"), ("README.md", "#")]);
    let evidence = detect(dir.path()).expect("detect");
    let mut origins = FieldOrigins::default();
    let intent = compile_intent(
        &evidence,
        &overrides(&[
            ("lane", "python_process"),
            ("launch.argv", "python3 server.py"),
            ("port.http", "8080"),
            ("readiness.http_path", "/"),
        ]),
        "/app",
        &mut origins,
    )
    .expect("an authored Python lane with a module is enough");

    assert_eq!(intent.lane, Lane::PythonProcess);
    assert_eq!(intent.launch_argv, ["python3", "server.py"]);
    assert_eq!(intent.exported_ports, [("http".to_owned(), 8080)]);
    assert_eq!(intent.readiness_http_path.as_deref(), Some("/"));
    // Nothing declared, nothing installed — said accurately rather than
    // inferred from a file that is not there.
    assert_eq!(intent.dependencies, DependencyPlan::None);
    assert_eq!(
        intent.runtime.get("python").map(String::as_str),
        Some(DEFAULT_PYTHON)
    );

    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    let names: Vec<&str> = plan.steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(
        names,
        ["provision-python"],
        "an interpreter, and no install"
    );
}

#[test]
fn a_marker_less_python_lane_is_never_chosen_on_its_own() {
    // The risk this guards: a static site with a helper script must not become
    // a Python process because a `.py` exists.
    let dir = tree(&[("index.html", "<h1>site</h1>"), ("tools/build.py", "#")]);
    let evidence = detect(dir.path()).expect("detect");
    let mut origins = FieldOrigins::default();
    let intent = compile_intent(&evidence, &overrides(&[]), "/app", &mut origins)
        .expect("compiles as what it is");
    assert_eq!(intent.lane, Lane::StaticWeb);

    // And an authored Python lane over a source with no module at all still
    // refuses: there is nothing to run.
    let dir = tree(&[("index.html", "<h1>site</h1>")]);
    let evidence = detect(dir.path()).expect("detect");
    let mut origins = FieldOrigins::default();
    let error = compile_intent(
        &evidence,
        &overrides(&[
            ("lane", "python_process"),
            ("launch.argv", "python3 server.py"),
            ("port.http", "8080"),
        ]),
        "/app",
        &mut origins,
    )
    .expect_err("no module, no lane");
    assert_eq!(error.code(), "intent_no_lane");
}
