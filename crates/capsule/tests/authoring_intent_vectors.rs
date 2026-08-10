//! Shared vector pinning the Rust-owned Program Intent normalizer.

use std::fs;
use std::path::{Path, PathBuf};

use capsule::authoring_intent::{
    PROGRAM_INTENT_DRAFT_V1_SCHEMA, ProgramCommandDraftV1, ProgramIntentDraftV1,
    ProgramIntentOrigin, ReadinessIntentV1, ToolchainRequirementV1, WorkspacePathV1,
    normalize_program_intent,
};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authoring_intent")
}

fn command(argv: &[&str]) -> ProgramCommandDraftV1 {
    ProgramCommandDraftV1::Argv {
        argv: argv.iter().map(|value| (*value).to_string()).collect(),
        cwd: WorkspacePathV1::root(),
        requested_environment: Vec::new(),
        required_tools: Vec::new(),
    }
}

#[test]
fn baseline_normalization_matches_shared_canonical_vector() {
    let envelope = normalize_program_intent(ProgramIntentDraftV1 {
        schema: PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::Inference,
        toolchains: vec![ToolchainRequirementV1 {
            name: "node".to_string(),
            version_constraint: "20".to_string(),
        }],
        build_steps: vec![command(&["pnpm", "install"]), command(&["pnpm", "build"])],
        launch: command(&["node", "server.js", "--label=a b"]),
        readiness: ReadinessIntentV1::Http {
            port: 8000,
            path: "/health".to_string(),
            timeout_seconds: 60,
        },
        build_output_roots: vec![WorkspacePathV1::parse("dist").expect("path")],
        static_web_output: None,
        bindings: Vec::new(),
        unresolved: Vec::new(),
    })
    .expect("normalize");
    let mut expected =
        fs::read(fixture_dir().join("baseline-normalized.canonical.json")).expect("read vector");
    assert_eq!(expected.pop(), Some(b'\n'));

    assert_eq!(
        serde_jcs::to_vec(&envelope.intent).expect("canonical"),
        expected
    );
    assert_eq!(
        envelope.digest, "blake3:cd67538fa6ed6116919ae3d9632884d587f2f0aeaa070e5f9845d523715d611b",
        "update only through the Rust SSOT vector generator"
    );
}
