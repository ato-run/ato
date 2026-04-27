use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Remove files installed by `curl ato.run/install.sh | sh`.
///
/// Homebrew-installed copies are left alone — Homebrew owns the file
/// metadata it tracks, and deleting under it makes `brew doctor`
/// unhappy. We detect that case and redirect the user.
pub fn uninstall(keep_data: bool, yes: bool) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("could not resolve the current ato executable path")?;

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

    let plan = build_removal_plan(&install_dir, keep_data);

    print_plan(&plan, &current_exe);

    if !yes && !confirm("Proceed with uninstall? [y/N] ")? {
        eprintln!("Aborted.");
        return Ok(());
    }

    for path in &plan.paths {
        remove_path(path);
    }

    println!();
    println!("✅ ato uninstalled.");
    println!(
        "   To finish: remove {} from your PATH (in .zshrc/.bashrc).",
        install_dir.display()
    );
    Ok(())
}

struct RemovalPlan {
    paths: Vec<PathBuf>,
}

fn build_removal_plan(install_dir: &Path, keep_data: bool) -> RemovalPlan {
    let mut paths = vec![install_dir.join("ato"), install_dir.join("nacelle")];

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Applications/Ato Desktop.app"));
    }

    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "linux")]
        paths.push(home.join("Applications/Ato-Desktop.AppImage"));

        if !keep_data {
            paths.push(home.join(".ato").join("desktop"));

            #[cfg(target_os = "macos")]
            {
                paths.push(home.join("Library/Application Support/Ato"));
                paths.push(home.join("Library/Caches/run.ato.desktop"));
                paths.push(home.join("Library/Logs/run.ato.desktop"));
                paths.push(home.join("Library/Preferences/run.ato.desktop.plist"));
            }
        }
    }

    RemovalPlan { paths }
}

fn print_plan(plan: &RemovalPlan, current_exe: &Path) {
    println!("ato uninstall — removal plan:");
    println!();
    for path in &plan.paths {
        let exists = path.exists() || path.symlink_metadata().is_ok();
        let marker = if exists { "  remove" } else { "  skip  " };
        let self_marker = if path == current_exe { " (self)" } else { "" };
        println!("{marker} {}{}", path.display(), self_marker);
    }
    println!();
}

fn remove_path(path: &Path) {
    let exists = path.exists() || path.symlink_metadata().is_ok();
    if !exists {
        return;
    }
    let result = if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => println!("removed {}", path.display()),
        Err(err) => eprintln!("warning: failed to remove {}: {}", path.display(), err),
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

#[cfg(test)]
mod tests {
    use super::detect_homebrew_install;
    use std::path::Path;

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
}
