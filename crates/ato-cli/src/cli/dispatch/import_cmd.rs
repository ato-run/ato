use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::cli::ImportArgs;

const GITHUB_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "ato-cli-source-import";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NormalizedGitHubInput {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) source_url_normalized: String,
}

#[derive(Debug, Serialize)]
struct ImportSource {
    source_url_normalized: String,
    revision_id: String,
    source_tree_hash: String,
    subdir: String,
}

#[derive(Debug, Serialize)]
struct ImportRecipe {
    toml: String,
    recipe_hash: String,
    origin: String,
}

#[derive(Debug, Serialize)]
struct ImportRun {
    status: String,
    phase: Option<String>,
    error_class: Option<String>,
    error_excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
struct ImportOutput {
    source: ImportSource,
    recipe: ImportRecipe,
    run: ImportRun,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubCommitResponse {
    sha: String,
    commit: GitHubCommitInner,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubCommitInner {
    tree: GitHubTreeRef,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubTreeRef {
    sha: String,
}

pub(super) fn execute_import_command(args: ImportArgs) -> Result<()> {
    let input = normalize_github_import_input(&args.repo)?;
    let source = resolve_github_source(&input)?;
    let (recipe_toml, origin) = match args.recipe.as_ref() {
        Some(path) => (
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read recipe {}", path.display()))?,
            "manual".to_string(),
        ),
        None => (infer_minimal_recipe(&input.repo), "inference".to_string()),
    };

    let output = ImportOutput {
        source,
        recipe: ImportRecipe {
            recipe_hash: blake3_label(recipe_toml.as_bytes()),
            toml: recipe_toml,
            origin,
        },
        run: if args.run {
            ImportRun {
                status: "failed".to_string(),
                phase: Some("run".to_string()),
                error_class: Some("run_execution_not_wired".to_string()),
                error_excerpt: Some(
                    "`ato import --run` currently resolves source and recipe metadata; execution will be wired through the source runtime session next."
                        .to_string(),
                ),
            }
        } else {
            ImportRun {
                status: "not_run".to_string(),
                phase: None,
                error_class: None,
                error_excerpt: None,
            }
        },
    };

    if args.emit_json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "Resolved {}\ncommit: {}\ntree: {}\nrecipe: {}",
            output.source.source_url_normalized,
            output.source.revision_id,
            output.source.source_tree_hash,
            output.recipe.recipe_hash,
        );
    }
    Ok(())
}

pub(crate) fn normalize_github_import_input(input: &str) -> Result<NormalizedGitHubInput> {
    let trimmed = input.trim();
    if trimmed.starts_with("capsule://") {
        bail!("capsule:// imports are not supported yet; pass a GitHub repository");
    }

    if is_owner_repo(trimmed) {
        let (owner, repo) = split_owner_repo(trimmed)?;
        return Ok(normalized(owner, repo));
    }

    let as_url = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&as_url).context("invalid GitHub repository URL")?;
    match url.host_str().map(str::to_ascii_lowercase).as_deref() {
        Some("github.com") | Some("www.github.com") => {}
        _ => bail!("only github.com repositories are supported"),
    }
    let parts = url
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    if parts.len() < 2 {
        bail!("GitHub repository must include owner and repo");
    }
    Ok(normalized(parts[0], parts[1]))
}

fn is_owner_repo(input: &str) -> bool {
    let parts = input.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(is_github_path_char))
}

fn is_github_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn split_owner_repo(input: &str) -> Result<(&str, &str)> {
    let mut parts = input.split('/');
    let owner = parts.next().context("missing GitHub owner")?;
    let repo = parts.next().context("missing GitHub repo")?;
    Ok((owner, repo))
}

fn normalized(owner: &str, repo_raw: &str) -> NormalizedGitHubInput {
    let repo = repo_raw.trim_end_matches(".git");
    let owner = owner.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    NormalizedGitHubInput {
        source_url_normalized: format!("https://github.com/{owner}/{repo}"),
        owner,
        repo,
    }
}

fn resolve_github_source(input: &NormalizedGitHubInput) -> Result<ImportSource> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("failed to create GitHub HTTP client")?;
    let repo_url = format!("{GITHUB_API_BASE}/repos/{}/{}", input.owner, input.repo);
    let repo = client
        .get(repo_url)
        .send()
        .context("failed to resolve GitHub repository")?
        .error_for_status()
        .context("GitHub repository lookup failed")?
        .json::<GitHubRepoResponse>()
        .context("failed to decode GitHub repository response")?;

    let commit_url = format!(
        "{GITHUB_API_BASE}/repos/{}/{}/commits/{}",
        input.owner, input.repo, repo.default_branch
    );
    let commit = client
        .get(commit_url)
        .send()
        .context("failed to resolve GitHub commit")?
        .error_for_status()
        .context("GitHub commit lookup failed")?
        .json::<GitHubCommitResponse>()
        .context("failed to decode GitHub commit response")?;

    Ok(ImportSource {
        source_url_normalized: input.source_url_normalized.clone(),
        revision_id: commit.sha,
        source_tree_hash: commit.commit.tree.sha,
        subdir: ".".to_string(),
    })
}

fn infer_minimal_recipe(repo: &str) -> String {
    format!(
        "schema_version = \"0.3\"\nname = \"{repo}\"\n\n[targets.app]\nruntime = \"source\"\nentrypoint = \".\"\n"
    )
}

fn blake3_label(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_github_inputs() {
        for input in [
            "blinkospace/blinko",
            "github.com/blinkospace/blinko",
            "https://github.com/blinkospace/blinko",
        ] {
            let normalized = normalize_github_import_input(input).expect("normalized");
            assert_eq!(normalized.owner, "blinkospace");
            assert_eq!(normalized.repo, "blinko");
            assert_eq!(
                normalized.source_url_normalized,
                "https://github.com/blinkospace/blinko"
            );
        }
    }

    #[test]
    fn rejects_capsule_scheme_for_now() {
        let error = normalize_github_import_input("capsule://store/foo/bar")
            .expect_err("capsule scheme rejected");
        assert!(error.to_string().contains("capsule:// imports"));
    }
}
