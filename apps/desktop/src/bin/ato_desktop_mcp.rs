use std::io::{BufRead, Write};

use ato_ipc::computation::ComputationCommand;

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                respond(&mut stdout, serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}));
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let response = match request.get("method").and_then(|value| value.as_str()) {
            Some("initialize") => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"ato-desktop","version":env!("CARGO_PKG_VERSION")}}}),
            Some("tools/list") => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                {"name":"run_capsule","description":"Run a portable .capsule file","inputSchema":{"type":"object","properties":{"capsule_file":{"type":"string"}},"required":["capsule_file"]}}
            ]}}),
            Some("tools/call") => call_tool(id, &request),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}}),
        };
        respond(&mut stdout, response);
    }
}

fn call_tool(id: serde_json::Value, request: &serde_json::Value) -> serde_json::Value {
    let params = request.get("params").cloned().unwrap_or_default();
    let command = match params.get("name").and_then(|value| value.as_str()) {
        Some("run_capsule") => ComputationCommand::RunPortable {
            capsule_file: params.get("arguments").and_then(|value| value.get("capsule_file")).and_then(|value| value.as_str()).unwrap_or_default().to_owned(),
        },
        _ => return serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"unknown tool"}}),
    };
    match ato_desktop::dispatch(&command) {
        Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"isError":!result.success,"content":[{"type":"text","text":result.output}]}}),
        Err(error) => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error.to_string()}}),
    }
}

fn respond(output: &mut impl Write, value: serde_json::Value) {
    let _ = writeln!(output, "{value}");
    let _ = output.flush();
}
