//! `capsule.toml` all the way to a Program Intent.
//!
//! The unit tests beside the parser pin the vocabulary it produces. These pin
//! what that vocabulary MEANS once the intent compiler reads it, because the
//! whole value of having one parser is that a manifest and a hand-written
//! override reach the same intent — and a test of the parser alone would not
//! notice the day they stopped doing so.

use std::path::Path;

use ato_formation::detect::{FieldOrigin, FieldOrigins, detect};
use ato_formation::intent::{
    AuthoredOverrides, IntentError, Lane, ProgramIntentV1, compile_intent,
};
use ato_formation::manifest::{
    MANIFEST_FILE_NAME, ManifestError, parse_manifest_overrides, read_manifest_overrides,
};

/// The Step 10 fixture: FastAPI on uvicorn, SQLite under a declared slot.
const FIXTURE_MANIFEST: &str = r#"
schema_version = "1"
name = "fastapi-sqlite-personal"

[tools]
python = "3.12.7"

[run]
command = "/opt/ato/toolchains/python/3.12.7/bin/python3 -m uvicorn main:app --host 0.0.0.0 --port 8000"

[web]
port = 8000
readiness_path = "/health"

[state.app_data]
mount = "/data"

[env]
APP_DB_PATH = "/data/app.sqlite"
"#;

const FIXTURE_SOURCE: &[(&str, &str)] = &[
    ("main.py", "app = object()\n"),
    ("requirements.txt", "fastapi==0.115.0\nuvicorn==0.30.6\n"),
];

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn fixture_tree(manifest: &str) -> tempfile::TempDir {
    let dir = tree(FIXTURE_SOURCE);
    std::fs::write(dir.path().join(MANIFEST_FILE_NAME), manifest).expect("write manifest");
    dir
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

fn compile_from_tree(dir: &Path) -> (Result<ProgramIntentV1, IntentError>, FieldOrigins) {
    let overrides = read_manifest_overrides(dir)
        .expect("manifest parses")
        .expect("manifest present");
    compile(dir, &overrides)
}

#[test]
fn the_fixture_manifest_becomes_the_process_the_author_declared() {
    let dir = fixture_tree(FIXTURE_MANIFEST);
    let (intent, origins) = compile_from_tree(dir.path());
    let intent = intent.expect("compiles");

    assert_eq!(intent.lane, Lane::PythonProcess);
    assert_eq!(
        intent.runtime.get("python").map(String::as_str),
        Some("3.12.7")
    );
    assert_eq!(
        intent.launch_argv,
        vec![
            "/opt/ato/toolchains/python/3.12.7/bin/python3",
            "-m",
            "uvicorn",
            "main:app",
            "--host",
            "0.0.0.0",
            "--port",
            "8000",
        ]
    );
    assert_eq!(intent.exported_ports, vec![("http".to_owned(), 8000)]);
    assert_eq!(intent.readiness_http_path.as_deref(), Some("/health"));
    assert_eq!(
        intent.state_slots,
        vec![("app_data".to_owned(), "/data".to_owned())]
    );
    assert_eq!(
        intent.public_env.get("APP_DB_PATH").map(String::as_str),
        Some("/data/app.sqlite")
    );

    // Every one of those came from the author, and the provenance says so.
    // "Why is it running that?" must be answerable without re-running the
    // detector, and "the detector decided" is the wrong answer here.
    for field in [
        "lane",
        "runtime.python",
        "launch.argv",
        "port.http",
        "readiness.http_path",
        "state.app_data",
        "env.APP_DB_PATH",
    ] {
        assert_eq!(
            origins.get(field),
            Some(&FieldOrigin::Authored),
            "{field} must be recorded as authored"
        );
    }
}

#[test]
fn a_manifest_and_a_hand_written_override_reach_the_same_intent() {
    let dir = fixture_tree(FIXTURE_MANIFEST);
    let from_manifest = compile_from_tree(dir.path()).0.expect("compiles");

    let by_hand = AuthoredOverrides(
        [
            ("lane", "python_process"),
            ("runtime.python", "3.12.7"),
            (
                "launch.argv",
                "/opt/ato/toolchains/python/3.12.7/bin/python3 -m uvicorn main:app --host 0.0.0.0 --port 8000",
            ),
            ("port.http", "8000"),
            ("readiness.http_path", "/health"),
            ("state.app_data.mount", "/data"),
            ("env.APP_DB_PATH", "/data/app.sqlite"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect(),
    );
    let from_hand = compile(dir.path(), &by_hand).0.expect("compiles");

    assert_eq!(from_manifest, from_hand);
    assert_eq!(
        from_manifest.canonical_digest().expect("digest"),
        from_hand.canonical_digest().expect("digest"),
        "one parser means one digest; two would be two builds"
    );
}

#[test]
fn a_source_without_a_manifest_authors_nothing() {
    let dir = tree(FIXTURE_SOURCE);
    assert_eq!(read_manifest_overrides(dir.path()), Ok(None));
}

#[test]
fn a_manifest_that_declares_no_launch_is_refused_and_never_guessed() {
    // A FastAPI app with `main.py` right there. Nothing about that is a launch:
    // the framework and the filename are exactly what must not become an argv.
    let dir = fixture_tree("[tools]\npython = \"3.12.7\"\n");
    let error = compile_from_tree(dir.path()).0.unwrap_err();
    assert_eq!(error.code(), "intent_requires_authoring");
    assert!(format!("{error}").contains("launch.argv"), "{error}");
}

#[test]
fn a_manifest_that_declares_no_state_gets_no_slot() {
    let dir = fixture_tree(
        r#"
[run]
command = "python3 -m uvicorn main:app --host 0.0.0.0 --port 8000"
[web]
port = 8000
"#,
    );
    let intent = compile_from_tree(dir.path()).0.expect("compiles");
    assert!(
        intent.state_slots.is_empty(),
        "a slot nobody declared must not exist: {:?}",
        intent.state_slots
    );
}

#[test]
fn an_invalid_mount_is_refused_rather_than_normalised() {
    for mount in ["data", "/data/../etc", "/data/.."] {
        let dir = fixture_tree(&format!(
            "[run]\ncommand = \"python3 x.py\"\n[web]\nport = 8000\n[state.app_data]\nmount = \"{mount}\"\n"
        ));
        let error = compile_from_tree(dir.path()).0.unwrap_err();
        assert_eq!(error.code(), "intent_malformed", "{mount} must be refused");
    }
}

#[test]
fn a_manifest_never_overrules_what_the_job_said() {
    // The layering the worker applies, asserted on the values themselves: the
    // job's overrides are merged first and the manifest only fills gaps.
    let manifest = parse_manifest_overrides(FIXTURE_MANIFEST).expect("parses");
    let mut merged =
        std::collections::BTreeMap::from([("readiness.http_path".to_owned(), "/ready".to_owned())]);
    for (key, value) in manifest.0 {
        merged.entry(key).or_insert(value);
    }
    assert_eq!(
        merged.get("readiness.http_path").map(String::as_str),
        Some("/ready")
    );
    assert_eq!(merged.get("port.http").map(String::as_str), Some("8000"));
}

#[test]
fn a_manifest_that_would_change_the_build_silently_is_refused() {
    let error = parse_manifest_overrides("[build]\nsteps = []\n").unwrap_err();
    assert_eq!(
        error,
        ManifestError::UnsupportedSection {
            section: "build".to_owned()
        }
    );
}

#[test]
fn the_committed_fixture_is_the_manifest_these_tests_describe() {
    // The acceptance runbook tells somebody to upload
    // `samples/fastapi-sqlite-personal`. If that folder's manifest and the
    // fixture in this file ever drift, the runbook stops testing the thing
    // these tests pin — and nothing would say so.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/fastapi-sqlite-personal/capsule.toml"
    );
    let committed = std::fs::read_to_string(path).expect("the fixture is committed");
    let from_file = parse_manifest_overrides(&committed).expect("the fixture parses");
    let from_here = parse_manifest_overrides(FIXTURE_MANIFEST).expect("parses");
    assert_eq!(from_file, from_here);
}
