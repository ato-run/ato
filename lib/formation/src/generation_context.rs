//! Bounded lexical evidence for generation, never source or execution authority.
//! This is not a Python parser: markers describe token presence, not behavior.
//! Callers read at most MAX_SOURCE_BYTES from a frozen regular file and supply
//! its full size separately. No filesystem, process or network access occurs here.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const CONTEXT_SCHEMA: &str = "ato.formation-generation-context/1";
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
pub const MAX_ENTRY_BYTES: usize = 1024;
pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;
pub const MAX_ENTRIES: usize = 16;
pub const MAX_FAILURES: usize = 16;
pub const MAX_INSPECTIONS: usize = 4;
pub const MAX_REASONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("generation_context_invalid")]
pub struct ContextError;

macro_rules! vocabulary {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
vocabulary!(Language { Python });
vocabulary!(SourceScan {
    Complete,
    Unavailable,
    TooLarge
});
vocabulary!(SizeBucket {
    Empty,
    UpTo1Kib,
    UpTo8Kib,
    UpTo64Kib,
    Over64Kib
});
vocabulary!(CountBucket {
    Zero,
    One,
    TwoToFour,
    FiveToSixteen,
    OverSixteen
});
vocabulary!(ImportMarker {
    HttpServer,
    Socketserver,
    Socket,
    Asyncio,
    Flask,
    Fastapi,
    Uvicorn,
    Aiohttp,
    Tornado,
    Wsgiref,
    Django
});
vocabulary!(FrameworkMarker {
    Flask,
    Fastapi,
    Uvicorn,
    Aiohttp,
    Tornado,
    Django
});
vocabulary!(FailureStatus {
    Fail,
    Inconclusive,
    Expired,
    Unknown
});
vocabulary!(EvidenceCode {
    Other,
    HttpStatusMismatch,
    HttpBodyMismatch,
    HttpContentTypeMismatch,
    HttpObservationFailed,
    HttpRequestFailed,
    ConnectionRefused,
    Timeout,
    ProcessExited,
    VerificationFailed,
    RuntimeUnavailable,
    RequirementMissing,
    CapabilityMissing,
    PlatformMismatch,
    NetworkDenied,
    BindingMissing,
    SearchTransferBudgetExceeded,
    SearchExpandedBudgetExceeded,
    SearchStoredBudgetExceeded,
    EnvironmentWithdrawn,
    RuntimeOffline,
    RuntimeUnhealthy,
    RuntimeRevoked,
    RuntimeDrained,
    NotAuthorized,
    ManagementPolicy,
    ExactRuntimeMismatch,
    RequirementUnmet,
    ProvisionNeedsNetwork,
    VerifierUnavailable,
    VerifierContainmentUnavailable,
    BindingUnavailable,
    EffectPolicy,
    CapacityExhausted
});
vocabulary!(InspectionKind {
    CandidateRefusals,
    AttemptFailures
});

fn count(n: usize) -> CountBucket {
    match n {
        0 => CountBucket::Zero,
        1 => CountBucket::One,
        2..=4 => CountBucket::TwoToFour,
        5..=16 => CountBucket::FiveToSixteen,
        _ => CountBucket::OverSixteen,
    }
}
fn size(n: u64) -> SizeBucket {
    match n {
        0 => SizeBucket::Empty,
        1..=1024 => SizeBucket::UpTo1Kib,
        1025..=8192 => SizeBucket::UpTo8Kib,
        8193..=65536 => SizeBucket::UpTo64Kib,
        _ => SizeBucket::Over64Kib,
    }
}
fn logical_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id != "none"
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn sorted_unique<T: Ord>(items: &[T]) -> bool {
    items.windows(2).all(|p| p[0] < p[1])
}
fn fits<T: Serialize>(value: &T, limit: usize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= limit)
}

// Deserialization enforces the same invariants as public constructors. Public
// fields are convenient for callers; validate again before crossing a boundary.
macro_rules! checked_struct {
    ($name:ident { $($field:ident: $ty:ty),+ $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        pub struct $name { $(pub $field: $ty),+ }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Wire { $($field: $ty),+ }
                let v = Wire::deserialize(d)?;
                let result = Self { $($field: v.$field),+ };
                result.validate().map_err(|_| serde::de::Error::custom("generation_context_invalid"))?;
                Ok(result)
            }
        }
    };
}
checked_struct!(EntryPointSummary {
    id: String, language: Language, size_bucket: SizeBucket, source_scan: SourceScan,
    imports: Vec<ImportMarker>, frameworks: Vec<FrameworkMarker>,
    functions: CountBucket, classes: CountBucket, main_guard: bool,
    server_listen: bool, custom_http_handler: bool
});
impl EntryPointSummary {
    pub fn validate(&self) -> Result<(), ContextError> {
        if !logical_id(&self.id)
            || !sorted_unique(&self.imports)
            || !sorted_unique(&self.frameworks)
            || self.imports.len() > 11
            || self.frameworks.len() > 6
            || !fits(self, MAX_ENTRY_BYTES)
            || (self.source_scan == SourceScan::TooLarge)
                != (self.size_bucket == SizeBucket::Over64Kib)
            || (self.source_scan != SourceScan::Complete
                && (!self.imports.is_empty()
                    || !self.frameworks.is_empty()
                    || self.functions != CountBucket::Zero
                    || self.classes != CountBucket::Zero
                    || self.main_guard
                    || self.server_listen
                    || self.custom_http_handler))
        {
            return Err(ContextError);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSummary {
    pub python: bool,
    pub node: bool,
    pub manifest: bool,
    pub lockfile: bool,
    pub readme: bool,
    pub static_html: bool,
    pub regular_files: CountBucket,
    pub python_files: CountBucket,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureSummary {
    pub status: FailureStatus,
    pub code: EvidenceCode,
}
checked_struct!(InspectionSummary {
    kind: InspectionKind, reasons: Vec<EvidenceCode>, failures: Vec<FailureSummary>
});
impl InspectionSummary {
    pub fn validate(&self) -> Result<(), ContextError> {
        if self.reasons.len() > MAX_REASONS
            || self.failures.len() > MAX_FAILURES
            || !sorted_unique(&self.reasons)
            || (self.kind == InspectionKind::CandidateRefusals && !self.failures.is_empty())
            || (self.kind == InspectionKind::AttemptFailures && !self.reasons.is_empty())
        {
            return Err(ContextError);
        }
        Ok(())
    }
}
checked_struct!(GenerationContext {
    schema: String, entrypoints: Vec<EntryPointSummary>, project_summary: ProjectSummary,
    failures: Vec<FailureSummary>, inspections: Vec<InspectionSummary>
});
impl GenerationContext {
    pub fn new(
        mut entrypoints: Vec<EntryPointSummary>,
        project_summary: ProjectSummary,
        failures: Vec<FailureSummary>,
        inspections: Vec<InspectionSummary>,
    ) -> Result<Self, ContextError> {
        entrypoints.sort_by(|a, b| a.id.cmp(&b.id));
        let result = Self {
            schema: CONTEXT_SCHEMA.into(),
            entrypoints,
            project_summary,
            failures,
            inspections,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<(), ContextError> {
        if self.schema != CONTEXT_SCHEMA
            || self.entrypoints.len() > MAX_ENTRIES
            || self.failures.len() > MAX_FAILURES
            || self.inspections.len() > MAX_INSPECTIONS
            || !self.entrypoints.windows(2).all(|p| p[0].id < p[1].id)
            || !fits(self, MAX_CONTEXT_BYTES)
        {
            return Err(ContextError);
        }
        for entry in &self.entrypoints {
            entry.validate()?;
        }
        for inspection in &self.inspections {
            inspection.validate()?;
        }
        Ok(())
    }
    /// Bound raw input before deserialization (including whitespace/unknown fields).
    pub fn from_json(bytes: &[u8]) -> Result<Self, ContextError> {
        if bytes.len() > MAX_CONTEXT_BYTES {
            return Err(ContextError);
        }
        serde_json::from_slice(bytes).map_err(|_| ContextError)
    }
}

/// Presence-only projection. Version strings, dependency names, scripts,
/// module names and candidate output paths are deliberately never read.
#[cfg(feature = "planning")]
pub fn project_project(
    e: &crate::detect::DetectorEvidence,
    files: &BTreeSet<String>,
) -> ProjectSummary {
    let p = e.python.as_ref();
    let n = e.node.as_ref();
    let present = |name: &str| files.contains(name) || e.present_files.iter().any(|f| f == name);
    let python_files = files.iter().filter(|f| f.ends_with(".py")).count();
    ProjectSummary {
        python: p.is_some() || python_files > 0,
        node: n.is_some()
            || present("package.json")
            || e.static_web.as_ref().is_some_and(|s| s.has_package_json),
        manifest: [
            "capsule.toml",
            "pyproject.toml",
            "package.json",
            "requirements.txt",
            "Pipfile",
        ]
        .iter()
        .any(|f| present(f))
            || p.is_some_and(|p| p.has_pyproject || p.has_requirements_txt)
            || n.is_some_and(|n| n.has_package_json)
            || e.static_web.as_ref().is_some_and(|s| s.has_package_json),
        lockfile: [
            "uv.lock",
            "poetry.lock",
            "Pipfile.lock",
            "package-lock.json",
            "npm-shrinkwrap.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        ]
        .iter()
        .any(|f| present(f))
            || p.is_some_and(|p| p.has_uv_lock || p.has_poetry_lock || p.has_pipfile_lock)
            || n.is_some_and(|n| {
                n.has_package_lock
                    || n.has_npm_shrinkwrap
                    || n.has_pnpm_lock
                    || n.has_yarn_lock
                    || n.has_bun_lock
            }),
        readme: files.iter().chain(&e.present_files).any(|f| {
            matches!(
                f.to_ascii_lowercase().as_str(),
                "readme" | "readme.md" | "readme.rst" | "readme.txt"
            )
        }),
        static_html: present("index.html")
            || e.static_web.as_ref().is_some_and(|s| s.has_root_index_html),
        regular_files: count(files.len()),
        python_files: count(python_files),
    }
}

fn code(value: Option<&Value>) -> EvidenceCode {
    // Only enum strings are examined. Unknown strings become a fixed marker,
    // never a trimmed, hashed or otherwise transformed fragment of raw data.
    value
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 64)
        .and_then(|s| serde_json::from_value(Value::String(s.into())).ok())
        .unwrap_or(EvidenceCode::Other)
}
fn failure(value: &Value) -> Option<FailureSummary> {
    let status = value.get("status")?.as_str()?;
    if status.len() > 16 {
        return None;
    }
    let status = serde_json::from_value(Value::String(status.into())).ok()?;
    Some(FailureSummary {
        status,
        code: code(value.get("failure_code")),
    })
}
/// Input is an array in durable chronological order. Keep the most recent
/// eligible records, preserving that order; never sort by attacker-controlled IDs.
pub fn project_failures(value: &Value) -> Vec<FailureSummary> {
    let Some(items) = value.as_array() else {
        return vec![];
    };
    let mut out: Vec<_> = items
        .iter()
        .rev()
        .filter_map(failure)
        .take(MAX_FAILURES)
        .collect();
    out.reverse();
    out
}
pub fn project_inspections(value: &Value) -> Vec<InspectionSummary> {
    let Some(items) = value.as_array() else {
        return vec![];
    };
    let mut out: Vec<_> = items
        .iter()
        .rev()
        .filter_map(|item| {
            let kind = match item.get("kind")?.as_str()? {
                "candidate_refusals" => InspectionKind::CandidateRefusals,
                "attempt_failures" => InspectionKind::AttemptFailures,
                _ => return None,
            };
            // Durable inspection rows wrap payloads in `result`; the API's
            // privacy-projected generationInput uses the same fields flat.
            // If a wrapper is present it owns the payload (never merge shapes).
            let result = item.get("result").unwrap_or(item);
            let mut reasons = BTreeSet::new();
            let mut failures = vec![];
            match kind {
                InspectionKind::CandidateRefusals => {
                    if let Some(refusals) = result.get("refusals").and_then(Value::as_array) {
                        for reason in refusals
                            .iter()
                            .rev()
                            .filter_map(|r| r.get("reasons").and_then(Value::as_array))
                            .flat_map(|r| r.iter().rev())
                            .take(MAX_REASONS)
                        {
                            reasons.insert(code(reason.get("code")));
                        }
                    }
                }
                InspectionKind::AttemptFailures => {
                    if let Some(value) = result.get("failures") {
                        failures = project_failures(value);
                    }
                }
            }
            Some(InspectionSummary {
                kind,
                reasons: reasons.into_iter().collect(),
                failures,
            })
        })
        .take(MAX_INSPECTIONS)
        .collect();
    out.reverse();
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a str),
    Symbol(u8),
    MainLiteral,
    Literal,
    End,
}
fn is_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 128
}
fn prefix(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "r" | "u" | "b" | "f" | "br" | "rb" | "fr" | "rf"
    )
}

// Recursive only for strings inside f-string expressions, capped independently
// of the 64KiB input limit. No interpolation expression becomes an output token.
fn skip_string(b: &[u8], start: usize, formatted: bool, depth: usize) -> Option<usize> {
    if depth > 16 {
        return None;
    }
    let quote = *b.get(start)?;
    let triple = b.get(start..start + 3).is_some_and(|s| s == [quote; 3]);
    let width = if triple { 3 } else { 1 };
    let mut i = start + width;
    let mut braces = 0usize;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if braces == 0 {
            if b[i] == quote && (!triple || b.get(i..i + 3).is_some_and(|s| s == [quote; 3])) {
                return Some(i + width);
            }
            if !triple && matches!(b[i], b'\n' | b'\r') {
                return None;
            }
            if formatted && matches!(b[i], b'{' | b'}') {
                if b.get(i + 1) == Some(&b[i]) {
                    i += 2;
                    continue;
                }
                if b[i] == b'}' {
                    return None;
                }
                braces = 1;
            }
        } else {
            match b[i] {
                b'\'' | b'"' => {
                    let mut p = i;
                    while p > start && b[p - 1].is_ascii_alphabetic() {
                        p -= 1;
                    }
                    let nested_f = b[p..i].iter().any(|c| matches!(c, b'f' | b'F'));
                    i = skip_string(b, i, nested_f, depth + 1)?;
                    continue;
                }
                b'{' => braces += 1,
                b'}' => braces -= 1,
                b'#' => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}
fn lex(source: &str) -> Option<Vec<Token<'_>>> {
    let b = source.as_bytes();
    let mut out = vec![];
    let mut stack = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\r' | 12 => i += 1,
            b'\n' | b';' => {
                out.push(Token::End);
                i += 1;
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' if b.get(i + 1) == Some(&b'\n') => i += 2,
            b'\'' | b'"' => {
                let end = skip_string(b, i, false, 0)?;
                let main = &source[i..end] == "\"__main__\"" || &source[i..end] == "'__main__'";
                out.push(if main {
                    Token::MainLiteral
                } else {
                    Token::Literal
                });
                i = end;
            }
            c if is_name(c) => {
                let start = i;
                while i < b.len() && is_name(b[i]) {
                    i += 1;
                }
                let word = &source[start..i];
                if prefix(word) && b.get(i).is_some_and(|c| matches!(c, b'\'' | b'"')) {
                    i = skip_string(b, i, word.bytes().any(|c| matches!(c, b'f' | b'F')), 0)?;
                    out.push(Token::Literal);
                } else {
                    out.push(Token::Name(word));
                }
            }
            c @ (b'(' | b'[' | b'{') => {
                stack.push(c);
                out.push(Token::Symbol(c));
                i += 1;
            }
            c @ (b')' | b']' | b'}') => {
                let expected = match c {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if stack.pop() != Some(expected) {
                    return None;
                }
                out.push(Token::Symbol(c));
                i += 1;
            }
            c if b".:,=+-*/%@<>!&|^~".contains(&c) => {
                out.push(Token::Symbol(c));
                i += 1;
            }
            _ => return None,
        }
    }
    stack.is_empty().then_some(out)
}
fn module_marker(tokens: &[Token<'_>]) -> Option<ImportMarker> {
    let Token::Name(name) = tokens.first()? else {
        return None;
    };
    Some(match *name {
        "http" if tokens.get(1..3) == Some(&[Token::Symbol(b'.'), Token::Name("server")]) => {
            ImportMarker::HttpServer
        }
        "socketserver" => ImportMarker::Socketserver,
        "socket" => ImportMarker::Socket,
        "asyncio" => ImportMarker::Asyncio,
        "flask" => ImportMarker::Flask,
        "fastapi" => ImportMarker::Fastapi,
        "uvicorn" => ImportMarker::Uvicorn,
        "aiohttp" => ImportMarker::Aiohttp,
        "tornado" => ImportMarker::Tornado,
        "wsgiref" => ImportMarker::Wsgiref,
        "django" => ImportMarker::Django,
        _ => return None,
    })
}

pub fn project_python(
    id: &str,
    bytes: &[u8],
    full_size: u64,
) -> Result<EntryPointSummary, ContextError> {
    if !logical_id(id) {
        return Err(ContextError);
    }
    let mut result = EntryPointSummary {
        id: id.into(),
        language: Language::Python,
        size_bucket: size(full_size),
        source_scan: SourceScan::Unavailable,
        imports: vec![],
        frameworks: vec![],
        functions: CountBucket::Zero,
        classes: CountBucket::Zero,
        main_guard: false,
        server_listen: false,
        custom_http_handler: false,
    };
    if full_size > MAX_SOURCE_BYTES as u64 {
        result.source_scan = SourceScan::TooLarge;
        return Ok(result);
    }
    if bytes.len() > MAX_SOURCE_BYTES || bytes.len() as u64 != full_size {
        return Ok(result);
    }
    let Some(tokens) = std::str::from_utf8(bytes).ok().and_then(lex) else {
        return Ok(result);
    };
    result.source_scan = SourceScan::Complete;
    let mut imports = BTreeSet::new();
    for statement in tokens.split(|t| *t == Token::End) {
        // Import statements can occur in a one-line suite following a colon.
        for (i, token) in statement.iter().enumerate() {
            if *token == Token::Name("from") {
                if let Some(marker) = module_marker(&statement[i + 1..]) {
                    imports.insert(marker);
                }
                break;
            }
            if *token == Token::Name("import") {
                for module in statement[i + 1..].split(|t| *t == Token::Symbol(b',')) {
                    if let Some(marker) = module_marker(module) {
                        imports.insert(marker);
                    }
                }
                break;
            }
        }
    }
    let mut functions = 0;
    let mut classes = 0;
    for (i, t) in tokens.iter().enumerate() {
        if *t == Token::Name("def") {
            functions += 1;
        }
        if *t == Token::Name("class") {
            classes += 1;
            let header = tokens[i + 1..]
                .iter()
                // Bound lookahead even on malformed repeated class keywords.
                .take(128)
                .take_while(|t| !matches!(t, Token::End | Token::Symbol(b':')));
            if header
                .skip_while(|t| **t != Token::Symbol(b'('))
                .skip(1)
                .any(|t| {
                    matches!(
                        t,
                        Token::Name("BaseHTTPRequestHandler" | "SimpleHTTPRequestHandler")
                    )
                })
            {
                result.custom_http_handler = true;
            }
        }
        if matches!(
            t,
            Token::Name(
                "serve_forever"
                    | "listen"
                    | "start_server"
                    | "run_app"
                    | "HTTPServer"
                    | "ThreadingHTTPServer"
                    | "TCPServer"
            )
        ) && tokens.get(i + 1) == Some(&Token::Symbol(b'('))
            && !matches!(
                i.checked_sub(1).and_then(|j| tokens.get(j)),
                Some(Token::Name("def" | "class"))
            )
        {
            result.server_listen = true;
        }
    }
    result.main_guard = tokens.windows(6).any(|w| {
        matches!(
            w,
            [
                Token::Name("if"),
                Token::Name("__name__"),
                Token::Symbol(b'='),
                Token::Symbol(b'='),
                Token::MainLiteral,
                Token::Symbol(b':')
            ] | [
                Token::Name("if"),
                Token::MainLiteral,
                Token::Symbol(b'='),
                Token::Symbol(b'='),
                Token::Name("__name__"),
                Token::Symbol(b':')
            ]
        )
    });
    result.functions = count(functions);
    result.classes = count(classes);
    result.frameworks = imports
        .iter()
        .filter_map(|m| {
            Some(match m {
                ImportMarker::Flask => FrameworkMarker::Flask,
                ImportMarker::Fastapi => FrameworkMarker::Fastapi,
                ImportMarker::Uvicorn => FrameworkMarker::Uvicorn,
                ImportMarker::Aiohttp => FrameworkMarker::Aiohttp,
                ImportMarker::Tornado => FrameworkMarker::Tornado,
                ImportMarker::Django => FrameworkMarker::Django,
                _ => return None,
            })
        })
        .collect();
    result.imports = imports.into_iter().collect();
    result.validate()?;
    Ok(result)
}
