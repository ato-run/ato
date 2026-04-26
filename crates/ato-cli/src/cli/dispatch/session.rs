use anyhow::{bail, Context, Result};

use crate::cli::session::IdentitySessionCommands as SessionCommands;

pub(crate) fn execute_session_command(command: SessionCommands) -> Result<()> {
    match command {
        SessionCommands::Start { ttl } => cmd_start(&ttl),
        SessionCommands::End => cmd_end(),
        SessionCommands::Status => cmd_status(),
    }
}

// ── Session file helpers ──────────────────────────────────────────────────────

/// Path for the current process's session key file.
///
/// If `ATO_SESSION_KEY_FILE` is set (forwarded by a parent `ato session start`),
/// uses that path.  Otherwise derives a new per-pid path.
fn session_key_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ATO_SESSION_KEY_FILE") {
        return std::path::PathBuf::from(p);
    }
    let home = dirs::home_dir().expect("failed to resolve home directory");
    home.join(".ato")
        .join("run")
        .join(format!("session-{}.key", std::process::id()))
}

/// Parse a human duration string ("8h", "30m", "1h30m") into seconds.
fn parse_ttl(s: &str) -> Result<u64> {
    let mut secs = 0u64;
    let mut num_buf = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            let n: u64 = num_buf
                .parse()
                .with_context(|| format!("invalid TTL number in '{s}'"))?;
            num_buf.clear();
            match ch {
                'h' => secs += n * 3600,
                'm' => secs += n * 60,
                's' => secs += n,
                _ => bail!("unknown TTL unit '{}' in '{}'", ch, s),
            }
        }
    }
    if !num_buf.is_empty() {
        bail!("TTL '{}' must end with a unit (h/m/s)", s);
    }
    if secs == 0 {
        bail!("TTL must be > 0 seconds");
    }
    Ok(secs)
}

// ── Subcommands ───────────────────────────────────────────────────────────────

fn cmd_start(ttl: &str) -> Result<()> {
    let ttl_secs = parse_ttl(ttl)?;
    let path = session_key_path();

    // Clean up stale session files from dead processes first.
    cleanup_stale_sessions();

    if path.exists() {
        eprintln!("⚠️  Session already active at {}", path.display());
        eprintln!("   Use `ato session status` to inspect, or `ato session end` to revoke it.");
        return Ok(());
    }

    // Load the identity (will prompt for passphrase if needed).
    let home = dirs::home_dir().context("failed to resolve home directory")?;
    let age = crate::application::credential::AgeFileBackend::new(home.clone());

    if !age.identity_exists() {
        bail!(
            "no age identity found\n\
             Run `ato secrets init` to create one."
        );
    }

    // Try loading without passphrase first (plain-text identity).
    if age.load_identity_with_passphrase(None).is_err() {
        // Need passphrase – prompt interactively.
        let pp = rpassword::prompt_password("Passphrase for identity.key: ")
            .context("failed to read passphrase")?;
        age.load_identity_with_passphrase(Some(&pp))
            .context("wrong passphrase")?;
    }

    // Write session key file: plain AGE-SECRET-KEY-1... (chmod 600).
    // The file is only used within this user session and is deleted on `session end`.
    let key_str = {
        let guard = age.identity_for_session();
        guard.context("identity not loaded after unlock")?
    };

    std::fs::create_dir_all(path.parent().unwrap()).context("failed to create ~/.ato/run/")?;
    crate::application::credential::write_secure_file(&path, key_str.as_bytes())?;

    // Compute and store expiry.
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + ttl_secs;

    let meta_path = path.with_extension("key.meta");
    let meta = format!(
        "{{\"pid\":{},\"expires_at\":{},\"ttl\":\"{ttl}\"}}",
        std::process::id(),
        expires_at
    );
    crate::application::credential::write_secure_file(&meta_path, meta.as_bytes())?;

    // Export for child processes.
    let path_str = path.to_string_lossy();
    eprintln!("✅ Session started (TTL: {ttl})");
    eprintln!("   Key file: {path_str}");
    eprintln!(
        "\nTo propagate to child processes, export:\n  export ATO_SESSION_KEY_FILE={path_str}"
    );
    Ok(())
}

fn cmd_end() -> Result<()> {
    let path = session_key_path();
    let meta_path = path.with_extension("key.meta");

    if !path.exists() {
        if let Ok(p) = std::env::var("ATO_SESSION_KEY_FILE") {
            let explicit = std::path::PathBuf::from(&p);
            if explicit.exists() {
                std::fs::remove_file(&explicit).ok();
                std::fs::remove_file(explicit.with_extension("key.meta")).ok();
                eprintln!("🔒 Session ended: {}", explicit.display());
                return Ok(());
            }
        }
        eprintln!("No active session found.");
        return Ok(());
    }

    std::fs::remove_file(&path).with_context(|| format!("failed to delete {}", path.display()))?;
    std::fs::remove_file(&meta_path).ok();

    eprintln!("🔒 Session ended: {}", path.display());
    Ok(())
}

fn cmd_status() -> Result<()> {
    let path = session_key_path();

    // Also check ATO_SESSION_KEY_FILE.
    let active_path = if let Ok(p) = std::env::var("ATO_SESSION_KEY_FILE") {
        let ep = std::path::PathBuf::from(p);
        if ep.exists() {
            ep
        } else {
            path.clone()
        }
    } else {
        path.clone()
    };

    if !active_path.exists() {
        // Scan for any session files for this user.
        let run_dir = dirs::home_dir()
            .map(|h| h.join(".ato/run"))
            .unwrap_or_default();
        let sessions = find_active_sessions(&run_dir);
        if sessions.is_empty() {
            eprintln!("No active sessions.");
        } else {
            eprintln!("Active sessions:");
            for s in &sessions {
                eprintln!("  {}", s.display());
            }
        }
        return Ok(());
    }

    let meta_path = active_path.with_extension("key.meta");
    if meta_path.exists() {
        let meta_raw = std::fs::read_to_string(&meta_path).unwrap_or_default();
        let meta: serde_json::Value = serde_json::from_str(&meta_raw).unwrap_or_default();
        let pid = meta.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
        let ttl = meta
            .get("ttl")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let expires_at = meta.get("expires_at").and_then(|v| v.as_u64()).unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        eprintln!("Session active:");
        eprintln!("  File:    {}", active_path.display());
        eprintln!("  PID:     {pid}");
        eprintln!("  TTL:     {ttl}");
        if expires_at > 0 {
            if expires_at > now {
                let remaining = expires_at - now;
                let h = remaining / 3600;
                let m = (remaining % 3600) / 60;
                eprintln!("  Expires: in {h}h {m}m");
            } else {
                eprintln!("  Expires: ⚠️  EXPIRED");
            }
        }
    } else {
        eprintln!("Session active: {}", active_path.display());
    }
    Ok(())
}

// ── Cleanup helpers ───────────────────────────────────────────────────────────

fn cleanup_stale_sessions() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let run_dir = home.join(".ato/run");
    if !run_dir.exists() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for entry in std::fs::read_dir(&run_dir).into_iter().flatten().flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("session-") || !name.ends_with(".key") {
            continue;
        }
        // Extract PID from filename.
        if let Some(pid_str) = name
            .strip_prefix("session-")
            .and_then(|s| s.strip_suffix(".key"))
        {
            if let Ok(pid) = pid_str.parse::<u32>() {
                let meta_path = p.with_extension("key.meta");
                let expired = if meta_path.exists() {
                    let raw = std::fs::read_to_string(&meta_path).unwrap_or_default();
                    let meta: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
                    let expires_at = meta
                        .get("expires_at")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(u64::MAX);
                    expires_at < now
                } else {
                    false
                };
                // Check if PID is still alive.
                let process_dead = !pid_is_alive(pid);
                if expired || process_dead {
                    std::fs::remove_file(&p).ok();
                    std::fs::remove_file(&meta_path).ok();
                }
            }
        }
    }
}

fn find_active_sessions(run_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !run_dir.exists() {
        return vec![];
    }
    std::fs::read_dir(run_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_owned();
            if name.starts_with("session-") && name.ends_with(".key") && p.is_file() {
                Some(p)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // kill(pid, 0) returns Ok if process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true // Conservative: assume alive on non-unix
}
