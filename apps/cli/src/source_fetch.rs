//! Fetch a pinned source snapshot from a public GitHub repository.
//!
//! One flow, in this order: parse the HTTPS URL, resolve the requested ref to a
//! full commit SHA, download the archive for that SHA, then hand the bytes to
//! the existing [`FileVerifiedArchive`] authority. That authority measures the
//! tree and applies the containment rules (path traversal, absolute paths,
//! links, size and file-count limits) during extraction; this module never
//! unpacks an archive itself.
//!
//! What this does NOT claim: the archive digest is computed here, because
//! codeload serves no digest. It is evidence of what arrived, not a signature
//! from GitHub. The commit SHA is the pinned identity, and it is resolved
//! through `api.github.com`, so the provenance produced here records both.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_formation::source::{FileVerifiedArchive, SourceLimits};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

pub const SOURCE_FETCH_SCHEMA: &str = "ato.source-fetch/1";
/// The archive this command will accept before it is even verified.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const GITHUB_API: &str = "https://api.github.com";

/// A parsed `https://github.com/<owner>/<repo>[/tree/<ref>[/<path>]]` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSource {
    pub owner: String,
    pub repository: String,
    pub reference: Option<String>,
    pub subpath: String,
}

impl GitHubSource {
    pub fn parse(input: &str) -> Result<Self> {
        let Some(rest) = input.strip_prefix("https://github.com/") else {
            bail!(
                "unsupported-source: only `https://github.com/<owner>/<repo>` URLs are accepted (got `{input}`)"
            );
        };
        let rest = rest.trim_end_matches('/');
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let mut segments = rest.split('/');
        let owner = segments.next().unwrap_or_default();
        let repository = segments.next().unwrap_or_default();
        validate_owner(owner)?;
        validate_repository(repository)?;
        let mut reference = None;
        let mut subpath = String::new();
        match segments.next() {
            None => {}
            Some("tree") => {
                let requested = segments.next().context(
                    "invalid-github-url: `/tree/` must name a ref, e.g. /tree/main or /tree/<commit>",
                )?;
                validate_ref(requested)?;
                reference = Some(requested.to_owned());
                let tail: Vec<&str> = segments.collect();
                if !tail.is_empty() {
                    let joined = tail.join("/");
                    validate_subpath(&joined)?;
                    subpath = joined;
                }
            }
            Some(other) => bail!(
                "invalid-github-url: `/{other}` is not a supported GitHub path; use /tree/<ref>[/<path>]"
            ),
        }
        Ok(Self {
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            reference,
            subpath,
        })
    }
}

fn validate_owner(owner: &str) -> Result<()> {
    let valid = !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !valid {
        bail!("invalid-github-url: `{owner}` is not a GitHub owner");
    }
    Ok(())
}

fn validate_repository(repository: &str) -> Result<()> {
    let valid = !repository.is_empty()
        && repository.len() <= 100
        && !matches!(repository, "." | "..")
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    if !valid {
        bail!("invalid-github-url: `{repository}` is not a GitHub repository name");
    }
    Ok(())
}

fn validate_ref(reference: &str) -> Result<()> {
    let valid = !reference.is_empty()
        && reference.len() <= 255
        && !reference.contains("..")
        && !reference.starts_with('-')
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte));
    if !valid {
        bail!("invalid-github-url: `{reference}` is not a usable ref");
    }
    Ok(())
}

fn validate_subpath(subpath: &str) -> Result<()> {
    let valid = !subpath.is_empty()
        && !subpath.starts_with('/')
        && subpath.split('/').all(|segment| {
            !segment.is_empty() && segment != "." && segment != ".." && !segment.contains('\\')
        });
    if !valid {
        bail!("invalid-github-url: `{subpath}` is not a contained subdirectory");
    }
    Ok(())
}

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn percent_encode_ref(reference: &str) -> String {
    let mut encoded = String::with_capacity(reference.len());
    for byte in reference.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Resolve the requested ref to the full commit SHA that pins this source.
pub fn resolve_commit(
    client: &Client,
    source: &GitHubSource,
    requested: Option<&str>,
) -> Result<String> {
    if let Some(reference) = requested
        && is_commit_sha(reference)
    {
        return Ok(reference.to_ascii_lowercase());
    }
    let reference = match requested {
        Some(reference) => reference.to_owned(),
        None => default_branch(client, source)?,
    };
    let url = format!(
        "{GITHUB_API}/repos/{}/{}/commits/{}",
        source.owner,
        source.repository,
        percent_encode_ref(&reference)
    );
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("github-unreachable: cannot reach api.github.com")?;
    let status = response.status();
    // GitHub answers 404 for an unknown repository but 422 ("No commit found
    // for SHA") for a ref that names nothing, so both mean the same thing here.
    if status == reqwest::StatusCode::NOT_FOUND
        || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
    {
        bail!(
            "github-ref-not-found: `{reference}` does not name a commit in {}/{}",
            source.owner,
            source.repository
        );
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        bail!(
            "github-rate-limited: api.github.com refused the ref lookup ({status}); retry later or pass an explicit 40-character commit with --ref"
        );
    }
    if !status.is_success() {
        bail!("github-api-failed: api.github.com answered {status} for the ref lookup");
    }
    let document: serde_json::Value = response
        .json()
        .context("github-api-failed: the ref lookup did not return JSON")?;
    let sha = document["sha"]
        .as_str()
        .context("github-api-failed: the ref lookup returned no commit sha")?;
    if !is_commit_sha(sha) {
        bail!("github-api-failed: the ref lookup returned a non-commit sha");
    }
    Ok(sha.to_ascii_lowercase())
}

fn default_branch(client: &Client, source: &GitHubSource) -> Result<String> {
    let url = format!("{GITHUB_API}/repos/{}/{}", source.owner, source.repository);
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("github-unreachable: cannot reach api.github.com")?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "github-repository-not-found: {}/{} is not a public repository",
            source.owner,
            source.repository
        );
    }
    if !status.is_success() {
        bail!("github-api-failed: api.github.com answered {status} for the repository lookup");
    }
    let document: serde_json::Value = response
        .json()
        .context("github-api-failed: the repository lookup did not return JSON")?;
    let branch = document["default_branch"]
        .as_str()
        .context("github-api-failed: the repository lookup returned no default branch")?;
    validate_ref(branch)?;
    Ok(branch.to_owned())
}

#[derive(Debug, clap::Args)]
pub struct SourceFetchArgs {
    /// `https://github.com/<owner>/<repo>[/tree/<ref>[/<path>]]`, HTTPS only.
    pub url: String,
    /// Resolve this ref instead of the URL's own. A 40-character commit is
    /// used as-is; anything else is resolved through api.github.com.
    #[arg(long)]
    pub reference: Option<String>,
    /// Materialize this contained subdirectory instead of the repository root.
    #[arg(long)]
    pub path: Option<String>,
    /// New directory that receives the archive, the tree and the provenance.
    #[arg(short, long, default_value = "source-snapshot")]
    pub out: PathBuf,
    /// Emit one JSON object instead of human-readable lines.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Subcommand)]
pub enum SourceCommands {
    /// Download, verify and materialize a pinned GitHub source snapshot.
    Fetch(SourceFetchArgs),
}

/// What one fetch produced, for callers that do not want its output printed.
pub struct FetchedSource {
    pub provenance: serde_json::Value,
    pub root: PathBuf,
    pub commit: String,
    pub tree_digest: String,
}

pub fn fetch(args: SourceFetchArgs) -> Result<()> {
    let fetched = fetch_to(
        &args.url,
        args.reference.as_deref(),
        args.path.as_deref(),
        &args.out,
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&fetched.provenance)?);
    } else {
        let source = &fetched.provenance["source"];
        println!(
            "Source: {}/{}",
            source["owner"].as_str().unwrap_or("-"),
            source["repository"].as_str().unwrap_or("-")
        );
        println!("Commit: {}", fetched.commit);
        println!("Tree:   {}", fetched.tree_digest);
        println!(
            "Files:  {} ({} bytes)",
            fetched.provenance["tree"]["file_count"], fetched.provenance["tree"]["expanded_bytes"]
        );
        println!("Root:   {}", fetched.root.display());
        println!("Provenance: {}", args.out.join("provenance.json").display());
    }
    Ok(())
}

/// Fetch without printing: the caller owns the output.
pub fn fetch_to(
    url: &str,
    reference: Option<&str>,
    path: Option<&str>,
    out: &Path,
) -> Result<FetchedSource> {
    let source = GitHubSource::parse(url)?;
    let subpath = match (path, source.subpath.as_str()) {
        (Some(path), "") => path.to_owned(),
        (None, subpath) => subpath.to_owned(),
        (Some(_), _) => bail!(
            "invalid-github-url: the URL already names a subdirectory; drop either the URL path or --path"
        ),
    };
    if !subpath.is_empty() {
        validate_subpath(&subpath)?;
    }
    let requested = reference.or(source.reference.as_deref());
    if let Some(reference) = requested {
        validate_ref(reference)?;
    }
    prepare_output(out)?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("ato-cli (source fetch)")
        .timeout(Duration::from_secs(120))
        .build()
        .context("cannot configure the source transport")?;

    let commit = resolve_commit(&client, &source, requested)?;
    let archive_path = out.join("source.archive");
    let (archive_sha256, archive_bytes) = download(
        &client,
        &format!(
            "https://codeload.github.com/{}/{}/tar.gz/{commit}",
            source.owner, source.repository
        ),
        &archive_path,
    )?;

    let limits = SourceLimits::default();
    let file =
        File::open(&archive_path).with_context(|| format!("read {}", archive_path.display()))?;
    let mut verified = FileVerifiedArchive::verify(file, &archive_sha256, archive_bytes, limits)
        .map_err(|error| anyhow::anyhow!("archive-rejected: {error}"))?;
    let tree_digest = verified.tree_digest().to_owned();
    let closure_ref = verified
        .closure_ref(&subpath)
        .map_err(|error| anyhow::anyhow!("archive-rejected: {error}"))?
        .to_string();
    let format = verified.archive_format();
    let expanded_bytes = verified.expanded_bytes();
    let file_count = verified.expanded_file_count();
    let root = verified
        .materialize(&out.join("source"), &subpath, limits)
        .map_err(|error| anyhow::anyhow!("archive-rejected: {error}"))?;

    let provenance = serde_json::json!({
        "schema": SOURCE_FETCH_SCHEMA,
        "source": {
            "host": "github.com",
            "url": url,
            "owner": source.owner,
            "repository": source.repository,
            "requested_ref": requested,
            "resolved_commit_sha": commit,
            "subdirectory": if subpath.is_empty() { serde_json::Value::Null } else { serde_json::json!(subpath) },
        },
        "archive": {
            "url": format!("https://codeload.github.com/{}/{}/tar.gz/{commit}", source.owner, source.repository),
            "format": format,
            "sha256": archive_sha256,
            "bytes": archive_bytes,
            "digest_authority": "client-computed: codeload serves no digest, so this is transport evidence, not a GitHub signature",
        },
        "tree": {
            "digest": tree_digest,
            "closure_ref": closure_ref,
            "expanded_bytes": expanded_bytes,
            "file_count": file_count,
        },
        "extracted": {
            "root": root.display().to_string(),
            "relative_to_output": root.strip_prefix(&out).unwrap_or(&root).display().to_string(),
        },
        "limits": {
            "max_archive_bytes": MAX_ARCHIVE_BYTES,
            "max_files": limits.max_files,
            "max_total_bytes": limits.max_total_bytes,
            "max_path_depth": limits.max_path_depth,
            "max_path_bytes": limits.max_path_bytes,
        },
    });
    let provenance_path = out.join("provenance.json");
    std::fs::write(&provenance_path, serde_json::to_vec_pretty(&provenance)?)
        .with_context(|| format!("write {}", provenance_path.display()))?;

    Ok(FetchedSource {
        provenance,
        root,
        commit,
        tree_digest,
    })
}

fn prepare_output(out: &Path) -> Result<()> {
    if out.exists() {
        let occupied = std::fs::read_dir(out)
            .with_context(|| format!("read {}", out.display()))?
            .next()
            .is_some();
        if occupied {
            bail!(
                "output-not-empty: {} already holds files; a source snapshot is written once",
                out.display()
            );
        }
    } else {
        std::fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    }
    Ok(())
}

fn download(client: &Client, url: &str, destination: &Path) -> Result<(String, u64)> {
    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("github-unreachable: cannot reach {url}"))?;
    if !response.status().is_success() {
        bail!(
            "github-archive-failed: {} answered {}",
            url,
            response.status()
        );
    }
    let mut file =
        File::create(destination).with_context(|| format!("create {}", destination.display()))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = response
            .read(&mut buffer)
            .context("github-archive-failed: the download stopped unexpectedly")?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > MAX_ARCHIVE_BYTES {
            bail!(
                "source-limit-exceeded: archive exceeds {MAX_ARCHIVE_BYTES} bytes; fetch it yourself and pin the commit"
            );
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .context("github-archive-failed: cannot write the archive")?;
    }
    file.flush()
        .context("github-archive-failed: cannot flush")?;
    Ok((format!("sha256:{:x}", hasher.finalize()), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_url_has_no_ref_and_no_subpath() {
        let source = GitHubSource::parse("https://github.com/ato-run/ato").unwrap();
        assert_eq!(source.owner, "ato-run");
        assert_eq!(source.repository, "ato");
        assert_eq!(source.reference, None);
        assert_eq!(source.subpath, "");
    }

    #[test]
    fn a_tree_url_carries_the_ref_and_an_optional_subdirectory() {
        let source =
            GitHubSource::parse("https://github.com/ato-run/ato/tree/main/docs/rfcs").unwrap();
        assert_eq!(source.reference.as_deref(), Some("main"));
        assert_eq!(source.subpath, "docs/rfcs");
    }

    #[test]
    fn a_commit_sha_is_accepted_as_a_ref() {
        let source = GitHubSource::parse(
            "https://github.com/ato-run/ato/tree/0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        assert!(is_commit_sha(source.reference.as_deref().unwrap()));
    }

    #[test]
    fn non_https_and_non_github_urls_are_refused() {
        for url in [
            "http://github.com/ato-run/ato",
            "https://gitlab.com/ato-run/ato",
            "https://github.com.evil.example/ato-run/ato",
            "https://github.com/ato-run",
        ] {
            assert!(GitHubSource::parse(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn traversal_and_unknown_paths_are_refused() {
        for url in [
            "https://github.com/ato-run/ato/tree/main/../etc",
            "https://github.com/ato-run/ato/blob/main/README.md",
            "https://github.com/ato-run/ato/tree/",
        ] {
            assert!(GitHubSource::parse(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn a_ref_with_a_slash_is_percent_encoded_for_the_api() {
        assert_eq!(percent_encode_ref("feature/x"), "feature%2Fx");
        assert_eq!(percent_encode_ref("v1.0.0"), "v1.0.0");
    }
}
