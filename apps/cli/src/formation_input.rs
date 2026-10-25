//! Owner input through the existing Client; no values in argv or checkpoints.
use anyhow::{Context, Result, ensure};
use ato_formation_worker::runtime_network::{
    Client, new_search_id, proposal::reasoning::save_owner_checkpoint,
};
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf};

#[derive(Debug, Args)]
pub(super) struct InputArgs {
    #[arg(long)]
    search_id: String,
    #[arg(long, env = "ATO_RUNTIME_NETWORK_API")]
    api: String,
    #[arg(long, env = "ATO_RUNTIME_NETWORK_TOKEN_FILE")]
    token_file: PathBuf,
    /// Zero-based item in the inspect response's required array.
    #[arg(long)]
    item: Option<usize>,
    /// Read the value from stdin, never from an argument or environment.
    #[arg(long,requires_all=["item","reuse"],conflicts_with="credential_id")]
    value_stdin: bool,
    #[arg(long, requires = "item", conflicts_with = "value_stdin")]
    credential_id: Option<String>,
    #[arg(long,value_parser=["this_formation","reusable"])]
    reuse: Option<String>,
    /// Required for reusable values. Saved once; retry never extends expiry.
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..=31536000))]
    expires_in_seconds: Option<u64>,
    /// Permit non-secret configuration to become reusable artifact bytes.
    #[arg(long)]
    allow_artifact_embedding: bool,
    /// Metadata-only durable registration identity for response-loss retry.
    #[arg(long)]
    checkpoint: Option<PathBuf>,
}

pub(super) fn run(args: InputArgs) -> Result<()> {
    let token =
        std::fs::read_to_string(&args.token_file).context("read requesting-account token file")?;
    let client = Client::new(&args.api, &token)?;
    let view = client.formation_input(&args.search_id)?;
    if !args.value_stdin && args.credential_id.is_none() {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    ensure!(
        view["expired"] == false,
        "the original Search/round input window is closed"
    );
    let deadline = view["deadline_at_ms"]
        .as_u64()
        .context("input deadline missing")?;
    let requirement = view["required"]
        .as_array()
        .and_then(|items| items.get(args.item?))
        .map(|item| item["requirement"].clone())
        .context("input item is not pending")?;
    let mut body = if let Some(id) = args.credential_id {
        json!({"kind":"select","requirement":requirement,"credential_id":id})
    } else {
        let reuse = args
            .reuse
            .context("select this_formation or reusable scope")?;
        ensure!(
            reuse != "reusable" || args.expires_in_seconds.is_some(),
            "reusable scope requires --expires-in-seconds"
        );
        ensure!(
            requirement["artifact_embedding"] != true || args.allow_artifact_embedding,
            "this operation requires --allow-artifact-embedding; withholding permission keeps it paused"
        );
        ensure!(
            requirement["secret"] != true || !args.allow_artifact_embedding,
            "secret embedding is unavailable"
        );
        let identity = json!({"search_id":args.search_id,"api":args.api,"source_closure_ref":view["source_closure_ref"],"requirement":requirement,"reuse":reuse,"expires_in_seconds":args.expires_in_seconds,"deadline_ms":deadline,"allow_artifact_embedding":args.allow_artifact_embedding});
        let digest = format!("{:x}", Sha256::digest(serde_jcs::to_vec(&identity)?));
        let path = args
            .checkpoint
            .unwrap_or_else(|| PathBuf::from(format!(".tmp/formation-input/{digest}.json")));
        let saved: Value = if path.exists() {
            let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            ensure!(
                saved["identity"] == identity,
                "input checkpoint scope changed"
            );
            saved
        } else {
            let mut entropy = [0; 16];
            getrandom::fill(&mut entropy)
                .map_err(|e| anyhow::anyhow!("input identity unavailable: {e}"))?;
            let now = ato_runtime_attempt::control::now_ms();
            let expiry = if reuse == "this_formation" {
                deadline
            } else {
                now.checked_add(args.expires_in_seconds.unwrap() * 1000)
                    .context("input expiry overflow")?
            };
            let saved = json!({"schema":"ato.formation-input-checkpoint/1","identity":identity,"request_id":new_search_id(entropy),"expires_at_ms":expiry});
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let created = !parent.exists();
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            if created {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            #[cfg(not(unix))]
            let _ = created;
            save_owner_checkpoint(&path, &serde_jcs::to_vec(&saved)?)?;
            saved
        };
        json!({"kind":"register","request_id":saved["request_id"],"requirement":requirement,"reuse":reuse,"expires_at_ms":saved["expires_at_ms"],"allow_artifact_embedding":args.allow_artifact_embedding})
    };
    if args.value_stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(16386)
            .read_to_end(&mut bytes)?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        ensure!(
            !bytes.is_empty() && bytes.len() <= 16384 && !bytes.contains(&0),
            "input value must be 1..16384 UTF-8 bytes without NUL"
        );
        body["value"] = json!(String::from_utf8(bytes).context("input value must be UTF-8")?);
    }
    let result = client
        .with_deadline(deadline)
        .provide_formation_input(&args.search_id, &body)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"binding":result,"search_budget":view["search_budget"],"resume":"Repeat the original ato form command with its existing exploration config and journal; the deadline and budget remain unchanged."})
        )?
    );
    Ok(())
}
