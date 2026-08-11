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

fn class_count(bytes: &[u8]) -> usize {
    let lower = bytes.iter().any(u8::is_ascii_lowercase);
    let upper = bytes.iter().any(u8::is_ascii_uppercase);
    let digit = bytes.iter().any(u8::is_ascii_digit);
    lower as usize + upper as usize + digit as usize
}

/// Fail-closed, high-precision scan for credential material. Findings never
/// include the credential value itself.
pub fn scan_credential_material(bytes: &[u8]) -> Vec<CredentialFinding> {
    let mut findings = Vec::new();
    for marker in PRIVATE_KEY_MARKERS {
        if let Some(offset) = bytes
            .windows(marker.len())
            .position(|window| window == *marker)
        {
            findings.push(CredentialFinding {
                offset,
                len: marker.len(),
                kind: "private-key",
                detail: "PEM/OpenSSH private key".to_owned(),
            });
        }
    }
    for prefix in PROVIDER_KEY_PREFIXES {
        let prefix = prefix.as_bytes();
        let mut offset = 0;
        while offset + prefix.len() <= bytes.len() {
            if &bytes[offset..offset + prefix.len()] != prefix {
                offset += 1;
                continue;
            }
            let boundary = offset == 0 || !is_token_byte(bytes[offset - 1]);
            let mut end = offset + prefix.len();
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            let suffix = &bytes[offset + prefix.len()..end];
            if boundary && suffix.len() >= MIN_PROVIDER_SUFFIX_LEN && class_count(suffix) >= 2 {
                findings.push(CredentialFinding {
                    offset,
                    len: end - offset,
                    kind: "provider-key",
                    detail: String::from_utf8_lossy(prefix).into_owned(),
                });
            }
            offset = end.max(offset + 1);
        }
    }
    for equals in (0..bytes.len()).filter(|index| bytes[*index] == b'=') {
        let mut start = equals;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let name = &bytes[start..equals];
        let name_upper = String::from_utf8_lossy(name).to_ascii_uppercase();
        if name.is_empty()
            || !SENSITIVE_ENV_MARKERS
                .iter()
                .any(|part| name_upper.contains(part))
        {
            continue;
        }
        let mut end = equals + 1;
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
        }
        let value = &bytes[equals + 1..end];
        if value.len() >= MIN_ENV_VALUE_LEN && class_count(value) >= 2 {
            findings.push(CredentialFinding {
                offset: start,
                len: end - start,
                kind: "env-assignment",
                detail: name_upper,
            });
        }
    }
    findings
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
}
