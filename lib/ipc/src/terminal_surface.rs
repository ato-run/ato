//! Wire contract for `ato.terminal-surface.v1`.
//!
//! PTY payload travels as binary WebSocket frames. Only bounded, typed control
//! messages use text frames; terminal bytes are never interpreted as JSON.

use serde::{Deserialize, Serialize};

/// WebSocket subprotocol required by the terminal surface gateway.
pub const TERMINAL_WEBSOCKET_SUBPROTOCOL: &str = "ato.terminal.v1";
/// Maximum PTY input carried by one client binary frame.
pub const MAX_TERMINAL_INPUT_FRAME_BYTES: usize = 64 * 1024;
/// Maximum PTY output carried by one server binary frame.
pub const MAX_TERMINAL_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
/// Maximum UTF-8 JSON control frame size in either direction.
pub const MAX_TERMINAL_CONTROL_FRAME_BYTES: usize = 4 * 1024;
/// Minimum terminal width accepted by the guest broker.
pub const MIN_TERMINAL_COLS: u16 = 2;
/// Maximum terminal width accepted by the guest broker.
pub const MAX_TERMINAL_COLS: u16 = 500;
/// Minimum terminal height accepted by the guest broker.
pub const MIN_TERMINAL_ROWS: u16 = 2;
/// Maximum terminal height accepted by the guest broker.
pub const MAX_TERMINAL_ROWS: u16 = 200;
/// Output window a gateway may send before browser render acknowledgements.
pub const MAX_UNACKED_TERMINAL_OUTPUT_BYTES: usize = 512 * 1024;

/// Browser-to-gateway text frames. PTY input itself is a binary frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientControl {
    Resize { cols: u16, rows: u16 },
    Ack { bytes: u64 },
}

impl TerminalClientControl {
    pub fn validate(&self) -> Result<(), TerminalControlError> {
        match self {
            Self::Resize { cols, rows } => validate_terminal_size(*cols, *rows),
            Self::Ack { bytes } if *bytes == 0 => Err(TerminalControlError::EmptyAck),
            Self::Ack { .. } => Ok(()),
        }
    }
}

/// Gateway-to-browser text frames. PTY output itself is a binary frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerControl {
    Ready {
        cols: u16,
        rows: u16,
    },
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Error {
        code: String,
        message: String,
    },
}

impl TerminalServerControl {
    pub fn validate(&self) -> Result<(), TerminalControlError> {
        match self {
            Self::Ready { cols, rows } => validate_terminal_size(*cols, *rows),
            Self::Exit { code, signal } if code.is_none() && signal.is_none() => {
                Err(TerminalControlError::ExitStatusMissing)
            }
            Self::Exit { .. } => Ok(()),
            Self::Error { code, message }
                if code.trim().is_empty() || message.trim().is_empty() =>
            {
                Err(TerminalControlError::EmptyErrorField)
            }
            Self::Error { .. } => Ok(()),
        }
    }
}

pub fn validate_terminal_size(cols: u16, rows: u16) -> Result<(), TerminalControlError> {
    if !(MIN_TERMINAL_COLS..=MAX_TERMINAL_COLS).contains(&cols) {
        return Err(TerminalControlError::InvalidColumns(cols));
    }
    if !(MIN_TERMINAL_ROWS..=MAX_TERMINAL_ROWS).contains(&rows) {
        return Err(TerminalControlError::InvalidRows(rows));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalControlError {
    #[error("terminal columns {0} are outside the v1 range")]
    InvalidColumns(u16),
    #[error("terminal rows {0} are outside the v1 range")]
    InvalidRows(u16),
    #[error("terminal output acknowledgements must be positive")]
    EmptyAck,
    #[error("terminal exit control requires a code or signal")]
    ExitStatusMissing,
    #[error("terminal error control requires non-empty code and message")]
    EmptyErrorField,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_controls_use_the_v1_wire_shape() {
        let resize = TerminalClientControl::Resize {
            cols: 120,
            rows: 40,
        };
        let ack = TerminalClientControl::Ack { bytes: 65_536 };

        assert_eq!(
            serde_json::to_value(resize).expect("serialize resize"),
            serde_json::json!({ "type": "resize", "cols": 120, "rows": 40 })
        );
        assert_eq!(
            serde_json::to_value(ack).expect("serialize ack"),
            serde_json::json!({ "type": "ack", "bytes": 65_536 })
        );
    }

    #[test]
    fn terminal_size_and_ack_are_bounded() {
        assert!(
            TerminalClientControl::Resize { cols: 1, rows: 40 }
                .validate()
                .is_err()
        );
        assert!(
            TerminalClientControl::Resize {
                cols: 120,
                rows: 201
            }
            .validate()
            .is_err()
        );
        assert!(TerminalClientControl::Ack { bytes: 0 }.validate().is_err());
    }
}
