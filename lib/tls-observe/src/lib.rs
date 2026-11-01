//! Observe the TLS certificate a logical port presents.
//!
//! The probe connects to an already-resolved socket address, optionally runs
//! the SMTP STARTTLS preamble, performs a TLS handshake with SNI, and reports
//! what the server presented: serial number, SAN dNSNames, validity, chain
//! length, and whether that chain validated against the configured anchors.
//!
//! Facts are reported even when certificate validation rejects the
//! certificate: a port that serves the wrong certificate is still an
//! observation, and callers decide the verdict from the facts. Only the
//! absence of a certificate (no connection, timeout, SMTP refusal, no
//! certificate message) is an error.
//!
//! Trust anchors are public roots in production (webpki-roots, the Mozilla
//! set); TestOnly injects a private anchor for tests. The observation does not
//! depend on a process-wide crypto provider: it installs the ring provider on
//! the ClientConfig it builds.

#![forbid(unsafe_code)]

mod der;
mod smtp;

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring;
use rustls::pki_types::{ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};
use rustls_pki_types::CertificateDer;

/// How TLS starts on the observed port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsStart {
    /// TLS from the first byte of the connection.
    Implicit,
    /// Plaintext SMTP until the STARTTLS handshake.
    SmtpStarttls,
}

impl TlsStart {
    /// The stable contract spelling of this start mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implicit => "implicit",
            Self::SmtpStarttls => "smtp-starttls",
        }
    }
}

/// Where a presented certificate chain has to anchor to be trusted.
#[derive(Debug, Clone)]
pub enum TrustAnchors {
    /// The Mozilla public root set (webpki-roots).
    PublicRoots,
    /// Exactly these certificates, for tests with private material.
    TestOnly(Vec<CertificateDer<'static>>),
}

impl TrustAnchors {
    fn label(&self) -> &'static str {
        match self {
            Self::PublicRoots => "public-roots",
            Self::TestOnly(_) => "test-only",
        }
    }
}

/// What a port presented, at the time it was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFacts {
    /// The name the probe asked for (the SNI the connection carried).
    pub server_name: String,
    /// How TLS started on the port.
    pub start: TlsStart,
    /// Leaf serialNumber as lowercase hex, without separators.
    pub presented_serial: String,
    /// dNSName SAN entries, ASCII-lowercased, deduped, in presented order.
    pub san: Vec<String>,
    /// notBefore as RFC 3339 UTC, for example 2026-11-02T00:00:00Z.
    pub not_before: String,
    /// notAfter as RFC 3339 UTC.
    pub not_after: String,
    /// Whole days from the observation time to notAfter, floored.
    pub days_remaining: i64,
    /// Presented leaf plus intermediates.
    pub chain_length: usize,
    /// public-roots or test-only.
    pub trust_anchor: String,
    /// Whether the chain validated against the anchors at observation time.
    pub trusted: bool,
    /// Whether the SAN covers the server name (exact or one-label wildcard).
    pub server_name_matches: bool,
    /// Whether the observation time was inside the validity period.
    pub within_validity: bool,
}

/// Why no certificate could be observed.
#[derive(Debug, thiserror::Error)]
#[error("TLS observation failed: {reason}")]
pub struct TlsProbeError {
    reason: String,
}

impl TlsProbeError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Why the observation could not be made.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

type PresentedSlot = Arc<Mutex<Option<PresentedCertificate>>>;

/// The end-entity and intermediates as presented, with the chain verdict.
///
/// Recorded before the inner verifier runs, so a rejected certificate still
/// yields facts.
#[derive(Debug, Clone)]
struct PresentedCertificate {
    end_entity: CertificateDer<'static>,
    intermediates: Vec<CertificateDer<'static>>,
    chain_verified: bool,
}

/// A verifier that records what the server presented and then delegates.
#[derive(Debug)]
struct RecordingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    presented: PresentedSlot,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verification = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(PresentedCertificate {
                end_entity: CertificateDer::from(end_entity.as_ref().to_vec()),
                intermediates: intermediates
                    .iter()
                    .map(|certificate| CertificateDer::from(certificate.as_ref().to_vec()))
                    .collect(),
                chain_verified: verification.is_ok(),
            });
        }
        verification
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Observe the certificate a port presents.
///
/// The caller resolves the logical port to a socket address and names the
/// server name the certificate must cover. Every network operation is bounded
/// by the timeout; SMTP replies are additionally bounded to 512-byte lines and
/// 64 lines per reply.
///
/// Returns Ok with facts whenever a certificate was presented, even if it is
/// untrusted, expired, or for another name. Returns Err when no certificate
/// could be obtained, or when a presented certificate cannot be read as an
/// X.509 certificate at all.
pub fn observe_tls(
    address: SocketAddr,
    server_name: &str,
    start: TlsStart,
    timeout: Duration,
    anchors: TrustAnchors,
) -> Result<TlsFacts, TlsProbeError> {
    if timeout.is_zero() {
        return Err(TlsProbeError::new(
            "the observation timeout must be greater than zero",
        ));
    }

    let now_epoch = unix_now();
    let server_name_owned = ServerName::try_from(server_name.to_owned()).map_err(|err| {
        TlsProbeError::new(format!(
            "{server_name:?} is not a valid TLS server name: {err}"
        ))
    })?;

    let mut tcp = TcpStream::connect_timeout(&address, timeout)
        .map_err(|err| TlsProbeError::new(format!("could not connect to {address}: {err}")))?;
    tcp.set_read_timeout(Some(timeout))
        .map_err(|err| TlsProbeError::new(format!("could not bound the read timeout: {err}")))?;
    tcp.set_write_timeout(Some(timeout))
        .map_err(|err| TlsProbeError::new(format!("could not bound the write timeout: {err}")))?;

    if start == TlsStart::SmtpStarttls {
        smtp::start_tls(&mut tcp, server_name)?;
    }

    let (config, presented) = client_config(&anchors)?;
    let connection = ClientConnection::new(Arc::new(config), server_name_owned)
        .map_err(|err| TlsProbeError::new(format!("could not start the TLS handshake: {err}")))?;

    let mut stream = StreamOwned::new(connection, tcp);
    let handshake_error = drive_handshake(&mut stream)
        .err()
        .map(|err| err.to_string());

    let certificate = presented.lock().ok().and_then(|slot| slot.clone());
    let Some(certificate) = certificate else {
        let reason = match handshake_error {
            Some(err) => format!("the TLS handshake presented no certificate: {err}"),
            None => "the TLS handshake completed without presenting a certificate".to_string(),
        };
        return Err(TlsProbeError::new(reason));
    };

    build_facts(certificate, server_name, start, anchors.label(), now_epoch)
}

fn drive_handshake(stream: &mut StreamOwned<ClientConnection, TcpStream>) -> io::Result<()> {
    stream.conn.complete_io(&mut stream.sock).map(|_| ())
}

fn build_facts(
    presented: PresentedCertificate,
    server_name: &str,
    start: TlsStart,
    trust_anchor: &'static str,
    now_epoch: i64,
) -> Result<TlsFacts, TlsProbeError> {
    let leaf = der::parse_certificate(presented.end_entity.as_ref()).map_err(|reason| {
        TlsProbeError::new(format!(
            "the presented certificate could not be read: {reason}"
        ))
    })?;

    let server_name_matches = der::server_name_matches(&leaf.san, server_name);
    let days_remaining = (leaf.not_after_epoch - now_epoch).div_euclid(der::SECONDS_PER_DAY);

    Ok(TlsFacts {
        server_name: server_name.to_owned(),
        start,
        presented_serial: leaf.serial_hex,
        san: leaf.san,
        not_before: format_rfc3339(leaf.not_before_epoch)?,
        not_after: format_rfc3339(leaf.not_after_epoch)?,
        days_remaining,
        chain_length: 1 + presented.intermediates.len(),
        trust_anchor: trust_anchor.to_string(),
        trusted: presented.chain_verified,
        server_name_matches,
        within_validity: now_epoch >= leaf.not_before_epoch && now_epoch <= leaf.not_after_epoch,
    })
}

fn client_config(anchors: &TrustAnchors) -> Result<(ClientConfig, PresentedSlot), TlsProbeError> {
    let provider = Arc::new(ring::default_provider());
    let mut roots = RootCertStore::empty();
    match anchors {
        TrustAnchors::PublicRoots => {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        TrustAnchors::TestOnly(certificates) => {
            for certificate in certificates {
                roots.add(certificate.clone()).map_err(|err| {
                    TlsProbeError::new(format!(
                        "test-only trust anchor is not a usable root certificate: {err}"
                    ))
                })?;
            }
        }
    }

    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|err| {
            TlsProbeError::new(format!("could not build the certificate verifier: {err}"))
        })?;
    let presented = Arc::new(Mutex::new(None));
    let verifier = Arc::new(RecordingVerifier {
        inner,
        presented: presented.clone(),
    });

    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| {
            TlsProbeError::new(format!("could not select TLS protocol versions: {err}"))
        })?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    Ok((config, presented))
}

fn format_rfc3339(epoch: i64) -> Result<String, TlsProbeError> {
    let value = time::OffsetDateTime::from_unix_timestamp(epoch).map_err(|err| {
        TlsProbeError::new(format!(
            "certificate time {epoch} is not representable: {err}"
        ))
    })?;
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| TlsProbeError::new(format!("could not format certificate time: {err}")))
}

fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_modes_have_stable_contract_spellings() {
        assert_eq!(TlsStart::Implicit.as_str(), "implicit");
        assert_eq!(TlsStart::SmtpStarttls.as_str(), "smtp-starttls");
    }

    #[test]
    fn anchors_are_labelled_as_the_contract_expects() {
        assert_eq!(TrustAnchors::PublicRoots.label(), "public-roots");
        assert_eq!(TrustAnchors::TestOnly(Vec::new()).label(), "test-only");
    }

    #[test]
    fn rfc3339_utc_has_no_subseconds_or_offset() {
        assert_eq!(
            format_rfc3339(1_767_225_600).unwrap(),
            "2026-01-01T00:00:00Z"
        );
        assert!(format_rfc3339(i64::MAX).is_err());
    }

    #[test]
    fn a_zero_timeout_is_rejected_before_any_io() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let result = observe_tls(
            address,
            "mail.ato.test",
            TlsStart::Implicit,
            Duration::ZERO,
            TrustAnchors::PublicRoots,
        );
        assert!(result.is_err());
    }

    #[test]
    fn invalid_server_names_are_rejected_before_any_io() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let result = observe_tls(
            address,
            "not a dns name",
            TlsStart::Implicit,
            Duration::from_millis(10),
            TrustAnchors::PublicRoots,
        );
        assert!(result.is_err());
    }

    #[test]
    fn tls_probe_error_display_carries_the_reason() {
        let error = TlsProbeError::new("no route to host");
        assert_eq!(
            error.to_string(),
            "TLS observation failed: no route to host"
        );
        assert_eq!(error.reason(), "no route to host");
    }
}
