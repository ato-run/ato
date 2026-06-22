/// Known secret prefixes used to detect accidentally committed credentials.
const SECRET_PREFIXES: &[&str] = &[
    "sk-ant-api",  // Anthropic (longer prefix first)
    "sk-ant-",     // Anthropic
    "sk-proj-",    // OpenAI project key
    "sk-svcacct-", // OpenAI service account
    "sk-",         // OpenAI / generic
    "xai-",        // xAI Grok
    "ghp_",        // GitHub personal access token
    "ghs_",        // GitHub server-to-server token
    "github_pat_", // GitHub fine-grained PAT
    "AKIA",        // AWS access key ID
    "AGPA",        // AWS group
    "AROA",        // AWS role
    "AIPA",        // AWS managed IAM policy
    "ANPA",        // AWS managed policy
    "ANVA",        // AWS versioned managed policy
    "ASIA",        // AWS STS temporary credential
    "xoxb-",       // Slack bot token
    "xoxp-",       // Slack user token
    "xoxs-",       // Slack app-level token
    "glpat-",      // GitLab personal access token
    "SG.",         // SendGrid API key
    "sk_live_",    // Stripe secret key
    "pk_live_",    // Stripe publishable key
    "rk_live_",    // Stripe restricted key
];

#[derive(Debug, Clone)]
pub(crate) struct SecretScanHit {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) prefix: String,
    pub(crate) snippet: String,
}

/// Scan text content for known secret patterns.
/// Returns hits with file path, 1-indexed line number, matched prefix, and a masked snippet.
pub(crate) fn scan_for_secret_patterns(content: &str, file_path: &str) -> Vec<SecretScanHit> {
    let mut hits = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        for prefix in SECRET_PREFIXES {
            if let Some(start) = find_secret_value_start(line, prefix) {
                let value_tail = &line[start + prefix.len()..];
                let raw_value = value_tail
                    .split(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')'));
                if raw_value.len() < 8 {
                    continue; // Too short to be a real secret
                }
                hits.push(SecretScanHit {
                    file: file_path.to_string(),
                    line: line_idx + 1,
                    prefix: prefix.to_string(),
                    snippet: mask_value(&format!("{}{}", prefix, raw_value)),
                });
                break; // Only report first hit per line
            }
        }
    }
    hits
}

fn find_secret_value_start(line: &str, prefix: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let prefix_bytes = prefix.as_bytes();
    let mut i = 0;
    while i + prefix_bytes.len() <= bytes.len() {
        if bytes[i..].starts_with(prefix_bytes) {
            // Check character before prefix: must be assignment, quote, whitespace, or start-of-line
            let before_ok = i == 0
                || matches!(
                    bytes[i - 1] as char,
                    '=' | '"' | '\'' | ' ' | '\t' | '(' | ':'
                );
            if before_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn mask_value(value: &str) -> String {
    let n = value.len();
    if n <= 8 {
        return "*".repeat(n);
    }
    // Show first 6 chars then mask
    let visible = &value[..6.min(n)];
    format!("{}...{}", visible, "*".repeat(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_openai_key() {
        let hits = scan_for_secret_patterns("OPENAI_API_KEY=sk-abcdefghij1234567890\n", "test.env");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].prefix, "sk-");
    }

    #[test]
    fn detects_aws_key() {
        let hits = scan_for_secret_patterns("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n", "test.env");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].prefix, "AKIA");
    }

    #[test]
    fn skips_comments() {
        let hits = scan_for_secret_patterns("# OPENAI_API_KEY=sk-abc\n", "test.env");
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn skips_too_short_values() {
        let hits = scan_for_secret_patterns("KEY=sk-abc\n", "test.env");
        assert_eq!(hits.len(), 0);
    }
}
