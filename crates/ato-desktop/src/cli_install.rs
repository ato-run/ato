//! Bundle-bound CLI helper installation into the user's PATH.
//!
//! `ato-desktop` ships a copy of the `ato` CLI inside the bundle:
//! - macOS:   `Ato Desktop.app/Contents/Helpers/ato`
//! - Linux:   alongside `ato-desktop` in `usr/bin/`
//! - Windows: `Program Files\Ato\bin\ato.exe` (already on PATH via MSI)
//!
//! For users who installed Desktop via direct `.dmg` / `.AppImage`
//! download (not Homebrew Cask, not MSI), the helper inside the bundle
//! is *not* yet on the user PATH. This module surfaces a one-shot
//! "install CLI" affordance that creates a symlink (or copy on
//! Windows) into a writable PATH directory.
//!
//! Spec: CPDS §4.2.1.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Outcome of [`try_install`] — surfaced to the UI as a toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// A symlink/copy was placed at this absolute path.
    Installed { target: PathBuf },
    /// `ato` was already resolvable from PATH and pointed at the same
    /// helper we would have installed; nothing to do.
    AlreadyInstalled { existing: PathBuf },
    /// We installed into `~/.local/bin` (or equivalent) and that
    /// directory is *not* on the current PATH. The UI should show the
    /// shell config snippet so the user can fix it.
    InstalledButPathMissing {
        target: PathBuf,
        path_directory: PathBuf,
        shell_hint: String,
    },
}

/// Persistent flag indicating that we have already shown the
/// first-launch install prompt (regardless of whether the user
/// accepted). Lives at `~/.ato/desktop/cli-install-prompted`.
pub fn first_launch_prompt_marker() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home
        .join(".ato")
        .join("desktop")
        .join("cli-install-prompted"))
}

pub fn first_launch_prompt_already_shown() -> bool {
    first_launch_prompt_marker()
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn record_first_launch_prompt_shown() -> Result<()> {
    let marker = first_launch_prompt_marker()?;
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&marker, b"").with_context(|| format!("failed to write {}", marker.display()))?;
    Ok(())
}

/// Resolve the in-bundle `ato` helper that should be installed.
/// Returns an error if the caller-supplied executable directory does
/// not contain the expected helper layout — exposing a typed error
/// here keeps `app.rs` from string-matching on filesystem messages.
pub fn locate_bundled_helper(current_exe: &Path) -> Result<PathBuf> {
    // The helper layout depends on the platform's bundle conventions:
    //   macOS:   <exe>=.../Contents/MacOS/ato-desktop
    //            helper=.../Contents/Helpers/ato
    //   linux:   <exe>=.../usr/bin/ato-desktop
    //            helper=.../usr/bin/ato
    //   windows: <exe>=.../ato-desktop.exe
    //            helper=.../bin/ato.exe
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?;

    #[cfg(target_os = "macos")]
    let candidate = exe_dir
        .parent()
        .ok_or_else(|| anyhow!("expected MacOS/.. for macOS bundle"))?
        .join("Helpers")
        .join("ato");

    #[cfg(target_os = "windows")]
    let candidate = exe_dir.join("bin").join("ato.exe");

    #[cfg(all(unix, not(target_os = "macos")))]
    let candidate = exe_dir.join("ato");

    if candidate.exists() {
        Ok(candidate)
    } else {
        bail!(
            "bundled ato helper not found at {} — is this a packaged build?",
            candidate.display()
        )
    }
}

/// Try to install the helper into a stable PATH location. Picks the
/// first writable target in this priority order:
///   1. `/usr/local/bin/ato`     (macOS / Linux — preferred, matches
///                                Homebrew Cask path)
///   2. `~/.local/bin/ato`       (XDG fallback, always writable)
///   3. `%LOCALAPPDATA%\Programs\Ato\bin\ato.exe` (Windows fallback;
///       MSI installs already populate Program Files\Ato\bin)
pub fn try_install(helper: &Path) -> Result<InstallOutcome> {
    // Short-circuit: if `ato` already resolves and points at the
    // same helper, the install is already done.
    if let Some(existing) = which_in_path("ato") {
        if same_canonical(&existing, helper) {
            return Ok(InstallOutcome::AlreadyInstalled { existing });
        }
    }

    let target_dir = pick_install_dir()?;
    let helper_name = if cfg!(windows) { "ato.exe" } else { "ato" };
    let target = target_dir.join(helper_name);

    fs::create_dir_all(&target_dir)
        .with_context(|| format!("failed to create install dir {}", target_dir.display()))?;

    // Re-installs are idempotent: blow away anything that's there so
    // a stale symlink from a prior version doesn't trip us up.
    if target.exists() || target.is_symlink() {
        fs::remove_file(&target).ok();
    }

    install_helper(helper, &target)?;

    if !path_contains(&target_dir) {
        return Ok(InstallOutcome::InstalledButPathMissing {
            target,
            path_directory: target_dir.clone(),
            shell_hint: shell_path_hint(&target_dir),
        });
    }
    Ok(InstallOutcome::Installed { target })
}

#[cfg(unix)]
fn install_helper(helper: &Path, target: &Path) -> Result<()> {
    // Prefer a symlink so the bundle update flows through to the CLI
    // automatically; fall back to a copy if symlinks fail (e.g. cross-
    // filesystem).
    if std::os::unix::fs::symlink(helper, target).is_err() {
        fs::copy(helper, target).with_context(|| {
            format!("failed to copy {} → {}", helper.display(), target.display())
        })?;
    }
    Ok(())
}

#[cfg(windows)]
fn install_helper(helper: &Path, target: &Path) -> Result<()> {
    // Windows symlink creation requires Developer Mode or admin; copy
    // is universally writable. The MSI install path already covers
    // most users so this fallback is for direct-download zip flows.
    fs::copy(helper, target)
        .with_context(|| format!("failed to copy {} → {}", helper.display(), target.display()))?;
    Ok(())
}

fn pick_install_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    {
        let preferred = PathBuf::from("/usr/local/bin");
        if is_writable_dir(&preferred) {
            return Ok(preferred);
        }
        let home = dirs::home_dir().context("no home dir")?;
        Ok(home.join(".local").join("bin"))
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| dirs::data_local_dir())
            .context("no LOCALAPPDATA")?;
        Ok(local.join("Programs").join("Ato").join("bin"))
    }
}

#[cfg(unix)]
fn is_writable_dir(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    // We don't try to chase real ACLs; mode bits + a probe write are
    // good enough for the writable-or-not classification we need.
    let mode = meta.permissions().mode();
    if mode & 0o002 != 0 {
        return true;
    }
    // Probe write: try touching a hidden marker. Cleans up after.
    let probe = path.join(".ato-cli-install-probe");
    match fs::write(&probe, b"") {
        Ok(_) => {
            fs::remove_file(&probe).ok();
            true
        }
        Err(_) => false,
    }
}

fn same_canonical(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn path_contains(dir: &Path) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|p| {
        // Compare canonicalized paths to handle symlink + trailing-
        // slash differences uniformly.
        match (fs::canonicalize(&p), fs::canonicalize(dir)) {
            (Ok(p), Ok(d)) => p == d,
            _ => p == dir,
        }
    })
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

fn shell_path_hint(dir: &Path) -> String {
    let display = dir.display();
    format!(
        "Add {display} to your shell's PATH:\n  \
         bash/zsh: echo 'export PATH=\"{display}:$PATH\"' >> ~/.zshrc\n  \
         fish:     fish_add_path {display}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn first_launch_marker_path_is_under_dot_ato() {
        let marker = first_launch_prompt_marker().expect("home dir resolves");
        let s = marker.to_string_lossy();
        assert!(
            s.contains("/.ato/desktop/cli-install-prompted")
                || s.contains("\\.ato\\desktop\\cli-install-prompted"),
            "marker path looked wrong: {s}"
        );
    }

    #[test]
    fn shell_hint_mentions_target_dir() {
        let hint = shell_path_hint(Path::new("/Users/test/.local/bin"));
        assert!(hint.contains("/Users/test/.local/bin"));
        assert!(hint.contains("export PATH"));
    }

    #[test]
    fn try_install_is_idempotent_when_target_already_points_at_same_helper() {
        let tmp = tempdir();
        let helper = tmp.join("ato");
        File::create(&helper).unwrap();

        // Spoof PATH so which_in_path can find a fake "ato" pointing
        // at the same helper. We can't fully exercise the symlink
        // path on every CI runner, so this asserts the early-return
        // branch.
        std::env::set_var("PATH", tmp.to_string_lossy().to_string());
        // PATH already containing the helper means the canonical-
        // match short-circuit fires.
        let outcome = try_install(&helper).expect("install should succeed");
        match outcome {
            InstallOutcome::AlreadyInstalled { .. } => {}
            other => panic!("expected AlreadyInstalled, got {other:?}"),
        }
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        // Unique per-test dir; we don't need a real RAII guard for a
        // few-bytes file in /tmp.
        p.push(format!(
            "ato-cli-install-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
