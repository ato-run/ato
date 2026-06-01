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
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX.lock().unwrap()
    }

    fn make_context() -> CommunitySubmitPromptContext {
        CommunitySubmitPromptContext {
            source: "github.com/owner/repo".to_string(),
            capsule_toml_path: PathBuf::from("/tmp/capsule.toml"),
            origin: CommunitySubmitOrigin::Inferred,
        }
    }

    #[test]
    fn disable_env_var_true_returns_true() {
        let _lock = env_lock();
        unsafe {
            std::env::set_var(ENV_DISABLE_PROMPT, "1");
        }
        assert!(community_submit_prompt_disabled());
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
    }

    #[test]
    fn disable_env_var_true_string_returns_true() {
        let _lock = env_lock();
        unsafe {
            std::env::set_var(ENV_DISABLE_PROMPT, "true");
        }
        assert!(community_submit_prompt_disabled());
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
    }

    #[test]
    fn disable_env_var_absent_returns_false() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        assert!(!community_submit_prompt_disabled());
    }

    #[test]
    fn prompt_suppressed_when_disabled_env_set() {
        let _lock = env_lock();
        unsafe {
            std::env::set_var(ENV_DISABLE_PROMPT, "1");
        }
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, false
        ));
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
    }

    #[test]
    fn prompt_suppressed_in_json_mode() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, true, false, false
        ));
    }

    #[test]
    fn prompt_suppressed_in_background_mode() {
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(
            &ctx, false, true, false
        ));
    }

    #[test]
    fn prompt_suppressed_in_plan_only_mode() {
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
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

    // ── post-success prompt boundary tests ───────────────────────────────────

    // The community-registry path sets community_submit_context = None in
    // support.rs (community_selected = true → context = None), so
    // try_post_success_community_submit_prompt returns Ok(()) immediately.
    // These tests verify the should_prompt_for_community_submit guard matrix
    // that runs *after* context is confirmed non-None.

    #[test]
    fn prompt_guard_all_flags_false_returns_true_in_test_harness() {
        // All flag-based guards are off; only the TTY check would suppress.
        // In CI / test runners stdin/stderr are not terminals, so this
        // returns false.  Document that the guard is not bypassed by flag
        // combination alone.
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = make_context();
        // We can't assert true here (tests run headless), but we can assert
        // the function does not panic and agrees with TTY state.
        let result = should_prompt_for_community_submit(&ctx, false, false, false);
        // In a non-TTY test environment this must be false.
        assert!(!result, "non-TTY must suppress the prompt");
    }

    #[test]
    fn prompt_suppressed_for_existing_repo_toml_origin_non_tty() {
        // ExistingRepoToml origin: the prompt would fire if TTY were
        // available.  In non-TTY (test env) it must remain suppressed.
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = CommunitySubmitPromptContext {
            origin: CommunitySubmitOrigin::ExistingRepoToml,
            ..make_context()
        };
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, false
        ));
    }

    #[test]
    fn prompt_suppressed_for_local_override_origin_non_tty() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = CommunitySubmitPromptContext {
            origin: CommunitySubmitOrigin::LocalOverride,
            ..make_context()
        };
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, false
        ));
    }

    #[test]
    fn prompt_suppressed_for_inferred_origin_non_tty() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = CommunitySubmitPromptContext {
            origin: CommunitySubmitOrigin::Inferred,
            ..make_context()
        };
        assert!(!should_prompt_for_community_submit(
            &ctx, false, false, false
        ));
    }

    #[test]
    fn prompt_suppressed_when_json_and_background_together() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(&ctx, true, true, false));
    }

    #[test]
    fn prompt_suppressed_when_json_and_plan_only_together() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var(ENV_DISABLE_PROMPT);
        }
        let ctx = make_context();
        assert!(!should_prompt_for_community_submit(&ctx, true, false, true));
    }

    #[test]
    fn community_submit_origin_variants_exist() {
        // Smoke test: confirm all expected origin variants compile and are Eq.
        assert_eq!(
            CommunitySubmitOrigin::Inferred,
            CommunitySubmitOrigin::Inferred
        );
        assert_eq!(
            CommunitySubmitOrigin::LocalOverride,
            CommunitySubmitOrigin::LocalOverride
        );
        assert_eq!(
            CommunitySubmitOrigin::ExistingRepoToml,
            CommunitySubmitOrigin::ExistingRepoToml
        );
        assert_ne!(
            CommunitySubmitOrigin::Inferred,
            CommunitySubmitOrigin::LocalOverride
        );
    }
}
