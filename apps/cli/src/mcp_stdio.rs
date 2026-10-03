//! Shared newline-delimited JSON-RPC framing for fixed-tool MCP facades.
use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) fn run_stdio(
    mut input: impl BufRead,
    mut output: impl Write,
    max_request_bytes: Option<usize>,
    mut handle: impl FnMut(&Value) -> Option<Value>,
) -> Result<()> {
    while let Some(frame) = read_frame(&mut input, max_request_bytes)? {
        let bytes = match frame {
            Ok(bytes) => bytes,
            Err(()) => {
                write_response(
                    &mut output,
                    &rpc_error(Value::Null, -32600, "frame too large"),
                )?;
                continue;
            }
        };
        if std::str::from_utf8(&bytes).is_ok_and(|line| line.trim().is_empty()) {
            continue;
        }
        let request: Value = match serde_json::from_slice(&bytes) {
            Ok(request) => request,
            Err(_) => {
                write_response(&mut output, &rpc_error(Value::Null, -32700, "parse error"))?;
                continue;
            }
        };
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            write_response(
                &mut output,
                &rpc_error(
                    request.get("id").cloned().unwrap_or(Value::Null),
                    -32600,
                    "invalid request",
                ),
            )?;
            continue;
        }
        if let Some(response) = handle(&request) {
            write_response(&mut output, &response)?;
        }
    }
    Ok(())
}

// Drain an oversized line only when the caller has a bounded request contract.
// None preserves the existing Activity framing, including JSON escaping that
// expands a valid memo or Interaction beyond Formation's envelope limit.
// A final line at EOF remains supported for both stdio interfaces.
fn read_frame(
    input: &mut impl BufRead,
    max_request_bytes: Option<usize>,
) -> Result<Option<Result<Vec<u8>, ()>>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = input.fill_buf().context("read MCP request")?;
        if buffer.is_empty() {
            return Ok(if oversized {
                Some(Err(()))
            } else if bytes.is_empty() {
                None
            } else {
                Some(Ok(bytes))
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |position| position + 1);
        if !oversized {
            if max_request_bytes.is_some_and(|cap| bytes.len().saturating_add(count) > cap) {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&buffer[..count]);
            }
        }
        input.consume(count);
        if newline.is_some() {
            return Ok(Some(if oversized { Err(()) } else { Ok(bytes) }));
        }
    }
}

fn write_response(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value).context("encode MCP response")?;
    output.write_all(b"\n").context("write MCP response")?;
    output.flush().context("flush MCP response")
}

pub(crate) fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub(crate) fn negotiated_protocol_version(request: &Value) -> String {
    request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 32)
        .unwrap_or("2025-03-26")
        .to_owned()
}

pub(crate) fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"error\":\"encoding_error\"}".to_owned());
    json!({"content":[{"type":"text","text":text}],"structuredContent":value,"isError":is_error})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn oversized_and_malformed_frames_are_redacted_and_next_frame_survives() -> Result<()> {
        let limit = 256 * 1024;
        let sentinel = "private-canary";
        let mut input = "\u{2003}\n".to_owned();
        input.push_str(&sentinel.repeat(limit / sentinel.len() + 1));
        input.push_str("\n{private-canary\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}");
        let mut output = Vec::new();
        run_stdio(Cursor::new(input), &mut output, Some(limit), |request| {
            Some(json!({"jsonrpc":"2.0","id":request["id"],"result":{}}))
        })?;
        let output = String::from_utf8(output)?;
        assert!(!output.contains(sentinel));
        let frames: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0]["error"]["code"], -32600);
        assert_eq!(frames[1]["error"]["code"], -32700);
        assert_eq!(frames[2]["id"], 1);
        Ok(())
    }
}
