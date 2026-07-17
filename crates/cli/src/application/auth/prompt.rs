use anyhow::{Context, Result};
use std::io::{self, Write};

/// The desktop OS families whose default-browser launcher differs. Passed
/// explicitly into `browser_open_command` (rather than read there from
/// `cfg!`) so the argv for all three can be unit-tested from a single host
/// build — the RFC 8252 native-app login (ato#1077) depends on the URL being
/// handed to the browser as one discrete argument on every platform, and that
/// property is worthless if it can only ever be exercised on the CI host's own
/// OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BrowserOpenOs {
    MacOs,
    Linux,
    Windows,
}

impl BrowserOpenOs {
    /// The OS this binary was compiled for, or `None` on a target with no
    /// known system-browser launcher (in which case `try_open_browser` is a
    /// no-op, exactly as the prior `#[allow(unreachable_code)] Ok(())` was).
    ///
    /// Matches on `std::env::consts::OS` (the compile-time target-OS string)
    /// rather than `#[cfg(target_os = ...)]` so every variant is constructed
    /// in-source on every target — a `cfg`-gated version leaves the two
    /// non-host variants "never constructed" and trips `dead_code` on a
    /// non-test build.
    fn current() -> Option<Self> {
        match std::env::consts::OS {
            "macos" => Some(Self::MacOs),
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Program + argument vector for opening `url` in the OS default browser.
///
/// Pure (no spawn, no `cfg`) so the exact argv is unit-testable for every OS
/// from one host, including the security-relevant property that `url` is
/// always a single discrete argument — never concatenated into a shell string
/// where its `?`, `&`, or quotes could be reinterpreted. Layouts:
/// - macOS: `open <url>`
/// - Linux: `xdg-open <url>`
/// - Windows: `cmd /C start "" <url>` — the empty `""` fills `start`'s
///   window-title slot, so a `url` that happens to start with `"` can't be
///   swallowed as the title instead of the target.
pub(super) fn browser_open_command(os: BrowserOpenOs, url: &str) -> (&'static str, Vec<String>) {
    match os {
        BrowserOpenOs::MacOs => ("open", vec![url.to_string()]),
        BrowserOpenOs::Linux => ("xdg-open", vec![url.to_string()]),
        BrowserOpenOs::Windows => (
            "cmd",
            vec![
                "/C".to_string(),
                "start".to_string(),
                String::new(),
                url.to_string(),
            ],
        ),
    }
}

pub(super) fn try_open_browser(url: &str) -> Result<()> {
    let Some(os) = BrowserOpenOs::current() else {
        return Ok(());
    };
    let (program, args) = browser_open_command(os, url);
    std::process::Command::new(program)
        .args(&args)
        .spawn()
        .with_context(|| format!("Failed to launch browser with `{program}`"))?;
    Ok(())
}

pub(super) fn prompt_line(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush().context("Failed to flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read from stdin")?;
    Ok(input.trim().to_string())
}

pub(super) fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    let answer = prompt_line(&format!("{} {}: ", prompt, suffix))?;
    if answer.is_empty() {
        return Ok(default_yes);
    }
    let normalized = answer.to_ascii_lowercase();
    if ["y", "yes"].contains(&normalized.as_str()) {
        return Ok(true);
    }
    if ["n", "no"].contains(&normalized.as_str()) {
        return Ok(false);
    }
    Ok(default_yes)
}
