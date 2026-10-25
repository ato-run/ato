//! Offline information recovery. Not a provider prompt or execution authority.
use super::*;

pub const CONTEXT_SCHEMA: &str = "ato.formation-generation-context/2";
vocabulary!(SourceScan {
    Complete,
    BoundedPrefix,
    Unavailable
});
vocabulary!(Encoding {
    Utf8,
    Latin1,
    Unsupported
});
vocabulary!(Delegation { None, PythonMain });
vocabulary!(RecoveryCode {
    CandidateNotObservable,
    FormationFailed,
    CandidateStopUnconfirmed,
    CandidateCleanupFailed
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EvidenceCode {
    Existing(super::EvidenceCode),
    Recovery(RecoveryCode),
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
checked_struct!(EntryPointSummary {
    id: String, language: Language, size_bucket: SizeBucket, source_scan: SourceScan,
    encoding: Encoding, delegation: Delegation,
    imports: Vec<ImportMarker>, frameworks: Vec<FrameworkMarker>,
    functions: CountBucket, classes: CountBucket, main_guard: bool,
    server_listen: bool, custom_http_handler: bool
});
impl EntryPointSummary {
    pub fn validate(&self) -> Result<(), ContextError> {
        let unavailable = self.source_scan == SourceScan::Unavailable;
        if !logical_id(&self.id)
            || !fits(self, MAX_ENTRY_BYTES)
            || !sorted_unique(&self.imports)
            || !sorted_unique(&self.frameworks)
            || self.imports.len() > 11
            || self.frameworks.len() > 6
            || (self.source_scan == SourceScan::Complete
                && self.size_bucket == SizeBucket::Over64Kib)
            || (self.source_scan == SourceScan::BoundedPrefix
                && self.size_bucket != SizeBucket::Over64Kib)
            || (self.encoding == Encoding::Unsupported && !unavailable)
            || (unavailable
                && (!self.imports.is_empty()
                    || !self.frameworks.is_empty()
                    || self.functions != CountBucket::Zero
                    || self.classes != CountBucket::Zero
                    || self.main_guard
                    || self.server_listen
                    || self.custom_http_handler
                    || self.delegation != Delegation::None))
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
    pub fn from_json(bytes: &[u8]) -> Result<Self, ContextError> {
        if bytes.len() > MAX_CONTEXT_BYTES {
            return Err(ContextError);
        }
        serde_json::from_slice(bytes).map_err(|_| ContextError)
    }
}

/// Only the first two physical lines can declare encoding. An oversized header,
/// malformed cookie, BOM conflict or unsupported codec fails closed. No codec lookup.
fn encoding(bytes: &[u8]) -> Encoding {
    let bom = bytes.starts_with(b"\xef\xbb\xbf");
    let bytes = if bom { &bytes[3..] } else { bytes };
    let mut found = None;
    for line in bytes.split(|b| *b == b'\n').take(2) {
        if line.len() > 1024 {
            return Encoding::Unsupported;
        }
        let line = line.trim_ascii_start();
        if !line.starts_with(b"#") {
            if !line.is_empty() {
                break;
            }
            continue;
        }
        // Ordinary prose mentioning coding is not an encoding declaration.
        // Search for the delimiter as well, so prose cannot hide a later cookie.
        let Some(at) = line
            .windows(7)
            .position(|w| w == b"coding:" || w == b"coding=")
        else {
            continue;
        };
        let tail = line[at + 7..].trim_ascii_start();
        let end = tail
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || b"-_.".contains(b)))
            .unwrap_or(tail.len());
        let codec = match &tail[..end] {
            b"utf-8" | b"utf8" => Encoding::Utf8,
            b"latin-1" | b"latin1" | b"iso-8859-1" => Encoding::Latin1,
            _ => return Encoding::Unsupported,
        };
        if found.is_some_and(|previous| previous != codec) || (bom && codec != Encoding::Utf8) {
            return Encoding::Unsupported;
        }
        found = Some(codec);
    }
    found.unwrap_or(Encoding::Utf8)
}

/// Intentionally narrow, unaliased call with a literal target and the exact
/// run_name keyword. Only token structure survives, never the target literal.
fn delegation(tokens: &[Token<'_>]) -> Delegation {
    let imported = tokens
        .split(|t| *t == Token::End)
        .any(|s| s == [Token::Name("import"), Token::Name("runpy")]);
    let call = tokens.split(|t| *t == Token::End).any(|w| {
        w == [
            Token::Name("runpy"),
            Token::Symbol(b'.'),
            Token::Name("run_path"),
            Token::Symbol(b'('),
            Token::Literal,
            Token::Symbol(b','),
            Token::Name("run_name"),
            Token::Symbol(b'='),
            Token::MainLiteral,
            Token::Symbol(b')'),
        ]
    });
    if imported && call {
        Delegation::PythonMain
    } else {
        Delegation::None
    }
}

/// `bytes` must be the exact full file or its first 64 KiB. Only a lexically
/// closed prefix ending at a physical line boundary (or in a comment) is used.
/// Prefix facts are hints; unseen suffixes may change all behavior.
pub fn project_python(
    id: &str,
    bytes: &[u8],
    full_size: u64,
) -> Result<EntryPointSummary, ContextError> {
    let mut base = super::project_python(id, b"", 0)?;
    let mut result = EntryPointSummary {
        id: id.into(),
        language: Language::Python,
        size_bucket: size(full_size),
        source_scan: SourceScan::Unavailable,
        encoding: Encoding::Unsupported,
        delegation: Delegation::None,
        imports: vec![],
        frameworks: vec![],
        functions: CountBucket::Zero,
        classes: CountBucket::Zero,
        main_guard: false,
        server_listen: false,
        custom_http_handler: false,
    };
    if bytes.len() != full_size.min(MAX_SOURCE_BYTES as u64) as usize {
        return Ok(result);
    }
    result.encoding = encoding(bytes);
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let decoded = match result.encoding {
        Encoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => return Ok(result),
        },
        Encoding::Latin1 => bytes.iter().map(|b| char::from(*b)).collect(),
        Encoding::Unsupported => return Ok(result),
    };
    let Some(tokens) = lex(&decoded) else {
        return Ok(result);
    };
    let prefix = full_size > MAX_SOURCE_BYTES as u64;
    if prefix && decoded.ends_with("\\\n") {
        return Ok(result);
    }
    if prefix && !decoded.ends_with('\n') {
        // Require that the final physical line is purely a comment. This avoids
        // accepting a cut inside a name, operator, continuation or inline literal.
        let tail = decoded.rsplit('\n').next().unwrap_or("").trim_start();
        if !tail.starts_with('#') {
            return Ok(result);
        }
    }
    summarize_tokens(&tokens, &mut base);
    result.source_scan = if prefix {
        SourceScan::BoundedPrefix
    } else {
        SourceScan::Complete
    };
    result.delegation = delegation(&tokens);
    result.imports = base.imports;
    result.frameworks = base.frameworks;
    result.functions = base.functions;
    result.classes = base.classes;
    result.main_guard = base.main_guard;
    result.server_listen = base.server_listen;
    result.custom_http_handler = base.custom_http_handler;
    result.validate()?;
    Ok(result)
}

fn normalize(value: Option<&Value>) -> EvidenceCode {
    match value.and_then(Value::as_str) {
        Some("candidate_not_observable") => {
            EvidenceCode::Recovery(RecoveryCode::CandidateNotObservable)
        }
        Some("formation_failed") => EvidenceCode::Recovery(RecoveryCode::FormationFailed),
        Some("candidate_stop_unconfirmed") => {
            EvidenceCode::Recovery(RecoveryCode::CandidateStopUnconfirmed)
        }
        Some("candidate_cleanup_failed") => {
            EvidenceCode::Recovery(RecoveryCode::CandidateCleanupFailed)
        }
        _ => EvidenceCode::Existing(super::code(value)),
    }
}
pub fn project_failures(value: &Value) -> Vec<FailureSummary> {
    let Some(items) = value.as_array() else {
        return vec![];
    };
    let mut out: Vec<_> = items
        .iter()
        .rev()
        .filter_map(|item| {
            let old = super::failure(item)?;
            Some(FailureSummary {
                status: old.status,
                code: normalize(item.get("failure_code")),
            })
        })
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
            let result = item.get("result").unwrap_or(item);
            let mut reasons = BTreeSet::new();
            if kind == InspectionKind::CandidateRefusals
                && let Some(refusals) = result.get("refusals").and_then(Value::as_array)
            {
                for reason in refusals
                    .iter()
                    .rev()
                    .filter_map(|r| r.get("reasons").and_then(Value::as_array))
                    .flat_map(|r| r.iter().rev())
                    .take(MAX_REASONS)
                {
                    reasons.insert(normalize(reason.get("code")));
                }
            }
            Some(InspectionSummary {
                kind,
                reasons: reasons.into_iter().collect(),
                failures: if kind == InspectionKind::AttemptFailures {
                    project_failures(&result["failures"])
                } else {
                    vec![]
                },
            })
        })
        .take(MAX_INSPECTIONS)
        .collect();
    out.reverse();
    out
}
