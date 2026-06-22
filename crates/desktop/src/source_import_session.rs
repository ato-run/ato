use anyhow::{Context, Result, bail};
use capsule::capsule::manifest::blake3_digest;
use serde::{Deserialize, Serialize};
use url::Url;

// ---------------------------------------------------------------------------
// CLI JSON mirror types
//
// These mirror the structs in crates/cli/src/cli/dispatch/import_cmd.rs.
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportRun {
    pub(crate) status: String,
    pub(crate) phase: Option<String>,
    pub(crate) error_class: Option<String>,
    pub(crate) error_excerpt: Option<String>,
    #[serde(default)]
    pub(crate) command_mode: Option<String>,
    #[serde(default)]
    pub(crate) requires_host_shell: Option<bool>,
    #[serde(default)]
    pub(crate) shell_kind: Option<String>,
    #[serde(default)]
    pub(crate) cleanup_status: Option<String>,
    #[serde(default)]
    pub(crate) cleanup_error: Option<String>,
    #[serde(default)]
    pub(crate) log_path: Option<String>,
    #[serde(default)]
    pub(crate) run_session_id: Option<String>,
    #[serde(default)]
    pub(crate) pid: Option<i32>,
    #[serde(default)]
    pub(crate) process_group_ids: Vec<i32>,
    #[serde(default)]
    pub(crate) primary_port: Option<u16>,
    #[serde(default)]
    pub(crate) primary_url: Option<String>,
    #[serde(default)]
    pub(crate) shadow_dir: Option<String>,
    #[serde(default)]
    pub(crate) readiness_state: Option<String>,
    #[serde(default)]
    pub(crate) cleanup_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImportOutput {
    pub(crate) source: ImportSource,
    pub(crate) recipe: ImportRecipe,
    pub(crate) run: ImportRun,
    #[serde(default)]
    pub(crate) recipe_resolution: Option<RecipeResolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RecipeResolution {
    pub(crate) source: String,
    #[serde(default)]
    pub(crate) fallback: Option<String>,
    #[serde(default)]
    pub(crate) error_class: Option<String>,
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
    InferenceFailed,
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
    /// True when `ato desktop-auth-handoff` succeeded for this session.
    /// Drives whether the source-imports API calls run and how the UI
    /// labels the Submit button (real action vs. "Sign in to submit").
    signed_in: bool,
    /// Source-import row id returned by the first
    /// `POST /v1/source-imports` call. Required for subsequent
    /// `/attempt` and `/submit-working-recipe` calls.
    source_import_id: Option<String>,
    /// When inference itself fails (CLI non-zero exit or parse error),
    /// these fields carry the error details for the UI.
    inference_error_class: Option<String>,
    inference_error_excerpt: Option<String>,
    /// When submit-working-recipe fails, this holds the error excerpt
    /// so the UI can surface it without losing the Verified state.
    submit_error_excerpt: Option<String>,
    /// Set to true after the user explicitly confirms they want to
    /// allow unsafe execution (e.g. source/native runtime). Required
    /// before `ato import --run` may proceed with source/native.
    unsafe_execution_confirmed: bool,
    /// Metadata about how the recipe was resolved (remote binding, inference, or fallback).
    recipe_resolution: Option<RecipeResolution>,
    /// Hash of the recipe TOML as first obtained from inference (whether
    /// the remote verified binding lookup or local inference). Captured in
    /// `apply_inferred_output` and used by `apply_run_result` to tell
    /// whether the post-run CLI output is the verbatim recipe or a
    /// user-edited variant. Without this, the runner's `--recipe <tmp.toml>`
    /// round-trip flips `recipe.origin` to "manual" even when the bytes are
    /// unchanged, destroying the verified-remote provenance.
    base_recipe_hash: Option<String>,
    /// Origin tag captured at inference time ("registry" / "inference").
    /// Restored onto `recipe.origin` after a same-hash run.
    base_recipe_origin: Option<String>,
    /// `RecipeResolution` captured at inference time. Preserved across a
    /// same-hash run for the same reason as `base_recipe_origin`.
    base_recipe_resolution: Option<RecipeResolution>,
    /// True when `edit_recipe` has been called with a TOML whose blake3
    /// hash differs from `base_recipe_hash`. The UI uses this to surface
    /// "Edited locally" alongside the base provenance.
    editable_recipe_dirty: bool,
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
            signed_in: false,
            source_import_id: None,
            inference_error_class: None,
            inference_error_excerpt: None,
            submit_error_excerpt: None,
            unsafe_execution_confirmed: false,
            recipe_resolution: None,
            base_recipe_hash: None,
            base_recipe_origin: None,
            base_recipe_resolution: None,
            editable_recipe_dirty: false,
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
    ///
    /// Captures the recipe's hash, origin, and resolution as the *base*
    /// provenance so that `apply_run_result` can later distinguish "user
    /// ran the verbatim recipe" from "user edited it before running".
    pub(crate) fn apply_inferred_output(&mut self, output: ImportOutput) -> Result<()> {
        if output.run.status != "not_run" {
            bail!(
                "apply_inferred_output expects run.status = \"not_run\", got {:?}",
                output.run.status
            );
        }
        self.editable_recipe_toml = Some(output.recipe.recipe_toml.clone());
        self.base_recipe_hash = Some(output.recipe.recipe_hash.clone());
        self.base_recipe_origin = Some(output.recipe.origin.clone());
        self.base_recipe_resolution = output.recipe_resolution.clone();
        self.editable_recipe_dirty = false;
        self.recipe_resolution = output.recipe_resolution;
        self.source = Some(output.source);
        self.recipe = Some(output.recipe);
        self.last_run = Some(output.run);
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::AwaitingTomlConfirmation;
        Ok(())
    }

    /// Replace the textarea TOML with user-edited content.
    ///
    /// Side-effect: re-computes `editable_recipe_dirty` against
    /// `base_recipe_hash`. Pure-whitespace or no-op edits that hash back to
    /// the verified-base recipe leave the dirty flag false, so the next Run
    /// still preserves the original `registry` provenance.
    pub(crate) fn edit_recipe(&mut self, toml: String) -> Result<()> {
        match self.state {
            GitHubImportSessionState::AwaitingTomlConfirmation
            | GitHubImportSessionState::FailedAwaitingRecipeEdit => {
                let new_hash = blake3_digest(toml.as_bytes());
                self.editable_recipe_dirty = match self.base_recipe_hash.as_deref() {
                    Some(base) => base != new_hash,
                    None => true,
                };
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
    ///
    /// Recipe provenance:
    /// - If the post-run `recipe.recipe_hash` equals `base_recipe_hash` (the
    ///   hash captured at inference time), the verified-base origin and
    ///   resolution are restored. Without this, the runner's
    ///   `--recipe <tmp.toml>` round-trip would flip `recipe.origin` to
    ///   "manual" even for the verbatim verified recipe.
    /// - Otherwise the recipe is treated as edited locally: origin becomes
    ///   `"edited_local"` and `recipe_resolution` is replaced with a
    ///   `RecipeResolution { source: "edited_local", .. }` marker so the
    ///   UI can show "Edited locally — based on: <base>".
    pub(crate) fn apply_run_result(&mut self, output: ImportOutput) -> Result<()> {
        let ImportOutput {
            source,
            mut recipe,
            run,
            recipe_resolution: cli_resolution,
        } = output;

        let hash_matches_base = self
            .base_recipe_hash
            .as_deref()
            .map(|base| base == recipe.recipe_hash)
            .unwrap_or(false);

        if hash_matches_base {
            if let Some(base_origin) = self.base_recipe_origin.clone() {
                recipe.origin = base_origin;
            }
            if let Some(base_resolution) = self.base_recipe_resolution.clone() {
                self.recipe_resolution = Some(base_resolution);
            } else if let Some(cli_resolution) = cli_resolution {
                self.recipe_resolution = Some(cli_resolution);
            }
            self.editable_recipe_dirty = false;
        } else {
            recipe.origin = "edited_local".to_string();
            self.recipe_resolution = Some(RecipeResolution {
                source: "edited_local".to_string(),
                fallback: None,
                error_class: None,
            });
            self.editable_recipe_dirty = true;
        }

        match run.status.as_str() {
            "passed" => {
                self.source = Some(source);
                self.recipe = Some(recipe);
                self.last_run = Some(run);
                self.submit_enabled = true;
                self.state = GitHubImportSessionState::Verified;
                Ok(())
            }
            "running" if run.readiness_state.as_deref() == Some("ready") => {
                self.source = Some(source);
                self.recipe = Some(recipe);
                self.last_run = Some(run);
                self.submit_enabled = true;
                self.state = GitHubImportSessionState::Verified;
                Ok(())
            }
            "failed" => {
                self.source = Some(source);
                self.recipe = Some(recipe);
                self.last_run = Some(run);
                self.submit_enabled = false;
                self.state = GitHubImportSessionState::FailedAwaitingRecipeEdit;
                Ok(())
            }
            other => bail!(
                "apply_run_result expects run.status passed|failed, got {:?}",
                other.to_string()
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
            base_recipe_hash: self.base_recipe_hash.clone(),
            base_recipe_resolution: self.base_recipe_resolution.clone(),
            edited_locally: self.editable_recipe_dirty,
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
            signed_in: self.signed_in,
            source_import_id: self.source_import_id.clone(),
            inference_error_class: self.inference_error_class.clone(),
            inference_error_excerpt: self.inference_error_excerpt.clone(),
            submit_error_excerpt: self.submit_error_excerpt.clone(),
            unsafe_execution_confirmed: self.unsafe_execution_confirmed,
            recipe_resolution: self.recipe_resolution.clone(),
            base_recipe_hash: self.base_recipe_hash.clone(),
            base_recipe_origin: self.base_recipe_origin.clone(),
            base_recipe_resolution: self.base_recipe_resolution.clone(),
            edited_locally: self.editable_recipe_dirty,
        }
    }

    pub(crate) fn state(&self) -> GitHubImportSessionState {
        self.state
    }

    pub(crate) fn submit_enabled(&self) -> bool {
        self.submit_enabled
    }

    pub(crate) fn active_run_session_id(&self) -> Option<&str> {
        self.last_run.as_ref()?.run_session_id.as_deref()
    }

    pub(crate) fn editable_recipe_toml(&self) -> Option<&str> {
        self.editable_recipe_toml.as_deref()
    }

    pub(crate) fn repo(&self) -> Option<&NormalizedGitHubRepo> {
        self.repo.as_ref()
    }

    /// Record whether the user is currently signed in to ato. The
    /// dispatch layer calls this once per session after the
    /// `ato desktop-auth-handoff` discovery completes.
    pub(crate) fn set_signed_in(&mut self, signed_in: bool) {
        self.signed_in = signed_in;
    }

    pub(crate) fn signed_in(&self) -> bool {
        self.signed_in
    }

    /// Record the source-import id returned by
    /// `POST /v1/source-imports`. Subsequent /attempt and
    /// /submit-working-recipe calls require this id.
    pub(crate) fn set_source_import_id(&mut self, id: String) {
        self.source_import_id = Some(id);
    }

    pub(crate) fn source_import_id(&self) -> Option<&str> {
        self.source_import_id.as_deref()
    }

    pub(crate) fn inference_error_class(&self) -> Option<&str> {
        self.inference_error_class.as_deref()
    }

    pub(crate) fn inference_error_excerpt(&self) -> Option<&str> {
        self.inference_error_excerpt.as_deref()
    }

    /// Record that inference (CLI `ato import --emit-json`) failed.
    /// Transitions from `InferringRecipe` → `InferenceFailed`.
    pub(crate) fn record_inference_failure(
        &mut self,
        error_class: String,
        error_excerpt: String,
    ) -> Result<()> {
        if self.state != GitHubImportSessionState::InferringRecipe {
            bail!(
                "record_inference_failure expects InferringRecipe, got {:?}",
                self.state
            );
        }
        self.inference_error_class = Some(error_class);
        self.inference_error_excerpt = Some(error_excerpt);
        self.submit_enabled = false;
        self.state = GitHubImportSessionState::InferenceFailed;
        Ok(())
    }

    /// Retry inference after it previously failed. Transitions from
    /// `InferenceFailed` → `InferringRecipe`.
    pub(crate) fn retry_inference(&mut self) -> Result<()> {
        if self.state != GitHubImportSessionState::InferenceFailed {
            bail!(
                "retry_inference expects InferenceFailed, got {:?}",
                self.state
            );
        }
        self.inference_error_class = None;
        self.inference_error_excerpt = None;
        self.state = GitHubImportSessionState::InferringRecipe;
        Ok(())
    }

    pub(crate) fn submit_error_excerpt(&self) -> Option<&str> {
        self.submit_error_excerpt.as_deref()
    }

    pub(crate) fn set_submit_error(&mut self, excerpt: String) {
        self.submit_error_excerpt = Some(excerpt);
    }

    pub(crate) fn clear_submit_error(&mut self) {
        self.submit_error_excerpt = None;
    }

    pub(crate) fn unsafe_execution_confirmed(&self) -> bool {
        self.unsafe_execution_confirmed
    }

    pub(crate) fn confirm_unsafe_execution(&mut self) {
        self.unsafe_execution_confirmed = true;
    }

    /// Hash of the recipe TOML as first obtained from inference. `None`
    /// until `apply_inferred_output` runs.
    pub(crate) fn base_recipe_hash(&self) -> Option<&str> {
        self.base_recipe_hash.as_deref()
    }

    /// True when the editable TOML's blake3 hash diverges from
    /// `base_recipe_hash`. The Submit payload uses this to mark the row
    /// `edited_locally=true` even when the recipe coincidentally fails to
    /// run; the UI uses it to render the "Edited locally" badge.
    pub(crate) fn edited_locally(&self) -> bool {
        self.editable_recipe_dirty
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SubmitPayload {
    pub(crate) source: ImportSource,
    pub(crate) recipe: ImportRecipe,
    pub(crate) last_run: ImportRun,
    /// Recipe hash captured at inference time. When this equals
    /// `recipe.recipe_hash`, the API can short-circuit recipe insert and
    /// reuse the existing verified binding instead of creating a new
    /// `manual` recipe row.
    pub(crate) base_recipe_hash: Option<String>,
    /// Provenance of the inferred-time recipe (`remote_binding` /
    /// `inference` / `remote_binding_failed`). Sent so the API has the same
    /// context as the Desktop UI when accepting the submission.
    pub(crate) base_recipe_resolution: Option<RecipeResolution>,
    /// True when the user edited the TOML between inference and submit
    /// (the editable TOML hashes to something different from
    /// `base_recipe_hash`). The API uses this to decide whether to record
    /// a fresh manual recipe row or attach the attempt to the existing
    /// verified binding.
    pub(crate) edited_locally: bool,
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
    /// True when ato desktop-auth-handoff returned credentials for
    /// this session. The React UI uses this to decide whether the
    /// Submit button reads "Submit this working recipe" (actionable)
    /// or "Sign in to submit" (no-op until login).
    pub(crate) signed_in: bool,
    /// Source-import row id; null until the first
    /// `POST /v1/source-imports` round-trip completes.
    pub(crate) source_import_id: Option<String>,
    pub(crate) inference_error_class: Option<String>,
    pub(crate) inference_error_excerpt: Option<String>,
    pub(crate) submit_error_excerpt: Option<String>,
    pub(crate) unsafe_execution_confirmed: bool,
    pub(crate) recipe_resolution: Option<RecipeResolution>,
    /// Hash of the recipe TOML captured at inference time. Used by the
    /// Import UI to show "Based on: Verified remote recipe" after the user
    /// has edited the TOML, and by the submit payload as the provenance
    /// pointer back to the verified binding.
    pub(crate) base_recipe_hash: Option<String>,
    /// Origin tag captured at inference time ("registry" / "inference").
    pub(crate) base_recipe_origin: Option<String>,
    /// `RecipeResolution` captured at inference time.
    pub(crate) base_recipe_resolution: Option<RecipeResolution>,
    /// True when the editable TOML diverges from the inferred base.
    pub(crate) edited_locally: bool,
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
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                ..ImportRun::default()
            },
            recipe_resolution: None,
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
                error_excerpt: Some("ModuleNotFoundError: No module named 'distutils'".to_string()),
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                ..ImportRun::default()
            },
            recipe_resolution: None,
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
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                ..ImportRun::default()
            },
            recipe_resolution: None,
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
            session
                .snapshot()
                .last_run
                .as_ref()
                .and_then(|r| r.error_class.clone()),
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
    fn signed_in_and_source_import_id_round_trip_through_snapshot() {
        let mut session = GitHubImportSession::default();
        assert!(!session.signed_in());
        assert!(session.source_import_id().is_none());

        session.set_signed_in(true);
        session.set_source_import_id("si_abc123".to_string());

        let snap = session.snapshot();
        assert!(snap.signed_in);
        assert_eq!(snap.source_import_id.as_deref(), Some("si_abc123"));

        // begin_resolve resets the session, including signed_in and id.
        session.begin_resolve("blinkospace/blinko").expect("source");
        assert!(!session.signed_in());
        assert!(session.source_import_id().is_none());
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
                    command_mode: None,
                    requires_host_shell: None,
                    shell_kind: None,
                    ..ImportRun::default()
                },
                recipe_resolution: None,
            })
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session.apply_run_result(output).expect("apply");
        assert_eq!(
            session.state(),
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );
        let snap = session.snapshot();
        assert_eq!(
            snap.last_run.as_ref().unwrap().phase.as_deref(),
            Some("install")
        );
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
                    command_mode: None,
                    requires_host_shell: None,
                    shell_kind: None,
                    ..ImportRun::default()
                },
                recipe_resolution: None,
            })
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session.apply_run_result(output).expect("apply");
        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
    }

    #[test]
    fn cli_keep_alive_ready_json_drives_verified_state_and_session_id() {
        let mut output = passed_output();
        output.run.status = "running".to_string();
        output.run.phase = Some("readiness".to_string());
        output.run.run_session_id = Some("preview-owner-repo-123".to_string());
        output.run.readiness_state = Some("ready".to_string());
        output.run.cleanup_policy = Some("keep_until_explicit_stop".to_string());
        output.run.primary_url = Some("http://127.0.0.1:1111/".to_string());

        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session.apply_run_result(output).expect("apply");

        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
        assert_eq!(
            session.active_run_session_id(),
            Some("preview-owner-repo-123")
        );
    }

    #[test]
    fn inference_failure_state_and_retry() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.begin_inference();
        assert_eq!(session.state(), GitHubImportSessionState::InferringRecipe);

        session
            .record_inference_failure(
                "cli_nonzero_exit".to_string(),
                "ato import failed (status 1)".to_string(),
            )
            .expect("record inference failure");
        assert_eq!(session.state(), GitHubImportSessionState::InferenceFailed);

        let snap = session.snapshot();
        assert_eq!(
            snap.inference_error_class.as_deref(),
            Some("cli_nonzero_exit")
        );
        assert_eq!(
            snap.inference_error_excerpt.as_deref(),
            Some("ato import failed (status 1)")
        );

        session.retry_inference().expect("retry inference");
        assert_eq!(session.state(), GitHubImportSessionState::InferringRecipe);
        assert!(session.inference_error_class().is_none());
        assert!(session.inference_error_excerpt().is_none());
    }

    #[test]
    fn inference_failure_includes_errors_in_snapshot() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("owner/repo").expect("source");
        session.begin_inference();
        session
            .record_inference_failure("parse_error".to_string(), "invalid JSON".to_string())
            .expect("record");

        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::InferenceFailed);
        assert_eq!(snap.inference_error_class.as_deref(), Some("parse_error"));
        assert_eq!(
            snap.inference_error_excerpt.as_deref(),
            Some("invalid JSON")
        );
        assert!(!snap.submit_enabled);
    }

    #[test]
    fn inference_failure_is_reset_on_new_resolve() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.begin_inference();
        session
            .record_inference_failure("cli_nonzero_exit".to_string(), "failed".to_string())
            .expect("record failure");

        session.begin_resolve("other/repo").expect("new resolve");
        assert!(session.inference_error_class().is_none());
        assert!(session.inference_error_excerpt().is_none());
        assert_eq!(session.state(), GitHubImportSessionState::ResolvingSource);
    }

    #[test]
    fn record_inference_failure_rejected_outside_inferring_state() {
        let mut session = GitHubImportSession::default();
        assert!(
            session
                .record_inference_failure("x".to_string(), "y".to_string())
                .is_err()
        );
        session.begin_resolve("owner/repo").expect("resolve");
        assert!(
            session
                .record_inference_failure("x".to_string(), "y".to_string())
                .is_err()
        );
    }

    #[test]
    fn retry_inference_rejected_outside_inference_failed_state() {
        let mut session = GitHubImportSession::default();
        assert!(session.retry_inference().is_err());
        session.begin_resolve("owner/repo").expect("resolve");
        assert!(session.retry_inference().is_err());
    }

    #[test]
    fn submit_error_leaves_session_verified_and_retryable() {
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

        session.set_submit_error("Connection refused".to_string());
        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
        assert_eq!(session.submit_error_excerpt(), Some("Connection refused"));

        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::Verified);
        assert_eq!(
            snap.submit_error_excerpt.as_deref(),
            Some("Connection refused")
        );
        // User can still submit again after seeing the error.
        assert!(snap.submit_enabled);
    }

    #[test]
    fn submit_error_cleared_and_round_trips_through_snapshot() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        session.set_submit_error("timeout".to_string());
        let snap = session.snapshot();
        assert_eq!(snap.submit_error_excerpt.as_deref(), Some("timeout"));

        session.clear_submit_error();
        assert!(session.submit_error_excerpt().is_none());
        let snap = session.snapshot();
        assert!(snap.submit_error_excerpt.is_none());
    }

    #[test]
    fn submit_error_reset_on_new_resolve() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.set_submit_error("stale error".to_string());

        session.begin_resolve("other/repo").expect("new resolve");
        assert!(session.submit_error_excerpt().is_none());
    }

    #[test]
    fn submit_success_marks_submitted() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        session.mark_submitted().expect("mark submitted");
        assert_eq!(session.state(), GitHubImportSessionState::Submitted);
        assert!(!session.submit_enabled());
    }

    #[test]
    fn snapshot_includes_inference_and_submit_error_fields() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("owner/repo").expect("source");
        session.begin_inference();
        session
            .record_inference_failure("inf_error".to_string(), "inference failed".to_string())
            .expect("record");

        session.set_submit_error("sub_error".to_string());
        let snap = session.snapshot();
        assert_eq!(snap.inference_error_class.as_deref(), Some("inf_error"));
        assert_eq!(
            snap.inference_error_excerpt.as_deref(),
            Some("inference failed")
        );
        assert_eq!(snap.submit_error_excerpt.as_deref(), Some("sub_error"));
    }

    #[test]
    fn unsafe_execution_defaults_to_false_and_resets_on_new_resolve() {
        let mut session = GitHubImportSession::default();
        assert!(!session.unsafe_execution_confirmed());
        session.confirm_unsafe_execution();
        assert!(session.unsafe_execution_confirmed());

        session.begin_resolve("owner/repo").expect("resolve");
        assert!(!session.unsafe_execution_confirmed());
    }

    #[test]
    fn unsafe_execution_flag_round_trips_through_snapshot() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("owner/repo").expect("resolve");
        session.confirm_unsafe_execution();

        let snap = session.snapshot();
        assert!(snap.unsafe_execution_confirmed);

        session.begin_resolve("other/repo").expect("new resolve");
        let snap = session.snapshot();
        assert!(!snap.unsafe_execution_confirmed);
    }

    #[test]
    fn unsafe_flag_not_in_submit_payload() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        session.confirm_unsafe_execution();
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        let payload = session.submit_payload().expect("payload");
        assert_eq!(payload.last_run.status, "passed");
    }

    #[test]
    fn full_happy_path_open_to_submitted_simulates_desktop_gui_aodd() {
        let mut session = GitHubImportSession::default();
        assert_eq!(session.state(), GitHubImportSessionState::Idle);

        // Open: resolve + infer
        session
            .begin_resolve("ato-run/import-fixture-static")
            .expect("resolve");
        session.begin_inference();
        session
            .apply_inferred_output(inferred_output())
            .expect("apply inferred");
        assert_eq!(
            session.state(),
            GitHubImportSessionState::AwaitingTomlConfirmation
        );

        let snap = session.snapshot();
        assert_eq!(
            snap.state,
            GitHubImportSessionState::AwaitingTomlConfirmation
        );
        assert!(snap.editable_recipe_toml.is_some());
        assert!(!snap.submit_enabled);
        assert!(!snap.unsafe_execution_confirmed);

        // User confirms unsafe execution
        session.confirm_unsafe_execution();
        assert!(session.unsafe_execution_confirmed());

        // Run
        session.start_run().expect("run starts");
        assert_eq!(session.state(), GitHubImportSessionState::Running);
        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::Running);

        // Run result: passed
        session
            .apply_run_result(passed_output())
            .expect("apply passed");
        assert_eq!(session.state(), GitHubImportSessionState::Verified);
        assert!(session.submit_enabled());
        assert!(session.submit_payload().is_some());

        // Simulate signed in + source_import_id
        session.set_signed_in(true);
        session.set_source_import_id("si_test".to_string());
        let snap = session.snapshot();
        assert!(snap.signed_in);
        assert_eq!(snap.source_import_id.as_deref(), Some("si_test"));

        // Submit
        session.mark_submitted().expect("mark submitted");
        assert_eq!(session.state(), GitHubImportSessionState::Submitted);
        assert!(!session.submit_enabled());
        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::Submitted);
    }

    // ─── Recipe provenance preservation across Run ─────────────────────────
    //
    // The desktop runner shells out to
    //   `ato import <repo> --recipe <tmp.toml> --run --emit-json`
    // and the CLI tags `--recipe`-driven runs as origin="manual" regardless
    // of whether the user actually edited the TOML. Without provenance
    // tracking here, a verbatim run of the verified remote recipe would
    // silently downgrade origin and recipe_resolution. These tests pin the
    // behaviour described in FINDING-04 of
    // .tmp/aodd-receipt-desktop-blinko-canonical.yaml.

    fn remote_recipe_resolution() -> RecipeResolution {
        RecipeResolution {
            source: "remote_binding".to_string(),
            fallback: None,
            error_class: None,
        }
    }

    fn registry_inferred_output() -> ImportOutput {
        let mut output = inferred_output();
        output.recipe.origin = "registry".to_string();
        output.recipe_resolution = Some(remote_recipe_resolution());
        output
    }

    fn registry_passed_output_returned_by_cli() -> ImportOutput {
        // The CLI re-tags --recipe-driven runs as origin="manual" and drops
        // the resolution (the runner does not pass --no-remote-recipe; it
        // passes --recipe, which forces the local branch in import_cmd.rs).
        // recipe_hash matches the inferred recipe because the runner writes
        // the verbatim TOML to a temp file.
        let mut output = passed_output();
        output.recipe.origin = "manual".to_string();
        output.recipe_resolution = None;
        output
    }

    #[test]
    fn apply_inferred_output_captures_base_provenance_and_clears_dirty() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session.begin_inference();
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");

        let snap = session.snapshot();
        assert_eq!(snap.recipe.as_ref().unwrap().origin, "registry");
        assert_eq!(
            snap.recipe_resolution.as_ref().unwrap().source,
            "remote_binding"
        );
        assert_eq!(snap.base_recipe_hash.as_deref(), Some("blake3:recipehash"));
        assert_eq!(snap.base_recipe_origin.as_deref(), Some("registry"));
        assert_eq!(
            snap.base_recipe_resolution.as_ref().unwrap().source,
            "remote_binding"
        );
        assert!(!snap.edited_locally);
    }

    #[test]
    fn run_result_preserves_registry_origin_when_hash_matches() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");

        // Same recipe_hash as the inferred output — the CLI re-tags
        // origin to "manual" because --recipe was used, but the session
        // restores "registry" from base_recipe_origin.
        session
            .apply_run_result(registry_passed_output_returned_by_cli())
            .expect("apply passed");

        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::Verified);
        assert_eq!(snap.recipe.as_ref().unwrap().origin, "registry");
        assert_eq!(
            snap.recipe_resolution.as_ref().unwrap().source,
            "remote_binding"
        );
        assert!(!snap.edited_locally);
    }

    #[test]
    fn remote_recipe_resolution_survives_failed_run_when_hash_matches() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");

        // Failed run with the verbatim recipe (host shmget, dep failure,
        // etc.). origin and resolution must still survive — the user has
        // not touched the TOML, so a retry would still be a remote-binding
        // run.
        let mut failed = failed_output("provider_failed");
        failed.recipe.origin = "manual".to_string();
        failed.recipe_resolution = None;
        session.apply_run_result(failed).expect("apply failed");

        let snap = session.snapshot();
        assert_eq!(
            snap.state,
            GitHubImportSessionState::FailedAwaitingRecipeEdit
        );
        assert_eq!(snap.recipe.as_ref().unwrap().origin, "registry");
        assert_eq!(
            snap.recipe_resolution.as_ref().unwrap().source,
            "remote_binding"
        );
        assert!(!snap.edited_locally);
    }

    #[test]
    fn run_result_marks_edited_local_when_recipe_hash_changes() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");

        // User edits the TOML. The dirty flag flips because the new TOML
        // hashes to a different value than the base.
        session
            .edit_recipe("schema_version = \"0.3\"\n# edited\n".to_string())
            .expect("edit");
        assert!(session.edited_locally());

        session.start_run().expect("run starts");

        // CLI emits the edited TOML's hash, which won't match the base.
        // The session swaps origin to "edited_local" and overrides the
        // resolution.
        let mut edited = passed_output();
        edited.recipe.origin = "manual".to_string();
        edited.recipe.recipe_hash = "blake3:edited-hash".to_string();
        edited.recipe_resolution = None;
        session.apply_run_result(edited).expect("apply edited");

        let snap = session.snapshot();
        assert_eq!(snap.state, GitHubImportSessionState::Verified);
        assert_eq!(snap.recipe.as_ref().unwrap().origin, "edited_local");
        assert_eq!(
            snap.recipe_resolution.as_ref().unwrap().source,
            "edited_local"
        );
        assert!(snap.edited_locally);
    }

    #[test]
    fn edit_recipe_does_not_mark_dirty_when_hash_unchanged() {
        // Build an inferred output whose recipe_hash actually matches the
        // blake3 of its recipe_toml — `sample_recipe` uses a fixed
        // placeholder hash, which is fine for the other tests but breaks
        // any test that round-trips through `edit_recipe`'s hash check.
        let base_toml = "schema_version = \"0.3\"\nname = \"blinko\"\n".to_string();
        let base_hash = blake3_digest(base_toml.as_bytes());
        let inferred = ImportOutput {
            source: sample_source(),
            recipe: ImportRecipe {
                origin: "registry".to_string(),
                target_label: Some("web".to_string()),
                platform_os: "darwin".to_string(),
                platform_arch: "arm64".to_string(),
                recipe_toml: base_toml.clone(),
                recipe_hash: base_hash.clone(),
            },
            run: ImportRun {
                status: "not_run".to_string(),
                phase: None,
                error_class: None,
                error_excerpt: None,
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                ..ImportRun::default()
            },
            recipe_resolution: Some(remote_recipe_resolution()),
        };

        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred)
            .expect("apply inferred");
        assert_eq!(session.base_recipe_hash(), Some(base_hash.as_str()));
        assert!(!session.edited_locally());

        // "Edit" to the exact same bytes — this happens when the UI fires
        // an edit on blur even though the user did not change anything.
        // The base hash must match and the dirty flag must stay false.
        session.edit_recipe(base_toml.clone()).expect("edit");
        assert!(!session.edited_locally());

        // Now actually edit.
        let edited = format!("{base_toml}# real edit\n");
        session.edit_recipe(edited).expect("edit");
        assert!(session.edited_locally());

        // Revert back to base content — dirty flag should clear.
        session.edit_recipe(base_toml).expect("edit");
        assert!(!session.edited_locally());
    }

    #[test]
    fn submit_payload_preserves_verified_binding_context_for_same_hash() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(registry_passed_output_returned_by_cli())
            .expect("apply passed");

        let payload = session.submit_payload().expect("payload available");
        assert_eq!(payload.recipe.origin, "registry");
        assert_eq!(payload.recipe.recipe_hash, "blake3:recipehash");
        assert_eq!(
            payload.base_recipe_hash.as_deref(),
            Some("blake3:recipehash"),
            "verified base hash must be in payload so API can reuse the binding"
        );
        assert_eq!(
            payload
                .base_recipe_resolution
                .as_ref()
                .map(|r| r.source.as_str()),
            Some("remote_binding"),
        );
        assert!(
            !payload.edited_locally,
            "verbatim verified recipe must not be marked edited"
        );
    }

    #[test]
    fn submit_payload_marks_edited_locally_when_user_changed_toml() {
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");
        session
            .edit_recipe("schema_version = \"0.3\"\n# edited\n".to_string())
            .expect("edit");
        session.start_run().expect("run starts");

        let mut edited = passed_output();
        edited.recipe.origin = "manual".to_string();
        edited.recipe.recipe_hash = "blake3:edited-hash".to_string();
        session.apply_run_result(edited).expect("apply edited");

        let payload = session.submit_payload().expect("payload available");
        assert_eq!(payload.recipe.origin, "edited_local");
        assert_eq!(payload.recipe.recipe_hash, "blake3:edited-hash");
        assert_eq!(
            payload.base_recipe_hash.as_deref(),
            Some("blake3:recipehash"),
            "base hash still points back to the original verified recipe",
        );
        assert!(
            payload.edited_locally,
            "user-edited recipe must be marked so API records it as a new manual row",
        );
        assert_eq!(
            payload
                .base_recipe_resolution
                .as_ref()
                .map(|r| r.source.as_str()),
            Some("remote_binding"),
            "API still gets the original provenance for the audit trail",
        );
    }

    #[test]
    fn snapshot_json_exposes_provenance_keys_to_import_ui() {
        // The ato-import HTML reads these keys verbatim:
        //   snapshot.recipe_resolution.{source,fallback,error_class}
        //   snapshot.base_recipe_hash
        //   snapshot.base_recipe_origin
        //   snapshot.base_recipe_resolution.source
        //   snapshot.edited_locally
        // Pin them here so a serde rename or struct rearrangement breaks
        // this test instead of silently breaking the UI.
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(registry_inferred_output())
            .expect("apply inferred");

        let snap = session.snapshot();
        let json = serde_json::to_value(&snap).expect("snapshot serializes");

        assert_eq!(
            json["recipe_resolution"]["source"].as_str(),
            Some("remote_binding"),
        );
        assert_eq!(json["base_recipe_hash"].as_str(), Some("blake3:recipehash"),);
        assert_eq!(json["base_recipe_origin"].as_str(), Some("registry"));
        assert_eq!(
            json["base_recipe_resolution"]["source"].as_str(),
            Some("remote_binding"),
        );
        assert_eq!(json["edited_locally"].as_bool(), Some(false));
    }

    #[test]
    fn submit_payload_carries_no_base_context_when_inference_was_local() {
        // Local inference (origin="inference", recipe_resolution=None)
        // should still produce a payload — just without the verified-base
        // pointers.
        let mut session = GitHubImportSession::default();
        session.begin_resolve("blinkospace/blinko").expect("source");
        session
            .apply_inferred_output(inferred_output()) // origin="inference", no resolution
            .expect("apply inferred");
        session.start_run().expect("run starts");
        session
            .apply_run_result(passed_output())
            .expect("apply passed");

        let payload = session.submit_payload().expect("payload available");
        assert_eq!(payload.recipe.origin, "inference");
        assert_eq!(
            payload.base_recipe_hash.as_deref(),
            Some("blake3:recipehash")
        );
        assert!(
            payload.base_recipe_resolution.is_none(),
            "no resolution emitted by CLI → no base resolution in payload",
        );
        assert!(!payload.edited_locally);
    }

    #[test]
    fn edit_recipe_keeps_dirty_true_when_base_hash_absent() {
        let mut session = GitHubImportSession::default();
        // No inference has been applied yet; base_recipe_hash is None.
        // Force the session into an editable state by faking a snapshot
        // through the public API: begin_resolve + begin_inference +
        // record_inference_failure → InferenceFailed (still not editable).
        // For this test, we go through apply_inferred_output (which sets
        // base hash) and then null it out by hand-rolling the state. We
        // just want to verify edit_recipe handles the None branch.
        session.begin_resolve("owner/repo").expect("source");
        session
            .apply_inferred_output(inferred_output())
            .expect("apply");
        // Erase the base so we are testing the None branch.
        session.base_recipe_hash = None;
        session.editable_recipe_dirty = false;

        session
            .edit_recipe("schema_version = \"0.3\"\n".to_string())
            .expect("edit");
        assert!(
            session.edited_locally(),
            "edit_recipe must default dirty=true when no base recipe is known"
        );
    }
}
