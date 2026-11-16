//! Declared terminal exchanges of a native Terminal surface.
//!
//! One operation is one bounded interaction: wait for the declared startup
//! prompt (or, on the first exchange, for the first output), type the declared
//! input, and read until the declared delimiter arrives. The capture is
//! bounded by the operation's own `max_output_bytes` and its digest is taken
//! over exactly the bytes read, delimiter included.
//!
//! A timeout, an output bound and a missing delimiter are all reported as that
//! operation's measurement, never as a launch failure: the Run stops the
//! workload safely and the Contract verdict fails closed. Nothing here decides
//! K.

use std::time::{Duration, Instant};

use ato_formation::native_surface::{NativeProbeOperation, NativeSurfaceAdapter};
use ato_formation::verify::RuntimeTerminalObservation;
use ato_pty_host::PtySession;
use sha2::{Digest, Sha256};

/// The terminal IO one exchange needs: write the input, read bounded output
/// until a predicate holds or the deadline passes.
///
/// A real run uses [`PtySession`]. Keeping the exchange logic over this trait
/// lets the framing, the bounds and the digest be decided by tests that need
/// no pseudo terminal, and admits exactly one implementation per run.
pub(crate) trait ExchangeIo {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn read_until(
        &mut self,
        captured: &mut Vec<u8>,
        bound: usize,
        deadline: Instant,
        done: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String>;
}

impl ExchangeIo for PtySession {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        PtySession::write(self, bytes)
    }

    fn read_until(
        &mut self,
        captured: &mut Vec<u8>,
        bound: usize,
        deadline: Instant,
        done: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        PtySession::read_until(self, captured, bound, deadline, done)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Perform `operations` in authored order and report each measurement.
///
/// The first unsuccessful exchange ends the sequence: the following operations
/// are not typed into a workload whose state is unknown, and are left
/// unobserved so the verifier fails them closed rather than inventing a
/// verdict.
pub(crate) fn exchanges(
    io: &mut dyn ExchangeIo,
    adapter: &NativeSurfaceAdapter,
    operations: &[(&str, &NativeProbeOperation)],
) -> Vec<RuntimeTerminalObservation> {
    let port = adapter.surface.port().to_owned();
    let mut measured = Vec::with_capacity(operations.len());
    for (index, (requirement_id, operation)) in operations.iter().enumerate() {
        let NativeProbeOperation::TerminalExchange {
            input,
            output_delimiter,
            max_output_bytes,
            timeout_ms,
            ready_output,
            ..
        } = operation
        else {
            // A presentation this Adapter does not drive. Reporting it as an
            // unperformed exchange is the honest answer.
            measured.push(RuntimeTerminalObservation {
                requirement_id: (*requirement_id).to_owned(),
                port: port.clone(),
                operation: operation.path().to_owned(),
                captured_bytes: 0,
                captured_sha256: digest(&[]),
                delimiter_found: false,
                failure: Some("the declared operation is not a terminal exchange".to_owned()),
            });
            break;
        };
        let deadline = Instant::now() + Duration::from_millis(*timeout_ms);
        let bound = *max_output_bytes as usize;
        let mut captured: Vec<u8> = Vec::new();
        let mut failure = None;
        let mut delimiter_found = false;
        // Startup framing. The first exchange waits for the declared prompt,
        // or for the first output when none is declared: typing before a
        // program printed anything would make the capture depend on timing.
        // A later exchange is already past the previous completion, so it only
        // waits when this operation declares a prompt of its own.
        let wait_for_ready = index == 0 || !ready_output.is_empty();
        if wait_for_ready {
            let mut ready = |capture: &[u8]| -> bool {
                if ready_output.is_empty() {
                    !capture.is_empty()
                } else {
                    capture.ends_with(ready_output)
                }
            };
            match io.read_until(&mut captured, bound, deadline, &mut ready) {
                Ok(true) => {}
                Ok(false) => {
                    failure = Some(format!(
                        "the workload did not reach its declared ready output within {timeout_ms} ms"
                    ));
                }
                Err(detail) => failure = Some(detail),
            }
        }
        if failure.is_none() {
            // A completion marker already waiting before the input is not this
            // exchange's answer.
            if captured
                .windows(output_delimiter.len())
                .any(|window| window == output_delimiter.as_slice())
            {
                failure =
                    Some("the declared delimiter was already present before the input".to_owned());
            } else if let Err(detail) = io.write(input) {
                failure = Some(detail);
            } else {
                let mut complete = |capture: &[u8]| -> bool {
                    capture
                        .windows(output_delimiter.len())
                        .any(|window| window == output_delimiter.as_slice())
                };
                match io.read_until(&mut captured, bound, deadline, &mut complete) {
                    Ok(true) => delimiter_found = true,
                    Ok(false) => {
                        failure = Some(format!(
                            "the workload did not answer the declared input within {timeout_ms} ms"
                        ));
                    }
                    Err(detail) => failure = Some(detail),
                }
            }
        }
        if delimiter_found
            && let Some(position) = captured
                .windows(output_delimiter.len())
                .position(|window| window == output_delimiter.as_slice())
        {
            // The capture ends at the delimiter: anything after it belongs to
            // the next exchange, not to this one's digest.
            captured.truncate(position + output_delimiter.len());
        }
        let failed = failure.is_some();
        measured.push(RuntimeTerminalObservation {
            requirement_id: (*requirement_id).to_owned(),
            port: port.clone(),
            operation: operation.path().to_owned(),
            captured_bytes: captured.len() as u64,
            captured_sha256: digest(&captured),
            delimiter_found,
            failure,
        });
        if failed {
            break;
        }
    }
    measured
}
