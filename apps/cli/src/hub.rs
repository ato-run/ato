//! `ato hub push` / `ato hub pull`: client-side capsulehub distribution.
//!
//! The hub is a blind ciphertext store (see `services/capsulehub` and the
//! Capsulehub RFC). This module owns the client half of the trust model:
//!
//! - **push** validates the formed Capsule *before* encryption, so only a
//!   bundle that the local Run path would accept is ever uploaded;
//! - **pull** re-runs the same validation *after* decryption and refuses to
//!   write anything when the ciphertext digest, the AEAD authentication, the
//!   bundle validation, or the claimed `ContractRef` disagrees.
//!
//! Push order is fixed: read the file, dispatch the wire document (v3/v4 only;
//! computation-root v2 and unknown profiles fail closed with a named error),
//! run the full `validate_bytes_all` path, encrypt to a temporary file, PUT the
//! ciphertext, print the shared link.
//!
//! Pull order is fixed: parse the link strictly, download the ciphertext with
//! redirects disabled, check the ciphertext digest, decrypt/authenticate,
//! validate the bundle, recompute the `ContractRef` from canonical
//! `ato.contract/1` bytes and compare it to `cid`, then fsync and atomically
//! rename into `--output`. Pull never runs or imports the capsule.
//!
//! The link key lives only in the URL fragment, which is never part of an HTTP
//! request target. Errors never include key material.
//!
//! ## Credentials
//!
//! Both directions accept an optional bearer credential (`--token` or
//! `ATO_HUB_TOKEN`). Reads are anonymous by default; a pull credential is sent
//! so a coordinator started with `--allow-anonymous-read false` remains
//! pull-able. Uploads go to the hub named by `--hub-url`, so the upload token
//! belongs to that explicit target.
//!
//! A shared link, however, can name any host. Therefore:
//!
//! - an explicit `--token` is always sent to the link's hub (the user chose
//!   that credential for that link);
//! - `ATO_HUB_TOKEN` is sent **only** when the link's hub matches the
//!   configured hub (`--hub-url` or `ATO_HUB_URL`), compared as scheme, host,
//!   explicit non-default port, and base path after normalization (casefolded
//!   host, trailing slashes removed). A different path on the same origin is a
//!   different hub. If no hub is configured or it differs, the environment
//!   token is withheld — never silently sent to a foreign host — and an
//!   authentication failure names that rule and asks for `--token`;
//! - with neither, the download is anonymous.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_objects::CapsuleBundleDocument;
use ato_objects::capsulehub::{
    HUB_CHUNK_BYTES, HubKey, HubObjectId, MAX_HUB_PLAINTEXT_BYTES, SharedLink, decrypt_file,
    encrypt_file, object_id_of_file,
};
use ato_objects::decode_capsule_bundle_document;
use ato_portable_application::validate_bytes_all;
use clap::{Args, Subcommand};
use reqwest::blocking::{Body, Client};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

/// Environment fallback for the hub base URL. There is deliberately no
/// built-in production default.
pub const HUB_URL_ENV: &str = "ATO_HUB_URL";
/// Environment fallback for the upload bearer token.
pub const HUB_TOKEN_ENV: &str = "ATO_HUB_TOKEN";

const HUB_OBJECTS_PATH: &str = "/v1/capsulehub/objects/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Subcommand)]
pub enum HubCommands {
    /// Validate a formed `.capsule`, encrypt it, upload it, and print the link.
    ///
    /// No Capsule identity is registered anywhere: the coordinator only stores
    /// opaque ciphertext addressed by its SHA-256 digest.
    Push(PushArgs),
    /// Download, verify, decrypt, and validate a shared link into a file.
    ///
    /// The capsule is never run or imported by this command.
    Pull(PullArgs),
}

#[derive(Args)]
pub struct PushArgs {
    /// `.capsule` bundle to publish through the hub.
    pub capsule: PathBuf,
    /// Hub base URL (origin plus optional path base). Falls back to
    /// `ATO_HUB_URL`; there is no default hub.
    #[arg(long)]
    pub hub_url: Option<String>,
    /// Upload bearer token. Falls back to `ATO_HUB_TOKEN`.
    #[arg(long)]
    pub token: Option<String>,
    /// Permit plain HTTP to a literal loopback host (self-hosted coordinator).
    #[arg(long)]
    pub allow_loopback_http: bool,
}

#[derive(Args)]
pub struct PullArgs {
    /// Shared link. The key is in the fragment and is never sent to the hub.
    pub link: String,
    /// Destination `.capsule` file.
    #[arg(long, short = 'o')]
    pub output: PathBuf,
    /// Overwrite an existing destination file.
    #[arg(long)]
    pub force: bool,
    /// Configured hub base URL (origin plus optional path base). Falls back to
    /// `ATO_HUB_URL`. Used only to decide whether `ATO_HUB_TOKEN` may be sent
    /// to the link's hub; the link itself names the download target.
    #[arg(long)]
    pub hub_url: Option<String>,
    /// Bearer token for coordinators that require authenticated reads. Sent
    /// only when given explicitly; otherwise `ATO_HUB_TOKEN` is sent only to
    /// the configured hub. Omitted means an anonymous download.
    #[arg(long)]
    pub token: Option<String>,
    /// Permit plain HTTP to a literal loopback host (self-hosted coordinator).
    #[arg(long)]
    pub allow_loopback_http: bool,
}

pub fn run(command: HubCommands) -> Result<()> {
    match command {
        HubCommands::Push(args) => push(args),
        HubCommands::Pull(args) => pull(args),
    }
}

fn push(args: PushArgs) -> Result<()> {
    let capsule = args.capsule;
    let metadata =
        fs::metadata(&capsule).with_context(|| format!("cannot read {}", capsule.display()))?;
    if metadata.len() > MAX_HUB_PLAINTEXT_BYTES {
        bail!(
            "{} is {} bytes; a hub object carries at most {} plaintext bytes",
            capsule.display(),
            metadata.len(),
            MAX_HUB_PLAINTEXT_BYTES
        );
    }
    let bytes = fs::read(&capsule).with_context(|| format!("cannot read {}", capsule.display()))?;
    // Formed-capsule admission is client-side: the hub cannot read the bytes.
    let contract_ref = validate_formed_bundle(&bytes)
        .with_context(|| format!("{} must not be pushed", capsule.display()))?;

    let hub_url = args
        .hub_url
        .or_else(|| std::env::var(HUB_URL_ENV).ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "a hub base URL is required: pass --hub-url or set {HUB_URL_ENV} (no default hub)"
            )
        })?;
    let base = resolve_hub_base(&hub_url, args.allow_loopback_http)?;
    let token = args
        .token
        .or_else(|| std::env::var(HUB_TOKEN_ENV).ok())
        .filter(|value| !value.is_empty());

    // Encrypt exactly the bytes that were just validated: re-reading the user's
    // file could encrypt a different (unvalidated) bundle if it changed after
    // validation.
    let validated = tempfile::Builder::new()
        .prefix("ato-hub-push-plaintext-")
        .suffix(".capsule")
        .tempfile()
        .context("failed to create a temporary validation file")?;
    fs::write(validated.path(), &bytes)
        .context("failed to stage the validated capsule for encryption")?;
    drop(bytes);

    let key = HubKey::generate().context("failed to generate a hub key")?;
    let ciphertext = tempfile::Builder::new()
        .prefix("ato-hub-push-")
        .suffix(".capsule.hub")
        .tempfile()
        .context("failed to create a temporary ciphertext file")?;
    let object_id = encrypt_file(&key, validated.path(), ciphertext.path())
        .context("failed to encrypt the capsule for the hub")?;

    let url = object_url(&base, &object_id);
    let client = hub_client()?;
    let file = fs::File::open(ciphertext.path())
        .context("failed to reopen the encrypted capsule for upload")?;
    let ciphertext_len = file
        .metadata()
        .context("failed to read the encrypted capsule size")?
        .len();
    let mut request = client
        .put(&url)
        .header(CONTENT_TYPE, "application/octet-stream")
        // A known length keeps the upload a single Content-Length body instead
        // of a chunked stream the coordinator would have to buffer differently.
        .body(Body::sized(file, ciphertext_len));
    if let Some(token) = token.as_deref() {
        request = request.header(AUTHORIZATION, bearer(token));
    }
    let response = request
        .send()
        .with_context(|| format!("failed to reach the hub at {base}"))?;
    upload_status(response.status(), &base)?;

    let link = SharedLink::new(&base, object_id, key, contract_ref)
        .context("failed to build the shared link")?;
    let link = link
        .to_url(&base)
        .context("failed to render the shared link")?;
    // The link carries the key in its fragment; stdout is the user-facing
    // artifact, not a log.
    println!("{link}");
    Ok(())
}

fn pull(args: PullArgs) -> Result<()> {
    let link = SharedLink::parse(&args.link).context("invalid capsulehub link")?;
    let base = link.base_url().context("invalid capsulehub link base")?;
    ensure_transport_allowed(&base, args.allow_loopback_http)?;

    let output = args.output;
    if output.exists() && !args.force {
        bail!(
            "refusing to overwrite {}; pass --force to replace it",
            output.display()
        );
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("the output directory {} does not exist", parent.display());
    }

    // The link names the download target, but it can name any host: never send
    // the environment token there unless it is the configured hub.
    let link_identity = hub_base_identity(&base)?;
    let configured_hub = args
        .hub_url
        .or_else(|| std::env::var(HUB_URL_ENV).ok())
        .filter(|value| !value.is_empty())
        .map(|value| {
            hub_base_identity(&value)
                .with_context(|| format!("invalid configured hub base URL `{value}`"))
        })
        .transpose()?;
    let explicit_token = args.token.filter(|value| !value.is_empty());
    let env_token = std::env::var(HUB_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    let (token, env_token_withheld) = match explicit_token {
        Some(token) => (Some(token), false),
        None => match env_token {
            Some(token) if configured_hub.as_deref() == Some(link_identity.as_str()) => {
                (Some(token), false)
            }
            Some(_) => (None, true),
            None => (None, false),
        },
    };

    let client = hub_client()?;
    let url = object_url(&base, &link.object_id);
    let mut request = client.get(&url);
    if let Some(token) = token.as_deref() {
        request = request.header(AUTHORIZATION, bearer(token));
    }
    let response = request
        .send()
        .with_context(|| format!("failed to reach the hub at {base}"))?;
    let status = response.status();
    if status.is_redirection() {
        bail!("hub returned a redirect ({status}); redirects are not followed");
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "the hub does not have object {} (it may have been removed by the operator)",
            link.object_id
        );
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        if env_token_withheld {
            bail!(
                "hub refused the download ({status}); {HUB_TOKEN_ENV} is only sent to the hub configured with --hub-url or {HUB_URL_ENV}, and this link names {base}; pass --token explicitly to authenticate to this hub"
            );
        }
        bail!(
            "hub refused the download ({status}); pass --token or set {HUB_TOKEN_ENV} with a matching --hub-url to authenticate to this coordinator"
        );
    }
    ensure!(status.is_success(), "hub download failed ({status})");

    // Download to a bounded temporary file next to the destination so the
    // final step can be an atomic rename on the same filesystem.
    let ciphertext = tempfile::Builder::new()
        .prefix(".ato-hub-pull-ciphertext-")
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "failed to create a temporary file next to {}",
                output.display()
            )
        })?;
    let limit = max_ciphertext_bytes();
    let mut reader = response.take(limit.saturating_add(1));
    let mut sink = ciphertext.as_file();
    let copied =
        io::copy(&mut reader, &mut sink).context("failed while downloading the hub object")?;
    if copied > limit {
        bail!("the hub response exceeds the {limit} byte bounded ciphertext size");
    }
    let actual =
        object_id_of_file(ciphertext.path()).context("failed to hash the downloaded ciphertext")?;
    if actual != link.object_id {
        bail!(
            "ciphertext digest mismatch: the link names {}, the hub served {}",
            link.object_id,
            actual
        );
    }

    let plaintext = tempfile::Builder::new()
        .prefix(".ato-hub-pull-capsule-")
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "failed to create a temporary file next to {}",
                output.display()
            )
        })?;
    decrypt_file(
        &link.key,
        &link.object_id,
        ciphertext.path(),
        plaintext.path(),
    )
    .context("failed to decrypt and authenticate the hub object")?;

    let bytes = fs::read(plaintext.path())
        .context("failed to read the decrypted capsule for validation")?;
    let contract_ref =
        validate_formed_bundle(&bytes).context("the decrypted capsule failed validation")?;
    if contract_ref != link.cid {
        bail!(
            "capsule identity mismatch: the link claims cid {}, the canonical ContractRef is {}",
            link.cid,
            contract_ref
        );
    }

    plaintext
        .as_file()
        .sync_all()
        .context("failed to fsync the downloaded capsule")?;
    if args.force {
        plaintext.persist(&output).map_err(|error| {
            anyhow::anyhow!("failed to move the capsule into place: {}", error.error)
        })?;
    } else {
        plaintext.persist_noclobber(&output).map_err(|error| {
            anyhow::anyhow!(
                "failed to move the capsule into place (does {} already exist?): {}",
                output.display(),
                error.error
            )
        })?;
    }
    sync_directory_best_effort(parent);

    println!("contract_ref={contract_ref}");
    println!("output={}", output.display());
    Ok(())
}

/// Client-side formed-capsule validation shared by push and pull.
///
/// Only contract-root portable application bundles (wire v3/v4) are hub
/// objects: their identity is a `ContractRef`. Computation-root wire v2 is a
/// different reference domain and must never be relabelled as a capsule
/// identity. The returned value is the `ContractRef` recomputed by the
/// portable-application validator from canonical `ato.contract/1` bytes.
fn validate_formed_bundle(bytes: &[u8]) -> Result<String> {
    let document = decode_capsule_bundle_document(bytes)
        .context("the file is not a supported portable application bundle (wire v3/v4)")?;
    match document {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => {
            let (_, validated) = validate_bytes_all(bytes)
                .context("portable-application validation failed (schema, digests, closure, declared routes, or the v4 snapshot binding)")?;
            let Some(first) = validated.first() else {
                bail!("the bundle declares no derivation");
            };
            for item in &validated {
                ensure!(
                    item.contract_ref == first.contract_ref,
                    "validated derivations disagree on the ContractRef"
                );
            }
            ensure!(
                bundle.index.root_contract_ref == first.contract_ref.as_str(),
                "bundle root_contract_ref does not name the canonical Contract"
            );
            Ok(first.contract_ref.to_string())
        }
        CapsuleBundleDocument::ComputationV2(_) => bail!(
            "computation-root wire v2 is not a contract-root capsule: its root is a ComputationRef, never a ContractRef; export a wire v3/v4 portable application before using a hub"
        ),
    }
}

fn hub_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("ato/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build the hub HTTP client")
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn object_url(base: &str, object_id: &HubObjectId) -> String {
    format!("{base}{HUB_OBJECTS_PATH}{}", object_id.as_str())
}

/// Ciphertext bound for one object: the plaintext bound plus per-frame
/// overhead (4-byte length, tag byte, 16-byte MAC) at the maximum chunk count.
fn max_ciphertext_bytes() -> u64 {
    let chunks = MAX_HUB_PLAINTEXT_BYTES / HUB_CHUNK_BYTES as u64 + 2;
    MAX_HUB_PLAINTEXT_BYTES
        .saturating_add(chunks.saturating_mul(4 + 17))
        .saturating_add(31)
}

fn upload_status(status: reqwest::StatusCode, base: &str) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("hub rejected the upload ({status}); check --token or {HUB_TOKEN_ENV}");
    }
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        bail!("hub rejected the upload ({status}); the ciphertext exceeds the hub's object limit");
    }
    if status == reqwest::StatusCode::INSUFFICIENT_STORAGE {
        bail!("hub rejected the upload ({status}); the hub storage quota is exhausted");
    }
    if status.is_redirection() {
        bail!("hub returned a redirect ({status}); redirects are not followed");
    }
    bail!("hub rejected the upload ({status}) at {base}");
}

/// Resolves and validates the hub base URL supplied for a push.
fn resolve_hub_base(input: &str, allow_loopback_http: bool) -> Result<String> {
    let base = input.trim();
    if base.is_empty() {
        bail!("the hub base URL is empty");
    }
    if base.contains('?') || base.contains('#') {
        bail!("the hub base URL must not carry a query or fragment");
    }
    let base = base.trim_end_matches('/');
    ensure_transport_allowed(base, allow_loopback_http)?;
    if base.ends_with("/c") {
        bail!("the hub base URL must not include the `/c/` link segment");
    }
    Ok(base.to_owned())
}

/// HTTPS is mandatory. Plain HTTP is accepted only for a literal loopback
/// host and only with the explicit `--allow-loopback-http`.
fn ensure_transport_allowed(base: &str, allow_loopback_http: bool) -> Result<()> {
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("the hub URL must be an absolute http(s) URL"))?;
    if rest.is_empty() {
        bail!("the hub URL has no host");
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        bail!("the hub URL has no host");
    }
    if authority.contains('@') {
        bail!("the hub URL must not carry userinfo");
    }
    match scheme {
        "https" => Ok(()),
        "http" => {
            if !allow_loopback_http {
                bail!(
                    "the hub URL uses plain HTTP; HTTPS is required except for a literal loopback host with --allow-loopback-http"
                );
            }
            if !is_loopback_host(authority) {
                bail!(
                    "plain HTTP is allowed only for a literal loopback host (127.0.0.1, [::1], localhost)"
                );
            }
            Ok(())
        }
        other => bail!("unsupported hub URL scheme `{other}`; http(s) is required"),
    }
}

fn is_loopback_host(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, _)) => &authority[..inside.len() + 2],
            None => authority,
        }
    } else if authority == "::1" {
        authority
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "127.0.0.1" | "[::1]" | "::1" | "localhost")
}

fn sync_directory_best_effort(path: &Path) {
    // Directory fsync is unsupported on some platforms (e.g. macOS). The
    // rename itself is atomic; failure to sync the directory is not reported.
    if let Ok(directory) = fs::File::open(path) {
        let _ = directory.sync_all();
    }
}

/// Identity of a hub base used to decide whether `ATO_HUB_TOKEN` may be sent.
///
/// Compares scheme, host, an explicit non-default port, and base path after
/// normalization: scheme/host casefolded, default ports elided, trailing
/// slashes removed. Query and fragment are already rejected by the link parser
/// and by the configured-hub validation, so they never participate.
///
/// A different path on the same origin is a different hub, and the token is
/// withheld for it.
fn hub_base_identity(base: &str) -> Result<String> {
    let base = base.trim().trim_end_matches('/');
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("the hub base URL must be an absolute http(s) URL"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        bail!("unsupported hub URL scheme `{scheme}`; http(s) is required");
    }
    if rest.is_empty() {
        bail!("the hub base URL has no host");
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    if authority.is_empty() {
        bail!("the hub base URL has no host");
    }
    if authority.contains('@') {
        bail!("the hub base URL must not carry userinfo");
    }
    let (host, port) = split_authority(authority)?;
    let port = port.filter(|port| default_port(&scheme) != Some(*port));
    let path = path.trim_matches('/');

    let mut identity = format!("{scheme}://{}", host.to_ascii_lowercase());
    if let Some(port) = port {
        identity.push(':');
        identity.push_str(&port.to_string());
    }
    if !path.is_empty() {
        identity.push('/');
        identity.push_str(path);
    }
    Ok(identity)
}

fn split_authority(authority: &str) -> Result<(&str, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (inside, remainder) = rest
            .split_once(']')
            .ok_or_else(|| anyhow::anyhow!("the hub base URL has an invalid IPv6 host"))?;
        let host = &authority[..inside.len() + 2];
        if remainder.is_empty() {
            return Ok((host, None));
        }
        let Some(port) = remainder.strip_prefix(':') else {
            bail!("the hub base URL has an invalid host");
        };
        return Ok((host, Some(parse_hub_port(port)?)));
    }
    // An unbracketed IPv6 loopback literal has no port.
    if authority == "::1" {
        return Ok((authority, None));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if port.is_empty() {
            bail!("the hub base URL has an invalid port");
        }
        if port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok((host, Some(parse_hub_port(port)?)));
        }
        // A non-bracketed authority with a non-numeric tail is not a valid
        // host:port form.
        bail!("the hub base URL has an invalid port");
    }
    Ok((authority, None))
}

fn parse_hub_port(text: &str) -> Result<u16> {
    text.parse()
        .map_err(|_| anyhow::anyhow!("the hub base URL has an invalid port"))
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_base_identity_normalizes_scheme_host_port_and_trailing_slashes() {
        assert_eq!(
            hub_base_identity("https://hub.example").unwrap(),
            "https://hub.example"
        );
        assert_eq!(
            hub_base_identity("https://hub.example/").unwrap(),
            "https://hub.example"
        );
        assert_eq!(
            hub_base_identity("https://HUB.Example:443").unwrap(),
            "https://hub.example"
        );
        assert_eq!(
            hub_base_identity("https://hub.example/base/").unwrap(),
            "https://hub.example/base"
        );
        assert_eq!(
            hub_base_identity("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            hub_base_identity("http://127.0.0.1:80").unwrap(),
            "http://127.0.0.1"
        );
        assert_eq!(
            hub_base_identity("https://[::1]:8443").unwrap(),
            "https://[::1]:8443"
        );
    }

    #[test]
    fn a_different_host_or_base_path_is_a_different_hub() {
        let configured = hub_base_identity("https://hub.example/base").unwrap();
        assert_ne!(
            configured,
            hub_base_identity("https://hub.example/other").unwrap()
        );
        assert_ne!(
            configured,
            hub_base_identity("https://hub.example").unwrap()
        );
        assert_ne!(
            configured,
            hub_base_identity("http://hub.example/base").unwrap()
        );
        assert_ne!(
            configured,
            hub_base_identity("https://other.example/base").unwrap()
        );
        assert_eq!(
            configured,
            hub_base_identity("https://HUB.EXAMPLE:443/base/").unwrap()
        );
    }

    #[test]
    fn hub_base_identity_rejects_malformed_values() {
        for value in [
            "",
            "hub.example",
            "ftp://hub.example",
            "https://",
            "https://user:pw@hub.example",
            "https://hub.example:",
            "https://hub.example:notaport",
            "https://[::1",
        ] {
            assert!(
                hub_base_identity(value).is_err(),
                "`{value}` must be rejected"
            );
        }
    }
}
