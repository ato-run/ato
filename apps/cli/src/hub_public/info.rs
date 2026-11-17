//! Version-scoped publisher advice. This never changes a bundle or its Contract.
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{HubRef, HubSelector, api_base, api_client, api_failure, parse_digest, resolve_remote};

const MAX_DOCUMENT_BYTES: u64 = 16 * 1024;
const MAX_REVISION: u64 = 9_007_199_254_740_990;

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Public reference: capsules/<owner>/<name>[:tag|@sha256:…].
    pub reference: String,
    /// Exact declared DerivationRef to read or describe.
    #[arg(long)]
    pub derivation: String,
    /// Write {"expected_revision":N,"info":{…}} from this JSON file.
    /// Without a file, prints the current public information as JSON.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Ato API origin, matching the credential saved by ato login.
    #[arg(long, env = "ATO_API_URL", default_value = crate::auth_device::DEFAULT_API_URL)]
    pub api_url: String,
    /// Owner credential for updates only; otherwise uses the origin-scoped saved login.
    #[arg(long, env = "ATO_API_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InfoUpdate {
    expected_revision: u64,
    info: serde_json::Map<String, Value>,
}

fn read_update(path: &std::path::Path) -> Result<InfoUpdate> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_DOCUMENT_BYTES,
        "derivation information exceeds 16 KiB"
    );
    let update: InfoUpdate = serde_json::from_slice(&bytes)
        .context("expected JSON {\"expected_revision\":N,\"info\":{…}}")?;
    ensure!(
        update.expected_revision <= MAX_REVISION,
        "invalid expected_revision"
    );
    Ok(update)
}

pub fn describe(args: DescribeArgs) -> Result<()> {
    let reference = HubRef::parse(&args.reference)?
        .context("expected capsules/<owner>/<name>[:tag|@sha256:…]")?;
    let derivation = parse_digest(&args.derivation).map_err(anyhow::Error::msg)?;
    // Parse the complete write intent before authenticating or making requests.
    let update = args.file.as_deref().map(read_update).transpose()?;
    let base = api_base(&args.api_url)?;
    let client = api_client()?;
    let resolved = resolve_remote(&client, &base, &reference)?;
    ensure!(
        resolved.reference == reference.reference(),
        "resolve returned a different Capsule"
    );
    let digest = parse_digest(&resolved.digest).map_err(anyhow::Error::msg)?;
    match &reference.selector {
        HubSelector::Digest(expected) => {
            ensure!(&digest == expected, "resolve returned a different digest")
        }
        HubSelector::Tag(expected) => ensure!(
            resolved.tag.as_ref() == Some(expected),
            "resolve returned a different tag"
        ),
    }
    // Pin the resolved digest, even when the caller named a mutable tag.
    let url = format!(
        "{base}/v1/hub/capsules/{}/{}/versions/{digest}/derivations/{derivation}/info",
        reference.owner, reference.name
    );
    let request = match &update {
        Some(update) => {
            let token = crate::auth_device::resolve_token(args.token, &args.api_url)?;
            client.put(&url).bearer_auth(token).json(update)
        }
        None => client.get(&url),
    };
    // api_client refuses redirects, including for credential-bearing writes.
    let response = request
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .with_context(|| format!("failed to reach the Ato API at {base}"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_DOCUMENT_BYTES,
        "derivation information response exceeds 16 KiB"
    );
    if !status.is_success() {
        return Err(api_failure(
            "describe",
            &reference.reference(),
            &base,
            status,
            &String::from_utf8_lossy(&bytes),
        ));
    }
    let value: Value =
        serde_json::from_slice(&bytes).context("invalid derivation information response")?;
    ensure!(
        value["digest"].as_str() == Some(&digest)
            && value["derivation_ref"].as_str() == Some(&derivation)
            && value["source"].as_str() == Some("publisher")
            && value["info"].is_object(),
        "derivation information response names a different target or source"
    );
    let revision = value["revision"]
        .as_u64()
        .context("invalid information revision")?;
    ensure!(revision <= MAX_REVISION + 1, "invalid information revision");
    if let Some(update) = update {
        ensure!(
            revision == update.expected_revision + 1,
            "server did not confirm the expected revision update"
        );
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
