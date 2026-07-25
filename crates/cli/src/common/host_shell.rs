//! Host shell invocation for manifest-level command strings.
//!
//! Lifecycle strings (`npm install && npm run build`, `uv venv … && uv pip
//! install …`) carry shell operators and quoting, so they must reach a real
//! shell verbatim — never split on whitespace, and never re-quoted by std's
//! CreateProcess argv escaping (cmd.exe does not understand `\"`-style
//! escapes).

use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, ExitStatus, Stdio};

// The definition moved to `snapshot::acceptance`, beside the acceptance protocol
// it protects, so the CLI's build path and the builder's hold path — which run
// the SAME `seal_at.command` — scrub the same namespace from one definition
// (RFC §8.4). Re-exported here so this module's callers are unchanged.
pub use snapshot::acceptance::sanitize_untrusted_environment;

/// The deterministic Windows shell invocation for a command string:
/// `cmd.exe /D /S /C "<command>"`.
///
/// - `/D` disables AutoRun so a broken/foreign `Command Processor\AutoRun`
///   script cannot pollute output or leak a non-zero exit code.
/// - `/S` pins cmd.exe to its documented strip-the-outer-quotes rule instead
///   of the quote-counting heuristic, which mangles commands that contain
///   more than one quoted segment.
/// - The command is appended via `raw_arg` inside explicit outer quotes, so
///   operators (`&&`, `|`) and inner quoting reach cmd.exe byte-for-byte.
#[cfg(windows)]
pub fn windows_cmd_shell_command(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/D").arg("/S").arg("/C");
    cmd.raw_arg(format!("\"{command}\""));
    sanitize_untrusted_environment(&mut cmd);
    cmd
}

/// Platform shell for lifecycle (provision/install/build) command strings:
/// `cmd.exe /D /S /C "<command>"` on Windows, `sh -c <command>` elsewhere.
pub fn lifecycle_shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        windows_cmd_shell_command(command)
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        sanitize_untrusted_environment(&mut cmd);
        cmd
    }
}

/// Builds a `Command` for a CLI that ships as a `.cmd`/`.bat` shim on
/// Windows (npm, npx, pnpm, yarn). CreateProcess cannot start those shims
/// directly — PATH lookup only appends `.exe` — so Windows routes through
/// cmd.exe, which resolves PATHEXT the way an interactive shell does.
///
/// Args are joined verbatim into the cmd.exe command line, so callers must
/// pass fixed, shell-safe tokens (no user input, no embedded quotes or
/// spaces inside a token).
pub fn cmd_shim_command(program: &str, args: &[&str]) -> Command {
    #[cfg(windows)]
    {
        let mut line = String::from(program);
        for arg in args {
            line.push(' ');
            line.push_str(arg);
        }
        windows_cmd_shell_command(&line)
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new(program);
        cmd.args(args);
        sanitize_untrusted_environment(&mut cmd);
        cmd
    }
}

const TAIL_MAX_LINES: usize = 20;
const TAIL_MAX_LINE_BYTES: usize = 400;

pub struct StreamedCommandOutput {
    pub status: ExitStatus,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// Runs `cmd` streaming its stdout/stderr through to the parent's stdio
/// (preserving the interactive progress UX of `Stdio::inherit`) while
/// retaining a bounded tail of each stream for failure reporting.
pub fn run_streaming_with_tails(cmd: &mut Command) -> io::Result<StreamedCommandOutput> {
    sanitize_untrusted_environment(cmd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout_thread = child
        .stdout
        .take()
        .map(|stream| std::thread::spawn(move || tee_tail(stream, io::stdout())));
    let stderr_thread = child
        .stderr
        .take()
        .map(|stream| std::thread::spawn(move || tee_tail(stream, io::stderr())));
    let status = child.wait()?;
    let stdout_tail = stdout_thread
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    let stderr_tail = stderr_thread
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    Ok(StreamedCommandOutput {
        status,
        stdout_tail,
        stderr_tail,
    })
}

/// Copies `reader` into `sink` line-by-line, keeping the last
/// [`TAIL_MAX_LINES`] lines (each capped at [`TAIL_MAX_LINE_BYTES`] bytes).
fn tee_tail(reader: impl Read, mut sink: impl Write) -> String {
    let mut reader = io::BufReader::new(reader);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_MAX_LINES);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let _ = sink.write_all(&buf);
                let _ = sink.flush();
                let mut line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                if line.len() > TAIL_MAX_LINE_BYTES {
                    let mut cut = TAIL_MAX_LINE_BYTES;
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    line.truncate(cut);
                    line.push('…');
                }
                if tail.len() == TAIL_MAX_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }
    }
    tail.into_iter().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn lifecycle_shell_passes_operators_through_in_one_argument() {
        let command = "npm install && npm run build";
        let cmd = lifecycle_shell_command(command);
        let args = rendered_args(&cmd);

        #[cfg(windows)]
        {
            assert_eq!(cmd.get_program(), "cmd.exe");
            assert_eq!(args[..3], ["/D", "/S", "/C"]);
            assert_eq!(args[3], format!("\"{command}\""));
        }

        #[cfg(not(windows))]
        {
            assert_eq!(cmd.get_program(), "sh");
            assert_eq!(args, ["-c", command]);
        }
    }

    #[test]
    fn lifecycle_shell_preserves_inner_quoting() {
        let command =
            r#"uv venv --seed --clear && uv pip install -r requirements.txt "setuptools<72""#;
        let cmd = lifecycle_shell_command(command);
        let args = rendered_args(&cmd);
        let payload = args.last().expect("command payload");
        assert!(
            payload.contains(r#""setuptools<72""#),
            "inner quotes must survive: {payload}"
        );
        assert!(payload.contains("&&"), "operators must survive: {payload}");
    }

    #[cfg(not(windows))]
    #[test]
    fn untrusted_shell_cannot_read_snapshot_acceptance_credentials() {
        let mut cmd = lifecycle_shell_command(
            "test -z \"$ATO_SNAPSHOT_ACCEPTANCE_MAC_KEY\" && test -z \"$ATO_SNAPSHOT_ACCEPTANCE_SIGNER_HELPER\"",
        );
        cmd.env("ATO_SNAPSHOT_ACCEPTANCE_MAC_KEY", "leaked-key")
            .env(
                "ATO_SNAPSHOT_ACCEPTANCE_SIGNER_HELPER",
                "/protected/acceptance-signer",
            );
        sanitize_untrusted_environment(&mut cmd);

        let status = cmd.status().expect("spawn malicious lifecycle command");
        assert!(status.success(), "acceptance credentials reached child");
    }

    #[test]
    fn streaming_runner_reports_exit_code_and_tails() {
        let mut cmd = lifecycle_shell_command("echo tail-marker && exit 7");
        cmd.stdin(Stdio::null());
        let output = run_streaming_with_tails(&mut cmd).expect("spawn shell");
        assert_eq!(output.status.code(), Some(7));
        assert!(
            output.stdout_tail.contains("tail-marker"),
            "stdout tail missing: {:?}",
            output.stdout_tail
        );
    }

    #[test]
    fn tee_tail_keeps_only_the_last_lines() {
        let lines = (0..50)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tee_tail(lines.as_bytes(), io::sink());
        assert!(!tail.contains("line-0"), "old lines must be dropped");
        assert!(tail.contains("line-49"), "newest line must be kept");
        assert_eq!(tail.lines().count(), TAIL_MAX_LINES);
    }
}
