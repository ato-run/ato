//! Real Formation → retained bytes → source deletion → fresh common attempt.
use ato_formation::{
    authoring::{BoundContract, BoundDerivation},
    capsule_toml::parse_capsule_toml,
    detect::detect,
    execution::lower_retained,
    request::RuntimeProfile,
    retained::*,
    source::{DownloadedArchive, SourceLimits},
};
use ato_formation_worker::{local::probe_local_runtime, pack::pack_tree};
use ato_runtime_attempt::{
    admission::EffectAuthorization,
    attempt::{AttemptRequest, Continuation, ReceiptContext, run_attempt},
    build_sandbox::{BuildLimits, NetworkPolicy},
    executor::{ExecutedCandidate, LocalAttemptExecutor},
    formation_realizer::FormationRealizer,
    journal::AttemptJournal,
    plan::{BoundCandidate, PlannedCandidate, plan_candidate},
    retained::RetainedCandidateRealizer,
};
use std::{collections::BTreeMap, path::Path, sync::Mutex};
static SERIAL: Mutex<()> = Mutex::new(());

fn replay_case(kind: &str) {
    let _guard = SERIAL.lock().unwrap();
    if kind != "static" && !ato_runtime_attempt::build_sandbox::containment_available() {
        eprintln!("skipping process acceptance: containment unavailable");
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let manifest = if kind == "static" {
        std::fs::write(source.path().join("index.html"), "retained static").unwrap();
        "schema = 'ato.capsule/1'\n[[input]]\nid='workspace'\nuse='ato.workspace@1'\npath='.'\n[[derive.step]]\nid='app'\nuse='ato.browser@1'\nop='serve'\nsource='workspace'\nroot=''\nentry='index.html'\n".to_owned()
    } else {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/runtime-network/notes");
        ato_runtime_attempt::plan::copy_tree(&fixture, source.path()).unwrap();
        let text = std::fs::read_to_string(source.path().join("capsule.toml")).unwrap();
        if kind == "node" {
            std::fs::write(source.path().join("app.js"), "require('http').createServer((q,s)=>{s.writeHead(200);s.end('fresh node')}).listen(8000,'0.0.0.0')").unwrap();
            text.replace("name = \"python\"", "name = \"node\"")
                .replace("version = \"3.12.7\"", "version = \"22.23.4\"")
                .replace(
                    "[\"/opt/ato/toolchains/python/3.12.7/bin/python3\", \"-B\", \"/app/app.py\"]",
                    "[\"node\", \"app.js\"]",
                )
        } else {
            text
        }
    };
    let manifest = if kind == "static" {
        manifest
            + "[[port]]\nid='app.http'\nuse='ato.http@1'\nfrom='app'\n[[contract.require]]\nid='http'\nuse='ato.contract.http@1'\nport='app.http'\nmethod='GET'\npath='/'\n[contract.require.expect]\nstatus=200\n"
    } else {
        manifest
    };
    std::fs::write(source.path().join("capsule.toml"), &manifest).unwrap();
    let bytes = pack_tree(source.path()).unwrap();
    let verified = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&content_ref(&bytes))
        .unwrap()
        .verify_tree_digest(None, SourceLimits::default())
        .unwrap();
    let closure = verified.closure_ref("").unwrap();
    let planned = plan_candidate(
        &parse_capsule_toml(&manifest).unwrap(),
        &closure,
        &detect(source.path()).unwrap(),
        BTreeMap::new(),
        "/app",
        "x86_64-linux-gnu",
    )
    .unwrap();
    let shim = Path::new(env!("CARGO_BIN_EXE_ato-formation-worker"));
    let profile = probe_local_runtime();
    let journal = AttemptJournal::new(scratch.path().join("journal"));
    let first_root = scratch.path().join("first");
    std::fs::create_dir_all(&first_root).unwrap();
    let spec = planned.attempt_spec();
    let request = AttemptRequest {
        request_id: "retained-search",
        attempt_id: "source-attempt",
        label: "source",
        spec: &spec,
        contract_ref: &planned.contract_ref,
        runtime_id: "original-runtime",
        profile: &profile,
        authorization: EffectAuthorization::Unattended,
        network: NetworkPolicy::DependencyResolution,
        browser: None,
        attempt_root: &first_root,
        continuation: Continuation::Stop,
        receipt: ReceiptContext::formation(),
        interrupt: None,
        control: None,
    };
    let first = run_attempt(
        &request,
        &FormationRealizer {
            planned: &planned,
            source_root: source.path(),
            shim,
            network: NetworkPolicy::DependencyResolution,
            builder: &LocalAttemptExecutor {
                shim: shim.into(),
                network: NetworkPolicy::DependencyResolution,
                limits: BuildLimits::default(),
            },
        },
        &journal,
    );
    assert!(
        first
            .attempt
            .receipt
            .as_ref()
            .is_some_and(|r| r.fully_satisfied),
        "{:?}",
        first.attempt
    );
    let (bytes, materialization_ref, shape, validation_profile) =
        match first.verified.as_ref().unwrap() {
            ExecutedCandidate::Process { workspace_root } => {
                let bytes = pack_tree(workspace_root).unwrap();
                let reference = content_ref(&bytes);
                (
                    bytes,
                    reference,
                    RetainedShape::ProcessWorkspace {
                        binding: RetainedProcessBinding {
                            toolchains: planned.plan.toolchains.clone(),
                            package_manager: planned.plan.package_manager.as_ref().map(|m| {
                                RetainedPackageManager {
                                    name: m.name.clone(),
                                    version: m.version.clone(),
                                }
                            }),
                            python_environment: planned.plan.lane
                                == ato_formation::intent::Lane::PythonProcess,
                        },
                    },
                    "ato.retained-process-workspace/1",
                )
            }
            ExecutedCandidate::StaticWeb { output } => (
                pack_tree(&output.bundle.bundle_root).unwrap(),
                output.manifest_digest.clone(),
                RetainedShape::StaticWeb,
                "ato.retained-static-web/1",
            ),
        };
    let verified = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&content_ref(&bytes))
        .unwrap()
        .verify_tree_digest(None, SourceLimits::default())
        .unwrap();
    let descriptor = RetainedCandidateV1 {
        schema: RETAINED_SCHEMA.into(),
        shape,
        artifact: RetainedArtifact {
            content_ref: content_ref(&bytes),
            bytes: bytes.len() as u64,
            expanded_bytes: verified.expanded_bytes(),
        },
        materialization_ref,
        source_closure_ref: closure.as_str().into(),
        derivation_ref: planned.derivation_ref.clone(),
        base_contract_ref: planned.contract_ref.clone(),
        contract_ref: planned.contract_ref.clone(),
        derivation: planned.derivation.clone(),
        base_contract: planned.contract.clone(),
        browser_contract: None,
        creation_attempt_id: "source-attempt".into(),
        validation_profile: validation_profile.into(),
    };
    let path = scratch.path().join("retained.tar");
    std::fs::write(&path, &bytes).unwrap();
    let descriptor_bytes = descriptor.canonical_bytes().unwrap();
    let descriptor =
        RetainedCandidateV1::parse(&descriptor_bytes, &descriptor.retained_ref().unwrap()).unwrap();
    // Neither original source nor build workspace survives into replay.
    source.close().unwrap();
    std::fs::remove_dir_all(&first_root).unwrap();
    let replay_plan = PlannedCandidate {
        bound: BoundCandidate {
            contract: descriptor.base_contract.clone(),
            derivation: descriptor.derivation.clone(),
            contract_ref: descriptor.base_contract_ref.clone(),
            derivation_ref: descriptor.derivation_ref.clone(),
        },
        plan: lower_retained(&descriptor).unwrap(),
    };
    assert!(replay_plan.plan.actions.is_empty());
    let replay_root = scratch.path().join("fresh");
    std::fs::create_dir_all(&replay_root).unwrap();
    let spec = replay_plan.attempt_spec();
    // Corruption is rejected before any launcher sees a candidate. Merely
    // holding the previous PASS receipt cannot replace current observation.
    let check = RetainedCandidateRealizer {
        exploration: None,
        descriptor: &descriptor,
        archive: &path,
        expected_contract_ref: &descriptor.contract_ref,
        expected_derivation_ref: &descriptor.derivation_ref,
        expanded_limit: descriptor.artifact.expanded_bytes,
        shim,
    };
    use ato_runtime_attempt::realize::CandidateRealizer;
    std::fs::write(&path, b"corrupt archive").unwrap();
    assert!(
        check
            .realize("tampered", &scratch.path().join("tampered"))
            .is_err()
    );
    assert!(!scratch.path().join("tampered/realization").exists());
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        check.admit(&profile).unwrap().code,
        "retained_object_missing"
    );
    std::fs::write(&path, &bytes).unwrap();
    let replay = run_attempt(
        &AttemptRequest {
            request_id: "fresh-request",
            attempt_id: "fresh-attempt",
            label: "retained",
            spec: &spec,
            contract_ref: &descriptor.contract_ref,
            runtime_id: "other-capable-runtime",
            profile: &profile,
            authorization: EffectAuthorization::Unattended,
            network: NetworkPolicy::Denied,
            browser: None,
            attempt_root: &replay_root,
            continuation: Continuation::Stop,
            receipt: ReceiptContext::formation(),
            interrupt: None,
            control: None,
        },
        &RetainedCandidateRealizer {
            exploration: None,
            descriptor: &descriptor,
            archive: &path,
            expected_contract_ref: &descriptor.contract_ref,
            expected_derivation_ref: &descriptor.derivation_ref,
            expanded_limit: descriptor.artifact.expanded_bytes,
            shim,
        },
        &journal,
    );
    let receipt = replay.attempt.receipt.as_ref().expect("fresh receipt");
    assert!(receipt.fully_satisfied, "{:?}", replay.attempt);
    assert_eq!(receipt.contract_ref, descriptor.base_contract_ref);
    assert_eq!(receipt.derivation_ref, descriptor.derivation_ref);
    assert_eq!(
        receipt.execution.as_ref().unwrap().attempt_id.as_deref(),
        Some("fresh-attempt")
    );
    assert_ne!(receipt.execution, first.attempt.receipt.unwrap().execution);
}
#[test]
fn static_source_unavailable_fresh_replay() {
    replay_case("static");
}
#[test]
fn python_source_unavailable_fresh_replay() {
    replay_case("python");
}
#[test]
fn node_source_unavailable_fresh_replay() {
    replay_case("node");
}

/// A sealed Derivation, hand-built because the patched authoring catalog can no
/// longer produce an affected Node pin. This stands in for a candidate sealed
/// before the catalog changed: its descriptor is internally consistent, so only
/// the execution-time runtime policy can refuse it.
fn retained_process_fixture(
    node_version: &str,
) -> (RetainedCandidateV1, std::path::PathBuf, tempfile::TempDir) {
    let closure = format!("sha256:{}", "a".repeat(64));
    let base_contract: BoundContract = serde_json::from_value(serde_json::json!({
        "schema": "ato.contract/1",
        "requirements": []
    }))
    .expect("a minimal contract");
    let base_contract_ref = base_contract.contract_ref().expect("contract ref");
    let derivation: BoundDerivation = serde_json::from_value(serde_json::json!({
        "schema": "ato.derivation/1",
        "inputs": [{
            "id": "workspace",
            "protocol": "ato.workspace@1",
            "content_ref": closure,
        }],
        "runtimes": { "node": node_version },
        "steps": [{
            "id": "app",
            "protocol": "ato.process@1",
            "op": "serve",
            "argv": ["node", "app.js"],
        }],
        "ports": [{
            "id": "app.http",
            "protocol": "ato.http@1",
            "from": "app",
            "guest_port": 8000,
        }],
        "state": [],
        "effects": "pure"
    }))
    .expect("a sealed process derivation");
    let derivation_ref = derivation.derivation_ref().expect("derivation ref");
    let scratch = tempfile::tempdir().expect("tempdir");
    let bytes = b"retained process archive placeholder".to_vec();
    let content = content_ref(&bytes);
    let path = scratch.path().join("retained.tar");
    std::fs::write(&path, &bytes).expect("archive placeholder");
    let descriptor = RetainedCandidateV1 {
        schema: RETAINED_SCHEMA.into(),
        shape: RetainedShape::ProcessWorkspace {
            binding: RetainedProcessBinding {
                toolchains: [("node".to_owned(), node_version.to_owned())].into(),
                package_manager: None,
                python_environment: false,
            },
        },
        artifact: RetainedArtifact {
            content_ref: content.clone(),
            bytes: bytes.len() as u64,
            expanded_bytes: bytes.len() as u64,
        },
        materialization_ref: content,
        source_closure_ref: closure,
        derivation_ref: derivation_ref.clone(),
        base_contract_ref: base_contract_ref.clone(),
        contract_ref: base_contract_ref,
        derivation,
        base_contract,
        browser_contract: None,
        creation_attempt_id: "pre-catalog-change".into(),
        validation_profile: "ato.retained-process-workspace/1".into(),
    };
    let canonical = descriptor.canonical_bytes().expect("canonical descriptor");
    let retained_ref = descriptor.retained_ref().expect("retained ref");
    let parsed =
        RetainedCandidateV1::parse(&canonical, &retained_ref).expect("a valid retained descriptor");
    (parsed, path, scratch)
}

fn retained_admission_code(node_version: &str) -> Option<String> {
    use ato_runtime_attempt::realize::CandidateRealizer;
    let (descriptor, path, _scratch) = retained_process_fixture(node_version);
    let realizer = RetainedCandidateRealizer {
        exploration: None,
        descriptor: &descriptor,
        archive: &path,
        expected_contract_ref: &descriptor.contract_ref,
        expected_derivation_ref: &descriptor.derivation_ref,
        expanded_limit: descriptor.artifact.expanded_bytes,
        shim: Path::new("/nonexistent-shim"),
    };
    realizer
        .admit(&RuntimeProfile::default())
        .map(|failure| failure.code)
}

#[test]
fn a_retained_toolchain_pin_is_policy_checked_before_the_host_has_it() {
    // The directory check ("retained_toolchain_unavailable") asks whether the
    // host still has the recorded Node. The policy asks whether that Node may
    // run at all, and it must answer first: a host that kept an affected
    // toolchain must not make it executable.
    for version in ["18.20.4", "20.20.2", "22.14.0", "24.18.0"] {
        assert_eq!(
            retained_admission_code(version).as_deref(),
            Some("runtime_version_not_patched"),
            "{version} must be refused by policy"
        );
    }
    // A retained binding must be exact semver; an unknown major is the
    // unverifiable shape it can actually carry.
    assert_eq!(
        retained_admission_code("28.0.0").as_deref(),
        Some("runtime_version_unverifiable"),
        "an unclassifiable pin must fail closed"
    );
    // A patched pin passes the policy. This host (macOS) cannot contain the
    // candidate, so the next refusal is the containment one — not the policy.
    let patched = retained_admission_code("22.23.4");
    assert!(
        !matches!(
            patched.as_deref(),
            Some("runtime_version_not_patched") | Some("runtime_version_unverifiable")
        ),
        "{patched:?}"
    );
}
