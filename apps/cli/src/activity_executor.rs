#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::activity_executor_host::{
    ActivityHostEvent, ActivityHostPageConfig, ActivityHostServer,
};
use crate::network_runner::{TcpProxy, UntrustedProcessEvaluator, read_session_report};

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
    source: Option<ActivityCapsuleSource>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityCapsuleSource {
    kind: String,
    bundle_id: String,
    transport_digest: String,
    computation_ref: String,
    exported_port_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityRtcConfiguration {
    ice_servers: Vec<ActivityIceServer>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivityIceServer {
    urls: Vec<String>,
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
    pub evaluator: &'a UntrustedProcessEvaluator,
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
    if let Some(source) = job.source.as_ref() {
        return execute_capsule_source(&input, &job, source);
    }
    let session_root = input.state_dir.join("activity-runs").join(input.run_id);
    let project = session_root.join("workspace");
    let runtime_dir = session_root.join("browser-runtime");
    if session_root.exists() {
        bail!("Activity executor refuses to reuse an existing Run directory");
    }
    fs::create_dir_all(&project).context("create Activity Run workspace")?;
    let executable = std::env::current_exe().context("resolve ato executable")?;
    let mut browser = None;
    let mut host = None;
    let result = (|| -> Result<()> {
        fs::create_dir(&runtime_dir).context("create Activity Browser runtime directory")?;
        restrict_private_directory(&session_root)?;
        restrict_private_directory(&runtime_dir)?;
        let controller = ActivityHostServer::start(
            ActivityHostPageConfig {
                run_id: job.run_id.clone(),
                generic_browser: false,
                experience_url: job.experience_url.clone(),
                experience_origin: job.experience_origin.clone(),
                room_url: job.room_url.clone(),
                executor_credential: job.executor_credential.clone(),
                ice_servers: serde_json::to_value(&job.rtc.ice_servers)?,
            },
            project.join(".capsule"),
        )?;
        write_activity_manifest(&project, controller.expected_origin())?;
        let target_url = controller.target_url().to_owned();
        let presentation_sink_url = controller.presentation_sink_url().to_owned();
        let presentation_sink_credential = controller.presentation_sink_credential().to_owned();
        host = Some(controller);
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
        let child = Command::new(&executable)
            .arg("__browser-host")
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--target-url")
            .arg(target_url)
            .arg("--chrome")
            .arg(input.chrome)
            .arg("--headless")
            .arg("--presentation-sink-url")
            .arg(presentation_sink_url)
            .env(
                "ATO_BROWSER_PRESENTATION_SINK_CREDENTIAL",
                presentation_sink_credential,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start Activity Browser Host")?;
        browser = Some(child);
        let browser_ref = browser.as_mut().context("Activity Browser Host missing")?;
        wait_for_browser(&runtime_dir, browser_ref)?;
        report_status(&input, "running")?;
        run_host_loop(
            &input,
            host.as_ref()
                .context("Activity Browser controller missing")?,
            browser_ref,
        )
    })();
    let _ = Command::new(&executable)
        .arg("stop")
        .arg(&project)
        .env("ATO_BROWSER_RUNTIME_DIR", &runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Some(browser) = browser.as_mut() {
        let _ = browser.kill();
        let _ = browser.wait();
    }
    if let Some(host) = host {
        let _ = host.stop();
    }
    let _ = fs::remove_dir_all(&session_root);
    result
}

fn run_host_loop(
    input: &ActivityExecutorInput<'_>,
    host: &ActivityHostServer,
    browser: &mut Child,
) -> Result<()> {
    let mut ready = false;
    loop {
        match host.recv_timeout(Duration::from_millis(100)) {
            Ok(ActivityHostEvent::Ready) if !ready => {
                report_status(input, "ready")?;
                ready = true;
            }
            Ok(ActivityHostEvent::Ready) => {}
            Ok(ActivityHostEvent::Ended) => return Ok(()),
            Ok(ActivityHostEvent::Failed) => bail!("Activity Browser controller failed"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Activity Browser controller stopped unexpectedly")
            }
        }
        if let Some(status) = browser.try_wait()? {
            bail!("Activity Browser Host exited during the Run: {status}");
        }
    }
}

fn execute_capsule_source(
    input: &ActivityExecutorInput<'_>,
    job: &ActivityExecutorJob,
    source: &ActivityCapsuleSource,
) -> Result<()> {
    let session_root = input.state_dir.join("activity-runs").join(input.run_id);
    if session_root.exists() {
        bail!("Activity executor refuses to reuse an existing Run directory");
    }
    fs::create_dir_all(&session_root).context("create generic Activity workspace")?;
    restrict_private_directory(&session_root)?;
    let bundle_path = session_root.join("input.capsule");
    let bytes = input
        .client
        .get(format!(
            "{}/v1/runner-leases/{}/capsule-bundle",
            input.api_base.trim_end_matches('/'),
            input.lease_id
        ))
        .bearer_auth(input.runner_token)
        .send()?
        .error_for_status()?
        .bytes()?;
    let actual_digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    if actual_digest != source.transport_digest {
        bail!("generic Activity Bundle transport digest mismatch");
    }
    fs::write(&bundle_path, &bytes).context("write generic Activity Bundle")?;

    let runtime_dir = session_root.join("browser-runtime");
    let mut capsule = None;
    let mut browser = None;
    let mut host = None;
    let mut surface_proxy = None;
    let result = (|| -> Result<()> {
        let mut child = input.evaluator.spawn_activity_session(
            &bundle_path,
            &session_root,
            &source.computation_ref,
            &source.exported_port_id,
        )?;
        let report = read_session_report(&mut child)?;
        if report.root_computation_ref != source.computation_ref {
            bail!("generic Activity session reported an unexpected source point");
        }
        let port = report
            .exported_ports
            .iter()
            .find(|port| port.port_id == source.exported_port_id)
            .context("generic Activity web Port was not realized")?;
        if port.protocol != "ato.http@1"
            || port.local_endpoint.as_deref() != Some("unix:surface.sock")
        {
            bail!("generic Activity source did not realize a Browser web surface");
        }
        capsule = Some(child);

        restrict_private_directory(&runtime_dir)?;
        let bootstrap = wait_for_activity_bootstrap(&runtime_dir, capsule.as_mut().unwrap())?;
        let target = Url::parse(&bootstrap.expected_origin)
            .context("parse generic Activity Browser origin")?;
        if target.scheme() != "http"
            || !matches!(target.host_str(), Some("127.0.0.1") | Some("localhost"))
            || target.port().is_none()
            || target.path() != "/"
        {
            bail!("generic Activity Browser origin must be an exact loopback HTTP origin");
        }
        let listen = SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            target
                .port()
                .context("generic Activity Browser port missing")?,
        ));
        surface_proxy = Some(TcpProxy::start_unix(
            listen,
            session_root.join("surface.sock"),
        )?);

        let controller = ActivityHostServer::start(
            ActivityHostPageConfig {
                run_id: job.run_id.clone(),
                generic_browser: true,
                experience_url: String::new(),
                experience_origin: String::new(),
                room_url: job.room_url.clone(),
                executor_credential: job.executor_credential.clone(),
                ice_servers: serde_json::to_value(&job.rtc.ice_servers)?,
            },
            session_root.join(".capsule"),
        )?;
        let presentation_sink_url = controller.presentation_sink_url().to_owned();
        let presentation_sink_credential = controller.presentation_sink_credential().to_owned();
        let product_controller_url = controller.target_url().to_owned();
        host = Some(controller);
        let executable = std::env::current_exe().context("resolve ato executable")?;
        let child = Command::new(&executable)
            .arg("__browser-host")
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--target-url")
            .arg(bootstrap.expected_origin)
            .arg("--chrome")
            .arg(input.chrome)
            .arg("--headless")
            .arg("--presentation-sink-url")
            .arg(presentation_sink_url)
            .arg("--product-controller-url")
            .arg(product_controller_url)
            .env(
                "ATO_BROWSER_PRESENTATION_SINK_CREDENTIAL",
                presentation_sink_credential,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start generic Activity Browser Host")?;
        browser = Some(child);
        let browser_ref = browser
            .as_mut()
            .context("generic Activity Browser Host missing")?;
        wait_for_browser(&runtime_dir, browser_ref)?;
        report_status(input, "running")?;
        run_host_loop(
            input,
            host.as_ref()
                .context("generic Activity media controller missing")?,
            browser_ref,
        )
    })();

    if let Some(browser) = browser.as_mut() {
        let _ = browser.kill();
        let _ = browser.wait();
    }
    if let Some(host) = host {
        let _ = host.stop();
    }
    drop(surface_proxy);
    if let Some(capsule) = capsule.as_mut() {
        request_capsule_quiesce(&session_root);
        let _ = capsule.kill();
        let _ = capsule.wait();
    }
    let _ = fs::remove_dir_all(&session_root);
    result
}

fn wait_for_activity_bootstrap(
    runtime_dir: &Path,
    capsule: &mut Child,
) -> Result<ato_adapter_browser::BrowserRuntimeBootstrap> {
    let deadline = Instant::now() + BROWSER_READY_WAIT;
    while Instant::now() < deadline {
        let mut discoveries = fs::read_dir(runtime_dir)
            .context("read generic Activity Browser runtime")?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("browser-") && name.ends_with(".json")
            })
            .map(|entry| entry.path())
            .collect::<Vec<PathBuf>>();
        discoveries.sort();
        if discoveries.len() > 1 {
            bail!("generic Activity source has multiple Browser Adapter instances");
        }
        if let Some(path) = discoveries.first() {
            return serde_json::from_slice(&fs::read(path)?)
                .context("decode generic Activity Browser discovery");
        }
        if let Some(status) = capsule.try_wait()? {
            bail!("generic Activity Capsule exited before Browser readiness: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("generic Activity Capsule did not attach ato.browser@1")
}

fn request_capsule_quiesce(session_root: &Path) {
    let request = session_root.join(".capsule/runs/stop.request");
    let ack = session_root.join(".capsule/runs/stop.ack");
    let _ = fs::remove_file(&ack);
    if fs::write(&request, b"stop").is_err() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ack.is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn validate_job(job: &ActivityExecutorJob, input: &ActivityExecutorInput<'_>) -> Result<()> {
    if job.activity_id != input.activity_id || job.run_id != input.activity_run_id {
        bail!("Activity executor job escaped its lease scope");
    }
    if let Some(source) = &job.source {
        if source.kind != "capsuleContinuation"
            || !source.bundle_id.starts_with("bnd_")
            || !valid_digest(&source.transport_digest, "sha256")
            || !valid_digest(&source.computation_ref, "blake3")
            || source.exported_port_id.trim().is_empty()
        {
            bail!("Activity executor job has an invalid pinned Capsule source");
        }
    } else {
        let experience = Url::parse(&job.experience_url).context("parse Experience URL")?;
        if !matches!(experience.scheme(), "http" | "https")
            || experience.origin().ascii_serialization() != job.experience_origin
        {
            bail!("Activity executor job has an invalid immutable Experience origin");
        }
        if !valid_digest(&job.experience_manifest_digest, "sha256") {
            bail!("Activity executor job has an invalid Experience manifest digest");
        }
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

fn valid_digest(value: &str, algorithm: &str) -> bool {
    value
        .strip_prefix(&format!("{algorithm}:"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
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
    #[cfg(not(unix))]
    let _ = path;
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

    #[test]
    fn source_job_decodes_without_a_template_or_experience_manifest() {
        let job: ActivityExecutorJob = serde_json::from_value(serde_json::json!({
            "activityId": "act_source",
            "runId": "arun_source",
            "experienceUrl": "",
            "experienceOrigin": "",
            "experienceManifestDigest": "",
            "roomUrl": "wss://activity.example/runner-room",
            "executorCredential": "ato_aes_secret",
            "expiresAt": "2026-08-22T12:00:00.000Z",
            "rtc": { "iceServers": [{ "urls": ["stun:stun.example:3478"] }] },
            "source": {
                "kind": "capsuleContinuation",
                "bundleId": "bnd_source",
                "transportDigest": format!("sha256:{}", "a".repeat(64)),
                "computationRef": format!("blake3:{}", "b".repeat(64)),
                "exportedPortId": "http"
            }
        }))
        .unwrap();
        let source = job.source.expect("source should decode");
        assert_eq!(source.kind, "capsuleContinuation");
        assert_eq!(source.exported_port_id, "http");
        assert!(job.experience_url.is_empty());
    }
}
