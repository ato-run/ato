//! Guest Bridge IPC envelope types (§9, Part II of the Capsule Protocol spec).
//!
//! These types define the JSON message envelopes exchanged between the host process
//! (ato-desktop or ato-cli broker) and a guest capsule over the Unix socket bridge.
//!
//! ## Wire format
//!
//! Every message is a newline-delimited JSON object.  The discriminator field (`kind` for
//! requests, `status` for responses) selects the variant; all field names are kebab-case.
//!
//! ## Capability gating
//!
//! Before honouring any [`GuestBridgeRequest::Invoke`] the host MUST verify that the
//! session's [`CapabilityGrant`] set contains the required capability.  Requests that
//! arrive without a valid grant MUST receive [`GuestBridgeResponse::Denied`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Capabilities that a guest capsule session may be granted by the host.
///
/// Grants are issued at session creation time and are NOT renegotiable during
/// a running session.  The host enforces them on every [`GuestBridgeRequest::Invoke`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityGrant {
    /// Read access to files declared in the capsule's state bindings.
    ReadFile,
    /// Write access to files declared in the capsule's state bindings.
    WriteFile,
    /// Read access to environment variables declared in `[secrets]`.
    ReadEnv,
    /// Outbound network access (subject to `egress_allow` / `egress_id_allow`).
    Network,
    /// Spawn child processes via the host orchestrator.
    SpawnProcess,
    /// Access to clipboard contents (desktop hosts only).
    Clipboard,
    /// Access to screen / screenshot APIs (desktop hosts only).
    ScreenCapture,
}

/// Requests sent from the guest capsule to the host broker.
///
/// Serialised with `"kind"` as the tag discriminator, all variant names are kebab-case.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum GuestBridgeRequest {
    /// Initial handshake.  The guest identifies itself with its session token.
    Handshake {
        /// Opaque session token issued by the host at capsule launch.
        session: String,
    },

    /// Invoke a host-side command.  The host checks `capability` against the session's
    /// [`CapabilityGrant`] set before executing.
    Invoke {
        /// Monotonically increasing request identifier (unique within the session).
        request_id: u64,
        /// Command name (e.g. `"read-file"`, `"write-file"`, `"env-get"`).
        command: String,
        /// Required capability for this command (checked server-side).
        capability: String,
        /// Command-specific payload.
        #[serde(default)]
        payload: Value,
    },

    /// Graceful shutdown request from the guest.
    Shutdown {
        /// Optional exit code hint.
        #[serde(default)]
        exit_code: Option<i32>,
    },
}

/// Responses sent from the host broker to the guest capsule.
///
/// Serialised with `"status"` as the tag discriminator, all variant names are kebab-case.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum GuestBridgeResponse {
    /// Request succeeded.
    Ok {
        /// Echoes the `request_id` from the originating `Invoke`, or `null` for
        /// unsolicited push messages (e.g. capability-revoked notifications).
        #[serde(default)]
        request_id: Option<u64>,
        /// Human-readable summary.
        message: String,
        /// Command-specific response payload.
        #[serde(default)]
        payload: Value,
    },

    /// Request was syntactically valid but denied by the capability gate.
    Denied {
        #[serde(default)]
        request_id: Option<u64>,
        /// Reason for denial.
        message: String,
    },

    /// Request could not be completed due to a host-side error.
    Error {
        #[serde(default)]
        request_id: Option<u64>,
        /// Human-readable error description.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handshake_roundtrip() {
        let req = GuestBridgeRequest::Handshake {
            session: "tok_abc123".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""kind":"handshake""#));
        let back: GuestBridgeRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, GuestBridgeRequest::Handshake { .. }));
    }

    #[test]
    fn invoke_roundtrip() {
        let req = GuestBridgeRequest::Invoke {
            request_id: 42,
            command: "read-file".to_string(),
            capability: "read-file".to_string(),
            payload: json!({"path": "/data/notes.txt"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""kind":"invoke""#));
        assert!(json.contains("42"));
        let back: GuestBridgeRequest = serde_json::from_str(&json).unwrap();
        if let GuestBridgeRequest::Invoke {
            request_id,
            command,
            ..
        } = back
        {
            assert_eq!(request_id, 42);
            assert_eq!(command, "read-file");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn response_ok_roundtrip() {
        let resp = GuestBridgeResponse::Ok {
            request_id: Some(42),
            message: "done".to_string(),
            payload: json!({"bytes": 1024}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"ok""#));
        let back: GuestBridgeResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, GuestBridgeResponse::Ok { .. }));
    }

    #[test]
    fn response_denied_roundtrip() {
        let resp = GuestBridgeResponse::Denied {
            request_id: Some(7),
            message: "capability network not granted".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"denied""#));
    }

    #[test]
    fn capability_grant_serde() {
        let g = CapabilityGrant::ReadFile;
        assert_eq!(serde_json::to_string(&g).unwrap(), r#""read-file""#);
        let back: CapabilityGrant = serde_json::from_str(r#""write-file""#).unwrap();
        assert_eq!(back, CapabilityGrant::WriteFile);
    }
}
