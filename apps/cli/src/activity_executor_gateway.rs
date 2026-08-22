#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const ACTIVITY_INPUT_REQUEST: &str = "runs/activity-input.request.json";
pub(crate) const ACTIVITY_INPUT_RESPONSE: &str = "runs/activity-input.response.json";
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityInputRequest {
    pub request_id: String,
    pub operation_id: String,
    pub actor_participant_id: String,
    pub client_sequence: u64,
    pub adapter_id: String,
    pub protocol_id: String,
    pub event: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityInputReceipt {
    pub request_id: String,
    pub operation_id: String,
    pub actor_participant_id: String,
    pub client_sequence: u64,
    pub run_sequence: u64,
    pub result: String,
    pub adapter_id: String,
    pub record_ref: Option<String>,
    pub error: Option<String>,
}

pub(crate) fn exchange(
    repository_root: &Path,
    request: &ActivityInputRequest,
) -> Result<ActivityInputReceipt> {
    let request_path = repository_root.join(ACTIVITY_INPUT_REQUEST);
    let response_path = repository_root.join(ACTIVITY_INPUT_RESPONSE);
    if request_path.exists() || response_path.exists() {
        bail!("Activity input gateway already has an in-flight request");
    }
    let pending = request_path.with_extension("pending");
    fs::write(
        &pending,
        serde_json::to_vec(request).context("encode Activity input request")?,
    )
    .context("write Activity input request")?;
    fs::rename(&pending, &request_path).context("publish Activity input request")?;
    let deadline = Instant::now() + GATEWAY_TIMEOUT;
    while Instant::now() < deadline {
        if response_path.exists() {
            let bytes = fs::read(&response_path).context("read Activity input response")?;
            fs::remove_file(&response_path).context("consume Activity input response")?;
            let receipt: ActivityInputReceipt =
                serde_json::from_slice(&bytes).context("decode Activity input response")?;
            if receipt.request_id != request.request_id {
                bail!("Activity input response has a mismatched request identity");
            }
            return Ok(receipt);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(request_path);
    bail!("Activity input gateway timed out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_rejects_unknown_fields() {
        let value = serde_json::json!({
            "request_id": "request_0001",
            "operation_id": "operation_0001",
            "actor_participant_id": "participant_0001",
            "client_sequence": 1,
            "adapter_id": "ato.browser@1",
            "protocol_id": "ato.browser@1",
            "event": {"type":"keyboard"},
            "role": "executor"
        });
        assert!(serde_json::from_value::<ActivityInputRequest>(value).is_err());
    }
}
