//! Validation before replacement, and the export directory that records it.
//!
//! Ordering is the safety property: every check runs, and every check passes,
//! before the first byte of the export directory changes. A rejection leaves
//! the previous export exactly as it was, so an operator can always fall back
//! to the pair the running Run was started with.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::allowlist::Entry;
use crate::cert::{CertFacts, CertInspector, san_covers};
use crate::time;

pub const META_SCHEMA: &str = "ato.cert-export.meta/1";
pub const CERTIFICATE_FILE: &str = "cert.pem";
pub const PRIVATE_KEY_FILE: &str = "key.pem";
pub const META_FILE: &str = "meta.json";

/// How much validity is enough to hand the running Run a new certificate.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// A pair that expires sooner than this is refused: a re-run would hand
    /// the workload a certificate that is about to lapse.
    pub min_remaining_hours: i64,
    /// Recorded as a warning so ops sees the renewal problem before it bites.
    pub warn_remaining_days: i64,
    /// A validity window longer than this is not a public certificate.
    pub max_validity_days: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_remaining_hours: 48,
            warn_remaining_days: 14,
            max_validity_days: 400,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Validation {
    pub certificate: CertFacts,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceDigests {
    pub certificate_sha256: String,
    pub private_key_sha256: String,
    pub certificate_bytes: u64,
    pub private_key_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertificateRecord {
    pub serial: String,
    pub sans: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub subject: String,
    pub issuer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviousRecord {
    pub serial: String,
    pub not_after: String,
    pub exported_at: String,
    pub archived_at: String,
}

/// The export record. It contains no private key material: only digests,
/// public certificate facts and the allowlisted identifiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Meta {
    pub schema: String,
    pub instance_id: String,
    pub domain: String,
    pub certificate_binding: String,
    pub private_key_binding: String,
    pub source: SourceDigests,
    pub certificate: CertificateRecord,
    pub exported_at: String,
    pub export_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousRecord>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub action: Action,
    pub validation: Validation,
    pub meta: Meta,
    pub export_dir: PathBuf,
}

/// What the export directory currently holds, verified against its meta.
#[derive(Debug)]
pub struct Exported {
    pub meta: Meta,
    pub certificate_pem: String,
    pub private_key_pem: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Inspect and check a pair without writing anything.
pub fn validate(
    entry: &Entry,
    thresholds: Thresholds,
    now: i64,
    inspector: &dyn CertInspector,
) -> Result<Validation, Error> {
    let pair = inspector
        .inspect_pair(&entry.certificate, &entry.private_key)
        .map_err(Error::tool)?;
    if !pair.key_matches_certificate {
        return Err(Error::validation(
            "the private key does not match the certificate public key",
        ));
    }
    let certificate = pair.certificate;
    if !san_covers(&certificate.sans, &entry.domain) {
        return Err(Error::validation(format!(
            "the certificate SAN does not cover {}",
            entry.domain
        )));
    }
    let remaining = certificate.not_after - now;
    if remaining <= 0 {
        return Err(Error::validation("the certificate has expired"));
    }
    if remaining < thresholds.min_remaining_hours.saturating_mul(3_600) {
        return Err(Error::validation(format!(
            "the certificate expires within {} hours",
            thresholds.min_remaining_hours
        )));
    }
    if certificate.not_before > now + 300 {
        return Err(Error::validation(
            "the certificate is not valid yet (clock skew or a future notBefore)",
        ));
    }
    let window = certificate.not_after - certificate.not_before;
    if window > thresholds.max_validity_days.saturating_mul(86_400) {
        return Err(Error::validation(format!(
            "the certificate validity window exceeds {} days",
            thresholds.max_validity_days
        )));
    }
    let mut warnings = Vec::new();
    if remaining < thresholds.warn_remaining_days.saturating_mul(86_400) {
        warnings.push(format!(
            "the certificate expires in {} days",
            remaining / 86_400
        ));
    }
    if certificate.sans.is_empty() {
        warnings.push("the certificate has no dNSName SAN entries".to_owned());
    }
    Ok(Validation {
        certificate,
        warnings,
    })
}

pub fn days_remaining(not_after: i64, now: i64) -> i64 {
    (not_after - now).div_euclid(86_400)
}

fn certificate_record(certificate: &CertFacts) -> CertificateRecord {
    CertificateRecord {
        serial: certificate.serial_hex.clone(),
        sans: certificate.sans.clone(),
        not_before: time::format_iso8601_utc(certificate.not_before),
        not_after: time::format_iso8601_utc(certificate.not_after),
        subject: certificate.subject.clone(),
        issuer: certificate.issuer.clone(),
    }
}

/// Validate, then bring the export directory up to date.
///
/// A pair whose source bytes are unchanged is a no-op: no write, no *.prev, and
/// no new Run to start.
pub fn export(
    entry: &Entry,
    thresholds: Thresholds,
    now: i64,
    inspector: &dyn CertInspector,
    export_dir: &Path,
) -> Result<Outcome, Error> {
    let validation = validate(entry, thresholds, now, inspector)?;

    let certificate_bytes = std::fs::read(&entry.certificate).map_err(Error::io)?;
    let private_key_bytes = std::fs::read(&entry.private_key).map_err(Error::io)?;
    let source = SourceDigests {
        certificate_sha256: sha256_hex(&certificate_bytes),
        private_key_sha256: sha256_hex(&private_key_bytes),
        certificate_bytes: certificate_bytes.len() as u64,
        private_key_bytes: private_key_bytes.len() as u64,
    };
    let existing = read_meta(export_dir)?;
    if let Some(meta) = &existing
        && meta.source == source
        && export_dir.join(CERTIFICATE_FILE).is_file()
        && export_dir.join(PRIVATE_KEY_FILE).is_file()
    {
        return Ok(Outcome {
            action: Action::Unchanged,
            validation,
            meta: meta.clone(),
            export_dir: export_dir.to_path_buf(),
        });
    }

    let certificate = certificate_record(&validation.certificate);
    let previous = existing.as_ref().map(|meta| PreviousRecord {
        serial: meta.certificate.serial.clone(),
        not_after: meta.certificate.not_after.clone(),
        exported_at: meta.exported_at.clone(),
        archived_at: time::format_iso8601_utc(now),
    });
    let meta = Meta {
        schema: META_SCHEMA.to_owned(),
        instance_id: entry.instance_id.clone(),
        domain: entry.domain.clone(),
        certificate_binding: entry.certificate_binding.clone(),
        private_key_binding: entry.private_key_binding.clone(),
        source,
        certificate,
        exported_at: time::format_iso8601_utc(now),
        export_dir: export_dir.display().to_string(),
        previous: previous.clone(),
    };
    write_export(export_dir, &certificate_bytes, &private_key_bytes, &meta)?;
    Ok(Outcome {
        action: if existing.is_some() {
            Action::Updated
        } else {
            Action::Created
        },
        validation,
        meta,
        export_dir: export_dir.to_path_buf(),
    })
}

pub fn export_dir_for(state_root: &Path, entry: &Entry) -> PathBuf {
    state_root.join(&entry.instance_id)
}

fn read_meta(export_dir: &Path) -> Result<Option<Meta>, Error> {
    let path = export_dir.join(META_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::io(error)),
    };
    let meta: Meta = serde_json::from_slice(&bytes).map_err(|_| {
        Error::io("the existing meta.json is unreadable or not a cert-export record")
    })?;
    if meta.schema != META_SCHEMA {
        return Err(Error::io(
            "the existing meta.json was written by a different schema",
        ));
    }
    Ok(Some(meta))
}

/// Read the export directory back, refusing an inconsistent record.
///
/// meta.json is written last and names the digests of the two PEM files, so a
/// crash between writes leaves a record that says so instead of a pair that
/// silently differs from what was validated.
pub fn load_export(export_dir: &Path) -> Result<Exported, Error> {
    let meta = read_meta(export_dir)?
        .ok_or_else(|| Error::io("the export directory has no meta.json; run export first"))?;
    let certificate_pem = std::fs::read_to_string(export_dir.join(CERTIFICATE_FILE))
        .map_err(|_| Error::io("the export directory has no readable cert.pem"))?;
    let private_key_pem = std::fs::read_to_string(export_dir.join(PRIVATE_KEY_FILE))
        .map_err(|_| Error::io("the export directory has no readable key.pem"))?;
    if sha256_hex(certificate_pem.as_bytes()) != meta.source.certificate_sha256
        || sha256_hex(private_key_pem.as_bytes()) != meta.source.private_key_sha256
    {
        return Err(Error::io(
            "the export directory does not match its meta.json (interrupted or edited export)",
        ));
    }
    Ok(Exported {
        meta,
        certificate_pem,
        private_key_pem,
    })
}

fn write_export(
    export_dir: &Path,
    certificate: &[u8],
    private_key: &[u8],
    meta: &Meta,
) -> Result<(), Error> {
    ensure_private_dir(export_dir)?;
    let mut json = serde_json::to_vec_pretty(meta).map_err(Error::tool)?;
    json.push(b'\n');
    let staged = [
        ("cert.pem.new", CERTIFICATE_FILE, certificate.to_vec()),
        ("key.pem.new", PRIVATE_KEY_FILE, private_key.to_vec()),
        ("meta.json.new", META_FILE, json),
    ];
    for (temporary, _, bytes) in &staged {
        write_private(&export_dir.join(temporary), bytes)?;
    }
    let archived = (|| -> Result<(), Error> {
        for (_, live, _) in &staged {
            let live_path = export_dir.join(live);
            if !live_path.is_file() {
                continue;
            }
            let previous = export_dir.join(format!("{live}.prev"));
            let _ = std::fs::remove_file(&previous);
            std::fs::rename(&live_path, &previous).map_err(Error::io)?;
        }
        for (temporary, live, _) in &staged {
            std::fs::rename(export_dir.join(temporary), export_dir.join(live))
                .map_err(Error::io)?;
        }
        Ok(())
    })();
    if archived.is_err() {
        for (temporary, _, _) in &staged {
            let _ = std::fs::remove_file(export_dir.join(temporary));
        }
    }
    archived
}

fn ensure_private_dir(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).map_err(Error::io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(Error::io)?;
    }
    Ok(())
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(Error::io)?;
    file.write_all(bytes).map_err(Error::io)?;
    file.sync_all().map_err(Error::io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::PairFacts;

    const NOW: i64 = 1_793_577_600; // 2026-11-02T00:00:00Z

    struct Fake {
        certificate: CertFacts,
        key_matches_certificate: bool,
    }

    impl Fake {
        fn valid() -> Self {
            Self {
                certificate: CertFacts {
                    serial_hex: "0a1b".to_owned(),
                    sans: vec!["mail.example.test".to_owned()],
                    not_before: NOW - 86_400,
                    not_after: NOW + 80 * 86_400,
                    not_before_raw: "Nov  1 00:00:00 2026 GMT".to_owned(),
                    not_after_raw: "Jan 21 00:00:00 2027 GMT".to_owned(),
                    subject: "CN=mail.example.test".to_owned(),
                    issuer: "CN=test".to_owned(),
                },
                key_matches_certificate: true,
            }
        }
    }

    impl CertInspector for Fake {
        fn inspect_pair(&self, _: &Path, _: &Path) -> anyhow::Result<PairFacts> {
            Ok(PairFacts {
                certificate: self.certificate.clone(),
                key_matches_certificate: self.key_matches_certificate,
            })
        }
    }

    fn workspace() -> tempfile::TempDir {
        std::fs::create_dir_all(".tmp").unwrap();
        tempfile::tempdir_in(".tmp").unwrap()
    }

    fn entry_in(root: &Path) -> Entry {
        std::fs::write(
            root.join("source.crt"),
            b"-----BEGIN CERTIFICATE-----\nAA\n",
        )
        .unwrap();
        std::fs::write(
            root.join("source.key"),
            b"-----BEGIN PRIVATE KEY-----\nBB\n",
        )
        .unwrap();
        Entry {
            instance_id: "i_test".to_owned(),
            domain: "mail.example.test".to_owned(),
            certificate: root.join("source.crt"),
            private_key: root.join("source.key"),
            certificate_binding: "mail_cert_pem".to_owned(),
            private_key_binding: "mail_key_pem".to_owned(),
        }
    }

    fn replace_sources(root: &Path, suffix: &str) {
        std::fs::write(root.join("source.crt"), format!("CERT-{suffix}")).unwrap();
        std::fs::write(root.join("source.key"), format!("KEY-{suffix}")).unwrap();
    }

    #[test]
    fn a_valid_pair_is_validated_into_an_export() {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        let outcome = export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Created);
        assert_eq!(outcome.meta.schema, META_SCHEMA);
        assert_eq!(outcome.meta.certificate.serial, "0a1b");
        assert_eq!(outcome.meta.certificate.not_after, "2027-01-21T00:00:00Z");
        assert!(outcome.meta.previous.is_none());
        assert!(export_dir.join(CERTIFICATE_FILE).is_file());
        assert!(export_dir.join(PRIVATE_KEY_FILE).is_file());
        assert!(export_dir.join(META_FILE).is_file());
        let meta_text = std::fs::read_to_string(export_dir.join(META_FILE)).unwrap();
        assert!(
            !meta_text.contains("PRIVATE KEY"),
            "meta must not carry the key"
        );
        assert!(!meta_text.contains("BEGIN CERTIFICATE"));
        let loaded = load_export(&export_dir).unwrap();
        assert_eq!(loaded.private_key_pem, "-----BEGIN PRIVATE KEY-----\nBB\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_and_meta_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        for name in [PRIVATE_KEY_FILE, META_FILE, CERTIFICATE_FILE] {
            let mode = std::fs::metadata(export_dir.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name} must be 0600");
        }
        let dir_mode = std::fs::metadata(&export_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn an_unchanged_pair_writes_nothing() {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        let before = std::fs::read(export_dir.join(META_FILE)).unwrap();
        let outcome = export(
            &entry,
            Thresholds::default(),
            NOW + 3_600,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Unchanged);
        assert_eq!(std::fs::read(export_dir.join(META_FILE)).unwrap(), before);
        assert!(!export_dir.join("cert.pem.prev").exists());
        assert!(!export_dir.join("key.pem.prev").exists());
    }

    #[test]
    fn a_changed_pair_keeps_the_previous_one_as_prev() {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        let old_certificate = std::fs::read_to_string(export_dir.join(CERTIFICATE_FILE)).unwrap();
        replace_sources(root.path(), "B");
        let mut second = Fake::valid();
        second.certificate.serial_hex = "0c0d".to_owned();
        let outcome = export(
            &entry,
            Thresholds::default(),
            NOW + 86_400,
            &second,
            &export_dir,
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Updated);
        assert_eq!(
            std::fs::read_to_string(export_dir.join("cert.pem.prev")).unwrap(),
            old_certificate
        );
        assert_eq!(
            std::fs::read_to_string(export_dir.join("key.pem.prev")).unwrap(),
            "-----BEGIN PRIVATE KEY-----\nBB\n"
        );
        assert!(export_dir.join("meta.json.prev").is_file());
        let previous = outcome.meta.previous.expect("previous record");
        assert_eq!(previous.serial, "0a1b");
        assert_eq!(previous.exported_at, "2026-11-02T00:00:00Z");
        assert_eq!(outcome.meta.certificate.serial, "0c0d");
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for name in [
            CERTIFICATE_FILE,
            PRIVATE_KEY_FILE,
            META_FILE,
            "cert.pem.prev",
            "key.pem.prev",
            "meta.json.prev",
        ] {
            if let Ok(bytes) = std::fs::read(dir.join(name)) {
                files.push((name.to_owned(), bytes));
            }
        }
        files
    }

    fn rejection(certificate: CertFacts, matches: bool) -> Error {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        let fake = Fake {
            certificate,
            key_matches_certificate: matches,
        };
        export(&entry, Thresholds::default(), NOW, &fake, &export_dir).unwrap_err()
    }

    #[test]
    fn a_key_that_does_not_match_the_certificate_is_refused() {
        let error = rejection(Fake::valid().certificate, false);
        assert_eq!(error.class(), crate::ErrorClass::Validation);
        assert!(error.to_string().contains("does not match"), "{error}");
    }

    #[test]
    fn a_certificate_that_does_not_cover_the_domain_is_refused() {
        let mut certificate = Fake::valid().certificate;
        certificate.sans = vec!["mx.example.test".to_owned()];
        let error = rejection(certificate, true);
        assert_eq!(error.class(), crate::ErrorClass::Validation);
        assert!(error.to_string().contains("SAN"), "{error}");
    }

    #[test]
    fn an_expired_certificate_is_refused() {
        let mut certificate = Fake::valid().certificate;
        certificate.not_before = NOW - 2 * 86_400;
        certificate.not_after = NOW - 1;
        let error = rejection(certificate, true);
        assert!(error.to_string().contains("expired"), "{error}");
    }

    #[test]
    fn a_certificate_that_expires_inside_the_threshold_is_refused() {
        let mut certificate = Fake::valid().certificate;
        certificate.not_after = NOW + 3_600;
        let error = rejection(certificate, true);
        assert!(
            error.to_string().contains("expires within 48 hours"),
            "{error}"
        );
    }

    #[test]
    fn a_certificate_that_is_not_valid_yet_is_refused() {
        let mut certificate = Fake::valid().certificate;
        certificate.not_before = NOW + 86_400;
        let error = rejection(certificate, true);
        assert!(error.to_string().contains("not valid yet"), "{error}");
    }

    #[test]
    fn an_implausible_validity_window_is_refused() {
        let mut certificate = Fake::valid().certificate;
        certificate.not_before = NOW - 86_400;
        certificate.not_after = NOW + 5_000 * 86_400;
        let error = rejection(certificate, true);
        assert!(error.to_string().contains("validity window"), "{error}");
    }

    #[test]
    fn a_soon_expiring_certificate_is_accepted_with_a_warning() {
        let root = workspace();
        let entry = entry_in(root.path());
        let mut fake = Fake::valid();
        fake.certificate.not_after = NOW + 10 * 86_400;
        let outcome = export(
            &entry,
            Thresholds::default(),
            NOW,
            &fake,
            &root.path().join("export"),
        )
        .unwrap();
        assert_eq!(outcome.validation.warnings.len(), 1);
        assert!(outcome.validation.warnings[0].contains("10 days"));
        assert_eq!(
            days_remaining(outcome.validation.certificate.not_after, NOW),
            10
        );
    }

    #[test]
    fn a_rejection_never_touches_the_previous_export() {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        let before = snapshot(&export_dir);
        replace_sources(root.path(), "B");
        let mut broken = Fake::valid();
        broken.key_matches_certificate = false;
        let error = export(
            &entry,
            Thresholds::default(),
            NOW + 86_400,
            &broken,
            &export_dir,
        )
        .unwrap_err();
        assert_eq!(error.class(), crate::ErrorClass::Validation);
        assert_eq!(snapshot(&export_dir), before);
        assert!(!export_dir.join("cert.pem.prev").exists());
    }

    #[test]
    fn an_export_edited_after_the_fact_is_refused() {
        let root = workspace();
        let entry = entry_in(root.path());
        let export_dir = root.path().join("export");
        export(
            &entry,
            Thresholds::default(),
            NOW,
            &Fake::valid(),
            &export_dir,
        )
        .unwrap();
        std::fs::write(export_dir.join(CERTIFICATE_FILE), b"tampered").unwrap();
        let error = load_export(&export_dir).unwrap_err();
        assert_eq!(error.class(), crate::ErrorClass::Io);
        assert!(error.to_string().contains("does not match"), "{error}");
    }

    #[test]
    fn a_missing_export_is_refused_rather_than_invented() {
        let root = workspace();
        let error = load_export(&root.path().join("absent")).unwrap_err();
        assert_eq!(error.class(), crate::ErrorClass::Io);
    }

    #[test]
    fn export_directories_are_per_instance() {
        let root = workspace();
        let entry = entry_in(root.path());
        assert_eq!(
            export_dir_for(root.path(), &entry),
            root.path().join("i_test")
        );
    }
}
