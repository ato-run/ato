//! Routing policy tests only: no real claim, credentials or workload launch.
use super::*;
use runtime_launch::lease::{RuntimeLaunchLeaseCommand, verified_spec};
use serde_json::{Value, json};

fn args() -> Vec<&'static str> {
    vec![
        "worker",
        "--api-base",
        "http://127.0.0.1:9",
        "--runner-id",
        "runner-owned",
        "--runner-token",
        "fixture-token",
        "--work-root",
        ".tmp/explicit-placement-unused",
        "--surface-target",
        "127.0.0.1:8080",
    ]
}
fn config() -> WorkerConfig {
    WorkerConfig::try_parse_from(args()).unwrap()
}
fn runtime_wire(source: &str) -> Value {
    let spec = ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::parse(source).unwrap();
    json!({"kind":"runtime_launch", "run_id":spec.context().run_id,
        "compute_instance_id":spec.context().compute_instance_id,
        "launch_spec_digest":spec.canonical_digest().unwrap(), "launch_spec":serde_json::from_str::<Value>(source).unwrap()})
}
const PROCESS: &str =
    include_str!("../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-process.json");
const GROUP: &str =
    include_str!("../../../lib/ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json");
const VOLUME: &str = include_str!(
    "../../../lib/ipc/tests/fixtures/runtime-launch-spec-v3/service-group-volume.json"
);
fn lease(command: Value) -> ClaimedLease {
    let run = command
        .get("run_id")
        .or_else(|| command.get("activity_run_id"))
        .unwrap()
        .clone();
    serde_json::from_value(
        json!({"id":"lease-fixture", "run_id":run, "command":command, "expires_at":null}),
    )
    .unwrap()
}
#[test]
fn explicit_runtime_placement_operator_flag_is_opt_in() {
    assert!(!config().require_explicit_runtime_placement);
    let mut values = args();
    values.push("--require-explicit-runtime-placement");
    assert!(
        WorkerConfig::try_parse_from(values)
            .unwrap()
            .require_explicit_runtime_placement
    );
}
#[test]
fn explicit_runtime_placement_environment_is_operator_configuration() {
    const CHILD: &str = "ATO_EXPLICIT_PLACEMENT_ENV_CHILD";
    if std::env::var_os(CHILD).is_some() {
        assert!(config().require_explicit_runtime_placement);
        return;
    }
    let status=std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","explicit_placement_tests::explicit_runtime_placement_environment_is_operator_configuration"])
        .env(CHILD,"1").env("ATO_RUNNER_REQUIRE_EXPLICIT_RUNTIME_PLACEMENT","true")
        .status().unwrap();
    assert!(status.success());
}
#[test]
fn advertisement_and_supported_kinds_have_the_same_opt_in_boundary() {
    assert_eq!(
        EXPLICIT_RUNNER_ASSIGNMENT_RUNTIME_FEATURE,
        "runtime_feature=explicit_runner_assignment_v1"
    );
    let base = vec!["execution_abi=oci"];
    assert_eq!(
        with_explicit_runtime_placement_capability(base.clone(), false),
        base
    );
    assert!(
        with_explicit_runtime_placement_capability(base, true)
            .contains(&EXPLICIT_RUNNER_ASSIGNMENT_RUNTIME_FEATURE)
    );
    let all = vec![
        PORTABLE_CAPSULE_LEASE_KIND,
        "runtime_launch",
        ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND,
        "state_volume_maintenance",
    ];
    assert_eq!(explicit_runtime_placement_kinds(all.clone(), false), all);
    assert_eq!(
        explicit_runtime_placement_kinds(all, true),
        vec!["runtime_launch"]
    );
    assert!(explicit_runtime_placement_kinds(vec![PORTABLE_CAPSULE_LEASE_KIND], true).is_empty());
    let mut cfg = config();
    cfg.require_explicit_runtime_placement = true;
    assert!(
        supported_lease_kinds(&cfg, true)
            .iter()
            .all(|kind| *kind == "runtime_launch")
    );
}
#[test]
fn old_envelopes_and_digests_stay_unchanged_but_cannot_bypass_explicit_policy() {
    for source in [PROCESS, GROUP, VOLUME] {
        let wire = runtime_wire(source);
        assert!(wire.get("explicit_runner_id").is_none());
        let before = wire["launch_spec_digest"].clone();
        let command: RuntimeLaunchLeaseCommand = serde_json::from_value(wire.clone()).unwrap();
        assert!(command.explicit_runner_id.is_none());
        assert!(
            command
                .validate_runner_assignment("runner-owned", false)
                .is_ok()
        );
        assert!(
            command
                .validate_runner_assignment("runner-owned", true)
                .is_err()
        );
        let original = verified_spec(&command).unwrap().canonical_digest().unwrap();
        assert_eq!(before, original);
        for required in [false, true] {
            for value in [
                json!(null),
                json!(""),
                json!(" "),
                json!("runner-other"),
                json!("runner-owned "),
                json!("runner-owned"),
            ] {
                let mut current = wire.clone();
                current["explicit_runner_id"] = value.clone();
                let command: RuntimeLaunchLeaseCommand = serde_json::from_value(current).unwrap();
                let expected = value == "runner-owned" || value.is_null() && !required;
                assert_eq!(
                    command
                        .validate_runner_assignment("runner-owned", required)
                        .is_ok(),
                    expected,
                    "{required}/{value}"
                );
                assert_eq!(
                    verified_spec(&command).unwrap().canonical_digest().unwrap(),
                    original
                );
            }
        }
        for bad in [json!(1), json!(false), json!([]), json!({})] {
            let mut current = wire.clone();
            current["explicit_runner_id"] = bad;
            assert!(serde_json::from_value::<RuntimeLaunchLeaseCommand>(current).is_err());
        }
    }
}
fn non_runtime_commands() -> Vec<Value> {
    vec![
        json!({"kind":"activity_browser_executor_v0","activity_id":"activity-fixture","activity_run_id":"run-fixture"}),
        json!({"kind":"portable_capsule_v2","bundle_id":"bnd_fixture","transport_digest":format!("sha256:{}","11".repeat(32)),"expected_root_computation_ref":format!("blake3:{}","22".repeat(32)),"run_id":"run-fixture","session_id":"run:run-fixture","exported_port_id":"web","surface_contract_version":"1","session_surface":{},"accepted_session_surfaces":[{}]}),
        json!({"kind":"state_volume_maintenance","run_id":"run-fixture","compute_instance_id":"instance-fixture","operation_id":"vop_fixture","operation":"delete","state_key":"data","volume_ref":"svol_01M2SVCGR0VP000000000000V1","writer_fence":1}),
    ]
}
#[test]
fn all_other_lease_kinds_are_refused_only_when_operator_restricts_placement() {
    for command in non_runtime_commands() {
        let lease = lease(command);
        validate_lease(&lease, SystemTime::now()).unwrap();
        let mut cfg = config();
        assert!(validate_explicit_runner_placement(&cfg, &lease.command).is_ok());
        cfg.require_explicit_runtime_placement = true;
        assert!(
            validate_explicit_runner_placement(&cfg, &lease.command)
                .unwrap_err()
                .to_string()
                .contains("unsupported_lease")
        );
    }
}
#[test]
fn both_entries_refuse_before_storage_preparing_directory_or_network_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    api.set_nonblocking(true).unwrap();
    let mut cfg = config();
    cfg.api_base = format!("http://{}", api.local_addr().unwrap());
    // Even the root is absent: storage probing would fail before an API call.
    cfg.work_root = temp.path().join("must-not-create");
    cfg.require_explicit_runtime_placement = true;
    let mut worker = ConnectedWorker {
        api: HttpRunnerApi::new(&cfg.api_base, &cfg.runner_id, "fixture-token").unwrap(),
        config: cfg,
        volume_store: None,
        fixed_tcp: runtime_launch::network_broker::FixedTcpRegistry::new("").unwrap(),
    };
    let mut commands = non_runtime_commands();
    commands.push(runtime_wire(PROCESS));
    let mut mismatch = runtime_wire(PROCESS);
    mismatch["explicit_runner_id"] = json!("runner-other");
    commands.push(mismatch);
    for value in commands {
        let lease = lease(value);
        validate_lease(&lease, SystemTime::now()).unwrap();
        let error = worker.execute_lease(&lease).unwrap_err();
        assert!(
            error.to_string().contains("explicit_runner_assignment"),
            "{error:#}"
        );
        if let LeaseCommand::RuntimeLaunch(command) = &lease.command {
            let error = worker
                .execute_runtime_launch_lease(
                    &lease,
                    command,
                    &worker.config.work_root.join("direct"),
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("explicit_runner_assignment"),
                "{error:#}"
            );
        }
        assert!(!worker.config.work_root.exists());
        assert!(matches!(api.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    }
    // Presence is enforced even when the local operator restriction is off.
    worker.config.require_explicit_runtime_placement = false;
    let mut wrong = runtime_wire(PROCESS);
    wrong["explicit_runner_id"] = json!("runner-other");
    let wrong = lease(wrong);
    assert!(
        worker
            .execute_lease(&wrong)
            .unwrap_err()
            .to_string()
            .contains("explicit_runner_assignment_mismatch")
    );
    let LeaseCommand::RuntimeLaunch(command) = &wrong.command else {
        unreachable!()
    };
    assert!(
        worker
            .execute_runtime_launch_lease(&wrong, command, &worker.config.work_root.join("direct"))
            .unwrap_err()
            .to_string()
            .contains("explicit_runner_assignment_mismatch")
    );
    assert!(!worker.config.work_root.exists());
    assert!(matches!(api.accept(), Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    // A matching ID passes only routing; malformed spec still fails admission.
    let mut value = runtime_wire(PROCESS);
    value["explicit_runner_id"] = json!("runner-owned");
    let lease = lease(value);
    assert!(validate_explicit_runner_placement(&worker.config, &lease.command).is_ok());
}
/// Explicit cross-repository wire acceptance only. Never starts a workload.
#[test]
#[ignore = "requires explicit API-produced existing claim-envelope fixture"]
fn api_produced_explicit_assignment_envelope_validates_without_execution() {
    let path =
        std::env::var_os("ATO_EXPLICIT_PLACEMENT_TEST_ENVELOPE").expect("explicit fixture path");
    let bytes = fs::read(path).unwrap();
    assert!(bytes.len() <= 1024 * 1024);
    let lease: ClaimedLease = serde_json::from_slice(&bytes).unwrap();
    validate_lease(&lease, SystemTime::now()).unwrap();
    let mut cfg = config();
    cfg.runner_id = std::env::var("ATO_EXPLICIT_PLACEMENT_TEST_RUNNER_ID").unwrap();
    cfg.require_explicit_runtime_placement = true;
    validate_explicit_runner_placement(&cfg, &lease.command).unwrap();
    let LeaseCommand::RuntimeLaunch(command) = lease.command else {
        panic!("runtime_launch only")
    };
    verified_spec(&command).unwrap();
}
