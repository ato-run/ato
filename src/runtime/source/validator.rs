//! Command Validator for Generic Source Runtime
//!
//! Provides security validation for `cmd` field in `capsule.toml`.
//! Implements two key security rules:
//!
//! 1. **File-First Execution**: The last argument must be a real file inside the capsule
//! 2. **Dangerous Flag Sanitization**: Known dangerous flags are rejected
//!
//! ## Security Model
//!
//! - **Non-Dev Mode (Production)**: Strict validation, all dangerous flags rejected
//! - **Dev Mode**: Relaxed validation, some debug flags allowed but inline scripts still blocked

use anyhow::{anyhow, Result};
use std::path::Path;
use tracing::{debug, warn};

/// Known dangerous flags that could bypass security or enable RCE
const DANGEROUS_FLAGS: &[&str] = &[
    // Inline script execution (highest risk)
    "-c",     // Python, Bash, Shell
    "-e",     // Perl, Ruby, Node.js
    "--eval", // Node.js
    "-exec",  // Various
    // Debug/Inspect ports (RCE risk)
    "--inspect",      // Node.js debugger
    "--inspect-brk",  // Node.js debugger with breakpoint
    "--inspect-wait", // Node.js debugger
    "--debug",        // Various debuggers
    "--debug-brk",    // Node.js legacy debugger
    // Remote execution
    "--remote-debugging-port", // Chromium-based
    // PHP specific
    "-r", // PHP inline code
    "-S", // PHP built-in server (can expose files)
    // Python specific
    "-m", // Python module execution (can be abused)
];

/// Flags that are dangerous in production but acceptable in dev mode
const DEV_ONLY_FLAGS: &[&str] = &[
    "--inspect",
    "--inspect-brk",
    "--inspect-wait",
    "--debug",
    "--debug-brk",
    "-m", // Allow python -m in dev mode for testing
];

/// Validate command arguments for security
///
/// # Arguments
/// * `cmd` - The command array (e.g., ["ruby", "app.rb"])
/// * `capsule_root` - Root directory of the capsule
/// * `dev_mode` - Whether development mode is enabled
///
/// # Returns
/// * `Ok(())` if validation passes
/// * `Err` with detailed message if validation fails
pub fn validate_cmd(cmd: &[String], capsule_root: &Path, dev_mode: bool) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow!("Security Violation: Empty command specified"));
    }

    debug!("Validating command: {:?} (dev_mode: {})", cmd, dev_mode);

    // Rule 1: Check for dangerous flags
    validate_flags(cmd, dev_mode)?;

    // Rule 2: File-First Execution - last argument should be a file in capsule
    validate_file_first(cmd, capsule_root)?;

    debug!("Command validation passed");
    Ok(())
}

/// Validate that no dangerous flags are present
fn validate_flags(cmd: &[String], dev_mode: bool) -> Result<()> {
    for (i, arg) in cmd.iter().enumerate() {
        // Skip the first argument (binary name)
        if i == 0 {
            continue;
        }

        // Check against dangerous flags
        for &dangerous in DANGEROUS_FLAGS {
            if arg == dangerous || arg.starts_with(&format!("{}=", dangerous)) {
                // In dev mode, some flags are allowed
                if dev_mode && DEV_ONLY_FLAGS.contains(&dangerous) {
                    warn!("Allowing dev-only flag '{}' in development mode", dangerous);
                    continue;
                }

                return Err(anyhow!(
                    "Security Violation: Dangerous flag '{}' detected.\n\
                     This flag can enable arbitrary code execution.\n\
                     If you need this for development, use: capsule open --dev",
                    dangerous
                ));
            }
        }

        // Additional check for inline code patterns
        if is_inline_code_pattern(arg) {
            return Err(anyhow!(
                "Security Violation: Inline code execution detected.\n\
                 All executed code must be in a file within the capsule.\n\
                 Found suspicious argument: '{}'",
                truncate_for_display(arg, 50)
            ));
        }
    }

    Ok(())
}

/// Validate File-First execution rule
///
/// The entrypoint (typically last argument) must be a file inside the capsule.
/// This prevents inline script execution like `python -c "malicious code"`.
fn validate_file_first(cmd: &[String], capsule_root: &Path) -> Result<()> {
    if cmd.len() < 2 {
        // Single command (just binary) - might be acceptable for some runtimes
        warn!("Command has no arguments - no file-first validation possible");
        return Ok(());
    }

    // Find the likely entrypoint (skip flags, find first file-like argument)
    let entrypoint = find_entrypoint(cmd);

    match entrypoint {
        Some(file_arg) => {
            // Resolve path relative to capsule root
            let target_path = if Path::new(file_arg).is_absolute() {
                // Absolute paths must still be within capsule
                Path::new(file_arg).to_path_buf()
            } else {
                capsule_root.join(file_arg)
            };

            // Security: Path traversal check
            let canonical_root = capsule_root
                .canonicalize()
                .unwrap_or_else(|_| capsule_root.to_path_buf());
            let canonical_target = target_path
                .canonicalize()
                .unwrap_or_else(|_| target_path.clone());

            if !canonical_target.starts_with(&canonical_root) {
                return Err(anyhow!(
                    "Security Violation: Path traversal detected.\n\
                     Entrypoint '{}' resolves to '{}' which is outside the capsule root '{}'.",
                    file_arg,
                    canonical_target.display(),
                    canonical_root.display()
                ));
            }

            // Check if file exists
            if !target_path.exists() {
                return Err(anyhow!(
                    "Security Violation: Entrypoint file not found.\n\
                     Expected file: '{}' (resolved: '{}')\n\
                     All executed code must be in a file within the capsule.",
                    file_arg,
                    target_path.display()
                ));
            }

            if !target_path.is_file() {
                return Err(anyhow!(
                    "Security Violation: Entrypoint is not a file.\n\
                     '{}' exists but is a directory or special file.",
                    target_path.display()
                ));
            }

            debug!("File-first validation passed: {}", target_path.display());
            Ok(())
        }
        None => {
            // No clear entrypoint found - could be flags only
            warn!("Could not identify entrypoint file in command");
            // In strict mode, this could be an error
            // For now, allow it as some runtimes might work this way
            Ok(())
        }
    }
}

/// Find the likely entrypoint file in command arguments
///
/// Skips known flags and looks for file-like arguments
fn find_entrypoint(cmd: &[String]) -> Option<&str> {
    for (i, arg) in cmd.iter().enumerate().rev() {
        // Skip the binary itself
        if i == 0 {
            continue;
        }

        // Skip flags (start with -)
        if arg.starts_with('-') {
            continue;
        }

        // Skip values that look like flag values (after = in previous flag)
        // This is a simple heuristic

        // Check if it looks like a file path
        if looks_like_file_path(arg) {
            return Some(arg);
        }
    }

    // Fallback: return last non-flag argument
    for arg in cmd.iter().rev().skip(0) {
        if !arg.starts_with('-') && !cmd.first().map(|f| f == arg).unwrap_or(false) {
            return Some(arg);
        }
    }

    None
}

/// Check if argument looks like a file path
fn looks_like_file_path(arg: &str) -> bool {
    // Has file extension
    if arg.contains('.') {
        let ext = arg.rsplit('.').next().unwrap_or("");
        let common_extensions = [
            "py", "rb", "js", "ts", "mjs", "cjs", "pl", "php", "sh", "bash", "lua", "r", "R", "jl",
            "go", "rs", "java", "kt", "swift", "ex", "exs",
        ];
        if common_extensions.contains(&ext) {
            return true;
        }
    }

    // Contains path separator
    if arg.contains('/') || arg.contains('\\') {
        return true;
    }

    // Starts with ./ or ../
    if arg.starts_with("./") || arg.starts_with("../") {
        return true;
    }

    false
}

/// Check for inline code execution patterns
fn is_inline_code_pattern(arg: &str) -> bool {
    // Check for common inline code indicators
    // These patterns often indicate someone trying to run inline code

    // Multi-line code with common language constructs
    let suspicious_patterns = [
        "import ",       // Python import in argument
        "require(",      // Node.js require
        "console.log",   // JavaScript
        "puts ",         // Ruby
        "print(",        // Various
        "exec(",         // Shell/Python
        "system(",       // Ruby/Perl
        "eval(",         // Various
        "os.system",     // Python
        "subprocess",    // Python
        "__import__",    // Python
        "Process.spawn", // Ruby
    ];

    for pattern in suspicious_patterns {
        if arg.contains(pattern) {
            return true;
        }
    }

    // Very long arguments that look like code
    if arg.len() > 200 && (arg.contains(';') || arg.contains('\n')) {
        return true;
    }

    false
}

/// Truncate string for display in error messages
fn truncate_for_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

/// Validate binary name against allowlist
///
/// Only whitelisted binaries can be executed to prevent arbitrary binary execution.
pub fn validate_binary(binary: &str, dev_mode: bool) -> Result<()> {
    const ALLOWED_BINARIES: &[&str] = &[
        // Python
        "python",
        "python3",
        "python3.11",
        "python3.12",
        "python3.13",
        // Node.js
        "node",
        "nodejs",
        // Deno
        "deno",
        // Ruby
        "ruby",
        // Go
        "go",
        // Perl
        "perl",
        // Rust (for running scripts via cargo-script etc)
        "cargo",
        // Bun
        "bun",
    ];

    // In dev mode, allow more flexibility
    const DEV_ALLOWED_BINARIES: &[&str] = &[
        // All production binaries plus:
        "npx",    // Node package runner
        "yarn",   // Yarn package manager
        "pnpm",   // pnpm package manager
        "uv",     // Python uv
        "pip",    // Python pip (for dev setup)
        "poetry", // Python poetry
    ];

    // Check production allowlist
    if ALLOWED_BINARIES.contains(&binary) {
        return Ok(());
    }

    // Check dev-only allowlist
    if dev_mode && DEV_ALLOWED_BINARIES.contains(&binary) {
        warn!("Allowing dev-only binary '{}' in development mode", binary);
        return Ok(());
    }

    // Explicitly deny shell
    const DENIED_BINARIES: &[&str] = &[
        "sh",
        "bash",
        "zsh",
        "fish",
        "csh",
        "tcsh",
        "dash",
        "cmd",
        "powershell",
        "pwsh",
    ];

    if DENIED_BINARIES.contains(&binary) {
        return Err(anyhow!(
            "Security Violation: Shell execution is not allowed.\n\
             Binary '{}' is explicitly denied.\n\
             Wrap your logic in a script file and use the appropriate runtime.",
            binary
        ));
    }

    Err(anyhow!(
        "Security Violation: Binary '{}' is not in the allowlist.\n\
         Allowed binaries: {:?}\n\
         If you need this binary for development, use: capsule open --dev",
        binary,
        ALLOWED_BINARIES
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_capsule() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("main.py"), "print('hello')").unwrap();
        fs::write(temp.path().join("app.rb"), "puts 'hello'").unwrap();
        temp
    }

    #[test]
    fn test_valid_python_command() {
        let capsule = setup_test_capsule();
        let cmd = vec!["python3".to_string(), "main.py".to_string()];
        assert!(validate_cmd(&cmd, capsule.path(), false).is_ok());
    }

    #[test]
    fn test_valid_ruby_command() {
        let capsule = setup_test_capsule();
        let cmd = vec!["ruby".to_string(), "app.rb".to_string()];
        assert!(validate_cmd(&cmd, capsule.path(), false).is_ok());
    }

    #[test]
    fn test_inline_script_rejected() {
        let capsule = setup_test_capsule();
        let cmd = vec![
            "python3".to_string(),
            "-c".to_string(),
            "import os; os.system('cat /etc/passwd')".to_string(),
        ];
        let result = validate_cmd(&cmd, capsule.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Dangerous flag"));
    }

    #[test]
    fn test_node_inspect_rejected_in_prod() {
        let capsule = setup_test_capsule();
        fs::write(capsule.path().join("main.js"), "console.log('hello')").unwrap();
        let cmd = vec![
            "node".to_string(),
            "--inspect=0.0.0.0:9229".to_string(),
            "main.js".to_string(),
        ];
        let result = validate_cmd(&cmd, capsule.path(), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_node_inspect_allowed_in_dev() {
        let capsule = setup_test_capsule();
        fs::write(capsule.path().join("main.js"), "console.log('hello')").unwrap();
        let cmd = vec![
            "node".to_string(),
            "--inspect".to_string(),
            "main.js".to_string(),
        ];
        // In dev mode, --inspect should be allowed
        assert!(validate_cmd(&cmd, capsule.path(), true).is_ok());
    }

    #[test]
    fn test_path_traversal_rejected() {
        let capsule = setup_test_capsule();
        let cmd = vec!["python3".to_string(), "../../../etc/passwd".to_string()];
        let result = validate_cmd(&cmd, capsule.path(), false);
        // This should fail either due to path traversal or file not found
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_file_rejected() {
        let capsule = setup_test_capsule();
        let cmd = vec!["python3".to_string(), "nonexistent.py".to_string()];
        let result = validate_cmd(&cmd, capsule.path(), false);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("file not found") || err_msg.contains("Entrypoint"),
            "Expected error about missing file, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_binary_allowlist() {
        assert!(validate_binary("python3", false).is_ok());
        assert!(validate_binary("ruby", false).is_ok());
        assert!(validate_binary("node", false).is_ok());
        assert!(validate_binary("bash", false).is_err());
        assert!(validate_binary("sh", false).is_err());
        assert!(validate_binary("arbitrary_binary", false).is_err());
    }

    #[test]
    fn test_dev_only_binary() {
        assert!(validate_binary("npx", false).is_err());
        assert!(validate_binary("npx", true).is_ok());
    }
}
