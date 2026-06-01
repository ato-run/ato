//! Wire types shared by every IPC transport (system capsule, guest capsule,
//! internal harness).
//!
//! All messages entering the desktop IPC layer are normalised into an
//! `IpcRequest`.  The dispatcher produces an `IpcResponse` for every
//! request that carries a `request_id`.  Requests without an `id` are
//! fire-and-forget (legacy envelopes, one-shot notifications).

use serde::{Deserialize, Serialize};

/// Typed response delivered back to the caller.
///
/// For system-capsule WebViews the response is serialised to JSON and
/// delivered via `evaluate_script("window.__atoIpcResolve(id, response)")`.
/// For guest capsules it is serialised into the existing
/// `GuestBridgeResponse` envelope returned over the fetch bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum IpcResponse {
    Ok {
        request_id: Option<u64>,
        payload: serde_json::Value,
    },
    Error {
        request_id: Option<u64>,
        /// Short machine-readable error code (`"unknown_command"`,
        /// `"forbidden"`, `"validation_error"`, etc.).
        code: String,
        message: String,
    },
}

impl IpcResponse {
    pub fn ok(request_id: Option<u64>, payload: serde_json::Value) -> Self {
        Self::Ok {
            request_id,
            payload,
        }
    }

    pub fn error(
        request_id: Option<u64>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Error {
            request_id,
            code: code.into(),
            message: message.into(),
        }
    }

    /// Convenience: unknown command.
    pub fn unknown_command(request_id: Option<u64>, command: &str) -> Self {
        Self::error(
            request_id,
            "unknown_command",
            format!("unknown command: {command}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_round_trips_as_json() {
        let r = IpcResponse::ok(Some(1), serde_json::json!({ "foo": "bar" }));
        let json = serde_json::to_string(&r).unwrap();
        let back: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            IpcResponse::Ok {
                request_id: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn error_carries_code_and_message() {
        let r = IpcResponse::unknown_command(Some(99), "session.start");
        assert!(matches!(
            r,
            IpcResponse::Error { ref code, .. } if code == "unknown_command"
        ));
    }
}
