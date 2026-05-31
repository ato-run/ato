use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;

const ENV_DISABLE_PROMPT: &str = "ATO_DISABLE_COMMUNITY_SUBMIT_PROMPT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommunitySubmitPromptContext {
    pub(crate) source: String,
    pub(crate) capsule_toml_path: PathBuf,
    pub(crate) origin: CommunitySubmitOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommunitySubmitOrigin {
    Inferred,
    LocalOverride,
    ExistingRepoToml,
}

pub(crate) fn community_submit_prompt_disabled() -> bool {
    match std::env::var(ENV_DISABLE_PROMPT) {
        Ok(val) => val == "1" || val.to_lowercase() == "true",
        Err(_) => false,
    }
}

pub(crate) fn should_prompt_for_community_submit(
    _context: &CommunitySubmitPromptContext,
    is_json: bool,
    is_background: bool,
    is_plan_only: bool,
) -> bool {
    if community_submit_prompt_disabled() {
        return false;
    }
    if is_json || is_background || is_plan_only {
        return false;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    true
}

pub(crate) fn confirm_community_submit_prompt(
    context: &CommunitySubmitPromptContext,
) -> std::io::Result<bool> {
    eprintln!();
    eprintln!("This capsule.toml worked locally.");
    eprintln!();
    eprintln!("  source: {}", context.source);
    eprintln!("  toml: {}", context.capsule_toml_path.display());
    eprintln!("  visibility: public");
    eprintln!("  trust: community");
    eprintln!();
    eprint!("Do you want to share this recipe to our community? [y/N] ");
    use std::io::Write;
    let _ = std::io::stderr().flush();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

pub(crate) async fn try_community_submit_after_run(
    context: &CommunitySubmitPromptContext,
) -> Result<()> {
    let toml_content = std::fs::read_to_string(&context.capsule_toml_path)
        .map_err(|e| anyhow::anyhow!("Failed to read capsule.toml for submission: {}", e))?;

    let submission = crate::community::submit::prepare_submission(
        &context.source,
        &context.capsule_toml_path,
        &toml_content,
        false,
        None,
    )?;

    let Some(submission) = submission else {
        return Ok(());
    };

    let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();

    if is_tty {
        eprintln!();
        eprintln!("Submit capsule.toml to Ato community?");
        eprintln!();
        eprintln!("  source: {}", context.source);
        eprintln!("  toml: {}", context.capsule_toml_path.display());
        eprintln!("  trust: community");
        eprintln!("  visibility: public");
        eprintln!();
        eprintln!("This will publish the capsule.toml as public execution metadata.");
        eprint!("Continue? [y/N] ");
        use std::io::Write;
        let _ = std::io::stderr().flush();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            eprintln!("Submission skipped.");
            return Ok(());
        }
    } else {
        return Ok(());
    }

    let result = crate::community::submit::submit_prepared_with_response(&submission).await?;

    eprintln!();
    eprintln!("Submitted community capsule.toml:");
    eprintln!("  id: {}", result.id);
    eprintln!("  url: {}", result.url);
    eprintln!("  status: {}", result.status);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_context() -> CommunitySubmitPromptContext {
        CommunitySubmitPromptContext {
            source: "github.com/owner/repo".to_string(),
            capsule_toml_path: PathBuf::from("/tmp/capsule.toml"),
            origin: CommunitySubmitOrigin::Inferred,
        }
    }

    #[test]
    fn disable_env_var_true_returns_true() {
        std::env::set_var(ENV_DISABLE_PROMPT, "1");
        assert!(community_submit_prompt_disabled());
        std::env::remove_var(ENV_DISABLE_PROMPT);
    }

    #[test]
    fn disable_env_var_true_string_returns_true() {
        std::env::set_var(ENV_DISABLE_PROMPT, "true");
        assert!(community_submit_prompt_disabled());
        std::env::remove_var(ENV_DISABLE_PROMPT);
    }

    #[test]
    fn disable_env_var_absent_returns_false() {
        std::env::remove_var(ENV_DISABLE_PROMPT);
        assert!(!community_submit_prompt_disabled());
    }

    #[test]
    fn prompt_suppressed_when_disabled_env_set() {
        std::env::set_var(ENV_DISABLE_PROMPT, "1");
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, false
        ));
        std::env::remove_var(ENV_DISABLE_PROMPT);
    }

    #[test]
    fn prompt_suppressed_in_json_mode() {
        std::env::remove_var(ENV_DISABLE_PROMPT);
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, true, false, false
        ));
    }

    #[test]
    fn prompt_suppressed_in_background_mode() {
        std::env::remove_var(ENV_DISABLE_PROMPT);
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, false, true, false
        ));
    }

    #[test]
    fn prompt_suppressed_in_plan_only_mode() {
        std::env::remove_var(ENV_DISABLE_PROMPT);
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, true
        ));
    }

    #[test]
    fn prompt_context_eq_works() {
        let a = CommunitySubmitPromptContext {
            source: "github.com/a/b".to_string(),
            capsule_toml_path: PathBuf::from("/tmp/a.toml"),
            origin: CommunitySubmitOrigin::Inferred,
        };
        let b = CommunitySubmitPromptContext {
            source: "github.com/a/b".to_string(),
            capsule_toml_path: PathBuf::from("/tmp/a.toml"),
            origin: CommunitySubmitOrigin::Inferred,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn prompt_context_not_eq_different_source() {
        let a = make_context();
        let b = CommunitySubmitPromptContext {
            source: "github.com/other/repo".to_string(),
            ..make_context()
        };
        assert_ne!(a, b);
    }

    #[test]
    fn prompt_context_not_eq_different_origin() {
        let a = make_context();
        let b = CommunitySubmitPromptContext {
            origin: CommunitySubmitOrigin::ExistingRepoToml,
            ..make_context()
        };
        assert_ne!(a, b);
    }
}
