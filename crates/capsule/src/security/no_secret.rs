//! Canonical credential-material policy shared by portable captures and snapshots.

/// Provider prefixes which are treated as credential material when followed by
/// a realistically shaped token.
pub const PROVIDER_KEY_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "AKIA",
    "ASIA",
    "xoxb-",
    "xoxp-",
    "AIza",
    "ya29.",
    "glpat-",
];

/// Environment-name fragments which make an assignment security-sensitive.
pub const SENSITIVE_ENV_MARKERS: &[&str] = &[
    "KEY",
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "PRIVATE",
    "ACCESS",
];

const PRIVATE_KEY_MARKERS: &[&[u8]] = &[
    b"-----BEGIN PRIVATE KEY-----",
    b"-----BEGIN RSA PRIVATE KEY-----",
    b"-----BEGIN EC PRIVATE KEY-----",
    b"-----BEGIN OPENSSH PRIVATE KEY-----",
];

const MIN_PROVIDER_SUFFIX_LEN: usize = 20;
const MIN_ENV_VALUE_LEN: usize = 6;
const MAX_MARKER_BYTES: usize = 40;
const MAX_ENV_NAME_DETAIL_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialFinding {
    pub offset: usize,
    pub len: usize,
    pub kind: &'static str,
    pub detail: String,
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-' | b'.')
}

/// Fail-closed, high-precision scan for credential material. Findings never
/// include the credential value itself.
pub fn scan_credential_material(bytes: &[u8]) -> Vec<CredentialFinding> {
    let mut scanner = CredentialScanner::new();
    scanner.push(bytes);
    scanner.finish()
}

#[derive(Debug)]
pub struct CredentialScanner {
    offset: usize,
    previous: Option<u8>,
    rolling: Vec<u8>,
    provider_candidates: Vec<ProviderCandidate>,
    env_name_start: usize,
    env_name_len: usize,
    env_name: Vec<u8>,
    env_name_rolling: Vec<u8>,
    env_name_sensitive: bool,
    env_candidate: Option<EnvCandidate>,
    private_key_seen: [bool; PRIVATE_KEY_MARKERS.len()],
    findings: Vec<CredentialFinding>,
}

#[derive(Debug)]
struct ProviderCandidate {
    prefix_index: usize,
    start: usize,
    suffix_len: usize,
    classes: u8,
}

#[derive(Debug)]
struct EnvCandidate {
    start: usize,
    name_len: usize,
    detail: String,
    value_len: usize,
    classes: u8,
}

impl Default for CredentialScanner {
    fn default() -> Self {
        Self {
            offset: 0,
            previous: None,
            rolling: Vec::with_capacity(MAX_MARKER_BYTES),
            provider_candidates: Vec::with_capacity(PROVIDER_KEY_PREFIXES.len()),
            env_name_start: 0,
            env_name_len: 0,
            env_name: Vec::with_capacity(MAX_ENV_NAME_DETAIL_BYTES),
            env_name_rolling: Vec::with_capacity(MAX_MARKER_BYTES),
            env_name_sensitive: false,
            env_candidate: None,
            private_key_seen: [false; PRIVATE_KEY_MARKERS.len()],
            findings: Vec::new(),
        }
    }
}

impl CredentialScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) {
        if let Some(&byte) = chunk.first()
            && chunk.iter().all(|candidate| *candidate == byte)
            && (byte.is_ascii_alphanumeric() || byte == b'_')
            && self.provider_candidates.is_empty()
            && self.env_candidate.is_none()
            && chunk.len() > MAX_MARKER_BYTES
        {
            for &byte in &chunk[..MAX_MARKER_BYTES] {
                self.accept(byte);
            }
            if self.provider_candidates.is_empty() && self.env_candidate.is_none() {
                self.accept_uniform_run(byte, chunk.len() - MAX_MARKER_BYTES);
                return;
            }
            for &byte in &chunk[MAX_MARKER_BYTES..] {
                self.accept(byte);
            }
            return;
        }
        for &byte in chunk {
            self.accept(byte);
        }
    }

    pub fn finish(mut self) -> Vec<CredentialFinding> {
        self.finish_provider_candidates();
        self.finish_env_candidate();
        self.findings
    }

    fn accept(&mut self, byte: u8) {
        self.advance_provider_candidates(byte);
        self.advance_env_candidate(byte);
        push_bounded(&mut self.rolling, byte, MAX_MARKER_BYTES);
        self.detect_private_key();
        self.detect_provider_prefix();
        self.advance_env_name(byte);
        self.previous = Some(byte);
        self.offset += 1;
    }

    fn accept_uniform_run(&mut self, byte: u8, count: usize) {
        if count == 0 {
            return;
        }
        self.offset += count;
        self.previous = Some(byte);
        self.rolling.clear();
        self.rolling.resize(MAX_MARKER_BYTES.min(count), byte);
        if self.env_name_len == 0 {
            self.env_name_start = self.offset - count;
        }
        self.env_name_len += count;
        let retained = (MAX_ENV_NAME_DETAIL_BYTES - self.env_name.len()).min(count);
        self.env_name
            .extend(std::iter::repeat_n(byte.to_ascii_uppercase(), retained));
        self.env_name_rolling.clear();
        self.env_name_rolling
            .resize(MAX_MARKER_BYTES.min(count), byte.to_ascii_uppercase());
    }

    fn advance_provider_candidates(&mut self, byte: u8) {
        let mut completed = Vec::new();
        self.provider_candidates.retain_mut(|candidate| {
            if is_token_byte(byte) {
                candidate.suffix_len += 1;
                candidate.classes |= byte_classes(byte);
                true
            } else {
                if candidate.suffix_len >= MIN_PROVIDER_SUFFIX_LEN
                    && candidate.classes.count_ones() >= 2
                {
                    completed.push(CredentialFinding {
                        offset: candidate.start,
                        len: self.offset - candidate.start,
                        kind: "provider-key",
                        detail: PROVIDER_KEY_PREFIXES[candidate.prefix_index].to_owned(),
                    });
                }
                false
            }
        });
        self.findings.extend(completed);
    }

    fn finish_provider_candidates(&mut self) {
        for candidate in self.provider_candidates.drain(..) {
            if candidate.suffix_len >= MIN_PROVIDER_SUFFIX_LEN
                && candidate.classes.count_ones() >= 2
            {
                self.findings.push(CredentialFinding {
                    offset: candidate.start,
                    len: self.offset - candidate.start,
                    kind: "provider-key",
                    detail: PROVIDER_KEY_PREFIXES[candidate.prefix_index].to_owned(),
                });
            }
        }
    }

    fn detect_provider_prefix(&mut self) {
        for (prefix_index, prefix) in PROVIDER_KEY_PREFIXES.iter().enumerate() {
            let prefix = prefix.as_bytes();
            if prefix.last() != self.rolling.last() || !self.rolling.ends_with(prefix) {
                continue;
            }
            let start = self.offset + 1 - prefix.len();
            let preceding = if start == 0 {
                None
            } else {
                self.rolling
                    .get(self.rolling.len().saturating_sub(prefix.len() + 1))
                    .copied()
            };
            if preceding.is_none_or(is_provider_boundary) {
                self.provider_candidates.push(ProviderCandidate {
                    prefix_index,
                    start,
                    suffix_len: 0,
                    classes: 0,
                });
            }
        }
    }

    fn detect_private_key(&mut self) {
        for (index, marker) in PRIVATE_KEY_MARKERS.iter().enumerate() {
            if !self.private_key_seen[index]
                && marker.last() == self.rolling.last()
                && self.rolling.ends_with(marker)
            {
                self.private_key_seen[index] = true;
                self.findings.push(CredentialFinding {
                    offset: self.offset + 1 - marker.len(),
                    len: marker.len(),
                    kind: "private-key",
                    detail: "PEM/OpenSSH private key".to_owned(),
                });
            }
        }
    }

    fn advance_env_name(&mut self, byte: u8) {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            if self.env_name_len == 0 {
                self.env_name_start = self.offset;
            }
            self.env_name_len += 1;
            if self.env_name.len() < MAX_ENV_NAME_DETAIL_BYTES {
                self.env_name.push(byte.to_ascii_uppercase());
            }
            push_bounded(
                &mut self.env_name_rolling,
                byte.to_ascii_uppercase(),
                MAX_MARKER_BYTES,
            );
            self.env_name_sensitive |= SENSITIVE_ENV_MARKERS.iter().any(|marker| {
                marker.as_bytes().last() == self.env_name_rolling.last()
                    && self.env_name_rolling.ends_with(marker.as_bytes())
            });
        } else if byte == b'=' {
            if self.env_name_len > 0 && self.env_name_sensitive {
                let mut detail = String::from_utf8_lossy(&self.env_name).into_owned();
                if self.env_name_len > self.env_name.len() {
                    detail.push('…');
                }
                self.env_candidate = Some(EnvCandidate {
                    start: self.env_name_start,
                    name_len: self.env_name_len,
                    detail,
                    value_len: 0,
                    classes: 0,
                });
            }
            self.reset_env_name();
        } else {
            self.reset_env_name();
        }
    }

    fn reset_env_name(&mut self) {
        self.env_name_len = 0;
        self.env_name.clear();
        self.env_name_rolling.clear();
        self.env_name_sensitive = false;
    }

    fn advance_env_candidate(&mut self, byte: u8) {
        let Some(candidate) = self.env_candidate.as_mut() else {
            return;
        };
        if is_token_byte(byte) {
            candidate.value_len += 1;
            candidate.classes |= byte_classes(byte);
        } else {
            self.finish_env_candidate();
        }
    }

    fn finish_env_candidate(&mut self) {
        let Some(candidate) = self.env_candidate.take() else {
            return;
        };
        if candidate.value_len >= MIN_ENV_VALUE_LEN && candidate.classes.count_ones() >= 2 {
            self.findings.push(CredentialFinding {
                offset: candidate.start,
                len: candidate.name_len + 1 + candidate.value_len,
                kind: "env-assignment",
                detail: candidate.detail,
            });
        }
    }

    #[cfg(test)]
    fn buffered_bytes(&self) -> usize {
        self.rolling.capacity()
            + self.provider_candidates.capacity() * std::mem::size_of::<ProviderCandidate>()
            + self.env_name.capacity()
            + self.env_name_rolling.capacity()
            + self
                .env_candidate
                .as_ref()
                .map_or(0, |candidate| candidate.detail.capacity())
    }
}

fn push_bounded(buffer: &mut Vec<u8>, byte: u8, maximum: usize) {
    if buffer.len() == maximum {
        buffer.remove(0);
    }
    buffer.push(byte);
}

fn is_provider_boundary(byte: u8) -> bool {
    !matches!(
        byte,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.'
    )
}

fn byte_classes(byte: u8) -> u8 {
    (u8::from(byte.is_ascii_lowercase()))
        | (u8::from(byte.is_ascii_uppercase()) << 1)
        | (u8::from(byte.is_ascii_digit()) << 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_provider_env_and_private_key_without_leaking_values() {
        let secret =
            b"OPENAI_API_KEY=sk-proj-ABCDEFGHIJ1234567890abcdef\n-----BEGIN PRIVATE KEY-----";
        let findings = scan_credential_material(secret);
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == "provider-key")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == "env-assignment")
        );
        assert!(findings.iter().any(|finding| finding.kind == "private-key"));
        assert!(!format!("{findings:?}").contains("ABCDEFGHIJ1234567890abcdef"));
    }

    #[test]
    fn incremental_scanner_is_independent_of_chunk_boundaries() {
        let input = b"safe\nOPENAI_API_KEY=sk-proj-ABCDEFGHIJ1234567890abcdef\n\
            -----BEGIN PRIVATE KEY-----\nmore safe output\n";
        let expected = scan_credential_material(input);
        for chunk_size in [1, 7, 4 * 1024, 64 * 1024] {
            let mut scanner = CredentialScanner::new();
            for chunk in input.chunks(chunk_size) {
                scanner.push(chunk);
            }
            assert_eq!(scanner.finish(), expected, "chunk size {chunk_size}");
        }

        let mut scanner = CredentialScanner::new();
        let mut offset = 0;
        let mut step = 17_usize;
        while offset < input.len() {
            step = (step * 37 + 11) % 31 + 1;
            let end = (offset + step).min(input.len());
            scanner.push(&input[offset..end]);
            offset = end;
        }
        assert_eq!(scanner.finish(), expected);
    }

    #[test]
    fn scanner_memory_is_bounded_for_large_delimiter_free_inputs() {
        for byte in [b'x', b'A'] {
            let mut scanner = CredentialScanner::new();
            let chunk = vec![byte; 64 * 1024];
            for _ in 0..(128 * 1024 * 1024 / chunk.len()) {
                scanner.push(&chunk);
                assert!(scanner.buffered_bytes() < 16 * 1024);
            }
            assert!(scanner.finish().is_empty());
        }
    }

    #[test]
    fn scanner_detects_markers_crossing_every_chunk_boundary() {
        let input = b"safe sk-proj-ABCDEFGHIJ1234567890abcdef and \
            -----BEGIN OPENSSH PRIVATE KEY----- end";
        let expected = scan_credential_material(input);
        for split in 1..input.len() {
            let mut scanner = CredentialScanner::new();
            scanner.push(&input[..split]);
            scanner.push(&input[split..]);
            assert_eq!(scanner.finish(), expected, "split {split}");
        }
    }
}
