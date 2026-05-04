use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use capsule_core::common::paths::nacelle_home_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UninstallOptions {
    pub purge: bool,
    pub include_config: bool,
    pub include_keys: bool,
    pub dry_run: bool,
    pub yes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemovalTarget {
    path: PathBuf,
    plan_label: String,
    summary_label: String,
    exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemovalPlan {
    remove_targets: Vec<RemovalTarget>,
    preserved_targets: Vec<String>,
    ato_home: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct ExecutionOutcome {
    removed_summaries: Vec<String>,
    failed_paths: Vec<String>,
    removed_anything: bool,
    removed_ato_home_root: bool,
}

/// Remove files installed by `curl ato.run/install.sh | sh`.
///
/// Homebrew-installed copies are left alone — Homebrew owns the file metadata it
/// tracks, and deleting under it makes `brew doctor` unhappy. We detect that
/// case and redirect the user.
pub fn uninstall(options: UninstallOptions) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("could not resolve the current ato executable path")?;

    if let Some(reason) = detect_workspace_build(&current_exe) {
        bail!(
            "ato uninstall only supports installed binaries; current executable looks like a workspace build ({reason})"
        );
    }

    if let Some(reason) = detect_homebrew_install(&current_exe) {
        eprintln!("ℹ️  Detected Homebrew install at {}", current_exe.display());
        eprintln!("    Reason: {reason}");
        eprintln!();
        eprintln!("    Run this instead so Homebrew can clean up its receipts:");
        eprintln!();
        eprintln!("        brew uninstall ato-cli");
        eprintln!("        # If you also installed the legacy Cask:");
        eprintln!("        brew uninstall --cask ato 2>/dev/null || true");
        bail!("ato uninstall refused — Homebrew is the canonical owner");
    }

    let install_dir = current_exe
        .parent()
        .map(Path::to_path_buf)
        .context("could not derive install directory from the running ato binary")?;
    let plan = build_removal_plan(&install_dir, options);

    if options.include_keys {
        eprintln!("⚠️  WARNING: --include-keys will permanently remove ~/.ato/keys.");
        eprintln!("    You may lose signing keys, trust anchors, and encrypted credential access.");
        eprintln!();
    }

    if options.dry_run {
        print_dry_run(&plan);
        return Ok(());
    }

    if options.purge && !options.yes {
        print_confirmation_preview(&plan);
        if !confirm(confirm_prompt(options))? {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    let mut outcome = execute_plan(&plan)?;
    if remove_ato_home_if_empty(plan.ato_home.as_deref())? {
        outcome.removed_ato_home_root = true;
    }

    print_summary(&plan, &outcome, &install_dir, options);
    Ok(())
}

fn build_removal_plan(install_dir: &Path, options: UninstallOptions) -> RemovalPlan {
    let ato_home = nacelle_home_dir().ok();
    let home = dirs::home_dir();
    let mut remove_targets = Vec::new();
    let mut preserved_targets = Vec::new();

    push_target(
        &mut remove_targets,
        install_dir.join("ato"),
        "binaries and shims",
        &home,
    );
    push_target(
        &mut remove_targets,
        install_dir.join("nacelle"),
        "binaries and shims",
        &home,
    );

    #[cfg(target_os = "macos")]
    {
        push_target(
            &mut remove_targets,
            PathBuf::from("/Applications/Ato Desktop.app"),
            "desktop bundle",
            &home,
        );
        if let Some(home_dir) = &home {
            push_target(
                &mut remove_targets,
                home_dir.join("Applications").join("Ato Desktop.app"),
                "desktop bundle",
                &home,
            );
        }
    }

    #[cfg(target_os = "linux")]
    if let Some(home_dir) = &home {
        push_target(
            &mut remove_targets,
            home_dir.join("Applications").join("Ato-Desktop.AppImage"),
            "desktop bundle",
            &home,
        );
    }

    if let Some(home_dir) = &home {
        for completion_path in shell_completion_targets(home_dir) {
            push_target(
                &mut remove_targets,
                completion_path,
                "shell completions",
                &home,
            );
        }

        for launcher_path in desktop_integration_targets(home_dir) {
            push_target(
                &mut remove_targets,
                launcher_path,
                "desktop integration / launchers",
                &home,
            );
        }
    }

    if let Some(ato_home) = &ato_home {
        if options.purge {
            push_target(
                &mut remove_targets,
                ato_home.join("store"),
                &display_path(&ato_home.join("store"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("runtimes"),
                &display_path(&ato_home.join("runtimes"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("run"),
                &display_path(&ato_home.join("run"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("runs"),
                &display_path(&ato_home.join("runs"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("logs"),
                &display_path(&ato_home.join("logs"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join(".tmp"),
                &display_path(&ato_home.join(".tmp"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("tmp"),
                &display_path(&ato_home.join("tmp"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("cache"),
                &display_path(&ato_home.join("cache"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("executions"),
                &display_path(&ato_home.join("executions"), &home),
                &home,
            );
            push_target(
                &mut remove_targets,
                ato_home.join("desktop"),
                &display_path(&ato_home.join("desktop"), &home),
                &home,
            );

            collect_app_session_targets(&mut remove_targets, ato_home, &home);
            collect_ephemeral_root_files(&mut remove_targets, ato_home, &home);

            #[cfg(target_os = "macos")]
            if let Some(home_dir) = &home {
                push_target(
                    &mut remove_targets,
                    home_dir.join("Library/Application Support/Ato"),
                    "desktop integration / launchers",
                    &home,
                );
                push_target(
                    &mut remove_targets,
                    home_dir.join("Library/Caches/run.ato.desktop"),
                    "desktop integration / launchers",
                    &home,
                );
                push_target(
                    &mut remove_targets,
                    home_dir.join("Library/Logs/run.ato.desktop"),
                    "desktop integration / launchers",
                    &home,
                );
                push_target(
                    &mut remove_targets,
                    home_dir.join("Library/Preferences/run.ato.desktop.plist"),
                    "desktop integration / launchers",
                    &home,
                );
            }
        }

        if options.purge && !options.include_config {
            preserved_targets.push(display_path(&ato_home.join("config.toml"), &home));
        } else if options.include_config {
            push_target(
                &mut remove_targets,
                ato_home.join("config.toml"),
                &display_path(&ato_home.join("config.toml"), &home),
                &home,
            );
        }

        if options.purge && !options.include_keys {
            preserved_targets.push(display_path(&ato_home.join("keys"), &home));
        } else if options.include_keys {
            push_target(
                &mut remove_targets,
                ato_home.join("keys"),
                &display_path(&ato_home.join("keys"), &home),
                &home,
            );
        }
    }

    RemovalPlan {
        remove_targets,
        preserved_targets,
        ato_home,
    }
}

fn push_target(
    targets: &mut Vec<RemovalTarget>,
    path: PathBuf,
    summary_label: &str,
    home: &Option<PathBuf>,
) {
    targets.push(RemovalTarget {
        exists: path.exists() || path.symlink_metadata().is_ok(),
        plan_label: display_path(&path, home),
        summary_label: summary_label.to_string(),
        path,
    });
}

fn collect_app_session_targets(
    targets: &mut Vec<RemovalTarget>,
    ato_home: &Path,
    home: &Option<PathBuf>,
) {
    let apps_dir = ato_home.join("apps");
    let Ok(entries) = fs::read_dir(&apps_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let app_root = entry.path();
        let sessions = app_root.join("sessions");
        let plan_label = display_path(&sessions, home);
        targets.push(RemovalTarget {
            exists: sessions.exists() || sessions.symlink_metadata().is_ok(),
            path: sessions,
            plan_label,
            summary_label: "~/.ato/apps/*/sessions".to_string(),
        });
    }
}

fn collect_ephemeral_root_files(
    targets: &mut Vec<RemovalTarget>,
    ato_home: &Path,
    home: &Option<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(ato_home) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let is_ephemeral = name.ends_with(".lock")
            || name.ends_with(".pid")
            || name.ends_with(".sock")
            || name == "lock"
            || name == "pid"
            || name == "socket";
        if !is_ephemeral {
            continue;
        }
        targets.push(RemovalTarget {
            exists: true,
            plan_label: display_path(&path, home),
            summary_label: "cache / lock / pid / socket files".to_string(),
            path,
        });
    }
}

fn shell_completion_targets(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/bash-completion/completions/ato"),
        home.join(".zsh/completions/_ato"),
        home.join(".local/share/zsh/site-functions/_ato"),
        home.join(".config/fish/completions/ato.fish"),
    ]
}

fn desktop_integration_targets(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/applications/ato.desktop"),
        home.join(".local/share/applications/ato-desktop.desktop"),
        home.join(".local/share/icons/hicolor/512x512/apps/ato.png"),
        home.join(".local/share/icons/hicolor/512x512/apps/ato-desktop.png"),
    ]
}

fn display_path(path: &Path, home: &Option<PathBuf>) -> String {
    if let Some(home_dir) = home {
        if let Ok(relative) = path.strip_prefix(home_dir) {
            if relative.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn print_dry_run(plan: &RemovalPlan) {
    println!("Dry run — ato uninstall");
    println!();
    println!("Would remove:");
    if plan.remove_targets.is_empty() {
        println!("- nothing");
    } else {
        for target in &plan.remove_targets {
            let marker = if target.exists { "remove" } else { "skip  " };
            println!("- [{marker}] {}", target.plan_label);
        }
    }

    if !plan.preserved_targets.is_empty() {
        println!();
        println!("Would preserve:");
        for preserved in &plan.preserved_targets {
            println!("- {preserved}");
        }
    }
}

fn print_confirmation_preview(plan: &RemovalPlan) {
    println!("ato uninstall --purge will remove:");
    for summary in summarize_existing_targets(&plan.remove_targets) {
        println!("- {summary}");
    }
    if !plan.preserved_targets.is_empty() {
        println!();
        println!("Preserved:");
        for preserved in &plan.preserved_targets {
            println!("- {preserved}");
        }
    }
    println!();
}

fn execute_plan(plan: &RemovalPlan) -> Result<ExecutionOutcome> {
    let mut outcome = ExecutionOutcome::default();

    for target in &plan.remove_targets {
        if !target.exists {
            continue;
        }

        match remove_path(&target.path) {
            Ok(()) => {
                outcome.removed_anything = true;
                push_unique(&mut outcome.removed_summaries, target.summary_label.clone());
            }
            Err(err) => {
                outcome
                    .failed_paths
                    .push(format!("{}: {}", target.plan_label, err));
            }
        }
    }

    Ok(outcome)
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn remove_ato_home_if_empty(ato_home: Option<&Path>) -> Result<bool> {
    let Some(ato_home) = ato_home else {
        return Ok(false);
    };
    if !(ato_home.exists() || ato_home.symlink_metadata().is_ok()) {
        return Ok(false);
    }
    if !ato_home.is_dir() || ato_home.is_symlink() {
        return Ok(false);
    }
    let mut entries = fs::read_dir(ato_home)
        .with_context(|| format!("failed to inspect {}", ato_home.display()))?;
    if entries.next().is_some() {
        return Ok(false);
    }
    fs::remove_dir(ato_home).with_context(|| format!("failed to remove {}", ato_home.display()))?;
    Ok(true)
}

fn summarize_existing_targets(targets: &[RemovalTarget]) -> Vec<String> {
    let mut summaries = Vec::new();
    for target in targets {
        if target.exists {
            push_unique(&mut summaries, target.summary_label.clone());
        }
    }
    summaries
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn print_summary(
    plan: &RemovalPlan,
    outcome: &ExecutionOutcome,
    install_dir: &Path,
    options: UninstallOptions,
) {
    println!();

    if outcome.removed_anything || outcome.removed_ato_home_root {
        println!("Removed:");
        for summary in &outcome.removed_summaries {
            println!("- {summary}");
        }
        if outcome.removed_ato_home_root {
            println!("- ~/.ato");
        }
    } else if outcome.failed_paths.is_empty() {
        println!("No matching installed files were found.");
    }

    if options.purge && !plan.preserved_targets.is_empty() {
        println!();
        println!("Preserved:");
        for preserved in &plan.preserved_targets {
            println!("- {preserved}");
        }
    }

    if options.purge && !plan.preserved_targets.is_empty() {
        println!();
        println!("Use --include-config and/or --include-keys for full removal.");
    }

    if !outcome.failed_paths.is_empty() {
        println!();
        println!("Failed:");
        for failure in &outcome.failed_paths {
            println!("- {failure}");
        }
    }

    println!();
    println!(
        "Remove {} from your PATH if you want a fully clean shell environment.",
        install_dir.display()
    );
}

fn confirm_prompt(options: UninstallOptions) -> &'static str {
    if options.include_keys {
        "Proceed? This will permanently delete ~/.ato/keys [y/N] "
    } else {
        "Proceed with purge? [y/N] "
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{self, BufRead, Write};

    let mut stdout = io::stdout().lock();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Returns Some(reason) if `path` lives under a directory that
/// Homebrew typically owns. Reason is a short human string used in
/// the redirect message.
fn detect_homebrew_install(path: &Path) -> Option<&'static str> {
    let p = path.to_string_lossy();
    if p.starts_with("/opt/homebrew/") || p.contains("/opt/homebrew/Cellar/") {
        Some("path is under /opt/homebrew/ (Apple Silicon brew prefix)")
    } else if p.starts_with("/usr/local/Cellar/") || p.starts_with("/usr/local/opt/") {
        Some("path is under /usr/local/Cellar/ or /usr/local/opt/ (Intel brew prefix)")
    } else if p.starts_with("/home/linuxbrew/") || p.contains("/.linuxbrew/") {
        Some("path is under linuxbrew prefix")
    } else {
        None
    }
}

fn detect_workspace_build(path: &Path) -> Option<String> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)?;
    let target_root = workspace_root.join("target");
    if path.starts_with(&target_root) {
        Some(format!("path is under {}", target_root.display()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_removal_plan, detect_homebrew_install, detect_workspace_build,
        remove_ato_home_if_empty, UninstallOptions,
    };
    use serial_test::serial;
    use std::fs;
    use std::path::Path;

    fn options() -> UninstallOptions {
        UninstallOptions {
            purge: false,
            include_config: false,
            include_keys: false,
            dry_run: false,
            yes: false,
        }
    }

    #[test]
    fn detects_apple_silicon_brew_prefix() {
        assert!(detect_homebrew_install(Path::new("/opt/homebrew/bin/ato")).is_some());
    }

    #[test]
    fn detects_intel_brew_cellar() {
        assert!(
            detect_homebrew_install(Path::new("/usr/local/Cellar/ato-cli/0.4.87/bin/ato"))
                .is_some()
        );
    }

    #[test]
    fn ignores_install_sh_default_path() {
        assert!(detect_homebrew_install(Path::new("/Users/me/.local/bin/ato")).is_none());
    }

    #[test]
    fn ignores_dev_target_path() {
        assert!(detect_homebrew_install(Path::new("/Users/me/repo/target/release/ato")).is_none());
    }

    #[test]
    fn detects_workspace_target_builds() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let debug_bin = workspace_root.join("target/debug/ato");
        assert!(detect_workspace_build(&debug_bin).is_some());
    }

    #[test]
    fn default_plan_only_targets_installed_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).expect("create install dir");

        let plan = build_removal_plan(&install_dir, options());

        let planned = plan
            .remove_targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        assert!(planned.contains(&install_dir.join("ato")));
        assert!(planned.contains(&install_dir.join("nacelle")));
        assert!(
            !planned.iter().any(|path| path.ends_with("store")),
            "default uninstall must not purge ~/.ato data"
        );
    }

    #[test]
    #[serial]
    fn purge_plan_preserves_config_and_keys_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).expect("create install dir");
        let home_dir = temp.path().join("home");
        let ato_home = home_dir.join(".ato");
        fs::create_dir_all(ato_home.join("apps/ato-desktop/sessions")).expect("create sessions");
        fs::write(ato_home.join("config.toml"), "theme = \"dark\"").expect("write config");
        fs::create_dir_all(ato_home.join("keys")).expect("create keys");
        fs::create_dir_all(ato_home.join("store")).expect("create store");

        let old_home = std::env::var_os("HOME");
        let old_ato_home = std::env::var_os("ATO_HOME");
        std::env::set_var("HOME", &home_dir);
        std::env::set_var("ATO_HOME", &ato_home);

        let mut purge_options = options();
        purge_options.purge = true;
        let plan = build_removal_plan(&install_dir, purge_options);

        if let Some(old_home) = old_home {
            std::env::set_var("HOME", old_home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(old_ato_home) = old_ato_home {
            std::env::set_var("ATO_HOME", old_ato_home);
        } else {
            std::env::remove_var("ATO_HOME");
        }

        let planned = plan
            .remove_targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        assert!(planned.contains(&ato_home.join("store")));
        assert!(planned.contains(&ato_home.join("apps/ato-desktop/sessions")));
        assert!(
            plan.preserved_targets
                .contains(&"~/.ato/config.toml".to_string()),
            "config.toml must be preserved unless explicitly included"
        );
        assert!(
            plan.preserved_targets.contains(&"~/.ato/keys".to_string()),
            "keys must be preserved unless explicitly included"
        );
    }

    #[test]
    fn remove_empty_ato_home_deletes_now_empty_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join(".ato");
        fs::create_dir_all(&ato_home).expect("create ato home");
        assert!(remove_ato_home_if_empty(Some(&ato_home)).expect("remove empty root"));
        assert!(!ato_home.exists());
    }

    #[test]
    fn remove_empty_ato_home_keeps_non_empty_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join(".ato");
        fs::create_dir_all(&ato_home).expect("create ato home");
        fs::write(ato_home.join("config.toml"), "theme = \"dark\"").expect("write config");
        assert!(!remove_ato_home_if_empty(Some(&ato_home)).expect("preserve non-empty root"));
        assert!(ato_home.exists());
    }
}
