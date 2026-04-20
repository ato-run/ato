use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::CapsuleReporter;

#[cfg(test)]
pub(crate) use crate::application::pipeline::hourglass::HourglassPhase as RunPhaseBoundary;
use crate::application::ports::OutputPort;
use crate::application::share;
use crate::install::support::{enforce_sandbox_mode_flags, execute_run_command};
#[cfg(test)]
pub(crate) use crate::install::support::{LocalRunManifestPreparationOutcome, ResolvedRunTarget};
use crate::progressive_ui;
use crate::reporters;
use crate::{
    CompatibilityFallbackBackend, EnforcementMode, GitHubAutoFixMode, ProviderToolchain,
    RunAgentMode,
};

pub(crate) struct RunLikeCommandArgs {
    pub(crate) path: PathBuf,
    pub(crate) target: Option<String>,
    pub(crate) entry: Option<String>,
    pub(crate) env_file: Option<PathBuf>,
    pub(crate) prompt_env: bool,
    pub(crate) args: Vec<String>,
    pub(crate) watch: bool,
    pub(crate) background: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) registry: Option<String>,
    pub(crate) state: Vec<String>,
    pub(crate) inject: Vec<String>,
    pub(crate) enforcement: EnforcementMode,
    pub(crate) sandbox_mode: bool,
    pub(crate) unsafe_mode_legacy: bool,
    pub(crate) unsafe_bypass_sandbox_legacy: bool,
    pub(crate) dangerously_skip_permissions: bool,
    pub(crate) compatibility_fallback: Option<CompatibilityFallbackBackend>,
    pub(crate) provider_toolchain: ProviderToolchain,
    pub(crate) yes: bool,
    pub(crate) verbose: bool,
    pub(crate) agent_mode: RunAgentMode,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) auto_fix_mode: Option<GitHubAutoFixMode>,
    pub(crate) allow_unverified: bool,
    pub(crate) read: Vec<String>,
    pub(crate) write: Vec<String>,
    pub(crate) read_write: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) deprecation_warning: Option<&'static str>,
    pub(crate) reporter: Arc<reporters::CliReporter>,
}

pub(crate) fn execute_run_like_command(args: RunLikeCommandArgs) -> Result<()> {
    let raw_target = args.path.to_string_lossy();
    if raw_target.trim_start().starts_with("capsule://") {
        bail!(
            "`ato run` does not accept canonical capsule handles in phase 1. Re-run with a terse ref such as `ato run github.com/owner/repo` or `ato run publisher/slug`."
        );
    }

    if let Some(warning) = args.deprecation_warning {
        eprintln!("{warning}");
    }

    let sandbox_requested =
        args.sandbox_mode || args.unsafe_mode_legacy || args.unsafe_bypass_sandbox_legacy;
    let effective_enforcement = enforce_sandbox_mode_flags(
        args.enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback,
        args.reporter.clone(),
    )?;
    if share::looks_like_share_run_input(raw_target.as_ref())
        || looks_like_local_share_artifact(raw_target.as_ref())
    {
        return share::execute_run_share(share::RunShareArgs {
            input: raw_target.into_owned(),
            entry: args.entry,
            args: args.args,
            env_file: args.env_file,
            prompt_env: args.prompt_env,
            watch: args.watch,
            background: args.background,
            reporter: args.reporter,
            compat_host: matches!(
                args.compatibility_fallback,
                Some(CompatibilityFallbackBackend::Host)
            ),
        });
    }

    execute_standard_run_with_env_assistance(args, effective_enforcement, sandbox_requested)
}

fn execute_standard_run_with_env_assistance(
    args: RunLikeCommandArgs,
    effective_enforcement: EnforcementMode,
    sandbox_requested: bool,
) -> Result<()> {
    let raw_target = args.path.to_string_lossy().to_string();
    let mut injected_env = ScopedEnv::default();
    let saved_env_path =
        saved_target_env_path(&fingerprint_target(&raw_target, args.target.as_deref()))?;

    if saved_env_path.exists() {
        injected_env.apply_map(load_env_file(&saved_env_path)?);
    }
    if let Some(env_file) = args.env_file.as_deref() {
        injected_env.apply_map(load_env_file(env_file)?);
    }

    let run_once = |args: &RunLikeCommandArgs, effective_enforcement, sandbox_requested| {
        execute_run_command(
            args.path.clone(),
            args.target.clone(),
            args.args.clone(),
            args.watch,
            args.background,
            args.nacelle.clone(),
            args.registry.clone(),
            effective_enforcement,
            sandbox_requested,
            args.dangerously_skip_permissions,
            args.compatibility_fallback
                .map(CompatibilityFallbackBackend::as_str)
                .map(str::to_string),
            args.provider_toolchain,
            args.yes,
            resolve_run_verbose(args.verbose),
            args.agent_mode,
            None,
            args.keep_failed_artifacts,
            args.auto_fix_mode,
            args.allow_unverified,
            args.read.clone(),
            args.write.clone(),
            args.read_write.clone(),
            args.cwd.clone(),
            args.state.clone(),
            args.inject.clone(),
            args.reporter.clone(),
        )
    };

    match run_once(&args, effective_enforcement, sandbox_requested) {
        Ok(()) => {
            report_next_step(&args, &raw_target)?;
            Ok(())
        }
        Err(error) => {
            let Some(missing_keys) = missing_required_env_keys(&error) else {
                return Err(error);
            };

            if args.env_file.is_some() {
                return Err(error.context(format!(
                    "Required environment is still missing after loading --env-file: {}",
                    missing_keys.join(", ")
                )));
            }

            if crate::application::secrets::is_ci_environment() {
                return Err(error.context(format!(
                    "CI environment detected — provide secrets via --env-file or process env. Missing: {}",
                    missing_keys.join(", ")
                )));
            }

            // Phase 2: Try the global SecretStore before falling back to interactive prompt.
            let missing_keys = {
                if let Ok(store) = crate::application::secrets::SecretStore::open() {
                    let mut loaded = Vec::new();
                    for key in &missing_keys {
                        if let Ok(Some(value)) = store.load(key, None) {
                            loaded.push((key.clone(), value));
                        }
                    }
                    if !loaded.is_empty() {
                        injected_env.apply_map(loaded);
                        match run_once(&args, effective_enforcement, sandbox_requested) {
                            Ok(()) => {
                                report_next_step(&args, &raw_target)?;
                                return Ok(());
                            }
                            Err(retry_err) => {
                                if let Some(still_missing) = missing_required_env_keys(&retry_err) {
                                    still_missing
                                } else {
                                    return Err(retry_err);
                                }
                            }
                        }
                    } else {
                        missing_keys
                    }
                } else {
                    missing_keys
                }
            };

            if !io_available_for_prompt() {
                return Err(error.context(format!(
                    "Provide the required environment with --env-file and rerun. Missing: {}",
                    missing_keys.join(", ")
                )));
            }

            if !args.prompt_env {
                futures::executor::block_on(args.reporter.warn(format!(
                    "Missing required environment for this run: {}",
                    missing_keys.join(", ")
                )))?;
                if !progressive_ui::confirm_with_fallback(
                    "Enter values now for this run? [Y/n] ",
                    true,
                    progressive_ui::can_use_progressive_ui(args.reporter.is_json()),
                )? {
                    return Err(error);
                }
            }

            let prompted = prompt_for_missing_env(&missing_keys)?;
            injected_env.apply_map(prompted.clone());
            if progressive_ui::confirm_with_fallback(
                "Save these values for this target? [y/N] ",
                false,
                progressive_ui::can_use_progressive_ui(args.reporter.is_json()),
            )? {
                persist_env_file(&saved_env_path, &prompted)?;
            }

            run_once(&args, effective_enforcement, sandbox_requested)?;
            report_next_step(&args, &raw_target)?;
            Ok(())
        }
    }
}

fn report_next_step(args: &RunLikeCommandArgs, raw_target: &str) -> Result<()> {
    let expanded_local = crate::local_input::expand_local_path(raw_target);
    let message = if crate::local_input::should_treat_input_as_local(raw_target, &expanded_local)
        && expanded_local.is_dir()
    {
        Some("Share it next: ato encap --share".to_string())
    } else if looks_like_remote_try_target(raw_target) {
        Some(format!(
            "Set it up locally next: ato decap {} --into ./{}",
            raw_target,
            suggested_into_dir(raw_target)
        ))
    } else {
        None
    };

    if let Some(message) = message {
        futures::executor::block_on(args.reporter.notify(message))?;
    }
    Ok(())
}

fn looks_like_local_share_artifact(raw_target: &str) -> bool {
    let path = PathBuf::from(raw_target);
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some("share.spec.json" | "share.lock.json")
    )
}

fn looks_like_remote_try_target(raw_target: &str) -> bool {
    raw_target.starts_with("github.com/")
        || raw_target.contains("/s/")
        || (raw_target.contains('/')
            && !crate::local_input::is_explicit_local_path_input(raw_target))
}

fn suggested_into_dir(raw_target: &str) -> String {
    if let Some(last) = raw_target.rsplit('/').next() {
        let trimmed = last.split("@r").next().unwrap_or(last).trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "workspace".to_string()
}

fn fingerprint_target(raw_target: &str, target: Option<&str>) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(raw_target.as_bytes());
    if let Some(target) = target {
        hasher.update(b"::");
        hasher.update(target.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn saved_target_env_path(fingerprint: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home directory for env cache")?;
    Ok(home
        .join(".ato")
        .join("env")
        .join("targets")
        .join(format!("{fingerprint}.env")))
}

fn load_env_file(path: &std::path::Path) -> Result<Vec<(String, String)>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read env file {}", path.display()))?;
    let mut pairs = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim().to_string();
        crate::common::env_security::check_user_env_safety(&key, &value)
            .with_context(|| format!("rejected env key in {}", path.display()))?;
        pairs.push((key, value));
    }
    Ok(pairs)
}

fn persist_env_file(path: &std::path::Path, envs: &[(String, String)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = envs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    crate::application::secrets::store::write_secure_file(path, format!("{rendered}\n").as_bytes())
}

fn prompt_for_missing_env(missing_keys: &[String]) -> Result<Vec<(String, String)>> {
    let mut values = Vec::new();
    for key in missing_keys {
        let value = if is_likely_secret_key(key) {
            rpassword::prompt_password(format!("{key} (hidden): "))
                .context("failed to read secret value")?
        } else {
            eprint!("{key}: ");
            std::io::stderr()
                .flush()
                .context("failed to flush env prompt")?;
            let mut value = String::new();
            std::io::stdin()
                .read_line(&mut value)
                .context("failed to read env prompt")?;
            value.trim().to_string()
        };
        crate::common::env_security::check_user_env_safety(key, &value)?;
        values.push((key.clone(), value));
    }
    Ok(values)
}

fn is_likely_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("PWD")
        || upper.contains("PRIVATE")
        || upper.contains("CREDENTIAL")
        || upper.contains("API_KEY")
}

fn io_available_for_prompt() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn missing_required_env_keys(error: &anyhow::Error) -> Option<Vec<String>> {
    let execution_error = error.downcast_ref::<AtoExecutionError>()?;
    if execution_error.name != "missing_required_env" {
        return None;
    }
    execution_error
        .details
        .as_ref()
        .and_then(|details| details.get("missing_keys"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
}

#[derive(Default)]
struct ScopedEnv {
    previous: Vec<(String, Option<String>)>,
}

impl ScopedEnv {
    fn apply_map(&mut self, envs: Vec<(String, String)>) {
        for (key, value) in envs {
            if self.previous.iter().all(|(existing, _)| existing != &key) {
                self.previous.push((key.clone(), std::env::var(&key).ok()));
            }
            std::env::set_var(key, value);
        }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..).rev() {
            if let Some(previous) = previous {
                std::env::set_var(key, previous);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

fn resolve_run_verbose(explicit_verbose: bool) -> bool {
    explicit_verbose || ato_log_requests_verbose()
}

fn ato_log_requests_verbose() -> bool {
    std::env::var("ATO_LOG")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "info" | "debug" | "trace"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use capsule_core::ato_lock::{self, AtoLock};
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};

    use super::{
        ato_log_requests_verbose, resolve_run_verbose, LocalRunManifestPreparationOutcome,
        ResolvedRunTarget, RunPhaseBoundary,
    };
    use crate::install::support::LocalRunManifestStatus;
    use std::sync::Arc;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn run_phase_boundaries_follow_hourglass_order() {
        assert!(RunPhaseBoundary::Install < RunPhaseBoundary::Prepare);
        assert!(RunPhaseBoundary::Prepare < RunPhaseBoundary::Build);
        assert!(RunPhaseBoundary::Build < RunPhaseBoundary::Verify);
        assert!(RunPhaseBoundary::Verify < RunPhaseBoundary::DryRun);
        assert!(RunPhaseBoundary::DryRun < RunPhaseBoundary::Execute);
        assert_eq!(RunPhaseBoundary::DryRun.as_str(), "dry_run");
    }

    #[test]
    fn local_directory_is_agent_eligible() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            crate::install::support::agent_local_root_for_path(tmp.path()),
            Some(tmp.path().to_path_buf())
        );
    }

    #[test]
    fn missing_manifest_is_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status =
            crate::install::support::inspect_local_run_manifest(&tmp.path().join("capsule.toml"))
                .expect("inspect");
        assert!(matches!(status, LocalRunManifestStatus::Missing));
    }

    #[test]
    fn invalid_manifest_is_backed_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(&manifest_path, "not = [valid").expect("manifest");

        let status =
            crate::install::support::inspect_local_run_manifest(&manifest_path).expect("inspect");
        assert!(matches!(status, LocalRunManifestStatus::Invalid { .. }));

        let backup_path =
            crate::install::support::backup_invalid_manifest(&manifest_path).expect("backup");
        assert!(!manifest_path.exists());
        assert!(backup_path.exists());
        assert_eq!(
            backup_path.parent().expect("parent"),
            tmp.path().join(".ato/tmp/run-invalid-manifests")
        );
        assert!(backup_path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name")
            .starts_with("capsule.toml.invalid."));
    }

    #[test]
    fn canonical_capsule_handles_are_rejected_by_run_surface() {
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));
        let error = super::execute_run_like_command(super::RunLikeCommandArgs {
            path: PathBuf::from("capsule://github.com/acme/chat"),
            target: None,
            entry: None,
            env_file: None,
            prompt_env: false,
            args: Vec::new(),
            watch: false,
            background: false,
            nacelle: None,
            registry: None,
            state: Vec::new(),
            inject: Vec::new(),
            enforcement: crate::EnforcementMode::Strict,
            sandbox_mode: false,
            unsafe_mode_legacy: false,
            unsafe_bypass_sandbox_legacy: false,
            dangerously_skip_permissions: false,
            compatibility_fallback: None,
            provider_toolchain: crate::ProviderToolchain::Auto,
            yes: false,
            verbose: false,
            agent_mode: crate::RunAgentMode::Auto,
            keep_failed_artifacts: false,
            auto_fix_mode: None,
            allow_unverified: false,
            read: Vec::new(),
            write: Vec::new(),
            read_write: Vec::new(),
            cwd: None,
            deprecation_warning: None,
            reporter,
        })
        .expect_err("canonical handle should be rejected");

        assert!(error
            .to_string()
            .contains("does not accept canonical capsule handles"));
    }

    #[test]
    fn missing_manifest_is_generated_with_yes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        let resolved = ResolvedRunTarget {
            path: tmp.path().to_path_buf(),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
            transient_workspace_root: None,
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);
        assert!(!tmp.path().join("capsule.toml").exists());
        assert!(!tmp.path().join("ato.lock.json").exists());
    }

    #[test]
    fn canonical_lock_input_skips_legacy_manifest_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "node", "cmd": ["index.js"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "node", "cmd": ["index.js"]}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "node", "version": "20.11.0"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "default", "runtime": "source", "driver": "node", "entrypoint": "node", "cmd": ["index.js"], "compatible": true}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete", "observed_lockfiles": []}),
        );
        ato_lock::write_pretty_to_path(&lock, &tmp.path().join("ato.lock.json")).expect("lock");
        let resolved = ResolvedRunTarget {
            path: tmp.path().join("ato.lock.json"),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
            transient_workspace_root: None,
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn ato_log_info_enables_verbose_run_output() {
        let _lock = env_lock().lock().unwrap();
        std::env::set_var("ATO_LOG", "info");

        assert!(ato_log_requests_verbose());
        assert!(resolve_run_verbose(false));

        std::env::remove_var("ATO_LOG");
    }

    #[test]
    fn explicit_verbose_overrides_silent_ato_log() {
        let _lock = env_lock().lock().unwrap();
        std::env::set_var("ATO_LOG", "warn");

        assert!(!ato_log_requests_verbose());
        assert!(resolve_run_verbose(true));

        std::env::remove_var("ATO_LOG");
    }
}
