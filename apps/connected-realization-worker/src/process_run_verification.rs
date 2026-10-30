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
    ensure!(
        matches!(spec.realization, LaunchRealizationV1::Process(_))
            && spec.state_attachments.is_empty(),
        "unsupported realization"
    );
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
            && endpoint_name == "http"
            && spec.endpoints.len() == 1
            && spec.endpoints[0].name == "http"
            && spec.endpoints[0].protocol == "http"
            && derivation.ports[0].protocol == "http"
            && spec.endpoints[0].guest_port == derivation.ports[0].guest_port,
        "unsupported route"
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
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()?;
    let mut observations = Vec::new();
    for required in required_http_observations(&spec) {
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
            "steps":[], "ports":[{"id":"app.http", "protocol":"http", "from":"serve", "guest_port":8000}], "state":[], "effects":"pure"})).unwrap();
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
    fn observation_target_cannot_escape_the_allocated_loopback_port() {
        assert!(probe_url(8080, "/health?ready=1").is_some());
        assert!(probe_url(8080, "//attacker.invalid/path").is_none());
        assert!(probe_url(8080, "/\\attacker.invalid/path").is_none());
        assert!(probe_url(8080, "http://attacker.invalid/").is_none());
        assert!(probe_url(8080, "/x#fragment").is_none());
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
        let (mut assignment, spec) = example();
        assignment.materialization_ref = format!("sha256:{}", "d".repeat(64));
        assert!(check_assignment(&assignment, "run1", "lease1", &spec, "http").is_err());
    }
}
