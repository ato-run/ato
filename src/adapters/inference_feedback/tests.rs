use std::path::{Path, PathBuf};

use super::editor::configured_editor_command_from_values;
use super::format::{build_smoke_excerpt, fallback_editor_command_for};
use super::*;

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn telemetry_can_be_disabled_via_env() {
    let _guard = EnvGuard::set(ENV_TELEMETRY, Some("0"));
    assert!(!telemetry_enabled());
}

#[test]
fn configured_editor_command_prefers_visual_and_splits_args() {
    let command = configured_editor_command_from_values(
        Some("code --wait".to_string()),
        Some("nano".to_string()),
    )
    .expect("visual should resolve");

    assert_eq!(command, vec!["code", "--wait"]);
}

#[test]
fn configured_editor_command_uses_editor_when_visual_is_blank() {
    let command =
        configured_editor_command_from_values(Some("   ".to_string()), Some("nano".to_string()))
            .expect("editor should resolve");

    assert_eq!(command, vec!["nano"]);
}

#[test]
fn fallback_editor_command_prefers_macos_open() {
    let command =
        fallback_editor_command_for("macos", |candidate| matches!(candidate, "open" | "nano"))
            .expect("mac fallback should resolve");

    assert_eq!(command, vec!["open", "-W", "-t"]);
}

#[test]
fn fallback_editor_command_prefers_terminal_editor_on_linux() {
    let command =
        fallback_editor_command_for("linux", |candidate| matches!(candidate, "editor" | "nano"))
            .expect("linux fallback should resolve");

    assert_eq!(command, vec!["editor"]);
}

#[test]
fn fallback_editor_command_returns_none_without_candidates() {
    assert!(fallback_editor_command_for("linux", |_| false).is_none());
}

#[test]
fn manifest_diff_summary_counts_changed_lines() {
    let summary = summarize_manifest_diff(
        "schema_version = \"0.2\"\nname = \"demo\"\n",
        "schema_version = \"0.2\"\nname = \"demo-fixed\"\n",
    );
    assert!(summary.contains("Updated 1 line"));
}

#[test]
fn manual_manifest_path_uses_repo_tmp_directory() {
    let path = build_manual_manifest_path(Path::new("/repo"), "koh0920/ato-cli", "attempt1");
    assert_eq!(
        path,
        PathBuf::from("/repo/.ato/tmp/inference/github.com/koh0920/ato-cli/attempt1/capsule.toml")
    );
}

#[test]
fn smoke_excerpt_is_capped_to_store_limit() {
    let report = capsule_core::smoke::SmokeFailureReport {
        class: capsule_core::smoke::SmokeFailureClass::ProcessExitedEarly,
        message: "process exited while waiting for port 8000".to_string(),
        stderr_tail: "x".repeat(5000),
        exit_status: Some(1),
    };

    let excerpt = build_smoke_excerpt(&report);
    assert!(excerpt.chars().count() <= MAX_SMOKE_ERROR_EXCERPT_CHARS);
    assert!(excerpt.contains("process exited while waiting for port 8000"));
    assert!(excerpt.contains("[...]"));
}

#[test]
fn manual_intervention_message_includes_path_and_steps() {
    let message = build_manual_intervention_message(
        Path::new("/repo/.ato/tmp/inference/github.com/koh0920/ato-cli/attempt1/capsule.toml"),
        "DATABASE_URL is required",
        &[
            "Set DATABASE_URL before rerunning.".to_string(),
            "Open the generated manifest and adjust the command if needed.".to_string(),
        ],
    );

    assert!(message.contains("manual intervention required"));
    assert!(message.contains("Generated capsule.toml"));
    assert!(message.contains("DATABASE_URL is required"));
    assert!(message.contains("Set DATABASE_URL before rerunning."));
}
