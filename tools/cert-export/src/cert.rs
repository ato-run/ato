//! Certificate and private-key inspection.
//!
//! The host has no offline-buildable X.509 parser crate, so inspection shells
//! out to the host's own 'openssl'. Only file paths are ever placed on the
//! command line: the private key is read by openssl from its path, never
//! passed as an argument, an environment value or a stdin payload. Nothing
//! this module returns contains key material, so its output is safe to put in
//! a log or in meta.json.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::time;

/// Facts about one certificate, all of them safe to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFacts {
    /// Leaf serial as lowercase hex, the value an operator compares with the
    /// serial the K5 observation saw on the wire.
    pub serial_hex: String,
    /// dNSName SAN entries, as the certificate states them.
    pub sans: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub not_before_raw: String,
    pub not_after_raw: String,
    pub subject: String,
    pub issuer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairFacts {
    pub certificate: CertFacts,
    pub key_matches_certificate: bool,
}

/// The seam that keeps validation logic testable without a host binary.
pub trait CertInspector {
    fn inspect_pair(&self, certificate: &Path, private_key: &Path) -> Result<PairFacts>;
}

/// Runs the host 'openssl' binary for inspection.
pub struct OpensslCli {
    program: PathBuf,
}

impl OpensslCli {
    /// Resolve and vet the binary before it is used.
    pub fn new(program: impl Into<PathBuf>, privileged: bool) -> Result<Self> {
        let program = program.into();
        verify_program(&program, privileged)?;
        Ok(Self { program })
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    fn output(&self, arguments: &[&OsStr]) -> Result<Vec<u8>> {
        let output = Command::new(&self.program)
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("could not run {}", self.program.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "openssl refused the input ({}): {}",
                output.status,
                first_line(&stderr)
            );
        }
        Ok(output.stdout)
    }
}

impl CertInspector for OpensslCli {
    fn inspect_pair(&self, certificate: &Path, private_key: &Path) -> Result<PairFacts> {
        let facts = self.output(&[
            OsStr::new("x509"),
            OsStr::new("-in"),
            certificate.as_os_str(),
            OsStr::new("-noout"),
            OsStr::new("-serial"),
            OsStr::new("-subject"),
            OsStr::new("-issuer"),
            OsStr::new("-startdate"),
            OsStr::new("-enddate"),
            OsStr::new("-ext"),
            OsStr::new("subjectAltName"),
        ])?;
        let certificate_facts = parse_openssl_facts(&String::from_utf8_lossy(&facts))?;

        // An encrypted key would block on a passphrase prompt; the empty
        // passphrase makes openssl fail immediately instead. 'pass:' carries no
        // secret, so it is safe on the command line.
        self.output(&[
            OsStr::new("pkey"),
            OsStr::new("-in"),
            private_key.as_os_str(),
            OsStr::new("-noout"),
            OsStr::new("-passin"),
            OsStr::new("pass:"),
        ])
        .context("the private key is not a readable, unencrypted key")?;

        let certificate_public_key = self.output(&[
            OsStr::new("x509"),
            OsStr::new("-in"),
            certificate.as_os_str(),
            OsStr::new("-noout"),
            OsStr::new("-pubkey"),
        ])?;
        let private_key_public_key = self.output(&[
            OsStr::new("pkey"),
            OsStr::new("-in"),
            private_key.as_os_str(),
            OsStr::new("-pubout"),
            OsStr::new("-passin"),
            OsStr::new("pass:"),
        ])?;
        let key_matches_certificate = pem_body(&String::from_utf8_lossy(&certificate_public_key))
            == pem_body(&String::from_utf8_lossy(&private_key_public_key));

        Ok(PairFacts {
            certificate: certificate_facts,
            key_matches_certificate,
        })
    }
}

/// Parse the exact set of fields requested from 'openssl x509'.
pub fn parse_openssl_facts(text: &str) -> Result<CertFacts> {
    let mut serial_hex = None;
    let mut subject = None;
    let mut issuer = None;
    let mut not_before_raw = None;
    let mut not_after_raw = None;
    let mut sans = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("serial=") {
            serial_hex = Some(value.trim().to_ascii_lowercase());
        } else if let Some(value) = line.strip_prefix("subject=") {
            subject = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("issuer=") {
            issuer = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("notBefore=") {
            not_before_raw = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("notAfter=") {
            not_after_raw = Some(value.trim().to_owned());
        } else if line.contains("DNS:") {
            sans.extend(parse_dns_names(line));
        }
    }
    let serial_hex = serial_hex.context("certificate has no serial")?;
    if serial_hex.is_empty() || !serial_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("certificate serial is not hexadecimal");
    }
    let not_before_raw = not_before_raw.context("certificate has no notBefore")?;
    let not_after_raw = not_after_raw.context("certificate has no notAfter")?;
    Ok(CertFacts {
        serial_hex,
        sans,
        not_before: time::parse_asn1_time(&not_before_raw)?,
        not_after: time::parse_asn1_time(&not_after_raw)?,
        not_before_raw,
        not_after_raw,
        subject: subject.unwrap_or_default(),
        issuer: issuer.unwrap_or_default(),
    })
}

/// Every 'DNS:name' token on a Subject Alternative Name line.
pub fn parse_dns_names(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = line;
    while let Some(index) = rest.find("DNS:") {
        rest = &rest[index + 4..];
        let end = rest
            .find(|character: char| character == ',' || character.is_whitespace())
            .unwrap_or(rest.len());
        let name = rest[..end]
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

/// The base64 body of a PEM block, without headers or line breaks.
pub fn pem_body(pem: &str) -> String {
    pem.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect::<String>()
}

/// True when a dNSName entry covers the given host name.
///
/// '*.example.com' covers exactly one label, never 'a.b.example.com' and never
/// 'example.com' itself. Wildcards are only meaningful in the leftmost label,
/// which is all a public CA may issue.
pub fn san_covers(sans: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    sans.iter().any(|entry| {
        let entry = entry.trim_end_matches('.').to_ascii_lowercase();
        if entry == host {
            return true;
        }
        let Some(suffix) = entry.strip_prefix("*.") else {
            return false;
        };
        let Some(label) = host
            .strip_suffix(suffix)
            .and_then(|rest| rest.strip_suffix('.'))
        else {
            return false;
        };
        !label.is_empty() && !label.contains('.')
    })
}

/// Refuse a binary that a non-root user could have replaced.
pub fn verify_program(program: &Path, privileged: bool) -> Result<()> {
    if !program.is_absolute() {
        bail!("the openssl program must be an absolute path");
    }
    let metadata = std::fs::metadata(program)
        .with_context(|| format!("openssl program {} is not usable", program.display()))?;
    if !metadata.is_file() {
        bail!("the openssl program is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("the openssl program is not executable");
        }
        if privileged && (metadata.uid() != 0 || metadata.mode() & 0o022 != 0) {
            bail!("the openssl program is not root-owned and non-writable by others");
        }
    }
    Ok(())
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 300 {
        let truncated: String = line.chars().take(300).collect();
        format!("{truncated}...")
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACTS: &str = "serial=486B6842C5850B65ADC230B72A347CFED5DC68FE
subject=CN=mail.example.test
issuer=CN=Example Test CA
notBefore=Nov  1 23:19:45 2026 GMT
notAfter=Jan 30 23:19:45 2027 GMT
X509v3 Subject Alternative Name: 
    DNS:mail.example.test, DNS:mx.example.test
";

    #[test]
    fn openssl_output_becomes_lowercase_hex_and_iso_times() {
        let facts = parse_openssl_facts(FACTS).unwrap();
        assert_eq!(facts.serial_hex, "486b6842c5850b65adc230b72a347cfed5dc68fe");
        assert_eq!(facts.sans, vec!["mail.example.test", "mx.example.test"]);
        assert_eq!(facts.not_before, 1_793_575_185);
        assert_eq!(facts.not_after, 1_801_351_185);
        assert_eq!(facts.not_before_raw, "Nov  1 23:19:45 2026 GMT");
        assert_eq!(facts.not_after_raw, "Jan 30 23:19:45 2027 GMT");
        assert!(facts.not_after > facts.not_before);
        assert_eq!(facts.subject, "CN=mail.example.test");
        assert_eq!(facts.issuer, "CN=Example Test CA");
    }

    #[test]
    fn a_certificate_without_required_fields_is_refused() {
        for text in [
            FACTS.replace("serial=", "serial-x="),
            FACTS.replace("notAfter=", "notAfter-x="),
            FACTS.replace("486B6842", "zzzz"),
        ] {
            assert!(parse_openssl_facts(&text).is_err());
        }
    }

    #[test]
    fn san_parsing_keeps_every_dns_entry() {
        assert_eq!(
            parse_dns_names("    DNS:a.example.to, DNS:b.example.to"),
            vec!["a.example.to", "b.example.to"]
        );
        assert_eq!(
            parse_dns_names("    DNS:A.Example.To."),
            vec!["a.example.to"]
        );
        assert!(parse_dns_names("    IP Address:127.0.0.1").is_empty());
    }

    #[test]
    fn wildcards_cover_one_label_only() {
        let sans = vec!["*.example.test".to_owned()];
        assert!(san_covers(&sans, "mail.example.test"));
        assert!(san_covers(&sans, "MAIL.EXAMPLE.TEST"));
        assert!(!san_covers(&sans, "a.b.example.test"));
        assert!(!san_covers(&sans, "example.test"));
        assert!(!san_covers(&sans, "example.test.evil"));
        assert!(!san_covers(&sans, ""));
        assert!(san_covers(
            &["mail.example.test".to_owned()],
            "mail.example.test."
        ));
    }

    #[test]
    fn a_pem_body_drops_headers_and_line_breaks() {
        let pem = "-----BEGIN PUBLIC KEY-----\nMFkwEwYH\nKoZIzj0C\n-----END PUBLIC KEY-----\n";
        assert_eq!(pem_body(pem), "MFkwEwYHKoZIzj0C");
        assert_eq!(pem_body(""), "");
    }

    #[test]
    fn a_relative_or_missing_openssl_program_is_refused() {
        assert!(verify_program(Path::new("openssl"), false).is_err());
        assert!(verify_program(Path::new("/nonexistent/openssl"), false).is_err());
    }
}
