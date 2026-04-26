//! Capsule Control Protocol (CCP) envelope tolerance.
//!
//! Every JSON object emitted by `ato app {resolve,session start,session stop}`
//! is a CCP envelope (see PAS §4.1, CPDS §4.4.1, and `docs/specs/CCP_SPEC.md`).
//! The wire shape pins three fields:
//!
//! ```text
//! {
//!   "schema_version": "ccp/v1",
//!   "package_id":     "ato/desky",
//!   "action":         "<action>",
//!   ...action-specific payload...
//! }
//! ```
//!
//! This module owns the **consumer-side** rules for how strict to be about
//! `schema_version`. The contract is intentionally permissive so the desktop
//! and CLI can ship on independent release trains within the v1.x lifetime.
//!
//! # Tolerance rules (CCP_SPEC.md §3)
//!
//! | Wire value         | Decision                                       |
//! | ------------------ | ---------------------------------------------- |
//! | absent / `null`    | accept (legacy CLI predating v0.5)             |
//! | `"ccp/v1"`         | accept (native)                                |
//! | `"ccp/v2"`+ later  | accept with warning (best-effort v1 subset)    |
//! | malformed          | reject — caller falls back to opaque error    |
//!
//! "Malformed" = a non-string `schema_version`, or a string that does not
//! match `^ccp/v\d+$`. We do **not** reject envelopes that lack a
//! `schema_version` entirely — that would break the legacy compatibility
//! row of the CCP_SPEC.md §5 matrix.

use tracing::warn;

/// Compatibility verdict for a CCP envelope's `schema_version` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcpCompat {
    /// `schema_version` was absent. Treated as a legacy v1 emitter.
    Legacy,
    /// `schema_version` is `"ccp/v1"` — native, no warning.
    NativeV1,
    /// `schema_version` is `"ccp/vN"` for `N >= 2`. We log a warning and
    /// best-effort parse the v1 subset; the caller proceeds normally.
    FutureMajor(u32),
    /// `schema_version` is present but does not match `^ccp/v\d+$`.
    /// The caller should treat this as a protocol error.
    Malformed,
}

/// Anything that exposes an optional `schema_version` field. The three
/// CCP envelopes (`ResolveEnvelope`, `SessionStartEnvelope`,
/// `SessionStopEnvelope`) all carry it as `Option<String>`.
pub trait HasSchemaVersion {
    fn schema_version(&self) -> Option<&str>;
}

/// Inspect a `schema_version` string and emit the compatibility verdict.
/// Splitting this out from [`enforce_ccp_compat`] keeps the pure logic
/// unit-testable without `tracing` capture.
pub fn classify_schema_version(raw: Option<&str>) -> CcpCompat {
    let Some(value) = raw else {
        return CcpCompat::Legacy;
    };
    let Some(major) = value.strip_prefix("ccp/v") else {
        return CcpCompat::Malformed;
    };
    let Ok(major) = major.parse::<u32>() else {
        return CcpCompat::Malformed;
    };
    match major {
        0 => CcpCompat::Malformed, // ccp/v0 was never released
        1 => CcpCompat::NativeV1,
        n => CcpCompat::FutureMajor(n),
    }
}

/// Run [`classify_schema_version`] and surface the verdict to logs.
/// Returns `Ok(())` for accept-paths (Legacy / NativeV1 / FutureMajor)
/// and `Err(MalformedSchemaVersion)` only when the wire value is
/// present-but-unparseable.
///
/// Callers that already have a deserialized envelope should use this
/// instead of re-deriving the check ad-hoc.
pub fn enforce_ccp_compat<E: HasSchemaVersion>(
    envelope: &E,
    action: &'static str,
) -> Result<(), MalformedSchemaVersion> {
    match classify_schema_version(envelope.schema_version()) {
        CcpCompat::Legacy | CcpCompat::NativeV1 => Ok(()),
        CcpCompat::FutureMajor(n) => {
            warn!(
                action,
                future_major = n,
                "received CCP envelope from a newer CLI (ccp/v{n}); \
                 best-effort parsing the v1 subset"
            );
            Ok(())
        }
        CcpCompat::Malformed => {
            let raw = envelope
                .schema_version()
                .map(str::to_owned)
                .unwrap_or_default();
            warn!(
                action,
                schema_version = %raw,
                "rejecting CCP envelope with malformed schema_version"
            );
            Err(MalformedSchemaVersion { action, raw })
        }
    }
}

/// Surfaced when a CCP envelope arrives with a non-empty but unparseable
/// `schema_version`. The orchestrator converts this into a
/// `LaunchError::Other` / `bail!` so the user gets a typed protocol error
/// instead of a silent best-effort parse.
#[derive(Debug)]
pub struct MalformedSchemaVersion {
    pub action: &'static str,
    pub raw: String,
}

impl std::fmt::Display for MalformedSchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ato-cli emitted a CCP envelope for '{}' with malformed schema_version {:?}; \
             expected absent or 'ccp/vN'",
            self.action, self.raw
        )
    }
}

impl std::error::Error for MalformedSchemaVersion {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_absent_as_legacy() {
        assert_eq!(classify_schema_version(None), CcpCompat::Legacy);
    }

    #[test]
    fn classifies_v1_as_native() {
        assert_eq!(
            classify_schema_version(Some("ccp/v1")),
            CcpCompat::NativeV1
        );
    }

    #[test]
    fn classifies_v2_plus_as_future_major() {
        assert_eq!(
            classify_schema_version(Some("ccp/v2")),
            CcpCompat::FutureMajor(2)
        );
        assert_eq!(
            classify_schema_version(Some("ccp/v17")),
            CcpCompat::FutureMajor(17)
        );
    }

    #[test]
    fn rejects_malformed_strings() {
        for bad in [
            "",
            "ccp",
            "ccp/",
            "ccp/v",
            "ccp/v0",       // never released
            "ccp/vabc",
            "desky-control-plane/v1", // legacy name retired in v0.5
            "v1",
            "ccp/1",
        ] {
            assert_eq!(
                classify_schema_version(Some(bad)),
                CcpCompat::Malformed,
                "expected Malformed for {bad:?}"
            );
        }
    }

    struct StubEnvelope(Option<String>);

    impl HasSchemaVersion for StubEnvelope {
        fn schema_version(&self) -> Option<&str> {
            self.0.as_deref()
        }
    }

    #[test]
    fn enforce_returns_ok_for_legacy_native_and_future() {
        assert!(enforce_ccp_compat(&StubEnvelope(None), "test").is_ok());
        assert!(
            enforce_ccp_compat(&StubEnvelope(Some("ccp/v1".into())), "test").is_ok()
        );
        assert!(
            enforce_ccp_compat(&StubEnvelope(Some("ccp/v2".into())), "test").is_ok()
        );
    }

    #[test]
    fn enforce_returns_err_for_malformed() {
        let err = enforce_ccp_compat(&StubEnvelope(Some("garbage".into())), "test")
            .expect_err("must reject");
        assert_eq!(err.action, "test");
        assert_eq!(err.raw, "garbage");
    }
}
