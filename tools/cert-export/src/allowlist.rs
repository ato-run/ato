//! The root-owned allowlist that bounds what this tool may ever export.
//!
//! One line is one (Instance, domain) pair plus the four things the renewal
//! path needs: where the pair lives on the host, and which two ato.secret@1
//! Binding ids receive it. Nothing else about an Instance is readable from
//! here - no Bundle, no Contract, no account - so a compromised root-side tool
//! is still bounded by the file's contents.
//!
//! Format (v1):
//!
//!   allowlist = "ato.cert-export.allowlist/1"
//!   # instance_id   domain        cert-path        key-path        cert-binding   key-binding
//!   i_01J8ZQ0K      mail.ato.run  /srv/certs/mail.ato.run.crt  /srv/certs/mail.ato.run.key  mail_tls_cert  mail_tls_key
//!
//! Whitespace separates fields, '#' starts a comment, and the header line is
//! mandatory so a future format change cannot be mistaken for this one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const HEADER: &str = "ato.cert-export.allowlist/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub instance_id: String,
    pub domain: String,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub certificate_binding: String,
    pub private_key_binding: String,
}

impl Entry {
    pub fn binding_ids(&self) -> [&str; 2] {
        [&self.certificate_binding, &self.private_key_binding]
    }
}

pub fn parse(text: &str) -> Result<Vec<Entry>> {
    let mut header_seen = false;
    let mut entries: Vec<Entry> = Vec::new();
    let mut instance_domains: BTreeSet<(String, String)> = BTreeSet::new();
    let mut instance_bindings: BTreeSet<(String, String)> = BTreeSet::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !header_seen {
            let expected = format!("allowlist = \"{HEADER}\"");
            if line != expected {
                bail!("line {number}: the first line must be the {HEADER} header");
            }
            header_seen = true;
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 6 {
            bail!("line {number}: expected 6 fields, found {}", fields.len());
        }
        let instance_id = fields[0];
        let domain = fields[1];
        if !valid_instance_id(instance_id) {
            bail!("line {number}: instance id is not a valid identifier");
        }
        if !valid_domain(domain) {
            bail!("line {number}: domain is not a lowercase DNS name");
        }
        let certificate = PathBuf::from(fields[2]);
        let private_key = PathBuf::from(fields[3]);
        for path in [&certificate, &private_key] {
            if !path.is_absolute() {
                bail!("line {number}: source path must be absolute");
            }
        }
        if certificate == private_key {
            bail!("line {number}: certificate and private key must be different files");
        }
        let certificate_binding = fields[4];
        let private_key_binding = fields[5];
        for binding in [certificate_binding, private_key_binding] {
            if !valid_binding_id(binding) {
                bail!("line {number}: binding id must match [a-z0-9_]+ up to 64 characters");
            }
        }
        if certificate_binding == private_key_binding {
            bail!("line {number}: the two bindings must be different");
        }
        if !instance_domains.insert((instance_id.to_owned(), domain.to_owned())) {
            bail!("line {number}: duplicate (instance, domain) entry");
        }
        for binding in [certificate_binding, private_key_binding] {
            if !instance_bindings.insert((instance_id.to_owned(), binding.to_owned())) {
                bail!("line {number}: a binding id is used twice for one instance");
            }
        }
        entries.push(Entry {
            instance_id: instance_id.to_owned(),
            domain: domain.to_owned(),
            certificate,
            private_key,
            certificate_binding: certificate_binding.to_owned(),
            private_key_binding: private_key_binding.to_owned(),
        });
    }
    if !header_seen {
        bail!("line 1: the {HEADER} header is missing");
    }
    if entries.is_empty() {
        bail!("the allowlist authorizes no (instance, domain) pair");
    }
    Ok(entries)
}

/// Load and vet the allowlist file itself.
pub fn load(path: &Path, privileged: bool) -> Result<Vec<Entry>> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("allowlist {} is not readable", path.display()))?;
    if !metadata.is_file() {
        bail!("the allowlist is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if privileged && (metadata.uid() != 0 || metadata.mode() & 0o077 != 0) {
            bail!("the allowlist must be root-owned and readable only by root");
        }
    }
    let _ = privileged;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("allowlist {} is not valid UTF-8", path.display()))?;
    parse(&text)
}

/// The one entry this invocation may act on.
pub fn select<'a>(
    entries: &'a [Entry],
    instance_id: &str,
    asserted_domain: Option<&str>,
) -> Result<&'a Entry> {
    let mut matches = entries
        .iter()
        .filter(|entry| entry.instance_id == instance_id);
    let Some(entry) = matches.next() else {
        bail!("instance is not allowlisted");
    };
    if matches.next().is_some() {
        bail!("instance is allowlisted for more than one domain; name one explicitly");
    }
    if let Some(domain) = asserted_domain
        && !domain.eq_ignore_ascii_case(&entry.domain)
    {
        bail!("the asserted domain is not the allowlisted domain for this instance");
    }
    Ok(entry)
}

pub fn valid_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

pub fn valid_binding_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Lowercase, no wildcard, at least two labels, no empty label.
pub fn valid_domain(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.ends_with('.') {
        return false;
    }
    let labels: Vec<&str> = value.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(body: &str) -> String {
        format!("allowlist = \"{HEADER}\"\n{body}")
    }

    #[test]
    fn a_complete_allowlist_parses() {
        let entries = parse(&text(
            "# mail\ni_01J8ZQ0K mail.ato.run /srv/certs/a.crt /srv/certs/a.key mail_tls_cert mail_tls_key\n",
        ))
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].instance_id, "i_01J8ZQ0K");
        assert_eq!(entries[0].domain, "mail.ato.run");
        assert_eq!(entries[0].binding_ids(), ["mail_tls_cert", "mail_tls_key"]);
    }

    #[test]
    fn a_missing_or_unknown_header_is_refused() {
        assert!(parse("i_01J8ZQ0K mail.ato.run /a.crt /a.key c k\n").is_err());
        assert!(parse("allowlist = \"ato.cert-export.allowlist/2\"\n").is_err());
    }

    #[test]
    fn malformed_lines_are_refused_with_a_line_number() {
        let cases = [
            "i_01J8ZQ0K mail.ato.run /a.crt /a.key c\n",
            "i_01J8ZQ0K mail.ato.run a.crt /a.key c k\n",
            "i_01J8ZQ0K mail.ato.run /a.crt /a.key C k\n",
            "i_01J8ZQ0K mail.ato.run /a.crt /a.crt c k\n",
            "i_01J8ZQ0K mail.ato.run /a.crt /a.key c c\n",
            "i_01J8ZQ0K MAIL.ato.run /a.crt /a.key c k\n",
            "i_01J8ZQ0K localhost /a.crt /a.key c k\n",
        ];
        for case in cases {
            let error = parse(&text(case)).unwrap_err().to_string();
            assert!(error.starts_with("line 2:"), "{case} -> {error}");
        }
    }

    #[test]
    fn duplicate_entries_are_refused() {
        let line = "i_01J8ZQ0K mail.ato.run /a.crt /a.key c k\n";
        assert!(parse(&text(&format!("{line}{line}"))).is_err());
        let reused =
            "i_01J8ZQ0K mail.ato.run /a.crt /a.key c k\ni_01J8ZQ0K mx.ato.run /b.crt /b.key c k2\n";
        assert!(parse(&text(reused)).is_err());
    }

    #[test]
    fn an_empty_allowlist_authorizes_nothing() {
        assert!(parse(&text("# nothing\n")).is_err());
    }

    #[test]
    fn selection_is_bounded_by_the_file_and_the_asserted_domain() {
        let entries = parse(&text(
            "i_a mail.ato.run /a.crt /a.key c k\ni_b mx.ato.run /b.crt /b.key c2 k2\n",
        ))
        .unwrap();
        assert_eq!(
            select(&entries, "i_a", None).unwrap().domain,
            "mail.ato.run"
        );
        assert!(select(&entries, "i_a", Some("mail.ato.run")).is_ok());
        assert!(select(&entries, "i_a", Some("mx.ato.run")).is_err());
        assert!(select(&entries, "i_c", None).is_err());
    }
}
