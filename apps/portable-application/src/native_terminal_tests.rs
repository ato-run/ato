//! A v2 native Terminal route end to end at the Adapter boundary.
//!
//! The authored surface compiles into a bundle whose Application Port is the
//! host-owned PTY and whose K is one `ato.contract.terminal@1` observation per
//! declared operation. The exchange framing, its bounds and its digest are
//! decided by tests over a scripted terminal, and two tests drive the same
//! code against a real PTY where the host permits one.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::Write as _;
#[cfg(unix)]
use std::path::Path;
use std::time::Instant;

use ato_formation::authoring::{
    BOUND_CONTRACT_SCHEMA, BoundContract, BoundRequirement, TERMINAL_CONTRACT_VERIFIER,
};
use ato_formation::native_surface::{
    NativePresentation, NativeProbeOperation, NativeSurfaceAdapter,
};
use ato_formation::verify::{
    RuntimeObservation, RuntimeTerminalObservation, VerificationExecutionEvidence, verify_runtime,
};
use sha2::{Digest, Sha256};

use super::*;
use crate::native_desktop::terminal::{ExchangeIo, exchanges};

const ENTRY: &str = "run.sh";
const READY: &[u8] = b"ready> ";
const INPUT: &[u8] = b"ping\n";
const DELIMITER: &[u8] = b"END";

/// A tarball carrying one executable entry, as a bundle-carried artifact.
fn artifact(body: &str) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, ENTRY, body.as_bytes())
            .expect("tar entry");
        builder.finish().expect("tar finish");
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("gzip finish")
}

fn integers(bytes: &[u8]) -> String {
    let values = bytes
        .iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]")
}

fn manifest(ready: &[u8], delimiter: &[u8], digest: Option<&str>) -> String {
    let digest = digest
        .map(|digest| format!("body_digest = \"{digest}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
schema = "ato.capsule/2"

[application]
title = "terminal"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derivation]]
id = "native-terminal"
use = "ato.native@1"
runtimes = {{}}
os = "macos"
arch = "aarch64"
entry = "run.sh"
artifact = "artifact.tar.gz"

[derivation.native_surface]
kind = "terminal"
port = "app.terminal"
cols = 100
rows = 30

[[derivation.native_surface.operations]]
kind = "terminal_exchange"
path = "/result"
input = {input}
output_delimiter = {delimiter}
max_output_bytes = 4096
timeout_ms = 5000
ready_output = {ready}

[[contract.observation]]
id = "exchange"
use = "ato.contract.terminal@1"
port = "app.terminal"
path = "/result"
{digest}
[effects]
default = "pure"
"#,
        input = integers(INPUT),
        delimiter = integers(delimiter),
        ready = integers(ready),
    )
}

fn source(body: &str, digest: Option<&str>) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("artifact.tar.gz"), artifact(body)).expect("artifact");
    // The portable tree must carry the workspace content; the artifact is a
    // payload blob outside it.
    std::fs::write(directory.path().join("README.md"), "terminal sample\n").expect("readme");
    std::fs::write(
        directory.path().join("capsule.toml"),
        manifest(READY, DELIMITER, digest),
    )
    .expect("manifest");
    directory
}

fn adapter(ready: &[u8], delimiter: &[u8], bound: u32, timeout_ms: u64) -> NativeSurfaceAdapter {
    NativeSurfaceAdapter {
        surface: NativePresentation::Terminal {
            port: "app.terminal".to_owned(),
            cols: 100,
            rows: 30,
        },
        probe_port: None,
        operations: vec![NativeProbeOperation::TerminalExchange {
            path: "/result".to_owned(),
            input: INPUT.to_vec(),
            output_delimiter: delimiter.to_vec(),
            max_output_bytes: bound,
            timeout_ms,
            ready_output: ready.to_vec(),
        }],
    }
}

/// A terminal that answers from scripted output chunks and records the input
/// it was given.
struct ScriptedIo {
    chunks: VecDeque<Vec<u8>>,
    written: Vec<u8>,
}

impl ScriptedIo {
    fn new(chunks: &[&[u8]]) -> Self {
        Self {
            chunks: chunks.iter().map(|chunk| chunk.to_vec()).collect(),
            written: Vec::new(),
        }
    }
}

impl ExchangeIo for ScriptedIo {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.written.extend_from_slice(bytes);
        Ok(())
    }

    fn read_until(
        &mut self,
        captured: &mut Vec<u8>,
        bound: usize,
        _deadline: Instant,
        done: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        loop {
            if done(captured) {
                return Ok(true);
            }
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(false);
            };
            if captured.len() + chunk.len() > bound {
                return Err(format!(
                    "the workload produced more than the declared {bound} bytes"
                ));
            }
            captured.extend_from_slice(&chunk);
        }
    }
}

fn contract(digest: Option<&str>) -> BoundContract {
    BoundContract {
        schema: BOUND_CONTRACT_SCHEMA.to_owned(),
        requirements: vec![BoundRequirement {
            id: "exchange".to_owned(),
            verifier: TERMINAL_CONTRACT_VERIFIER.to_owned(),
            port: Some("app.terminal".to_owned()),
            method: None,
            path: Some("/result".to_owned()),
            status: None,
            body_digest: digest.map(str::to_owned),
            input: None,
            digest: None,
            server_name: None,
            start: None,
        }],
    }
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn verify(
    observations: &[RuntimeTerminalObservation],
    digest: Option<&str>,
) -> (bool, Option<String>) {
    let verification = verify_runtime(
        &contract(digest),
        &RuntimeObservation {
            terminal: observations.to_vec(),
            ..Default::default()
        },
    );
    let code = verification.failure().map(|failure| failure.1.to_owned());
    (verification.fully_satisfied(), code)
}

/// The scripted exchange every framing test starts from.
fn answering(extra: &[&[u8]]) -> ScriptedIo {
    let mut chunks = vec![
        b"ready> ".as_slice(),
        b"got:ping".as_slice(),
        b"END".as_slice(),
    ];
    chunks.extend_from_slice(extra);
    ScriptedIo::new(&chunks)
}

#[test]
fn a_native_terminal_route_builds_a_bundle_over_the_terminal_port() {
    let directory = source(
        "stty -echo; printf 'ready> '; read line; printf 'got:%s' \"$line\"; printf 'END'",
        None,
    );
    let (bytes, bundle) = build_authored_bundle_v2(directory.path()).expect("builds");
    assert!(!bytes.is_empty());
    let routes = validate_all_derivations(&bundle).expect("validates");
    assert_eq!(routes.len(), 1);
    let route = &routes[0];
    assert_eq!(route.realization, PortableRealizationKind::NativeDesktop);
    // The Application names the Terminal Port, and the route promises no guest
    // TCP Port at all.
    assert_eq!(route.application.surfaces[0].port, "app.terminal");
    let native = route.native.as_ref().expect("native descriptor");
    assert!(native.ports.is_empty());
    let terminal = native.terminal.as_ref().expect("terminal surface");
    assert_eq!(terminal.port, "app.terminal");
    assert_eq!(terminal.surface.terminal_geometry(), Some((100, 30)));
    assert!(terminal.surface.probe_port.is_none());
    let requirements = route
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == TERMINAL_CONTRACT_VERIFIER)
        .collect::<Vec<_>>();
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[0].port.as_deref(), Some("app.terminal"));
    assert_eq!(requirements[0].path.as_deref(), Some("/result"));
    assert!(requirements[0].body_digest.is_none());
}

#[test]
fn a_declared_exchange_completes_and_satisfies_its_observation() {
    let adapter = adapter(READY, DELIMITER, 4096, 5000);
    let mut io = answering(&[]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    assert_eq!(io.written, INPUT);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.failure, None);
    assert!(observation.delimiter_found);
    let expected = b"ready> got:pingEND";
    assert_eq!(observation.captured_bytes, expected.len() as u64);
    assert_eq!(observation.captured_sha256, digest_of(expected));
    let (satisfied, code) = verify(&observations, Some(&observation.captured_sha256));
    assert!(satisfied, "{code:?}");
    let (satisfied, _) = verify(&observations, None);
    assert!(satisfied);
}

#[test]
fn a_capture_ends_at_the_delimiter() {
    let adapter = adapter(READY, DELIMITER, 4096, 5000);
    let mut io = answering(&[b"trailing output"]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let observation = &observations[0];
    assert!(observation.delimiter_found);
    assert_eq!(
        observation.captured_sha256,
        digest_of(b"ready> got:pingEND")
    );
}

#[test]
fn a_pinned_digest_must_match_the_captured_bytes() {
    let adapter = adapter(READY, DELIMITER, 4096, 5000);
    let mut io = answering(&[]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let wrong = digest_of(b"not the bytes this run captured");
    let (satisfied, code) = verify(&observations, Some(&wrong));
    assert!(!satisfied);
    assert_eq!(code.as_deref(), Some("terminal_output_digest_mismatch"));
    // Without a pinned digest the measured bytes are the evidence.
    let (satisfied, _) = verify(&observations, None);
    assert!(satisfied);
}

#[test]
fn a_missing_delimiter_is_an_unsatisfied_observation() {
    let adapter = adapter(READY, DELIMITER, 4096, 300);
    let mut io = ScriptedIo::new(&[b"ready> ", b"got:ping"]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let observation = &observations[0];
    assert!(!observation.delimiter_found);
    assert!(
        observation
            .failure
            .as_deref()
            .is_some_and(|detail| detail.contains("did not answer")),
        "{:?}",
        observation.failure
    );
    assert!(observation.captured_bytes > 0);
    let (satisfied, code) = verify(&observations, None);
    assert!(!satisfied);
    assert_eq!(code.as_deref(), Some("terminal_exchange_failed"));
}

#[test]
fn an_output_bound_is_an_unsatisfied_observation() {
    let adapter = adapter(READY, DELIMITER, 32, 5000);
    let mut io = ScriptedIo::new(&[b"ready> ", b"0123456789012345678901234567890123456789"]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let observation = &observations[0];
    assert!(!observation.delimiter_found);
    assert!(
        observation
            .failure
            .as_deref()
            .is_some_and(|detail| detail.contains("more than the declared 32 bytes")),
        "{:?}",
        observation.failure
    );
    let (satisfied, _) = verify(&observations, None);
    assert!(!satisfied);
}

#[test]
fn a_ready_prompt_that_never_arrives_is_an_unsatisfied_observation() {
    let adapter = adapter(READY, DELIMITER, 4096, 300);
    let mut io = ScriptedIo::new(&[b"booting"]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let observation = &observations[0];
    assert!(!observation.delimiter_found);
    assert!(
        observation
            .failure
            .as_deref()
            .is_some_and(|detail| detail.contains("ready output")),
        "{:?}",
        observation.failure
    );
    // Nothing was typed into a workload that was never ready.
    assert!(io.written.is_empty());
    let (satisfied, _) = verify(&observations, None);
    assert!(!satisfied);
}

#[test]
fn a_completion_that_preceded_the_input_is_not_accepted() {
    // With no declared prompt the first output is the ready signal, so a
    // completion already waiting in that output is not this exchange's answer.
    let adapter = adapter(b"", DELIMITER, 4096, 5000);
    let mut io = ScriptedIo::new(&[b"END"]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let observation = &observations[0];
    assert!(!observation.delimiter_found);
    assert!(
        observation
            .failure
            .as_deref()
            .is_some_and(|detail| detail.contains("already present before the input")),
        "{:?}",
        observation.failure
    );
    assert!(io.written.is_empty());
}

#[test]
fn a_failed_exchange_leaves_the_following_operations_unobserved() {
    let mut adapter = adapter(READY, DELIMITER, 4096, 5000);
    adapter
        .operations
        .push(NativeProbeOperation::TerminalExchange {
            path: "/second".to_owned(),
            input: b"again\n".to_vec(),
            output_delimiter: b"DONE".to_vec(),
            max_output_bytes: 4096,
            timeout_ms: 5000,
            ready_output: Vec::new(),
        });
    let mut io = ScriptedIo::new(&[b"ready> ", b"got:ping"]);
    let observations = exchanges(
        &mut io,
        &adapter,
        &[
            ("exchange", &adapter.operations[0]),
            ("second", &adapter.operations[1]),
        ],
    );
    assert_eq!(observations.len(), 1);
    assert!(observations[0].failure.is_some());
}

#[test]
fn a_receipt_states_the_exchange_in_the_existing_observation_shape() {
    let adapter = adapter(READY, DELIMITER, 4096, 5000);
    let mut io = answering(&[]);
    let observations = exchanges(&mut io, &adapter, &[("exchange", &adapter.operations[0])]);
    let contract = contract(Some(&observations[0].captured_sha256));
    let runtime = RuntimeObservation {
        terminal: observations.clone(),
        ..Default::default()
    };
    let verification = verify_runtime(&contract, &runtime);
    let contract_ref = contract.contract_ref().expect("contract ref");
    let derivation_ref = format!("sha256:{}", "d".repeat(64));
    let mut receipt = ato_formation::verify::ContractVerificationReceipt::from_attempt(
        &contract_ref,
        &derivation_ref,
        &contract,
        &runtime,
        verification,
    );
    receipt.execution = Some(VerificationExecutionEvidence {
        realization: "native_desktop".to_owned(),
        attempt_id: Some("attempt-1".to_owned()),
        ..Default::default()
    });
    // No new receipt schema: the terminal facts ride the existing evidence
    // fields of the /1 observation shape.
    assert_eq!(
        receipt.schema,
        ato_formation::verify::CONTRACT_VERIFICATION_RECEIPT_SCHEMA
    );
    assert!(receipt.fully_satisfied);
    let evidence = receipt.observations[0].evidence.as_ref().expect("evidence");
    assert_eq!(evidence.path.as_deref(), Some("/result"));
    assert_eq!(
        evidence.body_sha256.as_deref(),
        Some(observations[0].captured_sha256.as_str())
    );
    assert_eq!(
        evidence.captured_bytes,
        Some(b"ready> got:pingEND".len() as u64)
    );
    assert!(evidence.method.is_none());
    assert!(evidence.status.is_none());
    let value = serde_json::to_value(&receipt).expect("json");
    assert_eq!(value["observations"][0]["outcome"], "satisfied");
    assert_eq!(value["observations"][0]["evidence"]["path"], "/result");
    assert!(value["observations"][0]["evidence"].get("method").is_none());

    let assignment = ato_formation::receipt::ReceiptAssignment {
        contract: &contract,
        contract_ref: &contract_ref,
        derivation_ref: &derivation_ref,
        attempt_id: "attempt-1",
        request_id: None,
    };
    ato_formation::receipt::accept_receipt(&assignment, &receipt)
        .expect("the terminal receipt is internally consistent");

    // A receipt that claims a satisfied exchange of no bytes is refused.
    let mut forged = receipt.clone();
    forged.observations[0]
        .evidence
        .as_mut()
        .expect("evidence")
        .captured_bytes = Some(0);
    assert!(ato_formation::receipt::accept_receipt(&assignment, &forged).is_err());
}

/// Whether this host lets a process allocate a pseudo terminal.
#[cfg(unix)]
fn pty_available() -> bool {
    match ato_pty_host::PtySession::spawn(
        &["/bin/true".to_owned()],
        &BTreeMap::new(),
        Path::new("/"),
        80,
        24,
    ) {
        Ok(mut session) => {
            let _ = session.stop();
            true
        }
        Err(_) => false,
    }
}

/// The same exchanges against a real PTY: echo off, no newlines, so the
/// capture is exactly the program's output through the delimiter.
#[cfg(unix)]
#[test]
fn a_real_pty_runs_the_declared_exchange_and_matches_the_same_digest() {
    if !pty_available() {
        eprintln!("NOTE: pty unavailable on this host; live terminal exchange not exercised");
        return;
    }
    let adapter = adapter(READY, DELIMITER, 4096, 5000);
    let mut session = ato_pty_host::PtySession::spawn(
        &[
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "stty -echo; printf 'ready> '; read line; printf 'got:%s' \"$line\"; printf 'END'"
                .to_owned(),
        ],
        &BTreeMap::new(),
        Path::new("/"),
        100,
        30,
    )
    .expect("pty session");
    let observations = exchanges(
        &mut session,
        &adapter,
        &[("exchange", &adapter.operations[0])],
    );
    session.stop().expect("the session stops");
    let observation = &observations[0];
    assert_eq!(observation.failure, None);
    assert!(observation.delimiter_found);
    assert_eq!(
        observation.captured_sha256,
        digest_of(b"ready> got:pingEND")
    );
}
