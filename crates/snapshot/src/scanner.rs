//! Byte-level no-secret scanner for sealed layers (plan §8.1).
//!
//! Replaces the placeholder declared-marker search with a deterministic,
//! dependency-light, single-std-pass scanner over the raw bytes of **every**
//! layer (rootfs/runtime/dependency/app/vmstate/memory). It flags three things:
//!
//! 1. caller-**declared** secret markers (verbatim — drives the legacy
//!    fail-closed path unchanged),
//! 2. known **provider-key prefixes** (`sk-`, `ghp_`, `AKIA`, …) followed by a
//!    token run,
//! 3. high-**entropy** base64/hex token runs (windowed Shannon entropy).
//!
//! Findings carry `layer + offset + len + kind + a non-leaking detail label`
//! (the prefix family, the env NAME, or an entropy figure) — **never** the
//! secret value. The pattern tables mirror capsule's private redaction tables
//! (`SECRET_VALUE_MARKERS` in `placement_index/publisher.rs`,
//! `SENSITIVE_ENV_MARKERS`/`looks_like_secret_value` in
//! `installed_state/launch_input.rs`, and cli's `leip` prefix set) with
//! provenance comments — they are not importable (`cli` depends on `capsule`,
//! `snapshot` cannot depend on `cli`), so no new crate dependency is taken.

/// Scanner version stamped into [`crate::manifest::NoSecretProof`].
pub const SCANNER_VERSION: &str = "ato-rs-scan/0.2.0";

// ── tunable thresholds ─────────────────────────────────────────────────────
const MIN_PROVIDER_SUFFIX_LEN: usize = 8;
const MIN_ENV_VALUE_LEN: usize = 6;
const MIN_ENTROPY_RUN_LEN: usize = 24;
const MIN_ENTROPY_DISTINCT: usize = 12;
const MIN_ENTROPY_CLASS_COUNT: usize = 2;
const ENTROPY_BITS_THRESHOLD: f64 = 3.5;

/// Known provider-key prefixes (mirrors cli `leip` + capsule publisher tables).
const PROVIDER_KEY_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", "AKIA", "ASIA", "xoxb-", "xoxp-",
    "AIza", "ya29.", "glpat-",
];

/// Env-NAME substrings that mark a value as sensitive (mirrors capsule
/// `SENSITIVE_ENV_MARKERS`). Matched ASCII-uppercased.
const SENSITIVE_ENV_MARKERS: &[&str] = &[
    "KEY", "SECRET", "TOKEN", "PASSWORD", "PASSWD", "CREDENTIAL", "PRIVATE", "ACCESS",
];

/// What a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// A known provider-key prefix followed by a token run.
    ProviderKeyPrefix,
    /// A secret-named env assignment (`NAME=value`).
    EnvAssignment,
    /// A high-entropy base64/hex token run.
    HighEntropyToken,
}

impl FindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingKind::ProviderKeyPrefix => "provider-key",
            FindingKind::EnvAssignment => "env",
            FindingKind::HighEntropyToken => "high-entropy",
        }
    }
}

/// One heuristic finding. Carries a position span and a non-leaking label —
/// never the secret bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Layer name (matches [`crate::manifest::ReadyStateLayers::iter`]).
    pub layer: &'static str,
    /// Byte offset of the flagged run within that layer.
    pub offset: usize,
    /// Length in bytes of the flagged run (a span, not the value).
    pub len: usize,
    pub kind: FindingKind,
    /// Non-leaking label: the prefix family (`"sk-"`), the env NAME
    /// (`"OPENAI_API_KEY"`), or an entropy figure (`"base64 entropy=5.31"`).
    pub detail: String,
}

impl SecretFinding {
    /// `"<layer>@<offset>+<len>:<kind>:<detail>"` — never includes the value.
    pub fn summary(&self) -> String {
        format!(
            "{}@{}+{}:{}:{}",
            self.layer,
            self.offset,
            self.len,
            self.kind.as_str(),
            self.detail
        )
    }
}

/// Result of scanning all layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    /// Verbatim caller-declared markers found (the caller already holds these).
    pub declared_hits: Vec<String>,
    /// Heuristic findings (never contain a raw secret value).
    pub heuristic: Vec<SecretFinding>,
}

impl ScanReport {
    /// **Blocking** heuristic findings — provider-key prefixes and secret-named
    /// env assignments. These are high-precision, so the build fails closed on
    /// them (alongside any [`declared_hits`](Self::declared_hits)).
    pub fn blocking(&self) -> Vec<&SecretFinding> {
        self.heuristic
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    FindingKind::ProviderKeyPrefix | FindingKind::EnvAssignment
                )
            })
            .collect()
    }

    /// **Advisory** heuristic findings — high-entropy token runs. These
    /// false-positive on lockfile integrity hashes, minified assets, and binary
    /// blobs in real dependency/app layers, so they are reported (not gating):
    /// the build does NOT fail on them.
    pub fn advisory(&self) -> Vec<&SecretFinding> {
        self.heuristic
            .iter()
            .filter(|f| matches!(f.kind, FindingKind::HighEntropyToken))
            .collect()
    }
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Shannon entropy in bits/byte over a 256-bucket histogram.
fn shannon_bits(run: &[u8]) -> f64 {
    if run.is_empty() {
        return 0.0;
    }
    let mut hist = [0usize; 256];
    for &b in run {
        hist[b as usize] += 1;
    }
    let len = run.len() as f64;
    let mut bits = 0.0;
    for &count in hist.iter() {
        if count > 0 {
            let p = count as f64 / len;
            bits -= p * p.log2();
        }
    }
    bits
}

fn distinct_count(run: &[u8]) -> usize {
    let mut seen = [false; 256];
    let mut n = 0;
    for &b in run {
        if !seen[b as usize] {
            seen[b as usize] = true;
            n += 1;
        }
    }
    n
}

/// Count how many of {lowercase, uppercase, digit} appear in the run.
fn class_count(run: &[u8]) -> usize {
    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    for &b in run {
        if b.is_ascii_lowercase() {
            lower = true;
        } else if b.is_ascii_uppercase() {
            upper = true;
        } else if b.is_ascii_digit() {
            digit = true;
        }
    }
    lower as usize + upper as usize + digit as usize
}

/// Layer views in the canonical order used by `ReadyStateLayers::iter` and
/// `fake.rs`.
fn layer_views(layers: &crate::backend::BuildLayers) -> [(&'static str, &[u8]); 6] {
    [
        ("rootfs", &layers.rootfs[..]),
        ("runtime", layers.runtime.as_deref().unwrap_or(&[])),
        ("dependency", layers.dependency.as_deref().unwrap_or(&[])),
        ("app", layers.app.as_deref().unwrap_or(&[])),
        ("vmstate", &layers.vmstate[..]),
        ("memory", &layers.memory[..]),
    ]
}

fn scan_declared(views: &[(&'static str, &[u8])], markers: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for marker in markers {
        let needle = marker.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let found = views
            .iter()
            .any(|(_, bytes)| bytes.windows(needle.len()).any(|w| w == needle));
        if found && !hits.contains(marker) {
            hits.push(marker.clone());
        }
    }
    hits
}

fn overlaps(findings: &[SecretFinding], start: usize, end: usize) -> bool {
    findings
        .iter()
        .any(|f| start < f.offset + f.len && f.offset < end)
}

fn scan_provider_prefixes(layer: &'static str, bytes: &[u8], out: &mut Vec<SecretFinding>) {
    for prefix in PROVIDER_KEY_PREFIXES {
        let pb = prefix.as_bytes();
        if pb.is_empty() || pb.len() > bytes.len() {
            continue;
        }
        let mut i = 0;
        while i + pb.len() <= bytes.len() {
            if &bytes[i..i + pb.len()] == pb {
                let mut j = i + pb.len();
                while j < bytes.len() && is_token_byte(bytes[j]) {
                    j += 1;
                }
                let suffix_len = j - (i + pb.len());
                if suffix_len >= MIN_PROVIDER_SUFFIX_LEN {
                    out.push(SecretFinding {
                        layer,
                        offset: i,
                        len: j - i,
                        kind: FindingKind::ProviderKeyPrefix,
                        detail: (*prefix).to_string(),
                    });
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

fn scan_env_assignments(layer: &'static str, bytes: &[u8], out: &mut Vec<SecretFinding>) {
    let n = bytes.len();
    for eq in 0..n {
        if bytes[eq] != b'=' {
            continue;
        }
        // Walk back over the NAME (identifier bytes).
        let mut name_start = eq;
        while name_start > 0 && is_ident_byte(bytes[name_start - 1]) {
            name_start -= 1;
        }
        if name_start == eq {
            continue; // no NAME
        }
        let name = &bytes[name_start..eq];
        let name_upper: String = name
            .iter()
            .map(|&b| (b as char).to_ascii_uppercase())
            .collect();
        let sensitive = SENSITIVE_ENV_MARKERS
            .iter()
            .any(|m| name_upper.contains(m));
        if !sensitive {
            continue;
        }
        // Walk forward over the VALUE (token bytes).
        let mut val_end = eq + 1;
        while val_end < n && is_token_byte(bytes[val_end]) {
            val_end += 1;
        }
        let value_len = val_end - (eq + 1);
        if value_len >= MIN_ENV_VALUE_LEN {
            out.push(SecretFinding {
                layer,
                offset: name_start,
                len: val_end - name_start,
                kind: FindingKind::EnvAssignment,
                detail: String::from_utf8_lossy(name).into_owned(),
            });
        }
    }
}

fn scan_entropy_runs(layer: &'static str, bytes: &[u8], out: &mut Vec<SecretFinding>) {
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if !is_token_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && is_token_byte(bytes[i]) {
            i += 1;
        }
        let run = &bytes[start..i];
        if run.len() < MIN_ENTROPY_RUN_LEN {
            continue;
        }
        // Skip runs already covered by a provider/env finding (no double-flag).
        if overlaps(out, start, i) {
            continue;
        }
        if distinct_count(run) >= MIN_ENTROPY_DISTINCT && class_count(run) >= MIN_ENTROPY_CLASS_COUNT
        {
            let bits = shannon_bits(run);
            if bits >= ENTROPY_BITS_THRESHOLD {
                out.push(SecretFinding {
                    layer,
                    offset: start,
                    len: run.len(),
                    kind: FindingKind::HighEntropyToken,
                    detail: format!("base64 entropy={bits:.2}"),
                });
            }
        }
    }
}

fn scan_layer_heuristics(layer: &'static str, bytes: &[u8]) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    scan_provider_prefixes(layer, bytes, &mut out);
    scan_env_assignments(layer, bytes, &mut out);
    scan_entropy_runs(layer, bytes, &mut out);
    out
}

/// Scan all build layers. Declared markers are matched verbatim; the heuristic
/// passes run independently. Deterministic and single-pass per layer.
pub fn scan_build_layers(
    layers: &crate::backend::BuildLayers,
    declared_markers: &[String],
) -> ScanReport {
    let views = layer_views(layers);
    let declared_hits = scan_declared(&views, declared_markers);
    let mut heuristic = Vec::new();
    for (layer, bytes) in views.iter() {
        heuristic.extend(scan_layer_heuristics(layer, bytes));
    }
    ScanReport {
        declared_hits,
        heuristic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BuildLayers;

    fn layers_with_app(app: &[u8]) -> BuildLayers {
        BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(app.to_vec()),
            vmstate: vec![0xAB; 16],
            memory: vec![0u8; 16],
        }
    }

    fn layers_with_memory(mem: Vec<u8>) -> BuildLayers {
        BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app".to_vec()),
            vmstate: vec![0xAB; 16],
            memory: mem,
        }
    }

    #[test]
    fn provider_key_prefix_in_memory_is_detected() {
        let mut mem = vec![b' '; 32];
        let secret = b"sk-proj-ABCDEFGHIJ1234567890abcdef";
        let at = 32;
        mem.extend_from_slice(secret);
        mem.extend_from_slice(&[b' '; 32]);
        let report = scan_build_layers(&layers_with_memory(mem), &[]);
        let f = report
            .heuristic
            .iter()
            .find(|f| f.kind == FindingKind::ProviderKeyPrefix)
            .expect("provider finding");
        assert_eq!(f.layer, "memory");
        assert_eq!(f.detail, "sk-");
        assert_eq!(f.offset, at);
    }

    #[test]
    fn env_assignment_in_app_is_detected() {
        let report = scan_build_layers(&layers_with_app(b"OPENAI_API_KEY=supersecretvalue123\n"), &[]);
        let f = report
            .heuristic
            .iter()
            .find(|f| f.kind == FindingKind::EnvAssignment)
            .expect("env finding");
        assert_eq!(f.layer, "app");
        assert_eq!(f.detail, "OPENAI_API_KEY");
    }

    #[test]
    fn high_entropy_base64_run_is_detected() {
        let report =
            scan_build_layers(&layers_with_app(b"A1b2C3d4E5f6G7h8I9j0KlMnOpQrStUvWxYz9+/="), &[]);
        assert!(report
            .heuristic
            .iter()
            .any(|f| f.kind == FindingKind::HighEntropyToken && f.detail.starts_with("base64")));
    }

    #[test]
    fn clean_layers_produce_no_findings() {
        let mut layers = layers_with_app(b"hello world app");
        layers.rootfs = b"alpine base rootfs".to_vec();
        layers.runtime = Some(b"python runtime layer".to_vec());
        layers.memory = (0..300_000u32).map(|i| (i % 256) as u8).collect();
        let report = scan_build_layers(&layers, &[]);
        assert!(report.declared_hits.is_empty());
        assert!(
            report.heuristic.is_empty(),
            "unexpected findings: {:?}",
            report.heuristic.iter().map(|f| f.summary()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn findings_never_contain_raw_secret() {
        let raw = "sk-proj-DEADBEEFcafef00d12345678";
        let mut mem = vec![b' '; 8];
        mem.extend_from_slice(raw.as_bytes());
        mem.extend_from_slice(&[b' '; 8]);
        let report = scan_build_layers(&layers_with_memory(mem), &[]);
        assert!(!report.heuristic.is_empty());
        let secret_suffix = "DEADBEEFcafef00d12345678";
        for f in &report.heuristic {
            assert!(!f.detail.contains(secret_suffix));
            assert!(!f.summary().contains(secret_suffix));
            assert!(!format!("{f:?}").contains(secret_suffix));
        }
    }

    #[test]
    fn declared_marker_returns_verbatim_hit() {
        let report = scan_build_layers(
            &layers_with_app(b"config has MY_DECLARED in it"),
            &["MY_DECLARED".to_string()],
        );
        assert_eq!(report.declared_hits, vec!["MY_DECLARED".to_string()]);
    }

    #[test]
    fn entropy_ignores_single_class_run() {
        let report = scan_build_layers(&layers_with_app(b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), &[]);
        assert!(!report
            .heuristic
            .iter()
            .any(|f| f.kind == FindingKind::HighEntropyToken));
    }

    #[test]
    fn provider_prefix_requires_token_suffix() {
        // "sk-" appears inside "task-force" but with a short suffix -> no finding.
        let report = scan_build_layers(&layers_with_app(b"a task-force meeting"), &[]);
        assert!(!report
            .heuristic
            .iter()
            .any(|f| f.kind == FindingKind::ProviderKeyPrefix));
    }

    #[test]
    fn high_entropy_provider_key_is_flagged_once_not_double() {
        // A provider key that is also a high-entropy run must yield exactly ONE
        // finding (the provider-prefix), not a duplicate HighEntropyToken —
        // guards the de-overlap suppression in scan_entropy_runs.
        let mut mem = vec![b' '; 16];
        mem.extend_from_slice(b"sk-proj-ABCDEFGHIJ1234567890abcdefKLMNOP");
        mem.extend_from_slice(&[b' '; 16]);
        let report = scan_build_layers(&layers_with_memory(mem), &[]);
        assert_eq!(report.heuristic.len(), 1, "{:?}", report.heuristic);
        assert_eq!(report.heuristic[0].kind, FindingKind::ProviderKeyPrefix);
    }

    #[test]
    fn offset_and_len_point_at_match() {
        let secret = b"ghp_ABCDEFGHIJ1234567890abcdefXYZ";
        let mut app = vec![b'.'; 5];
        app.extend_from_slice(secret);
        let report = scan_build_layers(&layers_with_app(&app), &[]);
        let f = report
            .heuristic
            .iter()
            .find(|f| f.kind == FindingKind::ProviderKeyPrefix)
            .expect("finding");
        assert_eq!(f.offset, 5);
        assert_eq!(f.len, secret.len());
    }
}
