//! Validated `ato://` runtime intents emitted by the embedded PWA.
//!
//! The embedded PWA never calls the Runtime Control *write* API directly.
//! Instead it navigates to an `ato://<verb>/<id>` intent URL; the Desktop is the
//! only party with privileged authority. The native side:
//!
//! 1. checks the navigation came from a **trusted** origin
//!    ([`is_trusted_intent_origin`]),
//! 2. parses and schema-validates the verb + payload
//!    ([`parse_runtime_intent`]) — unknown verbs and malformed payloads are
//!    rejected, never executed,
//! 3. for privileged intents ([`RuntimeIntent::requires_confirmation`]) performs
//!    native confirmation before executing.
//!
//! This keeps `stop` / `run` authority on the native side and out of page JS.
//!
//! All runtime intents are namespaced under the `runtime` host as
//! `ato://runtime/<verb>/<id>` so they never collide with the existing `ato://`
//! routes (`app`, `open`, `cli`, `dock`):
//! * `ato://runtime/open/<session_id>`  — open/focus a running local session (non-privileged)
//! * `ato://runtime/stop/<session_id>`  — request stop of a local session (privileged)
//! * `ato://runtime/run/<install_profile_key>` — request launch of an installed app (privileged)
//!
//! Any other `ato://` host (e.g. `app`) parses to [`IntentError::NotAnIntent`]
//! so the caller falls through to the existing routing.

use url::Url;

/// A validated runtime intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeIntent {
    /// Open/focus the window for a running local session. Non-privileged.
    OpenSession { session_id: String },
    /// Request stop of a running local session. Privileged → native confirm.
    StopSession { session_id: String },
    /// Request launch of an installed app by `install_profile_key`. Privileged.
    RunApp { install_profile_key: String },
}

impl RuntimeIntent {
    /// Whether executing this intent requires explicit native confirmation
    /// before it runs. Read/observe-style actions (open) do not; actions that
    /// start or stop work (run / stop) do.
    pub(crate) fn requires_confirmation(&self) -> bool {
        matches!(
            self,
            RuntimeIntent::StopSession { .. } | RuntimeIntent::RunApp { .. }
        )
    }

    /// Stable verb label for logging/telemetry (never includes the payload).
    pub(crate) fn verb(&self) -> &'static str {
        match self {
            RuntimeIntent::OpenSession { .. } => "open",
            RuntimeIntent::StopSession { .. } => "stop",
            RuntimeIntent::RunApp { .. } => "run",
        }
    }
}

/// Why an `ato://` URL was not accepted as a runtime intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IntentError {
    /// Not a runtime-intent URL at all (wrong scheme, or a non-intent `ato://`
    /// host such as `app`). The caller should fall through to other routing
    /// rather than treat this as a rejection.
    NotAnIntent,
    /// A runtime-intent verb we don't recognise — rejected.
    UnknownVerb(String),
    /// A recognised verb with a malformed / unsafe payload — rejected.
    InvalidPayload(String),
}

/// Maximum length for an id payload (session id / install profile key).
const MAX_ID_LEN: usize = 256;

/// Validate an id payload: non-empty, bounded, and limited to a safe charset so
/// a crafted intent cannot smuggle path traversal, whitespace, or control
/// characters into downstream lookups. `ipk_<hex>` / `sess_<hex>` and the
/// `<key>::<profile>` form all satisfy this.
fn validate_id(kind: &str, id: &str) -> Result<String, IntentError> {
    if id.is_empty() {
        return Err(IntentError::InvalidPayload(format!("{kind} is empty")));
    }
    if id.len() > MAX_ID_LEN {
        return Err(IntentError::InvalidPayload(format!("{kind} is too long")));
    }
    let ok = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
    if !ok {
        return Err(IntentError::InvalidPayload(format!(
            "{kind} contains unsupported characters"
        )));
    }
    Ok(id.to_string())
}

/// Whether a raw URL is in the runtime-intent namespace (`ato://runtime/...`).
/// Lets callers cheaply decide to route to [`parse_runtime_intent`] before
/// falling through to other `ato://` handling.
pub(crate) fn is_runtime_intent_url(raw: &str) -> bool {
    let t = raw.trim();
    t.starts_with("ato://runtime/") || t == "ato://runtime"
}

/// Parse an `ato://runtime/<verb>/<id>` runtime intent.
pub(crate) fn parse_runtime_intent(raw: &str) -> Result<RuntimeIntent, IntentError> {
    let trimmed = raw.trim();
    let Ok(parsed) = Url::parse(trimmed) else {
        return Err(IntentError::NotAnIntent);
    };
    if parsed.scheme() != "ato" {
        return Err(IntentError::NotAnIntent);
    }
    // Only the `runtime` host carries runtime intents; everything else
    // (`app`, `open`, `cli`, `dock`, …) is handled by the existing routes.
    if parsed.host_str() != Some("runtime") {
        return Err(IntentError::NotAnIntent);
    }
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|seg| !seg.is_empty()).collect())
        .unwrap_or_default();
    // Shape: ato://runtime/<verb>/<id> → segments == [verb, id].
    if segments.len() != 2 {
        return Err(IntentError::InvalidPayload(
            "expected ato://runtime/<verb>/<id>".to_string(),
        ));
    }
    let (verb, id) = (segments[0], segments[1]);
    match verb {
        "open" => Ok(RuntimeIntent::OpenSession {
            session_id: validate_id("session_id", id)?,
        }),
        "stop" => Ok(RuntimeIntent::StopSession {
            session_id: validate_id("session_id", id)?,
        }),
        "run" => Ok(RuntimeIntent::RunApp {
            install_profile_key: validate_id("install_profile_key", id)?,
        }),
        other => Err(IntentError::UnknownVerb(other.to_string())),
    }
}

/// Whether a runtime intent may be honored from `origin`. Only the trusted PWA
/// origins (prod `app.ato.run` / `stg-app.ato.run`, or loopback dev origins in
/// debug builds) may drive privileged local actions. An OS-level deep link with
/// no web origin, or any untrusted page, is refused.
pub(crate) fn is_trusted_intent_origin(origin: Option<&str>, dev_mode: bool) -> bool {
    let Some(origin) = origin else {
        return false;
    };
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    crate::pwa_home::is_trusted_pwa_origin(&url, dev_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_stop_run() {
        assert_eq!(
            parse_runtime_intent("ato://runtime/open/sess_abc"),
            Ok(RuntimeIntent::OpenSession {
                session_id: "sess_abc".into()
            })
        );
        assert_eq!(
            parse_runtime_intent("ato://runtime/stop/sess_abc"),
            Ok(RuntimeIntent::StopSession {
                session_id: "sess_abc".into()
            })
        );
        assert_eq!(
            parse_runtime_intent("ato://runtime/run/ipk_deadbeef::default"),
            Ok(RuntimeIntent::RunApp {
                install_profile_key: "ipk_deadbeef::default".into()
            })
        );
    }

    #[test]
    fn is_runtime_intent_url_matches_namespace_only() {
        assert!(is_runtime_intent_url("ato://runtime/stop/s"));
        assert!(!is_runtime_intent_url("ato://app/ipk"));
        assert!(!is_runtime_intent_url("https://app.ato.run/"));
    }

    #[test]
    fn confirmation_policy() {
        assert!(!RuntimeIntent::OpenSession { session_id: "s".into() }.requires_confirmation());
        assert!(RuntimeIntent::StopSession { session_id: "s".into() }.requires_confirmation());
        assert!(
            RuntimeIntent::RunApp {
                install_profile_key: "k".into()
            }
            .requires_confirmation()
        );
    }

    #[test]
    fn unknown_verb_is_rejected() {
        assert_eq!(
            parse_runtime_intent("ato://runtime/franchise/sess_abc"),
            Err(IntentError::UnknownVerb("franchise".into()))
        );
    }

    #[test]
    fn non_runtime_hosts_fall_through_not_rejected() {
        // ato://app/<ipk> is handled by the existing installed-app route.
        assert_eq!(
            parse_runtime_intent("ato://app/ipk_abc"),
            Err(IntentError::NotAnIntent)
        );
        // ato://open?handle=... and ato://cli are existing routes, not intents.
        assert_eq!(
            parse_runtime_intent("ato://open?handle=acme%2Fchat"),
            Err(IntentError::NotAnIntent)
        );
    }

    #[test]
    fn non_ato_scheme_falls_through() {
        assert_eq!(
            parse_runtime_intent("https://app.ato.run/runtime/run/x"),
            Err(IntentError::NotAnIntent)
        );
        assert_eq!(parse_runtime_intent("not a url"), Err(IntentError::NotAnIntent));
    }

    #[test]
    fn invalid_payload_is_rejected() {
        // Missing id (verb only).
        assert!(matches!(
            parse_runtime_intent("ato://runtime/stop"),
            Err(IntentError::InvalidPayload(_))
        ));
        // Extra path segments.
        assert!(matches!(
            parse_runtime_intent("ato://runtime/stop/a/b"),
            Err(IntentError::InvalidPayload(_))
        ));
        // Path traversal / unsafe characters.
        assert!(matches!(
            parse_runtime_intent("ato://runtime/stop/..%2f..%2fetc"),
            Err(IntentError::InvalidPayload(_))
        ));
        assert!(matches!(
            parse_runtime_intent("ato://runtime/run/a b"),
            Err(IntentError::InvalidPayload(_))
        ));
    }

    #[test]
    fn untrusted_origin_is_refused() {
        assert!(is_trusted_intent_origin(Some("https://app.ato.run"), false));
        assert!(is_trusted_intent_origin(Some("https://stg-app.ato.run"), false));
        assert!(!is_trusted_intent_origin(Some("https://evil.example.com"), false));
        // No origin (OS-level deep link) is refused for privileged intents.
        assert!(!is_trusted_intent_origin(None, false));
        // Loopback only in dev.
        assert!(is_trusted_intent_origin(Some("http://localhost:5173"), true));
        assert!(!is_trusted_intent_origin(Some("http://localhost:5173"), false));
    }
}
