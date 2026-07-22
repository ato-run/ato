//! nacelle host↔helper IPC wire types.
//!
//! `NacelleEvent` (nacelle stdout → host) and `TerminalCommand` (host → nacelle
//! stdin, interactive PTY) are the NDJSON contract the host and the nacelle
//! helper exchange. Single-sourced here so the desktop shell stops hand-writing
//! and hand-parsing JSON that must match nacelle's schema; nacelle re-exports
//! these from `nacelle::internal_api` (keeping its `.emit()` behaviour there, as
//! stdout I/O has no place in this dependency-light wire crate).
//!
//! Not included: nacelle's `ExecEnvelope` (its versioned execution-input
//! contract) — a larger, central nacelle type folded in separately.

use serde::{Deserialize, Serialize};

pub const CURRENT_SPEC_VERSION: &str = "1.0";
pub const NEXT_SPEC_VERSION: &str = "2.0";
pub const LEGACY_SPEC_VERSION: &str = "0.1.0";

/// Validate an envelope `spec_version` against the supported set.
pub fn validate_spec_version(spec_version: &str) -> Result<(), String> {
    if is_supported_spec_version(spec_version) {
        return Ok(());
    }

    Err(format!(
        "Unsupported spec_version '{spec_version}'. Supported versions: {CURRENT_SPEC_VERSION}, {NEXT_SPEC_VERSION}, {LEGACY_SPEC_VERSION}"
    ))
}

/// Whether `spec_version` is one nacelle accepts.
pub fn is_supported_spec_version(spec_version: &str) -> bool {
    spec_version == CURRENT_SPEC_VERSION
        || spec_version == NEXT_SPEC_VERSION
        || spec_version == LEGACY_SPEC_VERSION
}

/// An artifact a workload exported (reported in `ExecutionCompleted`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportedArtifact {
    pub kind: String,
    pub relative_path: String,
    pub size_bytes: u64,
}

/// Events emitted by nacelle on stdout (one NDJSON line each).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NacelleEvent {
    IpcReady {
        service: String,
        endpoint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    /// The workload was launched but there is no readiness signal (no probe
    /// declared, and no port to synthesize a conservative probe from). This is
    /// the honest "started, not ready" state — it must NEVER be treated as
    /// ready. Distinct from `IpcReady`, which is emitted only on real probe
    /// success.
    ServiceStarted {
        service: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    ServiceExited {
        service: String,
        exit_code: Option<i32>,
    },
    ExecutionCompleted {
        service: String,
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        derived_output_path: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exported_artifacts: Vec<ExportedArtifact>,
        cleanup_policy_applied: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// PTY terminal data chunk (base64-encoded raw bytes)
    TerminalData {
        session_id: String,
        /// Base64-encoded raw terminal output bytes
        data_b64: String,
    },
    /// PTY terminal session exited
    TerminalExited {
        session_id: String,
        exit_code: Option<i32>,
    },
}

/// Commands sent from the host to nacelle via stdin (interactive PTY sessions).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalCommand {
    /// Keyboard/text input to forward to the PTY master.
    TerminalInput {
        session_id: String,
        /// Base64-encoded bytes to write to PTY master.
        data_b64: String,
    },
    /// Resize the PTY.
    TerminalResize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    /// Send a signal to the PTY child process.
    TerminalSignal {
        session_id: String,
        /// Signal name: "SIGINT" | "SIGTERM" | "SIGHUP".
        signal: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_current_and_legacy_spec_versions() {
        assert!(is_supported_spec_version(CURRENT_SPEC_VERSION));
        assert!(is_supported_spec_version(NEXT_SPEC_VERSION));
        assert!(is_supported_spec_version(LEGACY_SPEC_VERSION));
        assert!(validate_spec_version(CURRENT_SPEC_VERSION).is_ok());
        assert!(validate_spec_version(NEXT_SPEC_VERSION).is_ok());
        assert!(validate_spec_version(LEGACY_SPEC_VERSION).is_ok());
        assert!(validate_spec_version("3.0").is_err());
    }

    #[test]
    fn nacelle_event_terminal_data_round_trips() {
        let ev = NacelleEvent::TerminalData {
            session_id: "sess_1".to_string(),
            data_b64: "aGVsbG8=".to_string(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"event\":\"terminal_data\""));
        let back: NacelleEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn terminal_command_input_round_trips() {
        let cmd = TerminalCommand::TerminalInput {
            session_id: "sess_1".to_string(),
            data_b64: "YQ==".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"terminal_input\""));
    }
}
