use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_cli::{
    HttpObjectTransportApi, UploadConfig, upload_http_object_graph,
    upload_staging_negative_test_object_graph, vm_capture_receipt_refs,
};
use ato_objects::FsObjectStore;
use ato_runtime_object_graph::{ObjectGraphIndexV1, VisibilityPolicy, standard_reference_registry};
use base64::Engine;
use clap::Parser;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    objects: PathBuf,
    #[arg(long, default_value = "https://staging.api.ato.run")]
    api_url: String,
    #[arg(long, default_value = "ato")]
    auth_handoff_binary: PathBuf,
    /// Use the canonical browser-approved PKCE bridge instead of a stored Ato credential.
    #[arg(long)]
    device_login: bool,
    #[arg(long)]
    receipt: PathBuf,
    #[arg(long)]
    idempotency_key: Option<String>,
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    #[arg(long, default_value_t = 4)]
    retry_attempts: usize,
    #[arg(long, default_value_t = 240)]
    validation_poll_attempts: usize,
    #[arg(long, default_value_t = 1_000)]
    validation_poll_ms: u64,
    /// Private staging-only test that forges one declared semantic reference.
    #[arg(long)]
    negative_validator_test: bool,
}

#[derive(Deserialize)]
struct AuthHandoff {
    session_token: String,
}

#[derive(Deserialize)]
struct DeviceInit {
    session_id: String,
    user_code: String,
    poll_interval_sec: u64,
}

#[derive(Deserialize)]
struct DevicePoll {
    code: String,
    auth_code: Option<String>,
    poll_interval_sec: Option<u64>,
}

#[derive(Deserialize)]
struct DeviceExchange {
    access_token: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = fs::read(&args.index)?;
    let mut index: ObjectGraphIndexV1 = serde_json::from_slice(&bytes)?;
    ensure!(
        serde_jcs::to_vec(&index)? == bytes,
        "object graph index is not canonical JCS"
    );
    if args.negative_validator_test {
        forge_declared_reference(&mut index)?;
    }
    let objects = FsObjectStore::open(&args.objects)?;
    let token = if args.device_login {
        device_login(&args.api_url)?
    } else {
        load_auth_handoff(&args.auth_handoff_binary, &args.api_url)?
    };
    let api = HttpObjectTransportApi::new(&args.api_url, token)?;
    let idempotency_key = args.idempotency_key.unwrap_or_else(|| {
        if args.negative_validator_test {
            format!(
                "staging-negative-validator-{}",
                index.digest().expect("canonical graph index")
            )
        } else {
            format!(
                "ato-object-upload-v1-{}",
                index.digest().expect("canonical graph index")
            )
        }
    });
    let config = UploadConfig {
        concurrency: args.concurrency,
        retry_attempts: args.retry_attempts,
        validation_poll_attempts: args.validation_poll_attempts,
        validation_poll_interval: Duration::from_millis(args.validation_poll_ms),
    };
    if args.negative_validator_test {
        match upload_staging_negative_test_object_graph(
            &api,
            &index,
            &objects,
            &idempotency_key,
            config,
        ) {
            Ok(_) => bail!("malformed graph unexpectedly became ready"),
            Err(error) if error.to_string().contains("validation rejected") => {
                let rejection = error.to_string();
                let graph_id = rejected_graph_id(&rejection)?;
                let objects_deleted = api.delete_rejected_graph(graph_id)?;
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "rejected",
                        "source": "independent_validator",
                        "reason": rejection,
                        "graph_id": graph_id,
                        "cleanup_objects_deleted": objects_deleted
                    })
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }

    let mut receipt = upload_http_object_graph(
        &api,
        &index,
        &objects,
        &standard_reference_registry()?,
        &idempotency_key,
        config,
    )?;
    let (descriptor, frontier) = vm_capture_receipt_refs(&index, &objects)?;
    receipt.vm_materialization_descriptor_ref = descriptor;
    receipt.record_frontier_ref = frontier;
    write_receipt(&args.receipt, &serde_jcs::to_vec(&receipt)?)?;
    println!("{} {}", receipt.bundle_id, receipt.root_computation_ref);
    Ok(())
}

fn device_login(api_url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(api_url).context("invalid device-login API URL")?;
    ensure!(
        parsed.scheme() == "https" || parsed.host_str() == Some("localhost"),
        "device login requires HTTPS (except localhost)"
    );
    ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "device-login API URL cannot contain a query or fragment"
    );
    let base_url = api_url.trim_end_matches('/');
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy)
        .map_err(|error| anyhow::anyhow!("generate PKCE verifier entropy: {error}"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entropy);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("construct device-login HTTP client")?;
    let init_response = client
        .post(format!("{base_url}/v1/auth/bridge/init"))
        .json(&serde_json::json!({
            "code_challenge": challenge,
            "method": "S256",
            "device_info": format!("ato-object-graph-upload/{}", env!("CARGO_PKG_VERSION"))
        }))
        .send()
        .context("initialize device login")?;
    ensure!(
        init_response.status().is_success(),
        "device login init failed with HTTP {}",
        init_response.status()
    );
    let init: DeviceInit = init_response
        .json()
        .context("decode device login init response")?;
    eprintln!(
        "Open and approve this staging device login in Chrome:\n{base_url}/v1/auth/bridge/activate?session_id={}\nVerification code: {}",
        init.session_id, init.user_code
    );

    let auth_code = (0..200)
        .find_map(|_| {
            let response = client
                .post(format!("{base_url}/v1/auth/bridge/poll"))
                .json(&serde_json::json!({
                    "session_id": init.session_id,
                    "code_verifier": verifier
                }))
                .send()
                .ok()?;
            let status = response.status();
            let poll: DevicePoll = response.json().ok()?;
            if status.is_success() && poll.code == "SUCCESS" {
                return poll.auth_code;
            }
            std::thread::sleep(Duration::from_secs(
                poll.poll_interval_sec
                    .unwrap_or(init.poll_interval_sec)
                    .max(1),
            ));
            None
        })
        .context("device login was not approved before the polling limit")?;
    let exchange_response = client
        .post(format!("{base_url}/v1/auth/bridge/exchange"))
        .json(&serde_json::json!({
            "session_id": init.session_id,
            "auth_code": auth_code,
            "code_verifier": verifier
        }))
        .send()
        .context("exchange approved device login")?;
    ensure!(
        exchange_response.status().is_success(),
        "device login exchange failed with HTTP {}",
        exchange_response.status()
    );
    let exchange: DeviceExchange = exchange_response
        .json()
        .context("decode device login exchange response")?;
    ensure!(
        exchange.access_token.starts_with("ato_dev_"),
        "device login did not return a device credential"
    );
    Ok(exchange.access_token)
}

fn rejected_graph_id(message: &str) -> Result<&str> {
    let graph_id = message
        .strip_prefix("object graph ")
        .and_then(|value| value.split_once(" validation rejected:"))
        .map(|(graph_id, _)| graph_id)
        .context("validator rejection did not identify its object graph")?;
    ensure!(
        !graph_id.is_empty(),
        "validator rejection graph id is empty"
    );
    Ok(graph_id)
}

fn load_auth_handoff(binary: &Path, api_url: &str) -> Result<String> {
    let output = Command::new(binary)
        .arg("desktop-auth-handoff")
        .env("ATO_STORE_API_URL", api_url)
        .output()
        .context("run canonical Ato auth handoff")?;
    ensure!(
        output.status.success(),
        "canonical Ato auth handoff failed; run `ato login --headless` against staging"
    );
    let handoff: AuthHandoff =
        serde_json::from_slice(&output.stdout).context("invalid Ato auth handoff response")?;
    ensure!(
        handoff.session_token.starts_with("ato_dev_"),
        "auth handoff did not return a device credential"
    );
    Ok(handoff.session_token)
}

fn forge_declared_reference(index: &mut ObjectGraphIndexV1) -> Result<()> {
    ensure!(
        index.visibility_policy == VisibilityPolicy::Public,
        "negative fixture source must be the validated public graph"
    );
    index.visibility_policy = VisibilityPolicy::Private;
    let root_position = index
        .objects
        .iter()
        .position(|object| object.content_ref == index.root_computation_ref)
        .context("negative fixture root descriptor is absent")?;
    let original_references = index.objects[root_position].references.clone();
    let replacement = index
        .objects
        .iter()
        .map(|object| object.content_ref.clone())
        .find(|reference| {
            reference != &index.root_computation_ref && !original_references.contains(reference)
        })
        .context("negative fixture has no replacement object")?;
    let root = &mut index.objects[root_position];
    ensure!(
        !root.references.is_empty(),
        "negative fixture root has no references"
    );
    // Preserve the complete declared traversal so prepare accepts the graph;
    // the independent semantic validator must reject this extra forged edge.
    root.references.push(replacement);
    root.references.sort();
    Ok(())
}

fn write_receipt(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}
