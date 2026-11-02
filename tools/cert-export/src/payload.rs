//! The two JSON documents this tool produces.
//!
//! 'plan' is what the timer writes to the journal: identifiers, the certificate
//! facts and the action taken. It is safe to keep.
//!
//! 'binding update' is the request body an operator-scoped updater sends. It
//! carries the two Binding values and therefore the private key, so it is
//! written to stdout only by the command whose whole purpose is to produce it,
//! and that command says so on stderr.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::allowlist::Entry;
use crate::export::{Exported, Validation, days_remaining};

pub const UPDATE_SCHEMA: &str = "ato.certificate-binding-update/1";
pub const PLAN_SCHEMA: &str = "ato.certificate-binding-plan/1";
pub const UPDATE_METHOD: &str = "POST";

/// The one endpoint an operator-scoped updater exposes (ADR-044).
pub fn update_path(grant_id: &str) -> String {
    format!("/v1/operator/certificate-bindings/{grant_id}/apply")
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub sans: Vec<String>,
    /// Digest of the certificate bytes, so the audit record can name exactly
    /// which certificate was installed. No key digest is included.
    pub certificate_sha256: String,
    pub exported_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindingUpdate {
    pub schema: String,
    pub method: String,
    pub path: String,
    pub grant_id: String,
    pub instance_id: String,
    pub domain: String,
    /// The durable binding configuration generation this update expects; the
    /// updater refuses to apply over a newer generation.
    pub expected_generation: u64,
    pub values: BTreeMap<String, String>,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub schema: String,
    pub action: String,
    pub instance_id: String,
    pub domain: String,
    pub certificate_binding: String,
    pub private_key_binding: String,
    pub export_dir: String,
    pub serial: String,
    pub sans: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub days_remaining: i64,
    pub certificate_sha256: String,
    pub warnings: Vec<String>,
}

pub fn plan(
    entry: &Entry,
    export_dir: &Path,
    validation: &Validation,
    action: impl Into<String>,
    certificate_sha256: &str,
    now: i64,
) -> Plan {
    let certificate = &validation.certificate;
    Plan {
        schema: PLAN_SCHEMA.to_owned(),
        action: action.into(),
        instance_id: entry.instance_id.clone(),
        domain: entry.domain.clone(),
        certificate_binding: entry.certificate_binding.clone(),
        private_key_binding: entry.private_key_binding.clone(),
        export_dir: export_dir.display().to_string(),
        serial: certificate.serial_hex.clone(),
        sans: certificate.sans.clone(),
        not_before: crate::time::format_iso8601_utc(certificate.not_before),
        not_after: crate::time::format_iso8601_utc(certificate.not_after),
        days_remaining: days_remaining(certificate.not_after, now),
        certificate_sha256: certificate_sha256.to_owned(),
        warnings: validation.warnings.clone(),
    }
}

/// Build the request body from a verified export directory.
pub fn binding_update(
    entry: &Entry,
    exported: &Exported,
    grant_id: &str,
    expected_generation: u64,
) -> BindingUpdate {
    let mut values = BTreeMap::new();
    values.insert(
        entry.certificate_binding.clone(),
        exported.certificate_pem.clone(),
    );
    values.insert(
        entry.private_key_binding.clone(),
        exported.private_key_pem.clone(),
    );
    BindingUpdate {
        schema: UPDATE_SCHEMA.to_owned(),
        method: UPDATE_METHOD.to_owned(),
        path: update_path(grant_id),
        grant_id: grant_id.to_owned(),
        instance_id: exported.meta.instance_id.clone(),
        domain: exported.meta.domain.clone(),
        expected_generation,
        values,
        evidence: Evidence {
            serial: exported.meta.certificate.serial.clone(),
            not_before: exported.meta.certificate.not_before.clone(),
            not_after: exported.meta.certificate.not_after.clone(),
            sans: exported.meta.certificate.sans.clone(),
            certificate_sha256: exported.meta.source.certificate_sha256.clone(),
            exported_at: exported.meta.exported_at.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::CertFacts;
    use crate::export::{Action, CertificateRecord, META_SCHEMA, Meta, SourceDigests};

    fn entry() -> Entry {
        Entry {
            instance_id: "i_test".to_owned(),
            domain: "mail.example.test".to_owned(),
            certificate: "/srv/certs/cert.pem".into(),
            private_key: "/srv/certs/key.pem".into(),
            certificate_binding: "mail_cert_pem".to_owned(),
            private_key_binding: "mail_key_pem".to_owned(),
        }
    }

    fn exported() -> Exported {
        Exported {
            meta: Meta {
                schema: META_SCHEMA.to_owned(),
                instance_id: "i_test".to_owned(),
                domain: "mail.example.test".to_owned(),
                certificate_binding: "mail_cert_pem".to_owned(),
                private_key_binding: "mail_key_pem".to_owned(),
                source: SourceDigests {
                    certificate_sha256: "cert-digest".to_owned(),
                    private_key_sha256: "key-digest".to_owned(),
                    certificate_bytes: 10,
                    private_key_bytes: 20,
                },
                certificate: CertificateRecord {
                    serial: "0a1b".to_owned(),
                    sans: vec!["mail.example.test".to_owned()],
                    not_before: "2026-11-01T00:00:00Z".to_owned(),
                    not_after: "2027-01-21T00:00:00Z".to_owned(),
                    subject: "CN=mail.example.test".to_owned(),
                    issuer: "CN=test".to_owned(),
                },
                exported_at: "2026-11-02T00:00:00Z".to_owned(),
                export_dir: "/var/lib/ato/cert-export/i_test".to_owned(),
                previous: None,
            },
            certificate_pem: "-----BEGIN CERTIFICATE-----\nAA\n-----END CERTIFICATE-----\n"
                .to_owned(),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\nBB\n-----END PRIVATE KEY-----\n"
                .to_owned(),
        }
    }

    #[test]
    fn the_payload_names_the_endpoint_the_ids_and_the_generation() {
        let update = binding_update(&entry(), &exported(), "opcg_01", 7);
        assert_eq!(update.schema, UPDATE_SCHEMA);
        assert_eq!(
            update.path,
            "/v1/operator/certificate-bindings/opcg_01/apply"
        );
        assert_eq!(update.expected_generation, 7);
        assert_eq!(update.values.len(), 2);
        assert_eq!(
            update.values["mail_cert_pem"],
            "-----BEGIN CERTIFICATE-----\nAA\n-----END CERTIFICATE-----\n"
        );
        assert_eq!(update.evidence.serial, "0a1b");
        assert_eq!(update.evidence.certificate_sha256, "cert-digest");
    }

    #[test]
    fn the_payload_carries_no_key_digest() {
        let json =
            serde_json::to_string(&binding_update(&entry(), &exported(), "opcg", 0)).unwrap();
        assert!(!json.contains("key-digest"));
        assert!(!json.contains("key_sha256"));
    }

    #[test]
    fn the_plan_never_contains_pem_or_private_key_material() {
        let validation = Validation {
            certificate: CertFacts {
                serial_hex: "0a1b".to_owned(),
                sans: vec!["mail.example.test".to_owned()],
                not_before: 1_793_779_200,
                not_after: 1_800_000_000,
                not_before_raw: String::new(),
                not_after_raw: String::new(),
                subject: String::new(),
                issuer: String::new(),
            },
            warnings: vec!["the certificate expires in 10 days".to_owned()],
        };
        let plan = plan(
            &entry(),
            Path::new("/var/lib/ato/cert-export/i_test"),
            &validation,
            Action::Updated.as_str(),
            "cert-digest",
            1_793_786_400,
        );
        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("BEGIN"), "{json}");
        assert_eq!(plan.action, "updated");
        assert_eq!(plan.days_remaining, 71);
        assert!(json.contains("the certificate expires in 10 days"));
    }
}
