//! Observe a ready, source-formed process against its frozen Formation K.
//! A failed or absent report never changes readiness or authorizes a Run.
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_formation::verify::{
    RuntimeHttpObservation, VerificationExecutionEvidence, VerificationTargetKind,
};
use ato_ipc::runtime_launch::LaunchRealizationV1;
use ato_ipc::runtime_launch_v2::RuntimeLaunchSpec;
use ato_runtime_attempt::spec::{AttemptSpec, CandidateShape};
use ato_runtime_attempt::verification::{
    FrozenVerificationInput, ReceiptContext, required_http_observations, verify_observed_candidate,
};
use serde::Deserialize;

use crate::{
    ClaimedLease, HttpRunnerApi,
    runtime_launch::lease::{ActiveRun, ActiveWorkload},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    assignment: Assignment,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    run_id: String,
    lease_id: String,
    compute_instance_id: String,
    compute_schema_id: String,
    materialization_ref: String,
    source_closure_ref: String,
    contract_ref: String,
    derivation_ref: String,
    frozen_input: FrozenVerificationInput,
}

fn check_assignment<'a>(
    assignment: &'a Assignment,
    run_id: &str,
    lease_id: &str,
    spec: &RuntimeLaunchSpec,
    endpoint_name: &str,
) -> Result<AttemptSpec<'a>> {
    let RuntimeLaunchSpec::V1(spec) = spec else {
        bail!("unsupported launch profile")
    };
    let LaunchRealizationV1::Process(process) = &spec.realization else {
        bail!("unsupported realization")
    };
    ensure!(spec.state_attachments.is_empty(), "unsupported realization");
    ensure!(
        assignment.run_id == run_id
            && assignment.lease_id == lease_id
            && spec.context.run_id == run_id
            && assignment.compute_instance_id == spec.context.compute_instance_id
            && assignment.compute_schema_id == spec.context.compute_schema_id
            && assignment.materialization_ref == spec.workspace.materialization_ref,
        "assignment mismatch"
    );
    assignment
        .frozen_input
        .validate_assignment(&assignment.contract_ref, &assignment.derivation_ref)?;
    let derivation = &assignment.frozen_input.derivation;
    ensure!(
        derivation.state.is_empty()
            && derivation.inputs.len() == 1
            && derivation.ports.len() == 1
            && spec.endpoints.len() == 1
            && spec.endpoints[0].name == endpoint_name
            && (endpoint_name == derivation.ports[0].id || endpoint_name == "http")
            && spec.endpoints[0].protocol == "http"
            && derivation.ports[0].protocol == ato_formation::authoring::HTTP_PROTOCOL
            && spec.endpoints[0].guest_port == derivation.ports[0].guest_port,
        "unsupported route"
    );
    let serving: Vec<_> = derivation
        .steps
        .iter()
        .filter(|step| step.op == "serve")
        .collect();
    let [serve] = serving.as_slice() else {
        bail!("unsupported serving route")
    };
    ensure!(
        serve.protocol == ato_formation::authoring::PROCESS_PROTOCOL
            && derivation
                .steps
                .last()
                .is_some_and(|last| last.id == serve.id)
            && derivation.ports[0].from == serve.id
            && serve.argv == process.argv
            && serve.cwd == spec.workspace.cwd_relative
            && serve.env.iter().all(|(name, value)| spec
                .public_env
                .iter()
                .any(|actual| actual.name == *name && actual.value == *value)),
        "serving route mismatch"
    );
    let input = &derivation.inputs[0];
    ensure!(
        input.protocol == "ato.workspace@1" && input.content_ref == assignment.source_closure_ref,
        "source lineage mismatch"
    );
    ensure!(
        assignment.frozen_input.contract.requirements.len() <= 8,
        "too many observations"
    );
    let input_refs = BTreeMap::from([(input.id.clone(), input.content_ref.clone())]);
    Ok(AttemptSpec {
        contract: &assignment.frozen_input.contract,
        contract_ref: &assignment.contract_ref,
        derivation,
        derivation_ref: &assignment.derivation_ref,
        shape: CandidateShape::Process,
        input_refs,
        instance_snapshot_ref: None,
        scenarios: Vec::new(),
    })
}

fn probe_url(port: u16, path: &str) -> Option<url::Url> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('#') || path.contains('\\')
    {
        return None;
    }
    let parsed = url::Url::parse(&format!("http://127.0.0.1:{port}{path}")).ok()?;
    let observed_path = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_owned(),
    };
    (observed_path == path
        && parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.port_or_known_default() == Some(port)
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.fragment().is_none())
    .then_some(parsed)
}

fn observe_http(spec: &AttemptSpec<'_>, port: u16) -> Result<Vec<RuntimeHttpObservation>> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()?;
    let mut observations = Vec::new();
    for required in required_http_observations(spec) {
        // Parsing must not reinterpret a backslash or authority in an
        // untrusted path as another host, even without redirects or a proxy.
        let Some(url) = probe_url(port, &required.path) else {
            continue;
        };
        let Ok(response) = client.get(url).send() else {
            continue;
        };
        let status = response.status().as_u16();
        let Ok(body) = response
            .take(1024 * 1024 + 1)
            .bytes()
            .collect::<std::io::Result<Vec<_>>>()
        else {
            continue;
        };
        if body.len() > 1024 * 1024 {
            continue;
        }
        observations.push(RuntimeHttpObservation::from_response(
            required.port_id,
            "GET",
            required.path,
            status,
            &body,
        ));
    }
    Ok(observations)
}

pub(super) fn report(
    api: &HttpRunnerApi,
    lease: &ClaimedLease,
    spec: &RuntimeLaunchSpec,
    active: &ActiveRun,
    endpoint_name: &str,
) -> Result<()> {
    if !matches!(active.launched, ActiveWorkload::Process(_)) {
        return Ok(());
    }
    let url = format!(
        "{}/v1/runner-leases/{}/contract-verification",
        api.base, lease.id
    );
    let response = api
        .authorized(api.client.get(&url))
        .timeout(Duration::from_secs(5))
        .send()?;
    if matches!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::CONFLICT
    ) {
        return Ok(());
    }
    let response = response.error_for_status()?;
    let bytes = response
        .take(256 * 1024 + 1)
        .bytes()
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(bytes.len() <= 256 * 1024, "assignment too large");
    let assignment: Response = serde_json::from_slice(&bytes)?;
    let spec = check_assignment(
        &assignment.assignment,
        &lease.run_id,
        &lease.id,
        spec,
        endpoint_name,
    )?;
    let port = active
        .endpoint_port(endpoint_name)
        .context("missing process endpoint")?;
    let observations = observe_http(&spec, port)?;
    let running = active.execution_evidence();
    let execution = VerificationExecutionEvidence {
        realization: "process".into(),
        runtime_executable: running.runtime_executable,
        runtime_version: running.runtime_version,
        pid: running.pid,
        container_id: None,
        image: None,
        platform: None,
        endpoint: None,
        run_id: None,
        lease_id: Some(lease.id.clone()),
        attempt_id: Some(lease.id.clone()),
        request_id: None,
        dependency_fetches: Vec::new(),
        portability_profile: None,
        embedded_oci_image_loaded: None,
        services: Vec::new(),
    };
    let receipt = verify_observed_candidate(
        &spec,
        observations,
        Vec::new(),
        execution,
        &ReceiptContext {
            target: VerificationTargetKind::AtoRunHosted,
            bundle_sha256: None,
            portability_profile: None,
            run_id: Some(&lease.run_id),
        },
    )
    .receipt;
    // Retry exactly the same receipt after an indeterminate transport failure.
    // The server's compare-and-set treats an identical replay as successful.
    let payload = serde_json::json!({"receipt": receipt});
    for attempt in 0..2 {
        match api
            .authorized(api.client.post(&url))
            .timeout(Duration::from_secs(3))
            .json(&payload)
            .send()
        {
            Ok(response) => {
                response.error_for_status()?;
                return Ok(());
            }
            Err(error)
                if attempt == 0
                    && (error.is_timeout() || error.is_connect() || error.is_request()) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    bail!("receipt report unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::authoring::{BoundContract, BoundDerivation};
    use serde_json::json;

    fn example() -> (Assignment, RuntimeLaunchSpec) {
        let source = format!("sha256:{}", "a".repeat(64));
        let artifact = format!("sha256:{}", "b".repeat(64));
        let contract: BoundContract = serde_json::from_value(json!({"schema":"ato.contract/1", "requirements":[
            {"id":"root", "verifier":"ato.contract.http@1", "port":"app.http", "method":"GET", "path":"/", "status":200}
        ]})).unwrap();
        let derivation: BoundDerivation = serde_json::from_value(json!({"schema":"ato.derivation/1", "inputs":[
            {"id":"workspace", "protocol":"ato.workspace@1", "content_ref":source}],
            "steps":[{"id":"serve", "protocol":"ato.process@1", "op":"serve", "argv":["python3"]}],
            "ports":[{"id":"app.http", "protocol":"ato.http@1", "from":"serve", "guest_port":8000}], "state":[], "effects":"pure"})).unwrap();
        let k = contract.contract_ref().unwrap();
        let d = derivation.derivation_ref().unwrap();
        let assignment = Assignment {
            run_id: "run1".into(),
            lease_id: "lease1".into(),
            compute_instance_id: "instance1".into(),
            compute_schema_id: "schema1".into(),
            materialization_ref: artifact.clone(),
            source_closure_ref: source,
            contract_ref: k.clone(),
            derivation_ref: d.clone(),
            frozen_input: FrozenVerificationInput {
                schema: "ato.frozen-verification-input/1".into(),
                contract_ref: k,
                derivation_ref: d,
                contract,
                derivation,
            },
        };
        let spec = RuntimeLaunchSpec::parse(&json!({"protocol":"ato.runtime-launch-spec.v1",
            "context":{"run_id":"run1","compute_id":"cmp1","compute_schema_id":"schema1","compute_instance_id":"instance1"},
            "workspace":{"materialization_ref":artifact}, "realization":{"kind":"process","argv":["python3"]},
            "endpoints":[{"name":"http","protocol":"http","guest_port":8000,"allocation":"automatic"}],
            "readiness":{"kind":"http","endpoint_name":"http","path":"/","timeout_ms":10000},
            "lifecycle":{"graceful_shutdown_ms":1000,"force_kill_after_ms":2000}}).to_string()).unwrap();
        (assignment, spec)
    }

    #[test]
    fn real_http_response_drives_the_frozen_contract_verdict() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let (assignment, launch) = example();
        let spec = check_assignment(&assignment, "run1", "lease1", &launch, "http").unwrap();
        for (response_status, expected) in [(200, true), (503, false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap();
                assert!(
                    std::str::from_utf8(&request[..n])
                        .unwrap()
                        .starts_with("GET / HTTP/1.1")
                );
                write!(stream, "HTTP/1.1 {response_status} response\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            });
            let observed = observe_http(&spec, port).unwrap();
            server.join().unwrap();
            let execution = VerificationExecutionEvidence {
                realization: "process".into(),
                runtime_executable: None,
                runtime_version: None,
                pid: None,
                container_id: None,
                image: None,
                platform: None,
                endpoint: None,
                run_id: None,
                lease_id: Some("lease1".into()),
                attempt_id: Some("lease1".into()),
                request_id: None,
                dependency_fetches: Vec::new(),
                portability_profile: None,
                embedded_oci_image_loaded: None,
                services: Vec::new(),
            };
            let point = verify_observed_candidate(
                &spec,
                observed,
                Vec::new(),
                execution,
                &ReceiptContext {
                    target: VerificationTargetKind::AtoRunHosted,
                    bundle_sha256: None,
                    portability_profile: None,
                    run_id: Some("run1"),
                },
            );
            assert_eq!(point.receipt.fully_satisfied, expected);
            assert_eq!(point.receipt.contract_ref, assignment.contract_ref);
            assert_eq!(
                point.receipt.execution.unwrap().run_id.as_deref(),
                Some("run1")
            );
        }
    }

    // The exact authored K/D shape used by the previously approved public
    // staging server fixture (ato-run/ato-e2e-compute-server@2801f21).
    #[test]
    fn authored_server_fixture_maps_logical_http_and_source_identity() {
        use ato_formation::authoring::{BindingContext, bind};
        let draft = ato_formation::capsule_toml::parse_capsule_toml(
            r#"
            schema = "ato.capsule/1"
            [[input]]
            id = "workspace"
            use = "ato.workspace@1"
            path = "."
            [[runtime]]
            name = "python"
            version = "3.12.7"
            [[derive.step]]
            id = "app"
            use = "ato.process@1"
            op = "serve"
            argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "server.py"]
            [[port]]
            id = "app.http"
            use = "ato.http@1"
            from = "app"
            guest_port = 8080
            [[contract.require]]
            id = "root"
            use = "ato.contract.http@1"
            port = "app.http"
            method = "GET"
            path = "/"
            [contract.require.expect]
            status = 200
            [[contract.require]]
            id = "source"
            use = "ato.contract.workspace@1"
            input = "workspace"
            [contract.require.expect]
            digest = "capture"
        "#,
        )
        .unwrap();
        let (mut assignment, mut launch) = example();
        let (contract, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &assignment.source_closure_ref,
            },
        )
        .unwrap();
        assignment.contract_ref = contract.contract_ref().unwrap();
        assignment.derivation_ref = derivation.derivation_ref().unwrap();
        assignment.frozen_input = FrozenVerificationInput {
            schema: "ato.frozen-verification-input/1".into(),
            contract_ref: assignment.contract_ref.clone(),
            derivation_ref: assignment.derivation_ref.clone(),
            contract,
            derivation,
        };
        if let RuntimeLaunchSpec::V1(ref mut spec) = launch {
            let LaunchRealizationV1::Process(ref mut process) = spec.realization else {
                unreachable!()
            };
            process.argv = vec![
                "/opt/ato/toolchains/python/3.12.7/bin/python3".into(),
                "-B".into(),
                "server.py".into(),
            ];
            spec.endpoints[0].guest_port = Some(8080);
        }
        let spec = check_assignment(&assignment, "run1", "lease1", &launch, "http").unwrap();
        let observation = RuntimeHttpObservation::from_response("app.http", "GET", "/", 200, b"ok");
        let execution = VerificationExecutionEvidence {
            realization: "process".into(),
            runtime_executable: None,
            runtime_version: None,
            pid: None,
            container_id: None,
            image: None,
            platform: None,
            endpoint: None,
            run_id: None,
            lease_id: Some("lease1".into()),
            attempt_id: Some("lease1".into()),
            request_id: None,
            dependency_fetches: Vec::new(),
            portability_profile: None,
            embedded_oci_image_loaded: None,
            services: Vec::new(),
        };
        let result = verify_observed_candidate(
            &spec,
            vec![observation],
            Vec::new(),
            execution,
            &ReceiptContext {
                target: VerificationTargetKind::AtoRunHosted,
                bundle_sha256: None,
                portability_profile: None,
                run_id: Some("run1"),
            },
        );
        assert!(result.receipt.fully_satisfied);
        assert_eq!(result.receipt.observations.len(), 2);
    }

    #[test]
    fn observation_target_cannot_escape_the_allocated_loopback_port() {
        assert!(probe_url(8080, "/health?ready=1").is_some());
        assert!(probe_url(8080, "//attacker.invalid/path").is_none());
        assert!(probe_url(8080, "/\\attacker.invalid/path").is_none());
        assert!(probe_url(8080, "http://attacker.invalid/").is_none());
        assert!(probe_url(8080, "/x#fragment").is_none());
    }

    #[test]
    fn accepts_the_exact_logical_port_and_only_the_historical_http_alias() {
        let (assignment, mut spec) = example();
        if let RuntimeLaunchSpec::V1(ref mut launch) = spec {
            launch.endpoints[0].name = "app.http".into();
        }
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "app.http").is_ok());
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
        if let RuntimeLaunchSpec::V1(ref mut launch) = spec {
            launch.endpoints[0].name = "other".into();
        }
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "other").is_err());
    }

    #[test]
    fn binds_run_lease_materialization_and_frozen_contract() {
        let (mut assignment, spec) = example();
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_ok());
        assert!(check_assignment(&assignment, "other", "lease1", &spec, "http").is_err());
        assert!(check_assignment(&assignment, "run1", "other", &spec, "http").is_err());
        assignment.source_closure_ref = format!("sha256:{}", "c".repeat(64));
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
        let (mut assignment, spec) = example();
        assignment.frozen_input.contract.requirements[0].status = Some(404);
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
        let (assignment, mut spec) = example();
        if let RuntimeLaunchSpec::V1(ref mut launch) = spec {
            if let LaunchRealizationV1::Process(ref mut process) = launch.realization {
                process.argv[0] = "different".into();
            }
        }
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
        let (mut assignment, spec) = example();
        assignment.materialization_ref = format!("sha256:{}", "d".repeat(64));
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
    }
}
