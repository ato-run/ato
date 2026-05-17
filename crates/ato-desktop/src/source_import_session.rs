use anyhow::{bail, Context, Result};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedGitHubRepo {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) source_url_normalized: String,
    pub(crate) clone_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitHubImportSessionState {
    Idle,
    ResolvingSource,
    InferringRecipe,
    AwaitingTomlConfirmation,
    Running,
    FailedAwaitingRecipeEdit,
    Verified,
    Submitted,
}

#[derive(Debug, Clone)]
pub(crate) struct GitHubImportSession {
    state: GitHubImportSessionState,
    source: Option<NormalizedGitHubRepo>,
    recipe_toml: Option<String>,
    last_error_class: Option<String>,
    submit_enabled: bool,
}

impl Default for GitHubImportSession {
    fn default() -> Self {
        Self {
            state: GitHubImportSessionState::Idle,
            source: None,
            recipe_toml: None,
            last_error_class: None,
            submit_enabled: false,
        }
    }
}

impl GitHubImportSession {
    pub(crate) fn begin_resolve(&mut self, input: &str) -> Result<&NormalizedGitHubRepo> {
        let source = normalize_github_import_input(input)?;
        self.state = GitHubImportSessionState::ResolvingSource;
        self.source = Some(source);
        self.recipe_toml = None;
        self.last_error_class = None;
        self.submit_enabled = false;
        Ok(self.source.as_ref().expect("source just set"))
    }

    pub(crate) fn begin_inference(&mut self) {
        self.state = GitHubImportSessionState::InferringRecipe;
        self.submit_enabled = false;
    }

    pub(crate) fn set_inferred_recipe(&mut self, recipe_toml: String) {
        self.recipe_toml = Some(recipe_toml);
        self.last_error_class = None;
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::AwaitingTomlConfirmation;
    }

    pub(crate) fn start_run(&mut self) -> Result<()> {
        match self.state {
            GitHubImportSessionState::AwaitingTomlConfirmation
            | GitHubImportSessionState::FailedAwaitingRecipeEdit => {
                self.state = GitHubImportSessionState::Running;
                self.submit_enabled = false;
                Ok(())
            }
            _ => bail!("import session is not ready to run"),
        }
    }

    pub(crate) fn record_failed_run(&mut self, error_class: impl Into<String>) {
        self.last_error_class = Some(error_class.into());
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::FailedAwaitingRecipeEdit;
    }

    pub(crate) fn record_verified_run(&mut self) {
        self.last_error_class = None;
        self.submit_enabled = true;
        self.state = GitHubImportSessionState::Verified;
    }

    pub(crate) fn mark_submitted(&mut self) -> Result<()> {
        if !self.submit_enabled {
            bail!("working recipe is not verified");
        }
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::Submitted;
        Ok(())
    }

    pub(crate) fn state(&self) -> GitHubImportSessionState {
        self.state
    }

    pub(crate) fn submit_enabled(&self) -> bool {
        self.submit_enabled
    }
}

pub(crate) fn normalize_github_import_input(input: &str) -> Result<NormalizedGitHubRepo> {
    let trimmed = input.trim();
    if trimmed.starts_with("capsule://") {
        bail!("capsule:// imports are not supported in GitHub import sessions yet");
    }

    if is_owner_repo(trimmed) {
        let (owner, repo) = split_owner_repo(trimmed)?;
        return Ok(normalized(owner, repo));
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&candidate).with_context(|| {
        "Enter github.com/owner/repo, https://github.com/owner/repo, or owner/repo".to_string()
    })?;
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if url.scheme() != "https" || !matches!(host.as_str(), "github.com" | "www.github.com") {
        bail!("Only https://github.com/<owner>/<repo> sources are supported");
    }

    let segments: Vec<_> = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_else(Vec::new);
    if segments.len() != 2 {
        bail!("Use a repository root like github.com/owner/repo");
    }
    Ok(normalized(segments[0], segments[1]))
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

fn normalized(owner: &str, repo_raw: &str) -> NormalizedGitHubRepo {
    let repo = repo_raw.trim_end_matches(".git");
    let owner = owner.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let source_url_normalized = format!("https://github.com/{owner}/{repo}");
    let clone_url = format!("{source_url_normalized}.git");
    NormalizedGitHubRepo {
        owner,
        repo,
        source_url_normalized,
        clone_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_github_repo_inputs() {
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
            assert_eq!(
                normalized.clone_url,
                "https://github.com/blinkospace/blinko.git"
            );
        }
    }

    #[test]
    fn rejects_capsule_scheme_for_now() {
        assert!(normalize_github_import_input("capsule://github.com/owner/repo").is_err());
    }

    #[test]
    fn inferred_recipe_confirmation_state_appears_before_run() {
        let mut session = GitHubImportSession::default();
        session
            .begin_resolve("github.com/blinkospace/blinko")
            .expect("source");
        session.begin_inference();
        session.set_inferred_recipe("schema_version = \"0.3\"".to_string());

        assert_eq!(
            session.state(),
            GitHubImportSessionState::AwaitingTomlConfirmation
        );
        assert!(!session.submit_enabled());
    }

    #[test]
    fn failed_run_returns_to_toml_edit_state() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.set_inferred_recipe("schema_version = \"0.3\"".to_string());
        session.start_run().expect("run starts");
        session.record_failed_run("missing_required_env");

        assert_eq!(
            session.state(),
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );
        session.start_run().expect("retry starts");
    }

    #[test]
    fn successful_run_enables_submit_then_submits() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.set_inferred_recipe("schema_version = \"0.3\"".to_string());
        session.start_run().expect("run starts");
        session.record_verified_run();

        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
        session.mark_submitted().expect("submitted");
        assert_eq!(session.state(), GitHubImportSessionState::Submitted);
    }
}
