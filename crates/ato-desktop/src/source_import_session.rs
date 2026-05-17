use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use url::Url;

// ---------------------------------------------------------------------------
// CLI JSON mirror types
//
// These mirror the structs in crates/ato-cli/src/cli/dispatch/import_cmd.rs.
// We do not depend on the ato-cli crate directly because Desktop spawns the
// CLI as a subprocess. Keep these in sync with the CLI output shape.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportSource {
    pub(crate) source_url_normalized: String,
    pub(crate) source_host: String,
    pub(crate) repo_namespace: String,
    pub(crate) repo_name: String,
    pub(crate) revision_id: String,
    pub(crate) source_tree_hash: String,
    pub(crate) subdir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportRecipe {
    pub(crate) origin: String,
    pub(crate) target_label: Option<String>,
    pub(crate) platform_os: String,
    pub(crate) platform_arch: String,
    pub(crate) recipe_toml: String,
    pub(crate) recipe_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportRun {
    pub(crate) status: String,
    pub(crate) phase: Option<String>,
    pub(crate) error_class: Option<String>,
    pub(crate) error_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportOutput {
    pub(crate) source: ImportSource,
    pub(crate) recipe: ImportRecipe,
    pub(crate) run: ImportRun,
}

// ---------------------------------------------------------------------------
// Normalized input (kept for the existing dock.rs caller)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NormalizedGitHubRepo {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) source_url_normalized: String,
    pub(crate) clone_url: String,
}

// ---------------------------------------------------------------------------
// Session state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
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
    repo: Option<NormalizedGitHubRepo>,
    source: Option<ImportSource>,
    recipe: Option<ImportRecipe>,
    editable_recipe_toml: Option<String>,
    last_run: Option<ImportRun>,
    submit_enabled: bool,
}

impl Default for GitHubImportSession {
    fn default() -> Self {
        Self {
            state: GitHubImportSessionState::Idle,
            repo: None,
            source: None,
            recipe: None,
            editable_recipe_toml: None,
            last_run: None,
            submit_enabled: false,
        }
    }
}

impl GitHubImportSession {
    pub(crate) fn begin_resolve(&mut self, input: &str) -> Result<&NormalizedGitHubRepo> {
        let repo = normalize_github_import_input(input)?;
        *self = Self {
            state: GitHubImportSessionState::ResolvingSource,
            repo: Some(repo),
            ..Self::default()
        };
        Ok(self.repo.as_ref().expect("repo just set"))
    }

    pub(crate) fn begin_inference(&mut self) {
        self.state = GitHubImportSessionState::InferringRecipe;
        self.submit_enabled = false;
    }

    /// Apply the CLI `ato import --emit-json` output (without `--run`).
    pub(crate) fn apply_inferred_output(&mut self, output: ImportOutput) -> Result<()> {
        if output.run.status != "not_run" {
            bail!(
                "apply_inferred_output expects run.status = \"not_run\", got {:?}",
                output.run.status
            );
        }
        self.editable_recipe_toml = Some(output.recipe.recipe_toml.clone());
        self.source = Some(output.source);
        self.recipe = Some(output.recipe);
        self.last_run = Some(output.run);
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::AwaitingTomlConfirmation;
        Ok(())
    }

    /// Replace the textarea TOML with user-edited content.
    pub(crate) fn edit_recipe(&mut self, toml: String) -> Result<()> {
        match self.state {
            GitHubImportSessionState::AwaitingTomlConfirmation
            | GitHubImportSessionState::FailedAwaitingRecipeEdit => {
                self.editable_recipe_toml = Some(toml);
                Ok(())
            }
            _ => bail!("recipe is not editable in state {:?}", self.state),
        }
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

    /// Apply the CLI `ato import --run --emit-json` output.
    ///
    /// Updates `source` / `recipe` / `last_run` to reflect the latest run.
    /// `editable_recipe_toml` is preserved so the user's textarea content
    /// survives a server round-trip (the CLI may normalize whitespace).
    pub(crate) fn apply_run_result(&mut self, output: ImportOutput) -> Result<()> {
        match output.run.status.as_str() {
            "passed" => {
                self.source = Some(output.source);
                self.recipe = Some(output.recipe);
                self.last_run = Some(output.run);
                self.submit_enabled = true;
                self.state = GitHubImportSessionState::Verified;
                Ok(())
            }
            "failed" => {
                self.source = Some(output.source);
                self.recipe = Some(output.recipe);
                self.last_run = Some(output.run);
                self.submit_enabled = false;
                self.state = GitHubImportSessionState::FailedAwaitingRecipeEdit;
                Ok(())
            }
            other => bail!(
                "apply_run_result expects run.status passed|failed, got {:?}",
                other
            ),
        }
    }

    pub(crate) fn mark_submitted(&mut self) -> Result<()> {
        if !self.submit_enabled {
            bail!("working recipe is not verified");
        }
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::Submitted;
        Ok(())
    }

    pub(crate) fn submit_payload(&self) -> Option<SubmitPayload> {
        if self.state != GitHubImportSessionState::Verified {
            return None;
        }
        let source = self.source.clone()?;
        let recipe = self.recipe.clone()?;
        let last_run = self.last_run.clone()?;
        Some(SubmitPayload {
            source,
            recipe,
            last_run,
        })
    }

    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            state: self.state,
            repo: self.repo.clone(),
            source: self.source.clone(),
            recipe: self.recipe.clone(),
            editable_recipe_toml: self.editable_recipe_toml.clone(),
            last_run: self.last_run.clone(),
            submit_enabled: self.submit_enabled,
        }
    }

    pub(crate) fn state(&self) -> GitHubImportSessionState {
        self.state
    }

    pub(crate) fn submit_enabled(&self) -> bool {
        self.submit_enabled
    }

    pub(crate) fn editable_recipe_toml(&self) -> Option<&str> {
        self.editable_recipe_toml.as_deref()
    }

    pub(crate) fn repo(&self) -> Option<&NormalizedGitHubRepo> {
        self.repo.as_ref()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SubmitPayload {
    pub(crate) source: ImportSource,
    pub(crate) recipe: ImportRecipe,
    pub(crate) last_run: ImportRun,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionSnapshot {
    pub(crate) state: GitHubImportSessionState,
    pub(crate) repo: Option<NormalizedGitHubRepo>,
    pub(crate) source: Option<ImportSource>,
    pub(crate) recipe: Option<ImportRecipe>,
    pub(crate) editable_recipe_toml: Option<String>,
    pub(crate) last_run: Option<ImportRun>,
    pub(crate) submit_enabled: bool,
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

    fn sample_source() -> ImportSource {
        ImportSource {
            source_url_normalized: "https://github.com/blinkospace/blinko".to_string(),
            source_host: "github.com".to_string(),
            repo_namespace: "blinkospace".to_string(),
            repo_name: "blinko".to_string(),
            revision_id: "8bd89aabc1234567".to_string(),
            source_tree_hash: "blake3:treehash".to_string(),
            subdir: ".".to_string(),
        }
    }

    fn sample_recipe(origin: &str) -> ImportRecipe {
        ImportRecipe {
            origin: origin.to_string(),
            target_label: Some("web".to_string()),
            platform_os: "darwin".to_string(),
            platform_arch: "arm64".to_string(),
            recipe_toml: "schema_version = \"0.3\"\n".to_string(),
            recipe_hash: "blake3:recipehash".to_string(),
        }
    }

    fn inferred_output() -> ImportOutput {
        ImportOutput {
            source: sample_source(),
            recipe: sample_recipe("inference"),
            run: ImportRun {
                status: "not_run".to_string(),
                phase: None,
                error_class: None,
                error_excerpt: None,
            },
        }
    }

    fn failed_output(error_class: &str) -> ImportOutput {
        ImportOutput {
            source: sample_source(),
            recipe: sample_recipe("inference"),
            run: ImportRun {
                status: "failed".to_string(),
                phase: Some("install".to_string()),
                error_class: Some(error_class.to_string()),
                error_excerpt: Some(
                    "ModuleNotFoundError: No module named 'distutils'".to_string(),
                ),
            },
        }
    }

    fn passed_output() -> ImportOutput {
        ImportOutput {
            source: sample_source(),
            recipe: sample_recipe("inference"),
            run: ImportRun {
                status: "passed".to_string(),
                phase: None,
                error_class: None,
                error_excerpt: None,
            },
        }
    }

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
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");

        assert_eq!(
            session.state(),
            GitHubImportSessionState::AwaitingTomlConfirmation
        );
        assert!(!session.submit_enabled());
        assert_eq!(
            session.editable_recipe_toml(),
            Some("schema_version = \"0.3\"\n")
        );
    }

    #[test]
    fn failed_run_returns_to_toml_edit_state() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(failed_output("missing_required_env"))
            .expect("apply failed");

        assert_eq!(
            session.state(),
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );
        assert_eq!(
            session.snapshot().last_run.as_ref().and_then(|r| r.error_class.clone()),
            Some("missing_required_env".to_string())
        );
        session.start_run().expect("retry starts");
    }

    #[test]
    fn successful_run_enables_submit_then_submits() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
        assert!(session.submit_payload().is_some());
        session.mark_submitted().expect("submitted");
        assert_eq!(session.state(), GitHubImportSessionState::Submitted);
        assert!(session.submit_payload().is_none());
    }

    #[test]
    fn github_import_toml_edit_retry() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");

        // User edits TOML in textarea before first run.
        session
            .edit_recipe("schema_version = \"0.3\"\n# edited\n".to_string())
            .expect("edit allowed in awaiting state");
        assert_eq!(
            session.editable_recipe_toml(),
            Some("schema_version = \"0.3\"\n# edited\n")
        );

        session.start_run().expect("run starts");
        session
            .apply_run_result(failed_output("node_gyp_missing_distutils"))
            .expect("apply failed");
        assert_eq!(
            session.state(),
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );

        // Edit again after failure.
        session
            .edit_recipe("schema_version = \"0.3\"\n# retry\n".to_string())
            .expect("edit allowed in failed state");
        assert_eq!(
            session.editable_recipe_toml(),
            Some("schema_version = \"0.3\"\n# retry\n")
        );

        session.start_run().expect("retry run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");
        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
    }

    #[test]
    fn github_import_verified_enables_submit_prompt() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        let payload = session.submit_payload().expect("payload available");
        assert_eq!(payload.source.repo_name, "blinko");
        assert_eq!(payload.recipe.recipe_hash, "blake3:recipehash");
        assert_eq!(payload.last_run.status, "passed");
    }

    #[test]
    fn edit_recipe_rejected_outside_editable_states() {
        let mut session = GitHubImportSession::default();
        assert!(session.edit_recipe("anything".to_string()).is_err());
        session.begin_resolve("blinkospace/blinko").expect("source");
        // ResolvingSource — still not editable.
        assert!(session.edit_recipe("anything".to_string()).is_err());
        session.begin_inference();
        // InferringRecipe — still not editable.
        assert!(session.edit_recipe("anything".to_string()).is_err());
    }

    #[test]
    fn cli_inferred_json_drives_awaiting_toml_state() {
        let json = r#"{
            "source": {
                "source_url_normalized": "https://github.com/blinkospace/blinko",
                "source_host": "github.com",
                "repo_namespace": "blinkospace",
                "repo_name": "blinko",
                "revision_id": "8bd89aabc1234567",
                "source_tree_hash": "blake3:tree",
                "subdir": "."
            },
            "recipe": {
                "origin": "inference",
                "target_label": "web",
                "platform_os": "darwin",
                "platform_arch": "arm64",
                "recipe_toml": "schema_version = \"0.3\"\n",
                "recipe_hash": "blake3:recipe"
            },
            "run": {
                "status": "not_run",
                "phase": null,
                "error_class": null,
                "error_excerpt": null
            }
        }"#;
        let output: ImportOutput = serde_json::from_str(json).expect("parses");
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.begin_inference();
        session.apply_inferred_output(output).expect("apply");
        assert_eq!(
            session.state(),
            GitHubImportSessionState::AwaitingTomlConfirmation
        );
    }

    #[test]
    fn cli_failed_run_json_drives_failed_state() {
        let json = r#"{
            "source": {
                "source_url_normalized": "https://github.com/blinkospace/blinko",
                "source_host": "github.com",
                "repo_namespace": "blinkospace",
                "repo_name": "blinko",
                "revision_id": "8bd89a",
                "source_tree_hash": "blake3:tree",
                "subdir": "."
            },
            "recipe": {
                "origin": "inference",
                "target_label": null,
                "platform_os": "darwin",
                "platform_arch": "arm64",
                "recipe_toml": "schema_version = \"0.3\"\n",
                "recipe_hash": "blake3:recipe"
            },
            "run": {
                "status": "failed",
                "phase": "install",
                "error_class": "node_gyp_missing_distutils",
                "error_excerpt": "ModuleNotFoundError: No module named 'distutils'"
            }
        }"#;
        let output: ImportOutput = serde_json::from_str(json).expect("parses");
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(ImportOutput {
                source: output.source.clone(),
                recipe: output.recipe.clone(),
                run: ImportRun {
                    status: "not_run".to_string(),
                    phase: None,
                    error_class: None,
                    error_excerpt: None,
                },
            })
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session.apply_run_result(output).expect("apply");
        assert_eq!(
            session.state(),
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );
        let snap = session.snapshot();
        assert_eq!(snap.last_run.as_ref().unwrap().phase.as_deref(), Some("install"));
        assert_eq!(
            snap.last_run.as_ref().unwrap().error_class.as_deref(),
            Some("node_gyp_missing_distutils")
        );
    }

    #[test]
    fn cli_passed_run_json_drives_verified_state() {
        let json = r#"{
            "source": {
                "source_url_normalized": "https://github.com/blinkospace/blinko",
                "source_host": "github.com",
                "repo_namespace": "blinkospace",
                "repo_name": "blinko",
                "revision_id": "8bd89a",
                "source_tree_hash": "blake3:tree",
                "subdir": "."
            },
            "recipe": {
                "origin": "inference",
                "target_label": "web",
                "platform_os": "darwin",
                "platform_arch": "arm64",
                "recipe_toml": "schema_version = \"0.3\"\n",
                "recipe_hash": "blake3:recipe"
            },
            "run": {
                "status": "passed",
                "phase": null,
                "error_class": null,
                "error_excerpt": null
            }
        }"#;
        let output: ImportOutput = serde_json::from_str(json).expect("parses");
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(ImportOutput {
                source: output.source.clone(),
                recipe: output.recipe.clone(),
                run: ImportRun {
                    status: "not_run".to_string(),
                    phase: None,
                    error_class: None,
                    error_excerpt: None,
                },
            })
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session.apply_run_result(output).expect("apply");
        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
    }
}
