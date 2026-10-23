//! Declared native Adapter behavior of one Derivation. Physical addresses,
//! processes, authorization and expected Contract answers never belong here.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authoring::{BoundPort, BoundStep, HTTP_PROTOCOL, TCP_PROTOCOL};

pub const NATIVE_SURFACE_RUNTIME_FEATURE: &str = "runtime_feature=native_surface_probe_v1";
pub const TERMINAL_PORT_PROTOCOL: &str = "ato.pty@1";
/// RFB semantics belong to the Adapter/profile; the logical transport reuses
/// the existing opaque TCP Port instead of inventing a registered Protocol.
pub const PIXEL_PORT_PROTOCOL: &str = TCP_PROTOCOL;

/// Native presentation and an independent, private application-observation
/// Port. The probe returns measured bytes, never a claim that K was satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSurfaceAdapter {
    pub surface: NativePresentation,
    pub probe_port: String,
    pub operations: Vec<NativeProbeOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePresentation {
    Terminal {
        port: String,
        cols: u16,
        rows: u16,
    },
    PixelStream {
        port: String,
        width: u16,
        height: u16,
    },
}

impl NativePresentation {
    pub fn port(&self) -> &str {
        match self {
            Self::Terminal { port, .. } | Self::PixelStream { port, .. } => port,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Terminal { .. } => "terminal",
            Self::PixelStream { .. } => "pixel_stream",
        }
    }

    pub fn profile(&self) -> &'static str {
        match self {
            Self::Terminal { .. } => "ato.terminal-surface.v1",
            Self::PixelStream { .. } => "ato.pixel-stream.v1",
        }
    }

    pub fn protocol(&self) -> &'static str {
        match self {
            Self::Terminal { .. } => TERMINAL_PORT_PROTOCOL,
            Self::PixelStream { .. } => PIXEL_PORT_PROTOCOL,
        }
    }
}

/// Bounded input followed by measured output. An Adapter performs these in
/// authored order before verification and freezes each result at `path`.
/// Repeated HTTP GETs retrieve that result; they must never replay the input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeProbeOperation {
    TerminalExchange {
        path: String,
        /// Initial catalog profile: printable ASCII plus CR/LF. Interactive
        /// viewer input is separate and retains its existing byte protocol.
        input: Vec<u8>,
        /// Completion framing, not an expected answer or a K digest. Included
        /// in captured output, and forbidden in possible line-discipline echo
        /// output to avoid accepting the input echo as app completion.
        output_delimiter: Vec<u8>,
        max_output_bytes: u32,
        timeout_ms: u64,
    },
    PixelKeyFrame {
        path: String,
        keysyms: Vec<u32>,
        /// Exact full-frame B,G,R,X bytes. RFB pixel format is 32 bpp,
        /// depth 24, little-endian, true-colour, max 255, shifts 16/8/0.
        max_frame_bytes: u32,
        timeout_ms: u64,
    },
}

impl NativeProbeOperation {
    pub fn path(&self) -> &str {
        match self {
            Self::TerminalExchange { path, .. } | Self::PixelKeyFrame { path, .. } => path,
        }
    }

    pub fn timeout_ms(&self) -> u64 {
        match self {
            Self::TerminalExchange { timeout_ms, .. } | Self::PixelKeyFrame { timeout_ms, .. } => {
                *timeout_ms
            }
        }
    }
}

impl NativeSurfaceAdapter {
    /// Bounded Adapter catalog validation, shared with the physical Adapter.
    /// This grants no execution, endpoint access or Contract satisfaction.
    pub fn validate_catalog(&self) -> Result<(), &'static str> {
        let presentation_valid = match &self.surface {
            NativePresentation::Terminal { cols, rows, .. } => {
                (2..=500).contains(cols) && (2..=200).contains(rows)
            }
            NativePresentation::PixelStream { width, height, .. } => {
                (64..=4096).contains(width) && (64..=4096).contains(height)
            }
        };
        let valid_port_id = |id: &str| {
            !id.is_empty()
                && id.len() <= 160
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if !presentation_valid
            || !valid_port_id(self.surface.port())
            || !valid_port_id(&self.probe_port)
            || self.surface.port() == self.probe_port
        {
            return Err("native presentation and probe must be distinct valid Ports");
        }
        if self.operations.is_empty() || self.operations.len() > 8 {
            return Err("native probe requires between one and eight operations");
        }
        let mut paths = BTreeSet::new();
        let mut total_timeout = 0_u64;
        let mut total_output = 0_u64;
        for operation in &self.operations {
            let path = operation.path();
            if path.len() > 256
                || !path.starts_with('/')
                || path.split('/').skip(1).any(|segment| {
                    segment.is_empty()
                        || matches!(segment, "." | "..")
                        || !segment
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                })
                || !paths.insert(path)
                || !(1..=30_000).contains(&operation.timeout_ms())
            {
                return Err("native probe path or timeout is invalid or repeated");
            }
            total_timeout += operation.timeout_ms();
            let valid_operation = match (&self.surface, operation) {
                (
                    NativePresentation::Terminal { .. },
                    NativeProbeOperation::TerminalExchange {
                        input,
                        output_delimiter,
                        max_output_bytes,
                        ..
                    },
                ) => {
                    total_output += u64::from(*max_output_bytes);
                    !input.is_empty()
                        && input.len() <= 4096
                        && input
                            .iter()
                            .all(|byte| matches!(byte, 32..=126 | b'\r' | b'\n'))
                        && !output_delimiter.is_empty()
                        && output_delimiter.len() <= 256
                        && !terminal_echo_may_complete(input, output_delimiter)
                        && (1..=65_536).contains(max_output_bytes)
                        && output_delimiter.len() <= *max_output_bytes as usize
                }
                (
                    NativePresentation::PixelStream { width, height, .. },
                    NativeProbeOperation::PixelKeyFrame {
                        keysyms,
                        max_frame_bytes,
                        ..
                    },
                ) => {
                    total_output += u64::from(*max_frame_bytes);
                    !keysyms.is_empty()
                        && keysyms.len() <= 32
                        && keysyms.iter().all(|key| (1..=0x1fff_ffff).contains(key))
                        && *max_frame_bytes == u32::from(*width) * u32::from(*height) * 4
                }
                _ => false,
            };
            if !valid_operation {
                return Err(
                    "native probe operation does not match its bounded presentation profile",
                );
            }
        }
        if total_timeout > 120_000 || total_output > 64 * 1024 * 1024 {
            return Err("native probe operations exceed the total time or output limit");
        }
        Ok(())
    }

    /// Logical Port graph check in addition to the bounded catalog.
    pub fn validate(&self, ports: &[BoundPort], steps: &[BoundStep]) -> Result<(), &'static str> {
        self.validate_catalog()?;
        let find = |id: &str| {
            let mut matching = ports.iter().filter(|port| port.id == id);
            let port = matching.next()?;
            matching.next().is_none().then_some(port)
        };
        let surface = find(self.surface.port()).ok_or("native Surface Port is not unique")?;
        let probe = find(&self.probe_port).ok_or("native probe Port is not unique")?;
        if surface.protocol != self.surface.protocol()
            || probe.protocol != HTTP_PROTOCOL
            || surface.from != probe.from
            || steps.iter().filter(|step| step.id == surface.from).count() != 1
            || surface.guest_port.is_none_or(|port| port == 0)
            || probe.guest_port.is_none_or(|port| port == 0)
            || surface.guest_port == probe.guest_port
            || surface.client_address_transport.is_some()
            || probe.client_address_transport.is_some()
        {
            return Err(
                "native Surface and private HTTP probe must belong to the same serving step",
            );
        }
        Ok(())
    }
}

/// Conservative line-echo comparison. Dropping CR/LF closes raw, ICRNL,
/// IGNCR, INLCR, OCRNL and ONLCR variants without assuming the workload's
/// termios configuration. A CR produced by input translation may also echo
/// as `^M` under ECHOCTL. Reject every such candidate, including a delimiter
/// consisting only of line breaks. This deliberately over-refuses catalogs;
/// it never infers that a matching echo is an application result.
fn terminal_echo_may_complete(input: &[u8], delimiter: &[u8]) -> bool {
    let delimiter: Vec<_> = delimiter
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect();
    if delimiter.is_empty() {
        return true;
    }
    for caret_mask in 0..4 {
        let mut echo = Vec::with_capacity(input.len() * 2);
        for byte in input {
            match byte {
                b'\r' if caret_mask & 1 != 0 => echo.extend_from_slice(b"^M"),
                b'\n' if caret_mask & 2 != 0 => echo.extend_from_slice(b"^M"),
                b'\r' | b'\n' => {}
                byte => echo.push(*byte),
            }
        }
        if echo
            .windows(delimiter.len())
            .any(|window| window == delimiter)
        {
            return true;
        }
    }
    false
}
