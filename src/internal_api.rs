use serde::{Deserialize, Serialize};

pub const CURRENT_SPEC_VERSION: &str = "1.0";
pub const NEXT_SPEC_VERSION: &str = "2.0";
pub const LEGACY_SPEC_VERSION: &str = "0.1.0";

pub fn validate_spec_version(spec_version: &str) -> Result<(), String> {
    if is_supported_spec_version(spec_version) {
        return Ok(());
    }

    Err(format!(
        "Unsupported spec_version '{spec_version}'. Supported versions: {CURRENT_SPEC_VERSION}, {NEXT_SPEC_VERSION}, {LEGACY_SPEC_VERSION}"
    ))
}

pub fn is_supported_spec_version(spec_version: &str) -> bool {
    spec_version == CURRENT_SPEC_VERSION
        || spec_version == NEXT_SPEC_VERSION
        || spec_version == LEGACY_SPEC_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportedArtifact {
    pub kind: String,
    pub relative_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NacelleEvent {
    IpcReady {
        service: String,
        endpoint: String,
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

impl NacelleEvent {
    pub fn emit(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            println!("{}", json);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
}

/// Commands sent from ato-cli to nacelle via stdin (interactive PTY sessions)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalCommand {
    /// Keyboard/text input to forward to the PTY master
    TerminalInput {
        session_id: String,
        /// Base64-encoded bytes to write to PTY master
        data_b64: String,
    },
    /// Resize the PTY
    TerminalResize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    /// Send a signal to the PTY child process
    TerminalSignal {
        session_id: String,
        /// Signal name: "SIGINT" | "SIGTERM" | "SIGHUP"
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
}
