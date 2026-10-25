//! Executing a Contract's Scenarios against a running candidate.
//!
//! A Scenario is the immutable sequence of HTTP operations a Contract requires
//! (see `ato_formation::scenario`). This module is the runtime side: it issues
//! those operations against the candidate's own endpoints, records a
//! deterministic transcript, and states what it established. It does not start
//! or stop anything, and it never decides `K` — the verifier does that from the
//! observations returned here.
//!
//! ```text
//! readiness ─▶ Contract GET observations ─▶ scenarios (this module) ─▶ verify
//! ```
//!
//! Scenarios run after the read-only GET observations so their state changes
//! cannot affect what those observations measured.
//!
//! ## What the evidence is, and is not
//!
//! The receipt carries a digest of a canonical transcript in which captured
//! values are normalised to `${name}`; the raw sidecar keeps the resolved
//! requests, bounded response bodies and captures. The verifier only talks to
//! `candidate.endpoints()`: every path is origin-relative, redirects are not
//! followed, and no URL from the Scenario can name another host. That is not a
//! claim that the application cannot reach the network — this layer does not
//! control the candidate's egress.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::scenario::{
    SCENARIO_TRANSCRIPT_RAW_SCHEMA, SCENARIO_VERIFIER_VERSION, ScenarioExpectationV1,
    ScenarioJsonAssertionV1, ScenarioObservation, ScenarioResult, ScenarioStepV1,
    ScenarioTranscriptRequestV1, ScenarioTranscriptResponseV1, ScenarioTranscriptStepV1,
    ScenarioTranscriptV1,
};
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::realize::RunningCandidate;
use crate::spec::AttemptScenarioV1;

/// How much of one response body the raw sidecar keeps. The hashed transcript
/// only keeps digests, so this bound exists to keep evidence files finite.
const MAX_RAW_BODY_BYTES: usize = 64 * 1024;

/// Execute every scenario the attempt was given, in Contract order.
///
/// An error here is an attempt-level refusal (a port that was never realized, a
/// sidecar that cannot be written, an expired execution deadline). A step that
/// does not meet its expectation is not an error: it is a `Failed`
/// observation, which the verifier turns into `scenario_step_failed`.
pub fn execute_scenarios(
    candidate: &mut dyn RunningCandidate,
    plans: &[AttemptScenarioV1],
    attempt_root: &Path,
    interrupt: Option<&AtomicBool>,
    control: Option<&crate::control::ExecutionControl>,
) -> Result<Vec<ScenarioObservation>> {
    let mut observations = Vec::with_capacity(plans.len());
    if plans.is_empty() {
        return Ok(observations);
    }
    let client = reqwest::blocking::Client::builder()
        // A candidate has no business redirecting a Contract operation
        // somewhere else; only answers it gives directly are evidence.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("cannot build the scenario verifier's HTTP client")?;
    for plan in plans {
        plan.scenario.validate().map_err(|error| {
            FormationFailure::new(
                error.0,
                FailureStage::Verification,
                "the Contract's scenario object is invalid".to_owned(),
            )
        })?;
        if plan.scenario.verifier_version != SCENARIO_VERIFIER_VERSION {
            return Err(FormationFailure::new(
                "scenario_verifier_version_unsupported",
                FailureStage::Verification,
                format!(
                    "the scenario asks for verifier version {}; this runtime executes {SCENARIO_VERIFIER_VERSION}",
                    plan.scenario.verifier_version
                ),
            )
            .into());
        }
        let base = candidate
            .endpoints()
            .get(&plan.port)
            .cloned()
            .ok_or_else(|| {
                FormationFailure::new(
                    "scenario_port_not_realized",
                    FailureStage::Verification,
                    format!(
                        "the scenario's port {} was never realized, so it has no candidate to talk to",
                        plan.port
                    ),
                )
            })?;
        let executed = execute_one(&client, candidate, plan, &base, interrupt, control)?;
        write_raw_transcript(attempt_root, plan, &executed)?;
        observations.push(executed.observation);
    }
    Ok(observations)
}

/// Observations for a candidate that must not execute scenarios at all.
///
/// v1 requires a fresh instance; a candidate restored from state or a snapshot
/// is refused before anything runs. The refusal is an observation so the
/// receipt says exactly why, instead of the attempt failing without one.
pub fn isolation_failures(plans: &[AttemptScenarioV1]) -> Vec<ScenarioObservation> {
    plans
        .iter()
        .map(|plan| {
            ScenarioObservation::isolation_unavailable(
                plan.requirement_id.clone(),
                plan.scenario_ref.clone(),
            )
        })
        .collect()
}

struct ExecutedScenario {
    observation: ScenarioObservation,
    transcript: ScenarioTranscriptV1,
    steps: Vec<RawStep>,
    captures: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RawTranscript {
    schema: &'static str,
    requirement_id: String,
    scenario_ref: String,
    verifier_version: u32,
    transcript_digest: String,
    result: ScenarioResult,
    canonical_transcript: ScenarioTranscriptV1,
    steps: Vec<RawStep>,
    captures: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RawStep {
    id: String,
    method: String,
    path_resolved: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body_b64: Option<String>,
    response_body_truncated: bool,
    captures: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_detail: Option<String>,
}

/// The cookies a candidate set, in step order. Only `name=value` is kept:
/// v1 talks to one origin, so `Domain`/`Path`/`Expires` have nothing to decide.
#[derive(Default)]
struct CookieJar {
    values: BTreeMap<String, String>,
}

impl CookieJar {
    fn absorb(&mut self, response: &reqwest::blocking::Response) {
        for header in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let Ok(text) = header.to_str() else {
                continue;
            };
            let Some(pair) = text.split(';').next() else {
                continue;
            };
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if !name.is_empty() {
                self.values.insert(name.to_owned(), value.to_owned());
            }
        }
    }

    fn header(&self) -> Option<String> {
        if self.values.is_empty() {
            return None;
        }
        Some(
            self.values
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

fn execute_one(
    client: &reqwest::blocking::Client,
    candidate: &mut dyn RunningCandidate,
    plan: &AttemptScenarioV1,
    base: &str,
    interrupt: Option<&AtomicBool>,
    control: Option<&crate::control::ExecutionControl>,
) -> Result<ExecutedScenario> {
    let scenario = &plan.scenario;
    let started = Instant::now();
    let mut total = Duration::from_millis(scenario.limits.total_timeout_ms);
    if let Some(control) = control {
        total = total.min(control.cap(crate::control::AttemptPhase::Verification, total)?);
    }
    let total_deadline = started + total;

    let mut jar = CookieJar::default();
    let mut captures: BTreeMap<String, String> = BTreeMap::new();
    let mut transcript_steps = Vec::new();
    let mut raw_steps = Vec::new();
    let mut failure: Option<(String, String, String)> = None;
    let mut result = ScenarioResult::Satisfied;

    for step in &scenario.steps {
        if interrupt.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("interrupted before the Contract's scenarios completed");
        }
        let remaining = total_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            failure = Some((
                step.id.clone(),
                "timeout".to_owned(),
                "the scenario's total time budget was exhausted".to_owned(),
            ));
            result = ScenarioResult::Failed;
            break;
        }
        let step_timeout = Duration::from_millis(scenario.limits.step_timeout_ms).min(remaining);
        let outcome = execute_step(
            client,
            candidate,
            step,
            scenario.limits.max_body_bytes,
            base,
            &mut jar,
            &mut captures,
            step_timeout,
        )?;
        match outcome {
            Ok(mut executed) => {
                transcript_steps.push(executed.transcript);
                if let Some(step_failure) = executed.failure.take() {
                    raw_steps.push(executed.raw);
                    failure = Some((step.id.clone(), step_failure.0, step_failure.1));
                    result = ScenarioResult::Failed;
                    break;
                }
                raw_steps.push(executed.raw);
            }
            Err(StepError { code, detail, raw }) => {
                // A step that could not even be sent still has transcript
                // evidence: the request and the reason it produced nothing.
                transcript_steps.push(ScenarioTranscriptStepV1 {
                    id: step.id.clone(),
                    request: transcript_request(step)?,
                    response: ScenarioTranscriptResponseV1 {
                        status: 0,
                        body_sha256: digest_bytes(b""),
                    },
                    outcome: ScenarioResult::Failed,
                });
                raw_steps.push(raw);
                failure = Some((step.id.clone(), code, detail));
                result = ScenarioResult::Failed;
                break;
            }
        }
    }

    let transcript = ScenarioTranscriptV1::new(plan.scenario_ref.clone(), transcript_steps, result);
    let transcript_digest = transcript
        .transcript_digest()
        .map_err(|error| anyhow!(error))?;
    let observation = match &failure {
        None => ScenarioObservation::satisfied(
            plan.requirement_id.clone(),
            plan.scenario_ref.clone(),
            transcript_digest.clone(),
        ),
        Some((step, code, detail)) => ScenarioObservation::failed(
            plan.requirement_id.clone(),
            plan.scenario_ref.clone(),
            transcript_digest.clone(),
            Some(step.clone()),
            code.clone(),
            detail.clone(),
        ),
    };
    Ok(ExecutedScenario {
        observation,
        transcript,
        steps: raw_steps,
        captures,
    })
}

struct StepError {
    code: String,
    detail: String,
    raw: RawStep,
}

struct StepExecuted {
    transcript: ScenarioTranscriptStepV1,
    raw: RawStep,
    /// `Some((code, detail))` when the response did not meet the expectation.
    failure: Option<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
fn execute_step(
    client: &reqwest::blocking::Client,
    candidate: &mut dyn RunningCandidate,
    step: &ScenarioStepV1,
    max_body_bytes: u64,
    base: &str,
    jar: &mut CookieJar,
    captures: &mut BTreeMap<String, String>,
    timeout: Duration,
) -> Result<std::result::Result<StepExecuted, StepError>> {
    let path = substitute(&step.request.path, captures)?;
    let body = match &step.request.body_b64 {
        None => None,
        Some(encoded) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| anyhow!("the scenario step {} has an invalid body", step.id))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| anyhow!("the scenario step {} has a non-UTF8 body", step.id))?;
            Some(substitute(&text, captures)?.into_bytes())
        }
    };
    let cleared = step.request.cookies.is_some();
    let method = reqwest::Method::from_bytes(step.request.method.as_bytes())
        .map_err(|_| anyhow!("the scenario step {} has an invalid method", step.id))?;
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let cookie = if cleared { None } else { jar.header() };
    let raw_base = RawStep {
        id: step.id.clone(),
        method: step.request.method.clone(),
        path_resolved: path,
        body_b64: body
            .as_ref()
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
        response_status: None,
        response_body_b64: None,
        response_body_truncated: false,
        captures: BTreeMap::new(),
        failure_code: None,
        failure_detail: None,
    };
    let transcript_request = transcript_request(step)?;

    // A just-started candidate may not be listening yet. A refused connection
    // is not evidence until the step's own deadline passes; a candidate that
    // exited is a fact.
    let deadline = Instant::now() + timeout;
    let response = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Err(StepError {
                code: "request_failed".to_owned(),
                detail: format!("step {} could not be sent before its timeout", step.id),
                raw: raw_base,
            }));
        }
        let mut request = client.request(method.clone(), &url).timeout(remaining);
        for (name, value) in &step.request.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(cookie) = &cookie {
            request = request.header(reqwest::header::COOKIE, cookie.as_str());
        }
        if let Some(body) = &body {
            request = request.body(body.clone());
        }
        match request.send() {
            Ok(response) => break response,
            Err(error) => {
                if let Some(exit) = candidate.exited().map_err(|probe| {
                    anyhow!("cannot inspect the candidate after a failed scenario request: {probe}")
                })? {
                    return Ok(Err(StepError {
                        code: "candidate_exited".to_owned(),
                        detail: format!("the candidate exited during the scenario: {exit}"),
                        raw: raw_base,
                    }));
                }
                if Instant::now() >= deadline {
                    return Ok(Err(StepError {
                        code: "request_failed".to_owned(),
                        detail: format!("step {} could not be sent: {error}", step.id),
                        raw: raw_base,
                    }));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    let status = response.status().as_u16();
    jar.absorb(&response);
    let body_bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(Err(StepError {
                code: "response_unreadable".to_owned(),
                detail: format!("step {} response could not be read: {error}", step.id),
                raw: raw_base,
            }));
        }
    };
    if body_bytes.len() as u64 > max_body_bytes {
        let mut raw = raw_base;
        raw.response_status = Some(status);
        return Ok(Err(StepError {
            code: "body_too_large".to_owned(),
            detail: format!(
                "step {} returned {} bytes; the scenario allows {max_body_bytes}",
                step.id,
                body_bytes.len()
            ),
            raw,
        }));
    }
    let truncated = body_bytes.len() > MAX_RAW_BODY_BYTES;
    let raw = RawStep {
        response_status: Some(status),
        response_body_b64: Some(
            base64::engine::general_purpose::STANDARD
                .encode(&body_bytes[..body_bytes.len().min(MAX_RAW_BODY_BYTES)]),
        ),
        response_body_truncated: truncated,
        ..raw_base
    };

    let mut failure = check_expectation(step, status, &body_bytes)?;
    let mut normalized_captures = BTreeMap::new();
    if failure.is_none() {
        match apply_captures(step, &body_bytes, captures) {
            Ok(captured) => normalized_captures = captured,
            Err((code, detail)) => failure = Some((code, detail)),
        }
    }
    // Normalise with captures INCLUDING this step's own, so an app that
    // returns a fresh id in the response it captures still hashes the same.
    let response_digest = digest_bytes(&normalize_body(&body_bytes, captures));
    let transcript_step = ScenarioTranscriptStepV1 {
        id: step.id.clone(),
        request: transcript_request,
        response: ScenarioTranscriptResponseV1 {
            status,
            body_sha256: response_digest,
        },
        outcome: if failure.is_some() {
            ScenarioResult::Failed
        } else {
            ScenarioResult::Satisfied
        },
    };
    Ok(Ok(StepExecuted {
        transcript: transcript_step,
        raw: RawStep {
            captures: normalized_captures,
            failure_code: failure.as_ref().map(|(code, _)| code.clone()),
            failure_detail: failure.as_ref().map(|(_, detail)| detail.clone()),
            ..raw
        },
        failure,
    }))
}

fn transcript_request(step: &ScenarioStepV1) -> Result<ScenarioTranscriptRequestV1> {
    let body_sha256 = match &step.request.body_b64 {
        None => None,
        Some(encoded) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| anyhow!("the scenario step {} has an invalid body", step.id))?;
            Some(digest_bytes(&bytes))
        }
    };
    Ok(ScenarioTranscriptRequestV1 {
        method: step.request.method.clone(),
        path: step.request.path.clone(),
        body_sha256,
    })
}

fn check_expectation(
    step: &ScenarioStepV1,
    status: u16,
    body: &[u8],
) -> Result<Option<(String, String)>> {
    let expect: &ScenarioExpectationV1 = &step.expect;
    if status != expect.status {
        return Ok(Some((
            "status_mismatch".to_owned(),
            format!("status {status}; expected {}", expect.status),
        )));
    }
    if let Some(expected) = &expect.body_digest {
        let actual = digest_bytes(body);
        if actual != *expected {
            return Ok(Some((
                "body_digest_mismatch".to_owned(),
                format!("body {actual}; expected {expected}"),
            )));
        }
    }
    if !expect.json.is_empty() {
        let parsed: serde_json::Value = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(error) => {
                return Ok(Some((
                    "json_assertion_failed".to_owned(),
                    format!("the response is not JSON: {error}"),
                )));
            }
        };
        for assertion in &expect.json {
            if let Some(failure) = check_json_assertion(assertion, &parsed) {
                return Ok(Some(failure));
            }
        }
    }
    Ok(None)
}

fn check_json_assertion(
    assertion: &ScenarioJsonAssertionV1,
    body: &serde_json::Value,
) -> Option<(String, String)> {
    let value = json_pointer(body, &assertion.pointer);
    match (&assertion.equals, assertion.exists) {
        (Some(expected), _) => match value {
            Some(actual) if actual == expected => None,
            Some(actual) => Some((
                "json_assertion_failed".to_owned(),
                format!("{} is {actual}; expected {expected}", assertion.pointer),
            )),
            None => Some((
                "json_assertion_failed".to_owned(),
                format!("{} is absent", assertion.pointer),
            )),
        },
        (None, Some(true)) => (value.is_none()).then(|| {
            (
                "json_assertion_failed".to_owned(),
                format!("{} is absent", assertion.pointer),
            )
        }),
        (None, Some(false)) => value.is_some().then(|| {
            (
                "json_assertion_failed".to_owned(),
                format!("{} is present", assertion.pointer),
            )
        }),
        (None, None) => None,
    }
}

/// Apply a step's captures to the shared map, returning the values captured by
/// this step (for the raw sidecar).
fn apply_captures(
    step: &ScenarioStepV1,
    body: &[u8],
    captures: &mut BTreeMap<String, String>,
) -> std::result::Result<BTreeMap<String, String>, (String, String)> {
    if step.capture.is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).map_err(|error| {
        (
            "capture_failed".to_owned(),
            format!("the response is not JSON: {error}"),
        )
    })?;
    let mut step_captures = BTreeMap::new();
    for capture in &step.capture {
        let Some(value) = json_pointer(&parsed, &capture.json_pointer) else {
            return Err((
                "capture_failed".to_owned(),
                format!("{} is absent", capture.json_pointer),
            ));
        };
        let text = match value {
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            other => {
                return Err((
                    "capture_failed".to_owned(),
                    format!(
                        "{} is {}, not a string, number or boolean",
                        capture.json_pointer,
                        json_kind(other)
                    ),
                ));
            }
        };
        captures.insert(capture.name.clone(), text.clone());
        step_captures.insert(capture.name.clone(), text);
    }
    Ok(step_captures)
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// RFC 6901 pointer lookup. `""` is the whole document.
fn json_pointer<'v>(value: &'v serde_json::Value, pointer: &str) -> Option<&'v serde_json::Value> {
    if pointer.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for raw in pointer.strip_prefix('/')?.split('/') {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        current = match current {
            serde_json::Value::Object(map) => map.get(&segment)?,
            serde_json::Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Replace `${name}` with the captured value. Validation already checked that
/// every reference exists; a missing one here is a builder bug, not evidence.
fn substitute(template: &str, captures: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("unterminated ${{...}} in a scenario template");
        };
        let name = &after[..end];
        let value = captures
            .get(name)
            .ok_or_else(|| anyhow!("scenario template names the uncaptured value ${{{name}}}"))?;
        output.push_str(value);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

/// Normalise every captured value to `${name}` so the transcript digest is
/// stable across runs (an app that returns a random id still has one digest).
fn normalize_body(body: &[u8], captures: &BTreeMap<String, String>) -> Vec<u8> {
    let mut normalized = body.to_vec();
    for (name, value) in captures {
        if value.is_empty() {
            continue;
        }
        normalized = replace_bytes(
            &normalized,
            value.as_bytes(),
            format!("${{{name}}}").as_bytes(),
        );
    }
    normalized
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut output = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].starts_with(needle) {
            output.extend_from_slice(replacement);
            index += needle.len();
        } else {
            output.push(haystack[index]);
            index += 1;
        }
    }
    output
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn write_raw_transcript(
    attempt_root: &Path,
    plan: &AttemptScenarioV1,
    executed: &ExecutedScenario,
) -> Result<()> {
    let directory = attempt_root.join("scenario-transcripts");
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "cannot create the scenario transcript directory {}",
            directory.display()
        )
    })?;
    let name = sidecar_name(&plan.requirement_id);
    let path: PathBuf = directory.join(format!("{name}.json"));
    let raw = RawTranscript {
        schema: SCENARIO_TRANSCRIPT_RAW_SCHEMA,
        requirement_id: plan.requirement_id.clone(),
        scenario_ref: plan.scenario_ref.clone(),
        verifier_version: SCENARIO_VERIFIER_VERSION,
        transcript_digest: executed.observation.transcript_digest.clone(),
        result: executed.observation.result,
        canonical_transcript: executed.transcript.clone(),
        steps: executed
            .steps
            .iter()
            .map(|step| RawStep {
                id: step.id.clone(),
                method: step.method.clone(),
                path_resolved: step.path_resolved.clone(),
                body_b64: step.body_b64.clone(),
                response_status: step.response_status,
                response_body_b64: step.response_body_b64.clone(),
                response_body_truncated: step.response_body_truncated,
                captures: step.captures.clone(),
                failure_code: step.failure_code.clone(),
                failure_detail: step.failure_detail.clone(),
            })
            .collect(),
        captures: executed.captures.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&raw)
        .context("cannot serialize the scenario transcript sidecar")?;
    std::fs::write(&path, bytes).with_context(|| {
        format!(
            "cannot write the scenario transcript sidecar {}",
            path.display()
        )
    })?;
    Ok(())
}

/// A requirement id that cannot escape the sidecar directory.
fn sidecar_name(id: &str) -> String {
    let readable: String = id
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if readable.is_empty() {
        "scenario".to_owned()
    } else {
        readable
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use ato_formation::scenario::{
        ScenarioExpectationV1, ScenarioIsolation, ScenarioLimits, ScenarioNetwork,
        ScenarioRequestV1, ScenarioStepV1, ScenarioV1, is_scenario_ref,
    };

    use super::*;

    /// A minimal stateful HTTP app: a session cookie, in-memory items, and an
    /// unauthorized read when the cookie is absent.
    struct Fixture {
        endpoint: String,
        stop: Arc<AtomicBool>,
        seen_cookies: Arc<Mutex<Vec<Option<String>>>>,
        task: Option<std::thread::JoinHandle<()>>,
    }

    impl Fixture {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let endpoint = format!("http://{}", listener.local_addr().expect("address"));
            let stop = Arc::new(AtomicBool::new(false));
            let seen_cookies = Arc::new(Mutex::new(Vec::new()));
            let task_stop = Arc::clone(&stop);
            let task_cookies = Arc::clone(&seen_cookies);
            let task = std::thread::spawn(move || {
                let next_item = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                while !task_stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("read timeout");
                    let Some(request) = read_request(&mut stream) else {
                        continue;
                    };
                    let cookie = request.headers.get("cookie").cloned();
                    task_cookies.lock().unwrap().push(cookie.clone());
                    let authorized = cookie
                        .as_deref()
                        .is_some_and(|value| value.contains("session=ok"));
                    let (status, extra, body) = if request.method == "POST"
                        && request.path == "/login"
                    {
                        (200, "Set-Cookie: session=ok; Path=/\r\n", String::new())
                    } else if request.method == "POST" && request.path == "/items" {
                        let item = next_item.fetch_add(1, Ordering::SeqCst) + 1;
                        (201, "", format!(r#"{{"id":"item-{item}","name":"first"}}"#))
                    } else if request.method == "GET" && request.path.starts_with("/items/") {
                        let id = request.path.trim_start_matches("/items/").to_owned();
                        if authorized {
                            (200, "", format!(r#"{{"id":"{id}","name":"first"}}"#))
                        } else {
                            (401, "", r#"{"error":"unauthorized"}"#.to_owned())
                        }
                    } else if request.method == "PUT" && request.path.starts_with("/items/") {
                        let id = request.path.trim_start_matches("/items/").to_owned();
                        (200, "", format!(r#"{{"id":"{id}","name":"updated"}}"#))
                    } else if request.method == "DELETE" && request.path.starts_with("/items/") {
                        (204, "", String::new())
                    } else {
                        (404, "", r#"{"error":"not_found"}"#.to_owned())
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            Self {
                endpoint,
                stop,
                seen_cookies,
                task: Some(task),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(task) = self.task.take() {
                let _ = task.join();
            }
        }
    }

    struct Request {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                return None;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next()?.split_whitespace();
        let method = request_line.next()?.to_owned();
        let path = request_line.next()?.to_owned();
        let mut headers = BTreeMap::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        let length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = bytes[header_end..].to_vec();
        while body.len() < length {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        Some(Request {
            method,
            path,
            headers,
        })
    }

    struct Candidate {
        endpoints: BTreeMap<String, String>,
    }

    impl Candidate {
        fn new(endpoint: &str) -> Self {
            Self {
                endpoints: BTreeMap::from([("app.http".to_owned(), endpoint.to_owned())]),
            }
        }
    }

    impl RunningCandidate for Candidate {
        fn endpoints(&self) -> &BTreeMap<String, String> {
            &self.endpoints
        }
        fn exited(&mut self) -> Result<Option<String>> {
            Ok(None)
        }
        fn stop(self: Box<Self>) -> Result<()> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn step(id: &str, method: &str, path: &str, expect: ScenarioExpectationV1) -> ScenarioStepV1 {
        ScenarioStepV1 {
            id: id.to_owned(),
            request: ScenarioRequestV1 {
                method: method.to_owned(),
                path: path.to_owned(),
                headers: BTreeMap::new(),
                body_b64: None,
                cookies: None,
            },
            expect,
            capture: Vec::new(),
        }
    }

    fn expectation(status: u16) -> ScenarioExpectationV1 {
        ScenarioExpectationV1 {
            status,
            body_digest: None,
            json: Vec::new(),
        }
    }

    fn crud_scenario() -> ScenarioV1 {
        let mut login = step("login", "POST", "/login", expectation(200));
        login.request.body_b64 = Some(base64::engine::general_purpose::STANDARD.encode("{}"));
        let mut create = step(
            "create",
            "POST",
            "/items",
            ScenarioExpectationV1 {
                status: 201,
                body_digest: None,
                json: vec![ato_formation::scenario::ScenarioJsonAssertionV1 {
                    pointer: "/id".to_owned(),
                    equals: None,
                    exists: Some(true),
                }],
            },
        );
        create.request.body_b64 =
            Some(base64::engine::general_purpose::STANDARD.encode(r#"{"name":"first"}"#));
        create.capture = vec![ato_formation::scenario::ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/id".to_owned(),
        }];
        let mut unauthorized = step("unauthorized", "GET", "/items/${item}", expectation(401));
        unauthorized.request.cookies = Some(ato_formation::scenario::ScenarioCookiePolicy::Cleared);
        let read = step("read", "GET", "/items/${item}", expectation(200));
        ScenarioV1 {
            schema: ato_formation::scenario::SCENARIO_SCHEMA.to_owned(),
            verifier_version: ato_formation::scenario::SCENARIO_VERIFIER_VERSION,
            isolation: ScenarioIsolation {
                fresh_instance: true,
                network: ScenarioNetwork::CandidateOnly,
            },
            limits: ScenarioLimits {
                step_timeout_ms: 5_000,
                total_timeout_ms: 30_000,
                max_body_bytes: 1_048_576,
            },
            steps: vec![login, create, unauthorized, read],
        }
    }

    fn plan(scenario: ScenarioV1, requirement_id: &str) -> AttemptScenarioV1 {
        let scenario_ref = scenario.scenario_ref().expect("reference");
        AttemptScenarioV1 {
            requirement_id: requirement_id.to_owned(),
            port: "app.http".to_owned(),
            scenario_ref,
            scenario,
        }
    }

    fn run(
        fixture: &Fixture,
        plans: &[AttemptScenarioV1],
        root: &Path,
    ) -> Result<Vec<ScenarioObservation>> {
        let mut candidate = Candidate::new(&fixture.endpoint);
        execute_scenarios(&mut candidate, plans, root, None, None)
    }

    #[test]
    fn a_transcript_digest_is_stable_across_runs_with_different_capture_values() {
        let fixture = Fixture::new();
        let scratch = tempfile::tempdir().expect("scratch");
        let plan = plan(crud_scenario(), "crud");

        let first = run(&fixture, std::slice::from_ref(&plan), scratch.path()).expect("first run");
        let second =
            run(&fixture, std::slice::from_ref(&plan), scratch.path()).expect("second run");
        assert_eq!(first[0].result, ScenarioResult::Satisfied, "{first:?}");
        assert_eq!(second[0].result, ScenarioResult::Satisfied, "{second:?}");
        assert!(is_scenario_ref(&first[0].transcript_digest));
        assert_eq!(
            first[0].transcript_digest, second[0].transcript_digest,
            "captured ids differ between runs; the digest must not"
        );

        // The cleared-cookie step saw no session, and the following step did:
        // clearing is per request, not destructive.
        let cookies = fixture.seen_cookies.lock().unwrap();
        assert_eq!(cookies.len(), 8, "{cookies:?}");
        for unauthorized in [2usize, 6] {
            assert!(
                cookies[unauthorized].is_none()
                    || !cookies[unauthorized]
                        .as_deref()
                        .unwrap_or("")
                        .contains("session"),
                "the unauthorized read must send no session: {cookies:?}"
            );
            assert!(
                cookies[unauthorized + 1]
                    .as_deref()
                    .is_some_and(|cookie| cookie.contains("session=ok")),
                "the read after it must keep the session: {cookies:?}"
            );
        }

        // The sidecar keeps the raw evidence and the canonical transcript.
        let sidecar = scratch.path().join("scenario-transcripts/crud.json");
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar).expect("sidecar")).expect("json");
        assert_eq!(raw["schema"], SCENARIO_TRANSCRIPT_RAW_SCHEMA);
        assert_eq!(
            raw["canonical_transcript"]["schema"],
            "ato.scenario-transcript/1"
        );
        assert_eq!(raw["transcript_digest"], first[0].transcript_digest);
        assert_eq!(raw["result"], "satisfied");
    }

    #[test]
    fn a_step_that_misses_its_expectation_is_a_failed_observation() {
        let fixture = Fixture::new();
        let scratch = tempfile::tempdir().expect("scratch");
        let mut scenario = crud_scenario();
        // The app answers 200 for the read; demanding 404 fails that step.
        scenario.steps[3].expect.status = 404;
        let plan = plan(scenario, "crud");
        let observations = run(&fixture, &[plan], scratch.path()).expect("executes");
        let observation = &observations[0];
        assert_eq!(observation.result, ScenarioResult::Failed);
        assert_eq!(observation.failed_step.as_deref(), Some("read"));
        assert_eq!(observation.code.as_deref(), Some("status_mismatch"));
        assert!(
            observation
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("404")),
            "{observation:?}"
        );
        assert!(is_scenario_ref(&observation.transcript_digest));
    }

    #[test]
    fn a_scenario_cannot_run_on_a_port_that_was_not_realized() {
        let fixture = Fixture::new();
        let scratch = tempfile::tempdir().expect("scratch");
        let mut plan = plan(crud_scenario(), "crud");
        plan.port = "other.http".to_owned();
        let mut candidate = Candidate::new(&fixture.endpoint);
        let error = execute_scenarios(&mut candidate, &[plan], scratch.path(), None, None)
            .expect_err("must refuse");
        let failure = error
            .downcast_ref::<FormationFailure>()
            .expect("typed failure");
        assert_eq!(failure.code, "scenario_port_not_realized");
    }

    #[test]
    fn isolation_refusals_are_observations_not_silence() {
        let plans = vec![plan(crud_scenario(), "crud")];
        let observations = isolation_failures(&plans);
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].code.as_deref(),
            Some("scenario_isolation_unavailable")
        );
        assert_eq!(observations[0].result, ScenarioResult::Failed);
    }
}
