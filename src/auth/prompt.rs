use anyhow::{Context, Result};
use std::io::{self, Write};

pub(super) fn try_open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `open`")?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `xdg-open`")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .context("Failed to launch browser with `start`")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
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
