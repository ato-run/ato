//! First-run bootstrap for the age identity used by `AuthStore`/`SecretStore`.
//!
//! `ato auth login` (interactive) persists its session token through an age
//! file at `~/.ato/credentials/auth/session.age`, which is only usable once
//! `~/.ato/keys/identity.key` exists. Historically users had to run
//! `ato secrets init` themselves before logging in; forgetting that step meant
//! the token was silently kept only in memory and did not survive the process.
//!
//! This module provides a single entry point, [`ensure_identity_interactive`],
//! that detects the missing-identity case and offers to create it inline from
//! the login flow (or anywhere else that wants persistent credential storage).

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};
use capsule_core::common::paths::nacelle_home_dir;

use crate::application::credential::AgeFileBackend;

/// Outcome of a bootstrap attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootstrapOutcome {
    /// An identity already existed on disk — nothing to do.
    AlreadyPresent,
    /// A new identity was generated during this call.
    Created,
    /// The user declined the prompt. Caller should proceed without persistence.
    Declined,
    /// stdin/stderr is not a TTY, so we could not prompt. Caller should warn
    /// and proceed without persistence.
    NonInteractive,
}

/// Ensure an age identity exists at `~/.ato/keys/identity.key`, prompting the
/// user to create one if it is missing and the shell is interactive.
///
/// Never fails when the user simply declines or when we are running
/// non-interactively — those cases return `Declined` / `NonInteractive` so the
/// caller can fall back to the memory-only path with its own warning. Errors
/// are only propagated when identity creation itself fails (I/O, encryption).
pub(crate) fn ensure_identity_interactive() -> Result<BootstrapOutcome> {
    let ato_home = nacelle_home_dir().context("failed to resolve ato home")?;
    let age = AgeFileBackend::new(ato_home);

    if age.identity_exists() {
        return Ok(BootstrapOutcome::AlreadyPresent);
    }

    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(BootstrapOutcome::NonInteractive);
    }

    eprintln!(
        "🔐 No age identity found — needed to persist your login token across shell sessions."
    );
    eprintln!(
        "   Will create one at {}",
        age.identity_key_path().display()
    );
    eprint!("   Create now? [Y/n] ");
    io::stderr().flush().ok();

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read confirmation")?;
    let answer = answer.trim().to_lowercase();
    if !(answer.is_empty() || answer == "y" || answer == "yes") {
        return Ok(BootstrapOutcome::Declined);
    }

    let pp = rpassword::prompt_password("   Passphrase for identity.key (leave empty to skip): ")
        .context("failed to read passphrase")?;
    let passphrase = if pp.is_empty() { None } else { Some(pp) };

    let identity = age.init_identity(passphrase.as_deref())?;

    eprintln!(
        "✅ age identity created at {}",
        age.identity_key_path().display()
    );
    eprintln!("   Public key: {}", identity.to_public());
    if passphrase.is_some() {
        eprintln!("   Protected with passphrase.");
        eprintln!("   Run `ato session start` to unlock once per shell session.");
    } else {
        eprintln!("   ⚠️  No passphrase — protected by file permissions (chmod 600).");
    }

    Ok(BootstrapOutcome::Created)
}
