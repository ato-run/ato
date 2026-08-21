use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, connect};
use url::Url;

use crate::activity_executor_gateway::{ActivityInputRequest, exchange};

const ROOM_PROTOCOL: &str = "ato.activity.room@1";
const ROOM_WEBSOCKET_PROTOCOL: &str = "ato.activity.room.v1";
const BROWSER_PROTOCOL: &str = "ato.browser@1";
const BROWSER_READY_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityExecutorJob {
    activity_id: String,
    run_id: String,
    experience_url: String,
    experience_origin: String,
    experience_manifest_digest: String,
    room_url: String,
    executor_credential: String,
    expires_at: String,
    rtc: ActivityRtcConfiguration,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityRtcConfiguration {
    ice_servers: Vec<ActivityIceServer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityIceServer {
    urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomEnvelope {
    protocol: String,
    seq: u64,
    #[serde(rename = "type")]
    message_type: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunInput {
    run_id: String,
    operation_id: String,
    client_seq: u64,
    adapter_id: String,
    protocol_id: String,
    event: Value,
    actor_participant_id: String,
    source_connection_id: String,
}

#[derive(Serialize)]
struct LeaseStatus<'a> {
    status: &'a str,
}

pub(crate) struct ActivityExecutorInput<'a> {
    pub client: &'a Client,
    pub api_base: &'a str,
    pub runner_token: &'a str,
    pub lease_id: &'a str,
    pub run_id: &'a str,
    pub activity_id: &'a str,
    pub activity_run_id: &'a str,
    pub state_dir: &'a Path,
    pub chrome: &'a Path,
}

pub(crate) fn execute(input: ActivityExecutorInput<'_>) -> Result<()> {
    if input.run_id != input.activity_run_id {
        bail!("Activity lease Run identity does not match its ActivityRun");
    }
    report_status(&input, "preparing")?;
    let job: ActivityExecutorJob = input
        .client
        .post(format!(
            "{}/v1/activities/{}/runs/{}/executor-session",
            input.api_base.trim_end_matches('/'),
            input.activity_id,
            input.activity_run_id
        ))
        .bearer_auth(input.runner_token)
        .send()?
        .error_for_status()?
        .json()?;
    validate_job(&job, &input)?;
    let session_root = input.state_dir.join("activity-runs").join(input.run_id);
    let project = session_root.join("workspace");
    let runtime_dir = session_root.join("browser-runtime");
    if session_root.exists() {
        bail!("Activity executor refuses to reuse an existing Run directory");
    }
    fs::create_dir_all(&project).context("create Activity Run workspace")?;
    fs::create_dir(&runtime_dir).context("create Activity Browser runtime directory")?;
    restrict_private_directory(&session_root)?;
    restrict_private_directory(&runtime_dir)?;
    write_activity_manifest(&project, &job.experience_origin)?;
    let executable = std::env::current_exe().context("resolve ato executable")?;
    let init = Command::new(&executable)
        .arg("init")
        .arg(&project)
        .env("ATO_BROWSER_RUNTIME_DIR", &runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("start Activity Capsule Run")?;
    if !init.success() {
        bail!("Activity Capsule Run did not become active");
    }
    let mut browser = Command::new(&executable)
        .arg("__browser-host")
        .arg("--runtime-dir")
        .arg(&runtime_dir)
        .arg("--target-url")
        .arg(&job.experience_url)
        .arg("--chrome")
        .arg(input.chrome)
        .arg("--headless")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start Activity Browser Host")?;
    let result = run_room_loop(&input, &job, &project, &runtime_dir, &mut browser);
    let _ = Command::new(&executable)
        .arg("stop")
        .arg(&project)
        .env("ATO_BROWSER_RUNTIME_DIR", &runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = browser.kill();
    let _ = browser.wait();
    let _ = fs::remove_dir_all(&session_root);
    result
}

fn run_room_loop(
    input: &ActivityExecutorInput<'_>,
    job: &ActivityExecutorJob,
    project: &Path,
    runtime_dir: &Path,
    browser: &mut Child,
) -> Result<()> {
    wait_for_browser(runtime_dir, browser)?;
    report_status(input, "running")?;
    let mut request = job.room_url.clone().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        format!("ActivityExecutor {}", job.executor_credential).parse()?,
    );
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", ROOM_WEBSOCKET_PROTOCOL.parse()?);
    let (mut room, _) = connect(request).context("connect Activity Room")?;
    let mut outgoing_seq = 1_u64;
    send_room(
        &mut room,
        &mut outgoing_seq,
        "run.state",
        json!({"run_id": job.run_id, "state": "paused"}),
    )?;
    report_status(input, "ready")?;
    loop {
        let message = room.read().context("read Activity Room message")?;
        let Message::Text(text) = message else {
            if message.is_close() {
                return Ok(());
            }
            continue;
        };
        let envelope: RoomEnvelope =
            serde_json::from_str(text.as_str()).context("decode Activity Room message")?;
        if envelope.protocol != ROOM_PROTOCOL {
            bail!("Activity Room protocol changed during the Run");
        }
        let _transport_sequence = envelope.seq;
        match envelope.message_type.as_str() {
            "run.input" => {
                let run_input: RunInput = serde_json::from_value(envelope.payload)
                    .context("decode Run-scoped Browser input")?;
                if run_input.run_id != job.run_id
                    || run_input.adapter_id != BROWSER_PROTOCOL
                    || run_input.protocol_id != BROWSER_PROTOCOL
                    || run_input.source_connection_id.is_empty()
                {
                    bail!("Activity Room delivered input outside this Run authority");
                }
                let receipt = exchange(
                    &project.join(".capsule"),
                    &ActivityInputRequest {
                        request_id: format!("gateway_{}", run_input.operation_id),
                        operation_id: run_input.operation_id,
                        actor_participant_id: run_input.actor_participant_id,
                        client_sequence: run_input.client_seq,
                        adapter_id: run_input.adapter_id,
                        protocol_id: run_input.protocol_id,
                        event: run_input.event,
                    },
                )?;
                send_room(
                    &mut room,
                    &mut outgoing_seq,
                    "run.operation.receipt",
                    json!({
                        "run_id": job.run_id,
                        "run_sequence": receipt.run_sequence,
                        "operation_id": receipt.operation_id,
                        "actor_participant_id": receipt.actor_participant_id,
                        "client_sequence": receipt.client_sequence,
                        "result": receipt.result,
                        "adapter_id": receipt.adapter_id,
                        "record_ref": receipt.record_ref,
                        "applied_at": time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)?,
                    }),
                )?;
            }
            "activity.started" => send_room(
                &mut room,
                &mut outgoing_seq,
                "run.state",
                json!({"run_id": job.run_id, "state": "live"}),
            )?,
            "activity.ended" => return Ok(()),
            _ => {}
        }
    }
}

fn validate_job(job: &ActivityExecutorJob, input: &ActivityExecutorInput<'_>) -> Result<()> {
    if job.activity_id != input.activity_id || job.run_id != input.activity_run_id {
        bail!("Activity executor job escaped its lease scope");
    }
    let experience = Url::parse(&job.experience_url).context("parse Experience URL")?;
    if !matches!(experience.scheme(), "http" | "https")
        || experience.origin().ascii_serialization() != job.experience_origin
    {
        bail!("Activity executor job has an invalid immutable Experience origin");
    }
    if !job.experience_manifest_digest.starts_with("sha256:")
        || job.experience_manifest_digest.len() != 71
        || !job.experience_manifest_digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("Activity executor job has an invalid Experience manifest digest");
    }
    let room = Url::parse(&job.room_url).context("parse Activity Room URL")?;
    if !matches!(room.scheme(), "ws" | "wss")
        || !job.executor_credential.starts_with("ato_aes_")
        || job.expires_at.is_empty()
    {
        bail!("Activity executor job has an invalid realtime credential boundary");
    }
    if job.rtc.ice_servers.iter().any(|server| {
        server.urls.is_empty()
            || server
                .urls
                .iter()
                .any(|url| !url.starts_with("stun:") && !url.starts_with("stuns:"))
    }) {
        bail!("Activity executor job contains a non-STUN ICE server");
    }
    Ok(())
}

fn write_activity_manifest(project: &Path, expected_origin: &str) -> Result<()> {
    let executable = std::env::current_exe().context("resolve ato executable")?;
    let executable = toml::Value::String(executable.to_string_lossy().into_owned()).to_string();
    let origin = toml::Value::String(expected_origin.to_owned()).to_string();
    let manifest = format!(
        "schema = 1\n\n[[process]]\nid = \"activity\"\ncommand = [{executable}, \"__activity-idle\"]\n\n[[port]]\nid = \"activity.browser\"\nnode = \"activity\"\nprotocol = \"ato.browser@1\"\nrole = \"server\"\n\n[[adapter]]\nuse = \"ato.browser@1\"\nport = \"activity.browser\"\n\n[adapter.config]\nexpected_origin = {origin}\nallowed_non_text_codes = [\"KeyX\", \"KeyZ\"]\n"
    );
    fs::write(project.join("capsule.toml"), manifest).context("write Activity Capsule manifest")
}

fn wait_for_browser(runtime_dir: &Path, browser: &mut Child) -> Result<()> {
    let deadline = Instant::now() + BROWSER_READY_WAIT;
    let port_file = runtime_dir
        .join("browser-host-profile")
        .join("browser-host-cdp-port");
    while Instant::now() < deadline {
        if port_file.is_file() {
            return Ok(());
        }
        if let Some(status) = browser.try_wait()? {
            bail!("Activity Browser Host exited before ready: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("Activity Browser Host did not become ready")
}

fn send_room<S: std::io::Read + std::io::Write>(
    room: &mut tungstenite::WebSocket<S>,
    sequence: &mut u64,
    message_type: &str,
    payload: Value,
) -> Result<()> {
    room.send(Message::Text(
        serde_json::to_string(&json!({
            "protocol": ROOM_PROTOCOL,
            "seq": *sequence,
            "type": message_type,
            "payload": payload,
        }))?
        .into(),
    ))?;
    *sequence = sequence.checked_add(1).context("Room sequence exhausted")?;
    Ok(())
}

fn report_status(input: &ActivityExecutorInput<'_>, status: &str) -> Result<()> {
    input
        .client
        .post(format!(
            "{}/v1/runner-leases/{}/status",
            input.api_base.trim_end_matches('/'),
            input.lease_id
        ))
        .bearer_auth(input.runner_token)
        .json(&LeaseStatus { status })
        .send()?
        .error_for_status()?;
    Ok(())
}

fn restrict_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .context("restrict Activity executor directory")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_uses_one_generic_browser_adapter() {
        let root = tempfile::tempdir().unwrap();
        write_activity_manifest(root.path(), "https://example.test").unwrap();
        let manifest = fs::read_to_string(root.path().join("capsule.toml")).unwrap();
        assert_eq!(manifest.matches("ato.browser@1").count(), 2);
        assert!(manifest.contains("activity.browser"));
        assert!(manifest.contains("KeyX"));
        assert!(!manifest.contains("Tobu"));
    }
}
