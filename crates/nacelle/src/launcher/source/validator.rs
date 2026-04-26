//! Command Validator
//!
//! Policy: "Allow Any, Block Non-Portable"
//!
//! We allow ANY command name, trusting the OS Sandbox (Landlock/Seatbelt) to contain it.
//! We only block absolute paths and directory traversal because they break portability.
//!
//! ## Security Model
//!
//! This module performs **configuration validation**, NOT security enforcement.
//! The actual security boundary is the OS-level sandbox.
//!
//! Why no binary allowlist?
//! - Allowing `npm` or `python` already means arbitrary code can run
//! - `npm run dev` can execute anything in package.json scripts
//! - `python script.py` can do anything Python can do
//! - Security comes from the Sandbox restricting filesystem/network access
//!
//! Purpose of this validator:
//! 1. Sanity check: Catch typos and misconfiguration early
//! 2. Portability check: Reject absolute paths that won't work on other machines

use anyhow::{anyhow, Result};
use std::path::Path;
use tracing::debug;

/// Validate command configuration (NOT security enforcement)
///
/// # Arguments
/// * `cmd` - The command array (e.g., ["npm", "run", "dev"])
/// * `_capsule_root` - Root directory of the capsule (unused in simplified version)
/// * `_dev_mode` - Development mode flag (unused in simplified version)
///
/// # Returns
/// * `Ok(())` if the command is portable and well-formed
/// * `Err` only for clearly broken configurations (absolute paths, traversal)
pub fn validate_cmd(cmd: &[String], _capsule_root: &Path, _dev_mode: bool) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow!("Configuration Error: Empty command specified"));
    }

    let binary = &cmd[0];
    debug!("Validating command: {:?}", cmd);

    // Rule 1: Block absolute paths (portability issue)
    // Rationale: Absolute paths indicate the author is targeting a specific system,
    // which breaks portability and is almost always a mistake.
    if binary.starts_with('/') || binary.starts_with('\\') {
        return Err(anyhow!(
            "Configuration Error: Absolute paths are not allowed in entrypoint.\n\
             Use a relative command like 'python' instead of '/usr/bin/python'.\n\
             Found: '{}'",
            binary
        ));
    }

    // Rule 2: Block directory traversal (sanity check)
    // Rationale: `../../../bin/sh` is almost certainly a mistake or attack attempt.
    if binary.contains("../") || binary.contains("..\\") {
        return Err(anyhow!(
            "Configuration Error: Directory traversal in command is not allowed.\n\
             Found: '{}'",
            binary
        ));
    }

    // Rule 3: Block path-like binaries that aren't relative to current dir
    // Allow: ./my-script, python, npm
    // Block: /bin/sh, C:\Windows\System32\cmd.exe
    if binary.contains('/') && !binary.starts_with("./") {
        return Err(anyhow!(
            "Configuration Error: Path-like commands must be relative (start with './').\n\
             Found: '{}'",
            binary
        ));
    }

    debug!("Command validation passed (any command allowed)");
    Ok(())
}

/// Validate binary name (simplified - allows any binary)
///
/// This function is kept for API compatibility but now only performs
/// portability checks. Security is enforced by the OS sandbox, not here.
///
/// # Arguments
/// * `binary` - The binary/command name
/// * `_dev_mode` - Development mode flag (unused)
pub fn validate_binary(binary: &str, _dev_mode: bool) -> Result<()> {
    // Only block absolute paths for portability
    if binary.starts_with('/') || binary.starts_with('\\') {
        return Err(anyhow!(
            "Configuration Error: Absolute paths are not allowed.\n\
             Use a command name like 'python' instead of '/usr/bin/python'.\n\
             Found: '{}'",
            binary
        ));
    }

    // Block directory traversal
    if binary.contains("../") || binary.contains("..\\") {
        return Err(anyhow!(
            "Configuration Error: Directory traversal is not allowed.\n\
             Found: '{}'",
            binary
        ));
    }

    // All other binaries are allowed - security is handled by the sandbox
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn capsule_root() -> PathBuf {
        PathBuf::from("/tmp/test-capsule")
    }

    #[test]
    fn test_empty_command_rejected() {
        let cmd: Vec<String> = vec![];
        assert!(validate_cmd(&cmd, &capsule_root(), false).is_err());
    }

    #[test]
    fn test_simple_command_allowed() {
        let cmd = vec!["npm".to_string(), "run".to_string(), "dev".to_string()];
        assert!(validate_cmd(&cmd, &capsule_root(), false).is_ok());
    }

    #[test]
    fn test_python_command_allowed() {
        let cmd = vec!["python3".to_string(), "main.py".to_string()];
        assert!(validate_cmd(&cmd, &capsule_root(), false).is_ok());
    }

    #[test]
    fn test_arbitrary_command_allowed() {
        let cmd = vec!["super-weird-binary".to_string(), "arg1".to_string()];
        assert!(validate_cmd(&cmd, &capsule_root(), false).is_ok());
    }

    #[test]
    fn test_relative_script_allowed() {
        let cmd = vec!["./my-script.sh".to_string()];
        assert!(validate_cmd(&cmd, &capsule_root(), false).is_ok());
    }

    #[test]
    fn test_absolute_path_rejected() {
        let cmd = vec!["/usr/bin/python3".to_string(), "main.py".to_string()];
        let result = validate_cmd(&cmd, &capsule_root(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Absolute paths"));
    }

    #[test]
    fn test_windows_absolute_path_rejected() {
        let cmd = vec!["\\Windows\\System32\\cmd.exe".to_string()];
        let result = validate_cmd(&cmd, &capsule_root(), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_directory_traversal_rejected() {
        let cmd = vec!["../../../bin/sh".to_string()];
        let result = validate_cmd(&cmd, &capsule_root(), false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Directory traversal"));
    }

    #[test]
    fn test_path_like_without_dot_slash_rejected() {
        let cmd = vec!["bin/my-script".to_string()];
        let result = validate_cmd(&cmd, &capsule_root(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("relative"));
    }

    #[test]
    fn test_any_binary_allowed() {
        assert!(validate_binary("npm", false).is_ok());
        assert!(validate_binary("python3", false).is_ok());
        assert!(validate_binary("ruby", false).is_ok());
        assert!(validate_binary("bun", false).is_ok());
        assert!(validate_binary("deno", false).is_ok());
        assert!(validate_binary("my-custom-runtime", false).is_ok());
        assert!(validate_binary("bash", false).is_ok());
        assert!(validate_binary("sh", false).is_ok());
    }

    #[test]
    fn test_absolute_binary_rejected() {
        assert!(validate_binary("/usr/bin/python3", false).is_err());
        assert!(validate_binary("/bin/sh", false).is_err());
    }

    #[test]
    fn test_traversal_binary_rejected() {
        assert!(validate_binary("../../bin/sh", false).is_err());
        assert!(validate_binary("../python", false).is_err());
    }

    #[test]
    fn test_dev_mode_makes_no_difference() {
        assert!(validate_binary("npm", true).is_ok());
        assert!(validate_binary("npm", false).is_ok());
        assert!(validate_binary("/usr/bin/python", true).is_err());
        assert!(validate_binary("/usr/bin/python", false).is_err());
    }
}
