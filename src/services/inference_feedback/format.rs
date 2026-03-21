use rand::RngCore;
use std::path::Path;

use capsule_core::execution_plan::error::AtoExecutionError;

use super::MAX_SMOKE_ERROR_EXCERPT_CHARS;

pub fn summarize_manifest_diff(inferred_toml: &str, actual_toml: &str) -> String {
    let inferred_lines: Vec<&str> = inferred_toml.lines().collect();
    let actual_lines: Vec<&str> = actual_toml.lines().collect();
    let max_len = inferred_lines.len().max(actual_lines.len());
    let mut changed_lines = 0usize;
    for index in 0..max_len {
        if inferred_lines.get(index) != actual_lines.get(index) {
            changed_lines += 1;
        }
    }
    format!(
        "Updated {} line(s) ({} -> {}).",
        changed_lines,
        inferred_lines.len(),
        actual_lines.len()
    )
}

pub(super) fn build_smoke_excerpt(report: &capsule_core::smoke::SmokeFailureReport) -> String {
    let message = report.message.trim();
    let stderr = report.stderr_tail.trim();
    let combined = if stderr.is_empty() {
        message.to_string()
    } else {
        format!("{message}\n{stderr}")
    };
    cap_smoke_excerpt(&combined)
}

pub fn build_manual_intervention_message(
    manifest_path: &Path,
    failure_reason: &str,
    next_steps: &[String],
) -> String {
    let mut message = format!(
        "manual intervention required: {}\nGenerated capsule.toml: {}",
        failure_reason.trim(),
        manifest_path.display()
    );
    if !next_steps.is_empty() {
        message.push_str("\nNext steps:\n");
        for step in next_steps {
            message.push_str("- ");
            message.push_str(step.trim());
            message.push('\n');
        }
        message.pop();
    }
    message
}

pub fn build_manual_intervention_error(
    manifest_path: &Path,
    failure_reason: &str,
    next_steps: &[String],
) -> AtoExecutionError {
    AtoExecutionError::manual_intervention_required(
        build_manual_intervention_message(manifest_path, failure_reason, next_steps),
        Some(&manifest_path.display().to_string()),
        next_steps.to_vec(),
    )
}

pub(super) fn automatic_editor_command() -> Option<Vec<String>> {
    fallback_editor_command_for(std::env::consts::OS, |command| {
        which::which(command).is_ok()
    })
}

pub(super) fn fallback_editor_command_for<F>(os: &str, has_command: F) -> Option<Vec<String>>
where
    F: Fn(&str) -> bool,
{
    if os == "macos" && has_command("open") {
        return Some(vec!["open".to_string(), "-W".to_string(), "-t".to_string()]);
    }

    for command in ["sensible-editor", "editor", "nano", "vim", "vi"] {
        if has_command(command) {
            return Some(vec![command.to_string()]);
        }
    }

    None
}

fn cap_smoke_excerpt(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= MAX_SMOKE_ERROR_EXCERPT_CHARS {
        return trimmed.to_string();
    }

    let head_len = 1400usize;
    let tail_len = MAX_SMOKE_ERROR_EXCERPT_CHARS.saturating_sub(head_len + 7);
    let head: String = trimmed.chars().take(head_len).collect();
    let tail: String = trimmed
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n[...]\n{tail}")
}

pub(super) fn generate_event_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}-{}", hex::encode(bytes))
}
