//! Invoke declared operations through the existing HTTP Adapter, after owned
//! launch and before K. Grants and physical Port endpoints remain private.
use crate::{
    control::{AttemptPhase, ExecutionControl},
    variables::ResolvedVariable,
};
use anyhow::Result;
use ato_adapter_api::AdapterError;
use ato_adapter_http::bound_request as http;
use ato_formation::{
    authoring::BoundDerivation,
    failure::{FailureStage, FormationFailure},
    port_operations::{Method, PortOperationObservation, RequestTemplate},
    requirements::ExecutionPhase,
};
use std::collections::BTreeMap;
use std::net::SocketAddr;

fn failure(code: &str, message: &str) -> anyhow::Error {
    FormationFailure::new(code, FailureStage::Admission, message).into()
}

pub(crate) fn execute(
    derivation: &BoundDerivation,
    endpoints: &BTreeMap<String, String>,
    variables: &[ResolvedVariable],
    bound: bool,
    control: Option<&ExecutionControl>,
    evidence: &mut Vec<PortOperationObservation>,
) -> Result<()> {
    ato_formation::port_operations::validate_bound(
        &derivation.runtime_port_operations,
        &derivation.ports,
        &derivation.steps,
        &derivation.variable_bindings,
        &derivation.requirements,
    )
    .map_err(|_| {
        failure(
            "unsupported_capability",
            "HTTP Port operation declaration is not bound",
        )
    })?;
    execute_operations(
        &derivation.runtime_port_operations,
        endpoints,
        variables,
        bound,
        control,
        evidence,
    )
}

pub fn execute_acceptance(
    derivation: &BoundDerivation,
    plan: &ato_formation::functional_acceptance::FunctionalAcceptanceV1,
    ceiling: &ato_formation::requirements::ExecutionRequirements,
    endpoints: &BTreeMap<String, String>,
    variables: &[ResolvedVariable],
    control: &ExecutionControl,
    evidence: &mut Vec<PortOperationObservation>,
) -> Result<()> {
    plan.validate(derivation, ceiling).map_err(|_| {
        failure(
            "functional_acceptance_invalid",
            "functional HTTP actions exceed the approved scope",
        )
    })?;
    execute_operations(
        &plan.operations,
        endpoints,
        variables,
        true,
        Some(control),
        evidence,
    )
}

fn execute_operations(
    operations: &[ato_formation::port_operations::RuntimePortOperation],
    endpoints: &BTreeMap<String, String>,
    variables: &[ResolvedVariable],
    bound: bool,
    control: Option<&ExecutionControl>,
    evidence: &mut Vec<PortOperationObservation>,
) -> Result<()> {
    if operations.is_empty() {
        return Ok(());
    }
    let control = control.filter(|_| bound).ok_or_else(|| {
        failure(
            "unsupported_capability",
            "HTTP Port operations require a bound scoped Runtime and frozen deadline",
        )
    })?;
    control.remaining(AttemptPhase::Launch)?;
    // Resolve exactly the current redeemed grants. No fallback, store search or
    // value serialization is allowed here. Validate all inputs before dispatch.
    for operation in operations {
        for name in operation.request.json_bindings.values() {
            if variables
                .iter()
                .filter(|v| v.phase == ExecutionPhase::Runtime && v.value.name() == name)
                .count()
                != 1
            {
                return Err(failure(
                    "needs_input",
                    "HTTP Port operation needs one current Runtime input grant per declared slot",
                ));
            }
        }
    }
    for (operation_index, operation) in operations.iter().enumerate() {
        control.remaining(AttemptPhase::Launch)?;
        let target: SocketAddr = endpoints
            .get(&operation.port)
            .and_then(|s| s.strip_prefix("http://"))
            .and_then(|s| s.parse().ok())
            .filter(|a: &SocketAddr| a.ip().is_loopback() && a.port() != 0)
            .ok_or_else(|| {
                failure(
                    "unsupported_capability",
                    "HTTP Port has no assigned loopback endpoint",
                )
            })?;
        let mut request = |template: &RequestTemplate, guard: bool| -> Result<u16> {
            let transport = http::RequestTemplate {
                method: match template.method {
                    Method::Get => http::Method::Get,
                    Method::Post => http::Method::Post,
                },
                path: template.path.clone(),
                json_bindings: template.json_bindings.clone(),
            };
            let result = http::invoke(
                target,
                &transport,
                |name| {
                    variables
                        .iter()
                        .find(|v| v.phase == ExecutionPhase::Runtime && v.value.name() == name)
                        .map(|v| v.value.expose_for_spawn())
                        .ok_or_else(|| {
                            AdapterError::Operation("Runtime input grant is unavailable".into())
                        })
                },
                || {
                    control.remaining(AttemptPhase::Launch).map_err(|_| {
                        AdapterError::Operation("frozen execution deadline exceeded".into())
                    })
                },
                |observed| {
                    let observation = match observed {
                        http::Observation::RequestTemplate { .. } => {
                            ato_formation::port_operations::Observation::RequestTemplate {
                                template: template.clone(),
                            }
                        }
                        http::Observation::ResponseStatus { status } => {
                            ato_formation::port_operations::Observation::ResponseStatus { status }
                        }
                    };
                    evidence.push(PortOperationObservation {
                        operation_index,
                        port: operation.port.clone(),
                        guard,
                        observation,
                    });
                    Ok(())
                },
            );
            match result {
                Ok(status) => Ok(status),
                Err(_) => {
                    control.remaining(AttemptPhase::Launch)?;
                    Err(failure(
                        "source_runtime_http_operation_failed",
                        "declared HTTP interaction did not return a complete status; request is not automatically replayed",
                    ))
                }
            }
        };
        if let Some(guard) = &operation.when {
            let status = request(
                &RequestTemplate {
                    method: Method::Get,
                    path: guard.path.clone(),
                    json_bindings: BTreeMap::new(),
                },
                true,
            )?;
            if !guard.statuses.contains(&status) {
                continue;
            }
        }
        let status = request(&operation.request, false)?;
        if !operation.accepted_statuses.contains(&status) {
            return Err(failure(
                "source_runtime_http_status_rejected",
                &format!("declared HTTP operation {operation_index} returned status {status}"),
            ));
        }
    }
    // Response reporting is allowed after expiry, but a new K observation is not.
    control.remaining(AttemptPhase::Launch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::{
        authoring::{BindingContext, bind},
        capsule_toml::parse_capsule_toml,
        port_operations::{RuntimePortOperation, StatusGuard},
        variables::VariableRequirement,
    };
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    fn derivation() -> BoundDerivation {
        let draft = parse_capsule_toml(include_str!(
            "../../formation/tests/fixtures/proposal-python.toml"
        ))
        .unwrap();
        let (_, mut d) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
            },
        )
        .unwrap();
        let input: VariableRequirement = serde_json::from_value(json!({"name":"OWNER_INPUT","kind":"configuration",
            "purpose":"Source-declared local owner input","resource":"app.http","operation":"execute","phase":"runtime",
            "secret":false,"artifact_embedding":false})).unwrap();
        d.variable_bindings.push(input);
        let operation = RuntimePortOperation {
            port: "app.http".into(),
            request: RequestTemplate {
                method: Method::Post,
                path: "/source-declared/owner".into(),
                json_bindings: BTreeMap::from([("password".into(), "OWNER_INPUT".into())]),
            },
            accepted_statuses: vec![201],
            when: None,
        };
        d.requirements.authority.push(operation.authority());
        d.runtime_port_operations.push(operation);
        d
    }
    fn input(d: &BoundDerivation) -> ResolvedVariable {
        ResolvedVariable::new(
            &d.variable_bindings[0],
            "formation-variable:owned-current-grant".into(),
            "runtime-input-canary".into(),
        )
        .unwrap()
    }
    fn receive(socket: &mut std::net::TcpStream) -> Vec<u8> {
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut head = vec![];
        loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let length: usize = String::from_utf8_lossy(&head)
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        body
    }
    #[test]
    fn current_runtime_grant_is_private_and_status_observation_is_separate() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&receive(&mut socket)).unwrap()["password"],
                "runtime-input-canary"
            );
            socket.write_all(b"HTTP/1.1 201 private-reason-canary\r\nSet-Cookie: private-cookie-canary\r\nContent-Length: 19\r\n\r\nprivate-token-value").unwrap();
        });
        let d = derivation();
        let values = [input(&d)];
        let control = ExecutionControl::new(crate::control::now_ms() + 2000);
        let mut observations = vec![];
        execute(
            &d,
            &BTreeMap::from([("app.http".into(), format!("http://{target}"))]),
            &values,
            true,
            Some(&control),
            &mut observations,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(observations.len(), 2);
        let public = serde_json::to_string(&observations).unwrap();
        assert!(!public.contains("canary"));
        assert!(!public.contains("formation-variable"));
        assert!(!public.contains("127.0.0.1"));
        assert!(public.contains("OWNER_INPUT"));
        assert!(public.contains("201"));
    }
    #[test]
    fn grant_absence_ambiguity_unbound_and_expiry_refuse_before_any_request() {
        let d = derivation();
        let endpoint = BTreeMap::from([("app.http".into(), "http://127.0.0.1:1".into())]);
        for (values, bound, expired, code) in [
            (vec![], true, false, "needs_input"),
            (vec![input(&d), input(&d)], true, false, "needs_input"),
            (vec![input(&d)], false, false, "unsupported_capability"),
            (vec![input(&d)], true, true, "round_deadline_exceeded"),
        ] {
            let control =
                ExecutionControl::new(crate::control::now_ms() + if expired { 0 } else { 2000 });
            let mut evidence = vec![];
            let error =
                execute(&d, &endpoint, &values, bound, Some(&control), &mut evidence).unwrap_err();
            assert_eq!(error.downcast_ref::<FormationFailure>().unwrap().code, code);
            assert!(evidence.is_empty());
        }
    }
    #[test]
    fn explicit_guard_skips_mutation_without_inferring_contract_success() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            assert!(receive(&mut socket).is_empty());
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let mut d = derivation();
        d.runtime_port_operations[0].when = Some(StatusGuard {
            path: "/".into(),
            statuses: vec![302],
        });
        let values = [input(&d)];
        let control = ExecutionControl::new(crate::control::now_ms() + 2000);
        let mut evidence = vec![];
        execute(
            &d,
            &BTreeMap::from([("app.http".into(), format!("http://{target}"))]),
            &values,
            true,
            Some(&control),
            &mut evidence,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|e| e.guard));
        assert!(
            !serde_json::to_string(&evidence)
                .unwrap()
                .contains("satisfied")
        );
    }
    #[test]
    fn matching_guard_sends_one_mutation_to_the_same_port_without_following_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut guard, _) = listener.accept().unwrap();
            assert!(receive(&mut guard).is_empty());
            guard.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://external.invalid/credential\r\nContent-Length: 0\r\n\r\n").unwrap();
            let (mut mutation, _) = listener.accept().unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&receive(&mut mutation)).unwrap()["password"],
                "runtime-input-canary"
            );
            mutation
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let mut d = derivation();
        d.runtime_port_operations[0].when = Some(StatusGuard {
            path: "/".into(),
            statuses: vec![302],
        });
        let values = [input(&d)];
        let control = ExecutionControl::new(crate::control::now_ms() + 2000);
        let mut evidence = vec![];
        execute(
            &d,
            &BTreeMap::from([("app.http".into(), format!("http://{target}"))]),
            &values,
            true,
            Some(&control),
            &mut evidence,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(evidence.len(), 4);
        assert!(evidence[..2].iter().all(|e| e.guard));
        assert!(evidence[2..].iter().all(|e| !e.guard));
        assert!(
            !serde_json::to_string(&evidence)
                .unwrap()
                .contains("external.invalid")
        );
    }
    #[test]
    fn lost_mutating_response_never_replays_and_preserves_original_deadline_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            receive(&mut socket);
            std::thread::sleep(Duration::from_millis(200));
            drop(socket);
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let d = derivation();
        let values = [input(&d)];
        let control = ExecutionControl::new(crate::control::now_ms() + 60);
        let mut evidence = vec![];
        let error = execute(
            &d,
            &BTreeMap::from([("app.http".into(), format!("http://{target}"))]),
            &values,
            true,
            Some(&control),
            &mut evidence,
        )
        .unwrap_err();
        server.join().unwrap();
        assert_eq!(
            error.downcast_ref::<FormationFailure>().unwrap().code,
            "round_deadline_exceeded"
        );
        assert_eq!(evidence.len(), 1);
    }
}
