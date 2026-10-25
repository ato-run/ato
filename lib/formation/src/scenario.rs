//! Scenarios: the immutable, content-addressed sequence of HTTP operations a
//! Contract requires a running candidate to perform.
//!
//! A Scenario is referenced from a Contract requirement by its content address
//! (see [`crate::authoring::SCENARIO_CONTRACT_VERIFIER`]). The steps, inputs and
//! expected outputs live here; the implementation and runtime stay in the
//! Derivation. The object is canonical JCS, so the reference a Contract pins is
//! the digest of the bytes a bundle carries.
//!
//! What a Scenario is not: a new semantic root, a completeness claim, or an
//! authorization. `fully_satisfied` still means only "the defined Contract was
//! satisfied". Execution is a runtime concern (a later PR); this module only
//! states the immutable plan and validates it strictly.
//!
//! ## Shape
//!
//! ```text
//! steps: [ login, create, update, unauthorized-read, delete, read-after-delete ]
//!          │       │        │              │              │            │
//!          │       └─ capture item id ──────┘              │            │
//!          └─ cookie jar is carried between steps          │            │
//!                                                   cleared cookies → 401
//! ```
//!
//! Order is semantic: steps run in the order written. A capture from one step
//! may be substituted into a later step as `${name}` in its path or body. The
//! hashed transcript (runtime side) normalises those substitutions so the
//! digest is stable across runs.
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The schema id a Scenario object carries.
pub const SCENARIO_SCHEMA: &str = "ato.scenario/1";
/// The only verifier version this build can execute and decide.
pub const SCENARIO_VERIFIER_VERSION: u32 = 1;

pub const MAX_SCENARIO_STEPS: usize = 64;
pub const MAX_STEP_TIMEOUT_MS: u64 = 60_000;
pub const MAX_TOTAL_TIMEOUT_MS: u64 = 600_000;
/// Mirrors the Runtime's own bounded response body (`MAX_BODY_BYTES`).
pub const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_HEADERS: usize = 32;
pub const MAX_HEADER_NAME: usize = 64;
pub const MAX_HEADER_VALUE: usize = 4096;
pub const MAX_CAPTURES: usize = 16;
pub const MAX_PATH_BYTES: usize = 2048;
pub const MAX_JSON_ASSERTIONS: usize = 16;
pub const MAX_JSON_POINTER_BYTES: usize = 512;

const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];
/// Headers a Scenario may never state: the session is established by a step,
/// and the verifier owns the connection's framing.
const FORBIDDEN_HEADERS: &[&str] = &["authorization", "cookie", "host", "content-length"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ScenarioError(pub &'static str);

/// Whether `value` is a `sha256:<64 lowercase hex>` reference.
pub fn is_scenario_ref(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One ordered HTTP operation a Contract requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioV1 {
    pub schema: String,
    pub verifier_version: u32,
    pub isolation: ScenarioIsolation,
    pub limits: ScenarioLimits,
    pub steps: Vec<ScenarioStepV1>,
}

/// The conditions under which a Scenario's evidence means anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioIsolation {
    /// The candidate must start from a fresh instance: no restored state and no
    /// restored snapshot. A run that cannot promise this refuses the scenario.
    pub fresh_instance: bool,
    pub network: ScenarioNetwork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioNetwork {
    /// The verifier talks only to the candidate's own endpoints.
    CandidateOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioLimits {
    pub step_timeout_ms: u64,
    pub total_timeout_ms: u64,
    pub max_body_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioStepV1 {
    pub id: String,
    pub request: ScenarioRequestV1,
    pub expect: ScenarioExpectationV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture: Vec<ScenarioCaptureV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioRequestV1 {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// base64 of the UTF-8 request body template. `${name}` substitution is
    /// performed on the template before it is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
    /// `cleared` sends the request with an empty cookie jar: the negative case
    /// for "this must be unauthorized".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<ScenarioCookiePolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioCookiePolicy {
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioCaptureV1 {
    pub name: String,
    /// RFC 6901 pointer into the response body's JSON.
    pub json_pointer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioExpectationV1 {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub json: Vec<ScenarioJsonAssertionV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioJsonAssertionV1 {
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

impl ScenarioV1 {
    /// Structure and policy. Order is preserved; only membership is checked.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        if self.schema != SCENARIO_SCHEMA {
            return Err(ScenarioError("scenario_schema_unsupported"));
        }
        if self.verifier_version != SCENARIO_VERIFIER_VERSION {
            return Err(ScenarioError("scenario_verifier_version_unsupported"));
        }
        if !self.isolation.fresh_instance
            || self.isolation.network != ScenarioNetwork::CandidateOnly
        {
            return Err(ScenarioError("scenario_isolation_invalid"));
        }
        if self.limits.step_timeout_ms == 0
            || self.limits.step_timeout_ms > MAX_STEP_TIMEOUT_MS
            || self.limits.total_timeout_ms == 0
            || self.limits.total_timeout_ms > MAX_TOTAL_TIMEOUT_MS
            || self.limits.total_timeout_ms < self.limits.step_timeout_ms
            || self.limits.max_body_bytes == 0
            || self.limits.max_body_bytes > MAX_BODY_BYTES
        {
            return Err(ScenarioError("scenario_limits_invalid"));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_SCENARIO_STEPS {
            return Err(ScenarioError("scenario_step_count"));
        }

        let mut ids = BTreeSet::new();
        let mut captures = BTreeSet::new();
        for step in &self.steps {
            validate_step_id(&step.id)?;
            if !ids.insert(step.id.as_str()) {
                return Err(ScenarioError("scenario_step_id_duplicate"));
            }
            self.validate_request(&step.request, &captures)?;
            self.validate_expectation(&step.expect)?;
            if step.capture.len() > MAX_CAPTURES {
                return Err(ScenarioError("scenario_capture_invalid"));
            }
            for capture in &step.capture {
                validate_capture_name(&capture.name)?;
                if !captures.insert(capture.name.as_str()) {
                    return Err(ScenarioError("scenario_capture_duplicate"));
                }
                if !is_json_pointer(&capture.json_pointer) {
                    return Err(ScenarioError("scenario_capture_invalid"));
                }
            }
        }
        Ok(())
    }

    fn validate_request(
        &self,
        request: &ScenarioRequestV1,
        earlier_captures: &BTreeSet<&str>,
    ) -> Result<(), ScenarioError> {
        if !METHODS.contains(&request.method.as_str()) {
            return Err(ScenarioError("scenario_method_unsupported"));
        }
        if !is_origin_relative_path(&request.path) || request.path.len() > MAX_PATH_BYTES {
            return Err(ScenarioError("scenario_path_invalid"));
        }
        validate_template(&request.path, earlier_captures)?;
        if request.headers.len() > MAX_HEADERS {
            return Err(ScenarioError("scenario_header_invalid"));
        }
        for (name, value) in &request.headers {
            if !is_header_name(name)
                || name.len() > MAX_HEADER_NAME
                || value.len() > MAX_HEADER_VALUE
                || value.contains('\0')
                || value.contains('\n')
                || value.contains('\r')
            {
                return Err(ScenarioError("scenario_header_invalid"));
            }
            if FORBIDDEN_HEADERS.contains(&name.as_str()) {
                return Err(ScenarioError("scenario_header_forbidden"));
            }
        }
        match (&request.method.as_str(), &request.body_b64) {
            (&"GET" | &"HEAD", Some(_)) => {
                return Err(ScenarioError("scenario_body_invalid"));
            }
            (_, Some(encoded)) => {
                let body = decode_body(encoded, self.limits.max_body_bytes)?;
                let text = std::str::from_utf8(&body)
                    .map_err(|_| ScenarioError("scenario_body_invalid"))?;
                validate_template(text, earlier_captures)?;
            }
            (_, None) => {}
        }
        Ok(())
    }

    fn validate_expectation(&self, expect: &ScenarioExpectationV1) -> Result<(), ScenarioError> {
        if !(100..=599).contains(&expect.status) {
            return Err(ScenarioError("scenario_expectation_invalid"));
        }
        if let Some(digest) = &expect.body_digest
            && !is_scenario_ref(digest)
        {
            return Err(ScenarioError("scenario_body_digest_invalid"));
        }
        if expect.json.len() > MAX_JSON_ASSERTIONS {
            return Err(ScenarioError("scenario_json_assertion_invalid"));
        }
        for assertion in &expect.json {
            if !is_json_pointer(&assertion.pointer)
                || assertion.pointer.len() > MAX_JSON_POINTER_BYTES
            {
                return Err(ScenarioError("scenario_json_assertion_invalid"));
            }
            match (&assertion.equals, &assertion.exists) {
                (Some(_), None) => {}
                (None, Some(_)) => {}
                _ => return Err(ScenarioError("scenario_json_assertion_invalid")),
            }
        }
        Ok(())
    }

    /// JCS of the validated scenario. The struct's field order and the sorted
    /// header map make this deterministic; step order is authored order.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ScenarioError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|_| ScenarioError("scenario_canonicalization"))
    }

    /// `sha256:` of the canonical bytes: the Scenario's identity.
    pub fn scenario_ref(&self) -> Result<String, ScenarioError> {
        let bytes = self.canonical_bytes()?;
        Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
    }

    /// Parse strict JSON, validate it, and require it to already be canonical.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<(Self, String), ScenarioError> {
        let scenario: Self =
            serde_json::from_slice(bytes).map_err(|_| ScenarioError("scenario_malformed"))?;
        scenario.validate()?;
        if scenario.canonical_bytes()? != bytes {
            return Err(ScenarioError("scenario_not_canonical"));
        }
        let reference = scenario.scenario_ref()?;
        Ok((scenario, reference))
    }

    /// Parse strict JSON and validate it. Authoring files may be pretty-printed;
    /// the bundle builder canonicalises before it stores the object.
    pub fn parse_json(bytes: &[u8]) -> Result<Self, ScenarioError> {
        let scenario: Self =
            serde_json::from_slice(bytes).map_err(|_| ScenarioError("scenario_malformed"))?;
        scenario.validate()?;
        Ok(scenario)
    }
}

/// The schema of the deterministic transcript whose digest a receipt carries.
pub const SCENARIO_TRANSCRIPT_SCHEMA: &str = "ato.scenario-transcript/1";
/// The schema of the raw sidecar kept beside a Run's receipt. It is evidence,
/// not identity: captures, response bodies and failure details also live here,
/// so the hashed transcript can stay stable across runs.
pub const SCENARIO_TRANSCRIPT_RAW_SCHEMA: &str = "ato.scenario-transcript-raw/1";

/// The outcome of executing one Scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioResult {
    Satisfied,
    Failed,
}

/// What one runtime Scenario execution established, as the Runtime states it.
///
/// This is evidence, not identity: `scenario_ref` is the object K pins and
/// `transcript_digest` is a digest of the deterministic transcript. It never
/// carries response bodies or captured values; those stay in the raw sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioObservation {
    /// The Contract requirement id this execution answers.
    pub requirement_id: String,
    /// The `ato.scenario/1` object the Contract requirement pins.
    pub scenario_ref: String,
    pub verifier_version: u32,
    /// `sha256:` of the canonical transcript. Empty for an execution that was
    /// refused before it started (e.g. isolation unavailable).
    pub transcript_digest: String,
    pub result: ScenarioResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// The verifier-level failure code, when `result` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ScenarioObservation {
    pub fn satisfied(
        requirement_id: impl Into<String>,
        scenario_ref: impl Into<String>,
        transcript_digest: impl Into<String>,
    ) -> Self {
        Self {
            requirement_id: requirement_id.into(),
            scenario_ref: scenario_ref.into(),
            verifier_version: SCENARIO_VERIFIER_VERSION,
            transcript_digest: transcript_digest.into(),
            result: ScenarioResult::Satisfied,
            failed_step: None,
            code: None,
            detail: None,
        }
    }

    pub fn failed(
        requirement_id: impl Into<String>,
        scenario_ref: impl Into<String>,
        transcript_digest: impl Into<String>,
        failed_step: Option<String>,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            requirement_id: requirement_id.into(),
            scenario_ref: scenario_ref.into(),
            verifier_version: SCENARIO_VERIFIER_VERSION,
            transcript_digest: transcript_digest.into(),
            result: ScenarioResult::Failed,
            failed_step,
            code: Some(code.into()),
            detail: Some(detail.into()),
        }
    }

    /// A refusal before the scenario ran: the v1 verifier only executes on a
    /// fresh instance, never on a restored/stateful one.
    pub fn isolation_unavailable(
        requirement_id: impl Into<String>,
        scenario_ref: impl Into<String>,
    ) -> Self {
        Self::failed(
            requirement_id,
            scenario_ref,
            String::new(),
            None,
            "scenario_isolation_unavailable",
            "the candidate runs from restored state or a snapshot; v1 executes scenarios only \
             on a fresh instance",
        )
    }
}

/// The deterministic transcript of one Scenario execution.
///
/// Captured values are normalised to `${name}` before hashing, so an app that
/// returns a random id produces the same digest on every run. Timing, pids,
/// endpoints and cookie values are deliberately absent. The object is JCS
/// canonical; its SHA-256 is what the receipt records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioTranscriptV1 {
    pub schema: String,
    pub scenario_ref: String,
    pub verifier_version: u32,
    pub steps: Vec<ScenarioTranscriptStepV1>,
    pub result: ScenarioResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioTranscriptStepV1 {
    pub id: String,
    pub request: ScenarioTranscriptRequestV1,
    pub response: ScenarioTranscriptResponseV1,
    pub outcome: ScenarioResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioTranscriptRequestV1 {
    pub method: String,
    /// The authored path template: `${name}` stays in place.
    pub path: String,
    /// SHA-256 of the authored body template, not the substituted bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioTranscriptResponseV1 {
    pub status: u16,
    /// SHA-256 of the response body with captured values normalised to
    /// `${name}`.
    pub body_sha256: String,
}

impl ScenarioTranscriptV1 {
    pub fn new(
        scenario_ref: impl Into<String>,
        steps: Vec<ScenarioTranscriptStepV1>,
        result: ScenarioResult,
    ) -> Self {
        Self {
            schema: SCENARIO_TRANSCRIPT_SCHEMA.to_owned(),
            scenario_ref: scenario_ref.into(),
            verifier_version: SCENARIO_VERIFIER_VERSION,
            steps,
            result,
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ScenarioError> {
        serde_jcs::to_vec(self).map_err(|_| ScenarioError("scenario_transcript_canonicalization"))
    }

    /// `sha256:` of the canonical transcript bytes.
    pub fn transcript_digest(&self) -> Result<String, ScenarioError> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(self.canonical_bytes()?)
        ))
    }
}

fn decode_body(encoded: &str, max_bytes: u64) -> Result<Vec<u8>, ScenarioError> {
    let body = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ScenarioError("scenario_body_invalid"))?;
    if body.len() as u64 > max_bytes {
        return Err(ScenarioError("scenario_body_too_large"));
    }
    Ok(body)
}

fn validate_step_id(id: &str) -> Result<(), ScenarioError> {
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(ScenarioError("scenario_step_id_invalid"));
    }
    Ok(())
}

fn validate_capture_name(name: &str) -> Result<(), ScenarioError> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(ScenarioError("scenario_capture_invalid"));
    }
    Ok(())
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// An origin-relative request target: `/…`, no scheme, no authority, no
/// fragment, no query-string smuggling and no NUL/backslash.
fn is_origin_relative_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\0')
        && !path.contains('\\')
        && !path.contains('#')
        && !path.contains('?')
        && !path.contains('\n')
        && !path.contains('\r')
}

/// Validate every `${name}` in `template` refers to a capture from an earlier
/// step. A literal `$` not followed by `{` is allowed.
fn validate_template(
    template: &str,
    earlier_captures: &BTreeSet<&str>,
) -> Result<(), ScenarioError> {
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
            let start = index + 2;
            let Some(relative_end) = template[start..].find('}') else {
                return Err(ScenarioError("scenario_template_invalid"));
            };
            let name = &template[start..start + relative_end];
            if !is_capture_reference(name) || !earlier_captures.contains(name) {
                return Err(ScenarioError("scenario_template_invalid"));
            }
            index = start + relative_end + 1;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn is_capture_reference(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// RFC 6901: `""` or `/`-separated segments where `~` is only `~0`/`~1`.
fn is_json_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    for segment in pointer[1..].split('/') {
        let mut chars = segment.chars();
        while let Some(character) = chars.next() {
            if character == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, method: &str, path: &str) -> ScenarioStepV1 {
        ScenarioStepV1 {
            id: id.to_owned(),
            request: ScenarioRequestV1 {
                method: method.to_owned(),
                path: path.to_owned(),
                headers: BTreeMap::new(),
                body_b64: None,
                cookies: None,
            },
            expect: ScenarioExpectationV1 {
                status: 200,
                body_digest: None,
                json: Vec::new(),
            },
            capture: Vec::new(),
        }
    }

    fn scenario(steps: Vec<ScenarioStepV1>) -> ScenarioV1 {
        ScenarioV1 {
            schema: SCENARIO_SCHEMA.to_owned(),
            verifier_version: SCENARIO_VERIFIER_VERSION,
            isolation: ScenarioIsolation {
                fresh_instance: true,
                network: ScenarioNetwork::CandidateOnly,
            },
            limits: ScenarioLimits {
                step_timeout_ms: 5_000,
                total_timeout_ms: 60_000,
                max_body_bytes: 1_048_576,
            },
            steps,
        }
    }

    #[test]
    fn a_valid_sequence_has_a_stable_content_address() {
        let mut login = step("login", "POST", "/login");
        login.request.body_b64 =
            Some(base64::engine::general_purpose::STANDARD.encode(r#"{"user":"test"}"#));
        login.expect.status = 200;
        let mut create = step("create", "POST", "/items");
        create.capture.push(ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/id".to_owned(),
        });
        let mut update = step("update", "PUT", "/items/${item}");
        update.expect.json.push(ScenarioJsonAssertionV1 {
            pointer: "/ok".to_owned(),
            equals: Some(serde_json::json!(true)),
            exists: None,
        });
        let scenario = scenario(vec![login, create, update]);
        let reference = scenario.scenario_ref().unwrap();
        let bytes = scenario.canonical_bytes().unwrap();
        assert_eq!(
            ScenarioV1::from_canonical_bytes(&bytes).unwrap(),
            (scenario.clone(), reference.clone())
        );
        assert_eq!(reference, format!("sha256:{:x}", Sha256::digest(&bytes)));
        // Re-serialising the same plan yields the same bytes.
        let reparsed = ScenarioV1::parse_json(&bytes).unwrap();
        assert_eq!(reparsed.canonical_bytes().unwrap(), bytes);
    }

    #[test]
    fn invalid_plans_are_refused_by_code() {
        let cases: Vec<(&str, ScenarioV1)> = vec![
            ("scenario_schema_unsupported", {
                let mut s = scenario(vec![step("a", "GET", "/")]);
                s.schema = "ato.scenario/2".to_owned();
                s
            }),
            ("scenario_verifier_version_unsupported", {
                let mut s = scenario(vec![step("a", "GET", "/")]);
                s.verifier_version = 2;
                s
            }),
            ("scenario_step_count", scenario(vec![])),
            (
                "scenario_step_id_invalid",
                scenario(vec![step("Bad", "GET", "/")]),
            ),
            ("scenario_step_id_duplicate", {
                scenario(vec![step("a", "GET", "/"), step("a", "GET", "/x")])
            }),
            (
                "scenario_method_unsupported",
                scenario(vec![step("a", "TRACE", "/")]),
            ),
            (
                "scenario_path_invalid",
                scenario(vec![step("a", "GET", "https://example.com/")]),
            ),
            (
                "scenario_path_invalid",
                scenario(vec![step("a", "GET", "//example.com/")]),
            ),
            (
                "scenario_path_invalid",
                scenario(vec![step("a", "GET", "/x#frag")]),
            ),
            ("scenario_isolation_invalid", {
                let mut s = scenario(vec![step("a", "GET", "/")]);
                s.isolation.fresh_instance = false;
                s
            }),
            ("scenario_limits_invalid", {
                let mut s = scenario(vec![step("a", "GET", "/")]);
                s.limits.step_timeout_ms = MAX_STEP_TIMEOUT_MS + 1;
                s
            }),
            ("scenario_limits_invalid", {
                let mut s = scenario(vec![step("a", "GET", "/")]);
                s.limits.total_timeout_ms = s.limits.step_timeout_ms - 1;
                s
            }),
        ];
        for (code, invalid) in cases {
            assert_eq!(invalid.validate().unwrap_err().0, code, "{invalid:?}");
        }
    }

    #[test]
    fn request_rules_are_strict() {
        // Forbidden headers: the session belongs to a step, not a header.
        for name in FORBIDDEN_HEADERS {
            let mut s = step("a", "GET", "/");
            s.request.headers.insert((*name).to_owned(), "x".to_owned());
            assert_eq!(
                scenario(vec![s]).validate().unwrap_err().0,
                "scenario_header_forbidden",
                "{name}"
            );
        }
        // Uppercase header names are not the canonical spelling.
        let mut upper = step("a", "GET", "/");
        upper
            .request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());
        assert_eq!(
            scenario(vec![upper]).validate().unwrap_err().0,
            "scenario_header_invalid"
        );
        // GET and HEAD cannot carry a body.
        for method in ["GET", "HEAD"] {
            let mut s = step("a", method, "/");
            s.request.body_b64 = Some(String::new());
            assert_eq!(
                scenario(vec![s]).validate().unwrap_err().0,
                "scenario_body_invalid"
            );
        }
        // Invalid base64, non-UTF-8 body and oversized bodies are refused.
        let mut bad64 = step("a", "POST", "/");
        bad64.request.body_b64 = Some("!!!".to_owned());
        assert_eq!(
            scenario(vec![bad64]).validate().unwrap_err().0,
            "scenario_body_invalid"
        );
        let mut non_utf8 = step("a", "POST", "/");
        non_utf8.request.body_b64 =
            Some(base64::engine::general_purpose::STANDARD.encode([0xff_u8, 0xfe]));
        assert_eq!(
            scenario(vec![non_utf8]).validate().unwrap_err().0,
            "scenario_body_invalid"
        );
        let mut large = step("a", "POST", "/");
        large.request.body_b64 =
            Some(base64::engine::general_purpose::STANDARD.encode(vec![b'a'; 32]));
        let mut s = scenario(vec![large]);
        s.limits.max_body_bytes = 8;
        assert_eq!(s.validate().unwrap_err().0, "scenario_body_too_large");
        // Expectations are bounded and typed.
        let mut status = step("a", "GET", "/");
        status.expect.status = 99;
        assert_eq!(
            scenario(vec![status]).validate().unwrap_err().0,
            "scenario_expectation_invalid"
        );
        let mut digest = step("a", "GET", "/");
        digest.expect.body_digest = Some("sha256:ABC".to_owned());
        assert_eq!(
            scenario(vec![digest]).validate().unwrap_err().0,
            "scenario_body_digest_invalid"
        );
        let mut assertion = step("a", "GET", "/");
        assertion.expect.json.push(ScenarioJsonAssertionV1 {
            pointer: "/a".to_owned(),
            equals: None,
            exists: None,
        });
        assert_eq!(
            scenario(vec![assertion]).validate().unwrap_err().0,
            "scenario_json_assertion_invalid"
        );
        let mut both = step("a", "GET", "/");
        both.expect.json.push(ScenarioJsonAssertionV1 {
            pointer: "/a".to_owned(),
            equals: Some(serde_json::json!(1)),
            exists: Some(true),
        });
        assert_eq!(
            scenario(vec![both]).validate().unwrap_err().0,
            "scenario_json_assertion_invalid"
        );
    }

    #[test]
    fn captures_and_substitutions_are_order_sensitive() {
        // A placeholder before its capture is a plan error, not a runtime one.
        let mut early = step("early", "GET", "/items/${item}");
        early.capture.push(ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/id".to_owned(),
        });
        assert_eq!(
            scenario(vec![early]).validate().unwrap_err().0,
            "scenario_template_invalid"
        );
        // Duplicate capture names and malformed pointers are refused.
        let mut first = step("first", "GET", "/a");
        first.capture.push(ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/id".to_owned(),
        });
        let mut second = step("second", "GET", "/b");
        second.capture.push(ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/id".to_owned(),
        });
        assert_eq!(
            scenario(vec![first.clone(), second])
                .validate()
                .unwrap_err()
                .0,
            "scenario_capture_duplicate"
        );
        let mut bad_pointer = step("first", "GET", "/a");
        bad_pointer.capture.push(ScenarioCaptureV1 {
            name: "item".to_owned(),
            json_pointer: "/a~2".to_owned(),
        });
        assert_eq!(
            scenario(vec![bad_pointer]).validate().unwrap_err().0,
            "scenario_capture_invalid"
        );
        // A cleared-cookie step is the negative case and needs no capture.
        let mut cleared = step("unauthorized", "GET", "/items/1");
        cleared.request.cookies = Some(ScenarioCookiePolicy::Cleared);
        cleared.expect.status = 401;
        assert!(scenario(vec![first, cleared]).validate().is_ok());
    }

    #[test]
    fn pretty_printed_json_is_not_its_canonical_bytes() {
        let scenario = scenario(vec![step("a", "GET", "/")]);
        let pretty = serde_json::to_vec_pretty(&scenario).unwrap();
        assert_eq!(
            ScenarioV1::from_canonical_bytes(&pretty).unwrap_err().0,
            "scenario_not_canonical"
        );
        let canonical = scenario.canonical_bytes().unwrap();
        assert_eq!(ScenarioV1::parse_json(&pretty).unwrap(), scenario);
        assert_eq!(scenario.canonical_bytes().unwrap(), canonical);
        // Unknown fields are refused.
        let with_unknown = String::from_utf8(canonical)
            .unwrap()
            .replace('{', "{\"extra\":true,");
        assert_eq!(
            ScenarioV1::parse_json(with_unknown.as_bytes())
                .unwrap_err()
                .0,
            "scenario_malformed"
        );
    }
}
