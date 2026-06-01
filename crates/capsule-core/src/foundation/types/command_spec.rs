//! Command specification types for recipe target commands.
//!
//! Provides `CommandSpec` — a unified representation of a command that
//! can be either a plain argv tuple (no shell dependency) or an explicit
//! shell script. The `String` variant handles backward compatibility with
//! existing `run = "..."` / `build = "..."` manifest fields.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A command specification for a target lifecycle hook.
///
/// Three forms are supported in TOML:
///
/// ```toml
/// # Legacy string — auto-detected at execution time
/// install = "bun install"
///
/// # Argv — explicit program + args + optional cwd/env, no shell dependency
/// install = { cmd = "bun", args = ["install"] }
///
/// # Shell — explicit shell script, records shell dependency
/// prestart = { shell = "cd prisma && prisma migrate deploy" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandSpec {
    /// Explicit shell script. Records a shell dependency for reproducibility.
    Shell {
        shell: String,
        #[serde(default)]
        shell_kind: ShellKind,
    },
    /// Explicit argv command. No shell involvement.
    Argv {
        cmd: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Backward-compatible string form. At execution time, this is
    /// converted to either `Argv` or `Shell` based on whether the
    /// string contains shell operators.
    String(String),
}

impl CommandSpec {
    /// Returns the shell script if this is an explicit `Shell` variant.
    pub fn shell_script(&self) -> Option<&str> {
        match self {
            CommandSpec::Shell { shell, .. } => Some(shell),
            _ => None,
        }
    }

    /// Returns the shell kind if this is an explicit `Shell` variant.
    pub fn shell_kind(&self) -> Option<&ShellKind> {
        match self {
            CommandSpec::Shell { shell_kind, .. } => Some(shell_kind),
            _ => None,
        }
    }
}

/// Recognised shell types for `CommandSpec::Shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ShellKind {
    /// POSIX `/bin/sh` (macOS, Linux, WSL).
    #[serde(rename = "posix-sh")]
    #[default]
    PosixSh,
    /// Windows `cmd.exe /C`.
    #[serde(rename = "windows-cmd")]
    WindowsCmd,
    /// Windows `powershell -NoProfile -Command`.
    #[serde(rename = "powershell")]
    Powershell,
}

/// Convert a legacy string command into a `CommandSpec`.
///
/// Commands containing shell operators (`&&`, `||`, `;`, `|`, `>`, `<`, `$`)
/// or env-prefix syntax (`KEY=value command`) are treated as shell scripts.
/// Simple commands are split into argv.
pub fn command_spec_from_string(s: &str) -> CommandSpec {
    if contains_shell_operators(s) {
        return CommandSpec::Shell {
            shell: s.to_string(),
            shell_kind: ShellKind::PosixSh,
        };
    }
    let tokens = shell_words::split(s).unwrap_or_else(|_| vec![s.to_string()]);
    if tokens.is_empty() {
        return CommandSpec::Shell {
            shell: s.to_string(),
            shell_kind: ShellKind::PosixSh,
        };
    }
    CommandSpec::Argv {
        cmd: tokens[0].clone(),
        args: tokens[1..].to_vec(),
        cwd: None,
        env: HashMap::new(),
    }
}

/// Returns `true` when a command string contains operators that
/// require a shell interpreter.
///
/// Detects:
/// - Shell control operators: `&&`, `||`, `;`, `|`, `>`, `<`
/// - Variable expansion: `$`
/// - Env prefix assignment: `KEY=value command`
pub fn contains_shell_operators(s: &str) -> bool {
    // Env prefix: the string starts with one or more `KEY=value` assignments
    // followed by a space and a command. We detect this by looking for a
    // pattern like `KEY=value ` that appears before the first whitespace-only
    // word that is NOT an assignment.
    if has_env_prefix(s) {
        return true;
    }
    s.contains("&&")
        || s.contains("||")
        || s.contains(';')
        || s.contains('|')
        || s.contains('>')
        || s.contains('<')
        || s.contains('$')
}

/// Detect `KEY=value command` env prefix pattern.
///
/// Matches strings where the first word(s) are `KEY=value` assignments
/// followed by a space and a command word. This avoids false positives
/// on values or URLs that happen to contain `=`.
fn has_env_prefix(s: &str) -> bool {
    let trimmed = s.trim_start();
    let mut chars = trimmed.chars().peekable();
    let mut seen_assignment = false;

    loop {
        // Skip leading whitespace between assignments
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut word = String::new();
        let mut has_eq = false;
        // Collect characters until whitespace or end
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            if c == '=' && !has_eq {
                has_eq = true;
            }
            word.push(c);
            chars.next();
        }
        if word.is_empty() {
            break;
        }
        if has_eq {
            seen_assignment = true;
            // Consume trailing whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            // If there's more text after the assignment, it's a command
            if chars.peek().is_some() {
                return true;
            }
            // Otherwise, loop to check next word
        } else {
            // Not an assignment — if we've seen assignments before this
            // word, this is the command word → env prefix confirmed
            if seen_assignment {
                return true;
            }
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_shell_operators_detects_and_operator() {
        assert!(contains_shell_operators("cd server && bun start"));
    }

    #[test]
    fn contains_shell_operators_detects_or_operator() {
        assert!(contains_shell_operators("make || exit 1"));
    }

    #[test]
    fn contains_shell_operators_detects_pipe() {
        assert!(contains_shell_operators("cat file | grep pattern"));
    }

    #[test]
    fn contains_shell_operators_detects_semicolon() {
        assert!(contains_shell_operators("cmd1; cmd2"));
    }

    #[test]
    fn contains_shell_operators_detects_env_prefix() {
        assert!(contains_shell_operators("NODE_ENV=production bun start"));
    }

    #[test]
    fn contains_shell_operators_detects_multi_env_prefix() {
        assert!(contains_shell_operators("KEY1=val1 KEY2=val2 bun start"));
    }

    #[test]
    fn contains_shell_operators_no_false_positive_on_url() {
        assert!(!contains_shell_operators(
            "DATABASE_URL=postgresql://user:pass@host/db"
        ));
    }

    #[test]
    fn contains_shell_operators_no_false_positive_on_equals_in_arg() {
        assert!(!contains_shell_operators("bun --flag=value"));
    }

    #[test]
    fn contains_shell_operators_detects_dollar() {
        assert!(contains_shell_operators("echo $HOME"));
    }

    #[test]
    fn contains_shell_operators_simple_command_is_false() {
        assert!(!contains_shell_operators("bun install"));
    }

    #[test]
    fn command_spec_from_string_argv() {
        let spec = command_spec_from_string("bun install");
        match spec {
            CommandSpec::Argv { cmd, args, .. } => {
                assert_eq!(cmd, "bun");
                assert_eq!(args, vec!["install"]);
            }
            _ => panic!("expected Argv"),
        }
    }

    #[test]
    fn command_spec_from_string_shell_for_and() {
        let spec = command_spec_from_string("cd x && y");
        match spec {
            CommandSpec::Shell { shell, shell_kind } => {
                assert_eq!(shell, "cd x && y");
                assert_eq!(shell_kind, ShellKind::PosixSh);
            }
            _ => panic!("expected Shell"),
        }
    }

    #[test]
    fn command_spec_from_string_shell_for_env_prefix() {
        let spec = command_spec_from_string("NODE_ENV=production bun start");
        match spec {
            CommandSpec::Shell { shell, .. } => {
                assert_eq!(shell, "NODE_ENV=production bun start");
            }
            _ => panic!("expected Shell"),
        }
    }

    #[test]
    fn command_spec_string_deserializes() {
        let spec: CommandSpec = serde_json::from_str(r#""bun install""#).unwrap();
        match spec {
            CommandSpec::String(s) => assert_eq!(s, "bun install"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn command_spec_argv_deserializes() {
        let json = r#"{"cmd":"bun","args":["install"]}"#;
        let spec: CommandSpec = serde_json::from_str(json).unwrap();
        match spec {
            CommandSpec::Argv { cmd, args, .. } => {
                assert_eq!(cmd, "bun");
                assert_eq!(args, vec!["install"]);
            }
            _ => panic!("expected Argv"),
        }
    }

    #[test]
    fn command_spec_shell_deserializes() {
        let json = r#"{"shell":"cd x && y","shell_kind":"posix-sh"}"#;
        let spec: CommandSpec = serde_json::from_str(json).unwrap();
        match spec {
            CommandSpec::Shell { shell, shell_kind } => {
                assert_eq!(shell, "cd x && y");
                assert_eq!(shell_kind, ShellKind::PosixSh);
            }
            _ => panic!("expected Shell"),
        }
    }
}
