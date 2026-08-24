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
use clap::Parser;
use serde::Deserialize;

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
    let token = load_auth_handoff(&args.auth_handoff_binary, &args.api_url)?;
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
    root.references[0] = replacement;
    Ok(())
}

fn write_receipt(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}
